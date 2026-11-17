//! Live measurement of the platforms: their model catalogue and their quota.
//!
//! Two things are fetched with the connection's own credential, because only
//! the credential opens the answer:
//!
//! * the **catalogue** — what the platform serves today (ADR: «перечень
//!   моделей в реестре берётся из площадки, а не из памяти»);
//! * the **quota** — how much of the budget is left, read from the headers of
//!   the answer and from the text of a refusal.
//!
//! Both are measurements: what comes back is stored as it arrived, and a
//! platform that says nothing conclusive yields `unknown`, never a guess.

use crate::catalog::{self, CatalogModel};
use crate::quota::{self, QuotaReading};
use futures::StreamExt;
use heterion_router_db::repos;
use heterion_router_providers::ProviderRegistry;

/// What one connection's measurement produced.
#[derive(Debug, Clone)]
pub struct SyncOutcome {
    /// Provider id.
    pub provider: String,
    /// The connection that was used.
    pub connection_id: String,
    /// The models the platform listed.
    pub catalog: Vec<CatalogModel>,
    /// The quota reading, when the platform answered at all.
    pub quota: Option<QuotaReading>,
    /// What went wrong, when something did.
    pub error: Option<String>,
}

/// The models a platform lists, fetched with `token`.
pub async fn fetch_catalog(
    http: &reqwest::Client,
    provider: &str,
    format: Option<&str>,
    base_url: &str,
    token: &str,
) -> Result<Vec<CatalogModel>, String> {
    let Some(url) = catalog::models_url(format, base_url) else {
        return Err("the provider declares no models endpoint".to_string());
    };
    let mut request = http.get(&url);
    for (name, value) in catalog::catalog_headers(provider, format, token) {
        request = request.header(name, value);
    }
    let response = request
        .send()
        .await
        .map_err(|error| format!("catalogue request failed: {error}"))?;
    let status = response.status();
    let body = response
        .text()
        .await
        .map_err(|error| format!("catalogue answer unreadable: {error}"))?;
    if !status.is_success() {
        return Err(format!("catalogue answered HTTP {status}"));
    }
    Ok(catalog::parse_catalog(&body))
}

/// The quota of one provider, read from a minimal request.
///
/// The request is the smallest one the platform accepts (`max_tokens: 1`):
/// it is a measurement, not work.
pub async fn fetch_quota(
    http: &reqwest::Client,
    provider: &str,
    format: Option<&str>,
    base_url: &str,
    model: Option<&str>,
    token: &str,
) -> Result<QuotaReading, String> {
    let Some(url) = crate::provider_status::probe_url(format, base_url, model) else {
        return Err("the provider has no probeable endpoint".to_string());
    };
    let body = crate::provider_status::probe_body(format, model);
    let mut request = http.post(&url).body(body);
    for (name, value) in catalog::catalog_headers(provider, format, token) {
        request = request.header(name, value);
    }
    request = request.header("content-type", "application/json");
    let response = request
        .send()
        .await
        .map_err(|error| format!("quota request failed: {error}"))?;
    let status = response.status().as_u16();
    let headers: Vec<(String, String)> = response
        .headers()
        .iter()
        .map(|(name, value)| {
            (
                name.as_str().to_string(),
                value.to_str().unwrap_or_default().to_string(),
            )
        })
        .collect();
    let body = response
        .text()
        .await
        .map_err(|error| format!("quota answer unreadable: {error}"))?;
    Ok(quota::reading_from_response(status, &headers, &body))
}

/// Measure one connection: its catalogue and its quota.
pub async fn sync_connection(
    http: &reqwest::Client,
    provider: &str,
    format: Option<&str>,
    base_url: &str,
    connection_id: &str,
    token: &str,
    model: Option<&str>,
) -> SyncOutcome {
    let catalog = fetch_catalog(http, provider, format, base_url, token).await;
    let quota = fetch_quota(http, provider, format, base_url, model, token).await;
    let (catalog, error) = match catalog {
        Ok(catalog) => (catalog, None),
        Err(error) => (Vec::new(), Some(error)),
    };
    let quota = quota.ok();
    let error = match (error, &quota) {
        (Some(error), _) => Some(error),
        (None, None) => Some("the platform said nothing about the quota".to_string()),
        (None, Some(_)) => None,
    };
    SyncOutcome {
        provider: provider.to_string(),
        connection_id: connection_id.to_string(),
        catalog,
        quota,
        error,
    }
}

/// Store a measurement: the catalogue in `provider_catalog`, the quota in
/// `quota_snapshots` (the history table).
pub fn store_outcome(db: &heterion_router_db::Db, outcome: &SyncOutcome) {
    let guard = db.connection();
    if !outcome.catalog.is_empty() {
        let rows: Vec<(String, Option<i64>)> = outcome
            .catalog
            .iter()
            .map(|model| (model.id.clone(), model.context_length))
            .collect();
        let _ = repos::replace_provider_catalog(
            &guard,
            &outcome.provider,
            &outcome.connection_id,
            &rows,
        );
    }
    if let Some(reading) = &outcome.quota {
        let _ = repos::insert_quota_snapshot(
            &guard,
            &repos::QuotaSnapshotRow {
                provider: outcome.provider.clone(),
                connection_id: outcome.connection_id.clone(),
                window_key: reading.window_key.clone(),
                remaining_percentage: reading.remaining_percentage,
                is_exhausted: reading.state == quota::QuotaState::Exhausted,
                next_reset_at: reading.next_reset_at.clone(),
                window_duration_ms: reading.window_duration_ms,
                raw_data: Some(format!(
                    "{{\"state\":\"{}\",\"detail\":\"{}\"}}",
                    reading.state.as_str(),
                    reading.detail.replace('"', "'")
                )),
                created_at: String::new(),
            },
        );
    }
}

/// Measure every provider that has a credential.
pub async fn sync_all(
    db: &heterion_router_db::Db,
    registry: &ProviderRegistry,
    crypto: Option<&heterion_router_crypto::FieldCrypto>,
    http: &reqwest::Client,
) -> Vec<SyncOutcome> {
    let connections = {
        let guard = db.connection();
        repos::list_connections(&guard).unwrap_or_default()
    };

    let mut targets: Vec<(String, String, String, Option<String>, String, String)> = Vec::new();
    for connection in connections {
        if !connection.is_active {
            continue;
        }
        let Some(entry) = registry.get(&connection.provider) else {
            continue;
        };
        let Some(base_url) = entry.base_url.clone() else {
            continue;
        };
        let model = entry.models.first().map(|model| model.id.clone());
        let credentials = {
            let guard = db.connection();
            crate::credentials::load_credentials(&guard, &connection.provider, crypto)
                .unwrap_or_default()
        };
        for credential in credentials {
            targets.push((
                connection.provider.clone(),
                entry.format.clone().unwrap_or_default(),
                base_url.clone(),
                model.clone(),
                credential.connection_id.clone(),
                credential.token.clone(),
            ));
        }
    }

    // Eight at a time: one provider that hangs must not hold the rest of the
    // catalogue hostage.
    let outcomes = futures::stream::iter(targets)
        .map(
            |(provider, format, base_url, model, connection_id, token)| {
                let http = &*http;
                async move {
                    sync_connection(
                        http,
                        &provider,
                        Some(format.as_str()),
                        &base_url,
                        &connection_id,
                        &token,
                        model.as_deref(),
                    )
                    .await
                }
            },
        )
        .buffer_unordered(8)
        .collect::<Vec<_>>()
        .await;

    for outcome in &outcomes {
        store_outcome(db, outcome);
    }
    outcomes
}

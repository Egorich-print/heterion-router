use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::net::SocketAddr;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::sync::Arc;

use futures::StreamExt;
use heterion_router_config::GatewayConfig;
use heterion_router_crypto::{FieldCrypto, load_secret};
use heterion_router_db::Db;
use heterion_router_gateway::{
    AppState, StatusAwareBackend,
    backend::ChatBackend,
    backend::EchoBackend,
    build_router_with_state,
    claude::ClaudeBackend,
    credentials::load_credentials,
    exa::ExaBackend,
    gemini::{GeminiBackend, ThoughtSignatures},
    grok_cli::{Credential as GrokCredential, GrokCliBackend, RefreshCallback},
    openai::{OpenAiBackend, ProviderOpenAiBackend, resolve_chat_url, resolve_responses_url},
    responses::ResponsesBackend,
    routing::RoutingBackend,
    web::WebBackend,
    web_driver,
};
use heterion_router_providers::ProviderRegistry;
use heterion_router_routing::{RouteRule, Router};

/// Probe every keyless provider the registry declares and store the verdicts.
///
/// The probe asks each platform whether it answers *without* a credential.
/// The answer is what lets the provider into the catalogue (ADR-026): a
/// keyless provider whose platform wants a key is a claim, not a service, and
/// a catalogue entry that always fails is worse than an absent one (ADR-025).
///
/// The verdicts land in `provider_probes`, so the admin API and the matrix
/// report read the same measurement.
async fn probe_keyless_providers(registry: &ProviderRegistry, db: &Arc<Db>) -> usize {
    let Ok(http) = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
    else {
        return 0;
    };
    let connected: HashSet<String> = {
        let guard = db.connection();
        heterion_router_db::repos::list_connections(&guard)
            .unwrap_or_default()
            .into_iter()
            .map(|row| row.provider)
            .collect()
    };
    let candidates: Vec<(String, Option<String>, String, Option<String>)> = registry
        .iter()
        .filter(|entry| {
            heterion_router_gateway::provider_status::keyless_candidate(
                entry.format.as_deref(),
                entry.auth_type.as_deref(),
                entry.base_url.as_deref(),
                entry.executor.as_deref(),
                connected.contains(&entry.id),
            )
        })
        .filter_map(|entry| {
            let base_url = entry.base_url.as_deref()?.to_string();
            let model = entry.models.first().map(|model| model.id.clone());
            Some((entry.id.clone(), entry.format.clone(), base_url, model))
        })
        .collect();
    if candidates.is_empty() {
        return 0;
    }

    let probes = futures::stream::iter(candidates).map(|(provider, format, base_url, model)| {
        let http = &http;
        async move {
            let outcome = heterion_router_gateway::provider_status::probe(
                http,
                format.as_deref(),
                &base_url,
                model.as_deref(),
            )
            .await?;
            Some((provider, outcome))
        }
    });

    let mut probed = 0usize;
    for outcome in probes.buffer_unordered(8).collect::<Vec<_>>().await {
        let Some((provider, outcome)) = outcome else {
            continue;
        };
        probed += 1;
        tracing::info!(
            provider = %provider,
            status = ?outcome.status,
            reason = outcome.reason.as_str(),
            "keyless provider probed"
        );
        let guard = db.connection();
        let _ = heterion_router_db::repos::upsert_provider_probe(
            &guard,
            &heterion_router_db::repos::ProviderProbeRow {
                provider,
                url: outcome.url,
                status: outcome.status.map(|status| status as i64),
                reason: outcome.reason.as_str().to_string(),
                body_excerpt: outcome.body,
                probed_at: String::new(),
            },
        );
    }
    probed
}

/// Resolve grok-cli credentials: an explicit env token wins, otherwise every
/// active connection in the database (decrypted, priority-ordered) forms the
/// rotation pool.
fn grok_credentials(db: &Db, data_dir: &std::path::Path) -> Vec<GrokCredential> {
    if let Ok(token) = std::env::var("GROK_CLI_TOKEN")
        && !token.trim().is_empty()
    {
        return vec![GrokCredential {
            connection_id: None,
            token,
            refresh_token: None,
            expires_at: None,
        }];
    }
    let crypto = load_secret(Some(data_dir)).and_then(|secret| FieldCrypto::from_secret(&secret));
    let guard = db.connection();
    match load_credentials(&guard, "grok-cli", crypto.as_ref()) {
        Ok(credentials) => credentials
            .into_iter()
            .map(|credential| GrokCredential {
                connection_id: Some(credential.connection_id),
                token: credential.token,
                refresh_token: credential.refresh_token,
                expires_at: credential.expires_at,
            })
            .collect(),
        Err(error) => {
            tracing::warn!("grok-cli credential load failed: {error}");
            Vec::new()
        }
    }
}

/// Select the serving backend. Builds every configured backend and routes by
/// model: combo names expand from the database, `provider/model` ids map to
/// the provider backend, `grok-*` prefers grok-cli, and everything else uses
/// an explicit OpenAI-compatible endpoint. The echo backend is a test double
/// and is only registered when `HETERION_ROUTER_ENABLE_ECHO=1`
/// (`OMNIROUTE_ENABLE_ECHO` still honored) — it must never
/// silently answer production traffic.
type Selected = (
    Arc<dyn ChatBackend>,
    Arc<ProviderRegistry>,
    Vec<String>,
    Vec<String>,
);

/// Write a refreshed OAuth token pair back to `provider_connections`.
///
/// Fails closed on the secret-at-rest contract: every other write path
/// stores `enc:v1:` ciphertext (see `heterion_router_crypto::STATIC_SALT`),
/// so without a configured storage key the refreshed tokens are *not*
/// persisted at all rather than downgraded to plaintext. The in-memory
/// credential already holds the new token, so serving continues; only the
/// restart-persistence is lost, and the operator is told which connection.
fn persist_refreshed_tokens(
    db: &Db,
    crypto: Option<&FieldCrypto>,
    provider: &str,
    connection_id: &str,
    token: &str,
    refresh: Option<&str>,
    expires: Option<&str>,
) -> Result<(), String> {
    let Some(crypto) = crypto else {
        tracing::warn!(
            "refreshed {provider} tokens for connection {connection_id} were not persisted: \
             no STORAGE_ENCRYPTION_KEY configured, refusing to store a plaintext secret"
        );
        return Ok(());
    };
    let access_token = crypto.encrypt(token);
    let refresh_token = refresh.map(|value| crypto.encrypt(value));
    let guard = db.connection();
    heterion_router_db::repos::update_connection_tokens(
        &guard,
        connection_id,
        &access_token,
        refresh_token.as_deref(),
        expires,
    )
    .map_err(|error| error.to_string())
}

/// Base URL for a provider: `HETERION_LOCAL_BASE_URL` overrides the registry
/// entry for `heterion-local` (a same-machine Heterion server rarely listens
/// on the default port); every other provider comes from the registry as-is.
fn provider_base_url_from(
    registry: &ProviderRegistry,
    provider: &str,
    local_override: Option<&str>,
) -> Option<String> {
    if provider == "heterion-local"
        && let Some(url) = local_override.filter(|url| !url.trim().is_empty())
    {
        return expand_placeholders(url);
    }
    registry.base_url(provider).and_then(expand_placeholders)
}

/// Fill `{env:NAME}` placeholders in a provider base URL from the environment.
///
/// Some providers address an account-scoped endpoint: Cloudflare Workers AI
/// wants the account id inside the path
/// (`/client/v4/accounts/<account_id>/ai/v1`), and that id belongs to the
/// operator's account, not to the provider — so it cannot live in the static
/// registry. The registry keeps the placeholder, the environment carries the
/// value.
///
/// A placeholder that the environment does not fill yields `None`: a provider
/// whose address is half written cannot answer, and a half-filled address
/// would fail later as an opaque 404 instead of "not configured".
fn expand_placeholders(url: &str) -> Option<String> {
    expand_placeholders_with(url, |name| std::env::var(name).ok())
}

/// The placeholder expansion with an injected lookup, so the tests do not
/// have to mutate the process environment.
fn expand_placeholders_with(url: &str, lookup: impl Fn(&str) -> Option<String>) -> Option<String> {
    let mut out = String::with_capacity(url.len());
    let mut rest = url;
    while let Some(start) = rest.find("{env:") {
        out.push_str(&rest[..start]);
        let tail = &rest[start..];
        let Some(end) = tail.find('}') else {
            // A brace that never closes is not a placeholder: keep the text.
            out.push_str(tail);
            return Some(out);
        };
        let name = &tail["{env:".len()..end];
        let value = lookup(name).filter(|value| !value.trim().is_empty())?;
        out.push_str(&value);
        rest = &tail[end + 1..];
    }
    out.push_str(rest);
    Some(out)
}

/// Active `provider_connections` rows per provider id, ordered by id.
///
/// The counts are what makes a dropped connection visible: a provider id
/// alone cannot show that five accounts are silently unserved.
fn active_connection_counts(db: &Db) -> BTreeMap<String, usize> {
    let guard = db.connection();
    let mut counts = BTreeMap::new();
    let Ok(mut statement) = guard.prepare(
        "SELECT provider, COUNT(*) FROM provider_connections \
         WHERE is_active = 1 GROUP BY provider ORDER BY provider",
    ) else {
        return counts;
    };
    let Ok(rows) = statement.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, i64>(1)?.max(0) as usize,
        ))
    }) else {
        return counts;
    };
    for row in rows.flatten() {
        counts.insert(row.0, row.1);
    }
    counts
}

/// Record that `provider` produced no backend and will not serve traffic.
fn note_unserved(
    unserved: &mut BTreeMap<String, usize>,
    provider: &str,
    counts: &BTreeMap<String, usize>,
) {
    let connections = counts.get(provider).copied().unwrap_or(0);
    *unserved.entry(provider.to_string()).or_insert(0) += connections;
}

/// One aggregated startup warning for connections no backend serves.
///
/// Provider ids and connection counts only: no tokens, no account emails.
/// A single line keeps a nine-connection blind spot from scrolling past as
/// nine identical lines.
fn report_unserved(unserved: &BTreeMap<String, usize>) {
    if unserved.is_empty() {
        return;
    }
    let detail = unserved
        .iter()
        .map(|(provider, count)| format!("{provider}={count}"))
        .collect::<Vec<_>>()
        .join(", ");
    tracing::warn!(
        "{} active connection(s) across {} provider(s) are not served (unknown provider or \
         unsupported format): {detail}",
        unserved.values().sum::<usize>(),
        unserved.len(),
    );
}

/// Wrap a backend so an upstream refusal delivered inside a 200 body carries a
/// real status. Applied to every backend unconditionally, so no insertion path
/// can quietly skip it.
///
/// Without this the router sees a bare `Upstream(message)`, cannot tell a rate
/// limit from an outage, and answers 502 while tripping the breaker over a 4xx
/// the provider is entitled to send.
fn status_aware(backend: Arc<dyn ChatBackend>) -> Arc<dyn ChatBackend> {
    Arc::new(StatusAwareBackend::new(backend))
}

fn select_backend(
    db: Arc<Db>,
    data_dir: &std::path::Path,
) -> Result<Selected, Box<dyn std::error::Error>> {
    let mut backends: HashMap<String, Arc<dyn ChatBackend>> = HashMap::new();
    let echo_enabled = ["HETERION_ROUTER_ENABLE_ECHO", "OMNIROUTE_ENABLE_ECHO"]
        .iter()
        .any(|key| {
            std::env::var(key).is_ok_and(|value| matches!(value.as_str(), "1" | "true" | "yes"))
        });
    if echo_enabled {
        backends.insert("echo".to_string(), status_aware(Arc::new(EchoBackend)));
    }

    let crypto = load_secret(Some(data_dir)).and_then(|secret| FieldCrypto::from_secret(&secret));
    let mut grok_present = false;
    let credentials = grok_credentials(&db, data_dir);
    if !credentials.is_empty() {
        let base_url = std::env::var("GROK_CLI_BASE_URL")
            .unwrap_or_else(|_| "https://cli-chat-proxy.grok.com/v1".to_string());
        tracing::info!(
            "backend available: grok-cli ({base_url}, {} credential(s))",
            credentials.len()
        );
        let refresh_db = db.clone();
        let refresh_crypto = crypto.clone();
        let refresh_callback: RefreshCallback =
            Arc::new(move |connection_id, token, refresh, expires| {
                persist_refreshed_tokens(
                    &refresh_db,
                    refresh_crypto.as_ref(),
                    "grok-cli",
                    connection_id,
                    token,
                    refresh,
                    expires,
                )
            });
        let grok_backend = GrokCliBackend::with_credentials(base_url, credentials)?
            .with_refresh_callback(refresh_callback);
        backends.insert("grok-cli".to_string(), status_aware(Arc::new(grok_backend)));
        grok_present = true;
    }

    let mut openai_present = false;
    if let Ok(base_url) = std::env::var("OPENAI_COMPAT_BASE_URL")
        && !base_url.trim().is_empty()
    {
        let api_key = std::env::var("OPENAI_COMPAT_API_KEY").ok();
        backends.insert(
            "openai-compatible".to_string(),
            status_aware(Arc::new(OpenAiBackend::new(base_url.clone(), api_key)?)),
        );
        openai_present = true;
        tracing::info!("backend available: openai-compatible ({base_url})");
    }

    // Per-provider OpenAI-compatible backends from the registry + database.
    let registry = Arc::new(ProviderRegistry::load());
    // Thought signatures must outlive a restart, or the next turn of an
    // in-flight agent conversation is rejected upstream.
    let signatures = Arc::new(ThoughtSignatures::open(
        data_dir.join("gemini-thought-signatures.jsonl"),
    ));
    let providers = active_connection_counts(&db);
    let mut unserved: BTreeMap<String, usize> = BTreeMap::new();
    for provider in providers.keys() {
        if backends.contains_key(provider) {
            continue;
        }
        let local_override = (provider == "heterion-local")
            .then(|| std::env::var("HETERION_LOCAL_BASE_URL").ok())
            .flatten();
        let Some(base_url) = provider_base_url_from(&registry, provider, local_override.as_deref())
        else {
            // Not in `registry.json`: nothing can serve these rows.
            note_unserved(&mut unserved, provider, &providers);
            continue;
        };

        // Gemini speaks its own protocol (`generateContent` + API-key header).
        if registry
            .get(provider)
            .and_then(|entry| entry.format.as_deref())
            == Some("gemini")
        {
            let tokens: Vec<String> = {
                let guard = db.connection();
                load_credentials(&guard, provider, crypto.as_ref())
                    .unwrap_or_default()
                    .into_iter()
                    .map(|credential| credential.token)
                    .collect()
            };
            if tokens.is_empty() {
                continue;
            }
            let count = tokens.len();
            match GeminiBackend::with_signature_store(
                base_url.to_string(),
                tokens,
                Arc::clone(&signatures),
            ) {
                Ok(backend) => {
                    tracing::info!(
                        "backend available: {provider} (gemini, {base_url}, {count} credential(s))"
                    );
                    backends.insert(provider.clone(), status_aware(Arc::new(backend)));
                }
                Err(error) => tracing::warn!("backend {provider} skipped: {error}"),
            }
            continue;
        }

        // Responses API protocol (e.g. DeepSeek's `/responses`).
        if registry
            .get(provider)
            .and_then(|entry| entry.format.as_deref())
            == Some("openai-responses")
        {
            let tokens: Vec<String> = {
                let guard = db.connection();
                load_credentials(&guard, provider, crypto.as_ref())
                    .unwrap_or_default()
                    .into_iter()
                    .map(|credential| credential.token)
                    .collect()
            };
            if tokens.is_empty() && !registry.allows_keyless(provider) {
                continue;
            }
            let count = tokens.len();
            match ResponsesBackend::new(provider.clone(), base_url.to_string(), tokens) {
                Ok(backend) => {
                    if count == 0 {
                        tracing::info!(
                            "backend available: {provider} (responses, {base_url}, keyless)"
                        );
                    } else {
                        tracing::info!(
                            "backend available: {provider} (responses, {base_url}, {count} credential(s))"
                        );
                    }
                    backends.insert(provider.clone(), status_aware(Arc::new(backend)));
                }
                Err(error) => tracing::warn!("backend {provider} skipped: {error}"),
            }
            continue;
        }

        // Exa-style search providers (`executor: "exa"`): the last user
        // message is the query, results come back cited. A search tool on
        // the chat surface — never a chat combo member (ADR-016).
        if registry
            .get(provider)
            .and_then(|entry| entry.executor.as_deref())
            == Some("exa")
        {
            let tokens: Vec<String> = {
                let guard = db.connection();
                load_credentials(&guard, provider, crypto.as_ref())
                    .unwrap_or_default()
                    .into_iter()
                    .map(|credential| credential.token)
                    .collect()
            };
            if tokens.is_empty() && !registry.allows_keyless(provider) {
                continue;
            }
            let count = tokens.len();
            match ExaBackend::new(provider.clone(), base_url.to_string(), tokens) {
                Ok(backend) => {
                    tracing::info!(
                        "backend available: {provider} (exa, {base_url}, {count} credential(s))"
                    );
                    backends.insert(provider.clone(), status_aware(Arc::new(backend)));
                }
                Err(error) => tracing::warn!("backend {provider} skipped: {error}"),
            }
            continue;
        }

        // Claude-wire protocol (Anthropic `/v1/messages` dialect: zai,
        // agentrouter, kimi-coding, wafer, …).
        if registry
            .get(provider)
            .and_then(|entry| entry.format.as_deref())
            == Some("claude")
        {
            let tokens: Vec<String> = {
                let guard = db.connection();
                load_credentials(&guard, provider, crypto.as_ref())
                    .unwrap_or_default()
                    .into_iter()
                    .map(|credential| credential.token)
                    .collect()
            };
            if tokens.is_empty() && !registry.allows_keyless(provider) {
                continue;
            }
            let count = tokens.len();
            match ClaudeBackend::new(provider.clone(), base_url.to_string(), tokens) {
                Ok(backend) => {
                    tracing::info!(
                        "backend available: {provider} (claude, {base_url}, {count} credential(s))"
                    );
                    backends.insert(provider.clone(), status_aware(Arc::new(backend)));
                }
                Err(error) => tracing::warn!("backend {provider} skipped: {error}"),
            }
            continue;
        }

        if !registry.is_openai_format(provider) {
            // Registry entry exists but speaks a format this build does not
            // implement (`claude`, `kiro`, …).
            note_unserved(&mut unserved, provider, &providers);
            continue;
        }
        let tokens: Vec<String> = {
            let guard = db.connection();
            load_credentials(&guard, provider, crypto.as_ref())
                .unwrap_or_default()
                .into_iter()
                .map(|credential| credential.token)
                .collect()
        };
        // Public gateways (`auth_type: optional`) answer without a key, so an
        // empty pool is only fatal when the provider actually needs one.
        if tokens.is_empty() && !registry.allows_keyless(provider) {
            continue;
        }
        let count = tokens.len();
        let backend = ProviderOpenAiBackend::new(
            provider.clone(),
            resolve_chat_url(&base_url),
            tokens.clone(),
        );
        match backend {
            Ok(mut backend) => {
                let responses_models: HashSet<String> = registry
                    .get(provider)
                    .map(|entry| {
                        entry
                            .models
                            .iter()
                            .filter(|model| {
                                model.target_format.as_deref() == Some("openai-responses")
                            })
                            .map(|model| model.id.clone())
                            .collect()
                    })
                    .unwrap_or_default();
                if !responses_models.is_empty() {
                    backend = backend.with_responses_backend(
                        resolve_responses_url(&base_url),
                        tokens,
                        responses_models,
                    )?;
                }
                if count == 0 {
                    tracing::info!("backend available: {provider} ({base_url}, keyless)");
                } else {
                    tracing::info!(
                        "backend available: {provider} ({base_url}, {count} credential(s))"
                    );
                }
                backends.insert(provider.clone(), status_aware(Arc::new(backend)));
            }
            Err(error) => tracing::warn!("backend {provider} skipped: {error}"),
        }
    }

    // Web-application providers (`executor: "web"`): the answer comes from
    // the operator's browser session, driven by the Camoufox driver process
    // (ADR-025). They have no credential row — the session *is* the
    // credential — so they are picked up from the registry, not from the
    // connection table.
    //
    // Only when the driver is there: a web provider in the catalog that
    // cannot answer is worse than an absent one.
    let driver_url = std::env::var("HETERION_WEB_DRIVER_URL")
        .unwrap_or_else(|_| web_driver::DEFAULT_DRIVER_URL.to_string());
    // Only the providers the driver has an adapter for: without one the
    // backend could not answer, and a catalog entry that always fails is
    // worse than an absent provider.
    let supported: BTreeSet<String> = web_driver::driver_providers(&driver_url);
    if web_driver::driver_reachable(&driver_url) {
        let web_driver = web_driver::WebDriverClient::new(&driver_url);
        if supported.is_empty() {
            tracing::warn!(
                "web executors skipped: the driver at {driver_url} reports no adapter \
                 (start scripts/webdriver/server.py with the adapters beside it)"
            );
        }
        let registry_web: Vec<&str> = registry
            .iter()
            .filter(|entry| entry.executor.as_deref() == Some("web"))
            .map(|entry| entry.id.as_str())
            .collect();
        let registrable = heterion_router_gateway::web::registrable_providers(
            registry_web.iter().copied(),
            &supported,
        );
        for provider in &registrable {
            if backends.contains_key(provider) {
                continue;
            }
            backends.insert(
                provider.clone(),
                status_aware(Arc::new(WebBackend::new(
                    provider.clone(),
                    web_driver.clone(),
                ))),
            );
            tracing::info!("backend available: {provider} (web, {driver_url})");
        }
        for provider in registry_web {
            if !registrable.iter().any(|id| id == provider) {
                tracing::warn!(
                    "backend {provider} skipped: the driver has no adapter for it \
                     (the adapter lives beside the driver, outside the gateway)"
                );
            }
        }
    } else {
        tracing::warn!(
            "web executors skipped: the Camoufox driver does not answer at {driver_url} \
             (start scripts/webdriver/server.py, or point HETERION_WEB_DRIVER_URL at it)"
        );
    }

    // Keyless providers (ADR-026): the registry declares them usable without a
    // credential, but a declaration is a claim. One joins the catalogue only
    // after a probe proved that it answers without a key — a catalogue entry
    // that always fails is worse than an absent one (ADR-025).
    //
    // The verdict is the one the probe stored in `provider_probes`: the probe
    // ran at startup, without any credential.
    for entry in registry.iter() {
        if backends.contains_key(&entry.id) {
            continue;
        }
        let Some(base_url) = provider_base_url_from(&registry, &entry.id, None) else {
            continue;
        };
        if !heterion_router_gateway::provider_status::keyless_candidate(
            entry.format.as_deref(),
            entry.auth_type.as_deref(),
            Some(base_url.as_str()),
            entry.executor.as_deref(),
            providers.contains_key(&entry.id),
        ) {
            continue;
        }
        let verdict = {
            let guard = db.connection();
            heterion_router_db::repos::get_provider_probe(&guard, &entry.id)
        };
        let Some(verdict) = verdict else {
            tracing::debug!(
                provider = %entry.id,
                "keyless provider skipped: it was never probed"
            );
            continue;
        };
        if verdict.reason
            != heterion_router_gateway::provider_status::ProviderReason::Served.as_str()
        {
            tracing::info!(
                provider = %entry.id,
                reason = %verdict.reason,
                "keyless provider skipped: the platform does not answer without a key"
            );
            continue;
        }
        let backend =
            ProviderOpenAiBackend::new(entry.id.clone(), resolve_chat_url(&base_url), Vec::new());
        match backend {
            Ok(backend) => {
                backends.insert(entry.id.clone(), status_aware(Arc::new(backend)));
                tracing::info!("backend available: {} (keyless, {base_url})", entry.id);
            }
            Err(error) => {
                tracing::warn!("backend {} skipped: {error}", entry.id);
            }
        }
    }

    report_unserved(&unserved);

    let mut rules = Vec::new();
    if grok_present {
        rules.push(RouteRule {
            model_prefix: "grok-".to_string(),
            backends: vec!["grok-cli".to_string()],
        });
    }
    let mut default = Vec::new();
    if openai_present {
        default.push("openai-compatible".to_string());
    }
    if echo_enabled {
        default.push("echo".to_string());
        tracing::info!("backend available: echo (test double, opt-in)");
    }
    // Same registry as the backend loop above: `registry.json` is parsed once
    // per startup, not once per consumer.
    let names: Vec<String> = backends.keys().cloned().collect();
    // What each model can do — the ladder's intrinsic input. A table that is
    // missing or empty leaves the router exactly as it was: the static
    // catalogue is the fallback.
    let capabilities = {
        let guard = db.connection();
        heterion_router_gateway::capabilities::CapabilityStore::load(&guard)
    };
    tracing::info!(
        models = capabilities.len(),
        "model capabilities loaded for the ladder"
    );
    // Which models the router has actually talked to: a platform may *list* a
    // model whose id answers `404`, so research alone must not push a model
    // we know works out of the head of the ladder.
    let trust = {
        let guard = db.connection();
        heterion_router_gateway::trust::TrustStore::load(&guard)
    };
    tracing::info!(models = trust.len(), "trusted models loaded for the ladder");
    let routing = RoutingBackend::new(backends, Router::new(rules, default))
        .with_combo_source(db, registry.clone())
        .with_capabilities(Arc::new(capabilities))
        .with_trust(Arc::new(trust))
        // The relay itself is on; the token-saving variant is the operator's
        // call, because it costs the target model the picture itself.
        .with_vision_relay(true)
        .with_vision_saving(
            std::env::var("HETERION_ROUTER_VISION_SAVING")
                .is_ok_and(|value| matches!(value.as_str(), "1" | "true" | "yes")),
        );
    Ok((
        Arc::new(routing),
        registry,
        names,
        supported.into_iter().collect(),
    ))
}

/// When spawned as a Tauri sidecar, the app passes its pid; the gateway exits
/// once the parent is gone, so no app death (quit, force-quit, crash) leaves
/// an orphaned gateway holding the sidecar port. Not set under launchd — the
/// service must outlive unrelated processes.
fn watch_parent() {
    let Some(pid) = parent_watch_pid() else {
        return;
    };
    std::thread::spawn(move || {
        tracing::info!("watching parent {pid} every 2s");
        loop {
            std::thread::sleep(std::time::Duration::from_secs(2));
            if parent_gone(pid) {
                // No logging here: the parent's death usually took the log
                // pipe with it, and a write to that pipe from this thread
                // must never delay the exit.
                std::process::exit(0);
            }
        }
    });
}

/// The pid the sidecar must be watched against, if the app announced one.
///
/// Separate from [`watch_parent`] so the guard that decides whether a
/// watcher thread exists at all is observable from a test.
fn parent_watch_pid() -> Option<u32> {
    std::env::var("HETERION_ROUTER_PARENT_PID")
        .ok()
        .and_then(|value| value.trim().parse::<u32>().ok())
}

/// POSIX liveness probe: `kill(pid, 0)` fails with ESRCH once the process is
/// gone; any other failure (e.g. EPERM) still means "alive".
fn parent_gone(pid: u32) -> bool {
    // SAFETY: `kill` is a plain syscall wrapper with no memory contract, and
    // signal 0 only probes for existence. `pid as i32` is exact: `pid` is a
    // `u32` and a pid never exceeds `i32::MAX`, so the cast narrows nothing.
    let status = unsafe { libc::kill(pid as i32, 0) };
    status != 0 && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
}

fn is_loopback_host(host: &str) -> bool {
    matches!(host.trim(), "127.0.0.1" | "::1" | "localhost")
}

fn restrict_data_files(data_dir: &std::path::Path) -> Result<(), Box<dyn std::error::Error>> {
    #[cfg(unix)]
    for entry in std::fs::read_dir(data_dir)? {
        let path = entry?.path();
        if path.is_file() {
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
        }
    }
    Ok(())
}

/// Make sure the data directory carries a storage key.
///
/// The field encryption takes its key from `STORAGE_ENCRYPTION_KEY` — from the
/// environment or from `<data dir>/.env`. Without one the gateway refuses to
/// store a provider secret (it fails closed instead of writing a plaintext
/// one), so a fresh installation could not accept a key at all. Generating a
/// key on first start keeps the contract: every secret at rest is `enc:v1:`
/// ciphertext, and the key itself never leaves the data directory.
///
/// An existing `.env` is extended, never rewritten — it may carry other
/// settings, and the operator may have chosen the key themselves.
fn ensure_storage_key(data_dir: &std::path::Path) -> Result<(), Box<dyn std::error::Error>> {
    if load_secret(Some(data_dir)).is_some() {
        return Ok(());
    }

    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes)?;
    let secret = hex::encode(bytes);

    let env_path = data_dir.join(".env");
    let mut contents = std::fs::read_to_string(&env_path).unwrap_or_default();
    if !contents.is_empty() && !contents.ends_with('\n') {
        contents.push('\n');
    }
    contents.push_str(&format!("STORAGE_ENCRYPTION_KEY={secret}\n"));

    std::fs::write(&env_path, contents)?;
    #[cfg(unix)]
    std::fs::set_permissions(&env_path, std::fs::Permissions::from_mode(0o600))?;
    tracing::info!("storage encryption key generated at {}", env_path.display());
    Ok(())
}

fn validate_auth_policy(config: &GatewayConfig, db: &Db) -> Result<(), Box<dyn std::error::Error>> {
    if config.require_auth || is_loopback_host(&config.host) {
        return Ok(());
    }
    let guard = db.connection();
    // "A key exists" means a key that can actually authenticate: a table of
    // revoked/banned/expired rows must not unlock a public bind that then
    // answers every request with 401.
    if !heterion_router_gateway::auth::has_usable_api_key(&guard)? {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "non-loopback gateway requires at least one usable API key",
        )
        .into());
    }
    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = GatewayConfig::from_env();
    std::fs::create_dir_all(&config.data_dir)?;
    restrict_data_files(&config.data_dir)?;
    ensure_storage_key(&config.data_dir)?;

    let log_path = config.data_dir.join("gateway.log");
    let file_appender = tracing_appender::rolling::daily(&config.data_dir, "gateway.log");
    let (non_blocking, _guard) = tracing_appender::non_blocking(file_appender);

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(config.log_filter.clone())),
        )
        .with_target(false)
        .with_writer(non_blocking)
        .init();

    tracing::info!("log file: {}", log_path.display());
    watch_parent();

    let db_path = config.data_dir.join("storage.sqlite");
    let db = Db::open(&db_path)?;
    let applied = db.migrate()?;
    if applied.is_empty() {
        tracing::info!("database ready at {}", db_path.display());
    } else {
        tracing::info!(
            "database migrated ({}) at {}",
            applied.join(","),
            db_path.display()
        );
    }
    validate_auth_policy(&config, &db)?;

    let db = Arc::new(db);
    // Measure the keyless platforms before the catalogue is built: the
    // verdict decides whether a provider without a credential may serve.
    let probe_registry = Arc::new(ProviderRegistry::load());
    let probed = probe_keyless_providers(&probe_registry, &db).await;
    if probed > 0 {
        tracing::info!(probed, "keyless providers probed");
    }
    let (backend, registry, backend_names, web_adapters) =
        select_backend(db.clone(), &config.data_dir)?;
    let ui_dir = heterion_router_gateway::ui::ui_dir();
    match ui_dir.as_ref() {
        Some(dir) => tracing::info!("dashboard served from {}", dir.display()),
        None => tracing::info!("dashboard not built (API only)"),
    }
    let state = AppState::with_db(backend, db)
        .with_require_auth(config.require_auth)
        .with_max_body_bytes(config.max_body_bytes)
        .with_catalog(registry, backend_names)
        .with_web_adapters(web_adapters)
        .with_ui_dir(ui_dir)
        .with_data_dir(config.data_dir.clone());
    let app = build_router_with_state(state);

    let ip: std::net::IpAddr = config.host.parse()?;
    let addr = SocketAddr::new(ip, config.port);
    let listener = tokio::net::TcpListener::bind(addr).await?;

    tracing::info!("heterion-router-gateway listening on http://{addr}");
    axum::serve(listener, app).await?;
    Ok(())
}

/// Collect what the crate logs while `body` runs.
///
/// `tracing::subscriber::set_default` is thread-local, so a capture cannot
/// leak into the tests running in parallel on other threads.
#[cfg(test)]
fn capture_warnings(body: impl FnOnce()) -> String {
    #[derive(Clone, Default)]
    struct Capture(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

    impl std::io::Write for Capture {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Capture {
        type Writer = Capture;

        fn make_writer(&'a self) -> Self::Writer {
            self.clone()
        }
    }

    let capture = Capture::default();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(capture.clone())
        .with_ansi(false)
        .without_time()
        .finish();
    let _guard = tracing::subscriber::set_default(subscriber);
    body();
    let bytes = capture.0.lock().unwrap().clone();
    String::from_utf8(bytes).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECRET: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
    const FRESH_ACCESS: &str = "fresh-access-token";
    const FRESH_REFRESH: &str = "fresh-refresh-token";

    fn registry() -> ProviderRegistry {
        ProviderRegistry::load()
    }

    fn db_with_connection(id: &str, provider: &str) -> Db {
        let db = Db::open_in_memory().unwrap();
        db.migrate().unwrap();
        let guard = db.connection();
        heterion_router_db::repos::upsert_connection(
            &guard,
            &heterion_router_db::repos::ConnectionRow {
                id: id.to_string(),
                provider: provider.to_string(),
                auth_type: Some("oauth".to_string()),
                name: None,
                email: Some("owner@example.com".to_string()),
                priority: 10,
                is_active: true,
                access_token: Some("enc:v1:stored:previous:value".to_string()),
                refresh_token: Some("enc:v1:stored:previous:refresh".to_string()),
                expires_at: None,
                api_key: None,
                provider_specific_data: None,
            },
        )
        .unwrap();
        drop(guard);
        db
    }

    fn stored_tokens(db: &Db, id: &str) -> (String, Option<String>) {
        let guard = db.connection();
        guard
            .query_row(
                "SELECT access_token, refresh_token FROM provider_connections WHERE id = ?1",
                [id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap()
    }

    /// RED-FIRST (FIX 1): without a storage key the refreshed secret must
    /// never reach the database in plaintext.
    #[test]
    fn refresh_without_a_storage_key_never_persists_a_plaintext_token() {
        let db = db_with_connection("c1", "grok-cli");
        persist_refreshed_tokens(
            &db,
            None,
            "grok-cli",
            "c1",
            FRESH_ACCESS,
            Some(FRESH_REFRESH),
            Some("2999-01-01T00:00:00.000Z"),
        )
        .unwrap();

        let (access, refresh) = stored_tokens(&db, "c1");
        assert_ne!(
            access, FRESH_ACCESS,
            "refreshed access token was persisted in plaintext"
        );
        assert!(
            !refresh
                .as_deref()
                .is_some_and(|value| value == FRESH_REFRESH),
            "refreshed refresh token was persisted in plaintext"
        );
        // The row is untouched: still the previously stored ciphertext.
        assert_eq!(access, "enc:v1:stored:previous:value");
        assert_eq!(refresh.as_deref(), Some("enc:v1:stored:previous:refresh"));
    }

    /// The encrypted path must keep working — the live databases hold
    /// `enc:v1:` values keyed by `STATIC_SALT`.
    #[test]
    fn refresh_with_a_storage_key_stores_encrypted_tokens() {
        let db = db_with_connection("c1", "grok-cli");
        let crypto = FieldCrypto::from_secret(SECRET).unwrap();
        persist_refreshed_tokens(
            &db,
            Some(&crypto),
            "grok-cli",
            "c1",
            FRESH_ACCESS,
            Some(FRESH_REFRESH),
            Some("2999-01-01T00:00:00.000Z"),
        )
        .unwrap();

        let (access, refresh) = stored_tokens(&db, "c1");
        assert!(access.starts_with(heterion_router_crypto::PREFIX));
        assert_eq!(crypto.decrypt(&access).as_deref(), Some(FRESH_ACCESS));
        assert_eq!(
            refresh
                .as_deref()
                .and_then(|value| crypto.decrypt(value))
                .as_deref(),
            Some(FRESH_REFRESH)
        );
    }

    #[test]
    fn heterion_local_url_prefers_env_then_registry() {
        let registry = registry();
        assert_eq!(
            provider_base_url_from(&registry, "heterion-local", None).as_deref(),
            Some("http://127.0.0.1:8080/v1")
        );
        assert_eq!(
            provider_base_url_from(
                &registry,
                "heterion-local",
                Some("http://192.168.1.10:8080/v1")
            )
            .as_deref(),
            Some("http://192.168.1.10:8080/v1")
        );
        assert_eq!(
            provider_base_url_from(&registry, "heterion-local", Some("  ")).as_deref(),
            Some("http://127.0.0.1:8080/v1")
        );
    }

    /// A registry URL may carry an `{env:NAME}` placeholder: some providers
    /// address an account-scoped endpoint (Cloudflare Workers AI needs the
    /// account id in the path), and that id belongs to the operator, not to the
    /// provider. The placeholder is filled from the environment.
    #[test]
    fn env_placeholders_in_a_base_url_are_filled_from_the_environment() {
        let filled = expand_placeholders_with(
            "https://api.example/client/v4/accounts/{env:ACCOUNT}/ai/v1",
            |name| (name == "ACCOUNT").then(|| "acct-1".to_string()),
        );
        assert_eq!(
            filled.as_deref(),
            Some("https://api.example/client/v4/accounts/acct-1/ai/v1")
        );
    }

    /// A URL without a placeholder is returned as it is.
    #[test]
    fn a_base_url_without_placeholders_is_unchanged() {
        assert_eq!(
            expand_placeholders_with("https://api.example/v1", |_| None).as_deref(),
            Some("https://api.example/v1")
        );
    }

    /// An unresolved placeholder is not a URL: the provider cannot answer, so
    /// it must not be built with a half-filled address.
    #[test]
    fn an_unresolved_placeholder_yields_no_url() {
        assert_eq!(
            expand_placeholders_with("https://api.example/accounts/{env:MISSING}/ai/v1", |_| None),
            None
        );
        assert_eq!(
            expand_placeholders_with("https://api.example/accounts/{env:EMPTY}", |_| {
                Some(String::new())
            }),
            None
        );
    }

    /// Several placeholders in one URL are all filled.
    #[test]
    fn every_placeholder_in_a_url_is_filled() {
        let filled = expand_placeholders_with("https://{env:HOST}/{env:PATH}", |name| match name {
            "HOST" => Some("api.example".to_string()),
            "PATH" => Some("v1".to_string()),
            _ => None,
        });
        assert_eq!(filled.as_deref(), Some("https://api.example/v1"));
    }

    #[test]
    fn non_loopback_requires_a_key_before_startup() {
        let db = Db::open_in_memory().unwrap();
        db.migrate().unwrap();
        let config = GatewayConfig::from_map(&HashMap::from([(
            "HETERION_ROUTER_HOST".to_string(),
            "0.0.0.0".to_string(),
        )]));
        assert!(validate_auth_policy(&config, &db).is_err());

        let guard = db.connection();
        heterion_router_db::repos::insert_api_key(
            &guard,
            &heterion_router_db::repos::ApiKeyRow {
                id: "bootstrap".to_string(),
                name: "bootstrap".to_string(),
                key: "sk-bootstrap".to_string(),
                allowed_models: "[]".to_string(),
                no_log: false,
            },
        )
        .unwrap();
        drop(guard);
        assert!(validate_auth_policy(&config, &db).is_ok());
    }

    #[test]
    fn loopback_keeps_first_run_bootstrap_open() {
        let db = Db::open_in_memory().unwrap();
        db.migrate().unwrap();
        let config = GatewayConfig::from_map(&HashMap::from([(
            "HETERION_ROUTER_HOST".to_string(),
            "127.0.0.1".to_string(),
        )]));
        assert!(validate_auth_policy(&config, &db).is_ok());
    }

    // --- FIX 2: the startup gate must ask "is any key usable?", not "does
    // the table have rows?". A database where every key is dead must fail
    // a non-loopback bind exactly like an empty one.

    fn bound_config() -> GatewayConfig {
        GatewayConfig::from_map(&HashMap::from([(
            "HETERION_ROUTER_HOST".to_string(),
            "0.0.0.0".to_string(),
        )]))
    }

    fn db_with_api_key(id: &str, key: &str) -> Db {
        let db = Db::open_in_memory().unwrap();
        db.migrate().unwrap();
        let guard = db.connection();
        heterion_router_db::repos::insert_api_key(
            &guard,
            &heterion_router_db::repos::ApiKeyRow {
                id: id.to_string(),
                name: id.to_string(),
                key: key.to_string(),
                allowed_models: "[\"*\"]".to_string(),
                no_log: false,
            },
        )
        .unwrap();
        drop(guard);
        db
    }

    #[test]
    fn non_loopback_refuses_a_revoked_only_key_table() {
        let db = db_with_api_key("k1", "sk-revoked");
        let guard = db.connection();
        heterion_router_db::repos::set_api_key_revoked(&guard, "k1", true).unwrap();
        drop(guard);
        assert!(
            validate_auth_policy(&bound_config(), &db).is_err(),
            "a database with no usable key must not unlock a non-loopback bind"
        );
    }

    #[test]
    fn non_loopback_refuses_banned_inactive_expired_and_blank_keys() {
        for (label, sql) in [
            ("banned", "UPDATE api_keys SET is_banned = 1"),
            ("inactive", "UPDATE api_keys SET is_active = 0"),
            (
                "expired",
                "UPDATE api_keys SET expires_at = '2001-01-01 00:00:00'",
            ),
            ("blank", "UPDATE api_keys SET key = '  '"),
        ] {
            let db = db_with_api_key("k1", "sk-dead");
            let guard = db.connection();
            guard.execute(sql, []).unwrap();
            drop(guard);
            assert!(
                validate_auth_policy(&bound_config(), &db).is_err(),
                "{label} key must not unlock a non-loopback bind"
            );
        }
    }

    #[test]
    fn non_loopback_still_binds_when_one_key_is_usable() {
        let db = db_with_api_key("k1", "sk-dead");
        let guard = db.connection();
        heterion_router_db::repos::set_api_key_revoked(&guard, "k1", true).unwrap();
        drop(guard);
        let guard = db.connection();
        heterion_router_db::repos::insert_api_key(
            &guard,
            &heterion_router_db::repos::ApiKeyRow {
                id: "k2".to_string(),
                name: "k2".to_string(),
                key: "sk-live".to_string(),
                allowed_models: "[\"*\"]".to_string(),
                no_log: false,
            },
        )
        .unwrap();
        drop(guard);
        assert!(validate_auth_policy(&bound_config(), &db).is_ok());
    }

    #[test]
    fn other_providers_ignore_the_heterion_override() {
        let registry = registry();
        let from_registry = provider_base_url_from(&registry, "openai", None);
        assert_eq!(
            provider_base_url_from(&registry, "openai", Some("http://192.168.1.10:8080/v1")),
            from_registry
        );
    }

    // --- FIX 3: connections no backend serves must be visible in the log.

    fn db_with_connections(rows: &[(&str, &str, &str)]) -> Db {
        let db = Db::open_in_memory().unwrap();
        db.migrate().unwrap();
        let guard = db.connection();
        for (index, (id, provider, api_key)) in rows.iter().enumerate() {
            heterion_router_db::repos::upsert_connection(
                &guard,
                &heterion_router_db::repos::ConnectionRow {
                    id: (*id).to_string(),
                    provider: (*provider).to_string(),
                    auth_type: Some("apikey".to_string()),
                    name: None,
                    email: Some(format!("owner{index}@example.com")),
                    priority: 10,
                    is_active: true,
                    access_token: None,
                    refresh_token: None,
                    expires_at: None,
                    api_key: Some((*api_key).to_string()),
                    provider_specific_data: None,
                },
            )
            .unwrap();
        }
        drop(guard);
        db
    }

    fn empty_data_dir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "heterion-router-select-{tag}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// The aggregated warning must name the dropped provider ids and how
    /// many connections each one accounts for — never an account email.
    #[test]
    fn unknown_providers_are_reported_once_with_connection_counts() {
        // `exa-search` gained a real executor; a provider absent from the
        // registry entirely is the unknown case now.
        let db = db_with_connections(&[
            ("e1", "ghost-provider", "sk-a"),
            ("e2", "ghost-provider", "sk-b"),
            ("e3", "ghost-provider", "sk-c"),
            ("o1", "openai", "sk-openai"),
        ]);
        let data_dir = empty_data_dir("unknown");

        let logs = capture_warnings(|| {
            select_backend(std::sync::Arc::new(db), &data_dir).unwrap();
        });

        let lines: Vec<&str> = logs
            .lines()
            .filter(|line| line.contains("are not served"))
            .collect();
        assert_eq!(
            lines.len(),
            1,
            "expected exactly one aggregated line, got {lines:?}"
        );
        assert!(lines[0].contains("ghost-provider=3"), "got {}", lines[0]);
        assert!(!lines[0].contains("ghost-provider=1"), "count is wrong");
        assert!(!logs.contains("owner0@example.com"), "email leaked");
        assert!(!logs.contains("sk-a"), "secret leaked");
        let _ = std::fs::remove_dir_all(&data_dir);
    }

    /// A registry provider this build cannot speak (`claude`) is dropped the
    /// same way and must be reported too.
    #[test]
    fn unsupported_formats_are_reported_with_connection_counts() {
        // `claude` gained a real executor; these two formats are still
        // without one and must keep being reported.
        let db = db_with_connections(&[("k1", "kiro", "sk-kiro"), ("c2", "cursor", "sk-cursor")]);
        let data_dir = empty_data_dir("unsupported");

        let logs = capture_warnings(|| {
            select_backend(std::sync::Arc::new(db), &data_dir).unwrap();
        });

        assert_eq!(logs.matches("are not served").count(), 1, "got {logs}");
        assert!(logs.contains("kiro=1"), "got {logs}");
        assert!(logs.contains("cursor=1"), "got {logs}");
        let _ = std::fs::remove_dir_all(&data_dir);
    }

    /// Nothing dropped, nothing warned about.
    #[test]
    fn known_providers_are_not_reported_as_unserved() {
        let db = db_with_connections(&[("o1", "openai", "sk-openai")]);
        let data_dir = empty_data_dir("known");

        let logs = capture_warnings(|| {
            select_backend(std::sync::Arc::new(db), &data_dir).unwrap();
        });

        assert!(
            !logs.contains("are not served"),
            "a fully served database must stay quiet: {logs}"
        );
        let _ = std::fs::remove_dir_all(&data_dir);
    }

    /// `report_unserved` is the single log line; the aggregation itself.
    #[test]
    fn report_unserved_aggregates_into_one_line() {
        let mut unserved = BTreeMap::new();
        unserved.insert("exa-search".to_string(), 5);
        unserved.insert("claude".to_string(), 3);
        unserved.insert("kiro".to_string(), 1);

        let logs = capture_warnings(|| report_unserved(&unserved));
        assert_eq!(logs.matches("are not served").count(), 1, "got {logs}");
        assert!(logs.contains("exa-search=5"), "got {logs}");
        assert!(logs.contains("claude=3"), "got {logs}");
        assert!(logs.contains("kiro=1"), "got {logs}");
        assert!(logs.contains("9 active connection(s)"), "got {logs}");

        // Empty in, silent out.
        assert!(capture_warnings(|| report_unserved(&BTreeMap::new())).is_empty());
    }
}

#[cfg(test)]
mod parent_watch_tests {
    use super::*;

    #[test]
    fn live_process_is_not_gone() {
        assert!(!parent_gone(std::process::id()));
    }

    #[test]
    fn absent_process_is_gone() {
        assert!(parent_gone(999_999_999));
    }

    #[test]
    fn watch_parent_without_marker_is_a_no_op() {
        // The whole no-op is decided by the marker guard: with no marker
        // `watch_parent` returns before reaching `thread::spawn`, and the
        // probe thread must survive the call.
        let previous = std::env::var("HETERION_ROUTER_PARENT_PID").ok();
        // SAFETY: edition-2024 makes env mutation unsafe because it
        // races another thread reading the environment; this test owns
        // its variable name.
        unsafe { std::env::remove_var("HETERION_ROUTER_PARENT_PID") };

        // The marker decides the outcome; absent means nothing is armed.
        assert_eq!(parent_watch_pid(), None, "no marker must arm no watcher");

        let started = std::time::Instant::now();
        watch_parent();
        let elapsed = started.elapsed();
        assert!(
            elapsed < std::time::Duration::from_secs(1),
            "watch_parent blocked for {elapsed:?}"
        );

        // A malformed marker is equally inert — otherwise a typo in the
        // launchd plist would arm a watcher against pid 0.
        // SAFETY: edition-2024 makes env mutation unsafe because it
        // races another thread reading the environment; this test owns
        // its variable name.
        unsafe { std::env::set_var("HETERION_ROUTER_PARENT_PID", "not-a-pid") };
        assert_eq!(parent_watch_pid(), None);

        match previous {
            // SAFETY: edition-2024 makes env mutation unsafe because it
            // races another thread reading the environment; this test owns
            // its variable name.
            Some(value) => unsafe { std::env::set_var("HETERION_ROUTER_PARENT_PID", value) },
            // SAFETY: edition-2024 makes env mutation unsafe because it
            // races another thread reading the environment; this test owns
            // its variable name.
            None => unsafe { std::env::remove_var("HETERION_ROUTER_PARENT_PID") },
        }
    }
}

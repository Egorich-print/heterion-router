//! Admin API backing the dashboard.
//!
//! Serves what the JS dashboard showed, over the same SQLite the gateway
//! already uses: which providers are live, how combos resolve, recent usage
//! and recent calls, and the API keys that guard it all. Secrets are never
//! serialised — list endpoints expose only prefixes — except at creation,
//! when the new secret is returned exactly once.
//!
//! Mutations stay deliberately small: combo metadata and contents,
//! connection activity/priority, and key issuance/revocation. Credential
//! rotation for providers is not exposed here; it stays a JS-dashboard/CLI
//! operation until the Rust side owns decryption.

use std::borrow::Cow;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path as StdPath, PathBuf};

use axum::{
    Json,
    extract::{Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
};
use heterion_router_db::repos;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::AppState;
use crate::JsonBody;
use crate::apikeys;
use crate::auth::ManageAuthenticated;
use crate::combos::{dispatch_backend, parse_combo_rich};
use crate::restart;

/// Query for the time-ranged endpoints.
#[derive(Debug, Deserialize)]
pub struct ListQuery {
    /// Window length in days (clamped to 1..=90). Defaults to 7.
    pub days: Option<u32>,
    /// Maximum rows to return (clamped to 1..=500). Defaults to 100.
    pub limit: Option<u32>,
}

impl ListQuery {
    /// SQLite datetime modifier for the requested window.
    fn since(&self) -> String {
        format!("-{} days", self.days.unwrap_or(7).clamp(1, 90))
    }

    fn limit(&self) -> usize {
        self.limit.unwrap_or(100).clamp(1, 500) as usize
    }
}

/// `GET /api/overview` — gateway identity, live backends and row counts.
pub async fn overview(State(state): State<AppState>, _auth: ManageAuthenticated) -> Json<Value> {
    let mut combos = 0;
    let mut connections_active = 0;
    let mut connections_total = 0;
    let mut api_keys = 0;

    if let Some(db) = state.db.as_ref() {
        let guard = db.connection();
        combos = repos::list_active_combos(&guard)
            .map(|rows| rows.len())
            .unwrap_or(0);
        if let Ok(rows) = repos::list_connections(&guard) {
            connections_total = rows.len();
            connections_active = rows.iter().filter(|row| row.is_active).count();
        }
        api_keys = repos::count_active_api_keys(&guard).unwrap_or(0);
    }

    let mut backends = state.backend_names.clone();
    backends.sort();
    backends.dedup();

    Json(json!({
        "status": "ok",
        "backend": state.backend.name(),
        "backends": backends,
        "counts": {
            "combos": combos,
            "connections_active": connections_active,
            "connections_total": connections_total,
            "api_keys": api_keys,
        },
    }))
}

/// `GET /api/combos` — every combo with each entry's dispatch verdict.
///
/// `servable` per entry comes from the router's own predicate, so the UI shows
/// exactly what the gateway would do.
pub async fn combos(State(state): State<AppState>, _auth: ManageAuthenticated) -> Json<Value> {
    let Some(db) = state.db.as_ref() else {
        return Json(json!({ "combos": [] }));
    };
    let Some(registry) = state.registry.as_ref() else {
        return Json(json!({ "combos": [] }));
    };

    let available: std::collections::HashSet<String> =
        state.backend_names.iter().cloned().collect();
    let guard = db.connection();
    let mut out: Vec<Value> = Vec::new();

    if let Ok(rows) = repos::list_active_combos(&guard) {
        for row in rows {
            let data: Value = serde_json::from_str(&row.data).unwrap_or(Value::Null);
            let (strategy, rich) = parse_combo_rich(&data).unwrap_or_default();
            let entries: Vec<Value> = rich
                .iter()
                .map(|item| {
                    let backend = dispatch_backend(&item.entry, registry, &available);
                    json!({
                        "id": item.id,
                        "provider": item.entry.provider,
                        "model": item.entry.model,
                        "weight": item.weight,
                        "backend": backend,
                        "servable": backend.is_some(),
                    })
                })
                .collect();
            out.push(json!({
                "id": row.id,
                "name": row.name,
                "strategy": strategy,
                "sort_order": row.sort_order,
                "is_hidden": is_hidden(&row.data),
                "active": row.active,
                "servable_entries": entries.iter().filter(|entry| entry["servable"] == json!(true)).count(),
                "total_entries": entries.len(),
                "entries": entries,
            }));
        }
    }

    Json(json!({ "combos": out }))
}

/// `GET /api/connections` — providers and their connections, secrets masked.
pub async fn connections(State(state): State<AppState>, _auth: ManageAuthenticated) -> Json<Value> {
    let Some(db) = state.db.as_ref() else {
        return Json(json!({ "connections": [] }));
    };

    let servable: std::collections::HashSet<String> = {
        let mut set = std::collections::HashSet::new();
        if let Some(registry) = state.registry.as_ref() {
            for name in &state.backend_names {
                // Backend names are canonical ids; also expose aliases that
                // point at them so the UI can match stored provider strings.
                set.insert(name.clone());
                for entry in registry.iter() {
                    if &entry.id == name
                        && let Some(alias) = entry.alias.as_deref()
                    {
                        set.insert(alias.to_string());
                    }
                }
            }
        }
        set
    };

    let guard = db.connection();
    let rows = match repos::list_connections(&guard) {
        Ok(rows) => rows,
        Err(error) => {
            return Json(json!({ "connection_error": error.to_string(), "connections": [] }));
        }
    };

    let connections: Vec<Value> = rows
        .iter()
        .map(|row| {
            json!({
                "id": row.id,
                "provider": row.provider,
                "name": row.name,
                "auth_type": row.auth_type,
                "is_active": row.is_active,
                "priority": row.priority,
                "expires_at": row.expires_at,
                "test_status": row.test_status,
                "last_error": row.last_error,
                "has_credential": row.has_credential,
                "servable": servable.contains(&row.provider),
            })
        })
        .collect();

    Json(json!({ "connections": connections }))
}

/// `GET /api/keys` — every API key, secrets masked to their prefix.
///
/// Nothing here is credential material: the response carries the display
/// prefix and metadata only, never `api_keys.key` and never `key_hash`. A row
/// whose `key` column is `enc:v1:`-encrypted is reported as such — the
/// plaintext branch of the credential predicate skips those rows, so they
/// authenticate through their own `key_hash` and nothing else.
pub async fn keys(State(state): State<AppState>, _auth: ManageAuthenticated) -> Json<Value> {
    let Some(db) = state.db.as_ref() else {
        return Json(json!({ "keys": [] }));
    };
    let guard = db.connection();
    let rows = match repos::list_api_keys(&guard) {
        Ok(rows) => rows,
        Err(error) => {
            return Json(json!({ "error": error.to_string(), "keys": [] }));
        }
    };
    let keys: Vec<Value> = rows
        .iter()
        .map(|row| {
            json!({
                "id": row.id,
                "name": row.name,
                "prefix": row.key_prefix,
                "encrypted": row.is_encrypted,
                "created_at": row.created_at,
                "revoked_at": row.revoked_at,
                "expires_at": row.expires_at,
                "last_used_at": row.last_used_at,
                "is_active": row.is_active,
                "revoked": row.revoked_at.is_some(),
            })
        })
        .collect();
    Json(json!({ "keys": keys }))
}

/// Body for `POST /api/keys`.
#[derive(Debug, Deserialize)]
pub struct CreateKeyBody {
    pub name: Option<String>,
}

/// `POST /api/keys` — issue an operator key.
///
/// Mirrors the JS dashboard's key shape (`sk-` + 32 hex, prefix, sha256
/// hash, manage scope) so the key works on both sides. The row keeps only the
/// hash: `repos::insert_full_api_key` writes the `hash-only:<id>` marker into
/// the `key` column, so nothing readable in the database — not the secret, not
/// the digest, not the display prefix — can be presented back as a bearer
/// token. The full secret is returned exactly once, here, and never logged or
/// echoed into an error.
pub async fn create_key(
    State(state): State<AppState>,
    _auth: ManageAuthenticated,
    JsonBody(body): crate::JsonBody<CreateKeyBody>,
) -> impl IntoResponse {
    let name = body.name.unwrap_or_default().trim().to_string();
    if name.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "name must not be empty" })),
        );
    }
    if name.len() > 64 {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "name must be at most 64 characters" })),
        );
    }
    let Some(db) = state.db.as_ref() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({ "error": "database not configured" })),
        );
    };
    let guard = db.connection();
    // Paranoia retries on id/secret collision; 128 bits do not collide twice.
    for _ in 0..3 {
        let generated = match apikeys::generate() {
            Ok(generated) => generated,
            Err(error) => {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({ "error": format!("randomness failed: {error}") })),
                );
            }
        };
        let row = repos::NewApiKey {
            id: generated.id.clone(),
            name: name.clone(),
            // Carried for the caller that returns it once; the row keeps the
            // hash only, so the secret is not recoverable from the database.
            key: generated.secret.clone(),
            key_prefix: generated.prefix.clone(),
            key_hash: generated.hash.clone(),
            allowed_models: "[]".to_string(),
            scopes: "[\"manage\",\"self:usage\",\"self:account-quota\"]".to_string(),
        };
        match repos::insert_full_api_key(&guard, &row) {
            Ok(()) => {
                return (
                    StatusCode::CREATED,
                    Json(json!({
                        "status": "ok",
                        "id": generated.id,
                        "name": name,
                        "key": generated.secret,
                        "prefix": generated.prefix,
                        "warning": "Copy the key now — the gateway never shows it again.",
                    })),
                );
            }
            Err(error) if is_unique_violation(&error) => continue,
            Err(error) => {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({ "error": error.to_string() })),
                );
            }
        }
    }
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(json!({ "error": "could not issue a unique key" })),
    )
}

/// `POST /api/keys/:id/revoke` — revoke a key.
///
/// Takes effect immediately: authentication reads the table on every
/// request. Idempotent.
pub async fn revoke_key(
    State(state): State<AppState>,
    _auth: ManageAuthenticated,
    Path(id): Path<String>,
) -> impl IntoResponse {
    set_key_revoked(state, &id, true).await
}

/// `POST /api/keys/:id/restore` — un-revoke a key. Idempotent.
pub async fn restore_key(
    State(state): State<AppState>,
    _auth: ManageAuthenticated,
    Path(id): Path<String>,
) -> impl IntoResponse {
    set_key_revoked(state, &id, false).await
}

async fn set_key_revoked(state: AppState, id: &str, revoked: bool) -> (StatusCode, Json<Value>) {
    let Some(db) = state.db.as_ref() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({ "error": "database not configured" })),
        );
    };
    let guard = db.connection();
    match repos::set_api_key_revoked(&guard, id, revoked) {
        Ok(false) => (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "api key not found" })),
        ),
        Ok(true) => {
            let revoked_at = repos::get_api_key_summary(&guard, id)
                .ok()
                .flatten()
                .and_then(|row| row.revoked_at);
            (
                StatusCode::OK,
                Json(json!({
                    "status": "ok",
                    "id": id,
                    "revoked": revoked,
                    "revoked_at": revoked_at,
                })),
            )
        }
        Err(error) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": error.to_string() })),
        ),
    }
}

/// `POST /api/restart` — restart the supervised gateway.
///
/// The dashboard offers this after connection activity changes, which only
/// take effect on restart. Succeeds only when launchd supervises us
/// (`HETERION_ROUTER_SERVICE_LABEL`, legacy `OMNIROUTE_SERVICE_LABEL`,
/// default `com.heterion.router`): elsewhere it
/// answers 409 so a dev-process operator never kills their own shell by
/// accident. The restart re-execs the same binary ~2s after the 202, so
/// in-flight requests are dropped — call it from a quiet moment.
pub async fn restart_service(_auth: ManageAuthenticated) -> impl IntoResponse {
    let label = restart::service_label();
    let Some(uid) = restart::current_uid() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({ "error": "cannot determine current user" })),
        );
    };
    if !restart::is_supervised(&label, &uid) {
        return (
            StatusCode::CONFLICT,
            Json(json!({ "error": "gateway is not supervised by launchd; restart it yourself" })),
        );
    }
    match restart::request_restart(&label, &uid) {
        Ok(()) => (
            StatusCode::ACCEPTED,
            Json(json!({ "status": "restarting", "label": label })),
        ),
        Err(error) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": error.to_string() })),
        ),
    }
}

/// `GET /api/usage?days=7&limit=10` — totals plus top providers and models.
pub async fn usage(
    State(state): State<AppState>,
    _auth: ManageAuthenticated,
    Query(query): Query<ListQuery>,
) -> Json<Value> {
    let Some(db) = state.db.as_ref() else {
        return Json(json!({ "usage": null }));
    };
    let guard = db.connection();
    let since = query.since();
    let limit = query.limit().min(50);

    let summary = match repos::usage_summary(&guard, &since, limit) {
        Ok(summary) => summary,
        Err(error) => return Json(json!({ "error": error.to_string() })),
    };

    let buckets = |rows: &[repos::UsageBucket]| -> Vec<Value> {
        rows.iter()
            .map(|bucket| {
                json!({
                    "key": bucket.key,
                    "requests": bucket.requests,
                    "tokens_input": bucket.tokens_input,
                    "tokens_output": bucket.tokens_output,
                })
            })
            .collect()
    };

    Json(json!({
        "window_days": query.days.unwrap_or(7).clamp(1, 90),
        "requests": summary.requests,
        "successes": summary.successes,
        "tokens_input": summary.tokens_input,
        "tokens_output": summary.tokens_output,
        "tokens_cache_read": summary.tokens_cache_read,
        // Percentiles, not just sums: an operator reading a dashboard needs
        // the tail, and the tail is also the only data source a
        // `least-latency` routing strategy will ever have.
        "latency": {
            "p50_ms": summary.latency.p50_ms,
            "p95_ms": summary.latency.p95_ms,
            "p99_ms": summary.latency.p99_ms,
        },
        "by_provider": buckets(&summary.by_provider),
        "by_model": buckets(&summary.by_model),
    }))
}

/// `GET /api/logs?limit=50` — recent calls, newest first.
pub async fn logs(
    State(state): State<AppState>,
    _auth: ManageAuthenticated,
    Query(query): Query<ListQuery>,
) -> Json<Value> {
    let Some(db) = state.db.as_ref() else {
        return Json(json!({ "logs": [] }));
    };
    let guard = db.connection();
    let rows = match repos::recent_calls(&guard, query.limit()) {
        Ok(rows) => rows,
        Err(error) => return Json(json!({ "error": error.to_string(), "logs": [] })),
    };

    let logs: Vec<Value> = rows
        .iter()
        .map(|row| {
            json!({
                "timestamp": row.timestamp,
                "model": row.model,
                "requested_model": row.requested_model,
                "provider": row.provider,
                "status": row.status,
                "duration_ms": row.duration_ms,
                "tokens_input": row.tokens_input,
                "tokens_output": row.tokens_output,
                "combo_name": row.combo_name,
                "error_summary": row.error_summary,
            })
        })
        .collect();

    Json(json!({ "logs": logs }))
}

fn application_log_path(data_dir: &StdPath) -> PathBuf {
    let direct = data_dir.join("gateway.log");
    if direct.is_file() {
        return direct;
    }
    std::fs::read_dir(data_dir)
        .ok()
        .into_iter()
        .flatten()
        .filter_map(std::result::Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.is_file()
                && path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with("gateway.log."))
        })
        .max_by_key(|path| {
            path.file_name()
                .map(|name| name.to_owned())
                .unwrap_or_default()
        })
        .unwrap_or(direct)
}

/// Bytes read from the end of an application log (512 KiB). `GET /api/logs/app`
/// returns at most 1000 lines, so reading the whole daily log only ever
/// allocated megabytes to throw them away. The window is generous enough to
/// cover the limit many times over at any realistic line length.
pub const APP_LOG_TAIL_BYTES: usize = 512 * 1024;

/// Last `limit` lines of a log file, oldest first, reading only the tail.
///
/// Missing/unreadable files yield no lines, as before. When the read window
/// starts mid-file the first (partial) line is dropped, and when the cut
/// lands inside a multi-byte character the leading bytes are trimmed rather
/// than decoded, so a truncated read can never panic or emit a replacement
/// character.
fn tail_lines(path: &StdPath, limit: usize) -> Vec<String> {
    let Ok(mut file) = std::fs::File::open(path) else {
        return Vec::new();
    };
    let Ok(len) = file.metadata().map(|meta| meta.len()) else {
        return Vec::new();
    };
    let start = len.saturating_sub(APP_LOG_TAIL_BYTES as u64);
    let mut window = Vec::with_capacity((len - start) as usize);
    if file.seek(SeekFrom::Start(start)).is_err()
        || file.take(len - start).read_to_end(&mut window).is_err()
    {
        return Vec::new();
    }
    let text: Cow<'_, str> = match std::str::from_utf8(&window) {
        Ok(text) => Cow::Borrowed(text),
        // A multi-byte character straddles the cut: skip its leading bytes,
        // and only fall back to lossy for a genuinely corrupt log.
        Err(_) => (1..=3)
            .find_map(|skip| std::str::from_utf8(&window[skip..]).ok())
            .map_or_else(|| String::from_utf8_lossy(&window), Cow::Borrowed),
    };
    let mut lines: Vec<&str> = text.lines().collect();
    let start_index = lines.len().saturating_sub(limit);
    lines
        .split_off(start_index)
        .into_iter()
        .map(String::from)
        .collect()
}

/// `GET /api/logs/app?limit=50` — recent application/tracing log lines.
pub async fn logs_app(
    State(state): State<AppState>,
    _auth: ManageAuthenticated,
    Query(query): Query<LogAppQuery>,
) -> Json<Value> {
    let Some(data_dir) = state.data_dir.as_ref() else {
        return Json(json!({ "lines": [] }));
    };
    let log_path = application_log_path(data_dir);
    let limit = query.limit().min(1000) as usize;

    Json(json!({ "lines": tail_lines(&log_path, limit) }))
}

/// Query for the application log endpoint.
#[derive(Debug, Deserialize)]
pub struct LogAppQuery {
    /// Maximum lines to return (clamped to 1..=1000). Defaults to 50.
    pub limit: Option<u32>,
}

impl LogAppQuery {
    fn limit(&self) -> u32 {
        self.limit.unwrap_or(50).clamp(1, 1000)
    }
}

/// Read the `isHidden` flag out of a combo `data` blob.
///
/// A corrupt or non-object blob counts as visible: hiding must be explicit.
fn is_hidden(data: &str) -> bool {
    serde_json::from_str::<Value>(data)
        .ok()
        .and_then(|value| value.get("isHidden")?.as_bool())
        .unwrap_or(false)
}

/// Body for `PATCH /api/combos/:id`. Every field is optional; absent fields
/// are left untouched.
#[derive(Debug, Deserialize)]
pub struct UpdateComboBody {
    pub name: Option<String>,
    pub sort_order: Option<i64>,
    pub is_hidden: Option<bool>,
    pub is_active: Option<bool>,
    pub strategy: Option<String>,
    pub models: Option<Vec<ComboModelBody>>,
}

/// One model entry in a combo rewrite: provider plus bare model id.
///
/// `weight` defaults to 50 when absent. Entry `id` is kept when sent back
/// (the dashboard round-trips it); otherwise the server derives one.
#[derive(Debug, Deserialize)]
pub struct ComboModelBody {
    pub id: Option<String>,
    pub provider: String,
    pub model: String,
    pub weight: Option<i64>,
}

/// Strategies the gateway understands. Anything else is a 400: storing an
/// unknown strategy would silently change routing semantics.
fn is_known_strategy(strategy: &str) -> bool {
    matches!(strategy, "priority" | "auto")
}

/// Body for `PATCH /api/connections/:id`. Every field is optional; absent
/// fields are left untouched.
#[derive(Debug, Deserialize)]
pub struct UpdateConnectionBody {
    pub name: Option<String>,
    pub is_active: Option<bool>,
    pub priority: Option<i64>,
}

/// A SQLite unique-constraint failure (duplicate combo name on rename).
fn is_unique_violation(error: &heterion_router_db::DbError) -> bool {
    error.to_string().contains("UNIQUE constraint failed")
}

/// `PATCH /api/combos/:id` — rename, reorder, hide, restyle or restock a combo.
///
/// Combo routing reads the database on every request, so the change takes
/// effect immediately. `404` when `id` is unknown, `409` when `name` collides
/// with another combo, `400` when the body carries nothing usable, names an
/// unknown strategy, or sends an empty/unusable model list.
pub async fn update_combo(
    State(state): State<AppState>,
    _auth: ManageAuthenticated,
    Path(id): Path<String>,
    JsonBody(body): crate::JsonBody<UpdateComboBody>,
) -> impl IntoResponse {
    if let Some(name) = body.name.as_deref()
        && name.trim().is_empty()
    {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "name must not be empty" })),
        );
    }
    if let Some(strategy) = body.strategy.as_deref()
        && !is_known_strategy(strategy)
    {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "strategy must be \"priority\" or \"auto\"" })),
        );
    }
    let mut models: Option<Vec<repos::ComboModelSpec>> = None;
    if let Some(items) = body.models.as_deref() {
        if items.is_empty() {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": "models must not be empty" })),
            );
        }
        let mut specs = Vec::with_capacity(items.len());
        for item in items {
            if item.provider.trim().is_empty() || item.model.trim().is_empty() {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(json!({ "error": "every model needs a provider and a model id" })),
                );
            }
            let weight = item.weight.unwrap_or(50);
            if !(0..=100).contains(&weight) {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(json!({ "error": "weight must be between 0 and 100" })),
                );
            }
            specs.push(repos::ComboModelSpec {
                id: item.id.clone(),
                provider: item.provider.trim().to_string(),
                model: item.model.trim().to_string(),
                weight,
            });
        }
        models = Some(specs);
    }
    let patch = repos::ComboMetaPatch {
        name: body.name.map(|name| name.trim().to_string()),
        sort_order: body.sort_order,
        is_hidden: body.is_hidden,
        is_active: body.is_active,
        strategy: body.strategy,
        models,
    };
    if patch.name.is_none()
        && patch.sort_order.is_none()
        && patch.is_hidden.is_none()
        && patch.strategy.is_none()
        && patch.models.is_none()
    {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "no fields to update" })),
        );
    }
    let Some(db) = state.db.as_ref() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({ "error": "database not configured" })),
        );
    };
    let guard = db.connection();
    match repos::update_combo_meta(&guard, &id, &patch) {
        Ok(Some(row)) => (
            StatusCode::OK,
            Json(json!({
                "status": "ok",
                "id": row.id,
                "name": row.name,
                "sort_order": row.sort_order,
                "is_hidden": is_hidden(&row.data),
                "active": row.active,
            })),
        ),
        Ok(None) => (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "combo not found" })),
        ),
        Err(error) if is_unique_violation(&error) => (
            StatusCode::CONFLICT,
            Json(json!({ "error": "another combo already uses that name" })),
        ),
        Err(error) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": error.to_string() })),
        ),
    }
}

/// `PATCH /api/connections/:id` — rename, toggle or reprioritise a provider
/// connection.
///
/// `restart_required` is true when `is_active` flipped: backends are built
/// once at startup, so the live gateway only picks the change up on restart.
pub async fn update_connection(
    State(state): State<AppState>,
    _auth: ManageAuthenticated,
    Path(id): Path<String>,
    JsonBody(body): crate::JsonBody<UpdateConnectionBody>,
) -> impl IntoResponse {
    if body.name.is_none() && body.is_active.is_none() && body.priority.is_none() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "no fields to update" })),
        );
    }
    let Some(db) = state.db.as_ref() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({ "error": "database not configured" })),
        );
    };
    let guard = db.connection();
    let previously_active = repos::get_connection(&guard, &id)
        .ok()
        .flatten()
        .map(|row| row.is_active);
    let patch = repos::ConnectionMetaPatch {
        name: body.name,
        is_active: body.is_active,
        priority: body.priority,
    };
    match repos::update_connection_meta(&guard, &id, &patch) {
        Ok(Some(row)) => {
            let restart_required = previously_active.is_some_and(|active| active != row.is_active);
            (
                StatusCode::OK,
                Json(json!({
                    "status": "ok",
                    "id": row.id,
                    "is_active": row.is_active,
                    "priority": row.priority,
                    "restart_required": restart_required,
                })),
            )
        }
        Ok(None) => (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "connection not found" })),
        ),
        Err(error) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": error.to_string() })),
        ),
    }
}

#[cfg(test)]
mod tests {
    use std::alloc::{GlobalAlloc, Layout, System};
    use std::cell::Cell;
    use std::path::{Path as StdPath, PathBuf};
    use std::sync::Arc;

    use axum::extract::{Query, State};
    use serde_json::Value;

    use super::*;
    use crate::auth::ManageAuthenticated;
    use crate::backend::EchoBackend;

    /// Counts live bytes allocated by the *current thread* while armed, so a
    /// test can prove a handler did not pull a whole file into memory. Only
    /// armed tests pay for the bookkeeping; the counters are const-init
    /// `Cell`s (no destructor), so the allocator never allocates or unwinds.
    struct AllocationProbe;

    thread_local! {
        static ARMED: Cell<bool> = const { Cell::new(false) };
        static LIVE: Cell<isize> = const { Cell::new(0) };
        static PEAK: Cell<isize> = const { Cell::new(0) };
    }

    fn adjust(delta: isize) {
        if !ARMED.get() {
            return;
        }
        let live = LIVE.get() + delta;
        LIVE.set(live);
        PEAK.set(PEAK.get().max(live));
    }

    // SAFETY: every method forwards to `System`, the global allocator, with
    // the layout it was given, so alignment/size are the std contract's. The
    // only added work is a const-init `Cell` update on the current thread, so
    // no other thread observes it and the allocator never re-enters itself
    // (the counters have no destructor and allocate nothing).
    // SAFETY: see the note directly above this impl.
    unsafe impl GlobalAlloc for AllocationProbe {
        // SAFETY: forwarding to `System.alloc` with the same `layout`.
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            // SAFETY: as above.
            let ptr = unsafe { System.alloc(layout) };
            if !ptr.is_null() {
                adjust(layout.size() as isize);
            }
            ptr
        }

        // SAFETY: as above.
        unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
            // SAFETY: as above.
            let ptr = unsafe { System.alloc_zeroed(layout) };
            if !ptr.is_null() {
                adjust(layout.size() as isize);
            }
            ptr
        }

        // SAFETY: as above — the pointer came from this allocator.
        unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
            // SAFETY: as above.
            unsafe { System.dealloc(ptr, layout) };
            adjust(-(layout.size() as isize));
        }

        // SAFETY: as above — the pointer came from this allocator.
        unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
            // SAFETY: as above.
            let new_ptr = unsafe { System.realloc(ptr, layout, new_size) };
            if !new_ptr.is_null() {
                adjust(new_size as isize - layout.size() as isize);
            }
            new_ptr
        }
    }

    #[global_allocator]
    static PROBE: AllocationProbe = AllocationProbe;

    /// Peak live bytes allocated on this thread while `f` runs.
    fn peak_bytes<T>(f: impl FnOnce() -> T) -> (T, usize) {
        LIVE.set(0);
        PEAK.set(0);
        ARMED.set(true);
        let out = f();
        ARMED.set(false);
        (out, PEAK.get().max(0) as usize)
    }

    fn scratch_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "heterion-router-admin-{}-{name}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write_log(dir: &StdPath, content: &[u8]) {
        std::fs::write(dir.join("gateway.log"), content).unwrap();
    }

    /// Call `GET /api/logs/app` the way axum does and return its JSON body.
    async fn get_app_log(dir: &StdPath, limit: Option<u32>) -> Value {
        let state = AppState::new(Arc::new(EchoBackend)).with_data_dir(dir.to_path_buf());
        let query = LogAppQuery { limit };
        logs_app(State(state), ManageAuthenticated, Query(query))
            .await
            .0
    }

    fn lines_of(value: &Value) -> Vec<String> {
        value["lines"]
            .as_array()
            .expect("lines array")
            .iter()
            .map(|line| line.as_str().expect("string line").to_string())
            .collect()
    }

    /// The contract as it stood before the tail-read fix: last `limit` lines
    /// of the whole file, oldest first.
    fn expected_tail(content: &[u8], limit: usize) -> Vec<String> {
        let text = String::from_utf8_lossy(content);
        let all: Vec<&str> = text.lines().collect();
        all[all.len().saturating_sub(limit)..]
            .iter()
            .map(|line| line.to_string())
            .collect()
    }

    // --- The `api_keys.key` digest-is-a-credential exploit (W-13 / C-9 / E4),
    // driven through the real issuance path.

    /// Run the `Authenticated` extractor over a bearer token, exactly as axum
    /// does for `/v1/*`. `Ok(())` authenticated; `Err` is the rejection status.
    async fn authenticate(
        state: &AppState,
        token: &str,
    ) -> std::result::Result<(), axum::http::StatusCode> {
        use axum::body::Body;
        use axum::extract::FromRequestParts;
        use axum::http::Request;

        let request = Request::builder()
            .header(axum::http::header::AUTHORIZATION, format!("Bearer {token}"))
            .body(Body::empty())
            .unwrap();
        let (mut parts, _) = request.into_parts();
        <crate::auth::Authenticated as FromRequestParts<AppState>>::from_request_parts(
            &mut parts, state,
        )
        .await
        .map(|_authenticated| ())
        .map_err(|rejection| rejection.status())
    }

    /// Issue a key through `POST /api/keys` against a throwaway database file
    /// (never a live `~/.heterion-router`), then read the row back raw.
    async fn issue_key_on_disk(dir: &StdPath) -> (AppState, Value, String, String) {
        use http_body_util::BodyExt;

        let db =
            std::sync::Arc::new(heterion_router_db::Db::open(&dir.join("storage.sqlite")).unwrap());
        db.migrate().unwrap();
        let state = AppState::with_db(Arc::new(EchoBackend), db.clone());

        let response = create_key(
            State(state.clone()),
            ManageAuthenticated,
            JsonBody(CreateKeyBody {
                name: Some("ops".to_string()),
            }),
        )
        .await
        .into_response();
        assert_eq!(response.status(), StatusCode::CREATED);
        let bytes = response
            .into_body()
            .collect()
            .await
            .expect("body")
            .to_bytes();
        let body: Value = serde_json::from_slice(&bytes).expect("json body");
        let secret = body["key"].as_str().expect("secret once").to_string();
        assert!(secret.starts_with("sk-"));

        let (stored_key, stored_hash) = {
            let guard = db.connection();
            guard
                .query_row(
                    "SELECT key, key_hash FROM api_keys WHERE id = ?1",
                    rusqlite::params![body["id"].as_str().expect("id")],
                    |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
                )
                .unwrap()
        };
        (state, body, stored_key, stored_hash)
    }

    #[tokio::test(flavor = "current_thread")]
    async fn an_issued_key_stores_nothing_that_authenticates() {
        let dir = scratch_dir("key-exploit");
        let (state, body, stored_key, stored_hash) = issue_key_on_disk(&dir).await;
        let secret = body["key"].as_str().unwrap().to_string();

        assert_eq!(stored_hash, repos::api_key_hash(&secret));
        assert!(
            authenticate(&state, &secret).await.is_ok(),
            "the secret returned once must authenticate"
        );
        for (label, presented) in [("key column", &stored_key), ("digest", &stored_hash)] {
            assert_eq!(
                authenticate(&state, presented).await,
                Err(axum::http::StatusCode::UNAUTHORIZED),
                "the value read from the {label} authenticated as a bearer token"
            );
        }
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn the_key_list_endpoint_never_returns_the_secret_or_digest() {
        let dir = scratch_dir("key-list");
        let (state, body, stored_key, stored_hash) = issue_key_on_disk(&dir).await;
        let secret = body["key"].as_str().unwrap().to_string();

        let listed = keys(State(state), ManageAuthenticated).await.0;
        let rendered = listed.to_string();
        assert!(
            !rendered.contains(&secret),
            "GET /api/keys leaked the secret"
        );
        assert!(
            !rendered.contains(&stored_key),
            "GET /api/keys leaked api_keys.key"
        );
        assert!(
            !rendered.contains(&stored_hash),
            "GET /api/keys leaked the digest"
        );
        assert_eq!(listed["keys"][0]["id"], body["id"]);
        let _ = std::fs::remove_dir_all(dir);
    }

    /// A legacy row from the frozen JS server: plaintext in `key`, matched
    /// verbatim. It must keep authenticating after the fix.
    #[tokio::test(flavor = "current_thread")]
    async fn a_legacy_plaintext_row_still_authenticates() {
        let dir = scratch_dir("key-legacy");
        let db =
            std::sync::Arc::new(heterion_router_db::Db::open(&dir.join("storage.sqlite")).unwrap());
        db.migrate().unwrap();
        {
            let guard = db.connection();
            guard
                .execute(
                    "INSERT INTO api_keys (id, name, key, allowed_models, scopes, created_at)
                     VALUES ('js-1', 'js-1', 'sk-legacy-plaintext-synthetic', '[]',
                             '[\"manage\"]', datetime('now'))",
                    [],
                )
                .unwrap();
        }
        let state = AppState::with_db(Arc::new(EchoBackend), db);
        assert_eq!(
            authenticate(&state, "sk-legacy-plaintext-synthetic").await,
            Ok(()),
            "legacy plaintext rows must keep working"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn dated_application_log_is_discovered() {
        let dir =
            std::env::temp_dir().join(format!("heterion-router-log-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("gateway.log.2026-09-24"), "line\n").unwrap();
        assert!(application_log_path(&dir).ends_with("gateway.log.2026-09-24"));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn dated_application_log_is_discovered_in_scratch() {
        let dir = scratch_dir("dated");
        write_log(&dir, b"line\n");
        std::fs::write(dir.join("gateway.log.2026-09-24"), b"older\n").unwrap();
        assert!(application_log_path(&dir).ends_with("gateway.log"));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn app_log_endpoint_stays_bounded_on_a_huge_file() {
        let dir = scratch_dir("huge");
        let line = "2026-09-24T00:00:00Z INFO request served model=synthetic-model\n";
        let body = line.repeat(500_000); // ~27 MiB, far over any tail cap.
        write_log(&dir, body.as_bytes());

        let (value, peak) = peak_bytes(|| {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap()
                .block_on(get_app_log(&dir, Some(1000)))
        });

        let lines = lines_of(&value);
        assert_eq!(lines.len(), 1000);
        assert_eq!(lines.last().map(String::as_str), Some(line.trim_end()));
        assert!(
            peak < 4 * 1024 * 1024,
            "reading the log tail allocated {peak} bytes; the endpoint must not buffer the file"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn small_app_log_is_returned_whole() {
        let dir = scratch_dir("small");
        let content = "first line\nsecond line\nthird line\n";
        write_log(&dir, content.as_bytes());
        let lines = lines_of(&get_app_log(&dir, None).await);
        assert_eq!(lines, expected_tail(content.as_bytes(), 50));
        assert_eq!(lines, vec!["first line", "second line", "third line"]);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn many_lines_return_the_last_requested_lines() {
        let dir = scratch_dir("many");
        let mut content = String::new();
        for index in 0..40_000 {
            content.push_str(&format!(
                "2026-09-24T00:00:00Z INFO line {index:06} synthetic\n"
            ));
        }
        write_log(&dir, content.as_bytes());
        assert!(
            content.len() > 1024 * 1024,
            "fixture must exceed the tail cap"
        );

        let lines = lines_of(&get_app_log(&dir, Some(1000)).await);
        assert_eq!(lines.len(), 1000);
        assert_eq!(lines, expected_tail(content.as_bytes(), 1000));
        assert_eq!(
            lines.last().map(String::as_str),
            Some("2026-09-24T00:00:00Z INFO line 039999 synthetic")
        );
        assert_eq!(
            lines.first().map(String::as_str),
            Some("2026-09-24T00:00:00Z INFO line 039000 synthetic")
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn empty_app_log_returns_no_lines() {
        let dir = scratch_dir("empty");
        write_log(&dir, b"");
        let value = get_app_log(&dir, Some(1000)).await;
        assert_eq!(value, json!({ "lines": [] }));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn missing_app_log_returns_no_lines() {
        let dir = scratch_dir("missing");
        let value = get_app_log(&dir, Some(1000)).await;
        assert_eq!(value, json!({ "lines": [] }));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn multibyte_lines_survive_a_cut_inside_a_character() {
        let dir = scratch_dir("utf8");
        let mut content = String::new();
        for index in 0..20_000 {
            content.push_str(&format!("line {index:06} привет 😀 café\n"));
        }
        assert!(
            content.len() > APP_LOG_TAIL_BYTES,
            "fixture must exceed the tail window"
        );
        let mut bytes = content.into_bytes();
        // Park a 4-byte character so that the byte window the tail reader
        // seeks to starts inside it.
        let marker = "😀".as_bytes().to_vec();
        let cut = bytes.len().saturating_sub(APP_LOG_TAIL_BYTES) + 3;
        bytes.splice(cut..cut, marker);
        assert!(
            std::str::from_utf8(&bytes[bytes.len() - APP_LOG_TAIL_BYTES..]).is_err(),
            "fixture must split a multi-byte character"
        );
        write_log(&dir, &bytes);

        let lines = lines_of(&get_app_log(&dir, Some(1000)).await);
        assert_eq!(lines.len(), 1000);
        assert_eq!(lines, expected_tail(&bytes, 1000));
        assert!(lines.iter().all(|line| line.contains("😀")));
        let _ = std::fs::remove_dir_all(dir);
    }
}

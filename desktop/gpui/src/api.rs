//! Dead-code warnings are expected here: this module is the typed
//! mirror of the gateway's admin contract, not a call site list.
#![allow(dead_code)]

//! Typed client for the router's admin HTTP API.
//!
//! The native shell speaks exactly the same endpoints as the Svelte dashboard
//! (`desktop/ui/src/api.js`), so both frontends stay pinned to one gateway
//! contract. Every response type below mirrors the JSON the gateway actually
//! produces (`crates/heterion-router-gateway/src/admin.rs`).

use std::fmt;
use std::sync::LazyLock;
use std::time::Duration;

use gpui::Global;
use serde::Deserialize;
use serde::Serialize;
use serde::de::DeserializeOwned;

/// Shared HTTP client. One pool for the whole process: the dashboard fires
/// several requests in parallel when it opens.
static HTTP: LazyLock<reqwest::blocking::Client> = LazyLock::new(|| {
    reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(20))
        .build()
        .unwrap_or_default()
});

/// A failed API call.
#[derive(Debug, Clone)]
pub struct ApiError {
    /// HTTP status code, or `0` when the request never reached the gateway.
    pub status: u16,
    /// Human-readable detail — the gateway's `error` field when it sent one.
    pub message: String,
}

impl ApiError {
    /// A failure that never reached the gateway (connection refused, timeout).
    pub fn transport(message: impl Into<String>) -> Self {
        Self {
            status: 0,
            message: message.into(),
        }
    }
}

impl fmt::Display for ApiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.status == 0 {
            write!(f, "{}", self.message)
        } else {
            write!(f, "HTTP {}: {}", self.status, self.message)
        }
    }
}

impl std::error::Error for ApiError {}

/// A client bound to one gateway base URL and one operator key.
#[derive(Clone, Debug)]
pub struct ApiClient {
    base: String,
    key: String,
}

impl ApiClient {
    /// Build a client. The base URL keeps its scheme; a trailing slash is
    /// dropped so paths can be appended directly.
    pub fn new(base: impl Into<String>, key: impl Into<String>) -> Self {
        Self {
            base: base.into().trim_end_matches('/').to_string(),
            key: key.into(),
        }
    }

    /// The gateway base URL this client talks to.
    pub fn base(&self) -> &str {
        &self.base
    }

    /// `GET {path}` decoded into `T`.
    pub fn get_json<T: DeserializeOwned>(&self, path: &str) -> Result<T, ApiError> {
        self.request(reqwest::Method::GET, path, None)
    }

    /// `POST {path}` with a JSON body, decoded into `T`.
    pub fn post_json<T: DeserializeOwned, B: Serialize>(
        &self,
        path: &str,
        body: &B,
    ) -> Result<T, ApiError> {
        let body = serde_json::to_value(body).map_err(|error| ApiError {
            status: 0,
            message: format!("could not encode the request: {error}"),
        })?;
        self.request(reqwest::Method::POST, path, Some(body))
    }

    /// `PATCH {path}` with a JSON body, decoded into `T`.
    pub fn patch_json<T: DeserializeOwned, B: Serialize>(
        &self,
        path: &str,
        body: &B,
    ) -> Result<T, ApiError> {
        let body = serde_json::to_value(body).map_err(|error| ApiError {
            status: 0,
            message: format!("could not encode the request: {error}"),
        })?;
        self.request(reqwest::Method::PATCH, path, Some(body))
    }

    /// `POST {path}` without a body, decoded into `T`.
    pub fn post_empty<T: DeserializeOwned>(&self, path: &str) -> Result<T, ApiError> {
        self.request(reqwest::Method::POST, path, None)
    }

    fn request<T: DeserializeOwned>(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<serde_json::Value>,
    ) -> Result<T, ApiError> {
        let url = format!("{}{}", self.base, path);
        let mut request = HTTP
            .request(method, &url)
            .bearer_auth(self.key.clone())
            .header(reqwest::header::ACCEPT, "application/json");

        if let Some(body) = body {
            request = request.json(&body);
        }

        let response = request
            .send()
            .map_err(|error| ApiError::transport(error.to_string()))?;
        let status = response.status().as_u16();
        let text = response
            .text()
            .map_err(|error| ApiError::transport(error.to_string()))?;

        if !(200..300).contains(&status) {
            return Err(ApiError {
                status,
                message: error_message(&text).unwrap_or_else(|| {
                    if text.trim().is_empty() {
                        format!("HTTP {status}")
                    } else {
                        text.chars().take(240).collect()
                    }
                }),
            });
        }

        serde_json::from_str::<T>(&text).map_err(|error| ApiError {
            status,
            message: format!("unexpected response: {error}"),
        })
    }
}

/// Pull the gateway's `error` field out of a failed response body.
fn error_message(body: &str) -> Option<String> {
    let value = serde_json::from_str::<serde_json::Value>(body).ok()?;
    value
        .get("error")
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
        .filter(|message| !message.trim().is_empty())
}

/// The process-wide session: which gateway we talk to and which operator key
/// we present.
///
/// Views read the client from this global instead of caching it, so a key
/// change (or a sign-out) takes effect on the very next request.
pub struct Session {
    base: String,
    key: Option<String>,
}

impl Session {
    /// A session pointing at `base` with no key yet.
    pub fn new(base: impl Into<String>) -> Self {
        Self {
            base: base.into().trim_end_matches('/').to_string(),
            key: None,
        }
    }

    /// The gateway base URL.
    pub fn base(&self) -> &str {
        &self.base
    }

    /// The operator key, when the operator has signed in.
    pub fn key(&self) -> Option<&str> {
        self.key.as_deref()
    }

    /// `true` when a key is present.
    pub fn is_authenticated(&self) -> bool {
        self.key.is_some()
    }

    /// Remember an operator key.
    pub fn sign_in(&mut self, key: impl Into<String>) {
        let key = key.into().trim().to_string();
        if !key.is_empty() {
            self.key = Some(key);
        }
    }

    /// Forget the operator key.
    pub fn sign_out(&mut self) {
        self.key = None;
    }

    /// A client for the current session, or `None` when nobody signed in.
    pub fn client(&self) -> Option<ApiClient> {
        self.key
            .as_ref()
            .map(|key| ApiClient::new(self.base.clone(), key.clone()))
    }
}

impl Global for Session {}

/// The client of the current session, or `None` when nobody signed in.
///
/// Views read the client through this helper instead of caching one: a key
/// change or a sign-out then takes effect on the very next request.
pub fn session_client<T>(cx: &mut gpui::Context<'_, T>) -> Option<ApiClient> {
    app_mut(cx).global::<Session>().client()
}

/// The `&mut App` behind a view context.
///
/// `Context` implements `BorrowMut<App>`, but `&mut Context` also implements
/// `BorrowMut<Context>`, so the plain `borrow_mut()` call is ambiguous — this
/// names the impl explicitly.
pub fn app_mut<'a, T>(cx: &'a mut gpui::Context<'_, T>) -> &'a mut gpui::App {
    <gpui::Context<'_, T> as std::borrow::BorrowMut<gpui::App>>::borrow_mut(cx)
}

/// Run a blocking request on a worker thread and deliver its outcome to the
/// view on the UI thread.
///
/// The gateway is a local process, so the request is cheap; doing it inline
/// would stall the frame, and gpui's foreground executor has no HTTP client of
/// its own worth using here.
pub fn spawn_request<T, V, F>(
    cx: &mut gpui::Context<V>,
    work: F,
    done: impl FnOnce(Result<T, ApiError>, &mut V, &mut gpui::Context<V>) + 'static,
) where
    T: Send + 'static,
    V: 'static,
    F: FnOnce() -> Result<T, ApiError> + Send + 'static,
{
    let (tx, rx) = async_channel::bounded(1);
    std::thread::spawn(move || {
        let _ = tx.send_blocking(work());
    });

    cx.spawn(async move |this, cx| {
        if let Ok(result) = rx.recv().await {
            let _ = this.update(cx, |view, cx| done(result, view, cx));
        }
    })
    .detach();
}

/// `GET /healthz` — gateway identity, no authentication required.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct Health {
    /// `ok` when the gateway is serving.
    pub status: String,
    /// Backend name the gateway runs.
    #[serde(default)]
    pub backend: String,
}

/// `GET /api/overview` — gateway identity, live backends and row counts.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct Overview {
    /// Always `ok` when the request was authorized.
    pub status: String,
    /// Backend name the gateway runs.
    pub backend: String,
    /// Every backend the gateway knows about.
    #[serde(default)]
    pub backends: Vec<String>,
    /// Row counts of the dashboard entities.
    pub counts: OverviewCounts,
}

/// Row counts shown on the overview cards.
#[derive(Debug, Clone, Default, serde::Deserialize)]
pub struct OverviewCounts {
    /// Active combos.
    #[serde(default)]
    pub combos: usize,
    /// Active provider connections.
    #[serde(default)]
    pub connections_active: usize,
    /// All provider connections.
    #[serde(default)]
    pub connections_total: usize,
    /// Active API keys.
    #[serde(default)]
    pub api_keys: usize,
}

/// `GET /api/combos` — every combo with each entry's dispatch verdict.
#[derive(Debug, Clone, Default, serde::Deserialize)]
pub struct CombosResponse {
    /// The combos, in sort order.
    #[serde(default)]
    pub combos: Vec<Combo>,
}

/// One combo.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct Combo {
    /// Stable combo id.
    pub id: String,
    /// Display name.
    pub name: String,
    /// `priority` or `auto`.
    #[serde(default)]
    pub strategy: String,
    /// Ordering weight.
    #[serde(default)]
    pub sort_order: i64,
    /// Hidden combos are not offered to clients.
    #[serde(default)]
    pub is_hidden: bool,
    /// Whether the combo is active.
    #[serde(default)]
    pub active: bool,
    /// How many entries the gateway can actually dispatch to.
    #[serde(default)]
    pub servable_entries: usize,
    /// How many entries the combo has in total.
    #[serde(default)]
    pub total_entries: usize,
    /// The entries themselves.
    #[serde(default)]
    pub entries: Vec<ComboEntry>,
}

/// One model entry of a combo.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct ComboEntry {
    /// Entry id.
    #[serde(default)]
    pub id: String,
    /// Provider id the entry dispatches to.
    pub provider: String,
    /// Bare model id.
    pub model: String,
    /// Routing weight.
    #[serde(default)]
    pub weight: i64,
    /// Backend the router would use, when it found one.
    #[serde(default)]
    pub backend: Option<String>,
    /// Whether the gateway can serve this entry at all.
    #[serde(default)]
    pub servable: bool,
}

/// `GET /api/connections` — providers and their connections, secrets masked.
#[derive(Debug, Clone, Default, serde::Deserialize)]
pub struct ConnectionsResponse {
    /// The connections.
    #[serde(default)]
    pub connections: Vec<Connection>,
    /// Set when the connections could not be read at all.
    #[serde(default)]
    pub connection_error: Option<String>,
}

/// Read a value the gateway may send as `null` as its default.
///
/// The gateway serializes nullable database columns as they are, so a `NULL`
/// column arrives as `null` and not as an empty string — `provider_connections`
/// has nullable `name` and `auth_type`, `api_keys` a nullable `key_prefix`.
/// `#[serde(default)]` only covers a *missing* field, so without this helper a
/// single null column fails the whole response. The web dashboard guards the
/// same fields with `?? "—"`; the desktop client turns them into the default
/// and lets the view render its own placeholder.
fn null_to_default<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de> + Default,
{
    Ok(Option::<T>::deserialize(deserializer)?.unwrap_or_default())
}

/// One provider connection.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct Connection {
    /// Stable connection id.
    pub id: String,
    /// Provider id.
    pub provider: String,
    /// Display name.
    #[serde(default, deserialize_with = "null_to_default")]
    pub name: String,
    /// How the credential is obtained (`api_key`, `none`, …).
    #[serde(default, deserialize_with = "null_to_default")]
    pub auth_type: String,
    /// Whether the connection is active.
    #[serde(default)]
    pub is_active: bool,
    /// Dispatch priority.
    #[serde(default)]
    pub priority: i64,
    /// Credential expiry, when it has one.
    #[serde(default)]
    pub expires_at: Option<String>,
    /// Result of the last credential test.
    #[serde(default)]
    pub test_status: Option<String>,
    /// Last error the gateway recorded for this connection.
    #[serde(default)]
    pub last_error: Option<String>,
    /// Whether a credential is stored — never the credential itself.
    #[serde(default)]
    pub has_credential: bool,
    /// Whether the gateway knows this provider.
    #[serde(default)]
    pub servable: bool,
}

/// `GET /api/keys` — every API key, secrets masked to their prefix.
#[derive(Debug, Clone, Default, serde::Deserialize)]
pub struct KeysResponse {
    /// The keys.
    #[serde(default)]
    pub keys: Vec<ApiKey>,
    /// Set when the keys could not be read at all.
    #[serde(default)]
    pub error: Option<String>,
}

/// One API key row. Carries metadata only — never the secret.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct ApiKey {
    /// Stable key id.
    pub id: String,
    /// Display name.
    #[serde(default)]
    pub name: String,
    /// Display prefix of the key.
    #[serde(default, deserialize_with = "null_to_default")]
    pub prefix: String,
    /// Whether the stored key is encrypted at rest.
    #[serde(default)]
    pub encrypted: bool,
    /// Creation timestamp.
    #[serde(default)]
    pub created_at: Option<String>,
    /// Revocation timestamp.
    #[serde(default)]
    pub revoked_at: Option<String>,
    /// Expiry timestamp.
    #[serde(default)]
    pub expires_at: Option<String>,
    /// Last use timestamp.
    #[serde(default)]
    pub last_used_at: Option<String>,
    /// Whether the key may still authenticate.
    #[serde(default)]
    pub is_active: bool,
    /// Convenience mirror of `revoked_at`.
    #[serde(default)]
    pub revoked: bool,
}

/// `POST /api/keys` — the one and only response that carries a secret.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct CreatedKey {
    /// `ok`.
    pub status: String,
    /// Id of the new key.
    pub id: String,
    /// Name of the new key.
    #[serde(default)]
    pub name: String,
    /// The secret — shown once, never stored by the gateway.
    pub key: String,
    /// Display prefix of the secret.
    #[serde(default)]
    pub prefix: String,
    /// Warning the gateway attaches to the response.
    #[serde(default)]
    pub warning: String,
}

/// `GET /api/usage` — totals, latency percentiles and the top buckets.
#[derive(Debug, Clone, Default, serde::Deserialize)]
pub struct Usage {
    /// Size of the reporting window in days.
    #[serde(default)]
    pub window_days: i64,
    /// Total requests.
    #[serde(default)]
    pub requests: i64,
    /// Successful requests.
    #[serde(default)]
    pub successes: i64,
    /// Input tokens.
    #[serde(default)]
    pub tokens_input: i64,
    /// Output tokens.
    #[serde(default)]
    pub tokens_output: i64,
    /// Tokens served from the prompt cache.
    #[serde(default)]
    pub tokens_cache_read: i64,
    /// Latency percentiles.
    #[serde(default)]
    pub latency: Latency,
    /// Buckets per provider.
    #[serde(default)]
    pub by_provider: Vec<UsageBucket>,
    /// Buckets per model.
    #[serde(default)]
    pub by_model: Vec<UsageBucket>,
}

/// Latency percentiles of the reporting window.
#[derive(Debug, Clone, Default, serde::Deserialize)]
pub struct Latency {
    /// Median latency in milliseconds.
    #[serde(default)]
    pub p50_ms: Option<f64>,
    /// 95th percentile latency in milliseconds.
    #[serde(default)]
    pub p95_ms: Option<f64>,
    /// 99th percentile latency in milliseconds.
    #[serde(default)]
    pub p99_ms: Option<f64>,
}

/// One usage bucket — a provider or a model.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct UsageBucket {
    /// Provider id or model id.
    #[serde(default, deserialize_with = "null_to_default")]
    pub key: String,
    /// Requests in this bucket.
    #[serde(default)]
    pub requests: i64,
    /// Input tokens in this bucket.
    #[serde(default)]
    pub tokens_input: i64,
    /// Output tokens in this bucket.
    #[serde(default)]
    pub tokens_output: i64,
}

/// `GET /api/logs` — recent calls, newest first.
#[derive(Debug, Clone, Default, serde::Deserialize)]
pub struct LogsResponse {
    /// The call log rows.
    #[serde(default)]
    pub logs: Vec<CallLog>,
}

/// One call log row.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct CallLog {
    /// Timestamp of the call.
    #[serde(default)]
    pub timestamp: Option<String>,
    /// Model the gateway served.
    #[serde(default)]
    pub model: Option<String>,
    /// Model the client asked for.
    #[serde(default)]
    pub requested_model: Option<String>,
    /// Provider that served the call.
    #[serde(default)]
    pub provider: Option<String>,
    /// Upstream status.
    #[serde(default)]
    pub status: Option<String>,
    /// Duration in milliseconds.
    #[serde(default)]
    pub duration_ms: Option<i64>,
    /// Input tokens.
    #[serde(default)]
    pub tokens_input: Option<i64>,
    /// Output tokens.
    #[serde(default)]
    pub tokens_output: Option<i64>,
    /// Combo the call was routed through.
    #[serde(default)]
    pub combo_name: Option<String>,
    /// Error summary, when the call failed.
    #[serde(default)]
    pub error_summary: Option<String>,
}

/// `GET /api/logs/app` — recent application log lines.
#[derive(Debug, Clone, Default, serde::Deserialize)]
pub struct AppLogsResponse {
    /// Log lines, oldest first.
    #[serde(default)]
    pub lines: Vec<String>,
}

/// Body of `POST /api/keys`.
#[derive(Debug, Clone, Serialize)]
pub struct CreateKeyBody {
    /// Name of the new key.
    pub name: String,
}

/// Body of `PATCH /api/combos/{id}` — every field is optional.
#[derive(Debug, Clone, Default, Serialize)]
pub struct UpdateComboBody {
    /// New display name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// New ordering weight.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sort_order: Option<i64>,
    /// Whether the combo is hidden from clients.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_hidden: Option<bool>,
    /// Whether the combo is active.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_active: Option<bool>,
    /// `priority` or `auto`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub strategy: Option<String>,
    /// The full entry list — sent when the entries change.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub models: Option<Vec<ComboModelSpec>>,
}

/// One entry of a combo rewrite.
#[derive(Debug, Clone, Default, Serialize)]
pub struct ComboModelSpec {
    /// Entry id — the dashboard round-trips it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// Provider id.
    pub provider: String,
    /// Bare model id.
    pub model: String,
    /// Routing weight; the gateway defaults it to 50.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub weight: Option<i64>,
}

/// Body of `PATCH /api/connections/{id}` — every field is optional.
#[derive(Debug, Clone, Default, Serialize)]
pub struct UpdateConnectionBody {
    /// New display name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Whether the connection is active.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_active: Option<bool>,
    /// New dispatch priority.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub priority: Option<i64>,
}

/// `POST /api/keys/{id}/revoke` and `.../restore` — the same response shape.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct KeyStateResponse {
    /// `ok`.
    pub status: String,
    /// The key id.
    pub id: String,
    /// Whether the key is revoked now.
    #[serde(default)]
    pub revoked: bool,
    /// Revocation timestamp.
    #[serde(default)]
    pub revoked_at: Option<String>,
}

/// `POST /api/restart` — restarts the supervised gateway.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct RestartResponse {
    /// `restarting`.
    pub status: String,
    /// The launchd label that was restarted.
    #[serde(default)]
    pub label: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_message_prefers_the_gateway_error_field() {
        assert_eq!(
            error_message(r#"{"error":"name must not be empty"}"#).as_deref(),
            Some("name must not be empty")
        );
        assert_eq!(error_message("not json"), None);
        assert_eq!(error_message(r#"{"error":""}"#), None);
    }

    #[test]
    fn overview_deserializes_the_gateway_payload() {
        let payload = r#"{
            "status": "ok",
            "backend": "sqlite",
            "backends": ["sqlite", "postgres"],
            "counts": {
                "combos": 2,
                "connections_active": 1,
                "connections_total": 3,
                "api_keys": 4
            }
        }"#;
        let overview: Overview = serde_json::from_str(payload).expect("overview");
        assert_eq!(overview.counts.combos, 2);
        assert_eq!(overview.counts.connections_total, 3);
        assert_eq!(overview.backends.len(), 2);
    }

    #[test]
    fn nullable_log_fields_stay_optional() {
        let payload = r#"{
            "timestamp": "2026-10-28 12:00:00",
            "model": null,
            "duration_ms": null,
            "tokens_input": 12
        }"#;
        let row: CallLog = serde_json::from_str(payload).expect("log row");
        assert!(row.model.is_none());
        assert!(row.duration_ms.is_none());
        assert_eq!(row.tokens_input, Some(12));
    }

    #[test]
    fn empty_responses_do_not_fail_to_parse() {
        let combos: CombosResponse = serde_json::from_str(r#"{"combos":[]}"#).expect("combos");
        assert!(combos.combos.is_empty());
        let logs: LogsResponse = serde_json::from_str(r#"{"logs":[]}"#).expect("logs");
        assert!(logs.logs.is_empty());
        let usage: Usage = serde_json::from_str(r#"{"requests":0}"#).expect("usage");
        assert_eq!(usage.requests, 0);
        assert!(usage.latency.p50_ms.is_none());
    }

    #[test]
    fn patch_bodies_omit_absent_fields() {
        let body = UpdateComboBody {
            name: Some("renamed".into()),
            ..Default::default()
        };
        let json = serde_json::to_value(&body).expect("body");
        assert_eq!(json.get("name").and_then(|v| v.as_str()), Some("renamed"));
        assert!(json.get("strategy").is_none());
        assert!(json.get("models").is_none());
    }

    #[test]
    fn session_signs_in_and_out() {
        let mut session = Session::new("http://127.0.0.1:20128/");
        assert!(!session.is_authenticated());
        assert!(session.client().is_none());
        session.sign_in("  sk-test  ");
        assert!(session.is_authenticated());
        assert_eq!(session.base(), "http://127.0.0.1:20128");
        let client = session.client().expect("client");
        assert_eq!(client.base(), "http://127.0.0.1:20128");
        session.sign_out();
        assert!(!session.is_authenticated());
    }
}

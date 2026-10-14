//! Axum HTTP surface for the Rust gateway.
//!
//! Implements the OpenAI-compatible endpoints the rest of Heterion Router (and any
//! OpenAI SDK client) talks to. Streaming is emitted as SSE in Chat
//! Completions shape, so an `openai-responses` upstream just needs a
//! translation layer in front of [`backend::ChatBackend`] later.

pub mod admin;
pub mod apikeys;
pub mod auth;
pub mod backend;
pub mod budgets;
pub mod cache;
pub mod claude;
pub mod claude_endpoint;
pub mod combos;
pub mod cors;
pub mod credentials;
pub mod embeddings;
pub mod gemini;
pub mod grok_cli;
pub mod guardrails;
pub mod ids;
pub mod openai;
pub mod reasoning;
pub mod responses;
pub mod restart;
pub mod routing;
pub mod ui;

use std::convert::Infallible;
use std::sync::Arc;

use auth::Authenticated;
use axum::{
    Json, Router,
    body::Bytes,
    extract::{FromRequest, Path, State},
    http::{HeaderMap, HeaderName, HeaderValue, StatusCode},
    response::{
        IntoResponse, Response,
        sse::{Event, KeepAlive, Sse},
    },
    routing::{get, patch, post, put},
};
use futures::StreamExt;
use heterion_router_core::{
    ChatCompletionRequest, ChatCompletionResponse, ChatMessage, GatewayError, StreamChunk,
};
use heterion_router_db::Db;
use heterion_router_providers::ProviderRegistry;
use heterion_router_usage::{
    CachedUsage, CallLogRecord, PriceTable, UsageRecord, record_call_log, record_usage,
};
use serde::de::DeserializeOwned;
use serde_json::{Map, Value, json};

use backend::ChatBackend;

/// The gateway's own `Json` extractor: a body that will not parse, or does
/// not fit the endpoint, is answered with the project envelope instead of
/// axum's `text/plain` sentence.
///
/// `axum::Json` cannot be re-shaped from the outside, and `HandleErrorLayer`
/// is not the answer. The framework answers a `JsonRejection` itself, inside
/// the handler wrapper, *before* the handler body runs
/// (`axum-0.8/src/handler/mod.rs`: `Err(rejection) =>
/// return rejection.into_response()`), so the rejection never becomes a
/// service `Error`. That is also visible from the type: a `Router`'s error type
/// is `Infallible`, so the only handler `HandleErrorLayer` will accept over it
/// takes an `Infallible` — an arm that can never run. Deserializing here is
/// the one place that can, and it puts every malformed-body answer in the same
/// sanitized shape as every other error.
///
/// `serde_json`'s own message is deliberately dropped rather than reported:
/// for a wrong type it quotes the offending value back
/// (`invalid type: string "sk-live-…"`) and no request body may reach the
/// client.
///
/// The admin handlers take their bodies the same way; swap their `Json<T>` for
/// this and nothing about them changes but the shape of a malformed answer.
pub struct JsonBody<T>(pub T);

impl<T, S> FromRequest<S> for JsonBody<T>
where
    T: DeserializeOwned,
    S: Send + Sync,
{
    type Rejection = Response;

    async fn from_request(
        request: axum::http::Request<axum::body::Body>,
        state: &S,
    ) -> Result<Self, Self::Rejection> {
        let bytes = Bytes::from_request(request, state).await.map_err(|_| {
            json_rejection(StatusCode::BAD_REQUEST, "request body could not be read")
        })?;
        // The 400/422 split is axum's own (`axum-core/src/json.rs`): a body
        // that is not JSON at all is the client's syntax error, one that does
        // not fit the endpoint is ours to call unprocessable. A body that stops
        // mid-token is `Eof`, which axum counts as syntax too.
        match serde_json::from_slice::<T>(&bytes) {
            Ok(value) => Ok(JsonBody(value)),
            Err(error) if error.is_syntax() || error.is_eof() => Err(json_rejection(
                StatusCode::BAD_REQUEST,
                "request body is not valid JSON",
            )),
            Err(_) => Err(json_rejection(
                StatusCode::UNPROCESSABLE_ENTITY,
                "request body does not match this endpoint",
            )),
        }
    }
}

/// The project error envelope for a request that never reached a handler.
fn json_rejection(status: StatusCode, message: &str) -> Response {
    (
        status,
        Json(json!({
            "error": {
                "message": message,
                "type": "invalid_request_error",
                "code": "invalid_request",
            }
        })),
    )
        .into_response()
}

/// Cache and reasoning tokens the upstream reported in its own payload.
///
/// `core::Usage` only carries the three totals, so the discounted detail
/// is read back off the raw response: a cache hit the provider never
/// republishes in the raw payload bills as fresh input. A backend that
/// hands over no payload reports nothing, which is the old behaviour.
fn cached_usage(response: &ChatCompletionResponse) -> CachedUsage {
    match response.raw {
        Some(ref raw) => CachedUsage::from_payload(raw),
        None => CachedUsage::default(),
    }
}

/// Milliseconds since `started`, clamped to what the column can hold.
fn millis_since(started: std::time::Instant) -> u32 {
    started.elapsed().as_millis().min(u128::from(u32::MAX)) as u32
}

/// Cost and attribution headers for a proxied response (M1.5).
///
/// **Mechanism: set where the response is built**, in the two non-streaming
/// success arms, rather than in a response middleware — a middleware would
/// have to rediscover the price, the serving provider and the resolved model
/// out of `AppState` (the stream arms do not even know them until the body is
/// pumped), and it could not tell a proxied completion from any other 200 it
/// passed on the way out.
///
/// Precision: a plain USD decimal with **six places**. That is the `{:.6}`
/// rounding the pre-M1.5 `x-heterion-router-cost-usd` header used and the one
/// `PriceTable::cost_usd` documents as its only rounding step, so the number an
/// operator reads does not change when the name does.
///
/// An unpriced model gets **no** `x-router-cost`: an unknown cost is reported
/// as unknown, never as `0.000000`. Roadmap M3 will report a served-from-cache
/// call as `x-router-cache: hit` together with `x-router-cost: 0.000000` —
/// same name, same precision, so the two cases are told apart by the marker
/// rather than by guessing at the number.
///
/// Only values the gateway itself produced are put in a header: no upstream
/// body text and no credential ever passes through here.
fn router_headers(
    prices: &PriceTable,
    attribution: &backend::Attribution,
    model: &str,
    served: &str,
    usage: &heterion_router_core::Usage,
    details: CachedUsage,
) -> HeaderMap {
    let mut headers = HeaderMap::new();
    if let Some(cost) = prices.cost_usd(
        model,
        usage.prompt_tokens,
        usage.completion_tokens,
        details.cache_read,
        details.reasoning,
        details.cache_creation,
    ) && let Ok(value) = HeaderValue::from_str(&format!("{cost:.6}"))
    {
        headers.insert(HeaderName::from_static(HEADER_COST), value);
    }
    if let Some(provider) = attribution.provider.as_deref()
        && let Ok(value) = HeaderValue::from_str(provider)
    {
        headers.insert(HeaderName::from_static(HEADER_PROVIDER), value);
    }
    if let Ok(value) = HeaderValue::from_str(served) {
        headers.insert(HeaderName::from_static(HEADER_MODEL_USED), value);
    }
    headers
}

/// What the proxied call cost. See [`router_headers`] for the format and why
/// an unpriced model has none.
const HEADER_COST: &str = "x-router-cost";
/// Provider that actually answered, as `backend::Attribution` reports it.
const HEADER_PROVIDER: &str = "x-router-provider";
/// Model that answered, which differs from the requested one behind a combo
/// or an alias.
const HEADER_MODEL_USED: &str = "x-router-model-used";

/// Shared gateway state.
#[derive(Clone)]
pub struct AppState {
    pub(crate) backend: Arc<dyn ChatBackend>,
    pub(crate) db: Option<Arc<Db>>,
    pub(crate) require_auth: bool,
    pub(crate) prices: PriceTable,
    pub(crate) registry: Option<Arc<ProviderRegistry>>,
    pub(crate) backend_names: Vec<String>,
    pub(crate) data_dir: Option<std::path::PathBuf>,
    /// Built dashboard bundle, when one is present.
    pub(crate) ui_dir: Option<std::path::PathBuf>,
    /// Unix time the process built this state: lets the dashboard tell a
    /// restarted gateway apart from the one it just asked to restart.
    pub(crate) started_unix: u64,
}

/// Seconds since the unix epoch, or 0 when the clock is unavailable.
fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0)
}

impl AppState {
    /// Build state around a backend, without persistence or auth.
    pub fn new(backend: Arc<dyn ChatBackend>) -> Self {
        Self {
            backend,
            db: None,
            require_auth: false,
            prices: PriceTable::with_defaults(),
            registry: None,
            backend_names: Vec::new(),
            data_dir: None,
            ui_dir: None,
            started_unix: now_unix(),
        }
    }

    /// Build state backed by a database (enables auth + usage recording).
    pub fn with_db(backend: Arc<dyn ChatBackend>, db: Arc<Db>) -> Self {
        Self {
            backend,
            db: Some(db),
            require_auth: false,
            prices: PriceTable::with_defaults(),
            registry: None,
            backend_names: Vec::new(),
            data_dir: None,
            ui_dir: None,
            started_unix: now_unix(),
        }
    }

    /// Attach the provider registry and the backend names left after
    /// selector rules were applied (`echo` counts as a backend).
    pub fn with_catalog(
        mut self,
        registry: Arc<ProviderRegistry>,
        backend_names: Vec<String>,
    ) -> Self {
        self.registry = Some(registry);
        self.backend_names = backend_names;
        self
    }

    /// Serve the dashboard bundle from `dir`.
    pub fn with_ui_dir(mut self, dir: Option<std::path::PathBuf>) -> Self {
        self.ui_dir = dir;
        self
    }

    /// Set the data directory (used for log file paths).
    pub fn with_data_dir(mut self, dir: std::path::PathBuf) -> Self {
        self.data_dir = Some(dir);
        self
    }

    /// Force API-key auth even when the `api_keys` table is empty.
    pub fn with_require_auth(mut self, require: bool) -> Self {
        self.require_auth = require;
        self
    }
}

/// Build the router for a backend (no persistence, auth open).
pub fn build_router(backend: Arc<dyn ChatBackend>) -> Router {
    build_router_with_state(AppState::new(backend))
}

/// Build the router for full state. `/healthz` stays public; `/v1/*`
/// handlers each take the [`Authenticated`] extractor.
/// The router as production builds it: exact-match cache, per-key budgets and
/// guardrails, CORS outermost.
pub fn build_router_with_state(state: AppState) -> Router {
    build_router_with(state.clone(), cache::CacheLayer::new(&state))
}

/// Same router, with a caller-supplied cache layer.
///
/// Exists so a test can exercise a configured cache (short TTL, semantic mode)
/// through the real route table instead of layering a second cache on top of a
/// router that already has one — which would run the cache twice per request.
pub fn build_router_with(state: AppState, cache_layer: cache::CacheLayer) -> Router {
    let _ = &state;
    Router::new()
        .route("/healthz", get(healthz))
        .route("/api/overview", get(admin::overview))
        .route("/api/combos", get(admin::combos))
        .route("/api/combos/{id}", patch(admin::update_combo))
        .route("/api/connections", get(admin::connections))
        .route("/api/connections/{id}", patch(admin::update_connection))
        .route("/api/usage", get(admin::usage))
        .route("/api/logs", get(admin::logs))
        .route("/api/logs/app", get(admin::logs_app))
        .route("/api/keys", get(admin::keys).post(admin::create_key))
        .route("/api/keys/{id}/revoke", post(admin::revoke_key))
        .route("/api/keys/{id}/restore", post(admin::restore_key))
        .route("/api/restart", post(admin::restart_service))
        .route("/api/budgets", get(budgets::list))
        .route(
            "/api/budgets/{api_key_id}",
            put(budgets::upsert).delete(budgets::remove),
        )
        .route(
            "/api/guardrails",
            get(guardrails::settings).patch(guardrails::update_settings),
        )
        .route("/", get(ui::serve))
        .route("/{*path}", get(ui::serve))
        .route("/v1/models", get(list_models))
        .route("/v1/chat/completions", post(chat_completions))
        .route("/v1/embeddings", post(embeddings::embeddings))
        .route("/v1/responses", post(responses))
        .route(
            "/v1/responses/{id}",
            get(get_response).delete(delete_response),
        )
        .route("/v1/messages", post(claude_endpoint::messages))
        .route(
            "/v1/messages/count_tokens",
            post(claude_endpoint::count_tokens),
        )
        .layer(axum::middleware::from_fn_with_state(
            cache_layer,
            cache::dispatch,
        ))
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            budgets::enforce,
        ))
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            guardrails::middleware,
        ))
        .layer(axum::middleware::from_fn(cors::cors))
        .with_state(state)
}

async fn healthz(State(state): State<AppState>) -> impl IntoResponse {
    Json(json!({
        "status": "ok",
        "backend": state.backend.name(),
        "started_unix": state.started_unix,
    }))
}

async fn list_models(State(state): State<AppState>, _auth: Authenticated) -> impl IntoResponse {
    let mut data: Vec<Value> = Vec::new();

    // Combo names are first-class models for clients.
    if let Some(db) = state.db.as_ref() {
        let guard = db.connection();
        if let Ok(combos) = heterion_router_db::repos::list_active_combos(&guard) {
            for combo in combos {
                let hidden = serde_json::from_str::<Value>(&combo.data)
                    .ok()
                    .and_then(|value| value.get("isHidden")?.as_bool())
                    .unwrap_or(false);
                if hidden {
                    continue;
                }
                data.push(json!({
                    "id": combo.name,
                    "object": "model",
                    "owned_by": "heterion-router",
                    "heterion-router": { "kind": "combo" },
                }));
            }
        }
    }

    // `provider/model` ids for every provider that has a live backend.
    if let Some(registry) = state.registry.as_ref() {
        for entry in registry.iter() {
            if !state.backend_names.iter().any(|name| name == &entry.id) {
                continue;
            }
            for model in &entry.models {
                data.push(json!({
                    "id": format!("{}/{}", entry.id, model.id),
                    "object": "model",
                    "owned_by": entry.id,
                    "heterion-router": { "kind": "provider", "provider": entry.id },
                }));
            }
        }
    }

    Json(json!({ "object": "list", "data": data }))
}
async fn chat_completions(
    State(state): State<AppState>,
    auth: Authenticated,
    JsonBody(mut request): JsonBody<ChatCompletionRequest>,
) -> Response {
    if !auth.allows_model(&request.model) {
        return forbidden_response("model is not allowed for this api key");
    }
    let token = backend::stamp_attribution(&mut request);
    if request.stream {
        tracing::info!("completion model={} stream=true", request.model);
        return stream_response(state, auth, request).into_response();
    }

    let model = request.model.clone();
    let summary = request_summary(&request);
    let started = std::time::Instant::now();
    tracing::info!("completion model={model} stream=false");
    match state.backend.complete(request).await {
        Ok(response) => {
            let latency_ms = millis_since(started);
            tracing::info!(
                "completion ok model={model} resolved={} finish={} pt={} ct={} ms={latency_ms}",
                response.model,
                response.choices[0].finish_reason,
                response.usage.prompt_tokens,
                response.usage.completion_tokens,
            );
            let served = response.model.clone();
            let usage = response.usage;
            let details = cached_usage(&response);
            record_completion_usage(
                &state,
                &auth,
                CompletionUsage {
                    model: &model,
                    resolved: Some(&served),
                    token: &token,
                    usage: &usage,
                    details,
                    success: true,
                    call_status: 200,
                    latency_ms,
                    ttft_ms: 0,
                    error_code: None,
                    request_type: "chat",
                    summary: &summary,
                },
            );

            let mut http = Json(response).into_response();
            http.headers_mut().extend(router_headers(
                &state.prices,
                &state.backend.attribution(&token),
                &model,
                &served,
                &usage,
                details,
            ));
            http
        }
        Err(error) => {
            // Normalized here as well as inside `StatusAwareBackend`: an
            // executor that has not been wrapped yet still owes the client
            // the status its payload carried.
            let error = with_recovered_status(error);
            let latency_ms = millis_since(started);
            tracing::warn!("completion failed model={model}: {error}");
            record_completion_usage(
                &state,
                &auth,
                CompletionUsage {
                    model: &model,
                    resolved: None,
                    token: &token,
                    usage: &heterion_router_core::Usage::default(),
                    // An upstream that failed reported no tokens at all.
                    details: CachedUsage::default(),
                    success: false,
                    call_status: i64::from(error_status(&error).as_u16()),
                    latency_ms,
                    ttft_ms: 0,
                    error_code: Some("upstream_error"),
                    request_type: "chat",
                    summary: &summary,
                },
            );
            error_response(error)
        }
    }
}

/// One bounded line describing what was asked for.
///
/// Counts only, by construction: an agent turn can carry a megabyte of
/// context and the dashboard renders this column as a list, so a summary made
/// of content would be both a payload leak and unreadable.
fn request_summary(request: &ChatCompletionRequest) -> String {
    // `tools` is the raw array the client sent, so it is counted as whatever
    // shape it arrived in: the number is a hint for a human reader, and a
    // malformed array must not fail the request being logged.
    let tools = match request.tools.as_ref().and_then(Value::as_array) {
        Some(tools) => tools.len(),
        None if request.tools.as_ref().is_some_and(|tools| !tools.is_null()) => 1,
        None => 0,
    };
    format!("{} messages, {tools} tools", request.messages.len())
}

/// Wire format the client spoke, taken from the router verb it arrived on.
///
/// The vocabulary is the registry's own `format` value for that surface
/// (`registry.json` spells them `openai`, `openai-responses`, `claude`,
/// `gemini`), so `source_format` and `target_format` in one call log read the
/// same way.
fn source_format(request_type: &str) -> &'static str {
    match request_type {
        "responses" => "openai-responses",
        _ => "openai",
    }
}

/// Wire format the backend spoke, read out of the provider registry.
///
/// [`ProviderRegistry::model_target_format`] already resolves a model's own
/// `target_format` against its provider default, so this is a lookup and
/// never a second opinion. A provider or model the registry does not know
/// declares no format and stays NULL.
fn target_format(
    state: &AppState,
    attribution: &backend::Attribution,
    model: &str,
) -> Option<String> {
    let registry = state.registry.as_ref()?;
    let provider = attribution.provider.as_deref()?;
    registry
        .model_target_format(provider, model)
        .map(str::to_string)
}

struct CompletionUsage<'a> {
    /// Model the client asked for.
    model: &'a str,
    /// Model that actually served the request, when the backend reports one.
    resolved: Option<&'a str>,
    /// Attribution token tying these rows to the provider that served them.
    token: &'a str,
    usage: &'a heterion_router_core::Usage,
    /// Cache and reasoning counts read off the upstream payload.
    details: CachedUsage,
    success: bool,
    /// HTTP status stored on the call-log row.
    call_status: i64,
    latency_ms: u32,
    /// Time to the first upstream token. Zero on a non-streaming call, which
    /// has no first token to measure.
    ttft_ms: u32,
    error_code: Option<&'a str>,
    request_type: &'a str,
    /// Counts-only description of the request; never its content.
    summary: &'a str,
}

/// Call-log status for a stream the client abandoned: HTTP 499, the
/// conventional "client closed request".
const CLIENT_CLOSED_REQUEST: i64 = 499;

fn record_completion_usage(state: &AppState, auth: &Authenticated, context: CompletionUsage<'_>) {
    if auth.key.as_ref().is_some_and(|key| key.no_log) {
        return;
    }
    let CompletionUsage {
        model,
        resolved,
        token,
        usage,
        details,
        success,
        call_status,
        latency_ms,
        ttft_ms,
        error_code,
        request_type,
        summary,
    } = context;
    let Some(db) = state.db.as_ref() else {
        return;
    };
    // The backend that answered, not the router that picked it: usage is
    // per-provider analytics.
    let attribution = state.backend.attribution(token);
    let served = resolved.unwrap_or(model);
    let guard = db.connection();
    let record = UsageRecord {
        provider: attribution.provider.clone(),
        model: Some(served.to_string()),
        connection_id: attribution.connection_id.clone(),
        api_key_id: auth.key.as_ref().map(|key| key.id.clone()),
        api_key_name: auth.key.as_ref().map(|key| key.name.clone()),
        tokens_input: usage.prompt_tokens,
        tokens_output: usage.completion_tokens,
        tokens_cache_read: details.cache_read,
        tokens_cache_creation: details.cache_creation,
        tokens_reasoning: details.reasoning,
        // SHARED NOTE (M1.1) — the columns below are left at NULL / 0 / false
        // because no feature in this build can produce them, and an invented
        // value is worse for an operator than an honest NULL:
        //
        //  * `account_key` / `account_label` / `account_label_priority` (and
        //    `call_logs.account`): there is no accounts table and no
        //    credential → account mapping anywhere in the schema. Naming the
        //    account is the provider executor's job, and `Attribution` cannot
        //    carry it today.
        //  * `service_tier`: the column holds the tier the upstream *charged*,
        //    which only ever appears in its response body; nothing here parses
        //    it, and the tier the client asked for is not the tier billed.
        //  * `cache_source`: there is no local cache for a hit to come from
        //    (roadmap M3), so every count above came from the upstream and the
        //    schema default `'upstream'` already says so.
        //  * `detail_state` and the `artifact_*` triple: the artifact store is
        //    **not implemented**, so there is no captured body to point at.
        //    Before it is, a store needs a path outside the database, a
        //    per-key/per-account separation, redaction of `authorization` and
        //    of any api key found in a body, a size cap and a retention sweep
        //    — capturing raw prompts must never be a side effect of logging.
        //  * `has_request_body` / `has_response_body` / `has_pipeline_details`:
        //    honest zero, for the same reason — claiming a body would send a
        //    reader looking for a file that does not exist.
        //  * `tokens_compressed`: prompt compression does not exist yet.
        //  * `combo_step_id` / `combo_execution_key`: only the combo executor
        //    in `routing.rs` knows which step ran and which run it belonged
        //    to; `Attribution` carries the combo *name* only.
        account_key: None,
        account_label: None,
        account_label_priority: 0,
        service_tier: None,
        status: Some(if success { "succeeded" } else { "failed" }.to_string()),
        success,
        latency_ms,
        ttft_ms,
        error_code: error_code.map(str::to_string),
    };
    if let Err(error) = record_usage(&guard, &record) {
        tracing::warn!("usage record failed: {error}");
    }
    let id = ids::call_id();
    let call = CallLogRecord {
        id: id.clone(),
        method: "POST".to_string(),
        path: if request_type == "responses" {
            "/v1/responses".to_string()
        } else {
            "/v1/chat/completions".to_string()
        },
        status: call_status,
        model: Some(served.to_string()),
        requested_model: Some(model.to_string()),
        provider: attribution.provider.clone(),
        // See the SHARED NOTE above: no account model, no artifact store, no
        // compression, no per-step combo identity.
        account: None,
        // The connection the answering backend names for itself, mirroring
        // `usage_history.connection_id`.
        connection_id: attribution.connection_id.clone(),
        duration: latency_ms,
        tokens_in: usage.prompt_tokens,
        tokens_out: usage.completion_tokens,
        // The same discounted counts the usage row books, so the two tables
        // can never disagree about one call's bill.
        tokens_cache_read: Some(details.cache_read),
        tokens_cache_creation: Some(details.cache_creation),
        tokens_reasoning: Some(details.reasoning),
        tokens_compressed: None,
        cache_source: None,
        request_type: Some(request_type.to_string()),
        source_format: Some(source_format(request_type).to_string()),
        target_format: target_format(state, &attribution, served),
        api_key_id: auth.key.as_ref().map(|key| key.id.clone()),
        api_key_name: auth.key.as_ref().map(|key| key.name.clone()),
        // Authoritative: the row is written complete, so the follow-up
        // `UPDATE call_logs SET combo_name = ?1 WHERE id = ?2` that used to
        // come straight after `record_call_log` is gone. It existed only
        // because this literal left the field at `None`; with the name set
        // here the INSERT carries it and a second write could only overwrite
        // it.
        combo_name: attribution.combo_name.clone(),
        combo_step_id: None,
        combo_execution_key: None,
        error_summary: error_code.map(str::to_string),
        detail_state: None,
        artifact_relpath: None,
        artifact_size_bytes: None,
        artifact_sha256: None,
        has_request_body: false,
        has_response_body: false,
        has_pipeline_details: false,
        request_summary: Some(summary.to_string()),
        // The row's own id, so an operator grepping `call_logs` for a trace
        // complaint finds the row itself. No second identifier is invented:
        // it is the same `ids::call_id()` the attribution token is minted from.
        correlation_id: Some(id),
    };
    if let Err(error) = record_call_log(&guard, &call) {
        tracing::warn!("call log record failed: {error}");
    }
}

/// Books a streaming request exactly once: on the terminal chunk, on an
/// error, or on drop when the client hangs up mid-flight.
///
/// A dropped stream still burned upstream tokens and dropping the response
/// body is the only moment that sees it, so the accounting lives in `Drop`.
/// Best effort by construction: a failed write warns and never propagates.
struct StreamAccounting {
    state: AppState,
    auth: Authenticated,
    token: String,
    requested_model: String,
    usage: heterion_router_core::Usage,
    /// Discounted counts the terminal usage frame reported, if any.
    details: CachedUsage,
    success: bool,
    /// HTTP status for the call-log row of whatever outcome we end on.
    status: i64,
    started: std::time::Instant,
    /// Milliseconds to the first upstream chunk, latched once it arrives.
    /// `None` until then — and forever for a stream that produced no chunk,
    /// which books 0 rather than a made-up number.
    ttft_ms: Option<u32>,
    request_type: &'static str,
    /// Counts-only description of the request; never its content.
    summary: String,
    recorded: bool,
}

impl StreamAccounting {
    fn new(
        state: AppState,
        auth: Authenticated,
        token: String,
        requested_model: String,
        started: std::time::Instant,
        request_type: &'static str,
        summary: String,
    ) -> Self {
        Self {
            state,
            auth,
            token,
            requested_model,
            usage: heterion_router_core::Usage::default(),
            details: CachedUsage::default(),
            success: true,
            status: 200,
            started,
            ttft_ms: None,
            request_type,
            summary,
            recorded: false,
        }
    }

    /// Latch time to first token on the first chunk the pump forwards.
    ///
    /// Only the streaming pumps call this, so a non-streaming row keeps
    /// `ttft_ms = 0`: a completion has no first token, and repeating its total
    /// latency there would be a number that means something else.
    fn first_token(&mut self) {
        if self.ttft_ms.is_none() {
            self.ttft_ms = Some(millis_since(self.started));
        }
    }

    /// Record the tokens seen so far. Later calls are no-ops.
    fn record(&mut self) {
        if self.recorded {
            return;
        }
        self.recorded = true;
        record_completion_usage(
            &self.state,
            &self.auth,
            CompletionUsage {
                model: &self.requested_model,
                // A stream carries no model of its own; the requested id is
                // the only name the client asked for.
                resolved: None,
                token: &self.token,
                usage: &self.usage,
                // The discounted counts ride the terminal chunk now, so a
                // streamed row bills cached input at the cached rate.
                details: self.details,
                success: self.success,
                call_status: self.status,
                latency_ms: millis_since(self.started),
                ttft_ms: self.ttft_ms.unwrap_or(0),
                error_code: (!self.success).then_some("stream_error"),
                request_type: self.request_type,
                summary: &self.summary,
            },
        );
    }

    /// The stream failed before the terminal chunk, with the status the
    /// client is being told: a recovered upstream 4xx is booked as that 4xx,
    /// not as a blanket gateway failure.
    fn failed_with(&mut self, status: i64) {
        self.success = false;
        self.status = status;
        self.record();
    }

    /// The client went away before the terminal chunk: bill what it saw.
    fn aborted(&mut self) {
        self.success = false;
        self.status = CLIENT_CLOSED_REQUEST;
        self.record();
    }
}

impl Drop for StreamAccounting {
    fn drop(&mut self) {
        if !self.recorded {
            self.aborted();
        }
    }
}

fn stream_response(
    state: AppState,
    auth: Authenticated,
    request: ChatCompletionRequest,
) -> Sse<impl futures::Stream<Item = Result<Event, Infallible>>> {
    let id = ids::completion_id();
    let created = ids::unix_seconds();
    let model = request.model.clone();
    let summary = request_summary(&request);
    let started = std::time::Instant::now();
    let token = backend::request_token(&request).unwrap_or_default();
    let stream = state.backend.stream(request);
    let mut books =
        StreamAccounting::new(state, auth, token, model.clone(), started, "chat", summary);
    let mut role_emitted = false;
    let events = stream.map(move |item| {
        Ok(match item {
            Ok(chunk) => {
                // The clock starts when the request left, so this measures the
                // client's wait for the first token, not just the pump's.
                books.first_token();
                if let Some(usage) = chunk.usage {
                    books.usage = usage;
                }
                if let Some(details) = backend::chunk_cached_usage(&chunk) {
                    books.details = details;
                }
                let with_role = !role_emitted;
                role_emitted = true;
                let event = chunk_event(&id, created, &model, &chunk, with_role);
                if chunk.finish_reason.is_some() {
                    books.record();
                }
                event
            }
            Err(error) => {
                let error = with_recovered_status(error);
                // The 200 is already spent, so the recovered status can only
                // ride in the error frame — but it must be booked too.
                books.failed_with(i64::from(error_status(&error).as_u16()));
                error_event(&error)
            }
        })
    });

    let with_done = events.chain(futures::stream::once(async {
        Ok(Event::default().data("[DONE]"))
    }));

    Sse::new(with_done).keep_alive(KeepAlive::default())
}

fn chunk_event(
    id: &str,
    created: i64,
    model: &str,
    chunk: &StreamChunk,
    announce_role: bool,
) -> Event {
    let mut delta = serde_json::Map::new();
    if announce_role {
        delta.insert("role".into(), json!("assistant"));
    }
    if let Some(content) = &chunk.content {
        delta.insert("content".into(), json!(content));
    }
    if let Some(reasoning) = &chunk.reasoning {
        delta.insert("reasoning_content".into(), json!(reasoning));
    }
    if let Some(tool_calls) = &chunk.tool_calls {
        delta.insert("tool_calls".into(), tool_calls.clone());
    }

    let mut payload = json!({
        "id": id,
        "object": "chat.completion.chunk",
        "created": created,
        "model": model,
        "choices": [{
            "index": 0,
            "delta": Value::Object(delta),
            "finish_reason": chunk.finish_reason,
        }],
    });
    if let Some(usage) = chunk.usage {
        payload["usage"] = serde_json::to_value(usage).unwrap_or(Value::Null);
    }

    Event::default().data(payload.to_string())
}

/// Built-in server-side tools the chat path cannot execute. They are
/// rejected up front so the client sees the gap instead of a silent drop.
const UNSUPPORTED_RESPONSE_TOOLS: &[&str] = &[
    "file_search",
    "computer_use_preview",
    "code_interpreter",
    "image_generation",
    "mcp",
    "local_shell",
];

fn unsupported_builtin_tool(tools: Option<&Vec<Value>>) -> Option<String> {
    let tools = tools?;
    for tool in tools {
        let kind = tool.get("type").and_then(Value::as_str).unwrap_or("");
        if kind == "web_search" || kind == "x_search" {
            return Some(kind.to_string());
        }
        if UNSUPPORTED_RESPONSE_TOOLS.contains(&kind) {
            return Some(kind.to_string());
        }
    }
    None
}

/// Sanitized error payload shared by every client-facing surface.
/// Raw upstream bodies must never reach the client on the wire.
pub(crate) fn sanitized_error_payload(error: &GatewayError) -> Value {
    let message = match error {
        GatewayError::Upstream(_) => "upstream request failed".to_string(),
        other => other.to_string(),
    };
    let mut detail = Map::new();
    detail.insert("message".into(), json!(message));
    // The status is the one part of an upstream refusal the client can act
    // on, and it is ours to repeat: it came out of a structured field, not
    // out of the body the message carries.
    if let Some(status) = recovered_upstream_status(error) {
        detail.insert("status".into(), json!(status));
    }
    json!({ "error": Value::Object(detail) })
}

pub(crate) fn error_event(error: &GatewayError) -> Event {
    // ponytail: SSE goes through the same sanitized shape as error_response
    // because raw upstream bodies must never reach the client on the wire.
    Event::default().data(sanitized_error_payload(error).to_string())
}

pub(crate) fn forbidden_response(message: &str) -> Response {
    (
        StatusCode::FORBIDDEN,
        Json(json!({
            "error": {
                "message": message,
                "type": "permission_error",
                "code": "forbidden",
            }
        })),
    )
        .into_response()
}

fn upstream_status(message: &str) -> Option<u16> {
    message
        .strip_prefix("upstream status ")?
        .chars()
        .take_while(char::is_ascii_digit)
        .collect::<String>()
        .parse()
        .ok()
}

/// Where an upstream error object may spell the status it really carried.
///
/// Ordered by how much the field means: an explicit status beats a type or
/// code name, and the nested `error` object beats the envelope around it.
const STATUS_POINTERS: &[&str] = &[
    "/error/status",
    "/error/http_status",
    "/error/code",
    "/status",
    "/code",
];

/// Provider error codes that name a status without spelling the number.
fn named_status(code: &str) -> Option<u16> {
    Some(match code {
        "invalid_request_error" | "invalid_argument" | "bad_request" => 400,
        "authentication_error" | "unauthenticated" => 401,
        "permission_error" | "permission_denied" | "forbidden" => 403,
        "not_found_error" | "not_found" => 404,
        "conflict_error" => 409,
        "unprocessable_entity" => 422,
        "rate_limit_error" | "rate_limit_exceeded" | "too_many_requests" | "insufficient_quota" => {
            429
        }
        "server_error" | "internal_server_error" => 500,
        "overloaded_error" => 529,
        _ => return None,
    })
}

/// A status field, either a number or a code name.
fn status_field(value: &Value) -> Option<u16> {
    let status = match value {
        Value::Number(number) => number.as_u64()?,
        Value::String(code) => return named_status(code),
        _ => return None,
    };
    // Only a real HTTP status counts: an unrelated number in the payload
    // (`status: 0`, a model id, a byte count) is not a client verdict.
    (100..=599).contains(&status).then_some(status as u16)
}

/// The JSON object an upstream error message carries, if any.
fn error_payload(message: &str) -> Option<Value> {
    let start = message.find(['{', '['])?;
    serde_json::from_str(&message[start..]).ok()
}

/// The status an upstream refusal carried inside its payload.
///
/// A provider that reports the refusal in a body that arrived with HTTP 200 —
/// the only way an open stream can report one — leaves no `upstream status
/// {code}:` prefix for [`upstream_status`] to find. Recovering it is what
/// keeps a rate limit from reaching the client as a 502 and reaching the
/// breaker as an outage.
fn status_from_error_payload(message: &str) -> Option<u16> {
    let payload = error_payload(message)?;
    STATUS_POINTERS
        .iter()
        .find_map(|pointer| payload.pointer(pointer).and_then(status_field))
}

/// [`upstream_status`] widened to refusals delivered in a 200 body.
///
/// Public so `routing.rs::is_backend_outage` can classify against the same
/// rule the client-facing status uses; one call there replaces the prefix-only
/// parse and needs no executor changes.
pub fn upstream_status_or_recovered(message: &str) -> Option<u16> {
    upstream_status(message).or_else(|| status_from_error_payload(message))
}

/// The status an upstream error really carried, when it can be recovered.
///
/// Only [`GatewayError::Upstream`] has one: `InvalidRequest` is this crate's
/// own verdict and `UnknownModel` a routing outcome, neither is a provider
/// answer.
fn recovered_upstream_status(error: &GatewayError) -> Option<u16> {
    match error {
        GatewayError::Upstream(message) => upstream_status_or_recovered(message),
        _ => None,
    }
}

/// Upstream text kept in a message once its status has been recovered.
const UPSTREAM_DETAIL_LIMIT: usize = 512;

/// Re-spell an upstream error so a recovered status rides in the
/// `upstream status {code}:` prefix the breaker and the dashboards parse.
///
/// Idempotent, and it never invents a status: an error with no recoverable
/// one comes back untouched, so "unreachable provider" keeps tripping the
/// breaker exactly as before. Applied by [`StatusAwareBackend`] below the
/// router, and again at every point the gateway observes an error, so the
/// client status is right even for an executor that has not been wrapped.
fn with_recovered_status(error: GatewayError) -> GatewayError {
    let GatewayError::Upstream(message) = &error else {
        return error;
    };
    if upstream_status(message).is_some() {
        return error;
    }
    let Some(status) = status_from_error_payload(message) else {
        return error;
    };
    // The payload is kept, capped: an executor that forwards a whole error
    // object must not turn one refusal into an unbounded log line.
    let detail: String = message.chars().take(UPSTREAM_DETAIL_LIMIT).collect();
    GatewayError::Upstream(format!("upstream status {status}: {detail}"))
}

/// Provider executor wrapped so a refusal that arrived inside a 200 body
/// carries its real status before the breaker sees it.
///
/// [`routing::RoutingBackend`] calls a failure an outage when the
/// `upstream status {code}:` prefix is absent, so a rate limit delivered in a
/// 200 body read as a dead backend and took every other model on that
/// provider down for the whole cooldown — the frozen contract in AGENTS.md
/// ("a 4xx from the provider does not trip the breaker") said otherwise.
/// Normalizing one layer below the router is what makes the breaker, the
/// call logs and the client agree.
///
/// Wrapping is one line where the backends map is built
/// (`main.rs::select_backend`); the alternative is a single call to
/// [`upstream_status_or_recovered`] inside `routing.rs::is_backend_outage`,
/// which needs no wrapping at all. Both are flagged to the main agent — this
/// crate's routing layer is read-only here.
pub struct StatusAwareBackend {
    inner: Arc<dyn ChatBackend>,
}

impl StatusAwareBackend {
    /// Wrap `inner`, keeping its name and attribution untouched.
    pub fn new(inner: Arc<dyn ChatBackend>) -> Self {
        Self { inner }
    }
}

#[async_trait::async_trait]
impl ChatBackend for StatusAwareBackend {
    fn name(&self) -> &'static str {
        self.inner.name()
    }

    fn attribution(&self, token: &str) -> backend::Attribution {
        self.inner.attribution(token)
    }

    async fn complete(
        &self,
        request: ChatCompletionRequest,
    ) -> Result<ChatCompletionResponse, GatewayError> {
        self.inner
            .complete(request)
            .await
            .map_err(with_recovered_status)
    }

    fn stream(&self, request: ChatCompletionRequest) -> backend::ChunkStream {
        let stream = self.inner.stream(request);
        Box::pin(stream.map(|item| item.map_err(with_recovered_status)))
    }
}

fn error_status(error: &GatewayError) -> StatusCode {
    match error {
        GatewayError::InvalidRequest(_) => StatusCode::BAD_REQUEST,
        GatewayError::UnknownModel(_) => StatusCode::NOT_FOUND,
        GatewayError::Upstream(_) => client_status(recovered_upstream_status(error)),
    }
}

/// Client status for a recovered upstream status.
///
/// The map is deliberately the one that was already here: a status the client
/// can act on is repeated, everything else stays a gateway failure so an
/// unrecoverable or 5xx refusal keeps its 502 and its breaker failure.
fn client_status(status: Option<u16>) -> StatusCode {
    match status {
        Some(400) => StatusCode::BAD_REQUEST,
        Some(401) => StatusCode::UNAUTHORIZED,
        Some(403) => StatusCode::FORBIDDEN,
        Some(404) => StatusCode::NOT_FOUND,
        Some(409) => StatusCode::CONFLICT,
        Some(422) => StatusCode::UNPROCESSABLE_ENTITY,
        Some(429) => StatusCode::TOO_MANY_REQUESTS,
        _ => StatusCode::BAD_GATEWAY,
    }
}

pub(crate) fn error_response(error: GatewayError) -> Response {
    let status = error_status(&error);
    (status, Json(sanitized_error_payload(&error))).into_response()
}

const RESPONSE_STORE_NAMESPACE: &str = "responses";

/// How long a stored `/v1/responses` entry stays fetchable, in seconds.
///
/// The store used to be a pure sink: every full prompt *and* completion went
/// into `key_value` with no TTL, no cap and no sweep, and only a client
/// DELETing its own id ever removed one — which no client does in bulk. A
/// week is long enough for an agent to resume a session with
/// `previous_response_id`.
const RESPONSE_TTL_SECS: u64 = 7 * 24 * 60 * 60;

/// How many stored responses are retained at once, oldest evicted first.
///
/// `key_value` is shared with the settings, so a flood inside the TTL window
/// still must not be able to grow the table without bound.
const RESPONSE_ENTRY_CAP: usize = 512;

/// Bytes retained for one stored entry. Past this the entry is replaced by a
/// well-formed placeholder costing a few hundred bytes.
const RESPONSE_ENTRY_MAX_BYTES: usize = 256 * 1024;

/// Drop expired entries and cap the table, oldest first.
///
/// Runs on every insert, plus on access for an entry found expired. A sweep
/// that fails is logged and forgotten: losing the housekeeping must never
/// cost a client its completion, and the next insert sweeps again.
fn sweep_response_store(conn: &rusqlite::Connection) {
    let now = now_unix();
    // A clock reading 0 (no `SystemTime`) would age every entry at once, so
    // skip the age sweep rather than wipe the store.
    if now > 0 {
        let cutoff = i64::try_from(now.saturating_sub(RESPONSE_TTL_SECS)).unwrap_or(i64::MAX);
        // `stored_at` is this crate's own field; a row without one is left
        // alone rather than guessed at.
        if let Err(error) = conn.execute(
            "DELETE FROM key_value WHERE namespace = ?1
             AND CAST(COALESCE(json_extract(value, '$.stored_at'), 0) AS INTEGER)
                 BETWEEN 1 AND ?2",
            rusqlite::params![RESPONSE_STORE_NAMESPACE, cutoff],
        ) {
            tracing::warn!("response state store ttl sweep failed: {error}");
        }
    }
    // `rowid` grows with the insert, so the newest entry is the one kept.
    let cap = i64::try_from(RESPONSE_ENTRY_CAP).unwrap_or(i64::MAX);
    if let Err(error) = conn.execute(
        "DELETE FROM key_value WHERE namespace = ?1 AND rowid NOT IN (
             SELECT rowid FROM key_value WHERE namespace = ?1 ORDER BY rowid DESC LIMIT ?2)",
        rusqlite::params![RESPONSE_STORE_NAMESPACE, cap],
    ) {
        tracing::warn!("response state store cap sweep failed: {error}");
    }
}

/// Whether a stored entry is past its retention window.
fn response_expired(record: &Value) -> bool {
    let now = now_unix();
    if now == 0 {
        return false;
    }
    record
        .get("stored_at")
        .and_then(Value::as_u64)
        .is_some_and(|stored| now.saturating_sub(stored) > RESPONSE_TTL_SECS)
}

/// The placeholder kept for an entry too large to retain.
///
/// Still a valid Responses object — same id, model and usage, empty output,
/// marked incomplete — so a fetch answers with something parseable instead
/// of a 500, and a `previous_response_id` replay is a no-op rather than a
/// 400 (`replay_previous_response` skips truncated entries).
fn truncated_response(response: &Value) -> Value {
    json!({
        "id": response.get("id").cloned().unwrap_or(Value::Null),
        "object": "response",
        "created_at": response.get("created_at").cloned().unwrap_or_else(|| json!(ids::unix_seconds())),
        "status": "incomplete",
        "model": response.get("model").cloned().unwrap_or(Value::Null),
        "output": [],
        "incomplete_details": {"reason": "content_truncated"},
        "usage": response.get("usage").cloned().unwrap_or(json!({
            "input_tokens": 0, "output_tokens": 0, "total_tokens": 0,
        })),
    })
}

/// The envelope stored for one response.
///
/// The request/response pair is retained only while it fits
/// [`RESPONSE_ENTRY_MAX_BYTES`]: one completion can be megabytes, and
/// keeping it verbatim is the leak. Past the cap the entry degrades to a
/// placeholder so no unbounded bytes ever reach the table.
fn stored_response_record(
    state: &AppState,
    auth: &Authenticated,
    request: &Value,
    response: &Value,
) -> Value {
    let envelope = |request: Value, response: Value, truncated: bool| {
        json!({
            "version": 1,
            "api_key_id": response_owner(auth),
            "model": response.get("model").cloned().unwrap_or(Value::Null),
            "backend": state.backend.name(),
            "stored_at": now_unix(),
            "truncated": truncated,
            "request": request,
            "response": response,
        })
    };
    let record = envelope(request.clone(), response.clone(), false);
    if serde_json::to_string(&record).is_ok_and(|encoded| encoded.len() <= RESPONSE_ENTRY_MAX_BYTES)
    {
        return record;
    }
    tracing::warn!("response entry exceeds the store budget; keeping a placeholder");
    envelope(Value::Null, truncated_response(response), true)
}

fn valid_response_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 200
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"-_.".contains(&byte))
}

fn response_owner(auth: &Authenticated) -> Value {
    auth.key
        .as_ref()
        .map(|key| Value::String(key.id.clone()))
        .unwrap_or(Value::Null)
}

fn response_visible(auth: &Authenticated, record: &Value) -> bool {
    let stored_owner = record.get("api_key_id");
    match (&auth.key, stored_owner) {
        (Some(key), Some(owner)) => owner.as_str() == Some(key.id.as_str()),
        (None, Some(Value::Null)) => true,
        _ => false,
    }
}

fn persist_response(state: &AppState, auth: &Authenticated, request: &Value, response: &Value) {
    let Some(db) = state.db.as_ref() else {
        return;
    };
    let Some(id) = response.get("id").and_then(Value::as_str) else {
        return;
    };
    if !valid_response_id(id) {
        return;
    }
    let record = stored_response_record(state, auth, request, response);
    let guard = db.connection();
    if let Err(error) =
        heterion_router_db::repos::kv_set(&guard, RESPONSE_STORE_NAMESPACE, id, &record.to_string())
    {
        tracing::warn!("response state store write failed: {error}");
    }
    // Opportunistic housekeeping, after the write so the retained count
    // settles at the cap rather than one above it — and so the entry just
    // written, which owns the newest `rowid`, is one of the survivors.
    // Best effort: `sweep_response_store` logs and continues.
    sweep_response_store(&guard);
}

fn load_response_record(
    state: &AppState,
    auth: &Authenticated,
    id: &str,
) -> Result<Option<Value>, ()> {
    if !valid_response_id(id) {
        return Ok(None);
    }
    let Some(db) = state.db.as_ref() else {
        return Ok(None);
    };
    let guard = db.connection();
    let raw = heterion_router_db::repos::kv_get(&guard, RESPONSE_STORE_NAMESPACE, id).map_err(
        |error| {
            tracing::warn!("response state store read failed: {error}");
        },
    )?;
    let Some(raw) = raw else {
        return Ok(None);
    };
    let record: Value = serde_json::from_str(&raw).map_err(|error| {
        tracing::warn!("response state store record is invalid: {error}");
    })?;
    // Expired state is gone as far as the API is concerned, and dropping it
    // here is the lazy half of the sweep: the next insert would catch it,
    // but a quiet gateway should shrink too.
    if response_expired(&record) {
        if let Err(error) =
            heterion_router_db::repos::kv_delete(&guard, RESPONSE_STORE_NAMESPACE, id)
        {
            tracing::warn!("response state store sweep delete failed: {error}");
        }
        return Ok(None);
    }
    if !response_visible(auth, &record) {
        return Ok(None);
    }
    let model = record
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if !auth.allows_model(model) {
        return Ok(None);
    }
    if !record.get("response").is_some_and(Value::is_object) {
        return Err(());
    }
    Ok(Some(record))
}

fn load_response(state: &AppState, auth: &Authenticated, id: &str) -> Result<Option<Value>, ()> {
    let Some(record) = load_response_record(state, auth, id)? else {
        return Ok(None);
    };
    Ok(record.get("response").cloned())
}

fn response_output_messages(response: &Value) -> Vec<ChatMessage> {
    let mut text = String::new();
    let mut reasoning = String::new();
    let mut tool_calls = Vec::new();
    for item in response
        .get("output")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        match item.get("type").and_then(Value::as_str) {
            Some("message") => {
                let part = input_item_text(item.get("content"));
                if !part.is_empty() {
                    text.push_str(&part);
                }
            }
            Some("function_call") => {
                let call_id = item
                    .get("call_id")
                    .and_then(Value::as_str)
                    .or_else(|| item.get("id").and_then(Value::as_str))
                    .unwrap_or("");
                let name = item.get("name").and_then(Value::as_str).unwrap_or("");
                if name.is_empty() {
                    continue;
                }
                tool_calls.push(json!({
                    "id": call_id,
                    "type": "function",
                    "function": {
                        "name": name,
                        "arguments": item.get("arguments").and_then(Value::as_str).unwrap_or("{}"),
                    },
                }));
            }
            Some("reasoning") => {
                if let Some(summary) = item.get("summary").and_then(Value::as_str) {
                    reasoning.push_str(summary);
                }
            }
            _ => {}
        }
    }
    if text.is_empty() && reasoning.is_empty() && tool_calls.is_empty() {
        return Vec::new();
    }
    vec![ChatMessage {
        role: "assistant".to_string(),
        content: (!text.is_empty()).then_some(Value::String(text)),
        reasoning_content: (!reasoning.is_empty()).then_some(reasoning),
        tool_calls: (!tool_calls.is_empty()).then_some(Value::Array(tool_calls)),
        tool_call_id: None,
        name: None,
    }]
}

fn append_input_items(target: &mut Vec<Value>, input: Option<&Value>) {
    match input {
        Some(Value::String(text)) => target.push(json!({
            "type": "message",
            "role": "user",
            "content": [{"type": "input_text", "text": text}],
        })),
        Some(Value::Array(items)) => target.extend(items.iter().cloned()),
        Some(value @ Value::Object(_)) => target.push(value.clone()),
        _ => {}
    }
}

fn replay_previous_response(
    state: &AppState,
    auth: &Authenticated,
    request: &mut ChatCompletionRequest,
    previous_id: &str,
) -> Result<bool, GatewayError> {
    let Some(record) = load_response_record(state, auth, previous_id)
        .map_err(|_| GatewayError::Upstream("response state store unavailable".to_string()))?
    else {
        return Ok(false);
    };
    // A size-capped placeholder kept the id but not the conversation, so there
    // is nothing to replay: same as an id the client never had.
    if record.get("truncated").and_then(Value::as_bool) == Some(true) {
        return Ok(false);
    }
    let Some(previous_request) = record.get("request") else {
        return Ok(false);
    };
    let Some(previous_response) = record.get("response") else {
        return Ok(false);
    };
    let mut previous = responses_to_chat(previous_request).map_err(|message| {
        GatewayError::InvalidRequest(format!("stored response is invalid: {message}"))
    })?;
    if previous.model != request.model {
        return Err(GatewayError::InvalidRequest(
            "previous_response_id belongs to a different model".to_string(),
        ));
    }
    previous
        .messages
        .extend(response_output_messages(previous_response));
    previous.messages.append(&mut request.messages);
    request.messages = previous.messages;
    if request.tools.is_none() {
        request.tools = previous.tools;
    }
    let mut input = Vec::new();
    append_input_items(&mut input, previous_request.get("input"));
    if let Some(output) = previous_response.get("output").and_then(Value::as_array) {
        input.extend(output.iter().cloned());
    }
    append_input_items(&mut input, request.responses_input.as_ref());
    request.responses_input = Some(Value::Array(input));
    request.extra.remove("previous_response_id");
    Ok(true)
}

fn response_not_found() -> Response {
    (
        StatusCode::NOT_FOUND,
        Json(json!({"error": {"message": "response not found", "code": "response_not_found"}})),
    )
        .into_response()
}

fn response_store_error() -> Response {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(json!({"error": {"message": "response state store unavailable"}})),
    )
        .into_response()
}

async fn get_response(
    State(state): State<AppState>,
    auth: Authenticated,
    Path(id): Path<String>,
) -> Response {
    match load_response(&state, &auth, &id) {
        Ok(Some(response)) => Json(response).into_response(),
        Ok(None) => response_not_found(),
        Err(()) => response_store_error(),
    }
}

async fn delete_response(
    State(state): State<AppState>,
    auth: Authenticated,
    Path(id): Path<String>,
) -> Response {
    let Some(response) = (match load_response(&state, &auth, &id) {
        Ok(value) => value,
        Err(()) => return response_store_error(),
    }) else {
        return response_not_found();
    };
    let Some(db) = state.db.as_ref() else {
        return response_not_found();
    };
    let guard = db.connection();
    if let Err(error) = heterion_router_db::repos::kv_delete(&guard, RESPONSE_STORE_NAMESPACE, &id)
    {
        tracing::warn!("response state store delete failed: {error}");
        return response_store_error();
    }
    let response_id = response
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or(&id)
        .to_string();
    Json(json!({
        "id": response_id,
        "object": "response.deleted",
        "deleted": true,
    }))
    .into_response()
}

/// `POST /v1/responses` — the OpenAI Responses API surface, so Responses-only
/// clients (grok CLI, Codex) can point at the gateway. The request is
/// translated to the internal chat shape, served by the same backend
/// machinery, and translated back; streaming emits Responses events.
async fn responses(
    State(state): State<AppState>,
    auth: Authenticated,
    JsonBody(request): JsonBody<Value>,
) -> Response {
    let should_store = request
        .get("store")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    let request_value = request.clone();
    let mut request = match responses_to_chat(&request) {
        Ok(request) => request,
        Err(message) => return error_response(GatewayError::InvalidRequest(message)),
    };
    if let Some(previous_id) = request
        .extra
        .get("previous_response_id")
        .and_then(Value::as_str)
        .map(str::to_string)
    {
        match replay_previous_response(&state, &auth, &mut request, &previous_id) {
            Ok(true) | Ok(false) => {}
            Err(error) => return error_response(error),
        }
    }
    if !auth.allows_model(&request.model) {
        return forbidden_response("model is not allowed for this api key");
    }
    let token = backend::stamp_attribution(&mut request);
    if request.stream {
        tracing::info!("responses model={} stream=true", request.model);
        return responses_stream(state, auth, request).into_response();
    }

    let model = request.model.clone();
    let summary = request_summary(&request);
    let started = std::time::Instant::now();
    tracing::info!("responses model={model} stream=false");
    match state.backend.complete(request).await {
        Ok(response) => {
            let latency_ms = millis_since(started);
            let served = response.model.clone();
            let usage = response.usage;
            let details = cached_usage(&response);
            record_completion_usage(
                &state,
                &auth,
                CompletionUsage {
                    model: &model,
                    resolved: Some(&served),
                    token: &token,
                    usage: &usage,
                    details,
                    success: true,
                    call_status: 200,
                    latency_ms,
                    ttft_ms: 0,
                    error_code: None,
                    request_type: "responses",
                    summary: &summary,
                },
            );
            let mut response_object = chat_to_responses_object(&response);
            response_object["store"] = json!(should_store);
            if should_store {
                persist_response(&state, &auth, &request_value, &response_object);
            }
            let mut http = Json(response_object).into_response();
            http.headers_mut().extend(router_headers(
                &state.prices,
                &state.backend.attribution(&token),
                &model,
                &served,
                &usage,
                details,
            ));
            http
        }
        Err(error) => {
            let error = with_recovered_status(error);
            let latency_ms = millis_since(started);
            tracing::warn!("responses failed model={model}: {error}");
            record_completion_usage(
                &state,
                &auth,
                CompletionUsage {
                    model: &model,
                    resolved: None,
                    token: &token,
                    usage: &heterion_router_core::Usage::default(),
                    // An upstream that failed reported no tokens at all.
                    details: CachedUsage::default(),
                    success: false,
                    call_status: i64::from(error_status(&error).as_u16()),
                    latency_ms,
                    ttft_ms: 0,
                    error_code: Some("upstream_error"),
                    request_type: "responses",
                    summary: &summary,
                },
            );
            error_response(error)
        }
    }
}

/// Translate a Responses API request to the internal chat request.
///
/// Covers the shapes Responses-only clients actually send: `instructions`,
/// string and item-array `input`, `max_output_tokens`, Responses-shaped
/// `tools`. Unmodelled item kinds are skipped, never fabricated. Built-in
/// server-side tools (`web_search` and friends) are rejected fail-closed:
/// the chat path has no executor for them, so dropping them would corrupt
/// the agent loop silently.
fn responses_to_chat(request: &Value) -> Result<ChatCompletionRequest, String> {
    let model = request
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    if model.is_empty() {
        return Err("model is required".into());
    }
    if let Some(kind) = unsupported_builtin_tool(request.get("tools").and_then(Value::as_array)) {
        return Err(format!("unsupported tool: {kind}"));
    }

    let mut messages: Vec<heterion_router_core::ChatMessage> = Vec::new();
    if let Some(instructions) = request.get("instructions").and_then(Value::as_str)
        && !instructions.is_empty()
    {
        messages.push(heterion_router_core::ChatMessage::plain(
            "system".to_string(),
            instructions.to_string(),
        ));
    }

    match request.get("input") {
        Some(Value::String(text)) if !text.is_empty() => {
            messages.push(heterion_router_core::ChatMessage::plain(
                "user".to_string(),
                text.clone(),
            ));
        }
        Some(Value::Array(items)) => {
            for item in items {
                match item
                    .get("type")
                    .and_then(Value::as_str)
                    .unwrap_or("message")
                {
                    "message" => {
                        let role = item.get("role").and_then(Value::as_str).unwrap_or("user");
                        let text = input_item_text(item.get("content"));
                        if text.is_empty() {
                            continue;
                        }
                        messages.push(heterion_router_core::ChatMessage::plain(
                            role.to_string(),
                            text,
                        ));
                    }
                    // A Responses `function_call` item is the assistant's
                    // tool invocation; map to the OpenAI tool_calls shape.
                    "function_call" => {
                        let name = item.get("name").and_then(Value::as_str).unwrap_or("");
                        if name.is_empty() {
                            continue;
                        }
                        messages.push(heterion_router_core::ChatMessage {
                            role: "assistant".to_string(),
                            content: None,
                            reasoning_content: None,
                            tool_calls: Some(json!([{
                                "id": item.get("call_id").and_then(Value::as_str).unwrap_or(""),
                                "type": "function",
                                "function": {
                                    "name": name,
                                    "arguments": item
                                        .get("arguments")
                                        .and_then(Value::as_str)
                                        .unwrap_or("{}"),
                                },
                            }])),
                            tool_call_id: None,
                            name: None,
                        });
                    }
                    // A Responses `function_call_output` item is the tool
                    // result; map to the OpenAI tool message.
                    "function_call_output" => {
                        let call_id = item.get("call_id").and_then(Value::as_str).unwrap_or("");
                        let output = match item.get("output") {
                            Some(Value::String(text)) => text.clone(),
                            Some(other) => other.to_string(),
                            None => continue,
                        };
                        messages.push(heterion_router_core::ChatMessage {
                            role: "tool".to_string(),
                            content: Some(Value::from(output)),
                            reasoning_content: None,
                            tool_calls: None,
                            tool_call_id: if call_id.is_empty() {
                                None
                            } else {
                                Some(call_id.to_string())
                            },
                            name: None,
                        });
                    }
                    // ponytail: reasoning/other item kinds are skipped; map
                    // them when a client actually needs them surfaced.
                    _ => {}
                }
            }
        }
        _ => return Err("input is required".into()),
    }

    if messages.is_empty() {
        return Err("input produced no messages".into());
    }

    let mut extra = Map::new();
    extra.insert(
        "store".to_string(),
        request.get("store").cloned().unwrap_or(Value::Bool(true)),
    );
    for key in [
        "include",
        "reasoning",
        "previous_response_id",
        "text",
        "prompt_cache_key",
        "session_id",
        "conversation_id",
    ] {
        if let Some(value) = request.get(key) {
            extra.insert(key.to_string(), value.clone());
        }
    }

    Ok(ChatCompletionRequest {
        model,
        messages,
        stream: matches!(request.get("stream"), Some(Value::Bool(true))),
        max_tokens: request
            .get("max_output_tokens")
            .and_then(Value::as_u64)
            .map(|value| value.min(u64::from(u32::MAX)) as u32),
        temperature: request
            .get("temperature")
            .and_then(Value::as_f64)
            .map(|value| value as f32),
        top_p: request
            .get("top_p")
            .and_then(Value::as_f64)
            .map(|value| value as f32),
        // Responses tools are flat ({type:"function", name, parameters});
        // the internal shape wraps them in a `function` member.
        tools: request.get("tools").and_then(Value::as_array).map(|tools| {
            json!(tools
                .iter()
                .filter_map(|tool| {
                    let name = tool.get("name").and_then(Value::as_str)?;
                    Some(json!({
                        "type": tool.get("type").and_then(Value::as_str).unwrap_or("function"),
                        "function": {
                            "name": name,
                            "description": tool.get("description").and_then(Value::as_str).unwrap_or(""),
                            "parameters": tool
                                .get("parameters")
                                .cloned()
                                .unwrap_or(json!({"type": "object"})),
                        },
                    }))
                })
                .collect::<Vec<_>>())
        }),
         tool_choice: request.get("tool_choice").map(|choice| {
             if choice.get("type").and_then(Value::as_str) == Some("function")
                 && let Some(name) = choice.get("name").and_then(Value::as_str)
             {
                 json!({"type": "function", "function": {"name": name}})
             } else {
                 choice.clone()
             }
         }),
         reasoning_effort: request
             .get("reasoning")
             .and_then(|value| value.get("effort"))
             .or_else(|| request.get("reasoning_effort"))
             .and_then(Value::as_str)
             .map(str::to_string),
         stream_options: request.get("stream_options").cloned(),
         parallel_tool_calls: request.get("parallel_tool_calls").and_then(Value::as_bool),
         metadata: request.get("metadata").cloned(),
         service_tier: request.get("service_tier").and_then(Value::as_str).map(str::to_string),
         responses_input: request.get("input").cloned(),
         extra,
         ..Default::default()
    })
}

/// Flatten a Responses message content (string, or input/output_text parts)
/// to plain text.
fn input_item_text(content: Option<&Value>) -> String {
    match content {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Array(parts)) => parts
            .iter()
            .filter_map(|part| {
                let part_type = part.get("type").and_then(Value::as_str).unwrap_or("text");
                if matches!(part_type, "input_text" | "output_text" | "text" | "refusal") {
                    part.get("text")
                        .and_then(Value::as_str)
                        .or_else(|| part.get("refusal").and_then(Value::as_str))
                } else {
                    None
                }
            })
            .collect::<Vec<_>>()
            .join(""),
        _ => String::new(),
    }
}

/// Wrap a completed chat completion in a Responses API object.
fn chat_to_responses_object(response: &ChatCompletionResponse) -> Value {
    if let Some(raw) = response.raw.as_ref().filter(|value| value.is_object()) {
        return raw.clone();
    }
    let message = &response.choices[0].message;
    let response_id = if response.id.starts_with("resp-") || response.id.starts_with("resp_") {
        response.id.clone()
    } else {
        format!("resp_{}", response.id)
    };
    let mut output = Vec::new();
    if let Some(text) = message.content.as_ref().and_then(Value::as_str) {
        output.push(json!({
            "type": "message",
            "id": format!("msg_{}", response.id),
            "status": "completed",
            "role": message.role,
            "content": [{"type": "output_text", "text": text, "annotations": []}],
        }));
    }
    if let Some(calls) = message.tool_calls.as_ref().and_then(Value::as_array) {
        for call in calls {
            output.push(json!({
                "type": "function_call",
                "id": format!("fc_{}", call.get("id").and_then(Value::as_str).unwrap_or("")),
                "call_id": call.get("id").and_then(Value::as_str).unwrap_or(""),
                "name": call.pointer("/function/name").and_then(Value::as_str).unwrap_or(""),
                "arguments": call
                    .pointer("/function/arguments")
                    .and_then(Value::as_str)
                    .unwrap_or("{}"),
                "status": "completed",
            }));
        }
    }

    json!({
        "id": response_id,
        "object": "response",
        "created_at": response.created,
        "status": "completed",
        "model": response.model,
        "output": output,
        "usage": {
            "input_tokens": response.usage.prompt_tokens,
            "output_tokens": response.usage.completion_tokens,
            "total_tokens": response.usage.total_tokens,
        },
    })
}

/// Stream a completion as Responses API events.
fn responses_stream(
    state: AppState,
    auth: Authenticated,
    request: ChatCompletionRequest,
) -> Sse<impl futures::Stream<Item = Result<Event, Infallible>>> {
    let model = request.model.clone();
    let summary = request_summary(&request);
    let created = ids::unix_seconds();
    let started = std::time::Instant::now();
    let token = backend::request_token(&request).unwrap_or_default();
    let stream = state.backend.stream(request);
    let mut books = StreamAccounting::new(
        state,
        auth,
        token,
        model.clone(),
        started,
        "responses",
        summary,
    );

    let events = stream.flat_map(move |item| {
        let mut failed_status = None;
        let (events, success) = match item {
            Ok(chunk) => {
                books.first_token();
                if let Some(usage) = chunk.usage {
                    books.usage = usage;
                }
                if let Some(details) = backend::chunk_cached_usage(&chunk) {
                    books.details = details;
                }
                let success = chunk.finish_reason.is_some();
                (
                    chat_chunk_to_responses_events(&chunk, &created, &model),
                    success,
                )
            }
            Err(error) => {
                let error = with_recovered_status(error);
                failed_status = Some(i64::from(error_status(&error).as_u16()));
                (vec![sanitized_error_payload(&error)], false)
            }
        };
        match (success, failed_status) {
            // Only a terminal chunk books a success; a refusal books the
            // status the client was told, and a mid-stream delta books
            // nothing at all.
            (true, _) => books.record(),
            (_, Some(status)) => books.failed_with(status),
            (false, None) => {}
        }
        futures::stream::iter(
            events
                .into_iter()
                .map(|payload| {
                    let event = payload
                        .get("type")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string();
                    Ok::<_, Infallible>(Event::default().event(event).data(payload.to_string()))
                })
                .collect::<Vec<_>>(),
        )
    });

    Sse::new(events).keep_alive(KeepAlive::default())
}

/// Map one internal stream chunk to Responses API events.
///
/// `response.completed` carries upstream usage when the terminal chunk
/// provides it; tool calls are emitted whole in one `output_item.added`.
fn chat_chunk_to_responses_events(chunk: &StreamChunk, created: &i64, model: &str) -> Vec<Value> {
    let mut events = Vec::new();
    if let Some(reasoning) = &chunk.reasoning {
        events.push(json!({
            "type": "response.reasoning_text.delta",
            "delta": reasoning,
        }));
    }
    if let Some(content) = &chunk.content {
        events.push(json!({
            "type": "response.output_text.delta",
            "delta": content,
        }));
    }
    if let Some(tool_calls) = &chunk.tool_calls {
        events.push(json!({
            "type": "response.output_item.added",
            "output_index": 0,
            "item": {
                "type": "function_call",
                "call_id": "",
                "arguments": "",
                "tool_calls": tool_calls,
            },
        }));
    }
    if let Some(finish) = &chunk.finish_reason {
        events.push(json!({
            "type": "response.completed",
            "response": {
                "object": "response",
                "created_at": created,
                "status": "completed",
                "model": model,
                "usage": {
                    "input_tokens": chunk.usage.map(|usage| usage.prompt_tokens).unwrap_or(0),
                    "output_tokens": chunk.usage.map(|usage| usage.completion_tokens).unwrap_or(0),
                    "total_tokens": chunk.usage.map(|usage| usage.total_tokens).unwrap_or(0),
                },
            },
            "finish_reason": finish,
        }));
    }
    events
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::{Body, to_bytes};
    use axum::http::Request;
    use futures::StreamExt;
    use heterion_router_providers::ProviderRegistry;
    use heterion_router_routing::Router as Planner;
    use http_body_util::BodyExt;
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tower::ServiceExt;

    fn test_router() -> Router {
        build_router(Arc::new(backend::EchoBackend))
    }

    /// A per-provider executor that names its own provider and connection,
    /// the way the real registry-driven backends do: `name()` is the generic
    /// `"provider"` because the identity lives in the credential pool.
    #[derive(Clone)]
    struct AttributedBackend {
        provider: &'static str,
        connection: &'static str,
    }

    #[async_trait::async_trait]
    impl ChatBackend for AttributedBackend {
        fn name(&self) -> &'static str {
            "provider"
        }

        fn attribution(&self, _token: &str) -> backend::Attribution {
            backend::Attribution {
                provider: Some(self.provider.to_string()),
                connection_id: Some(self.connection.to_string()),
                combo_name: None,
            }
        }

        async fn complete(
            &self,
            request: ChatCompletionRequest,
        ) -> Result<ChatCompletionResponse, GatewayError> {
            Ok(ChatCompletionResponse {
                id: ids::completion_id(),
                object: "chat.completion",
                created: ids::unix_seconds(),
                model: request.model.clone(),
                choices: vec![heterion_router_core::ChatChoice {
                    index: 0,
                    message: ChatMessage::plain("assistant".to_string(), "ok".to_string()),
                    finish_reason: "stop".to_string(),
                }],
                usage: heterion_router_core::Usage {
                    prompt_tokens: 7,
                    completion_tokens: 4,
                    total_tokens: 11,
                },
                raw: None,
            })
        }

        fn stream(&self, _request: ChatCompletionRequest) -> backend::ChunkStream {
            Box::pin(futures::stream::once(async {
                Ok(StreamChunk::done("stop"))
            }))
        }
    }

    /// Two providers behind one routing backend, both keyless-safe.
    fn routing_backend(db: Arc<heterion_router_db::Db>) -> Arc<dyn ChatBackend> {
        let mut backends: HashMap<String, Arc<dyn ChatBackend>> = HashMap::new();
        for (provider, connection) in [
            ("openrouter", "conn-openrouter"),
            ("heterion-local", "conn-heterion-local"),
        ] {
            backends.insert(
                provider.to_string(),
                Arc::new(AttributedBackend {
                    provider,
                    connection,
                }),
            );
        }
        Arc::new(
            routing::RoutingBackend::new(backends, Planner::new(vec![], vec![]))
                .with_combo_source(db, Arc::new(ProviderRegistry::load())),
        )
    }

    /// Read a query into rows of text cells for assertion: SQLite values are
    /// rendered as text, NULL stays `None`.
    fn rows(conn: &rusqlite::Connection, sql: &str) -> Vec<Vec<Option<String>>> {
        use rusqlite::types::ValueRef;

        let mut statement = conn.prepare(sql).unwrap();
        let count = statement.column_count();
        let mut query = statement.query([]).unwrap();
        let mut out = Vec::new();
        while let Some(row) = query.next().unwrap() {
            let mut values = Vec::with_capacity(count);
            for index in 0..count {
                values.push(match row.get_ref(index).unwrap() {
                    ValueRef::Null => None,
                    ValueRef::Integer(value) => Some(value.to_string()),
                    ValueRef::Real(value) => Some(value.to_string()),
                    ValueRef::Text(bytes) => Some(String::from_utf8_lossy(bytes).to_string()),
                    ValueRef::Blob(bytes) => Some(format!("<{} bytes>", bytes.len())),
                });
            }
            out.push(values);
        }
        out
    }

    #[derive(Clone)]
    struct UsageStreamBackend;

    #[async_trait::async_trait]
    impl ChatBackend for UsageStreamBackend {
        fn name(&self) -> &'static str {
            "usage-stream"
        }

        async fn complete(
            &self,
            _request: ChatCompletionRequest,
        ) -> Result<ChatCompletionResponse, GatewayError> {
            Err(GatewayError::InvalidRequest("stream backend".into()))
        }

        fn stream(&self, _request: ChatCompletionRequest) -> backend::ChunkStream {
            futures::stream::iter(vec![
                Ok(StreamChunk::content("ok")),
                Ok(StreamChunk {
                    finish_reason: Some("stop".to_string()),
                    usage: Some(heterion_router_core::Usage {
                        prompt_tokens: 3,
                        completion_tokens: 2,
                        total_tokens: 5,
                    }),
                    ..Default::default()
                }),
            ])
            .boxed()
        }
    }

    #[tokio::test]
    async fn healthz_reports_ok() {
        let response = test_router()
            .oneshot(
                Request::builder()
                    .uri("/healthz")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[derive(Clone)]
    struct FailingBackend {
        message: String,
    }

    #[async_trait::async_trait]
    impl ChatBackend for FailingBackend {
        fn name(&self) -> &'static str {
            "failing"
        }

        async fn complete(
            &self,
            _request: ChatCompletionRequest,
        ) -> Result<ChatCompletionResponse, GatewayError> {
            Err(GatewayError::Upstream(self.message.clone()))
        }

        fn stream(&self, _request: ChatCompletionRequest) -> backend::ChunkStream {
            let message = self.message.clone();
            futures::stream::once(async move { Err(GatewayError::Upstream(message)) }).boxed()
        }
    }

    #[tokio::test]
    async fn stream_errors_never_leak_upstream_text() {
        // ponytail: wire-level check because axum's Event has no public body
        // accessor; the router + body bytes are the contract that matters.
        let marker = "CANARY-9f3a-upstream-secret";
        let backend = Arc::new(FailingBackend {
            message: format!("upstream status 500: private body {marker}"),
        });
        for uri in ["/v1/chat/completions", "/v1/responses"] {
            let body = if uri.ends_with("responses") {
                json!({"model": "m", "input": "hi", "stream": true})
            } else {
                json!({"model": "m", "messages": [{"role": "user", "content": "hi"}], "stream": true})
            };
            let response = build_router(backend.clone())
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri(uri)
                        .header("content-type", "application/json")
                        .body(Body::from(body.to_string()))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
            let text = String::from_utf8_lossy(&bytes);
            assert!(text.contains("upstream request failed"), "{uri}: {text}");
            assert!(!text.contains(marker), "{uri}: {text}");
        }
    }

    #[test]
    fn unsupported_builtin_tools_fail_closed() {
        for kind in [
            "file_search",
            "computer_use_preview",
            "code_interpreter",
            "image_generation",
            "mcp",
            "local_shell",
            "web_search",
            "x_search",
        ] {
            let error = responses_to_chat(&json!({
                "model": "m",
                "input": "hi",
                "tools": [{"type": kind}],
            }))
            .unwrap_err();
            assert!(error.contains(kind), "missing kind in: {error}");
        }
        // Client-defined function tools keep flowing to the chat path.
        let request = responses_to_chat(&json!({
            "model": "m",
            "input": "hi",
            "tools": [{"type": "function", "name": "get_time"}],
        }))
        .unwrap();
        assert_eq!(
            request.tools.unwrap()[0]["function"]["name"],
            json!("get_time")
        );
    }

    #[test]
    fn upstream_statuses_are_preserved_for_clients() {
        assert_eq!(
            error_status(&GatewayError::Upstream(
                "upstream status 429: slow".to_string()
            )),
            StatusCode::TOO_MANY_REQUESTS
        );
        assert_eq!(
            error_status(&GatewayError::Upstream(
                "upstream status 404: missing".to_string()
            )),
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            error_status(&GatewayError::Upstream(
                "upstream status 503: down".to_string()
            )),
            StatusCode::BAD_GATEWAY
        );
    }

    #[tokio::test]
    async fn model_list_omits_hidden_combos() {
        let (router, db) = open_state();
        {
            let guard = db.connection();
            for (id, name, hidden) in [("visible", "visible", false), ("hidden", "hidden", true)] {
                heterion_router_db::repos::upsert_combo(
                    &guard,
                    &heterion_router_db::repos::ComboRow {
                        id: id.to_string(),
                        name: name.to_string(),
                        data: json!({"isHidden": hidden}).to_string(),
                        sort_order: 0,
                        active: true,
                    },
                )
                .unwrap();
            }
        }
        let response = router
            .oneshot(
                Request::builder()
                    .uri("/v1/models")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let value: Value = serde_json::from_slice(&bytes).unwrap();
        let ids: Vec<&str> = value["data"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|item| item["id"].as_str())
            .collect();
        assert!(ids.contains(&"visible"));
        assert!(!ids.contains(&"hidden"));
    }

    #[tokio::test]
    async fn chat_completions_returns_echo() {
        let body = json!({
            "model": "grok-4.6",
            "messages": [{"role": "user", "content": "hello"}]
        })
        .to_string();

        let response = test_router()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/chat/completions")
                    .header("content-type", "application/json")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let value: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(
            value["choices"][0]["message"]["content"],
            json!("echo: hello")
        );
    }

    #[tokio::test]
    async fn streaming_emits_content_then_done() {
        let body = json!({
            "model": "grok-4.6",
            "messages": [{"role": "user", "content": "hi"}],
            "stream": true
        })
        .to_string();

        let response = test_router()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/chat/completions")
                    .header("content-type", "application/json")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let text = String::from_utf8(bytes.to_vec()).unwrap();
        assert!(text.contains("echo:"));
        assert!(text.contains("\"finish_reason\":\"stop\""));
        assert!(text.contains("[DONE]"));
    }

    #[tokio::test]
    async fn streaming_usage_is_recorded() {
        let db = Arc::new(heterion_router_db::Db::open_in_memory().unwrap());
        db.migrate().unwrap();
        let router =
            build_router_with_state(AppState::with_db(Arc::new(UsageStreamBackend), db.clone()));
        let response = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/chat/completions")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({
                            "model": "grok-4.6",
                            "messages": [{"role": "user", "content": "hi"}],
                            "stream": true
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let _ = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let guard = db.connection();
        let usage: (i64, i64, String) = guard
            .query_row(
                "SELECT tokens_input, tokens_output, status FROM usage_history WHERE model = 'grok-4.6'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(usage, (3, 2, "succeeded".to_string()));
        let calls: i64 = guard
            .query_row(
                "SELECT COUNT(*) FROM call_logs WHERE request_type = 'chat' AND status = 200",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(calls, 1);
    }

    #[tokio::test]
    async fn usage_rows_carry_the_provider_that_actually_served_the_request() {
        // Two providers behind one routing backend. The stubs are shaped like
        // the real per-provider executor: `name()` is the generic "provider"
        // and the identity lives in `attribution()`.
        let db = Arc::new(heterion_router_db::Db::open_in_memory().unwrap());
        db.migrate().unwrap();
        let router =
            build_router_with_state(AppState::with_db(routing_backend(db.clone()), db.clone()));

        for model in ["openrouter/grok-4.6", "heterion-local/grok-4.6"] {
            let response = router
                .clone()
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri("/v1/chat/completions")
                        .header("content-type", "application/json")
                        .body(Body::from(
                            json!({"model": model, "messages": [{"role": "user", "content": "hi"}]})
                                .to_string(),
                        ))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK, "{model}");
        }

        let guard = db.connection();
        let usage = rows(
            &guard,
            "SELECT provider, connection_id, model FROM usage_history ORDER BY id",
        );
        assert_eq!(usage.len(), 2, "{usage:?}");
        assert_eq!(usage[0][0].as_deref(), Some("openrouter"), "{usage:?}");
        assert_eq!(usage[0][1].as_deref(), Some("conn-openrouter"), "{usage:?}");
        // `provider/model` requested, bare model served.
        assert_eq!(usage[0][2].as_deref(), Some("grok-4.6"), "{usage:?}");
        assert_eq!(usage[1][0].as_deref(), Some("heterion-local"), "{usage:?}");
        assert_eq!(
            usage[1][1].as_deref(),
            Some("conn-heterion-local"),
            "{usage:?}"
        );
        assert_eq!(usage[1][2].as_deref(), Some("grok-4.6"), "{usage:?}");

        let calls = rows(
            &guard,
            "SELECT provider, requested_model, model FROM call_logs ORDER BY rowid",
        );
        assert_eq!(calls.len(), 2, "{calls:?}");
        assert_eq!(calls[0][0].as_deref(), Some("openrouter"), "{calls:?}");
        assert_eq!(
            calls[0][1].as_deref(),
            Some("openrouter/grok-4.6"),
            "{calls:?}"
        );
        assert_eq!(calls[0][2].as_deref(), Some("grok-4.6"), "{calls:?}");
        assert_eq!(calls[1][0].as_deref(), Some("heterion-local"), "{calls:?}");
        assert_eq!(
            calls[1][1].as_deref(),
            Some("heterion-local/grok-4.6"),
            "{calls:?}"
        );
        assert_eq!(calls[1][2].as_deref(), Some("grok-4.6"), "{calls:?}");
    }

    /// A request that never reached a provider must not put one in the table.
    ///
    /// The router used to fall back to its own backend name, so every unknown
    /// model landed in `usage_history.provider` as `"routing"` — a non-provider
    /// that no `WHERE provider = ?` aggregate can ever exclude.
    #[tokio::test]
    async fn a_request_that_reached_no_provider_records_no_provider() {
        let db = Arc::new(heterion_router_db::Db::open_in_memory().unwrap());
        db.migrate().unwrap();
        let router =
            build_router_with_state(AppState::with_db(routing_backend(db.clone()), db.clone()));

        let response = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/chat/completions")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({"model": "no-such-provider/no-such-model",
                               "messages": [{"role": "user", "content": "hi"}]})
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);

        let guard = db.connection();
        let usage = rows(&guard, "SELECT provider, connection_id FROM usage_history");
        let calls = rows(&guard, "SELECT provider, connection_id FROM call_logs");
        assert_eq!(usage.len(), 1, "{usage:?}");
        assert_eq!(
            usage[0][0].as_deref(),
            None,
            "provider must be NULL: {usage:?}"
        );
        assert_eq!(usage[0][1].as_deref(), None, "{usage:?}");
        assert_eq!(calls.len(), 1, "{calls:?}");
        assert_eq!(
            calls[0][0].as_deref(),
            None,
            "provider must be NULL: {calls:?}"
        );
    }

    #[tokio::test]
    async fn combo_requests_record_the_combo_name_and_the_serving_provider() {
        let db = Arc::new(heterion_router_db::Db::open_in_memory().unwrap());
        db.migrate().unwrap();
        {
            let guard = db.connection();
            heterion_router_db::repos::upsert_combo(
                &guard,
                &heterion_router_db::repos::ComboRow {
                    id: "c1".to_string(),
                    name: "dual".to_string(),
                    data: json!({
                        "strategy": "priority",
                        "models": [
                            {"kind": "model", "model": "heterion-local/grok-4.6",
                             "providerId": "heterion-local"},
                        ],
                    })
                    .to_string(),
                    sort_order: 0,
                    active: true,
                },
            )
            .unwrap();
        }
        let router =
            build_router_with_state(AppState::with_db(routing_backend(db.clone()), db.clone()));

        let response = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/chat/completions")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({"model": "dual", "messages": [{"role": "user", "content": "hi"}]})
                            .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let guard = db.connection();
        let calls = rows(
            &guard,
            "SELECT provider, combo_name, requested_model, model FROM call_logs",
        );
        assert_eq!(calls.len(), 1, "{calls:?}");
        assert_eq!(calls[0][0].as_deref(), Some("heterion-local"), "{calls:?}");
        assert_eq!(calls[0][1].as_deref(), Some("dual"), "{calls:?}");
        assert_eq!(calls[0][2].as_deref(), Some("dual"), "{calls:?}");
        assert_eq!(calls[0][3].as_deref(), Some("grok-4.6"), "{calls:?}");
    }

    #[tokio::test]
    async fn an_unreadable_combo_store_is_not_reported_as_an_unknown_model() {
        let db = Arc::new(heterion_router_db::Db::open_in_memory().unwrap());
        db.migrate().unwrap();
        db.connection().execute_batch("DROP TABLE combos").unwrap();
        let router =
            build_router_with_state(AppState::with_db(routing_backend(db.clone()), db.clone()));

        // A combo name cannot be planned without the combo store: say the
        // store is unavailable instead of blaming the client's model id.
        let response = post_completions(router.clone(), None).await;
        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let text = String::from_utf8_lossy(&bytes);
        assert!(!text.contains("no such table"), "{text}");
        assert!(!text.contains("unknown model"), "{text}");

        // `provider/model` ids never consult the combo store, so they keep
        // working against a broken one.
        let served = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/chat/completions")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({"model": "openrouter/grok-4.6",
                               "messages": [{"role": "user", "content": "hi"}]})
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(served.status(), StatusCode::OK);
    }

    /// Streams one chunk with usage, then never finishes: the client hangs
    /// up mid-flight.
    #[derive(Clone)]
    struct HangingStreamBackend;

    #[async_trait::async_trait]
    impl ChatBackend for HangingStreamBackend {
        fn name(&self) -> &'static str {
            "hanging"
        }

        fn attribution(&self, _token: &str) -> backend::Attribution {
            backend::Attribution {
                provider: Some("heterion-local".to_string()),
                connection_id: Some("conn-heterion-local".to_string()),
                combo_name: None,
            }
        }

        async fn complete(
            &self,
            _request: ChatCompletionRequest,
        ) -> Result<ChatCompletionResponse, GatewayError> {
            Err(GatewayError::InvalidRequest("stream backend".into()))
        }

        fn stream(&self, _request: ChatCompletionRequest) -> backend::ChunkStream {
            futures::stream::unfold(0u8, |step| async move {
                match step {
                    0 => Some((
                        Ok(StreamChunk {
                            content: Some("partial".to_string()),
                            usage: Some(heterion_router_core::Usage {
                                prompt_tokens: 5,
                                completion_tokens: 2,
                                total_tokens: 7,
                            }),
                            ..Default::default()
                        }),
                        1,
                    )),
                    _ => std::future::pending().await,
                }
            })
            .boxed()
        }
    }

    #[tokio::test]
    async fn an_aborted_stream_is_still_accounted() {
        let db = Arc::new(heterion_router_db::Db::open_in_memory().unwrap());
        db.migrate().unwrap();
        let router = build_router_with_state(AppState::with_db(
            Arc::new(HangingStreamBackend),
            db.clone(),
        ));

        let mut body = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/chat/completions")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({"model": "heterion-local/grok-4.6",
                               "messages": [{"role": "user", "content": "hi"}],
                               "stream": true})
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap()
            .into_body();

        // One delta reaches the client, then the client disconnects: the
        // response body is dropped with the stream still open.
        assert!(body.frame().await.is_some(), "no delta reached the client");
        drop(body);

        let guard = db.connection();
        let usage = rows(
            &guard,
            "SELECT provider, connection_id, tokens_input, tokens_output, status, error_code \
             FROM usage_history",
        );
        assert_eq!(usage.len(), 1, "{usage:?}");
        assert_eq!(usage[0][0].as_deref(), Some("heterion-local"), "{usage:?}");
        assert_eq!(
            usage[0][1].as_deref(),
            Some("conn-heterion-local"),
            "{usage:?}"
        );
        assert_eq!(usage[0][2].as_deref(), Some("5"), "{usage:?}");
        assert_eq!(usage[0][3].as_deref(), Some("2"), "{usage:?}");
        assert_eq!(usage[0][4].as_deref(), Some("failed"), "{usage:?}");
        assert_eq!(usage[0][5].as_deref(), Some("stream_error"), "{usage:?}");

        let calls = rows(&guard, "SELECT status, error_summary FROM call_logs");
        assert_eq!(calls.len(), 1, "{calls:?}");
        assert_eq!(calls[0][0].as_deref(), Some("499"), "{calls:?}");
        assert_eq!(calls[0][1].as_deref(), Some("stream_error"), "{calls:?}");
    }

    pub(crate) fn open_state() -> (Router, Arc<heterion_router_db::Db>) {
        let db = Arc::new(heterion_router_db::Db::open_in_memory().unwrap());
        db.migrate().unwrap();
        let router = build_router_with_state(AppState::with_db(
            Arc::new(backend::EchoBackend),
            db.clone(),
        ));
        (router, db)
    }

    pub(crate) fn keyed_state() -> (Router, Arc<heterion_router_db::Db>) {
        let db = Arc::new(heterion_router_db::Db::open_in_memory().unwrap());
        db.migrate().unwrap();
        {
            let guard = db.connection();
            heterion_router_db::repos::insert_api_key(
                &guard,
                &heterion_router_db::repos::ApiKeyRow {
                    id: "k1".to_string(),
                    name: "test".to_string(),
                    key: "sk-test-123".to_string(),
                    allowed_models: "[\"*\"]".to_string(),
                    no_log: false,
                },
            )
            .unwrap();
        }
        let router = build_router_with_state(AppState::with_db(
            Arc::new(backend::EchoBackend),
            db.clone(),
        ));
        (router, db)
    }

    fn completion_body(model: &str) -> String {
        json!({
            "model": model,
            "messages": [{"role": "user", "content": "hi"}]
        })
        .to_string()
    }

    async fn post_completions(router: Router, key: Option<&str>) -> Response {
        let mut builder = Request::builder()
            .method("POST")
            .uri("/v1/chat/completions")
            .header("content-type", "application/json");
        if let Some(key) = key {
            builder = builder.header("authorization", format!("Bearer {key}"));
        }
        router
            .oneshot(
                builder
                    .body(Body::from(completion_body("grok-4.6")))
                    .unwrap(),
            )
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn auth_open_when_no_keys() {
        let (router, _) = open_state();
        let response = post_completions(router, None).await;
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn auth_blocks_anonymous_when_keys_exist() {
        let (router, _) = keyed_state();
        let response = post_completions(router, None).await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn auth_db_failure_does_not_open_the_gateway() {
        let db = Arc::new(heterion_router_db::Db::open_in_memory().unwrap());
        let router = build_router_with_state(AppState::with_db(Arc::new(backend::EchoBackend), db));
        let response = post_completions(router, None).await;
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn application_logs_require_authentication() {
        let (router, _) = keyed_state();
        let response = router
            .oneshot(
                Request::builder()
                    .uri("/api/logs/app")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn auth_blocks_wrong_key() {
        let (router, _) = keyed_state();
        let response = post_completions(router, Some("sk-wrong")).await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn restricted_key_cannot_administer_or_use_unlisted_models() {
        let db = Arc::new(heterion_router_db::Db::open_in_memory().unwrap());
        db.migrate().unwrap();
        {
            let guard = db.connection();
            heterion_router_db::repos::insert_full_api_key(
                &guard,
                &heterion_router_db::repos::NewApiKey {
                    id: "restricted".to_string(),
                    name: "restricted".to_string(),
                    key: "sk-restricted".to_string(),
                    key_prefix: "sk-restricted".to_string(),
                    key_hash: heterion_router_db::repos::api_key_hash("sk-restricted"),
                    allowed_models: "[\"safe/*\"]".to_string(),
                    scopes: "[\"self:usage\"]".to_string(),
                },
            )
            .unwrap();
        }
        let router = build_router_with_state(AppState::with_db(Arc::new(backend::EchoBackend), db));

        let admin = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/keys")
                    .header("authorization", "Bearer sk-restricted")
                    .header("content-type", "application/json")
                    .body(Body::from(json!({"name": "forbidden"}).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(admin.status(), StatusCode::FORBIDDEN);

        let completion = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/chat/completions")
                    .header("authorization", "Bearer sk-restricted")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({
                            "model": "other/grok-4.6",
                            "messages": [{"role": "user", "content": "hi"}]
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(completion.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn no_log_key_does_not_write_usage_or_call_logs() {
        let db = Arc::new(heterion_router_db::Db::open_in_memory().unwrap());
        db.migrate().unwrap();
        {
            let guard = db.connection();
            heterion_router_db::repos::insert_full_api_key(
                &guard,
                &heterion_router_db::repos::NewApiKey {
                    id: "private".to_string(),
                    name: "private".to_string(),
                    key: "sk-private".to_string(),
                    key_prefix: "sk-private".to_string(),
                    key_hash: heterion_router_db::repos::api_key_hash("sk-private"),
                    allowed_models: "[]".to_string(),
                    scopes: "[\"manage\"]".to_string(),
                },
            )
            .unwrap();
            guard
                .execute("UPDATE api_keys SET no_log = 1 WHERE id = 'private'", [])
                .unwrap();
        }
        let router = build_router_with_state(AppState::with_db(
            Arc::new(backend::EchoBackend),
            db.clone(),
        ));
        let response = post_completions(router, Some("sk-private")).await;
        assert_eq!(response.status(), StatusCode::OK);
        let guard = db.connection();
        let usage_count: i64 = guard
            .query_row("SELECT COUNT(*) FROM usage_history", [], |row| row.get(0))
            .unwrap();
        let call_count: i64 = guard
            .query_row("SELECT COUNT(*) FROM call_logs", [], |row| row.get(0))
            .unwrap();
        assert_eq!(usage_count, 0);
        assert_eq!(call_count, 0);
    }

    #[tokio::test]
    async fn auth_allows_valid_key_records_usage_and_prices() {
        let (router, db) = keyed_state();
        let response = post_completions(router, Some("sk-test-123")).await;
        assert_eq!(response.status(), StatusCode::OK);

        let cost = response
            .headers()
            .get(HEADER_COST)
            .expect("cost header")
            .to_str()
            .unwrap()
            .to_string();
        let cost_value: f64 = cost.parse().unwrap();
        assert!(cost_value > 0.0, "{cost}");

        let guard = db.connection();
        let (count, api_key_id): (i64, String) = guard
            .query_row(
                "SELECT COUNT(*), MAX(api_key_id) FROM usage_history WHERE model = 'grok-4.6'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(count, 1);
        assert_eq!(api_key_id, "k1");
    }

    /// A backend whose upstream payload reports cached and reasoning
    /// tokens, the way a cache-heavy provider replies.
    #[derive(Clone)]
    struct CachedUsageBackend {
        raw: Arc<Value>,
    }

    #[async_trait::async_trait]
    impl ChatBackend for CachedUsageBackend {
        fn name(&self) -> &'static str {
            "cached-usage"
        }

        async fn complete(
            &self,
            request: ChatCompletionRequest,
        ) -> Result<ChatCompletionResponse, GatewayError> {
            Ok(ChatCompletionResponse {
                id: ids::completion_id(),
                object: "chat.completion",
                created: ids::unix_seconds(),
                model: request.model,
                choices: vec![heterion_router_core::ChatChoice {
                    index: 0,
                    message: ChatMessage::plain("assistant".to_string(), "ok".to_string()),
                    finish_reason: "stop".to_string(),
                }],
                usage: heterion_router_core::Usage {
                    prompt_tokens: 1000,
                    completion_tokens: 500,
                    total_tokens: 1500,
                },
                raw: Some((*self.raw).clone()),
            })
        }

        fn stream(&self, _request: ChatCompletionRequest) -> backend::ChunkStream {
            Box::pin(futures::stream::empty())
        }
    }

    fn cached_usage_state(raw: Value) -> (Router, Arc<heterion_router_db::Db>) {
        let db = Arc::new(heterion_router_db::Db::open_in_memory().unwrap());
        db.migrate().unwrap();
        let router = build_router_with_state(AppState::with_db(
            Arc::new(CachedUsageBackend { raw: Arc::new(raw) }),
            db.clone(),
        ));
        (router, db)
    }

    /// `prompt_tokens_details.cached_tokens` is what a Chat Completions
    /// upstream sends (see `translate/src/responses.rs:476`).
    fn chat_completion_payload_with_cache() -> Value {
        json!({
            "usage": {
                "prompt_tokens": 1000,
                "completion_tokens": 500,
                "prompt_tokens_details": {"cached_tokens": 200},
                "completion_tokens_details": {"reasoning_tokens": 64},
            }
        })
    }

    #[tokio::test]
    async fn usage_row_carries_the_cache_and_reasoning_tokens_the_upstream_reported() {
        let (router, db) = cached_usage_state(chat_completion_payload_with_cache());
        let response = post_completions(router, None).await;
        assert_eq!(response.status(), StatusCode::OK);

        let guard = db.connection();
        let row: (i64, i64, i64) = guard
            .query_row(
                "SELECT tokens_cache_read, tokens_cache_creation, tokens_reasoning
                 FROM usage_history WHERE model = 'grok-4.6'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(
            row,
            (200, 0, 64),
            "upstream said 200 cached / 64 reasoning tokens"
        );
    }

    #[tokio::test]
    async fn cost_header_discounts_cached_input_instead_of_billing_it_full_price() {
        let (router, _db) = cached_usage_state(chat_completion_payload_with_cache());
        let response = post_completions(router, None).await;
        let cost: f64 = response
            .headers()
            .get(HEADER_COST)
            .expect("cost header")
            .to_str()
            .unwrap()
            .parse()
            .unwrap();
        // grok-4.6 per 1M: input 2.0, cached 0.5, output 6.0 →
        // 800*2.0 + 200*0.5 + 500*6.0 = 4700 → 0.0047. The hardcoded
        // zeros billed all 1000 input tokens full price: 0.005.
        assert_eq!(cost, 0.0047);
    }

    #[tokio::test]
    async fn a_plain_upstream_payload_prices_exactly_as_before() {
        let (router, _db) = cached_usage_state(json!({
            "usage": {"prompt_tokens": 1000, "completion_tokens": 500}
        }));
        let response = post_completions(router, None).await;
        let cost: f64 = response
            .headers()
            .get(HEADER_COST)
            .expect("cost header")
            .to_str()
            .unwrap()
            .parse()
            .unwrap();
        assert_eq!(cost, 0.005);
    }

    #[tokio::test]
    async fn healthz_stays_public_with_keys() {
        let (router, _) = keyed_state();
        let response = router
            .oneshot(
                Request::builder()
                    .uri("/healthz")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    // ---- upstream refusal inside a 200 body -----------------------------
    //
    // A provider that answers HTTP 200 with an error object has already
    // spent its status line: the router sees `GatewayError::Upstream` with
    // no `upstream status {code}` prefix, so the client gets a blanket 502
    // and the breaker reads a client-side refusal as an outage.

    /// Refuses every request from inside the body, the way a streaming
    /// upstream has to. The whole error object rides in the message on
    /// purpose: recovering the status needs the payload, and today's
    /// executors drop it (`openai.rs` forwards `error.message` alone).
    #[derive(Clone)]
    struct BodyErrorBackend {
        payload: Value,
        calls: Arc<AtomicUsize>,
    }

    #[async_trait::async_trait]
    impl ChatBackend for BodyErrorBackend {
        fn name(&self) -> &'static str {
            "body-error"
        }

        async fn complete(
            &self,
            _request: ChatCompletionRequest,
        ) -> Result<ChatCompletionResponse, GatewayError> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            Err(GatewayError::Upstream(self.payload.to_string()))
        }

        fn stream(&self, _request: ChatCompletionRequest) -> backend::ChunkStream {
            let payload = self.payload.to_string();
            futures::stream::once(async move { Err(GatewayError::Upstream(payload)) }).boxed()
        }
    }

    /// Gateway in front of one provider that always refuses with `payload`.
    /// A breaker `threshold` of 1 makes "the breaker tripped" observable as
    /// "the provider was asked exactly once".
    fn body_error_router(
        payload: Value,
        calls: Arc<AtomicUsize>,
        threshold: u32,
    ) -> (Router, Arc<heterion_router_db::Db>) {
        let db = Arc::new(heterion_router_db::Db::open_in_memory().unwrap());
        db.migrate().unwrap();
        let mut backends: HashMap<String, Arc<dyn ChatBackend>> = HashMap::new();
        backends.insert(
            "openrouter".to_string(),
            Arc::new(StatusAwareBackend::new(Arc::new(BodyErrorBackend {
                payload,
                calls: Arc::clone(&calls),
            }))),
        );
        let routing = routing::RoutingBackend::with_breaker_settings(
            backends,
            Planner::new(vec![], vec![]),
            threshold,
            3600,
        );
        (
            build_router_with_state(AppState::with_db(Arc::new(routing), db.clone())),
            db,
        )
    }

    /// POST the one request shape each surface wants; return status + body.
    async fn ask(router: Router, uri: &str) -> (StatusCode, String) {
        let body = if uri.ends_with("responses") {
            json!({"model": "openrouter/grok-4.6", "input": "hi"})
        } else {
            json!({"model": "openrouter/grok-4.6",
                   "messages": [{"role": "user", "content": "hi"}]})
        };
        let response = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(uri)
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        (status, String::from_utf8_lossy(&bytes).to_string())
    }

    #[tokio::test]
    async fn an_upstream_4xx_inside_a_200_body_reaches_the_client_as_itself() {
        for (payload, expected, marker) in [
            (
                json!({"error": {"message": "Rate limit reached for gpt-4o",
                                 "type": "rate_limit_error",
                                 "code": "rate_limit_exceeded"}}),
                StatusCode::TOO_MANY_REQUESTS,
                "Rate limit reached",
            ),
            (
                json!({"error": {"message": "unsupported parameter `stream`",
                                 "status": 400}}),
                StatusCode::BAD_REQUEST,
                "unsupported parameter",
            ),
        ] {
            let calls = Arc::new(AtomicUsize::new(0));
            let (router, _db) = body_error_router(payload.clone(), Arc::clone(&calls), 1);
            for uri in ["/v1/chat/completions", "/v1/responses"] {
                let (status, text) = ask(router.clone(), uri).await;
                assert_eq!(status, expected, "{uri} carrying {payload}");
                // Sanitized: the client is told the status, never the body.
                assert!(text.contains("upstream request failed"), "{uri}: {text}");
                assert!(!text.contains(marker), "{uri} leaked: {text}");
            }
            // Frozen contract (AGENTS.md): a provider 4xx must not trip the
            // breaker, so the provider is still asked after two refusals.
            let (status, _text) = ask(router.clone(), "/v1/chat/completions").await;
            assert_eq!(status, expected, "later request must not see a cooldown");
            assert_eq!(
                calls.load(Ordering::Relaxed),
                3,
                "a client-side 4xx tripped the breaker"
            );
        }
    }

    #[tokio::test]
    async fn an_upstream_5xx_or_unrecoverable_refusal_stays_502_and_trips_the_breaker() {
        for payload in [
            json!({"error": {"message": "upstream exploded", "status": 500}}),
            json!({"error": {"message": "something went wrong"}}),
        ] {
            let calls = Arc::new(AtomicUsize::new(0));
            let (router, _db) = body_error_router(payload.clone(), Arc::clone(&calls), 1);
            for _ in 0..3 {
                let (status, text) = ask(router.clone(), "/v1/chat/completions").await;
                assert_eq!(status, StatusCode::BAD_GATEWAY, "{payload}");
                assert!(
                    text.contains("upstream request failed"),
                    "{payload}: {text}"
                );
            }
            assert_eq!(
                calls.load(Ordering::Relaxed),
                1,
                "a 5xx must still trip the breaker: {payload}"
            );
        }
    }

    #[tokio::test]
    async fn a_streaming_refusal_names_the_recovered_status_it_cannot_send() {
        // The 200 is already spent when the refusal arrives mid-SSE, so the
        // status has to ride in the error frame — and be booked as such.
        let calls = Arc::new(AtomicUsize::new(0));
        let (router, db) = body_error_router(
            json!({"error": {"message": "slow down", "code": "rate_limit_exceeded"}}),
            Arc::clone(&calls),
            1,
        );
        let (status, text) = ask_stream(router, "/v1/chat/completions").await;
        assert_eq!(status, StatusCode::OK);
        assert!(text.contains("\"status\":429"), "{text}");
        assert!(!text.contains("slow down"), "leaked: {text}");

        let logs = rows(&db.connection(), "SELECT status FROM call_logs");
        assert_eq!(logs[0][0].as_deref(), Some("429"), "{logs:?}");
    }

    /// POST a streaming request to `uri` and drain the SSE body.
    async fn ask_stream(router: Router, uri: &str) -> (StatusCode, String) {
        let body = if uri.ends_with("responses") {
            json!({"model": "openrouter/grok-4.6", "input": "hi", "stream": true})
        } else {
            json!({"model": "openrouter/grok-4.6",
                   "messages": [{"role": "user", "content": "hi"}], "stream": true})
        };
        let response = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(uri)
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        (status, String::from_utf8_lossy(&bytes).to_string())
    }

    #[test]
    fn an_upstream_status_is_recovered_from_the_error_payload() {
        for (message, expected) in [
            ("upstream status 429: slow", Some(429)),
            (r#"{"error":{"status":429,"message":"slow"}}"#, Some(429)),
            (r#"{"error":{"code":"rate_limit_exceeded"}}"#, Some(429)),
            (r#"{"error":{"code":"insufficient_quota"}}"#, Some(429)),
            (r#"{"error":{"code":"invalid_request_error"}}"#, Some(400)),
            (r#"{"error":{"code":"permission_error"}}"#, Some(403)),
            (r#"{"error":{"code":"authentication_error"}}"#, Some(401)),
            (r#"{"error":{"code":"not_found_error"}}"#, Some(404)),
            (r#"{"status":402}"#, Some(402)),
            (r#"{"error":{"http_status":418}}"#, Some(418)),
            ("provider: all credentials failed", None),
            (r#"{"error":{"message":"no code here"}}"#, None),
            (r#"{"error":{"code":"weird_code"}}"#, None),
            (r#"{"error":{"status":"nine hundred"}}"#, None),
            (r#"{"error":{"status":99}}"#, None),
            ("upstream stream ended before terminal event", None),
        ] {
            assert_eq!(
                recovered_upstream_status(&GatewayError::Upstream(message.to_string())),
                expected,
                "{message}"
            );
        }
        // Only an upstream failure carries a status: our own rejections and
        // unknown models must never borrow one.
        assert_eq!(
            recovered_upstream_status(&GatewayError::InvalidRequest(
                r#"{"error":{"status":429}}"#.to_string()
            )),
            None
        );
        assert_eq!(
            recovered_upstream_status(&GatewayError::UnknownModel(r#"{"status":429}"#.to_string())),
            None
        );
    }

    #[test]
    fn recovering_a_status_never_resanitizes_into_a_502() {
        // The recovered status must survive into the spellings the breaker
        // and the dashboards parse, and stay absent when nothing is known.
        let recovered = with_recovered_status(GatewayError::Upstream(
            r#"{"error":{"status":429,"message":"raw upstream text"}}"#.to_string(),
        ));
        let recovered_message = match &recovered {
            GatewayError::Upstream(message) => message.clone(),
            other => panic!("expected an upstream error, got {other:?}"),
        };
        assert!(
            recovered_message.starts_with("upstream status 429: "),
            "{recovered_message}"
        );
        assert!(
            recovered_message.contains("raw upstream text"),
            "diagnostics must survive: {recovered_message}"
        );
        assert_eq!(recovered_upstream_status(&recovered), Some(429));
        // Already spelled, and unsalvageable: both left exactly as they were.
        let spelled = GatewayError::Upstream("upstream status 503: down".to_string());
        assert_eq!(with_recovered_status(spelled.clone()), spelled);
        let opaque = GatewayError::Upstream("transport error".to_string());
        assert_eq!(with_recovered_status(opaque.clone()), opaque);
        // An executor that forwards a whole error object must not be able to
        // grow the message without bound.
        let huge = with_recovered_status(GatewayError::Upstream(format!(
            r#"{{"error":{{"status":429,"message":"{}"}}}}"#,
            "y".repeat(100_000)
        )));
        let huge_message = match &huge {
            GatewayError::Upstream(message) => message.len(),
            other => panic!("expected an upstream error, got {other:?}"),
        };
        assert!(
            huge_message < 600,
            "{huge_message} bytes kept in one upstream message"
        );
        // Wrapping the client payload stays sanitized either way.
        let payload = sanitized_error_payload(&recovered);
        assert_eq!(
            payload["error"]["message"],
            json!("upstream request failed")
        );
        assert_eq!(payload["error"]["status"], json!(429));
        assert!(!payload.to_string().contains("raw upstream text"));
        assert!(
            sanitized_error_payload(&opaque)
                .get("error")
                .unwrap()
                .get("status")
                .is_none()
        );
    }

    // ---- M1.1: the columns the gateway can actually fill --------------------

    /// Columns the gateway can read off the request it just served.
    #[tokio::test]
    async fn call_logs_carry_the_columns_the_gateway_can_know() {
        let db = Arc::new(heterion_router_db::Db::open_in_memory().unwrap());
        db.migrate().unwrap();
        let router =
            build_router_with_state(AppState::with_db(routing_backend(db.clone()), db.clone()));
        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/chat/completions")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({
                            "model": "heterion-local/grok-4.6",
                            "messages": [
                                {"role": "system", "content": "be brief"},
                                {"role": "user", "content": "hi"},
                            ],
                            "tools": [{"type": "function", "function": {"name": "get_time"}}],
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let guard = db.connection();
        let calls = rows(
            &guard,
            "SELECT id, provider, connection_id, correlation_id, request_summary,
                    source_format, target_format, tokens_cache_read, tokens_reasoning
             FROM call_logs",
        );
        assert_eq!(calls.len(), 1, "{calls:?}");
        let row = &calls[0];
        assert_eq!(row[1].as_deref(), Some("heterion-local"), "{calls:?}");
        assert_eq!(
            row[2].as_deref(),
            Some("conn-heterion-local"),
            "connection_id comes from the backend that answered: {calls:?}"
        );
        assert_eq!(
            row[3].as_deref(),
            row[0].as_deref(),
            "correlation_id must be the row id an operator greps for: {calls:?}"
        );
        assert_eq!(row[4].as_deref(), Some("2 messages, 1 tools"), "{calls:?}");
        assert_eq!(row[5].as_deref(), Some("openai"), "{calls:?}");
        // `heterion-local` is not in the shipped registry, so no target format
        // is known — NULL, not a guess.
        assert_eq!(row[6].as_deref(), None, "{calls:?}");
        assert_eq!(row[7].as_deref(), Some("0"), "{calls:?}");
        assert_eq!(row[8].as_deref(), Some("0"), "{calls:?}");
    }

    /// `target_format` is read out of the registry, not re-derived.
    #[tokio::test]
    async fn call_logs_take_the_target_format_from_the_registry() {
        let db = Arc::new(heterion_router_db::Db::open_in_memory().unwrap());
        db.migrate().unwrap();
        let registry = Arc::new(ProviderRegistry::load());
        // `grok-cli` is the one entry the shipped registry declares a model
        // for that `AttributedBackend` can answer as.
        let router = build_router_with_state(
            AppState::with_db(
                Arc::new(AttributedBackend {
                    provider: "grok-cli",
                    connection: "conn-grok-cli",
                }),
                db.clone(),
            )
            .with_catalog(registry.clone(), vec!["grok-cli".to_string()]),
        );
        let response = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/chat/completions")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({"model": "grok-4.6",
                               "messages": [{"role": "user", "content": "hi"}]})
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let calls = rows(
            &db.connection(),
            "SELECT provider, source_format, target_format FROM call_logs",
        );
        assert_eq!(calls.len(), 1, "{calls:?}");
        assert_eq!(calls[0][0].as_deref(), Some("grok-cli"), "{calls:?}");
        assert_eq!(calls[0][1].as_deref(), Some("openai"), "{calls:?}");
        assert_eq!(
            calls[0][2].as_deref(),
            registry.model_target_format("grok-cli", "grok-4.6"),
            "target_format must be whatever the registry says: {calls:?}"
        );
    }

    /// A response path stores the format the client spoke.
    #[tokio::test]
    async fn a_responses_call_records_its_own_source_format() {
        let (router, db) = open_state();
        let response = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/responses")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({"model": "grok-4.6", "input": "hi"}).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let calls = rows(
            &db.connection(),
            "SELECT source_format, request_type FROM call_logs",
        );
        assert_eq!(
            calls,
            vec![vec![
                Some("openai-responses".to_string()),
                Some("responses".to_string())
            ]],
            "{calls:?}"
        );
    }

    /// The columns no feature in this build produces stay at the schema's
    /// NULL/default — see the SHARED NOTE on `record_completion_usage`.
    #[tokio::test]
    async fn the_columns_no_feature_produces_stay_null() {
        let (router, db) = open_state();
        let response = post_completions(router, None).await;
        assert_eq!(response.status(), StatusCode::OK);

        let guard = db.connection();
        let calls = rows(
            &guard,
            "SELECT account, combo_step_id, combo_execution_key, artifact_relpath,
                    artifact_size_bytes, artifact_sha256, has_request_body,
                    has_response_body, has_pipeline_details, cache_source,
                    detail_state, tokens_compressed
             FROM call_logs",
        );
        assert_eq!(calls.len(), 1, "{calls:?}");
        for (index, column) in [
            (0, "account"),
            (1, "combo_step_id"),
            (2, "combo_execution_key"),
            (3, "artifact_relpath"),
            (4, "artifact_size_bytes"),
            (5, "artifact_sha256"),
            (11, "tokens_compressed"),
        ] {
            assert_eq!(calls[0][index].as_deref(), None, "{column}: {calls:?}");
        }
        for (index, column) in [
            (6, "has_request_body"),
            (7, "has_response_body"),
            (8, "has_pipeline_details"),
        ] {
            assert_eq!(calls[0][index].as_deref(), Some("0"), "{column}: {calls:?}");
        }
        assert_eq!(calls[0][9].as_deref(), Some("upstream"), "{calls:?}");
        assert_eq!(calls[0][10].as_deref(), Some("none"), "{calls:?}");

        let usage = rows(
            &guard,
            "SELECT account_key, account_label, account_label_priority, service_tier, ttft_ms
             FROM usage_history",
        );
        assert_eq!(usage.len(), 1, "{usage:?}");
        assert_eq!(usage[0][0].as_deref(), None, "{usage:?}");
        assert_eq!(usage[0][1].as_deref(), None, "{usage:?}");
        assert_eq!(usage[0][2].as_deref(), Some("0"), "{usage:?}");
        assert_eq!(usage[0][3].as_deref(), Some("standard"), "{usage:?}");
    }

    /// The summary is a size, never the prompt: an agent turn can carry a
    /// megabyte of context and none of it may land in `call_logs`.
    #[tokio::test]
    async fn request_summary_is_a_count_and_never_the_body() {
        let marker = "CANARY-4b1e-do-not-store-this-prompt";
        let huge = "y".repeat(64 * 1024);
        let (router, db) = open_state();
        let response = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/chat/completions")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({
                            "model": "grok-4.6",
                            "messages": [
                                {"role": "user", "content": marker},
                                {"role": "user", "content": huge},
                            ],
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let guard = db.connection();
        let summary: String = guard
            .query_row(
                "SELECT COALESCE(request_summary, '') FROM call_logs",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(summary, "2 messages, 0 tools");
        assert!(!summary.contains(marker), "{summary}");
        assert!(summary.len() < 64, "{summary}");
        // Nothing anywhere in the row keeps the prompt.
        let stored: String = guard
            .query_row(
                "SELECT COALESCE(COALESCE(request_summary, '') || COALESCE(error_summary, '')
                        || COALESCE(artifact_relpath, ''), '') FROM call_logs",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(!stored.contains(marker), "prompt leaked into call_logs");
        assert!(
            !stored.contains(&"y".repeat(1024)),
            "prompt leaked into call_logs"
        );
    }

    /// Streams its first delta only after a real delay, so "time to first
    /// token" is measurable instead of rounding to 0 ms.
    #[derive(Clone)]
    struct SlowFirstChunkBackend;

    #[async_trait::async_trait]
    impl ChatBackend for SlowFirstChunkBackend {
        fn name(&self) -> &'static str {
            "slow-first-chunk"
        }

        async fn complete(
            &self,
            _request: ChatCompletionRequest,
        ) -> Result<ChatCompletionResponse, GatewayError> {
            Err(GatewayError::InvalidRequest("stream backend".into()))
        }

        fn stream(&self, _request: ChatCompletionRequest) -> backend::ChunkStream {
            futures::stream::unfold(0u8, |step| async move {
                match step {
                    0 => {
                        tokio::time::sleep(std::time::Duration::from_millis(120)).await;
                        Some((Ok(StreamChunk::content("ok")), 1))
                    }
                    1 => Some((
                        Ok(StreamChunk {
                            finish_reason: Some("stop".to_string()),
                            usage: Some(heterion_router_core::Usage {
                                prompt_tokens: 3,
                                completion_tokens: 2,
                                total_tokens: 5,
                            }),
                            ..Default::default()
                        }),
                        2,
                    )),
                    _ => None,
                }
            })
            .boxed()
        }
    }

    #[tokio::test]
    async fn a_stream_measures_time_to_first_token_and_a_completion_never_does() {
        let db = Arc::new(heterion_router_db::Db::open_in_memory().unwrap());
        db.migrate().unwrap();
        let router = build_router_with_state(AppState::with_db(
            Arc::new(SlowFirstChunkBackend),
            db.clone(),
        ));
        let response = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/chat/completions")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({"model": "grok-4.6",
                               "messages": [{"role": "user", "content": "hi"}],
                               "stream": true})
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let _ = to_bytes(response.into_body(), usize::MAX).await.unwrap();

        let usage: (i64, i64) = db
            .connection()
            .query_row("SELECT ttft_ms, latency_ms FROM usage_history", [], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })
            .unwrap();
        assert!(
            usage.0 >= 100,
            "first token arrived after ~120ms, not {}ms: {usage:?}",
            usage.0
        );
        assert!(
            usage.0 <= usage.1,
            "ttft must not exceed latency: {usage:?}"
        );

        // A non-streaming completion has no first token, and says so with 0
        // rather than repeating its latency.
        let (router, db) = open_state();
        let response = post_completions(router, None).await;
        assert_eq!(response.status(), StatusCode::OK);
        let ttft: i64 = db
            .connection()
            .query_row("SELECT ttft_ms FROM usage_history", [], |row| row.get(0))
            .unwrap();
        assert_eq!(ttft, 0);
    }

    /// The cache-token work already computes the discounted counts for
    /// `usage_history`; the call log must not be the only table missing them.
    #[tokio::test]
    async fn call_logs_mirror_the_cache_and_reasoning_tokens() {
        let (router, db) = cached_usage_state(chat_completion_payload_with_cache());
        let response = post_completions(router, None).await;
        assert_eq!(response.status(), StatusCode::OK);

        let guard = db.connection();
        let calls = rows(
            &guard,
            "SELECT tokens_in, tokens_out, tokens_cache_read, tokens_cache_creation,
                    tokens_reasoning FROM call_logs",
        );
        assert_eq!(calls.len(), 1, "{calls:?}");
        assert_eq!(calls[0][2].as_deref(), Some("200"), "{calls:?}");
        assert_eq!(calls[0][3].as_deref(), Some("0"), "{calls:?}");
        assert_eq!(calls[0][4].as_deref(), Some("64"), "{calls:?}");
    }

    // ---- M1.5: cost headers on proxied responses ---------------------------

    #[tokio::test]
    async fn a_successful_proxied_response_carries_the_router_cost_headers() {
        let (router, _db) = cached_usage_state(chat_completion_payload_with_cache());
        let response = post_completions(router, None).await;
        assert_eq!(response.status(), StatusCode::OK);
        let headers = response.headers();
        assert_eq!(
            headers.get("x-router-cost").and_then(|v| v.to_str().ok()),
            Some("0.004700"),
            "cost as a plain decimal with the six places the price table documents"
        );
        assert_eq!(
            headers
                .get("x-router-provider")
                .and_then(|v| v.to_str().ok()),
            Some("cached-usage"),
            "the provider that actually answered"
        );
        assert_eq!(
            headers
                .get("x-router-model-used")
                .and_then(|v| v.to_str().ok()),
            Some("grok-4.6"),
            "the resolved model, not the requested one"
        );
    }

    #[tokio::test]
    async fn an_error_response_carries_no_router_cost_headers() {
        let backend = Arc::new(FailingBackend {
            message: "upstream status 500: private body CANARY-1d2e".to_string(),
        });
        let response = build_router(backend)
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/chat/completions")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({"model": "grok-4.6",
                               "messages": [{"role": "user", "content": "hi"}]})
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
        for header in ["x-router-cost", "x-router-provider", "x-router-model-used"] {
            assert!(
                response.headers().get(header).is_none(),
                "{header} must not describe a response that was never served"
            );
        }
        // And no upstream text anywhere in the header block.
        let all = format!("{:?}", response.headers());
        assert!(!all.contains("CANARY-1d2e"), "{all}");
    }

    /// A keyless provider is served exactly like an authenticated one: no
    /// credential means no reason to withhold what the call cost.
    #[tokio::test]
    async fn a_keyless_backend_still_gets_the_router_cost_headers() {
        let response = post_completions(test_router(), None).await;
        assert_eq!(response.status(), StatusCode::OK);
        let headers = response.headers();
        assert_eq!(
            headers.get("x-router-cost").and_then(|v| v.to_str().ok()),
            Some("0.000052"),
            "2 prompt + 8 completion tokens on grok-4.6"
        );
        assert_eq!(
            headers
                .get("x-router-provider")
                .and_then(|v| v.to_str().ok()),
            Some("echo")
        );
        assert_eq!(
            headers
                .get("x-router-model-used")
                .and_then(|v| v.to_str().ok()),
            Some("grok-4.6")
        );
    }

    /// An unpriced model is reported as unknown, not as a fabricated zero.
    #[tokio::test]
    async fn an_unpriced_model_gets_no_cost_header_rather_than_a_fake_zero() {
        let (router, _db) = cached_usage_state(json!({"usage": {"prompt_tokens": 1}}));
        let response = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/chat/completions")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({"model": "no-such-model",
                               "messages": [{"role": "user", "content": "hi"}]})
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert!(
            response.headers().get("x-router-cost").is_none(),
            "an unknown price is unknown, never 0.000000"
        );
        assert_eq!(
            response
                .headers()
                .get("x-router-model-used")
                .and_then(|v| v.to_str().ok()),
            Some("no-such-model"),
            "the model that answered is still worth reporting"
        );
    }

    // ---- M2.5: JSON rejections use the error envelope ---------------------

    #[tokio::test]
    async fn a_malformed_json_body_is_answered_with_the_error_envelope() {
        let marker = "CANARY-7a3b-raw-body";
        for (uri, body) in [
            (
                "/v1/chat/completions",
                "{\"model\": \"grok-4.6\", \"messages\":".to_string(),
            ),
            (
                "/v1/responses",
                format!("{{\"model\": \"grok-4.6\", \"input\": \"{marker}\",}}"),
            ),
        ] {
            let response = build_router(Arc::new(backend::EchoBackend))
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri(uri)
                        .header("content-type", "application/json")
                        .body(Body::from(body.clone()))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{uri}");
            let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
            let value: Value = serde_json::from_slice(&bytes).unwrap_or_else(|error| {
                panic!(
                    "{uri} answered non-JSON ({error}): {}",
                    String::from_utf8_lossy(&bytes)
                )
            });
            let detail = &value["error"];
            assert!(
                detail.is_object(),
                "{uri} must answer the project envelope: {value}"
            );
            assert!(
                detail["message"].as_str().is_some_and(|m| !m.is_empty()),
                "{uri}: {value}"
            );
            let text = String::from_utf8_lossy(&bytes);
            assert!(!text.contains(marker), "{uri} echoed the body: {text}");
        }
    }

    /// A body that parses but does not fit the endpoint is our own 422, not
    /// axum's — and never quotes the value that failed.
    #[tokio::test]
    async fn a_well_formed_body_of_the_wrong_shape_uses_the_envelope_too() {
        let marker = "CANARY-8c4d-wrong-shape";
        let response = build_router(Arc::new(backend::EchoBackend))
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/chat/completions")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({"model": 42, "messages": marker}).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let value: Value = serde_json::from_slice(&bytes).unwrap();
        assert!(value["error"]["message"].is_string(), "{value}");
        assert!(
            !String::from_utf8_lossy(&bytes).contains(marker),
            "the offending value was echoed back"
        );
    }
}

#[cfg(test)]
mod responses_tests {
    use super::*;
    use axum::body::{Body, to_bytes};
    use axum::http::Request;
    use futures::StreamExt;
    use tower::ServiceExt;

    fn responses_router() -> Router {
        build_router(Arc::new(backend::EchoBackend))
    }

    #[derive(Clone)]
    struct CaptureBackend {
        requests: Arc<std::sync::Mutex<Vec<ChatCompletionRequest>>>,
    }

    #[async_trait::async_trait]
    impl ChatBackend for CaptureBackend {
        fn name(&self) -> &'static str {
            "capture"
        }

        async fn complete(
            &self,
            request: ChatCompletionRequest,
        ) -> Result<heterion_router_core::ChatCompletionResponse, GatewayError> {
            self.requests.lock().unwrap().push(request.clone());
            let text = request
                .messages
                .iter()
                .filter(|message| message.role == "user")
                .map(ChatMessage::text)
                .collect::<Vec<_>>()
                .join("|");
            Ok(heterion_router_core::ChatCompletionResponse {
                id: "chatcmpl-capture".to_string(),
                object: "chat.completion",
                created: ids::unix_seconds(),
                model: request.model,
                choices: vec![heterion_router_core::ChatChoice {
                    index: 0,
                    message: ChatMessage::plain("assistant".to_string(), text),
                    finish_reason: "stop".to_string(),
                }],
                usage: heterion_router_core::Usage::default(),
                raw: None,
            })
        }

        fn stream(&self, _request: ChatCompletionRequest) -> backend::ChunkStream {
            futures::stream::empty().boxed()
        }
    }

    /// A backend that hands over an upstream payload verbatim, so the cache
    /// detail in it survives all the way to the usage row.
    #[derive(Clone)]
    struct RawBackend {
        raw: Value,
    }

    #[async_trait::async_trait]
    impl ChatBackend for RawBackend {
        fn name(&self) -> &'static str {
            "raw"
        }

        async fn complete(
            &self,
            request: ChatCompletionRequest,
        ) -> Result<ChatCompletionResponse, GatewayError> {
            Ok(ChatCompletionResponse {
                id: ids::completion_id(),
                object: "chat.completion",
                created: ids::unix_seconds(),
                model: request.model,
                choices: vec![heterion_router_core::ChatChoice {
                    index: 0,
                    message: ChatMessage::plain("assistant".to_string(), "ok".to_string()),
                    finish_reason: "stop".to_string(),
                }],
                usage: heterion_router_core::Usage {
                    prompt_tokens: 1000,
                    completion_tokens: 500,
                    total_tokens: 1500,
                },
                raw: Some(self.raw.clone()),
            })
        }

        fn stream(&self, _request: ChatCompletionRequest) -> backend::ChunkStream {
            Box::pin(futures::stream::empty())
        }
    }

    #[test]
    fn responses_request_translates_to_chat_shape() {
        let request = responses_to_chat(&json!({
            "model": "grok-4.6",
            "instructions": "be brief",
            "input": [
                {"type": "message", "role": "user",
                 "content": [{"type": "input_text", "text": "hi"}]},
                {"type": "message", "role": "assistant",
                 "content": [{"type": "output_text", "text": "hello"}]},
                {"type": "function_call", "call_id": "call_1", "name": "get_time",
                 "arguments": "{\"city\":\"SF\"}"},
                {"type": "function_call_output", "call_id": "call_1", "output": "12:00"},
                {"type": "reasoning", "summary": "skipped-kind"},
            ],
            "max_output_tokens": 50,
            "reasoning": {"effort": "high"},
            "store": true,
            "previous_response_id": "resp_prev",
            "parallel_tool_calls": false,
            "metadata": {"trace": "t1"},
            "tools": [{"type": "function", "name": "get_time",
                       "description": "clock", "parameters": {"type": "object"}}],
            "stream": true,
        }))
        .unwrap();

        assert_eq!(request.model, "grok-4.6");
        assert!(request.stream);
        assert_eq!(request.max_tokens, Some(50));
        assert_eq!(request.reasoning_effort.as_deref(), Some("high"));
        assert_eq!(request.parallel_tool_calls, Some(false));
        assert_eq!(request.metadata.as_ref().unwrap()["trace"], json!("t1"));
        assert_eq!(request.extra["store"], json!(true));
        assert_eq!(request.extra["previous_response_id"], json!("resp_prev"));
        let roles: Vec<&str> = request.messages.iter().map(|m| m.role.as_str()).collect();
        assert_eq!(roles, ["system", "user", "assistant", "assistant", "tool"]);
        assert_eq!(request.messages[0].text(), "be brief");
        assert_eq!(request.messages[1].text(), "hi");
        let calls = request.messages[3].tool_calls.as_ref().unwrap();
        assert_eq!(calls[0]["function"]["name"], json!("get_time"));
        assert_eq!(request.messages[4].tool_call_id.as_deref(), Some("call_1"));
        assert_eq!(request.messages[4].text(), "12:00");
        let tools = request.tools.unwrap();
        assert_eq!(tools[0]["function"]["name"], json!("get_time"));
        assert_eq!(tools[0]["function"]["description"], json!("clock"));
    }

    #[test]
    fn responses_string_input_becomes_a_user_message() {
        let request = responses_to_chat(&json!({"model": "m", "input": "plain text"})).unwrap();
        assert_eq!(request.messages.len(), 1);
        assert_eq!(request.messages[0].role, "user");
        assert_eq!(request.messages[0].text(), "plain text");
    }

    #[test]
    fn responses_request_without_model_or_input_is_rejected() {
        assert!(responses_to_chat(&json!({"input": "hi"})).is_err());
        assert!(responses_to_chat(&json!({"model": "m"})).is_err());
        assert!(responses_to_chat(&json!({"model": "m", "input": []})).is_err());
    }

    /// The `/v1/responses` handler books the same cache detail the chat
    /// handler does: `usage.input_tokens_details.cached_tokens`,
    /// the spelling the Responses fixtures carry.
    #[tokio::test]
    async fn responses_records_cache_and_reasoning_tokens_from_the_upstream_payload() {
        let db = std::sync::Arc::new(heterion_router_db::Db::open_in_memory().unwrap());
        db.migrate().unwrap();
        let raw = json!({
            "id": "resp_cached",
            "object": "response",
            "status": "completed",
            "model": "grok-4.6",
            "usage": {
                "input_tokens": 1000,
                "output_tokens": 500,
                "total_tokens": 1500,
                "input_tokens_details": {"cached_tokens": 200},
                "output_tokens_details": {"reasoning_tokens": 64},
            }
        });
        let router = build_router_with_state(AppState::with_db(
            std::sync::Arc::new(RawBackend { raw }),
            db.clone(),
        ));
        let response = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/responses")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({"model": "grok-4.6", "input": "hi"}).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let cost: f64 = response
            .headers()
            .get(HEADER_COST)
            .expect("cost header")
            .to_str()
            .unwrap()
            .parse()
            .unwrap();
        assert_eq!(cost, 0.0047, "cached input must be discounted here too");

        let guard = db.connection();
        let row: (i64, i64, i64) = guard
            .query_row(
                "SELECT tokens_cache_read, tokens_cache_creation, tokens_reasoning
                 FROM usage_history WHERE model = 'grok-4.6'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(row, (200, 0, 64));
    }

    #[tokio::test]
    async fn responses_returns_a_response_object() {
        let response = responses_router()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/responses")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({"model": "grok-4.6", "input": "hi"}).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let value: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(value["object"], "response");
        assert_eq!(value["status"], "completed");
        assert_eq!(value["output"][0]["content"][0]["text"], json!("echo: hi"));
        assert!(
            value["usage"]["total_tokens"].as_u64().unwrap_or(0) > 0,
            "usage translated through: {value}"
        );
    }

    #[tokio::test]
    async fn responses_streaming_emits_deltas_then_completed() {
        let response = responses_router()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/responses")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({"model": "grok-4.6", "input": "hi", "stream": true}).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let text = String::from_utf8(bytes.to_vec()).unwrap();
        assert!(text.contains("event: response.output_text.delta"));
        assert!(text.contains("echo:"));
        assert!(text.contains("event: response.completed"));
        assert!(text.contains("\"finish_reason\":\"stop\""));
    }

    #[tokio::test]
    async fn stored_responses_can_be_retrieved_and_deleted() {
        let (router, _) = crate::tests::open_state();
        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/responses")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({"model": "grok-4.6", "input": "hi", "store": true}).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let value: Value = serde_json::from_slice(&bytes).unwrap();
        let id = value["id"].as_str().unwrap();

        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/v1/responses/{id}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let retrieved: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(retrieved["id"], value["id"]);

        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri(format!("/v1/responses/{id}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let deleted: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(deleted["deleted"], json!(true));

        let response = router
            .oneshot(
                Request::builder()
                    .uri(format!("/v1/responses/{id}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn store_false_does_not_create_retrievable_state() {
        let (router, _) = crate::tests::open_state();
        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/responses")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({"model": "grok-4.6", "input": "hi", "store": false}).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let value: Value = serde_json::from_slice(&bytes).unwrap();
        let id = value["id"].as_str().unwrap();
        let response = router
            .oneshot(
                Request::builder()
                    .uri(format!("/v1/responses/{id}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn previous_response_id_replays_stored_input_after_restart_state() {
        let requests = Arc::new(std::sync::Mutex::new(Vec::new()));
        let db = Arc::new(heterion_router_db::Db::open_in_memory().unwrap());
        db.migrate().unwrap();
        let first_router = build_router_with_state(AppState::with_db(
            Arc::new(CaptureBackend {
                requests: Arc::clone(&requests),
            }),
            db.clone(),
        ));
        let first = first_router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/responses")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({"model": "grok-4.6", "input": "first", "store": true}).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        let bytes = to_bytes(first.into_body(), usize::MAX).await.unwrap();
        let first_value: Value = serde_json::from_slice(&bytes).unwrap();
        let previous_id = first_value["id"].as_str().unwrap();
        let router = build_router_with_state(AppState::with_db(
            Arc::new(CaptureBackend {
                requests: Arc::clone(&requests),
            }),
            db,
        ));

        let second = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/responses")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({
                            "model": "grok-4.6",
                            "input": "second",
                            "previous_response_id": previous_id,
                            "store": true
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(second.status(), StatusCode::OK);
        let requests = requests.lock().unwrap();
        assert_eq!(requests.len(), 2);
        let replayed = &requests[1];
        let roles: Vec<&str> = replayed
            .messages
            .iter()
            .map(|message| message.role.as_str())
            .collect();
        assert_eq!(roles, ["user", "assistant", "user"]);
        assert_eq!(replayed.messages[0].text(), "first");
        assert_eq!(replayed.messages[1].text(), "first");
        assert_eq!(replayed.messages[2].text(), "second");
        assert!(!replayed.extra.contains_key("previous_response_id"));
        assert!(
            replayed
                .responses_input
                .as_ref()
                .is_some_and(|input| { input.as_array().is_some_and(|items| items.len() >= 3) })
        );
    }

    #[tokio::test]
    async fn stored_responses_are_scoped_to_the_creating_key() {
        let (router, db) = crate::tests::keyed_state();
        {
            let guard = db.connection();
            heterion_router_db::repos::insert_api_key(
                &guard,
                &heterion_router_db::repos::ApiKeyRow {
                    id: "k2".to_string(),
                    name: "other".to_string(),
                    key: "sk-test-456".to_string(),
                    allowed_models: "[\"*\"]".to_string(),
                    no_log: false,
                },
            )
            .unwrap();
        }
        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/responses")
                    .header("authorization", "Bearer sk-test-123")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({"model": "grok-4.6", "input": "hi", "store": true}).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let value: Value = serde_json::from_slice(&bytes).unwrap();
        let id = value["id"].as_str().unwrap();

        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/v1/responses/{id}"))
                    .header("authorization", "Bearer sk-test-456")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);

        let response = router
            .oneshot(
                Request::builder()
                    .uri(format!("/v1/responses/{id}"))
                    .header("authorization", "Bearer sk-test-123")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn responses_auth_blocks_anonymous_when_keys_exist() {
        let (router, _) = crate::tests::keyed_state();
        let response = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/responses")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({"model": "grok-4.6", "input": "hi"}).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }
}

/// The `/v1/responses` state store is a disk sink: it used to keep the full
/// request *and* response of every interaction forever, growing without a
/// bound until the client DELETed its own id.
#[cfg(test)]
mod response_store_tests {
    use super::*;
    use axum::body::{Body, to_bytes};
    use axum::http::Request;
    use tower::ServiceExt;

    /// A completion big enough to blow the per-entry byte cap on its own.
    #[derive(Clone)]
    struct HugeBackend {
        text: Arc<String>,
    }

    #[async_trait::async_trait]
    impl ChatBackend for HugeBackend {
        fn name(&self) -> &'static str {
            "huge"
        }

        async fn complete(
            &self,
            request: ChatCompletionRequest,
        ) -> Result<ChatCompletionResponse, GatewayError> {
            Ok(ChatCompletionResponse {
                id: ids::completion_id(),
                object: "chat.completion",
                created: ids::unix_seconds(),
                model: request.model,
                choices: vec![heterion_router_core::ChatChoice {
                    index: 0,
                    message: ChatMessage::plain("assistant".to_string(), (*self.text).clone()),
                    finish_reason: "stop".to_string(),
                }],
                usage: heterion_router_core::Usage::default(),
                raw: None,
            })
        }

        fn stream(&self, _request: ChatCompletionRequest) -> backend::ChunkStream {
            Box::pin(futures::stream::empty())
        }
    }

    fn open() -> (AppState, Arc<heterion_router_db::Db>) {
        let db = Arc::new(heterion_router_db::Db::open_in_memory().unwrap());
        db.migrate().unwrap();
        (
            AppState::with_db(Arc::new(backend::EchoBackend), db.clone()),
            db,
        )
    }

    /// `(entry count, total stored bytes)` in the responses namespace.
    fn store_stats(db: &heterion_router_db::Db) -> (i64, i64) {
        let guard = db.connection();
        let count: i64 = guard
            .query_row(
                "SELECT COUNT(*) FROM key_value WHERE namespace = ?1",
                rusqlite::params![RESPONSE_STORE_NAMESPACE],
                |row| row.get(0),
            )
            .unwrap();
        let bytes: i64 = guard
            .query_row(
                "SELECT COALESCE(SUM(LENGTH(value)), 0) FROM key_value WHERE namespace = ?1",
                rusqlite::params![RESPONSE_STORE_NAMESPACE],
                |row| row.get(0),
            )
            .unwrap();
        (count, bytes)
    }

    fn stored_value(db: &heterion_router_db::Db, id: &str) -> Option<Value> {
        let guard = db.connection();
        heterion_router_db::repos::kv_get(&guard, RESPONSE_STORE_NAMESPACE, id)
            .unwrap()
            .map(|raw| serde_json::from_str(&raw).unwrap())
    }

    fn response_object(id: &str) -> Value {
        json!({
            "id": id,
            "object": "response",
            "created_at": 1,
            "status": "completed",
            "model": "grok-4.6",
            "output": [{
                "type": "message",
                "id": "msg_1",
                "status": "completed",
                "role": "assistant",
                "content": [{"type": "output_text", "text": "echo: hi", "annotations": []}],
            }],
            "usage": {"input_tokens": 3, "output_tokens": 2, "total_tokens": 5},
        })
    }

    async fn post(router: Router, body: Value) -> (StatusCode, Value) {
        let response = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/responses")
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        (status, serde_json::from_slice(&bytes).unwrap())
    }

    async fn fetch(router: Router, id: &str) -> (StatusCode, Value) {
        let response = router
            .oneshot(
                Request::builder()
                    .uri(format!("/v1/responses/{id}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        (status, serde_json::from_slice(&bytes).unwrap())
    }

    fn store_router(state: &AppState) -> Router {
        build_router_with_state(state.clone())
    }

    #[tokio::test]
    async fn a_recent_response_is_still_fetchable_and_replayable() {
        // Regression guard: bounding the store must not break the happy path.
        let (state, db) = open();
        let router = store_router(&state);
        let (status, created) = post(
            router.clone(),
            json!({"model": "grok-4.6", "input": "hi", "store": true}),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let id = created["id"].as_str().unwrap().to_string();
        assert_eq!(
            created["output"][0]["content"][0]["text"],
            json!("echo: hi")
        );

        let (status, fetched) = fetch(router.clone(), &id).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(fetched["id"], json!(id));
        assert_eq!(fetched["object"], json!("response"));
        assert_eq!(
            fetched["output"][0]["content"][0]["text"],
            json!("echo: hi")
        );

        // A later store (which sweeps) must not take the entry with it.
        let (status, _) = post(
            router.clone(),
            json!({"model": "grok-4.6", "input": "again", "store": true}),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let (status, _) = fetch(router.clone(), &id).await;
        assert_eq!(status, StatusCode::OK);

        // ... and `previous_response_id` still replays its context.
        let (status, _) = post(
            router,
            json!({"model": "grok-4.6", "input": "third",
                   "previous_response_id": id, "store": true}),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        // Three stored requests, three entries: the replay added one of its
        // own and duplicated none.
        assert_eq!(store_stats(&db).0, 3, "{:?}", store_stats(&db));
    }

    #[tokio::test]
    async fn a_stored_response_older_than_the_ttl_is_not_returned() {
        let (state, db) = open();
        let router = store_router(&state);
        let (status, created) = post(
            router.clone(),
            json!({"model": "grok-4.6", "input": "hi", "store": true}),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let id = created["id"].as_str().unwrap().to_string();

        // Age the entry past the retention window.
        let stale = now_unix().saturating_sub(RESPONSE_TTL_SECS + 60);
        let mut record = stored_value(&db, &id).expect("stored");
        record["stored_at"] = json!(stale);
        {
            let guard = db.connection();
            heterion_router_db::repos::kv_set(
                &guard,
                RESPONSE_STORE_NAMESPACE,
                &id,
                &record.to_string(),
            )
            .unwrap();
        }

        // Expired state is invisible: not served, not replayable, gone from disk.
        let (status, _) = fetch(router.clone(), &id).await;
        assert_eq!(
            status,
            StatusCode::NOT_FOUND,
            "an expired entry was returned"
        );
        let (status, _) = post(
            router.clone(),
            json!({"model": "grok-4.6", "input": "next",
                   "previous_response_id": id, "store": true}),
        )
        .await;
        assert_eq!(
            status,
            StatusCode::OK,
            "replaying an expired entry must not fail"
        );
        assert!(
            stored_value(&db, created["id"].as_str().unwrap()).is_none(),
            "the sweep left the expired row behind: {:?}",
            store_stats(&db)
        );
    }

    #[tokio::test]
    async fn the_store_never_grows_past_the_entry_cap() {
        let (state, db) = open();
        let auth = auth::Authenticated { key: None };
        let request = json!({"model": "grok-4.6", "input": "hi"});
        let flood = RESPONSE_ENTRY_CAP + 25;
        for index in 0..flood {
            persist_response(
                &state,
                &auth,
                &request,
                &response_object(&format!("resp_{index}")),
            );
        }
        let (count, _) = store_stats(&db);
        assert!(
            count <= i64::try_from(RESPONSE_ENTRY_CAP).unwrap(),
            "{count} entries retained after {flood} inserts"
        );

        // The newest entries are the survivors; the oldest are the casualties.
        assert!(
            stored_value(&db, &format!("resp_{}", flood - 1)).is_some(),
            "the most recent entry was evicted: {}",
            store_stats(&db).0
        );
        assert!(stored_value(&db, "resp_0").is_none());
    }

    #[tokio::test]
    async fn an_oversized_response_is_stored_truncated_but_still_fetchable() {
        let marker = "CANARY-truncation-marker";
        let text = format!("{marker}{}", "x".repeat(RESPONSE_ENTRY_MAX_BYTES * 4));
        let db = Arc::new(heterion_router_db::Db::open_in_memory().unwrap());
        db.migrate().unwrap();
        let state = AppState::with_db(
            Arc::new(HugeBackend {
                text: Arc::new(text.clone()),
            }),
            db.clone(),
        );
        let router = store_router(&state);

        let (status, created) = post(
            router.clone(),
            json!({"model": "grok-4.6", "input": "hi", "store": true}),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let id = created["id"].as_str().unwrap().to_string();

        // Never unbounded bytes on disk.
        let (_, bytes) = store_stats(&db);
        assert!(
            bytes <= i64::try_from(RESPONSE_ENTRY_MAX_BYTES).unwrap(),
            "{bytes} bytes retained for one entry"
        );

        // ... and the client still gets a valid, well-formed response object.
        let (status, fetched) = fetch(router.clone(), &id).await;
        assert_eq!(
            status,
            StatusCode::OK,
            "a truncated entry must stay fetchable"
        );
        assert_eq!(fetched["id"], json!(id));
        assert_eq!(fetched["object"], json!("response"));
        assert_eq!(fetched["model"], json!("grok-4.6"));
        assert!(
            fetched["output"]
                .as_array()
                .is_some_and(|out| out.is_empty()),
            "output must be a well-formed (empty) array: {fetched}"
        );
        assert_eq!(fetched["status"], json!("incomplete"));
        assert_eq!(
            fetched["incomplete_details"]["reason"],
            json!("content_truncated")
        );
        assert!(fetched["usage"]["total_tokens"].is_number(), "{fetched}");
        let serialized = fetched.to_string();
        assert!(
            !serialized.contains(marker),
            "the truncated body leaked: {serialized}"
        );
        assert!(serialized.len() <= RESPONSE_ENTRY_MAX_BYTES);

        // Replaying it is a no-op, not a 400: a size-capped placeholder has
        // no context left to replay.
        let (status, _) = post(
            router,
            json!({"model": "grok-4.6", "input": "next",
                   "previous_response_id": id, "store": true}),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
    }

    #[tokio::test]
    async fn a_sweep_that_cannot_run_never_fails_the_request() {
        // Fail-safe by construction: drop the table the sweep writes to and
        // the store degrades to "not persisted", not to a 500.
        let (state, db) = open();
        let router = store_router(&state);
        db.connection()
            .execute_batch("DROP TABLE key_value")
            .unwrap();
        let auth = auth::Authenticated { key: None };
        persist_response(
            &state,
            &auth,
            &json!({"model": "grok-4.6", "input": "hi"}),
            &response_object("resp_swept"),
        );

        // (1) The write degraded instead of panicking or propagating: the
        // namespace is simply unreadable, i.e. nothing was persisted.
        {
            let guard = db.connection();
            assert!(
                heterion_router_db::repos::kv_get(&guard, RESPONSE_STORE_NAMESPACE, "resp_swept")
                    .is_err(),
                "a dropped table must surface as a failed read, not a stored entry"
            );
        }

        // (2) And the caller-visible consequence is what the name promises:
        // a request through the router still succeeds instead of becoming a
        // 500. Reaching here at all proves the sweep did not unwind.
        let (status, created) = post(
            router,
            json!({"model": "grok-4.6", "input": "hi", "store": true}),
        )
        .await;
        assert_eq!(
            status,
            StatusCode::OK,
            "a store that cannot be swept must not fail the request: {created}"
        );
        assert!(
            created["id"].is_string(),
            "the response must still be complete: {created}"
        );
    }
}

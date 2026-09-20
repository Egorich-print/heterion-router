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
pub mod combos;
pub mod cors;
pub mod credentials;
pub mod gemini;
pub mod grok_cli;
pub mod ids;
pub mod openai;
pub mod responses;
pub mod restart;
pub mod routing;
pub mod ui;

use std::convert::Infallible;
use std::sync::Arc;

use auth::Authenticated;
use axum::{
    Json, Router,
    extract::State,
    http::StatusCode,
    response::{
        IntoResponse, Response,
        sse::{Event, KeepAlive, Sse},
    },
    routing::{get, patch, post},
};
use futures::StreamExt;
use heterion_router_core::{
    ChatCompletionRequest, ChatCompletionResponse, GatewayError, StreamChunk,
};
use heterion_router_db::Db;
use heterion_router_providers::ProviderRegistry;
use heterion_router_usage::{PriceTable, UsageRecord, record_usage};
use serde_json::{Value, json};

use backend::ChatBackend;

/// Shared gateway state.
#[derive(Clone)]
pub struct AppState {
    pub(crate) backend: Arc<dyn ChatBackend>,
    pub(crate) db: Option<Arc<Db>>,
    pub(crate) require_auth: bool,
    pub(crate) prices: PriceTable,
    pub(crate) registry: Option<Arc<ProviderRegistry>>,
    pub(crate) backend_names: Vec<String>,
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
pub fn build_router_with_state(state: AppState) -> Router {
    Router::new()
        .route("/healthz", get(healthz))
        .route("/api/overview", get(admin::overview))
        .route("/api/combos", get(admin::combos))
        .route("/api/combos/{id}", patch(admin::update_combo))
        .route("/api/connections", get(admin::connections))
        .route("/api/connections/{id}", patch(admin::update_connection))
        .route("/api/usage", get(admin::usage))
        .route("/api/logs", get(admin::logs))
        .route("/api/keys", get(admin::keys).post(admin::create_key))
        .route("/api/keys/{id}/revoke", post(admin::revoke_key))
        .route("/api/keys/{id}/restore", post(admin::restore_key))
        .route("/api/restart", post(admin::restart_service))
        .route("/", get(ui::serve))
        .route("/{*path}", get(ui::serve))
        .route("/v1/models", get(list_models))
        .route("/v1/chat/completions", post(chat_completions))
        .route("/v1/responses", post(responses))
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
        if let Ok(combos) = heterion_router_db::repos::list_combos(&guard) {
            for combo in combos {
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
    Json(request): Json<ChatCompletionRequest>,
) -> Response {
    if request.stream {
        tracing::info!("completion model={} stream=true", request.model);
        return stream_response(state, request).into_response();
    }

    let model = request.model.clone();
    let started = std::time::Instant::now();
    tracing::info!("completion model={model} stream=false");
    match state.backend.complete(request).await {
        Ok(response) => {
            let latency_ms = started.elapsed().as_millis().min(u128::from(u32::MAX)) as u32;
            tracing::info!(
                "completion ok model={model} resolved={} finish={} pt={} ct={} ms={latency_ms}",
                response.model,
                response.choices[0].finish_reason,
                response.usage.prompt_tokens,
                response.usage.completion_tokens,
            );
            let usage = response.usage;
            record_completion_usage(&state, &auth, &model, &usage, true, latency_ms, None);

            let mut http = Json(response).into_response();
            if let Some(cost) = state.prices.cost_usd(
                &model,
                usage.prompt_tokens,
                usage.completion_tokens,
                0,
                0,
                0,
            ) && let Ok(value) = axum::http::HeaderValue::from_str(&format!("{cost:.6}"))
            {
                http.headers_mut()
                    .insert("x-heterion-router-cost-usd", value);
            }
            http
        }
        Err(error) => {
            let latency_ms = started.elapsed().as_millis().min(u128::from(u32::MAX)) as u32;
            tracing::warn!("completion failed model={model}: {error}");
            record_completion_usage(
                &state,
                &auth,
                &model,
                &heterion_router_core::Usage::default(),
                false,
                latency_ms,
                Some("upstream_error"),
            );
            error_response(error)
        }
    }
}
fn record_completion_usage(
    state: &AppState,
    auth: &Authenticated,
    model: &str,
    usage: &heterion_router_core::Usage,
    success: bool,
    latency_ms: u32,
    error_code: Option<&str>,
) {
    let Some(db) = state.db.as_ref() else {
        return;
    };
    let guard = db.connection();
    let record = UsageRecord {
        provider: Some(state.backend.name().to_string()),
        model: Some(model.to_string()),
        connection_id: None,
        api_key_id: auth.key.as_ref().map(|key| key.id.clone()),
        api_key_name: auth.key.as_ref().map(|key| key.name.clone()),
        tokens_input: usage.prompt_tokens,
        tokens_output: usage.completion_tokens,
        tokens_cache_read: 0,
        tokens_cache_creation: 0,
        tokens_reasoning: 0,
        status: Some(if success { "succeeded" } else { "failed" }.to_string()),
        success,
        latency_ms,
        error_code: error_code.map(str::to_string),
    };
    if let Err(error) = record_usage(&guard, &record) {
        tracing::warn!("usage record failed: {error}");
    }
}

fn stream_response(
    state: AppState,
    request: ChatCompletionRequest,
) -> Sse<impl futures::Stream<Item = Result<Event, Infallible>>> {
    let id = ids::completion_id();
    let created = ids::unix_seconds();
    let model = request.model.clone();
    let stream = state.backend.stream(request);

    // OpenAI streams announce the assistant role on the first delta; clients
    // keying off it (LangChain-style) drop deltas without it.
    let mut role_emitted = false;
    let events = stream.map(move |item| {
        Ok(match item {
            Ok(chunk) => {
                let with_role = !role_emitted;
                role_emitted = true;
                chunk_event(&id, created, &model, &chunk, with_role)
            }
            Err(error) => error_event(&error),
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

    let payload = json!({
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

    Event::default().data(payload.to_string())
}

fn error_event(error: &GatewayError) -> Event {
    Event::default().data(json!({ "error": { "message": error.to_string() } }).to_string())
}

fn error_response(error: GatewayError) -> Response {
    let status = match error {
        GatewayError::InvalidRequest(_) => StatusCode::BAD_REQUEST,
        GatewayError::UnknownModel(_) => StatusCode::NOT_FOUND,
        GatewayError::Upstream(_) => StatusCode::BAD_GATEWAY,
    };
    (
        status,
        Json(json!({ "error": { "message": error.to_string() } })),
    )
        .into_response()
}

/// `POST /v1/responses` — the OpenAI Responses API surface, so Responses-only
/// clients (grok CLI, Codex) can point at the gateway. The request is
/// translated to the internal chat shape, served by the same backend
/// machinery, and translated back; streaming emits Responses events.
async fn responses(
    State(state): State<AppState>,
    auth: Authenticated,
    Json(request): Json<Value>,
) -> Response {
    let request = match responses_to_chat(&request) {
        Ok(request) => request,
        Err(message) => return error_response(GatewayError::InvalidRequest(message)),
    };
    if request.stream {
        tracing::info!("responses model={} stream=true", request.model);
        return responses_stream(state, request).into_response();
    }

    let model = request.model.clone();
    let started = std::time::Instant::now();
    tracing::info!("responses model={model} stream=false");
    match state.backend.complete(request).await {
        Ok(response) => {
            let latency_ms = started.elapsed().as_millis().min(u128::from(u32::MAX)) as u32;
            let usage = response.usage;
            record_completion_usage(&state, &auth, &model, &usage, true, latency_ms, None);
            let mut http = Json(chat_to_responses_object(&response)).into_response();
            if let Some(cost) = state.prices.cost_usd(
                &model,
                usage.prompt_tokens,
                usage.completion_tokens,
                0,
                0,
                0,
            ) && let Ok(value) = axum::http::HeaderValue::from_str(&format!("{cost:.6}"))
            {
                http.headers_mut()
                    .insert("x-heterion-router-cost-usd", value);
            }
            http
        }
        Err(error) => {
            let latency_ms = started.elapsed().as_millis().min(u128::from(u32::MAX)) as u32;
            tracing::warn!("responses failed model={model}: {error}");
            record_completion_usage(
                &state,
                &auth,
                &model,
                &heterion_router_core::Usage::default(),
                false,
                latency_ms,
                Some("upstream_error"),
            );
            error_response(error)
        }
    }
}

/// Translate a Responses API request to the internal chat request.
///
/// Covers the shapes Responses-only clients actually send: `instructions`,
/// string and item-array `input`, `max_output_tokens`, Responses-shaped
/// `tools`. Unmodelled item kinds are skipped, never fabricated.
fn responses_to_chat(request: &Value) -> Result<ChatCompletionRequest, String> {
    let model = request
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    if model.is_empty() {
        return Err("model is required".into());
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
        tool_choice: request.get("tool_choice").cloned(),
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
    let message = &response.choices[0].message;
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
        "id": format!("resp_{}", response.id),
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
    request: ChatCompletionRequest,
) -> Sse<impl futures::Stream<Item = Result<Event, Infallible>>> {
    let model = request.model.clone();
    let created = ids::unix_seconds();
    let stream = state.backend.stream(request);

    let events = stream.flat_map(move |item| {
        futures::stream::iter(match item {
            Ok(chunk) => chat_chunk_to_responses_events(&chunk, &created, &model)
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
            Err(error) => vec![Ok(Event::default()
                .data(json!({ "error": { "message": error.to_string() } }).to_string()))],
        })
    });

    Sse::new(events).keep_alive(KeepAlive::default())
}

/// Map one internal stream chunk to Responses API events.
///
/// ponytail: `response.completed` carries a zero usage (stream chunks do not
/// carry token counts) and tool calls are emitted whole in one
/// `output_item.added` (no incremental argument deltas) — upgrade both when a
/// client actually needs them.
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
                "usage": {"input_tokens": 0, "output_tokens": 0, "total_tokens": 0},
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
    use tower::ServiceExt;

    fn test_router() -> Router {
        build_router(Arc::new(backend::EchoBackend))
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
    async fn auth_blocks_wrong_key() {
        let (router, _) = keyed_state();
        let response = post_completions(router, Some("sk-wrong")).await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn auth_allows_valid_key_records_usage_and_prices() {
        let (router, db) = keyed_state();
        let response = post_completions(router, Some("sk-test-123")).await;
        assert_eq!(response.status(), StatusCode::OK);

        let cost = response
            .headers()
            .get("x-heterion-router-cost-usd")
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
}

#[cfg(test)]
mod responses_tests {
    use super::*;
    use axum::body::{Body, to_bytes};
    use axum::http::Request;
    use tower::ServiceExt;

    fn responses_router() -> Router {
        build_router(Arc::new(backend::EchoBackend))
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
            "tools": [{"type": "function", "name": "get_time",
                       "description": "clock", "parameters": {"type": "object"}}],
            "stream": true,
        }))
        .unwrap();

        assert_eq!(request.model, "grok-4.6");
        assert!(request.stream);
        assert_eq!(request.max_tokens, Some(50));
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

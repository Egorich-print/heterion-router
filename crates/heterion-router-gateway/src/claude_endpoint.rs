//! Inbound Anthropic Messages surface: `POST /v1/messages` (streaming and
//! not) and `POST /v1/messages/count_tokens`.
//!
//! A request is translated into the gateway's internal chat shape and served
//! by the same [`backend::ChatBackend`] machinery as `/v1/chat/completions`
//! — same routing, breaker, retry, attribution and usage rows, no second HTTP
//! client. The completion is translated back into the Anthropic dialect on
//! the way out, and every refusal travels the shared sanitized error path
//! ([`crate::sanitized_error_payload`]), so no upstream text reaches the wire.
//!
//! The *outbound* direction (an OpenAI-shaped request to a Claude provider) is
//! the ported pair in `heterion-router-translate` (`claude_request.rs`,
//! `claude.rs`); nothing here duplicates it. Text flattening reuses core's
//! `content_text_with` under the join rules the Claude adapter itself pins.
//!
//! Deferred, and dropped with a debug log rather than silently: `thinking`
//! configuration and historical thinking blocks (as in every translator in
//! this workspace), `document` content blocks, and the server-side tools.

use std::convert::Infallible;

use axum::{
    Json,
    extract::State,
    response::{
        IntoResponse, Response,
        sse::{Event, KeepAlive, Sse},
    },
};
use futures::StreamExt;
use heterion_router_core::{
    ChatCompletionRequest, ChatCompletionResponse, ChatMessage, GatewayError, StreamChunk,
    TextJoin, TextParts, Usage, content_text_with,
};
use serde_json::{Value, json};

use crate::auth::Authenticated;
use crate::backend;
use crate::ids;
use crate::{
    AppState, CompletionUsage, JsonBody, StreamAccounting, cached_usage, error_response,
    error_status, forbidden_response, millis_since, record_completion_usage, request_summary,
    router_headers, sanitized_error_payload, with_recovered_status,
};

/// Text flattening for Claude content blocks: `type: "text"` parts joined with
/// a newline, the rule the outbound Claude adapter pins for itself.
const CLAUDE_TEXT: TextJoin = TextJoin::new(TextParts::TypedText, "\n", false);

/// Anthropic-shaped message id: `msg_` plus this crate's unique suffix.
fn message_id() -> String {
    format!("msg_{}", ids::call_id().trim_start_matches("call-"))
}

/// `POST /v1/messages`.
pub async fn messages(
    State(state): State<AppState>,
    auth: Authenticated,
    JsonBody(request): JsonBody<Value>,
) -> Response {
    let mut request = match claude_request_to_chat(&request) {
        Ok(request) => request,
        Err(message) => return error_response(GatewayError::InvalidRequest(message)),
    };
    if !auth.allows_model(&request.model) {
        return forbidden_response("model is not allowed for this api key");
    }
    let token = backend::stamp_attribution(&mut request);
    if request.stream {
        tracing::info!("claude messages model={} stream=true", request.model);
        return claude_stream(state, auth, request).into_response();
    }

    let model = request.model.clone();
    let summary = request_summary(&request);
    let started = std::time::Instant::now();
    let prompt_estimate = crate::usage_estimate::estimate_prompt_tokens(&request);
    tracing::info!("claude messages model={model} stream=false");
    // A completion the dialect cannot render is an upstream fault, not an
    // answer: it must be booked and reported as a failure, never as an empty
    // message the client would believe.
    let outcome = state
        .backend
        .complete(request)
        .await
        .and_then(|response| claude_message(&response).map(|body| (response, body)));
    match outcome {
        Ok((response, body)) => {
            let latency_ms = millis_since(started);
            tracing::info!(
                "claude messages ok model={model} resolved={} pt={} ct={} ms={latency_ms}",
                response.model,
                response.usage.prompt_tokens,
                response.usage.completion_tokens,
            );
            let served = response.model.clone();
            let mut usage = response.usage;
            // A provider that reports no usage must not look free.
            let output_bytes = crate::usage_estimate::value_text_bytes(
                response
                    .choices
                    .first()
                    .and_then(|choice| choice.message.content.as_ref()),
            );
            crate::usage_estimate::fill_missing(&mut usage, prompt_estimate, output_bytes);
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
                    // A completion has no first token to measure.
                    ttft_ms: 0,
                    error_code: None,
                    request_type: "messages",
                    summary: &summary,
                },
            );

            let mut http = Json(body).into_response();
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
            // executor that has not been wrapped yet still owes the client the
            // status its payload carried.
            let error = with_recovered_status(error);
            let latency_ms = millis_since(started);
            tracing::warn!("claude messages failed model={model}: {error}");
            record_completion_usage(
                &state,
                &auth,
                CompletionUsage {
                    model: &model,
                    resolved: None,
                    token: &token,
                    usage: &heterion_router_core::Usage::default(),
                    details: heterion_router_usage::CachedUsage::default(),
                    success: false,
                    call_status: i64::from(error_status(&error).as_u16()),
                    latency_ms,
                    ttft_ms: 0,
                    error_code: Some("upstream_error"),
                    request_type: "messages",
                    summary: &summary,
                },
            );
            error_response(error)
        }
    }
}

/// `POST /v1/messages/count_tokens`.
///
/// **Accuracy: an estimate, not a tokenizer count.** `heterion-router-core`
/// ships no tokenizer and this endpoint may not add a BPE dependency, so the
/// count is the project's documented fallback — UTF-8 bytes / 4 plus a fixed
/// per-message framing allowance (`docs/research/audit-2026-10/03-futureagi-parity.md`).
/// For English prose that lands within roughly ±25%. It is *wrong in a way
/// that matters* for CJK text (three bytes per character is 0.75 tokens per
/// character, where the real figure is nearer one) and for tool schemas,
/// which are counted as raw JSON rather than as the dialect's tokenized form.
/// Treat it as a budget hint, never as a billing figure — and nothing here
/// reaches the usage tables either, because no upstream was called.
pub async fn count_tokens(auth: Authenticated, JsonBody(request): JsonBody<Value>) -> Response {
    let request = match claude_request_to_chat(&request) {
        Ok(request) => request,
        Err(message) => return error_response(GatewayError::InvalidRequest(message)),
    };
    if !auth.allows_model(&request.model) {
        return forbidden_response("model is not allowed for this api key");
    }
    Json(json!({ "input_tokens": crate::usage_estimate::estimate_prompt_tokens(&request) }))
        .into_response()
}

/// Translate an Anthropic Messages request into the internal chat request.
///
/// Covers what Claude clients send: `system` (string or block array), the
/// `messages` content block array with `text` / `image` / `tool_use` /
/// `tool_result`, `tools`, `tool_choice`, `max_tokens`, `stop_sequences`,
/// `temperature`, `top_p`, `top_k` and `metadata`. `max_tokens` is optional
/// here even though the Anthropic API demands it: a client that omits it
/// gets the provider's own default instead of a 400, the same way the
/// outbound translator defaults it.
fn claude_request_to_chat(request: &Value) -> Result<ChatCompletionRequest, String> {
    let model = request
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    if model.is_empty() {
        return Err("model is required".into());
    }

    let mut messages: Vec<ChatMessage> = Vec::new();
    // Anthropic carries the system prompt beside `messages`; the chat path
    // already knows how to forward a hoisted system turn to every provider.
    let system = request
        .get("system")
        .map(|system| content_text_with(Some(system), CLAUDE_TEXT))
        .unwrap_or_default();
    if !system.is_empty() {
        messages.push(ChatMessage::plain("system".to_string(), system));
    }

    let turns = request
        .get("messages")
        .and_then(Value::as_array)
        .ok_or("messages is required")?;
    let mut turns_seen = 0usize;
    for turn in turns {
        turns_seen += push_claude_turn(&mut messages, turn);
    }
    if turns_seen == 0 {
        return Err("messages produced no turns".into());
    }

    Ok(ChatCompletionRequest {
        model,
        messages,
        stream: matches!(request.get("stream"), Some(Value::Bool(true))),
        max_tokens: request
            .get("max_tokens")
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
        top_k: request
            .get("top_k")
            .and_then(Value::as_u64)
            .map(|value| value.min(u64::from(u32::MAX)) as u32),
        tools: claude_tools(request.get("tools")),
        tool_choice: claude_tool_choice(request.get("tool_choice")),
        stop: request.get("stop_sequences").cloned(),
        metadata: request.get("metadata").cloned(),
        ..Default::default()
    })
}

/// Append one Anthropic turn as chat message(s); answer how many turns it
/// produced, so a request whose `messages` hold nothing usable is rejected
/// instead of answered from the system prompt alone.
fn push_claude_turn(messages: &mut Vec<ChatMessage>, turn: &Value) -> usize {
    let role = turn.get("role").and_then(Value::as_str).unwrap_or("user");
    let content = turn.get("content");
    let Some(blocks) = content.and_then(Value::as_array) else {
        let text = content_text_with(content, CLAUDE_TEXT);
        if text.is_empty() && content.is_none() {
            return 0;
        }
        messages.push(ChatMessage::plain(role.to_string(), text));
        return 1;
    };

    let mut produced = 0;
    let mut parts: Vec<Value> = Vec::new();
    let mut calls: Vec<Value> = Vec::new();
    for block in blocks {
        match block.get("type").and_then(Value::as_str).unwrap_or("text") {
            "text" => parts.push(json!({
                "type": "text",
                "text": block.get("text").and_then(Value::as_str).unwrap_or(""),
            })),
            "tool_use" => calls.push(json!({
                "id": block.get("id").and_then(Value::as_str).unwrap_or(""),
                "type": "function",
                "function": {
                    "name": block.get("name").and_then(Value::as_str).unwrap_or(""),
                    "arguments": block.get("input").map(Value::to_string).unwrap_or_else(|| "{}".to_string()),
                },
            })),
            // The dialect requires tool results first in a user turn, and the
            // chat path carries them as their own `tool` messages.
            "tool_result" => {
                messages.push(ChatMessage {
                    role: "tool".to_string(),
                    content: Some(tool_result_content(block.get("content"))),
                    reasoning_content: None,
                    tool_calls: None,
                    tool_call_id: Some(
                        block
                            .get("tool_use_id")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_string(),
                    ),
                    name: None,
                });
                produced += 1;
            }
            "image" => parts.extend(image_part(block)),
            // thinking / document / server_tool_use: no executor on the chat
            // path, so they are dropped loudly instead of forwarded in a
            // spelling the upstream would reject.
            other => tracing::debug!("claude_endpoint: dropping content block type {other}"),
        }
    }
    // A turn that was nothing but tool results already spoke for itself.
    if parts.is_empty() && calls.is_empty() {
        return produced;
    }
    messages.push(ChatMessage {
        role: role.to_string(),
        content: (!parts.is_empty()).then_some(Value::Array(parts)),
        reasoning_content: None,
        tool_calls: (!calls.is_empty()).then_some(Value::Array(calls)),
        tool_call_id: None,
        name: None,
    });
    produced + 1
}

/// `tool_result.content` is a string or a block array; the chat path wants the
/// flat text both spellings carry.
fn tool_result_content(content: Option<&Value>) -> Value {
    match content {
        Some(Value::String(text)) => Value::from(text.clone()),
        Some(blocks) => Value::from(content_text_with(Some(blocks), CLAUDE_TEXT)),
        None => Value::from(""),
    }
}

/// Anthropic image block → the OpenAI `image_url` part, for the `base64` and
/// `url` source spellings. Empty for a source this gateway cannot forward.
fn image_part(block: &Value) -> Option<Value> {
    let source = block.get("source")?;
    let url = match source.get("type").and_then(Value::as_str).unwrap_or("") {
        "base64" => format!(
            "data:{};base64,{}",
            source
                .get("media_type")
                .and_then(Value::as_str)
                .unwrap_or("image/png"),
            source.get("data").and_then(Value::as_str).unwrap_or("")
        ),
        "url" => source.get("url").and_then(Value::as_str)?.to_string(),
        _ => return None,
    };
    Some(json!({"type": "image_url", "image_url": {"url": url}}))
}

/// Anthropic tools are flat (`name` / `input_schema`); the chat shape wraps
/// them in a `function` member. A tool without a name is dropped: the chat
/// path has nothing to call.
fn claude_tools(tools: Option<&Value>) -> Option<Value> {
    let tools = tools?.as_array()?;
    Some(Value::Array(
        tools
            .iter()
            .filter_map(|tool| {
                let name = tool.get("name").and_then(Value::as_str)?;
                Some(json!({
                    "type": "function",
                    "function": {
                        "name": name,
                        "description": tool
                            .get("description")
                            .and_then(Value::as_str)
                            .unwrap_or(""),
                        "parameters": tool
                            .get("input_schema")
                            .cloned()
                            .unwrap_or_else(|| json!({"type": "object"})),
                    },
                }))
            })
            .collect(),
    ))
}

/// `auto` / `any` / `none` / a named `tool` → the chat path's spellings.
fn claude_tool_choice(choice: Option<&Value>) -> Option<Value> {
    let choice = choice?;
    Some(
        match choice.get("type").and_then(Value::as_str).unwrap_or("") {
            "auto" => json!("auto"),
            "any" => json!("required"),
            "none" => json!("none"),
            "tool" => json!({
                "type": "function",
                "function": {"name": choice.get("name").and_then(Value::as_str).unwrap_or("")},
            }),
            _ => choice.clone(),
        },
    )
}

/// An Anthropic message object for a completed chat completion.
///
/// `Err` when the completion carries no choice: an upstream that answered with
/// no answer is a fault, not an empty message.
fn claude_message(response: &ChatCompletionResponse) -> Result<Value, GatewayError> {
    let choice = response
        .choices
        .first()
        .ok_or_else(|| GatewayError::Upstream("completion carried no choices".to_string()))?;
    let message = &choice.message;

    let mut content = Vec::new();
    if let Some(text) = message
        .content
        .as_ref()
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
    {
        content.push(json!({"type": "text", "text": text}));
    }
    if let Some(calls) = message.tool_calls.as_ref().and_then(Value::as_array) {
        for call in calls {
            let arguments = call
                .pointer("/function/arguments")
                .and_then(Value::as_str)
                .unwrap_or("{}");
            content.push(json!({
                "type": "tool_use",
                "id": call.get("id").and_then(Value::as_str).unwrap_or(""),
                "name": call.pointer("/function/name").and_then(Value::as_str).unwrap_or(""),
                "input": serde_json::from_str::<Value>(arguments).unwrap_or_else(|_| json!({})),
            }));
        }
    }

    // The discounted counts the upstream published in its own payload ride
    // along, so an Anthropic client sees the same cache accounting the usage
    // row is billed from.
    let details = cached_usage(response);
    let mut usage = serde_json::Map::new();
    usage.insert(
        "input_tokens".to_string(),
        json!(response.usage.prompt_tokens),
    );
    usage.insert(
        "output_tokens".to_string(),
        json!(response.usage.completion_tokens),
    );
    if details.cache_read > 0 {
        usage.insert(
            "cache_read_input_tokens".to_string(),
            json!(details.cache_read),
        );
    }
    if details.cache_creation > 0 {
        usage.insert(
            "cache_creation_input_tokens".to_string(),
            json!(details.cache_creation),
        );
    }

    Ok(json!({
        "id": message_id(),
        "type": "message",
        "role": "assistant",
        "model": response.model,
        "content": content,
        "stop_reason": claude_stop_reason(&choice.finish_reason),
        "stop_sequence": Value::Null,
        "usage": Value::Object(usage),
    }))
}

/// Chat `finish_reason` → the Anthropic `stop_reason` vocabulary.
fn claude_stop_reason(finish: &str) -> &'static str {
    match finish {
        "length" | "max_tokens" => "max_tokens",
        "tool_calls" | "function_call" => "tool_use",
        "content_filter" => "refusal",
        _ => "end_turn",
    }
}

/// One step of the SSE body: open the dialect, relay a chunk, close it.
enum Step {
    Start,
    Chunk(Result<StreamChunk, GatewayError>),
    Close,
}

/// Stream a completion in the Anthropic event dialect.
///
/// One pump over [`ChatBackend::stream`] — the same source the chat and
/// Responses surfaces use, and therefore the same
/// [`crate::responses::pump_sse_stream`] decoder behind it, with its bounded
/// event size and its fail-closed overflow. Nothing here re-reads a byte
/// stream, so that decoder stays the only one. The `Close` step closes the
/// dialect whether or not a terminal chunk arrived, so an executor that
/// synthesises its stop chunk (`MissingTerminal::SynthesizeStop`) and one that
/// merely stops both end on a well-formed message.
///
/// The opening and closing frames are steps of the same stream rather than
/// separate concatenations, so one owner holds the dialect state: an
/// `error` frame suppresses the closing pair, exactly as Anthropic clients
/// expect.
fn claude_stream(
    state: AppState,
    auth: Authenticated,
    request: ChatCompletionRequest,
) -> Sse<impl futures::Stream<Item = Result<Event, Infallible>>> {
    let model = request.model.clone();
    let summary = request_summary(&request);
    let token = backend::request_token(&request).unwrap_or_default();
    let started = std::time::Instant::now();
    let prompt_estimate = crate::usage_estimate::estimate_prompt_tokens(&request);
    let stream = state.backend.stream(request);
    // A model that degenerates repeats itself until the token cap; the guard
    // stops the stream there instead of passing the loop on.
    let stream = crate::repetition_guard::guard_stream(stream, &model);
    let mut books = StreamAccounting::new(
        state,
        auth,
        token,
        model.clone(),
        started,
        crate::StreamRequest {
            request_type: "messages",
            summary,
            prompt_estimate,
        },
    );
    let mut dialect = ClaudeDialect::new(&model);

    let steps = futures::stream::iter([Step::Start])
        .chain(stream.map(Step::Chunk))
        .chain(futures::stream::once(async { Step::Close }));

    let events = steps.flat_map(move |step| {
        let mut failed = None;
        let frames = match step {
            Step::Start => dialect.start(),
            Step::Close => dialect.finish(),
            Step::Chunk(Ok(chunk)) => {
                books.first_token();
                books.count_output(&chunk);
                if let Some(usage) = chunk.usage {
                    books.usage = usage;
                    books.usage_reported = true;
                }
                if let Some(details) = backend::chunk_cached_usage(&chunk) {
                    books.details = details;
                }
                if chunk.finish_reason.is_some() {
                    books.record();
                }
                dialect.chunk(&chunk)
            }
            Step::Chunk(Err(error)) => {
                let error = with_recovered_status(error);
                // The 200 is already spent, so the recovered status can only
                // ride in the error frame — but it must be booked too.
                failed = Some(i64::from(error_status(&error).as_u16()));
                dialect.refusal(&error)
            }
        };
        if let Some(status) = failed {
            books.failed_with(status);
        }
        futures::stream::iter(
            frames
                .into_iter()
                .map(|frame| {
                    let event = frame
                        .get("type")
                        .and_then(Value::as_str)
                        .unwrap_or("message")
                        .to_string();
                    Ok::<_, Infallible>(Event::default().event(event).data(frame.to_string()))
                })
                .collect::<Vec<_>>(),
        )
    });

    Sse::new(events).keep_alive(KeepAlive::default())
}

/// The block currently open, if any.
struct OpenBlock {
    /// The `index` every frame of this block carries.
    index: u32,
    /// Block kind, so a delta knows which block it belongs to.
    kind: &'static str,
    /// Position of the `tool_calls` entry a `tool_use` block is collecting, so
    /// an upstream that fragments one call across chunks continues the same
    /// block instead of opening a second one.
    part: Option<u64>,
}

/// The Anthropic SSE dialect over the gateway's format-agnostic chunks.
///
/// `message_start`, then one `content_block_start` / `content_block_delta` /
/// `content_block_stop` group per block, then `message_delta` (the
/// `stop_reason` plus the final usage) and `message_stop`. Exactly one block
/// is open at a time, which is what the dialect requires: a text delta and a
/// `tool_use` delta cannot share an index.
struct ClaudeDialect {
    model: String,
    /// Index the next opened block takes.
    next_index: u32,
    open: Option<OpenBlock>,
    /// Whether the closing pair already went out.
    stopped: bool,
    /// Whether a refusal took the stream instead.
    failed: bool,
    /// Terminal usage, as the backend reported it.
    usage: Option<Usage>,
}

impl ClaudeDialect {
    fn new(model: &str) -> Self {
        Self {
            model: model.to_string(),
            next_index: 0,
            open: None,
            stopped: false,
            failed: false,
            usage: None,
        }
    }

    /// The `message_start` envelope every Anthropic client waits for. Input
    /// tokens are unknown until the terminal usage arrives, so they open at
    /// zero and are restated in `message_delta`.
    fn start(&self) -> Vec<Value> {
        vec![json!({
            "type": "message_start",
            "message": {
                "id": message_id(),
                "type": "message",
                "role": "assistant",
                "model": self.model,
                "content": [],
                "stop_reason": Value::Null,
                "stop_sequence": Value::Null,
                "usage": {"input_tokens": 0, "output_tokens": 0},
            },
        })]
    }

    /// Frames for one gateway chunk.
    fn chunk(&mut self, chunk: &StreamChunk) -> Vec<Value> {
        let mut frames = Vec::new();
        if let Some(text) = &chunk.content {
            frames.extend(self.continue_block("text", json!({"type": "text", "text": ""}), None));
            frames.push(self.delta(json!({"type": "text_delta", "text": text})));
        }
        if let Some(thinking) = &chunk.reasoning {
            frames.extend(self.continue_block(
                "thinking",
                json!({"type": "thinking", "thinking": ""}),
                None,
            ));
            frames.push(self.delta(json!({"type": "thinking_delta", "thinking": thinking})));
        }
        if let Some(calls) = chunk.tool_calls.as_ref().and_then(Value::as_array) {
            frames.extend(self.tool_use(calls));
        }
        if let Some(usage) = chunk.usage {
            self.usage = Some(usage);
        }
        if let Some(reason) = &chunk.finish_reason {
            frames.extend(self.close_block());
            frames.extend(self.stop(claude_stop_reason(reason)));
        }
        frames
    }

    /// `tool_use` blocks for the calls an OpenAI-shaped chunk carries.
    ///
    /// Emitted whole where the chunk is whole: a call fragment continues the
    /// block that is already open, a new `index` opens a new one.
    fn tool_use(&mut self, calls: &[Value]) -> Vec<Value> {
        let mut frames = Vec::new();
        for call in calls {
            let part = call.get("index").and_then(Value::as_u64);
            let continues = self.open.as_ref().is_some_and(|open| {
                open.kind == "tool_use" && open.part.unwrap_or(0) == part.unwrap_or(0)
            });
            if !continues {
                frames.extend(self.begin(
                    "tool_use",
                    json!({
                        "type": "tool_use",
                        "id": call.get("id").and_then(Value::as_str).unwrap_or(""),
                        "name": call.pointer("/function/name").and_then(Value::as_str).unwrap_or(""),
                        "input": {},
                    }),
                    part,
                ));
            }
            let arguments = call
                .pointer("/function/arguments")
                .and_then(Value::as_str)
                .unwrap_or("");
            frames.push(self.delta(json!({
                "type": "input_json_delta",
                "partial_json": arguments,
            })));
        }
        frames
    }

    /// The Anthropic `error` frame, carrying the gateway's shared sanitized
    /// payload: the dialect tag is ours, the message never is.
    fn refusal(&mut self, error: &GatewayError) -> Vec<Value> {
        self.failed = true;
        let mut frame = json!({"type": "error"});
        if let Some(payload) = sanitized_error_payload(error).get("error") {
            frame["error"] = payload.clone();
        }
        vec![frame]
    }

    /// Closing frames for a stream that ended without a terminal chunk.
    /// Suppressed after a refusal: Anthropic clients read an `error` frame as
    /// the end of the message.
    fn finish(&mut self) -> Vec<Value> {
        if self.failed || self.stopped {
            return Vec::new();
        }
        self.stop("end_turn")
    }

    /// `message_delta` + `message_stop`.
    fn stop(&mut self, stop_reason: &str) -> Vec<Value> {
        self.stopped = true;
        let usage = self.usage.unwrap_or_default();
        // Close the open block before the message ends. A stream cut short by
        // an absent terminal chunk still owes the client a
        // `content_block_stop`, or the `content_block_start` it already sent
        // never resolves.
        let mut frames = self.close_block();
        frames.extend([
            json!({
                "type": "message_delta",
                "delta": {"stop_reason": stop_reason, "stop_sequence": Value::Null},
                "usage": {
                    "input_tokens": usage.prompt_tokens,
                    "output_tokens": usage.completion_tokens,
                },
            }),
            json!({"type": "message_stop"}),
        ]);
        frames
    }

    /// Open a `kind` block, or nothing at all when one is already open: a run
    /// of deltas belongs to the block the first one opened.
    fn continue_block(
        &mut self,
        kind: &'static str,
        block: Value,
        part: Option<u64>,
    ) -> Vec<Value> {
        if self.open.as_ref().is_some_and(|open| open.kind == kind) {
            return Vec::new();
        }
        self.begin(kind, block, part)
    }

    /// Close whatever block is open and open a `kind` block at a fresh index.
    fn begin(&mut self, kind: &'static str, block: Value, part: Option<u64>) -> Vec<Value> {
        let mut frames = self.close_block();
        let index = self.next_index;
        self.next_index += 1;
        self.open = Some(OpenBlock { index, kind, part });
        frames.push(json!({
            "type": "content_block_start",
            "index": index,
            "content_block": block,
        }));
        frames
    }

    /// `content_block_stop` for the open block, if there is one.
    fn close_block(&mut self) -> Vec<Value> {
        let Some(open) = self.open.take() else {
            return Vec::new();
        };
        vec![json!({"type": "content_block_stop", "index": open.index})]
    }

    /// `content_block_delta` for the open block.
    fn delta(&self, delta: Value) -> Value {
        json!({
            "type": "content_block_delta",
            // A delta always follows a `content_block_start`.
            "index": self.open.as_ref().map(|open| open.index).unwrap_or(0),
            "delta": delta,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::ChatBackend;
    use axum::{
        Router,
        body::{Body, to_bytes},
        http::{Request, StatusCode},
        routing::post,
    };
    use futures::StreamExt;
    use heterion_router_core::{
        ChatCompletionRequest, ChatCompletionResponse, ChatMessage, StreamChunk, Usage,
    };
    use serde_json::json;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use tower::ServiceExt;

    /// The two routes exactly as `lib.rs` mounts them: the handlers are the
    /// contract, only the wiring belongs to the crate root.
    fn claude_router(state: AppState) -> Router {
        Router::new()
            .route("/v1/messages", post(messages))
            .route("/v1/messages/count_tokens", post(count_tokens))
            .with_state(state)
    }

    /// Records every request it is handed and answers with the last user turn,
    /// so a test can assert both what the provider was asked and what the
    /// client got back.
    #[derive(Clone, Default)]
    struct CaptureBackend {
        seen: Arc<Mutex<Vec<ChatCompletionRequest>>>,
        deltas: Vec<StreamChunk>,
        usage: Usage,
    }

    impl CaptureBackend {
        /// Answers with the usage every test below asserts on.
        fn answering() -> Self {
            Self {
                usage: Usage {
                    prompt_tokens: 11,
                    completion_tokens: 5,
                    total_tokens: 16,
                },
                ..Self::default()
            }
        }

        fn with_deltas(deltas: Vec<StreamChunk>) -> Self {
            Self {
                deltas,
                ..Self::answering()
            }
        }
    }

    #[async_trait::async_trait]
    impl ChatBackend for CaptureBackend {
        fn name(&self) -> &'static str {
            "capture"
        }

        async fn complete(
            &self,
            request: ChatCompletionRequest,
        ) -> Result<ChatCompletionResponse, GatewayError> {
            self.seen.lock().unwrap().push(request.clone());
            let text = request
                .messages
                .iter()
                .rev()
                .find(|message| message.role == "user")
                .map(ChatMessage::text)
                .unwrap_or_default();
            Ok(ChatCompletionResponse {
                id: "chatcmpl-capture".to_string(),
                object: "chat.completion",
                created: crate::ids::unix_seconds(),
                model: request.model,
                choices: vec![heterion_router_core::ChatChoice {
                    index: 0,
                    message: ChatMessage::plain("assistant".to_string(), text),
                    finish_reason: "stop".to_string(),
                }],
                usage: self.usage,
                raw: None,
            })
        }

        fn stream(&self, request: ChatCompletionRequest) -> backend::ChunkStream {
            self.seen.lock().unwrap().push(request.clone());
            let deltas = self.deltas.clone();
            futures::stream::iter(deltas.into_iter().map(Ok)).boxed()
        }
    }

    /// Refuses every request from inside a 200 body, the way a streaming
    /// upstream has to. The whole error object rides in the message because
    /// recovering the status needs the payload.
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
            self.calls.fetch_add(1, Ordering::Relaxed);
            let payload = self.payload.to_string();
            futures::stream::once(async move { Err(GatewayError::Upstream(payload)) }).boxed()
        }
    }

    /// Router + database in front of one provider that always refuses with
    /// `payload`. A breaker threshold of 1 makes "the breaker tripped"
    /// observable as "the provider stopped being asked".
    fn refusal_router(
        payload: Value,
        calls: Arc<AtomicUsize>,
    ) -> (Router, Arc<heterion_router_db::Db>) {
        let db = Arc::new(heterion_router_db::Db::open_in_memory().unwrap());
        db.migrate().unwrap();
        let mut backends: std::collections::HashMap<String, Arc<dyn ChatBackend>> =
            std::collections::HashMap::new();
        backends.insert(
            "openrouter".to_string(),
            Arc::new(crate::StatusAwareBackend::new(Arc::new(BodyErrorBackend {
                payload,
                calls: Arc::clone(&calls),
            }))),
        );
        let routing = crate::routing::RoutingBackend::with_breaker_settings(
            backends,
            heterion_router_routing::Router::new(vec![], vec![]),
            1,
            3600,
        );
        (
            claude_router(AppState::with_db(Arc::new(routing), db.clone())),
            db,
        )
    }

    /// A plain Anthropic request: system prompt, a content block array, tools
    /// and the sampling knobs.
    fn anthropic_request() -> Value {
        json!({
            "model": "claude-sonnet-4-6",
            "max_tokens": 256,
            "system": "be terse",
            "messages": [{
                "role": "user",
                "content": [
                    {"type": "text", "text": "hello"},
                    {"type": "text", "text": "world"},
                ],
            }],
            "tools": [{
                "name": "get_time",
                "description": "current time",
                "input_schema": {"type": "object", "properties": {"city": {"type": "string"}}},
            }],
            "tool_choice": {"type": "tool", "name": "get_time"},
            "stop_sequences": ["STOP"],
            "temperature": 0.25,
            "stream": false,
        })
    }

    /// The same request under a model the routing backend can dispatch: a
    /// bare model id has no `provider/model` candidate to plan against.
    fn routed_request() -> Value {
        let mut request = anthropic_request();
        request["model"] = json!("openrouter/claude-sonnet-4-6");
        request
    }

    /// POST `body` to `uri`; answer status + raw body text.
    async fn ask(router: Router, uri: &str, body: Value) -> (StatusCode, String) {
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

    /// `(event name, payload)` for every SSE frame, in order.
    fn frames(sse: &str) -> Vec<(String, Value)> {
        let mut out = Vec::new();
        let mut name = String::new();
        for line in sse.lines() {
            if let Some(event) = line.strip_prefix("event: ") {
                name = event.to_string();
            } else if let Some(data) = line.strip_prefix("data: ")
                && let Ok(value) = serde_json::from_str::<Value>(data)
            {
                out.push((name.clone(), value));
            }
        }
        out
    }

    fn event_names(sse: &str) -> Vec<String> {
        frames(sse).into_iter().map(|(name, _)| name).collect()
    }

    #[tokio::test]
    async fn a_request_comes_back_in_anthropic_shape() {
        let (status, text) = ask(
            claude_router(AppState::new(Arc::new(CaptureBackend::answering()))),
            "/v1/messages",
            anthropic_request(),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{text}");
        let body: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(body["type"], json!("message"));
        assert_eq!(body["role"], json!("assistant"));
        assert!(body["id"].as_str().unwrap().starts_with("msg_"), "{text}");
        assert_eq!(body["model"], json!("claude-sonnet-4-6"));
        assert_eq!(
            body["content"],
            json!([{"type": "text", "text": "helloworld"}]),
            "{text}"
        );
        assert_eq!(body["stop_reason"], json!("end_turn"));
        assert!(body["stop_sequence"].is_null(), "{text}");
        assert_eq!(body["usage"]["input_tokens"], json!(11));
        assert_eq!(body["usage"]["output_tokens"], json!(5));
    }

    #[tokio::test]
    async fn an_anthropic_request_reaches_the_backend_as_internal_chat() {
        let backend = CaptureBackend::answering();
        let (status, text) = ask(
            claude_router(AppState::new(Arc::new(backend.clone()))),
            "/v1/messages",
            anthropic_request(),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{text}");

        let seen = backend.seen.lock().unwrap()[0].clone();
        // The system prompt rides as the hoisted system turn the chat path
        // already forwards to every provider.
        assert_eq!(seen.messages[0].role, "system");
        assert_eq!(seen.messages[0].text(), "be terse");
        assert_eq!(seen.messages[1].role, "user");
        // The content block array survives as content parts; the chat path's
        // own text view concatenates them, as Anthropic does.
        assert_eq!(seen.messages[1].text(), "helloworld");
        assert_eq!(
            seen.messages[1].content,
            Some(json!([{"type": "text", "text": "hello"}, {"type": "text", "text": "world"}]))
        );
        // Anthropic tools are flat, OpenAI tools wrap `function`.
        assert_eq!(
            seen.tools.as_ref().unwrap()[0],
            json!({"type": "function", "function": {
                "name": "get_time",
                "description": "current time",
                "parameters": {"type": "object", "properties": {"city": {"type": "string"}}},
            }})
        );
        assert_eq!(
            seen.tool_choice,
            Some(json!({"type": "function", "function": {"name": "get_time"}}))
        );
        assert_eq!(seen.max_tokens, Some(256));
        assert_eq!(seen.stop, Some(json!(["STOP"])));
        assert_eq!(seen.temperature, Some(0.25));
        assert!(!seen.stream);
        assert_eq!(seen.model, "claude-sonnet-4-6");
    }

    #[tokio::test]
    async fn a_tool_turn_and_its_result_round_trip_into_the_chat_shape() {
        let backend = CaptureBackend::answering();
        let (status, text) = ask(
            claude_router(AppState::new(Arc::new(backend.clone()))),
            "/v1/messages",
            json!({
                "model": "claude-sonnet-4-6",
                "max_tokens": 64,
                "messages": [
                    {"role": "user", "content": "time?"},
                    {"role": "assistant", "content": [
                        {"type": "tool_use", "id": "toolu_1", "name": "get_time",
                         "input": {"city": "SF"}},
                    ]},
                    {"role": "user", "content": [
                        {"type": "tool_result", "tool_use_id": "toolu_1", "content": "12:00"},
                    ]},
                ],
            }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{text}");

        let seen = backend.seen.lock().unwrap()[0].clone();
        assert_eq!(seen.messages.len(), 3, "{text}");
        let call = &seen.messages[1].tool_calls.as_ref().unwrap()[0];
        assert_eq!(call["id"], json!("toolu_1"));
        assert_eq!(call["function"]["name"], json!("get_time"));
        assert_eq!(call["function"]["arguments"], json!("{\"city\":\"SF\"}"));
        // An assistant turn that only calls a tool carries no content, the
        // shape every OpenAI-shaped upstream expects.
        assert!(seen.messages[1].content.is_none());
        // The tool result becomes its own `tool` turn.
        assert_eq!(seen.messages[2].role, "tool");
        assert_eq!(seen.messages[2].tool_call_id.as_deref(), Some("toolu_1"));
        assert_eq!(seen.messages[2].text(), "12:00");
    }

    #[tokio::test]
    async fn a_tool_use_answer_comes_back_as_a_tool_use_block() {
        #[derive(Clone)]
        struct ToolBackend;

        #[async_trait::async_trait]
        impl ChatBackend for ToolBackend {
            fn name(&self) -> &'static str {
                "tool"
            }

            async fn complete(
                &self,
                request: ChatCompletionRequest,
            ) -> Result<ChatCompletionResponse, GatewayError> {
                Ok(ChatCompletionResponse {
                    id: "chatcmpl-tool".to_string(),
                    object: "chat.completion",
                    created: crate::ids::unix_seconds(),
                    model: request.model,
                    choices: vec![heterion_router_core::ChatChoice {
                        index: 0,
                        message: ChatMessage {
                            role: "assistant".to_string(),
                            content: None,
                            reasoning_content: None,
                            tool_calls: Some(json!([{
                                "id": "call_1",
                                "type": "function",
                                "function": {"name": "get_time", "arguments": "{\"city\":\"SF\"}"},
                            }])),
                            tool_call_id: None,
                            name: None,
                        },
                        finish_reason: "tool_calls".to_string(),
                    }],
                    usage: Usage::default(),
                    raw: None,
                })
            }

            fn stream(&self, _request: ChatCompletionRequest) -> backend::ChunkStream {
                futures::stream::empty().boxed()
            }
        }

        let (status, text) = ask(
            claude_router(AppState::new(Arc::new(ToolBackend))),
            "/v1/messages",
            json!({"model": "claude-sonnet-4-6", "max_tokens": 64,
                   "messages": [{"role": "user", "content": "time?"}]}),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{text}");
        let body: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(body["stop_reason"], json!("tool_use"));
        assert_eq!(
            body["content"],
            json!([{"type": "tool_use", "id": "call_1", "name": "get_time",
                    "input": {"city": "SF"}}])
        );
    }

    /// Anthropic pairs every `content_block_start` with a
    /// `content_block_stop`. A stream that ends without a terminal chunk still
    /// has an open block, so closing it is not optional bookkeeping — an
    /// unpaired start leaves a strict client with a message it cannot assemble.
    #[tokio::test]
    async fn a_stream_without_a_terminal_chunk_closes_its_open_block() {
        // No `finish_reason` on the last chunk: the stream stops mid-message.
        let backend = CaptureBackend::with_deltas(vec![StreamChunk::content("half")]);
        let mut request = anthropic_request();
        request["stream"] = json!(true);
        let (status, text) = ask(
            claude_router(AppState::new(Arc::new(backend))),
            "/v1/messages",
            request,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{text}");

        let names = event_names(&text);
        assert!(
            names.iter().any(|n| n == "content_block_start"),
            "expected an open block: {names:?}"
        );
        assert!(
            names.iter().any(|n| n == "content_block_stop"),
            "every content_block_start needs a matching stop: {names:?}"
        );
        let stop = names
            .iter()
            .position(|k| k.as_str() == "content_block_stop")
            .unwrap();
        let delta = names
            .iter()
            .position(|k| k.as_str() == "message_delta")
            .unwrap();
        assert!(stop < delta, "the block must close first: {names:?}");
        assert_eq!(
            names.last().map(String::as_str),
            Some("message_stop"),
            "{names:?}"
        );
    }

    #[tokio::test]
    async fn streaming_follows_the_anthropic_event_dialect() {
        let backend = CaptureBackend::with_deltas(vec![
            StreamChunk::content("Hel"),
            StreamChunk::content("lo"),
            StreamChunk {
                finish_reason: Some("stop".to_string()),
                usage: Some(Usage {
                    prompt_tokens: 11,
                    completion_tokens: 5,
                    total_tokens: 16,
                }),
                ..Default::default()
            },
        ]);
        let mut request = anthropic_request();
        request["stream"] = json!(true);
        let (status, text) = ask(
            claude_router(AppState::new(Arc::new(backend))),
            "/v1/messages",
            request,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{text}");

        assert_eq!(
            event_names(&text),
            vec![
                "message_start",
                "content_block_start",
                "content_block_delta",
                "content_block_delta",
                "content_block_stop",
                "message_delta",
                "message_stop",
            ],
            "{text}"
        );
        let frames = frames(&text);
        assert_eq!(frames[0].1["message"]["role"], json!("assistant"), "{text}");
        assert_eq!(
            frames[1].1["content_block"],
            json!({"type": "text", "text": ""}),
            "{text}"
        );
        assert_eq!(
            frames[2].1["delta"],
            json!({"type": "text_delta", "text": "Hel"}),
            "{text}"
        );
        assert_eq!(
            frames[3].1["delta"],
            json!({"type": "text_delta", "text": "lo"}),
            "{text}"
        );
        assert_eq!(frames[5].1["delta"]["stop_reason"], json!("end_turn"));
        assert_eq!(frames[5].1["usage"]["output_tokens"], json!(5));
        assert_eq!(frames[5].1["usage"]["input_tokens"], json!(11));
        // Every block frame belongs to the block that opened at index 0.
        for (_, payload) in &frames[1..5] {
            assert_eq!(payload["index"], json!(0), "{text}");
        }
        // The Anthropic dialect has no `[DONE]` sentinel.
        assert!(!text.contains("[DONE]"), "{text}");
    }

    #[tokio::test]
    async fn a_streaming_tool_call_becomes_a_tool_use_block() {
        let backend = CaptureBackend::with_deltas(vec![StreamChunk {
            tool_calls: Some(json!([{
                "index": 0,
                "id": "call_1",
                "type": "function",
                "function": {"name": "get_time", "arguments": "{\"city\":\"SF\"}"},
            }])),
            ..Default::default()
        }]);
        let mut request = anthropic_request();
        request["stream"] = json!(true);
        let (status, text) = ask(
            claude_router(AppState::new(Arc::new(backend))),
            "/v1/messages",
            request,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{text}");
        let frames = frames(&text);
        let started = frames
            .iter()
            .find(|(name, _)| name == "content_block_start")
            .expect("a tool_use block opens");
        assert_eq!(
            started.1["content_block"]["type"],
            json!("tool_use"),
            "{text}"
        );
        assert_eq!(started.1["content_block"]["name"], json!("get_time"));
        let delta = frames
            .iter()
            .find(|(name, _)| name == "content_block_delta")
            .expect("arguments stream as input_json_delta");
        assert_eq!(
            delta.1["delta"],
            json!({"type": "input_json_delta", "partial_json": "{\"city\":\"SF\"}"}),
            "{text}"
        );
    }

    #[tokio::test]
    async fn count_tokens_counts_a_non_trivial_body() {
        let (status, text) = ask(
            claude_router(AppState::new(Arc::new(CaptureBackend::answering()))),
            "/v1/messages/count_tokens",
            anthropic_request(),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{text}");
        let body: Value = serde_json::from_str(&text).unwrap();
        let count = body["input_tokens"]
            .as_u64()
            .unwrap_or_else(|| panic!("input_tokens missing: {text}"));
        assert!(count > 0, "{text}");
        // A longer prompt must not report the same count as a short one.
        let (_, longer) = ask(
            claude_router(AppState::new(Arc::new(CaptureBackend::answering()))),
            "/v1/messages/count_tokens",
            json!({"model": "claude-sonnet-4-6", "messages": [
                {"role": "user", "content": "x".repeat(4000)}
            ]}),
        )
        .await;
        let longer: u64 = serde_json::from_str::<Value>(&longer).unwrap()["input_tokens"]
            .as_u64()
            .unwrap();
        assert!(longer > count, "{longer} vs {count}");
    }

    #[tokio::test]
    async fn count_tokens_rejects_an_empty_messages_array() {
        let (status, text) = ask(
            claude_router(AppState::new(Arc::new(CaptureBackend::answering()))),
            "/v1/messages/count_tokens",
            json!({"model": "claude-sonnet-4-6", "max_tokens": 16, "messages": []}),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{text}");
    }

    #[tokio::test]
    async fn messages_rejects_an_empty_messages_array() {
        let (status, text) = ask(
            claude_router(AppState::new(Arc::new(CaptureBackend::answering()))),
            "/v1/messages",
            json!({"model": "claude-sonnet-4-6", "max_tokens": 16, "messages": []}),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{text}");
    }

    #[tokio::test]
    async fn a_refusal_carrying_429_is_not_a_502_and_does_not_trip_the_breaker() {
        let calls = Arc::new(AtomicUsize::new(0));
        let (router, _db) = refusal_router(
            json!({"error": {"message": "Rate limit reached", "code": "rate_limit_exceeded"}}),
            Arc::clone(&calls),
        );
        for _ in 0..2 {
            let (status, text) = ask(router.clone(), "/v1/messages", routed_request()).await;
            assert_eq!(status, StatusCode::TOO_MANY_REQUESTS, "{text}");
            assert!(text.contains("upstream request failed"), "{text}");
            assert!(!text.contains("Rate limit reached"), "leaked: {text}");
        }
        // Frozen contract (AGENTS.md): a provider 4xx does not trip the
        // breaker, so the second request still reached the provider.
        assert_eq!(
            calls.load(Ordering::Relaxed),
            2,
            "a client-side 4xx tripped the breaker"
        );
    }

    #[tokio::test]
    async fn raw_upstream_text_never_reaches_the_client() {
        let canary = "CANARY-7c1e-upstream-secret";
        let calls = Arc::new(AtomicUsize::new(0));
        let (router, _db) = refusal_router(
            json!({"error": {"message": canary, "status": 500}}),
            Arc::clone(&calls),
        );
        let (status, text) = ask(router.clone(), "/v1/messages", routed_request()).await;
        assert_eq!(status, StatusCode::BAD_GATEWAY, "{text}");
        assert!(!text.contains(canary), "{text}");

        // The streaming half: the 200 is already spent, so the failure rides
        // in the error frame and must still be sanitized.
        let mut request = routed_request();
        request["stream"] = json!(true);
        let (status, text) = ask(router, "/v1/messages", request).await;
        assert_eq!(status, StatusCode::OK, "{text}");
        assert!(text.contains("upstream request failed"), "{text}");
        assert!(!text.contains(canary), "{text}");
        // Anthropic clients read the failure as `event: error`, not a close.
        assert!(!text.contains("event: message_stop"), "{text}");
    }

    #[tokio::test]
    async fn usage_and_call_log_rows_are_written_for_a_messages_request() {
        let db = Arc::new(heterion_router_db::Db::open_in_memory().unwrap());
        db.migrate().unwrap();
        let backend = CaptureBackend::answering();
        let state = AppState::with_db(Arc::new(backend), db.clone());
        let (status, text) = ask(claude_router(state), "/v1/messages", anthropic_request()).await;
        assert_eq!(status, StatusCode::OK, "{text}");

        let guard = db.connection();
        let usage: (String, i64, i64, i64) = guard
            .query_row(
                "SELECT provider, tokens_input, tokens_output, ttft_ms FROM usage_history",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap();
        // The backend that answered is the provider, never "routing".
        assert_eq!(usage.0, "capture");
        assert_eq!((usage.1, usage.2), (11, 5));
        // A completion has no first token to measure.
        assert_eq!(usage.3, 0);
        // The row is filed under this surface's own `request_type`, and the
        // call log under its HTTP path. The writer's `path` column has no
        // `messages` arm yet, so it names the chat path whose machinery it
        // shares; one match arm in `lib.rs::record_completion_usage` is what
        // turns it into `/v1/messages`.
        let call: (Option<String>, String) = guard
            .query_row("SELECT request_type, path FROM call_logs", [], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })
            .unwrap();
        assert_eq!(call.0.as_deref(), Some("messages"));
        assert!(call.1.starts_with("/v1/"), "{call:?}");
    }

    #[tokio::test]
    async fn a_streaming_request_books_the_tokens_it_emitted() {
        let db = Arc::new(heterion_router_db::Db::open_in_memory().unwrap());
        db.migrate().unwrap();
        let backend = CaptureBackend::with_deltas(vec![
            StreamChunk::content("hi"),
            StreamChunk {
                finish_reason: Some("stop".to_string()),
                usage: Some(Usage {
                    prompt_tokens: 11,
                    completion_tokens: 5,
                    total_tokens: 16,
                }),
                ..Default::default()
            },
        ]);
        let mut request = anthropic_request();
        request["stream"] = json!(true);
        let state = AppState::with_db(Arc::new(backend), db.clone());
        let (status, text) = ask(claude_router(state), "/v1/messages", request).await;
        assert_eq!(status, StatusCode::OK, "{text}");

        let guard = db.connection();
        let (tokens_in, tokens_out, status): (i64, i64, i64) = guard
            .query_row(
                "SELECT tokens_in, tokens_out, status FROM call_logs \
                 WHERE request_type = 'messages'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!((tokens_in, tokens_out), (11, 5));
        assert_eq!(status, 200);
        // The first chunk latched a time to first token, which the usage row
        // keeps.
        let ttft_ms: i64 = guard
            .query_row("SELECT ttft_ms FROM usage_history", [], |row| row.get(0))
            .unwrap();
        assert!(ttft_ms >= 0, "{ttft_ms}");
    }

    #[tokio::test]
    async fn a_count_tokens_request_writes_no_usage_rows() {
        // Nothing was called upstream, so nothing may be billed.
        let db = Arc::new(heterion_router_db::Db::open_in_memory().unwrap());
        db.migrate().unwrap();
        let state = AppState::with_db(Arc::new(CaptureBackend::answering()), db.clone());
        let (status, _) = ask(
            claude_router(state),
            "/v1/messages/count_tokens",
            anthropic_request(),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let guard = db.connection();
        let rows: i64 = guard
            .query_row("SELECT COUNT(*) FROM usage_history", [], |row| row.get(0))
            .unwrap();
        assert_eq!(rows, 0);
    }

    /// The keyless contract (`keyless_provider_sends_no_bearer`) has an
    /// equivalent here: this handler adds no HTTP layer of its own, so a
    /// provider with an empty credential pool must still go out without a
    /// bearer — driven end to end through the real executor.
    #[tokio::test]
    async fn a_keyless_backend_is_sent_no_bearer() {
        use wiremock::matchers::{header_exists, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .and(header_exists("authorization"))
            .respond_with(ResponseTemplate::new(500))
            .expect(0)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": "1",
                "model": "gemini",
                "choices": [{
                    "index": 0,
                    "message": {"role": "assistant", "content": "hi"},
                    "finish_reason": "stop",
                }],
                "usage": {"prompt_tokens": 3, "completion_tokens": 2, "total_tokens": 5},
            })))
            .expect(1)
            .mount(&server)
            .await;

        let backend = crate::openai::ProviderOpenAiBackend::new(
            "pollinations".to_string(),
            crate::openai::resolve_chat_url(&format!("{}/v1", server.uri())),
            Vec::new(),
        )
        .unwrap();
        assert_eq!(backend.token_count(), 0);

        let (status, text) = ask(
            claude_router(AppState::new(Arc::new(backend))),
            "/v1/messages",
            anthropic_request(),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{text}");
        server.verify().await;
    }

    #[tokio::test]
    async fn a_malformed_body_is_answered_in_the_project_envelope() {
        // The `/v1/*` surfaces take their body through the gateway's own
        // `JsonBody`, so a body that will not parse is a JSON error here and
        // not axum's plain-text sentence.
        let router = claude_router(AppState::new(Arc::new(CaptureBackend::answering())));
        let response = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/messages")
                    .header("content-type", "application/json")
                    .body(Body::from("{not json"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let text = String::from_utf8_lossy(&bytes);
        assert!(serde_json::from_str::<Value>(&text).is_ok(), "{text}");
    }

    #[tokio::test]
    async fn a_request_without_a_model_is_rejected() {
        // The allow-list guard itself is `lib.rs`'s shared one; what this pins
        // is that the handler consults it and that a model-less body is our
        // own 400, not an upstream round trip.
        let (status, text) = ask(
            claude_router(AppState::new(Arc::new(CaptureBackend::answering()))),
            "/v1/messages",
            json!({"messages": [{"role": "user", "content": "hi"}]}),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{text}");
        assert!(text.contains("model is required"), "{text}");
    }
}

//! Generic OpenAI Responses (`openai-responses` format) backend over HTTP.
//!
//! Handles providers like `deepseek` that expose a `/responses` API endpoint
//! rather than `/chat/completions`. Translates OpenAI chat requests to
//! Responses requests using `heterion_router_translate::requests::openai_to_responses`
//! and responses back via `heterion_router_translate::responses`.

use async_trait::async_trait;
use futures::StreamExt;
use heterion_router_core::{
    ChatChoice, ChatCompletionRequest, ChatCompletionResponse, ChatMessage, GatewayError,
    StreamChunk, Usage,
};
use heterion_router_http::{HttpClient, SseDecoder, SseEvent};
use heterion_router_translate::{
    requests::openai_to_responses,
    responses::{Converted, ResponsesState, convert_event},
};
use serde_json::{Value, json};
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;

use crate::backend::{
    ChatBackend, ChunkStream, next_token, openai_chunk_to_stream_chunk, openai_request_body,
};
use crate::ids;
use crate::openai::try_each_credential;

/// Message for an SSE body that ended without the terminal event it promised.
const UNTERMINATED: &str = "upstream stream ended before terminal event";

/// What a `data: [DONE]` sentinel means when the upstream never sent the
/// terminal chunk it normally precedes.
///
/// The two answers are not the same behaviour: an OpenAI-shaped backend
/// synthesises the `stop` chunk its clients wait for, while a backend whose
/// translator reports completion out of band must fail rather than invent one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MissingTerminal {
    /// Send a synthetic `stop` chunk, then end the stream cleanly.
    SynthesizeStop,
    /// Fail the stream: the sentinel is not a substitute for the terminal event.
    ///
    /// The Gemini pump picks this: its adapter treats a missing `[DONE]` as an
    /// upstream fault rather than synthesizing a `stop`.
    Fail,
}

/// Event handler for a Responses API stream: the `event:` name (falling back
/// to the payload `type`) through [`convert_event`].
///
/// Owns the [`ResponsesState`] translation needs, so every backend speaking
/// this protocol shares one handler and its `Fail` arm becomes the single
/// upstream-failure path.
pub(crate) fn responses_event_handler(
    model: &str,
) -> impl FnMut(&SseEvent, &Value) -> Result<Vec<Value>, GatewayError> {
    let mut translator = ResponsesState::new(
        "chatcmpl-rust".to_string(),
        ids::unix_seconds(),
        model.to_string(),
    );
    move |event, data| {
        let event_type = event
            .event
            .as_deref()
            .or_else(|| data.get("type").and_then(Value::as_str))
            .unwrap_or("");
        Ok(match convert_event(&mut translator, event_type, data) {
            Converted::Emit(chunk_json) => vec![chunk_json],
            Converted::Ignore => Vec::new(),
            Converted::Fail(message) => return Err(GatewayError::Upstream(message)),
        })
    }
}

/// Drain an upstream SSE body into [`StreamChunk`]s on `tx`.
///
/// One pump for every SSE-speaking backend: it owns the [`SseDecoder`], the
/// `[DONE]` sentinel, the terminal-chunk bookkeeping and the fail-closed
/// [`UNTERMINATED`] error. `on_event` translates one decoded frame into zero
/// or more OpenAI-shaped chunk JSONs (empty = nothing to emit) or fails the
/// stream, so per-provider state stays in the backend that owns it.
///
/// Returns `Ok(())` when the client hung up or the stream ended with its
/// terminal chunk; the caller forwards a returned error to `tx` itself.
///
/// [`SseDecoder`]'s fail-closed overflow path arrives here as an ordinary
/// `error` frame carrying only the decoder's own reason, so every backend
/// ends up on the same sanitized client-facing error and no upstream text is
/// echoed on the wire.
pub(crate) async fn pump_sse_stream<F>(
    response: reqwest::Response,
    tx: &mpsc::Sender<Result<StreamChunk, GatewayError>>,
    mut on_event: F,
    missing_terminal: MissingTerminal,
) -> Result<(), GatewayError>
where
    F: FnMut(&SseEvent, &Value) -> Result<Vec<Value>, GatewayError>,
{
    let mut bytes = response.bytes_stream();
    let mut decoder = SseDecoder::new();
    let mut finish_seen = false;

    loop {
        let chunk = tokio::select! {
            _ = tx.closed() => return Ok(()),
            chunk = bytes.next() => chunk,
        };
        let Some(chunk) = chunk else {
            break;
        };
        let chunk = chunk.map_err(|error| GatewayError::Upstream(error.to_string()))?;
        for event in decoder.push_bytes(&chunk) {
            if event.data.trim() == "[DONE]" {
                return end_on_done(finish_seen, missing_terminal, tx).await;
            }
            let data: Value = match serde_json::from_str(&event.data) {
                Ok(data) => data,
                Err(_) => continue,
            };
            for chunk_json in on_event(&event, &data)? {
                let Some(stream_chunk) = openai_chunk_to_stream_chunk(&chunk_json) else {
                    continue;
                };
                if stream_chunk.finish_reason.is_some() {
                    finish_seen = true;
                }
                if tx.send(Ok(stream_chunk)).await.is_err() {
                    return Ok(());
                }
            }
        }
    }

    if !finish_seen {
        return Err(GatewayError::Upstream(UNTERMINATED.to_string()));
    }
    Ok(())
}

/// Finish on a `data: [DONE]` sentinel, per [`MissingTerminal`].
async fn end_on_done(
    finish_seen: bool,
    missing_terminal: MissingTerminal,
    tx: &mpsc::Sender<Result<StreamChunk, GatewayError>>,
) -> Result<(), GatewayError> {
    if finish_seen {
        return Ok(());
    }
    match missing_terminal {
        MissingTerminal::SynthesizeStop => {
            let _ = tx.send(Ok(StreamChunk::done("stop"))).await;
            Ok(())
        }
        MissingTerminal::Fail => Err(GatewayError::Upstream(UNTERMINATED.to_string())),
    }
}

/// Generic Responses API executor over HTTP with token rotation.
#[derive(Debug, Clone)]
pub struct ResponsesBackend {
    http: HttpClient,
    provider: String,
    endpoint_url: String,
    tokens: Vec<String>,
    next: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}

impl ResponsesBackend {
    /// Build for `provider` against `endpoint_url` with rotating tokens.
    pub fn new(
        provider: String,
        endpoint_url: String,
        tokens: Vec<String>,
    ) -> Result<Self, heterion_router_http::client::HttpError> {
        Ok(Self {
            http: HttpClient::new(600)?,
            provider,
            endpoint_url: endpoint_url.trim_end_matches('/').to_string(),
            tokens,
            next: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        })
    }

    /// Pool size (number of rotated credentials).
    pub fn token_count(&self) -> usize {
        self.tokens.len()
    }

    async fn drive_stream(
        &self,
        request: ChatCompletionRequest,
        tx: mpsc::Sender<Result<StreamChunk, GatewayError>>,
    ) -> Result<(), GatewayError> {
        if request.messages.is_empty() {
            return Err(GatewayError::InvalidRequest("messages is empty".into()));
        }

        let mut upstream = openai_to_responses(&request.model, &openai_request_body(&request));
        if let Some(input) = &request.responses_input {
            upstream["input"] = input.clone();
        }
        upstream["stream"] = Value::from(true);
        let response = self
            .http
            .post_json(
                &self.endpoint_url,
                next_token(&self.tokens, &self.next),
                &upstream,
            )
            .await
            .map_err(|error| GatewayError::Upstream(error.to_string()))?;

        pump_sse_stream(
            response,
            &tx,
            responses_event_handler(&request.model),
            MissingTerminal::SynthesizeStop,
        )
        .await
    }
}

#[async_trait]
impl ChatBackend for ResponsesBackend {
    fn name(&self) -> &'static str {
        "responses"
    }

    async fn complete(
        &self,
        request: ChatCompletionRequest,
    ) -> Result<ChatCompletionResponse, GatewayError> {
        if request.messages.is_empty() {
            return Err(GatewayError::InvalidRequest("messages is empty".into()));
        }
        let mut upstream = openai_to_responses(&request.model, &openai_request_body(&request));
        if let Some(input) = &request.responses_input {
            upstream["input"] = input.clone();
        }
        upstream["stream"] = Value::from(false);

        // A keyless pool gets exactly one attempt, a pool one per credential.
        let payload = try_each_credential(&self.provider, self.tokens.len().max(1), || async {
            let response = self
                .http
                .post_json(
                    &self.endpoint_url,
                    next_token(&self.tokens, &self.next),
                    &upstream,
                )
                .await
                .map_err(|error| GatewayError::Upstream(error.to_string()))?;
            response
                .json()
                .await
                .map_err(|error| GatewayError::Upstream(error.to_string()))
        })
        .await?;
        Ok(response_to_completion(&request.model, &payload))
    }

    fn stream(&self, request: ChatCompletionRequest) -> ChunkStream {
        let (tx, rx) = mpsc::channel(64);
        let backend = self.clone();
        tokio::spawn(async move {
            if let Err(error) = backend.drive_stream(request, tx.clone()).await {
                let _ = tx.send(Err(error)).await;
            }
        });
        Box::pin(ReceiverStream::new(rx))
    }
}

/// Convert a completed Responses API object to a chat completion.
///
/// Canonical copy for every backend that speaks the Responses API
/// (`grok_cli` included): one request body in, one chat completion out.
pub(crate) fn response_to_completion(model: &str, payload: &Value) -> ChatCompletionResponse {
    let id = payload
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or("resp-unknown")
        .to_string();

    let mut text = String::new();
    let mut tool_calls: Vec<Value> = Vec::new();
    if let Some(output) = payload.get("output").and_then(Value::as_array) {
        for item in output {
            match item.get("type").and_then(Value::as_str) {
                Some("message") => {
                    if let Some(content) = item.get("content").and_then(Value::as_array) {
                        for part in content {
                            if part.get("type").and_then(Value::as_str) == Some("output_text")
                                && let Some(part_text) = part.get("text").and_then(Value::as_str)
                            {
                                text.push_str(part_text);
                            }
                        }
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
                            "arguments": item
                                .get("arguments")
                                .and_then(Value::as_str)
                                .unwrap_or("{}"),
                        },
                    }));
                }
                _ => {}
            }
        }
    }

    let usage = payload.get("usage");
    let prompt = usage
        .and_then(|usage| usage.get("input_tokens"))
        .and_then(Value::as_u64)
        .unwrap_or(0) as u32;
    let completion = usage
        .and_then(|usage| usage.get("output_tokens"))
        .and_then(Value::as_u64)
        .unwrap_or(0) as u32;
    let total = usage
        .and_then(|usage| usage.get("total_tokens"))
        .and_then(Value::as_u64)
        .map(|total| total as u32)
        .unwrap_or_else(|| prompt + completion);

    let has_calls = !tool_calls.is_empty();
    let message = ChatMessage {
        role: "assistant".to_string(),
        content: if text.is_empty() {
            None
        } else {
            Some(Value::from(text))
        },
        reasoning_content: None,
        tool_calls: has_calls.then_some(Value::Array(tool_calls)),
        tool_call_id: None,
        name: None,
    };

    ChatCompletionResponse {
        id,
        object: "chat.completion",
        created: ids::unix_seconds(),
        model: model.to_string(),
        choices: vec![ChatChoice {
            index: 0,
            message,
            finish_reason: if has_calls { "tool_calls" } else { "stop" }.to_string(),
        }],
        usage: Usage {
            prompt_tokens: prompt,
            completion_tokens: completion,
            total_tokens: total,
        },
        raw: Some(payload.clone()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn request() -> ChatCompletionRequest {
        ChatCompletionRequest {
            model: "deepseek-v4-pro".to_string(),
            messages: vec![ChatMessage::plain("user".to_string(), "hello".to_string())],
            stream: false,
            max_tokens: None,
            temperature: None,
            top_p: None,
            tools: None,
            tool_choice: None,
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn completes_via_responses_api() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/responses"))
            .and(header("authorization", "Bearer test-key"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": "resp-123",
                "output": [{
                    "type": "message",
                    "content": [{"type": "output_text", "text": "PONG"}]
                }],
                "usage": {"input_tokens": 5, "output_tokens": 2, "total_tokens": 7},
                "metadata": {"opaque": "keep"}
            })))
            .expect(1)
            .mount(&server)
            .await;

        let backend = ResponsesBackend::new(
            "deepseek".to_string(),
            format!("{}/responses", server.uri()),
            vec!["test-key".to_string()],
        )
        .unwrap();

        let response = backend.complete(request()).await.unwrap();
        assert_eq!(response.id, "resp-123");
        assert_eq!(
            response.raw.as_ref().unwrap()["metadata"]["opaque"],
            json!("keep")
        );
        assert_eq!(response.choices[0].message.text(), "PONG");
        assert_eq!(response.usage.total_tokens, 7);
        server.verify().await;

        // The pooled credential must be the one presented upstream.
        let sent = server.received_requests().await.unwrap();
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].headers["authorization"], "Bearer test-key");
    }

    /// A `reqwest::Response` over `body`, so the pump decodes a real byte
    /// stream instead of a hand-fed list of frames.
    fn sse_response(body: String) -> reqwest::Response {
        reqwest::Response::from(
            axum::http::Response::builder()
                .status(200)
                .body(reqwest::Body::from(body))
                .expect("a fixed status and body always build"),
        )
    }

    /// Run the canonical pump over `body`; collect what it emitted plus how it
    /// returned. The pump's `Err` is the caller's to forward, so the emitted
    /// chunks never carry a failure of their own.
    async fn pump(
        body: String,
        on_event: impl FnMut(&SseEvent, &Value) -> Result<Vec<Value>, GatewayError> + Send + 'static,
        missing_terminal: MissingTerminal,
    ) -> (
        Vec<Result<StreamChunk, GatewayError>>,
        Result<(), GatewayError>,
    ) {
        let (tx, mut rx) = mpsc::channel(64);
        let driving = tokio::spawn(async move {
            pump_sse_stream(sse_response(body), &tx, on_event, missing_terminal).await
        });
        let result = driving.await.expect("the pump task must not panic");
        let mut emitted = Vec::new();
        while let Some(chunk) = rx.recv().await {
            emitted.push(chunk);
        }
        (emitted, result)
    }

    fn upstream_message(error: GatewayError) -> String {
        match error {
            GatewayError::Upstream(message) => message,
            other => panic!("expected an upstream failure, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn pump_emits_deltas_then_the_terminal_chunk() {
        let body = concat!(
            "event: response.created\ndata: {\"type\":\"response.created\"}\n\n",
            "event: response.output_text.delta\n",
            "data: {\"type\":\"response.output_text.delta\",\"delta\":\"Hi\"}\n\n",
            ": keepalive\n\n",
            "event: response.output_text.delta\ndata: {\"delta\":\"!\"}\n\n",
            "event: response.completed\n",
            "data: {\"response\":{\"usage\":{\"input_tokens\":5,\"output_tokens\":2}}}\n\n",
            "data: [DONE]\n\n",
        )
        .to_string();

        let (chunks, result) = pump(
            body,
            responses_event_handler("deepseek-v4-pro"),
            MissingTerminal::SynthesizeStop,
        )
        .await;

        assert!(result.is_ok(), "{result:?}");
        assert_eq!(chunks.len(), 3, "{chunks:?}");
        let chunks: Vec<StreamChunk> = chunks.into_iter().map(Result::unwrap).collect();
        // The second delta carries no `event:` line: the payload `type` is the
        // fallback the handler has to keep honouring.
        assert_eq!(chunks[0].content.as_deref(), Some("Hi"));
        assert_eq!(chunks[1].content.as_deref(), Some("!"));
        assert_eq!(chunks[2].finish_reason.as_deref(), Some("stop"));
        assert_eq!(
            chunks[2]
                .usage
                .expect("terminal chunk carries usage")
                .total_tokens,
            7
        );
    }

    #[tokio::test]
    async fn pump_fails_the_stream_on_an_upstream_error_frame() {
        let body = concat!(
            "event: response.output_text.delta\ndata: {\"delta\":\"partial\"}\n\n",
            "event: error\ndata: {\"error\":{\"message\":\"model exploded\"}}\n\n",
        )
        .to_string();

        let (chunks, result) = pump(
            body,
            responses_event_handler("deepseek-v4-pro"),
            MissingTerminal::SynthesizeStop,
        )
        .await;

        // The pump hands the failure back rather than emitting it in band;
        // `lib.rs` sanitizes it on the way out (see its
        // `stream_errors_never_leak_upstream_text`).
        assert!(
            upstream_message(result.expect_err("an error frame must fail the stream"))
                .contains("model exploded")
        );
        assert_eq!(chunks.len(), 1, "{chunks:?}");
        assert_eq!(
            chunks[0]
                .as_ref()
                .expect("partial delta")
                .content
                .as_deref(),
            Some("partial")
        );
    }

    #[tokio::test]
    async fn pump_fails_closed_on_an_overflowed_event() {
        let canary = "CANARY-overflow-marker";
        let body = format!(
            "data: {canary}{}",
            "x".repeat(heterion_router_http::sse::MAX_SSE_EVENT_BYTES + 4096)
        );

        let (chunks, result) = pump(
            body,
            responses_event_handler("deepseek-v4-pro"),
            MissingTerminal::SynthesizeStop,
        )
        .await;

        // The decoder's own reason is the whole message: an un-terminated
        // event must never echo the megabyte of upstream text behind it.
        let message = upstream_message(result.expect_err("an overflowed event must fail"));
        assert_eq!(message, "upstream sse event exceeded the decoder limit");
        assert!(!message.contains(canary), "{message}");
        assert!(
            chunks.iter().all(|chunk| match chunk {
                Ok(chunk) => !chunk
                    .content
                    .as_deref()
                    .is_some_and(|text| text.contains(canary)),
                Err(_) => false,
            }),
            "{chunks:?}"
        );
    }

    #[tokio::test]
    async fn pump_reports_an_empty_body_as_unterminated() {
        let (chunks, result) = pump(
            String::new(),
            responses_event_handler("deepseek-v4-pro"),
            MissingTerminal::SynthesizeStop,
        )
        .await;

        assert!(chunks.is_empty(), "{chunks:?}");
        assert_eq!(
            upstream_message(result.expect_err("an empty body is not a finished stream")),
            UNTERMINATED
        );
    }

    #[tokio::test]
    async fn pump_synthesizes_the_terminal_chunk_a_done_sentinel_owes() {
        let body = concat!(
            "event: response.output_text.delta\ndata: {\"delta\":\"Hi\"}\n\n",
            "data: [DONE]\n\n",
        )
        .to_string();

        let (chunks, result) = pump(
            body,
            responses_event_handler("deepseek-v4-pro"),
            MissingTerminal::SynthesizeStop,
        )
        .await;

        assert!(result.is_ok(), "{result:?}");
        let chunks: Vec<StreamChunk> = chunks.into_iter().map(Result::unwrap).collect();
        assert_eq!(chunks.len(), 2, "{chunks:?}");
        assert_eq!(chunks[1].finish_reason.as_deref(), Some("stop"));
    }

    #[tokio::test]
    async fn pump_refuses_to_invent_a_terminal_chunk_under_the_strict_policy() {
        let body = concat!(
            "event: response.output_text.delta\ndata: {\"delta\":\"Hi\"}\n\n",
            "data: [DONE]\n\n",
        )
        .to_string();

        let (chunks, result) = pump(
            body,
            responses_event_handler("deepseek-v4-pro"),
            MissingTerminal::Fail,
        )
        .await;

        assert_eq!(chunks.len(), 1, "{chunks:?}");
        assert_eq!(
            upstream_message(result.expect_err("a bare [DONE] is not a finished stream")),
            UNTERMINATED
        );
    }
}

/// Characterization of the credential-pool walk `ResponsesBackend::complete`
/// performs, in the terms the canonical `crate::openai::try_each_credential`
/// speaks: one attempt per credential, a rejected credential rotating, the last
/// error on exhaustion, and `response_to_completion` still mapping the winner.
#[cfg(test)]
mod credential_pool_tests {
    use super::*;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn request() -> ChatCompletionRequest {
        ChatCompletionRequest {
            model: "deepseek-v4-pro".to_string(),
            messages: vec![ChatMessage::plain("user".to_string(), "hello".to_string())],
            stream: false,
            ..Default::default()
        }
    }

    fn backend(server: &MockServer, tokens: &[&str]) -> ResponsesBackend {
        ResponsesBackend::new(
            "deepseek".to_string(),
            format!("{}/responses", server.uri()),
            tokens.iter().map(|token| (*token).to_string()).collect(),
        )
        .unwrap()
    }

    /// The bearer of every request the pool actually made, in order.
    async fn bearers(server: &MockServer) -> Vec<String> {
        server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .map(|request| {
                request.headers["authorization"]
                    .to_str()
                    .unwrap_or("<none>")
                    .to_string()
            })
            .collect()
    }

    fn upstream_message(error: GatewayError) -> String {
        match error {
            GatewayError::Upstream(message) => message,
            other => panic!("expected an upstream failure, got {other:?}"),
        }
    }

    /// A completed Responses object, as `response_to_completion` has to see it.
    fn responses_object(id: &str) -> Value {
        json!({
            "id": id,
            "output": [
                {
                    "type": "message",
                    "content": [{"type": "output_text", "text": "PONG"}]
                },
                {
                    "type": "function_call",
                    "call_id": "call_1",
                    "name": "get_weather",
                    "arguments": "{\"city\":\"Oslo\"}"
                },
            ],
            "usage": {"input_tokens": 5, "output_tokens": 2, "total_tokens": 7},
            "metadata": {"opaque": "keep"}
        })
    }

    /// A 401 on the first credential must rotate to the second one instead of
    /// failing the request, and the credential that answered must be the one
    /// whose result reaches the caller.
    #[tokio::test]
    async fn first_credential_rejected_rotates_to_the_second() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/responses"))
            .and(header("authorization", "Bearer bad"))
            .respond_with(ResponseTemplate::new(401).set_body_json(json!({
                "error": {"message": "invalid api key"}
            })))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/responses"))
            .and(header("authorization", "Bearer good"))
            .respond_with(ResponseTemplate::new(200).set_body_json(responses_object("resp-2")))
            .expect(1)
            .mount(&server)
            .await;

        let response = backend(&server, &["bad", "good"])
            .complete(request())
            .await
            .unwrap();

        assert_eq!(response.id, "resp-2");
        assert_eq!(response.choices[0].message.text(), "PONG");
        server.verify().await;
        // Rotation order, not just traffic: the rejected credential is tried
        // first and only the second one answers.
        assert_eq!(bearers(&server).await, vec!["Bearer bad", "Bearer good"]);
    }

    /// A 200 whose body is an `error` object is a failed credential too, so
    /// the walk must continue past it instead of handing it to the client.
    #[tokio::test]
    async fn error_object_in_a_200_body_rotates_to_the_next_credential() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/responses"))
            .and(header("authorization", "Bearer bad"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "error": {"message": "quota exceeded"}
            })))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/responses"))
            .and(header("authorization", "Bearer good"))
            .respond_with(ResponseTemplate::new(200).set_body_json(responses_object("resp-2")))
            .expect(1)
            .mount(&server)
            .await;

        let response = backend(&server, &["bad", "good"])
            .complete(request())
            .await
            .unwrap();

        assert_eq!(response.id, "resp-2");
        server.verify().await;
        assert_eq!(
            bearers(&server).await,
            vec!["Bearer bad", "Bearer good"],
            "an error body must rotate the credential, not be treated as success"
        );
    }

    /// Exhaustion: exactly one attempt per credential, and the LAST error —
    /// not the first — is what the caller is told about.
    #[tokio::test]
    async fn exhaustion_reports_the_last_error_after_one_attempt_per_credential() {
        let server = MockServer::start().await;
        for (token, message) in [
            ("a", "key a rejected"),
            ("b", "key b rejected"),
            ("c", "key c rejected"),
        ] {
            Mock::given(method("POST"))
                .and(path("/responses"))
                .and(header("authorization", format!("Bearer {token}")))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                    "error": {"message": message}
                })))
                .expect(1)
                .mount(&server)
                .await;
        }

        let error = backend(&server, &["a", "b", "c"])
            .complete(request())
            .await
            .unwrap_err();

        server.verify().await;
        assert_eq!(
            bearers(&server).await,
            vec!["Bearer a", "Bearer b", "Bearer c"],
            "one attempt per credential, no repeats"
        );
        assert_eq!(upstream_message(error), "key c rejected");
    }

    /// A keyless pool gets exactly one anonymous attempt and never grows a
    /// bearer header while making it.
    #[tokio::test]
    async fn keyless_pool_makes_a_single_attempt() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/responses"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;

        backend(&server, &[]).complete(request()).await.unwrap_err();

        assert_eq!(
            server.received_requests().await.unwrap().len(),
            1,
            "one anonymous attempt, no retries"
        );
    }

    /// Whatever credential won the walk, its payload is still mapped by
    /// `response_to_completion` on the way out.
    #[tokio::test]
    async fn the_winning_payload_is_mapped_by_response_to_completion() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/responses"))
            .respond_with(ResponseTemplate::new(200).set_body_json(responses_object("resp-9")))
            .mount(&server)
            .await;

        let response = backend(&server, &["only"])
            .complete(request())
            .await
            .unwrap();

        assert_eq!(response.id, "resp-9");
        assert_eq!(response.object, "chat.completion");
        assert_eq!(response.model, "deepseek-v4-pro");
        assert_eq!(response.choices[0].message.role, "assistant");
        assert_eq!(response.choices[0].message.text(), "PONG");
        // A `function_call` output item decides both the mapped tool call and
        // the finish reason, so a raw-payload pass-through could not fake it.
        assert_eq!(response.choices[0].finish_reason, "tool_calls");
        assert_eq!(
            response.choices[0].message.tool_calls.as_ref().unwrap(),
            &json!([{
                "id": "call_1",
                "type": "function",
                "function": {"name": "get_weather", "arguments": "{\"city\":\"Oslo\"}"},
            }])
        );
        assert_eq!(response.usage.prompt_tokens, 5);
        assert_eq!(response.usage.completion_tokens, 2);
        assert_eq!(response.usage.total_tokens, 7);
        assert_eq!(
            response.raw.as_ref().unwrap()["metadata"]["opaque"],
            json!("keep")
        );
    }
}

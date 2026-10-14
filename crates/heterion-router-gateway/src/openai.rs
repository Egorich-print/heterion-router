//! Generic OpenAI-compatible backend (no translation).
//!
//! Serves the registry's OpenAI-format providers: the request body goes
//! through verbatim and SSE frames map directly via the shared chunk helper.

use std::collections::HashSet;

use async_trait::async_trait;
use heterion_router_core::{
    ChatChoice, ChatCompletionRequest, ChatCompletionResponse, ChatMessage, GatewayError,
    StreamChunk, Usage,
};
use heterion_router_http::HttpClient;
use serde_json::Value;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;

use crate::backend::{ChatBackend, ChunkStream, next_token, openai_request_body};
use crate::ids;
use crate::responses::{MissingTerminal, ResponsesBackend, pump_sse_stream};

/// OpenAI-compatible executor over HTTP.
#[derive(Debug, Clone)]
pub struct OpenAiBackend {
    http: HttpClient,
    base_url: String,
    api_key: Option<String>,
}

impl OpenAiBackend {
    /// Build against `base_url` (e.g. `https://api.openai.com/v1`) with an
    /// optional Bearer `api_key`.
    pub fn new(
        base_url: String,
        api_key: Option<String>,
    ) -> Result<Self, heterion_router_http::client::HttpError> {
        Ok(Self {
            http: HttpClient::new(600)?,
            base_url: base_url.trim_end_matches('/').to_string(),
            api_key,
        })
    }

    fn chat_url(&self) -> String {
        format!("{}/chat/completions", self.base_url)
    }

    fn bearer(&self) -> Option<&str> {
        self.api_key.as_deref()
    }

    async fn drive_stream(
        &self,
        request: ChatCompletionRequest,
        tx: mpsc::Sender<Result<StreamChunk, GatewayError>>,
    ) -> Result<(), GatewayError> {
        drive_openai_sse(&self.http, &self.chat_url(), self.bearer(), request, tx).await
    }
}

/// Pump an OpenAI-compatible `/chat/completions` SSE stream into `tx`.
///
/// Canonical for both OpenAI-shaped executors here: the single-key
/// [`OpenAiBackend`] and the pooled [`ProviderOpenAiBackend`] differ only in
/// where the URL and the bearer come from, so the frame loop lives once.
async fn drive_openai_sse(
    http: &HttpClient,
    url: &str,
    bearer: Option<&str>,
    request: ChatCompletionRequest,
    tx: mpsc::Sender<Result<StreamChunk, GatewayError>>,
) -> Result<(), GatewayError> {
    if request.messages.is_empty() {
        return Err(GatewayError::InvalidRequest("messages is empty".into()));
    }
    let mut body = openai_request_body(&request);
    body["stream"] = Value::from(true);
    let response = http
        .post_json(url, bearer, &body)
        .await
        .map_err(|error| GatewayError::Upstream(error.to_string()))?;

    // An OpenAI chunk is already gateway-shaped: the only translation this
    // path owns is refusing an `error` frame, which the shared pump turns
    // into the stream's single failure.
    pump_sse_stream(
        response,
        &tx,
        |_event, data| {
            if let Some(error) = data.get("error") {
                return Err(GatewayError::Upstream(upstream_refusal(error)));
            }
            Ok(vec![data.clone()])
        },
        // OpenAI clients wait for a terminal chunk, so a `[DONE]` that
        // arrives without one still gets a synthesized `stop`.
        MissingTerminal::SynthesizeStop,
    )
    .await
}

/// An upstream refusal rendered for a [`GatewayError::Upstream`] message.
///
/// The provider's own message leads so logs stay readable, and the whole
/// `error` object follows as JSON: a provider that reports the refusal in a
/// body that arrived with HTTP 200 leaves no `upstream status {code}:` prefix
/// to parse, so the payload is the only place the real status can come from.
/// It never reaches a client — `sanitized_error_payload` replaces the message
/// of an upstream error wholesale.
pub(crate) fn upstream_refusal(error: &Value) -> String {
    match error.get("message").and_then(Value::as_str) {
        // A single-field object carries nothing the message does not already
        // say — the decoder's own overflow refusal looks exactly like this —
        // so echoing it back would only duplicate text.
        Some(message) if error.as_object().is_some_and(|object| object.len() == 1) => {
            message.to_string()
        }
        Some(message) => format!("{message}: {error}"),
        None => error.to_string(),
    }
}

/// Walk a credential pool once per credential, in rotation order, and return
/// the first raw payload that is not an error object.
///
/// Canonical for every pooled executor in this crate (OpenAI-shaped
/// [`ProviderOpenAiBackend`], Gemini, and later the Responses/Grok paths).
/// `attempt` performs one HTTP round trip and returns the decoded payload; a
/// transport error, an undecodable body or an `error` object all count as a
/// failed credential and the pool moves on to the next one.
///
/// Contract: exactly `attempts` tries (a keyless pool passes
/// `tokens.len().max(1)` for its single anonymous attempt), and a failed
/// attempt never falls through to the next credential carrying a token the
/// caller has already had rejected — the rotation index advances before the
/// retry, so each try uses a different credential. On exhaustion the last
/// observed error is returned, or `{provider}: all credentials failed` when
/// no attempt produced one.
pub(crate) async fn try_each_credential<F, Fut>(
    provider: &str,
    attempts: usize,
    mut attempt: F,
) -> Result<Value, GatewayError>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<Value, GatewayError>>,
{
    let mut last_error: Option<GatewayError> = None;
    for _ in 0..attempts {
        match attempt().await {
            Ok(payload) => {
                if let Some(error) = payload.get("error") {
                    last_error = Some(GatewayError::Upstream(upstream_refusal(error)));
                    continue;
                }
                return Ok(payload);
            }
            Err(error) => last_error = Some(error),
        }
    }
    Err(last_error
        .unwrap_or_else(|| GatewayError::Upstream(format!("{provider}: all credentials failed"))))
}

#[async_trait]
impl ChatBackend for OpenAiBackend {
    fn name(&self) -> &'static str {
        "openai-compatible"
    }

    async fn complete(
        &self,
        request: ChatCompletionRequest,
    ) -> Result<ChatCompletionResponse, GatewayError> {
        if request.messages.is_empty() {
            return Err(GatewayError::InvalidRequest("messages is empty".into()));
        }
        let mut body = openai_request_body(&request);
        body["stream"] = Value::from(false);
        let response = self
            .http
            .post_json(&self.chat_url(), self.bearer(), &body)
            .await
            .map_err(|error| GatewayError::Upstream(error.to_string()))?;
        let payload: Value = response
            .json()
            .await
            .map_err(|error| GatewayError::Upstream(error.to_string()))?;
        if let Some(error) = payload.get("error") {
            return Err(GatewayError::Upstream(upstream_refusal(error)));
        }
        Ok(openai_payload_to_completion(&request.model, &payload))
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

/// Convert an OpenAI completion object to the typed response.
fn openai_payload_to_completion(model: &str, payload: &Value) -> ChatCompletionResponse {
    let id = payload
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or("chatcmpl-unknown")
        .to_string();
    let choice = payload.get("choices").and_then(|choices| choices.get(0));
    let message = choice.and_then(|choice| choice.get("message"));
    // Preserve the upstream assistant turn verbatim: tool_calls, null
    // content and the real finish_reason must survive, or agents never see
    // their function calls.
    let content = message
        .and_then(|message| message.get("content"))
        .cloned()
        // No fallback to a re-read of `content`: this arm only runs when
        // `content` is absent, so any helper reading that same field returns
        // empty and the arm is dead. Proved by substituting a sentinel — no
        // observable behaviour changed.
        ;
    let mut tool_calls = message
        .and_then(|message| message.get("tool_calls"))
        .cloned();
    // Strict clients reject a non-string id, and some upstreams omit it.
    if let Some(calls) = tool_calls.as_mut() {
        crate::backend::normalize_tool_call_ids(calls);
    }
    let finish = choice
        .and_then(|choice| choice.get("finish_reason"))
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| {
            if tool_calls.is_some() {
                "tool_calls".to_string()
            } else {
                "stop".to_string()
            }
        });
    let usage = payload.get("usage");
    let prompt = usage
        .and_then(|usage| usage.get("prompt_tokens"))
        .and_then(Value::as_u64)
        .unwrap_or(0) as u32;
    let completion = usage
        .and_then(|usage| usage.get("completion_tokens"))
        .and_then(Value::as_u64)
        .unwrap_or(0) as u32;
    let total = usage
        .and_then(|usage| usage.get("total_tokens"))
        .and_then(Value::as_u64)
        .map(|total| total as u32)
        .unwrap_or_else(|| prompt + completion);

    ChatCompletionResponse {
        id,
        object: "chat.completion",
        created: ids::unix_seconds(),
        model: model.to_string(),
        choices: vec![ChatChoice {
            index: 0,
            message: ChatMessage {
                role: "assistant".to_string(),
                content,
                reasoning_content: message
                    .and_then(|message| message.get("reasoning_content"))
                    .and_then(Value::as_str)
                    .map(str::to_string),
                tool_calls,
                tool_call_id: None,
                name: None,
            },
            finish_reason: finish,
        }],
        usage: Usage {
            prompt_tokens: prompt,
            completion_tokens: completion,
            total_tokens: total,
        },
        // Handed over so the accounting layer can read the discounted counts
        // (`prompt_tokens_details` and friends) the totals do not carry.
        raw: Some(payload.clone()),
    }
}

/// The upstream assistant turn is preserved verbatim, including when
/// `content` is null or absent — the gateway must not invent text.
#[cfg(test)]
mod absent_content_tests {
    use super::*;
    use serde_json::json;

    fn content_of(assistant: Value) -> Option<Value> {
        let payload = json!({
            "choices": [{ "message": assistant, "finish_reason": "stop" }],
        });
        openai_payload_to_completion("m", &payload)
            .choices
            .into_iter()
            .next()
            .expect("one choice")
            .message
            .content
    }

    #[test]
    fn a_message_without_content_decodes_to_no_content() {
        assert_eq!(
            content_of(json!({ "role": "assistant" })),
            None,
            "an absent `content` must not be filled with a synthesised string"
        );
    }

    #[test]
    fn an_explicit_null_content_stays_null() {
        assert_eq!(
            content_of(json!({ "role": "assistant", "content": null })),
            Some(Value::Null)
        );
    }

    #[test]
    fn a_string_content_is_preserved_verbatim() {
        assert_eq!(
            content_of(json!({ "role": "assistant", "content": "kept" })),
            Some(json!("kept"))
        );
    }

    #[test]
    fn a_part_array_content_is_preserved_verbatim() {
        let parts = json!([{ "type": "text", "text": "a" }, { "type": "text", "text": "b" }]);
        assert_eq!(
            content_of(json!({ "role": "assistant", "content": parts })),
            Some(parts)
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::StreamExt;
    use heterion_router_core::{ChatCompletionRequest, ChatMessage};
    use serde_json::json;
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{method, path},
    };

    fn request() -> ChatCompletionRequest {
        ChatCompletionRequest {
            model: "test-model".to_string(),
            messages: vec![ChatMessage::plain("user".to_string(), "Hi".to_string())],
            stream: true,
            max_tokens: None,
            temperature: None,
            top_p: None,
            tools: None,
            tool_choice: None,
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn streams_openai_sse_to_finish() {
        let server = MockServer::start().await;
        let sse = concat!(
            "data: {\"id\":\"c1\",\"object\":\"chat.completion.chunk\",\"created\":1,\"model\":\"m\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"Hello\"},\"finish_reason\":null}]}\n\n",
            "data: {\"id\":\"c1\",\"object\":\"chat.completion.chunk\",\"created\":1,\"model\":\"m\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
            "data: [DONE]\n\n",
        );
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string(sse)
                    .insert_header("content-type", "text/event-stream"),
            )
            .expect(1)
            .mount(&server)
            .await;

        let backend = OpenAiBackend::new(server.uri(), Some("k".to_string())).unwrap();
        let chunks: Vec<StreamChunk> = backend
            .stream(request())
            .collect::<Vec<_>>()
            .await
            .into_iter()
            .map(Result::unwrap)
            .collect();

        let text: String = chunks
            .iter()
            .filter_map(|chunk| chunk.content.clone())
            .collect();
        assert_eq!(text, "Hello");
        assert_eq!(
            chunks.last().unwrap().finish_reason.as_deref(),
            Some("stop")
        );
        server.verify().await;
    }

    #[tokio::test]
    async fn complete_returns_typed_completion() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": "chatcmpl-1",
                "object": "chat.completion",
                "created": 1,
                "model": "m",
                "choices": [{
                    "index": 0,
                    "message": {"role": "assistant", "content": "Hi there"},
                    "finish_reason": "stop",
                }],
                "usage": {"prompt_tokens": 2, "completion_tokens": 3, "total_tokens": 5},
            })))
            .expect(1)
            .mount(&server)
            .await;

        let backend = OpenAiBackend::new(server.uri(), None).unwrap();
        let mut req = request();
        req.stream = false;
        let response = backend.complete(req).await.unwrap();

        assert_eq!(response.choices[0].message.text(), "Hi there");
        assert_eq!(response.usage.total_tokens, 5);
        server.verify().await;
    }

    #[tokio::test]
    async fn upstream_error_surfaces() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(401).set_body_json(json!({
                "error": {"message": "bad key", "code": "invalid_api_key"}
            })))
            .mount(&server)
            .await;

        let backend = OpenAiBackend::new(server.uri(), None).unwrap();
        let mut req = request();
        req.stream = false;
        let error = backend.complete(req).await.unwrap_err();
        assert!(error.to_string().contains("bad key"));
    }

    // ---- streaming-pump characterization -------------------------------
    //
    // These pin the exact `StreamChunk` sequence the OpenAI-shaped stream
    // emits, item for item, so a refactor of the SSE pump cannot quietly
    // reorder, drop, add or reword anything the client sees.

    /// Drive the shared pump through [`OpenAiBackend`] over `sse` and return
    /// every item it put on the channel — emitted chunks *and* a trailing
    /// failure, which the executor forwards in band.
    async fn stream_items(sse: String) -> Vec<Result<StreamChunk, GatewayError>> {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string(sse)
                    .insert_header("content-type", "text/event-stream"),
            )
            .mount(&server)
            .await;
        let backend = OpenAiBackend::new(server.uri(), Some("k".to_string())).unwrap();
        backend.stream(request()).collect::<Vec<_>>().await
    }

    /// The last item's message, or a panic naming the sequence.
    fn failure_message(items: &[Result<StreamChunk, GatewayError>]) -> String {
        match items.last() {
            Some(Err(GatewayError::Upstream(message))) => message.clone(),
            other => panic!("expected a trailing upstream failure, got {other:?}"),
        }
    }

    /// Assert `items` is exactly `chunks`, so a pin cannot pass on a prefix.
    fn assert_items(items: &[Result<StreamChunk, GatewayError>], chunks: Vec<StreamChunk>) {
        assert_eq!(items.len(), chunks.len(), "{items:?}");
        for (index, (item, expected)) in items.iter().zip(&chunks).enumerate() {
            assert_eq!(
                item.as_ref()
                    .unwrap_or_else(|error| panic!("item {index} failed: {error:?}")),
                expected,
                "item {index}"
            );
        }
    }

    /// The fail-closed message a stream that never reached its terminal
    /// event gets, spelled out rather than imported: it is the string the
    /// shared pump sends and `lib.rs` sanitizes away on the wire.
    const UNTERMINATED: &str = "upstream stream ended before terminal event";

    /// One chunk per frame, in order: the content delta, the tool-call delta
    /// with the usage the same frame reported, then the upstream's own
    /// terminal chunk (never a synthesized one).
    #[tokio::test]
    async fn stream_passes_every_frame_through_in_order() {
        let items = stream_items(
            concat!(
                ": keepalive\n\n",
                "data: {\"id\":\"c1\",\"object\":\"chat.completion.chunk\",\"created\":1,\"model\":\"m\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"Hel\"},\"finish_reason\":null}]}\n\n",
                "data: {\"id\":\"c1\",\"object\":\"chat.completion.chunk\",\"created\":1,\"model\":\"m\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"lo\"},\"finish_reason\":null}]}\n\n",
                "data: {\"id\":\"c1\",\"object\":\"chat.completion.chunk\",\"created\":1,\"model\":\"m\",\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\"type\":\"function\",\"function\":{\"name\":\"get_time\",\"arguments\":\"{}\"}}]},\"finish_reason\":null}],\"usage\":{\"prompt_tokens\":1,\"completion_tokens\":2,\"total_tokens\":3}}\n\n",
                "data: {\"id\":\"c1\",\"object\":\"chat.completion.chunk\",\"created\":1,\"model\":\"m\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n\n",
                "data: [DONE]\n\n",
            )
            .to_string(),
        )
        .await;

        assert_items(
            &items,
            vec![
                StreamChunk::content("Hel"),
                StreamChunk::content("lo"),
                StreamChunk {
                    tool_calls: Some(json!([{
                        "index": 0,
                        "id": "call_1",
                        "type": "function",
                        "function": {"name": "get_time", "arguments": "{}"},
                    }])),
                    usage: Some(Usage {
                        prompt_tokens: 1,
                        completion_tokens: 2,
                        total_tokens: 3,
                    }),
                    ..StreamChunk::default()
                },
                StreamChunk::done("tool_calls"),
            ],
        );
    }

    /// An upstream error frame fails the stream after the chunks already
    /// delivered; `lib.rs` replaces the message on the wire (see its
    /// `stream_errors_never_leak_upstream_text`).
    #[tokio::test]
    async fn stream_fails_the_stream_on_an_upstream_error_frame() {
        let items = stream_items(
            concat!(
                "data: {\"id\":\"c1\",\"object\":\"chat.completion.chunk\",\"created\":1,\"model\":\"m\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"partial\"},\"finish_reason\":null}]}\n\n",
                "data: {\"error\":{\"message\":\"model overloaded\",\"code\":429}}\n\n",
            )
            .to_string(),
        )
        .await;

        assert_eq!(items.len(), 2, "one chunk, then the failure: {items:?}");
        assert_items(&items[..1], vec![StreamChunk::content("partial")]);
        // The provider's own text still leads, and the payload behind it is
        // what lets the router recover the 429 the 200 body swallowed.
        let message = failure_message(&items);
        assert!(
            message.starts_with("model overloaded"),
            "message must lead with the provider text: {message}"
        );
        assert_eq!(
            crate::upstream_status_or_recovered(&message),
            Some(429),
            "the refusal must keep the status recoverable: {message}"
        );
    }

    /// A body that stops without `[DONE]` never promised a terminal chunk,
    /// so the stream fails closed rather than inventing one.
    #[tokio::test]
    async fn stream_without_a_done_sentinel_fails_closed() {
        let items = stream_items(
            "data: {\"id\":\"c1\",\"object\":\"chat.completion.chunk\",\"created\":1,\"model\":\"m\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"partial\"},\"finish_reason\":null}]}\n\n"
                .to_string(),
        )
        .await;

        assert_eq!(items.len(), 2, "{items:?}");
        assert_items(&items[..1], vec![StreamChunk::content("partial")]);
        assert_eq!(failure_message(&items), UNTERMINATED);
    }

    /// A bare `[DONE]` with no terminal chunk behind it still owes the
    /// client's `stop`, so the pump synthesizes exactly one.
    #[tokio::test]
    async fn stream_synthesizes_the_terminal_chunk_a_bare_done_owes() {
        let items = stream_items(
            concat!(
                "data: {\"id\":\"c1\",\"object\":\"chat.completion.chunk\",\"created\":1,\"model\":\"m\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"partial\"},\"finish_reason\":null}]}\n\n",
                "data: [DONE]\n\n",
            )
            .to_string(),
        )
        .await;

        assert_items(
            &items,
            vec![StreamChunk::content("partial"), StreamChunk::done("stop")],
        );
    }

    /// An empty body emitted no terminal chunk and no `[DONE]`: one
    /// fail-closed failure, nothing else.
    #[tokio::test]
    async fn empty_stream_emits_nothing_and_fails_closed() {
        let items = stream_items(String::new()).await;

        assert_eq!(items.len(), 1, "{items:?}");
        assert_eq!(failure_message(&items), UNTERMINATED);
    }

    /// The decoder's own reason is the whole message: an un-terminated event
    /// must never echo the megabyte of upstream text behind it.
    #[tokio::test]
    async fn an_overflowed_event_fails_closed_without_echoing_upstream_text() {
        let canary = "CANARY-openai-overflow-marker";
        let items = stream_items(format!(
            "data: {canary}{}",
            "x".repeat(heterion_router_http::sse::MAX_SSE_EVENT_BYTES + 4096)
        ))
        .await;

        assert_eq!(items.len(), 1, "{items:?}");
        let message = failure_message(&items);
        assert_eq!(message, "upstream sse event exceeded the decoder limit");
        assert!(!message.contains(canary), "{message}");
    }
}

/// Resolve a full chat-completions URL from a registry `baseUrl`.
///
/// Registry entries vary: some declare the endpoint (`.../chat/completions`),
/// some only the API root. Append the path only when it is missing.
pub fn resolve_chat_url(base_url: &str) -> String {
    let trimmed = base_url.trim_end_matches('/');
    if trimmed.ends_with("/chat/completions") {
        trimmed.to_string()
    } else {
        format!("{trimmed}/chat/completions")
    }
}

pub fn resolve_responses_url(base_url: &str) -> String {
    let trimmed = base_url.trim_end_matches('/');
    if let Some(root) = trimmed.strip_suffix("/chat/completions") {
        format!("{root}/responses")
    } else if trimmed.ends_with("/responses") {
        trimmed.to_string()
    } else {
        format!("{trimmed}/responses")
    }
}

/// Per-provider OpenAI-compatible backend.
///
/// Generalises [`OpenAiBackend`] over the registry: the URL comes from the
/// provider entry, credentials from `provider_connections`, and a pool is
/// rotated so one exhausted key does not fail the request.
#[derive(Debug, Clone)]
pub struct ProviderOpenAiBackend {
    http: HttpClient,
    provider: String,
    chat_url: String,
    tokens: Vec<String>,
    next: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    responses: Option<ResponsesBackend>,
    responses_models: HashSet<String>,
}

impl ProviderOpenAiBackend {
    /// Build for `provider` against `chat_url` with a credential pool.
    ///
    /// An empty pool is allowed: `auth_type: optional` providers serve
    /// anonymous traffic, and the request then goes out without a bearer.
    pub fn new(
        provider: String,
        chat_url: String,
        tokens: Vec<String>,
    ) -> Result<Self, heterion_router_http::client::HttpError> {
        Ok(Self {
            http: HttpClient::new(600)?,
            provider,
            chat_url,
            tokens,
            next: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            responses: None,
            responses_models: HashSet::new(),
        })
    }

    pub fn with_responses_backend(
        mut self,
        endpoint_url: String,
        tokens: Vec<String>,
        responses_models: HashSet<String>,
    ) -> Result<Self, heterion_router_http::client::HttpError> {
        self.responses = Some(ResponsesBackend::new(
            self.provider.clone(),
            endpoint_url,
            tokens,
        )?);
        self.responses_models = responses_models;
        Ok(self)
    }

    /// Pool size.
    pub fn token_count(&self) -> usize {
        self.tokens.len()
    }

    async fn drive_stream(
        &self,
        request: ChatCompletionRequest,
        tx: mpsc::Sender<Result<StreamChunk, GatewayError>>,
    ) -> Result<(), GatewayError> {
        drive_openai_sse(
            &self.http,
            &self.chat_url,
            next_token(&self.tokens, &self.next),
            request,
            tx,
        )
        .await
    }
}

#[async_trait]
impl ChatBackend for ProviderOpenAiBackend {
    fn name(&self) -> &'static str {
        // Static str required by the trait; the pool/URL carry the provider.
        "provider"
    }

    async fn complete(
        &self,
        request: ChatCompletionRequest,
    ) -> Result<ChatCompletionResponse, GatewayError> {
        if request.messages.is_empty() {
            return Err(GatewayError::InvalidRequest("messages is empty".into()));
        }
        if self.responses_models.contains(&request.model)
            && let Some(responses) = &self.responses
        {
            return responses.complete(request).await;
        }
        let mut body = openai_request_body(&request);
        body["stream"] = Value::from(false);

        // A keyless provider gets exactly one attempt, a pool one per key.
        let payload = try_each_credential(&self.provider, self.tokens.len().max(1), || async {
            let response = self
                .http
                .post_json(&self.chat_url, next_token(&self.tokens, &self.next), &body)
                .await
                .map_err(|error| GatewayError::Upstream(error.to_string()))?;
            response
                .json()
                .await
                .map_err(|error| GatewayError::Upstream(error.to_string()))
        })
        .await?;
        Ok(openai_payload_to_completion(&request.model, &payload))
    }

    fn stream(&self, request: ChatCompletionRequest) -> ChunkStream {
        if self.responses_models.contains(&request.model)
            && let Some(responses) = &self.responses
        {
            return responses.stream(request);
        }
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

#[cfg(test)]
mod provider_tests {
    use super::*;
    use heterion_router_core::{ChatCompletionRequest, ChatMessage};
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{method, path},
    };

    #[test]
    fn chat_url_resolution_handles_both_shapes() {
        assert_eq!(
            resolve_chat_url("https://openrouter.ai/api/v1"),
            "https://openrouter.ai/api/v1/chat/completions"
        );
        assert_eq!(
            resolve_chat_url("https://openrouter.ai/api/v1/chat/completions"),
            "https://openrouter.ai/api/v1/chat/completions"
        );
        assert_eq!(
            resolve_responses_url("https://api.x.ai/v1/chat/completions"),
            "https://api.x.ai/v1/responses"
        );
        assert_eq!(
            resolve_responses_url("https://api.x.ai/v1"),
            "https://api.x.ai/v1/responses"
        );
    }

    #[tokio::test]
    async fn provider_backend_serves_completion() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": "gen-1",
                "choices": [{
                    "index": 0,
                    "message": {"role": "assistant", "content": "hi"},
                    "finish_reason": "stop",
                }],
                "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2},
            })))
            .expect(1)
            .mount(&server)
            .await;

        let backend = ProviderOpenAiBackend::new(
            "openrouter".to_string(),
            resolve_chat_url(&format!("{}/v1", server.uri())),
            vec!["k".to_string()],
        )
        .unwrap();

        let response = backend
            .complete(ChatCompletionRequest {
                model: "some/model:free".to_string(),
                messages: vec![ChatMessage::plain("user".to_string(), "hi".to_string())],
                stream: false,
                max_tokens: None,
                temperature: None,
                top_p: None,
                tools: None,
                tool_choice: None,
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(response.choices[0].message.text(), "hi");
        server.verify().await;
    }

    #[tokio::test]
    async fn xai_responses_model_uses_responses_endpoint() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/responses"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": "resp-1",
                "output": [{
                    "type": "message",
                    "content": [{"type": "output_text", "text": "from responses"}]
                }]
            })))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(ResponseTemplate::new(500))
            .expect(0)
            .mount(&server)
            .await;

        let backend = ProviderOpenAiBackend::new(
            "xai".to_string(),
            resolve_chat_url(&format!("{}/v1/chat/completions", server.uri())),
            vec!["k".to_string()],
        )
        .unwrap()
        .with_responses_backend(
            resolve_responses_url(&format!("{}/v1/chat/completions", server.uri())),
            vec!["k".to_string()],
            HashSet::from(["grok-4.6".to_string()]),
        )
        .unwrap();

        let response = backend
            .complete(ChatCompletionRequest {
                model: "grok-4.6".to_string(),
                messages: vec![ChatMessage::plain("user".to_string(), "hi".to_string())],
                stream: false,
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(response.choices[0].message.text(), "from responses");
        server.verify().await;

        // Routing: the `.expect(0)` on /v1/chat/completions already fails if
        // the wrong endpoint is taken, but name the winner explicitly too.
        let sent = server.received_requests().await.unwrap();
        assert_eq!(sent.len(), 1, "exactly one upstream call, no fallback");
        assert_eq!(
            sent[0].url.path(),
            "/v1/responses",
            "grok-4.6 must route to the Responses endpoint"
        );
    }

    #[tokio::test]
    async fn keyless_provider_sends_no_bearer() {
        use wiremock::matchers::header_exists;

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
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": "1",
                "model": "gemini",
                "choices": [{
                    "index": 0,
                    "message": {"role": "assistant", "content": "hi"},
                    "finish_reason": "stop",
                }],
            })))
            .expect(1)
            .mount(&server)
            .await;

        let backend = ProviderOpenAiBackend::new(
            "pollinations".to_string(),
            resolve_chat_url(&format!("{}/v1", server.uri())),
            Vec::new(),
        )
        .unwrap();
        assert_eq!(backend.token_count(), 0);

        let response = backend
            .complete(ChatCompletionRequest {
                model: "gemini".to_string(),
                messages: vec![ChatMessage::plain("user".to_string(), "hi".to_string())],
                stream: false,
                max_tokens: None,
                temperature: None,
                top_p: None,
                tools: None,
                tool_choice: None,
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(response.choices[0].message.text(), "hi");
        server.verify().await;

        // Content, not just traffic: the whole point is that a keyless pool
        // puts nothing on the wire. A count-only check would pass even if the
        // bearer had been added back.
        let sent = server.received_requests().await.unwrap();
        assert_eq!(sent.len(), 1, "one anonymous attempt, no retries");
        assert!(
            sent[0].headers.get("authorization").is_none(),
            "a keyless provider must not grow an Authorization header, got {:?}",
            sent[0].headers.get("authorization")
        );
    }
}

/// Characterization of the canonical credential-pool retry loop
/// (`try_each_credential`), through the OpenAI-shaped executor that owns it.
#[cfg(test)]
mod credential_pool_tests {
    use super::*;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn request() -> ChatCompletionRequest {
        ChatCompletionRequest {
            model: "m".to_string(),
            messages: vec![ChatMessage::plain("user".to_string(), "hi".to_string())],
            stream: false,
            ..Default::default()
        }
    }

    fn backend(server: &MockServer, tokens: &[&str]) -> ProviderOpenAiBackend {
        ProviderOpenAiBackend::new(
            "pool-test".to_string(),
            resolve_chat_url(&format!("{}/v1", server.uri())),
            tokens.iter().map(|token| (*token).to_string()).collect(),
        )
        .unwrap()
    }

    /// A 401 on the first credential must rotate to the next one rather than
    /// fail the request, and the successful credential must be the one that
    /// actually answered.
    #[tokio::test]
    async fn first_credential_rejected_rotates_to_the_second() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .and(header("authorization", "Bearer bad"))
            .respond_with(ResponseTemplate::new(401).set_body_json(serde_json::json!({
                "error": {"message": "invalid api key"}
            })))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .and(header("authorization", "Bearer good"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": "chatcmpl-1",
                "choices": [{
                    "index": 0,
                    "message": {"role": "assistant", "content": "second wins"},
                    "finish_reason": "stop",
                }],
            })))
            .expect(1)
            .mount(&server)
            .await;

        let response = backend(&server, &["bad", "good"])
            .complete(request())
            .await
            .unwrap();
        assert_eq!(response.choices[0].message.text(), "second wins");
        assert_eq!(server.received_requests().await.unwrap().len(), 2);
        server.verify().await;

        // Rotation order, not just traffic: the rejected credential is tried
        // first and the second one is what answers.
        let seen = server.received_requests().await.unwrap();
        let bearers: Vec<_> = seen
            .iter()
            .map(|request| {
                request.headers["authorization"]
                    .to_str()
                    .unwrap()
                    .to_string()
            })
            .collect();
        assert_eq!(
            bearers,
            vec!["Bearer bad", "Bearer good"],
            "credentials must be tried in pool order"
        );
    }

    /// A 200 whose body is an error object counts as a failed credential, so
    /// the pool still moves on.
    #[tokio::test]
    async fn error_payload_body_rotates_to_the_next_credential() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .and(header("authorization", "Bearer bad"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "error": {"message": "quota exceeded"}
            })))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .and(header("authorization", "Bearer good"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": "chatcmpl-1",
                "choices": [{
                    "index": 0,
                    "message": {"role": "assistant", "content": "recovered"},
                    "finish_reason": "stop",
                }],
            })))
            .expect(1)
            .mount(&server)
            .await;

        let response = backend(&server, &["bad", "good"])
            .complete(request())
            .await
            .unwrap();
        assert_eq!(response.choices[0].message.text(), "recovered");
        server.verify().await;

        // A 200 carrying an error body must still cost one attempt on the
        // first credential and hand over to the second.
        let seen = server.received_requests().await.unwrap();
        let bearers: Vec<_> = seen
            .iter()
            .map(|request| {
                request.headers["authorization"]
                    .to_str()
                    .unwrap()
                    .to_string()
            })
            .collect();
        assert_eq!(
            bearers,
            vec!["Bearer bad", "Bearer good"],
            "an error body must rotate the credential, not be treated as success"
        );
    }

    /// Exhaustion: exactly one attempt per credential, and the last upstream
    /// error is what the caller sees.
    #[tokio::test]
    async fn exhaustion_tries_every_credential_once_and_reports_the_last_error() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(ResponseTemplate::new(401).set_body_json(serde_json::json!({
                "error": {"message": "key n rejected"}
            })))
            .mount(&server)
            .await;

        let error = backend(&server, &["a", "b", "c"])
            .complete(request())
            .await
            .unwrap_err();
        let seen = server.received_requests().await.unwrap();
        assert_eq!(seen.len(), 3, "one attempt per credential, no repeats");
        assert!(error.to_string().contains("key n rejected"));
    }

    /// A keyless pool gets exactly one anonymous attempt and never grows a
    /// bearer header while doing it.
    #[tokio::test]
    async fn keyless_pool_makes_a_single_attempt() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;

        backend(&server, &[]).complete(request()).await.unwrap_err();
        assert_eq!(server.received_requests().await.unwrap().len(), 1);
    }
}

#[cfg(test)]
mod tool_passthrough_tests {
    use super::*;
    use heterion_router_core::{ChatCompletionRequest, ChatMessage};
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{body_string_contains, method, path},
    };

    #[test]
    fn completion_preserves_tool_calls_and_finish_reason() {
        let payload = serde_json::json!({
            "id": "c1",
            "choices": [{
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [{
                        "id": "call_1",
                        "type": "function",
                        "function": {"name": "get_time", "arguments": "{\"city\":\"SF\"}"}
                    }]
                },
                "finish_reason": "tool_calls"
            }],
            "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
        });
        let response = openai_payload_to_completion("m", &payload);
        assert_eq!(response.choices[0].finish_reason, "tool_calls");
        let message = &response.choices[0].message;
        assert!(message.has_tool_calls());
        assert_eq!(message.text(), "");
    }

    #[tokio::test]
    async fn tool_definitions_reach_the_upstream() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .and(body_string_contains("\"get_time\""))
            .and(body_string_contains("\"tool_calls\""))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": "c1",
                "choices": [{
                    "index": 0,
                    "message": {"role": "assistant", "content": "ok"},
                    "finish_reason": "stop",
                }],
            })))
            .expect(1)
            .mount(&server)
            .await;

        let backend = ProviderOpenAiBackend::new(
            "test".to_string(),
            resolve_chat_url(&format!("{}/v1", server.uri())),
            vec!["k".to_string()],
        )
        .unwrap();

        let request = ChatCompletionRequest {
            model: "m".to_string(),
            messages: vec![
                ChatMessage::plain("user", "time?"),
                ChatMessage {
                    role: "assistant".to_string(),
                    content: None,
                    reasoning_content: None,
                    tool_calls: Some(serde_json::json!([{
                        "id": "call_1",
                        "type": "function",
                        "function": {"name": "get_time", "arguments": "{}"}
                    }])),
                    tool_call_id: None,
                    name: None,
                },
                ChatMessage {
                    role: "tool".to_string(),
                    content: Some(serde_json::Value::from("12:00")),
                    reasoning_content: None,
                    tool_calls: None,
                    tool_call_id: Some("call_1".to_string()),
                    name: None,
                },
            ],
            stream: false,
            max_tokens: None,
            temperature: None,
            top_p: None,
            tools: Some(serde_json::json!([{
                "type": "function",
                "function": {"name": "get_time", "parameters": {"type": "object"}}
            }])),
            tool_choice: Some(serde_json::json!("auto")),
            ..Default::default()
        };

        let response = backend.complete(request).await.unwrap();
        assert_eq!(response.choices[0].message.text(), "ok");
        server.verify().await;

        // The mock's body matcher is what makes this test meaningful, but a
        // reader should not have to know that to see the claim: assert the
        // tool payload really is on the wire.
        let sent = server.received_requests().await.unwrap();
        assert_eq!(sent.len(), 1);
        let body = String::from_utf8(sent[0].body.clone()).unwrap();
        assert!(
            body.contains("get_time"),
            "tool definitions did not reach the upstream: {body}"
        );
        assert!(
            body.contains("\"tool_calls\""),
            "assistant tool_calls did not reach the upstream: {body}"
        );
    }
}

/// Cache and reasoning accounting on the streaming path.
///
/// A stream publishes its discounted token counts on the usage frame of a
/// terminal chunk and nowhere else, so the end-to-end proof here drives a
/// real upstream over a real SSE body and reads the row the gateway booked.
#[cfg(test)]
mod stream_accounting_tests {
    use super::*;
    use crate::{AppState, build_router_with_state};
    use axum::Router;
    use axum::body::{Body, to_bytes};
    use axum::http::{Request, StatusCode};
    use futures::StreamExt;
    use heterion_router_db::Db;
    use heterion_router_usage::{CachedUsage, PriceTable, UsageRecord, record_usage};
    use serde_json::json;
    use std::sync::Arc;
    use tower::ServiceExt;
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{method, path},
    };

    /// A two-frame stream whose terminal frame optionally carries `usage`.
    fn sse(usage: Option<&str>) -> String {
        let usage = usage
            .map(|usage| format!(",\"usage\":{{{usage}}}"))
            .unwrap_or_default();
        concat!(
            "data: {\"id\":\"c1\",\"object\":\"chat.completion.chunk\",\"created\":1,\"model\":\"m\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"Hello\"},\"finish_reason\":null}]}\n\n",
            "data: {\"id\":\"c1\",\"object\":\"chat.completion.chunk\",\"created\":1,\"model\":\"m\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]__USAGE__}\n\n",
            "data: [DONE]\n\n",
        )
        .replace("__USAGE__", &usage)
    }

    fn usage_with_cache() -> &'static str {
        r#""prompt_tokens":1000,"completion_tokens":500,"total_tokens":1500,"prompt_tokens_details":{"cached_tokens":200},"completion_tokens_details":{"reasoning_tokens":64}"#
    }

    fn usage_without_detail() -> &'static str {
        r#""prompt_tokens":1000,"completion_tokens":500,"total_tokens":1500"#
    }

    async fn upstream(sse: String) -> MockServer {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string(sse)
                    .insert_header("content-type", "text/event-stream"),
            )
            .mount(&server)
            .await;
        server
    }

    /// POST one streamed completion through the real HTTP surface and return
    /// the database the gateway booked into.
    async fn stream_through_gateway(sse: &str) -> Arc<Db> {
        let server = upstream(sse.to_string()).await;
        let db = Arc::new(Db::open_in_memory().unwrap());
        db.migrate().unwrap();
        let backend = OpenAiBackend::new(server.uri(), Some("k".to_string())).unwrap();
        let router: Router =
            build_router_with_state(AppState::with_db(Arc::new(backend), db.clone()));
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
                            "stream": true,
                            "stream_options": {"include_usage": true},
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        // Drain the SSE body: the row is booked while the stream is consumed.
        let text =
            String::from_utf8_lossy(&to_bytes(response.into_body(), usize::MAX).await.unwrap())
                .to_string();
        assert!(text.contains("Hello"), "{text}");
        db
    }

    /// `(input, output, cache_read, cache_creation, reasoning)` of the single
    /// booked row.
    fn booked(db: &Db) -> (i64, i64, i64, i64, i64) {
        let guard = db.connection();
        guard
            .query_row(
                "SELECT tokens_input, tokens_output, tokens_cache_read,
                        tokens_cache_creation, tokens_reasoning
                 FROM usage_history",
                [],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                },
            )
            .expect("one usage row")
    }

    /// The gap: the upstream said 200 cached and 64 reasoning tokens on its
    /// usage frame, and the streamed row lost both.
    #[tokio::test]
    async fn a_streamed_usage_row_carries_the_cached_and_reasoning_tokens() {
        let db = stream_through_gateway(&sse(Some(usage_with_cache()))).await;
        assert_eq!(
            booked(&db),
            (1000, 500, 200, 0, 64),
            "the usage frame reported 200 cached / 64 reasoning tokens"
        );
    }

    /// The bill that follows from the row: cached input at its own rate
    /// (800*2.0 + 200*0.5 + 500*6.0 = 4700), never the full-price 5000.
    #[tokio::test]
    async fn a_streamed_request_costs_the_discounted_price_not_the_full_one() {
        let db = stream_through_gateway(&sse(Some(usage_with_cache()))).await;
        let (input, output, cache_read, cache_creation, reasoning) = booked(&db);
        let prices = PriceTable::with_defaults();
        let cost = prices
            .cost_usd(
                "grok-4.6",
                input as u32,
                output as u32,
                cache_read as u32,
                reasoning as u32,
                cache_creation as u32,
            )
            .unwrap();
        assert_eq!(cost, 0.0047, "200 cached tokens billed at 0.5/1M");
        assert_ne!(
            cost, 0.005,
            "the full-price bill is what a dropped detail produces"
        );
    }

    /// Regression guard: a stream whose upstream never sent a usage frame
    /// books exactly the zeros it booked before, detail included.
    #[tokio::test]
    async fn a_stream_without_a_usage_frame_books_exactly_what_it_booked_before() {
        let db = stream_through_gateway(&sse(None)).await;
        assert_eq!(
            booked(&db),
            (0, 0, 0, 0, 0),
            "no usage frame means no tokens to attribute"
        );
    }

    /// Regression guard, second spelling: a usage frame with no detail block
    /// books its totals and no invented discount.
    #[tokio::test]
    async fn a_plain_usage_frame_books_its_totals_and_nothing_else() {
        let db = stream_through_gateway(&sse(Some(usage_without_detail()))).await;
        assert_eq!(
            booked(&db),
            (1000, 500, 0, 0, 0),
            "a provider that never caches is billed as before"
        );
    }

    /// The accounting boundary itself: the counts a stream carries are the
    /// ones the row writer and the price table consume, so the row above
    /// cannot be produced by anything but these values.
    #[tokio::test]
    async fn a_streamed_usage_frame_books_its_detail_at_the_accounting_boundary() {
        let server = upstream(sse(Some(usage_with_cache()))).await;
        let backend = OpenAiBackend::new(server.uri(), Some("k".to_string())).unwrap();
        let chunks: Vec<StreamChunk> = backend
            .stream(ChatCompletionRequest {
                model: "grok-4.6".to_string(),
                messages: vec![ChatMessage::plain("user", "hi")],
                stream: true,
                ..Default::default()
            })
            .map(|item| item.expect("chunk"))
            .collect()
            .await;

        let usage = chunks
            .iter()
            .filter_map(|chunk| chunk.usage)
            .next_back()
            .expect("a usage frame");
        let details = chunks
            .iter()
            .filter_map(crate::backend::chunk_cached_usage)
            .next_back()
            .expect("a discounted detail");
        assert_eq!(
            details,
            CachedUsage {
                cache_read: 200,
                cache_creation: 0,
                reasoning: 64,
            }
        );

        let db = Arc::new(Db::open_in_memory().unwrap());
        db.migrate().unwrap();
        {
            let guard = db.connection();
            record_usage(
                &guard,
                &UsageRecord {
                    provider: Some("openai-compatible".to_string()),
                    model: Some("grok-4.6".to_string()),
                    tokens_input: usage.prompt_tokens,
                    tokens_output: usage.completion_tokens,
                    tokens_cache_read: details.cache_read,
                    tokens_cache_creation: details.cache_creation,
                    tokens_reasoning: details.reasoning,
                    success: true,
                    ..Default::default()
                },
            )
            .unwrap();
        }
        assert_eq!(booked(&db), (1000, 500, 200, 0, 64));
    }

    /// The non-streaming side keeps its own spelling: the whole upstream
    /// payload rides `raw`, and `lib.rs` reads the detail off it exactly as
    /// before.
    #[tokio::test]
    async fn complete_hands_the_upstream_payload_to_the_accounting_layer() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": "chatcmpl-1",
                "object": "chat.completion",
                "created": 1,
                "model": "m",
                "choices": [{
                    "index": 0,
                    "message": {"role": "assistant", "content": "Hi there"},
                    "finish_reason": "stop",
                }],
                "usage": {
                    "prompt_tokens": 1000,
                    "completion_tokens": 500,
                    "total_tokens": 1500,
                    "prompt_tokens_details": {"cached_tokens": 200},
                    "completion_tokens_details": {"reasoning_tokens": 64},
                },
            })))
            .expect(1)
            .mount(&server)
            .await;

        let backend = OpenAiBackend::new(server.uri(), None).unwrap();
        let response = backend
            .complete(ChatCompletionRequest {
                model: "grok-4.6".to_string(),
                messages: vec![ChatMessage::plain("user", "Hi")],
                ..Default::default()
            })
            .await
            .unwrap();

        let cached = CachedUsage::from_payload(response.raw.as_ref().expect("upstream payload"));
        assert_eq!(
            cached,
            CachedUsage {
                cache_read: 200,
                cache_creation: 0,
                reasoning: 64,
            },
            "a non-streamed completion books the same counts"
        );
        // The wire shape is untouched: `raw` is `#[serde(skip)]`.
        let wire = serde_json::to_value(&response).unwrap();
        assert_eq!(wire["usage"]["prompt_tokens"], 1000);
        assert_eq!(wire.get("raw"), None);
        server.verify().await;

        // Attribution: the counts above were booked against the request that
        // actually went upstream, so pin the model that was forwarded.
        let sent = server.received_requests().await.unwrap();
        assert_eq!(sent.len(), 1);
        let forwarded: serde_json::Value = sent[0].body_json().unwrap();
        assert_eq!(
            forwarded["model"], "grok-4.6",
            "the upstream call must carry the requested model"
        );
    }
}

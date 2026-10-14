//! Upstream Claude-wire (`claude` format) executor.
//!
//! Serves providers that expose Anthropic's `/v1/messages` dialect —
//! `zai`, `agentrouter`, `anthropic`, `kimi-coding`, `wafer`, … — none of
//! which an OpenAI-shaped backend can call. Requests are translated with
//! `openai_to_claude`, answers come back through the shared claude→openai
//! stream translator, and non-streaming messages get a dedicated response
//! converter. Errors keep the `upstream status {code}: {body}` spelling so
//! the status-aware wrapper can recover the real status.

use async_trait::async_trait;
use heterion_router_core::{
    ChatChoice, ChatCompletionRequest, ChatCompletionResponse, ChatMessage, GatewayError,
    StreamChunk, Usage,
};
use heterion_router_http::HttpClient;
use heterion_router_translate::claude::{ClaudeState, Converted, convert_event};
use heterion_router_translate::claude_request::openai_to_claude;
use serde_json::{Value, json};
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;

use crate::backend::{ChatBackend, ChunkStream, next_token, openai_request_body};
use crate::ids;
use crate::openai::try_each_credential;

/// The `anthropic-version` header every `/v1/messages` endpoint expects.
const ANTHROPIC_VERSION: &str = "2023-06-01";

/// Map a Claude `stop_reason` to an OpenAI finish reason.
fn claude_stop_reason(stop_reason: Option<&str>) -> &'static str {
    match stop_reason {
        Some("max_tokens") => "length",
        Some("tool_use") => "tool_calls",
        _ => "stop",
    }
}

/// Convert a complete Claude `message` object to a chat completion.
///
/// `text` blocks join the message text, `thinking` blocks join
/// `reasoning_content`, `tool_use` blocks become OpenAI `tool_calls`, and
/// usage maps input/output tokens.
pub fn claude_message_to_completion(model: &str, payload: &Value) -> ChatCompletionResponse {
    let id = payload
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or("msg-unknown")
        .to_string();

    let mut text = String::new();
    let mut reasoning = String::new();
    let mut tool_calls: Vec<Value> = Vec::new();
    for block in payload
        .get("content")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        match block.get("type").and_then(Value::as_str) {
            Some("text") => {
                if let Some(part) = block.get("text").and_then(Value::as_str) {
                    if !text.is_empty() {
                        text.push('\n');
                    }
                    text.push_str(part);
                }
            }
            Some("thinking") => {
                if let Some(part) = block.get("thinking").and_then(Value::as_str) {
                    if !reasoning.is_empty() {
                        reasoning.push('\n');
                    }
                    reasoning.push_str(part);
                }
            }
            Some("tool_use") => {
                tool_calls.push(json!({
                    "id": block.get("id").cloned().unwrap_or(Value::Null),
                    "type": "function",
                    "function": {
                        "name": block.get("name").cloned().unwrap_or(Value::Null),
                        "arguments": block
                            .get("input")
                            .cloned()
                            .unwrap_or_else(|| json!({}))
                            .to_string(),
                    },
                }));
            }
            _ => {}
        }
    }

    let prompt = payload
        .pointer("/usage/input_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(0) as u32;
    let completion = payload
        .pointer("/usage/output_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(0) as u32;

    let message = ChatMessage {
        role: "assistant".to_string(),
        content: (!text.is_empty()).then(|| Value::from(text)),
        reasoning_content: (!reasoning.is_empty()).then_some(reasoning),
        tool_calls: (!tool_calls.is_empty()).then(|| Value::from(tool_calls)),
        tool_call_id: None,
        name: None,
    };

    ChatCompletionResponse {
        id: format!("chatcmpl-{id}"),
        object: "chat.completion",
        created: ids::unix_seconds(),
        model: model.to_string(),
        choices: vec![ChatChoice {
            index: 0,
            finish_reason: claude_stop_reason(payload.get("stop_reason").and_then(Value::as_str))
                .to_string(),
            message,
        }],
        usage: Usage {
            prompt_tokens: prompt,
            completion_tokens: completion,
            total_tokens: prompt + completion,
        },
        raw: None,
    }
}

/// Generic Claude-wire executor over HTTP with token rotation.
#[derive(Debug, Clone)]
pub struct ClaudeBackend {
    http: HttpClient,
    provider: String,
    endpoint_url: String,
    tokens: Vec<String>,
    next: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}

impl ClaudeBackend {
    /// Build for `provider` against `endpoint_url` (a full `/v1/messages`
    /// URL) with rotating tokens.
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
        let model = request.model.clone();
        let upstream = openai_to_claude(&model, &openai_request_body(&request), true);

        let response = self
            .post_messages(&upstream)
            .await
            .map_err(|error| GatewayError::Upstream(error.to_string()))?;

        let mut translator = ClaudeState::default();
        crate::responses::pump_sse_stream(
            response,
            &tx,
            |event, data| {
                let event_type = data
                    .get("type")
                    .and_then(Value::as_str)
                    .or(event.event.as_deref())
                    .unwrap_or("");
                Ok(
                    match convert_event(&mut translator, event_type, data, ids::unix_seconds()) {
                        Converted::Emit(chunk_json) => vec![chunk_json],
                        Converted::Ignore => Vec::new(),
                    },
                )
            },
            // Claude never sends `[DONE]`; the `message_stop` event emits the
            // terminal finish chunk. A body that ends without it is a fault.
            crate::responses::MissingTerminal::Fail,
        )
        .await
    }

    /// One `/v1/messages` POST with the rotating credential in both the
    /// `Authorization: Bearer` slot and `x-api-key`.
    async fn post_messages(
        &self,
        upstream: &Value,
    ) -> Result<reqwest::Response, heterion_router_http::client::HttpError> {
        let token = next_token(&self.tokens, &self.next).map(str::to_string);
        let mut headers: Vec<(&str, &str)> = vec![("anthropic-version", ANTHROPIC_VERSION)];
        if let Some(value) = token.as_deref() {
            headers.push(("x-api-key", value));
        }
        self.http
            .post_json_with_headers(&self.endpoint_url, token.as_deref(), &headers, upstream)
            .await
    }
}

#[async_trait]
impl ChatBackend for ClaudeBackend {
    fn name(&self) -> &'static str {
        "claude"
    }

    async fn complete(
        &self,
        request: ChatCompletionRequest,
    ) -> Result<ChatCompletionResponse, GatewayError> {
        if request.messages.is_empty() {
            return Err(GatewayError::InvalidRequest("messages is empty".into()));
        }
        let model = request.model.clone();
        let upstream = openai_to_claude(&model, &openai_request_body(&request), false);

        // A keyless pool gets exactly one attempt, a pool one per credential.
        let payload = try_each_credential(&self.provider, self.tokens.len().max(1), || {
            let upstream = upstream.clone();
            async move {
                let response = self
                    .post_messages(&upstream)
                    .await
                    .map_err(|error| GatewayError::Upstream(error.to_string()))?;
                response
                    .json()
                    .await
                    .map_err(|error| GatewayError::Upstream(error.to_string()))
            }
        })
        .await?;
        Ok(claude_message_to_completion(&model, &payload))
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

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{body_string_contains, header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn request(text: &str) -> ChatCompletionRequest {
        ChatCompletionRequest {
            model: "glm-5.3-flash".to_string(),
            messages: vec![ChatMessage {
                role: "user".to_string(),
                content: Some(Value::from(text)),
                reasoning_content: None,
                tool_calls: None,
                tool_call_id: None,
                name: None,
            }],
            stream: false,
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn completes_via_messages_api_with_anthropic_headers() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .and(header("anthropic-version", "2023-06-01"))
            .and(header("x-api-key", "sk-test"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": "msg_1",
                "type": "message",
                "role": "assistant",
                "content": [{"type": "text", "text": "OK"}],
                "stop_reason": "end_turn",
                "usage": {"input_tokens": 7, "output_tokens": 2}
            })))
            .mount(&server)
            .await;

        let backend = ClaudeBackend::new(
            "zai".to_string(),
            format!("{}/v1/messages", server.uri()),
            vec!["sk-test".to_string()],
        )
        .unwrap();
        let response = backend.complete(request("hi")).await.unwrap();
        assert_eq!(response.choices[0].message.text(), "OK");
        assert_eq!(response.choices[0].finish_reason, "stop");
        assert_eq!(response.usage.prompt_tokens, 7);
        assert_eq!(response.usage.completion_tokens, 2);
        assert_eq!(response.id, "chatcmpl-msg_1");
    }

    #[tokio::test]
    async fn a_tool_use_block_becomes_a_tool_call() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": "msg_2",
                "type": "message",
                "role": "assistant",
                "content": [
                    {"type": "text", "text": "calling"},
                    {"type": "tool_use", "id": "tu_1", "name": "search", "input": {"q": "rust"}}
                ],
                "stop_reason": "tool_use",
                "usage": {"input_tokens": 9, "output_tokens": 5}
            })))
            .mount(&server)
            .await;

        let backend = ClaudeBackend::new(
            "zai".to_string(),
            format!("{}/v1/messages", server.uri()),
            vec!["sk-test".to_string()],
        )
        .unwrap();
        let response = backend.complete(request("search for rust")).await.unwrap();
        let choice = &response.choices[0];
        assert_eq!(choice.finish_reason, "tool_calls");
        let calls = choice.message.tool_calls.as_ref().unwrap();
        assert_eq!(calls[0]["function"]["name"], json!("search"));
        assert!(
            calls[0]["function"]["arguments"]
                .as_str()
                .unwrap()
                .contains("rust"),
            "arguments must carry the tool input JSON"
        );
    }

    #[tokio::test]
    async fn an_upstream_401_surfaces_its_status() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .respond_with(ResponseTemplate::new(401).set_body_json(json!({
                "type": "error", "error": {"type": "authentication_error"}
            })))
            .mount(&server)
            .await;

        let backend = ClaudeBackend::new(
            "zai".to_string(),
            format!("{}/v1/messages", server.uri()),
            vec!["sk-bad".to_string()],
        )
        .unwrap();
        let error = backend.complete(request("hi")).await.unwrap_err();
        assert!(error.to_string().contains("upstream status 401"), "{error}");
    }

    #[tokio::test]
    async fn streams_claude_events_to_a_terminal_finish_chunk() {
        let server = MockServer::start().await;
        let sse = concat!(
            "event: message_start\n",
            "data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_s\",\"role\":\"assistant\"}}\n\n",
            "event: content_block_start\n",
            "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
            "event: content_block_delta\n",
            "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"he\"}}\n\n",
            "event: content_block_delta\n",
            "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"llo\"}}\n\n",
            "event: message_delta\n",
            "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":2}}\n\n",
            "event: message_stop\n",
            "data: {\"type\":\"message_stop\"}\n\n"
        );
        Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .and(body_string_contains("\"stream\":true"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "text/event-stream")
                    .set_body_string(sse),
            )
            .mount(&server)
            .await;

        let backend = ClaudeBackend::new(
            "zai".to_string(),
            format!("{}/v1/messages", server.uri()),
            vec!["sk-test".to_string()],
        )
        .unwrap();
        let mut request = request("hi");
        request.stream = true;
        use futures::StreamExt;
        let mut stream = backend.stream(request);
        let mut text = String::new();
        let mut finish = None;
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.unwrap();
            if let Some(content) = chunk.content {
                text.push_str(&content);
            }
            if let Some(reason) = chunk.finish_reason {
                finish = Some(reason);
            }
        }
        assert_eq!(text, "hello");
        assert_eq!(finish.as_deref(), Some("stop"));
    }
}

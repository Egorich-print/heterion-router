//! Chat backends.
//!
//! A [`ChatBackend`] abstracts "something that can answer a chat completion".
//! The first implementation is [`EchoBackend`], a deterministic in-process
//! backend used to prove out the HTTP/SSE surface before real provider
//! executors (grok-cli, OpenAI, Anthropic, ...) are ported over.

use async_trait::async_trait;
use futures::StreamExt;
use futures::stream::BoxStream;
use heterion_router_core::{
    ChatChoice, ChatCompletionRequest, ChatCompletionResponse, ChatMessage, GatewayError,
    StreamChunk, Usage, UsageDetail,
};
use heterion_router_usage::CachedUsage;
use serde_json::{Value, json};

/// A boxed, owned stream of deltas.
pub type ChunkStream = BoxStream<'static, Result<StreamChunk, GatewayError>>;

/// The discounted counts one stream chunk carried, as the accounting row's
/// type.
///
/// `None` when the chunk published none, so an accounting caller can tell
/// "nothing to book" from "reported zero" — the same thing
/// [`ChatCompletionResponse::raw`] does on the non-streaming path.
pub fn chunk_cached_usage(chunk: &StreamChunk) -> Option<CachedUsage> {
    chunk.usage_detail.map(|detail| CachedUsage {
        cache_read: detail.cache_read,
        cache_creation: detail.cache_creation,
        reasoning: detail.reasoning,
    })
}

/// Internal request key that ties usage rows to the request they belong to.
///
/// The gateway stamps it into [`ChatCompletionRequest::extra`]; a backend
/// that dispatches to other backends must remove it again before the request
/// reaches an upstream provider.
pub const ATTRIBUTION_TOKEN: &str = "_heterion_router_attribution";

/// Who served a request.
///
/// Usage rows and call logs are per-provider analytics, so the backend that
/// actually answered has to identify itself — not the router that picked it.
/// A backend serving one request at a time just describes itself; a router
/// reports the candidate it dispatched to.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Attribution {
    /// Canonical provider id (`"openrouter"`, `"grok-cli"`, ...), or `None`
    /// when nothing was ever dispatched — an unknown model, a combo whose
    /// every step failed. Recording the router's own name there would put a
    /// non-provider in `usage_history.provider` and poison every
    /// `WHERE provider = ?` aggregate.
    pub provider: Option<String>,
    /// Row id in `provider_connections`, when one can be named.
    ///
    /// A backend that dispatches through a named credential pool reports the
    /// row it really used. When it cannot, the router names the head of that
    /// provider's active rows in `priority DESC, id ASC` — the row dispatch
    /// tries first — rather than leaving the field `None`; see
    /// `routing::RoutingBackend::dispatch_head_connection_id` for why a
    /// deterministic dispatch-order row beats a blank. `None` therefore means
    /// "no row exists to name" (a keyless provider), not "unknown".
    pub connection_id: Option<String>,
    /// Combo the request was served through, when it was served through one.
    pub combo_name: Option<String>,
}

/// Stamp `request` with a fresh attribution token and hand it back.
pub fn stamp_attribution(request: &mut ChatCompletionRequest) -> String {
    let token = crate::ids::call_id();
    request
        .extra
        .insert(ATTRIBUTION_TOKEN.to_string(), json!(token.clone()));
    token
}

/// The attribution token [`stamp_attribution`] put in `request.extra`.
pub fn request_token(request: &ChatCompletionRequest) -> Option<String> {
    request
        .extra
        .get(ATTRIBUTION_TOKEN)
        .and_then(Value::as_str)
        .map(str::to_string)
}

/// Round-robin selection from a backend's credential pool.
///
/// One helper for every HTTP executor: each backend owns its own `tokens`
/// slice and `next` cursor, so the rotation rule lives here instead of being
/// copied per backend. `Relaxed` ordering is enough — the counter only needs
/// to be unique per call, not ordered against the pool read.
///
/// An empty pool is `None` rather than a panic: a provider with
/// `auth_type: optional` legitimately serves anonymous traffic. The cursor is
/// left untouched in that case, so a keyless pool never consumes a slot from
/// the rotation a later-inserted key would see.
pub(crate) fn next_token<'a>(
    tokens: &'a [String],
    next: &std::sync::atomic::AtomicUsize,
) -> Option<&'a str> {
    if tokens.is_empty() {
        return None;
    }
    let index = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed) % tokens.len();
    tokens.get(index).map(String::as_str)
}

/// Anything that can serve a chat completion.
#[async_trait]
pub trait ChatBackend: Send + Sync + 'static {
    /// Human-readable backend name, used in logs and health output.
    fn name(&self) -> &'static str;

    /// Attribution for the request stamped with `token`.
    ///
    /// Defaults to "me": a backend that serves a request directly knows both
    /// its provider id and (when it has a single named connection) its
    /// connection id.
    fn attribution(&self, _token: &str) -> Attribution {
        Attribution {
            provider: Some(self.name().to_string()),
            connection_id: None,
            combo_name: None,
        }
    }

    /// Serve a complete (non-streaming) response.
    async fn complete(
        &self,
        request: ChatCompletionRequest,
    ) -> Result<ChatCompletionResponse, GatewayError>;

    /// Serve a streaming response as a sequence of deltas.
    fn stream(&self, request: ChatCompletionRequest) -> ChunkStream;
}

/// Build an OpenAI-shaped request body from a typed request.
///
/// Shared by executors that speak OpenAI Chat Completions natively or via a
/// translator that consumes the same shape.
pub(crate) fn openai_request_body(request: &ChatCompletionRequest) -> Value {
    // Messages are forwarded verbatim (typed as JSON) so agent turns keep
    // their `tool_calls` / `tool_call_id` / null content intact.
    let mut body = json!({
        "model": request.model,
        "messages": request.messages,
        "stream": request.stream,
    });
    if let Some(max_tokens) = request.max_tokens {
        body["max_tokens"] = json!(max_tokens);
    }
    if let Some(temperature) = request.temperature {
        body["temperature"] = json!(temperature);
    }
    if let Some(top_p) = request.top_p {
        body["top_p"] = json!(top_p);
    }
    // Tool definitions must reach the upstream, or the model invents an XML
    // pseudo-call instead of a real `tool_calls` response.
    if let Some(tools) = &request.tools {
        body["tools"] = tools.clone();
    }
    // Codestral drifts to prose when tools are merely offered; pin the choice
    // unless the client made one (ADR-014-style policy, see reasoning.rs).
    if request
        .tools
        .as_ref()
        .and_then(Value::as_array)
        .is_some_and(|tools| !tools.is_empty())
        && body.get("tool_choice").is_none()
        && crate::reasoning::needs_explicit_tool_choice(&request.model)
    {
        body["tool_choice"] = json!("required");
    }
    if let Some(tool_choice) = &request.tool_choice {
        body["tool_choice"] = tool_choice.clone();
    }
    if let Some(response_format) = &request.response_format {
        body["response_format"] = response_format.clone();
    }
    if let Some(stop) = &request.stop {
        body["stop"] = stop.clone();
    }
    if let Some(top_k) = request.top_k {
        body["top_k"] = json!(top_k);
    }
    if let Some(max_completion_tokens) = request.max_completion_tokens {
        body["max_completion_tokens"] = json!(max_completion_tokens);
    }
    // ADR-014: the router predefines a reasoning level per model family;
    // an explicit client `reasoning_effort` always wins over the default.
    let effort = request
        .reasoning_effort
        .clone()
        .or_else(|| crate::reasoning::default_effort(&request.model).map(str::to_string));
    if let Some(effort) = effort {
        body["reasoning_effort"] = json!(effort);
    }
    if let Some(stream_options) = &request.stream_options {
        body["stream_options"] = stream_options.clone();
    }
    if let Some(parallel_tool_calls) = request.parallel_tool_calls {
        body["parallel_tool_calls"] = json!(parallel_tool_calls);
    }
    if let Some(metadata) = &request.metadata {
        body["metadata"] = metadata.clone();
    }
    if let Some(service_tier) = &request.service_tier {
        body["service_tier"] = json!(service_tier);
    }
    if let Some(seed) = request.seed {
        body["seed"] = json!(seed);
    }
    if let Some(verbosity) = &request.verbosity {
        body["verbosity"] = json!(verbosity);
    }
    for (key, value) in &request.extra {
        body[key] = value.clone();
    }
    body
}

/// Reduce an OpenAI `chat.completion.chunk` object to a gateway `StreamChunk`.
///
/// Returns `None` for chunks without content, reasoning or finish signal.
pub(crate) fn openai_chunk_to_stream_chunk(value: &Value) -> Option<StreamChunk> {
    let choice = value.get("choices")?.get(0)?;
    let delta = choice.get("delta")?;
    // Empty-string content is a placeholder some upstreams (OpenRouter free
    // tiers) stream alongside reasoning; it must not become a delta.
    let content = delta
        .get("content")
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
        .map(str::to_string);
    // OpenRouter uses `reasoning`; OpenAI-compatible proxies use
    // `reasoning_content`. Accept both.
    let reasoning = delta
        .get("reasoning_content")
        .or_else(|| delta.get("reasoning"))
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
        .map(str::to_string);
    let finish = choice
        .get("finish_reason")
        .and_then(Value::as_str)
        .map(str::to_string);
    let tool_calls = delta.get("tool_calls").cloned();
    let usage = value
        .get("usage")
        .filter(|value| value.is_object())
        .map(|value| {
            let prompt_tokens = value
                .get("prompt_tokens")
                .or_else(|| value.get("input_tokens"))
                .and_then(Value::as_u64)
                .unwrap_or(0) as u32;
            let completion_tokens = value
                .get("completion_tokens")
                .or_else(|| value.get("output_tokens"))
                .and_then(Value::as_u64)
                .unwrap_or(0) as u32;
            let total_tokens = value
                .get("total_tokens")
                .and_then(Value::as_u64)
                .unwrap_or(u64::from(prompt_tokens) + u64::from(completion_tokens))
                as u32;
            Usage {
                prompt_tokens,
                completion_tokens,
                total_tokens,
            }
        });
    if content.is_none()
        && reasoning.is_none()
        && tool_calls.is_none()
        && finish.is_none()
        && usage.is_none()
    {
        return None;
    }
    // The discounted counts ride the same usage frame, read by the
    // accounting layer's parser so every upstream spelling is known in
    // exactly one place. Absent means the frame had nothing to discount.
    let cached = CachedUsage::from_payload(value);
    let usage_detail = (cached != CachedUsage::default()).then_some(UsageDetail {
        cache_read: cached.cache_read,
        cache_creation: cached.cache_creation,
        reasoning: cached.reasoning,
    });
    Some(StreamChunk {
        content,
        reasoning,
        tool_calls,
        finish_reason: finish,
        usage,
        usage_detail,
    })
}

/// Deterministic backend that echoes the last user message.
///
/// Useful as a smoke target and as a reference implementation for the
/// streaming contract; it never touches the network.
#[derive(Debug, Default, Clone)]
pub struct EchoBackend;

impl EchoBackend {
    fn reply(request: &ChatCompletionRequest) -> String {
        let last_user = request
            .messages
            .iter()
            .rev()
            .find(|m| m.role == "user")
            .map(|m| m.text())
            .unwrap_or_default();
        format!("echo: {last_user}")
    }
}

#[async_trait]
impl ChatBackend for EchoBackend {
    fn name(&self) -> &'static str {
        "echo"
    }

    async fn complete(
        &self,
        request: ChatCompletionRequest,
    ) -> Result<ChatCompletionResponse, GatewayError> {
        if request.messages.is_empty() {
            return Err(GatewayError::InvalidRequest("messages is empty".into()));
        }

        let reply = Self::reply(&request);
        let prompt_tokens = request
            .messages
            .iter()
            .map(|m| m.text().len())
            .sum::<usize>() as u32;
        let completion_tokens = reply.len() as u32;

        Ok(ChatCompletionResponse {
            id: crate::ids::completion_id(),
            object: "chat.completion",
            created: crate::ids::unix_seconds(),
            model: request.model,
            choices: vec![ChatChoice {
                index: 0,
                message: ChatMessage::assistant_text(reply),
                finish_reason: "stop".into(),
            }],
            usage: Usage {
                prompt_tokens,
                completion_tokens,
                total_tokens: prompt_tokens + completion_tokens,
            },
            raw: None,
        })
    }

    fn stream(&self, request: ChatCompletionRequest) -> ChunkStream {
        if request.messages.is_empty() {
            return Box::pin(futures::stream::once(async {
                Err(GatewayError::InvalidRequest("messages is empty".into()))
            }));
        }

        let words: Vec<String> = Self::reply(&request)
            .split_inclusive(' ')
            .map(|word| word.to_string())
            .collect();

        let deltas = futures::stream::iter(
            words
                .into_iter()
                .map(|word| Ok(StreamChunk::content(word)))
                .collect::<Vec<Result<StreamChunk, GatewayError>>>(),
        );

        Box::pin(deltas.chain(futures::stream::once(async {
            Ok(StreamChunk::done("stop"))
        })))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn next_token_rotates_and_wraps() {
        let tokens = vec!["a".to_string(), "b".to_string(), "c".to_string()];
        let cursor = std::sync::atomic::AtomicUsize::new(0);
        let picked: Vec<&str> = (0..5)
            .map(|_| next_token(&tokens, &cursor).expect("key"))
            .collect();
        assert_eq!(picked, vec!["a", "b", "c", "a", "b"]);
    }

    #[test]
    fn next_token_on_empty_pool_is_none_and_spares_the_cursor() {
        let tokens: Vec<String> = Vec::new();
        let cursor = std::sync::atomic::AtomicUsize::new(0);
        assert_eq!(next_token(&tokens, &cursor), None);
        // A keyless provider must not burn a rotation slot.
        assert_eq!(cursor.load(std::sync::atomic::Ordering::Relaxed), 0);
    }

    #[test]
    fn next_token_resumes_at_zero_after_a_keyless_call() {
        let tokens = vec!["only".to_string()];
        let cursor = std::sync::atomic::AtomicUsize::new(0);
        let empty: Vec<String> = Vec::new();
        assert_eq!(next_token(&empty, &cursor), None);
        assert_eq!(next_token(&tokens, &cursor), Some("only"));
    }

    #[test]
    fn chunk_mapping_carries_tool_calls() {
        let chunk = openai_chunk_to_stream_chunk(&json!({
            "choices": [{
                "index": 0,
                "delta": {"tool_calls": [{"index": 0, "function": {"arguments": "{}"}}]},
                "finish_reason": null,
            }],
        }))
        .expect("chunk present");
        assert!(chunk.tool_calls.is_some());
        assert!(chunk.content.is_none());
    }

    #[test]
    fn chunk_mapping_accepts_openrouter_reasoning() {
        let chunk = openai_chunk_to_stream_chunk(&json!({
            "choices": [{
                "index": 0,
                "delta": {"content": "", "role": "assistant", "reasoning": "thinking"},
                "finish_reason": null,
            }],
        }))
        .expect("reasoning chunk present");
        assert_eq!(chunk.reasoning.as_deref(), Some("thinking"));
        assert!(chunk.content.is_none(), "empty content must be dropped");
    }

    #[test]
    fn chunk_mapping_extracts_terminal_usage() {
        let chunk = openai_chunk_to_stream_chunk(&json!({
            "choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}],
            "usage": {"prompt_tokens": 3, "completion_tokens": 2, "total_tokens": 5},
        }))
        .expect("terminal chunk present");
        assert_eq!(
            chunk.usage,
            Some(Usage {
                prompt_tokens: 3,
                completion_tokens: 2,
                total_tokens: 5,
            })
        );
    }

    /// The usage frame of a stream is the only place a cache hit is ever
    /// published, so dropping its detail bills the hit as fresh input.
    #[test]
    fn chunk_mapping_keeps_the_discounted_detail_off_the_usage_frame() {
        let chunk = openai_chunk_to_stream_chunk(&json!({
            "choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}],
            "usage": {
                "prompt_tokens": 1000,
                "completion_tokens": 500,
                "prompt_tokens_details": {"cached_tokens": 200},
                "completion_tokens_details": {"reasoning_tokens": 64},
            },
        }))
        .expect("terminal chunk present");
        assert_eq!(
            chunk.usage_detail,
            Some(UsageDetail {
                cache_read: 200,
                cache_creation: 0,
                reasoning: 64,
            }),
            "the usage frame reported 200 cached and 64 reasoning tokens"
        );
        assert_eq!(
            chunk_cached_usage(&chunk),
            Some(CachedUsage {
                cache_read: 200,
                cache_creation: 0,
                reasoning: 64,
            }),
            "the accounting layer reads the same counts"
        );
    }

    /// A usage frame with no detail block is what every provider that never
    /// caches streams: nothing to book, and nothing invented.
    #[test]
    fn chunk_mapping_reports_no_detail_for_a_plain_usage_frame() {
        let chunk = openai_chunk_to_stream_chunk(&json!({
            "choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}],
            "usage": {"prompt_tokens": 3, "completion_tokens": 2, "total_tokens": 5},
        }))
        .expect("terminal chunk present");
        assert_eq!(chunk.usage_detail, None);
        assert_eq!(chunk_cached_usage(&chunk), None);
        // A content delta never carries one either.
        assert_eq!(
            openai_chunk_to_stream_chunk(&json!({
                "choices": [{"index": 0, "delta": {"content": "hi"}, "finish_reason": null}],
            }))
            .expect("content chunk present")
            .usage_detail,
            None
        );
    }

    #[test]
    fn chunk_mapping_drops_empty_deltas() {
        assert!(
            openai_chunk_to_stream_chunk(&json!({
                "choices": [{"index": 0, "delta": {}, "finish_reason": null}],
            }))
            .is_none()
        );
    }

    fn request(text: &str) -> ChatCompletionRequest {
        ChatCompletionRequest {
            model: "test-model".into(),
            messages: vec![user_message(text)],
            stream: false,
            max_tokens: None,
            temperature: None,
            top_p: None,
            tools: None,
            tool_choice: None,
            ..Default::default()
        }
    }

    fn user_message(text: &str) -> ChatMessage {
        ChatMessage {
            role: "user".into(),
            content: Some(serde_json::Value::from(text)),
            reasoning_content: None,
            tool_calls: None,
            tool_call_id: None,
            name: None,
        }
    }

    #[test]
    fn request_body_forwards_agent_controls() {
        let mut request = request("hello");
        request.max_completion_tokens = Some(128);
        request.reasoning_effort = Some("high".to_string());
        request.parallel_tool_calls = Some(false);
        request.stream_options = Some(json!({"include_usage": true}));
        request.metadata = Some(json!({"trace": "t1"}));
        let body = openai_request_body(&request);
        assert_eq!(body["max_completion_tokens"], json!(128));
        assert_eq!(body["reasoning_effort"], json!("high"));
        assert_eq!(body["parallel_tool_calls"], json!(false));
        assert_eq!(body["stream_options"]["include_usage"], json!(true));
        assert_eq!(body["metadata"]["trace"], json!("t1"));
    }

    #[test]
    fn request_body_predefines_reasoning_for_evidenced_families() {
        // ADR-014: no explicit effort → the family default applies.
        let mut request = request("hello");
        request.model = "grok-4.6".into();
        let body = openai_request_body(&request);
        assert_eq!(body["reasoning_effort"], json!("medium"));

        request.model = "claude-opus-4".into();
        let body = openai_request_body(&request);
        assert_eq!(body["reasoning_effort"], json!("high"));

        // Combo-rewritten ids resolve the bare model part.
        request.model = "grok-cli/grok-4.6".into();
        let body = openai_request_body(&request);
        assert_eq!(body["reasoning_effort"], json!("medium"));

        // Unevidenced families stay on the provider default.
        request.model = "mistral-large".into();
        let body = openai_request_body(&request);
        assert!(body.get("reasoning_effort").is_none());
    }

    #[test]
    fn explicit_reasoning_effort_wins_over_the_predefined_default() {
        let mut request = request("hello");
        request.model = "grok-4.6".into();
        request.reasoning_effort = Some("low".to_string());
        let body = openai_request_body(&request);
        assert_eq!(body["reasoning_effort"], json!("low"));
    }

    #[test]
    fn codestral_gets_a_pinned_tool_choice_when_tools_are_present() {
        let tool = json!({"type": "function", "function": {"name": "read_file", "parameters": {}}});
        let mut request = request("read /etc/hosts");
        request.model = "mistral/codestral-latest".into();
        request.tools = Some(json!([tool]));
        // Without a client choice, the family policy pins the call.
        let body = openai_request_body(&request);
        assert_eq!(body["tool_choice"], json!("required"));
        assert_eq!(body["tools"][0]["function"]["name"], json!("read_file"));

        // A client-set choice is forwarded untouched.
        let mut with_choice = request.clone();
        with_choice.tool_choice = Some(json!({"type": "function", "function": {"name": "other"}}));
        let body = openai_request_body(&with_choice);
        assert_eq!(
            body["tool_choice"],
            json!({"type": "function", "function": {"name": "other"}})
        );

        // No tools → nothing to pin; other families stay untouched.
        let mut no_tools = request.clone();
        no_tools.tools = None;
        assert!(openai_request_body(&no_tools).get("tool_choice").is_none());
        let mut empty_tools = request.clone();
        empty_tools.tools = Some(json!([]));
        assert!(
            openai_request_body(&empty_tools)
                .get("tool_choice")
                .is_none()
        );
        let mut other = request;
        other.model = "grok-4.6".into();
        assert!(openai_request_body(&other).get("tool_choice").is_none());
    }

    #[tokio::test]
    async fn complete_echoes_last_user_message() {
        let backend = EchoBackend;
        let response = backend.complete(request("hello")).await.unwrap();
        assert_eq!(response.choices[0].message.text(), "echo: hello");
        assert_eq!(response.choices[0].finish_reason, "stop");
    }

    #[tokio::test]
    async fn complete_rejects_empty_messages() {
        let backend = EchoBackend;
        let mut req = request("x");
        req.messages.clear();
        assert_eq!(
            backend.complete(req).await.unwrap_err(),
            GatewayError::InvalidRequest("messages is empty".into())
        );
    }

    #[tokio::test]
    async fn stream_ends_with_finish_reason() {
        use futures::StreamExt;

        let backend = EchoBackend;
        let chunks: Vec<_> = backend
            .stream(request("hi"))
            .collect::<Vec<_>>()
            .await
            .into_iter()
            .map(Result::unwrap)
            .collect();

        assert_eq!(
            chunks.last().unwrap().finish_reason.as_deref(),
            Some("stop")
        );
        let text: String = chunks
            .iter()
            .filter_map(|c| c.content.clone())
            .collect::<Vec<_>>()
            .join("");
        assert_eq!(text, "echo: hi");
    }
}

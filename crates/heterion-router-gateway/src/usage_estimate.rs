//! Token estimates for upstreams that report no usage at all.
//!
//! Several upstreams never send a `usage` frame on a streamed response:
//! Ollama Cloud, the `vivanta-*` gateways, `OpenRouter-free` and `Google`.
//! Without a local estimate those calls book zero tokens, so a provider that
//! is being used heavily looks exactly like one that was never called — and
//! the cost of a long mission becomes invisible.
//!
//! The estimate is the same bytes/4 heuristic the Anthropic
//! `/v1/messages/count_tokens` endpoint already uses, so both sides of the
//! gateway agree on what "a token" means here. It is an approximation, and it
//! is only ever used when the upstream said nothing: a provider that reports
//! usage is never re-estimated, so counts are never double booked.

use heterion_router_core::{ChatCompletionRequest, Usage};
use serde_json::Value;

/// Fill in the counts an upstream left at zero.
///
/// A provider that reports its own usage is never touched: each field is only
/// filled when the upstream said nothing about it. That keeps the counts
/// honest in both directions — a real zero is not overwritten, and a missing
/// count does not read as "free".
pub fn fill_missing(usage: &mut Usage, prompt_estimate: u32, output_bytes: usize) -> bool {
    let mut filled = false;
    if usage.prompt_tokens == 0 {
        usage.prompt_tokens = prompt_estimate.max(1);
        filled = true;
    }
    if usage.completion_tokens == 0 {
        usage.completion_tokens = estimate_output_tokens(output_bytes);
        filled = true;
    }
    if filled {
        usage.total_tokens = usage.prompt_tokens.saturating_add(usage.completion_tokens);
    }
    filled
}

/// Bytes of assistant text in a response payload.
///
/// The content is a plain string for most providers and an array of parts for
/// the multimodal ones, so both shapes have to be measured.
pub fn value_text_bytes(value: Option<&Value>) -> usize {
    let Some(value) = value else { return 0 };
    match value {
        Value::String(text) => text.len(),
        Value::Array(parts) => parts
            .iter()
            .map(|part| match part.get("text").and_then(Value::as_str) {
                Some(text) => text.len(),
                None => part.to_string().len(),
            })
            .sum(),
        Value::Null => 0,
        other => other.to_string().len(),
    }
}

/// Rough prompt size in tokens: bytes / 4 over the flattened turns, the tool
/// calls riding on them and the tool definitions, plus four tokens of framing
/// per turn. Floored at one so even an empty body reports a positive count.
pub fn estimate_prompt_tokens(request: &ChatCompletionRequest) -> u32 {
    let mut bytes = 0usize;
    for message in &request.messages {
        bytes += message.role.len() + message.text().len() + 16;
        if let Some(calls) = &message.tool_calls {
            bytes += calls.to_string().len();
        }
        if let Some(id) = &message.tool_call_id {
            bytes += id.len();
        }
    }
    if let Some(tools) = &request.tools {
        bytes += tools.to_string().len();
    }
    bytes_to_tokens(bytes)
}

/// Rough completion size in tokens for text the gateway has already seen.
///
/// `bytes` is counted rather than kept as a string: the estimate only needs the
/// size, and a long stream would otherwise double the memory the response
/// costs.
pub fn estimate_output_tokens(bytes: usize) -> u32 {
    bytes_to_tokens(bytes)
}

/// Bytes / 4, floored at one.
fn bytes_to_tokens(bytes: usize) -> u32 {
    (bytes / 4 + 1) as u32
}

#[cfg(test)]
mod tests {
    use super::*;
    use heterion_router_core::{ChatCompletionRequest, ChatMessage};

    fn request_with(text: &str) -> ChatCompletionRequest {
        ChatCompletionRequest {
            model: "m".to_string(),
            messages: vec![ChatMessage::plain("user", text)],
            ..Default::default()
        }
    }

    #[test]
    fn an_empty_request_still_reports_one_token() {
        let request = ChatCompletionRequest {
            model: "m".to_string(),
            ..Default::default()
        };
        assert_eq!(estimate_prompt_tokens(&request), 1);
    }

    #[test]
    fn prompt_estimate_grows_with_the_request() {
        let small = estimate_prompt_tokens(&request_with("hi"));
        let large = estimate_prompt_tokens(&request_with(&"x".repeat(4000)));
        assert!(large > small, "{large} !> {small}");
    }

    #[test]
    fn output_estimate_grows_with_the_bytes() {
        assert_eq!(estimate_output_tokens(0), 1);
        assert_eq!(estimate_output_tokens(4), 2);
        assert!(estimate_output_tokens(4000) > estimate_output_tokens(400));
    }

    #[test]
    fn fill_missing_only_touches_the_fields_the_upstream_left_at_zero() {
        let mut usage = Usage {
            prompt_tokens: 0,
            completion_tokens: 0,
            total_tokens: 0,
        };
        assert!(fill_missing(&mut usage, 100, 40));
        assert_eq!(usage.prompt_tokens, 100);
        assert_eq!(usage.completion_tokens, 11);
        assert_eq!(usage.total_tokens, 111);

        // A provider that reported its own counts keeps them.
        let mut reported = Usage {
            prompt_tokens: 7,
            completion_tokens: 9,
            total_tokens: 16,
        };
        assert!(!fill_missing(&mut reported, 100, 40));
        assert_eq!(
            reported,
            Usage {
                prompt_tokens: 7,
                completion_tokens: 9,
                total_tokens: 16
            }
        );
    }

    #[test]
    fn text_bytes_cover_both_content_shapes() {
        assert_eq!(value_text_bytes(None), 0);
        assert_eq!(
            value_text_bytes(Some(&Value::String("hello".to_string()))),
            5
        );
        let parts = serde_json::json!([
            {"type": "text", "text": "abc"},
            {"type": "text", "text": "de"}
        ]);
        assert_eq!(value_text_bytes(Some(&parts)), 5);
    }
}

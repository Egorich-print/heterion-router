//! Shared `chat.completion.chunk` scaffolding.
//!
//! Every OpenAI-facing streaming adapter (`claude`, `gemini`, `responses`)
//! emits the same single-choice envelope; only the identity sources, the role
//! announcement policy, the `finish_reason` mapping and the `usage` shape are
//! genuinely per-protocol. Those differences live in the adapters — this module
//! holds only the parts that are byte-identical across all of them, so the wire
//! format is defined once.
//!
//! Nothing here emits `[DONE]`: the terminal sentinel is appended by the SSE
//! writer downstream, after the adapter's finish chunk.

use serde_json::{Map, Value, json};

/// `object` stamped on every chunk of an OpenAI-compatible stream.
pub(crate) const OBJECT: &str = "chat.completion.chunk";

/// Role announced once per stream.
pub(crate) const ROLE_ASSISTANT: &str = "assistant";

/// Identity fallback for adapters whose state has not seen an id/model yet.
pub(crate) const UNKNOWN: &str = "unknown";

/// `chatcmpl-` identity for the adapters that hold an optional upstream id.
pub(crate) fn chatcmpl_id(message_id: Option<&str>) -> String {
    format!("chatcmpl-{}", message_id.unwrap_or(UNKNOWN))
}

/// Model name for the adapters that hold an optional upstream model.
pub(crate) fn chunk_model(model: Option<&str>) -> String {
    model.unwrap_or(UNKNOWN).to_string()
}

/// Announce `role: "assistant"` once, on the first delta to go out.
///
/// Adapters that emit a standalone role chunk (`claude`, `gemini`) leave their
/// `role_sent` flag untouched and call this only from their tool/delta paths;
/// `responses` piggybacks the role on its very first delta instead.
pub(crate) fn announce_role(delta: &mut Map<String, Value>, role_sent: &mut bool) {
    if !*role_sent {
        delta.insert("role".to_string(), Value::from(ROLE_ASSISTANT));
        *role_sent = true;
    }
}

/// Add the `content` / `reasoning_content` delta fields; absent when `None`.
pub(crate) fn insert_text(
    delta: &mut Map<String, Value>,
    content: Option<&str>,
    reasoning: Option<&str>,
) {
    if let Some(text) = content {
        delta.insert("content".to_string(), Value::from(text));
    }
    if let Some(text) = reasoning {
        delta.insert("reasoning_content".to_string(), Value::from(text));
    }
}

/// The single-choice envelope. `finish_reason` is `Value::Null` for streamed
/// chunks (see [`stream_chunk`]) and the mapped reason for the terminal one.
pub(crate) fn chunk(
    id: &str,
    created: i64,
    model: &str,
    delta: Value,
    finish_reason: Value,
) -> Value {
    json!({
        "id": id,
        "object": OBJECT,
        "created": created,
        "model": model,
        "choices": [{
            "index": 0,
            "delta": delta,
            "finish_reason": finish_reason,
        }],
    })
}

/// A non-terminal chunk: `finish_reason` is always null, whatever the delta.
pub(crate) fn stream_chunk(id: &str, created: i64, model: &str, delta: Value) -> Value {
    chunk(id, created, model, delta, Value::Null)
}

/// Attach the terminal `usage` object. No usage leaves the chunk untouched.
pub(crate) fn attach_usage(mut chunk: Value, usage: Option<Value>) -> Value {
    if let Some(usage) = usage {
        chunk
            .as_object_mut()
            .expect("chunk is an object")
            .insert("usage".to_string(), usage);
    }
    chunk
}

/// `usage` skeleton; adapters add the per-protocol detail blocks on top.
pub(crate) fn usage_root(
    prompt_tokens: u32,
    completion_tokens: u32,
    total_tokens: u32,
) -> Map<String, Value> {
    let mut root = Map::new();
    root.insert("prompt_tokens".to_string(), json!(prompt_tokens));
    root.insert("completion_tokens".to_string(), json!(completion_tokens));
    root.insert("total_tokens".to_string(), json!(total_tokens));
    root
}

/// `prompt_tokens_details` for cache hits, or `None` when there are none.
///
/// Gemini has no cache-creation field, so it passes `0` for `cache_creation`.
pub(crate) fn cached_prompt_details(cached: u32, cache_creation: u32) -> Option<Value> {
    if cached == 0 && cache_creation == 0 {
        return None;
    }
    let mut details = Map::new();
    if cached > 0 {
        details.insert("cached_tokens".to_string(), json!(cached));
    }
    if cache_creation > 0 {
        details.insert("cache_creation_tokens".to_string(), json!(cache_creation));
    }
    Some(Value::Object(details))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_falls_back_to_unknown() {
        assert_eq!(chatcmpl_id(Some("abc")), "chatcmpl-abc");
        assert_eq!(chatcmpl_id(None), "chatcmpl-unknown");
        assert_eq!(chunk_model(Some("m")), "m");
        assert_eq!(chunk_model(None), "unknown");
    }

    #[test]
    fn role_is_announced_exactly_once() {
        let mut sent = false;
        let mut first = Map::new();
        announce_role(&mut first, &mut sent);
        assert_eq!(first["role"], json!(ROLE_ASSISTANT));
        assert!(sent);

        let mut second = Map::new();
        announce_role(&mut second, &mut sent);
        assert!(second.is_empty());
    }

    #[test]
    fn text_delta_skips_absent_fields() {
        let mut delta = Map::new();
        insert_text(&mut delta, None, None);
        assert!(delta.is_empty(), "empty delta object");

        insert_text(&mut delta, Some("hi"), Some("why"));
        assert_eq!(delta["content"], json!("hi"));
        assert_eq!(delta["reasoning_content"], json!("why"));
    }

    #[test]
    fn streamed_chunks_never_carry_a_finish_reason() {
        let value = stream_chunk("chatcmpl-1", 7, "m", Value::Object(Map::new()));
        assert_eq!(value["object"], json!(OBJECT));
        assert_eq!(value["created"], json!(7));
        assert_eq!(value["choices"][0]["index"], json!(0));
        assert_eq!(value["choices"][0]["finish_reason"], Value::Null);
        assert!(value.get("usage").is_none());
    }

    #[test]
    fn usage_is_attached_only_when_present() {
        let plain = stream_chunk("chatcmpl-1", 7, "m", json!({}));
        assert!(attach_usage(plain.clone(), None).get("usage").is_none());

        let with_usage = attach_usage(plain, Some(json!({"prompt_tokens": 1})));
        assert_eq!(with_usage["usage"]["prompt_tokens"], json!(1));
    }

    #[test]
    fn cached_details_are_omitted_when_zero() {
        assert!(cached_prompt_details(0, 0).is_none());
        assert_eq!(
            cached_prompt_details(3, 0).unwrap(),
            json!({"cached_tokens": 3})
        );
        assert_eq!(
            cached_prompt_details(3, 2).unwrap(),
            json!({"cached_tokens": 3, "cache_creation_tokens": 2})
        );
    }
}

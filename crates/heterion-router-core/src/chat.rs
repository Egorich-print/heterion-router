//! Request/response shapes for the OpenAI-compatible surface.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// A single chat message.
///
/// `content` stays raw JSON: OpenAI allows a string, an array of parts, or
/// `null` (an assistant turn that carries only tool calls). `tool_calls` and
/// `tool_call_id` are forwarded verbatim so agent turns round-trip instead of
/// failing deserialization.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<Value>,
    /// Thinking text for models that expose it (DeepSeek/OpenRouter style).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_content: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

impl ChatMessage {
    /// A message carrying a plain string content.
    pub fn plain(role: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            role: role.into(),
            content: Some(Value::from(content.into())),
            reasoning_content: None,
            tool_calls: None,
            tool_call_id: None,
            name: None,
        }
    }

    /// A plain assistant text message.
    pub fn assistant_text(text: impl Into<String>) -> Self {
        Self {
            role: "assistant".to_string(),
            content: Some(Value::from(text.into())),
            reasoning_content: None,
            tool_calls: None,
            tool_call_id: None,
            name: None,
        }
    }

    /// Plain-text view of `content` (string, or concatenated `text` parts).
    pub fn text(&self) -> String {
        content_text(self.content.as_ref())
    }

    /// Whether the message carries tool calls.
    pub fn has_tool_calls(&self) -> bool {
        self.tool_calls
            .as_ref()
            .and_then(Value::as_array)
            .is_some_and(|calls| !calls.is_empty())
    }
}

/// Which entries of a chat `content` array contribute their text.
///
/// The default ([`TextParts::TextField`]) is what OpenAI and Gemini clients
/// mean in practice; the other modes exist because the Claude and Responses
/// adapters accept different shapes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TextParts {
    /// Objects carrying a **string** `text` field, whatever their `type`.
    ///
    /// Bare strings, `refusal`-only parts, objects without `text` and
    /// non-string `text` values all contribute nothing.
    #[default]
    TextField,
    /// Objects whose `type` is exactly `"text"`.
    ///
    /// Unlike [`TextParts::TextField`] the entry is kept whatever its `text`
    /// holds: a number or bool is rendered in its JSON form, and a part with
    /// no usable `text` yields `""`. Everything else is dropped.
    TypedText,
    /// Bare strings plus objects carrying a string `text` field, with an
    /// object tagged `"text"` but without a usable `text` yielding `""`.
    Loose,
}

/// Rules for joining the text of the entries [`content_text_with`] kept.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TextJoin {
    /// Which array entries count as text.
    pub parts: TextParts,
    /// Placed between two kept entries.
    pub separator: &'static str,
    /// Drop empty entry texts before joining. Only [`TextParts::Loose`] can
    /// produce them, so this only has an effect there.
    pub skip_empty: bool,
}

impl TextJoin {
    /// Build join rules; see the fields for their meaning.
    pub const fn new(parts: TextParts, separator: &'static str, skip_empty: bool) -> Self {
        Self {
            parts,
            separator,
            skip_empty,
        }
    }
}

/// Flatten a chat `content` value into plain text with the default rules:
/// [`TextParts::TextField`] entries concatenated with no separator.
///
/// `content` is the raw field itself, so it mirrors `Value::get`: a missing
/// field and an explicit `null` are the same case, and a string is returned
/// verbatim while any other non-array value yields `""`. An array of objects
/// contributes the `text` of each part that has one as a string, which is
/// what OpenAI and Gemini clients send.
pub fn content_text(content: Option<&Value>) -> String {
    content_text_with(content, TextJoin::default())
}

/// Flatten a chat `content` value into plain text under explicit join rules.
///
/// Same value handling as [`content_text`]; `join` only changes which array
/// entries are kept ([`TextParts`]) and how their texts are combined
/// ([`TextJoin`]). The translators pin the adapter they mirror — Claude joins
/// `type: "text"` parts with `\n`, the Responses adapter keeps bare strings
/// and joins with a blank line — so the difference is stated here once
/// instead of being re-derived in every translator.
pub fn content_text_with(content: Option<&Value>, join: TextJoin) -> String {
    let mut texts = match content {
        Some(Value::String(text)) => return text.clone(),
        Some(Value::Array(parts)) => parts
            .iter()
            .filter_map(|part| part_text(part, join.parts))
            .collect::<Vec<_>>(),
        _ => return String::new(),
    };
    if join.skip_empty {
        texts.retain(|text| !text.is_empty());
    }
    texts.join(join.separator)
}

/// Text of one `content` array entry, or `None` when it is not text.
fn part_text(part: &Value, mode: TextParts) -> Option<String> {
    match mode {
        TextParts::TextField => part.get("text").and_then(Value::as_str).map(str::to_string),
        TextParts::TypedText => {
            if part.get("type").and_then(Value::as_str) != Some("text") {
                return None;
            }
            Some(match part.get("text") {
                Some(Value::String(text)) => text.clone(),
                Some(Value::Number(number)) => number.to_string(),
                Some(Value::Bool(flag)) => flag.to_string(),
                _ => String::new(),
            })
        }
        TextParts::Loose => match part {
            Value::String(text) => Some(text.clone()),
            _ if part.get("type").and_then(Value::as_str) == Some("text")
                || part.get("text").and_then(Value::as_str).is_some() =>
            {
                Some(
                    part.get("text")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string(),
                )
            }
            _ => None,
        },
    }
}

/// Incoming `POST /v1/chat/completions` body.
///
/// Unknown fields are ignored so clients can send the full OpenAI payload
/// while the router only consumes what it currently understands.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ChatCompletionRequest {
    pub model: String,
    pub messages: Vec<ChatMessage>,
    #[serde(default)]
    pub stream: bool,
    #[serde(default)]
    pub max_tokens: Option<u32>,
    #[serde(default)]
    pub temperature: Option<f32>,
    #[serde(default)]
    pub top_p: Option<f32>,
    #[serde(default)]
    pub tools: Option<Value>,
    #[serde(default)]
    pub tool_choice: Option<Value>,
    /// JSON-response request; translated for providers that have a native mode.
    #[serde(default)]
    pub response_format: Option<Value>,
    #[serde(default)]
    pub stop: Option<Value>,
    #[serde(default)]
    pub top_k: Option<u32>,
    #[serde(default)]
    pub max_completion_tokens: Option<u32>,
    #[serde(default)]
    pub reasoning_effort: Option<String>,
    #[serde(default)]
    pub stream_options: Option<Value>,
    #[serde(default)]
    pub parallel_tool_calls: Option<bool>,
    #[serde(default)]
    pub metadata: Option<Value>,
    #[serde(default)]
    pub service_tier: Option<String>,
    #[serde(default)]
    pub seed: Option<i64>,
    #[serde(default)]
    pub verbosity: Option<String>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
    #[serde(skip)]
    pub responses_input: Option<Value>,
}

/// One completion choice in a non-streaming response.
#[derive(Debug, Clone, Serialize)]
pub struct ChatChoice {
    pub index: u32,
    pub message: ChatMessage,
    pub finish_reason: String,
}

/// Token accounting, kept OpenAI-shaped.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct Usage {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub total_tokens: u32,
}

/// Non-streaming `POST /v1/chat/completions` response.
#[derive(Debug, Clone, Serialize)]
pub struct ChatCompletionResponse {
    pub id: String,
    pub object: &'static str,
    pub created: i64,
    pub model: String,
    pub choices: Vec<ChatChoice>,
    pub usage: Usage,
    #[serde(skip)]
    pub raw: Option<Value>,
}

/// The discounted half of a request's token bill.
///
/// [`Usage`] carries the three totals a provider always reports, so a cache
/// hit that stays hidden in them is billed as fresh input. Streaming
/// publishers only ever mention that share on the usage frame of a terminal
/// chunk, which is why this rides next to [`StreamChunk::usage`] rather than
/// inside it.
///
/// This is `heterion_router_usage::CachedUsage` mirrored into the shape
/// crate: the accounting crate depends on this one, so its type cannot be
/// named here. The gateway maps one onto the other at the accounting
/// boundary (`backend::chunk_cached_usage`), so there is still exactly one
/// parser for the upstream spellings.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct UsageDetail {
    /// Prompt tokens the upstream served out of its cache.
    pub cache_read: u32,
    /// Prompt tokens the upstream spent writing into its cache.
    pub cache_creation: u32,
    /// Reasoning/thinking tokens, counted inside the completion total.
    pub reasoning: u32,
}

/// A single streamed delta, format-agnostic.
///
/// The gateway turns these into SSE frames for the client's wire format.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct StreamChunk {
    /// Visible assistant text for this delta, if any.
    pub content: Option<String>,
    /// Reasoning/thinking text for this delta, if any.
    pub reasoning: Option<String>,
    /// OpenAI-shaped `tool_calls` delta array, forwarded verbatim.
    pub tool_calls: Option<serde_json::Value>,
    /// Set on the terminal chunk (`"stop"`, `"length"`, ...).
    pub finish_reason: Option<String>,
    pub usage: Option<Usage>,
    /// Discounted counts this delta's usage frame reported, when it reported
    /// any. Absent means the frame published nothing to discount, not zero.
    pub usage_detail: Option<UsageDetail>,
}

impl StreamChunk {
    /// A visible-text delta.
    pub fn content(text: impl Into<String>) -> Self {
        Self {
            content: Some(text.into()),
            ..Self::default()
        }
    }

    /// The terminal chunk.
    pub fn done(reason: impl Into<String>) -> Self {
        Self {
            finish_reason: Some(reason.into()),
            ..Self::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn request_ignores_unknown_fields() {
        let raw = r#"{
            "model": "grok-4.6",
            "messages": [{"role": "user", "content": "hi"}],
            "temperature": 0.2,
            "some_future_field": true
        }"#;
        let req: ChatCompletionRequest = serde_json::from_str(raw).unwrap();
        assert_eq!(req.model, "grok-4.6");
        assert_eq!(req.messages.len(), 1);
        assert!(!req.stream);
        assert_eq!(
            req.extra.get("some_future_field"),
            Some(&serde_json::json!(true))
        );
    }

    #[test]
    fn assistant_tool_call_turn_deserializes() {
        // The exact shape agents send after executing a tool: null content
        // plus tool_calls, followed by a `tool` result message.
        let raw = r#"{
            "model": "grok-4.6",
            "messages": [
                {"role": "user", "content": "time?"},
                {"role": "assistant", "content": null,
                 "tool_calls": [{"id": "call_1", "type": "function",
                                 "function": {"name": "get_time", "arguments": "{\"city\":\"SF\"}"}}]},
                {"role": "tool", "tool_call_id": "call_1", "content": "12:00"}
            ],
            "tools": [{"type": "function",
                       "function": {"name": "get_time", "parameters": {"type": "object"}}}]
        }"#;
        let req: ChatCompletionRequest = serde_json::from_str(raw).unwrap();
        assert_eq!(req.messages.len(), 3);
        assert!(req.messages[1].has_tool_calls());
        assert_eq!(req.messages[1].text(), "");
        assert_eq!(req.messages[2].tool_call_id.as_deref(), Some("call_1"));
        assert!(req.tools.is_some());
    }

    #[test]
    fn message_text_reads_string_and_parts() {
        let plain = ChatMessage {
            role: "user".to_string(),
            content: Some(Value::from("hello")),
            reasoning_content: None,
            tool_calls: None,
            tool_call_id: None,
            name: None,
        };
        assert_eq!(plain.text(), "hello");

        let parts = ChatMessage {
            role: "user".to_string(),
            content: Some(
                serde_json::json!([{"type": "text", "text": "a"}, {"type": "text", "text": "b"}]),
            ),
            reasoning_content: None,
            tool_calls: None,
            tool_call_id: None,
            name: None,
        };
        assert_eq!(parts.text(), "ab");
    }

    #[test]
    fn content_chunk_carries_only_text() {
        let chunk = StreamChunk::content("hello");
        assert_eq!(chunk.content.as_deref(), Some("hello"));
        assert!(chunk.reasoning.is_none());
        assert!(chunk.finish_reason.is_none());
    }

    #[test]
    fn a_chunk_carries_the_discounted_detail_beside_its_totals() {
        let chunk = StreamChunk {
            usage: Some(Usage {
                prompt_tokens: 1000,
                completion_tokens: 500,
                total_tokens: 1500,
            }),
            usage_detail: Some(UsageDetail {
                cache_read: 200,
                cache_creation: 0,
                reasoning: 64,
            }),
            ..StreamChunk::default()
        };
        // The totals and the detail are independent: the detail is what
        // keeps a cache hit from billing as fresh input.
        assert_eq!(chunk.usage.unwrap().prompt_tokens, 1000);
        assert_eq!(chunk.usage_detail.unwrap().cache_read, 200);
        // A delta that reported nothing to discount says so by omission.
        assert!(StreamChunk::done("stop").usage_detail.is_none());
        assert!(StreamChunk::content("hi").usage_detail.is_none());
    }

    // --- content_text / content_text_with characterization -----------------
    //
    // These pin the flattening rules the translators rely on. `TextField` is
    // the default (OpenAI/Gemini), `TypedText` is the Claude adapter and
    // `Loose` the Responses adapter; the three deliberately disagree.

    fn parts(content: &str) -> Value {
        serde_json::from_str(content).unwrap()
    }

    #[test]
    fn content_text_reads_a_plain_string_verbatim() {
        assert_eq!(content_text(Some(&Value::from("hello"))), "hello");
        // An empty string stays empty rather than becoming a fallback.
        assert_eq!(content_text(Some(&Value::from(""))), "");
    }

    #[test]
    fn content_text_joins_array_text_parts_with_no_separator() {
        let content = parts(r#"[{"type":"text","text":"a"},{"type":"text","text":"b"}]"#);
        assert_eq!(content_text(Some(&content)), "ab");
        // A `text` field counts whatever the `type` says.
        let untyped = parts(r#"[{"text":"a"}]"#);
        assert_eq!(content_text(Some(&untyped)), "a");
        // Empty parts are kept, not skipped: joining "" adds nothing.
        let empty = parts(r#"[{"type":"text","text":""},{"type":"text","text":"b"}]"#);
        assert_eq!(content_text(Some(&empty)), "b");
    }

    #[test]
    fn content_text_treats_null_missing_and_other_scalars_as_empty() {
        assert_eq!(content_text(None), "");
        assert_eq!(content_text(Some(&Value::Null)), "");
        assert_eq!(content_text(Some(&Value::Array(Vec::new()))), "");
        assert_eq!(content_text(Some(&json!({}))), "");
        assert_eq!(content_text(Some(&json!(42))), "");
        assert_eq!(content_text(Some(&json!(true))), "");
        // Explicit null entries and non-object scalars inside an array are
        // dropped, never rendered as the string "null".
        let mixed = parts(r#"[null, 42, true, {"type":"text","text":"a"}]"#);
        assert_eq!(content_text(Some(&mixed)), "a");
    }

    #[test]
    fn content_text_skips_parts_without_a_string_text_field() {
        let content = parts(
            r#"[{"type":"image_url","image_url":{"url":"u"}},
                {"type":"text"},
                {"type":"text","text":null},
                {"type":"text","text":42},
                {"nested":{"text":"deep"}},
                "bare",
                {"type":"text","text":"kept"}]"#,
        );
        assert_eq!(content_text(Some(&content)), "kept");
    }

    #[test]
    fn content_text_handles_multibyte_and_newline_text_unchanged() {
        assert_eq!(content_text(Some(&Value::from("héllo 🌍"))), "héllo 🌍");
        let content = parts(r#"[{"type":"text","text":"日本語"},{"type":"text","text":"🌏"}]"#);
        assert_eq!(content_text(Some(&content)), "日本語🌏");
        // Newlines survive; only the separator is controlled.
        assert_eq!(content_text(Some(&Value::from("a\nb"))), "a\nb");
        let spaced = parts(r#"[{"type":"text","text":"a\nb"},{"type":"text","text":"c"}]"#);
        assert_eq!(content_text(Some(&spaced)), "a\nbc");
    }

    #[test]
    fn typed_text_joins_with_newline_and_renders_non_string_text() {
        let join = TextJoin::new(TextParts::TypedText, "\n", false);
        let content = parts(r#"[{"type":"text","text":"a"},{"type":"text","text":"b"}]"#);
        assert_eq!(content_text_with(Some(&content), join), "a\nb");
        // Only `type: "text"` parts count, unlike the default.
        let untyped = parts(r#"[{"text":"a"}]"#);
        assert_eq!(content_text_with(Some(&untyped), join), "");
        // A part tagged "text" is kept whatever `text` holds.
        let scalars = parts(r#"[{"type":"text","text":42},{"type":"text","text":true}]"#);
        assert_eq!(content_text_with(Some(&scalars), join), "42\ntrue");
        let missing = parts(r#"[{"type":"text"}]"#);
        assert_eq!(content_text_with(Some(&missing), join), "");
        assert_eq!(content_text_with(None, join), "");
        assert_eq!(content_text_with(Some(&Value::from("hi")), join), "hi");
    }

    #[test]
    fn loose_keeps_bare_strings_and_drops_empty_parts() {
        let join = TextJoin::new(TextParts::Loose, "\n\n", true);
        let content = parts(r#"["a", {"type":"text","text":"b"}, {"text":"c"}]"#);
        assert_eq!(content_text_with(Some(&content), join), "a\n\nb\n\nc");
        // Empties are dropped rather than producing stray separators.
        let empties = parts(r#"["", {"type":"text"}, {"type":"text","text":"x"}]"#);
        assert_eq!(content_text_with(Some(&empties), join), "x");
        assert_eq!(content_text_with(Some(&Value::Array(Vec::new())), join), "");
        assert_eq!(content_text_with(Some(&Value::Null), join), "");
        // Non-text parts stay out even though they carry no `text`.
        let binary = parts(r#"[{"type":"image_url","image_url":{"url":"u"}},"a"]"#);
        assert_eq!(content_text_with(Some(&binary), join), "a");
        assert_eq!(
            content_text_with(Some(&parts(r#"[{"text":"日本 🌍"}]"#)), join),
            "日本 🌍"
        );
    }

    #[test]
    fn chat_message_text_uses_the_canonical_helper() {
        let message = ChatMessage {
            role: "user".to_string(),
            content: Some(parts(
                r#"[{"type":"text","text":"a"},{"type":"text","text":42}]"#,
            )),
            reasoning_content: None,
            tool_calls: None,
            tool_call_id: None,
            name: None,
        };
        assert_eq!(message.text(), "a");
        assert_eq!(
            ChatMessage::plain("user", "hi").text(),
            content_text(Some(&Value::from("hi")))
        );
    }
}

//! OpenAI chat → Claude Messages request conversion.
//!
//! Mirrors `openaiToClaudeRequest` in
//! `open-sse/translator/request/openai-to-claude.ts` for plain text:
//! system/developer hoisting with cache control, role merging, text blocks,
//! sampling params, stop sequences and the empty-messages guard — plus the
//! tool surface agentic clients require: `tools` → `input_schema`,
//! `tool_choice`, assistant `tool_calls` → `tool_use` blocks, and `tool`
//! role turns → `tool_result` blocks.
//!
//! Still deferred: thinking/reasoning configuration, images/files,
//! `body.system` merging, cache_control beyond the two prescribed spots,
//! and Kimi-specific paths.

use heterion_router_core::{TextJoin, TextParts, content_text_with};
use serde_json::{Map, Value, json};

const DEFAULT_MAX_TOKENS: u32 = 64000;
const DEFAULT_MIN_TOKENS: u32 = 32000;

/// Whether the model forces extended thinking server-side, in which case
/// `temperature` must be stripped (mirrors the JS `/claude-(?:opus|sonnet)-4/i`
/// test).
fn model_forces_thinking(model: &str) -> bool {
    let lower = model.to_lowercase();
    lower.contains("claude-opus-4") || lower.contains("claude-sonnet-4")
}

/// `adjustMaxTokens`: `max_tokens ?? max_completion_tokens`, defaulted,
/// raised for tool calls, clamped to at least 1.
fn adjust_max_tokens(body: &Value, has_tools: bool) -> u32 {
    let requested = body
        .get("max_tokens")
        .or_else(|| body.get("max_completion_tokens"))
        .and_then(Value::as_u64)
        .unwrap_or(u64::from(DEFAULT_MAX_TOKENS)) as u32;
    let mut max_tokens = if requested == 0 {
        DEFAULT_MAX_TOKENS
    } else {
        requested
    };
    if has_tools && max_tokens < DEFAULT_MIN_TOKENS {
        max_tokens = DEFAULT_MIN_TOKENS;
    }
    max_tokens.max(1)
}

/// `normalizeContentToString` flattening rules: `type: "text"` parts joined
/// with a newline, a non-string `text` rendered in its JSON form.
const NORMALIZED: TextJoin = TextJoin::new(TextParts::TypedText, "\n", false);

/// Message content → Claude text blocks. Empty strings and empty text parts
/// are dropped, matching the JS truthiness checks.
fn text_blocks(content: Option<&Value>) -> Vec<Value> {
    match content {
        Some(Value::String(text)) if !text.is_empty() => {
            vec![json!({"type": "text", "text": text})]
        }
        Some(Value::Array(parts)) => parts
            .iter()
            .filter(|part| {
                part.get("type").and_then(Value::as_str) == Some("text")
                    && part
                        .get("text")
                        .and_then(Value::as_str)
                        .is_some_and(|text| !text.is_empty())
            })
            .map(|part| {
                json!({
                    "type": "text",
                    "text": part.get("text").and_then(Value::as_str).unwrap_or(""),
                })
            })
            .collect(),
        _ => Vec::new(),
    }
}

/// Flush the accumulated turn into `messages` when non-empty.
fn flush_turn(messages: &mut Vec<Value>, role: &mut Option<String>, blocks: &mut Vec<Value>) {
    if let Some(role) = role.take()
        && !blocks.is_empty()
    {
        messages.push(json!({
            "role": role,
            "content": std::mem::take(blocks),
        }));
    }
}

/// OpenAI `tools` entries → Claude tool definitions.
///
/// Only `type: "function"` entries map; anything else is skipped rather
/// than sent as a malformed definition.
fn claude_tools(tools: Option<&Value>) -> Option<Vec<Value>> {
    let tools = tools?.as_array()?;
    let mapped: Vec<Value> = tools
        .iter()
        .filter(|tool| tool.get("type").and_then(Value::as_str) == Some("function"))
        .filter_map(|tool| {
            let function = tool.get("function")?;
            let name = function.get("name")?.as_str()?.to_string();
            Some(json!({
                "name": name,
                "description": function.get("description").cloned().unwrap_or(Value::Null),
                "input_schema": function
                    .get("parameters")
                    .cloned()
                    .unwrap_or_else(|| json!({"type": "object"})),
            }))
        })
        .collect();
    (!mapped.is_empty()).then_some(mapped)
}

/// OpenAI `tool_choice` → Claude `tool_choice`.
fn claude_tool_choice(choice: Option<&Value>) -> Option<Value> {
    match choice? {
        Value::String(kind) => match kind.as_str() {
            "auto" => Some(json!({"type": "auto"})),
            "required" => Some(json!({"type": "any"})),
            "none" => Some(json!({"type": "none"})),
            _ => None,
        },
        Value::Object(_) => {
            let name = choice
                .and_then(|choice| choice.get("function"))
                .and_then(|function| function.get("name"))
                .and_then(Value::as_str)?;
            Some(json!({"type": "tool", "name": name}))
        }
        _ => None,
    }
}

/// Assistant `tool_calls` → Claude `tool_use` blocks.
///
/// OpenAI carries arguments as a JSON *string*; Claude wants an *object*,
/// so the string is parsed (a non-JSON body degrades to `{}` rather than
/// corrupting the turn).
fn tool_use_blocks(message: &Value) -> Vec<Value> {
    message
        .get("tool_calls")
        .and_then(Value::as_array)
        .map(|calls| {
            calls
                .iter()
                .map(|call| {
                    let arguments = call
                        .pointer("/function/arguments")
                        .and_then(Value::as_str)
                        .unwrap_or("{}");
                    let input: Value =
                        serde_json::from_str(arguments).unwrap_or_else(|_| json!({}));
                    json!({
                        "type": "tool_use",
                        "id": call.get("id").cloned().unwrap_or(Value::Null),
                        "name": call.pointer("/function/name").cloned().unwrap_or(Value::Null),
                        "input": input,
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// A `tool`-role message → one Claude `tool_result` block.
fn tool_result_block(message: &Value) -> Value {
    let text = content_text_with(message.get("content"), NORMALIZED);
    json!({
        "type": "tool_result",
        "tool_use_id": message.get("tool_call_id").cloned().unwrap_or(Value::Null),
        "content": text,
    })
}

/// Convert an OpenAI chat request body to a Claude Messages request body.
///
/// `model` is stamped through and `stream` passed through. Tool-carrying,
/// thinking and multimodal inputs are deferred (see module docs).
pub fn openai_to_claude(model: &str, body: &Value, stream: bool) -> Value {
    let has_tools = body
        .get("tools")
        .and_then(Value::as_array)
        .is_some_and(|tools| !tools.is_empty());

    let mut result = Map::new();
    result.insert("model".to_string(), Value::from(model));
    result.insert(
        "max_tokens".to_string(),
        json!(adjust_max_tokens(body, has_tools)),
    );
    result.insert("stream".to_string(), Value::from(stream));

    if let Some(temperature) = body.get("temperature")
        && !model_forces_thinking(model)
    {
        result.insert("temperature".to_string(), temperature.clone());
    } else if body.get("temperature").is_none()
        && let Some(top_p) = body.get("top_p")
    {
        result.insert("top_p".to_string(), top_p.clone());
    }
    if let Some(stop) = body.get("stop") {
        let sequences = match stop {
            Value::Array(items) => items.clone(),
            _ => vec![stop.clone()],
        };
        result.insert("stop_sequences".to_string(), Value::Array(sequences));
    }

    if let Some(tools) = claude_tools(body.get("tools")) {
        result.insert("tools".to_string(), Value::Array(tools));
        if let Some(choice) = claude_tool_choice(body.get("tool_choice")) {
            result.insert("tool_choice".to_string(), choice);
        }
    }

    let mut system_parts: Vec<String> = Vec::new();
    let mut messages: Vec<Value> = Vec::new();
    let mut current_role: Option<String> = None;
    let mut current_blocks: Vec<Value> = Vec::new();

    if let Some(list) = body.get("messages").and_then(Value::as_array) {
        for message in list {
            let role = message.get("role").and_then(Value::as_str).unwrap_or("");
            if role == "system" || role == "developer" {
                system_parts.push(content_text_with(message.get("content"), NORMALIZED));
                continue;
            }

            // A tool result lands in a user turn, and Claude wants
            // tool_result blocks to follow the assistant tool_use turn
            // directly — never merged behind stray user text.
            if role == "tool" {
                if current_role.as_deref() != Some("user")
                    || current_blocks
                        .last()
                        .and_then(|block| block.get("type"))
                        .and_then(Value::as_str)
                        == Some("text")
                {
                    flush_turn(&mut messages, &mut current_role, &mut current_blocks);
                    current_role = Some("user".to_string());
                }
                current_blocks.push(tool_result_block(message));
                continue;
            }

            let new_role = if role == "user" { "user" } else { "assistant" }.to_string();
            let mut blocks = text_blocks(message.get("content"));
            if new_role == "assistant" {
                blocks.extend(tool_use_blocks(message));
            }

            if current_role.as_deref() != Some(new_role.as_str()) {
                flush_turn(&mut messages, &mut current_role, &mut current_blocks);
                current_role = Some(new_role);
            }
            current_blocks.extend(blocks);
        }
    }
    flush_turn(&mut messages, &mut current_role, &mut current_blocks);

    // Drop assistant messages left with empty content.
    messages.retain(|message| {
        if message.get("role").and_then(Value::as_str) != Some("assistant") {
            return true;
        }
        message
            .get("content")
            .and_then(Value::as_array)
            .is_some_and(|content| !content.is_empty())
    });

    // Cache the last block of the last assistant message.
    for message in messages.iter_mut().rev() {
        if message.get("role").and_then(Value::as_str) != Some("assistant") {
            continue;
        }
        if let Some(blocks) = message.get_mut("content").and_then(Value::as_array_mut)
            && let Some(last) = blocks.last_mut()
        {
            if let Some(object) = last.as_object_mut() {
                object.insert("cache_control".to_string(), json!({"type": "ephemeral"}));
            }
            break;
        }
    }

    if !system_parts.is_empty() {
        let system_block = json!({
            "type": "text",
            "text": system_parts.join("\n"),
            "cache_control": {"type": "ephemeral", "ttl": "1h"},
        });
        // body.system merging is deferred with the multimodal paths; the
        // fixtures only exercise role="system" messages.
        result.insert("system".to_string(), Value::Array(vec![system_block]));
    }

    if messages.is_empty() {
        messages.push(json!({
            "role": "user",
            "content": [{"type": "text", "text": "."}],
        }));
    }
    result.insert("messages".to_string(), Value::Array(messages));

    Value::Object(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn max_tokens_defaults_and_floors() {
        assert_eq!(adjust_max_tokens(&json!({}), false), 64000);
        assert_eq!(adjust_max_tokens(&json!({"max_tokens": 0}), false), 64000);
        assert_eq!(
            adjust_max_tokens(&json!({"max_completion_tokens": 50}), false),
            50
        );
        assert_eq!(adjust_max_tokens(&json!({"max_tokens": 100}), true), 32000);
    }

    #[test]
    fn thinking_models_match() {
        assert!(model_forces_thinking("claude-sonnet-4-6"));
        assert!(model_forces_thinking("CLAUDE-OPUS-4-1"));
        assert!(!model_forces_thinking("claude-haiku-4-5"));
        assert!(!model_forces_thinking("gpt-4o"));
    }

    #[test]
    fn tools_map_to_input_schema_and_tool_choice_variants() {
        let got = openai_to_claude(
            "glm-5.3-flash",
            &json!({
                "messages": [{"role": "user", "content": "hi"}],
                "tools": [
                    {"type": "function", "function": {"name": "get_weather",
                     "description": "weather", "parameters": {"type": "object", "properties": {"city": {"type": "string"}}}}},
                    {"type": "web_search", "function": {"name": "ignored"}},
                ],
                "tool_choice": "auto",
            }),
            false,
        );
        let tools = got["tools"].as_array().expect("tools");
        assert_eq!(tools.len(), 1, "non-function tools are skipped");
        assert_eq!(tools[0]["name"], json!("get_weather"));
        assert_eq!(tools[0]["input_schema"]["type"], json!("object"));
        assert_eq!(got["tool_choice"], json!({"type": "auto"}));
        // The 32K floor only raises smaller values; an unset max_tokens
        // keeps the 64K default.
        assert_eq!(got["max_tokens"], json!(64000));

        let named = openai_to_claude(
            "glm-5.3-flash",
            &json!({
                "messages": [{"role": "user", "content": "hi"}],
                "tools": [{"type": "function", "function": {"name": "f", "parameters": {}}}],
                "tool_choice": {"type": "function", "function": {"name": "f"}},
            }),
            false,
        );
        assert_eq!(named["tool_choice"], json!({"type": "tool", "name": "f"}));

        let required = openai_to_claude(
            "glm-5.3-flash",
            &json!({
                "messages": [{"role": "user", "content": "hi"}],
                "tools": [{"type": "function", "function": {"name": "f", "parameters": {}}}],
                "tool_choice": "required",
            }),
            false,
        );
        assert_eq!(required["tool_choice"], json!({"type": "any"}));
    }

    #[test]
    fn a_full_tool_round_trip_reaches_claude_shape() {
        let got = openai_to_claude(
            "glm-5.3-flash",
            &json!({
                "messages": [
                    {"role": "user", "content": "weather in paris?"},
                    {"role": "assistant", "content": null, "tool_calls": [
                        {"id": "call_1", "type": "function",
                         "function": {"name": "get_weather", "arguments": "{\"city\":\"Paris\"}"}}
                    ]},
                    {"role": "tool", "tool_call_id": "call_1", "content": "22C sunny"},
                    {"role": "user", "content": "thanks"},
                ],
                "tools": [{"type": "function", "function": {"name": "get_weather", "parameters": {}}}],
            }),
            false,
        );
        let messages = got["messages"].as_array().expect("messages");
        // user → assistant(tool_use) → user(tool_result + merged text):
        // consecutive user-role turns merge, tool_result first (valid Claude).
        assert_eq!(messages.len(), 3, "got {messages:?}");
        let tool_use = &messages[1]["content"][0];
        assert_eq!(tool_use["type"], json!("tool_use"));
        assert_eq!(tool_use["id"], json!("call_1"));
        assert_eq!(tool_use["name"], json!("get_weather"));
        assert_eq!(tool_use["input"], json!({"city": "Paris"}));
        let tool_result = &messages[2]["content"][0];
        assert_eq!(tool_result["type"], json!("tool_result"));
        assert_eq!(tool_result["tool_use_id"], json!("call_1"));
        assert_eq!(tool_result["content"], json!("22C sunny"));
        assert_eq!(messages[2]["content"][1]["text"], json!("thanks"));
    }

    #[test]
    fn malformed_tool_arguments_degrade_to_an_empty_object() {
        let got = openai_to_claude(
            "glm-5.3-flash",
            &json!({
                "messages": [
                    {"role": "assistant", "tool_calls": [
                        {"id": "call_1", "type": "function",
                         "function": {"name": "f", "arguments": "not json"}}
                    ]},
                ],
            }),
            false,
        );
        let tool_use = &got["messages"][0]["content"][0];
        assert_eq!(tool_use["type"], json!("tool_use"));
        assert_eq!(tool_use["input"], json!({}));
    }

    #[test]
    fn an_assistant_turn_with_only_tool_calls_is_kept() {
        let got = openai_to_claude(
            "glm-5.3-flash",
            &json!({
                "messages": [
                    {"role": "user", "content": "go"},
                    {"role": "assistant", "content": null, "tool_calls": [
                        {"id": "c1", "type": "function", "function": {"name": "f", "arguments": "{}"}}
                    ]},
                ],
            }),
            false,
        );
        let messages = got["messages"].as_array().expect("messages");
        assert_eq!(
            messages.len(),
            2,
            "tool_use-only assistant turn must survive"
        );
        assert_eq!(messages[1]["content"][0]["type"], json!("tool_use"));
    }
}

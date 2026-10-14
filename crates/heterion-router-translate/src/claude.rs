//! Streaming `claude → openai` conversion.
//!
//! Mirrors `claudeToOpenAIResponse` in
//! `open-sse/translator/response/claude-to-openai.ts` for plain text:
//! `message_start` announces the role, `content_block_delta` text/thinking
//! deltas become content/`reasoning_content`, and `message_delta` usage plus
//! `message_stop` close the stream with a finish chunk.
//!
//! Tool calls are translated: a `tool_use` content block accumulates its
//! `input_json_delta` fragments and is emitted as an OpenAI `tool_calls`
//! delta when the block stops — without this, agentic clients see a
//! `tool_calls` finish reason with nothing to execute and stall.
//!
//! Still deferred: `<think>` tag parsing, `pendingThinkClose` markers, and
//! signature handling. Unknown event kinds are ignored, never fabricated.

use crate::chunk::{
    ROLE_ASSISTANT, attach_usage, cached_prompt_details, chatcmpl_id, chunk, chunk_model,
    insert_text, stream_chunk, usage_root,
};
use serde_json::{Map, Value, json};

/// Mutable per-stream conversion state.
#[derive(Debug, Clone, Default)]
pub struct ClaudeState {
    /// Claude `message.id`, stamped as `chatcmpl-{id}`.
    pub message_id: Option<String>,
    /// Model name from `message_start`.
    pub model: Option<String>,
    /// Whether the terminal finish chunk already went out.
    pub finish_sent: bool,
    /// Finish reason captured from a `stop_reason`, if any.
    pub finish_reason: Option<String>,
    /// Accumulated token accounting.
    pub usage: Option<ClaudeUsage>,
    /// One accumulator per `tool_use` block seen so far, keyed by the
    /// Claude content-block index.
    pub tools: Vec<ToolCallAcc>,
    /// OpenAI `tool_calls[].index` for the next synthesized call.
    pub next_tool_index: usize,
}

/// One accumulating `tool_use` block: id/name from `content_block_start`,
/// arguments assembled from `input_json_delta` fragments.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ToolCallAcc {
    /// Claude content-block index this accumulator belongs to.
    pub block_index: i64,
    /// `tool_use.id`, stamped verbatim into the OpenAI delta.
    pub id: String,
    /// `tool_use.name`.
    pub name: String,
    /// Assembled JSON string from `input_json_delta` fragments.
    pub arguments: String,
}

/// Token accounting in Claude-native fields.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ClaudeUsage {
    pub input: u32,
    pub output: u32,
    pub cache_read: u32,
    pub cache_creation: u32,
    pub reasoning: Option<u32>,
}

/// Outcome of converting one upstream event.
#[derive(Debug, Clone, PartialEq)]
pub enum Converted {
    /// A `chat.completion.chunk` object to emit to the client.
    Emit(Value),
    /// The event carries nothing for an OpenAI client on its own.
    Ignore,
}

/// Convert one Claude SSE event to an OpenAI chunk.
///
/// `event_type` is the SSE `type` field, `data` the full event object, and
/// `now` the Unix timestamp stamped as `created` (injected for deterministic
/// tests; production passes wall time).
pub fn convert_event(
    state: &mut ClaudeState,
    event_type: &str,
    data: &Value,
    now: i64,
) -> Converted {
    match event_type {
        "message_start" => {
            let message = data.get("message");
            state.message_id = message
                .and_then(|message| message.get("id"))
                .and_then(Value::as_str)
                .map(str::to_string)
                .or_else(|| Some(format!("msg_{now}")));
            state.model = message
                .and_then(|message| message.get("model"))
                .and_then(Value::as_str)
                .map(str::to_string);
            if let Some(usage) = message.and_then(|message| message.get("usage")) {
                let input = first_number(usage, &["input_tokens", "prompt_tokens"]);
                let output = first_number(usage, &["output_tokens", "completion_tokens"]);
                let cache_read = first_number(usage, &["cache_read_input_tokens"]);
                let cache_creation = first_number(usage, &["cache_creation_input_tokens"]);
                if input > 0 || output > 0 || cache_read > 0 || cache_creation > 0 {
                    state.usage = Some(ClaudeUsage {
                        input,
                        output,
                        cache_read,
                        cache_creation,
                        reasoning: None,
                    });
                }
            }
            Converted::Emit(role_chunk(state, now))
        }
        "content_block_start" => {
            let block = data.get("content_block");
            if block
                .and_then(|block| block.get("type"))
                .and_then(Value::as_str)
                != Some("tool_use")
            {
                return Converted::Ignore;
            }
            state.tools.push(ToolCallAcc {
                block_index: data.get("index").and_then(Value::as_i64).unwrap_or(-1),
                id: block
                    .and_then(|block| block.get("id"))
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                name: block
                    .and_then(|block| block.get("name"))
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                arguments: String::new(),
            });
            Converted::Ignore
        }
        "content_block_stop" => {
            let index = data.get("index").and_then(Value::as_i64).unwrap_or(-1);
            let Some(position) = state.tools.iter().position(|acc| acc.block_index == index) else {
                return Converted::Ignore;
            };
            let acc = state.tools.remove(position);
            let tool_index = state.next_tool_index;
            state.next_tool_index += 1;
            Converted::Emit(tool_call_chunk(state, now, tool_index, &acc))
        }
        "content_block_delta" => {
            let delta = data.get("delta");
            let kind = delta
                .and_then(|delta| delta.get("type"))
                .and_then(Value::as_str)
                .unwrap_or("");
            match kind {
                "text_delta" => {
                    let text = delta
                        .and_then(|delta| delta.get("text"))
                        .and_then(Value::as_str)
                        .unwrap_or("");
                    if text.is_empty() {
                        return Converted::Ignore;
                    }
                    Converted::Emit(delta_chunk(state, now, Some(text), None))
                }
                "thinking_delta" => {
                    let text = delta
                        .and_then(|delta| delta.get("thinking"))
                        .and_then(Value::as_str)
                        .unwrap_or("");
                    if text.is_empty() {
                        return Converted::Ignore;
                    }
                    Converted::Emit(delta_chunk(state, now, None, Some(text)))
                }
                "input_json_delta" => {
                    let fragment = delta
                        .and_then(|delta| delta.get("partial_json"))
                        .and_then(Value::as_str)
                        .unwrap_or("");
                    if fragment.is_empty() {
                        return Converted::Ignore;
                    }
                    // Append to the most recently started tool block; Claude
                    // deltas carry the block index too, so honour it when the
                    // event provides one.
                    let index = data.get("index").and_then(Value::as_i64);
                    let target = match index {
                        Some(index) => state
                            .tools
                            .iter_mut()
                            .rev()
                            .find(|acc| acc.block_index == index),
                        None => state.tools.last_mut(),
                    };
                    match target {
                        Some(acc) => {
                            acc.arguments.push_str(fragment);
                            Converted::Ignore
                        }
                        // A fragment without a started block is dropped, not
                        // fabricated into a nameless tool call.
                        None => Converted::Ignore,
                    }
                }
                _ => Converted::Ignore,
            }
        }
        "message_delta" => {
            if let Some(usage) = data.get("usage") {
                merge_usage(state, usage);
            }
            let stop_reason = data
                .get("delta")
                .and_then(|delta| delta.get("stop_reason"))
                .and_then(Value::as_str);
            match stop_reason {
                Some(reason) => {
                    state.finish_reason = Some(convert_stop_reason(reason));
                    state.finish_sent = true;
                    Converted::Emit(finish_chunk(state, now))
                }
                None => Converted::Ignore,
            }
        }
        "message_stop" => {
            if state.finish_sent {
                return Converted::Ignore;
            }
            state.finish_sent = true;
            Converted::Emit(finish_chunk(state, now))
        }
        _ => Converted::Ignore,
    }
}

/// Map a Claude `stop_reason` to an OpenAI `finish_reason`.
fn convert_stop_reason(reason: &str) -> String {
    match reason {
        "end_turn" | "stop_sequence" => "stop",
        "max_tokens" => "length",
        "tool_use" => "tool_calls",
        _ => "stop",
    }
    .to_string()
}

fn first_number(value: &Value, keys: &[&str]) -> u32 {
    keys.iter()
        .filter_map(|key| value.get(*key))
        .filter_map(Value::as_u64)
        .find(|value| *value != 0)
        .unwrap_or(0) as u32
}

/// Merge a `message_delta` usage block into the accumulated accounting,
/// mirroring the JS billable-input rule (new input wins when present,
/// otherwise the previous total stands; cache fields persist).
fn merge_usage(state: &mut ClaudeState, usage: &Value) {
    let previous = state.usage.unwrap_or_default();
    let input = usage
        .get("input_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(0) as u32;
    let output = usage
        .get("output_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(0) as u32;
    let cache_read = usage
        .get("cache_read_input_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(0) as u32;
    let cache_creation = usage
        .get("cache_creation_input_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(0) as u32;
    let reasoning = usage
        .get("output_tokens_details")
        .and_then(|details| details.get("thinking_tokens"))
        .and_then(Value::as_u64)
        .map(|value| value as u32);

    let billable = if input > 0 || cache_read > 0 || cache_creation > 0 {
        input + cache_read
    } else {
        previous.input
    };
    state.usage = Some(ClaudeUsage {
        input: billable,
        output,
        cache_read: if cache_read > 0 {
            cache_read
        } else {
            previous.cache_read
        },
        cache_creation: if cache_creation > 0 {
            cache_creation
        } else {
            previous.cache_creation
        },
        reasoning: reasoning.or(previous.reasoning),
    });
}

fn role_chunk(state: &ClaudeState, now: i64) -> Value {
    let mut delta = Map::new();
    delta.insert("role".to_string(), Value::from(ROLE_ASSISTANT));
    stream_chunk(
        &chatcmpl_id(state.message_id.as_deref()),
        now,
        &chunk_model(state.model.as_deref()),
        Value::Object(delta),
    )
}

fn delta_chunk(
    state: &ClaudeState,
    now: i64,
    content: Option<&str>,
    reasoning: Option<&str>,
) -> Value {
    let mut delta = Map::new();
    insert_text(&mut delta, content, reasoning);
    stream_chunk(
        &chatcmpl_id(state.message_id.as_deref()),
        now,
        &chunk_model(state.model.as_deref()),
        Value::Object(delta),
    )
}

/// Emit one completed tool call as an OpenAI `tool_calls` delta.
///
/// The assembled arguments go out in a single delta when the Claude block
/// stops — OpenAI clients accept a full-arguments delta and it keeps the
/// synthesizer stateless beyond the accumulator itself.
fn tool_call_chunk(state: &ClaudeState, now: i64, tool_index: usize, acc: &ToolCallAcc) -> Value {
    stream_chunk(
        &chatcmpl_id(state.message_id.as_deref()),
        now,
        &chunk_model(state.model.as_deref()),
        json!({
            "tool_calls": [{
                "index": tool_index,
                "type": "function",
                "id": acc.id,
                "function": {
                    "name": acc.name,
                    "arguments": acc.arguments,
                },
            }],
        }),
    )
}

fn finish_chunk(state: &ClaudeState, now: i64) -> Value {
    let reason = state
        .finish_reason
        .clone()
        .unwrap_or_else(|| "stop".to_string());
    let usage = state.usage.map(|usage| {
        let mut root = usage_root(usage.input, usage.output, usage.input + usage.output);
        if let Some(reasoning) = usage.reasoning {
            root.insert("reasoning_tokens".to_string(), json!(reasoning));
            root.insert(
                "completion_tokens_details".to_string(),
                json!({ "reasoning_tokens": reasoning }),
            );
        }
        if let Some(details) = cached_prompt_details(usage.cache_read, usage.cache_creation) {
            root.insert("prompt_tokens_details".to_string(), details);
        }
        Value::Object(root)
    });
    attach_usage(
        chunk(
            &chatcmpl_id(state.message_id.as_deref()),
            now,
            &chunk_model(state.model.as_deref()),
            json!({}),
            json!(reason),
        ),
        usage,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const NOW: i64 = 1700000000;

    fn started() -> ClaudeState {
        let mut state = ClaudeState::default();
        convert_event(
            &mut state,
            "message_start",
            &json!({"message": {"id": "msg_1", "model": "m"}}),
            NOW,
        );
        state
    }

    #[test]
    fn empty_text_delta_is_ignored() {
        let mut state = started();
        assert_eq!(
            convert_event(
                &mut state,
                "content_block_delta",
                &json!({"delta": {"type": "text_delta", "text": ""}}),
                NOW,
            ),
            Converted::Ignore
        );
    }

    #[test]
    fn stop_reasons_map() {
        assert_eq!(convert_stop_reason("end_turn"), "stop");
        assert_eq!(convert_stop_reason("max_tokens"), "length");
        assert_eq!(convert_stop_reason("tool_use"), "tool_calls");
        assert_eq!(convert_stop_reason("stop_sequence"), "stop");
        assert_eq!(convert_stop_reason("weird"), "stop");
    }

    #[test]
    fn second_message_stop_is_ignored() {
        let mut state = started();
        assert!(matches!(
            convert_event(&mut state, "message_stop", &json!({}), NOW),
            Converted::Emit(_)
        ));
        assert_eq!(
            convert_event(&mut state, "message_stop", &json!({}), NOW),
            Converted::Ignore
        );
    }

    #[test]
    fn tool_use_blocks_become_tool_call_deltas() {
        let mut state = started();
        // A text block streams through untouched.
        convert_event(
            &mut state,
            "content_block_start",
            &json!({"index": 0, "content_block": {"type": "text", "text": ""}}),
            NOW,
        );
        assert!(matches!(
            convert_event(
                &mut state,
                "content_block_delta",
                &json!({"index": 0, "delta": {"type": "text_delta", "text": "calling"}}),
                NOW,
            ),
            Converted::Emit(_)
        ));
        assert_eq!(
            convert_event(&mut state, "content_block_stop", &json!({"index": 0}), NOW,),
            Converted::Ignore
        );

        // The tool_use block accumulates fragments and emits on stop.
        assert_eq!(
            convert_event(
                &mut state,
                "content_block_start",
                &json!({"index": 1, "content_block": {"type": "tool_use", "id": "tu_1", "name": "search"}}),
                NOW,
            ),
            Converted::Ignore
        );
        assert_eq!(
            convert_event(
                &mut state,
                "content_block_delta",
                &json!({"index": 1, "delta": {"type": "input_json_delta", "partial_json": "{\"q\":"}}),
                NOW,
            ),
            Converted::Ignore
        );
        convert_event(
            &mut state,
            "content_block_delta",
            &json!({"index": 1, "delta": {"type": "input_json_delta", "partial_json": "\"rust\"}"}}),
            NOW,
        );
        let Converted::Emit(chunk) =
            convert_event(&mut state, "content_block_stop", &json!({"index": 1}), NOW)
        else {
            panic!("tool block stop must emit a tool_calls delta");
        };
        let calls = chunk["choices"][0]["delta"]["tool_calls"]
            .as_array()
            .expect("tool_calls array");
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0]["index"], json!(0));
        assert_eq!(calls[0]["type"], json!("function"));
        assert_eq!(calls[0]["id"], json!("tu_1"));
        assert_eq!(calls[0]["function"]["name"], json!("search"));
        assert_eq!(calls[0]["function"]["arguments"], json!("{\"q\":\"rust\"}"));

        // The terminal stop_reason maps to tool_calls.
        let Converted::Emit(finish) = convert_event(
            &mut state,
            "message_delta",
            &json!({"delta": {"stop_reason": "tool_use"}, "usage": {"output_tokens": 5}}),
            NOW,
        ) else {
            panic!("message_delta with stop_reason must emit a finish chunk");
        };
        assert_eq!(finish["choices"][0]["finish_reason"], json!("tool_calls"));
    }

    #[test]
    fn two_tool_calls_get_sequential_openai_indexes() {
        let mut state = started();
        let calls = [("tu_a", "alpha"), ("tu_b", "beta")];
        for (index, (id, name)) in calls.iter().enumerate() {
            convert_event(
                &mut state,
                "content_block_start",
                &json!({"index": index, "content_block": {"type": "tool_use", "id": id, "name": name}}),
                NOW,
            );
            convert_event(
                &mut state,
                "content_block_delta",
                &json!({"index": index, "delta": {"type": "input_json_delta", "partial_json": "{}"}}),
                NOW,
            );
            let Converted::Emit(chunk) = convert_event(
                &mut state,
                "content_block_stop",
                &json!({"index": index}),
                NOW,
            ) else {
                panic!("block stop must emit");
            };
            assert_eq!(
                chunk["choices"][0]["delta"]["tool_calls"][0]["index"],
                json!(index)
            );
            assert_eq!(
                chunk["choices"][0]["delta"]["tool_calls"][0]["id"],
                json!(id)
            );
        }
    }

    #[test]
    fn a_json_fragment_without_a_started_block_is_dropped() {
        let mut state = started();
        assert_eq!(
            convert_event(
                &mut state,
                "content_block_delta",
                &json!({"index": 9, "delta": {"type": "input_json_delta", "partial_json": "{}"}}),
                NOW,
            ),
            Converted::Ignore
        );
        assert!(state.tools.is_empty());
        assert_eq!(state.next_tool_index, 0);
    }
}

//! Characterization tests: the exact `chat.completion.chunk` JSON emitted by
//! every OpenAI-facing streaming adapter (`claude`, `gemini`, `responses`).
//!
//! These pin the wire shape field by field — envelope (`id`/`object`/`created`/
//! `model`), the `choices[0]` triple (`index`/`delta`/`finish_reason`) and the
//! `usage` attachment — so a refactor of the shared chunk builder cannot change
//! emitted bytes unnoticed. They drive the public `convert_event` API, i.e. what
//! actually reaches the client, not the private builders.

use heterion_router_translate::claude::{ClaudeState, Converted as ClaudeConverted};
use heterion_router_translate::gemini::GeminiState;
use heterion_router_translate::responses::{Converted, ResponsesState};
use serde_json::{Value, json};

const NOW: i64 = 1700000000;

fn claude_started() -> ClaudeState {
    let mut state = ClaudeState::default();
    let emitted = heterion_router_translate::claude::convert_event(
        &mut state,
        "message_start",
        &json!({"message": {"id": "msg_1", "model": "m"}}),
        NOW,
    );
    assert!(matches!(emitted, ClaudeConverted::Emit(_)));
    state
}

fn claude_emit(state: &mut ClaudeState, event_type: &str, data: &Value) -> Value {
    match heterion_router_translate::claude::convert_event(state, event_type, data, NOW) {
        ClaudeConverted::Emit(value) => value,
        other => panic!("expected a chunk, got {other:?}"),
    }
}

fn gemini_primed() -> GeminiState {
    GeminiState {
        message_id: Some("resp_1".to_string()),
        model: Some("gemini-3-pro".to_string()),
        ..Default::default()
    }
}

// ---------------------------------------------------------------------------
// claude → openai
// ---------------------------------------------------------------------------

#[test]
fn claude_text_delta_chunk_shape() {
    let mut state = claude_started();
    let chunk = claude_emit(
        &mut state,
        "content_block_delta",
        &json!({"delta": {"type": "text_delta", "text": "Hi"}}),
    );
    assert_eq!(
        chunk,
        json!({
            "id": "chatcmpl-msg_1",
            "object": "chat.completion.chunk",
            "created": NOW,
            "model": "m",
            "choices": [{
                "index": 0,
                "delta": {"content": "Hi"},
                "finish_reason": null,
            }],
        })
    );
}

#[test]
fn claude_thinking_delta_chunk_shape() {
    let mut state = claude_started();
    let chunk = claude_emit(
        &mut state,
        "content_block_delta",
        &json!({"delta": {"type": "thinking_delta", "thinking": "hmm"}}),
    );
    assert_eq!(
        chunk,
        json!({
            "id": "chatcmpl-msg_1",
            "object": "chat.completion.chunk",
            "created": NOW,
            "model": "m",
            "choices": [{
                "index": 0,
                "delta": {"reasoning_content": "hmm"},
                "finish_reason": null,
            }],
        })
    );
}

#[test]
fn claude_finish_reason_chunk_shape() {
    let mut state = claude_started();
    let chunk = claude_emit(
        &mut state,
        "message_delta",
        &json!({"delta": {"stop_reason": "max_tokens"}}),
    );
    // Empty delta object, reason mapped, and no `usage` key: no tokens seen.
    assert_eq!(
        chunk,
        json!({
            "id": "chatcmpl-msg_1",
            "object": "chat.completion.chunk",
            "created": NOW,
            "model": "m",
            "choices": [{
                "index": 0,
                "delta": {},
                "finish_reason": "length",
            }],
        })
    );
    assert!(chunk.get("usage").is_none());
}

#[test]
fn claude_final_usage_chunk_shape() {
    let mut state = ClaudeState::default();
    heterion_router_translate::claude::convert_event(
        &mut state,
        "message_start",
        &json!({"message": {"id": "msg_1", "model": "m", "usage": {"input_tokens": 10, "output_tokens": 0}}}),
        NOW,
    );
    let chunk = claude_emit(&mut state, "message_stop", &json!({}));
    assert_eq!(
        chunk,
        json!({
            "id": "chatcmpl-msg_1",
            "object": "chat.completion.chunk",
            "created": NOW,
            "model": "m",
            "choices": [{
                "index": 0,
                "delta": {},
                "finish_reason": "stop",
            }],
            "usage": {
                "prompt_tokens": 10,
                "completion_tokens": 0,
                "total_tokens": 10,
            },
        })
    );
}

#[test]
fn claude_usage_chunk_shape_with_cache_and_reasoning() {
    let mut state = claude_started();
    let chunk = claude_emit(
        &mut state,
        "message_delta",
        &json!({
            "usage": {
                "output_tokens": 25,
                "cache_read_input_tokens": 3,
                "cache_creation_input_tokens": 2,
                "output_tokens_details": {"thinking_tokens": 9},
            },
            "delta": {"stop_reason": "end_turn"},
        }),
    );
    assert_eq!(
        chunk["usage"],
        json!({
            // Billable prompt = input(0) + cache_read(3).
            "prompt_tokens": 3,
            "completion_tokens": 25,
            "total_tokens": 28,
            "reasoning_tokens": 9,
            "completion_tokens_details": {"reasoning_tokens": 9},
            "prompt_tokens_details": {"cached_tokens": 3, "cache_creation_tokens": 2},
        })
    );
    assert_eq!(chunk["choices"][0]["finish_reason"], json!("stop"));
}

#[test]
fn claude_empty_delta_is_ignored() {
    let mut state = claude_started();
    assert_eq!(
        heterion_router_translate::claude::convert_event(
            &mut state,
            "content_block_delta",
            &json!({"delta": {"type": "text_delta", "text": ""}}),
            NOW,
        ),
        ClaudeConverted::Ignore
    );
}

#[test]
fn claude_unknown_identity_falls_back() {
    let mut state = ClaudeState::default();
    let chunk = claude_emit(&mut state, "message_start", &json!({"message": {}}));
    assert_eq!(chunk["id"], json!("chatcmpl-msg_1700000000"));
    assert_eq!(chunk["model"], json!("unknown"));
}

// ---------------------------------------------------------------------------
// gemini → openai
// ---------------------------------------------------------------------------

#[test]
fn gemini_text_delta_chunk_shape() {
    let mut state = gemini_primed();
    let out = heterion_router_translate::gemini::convert_event(
        &mut state,
        &json!({"candidates": [{"content": {"parts": [{"text": "Hi"}]}}]}),
        NOW,
    );
    assert_eq!(
        out,
        vec![json!({
            "id": "chatcmpl-resp_1",
            "object": "chat.completion.chunk",
            "created": NOW,
            "model": "gemini-3-pro",
            "choices": [{
                "index": 0,
                "delta": {"content": "Hi"},
                "finish_reason": null,
            }],
        })]
    );
}

#[test]
fn gemini_first_candidate_announces_role_chunk_shape() {
    let mut state = GeminiState::default();
    let out = heterion_router_translate::gemini::convert_event(
        &mut state,
        &json!({
            "responseId": "resp_1",
            "modelVersion": "gemini-3-pro",
            "candidates": [{"content": {"parts": [{"text": "Hi"}]}}],
        }),
        NOW,
    );
    assert_eq!(
        out[0],
        json!({
            "id": "chatcmpl-resp_1",
            "object": "chat.completion.chunk",
            "created": NOW,
            "model": "gemini-3-pro",
            "choices": [{
                "index": 0,
                "delta": {"role": "assistant"},
                "finish_reason": null,
            }],
        })
    );
    assert!(state.role_sent);
}

#[test]
fn gemini_finish_reason_chunk_shape() {
    let mut state = gemini_primed();
    let out = heterion_router_translate::gemini::convert_event(
        &mut state,
        &json!({"candidates": [{"finishReason": "STOP"}]}),
        NOW,
    );
    assert_eq!(
        out,
        vec![json!({
            "id": "chatcmpl-resp_1",
            "object": "chat.completion.chunk",
            "created": NOW,
            "model": "gemini-3-pro",
            "choices": [{
                "index": 0,
                "delta": {},
                "finish_reason": "stop",
            }],
        })]
    );
    assert!(out[0].get("usage").is_none());
}

#[test]
fn gemini_final_usage_chunk_shape() {
    let mut state = gemini_primed();
    let out = heterion_router_translate::gemini::convert_event(
        &mut state,
        &json!({
            "candidates": [{"finishReason": "MAX_TOKENS"}],
            "usageMetadata": {
                "promptTokenCount": 10,
                "candidatesTokenCount": 4,
                "totalTokenCount": 14,
                "cachedContentTokenCount": 2,
                "thoughtsTokenCount": 1,
            },
        }),
        NOW,
    );
    assert_eq!(
        out,
        vec![json!({
            "id": "chatcmpl-resp_1",
            "object": "chat.completion.chunk",
            "created": NOW,
            "model": "gemini-3-pro",
            "choices": [{
                "index": 0,
                "delta": {},
                "finish_reason": "length",
            }],
            "usage": {
                "prompt_tokens": 10,
                "completion_tokens": 5,
                "total_tokens": 14,
                "prompt_tokens_details": {"cached_tokens": 2},
                "completion_tokens_details": {"reasoning_tokens": 1},
            },
        })]
    );
}

#[test]
fn gemini_tool_calls_reason_wins_over_finish_reason() {
    let mut state = gemini_primed();
    let out = heterion_router_translate::gemini::convert_event(
        &mut state,
        &json!({"candidates": [{
            "content": {"parts": [{"functionCall": {"name": "bash", "args": {}}}]},
            "finishReason": "STOP",
        }]}),
        NOW,
    );
    // One payload yields two chunks: the tool_calls delta, then the terminal one.
    assert_eq!(out.len(), 2);
    assert_eq!(
        out[0]["choices"][0]["delta"]["tool_calls"][0]["function"]["name"],
        json!("bash")
    );
    assert_eq!(out[1]["choices"][0]["finish_reason"], json!("tool_calls"));
}

#[test]
fn gemini_empty_text_part_is_skipped() {
    let mut state = gemini_primed();
    let out = heterion_router_translate::gemini::convert_event(
        &mut state,
        &json!({"candidates": [{"content": {"parts": [{"text": ""}]}}]}),
        NOW,
    );
    assert!(out.is_empty());
}

// ---------------------------------------------------------------------------
// openai-responses → openai
// ---------------------------------------------------------------------------

fn responses_state() -> ResponsesState {
    ResponsesState::new("chatcmpl-test-001".to_string(), NOW, "grok-4.6".to_string())
}

fn responses_emit(state: &mut ResponsesState, event_type: &str, data: &Value) -> Value {
    match heterion_router_translate::responses::convert_event(state, event_type, data) {
        Converted::Emit(value) => value,
        other => panic!("expected a chunk, got {other:?}"),
    }
}

#[test]
fn responses_text_delta_chunk_shape_carries_role() {
    let mut state = responses_state();
    let chunk = responses_emit(
        &mut state,
        "response.output_text.delta",
        &json!({"delta": "Hi"}),
    );
    assert_eq!(
        chunk,
        json!({
            "id": "chatcmpl-test-001",
            "object": "chat.completion.chunk",
            "created": NOW,
            "model": "grok-4.6",
            "choices": [{
                "index": 0,
                "delta": {"role": "assistant", "content": "Hi"},
                "finish_reason": null,
            }],
        })
    );
}

#[test]
fn responses_second_text_delta_drops_role() {
    let mut state = responses_state();
    responses_emit(
        &mut state,
        "response.output_text.delta",
        &json!({"delta": "Hi"}),
    );
    let chunk = responses_emit(
        &mut state,
        "response.output_text.delta",
        &json!({"delta": "!"}),
    );
    assert_eq!(
        chunk["choices"][0]["delta"],
        json!({"content": "!"}),
        "role is announced exactly once"
    );
}

#[test]
fn responses_finish_reason_chunk_shape() {
    let mut state = responses_state();
    let chunk = responses_emit(&mut state, "response.completed", &json!({}));
    assert_eq!(
        chunk,
        json!({
            "id": "chatcmpl-test-001",
            "object": "chat.completion.chunk",
            "created": NOW,
            "model": "grok-4.6",
            "choices": [{
                "index": 0,
                "delta": {},
                "finish_reason": "stop",
            }],
        })
    );
}

#[test]
fn responses_final_usage_chunk_shape() {
    let mut state = responses_state();
    let chunk = responses_emit(
        &mut state,
        "response.completed",
        &json!({"response": {"usage": {
            "input_tokens": 10,
            "output_tokens": 25,
            "input_tokens_details": {"cached_tokens": 4},
        }}}),
    );
    assert_eq!(
        chunk,
        json!({
            "id": "chatcmpl-test-001",
            "object": "chat.completion.chunk",
            "created": NOW,
            "model": "grok-4.6",
            "choices": [{
                "index": 0,
                "delta": {},
                "finish_reason": "stop",
            }],
            "usage": {
                "prompt_tokens": 10,
                "completion_tokens": 25,
                "total_tokens": 35,
                "prompt_tokens_details": {"cached_tokens": 4},
            },
        })
    );
}

#[test]
fn responses_finish_reason_is_tool_calls_after_a_call() {
    let mut state = responses_state();
    responses_emit(
        &mut state,
        "response.output_item.added",
        &json!({
            "item": {"type": "function_call", "id": "fc_1", "call_id": "call_1", "name": " bash "},
            "output_index": 0,
        }),
    );
    let chunk = responses_emit(&mut state, "response.completed", &json!({}));
    assert_eq!(chunk["choices"][0]["finish_reason"], json!("tool_calls"));
}

#[test]
fn responses_empty_delta_is_ignored() {
    let mut state = responses_state();
    assert_eq!(
        heterion_router_translate::responses::convert_event(
            &mut state,
            "response.output_text.delta",
            &json!({"delta": ""})
        ),
        Converted::Ignore
    );
}

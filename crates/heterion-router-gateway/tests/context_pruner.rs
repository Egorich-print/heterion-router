//! Integration Test Suite for Milestone 2: Zero-Quota Algorithmic Context Pruning.
//!
//! Validates:
//! 1. Sub-50 microsecond pure Rust execution benchmark (`test_zero_quota_pruning`).
//! 2. Sizing failovers from 1M to 262k/128k tiers.
//! 3. Preservation of system prompt, initial user goal, and active dialogue tail.
//! 4. Strict 1-to-1 `tool_call_id` parity.
//! 5. Comprehensive edge cases (zero tools, <4 turns, multimodal/non-string, <=512 bytes, emergency pass 2).

use heterion_router_core::{ChatCompletionRequest, ChatMessage};
use heterion_router_gateway::context_pruner::{
    TOKEN_BUDGET_SAFETY_FACTOR, model_context_limit, prune_request,
};
use heterion_router_gateway::usage_estimate::estimate_prompt_tokens;
use serde_json::{Value, json};
use std::time::{Duration, Instant};

// ---------------------------------------------------------------------------
// Test Fixture Builders
// ---------------------------------------------------------------------------

fn make_user_msg(text: &str) -> ChatMessage {
    ChatMessage::plain("user", text)
}

fn make_system_msg(text: &str) -> ChatMessage {
    ChatMessage::plain("system", text)
}

fn make_assistant_tool_call(call_id: &str, tool_name: &str) -> ChatMessage {
    ChatMessage {
        role: "assistant".to_string(),
        content: None,
        reasoning_content: None,
        tool_calls: Some(json!([
            {
                "id": call_id,
                "type": "function",
                "function": {
                    "name": tool_name,
                    "arguments": "{\"query\":\"test\"}"
                }
            }
        ])),
        tool_call_id: None,
        name: None,
    }
}

fn make_tool_msg(call_id: &str, content: Value) -> ChatMessage {
    ChatMessage {
        role: "tool".to_string(),
        content: Some(content),
        reasoning_content: None,
        tool_calls: None,
        tool_call_id: Some(call_id.to_string()),
        name: None,
    }
}

fn make_parallel_assistant_tool_call(calls: &[(&str, &str)]) -> ChatMessage {
    let tool_calls_json: Vec<Value> = calls
        .iter()
        .map(|(id, name)| {
            json!({
                "id": id,
                "type": "function",
                "function": {
                    "name": name,
                    "arguments": "{}"
                }
            })
        })
        .collect();

    ChatMessage {
        role: "assistant".to_string(),
        content: None,
        reasoning_content: None,
        tool_calls: Some(Value::Array(tool_calls_json)),
        tool_call_id: None,
        name: None,
    }
}

/// Builds a multi-turn autonomous agent session.
/// - 1 system prompt
/// - 1 initial user goal
/// - `intermediate_turns` of (assistant call -> tool output of `intermediate_bytes`)
/// - `tail_turns` of (assistant call -> tool output of `tail_bytes`)
fn build_agent_session(
    intermediate_turns: usize,
    intermediate_bytes: usize,
    tail_turns: usize,
    tail_bytes: usize,
) -> ChatCompletionRequest {
    let mut messages = Vec::new();

    // 1. System prompt
    messages.push(make_system_msg(
        "You are an expert autonomous systems engineer with full tool capabilities.",
    ));

    // 2. Initial user goal
    messages.push(make_user_msg(
        "Implement zero-quota algorithmic context pruning in Rust and run full verification.",
    ));

    // 3. Intermediate turns (older history)
    for i in 0..intermediate_turns {
        let call_id = format!("call_intermediate_{i}");
        messages.push(make_assistant_tool_call(&call_id, "run_command"));

        // Generate large realistic command/file output
        let content_text = format!(
            "intermediate tool output chunk {i}\n{}\nend of chunk {i}",
            "x".repeat(intermediate_bytes.saturating_sub(60))
        );
        messages.push(make_tool_msg(&call_id, json!(content_text)));
    }

    // 4. Trailing active turns (active dialogue tail)
    for i in 0..tail_turns {
        let call_id = format!("call_tail_{i}");
        messages.push(make_assistant_tool_call(&call_id, "view_file"));

        let content_text = format!(
            "tail active output {i}: line content\n{}",
            "y".repeat(tail_bytes.saturating_sub(40))
        );
        messages.push(make_tool_msg(&call_id, json!(content_text)));
    }

    ChatCompletionRequest {
        model: "heterion-router/auto".to_string(),
        messages,
        ..Default::default()
    }
}

// ---------------------------------------------------------------------------
// 1. Core Unit Benchmark & Contract Test
// ---------------------------------------------------------------------------

#[test]
fn test_zero_quota_pruning() {
    // Construct a session with 6 intermediate turns (each 80KB, totaling ~480KB / ~120k tokens)
    // and 4 active tail turns (each 2KB).
    let base_req = build_agent_session(6, 80_000, 4, 2_000);
    let original_message_count = base_req.messages.len();

    // Record initial state for invariant checks
    let orig_system_content = base_req.messages[0].content.clone();
    let orig_user_goal_content = base_req.messages[1].content.clone();
    let orig_tail_messages = base_req.messages[base_req.messages.len() - 8..].to_vec();

    // BENCHMARK: Pure Rust execution time measurement
    // Uses multi-iteration best-of-N sampling (up to 15 iterations) with early exit.
    // This isolates pure algorithmic CPU execution from external OS jitter, thread preemption,
    // and CI noisy-neighbor interruptions on non-real-time kernels.
    const BENCH_ITERATIONS: usize = 15;
    let mut min_duration = Duration::MAX;
    let mut pruned_req = base_req.clone();
    let mut pruned_tokens = 0;
    let mut orig_tokens = 0;

    for _ in 0..BENCH_ITERATIONS {
        let mut sample_req = base_req.clone();
        let start = Instant::now();
        let (p_toks, o_toks) = prune_request(&mut sample_req, 128_000);
        let elapsed = start.elapsed();

        if elapsed < min_duration {
            min_duration = elapsed;
            pruned_req = sample_req;
            pruned_tokens = p_toks;
            orig_tokens = o_toks;
        }

        // Early break if target latency requirement is already proven
        #[cfg(not(debug_assertions))]
        if min_duration < Duration::from_micros(50) {
            break;
        }
        #[cfg(debug_assertions)]
        if min_duration < Duration::from_micros(500) {
            break;
        }
    }

    // 1. Latency assertions:
    // In release mode: strictly < 50 microseconds (<0.05 ms) SLA from Requirement R2.
    #[cfg(not(debug_assertions))]
    assert!(
        min_duration < Duration::from_micros(50),
        "Zero-quota pruning execution exceeded 50µs SLA in release mode: min took {:?}",
        min_duration
    );
    // In debug mode: unoptimized (opt-level=0) sanity guard (<5ms) preventing O(N^2) regressions.
    #[cfg(debug_assertions)]
    assert!(
        min_duration < Duration::from_millis(5),
        "Zero-quota pruning execution exceeded 5ms debug sanity threshold: min took {:?}",
        min_duration
    );

    // 2. Token reduction assertion
    assert!(
        orig_tokens > 35_000,
        "Original tokens should reflect large intermediate payloads: {orig_tokens}"
    );
    assert!(
        pruned_tokens < orig_tokens,
        "Pruned tokens ({pruned_tokens}) must be less than original tokens ({orig_tokens})"
    );
    let budget = (128_000.0 * TOKEN_BUDGET_SAFETY_FACTOR) as u32;
    assert!(
        pruned_tokens <= budget,
        "Pruned tokens ({pruned_tokens}) must fit within safety budget ({budget})"
    );

    // 3. Message count invariant: no messages deleted
    assert_eq!(
        pruned_req.messages.len(),
        original_message_count,
        "Pruner must mutate content in-place; message count must remain identical"
    );

    // 4. System prompt 100% preserved
    assert_eq!(pruned_req.messages[0].role, "system");
    assert_eq!(pruned_req.messages[0].content, orig_system_content);

    // 5. Initial user goal 100% preserved
    assert_eq!(pruned_req.messages[1].role, "user");
    assert_eq!(pruned_req.messages[1].content, orig_user_goal_content);

    // 6. Intermediate tool outputs folded into structured digests
    for i in 0..6 {
        let tool_msg_idx = 2 + (i * 2) + 1; // Tool message index
        let msg = &pruned_req.messages[tool_msg_idx];
        assert_eq!(msg.role, "tool");

        let content_str = msg.text();
        let expected_call_id = format!("call_intermediate_{i}");

        // Strict 1-to-1 tool_call_id parity
        assert_eq!(
            msg.tool_call_id,
            Some(expected_call_id.clone()),
            "tool_call_id parity violated at index {tool_msg_idx}"
        );

        // Structured digest formatting
        assert!(
            content_str.contains(&format!("[Tool Output Digest: id={expected_call_id}")),
            "Folded output missing digest header: {content_str}"
        );
        assert!(
            content_str.contains("... [omitted "),
            "Folded output missing omitted byte marker"
        );
        assert!(
            content_str.contains("Tail:\n"),
            "Folded output missing tail marker"
        );
    }

    // 7. Trailing 4 interaction turns (last 8 messages) 100% preserved verbatim
    let current_tail = &pruned_req.messages[pruned_req.messages.len() - 8..];
    assert_eq!(
        current_tail,
        orig_tail_messages.as_slice(),
        "Active dialogue tail (last 4 interaction turns) was mutated during Pass 1"
    );
}

// ---------------------------------------------------------------------------
// 2. Sizing Failover Tests (1M -> 262k/128k)
// ---------------------------------------------------------------------------

#[test]
fn test_sizing_failover_1m_to_128k() {
    // Construct request with ~160k tokens of tool history (10 intermediate turns * 64KB)
    let mut req = build_agent_session(10, 64_000, 4, 1_000);
    let orig_tokens = estimate_prompt_tokens(&req);
    assert!(
        orig_tokens > 130_000,
        "Request must exceed 128k tokens: got {orig_tokens}"
    );

    // Prune for 128k model (DeepSeek, Mistral, Gemma)
    let (pruned_tokens, reported_orig) = prune_request(&mut req, 128_000);
    assert_eq!(reported_orig, orig_tokens);

    let budget_128k = (128_000.0 * TOKEN_BUDGET_SAFETY_FACTOR) as u32;
    assert!(
        pruned_tokens <= budget_128k,
        "Pruned tokens ({pruned_tokens}) exceeds 128k budget ({budget_128k})"
    );
    assert!(
        pruned_tokens < 30_000,
        "Folded prompt should be well below 30k tokens: got {pruned_tokens}"
    );
}

#[test]
fn test_sizing_failover_1m_to_256k() {
    // Construct request with ~300k tokens of tool history (12 intermediate turns * 100KB)
    let mut req = build_agent_session(12, 100_000, 4, 1_000);
    let orig_tokens = estimate_prompt_tokens(&req);
    assert!(
        orig_tokens > 260_000,
        "Request must exceed 256k tokens: got {orig_tokens}"
    );

    // Prune for 256k model (Codestral)
    let (pruned_tokens, _) = prune_request(&mut req, 256_000);
    let budget_256k = (256_000.0 * TOKEN_BUDGET_SAFETY_FACTOR) as u32;
    assert!(
        pruned_tokens <= budget_256k,
        "Pruned tokens ({pruned_tokens}) exceeds 256k budget ({budget_256k})"
    );
}

#[test]
fn test_sizing_no_op_when_within_budget() {
    // Request with ~10k tokens (within 128k target)
    let mut req = build_agent_session(2, 4_000, 2, 1_000);
    let orig_tokens = estimate_prompt_tokens(&req);
    let cloned_messages = req.messages.clone();

    // Calling prune_request with 128_000 should be an instant no-op
    let (pruned_tokens, reported_orig) = prune_request(&mut req, 128_000);
    assert_eq!(pruned_tokens, orig_tokens);
    assert_eq!(reported_orig, orig_tokens);
    assert_eq!(
        req.messages, cloned_messages,
        "Messages must not be mutated when request fits within budget"
    );
}

#[test]
fn test_tokens_compressed_calculation() {
    let mut req = build_agent_session(8, 70_000, 4, 1_000);
    let (pruned_tokens, orig_tokens) = prune_request(&mut req, 128_000);

    let tokens_compressed = orig_tokens.saturating_sub(pruned_tokens) as i64;
    assert!(
        tokens_compressed > 20_000,
        "tokens_compressed metric must record token savings: got {tokens_compressed}"
    );
}

// ---------------------------------------------------------------------------
// 3. Edge Case Coverage
// ---------------------------------------------------------------------------

#[test]
fn test_edge_case_zero_tool_calls() {
    // Request consisting of multiple user and assistant turns without any tools
    let mut req = ChatCompletionRequest {
        model: "heterion-router/auto".to_string(),
        messages: vec![
            make_system_msg("System prompt"),
            make_user_msg("Explain the Raft consensus algorithm."),
            ChatMessage::assistant_text(
                "Raft is a consensus algorithm designed for understandability...",
            ),
            make_user_msg("How does leader election handle network partitions?"),
            ChatMessage::assistant_text(
                "In a partition, the minority cluster cannot reach consensus...",
            ),
        ],
        ..Default::default()
    };
    let orig_messages = req.messages.clone();

    let (pruned_tokens, orig_tokens) = prune_request(&mut req, 128_000);
    assert_eq!(pruned_tokens, orig_tokens);
    assert_eq!(
        req.messages, orig_messages,
        "Request without tool messages must remain completely unaltered"
    );
}

#[test]
fn test_edge_case_fewer_than_four_turns() {
    // Request with only 2 interaction turns:
    // Turn 1: User prompt
    // Turn 2: Assistant tool call -> large tool output (50KB)
    let mut req = ChatCompletionRequest {
        model: "heterion-router/auto".to_string(),
        messages: vec![
            make_system_msg("System instructions"),
            make_user_msg("Fetch repository status"),
            make_assistant_tool_call("call_status_1", "git_status"),
            make_tool_msg("call_status_1", json!("status ".repeat(5_000))),
        ],
        ..Default::default()
    };
    let orig_tool_content = req.messages[3].content.clone();

    // With target 128k, total tokens are ~12.5k (under budget).
    // Because total turns (2) <= DEFAULT_RETAIN_TAIL_TURNS (4), all turns belong to active tail.
    let _ = prune_request(&mut req, 128_000);

    assert_eq!(
        req.messages[3].content, orig_tool_content,
        "Tool output in sessions with <= 4 turns must not be folded during standard Pass 1"
    );
}

#[test]
fn test_edge_case_complex_multimodal_and_non_string_content() {
    let mut req = ChatCompletionRequest {
        model: "heterion-router/auto".to_string(),
        messages: vec![
            make_system_msg("System prompt"),
            make_user_msg("Execute diagnostics with varied content types"),
            // Turn 1: tool with Array parts
            make_assistant_tool_call("call_array_1", "tool_array"),
            make_tool_msg(
                "call_array_1",
                json!([
                    {"type": "text", "text": "diagnostics log: ".to_string() + &"z".repeat(10_000)},
                    {"type": "image_url", "image_url": {"url": "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg=="}}
                ]),
            ),
            // Turn 2: tool with numeric content
            make_assistant_tool_call("call_num_1", "tool_num"),
            make_tool_msg("call_num_1", json!(42)),
            // Turn 3: tool with boolean content
            make_assistant_tool_call("call_bool_1", "tool_bool"),
            make_tool_msg("call_bool_1", json!(true)),
            // Turn 4: tool with Null content
            make_assistant_tool_call("call_null_1", "tool_null"),
            make_tool_msg("call_null_1", Value::Null),
            // Active tail turns to ensure preceding are intermediate
            make_assistant_tool_call("call_tail_1", "tail_tool"),
            make_tool_msg("call_tail_1", json!("tail result 1")),
            make_assistant_tool_call("call_tail_2", "tail_tool"),
            make_tool_msg("call_tail_2", json!("tail result 2")),
            make_assistant_tool_call("call_tail_3", "tail_tool"),
            make_tool_msg("call_tail_3", json!("tail result 3")),
            make_assistant_tool_call("call_tail_4", "tail_tool"),
            make_tool_msg("call_tail_4", json!("tail result 4")),
        ],
        ..Default::default()
    };

    // Force pruning by targeting a low limit (e.g. 1_000 tokens)
    let (pruned_tokens, _) = prune_request(&mut req, 1_000);
    assert!(pruned_tokens > 0);

    // Verify non-crash and proper handling:
    // 1. Array content was converted to folded string digest
    let array_tool = &req.messages[3];
    assert_eq!(array_tool.tool_call_id, Some("call_array_1".to_string()));
    assert!(
        array_tool
            .text()
            .contains("[Tool Output Digest: id=call_array_1"),
        "Array content was not properly folded into digest"
    );

    // 2. Numeric, boolean, null tool messages preserved without panic
    assert_eq!(req.messages[5].tool_call_id, Some("call_num_1".to_string()));
    assert_eq!(
        req.messages[7].tool_call_id,
        Some("call_bool_1".to_string())
    );
    assert_eq!(
        req.messages[9].tool_call_id,
        Some("call_null_1".to_string())
    );
}

#[test]
fn test_edge_case_small_tool_output_not_folded() {
    let mut req = ChatCompletionRequest {
        model: "heterion-router/auto".to_string(),
        messages: vec![
            make_system_msg("System prompt"),
            make_user_msg("Perform small check"),
            // Intermediate turn with tiny tool output (15 bytes <= MIN_TOOL_PRUNE_BYTES 512)
            make_assistant_tool_call("call_small_1", "ping"),
            make_tool_msg("call_small_1", json!("pong: ok (2ms)")),
            // Intermediate turn with 500-byte output (<= MIN_TOOL_PRUNE_BYTES 512)
            make_assistant_tool_call("call_small_2", "read"),
            make_tool_msg("call_small_2", json!("a".repeat(500))),
            // Tail turns
            make_assistant_tool_call("call_t1", "t"),
            make_tool_msg("call_t1", json!("tail")),
            make_assistant_tool_call("call_t2", "t"),
            make_tool_msg("call_t2", json!("tail")),
            make_assistant_tool_call("call_t3", "t"),
            make_tool_msg("call_t3", json!("tail")),
            make_assistant_tool_call("call_t4", "t"),
            make_tool_msg("call_t4", json!("tail")),
        ],
        ..Default::default()
    };

    // Force pruning check
    let _ = prune_request(&mut req, 500);

    // Tool outputs <= 512 bytes are bypassed
    let ping_msg = &req.messages[3];
    assert_eq!(
        ping_msg.text(),
        "pong: ok (2ms)",
        "Small tool outputs (<= MIN_TOOL_PRUNE_BYTES) must not be folded"
    );
    let read_msg = &req.messages[5];
    assert_eq!(
        read_msg.text(),
        "a".repeat(500),
        "500-byte tool outputs (<= MIN_TOOL_PRUNE_BYTES) must not be folded"
    );
}

#[test]
fn test_edge_case_emergency_pass_2_huge_tail_tool() {
    // Scenario: Agent receives a gigantic 600KB file in the *current* turn (inside active tail)
    let huge_output = "ERROR: stack overflow trace:\n".to_string() + &"trace_frame ".repeat(40_000);
    assert!(huge_output.len() > 100_000);

    let mut req = ChatCompletionRequest {
        model: "heterion-router/auto".to_string(),
        messages: vec![
            make_system_msg("System prompt"),
            make_user_msg("Analyze dump"),
            make_assistant_tool_call("call_tail_huge", "read_huge_dump"),
            make_tool_msg("call_tail_huge", json!(huge_output)),
        ],
        ..Default::default()
    };

    let orig_tokens = estimate_prompt_tokens(&req);
    assert!(
        orig_tokens > 30_000,
        "Original tokens must reflect huge tail dump: {orig_tokens}"
    );

    // Prune targeting 20_000 tokens (e.g. tight budget)
    let (pruned_tokens, _) = prune_request(&mut req, 20_000);

    let budget = (20_000.0 * TOKEN_BUDGET_SAFETY_FACTOR) as u32;
    assert!(
        pruned_tokens <= budget,
        "Emergency Pass 2 must reduce prompt ({pruned_tokens}) below target budget ({budget})"
    );

    // Verify huge tail tool was folded into digest with error status
    let tail_tool = &req.messages[3];
    assert_eq!(tail_tool.tool_call_id, Some("call_tail_huge".to_string()));
    let text = tail_tool.text();
    assert!(
        text.contains("[Tool Output Digest: id=call_tail_huge"),
        "Huge tail tool was not folded by Pass 2 emergency pruner"
    );
    assert!(
        text.contains("status: completed with errors/warnings"),
        "Error markers in huge output should be detected in digest status"
    );
}

#[test]
fn test_edge_case_utf8_char_boundaries() {
    // Construct multi-byte characters (Russian Cyrillic, 2 bytes/char; Chinese, 3 bytes/char; Emoji, 4 bytes/char)
    // positioned directly across the 160-byte boundary.
    let prefix = "А".repeat(78); // 78 * 2 = 156 bytes
    let multibyte_str = format!("{prefix}🦀🎉Привет Мир! 中文测试");

    // Slicing blindly at index 160 would land in the middle of the 4-byte 🦀 (156..160)
    // or inside a multi-byte sequence, causing a panic.
    let mut req = ChatCompletionRequest {
        model: "heterion-router/auto".to_string(),
        messages: vec![
            make_system_msg("System instructions"),
            make_user_msg("UTF-8 boundary verification"),
            make_assistant_tool_call("call_utf8_1", "multibyte_tool"),
            make_tool_msg(
                "call_utf8_1",
                json!(multibyte_str.clone() + &" extra content ".repeat(50)),
            ),
            make_assistant_tool_call("call_t1", "t"),
            make_tool_msg("call_t1", json!("tail")),
            make_assistant_tool_call("call_t2", "t"),
            make_tool_msg("call_t2", json!("tail")),
            make_assistant_tool_call("call_t3", "t"),
            make_tool_msg("call_t3", json!("tail")),
            make_assistant_tool_call("call_t4", "t"),
            make_tool_msg("call_t4", json!("tail")),
        ],
        ..Default::default()
    };

    // Prune should execute without panic
    let (pruned_tokens, _) = prune_request(&mut req, 100);
    assert!(pruned_tokens > 0);

    let text = req.messages[3].text();
    assert!(text.contains("[Tool Output Digest: id=call_utf8_1"));
    // Valid UTF-8 string integrity confirmed by str operations
    assert!(!text.is_empty());
}

#[test]
fn test_edge_case_parallel_tool_calls() {
    // An assistant turn issuing 3 parallel tool calls: call_p1, call_p2, call_p3
    let parallel_calls = [
        ("call_p1", "tool_alpha"),
        ("call_p2", "tool_beta"),
        ("call_p3", "tool_gamma"),
    ];

    let mut req = ChatCompletionRequest {
        model: "heterion-router/auto".to_string(),
        messages: vec![
            make_system_msg("System prompt"),
            make_user_msg("Execute 3 tasks in parallel"),
            // Intermediate Turn 1 (parallel)
            make_parallel_assistant_tool_call(&parallel_calls),
            make_tool_msg("call_p1", json!("output alpha ".repeat(1_000))),
            make_tool_msg("call_p2", json!("output beta ".repeat(1_000))),
            make_tool_msg("call_p3", json!("output gamma ".repeat(1_000))),
            // 4 Tail turns
            make_assistant_tool_call("call_t1", "t"),
            make_tool_msg("call_t1", json!("tail")),
            make_assistant_tool_call("call_t2", "t"),
            make_tool_msg("call_t2", json!("tail")),
            make_assistant_tool_call("call_t3", "t"),
            make_tool_msg("call_t3", json!("tail")),
            make_assistant_tool_call("call_t4", "t"),
            make_tool_msg("call_t4", json!("tail")),
        ],
        ..Default::default()
    };

    let _ = prune_request(&mut req, 2_000);

    // Verify all 3 parallel tool outputs folded and preserved IDs
    assert_eq!(req.messages[3].tool_call_id, Some("call_p1".to_string()));
    assert!(
        req.messages[3]
            .text()
            .contains("[Tool Output Digest: id=call_p1")
    );

    assert_eq!(req.messages[4].tool_call_id, Some("call_p2".to_string()));
    assert!(
        req.messages[4]
            .text()
            .contains("[Tool Output Digest: id=call_p2")
    );

    assert_eq!(req.messages[5].tool_call_id, Some("call_p3".to_string()));
    assert!(
        req.messages[5]
            .text()
            .contains("[Tool Output Digest: id=call_p3")
    );
}

#[test]
fn test_edge_case_pruning_idempotency() {
    let mut req = build_agent_session(6, 20_000, 4, 1_000);

    // First pruning pass
    let (tokens_pass_1, _) = prune_request(&mut req, 128_000);

    let messages_after_pass_1 = req.messages.clone();

    // Second pruning pass on the already pruned request
    let (tokens_pass_2, _) = prune_request(&mut req, 128_000);

    assert_eq!(
        tokens_pass_2, tokens_pass_1,
        "Subsequent pruning passes must be idempotent in token count"
    );
    assert_eq!(
        req.messages, messages_after_pass_1,
        "Subsequent pruning passes must not mutate already folded digests"
    );
}

#[test]
fn test_model_context_limits_catalog() {
    // The pruner plans and prunes against the capability catalog's window, so
    // the two tables cannot drift apart.
    for model in [
        "gemini-2.5-pro",
        "google/gemini-2.5-flash",
        "gemini-3-flash-preview",
        "agentrouter/gpt-5.6-sol",
        "gpt-6.1-sol",
        "gpt-6-astra",
        "mistral/codestral-latest",
        "codestral-2501",
        "claude-3-5-sonnet-latest",
        "deepseek-chat",
        "deepseek/deepseek-reasoner",
        "gemini/gemma-4-26b-a4b-it",
        "gemma-4-26b-a4b-it",
        "mistral/mistral-large-latest",
        "unknown-model",
    ] {
        assert_eq!(
            model_context_limit(model),
            heterion_router_gateway::model_spec::model_spec(model).context_window,
            "the pruner and the capability catalog disagree about {model}"
        );
    }

    // 1M models
    assert_eq!(model_context_limit("gemini-2.5-pro"), 1_000_000);
    assert_eq!(model_context_limit("google/gemini-2.5-flash"), 1_000_000);
    assert_eq!(model_context_limit("gemini-3-flash-preview"), 1_000_000);

    // 512k models — the operational window, deliberately below the ~870k the
    // agentrouter platform measured, so a request never runs into its
    // balancer timeout.
    assert_eq!(model_context_limit("agentrouter/gpt-5.6-sol"), 512_000);
    assert_eq!(model_context_limit("gpt-6.1-sol"), 512_000);
    assert_eq!(model_context_limit("gpt-6-astra"), 512_000);

    // 256k models
    assert_eq!(model_context_limit("mistral/codestral-latest"), 256_000);
    assert_eq!(model_context_limit("codestral-2501"), 256_000);
    assert_eq!(model_context_limit("gemma-4-26b-a4b-it"), 256_000);
    assert_eq!(model_context_limit("gemini/gemma-4-26b-a4b-it"), 256_000);

    // 200k models
    assert_eq!(model_context_limit("claude-3-5-sonnet-latest"), 200_000);

    // 128k models
    assert_eq!(model_context_limit("deepseek-chat"), 128_000);
    assert_eq!(model_context_limit("deepseek/deepseek-reasoner"), 128_000);
    assert_eq!(model_context_limit("mistral/mistral-large-latest"), 128_000);
    assert_eq!(model_context_limit("unknown-model"), 128_000);
}

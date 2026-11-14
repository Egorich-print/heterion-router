//! Empirical Challenge Test Suite for Milestone 2: Zero-Quota Algorithmic Context Pruning.
//!
//! Authored by: challenger_m2_2 (EMPIRICAL CHALLENGER)
//! Verification target: Milestone 2 Context Pruner implementation and arithmetic invariants.

use heterion_router_core::{ChatCompletionRequest, ChatMessage};
use heterion_router_gateway::context_pruner::{
    MIN_TOOL_PRUNE_BYTES, TOKEN_BUDGET_SAFETY_FACTOR, fast_estimate_tokens, fold_tool_content,
    model_context_limit, prune_request,
};
use heterion_router_gateway::usage_estimate::estimate_prompt_tokens;
use serde_json::{Value, json};

// ---------------------------------------------------------------------------
// Helpers and Generators
// ---------------------------------------------------------------------------

fn make_user(text: &str) -> ChatMessage {
    ChatMessage::plain("user", text)
}

fn make_system(text: &str) -> ChatMessage {
    ChatMessage::plain("system", text)
}

fn make_assistant_call(call_id: &str, tool_name: &str) -> ChatMessage {
    ChatMessage {
        role: "assistant".to_string(),
        content: None,
        reasoning_content: None,
        tool_calls: Some(json!([{
            "id": call_id,
            "type": "function",
            "function": { "name": tool_name, "arguments": "{}" }
        }])),
        tool_call_id: None,
        name: None,
    }
}

fn make_tool(call_id: &str, content: Value) -> ChatMessage {
    ChatMessage {
        role: "tool".to_string(),
        content: Some(content),
        reasoning_content: None,
        tool_calls: None,
        tool_call_id: Some(call_id.to_string()),
        name: None,
    }
}

/// Builds a large agent session with `intermediate_turns` and `tail_turns`.
fn build_challenge_session(
    intermediate_turns: usize,
    bytes_per_intermediate: usize,
    tail_turns: usize,
    bytes_per_tail: usize,
) -> ChatCompletionRequest {
    let mut messages = Vec::new();
    messages.push(make_system(
        "System prompt: Enterprise autonomous router agent",
    ));
    messages.push(make_user(
        "User goal: Stress test context pruning failovers",
    ));

    for i in 0..intermediate_turns {
        let call_id = format!("call_inter_{i}");
        messages.push(make_assistant_call(&call_id, "exec_tool"));
        let body = format!(
            "intermediate output #{i}: header\n{}\nfooter #{i}",
            "A".repeat(bytes_per_intermediate.saturating_sub(60))
        );
        messages.push(make_tool(&call_id, json!(body)));
    }

    for i in 0..tail_turns {
        let call_id = format!("call_tail_{i}");
        messages.push(make_assistant_call(&call_id, "tail_tool"));
        let body = format!(
            "tail active output #{i}:\n{}",
            "B".repeat(bytes_per_tail.saturating_sub(30))
        );
        messages.push(make_tool(&call_id, json!(body)));
    }

    ChatCompletionRequest {
        model: "heterion-router/auto".to_string(),
        messages,
        ..Default::default()
    }
}

// ---------------------------------------------------------------------------
// 1. Sizing Failover Invariants (1M -> 256k and 1M -> 128k)
// ---------------------------------------------------------------------------

#[test]
fn test_challenge_1m_to_256k_failover() {
    // 25 intermediate turns * 160KB = 4,000,000 bytes = ~1,000,000 tokens
    let mut req = build_challenge_session(25, 160_000, 4, 2_000);
    let orig_tokens = estimate_prompt_tokens(&req);
    assert!(
        orig_tokens >= 950_000,
        "Request should simulate ~1M context: got {orig_tokens}"
    );

    let target_limit = model_context_limit("mistral/codestral-latest");
    assert_eq!(target_limit, 256_000);
    let budget = (target_limit as f64 * TOKEN_BUDGET_SAFETY_FACTOR) as u32;

    let orig_msg_count = req.messages.len();
    let orig_system = req.messages[0].content.clone();
    let orig_user_goal = req.messages[1].content.clone();
    let orig_tail = req.messages[req.messages.len() - 8..].to_vec();

    let (pruned_tokens, reported_orig) = prune_request(&mut req, target_limit);
    assert_eq!(reported_orig, orig_tokens);

    // Invariant: folded request fits strictly within target_limit * 0.90
    assert!(
        pruned_tokens <= budget,
        "Pruned tokens ({pruned_tokens}) must fit within Codestral budget ({budget})"
    );
    assert!(
        pruned_tokens < 30_000,
        "Pruned request should be compact (<30k tokens): got {pruned_tokens}"
    );

    // Structural invariants
    assert_eq!(req.messages.len(), orig_msg_count);
    assert_eq!(req.messages[0].content, orig_system);
    assert_eq!(req.messages[1].content, orig_user_goal);
    assert_eq!(
        &req.messages[req.messages.len() - 8..],
        orig_tail.as_slice()
    );

    // Strict 1-to-1 tool_call_id parity
    for i in 0..25 {
        let tool_idx = 2 + (i * 2) + 1;
        let expected_id = format!("call_inter_{i}");
        assert_eq!(
            req.messages[tool_idx].tool_call_id.as_deref(),
            Some(expected_id.as_str())
        );
        let text = req.messages[tool_idx].text();
        assert!(text.starts_with(&format!("[Tool Output Digest: id={expected_id}")));
    }
}

#[test]
fn test_challenge_1m_to_128k_failover() {
    // 25 intermediate turns * 160KB = 4,000,000 bytes = ~1,000,000 tokens
    let mut req = build_challenge_session(25, 160_000, 4, 2_000);
    let orig_tokens = estimate_prompt_tokens(&req);
    assert!(orig_tokens >= 950_000);

    let target_limit = model_context_limit("deepseek/deepseek-chat");
    assert_eq!(target_limit, 128_000);
    let budget = (target_limit as f64 * TOKEN_BUDGET_SAFETY_FACTOR) as u32;

    let (pruned_tokens, reported_orig) = prune_request(&mut req, target_limit);
    assert_eq!(reported_orig, orig_tokens);

    // Invariant: folded request fits strictly within target_limit * 0.90
    assert!(
        pruned_tokens <= budget,
        "Pruned tokens ({pruned_tokens}) must fit within DeepSeek/Mistral budget ({budget})"
    );
}

#[test]
fn test_challenge_folded_request_fits_within_target_limit_0_90() {
    // Test various target limits with different payload scales
    let test_cases = [
        (128_000, 10, 80_000), // ~200k tokens -> 128k target
        (200_000, 15, 80_000), // ~300k tokens -> 200k target
        (256_000, 20, 80_000), // ~400k tokens -> 256k target
        (500_000, 30, 80_000), // ~600k tokens -> 500k target
    ];

    for (target_limit, turns, bytes) in test_cases {
        let mut req = build_challenge_session(turns, bytes, 4, 1_000);
        let orig = fast_estimate_tokens(&req);
        let budget = (target_limit as f64 * TOKEN_BUDGET_SAFETY_FACTOR) as u32;

        if orig > budget {
            let (pruned, _) = prune_request(&mut req, target_limit);
            assert!(
                pruned <= budget,
                "Failed invariant for target {target_limit}: pruned {pruned} > budget {budget}"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// 2. Edge Cases
// ---------------------------------------------------------------------------

#[test]
fn test_challenge_edge_case_zero_tool_calls() {
    // 20 turns of purely user and assistant interaction (no tools)
    let mut messages = Vec::new();
    messages.push(make_system("System instructions"));
    for i in 0..20 {
        messages.push(make_user(&format!(
            "User message {i}: {}",
            "U".repeat(5_000)
        )));
        messages.push(ChatMessage::assistant_text(format!(
            "Assistant response {i}: {}",
            "A".repeat(5_000)
        )));
    }

    let mut req = ChatCompletionRequest {
        model: "heterion-router/auto".to_string(),
        messages,
        ..Default::default()
    };

    let orig_msgs = req.messages.clone();
    let orig_tokens = fast_estimate_tokens(&req);
    assert!(orig_tokens > 50_000);

    // Prune targeting 10k tokens (well below orig_tokens)
    let (pruned_tokens, reported_orig) = prune_request(&mut req, 10_000);

    assert_eq!(pruned_tokens, orig_tokens);
    assert_eq!(reported_orig, orig_tokens);
    assert_eq!(
        req.messages, orig_msgs,
        "Conversations with 0 tool calls must remain completely unaltered"
    );
}

#[test]
fn test_challenge_edge_case_fewer_than_4_turns_all_scenarios() {
    // 0 turns (empty messages)
    let mut req_0 = ChatCompletionRequest {
        model: "auto".to_string(),
        messages: vec![],
        ..Default::default()
    };
    let (p0, o0) = prune_request(&mut req_0, 128_000);
    assert_eq!(p0, o0);
    assert_eq!(p0, 1);

    // 1 turn (User prompt only)
    let mut req_1 = ChatCompletionRequest {
        model: "auto".to_string(),
        messages: vec![make_user(&"large prompt ".repeat(10_000))],
        ..Default::default()
    };
    let orig_1 = req_1.messages.clone();
    let (p1, o1) = prune_request(&mut req_1, 128_000);
    assert_eq!(p1, o1);
    assert_eq!(req_1.messages, orig_1);

    // 2 turns (User + Assistant text)
    let mut req_2 = ChatCompletionRequest {
        model: "auto".to_string(),
        messages: vec![
            make_user("User task"),
            ChatMessage::assistant_text("analysis ".repeat(10_000)),
        ],
        ..Default::default()
    };
    let orig_2 = req_2.messages.clone();
    let (p2, o2) = prune_request(&mut req_2, 128_000);
    assert_eq!(p2, o2);
    assert_eq!(req_2.messages, orig_2);

    // 3 turns (User -> Assistant call -> Tool output 10KB <= 16KB)
    // Within 128k target: Pass 1 does not fold (turns <= 4), Pass 2 does not fold (<=16KB)
    let mut req_3 = ChatCompletionRequest {
        model: "auto".to_string(),
        messages: vec![
            make_user("Run tool"),
            make_assistant_call("call_3", "cmd"),
            make_tool("call_3", json!("x".repeat(10_000))),
        ],
        ..Default::default()
    };
    let orig_3 = req_3.messages.clone();
    let (p3, o3) = prune_request(&mut req_3, 128_000);
    assert_eq!(p3, o3);
    assert_eq!(req_3.messages, orig_3);
}

#[test]
fn test_challenge_edge_case_massive_tail_tools_emergency_pass_2() {
    // 16,384 byte boundary:
    // If tail tool is exactly 16,384 bytes, Pass 2 does NOT fold it.
    // If tail tool is 16,385 bytes, Pass 2 DOES fold it.
    let mut req_boundary = ChatCompletionRequest {
        model: "auto".to_string(),
        messages: vec![
            make_user("Analyze 16384 byte tool"),
            make_assistant_call("c_16384", "t"),
            make_tool("c_16384", json!("A".repeat(16384))),
            make_assistant_call("c_16385", "t"),
            make_tool("c_16385", json!("B".repeat(16385))),
        ],
        ..Default::default()
    };

    // Force Pass 2 by setting target limit tiny (1_000 tokens)
    let (pruned, orig) = prune_request(&mut req_boundary, 1_000);
    assert!(pruned < orig);

    // c_16384 should remain raw
    assert_eq!(
        req_boundary.messages[2].text(),
        "A".repeat(16384),
        "Tool output of exactly 16384 bytes must not be folded in Pass 2"
    );

    // c_16385 should be folded
    let folded_text = req_boundary.messages[4].text();
    assert!(
        folded_text.starts_with("[Tool Output Digest: id=c_16385"),
        "Tool output of 16385 bytes (>16KB) must be folded by Pass 2 emergency pruner"
    );
}

#[test]
fn test_challenge_edge_case_multimodal_complex_arrays_and_scalars() {
    let mut req = ChatCompletionRequest {
        model: "auto".to_string(),
        messages: vec![
            make_system("System prompt"),
            make_user("Multi-type diagnostics"),
            // Turn 1: Multi-part Array
            make_assistant_call("call_arr", "tool_arr"),
            make_tool(
                "call_arr",
                json!([
                    {"type": "text", "text": "log prefix: ".to_string() + &"L".repeat(5_000)},
                    {"type": "image_url", "image_url": {"url": "https://example.com/graph.png"}},
                    {"custom_field": 12345}
                ]),
            ),
            // Turn 2: Non-string scalars
            make_assistant_call("call_null", "tool_null"),
            make_tool("call_null", Value::Null),
            make_assistant_call("call_bool", "tool_bool"),
            make_tool("call_bool", json!(false)),
            make_assistant_call("call_int", "tool_int"),
            make_tool("call_int", json!(42)),
            make_assistant_call("call_float", "tool_float"),
            make_tool("call_float", json!(3.15)),
            // 4 Tail turns to make above turns intermediate
            make_assistant_call("t1", "t"),
            make_tool("t1", json!("tail 1")),
            make_assistant_call("t2", "t"),
            make_tool("t2", json!("tail 2")),
            make_assistant_call("t3", "t"),
            make_tool("t3", json!("tail 3")),
            make_assistant_call("t4", "t"),
            make_tool("t4", json!("tail 4")),
        ],
        ..Default::default()
    };

    let (pruned, _) = prune_request(&mut req, 1_000);
    assert!(pruned > 0);

    // Verify Array was folded to digest String
    let arr_msg = &req.messages[3];
    assert!(
        arr_msg
            .text()
            .starts_with("[Tool Output Digest: id=call_arr")
    );

    // Verify non-string scalars were preserved without panic or corruption
    assert_eq!(req.messages[5].content, Some(Value::Null));
    assert_eq!(req.messages[7].content, Some(json!(false)));
    assert_eq!(req.messages[9].content, Some(json!(42)));
    assert_eq!(req.messages[11].content, Some(json!(3.15)));
}

#[test]
fn test_challenge_edge_case_small_tool_outputs_boundary() {
    // Exact boundary at MIN_TOOL_PRUNE_BYTES = 512:
    // <= 512 bytes must NOT be folded
    // > 512 bytes (e.g. 513) MUST be folded when in intermediate turns
    let mut req = ChatCompletionRequest {
        model: "auto".to_string(),
        messages: vec![
            make_system("Sys"),
            make_user("Small tools test"),
            make_assistant_call("c_0", "t"),
            make_tool("c_0", json!("")),
            make_assistant_call("c_1", "t"),
            make_tool("c_1", json!("a")),
            make_assistant_call("c_512", "t"),
            make_tool("c_512", json!("x".repeat(MIN_TOOL_PRUNE_BYTES))),
            make_assistant_call("c_513", "t"),
            make_tool("c_513", json!("y".repeat(MIN_TOOL_PRUNE_BYTES + 1))),
            // Large intermediate turn to ensure request exceeds token budget
            make_assistant_call("c_large", "t"),
            make_tool("c_large", json!("L".repeat(10_000))),
            // 4 tail turns
            make_assistant_call("t1", "t"),
            make_tool("t1", json!("tail")),
            make_assistant_call("t2", "t"),
            make_tool("t2", json!("tail")),
            make_assistant_call("t3", "t"),
            make_tool("t3", json!("tail")),
            make_assistant_call("t4", "t"),
            make_tool("t4", json!("tail")),
        ],
        ..Default::default()
    };

    // Target limit 1,000 tokens (budget 900). With 10,000 bytes in c_large, tokens are ~2,800 > 900.
    let _ = prune_request(&mut req, 1_000);

    // 0 bytes: not folded
    assert_eq!(req.messages[3].text(), "");
    // 1 byte: not folded
    assert_eq!(req.messages[5].text(), "a");
    // 512 bytes: not folded
    assert_eq!(req.messages[7].text(), "x".repeat(512));
    // 513 bytes: folded into digest
    assert!(
        req.messages[9]
            .text()
            .starts_with("[Tool Output Digest: id=c_513")
    );
}

// ---------------------------------------------------------------------------
// 3. Idempotency (2x, 3x repeated pruning & cascading limits)
// ---------------------------------------------------------------------------

#[test]
fn test_challenge_idempotency_deep_stress() {
    // 8 intermediate turns * 70KB = 560,000 bytes = ~140,000 tokens > 115,200 (128k budget)
    let mut req = build_challenge_session(8, 70_000, 4, 2_000);

    // Pass 1
    let (tokens_1, orig_1) = prune_request(&mut req, 128_000);
    assert!(
        tokens_1 < orig_1,
        "Pass 1 must reduce tokens: {tokens_1} vs {orig_1}"
    );
    let snapshot_1 = req.messages.clone();

    // Pass 2
    let (tokens_2, orig_2) = prune_request(&mut req, 128_000);
    assert_eq!(tokens_2, tokens_1, "Pass 2 tokens must match Pass 1");
    assert_eq!(orig_2, tokens_1);
    assert_eq!(
        req.messages, snapshot_1,
        "Pass 2 messages must be identical to Pass 1"
    );

    // Pass 3
    let (tokens_3, orig_3) = prune_request(&mut req, 128_000);
    assert_eq!(tokens_3, tokens_1, "Pass 3 tokens must match Pass 1");
    assert_eq!(orig_3, tokens_1);
    assert_eq!(
        req.messages, snapshot_1,
        "Pass 3 messages must be identical to Pass 1"
    );

    // Inspect every folded tool message to verify NO double-wrapping
    for msg in &req.messages {
        if msg.role == "tool" {
            let text = msg.text();
            if text.starts_with("[Tool Output Digest:") {
                let occurrences = text.matches("[Tool Output Digest:").count();
                assert_eq!(
                    occurrences, 1,
                    "Digest must never be double-wrapped: found {occurrences} in: {text}"
                );
            }
        }
    }
}

#[test]
fn test_challenge_idempotency_cascading_limits() {
    // 12 intermediate turns * 80KB = 960,000 bytes = ~240,000 tokens > 230,400 (256k budget)
    let mut req = build_challenge_session(12, 80_000, 4, 1_000);

    // 1. Prune for 256k limit
    let (tokens_256, orig) = prune_request(&mut req, 256_000);
    assert!(tokens_256 < orig);

    // 2. Cascading prune same request for 128k limit
    let (tokens_128, _) = prune_request(&mut req, 128_000);

    assert!(
        tokens_128 <= tokens_256,
        "Cascading pruning tokens must decrease or remain equal: 128k ({tokens_128}) vs 256k ({tokens_256})"
    );

    // Check no double wrapping
    for msg in &req.messages {
        if msg.role == "tool" {
            let text = msg.text();
            if text.starts_with("[Tool Output Digest:") {
                assert_eq!(
                    text.matches("[Tool Output Digest:").count(),
                    1,
                    "No double wrapping under cascading limits"
                );
            }
        }
    }
}

// ---------------------------------------------------------------------------
// 4. Arithmetic and tokens_compressed Invariants
// ---------------------------------------------------------------------------

#[test]
fn test_challenge_tokens_compressed_invariants() {
    // Case A: Normal pruning scenario (6 * 80KB = 480KB = ~120k tokens > 115.2k budget)
    let mut req_a = build_challenge_session(6, 80_000, 4, 1_000);
    let (new_a, orig_a) = prune_request(&mut req_a, 128_000);
    let comp_a = orig_a.saturating_sub(new_a);
    assert!(comp_a > 0);
    assert_eq!(comp_a as u64, orig_a as u64 - new_a as u64);

    // Case B: No-op scenario (request already within budget)
    let mut req_b = build_challenge_session(1, 1_000, 2, 500);
    let (new_b, orig_b) = prune_request(&mut req_b, 128_000);
    let comp_b = orig_b.saturating_sub(new_b);
    assert_eq!(comp_b, 0);

    // Case C: Small tool outputs (260 bytes) where digest (~480 bytes) is larger than original
    let mut req_c = ChatCompletionRequest {
        model: "auto".to_string(),
        messages: vec![
            make_system("Sys"),
            make_user("Task"),
            make_assistant_call("c0", "t"),
            make_tool("c0", json!("X".repeat(260))),
            make_assistant_call("t1", "t"),
            make_tool("t1", json!("tail")),
            make_assistant_call("t2", "t"),
            make_tool("t2", json!("tail")),
            make_assistant_call("t3", "t"),
            make_tool("t3", json!("tail")),
            make_assistant_call("t4", "t"),
            make_tool("t4", json!("tail")),
        ],
        ..Default::default()
    };
    let (new_c, orig_c) = prune_request(&mut req_c, 50);
    let comp_c = orig_c.saturating_sub(new_c);
    // Even if new_c >= orig_c, saturating_sub ensures it never panics and is >= 0
    assert_eq!(comp_c, 0);
}

#[test]
fn test_challenge_utf8_boundary_slicing_adversarial() {
    // Generate tricky UTF-8 boundary characters directly at DIGEST_HEAD_BYTES (160)
    // and DIGEST_TAIL_BYTES (160).
    for pad in 150..165 {
        let mut text = "a".repeat(pad);
        text.push_str("🦀🎉Привет Мир! 🌍🔥"); // 4-byte and 2-byte characters
        text.push_str(&"x".repeat(500));

        let folded = fold_tool_content(&text, "call_utf8_adv", false);
        assert!(folded.starts_with("[Tool Output Digest: id=call_utf8_adv"));
        assert!(folded.ends_with(']'));
    }
}

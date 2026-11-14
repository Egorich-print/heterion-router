//! Empirical Challenge Suite for Milestone 2: Zero-Quota Algorithmic Context Pruning.
//!
//! Author: challenger_m2_1 (Adversarial Critic & Domain Specialist)
//!
//! Verification targets:
//! 1. Microsecond Latency Benchmark: `prune_request` < 50µs (< 0.05ms) in release mode under 50-turn and 100-turn agent conversations.
//! 2. UTF-8 Slicing Boundary Stress: Exhaustive boundary testing with Cyrillic (2B), CJK (3B), Emoji (4B), and ZWJ sequences across 160-byte head and tail boundaries.
//! 3. Message Invariants: 100% message count, exact ordering, system/goal preservation, active tail preservation, tool_call_id parity.
//! 4. Extreme payloads & Edge Cases: 5MB payloads, exact boundary lengths (512/513 bytes), newline stress, array flattener safety, and idempotency.

use heterion_router_core::{ChatCompletionRequest, ChatMessage};
use heterion_router_gateway::context_pruner::{
    MIN_TOOL_PRUNE_BYTES, fast_estimate_tokens, fold_tool_content, fold_tool_string_in_place,
    partition_conversation, prune_request,
};
use serde_json::{Value, json};
use std::time::{Duration, Instant};

// ---------------------------------------------------------------------------
// Helpers
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
                    "arguments": "{\"query\":\"empirical_challenge\"}"
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

fn build_turn_session(
    intermediate_turns: usize,
    intermediate_bytes: usize,
    tail_turns: usize,
    tail_bytes: usize,
) -> ChatCompletionRequest {
    let mut messages = Vec::with_capacity(2 + (intermediate_turns + tail_turns) * 2);

    messages.push(make_system_msg(
        "You are an autonomous engineering agent under empirical stress testing.",
    ));
    messages.push(make_user_msg(
        "Execute end-to-end benchmark and validation mission.",
    ));

    for i in 0..intermediate_turns {
        let call_id = format!("call_interm_{i}");
        messages.push(make_assistant_tool_call(&call_id, "bash_exec"));
        let body = format!(
            "step_{i}_header\n{}\nstep_{i}_footer",
            "x".repeat(intermediate_bytes.saturating_sub(40))
        );
        messages.push(make_tool_msg(&call_id, json!(body)));
    }

    for i in 0..tail_turns {
        let call_id = format!("call_tail_{i}");
        messages.push(make_assistant_tool_call(&call_id, "read_log"));
        let body = format!(
            "tail_{i}_content\n{}",
            "y".repeat(tail_bytes.saturating_sub(20))
        );
        messages.push(make_tool_msg(&call_id, json!(body)));
    }

    ChatCompletionRequest {
        model: "heterion-router/auto".to_string(),
        messages,
        ..Default::default()
    }
}

// ---------------------------------------------------------------------------
// 1. Latency Benchmarks (50-Turn & 100-Turn Conversations)
// ---------------------------------------------------------------------------

#[test]
fn test_empirical_latency_benchmark_50_turns() {
    // 50 turns: 46 intermediate (15KB each = ~690KB raw payload) + 4 tail turns (1KB each)
    let template = build_turn_session(46, 15_000, 4, 1_000);
    assert_eq!(template.messages.len(), 2 + 50 * 2); // 102 messages total

    // Warm-up pass
    let mut warmup = template.clone();
    let _ = prune_request(&mut warmup, 128_000);

    // Run 100 iterations measuring pure execution time
    const ITERATIONS: usize = 100;
    let mut durations = Vec::with_capacity(ITERATIONS);

    for _ in 0..ITERATIONS {
        let mut req = template.clone();
        let start = Instant::now();
        let (pruned_tokens, orig_tokens) = prune_request(&mut req, 128_000);
        let elapsed = start.elapsed();
        durations.push(elapsed);
        assert!(pruned_tokens < orig_tokens);
    }

    durations.sort();
    let median = durations[ITERATIONS / 2];
    let min = durations[0];
    let max = durations[ITERATIONS - 1];
    let p90 = durations[(ITERATIONS as f64 * 0.90) as usize];

    eprintln!("\n[BENCHMARK 50-TURN] min: {min:?}, median: {median:?}, p90: {p90:?}, max: {max:?}");

    #[cfg(not(debug_assertions))]
    {
        // Enforce strictly < 50µs (< 0.05ms) in release mode
        assert!(
            median < std::time::Duration::from_micros(50),
            "Median latency for 50-turn session exceeded 50µs threshold: {median:?}"
        );
    }
}

#[test]
fn test_empirical_latency_benchmark_100_turns() {
    // 100 turns: 96 intermediate (10KB each = ~960KB raw payload) + 4 tail turns (1KB each)
    let template = build_turn_session(96, 10_000, 4, 1_000);
    assert_eq!(template.messages.len(), 2 + 100 * 2); // 202 messages total

    // Warm-up pass
    let mut warmup = template.clone();
    let _ = prune_request(&mut warmup, 128_000);

    // Run 100 iterations measuring pure execution time
    const ITERATIONS: usize = 100;
    let mut durations = Vec::with_capacity(ITERATIONS);

    for _ in 0..ITERATIONS {
        let mut req = template.clone();
        let start = Instant::now();
        let (pruned_tokens, orig_tokens) = prune_request(&mut req, 128_000);
        let elapsed = start.elapsed();
        durations.push(elapsed);
        assert!(pruned_tokens < orig_tokens);
    }

    durations.sort();
    let median = durations[ITERATIONS / 2];
    let min = durations[0];
    let max = durations[ITERATIONS - 1];
    let p90 = durations[(ITERATIONS as f64 * 0.90) as usize];

    eprintln!(
        "\n[BENCHMARK 100-TURN] min: {min:?}, median: {median:?}, p90: {p90:?}, max: {max:?}"
    );

    #[cfg(not(debug_assertions))]
    {
        // Enforce strictly < 50µs (< 0.05ms) in release mode
        assert!(
            median < std::time::Duration::from_micros(50),
            "Median latency for 100-turn session exceeded 50µs threshold: {median:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// 2. UTF-8 Slicing Boundary Stress & Fuzzing
// ---------------------------------------------------------------------------

#[test]
fn test_stress_utf8_boundary_sliding_window() {
    // We construct test strings where multi-byte UTF-8 sequences are placed across
    // every offset around DIGEST_HEAD_BYTES (160) and DIGEST_TAIL_BYTES (160).
    // Test characters:
    // - 2-byte: 'я', 'Ш', 'é'
    // - 3-byte: '中', '文', '語', '한'
    // - 4-byte: '🦀', '🚀', '🎉', '🧠'
    // - Complex ZWJ: "👨‍👩‍👧‍👦" (18 bytes), "🏳️‍🌈" (6 bytes)

    let test_chars = [
        "я",
        "Ш",
        "é",
        "中",
        "文",
        "語",
        "한",
        "🦀",
        "🚀",
        "🎉",
        "🧠",
        "👨‍👩‍👧‍👦",
        "🏳️‍🌈",
    ];

    for &probe in &test_chars {
        // Vary prefix length from 140 to 180 bytes (crossing 160-byte head boundary)
        for prefix_len in 140..=180 {
            // Vary total length from 600 to 1000 bytes (crossing tail boundary above 512-byte threshold)
            for total_target in [600, 700, 800, 900, 1000] {
                let mut s = String::with_capacity(total_target + 50);
                s.push_str(&"a".repeat(prefix_len));
                s.push_str(probe);

                // Pad up to total target
                if s.len() < total_target {
                    let rem = total_target - s.len();
                    s.push_str(&"b".repeat(rem));
                }

                // Append probe at the tail boundary as well
                s.push_str(probe);
                s.push_str(&"c".repeat(50));

                let call_id = "call_fuzz_utf8";
                let folded = fold_tool_content(&s, call_id, false);

                // Zero panics assertion
                assert!(
                    folded.starts_with("[Tool Output Digest: id=call_fuzz_utf8"),
                    "Folded string corrupted header"
                );
                assert!(folded.ends_with(']'), "Folded string corrupted footer");

                // Valid UTF-8 string integrity: must be valid str with proper slices
                assert!(folded.contains("Head:\n"));
                assert!(folded.contains("Tail:\n"));
            }
        }
    }
}

#[test]
fn test_stress_utf8_pure_cjk_and_cyrillic_payloads() {
    // Pure Cyrillic string of varying lengths
    for count in [150, 200, 300, 1000] {
        let cyrillic_raw =
            "Привет, мир! Это тестовая строка для проверки алгоритмического сжатия. ".repeat(count);
        let folded = fold_tool_content(&cyrillic_raw, "call_cyrillic", false);
        assert!(folded.contains("[Tool Output Digest: id=call_cyrillic"));
        assert!(folded.contains("... [omitted "));
    }

    // Pure CJK string of varying lengths
    for count in [100, 200, 300, 800] {
        let cjk_raw =
            "这是一段用于测试异构路由器算法压缩的中文长文本，包含多字节字符。".repeat(count);
        let folded = fold_tool_content(&cjk_raw, "call_cjk", false);
        assert!(folded.contains("[Tool Output Digest: id=call_cjk"));
        assert!(folded.contains("... [omitted "));
    }

    // Pure Emoji string of varying lengths
    for count in [50, 100, 200, 500] {
        let emoji_raw = "🦀🚀🔥🎉🧠🤖💻⚡🛡️📦".repeat(count);
        let folded = fold_tool_content(&emoji_raw, "call_emoji", false);
        assert!(folded.contains("[Tool Output Digest: id=call_emoji"));
        assert!(folded.contains("... [omitted "));
    }
}

// ---------------------------------------------------------------------------
// 3. Invariant Verification (100% Messages, Order, Goals, Graph Topology)
// ---------------------------------------------------------------------------

#[test]
fn test_empirical_invariants_preserved_across_all_turns() {
    let mut req = build_turn_session(20, 25_000, 4, 2_000);
    let original_count = req.messages.len();
    assert_eq!(original_count, 50);

    let orig_system = req.messages[0].clone();
    let orig_user_goal = req.messages[1].clone();
    let orig_tail_messages = req.messages[42..].to_vec(); // Last 8 messages (4 turns)

    // Execute pruning
    let (pruned_tokens, orig_tokens) = prune_request(&mut req, 128_000);
    assert!(pruned_tokens < orig_tokens);

    // Invariant 1: 100% of messages preserved in quantity
    assert_eq!(
        req.messages.len(),
        original_count,
        "Message count mutated! Messages must never be deleted"
    );

    // Invariant 2: System prompt untouched
    assert_eq!(
        req.messages[0], orig_system,
        "System prompt was mutated during pruning"
    );

    // Invariant 3: Initial user goal untouched
    assert_eq!(
        req.messages[1], orig_user_goal,
        "Initial user goal was mutated during pruning"
    );

    // Invariant 4: Active tail (4 turns = 8 messages) preserved verbatim
    assert_eq!(
        &req.messages[42..],
        orig_tail_messages.as_slice(),
        "Active dialogue tail was mutated during Pass 1"
    );

    // Invariant 5: 1-to-1 tool_call_id graph parity for all 20 intermediate turns
    for turn_idx in 0..20 {
        let assistant_idx = 2 + turn_idx * 2;
        let tool_idx = assistant_idx + 1;

        let assistant_msg = &req.messages[assistant_idx];
        let tool_msg = &req.messages[tool_idx];

        assert_eq!(assistant_msg.role, "assistant");
        assert_eq!(tool_msg.role, "tool");

        let call_id = format!("call_interm_{turn_idx}");
        assert_eq!(tool_msg.tool_call_id, Some(call_id.clone()));

        // Tool output must be formatted as structured digest
        let text = tool_msg.text();
        assert!(
            text.starts_with(&format!("[Tool Output Digest: id={call_id}")),
            "Tool digest header missing at turn {turn_idx}"
        );
    }
}

// ---------------------------------------------------------------------------
// 4. Extreme Payloads & Boundary Edge Cases
// ---------------------------------------------------------------------------

#[test]
fn test_extreme_payload_5mb_tool_output() {
    let five_mb = "X".repeat(5 * 1024 * 1024);
    let mut s = five_mb;
    let folded_len = fold_tool_string_in_place(&mut s, "call_5mb", false);

    assert!(folded_len < 1000, "Folded 5MB string must be < 1000 bytes");
    assert!(s.contains("5242880 bytes"));
    assert!(s.contains("... [omitted "));
}

#[test]
fn test_exact_boundary_lengths_512_and_513_bytes() {
    // Exactly MIN_TOOL_PRUNE_BYTES (512 bytes) -> must NOT be folded
    let exact_512 = "A".repeat(MIN_TOOL_PRUNE_BYTES);
    let folded_512 = fold_tool_content(&exact_512, "call_512", false);
    assert_eq!(
        folded_512, exact_512,
        "Tool outputs <= 512 bytes must not be folded"
    );

    // Exactly 513 bytes (MIN_TOOL_PRUNE_BYTES + 1) -> MUST be folded
    let exact_513 = "B".repeat(MIN_TOOL_PRUNE_BYTES + 1);
    let folded_513 = fold_tool_content(&exact_513, "call_513", false);
    assert!(
        folded_513.starts_with("[Tool Output Digest: id=call_513"),
        "Tool output of 513 bytes must be folded"
    );
    assert!(folded_513.contains("513 bytes"));
}

#[test]
fn test_newline_stress_10000_newlines() {
    // 10,000 consecutive newlines
    let newlines = "\n".repeat(10_000);
    let folded = fold_tool_content(&newlines, "call_newlines", false);

    assert!(folded.contains("10001 lines"));
    assert!(folded.contains("10000 bytes"));
}

#[test]
fn test_zero_newlines_large_payload() {
    // Large payload without a single newline
    let no_newlines = "a".repeat(100_000);
    let folded = fold_tool_content(&no_newlines, "call_no_nl", false);

    assert!(folded.contains("1 lines"));
    assert!(folded.contains("100000 bytes"));
}

#[test]
fn test_repeated_pruning_idempotency_10_passes() {
    let mut req = build_turn_session(10, 20_000, 4, 1_000);
    let (pass1_pruned, _pass1_orig) = prune_request(&mut req, 128_000);

    for pass in 2..=10 {
        let (tokens, orig) = prune_request(&mut req, 128_000);
        assert_eq!(
            tokens, pass1_pruned,
            "Pass {pass} yielded different token count"
        );
        assert_eq!(orig, tokens, "Pass {pass} reported differing orig tokens");
    }
}
#[test]
fn test_breakdown_profiler() {
    let template = build_turn_session(46, 15_000, 4, 1_000);

    // 1. Measure fast_estimate_tokens on raw request
    let t0 = Instant::now();
    for _ in 0..100 {
        let _ = fast_estimate_tokens(&template);
    }
    let elapsed_estimate = t0.elapsed() / 100;

    // 2. Measure partition_conversation
    let t0 = Instant::now();
    for _ in 0..100 {
        let _ = partition_conversation(&template.messages, 4);
    }
    let elapsed_partition = t0.elapsed() / 100;

    // 3. Measure folding 46 tool strings
    let req = template.clone();
    let partition = partition_conversation(&req.messages, 4);
    let t0 = Instant::now();
    for _ in 0..100 {
        let mut clone_req = req.clone();
        for i in 0..partition.tail_cutoff_idx {
            if clone_req.messages[i].role == "tool"
                && let Some(Value::String(s)) = clone_req.messages[i].content.as_mut()
            {
                fold_tool_string_in_place(s, "call_id", false);
            }
        }
    }
    let elapsed_fold_with_clone = t0.elapsed() / 100;

    eprintln!(
        "\n[PROFILE BREAKDOWN 50-TURN]\n- fast_estimate_tokens (raw): {elapsed_estimate:?}\n- partition_conversation: {elapsed_partition:?}\n- fold 46 strings (incl clone): {elapsed_fold_with_clone:?}"
    );
}
#[test]
fn test_measure_is_tool_error() {
    let template = build_turn_session(46, 15_000, 4, 1_000);
    let mut called_inspector = 0;
    let t0 = Instant::now();
    for msg in &template.messages {
        if msg.role == "tool" {
            // Check if contains_err_marker triggered
            if let Some(Value::String(s)) = &msg.content {
                let bytes = s.as_bytes();
                let len = bytes.len();
                let _head = &bytes[..2048.min(len)];
                let _tail = &bytes[len.saturating_sub(2048)..];
                // Check if head or tail triggered
                if s.contains("err")
                    || s.contains("exit")
                    || s.contains("fail")
                    || s.contains("panic")
                {
                    called_inspector += 1;
                }
            }
        }
    }
    eprintln!(
        "\ncalled_inspector: {called_inspector}, elapsed: {:?}",
        t0.elapsed()
    );
}
#[test]
fn test_measure_prune_request_internal_breakdown() {
    let template = build_turn_session(46, 15_000, 4, 1_000);

    let mut t_est1 = Duration::ZERO;
    let mut t_part = Duration::ZERO;
    let mut t_loop1 = Duration::ZERO;
    let mut t_est2 = Duration::ZERO;
    let mut t_pass2 = Duration::ZERO;

    for _ in 0..100 {
        let mut req = template.clone();

        let s1 = Instant::now();
        let original_tokens = fast_estimate_tokens(&req);
        let budget = (128_000.0 * 0.90) as u32;
        t_est1 += s1.elapsed();

        if original_tokens <= budget {
            continue;
        }

        let s2 = Instant::now();
        let partition = partition_conversation(&req.messages, 4);
        let goal_idx = partition.initial_user_goal_idx;
        let tail_idx = partition.tail_cutoff_idx;
        t_part += s2.elapsed();

        let s3 = Instant::now();
        let mut modified = false;
        for i in 0..tail_idx {
            if Some(i) == goal_idx {
                continue;
            }
            if req.messages[i].role == "system" || req.messages[i].role == "developer" {
                continue;
            }
            if req.messages[i].role != "tool" {
                continue;
            }

            let msg = &mut req.messages[i];
            let call_id = msg.tool_call_id.as_deref().unwrap_or_default();
            // Let's call the actual fold
            if let Some(content) = msg.content.as_mut()
                && let Value::String(s) = content
            {
                let old_len = s.len();
                let new_len = fold_tool_string_in_place(s, call_id, false);
                if new_len < old_len {
                    modified = true;
                }
            }
        }
        t_loop1 += s3.elapsed();

        let s4 = Instant::now();
        let current_tokens = if modified {
            fast_estimate_tokens(&req)
        } else {
            original_tokens
        };
        t_est2 += s4.elapsed();

        let s5 = Instant::now();
        if current_tokens > budget {
            // pass 2
        }
        t_pass2 += s5.elapsed();
    }

    eprintln!(
        "\n[INTERNAL BREAKDOWN (avg of 100 runs)]\n- t_est1: {:?}\n- t_part: {:?}\n- t_loop1: {:?}\n- t_est2: {:?}\n- t_pass2: {:?}",
        t_est1 / 100,
        t_part / 100,
        t_loop1 / 100,
        t_est2 / 100,
        t_pass2 / 100,
    );
}
#[test]
fn test_exact_prune_request_profiler() {
    let template = build_turn_session(46, 15_000, 4, 1_000);

    let mut total_duration = Duration::ZERO;
    let mut min = Duration::MAX;
    let mut max = Duration::ZERO;

    for _ in 0..100 {
        let mut req = template.clone();
        let start = Instant::now();
        let _ = prune_request(&mut req, 128_000);
        let el = start.elapsed();
        total_duration += el;
        if el < min {
            min = el;
        }
        if el > max {
            max = el;
        }
    }

    let avg = total_duration / 100;
    eprintln!("\n[EXACT PRUNE REQUEST 50-TURN] avg: {avg:?}, min: {min:?}, max: {max:?}");
}

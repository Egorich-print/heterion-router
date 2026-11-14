//! Empirical Challenge Test Suite for Milestone 2 Iteration 2 (m2_it2).
//!
//! Author: challenger_m2_it2_1 (EMPIRICAL CHALLENGER)
//! Verification target:
//! 1. Empirical proof of non-expansion: N' <= N always, and whenever folding occurs, N' < N strictly.
//!    Payloads tested across 0..4096 bytes with varied tool_call_id lengths and statuses.
//! 2. Zero content duplication / disjoint slicing verification.
//! 3. Latency profiling across 10, 50, and 100+ turns.

use heterion_router_core::{ChatCompletionRequest, ChatMessage};
use heterion_router_gateway::context_pruner::{
    DIGEST_HEAD_BYTES, DIGEST_TAIL_BYTES, MIN_TOOL_PRUNE_BYTES, fold_tool_content,
    fold_tool_string_in_place, prune_request,
};
use serde_json::json;
use std::time::Instant;

#[test]
fn test_empirical_non_expansion_exhaustive_0_to_4096() {
    let tool_call_ids = [
        "",
        "c",
        "call_1234",
        "call_intermediate_turn_0",
        "call_uuid_like_12345678-1234-5678-1234-567812345678",
        &"x".repeat(100),
        &"y".repeat(256),
    ];

    let statuses = [false, true];

    // Exhaustive test across 0..4096 bytes
    for n in 0..=4096 {
        // Test with simple ASCII repeated characters
        let raw_ascii = "a".repeat(n);
        // Test with multi-line ASCII
        let raw_multiline = {
            let mut s = String::with_capacity(n);
            let mut cur = 0;
            while cur < n {
                let chunk_len = (n - cur).min(32);
                s.push_str(&"m".repeat(chunk_len.saturating_sub(1)));
                if s.len() < n {
                    s.push('\n');
                }
                cur = s.len();
            }
            s
        };

        for raw in [&raw_ascii, &raw_multiline] {
            for &is_err in &statuses {
                for &id in &tool_call_ids {
                    let mut s = raw.clone();
                    let folded_len = fold_tool_string_in_place(&mut s, id, is_err);

                    assert_eq!(folded_len, s.len(), "Returned len must match s.len()");

                    // Invariant 1: Non-expansion (N' <= N always)
                    assert!(
                        folded_len <= n,
                        "CRITICAL: Expansion detected! n={n}, folded_len={folded_len}, id_len={}, is_err={is_err}",
                        id.len()
                    );

                    // Invariant 2: Strict shrinkage when folding occurs
                    if s.starts_with("[Tool Output Digest:") {
                        assert!(
                            folded_len < n,
                            "CRITICAL: Folding occurred but string did not strictly shrink! n={n}, folded_len={folded_len}"
                        );
                        // Check header contains required fields
                        assert!(s.contains("Head:\n"));
                        assert!(s.contains("Tail:\n"));
                        assert!(s.contains("... [omitted "));
                    } else {
                        // Invariant 3: If not folded, must remain byte-for-byte identical
                        assert_eq!(
                            folded_len, n,
                            "Non-folded string length changed! expected={n}, got={folded_len}"
                        );
                        assert_eq!(s, *raw, "Non-folded string content altered! n={n}");
                    }
                }
            }
        }
    }
}

#[test]
fn test_empirical_key_boundary_payload_sizes() {
    // Test key boundary sizes specifically: 256, 320, 511, 512, 513, 768, 1024, 2048
    let key_sizes = [
        0, 1, 15, 64, 128, 255, 256, 319, 320, 321, 384, 511, 512, 513, 514, 600, 768, 1024, 2048,
        4096,
    ];

    for &size in &key_sizes {
        let raw = "K".repeat(size);
        for &is_err in &[false, true] {
            for id in ["call_1", "call_long_id_test_1234567890"] {
                let folded = fold_tool_content(&raw, id, is_err);
                let folded_len = folded.len();

                assert!(
                    folded_len <= size,
                    "Size {size} expanded to {folded_len} for id={id}, is_err={is_err}"
                );

                if size <= MIN_TOOL_PRUNE_BYTES {
                    assert_eq!(
                        folded_len, size,
                        "Size {size} <= MIN_TOOL_PRUNE_BYTES (512) was modified!"
                    );
                    assert_eq!(folded, raw);
                } else if folded.starts_with("[Tool Output Digest:") {
                    assert!(
                        folded_len < size,
                        "Size {size} folded to {folded_len} which is not strictly smaller!"
                    );
                }
            }
        }
    }
}

#[test]
fn test_zero_content_duplication_disjoint_slices() {
    // Test that for all folded strings, Head slice and Tail slice NEVER overlap or duplicate
    for total_bytes in (MIN_TOOL_PRUNE_BYTES + 1)..=2048 {
        // Construct string with unique position markers e.g. [P:000000] [P:000001]
        let mut raw = String::with_capacity(total_bytes);
        let mut idx = 0;
        while raw.len() < total_bytes {
            let tag = format!("[P:{:06}]", idx);
            if raw.len() + tag.len() <= total_bytes {
                raw.push_str(&tag);
            } else {
                let rem = total_bytes - raw.len();
                raw.push_str(&"X".repeat(rem));
                break;
            }
            idx += 1;
        }

        let folded = fold_tool_content(&raw, "call_disjoint", false);
        if folded.starts_with("[Tool Output Digest:") {
            let head_marker = "Head:\n";
            let omitted_marker = "\n... [omitted ";
            let tail_marker = "Tail:\n";

            let head_start = folded.find(head_marker).unwrap() + head_marker.len();
            let head_end = folded.find(omitted_marker).unwrap();
            let head_text = &folded[head_start..head_end];

            let tail_start = folded.find(tail_marker).unwrap() + tail_marker.len();
            let tail_end = folded.rfind(']').unwrap();
            let tail_text = &folded[tail_start..tail_end];

            // Verify head and tail lengths do not exceed limits
            assert!(head_text.len() <= DIGEST_HEAD_BYTES);
            assert!(tail_text.len() <= DIGEST_TAIL_BYTES);

            // Verify no marker tag appears in both head and tail
            for i in 0..idx {
                let tag = format!("[P:{:06}]", i);
                let in_head = head_text.contains(&tag);
                let in_tail = tail_text.contains(&tag);
                assert!(
                    !(in_head && in_tail),
                    "DUPLICATION DETECTED: Tag {tag} found in BOTH head and tail at total_bytes={total_bytes}"
                );
            }
        }
    }
}

fn build_bench_session(
    intermediate_turns: usize,
    bytes_per_tool: usize,
    tail_turns: usize,
    bytes_per_tail: usize,
) -> ChatCompletionRequest {
    let mut messages = Vec::with_capacity(2 + (intermediate_turns + tail_turns) * 2);
    messages.push(ChatMessage::plain(
        "system",
        "You are an autonomous engineering system under latency audit.",
    ));
    messages.push(ChatMessage::plain(
        "user",
        "Perform large scale simulation and audit.",
    ));

    for i in 0..intermediate_turns {
        let call_id = format!("call_bench_{i}");
        messages.push(ChatMessage {
            role: "assistant".to_string(),
            content: None,
            reasoning_content: None,
            tool_calls: Some(json!([{
                "id": call_id,
                "type": "function",
                "function": { "name": "bash", "arguments": "{}" }
            }])),
            tool_call_id: None,
            name: None,
        });
        messages.push(ChatMessage {
            role: "tool".to_string(),
            content: Some(json!("A".repeat(bytes_per_tool))),
            reasoning_content: None,
            tool_calls: None,
            tool_call_id: Some(call_id),
            name: None,
        });
    }

    for i in 0..tail_turns {
        let call_id = format!("call_tail_{i}");
        messages.push(ChatMessage {
            role: "assistant".to_string(),
            content: None,
            reasoning_content: None,
            tool_calls: Some(json!([{
                "id": call_id,
                "type": "function",
                "function": { "name": "tail_bash", "arguments": "{}" }
            }])),
            tool_call_id: None,
            name: None,
        });
        messages.push(ChatMessage {
            role: "tool".to_string(),
            content: Some(json!("T".repeat(bytes_per_tail))),
            reasoning_content: None,
            tool_calls: None,
            tool_call_id: Some(call_id),
            name: None,
        });
    }

    ChatCompletionRequest {
        model: "heterion-router/auto".to_string(),
        messages,
        ..Default::default()
    }
}

#[test]
fn test_latency_empirical_measurement_profile() {
    // Measure latency across 6 turns (~480KB), 50 turns (~500KB), and 100 turns (~1MB)
    let configs = [
        ("6-turn-480KB", 6, 80_000, 4, 2_000),
        ("50-turn-500KB", 46, 10_800, 4, 1_000),
        ("100-turn-1MB", 96, 10_400, 4, 1_000),
    ];

    const SAMPLES: usize = 50;

    for (label, interm_turns, tool_bytes, tail_turns, tail_bytes) in configs {
        let template = build_bench_session(interm_turns, tool_bytes, tail_turns, tail_bytes);

        // Warmup
        let mut warmup = template.clone();
        let _ = prune_request(&mut warmup, 128_000);

        let mut durations = Vec::with_capacity(SAMPLES);
        for _ in 0..SAMPLES {
            let mut req = template.clone();
            let t0 = Instant::now();
            let _ = prune_request(&mut req, 128_000);
            durations.push(t0.elapsed());
        }

        durations.sort();
        let min = durations[0];
        let median = durations[SAMPLES / 2];
        let p90 = durations[(SAMPLES as f64 * 0.9) as usize];
        let max = durations[SAMPLES - 1];

        eprintln!(
            "[EMPIRICAL LATENCY AUDIT: {label}] min={min:?}, median={median:?}, p90={p90:?}, max={max:?}"
        );
    }
}

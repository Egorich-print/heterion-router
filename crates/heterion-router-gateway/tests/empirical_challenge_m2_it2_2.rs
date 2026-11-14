//! Empirical Challenge Test Suite for Milestone 2 Iteration 2
//! Zero-Quota Algorithmic Context Pruning Remediation.
//!
//! Author: challenger_m2_it2_2 (EMPIRICAL CHALLENGER)
//! Verification Scope:
//! 1. UTF-8 Boundary Slicing on Multi-byte Sequences (2-byte Cyrillic, 3-byte CJK, 4-byte Emoji, ZWJ)
//!    positioned exactly across `head_end` (160B) and `tail_start` boundaries.
//! 2. SWAR Line Counter Regression & Full Byte-by-Byte Oracle Parity:
//!    `\n\v`, `\n\r`, `\n\t`, binary sequences `0x00..=0xFF`, repeated newlines, trailing newlines, no newlines.
//! 3. Preservation of Structural Invariants, Non-Expansion, and Idempotency.

use heterion_router_core::{ChatCompletionRequest, ChatMessage};
use heterion_router_gateway::context_pruner::{
    DIGEST_HEAD_BYTES, DIGEST_TAIL_BYTES, MIN_TOOL_PRUNE_BYTES, fold_tool_content,
    fold_tool_string_in_place, prune_request,
};
use serde_json::json;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Parses the line count from a structured tool output digest:
/// `"[Tool Output Digest: id=... | <line_count> lines, <bytes> bytes | ..."`
fn parse_digest_line_count(digest: &str) -> Option<usize> {
    let bar_idx = digest.find(" | ")?;
    let rest = &digest[bar_idx + 3..];
    let lines_idx = rest.find(" lines, ")?;
    rest[..lines_idx].parse::<usize>().ok()
}

/// Parses the byte count from a structured tool output digest
fn parse_digest_byte_count(digest: &str) -> Option<usize> {
    let lines_idx = digest.find(" lines, ")?;
    let rest = &digest[lines_idx + 8..];
    let bytes_idx = rest.find(" bytes | ")?;
    rest[..bytes_idx].parse::<usize>().ok()
}

/// Extracts the Head and Tail sections from a structured digest
fn extract_head_tail(digest: &str) -> (&str, &str) {
    let head_marker = "\nHead:\n";
    let omitted_marker = "\n... [omitted ";
    let tail_marker = " bytes of intermediate tool output] ...\nTail:\n";

    let head_start = digest.find(head_marker).expect("Head marker missing") + head_marker.len();
    let head_end = digest.find(omitted_marker).expect("Omitted marker missing");

    let tail_start = digest.find(tail_marker).expect("Tail marker missing") + tail_marker.len();
    let tail_end = digest.rfind(']').expect("Ending bracket missing");

    (&digest[head_start..head_end], &digest[tail_start..tail_end])
}

// ---------------------------------------------------------------------------
// 1. Empirical UTF-8 Boundary Slicing Stress
// ---------------------------------------------------------------------------

#[test]
fn test_utf8_head_boundary_exact_crossings() {
    // DIGEST_HEAD_BYTES = 160.
    // We test characters crossing the 160-byte boundary:
    // - 2-byte Cyrillic: spans [159..161]
    // - 3-byte CJK: spans [158..161] and [159..162]
    // - 4-byte Emoji: spans [157..161], [158..162], and [159..163]

    let test_cases = [
        // (prefix_len, probe, expected_head_end)
        // 2-byte Cyrillic 'я' (2 bytes)
        (159, "я", 159), // [159..161] crosses 160 -> floor is 159
        (160, "я", 160), // [160..162] starts at 160 -> floor is 160
        // 3-byte CJK '中' (3 bytes)
        (158, "中", 158), // [158..161] crosses 160 -> floor is 158
        (159, "中", 159), // [159..162] crosses 160 -> floor is 159
        (160, "中", 160), // [160..163] starts at 160 -> floor is 160
        // 4-byte Emoji '🦀' (4 bytes)
        (157, "🦀", 157), // [157..161] crosses 160 -> floor is 157
        (158, "🦀", 158), // [158..162] crosses 160 -> floor is 158
        (159, "🦀", 159), // [159..163] crosses 160 -> floor is 159
        (160, "🦀", 160), // [160..164] starts at 160 -> floor is 160
    ];

    for (prefix_len, probe, expected_head_end) in test_cases {
        let mut s = "A".repeat(prefix_len);
        s.push_str(probe);
        // Pad with ASCII to exceed MIN_TOOL_PRUNE_BYTES (512) and ensure folding
        s.push_str(&"B".repeat(600));

        let folded = fold_tool_content(&s, "call_utf8_head", false);
        assert!(
            folded.starts_with("[Tool Output Digest: id=call_utf8_head"),
            "Failed header for probe {probe} at prefix {prefix_len}"
        );

        let (head, _tail) = extract_head_tail(&folded);
        assert_eq!(
            head.len(),
            expected_head_end,
            "Head length mismatch for probe '{probe}' with prefix {prefix_len}: expected {expected_head_end}, got {}",
            head.len()
        );

        // Crucial: The head slice MUST be a valid UTF-8 substring and NEVER split the probe
        assert!(
            head.is_char_boundary(head.len()),
            "Head slice is not at a char boundary"
        );
        if prefix_len < expected_head_end {
            assert!(head.ends_with(probe));
        } else {
            assert!(!head.ends_with(probe));
        }
    }
}

#[test]
fn test_utf8_tail_boundary_exact_crossings() {
    // DIGEST_TAIL_BYTES = 160.
    // If total_len = 800, tail_target = 800 - 160 = 640.
    // We position multi-byte probes to cross offset 640:
    // - 2-byte probe at 639..641 -> ceil is 641, tail_len = 800 - 641 = 159
    // - 3-byte probe at 638..641 -> ceil is 641, tail_len = 159
    // - 3-byte probe at 639..642 -> ceil is 642, tail_len = 158
    // - 4-byte probe at 637..641 -> ceil is 641, tail_len = 159
    // - 4-byte probe at 638..642 -> ceil is 642, tail_len = 158
    // - 4-byte probe at 639..643 -> ceil is 643, tail_len = 157

    let total_len: usize = 800;

    let test_cases = [
        // (probe_start, probe, expected_tail_start)
        (639, "я", 641),  // 2-byte at 639..641
        (640, "я", 640),  // 2-byte at 640..642
        (638, "中", 641), // 3-byte at 638..641
        (639, "中", 642), // 3-byte at 639..642
        (640, "中", 640), // 3-byte at 640..643
        (637, "🦀", 641), // 4-byte at 637..641
        (638, "🦀", 642), // 4-byte at 638..642
        (639, "🦀", 643), // 4-byte at 639..643
        (640, "🦀", 640), // 4-byte at 640..644
    ];

    for (probe_start, probe, expected_tail_start) in test_cases {
        let probe_len = probe.len();
        let mut s = "X".repeat(probe_start);
        s.push_str(probe);
        let rem = total_len.saturating_sub(probe_start + probe_len);
        s.push_str(&"Y".repeat(rem));
        assert_eq!(s.len(), total_len);

        let folded = fold_tool_content(&s, "call_utf8_tail", false);
        assert!(folded.starts_with("[Tool Output Digest: id=call_utf8_tail"));

        let (_head, tail) = extract_head_tail(&folded);
        let expected_tail_len = total_len - expected_tail_start;
        assert_eq!(
            tail.len(),
            expected_tail_len,
            "Tail length mismatch for probe '{probe}' at {probe_start}: expected {expected_tail_len}, got {}",
            tail.len()
        );

        // Crucial: Tail slice MUST be valid UTF-8 and start on character boundary
        assert!(
            tail.is_char_boundary(0),
            "Tail slice does not start on char boundary"
        );
        if expected_tail_start == probe_start {
            assert!(tail.starts_with(probe));
        } else {
            assert!(!tail.starts_with(probe));
        }
    }
}

#[test]
fn test_utf8_comprehensive_sliding_window_matrix() {
    // Comprehensive stress test: slide a multi-byte sequence across ALL offsets from 145 to 175
    // with various payload sizes (600, 750, 1000 bytes) and various character sets:
    // Cyrillic (2B), CJK (3B), Emoji (4B), and ZWJ (18B).
    let multi_byte_probes = [
        "Привет",     // 12 bytes
        "こんにちは", // 15 bytes
        "🦀🚀🎉🔥",   // 16 bytes
        "👨‍👩‍👧‍👦",         // 18 bytes (ZWJ)
    ];

    for probe in multi_byte_probes {
        for offset in 145..=175 {
            for total in [600, 750, 1000] {
                let mut s = "z".repeat(offset);
                s.push_str(probe);
                if s.len() < total {
                    let pad = total - s.len();
                    s.push_str(&"w".repeat(pad));
                }

                let folded = fold_tool_content(&s, "call_matrix", false);

                // Verification checks:
                // 1. Must be valid UTF-8 String (guaranteed by Rust String type, but check integrity)
                assert!(folded.starts_with("[Tool Output Digest: id=call_matrix"));
                assert!(folded.ends_with(']'));

                // 2. Extract Head & Tail without panic
                let (head, tail) = extract_head_tail(&folded);
                assert!(head.len() <= DIGEST_HEAD_BYTES);
                assert!(tail.len() <= DIGEST_TAIL_BYTES);

                // 3. Must not contain replacement character U+FFFD (no corruption)
                assert!(!folded.contains('\u{FFFD}'));

                // 4. Digest byte count must match original input length
                let parsed_bytes = parse_digest_byte_count(&folded).unwrap();
                assert_eq!(parsed_bytes, s.len());
            }
        }
    }
}

#[test]
fn test_utf8_pure_multibyte_dense_payloads() {
    // 1. Pure 2-byte Cyrillic: 400 characters = 800 bytes
    let cyrillic = "Б".repeat(400);
    let folded_cyrillic = fold_tool_content(&cyrillic, "call_cyr", false);
    assert!(folded_cyrillic.contains("[Tool Output Digest: id=call_cyr"));
    let (head_c, tail_c) = extract_head_tail(&folded_cyrillic);
    assert_eq!(head_c.len(), 160); // 160 is even, exactly 80 characters
    assert_eq!(tail_c.len(), 160); // 160 is even, exactly 80 characters
    assert_eq!(head_c, "Б".repeat(80));
    assert_eq!(tail_c, "Б".repeat(80));

    // 2. Pure 3-byte CJK: 250 characters = 750 bytes
    let cjk = "漢".repeat(250);
    let folded_cjk = fold_tool_content(&cjk, "call_cjk", false);
    assert!(folded_cjk.contains("[Tool Output Digest: id=call_cjk"));
    let (head_k, tail_k) = extract_head_tail(&folded_cjk);
    // 160 / 3 = 53 characters = 159 bytes (floor boundary)
    assert_eq!(head_k.len(), 159);
    assert_eq!(head_k, "漢".repeat(53));
    // tail_target = 750 - 160 = 590. ceil_char_boundary(590) = 591 (197 * 3).
    // tail_len = 750 - 591 = 159 bytes = 53 characters.
    assert_eq!(tail_k.len(), 159);
    assert_eq!(tail_k, "漢".repeat(53));

    // 3. Pure 4-byte Emoji: 200 characters = 800 bytes
    let emoji = "🦀".repeat(200);
    let folded_emoji = fold_tool_content(&emoji, "call_emj", false);
    assert!(folded_emoji.contains("[Tool Output Digest: id=call_emj"));
    let (head_e, tail_e) = extract_head_tail(&folded_emoji);
    // 160 / 4 = 40 characters = 160 bytes
    assert_eq!(head_e.len(), 160);
    assert_eq!(tail_e.len(), 160);
    assert_eq!(head_e, "🦀".repeat(40));
    assert_eq!(tail_e, "🦀".repeat(40));
}

// ---------------------------------------------------------------------------
// 2. Empirical SWAR Line Counter Regression & Oracle Parity
// ---------------------------------------------------------------------------

#[test]
fn test_line_counter_swar_borrow_cascade_suite() {
    // The previous auditor identified that the unsafe SWAR implementation
    // in `crates/heterion-router-gateway/src/context_pruner.rs` had an arithmetic
    // underflow borrow cascade bug when 0x0A (\n) was followed by 0x0B (\v).
    // We test this thoroughly using `fold_tool_content` oracle verification.

    // 1. Auditor specific sequence: [10, 11, 117, 45, 236, 101, 134, 86]
    // In raw bytes:
    let raw_auditor_bytes = [10u8, 11, 117, 45, 236, 101, 134, 86];
    let memchr_auditor = memchr::memchr_iter(b'\n', &raw_auditor_bytes).count() + 1;
    let byte_oracle_auditor = raw_auditor_bytes.iter().filter(|&&b| b == b'\n').count() + 1;
    assert_eq!(memchr_auditor, 2);
    assert_eq!(memchr_auditor, byte_oracle_auditor);

    // In valid UTF-8 string: \n followed by \v (\x0B)
    let mut s1 = "\n\x0Babc".to_string();
    s1.push_str(&"x".repeat(600)); // pad above 512
    let oracle1 = s1.as_bytes().iter().filter(|&&b| b == b'\n').count() + 1;
    let folded1 = fold_tool_content(&s1, "c1", false);
    let count1 = parse_digest_line_count(&folded1).expect("Parse line count");
    assert_eq!(
        count1, oracle1,
        "Failed on \\n\\v sequence: got {count1}, expected {oracle1}"
    );
    assert_eq!(count1, 2); // exactly 1 newline -> 2 lines

    // 2. Alternating \n and \v repeated 100 times: 100 newlines + 1 = 101 lines
    let mut s2 = String::new();
    for _ in 0..100 {
        s2.push('\n');
        s2.push('\x0B'); // \v
    }
    s2.push_str(&"pad".repeat(200));
    let oracle2 = s2.as_bytes().iter().filter(|&&b| b == b'\n').count() + 1;
    let folded2 = fold_tool_content(&s2, "c2", false);
    let count2 = parse_digest_line_count(&folded2).unwrap();
    assert_eq!(count2, oracle2);
    assert_eq!(count2, 101);

    // 3. \n\r, \n\t, \r\n, \n\x00 combinations
    let test_pairs: &[(&str, usize)] = &[
        ("\n\r", 2),
        ("\r\n", 2),
        ("\n\t", 2),
        ("\n\x01", 2),
        ("\n\0", 2),
        ("\n\n", 3),
        ("\n\n\n", 4),
        ("\n\x0C", 2), // formfeed
    ];
    for &(pair, expected_base) in test_pairs {
        let mut s = pair.to_string();
        s.push_str(&"Z".repeat(600));
        let oracle = s.as_bytes().iter().filter(|&&b| b == b'\n').count() + 1;
        let folded = fold_tool_content(&s, "cpair", false);
        let count = parse_digest_line_count(&folded).unwrap();
        assert_eq!(count, oracle, "Failed on pair {pair:?}");
        assert_eq!(count, expected_base);
    }
}

#[test]
fn test_line_counter_exhaustive_256_byte_pairs() {
    // Test EVERY possible 2-byte sequence `[\n, byte]` for all 256 byte values.
    // Ensure that no adjacent byte value induces a false positive newline count.
    for b in 0..=255u8 {
        let mut bytes = Vec::with_capacity(700);
        bytes.push(b'\n');
        bytes.push(b);
        bytes.resize(700, b'a');

        // Check oracle
        let oracle = bytes.iter().filter(|&&x| x == b'\n').count() + 1;
        // Verify via memchr directly (the production implementation)
        let sim_count = memchr::memchr_iter(b'\n', &bytes).count() + 1;
        assert_eq!(
            sim_count, oracle,
            "memchr failed for byte pair [\\n, {b:#04X}]"
        );
    }
}

#[test]
fn test_line_counter_full_0_to_255_byte_sequences() {
    // Test full sequence of bytes 0x00..=0xFF
    let all_bytes: Vec<u8> = (0..=255u8).collect();
    let oracle = all_bytes.iter().filter(|&&b| b == b'\n').count() + 1;
    let sim_count = memchr::memchr_iter(b'\n', &all_bytes).count() + 1;
    assert_eq!(oracle, 2); // 0x0A appears exactly once
    assert_eq!(sim_count, oracle);

    // Repeated 100 times: 25,600 bytes
    let mut repeated = Vec::with_capacity(25600);
    for _ in 0..100 {
        repeated.extend_from_slice(&all_bytes);
    }
    let oracle_rep = repeated.iter().filter(|&&b| b == b'\n').count() + 1;
    let sim_rep = memchr::memchr_iter(b'\n', &repeated).count() + 1;
    assert_eq!(oracle_rep, 101);
    assert_eq!(sim_rep, oracle_rep);
}

#[test]
fn test_line_counter_edge_cases_no_newlines_and_dense_newlines() {
    // 1. Zero newlines in a 100KB payload
    let no_nl = "x".repeat(100_000);
    let folded_no_nl = fold_tool_content(&no_nl, "c_no_nl", false);
    let count_no_nl = parse_digest_line_count(&folded_no_nl).unwrap();
    assert_eq!(count_no_nl, 1, "Payload with 0 newlines must report 1 line");

    // 2. Trailing newlines: 600 bytes with 1, 2, 5 trailing newlines
    for trailing_nls in 1..=10 {
        let mut s = "a".repeat(600);
        s.push_str(&"\n".repeat(trailing_nls));
        let oracle = s.as_bytes().iter().filter(|&&b| b == b'\n').count() + 1;
        let folded = fold_tool_content(&s, "c_trailing", false);
        let count = parse_digest_line_count(&folded).unwrap();
        assert_eq!(count, oracle, "Failed on {trailing_nls} trailing newlines");
        assert_eq!(count, trailing_nls + 1);
    }

    // 3. Leading newlines
    for leading_nls in 1..=10 {
        let mut s = "\n".repeat(leading_nls);
        s.push_str(&"b".repeat(600));
        let oracle = s.as_bytes().iter().filter(|&&b| b == b'\n').count() + 1;
        let folded = fold_tool_content(&s, "c_leading", false);
        let count = parse_digest_line_count(&folded).unwrap();
        assert_eq!(count, oracle, "Failed on {leading_nls} leading newlines");
        assert_eq!(count, leading_nls + 1);
    }

    // 4. Repeated 5,000 newlines only
    let pure_nls = "\n".repeat(5_000);
    let folded_pure = fold_tool_content(&pure_nls, "c_pure", false);
    let count_pure = parse_digest_line_count(&folded_pure).unwrap();
    assert_eq!(count_pure, 5001);
}

// ---------------------------------------------------------------------------
// 3. Empirical Non-Expansion & Invariant Verification
// ---------------------------------------------------------------------------

#[test]
fn test_strict_non_expansion_across_all_byte_lengths() {
    // Sweep across all lengths from 1 to 1500 bytes.
    // Invariant: fold_tool_string_in_place MUST NEVER produce a string
    // longer than the original input!
    for len in 1..=1500 {
        let raw = "x".repeat(len);
        let mut s = raw.clone();
        let new_len = fold_tool_string_in_place(&mut s, "call_id_test", false);

        assert_eq!(new_len, s.len());
        assert!(
            new_len <= len,
            "Expansion detected! Input len {len} expanded to {new_len}"
        );

        if len <= MIN_TOOL_PRUNE_BYTES {
            assert_eq!(
                new_len, len,
                "Input <= MIN_TOOL_PRUNE_BYTES must not mutate"
            );
            assert_eq!(s, raw);
        }
    }
}

#[test]
fn test_no_window_overlap_disjointness_property() {
    // Mathematically test that head_end <= tail_start for all inputs.
    // Construct a payload of 600 bytes with unique index tags: [000],[001],...
    let mut raw = String::new();
    for i in 0..120 {
        raw.push_str(&format!("[{:03}]", i)); // 5 bytes * 120 = 600 bytes
    }
    assert_eq!(raw.len(), 600);

    let folded = fold_tool_content(&raw, "c_disjoint", false);
    let (head, tail) = extract_head_tail(&folded);

    // Ensure no tag appears in BOTH head and tail
    for i in 0..120 {
        let tag = format!("[{:03}]", i);
        let in_head = head.contains(&tag);
        let in_tail = tail.contains(&tag);
        assert!(
            !(in_head && in_tail),
            "Tag '{tag}' appeared in both head and tail! Slices overlap."
        );
    }
}

#[test]
fn test_prune_request_structural_invariants_under_stress() {
    // Create a 30-turn request (26 intermediate + 4 tail)
    let mut messages = Vec::new();
    messages.push(ChatMessage::plain("system", "Strict System Invariant"));
    messages.push(ChatMessage::plain("user", "Initial User Goal Invariant"));

    for i in 0..26 {
        let cid = format!("interm_{i}");
        messages.push(ChatMessage {
            role: "assistant".to_string(),
            content: None,
            reasoning_content: None,
            tool_calls: Some(json!([{
                "id": cid,
                "type": "function",
                "function": { "name": "bash", "arguments": "{}" }
            }])),
            tool_call_id: None,
            name: None,
        });
        messages.push(ChatMessage {
            role: "tool".to_string(),
            content: Some(json!(format!("output_{i}: {}", "DATA_".repeat(5000)))),
            reasoning_content: None,
            tool_calls: None,
            tool_call_id: Some(cid),
            name: None,
        });
    }

    for i in 0..4 {
        let cid = format!("tail_{i}");
        messages.push(ChatMessage {
            role: "assistant".to_string(),
            content: None,
            reasoning_content: None,
            tool_calls: Some(json!([{
                "id": cid,
                "type": "function",
                "function": { "name": "tail_bash", "arguments": "{}" }
            }])),
            tool_call_id: None,
            name: None,
        });
        messages.push(ChatMessage {
            role: "tool".to_string(),
            content: Some(json!(format!("tail_output_{i}: {}", "TAIL_".repeat(100)))),
            reasoning_content: None,
            tool_calls: None,
            tool_call_id: Some(cid),
            name: None,
        });
    }

    let orig_count = messages.len();
    let orig_system = messages[0].clone();
    let orig_user = messages[1].clone();
    let orig_tail_slice = messages[orig_count - 8..].to_vec();

    let mut req = ChatCompletionRequest {
        model: "heterion-router/auto".to_string(),
        messages,
        ..Default::default()
    };

    let (pruned_tokens, orig_tokens) = prune_request(&mut req, 128_000);
    assert!(pruned_tokens < orig_tokens);

    // Invariants:
    // 1. Total message count unchanged
    assert_eq!(req.messages.len(), orig_count);
    // 2. System message identical
    assert_eq!(req.messages[0], orig_system);
    // 3. User goal identical
    assert_eq!(req.messages[1], orig_user);
    // 4. Active tail (last 8 messages = 4 interaction turns) untouched
    assert_eq!(&req.messages[orig_count - 8..], orig_tail_slice.as_slice());
    // 5. Tool call IDs preserved 1-to-1
    for i in 0..26 {
        let tool_idx = 2 + (i * 2) + 1;
        let expected_id = format!("interm_{i}");
        assert_eq!(
            req.messages[tool_idx].tool_call_id.as_deref(),
            Some(expected_id.as_str())
        );
    }
}

// ---------------------------------------------------------------------------
// 4. Adversarial Edge Cases & Stress Scenarios
// ---------------------------------------------------------------------------

#[test]
fn test_adversarial_tool_call_ids() {
    // 1. Empty tool_call_id
    let raw = "log line\n".repeat(80); // ~720 bytes
    let folded_empty_id = fold_tool_content(&raw, "", false);
    assert!(folded_empty_id.starts_with("[Tool Output Digest: id= |"));
    assert!(folded_empty_id.len() < raw.len());

    // 2. Unicode / Emoji tool_call_id
    let unicode_id = "call_🔥_контекст_日本語_123";
    let folded_unicode_id = fold_tool_content(&raw, unicode_id, false);
    assert!(folded_unicode_id.starts_with(&format!("[Tool Output Digest: id={unicode_id} |")));
    assert!(folded_unicode_id.len() < raw.len());

    // 3. Huge tool_call_id (500 bytes) on 600-byte input
    // omitted_bytes is ~280 bytes. envelope_len with 500-byte id is > 650 bytes.
    // Invariant: MUST NOT fold because omitted_bytes <= envelope_len!
    let huge_id = "id_".to_string() + &"x".repeat(497);
    let mut s = "a".repeat(600);
    let old_len = s.len();
    let new_len = fold_tool_string_in_place(&mut s, &huge_id, false);
    assert_eq!(
        new_len, old_len,
        "Must not fold when id causes envelope > omitted_bytes"
    );
    assert_eq!(s, "a".repeat(600));
}

#[test]
fn test_borderline_513_bytes_envelope_cutoff() {
    // Exactly 513 bytes:
    // head_end = 160. tail_start = 513 - 160 = 353. omitted_bytes = 353 - 160 = 193.
    // With 4-byte tool_call_id ("c001") and status len 22 ("completed successfully"):
    // envelope_len = 124 + 4 + 22 + 1 (line) + 3 (total) + 3 (omitted) = 157.
    // 193 > 157 -> folds!
    let raw_513 = "x".repeat(513);
    let mut s1 = raw_513.clone();
    let folded_len = fold_tool_string_in_place(&mut s1, "c001", false);
    assert!(s1.starts_with("[Tool Output Digest: id=c001"));
    assert!(
        folded_len < 513,
        "Folded length {folded_len} must be strictly less than 513"
    );

    // Now test with tool_call_id of 40 bytes:
    // envelope_len = 124 + 40 + 22 + 1 + 3 + 3 = 193.
    // omitted_bytes (193) <= envelope_len (193) -> MUST NOT FOLD!
    let id_40 = "c".repeat(40);
    let mut s2 = raw_513.clone();
    let uncompressed_len = fold_tool_string_in_place(&mut s2, &id_40, false);
    assert_eq!(uncompressed_len, 513);
    assert_eq!(
        s2, raw_513,
        "Must not mutate when omitted_bytes <= envelope_len"
    );
}

#[test]
fn test_null_bytes_and_control_chars_safety() {
    // Tool output containing raw null bytes and ASCII control characters
    let mut raw = String::new();
    for i in 0..20 {
        raw.push_str(&format!(
            "entry_{i}:\0\x01\x02\x03\x04\x05\x06\x07\x08\t\r\x0E\x0F\x1B\x7F\n"
        ));
        raw.push_str(&"DATA_PAYLOAD_".repeat(3));
    }
    assert!(raw.len() > 600);

    let folded = fold_tool_content(&raw, "call_ctrl", false);
    assert!(folded.starts_with("[Tool Output Digest: id=call_ctrl"));
    assert!(folded.ends_with(']'));
    let (head, tail) = extract_head_tail(&folded);
    assert!(head.len() <= DIGEST_HEAD_BYTES);
    assert!(tail.len() <= DIGEST_TAIL_BYTES);
}

#[test]
fn test_pass2_tail_emergency_folding_behavior() {
    // Pass 2 only folds tools in the tail if they exceed 16,384 bytes AND request still exceeds budget.
    let mut req = ChatCompletionRequest {
        model: "auto".to_string(),
        messages: vec![
            ChatMessage::plain("system", "Sys"),
            ChatMessage::plain("user", "Run tasks"),
            // 4 interaction turns: all in the tail
            ChatMessage::assistant_text("turn 1"),
            ChatMessage {
                role: "tool".to_string(),
                content: Some(json!("A".repeat(16384))), // exactly 16384 -> NOT folded
                reasoning_content: None,
                tool_calls: None,
                tool_call_id: Some("call_t1".to_string()),
                name: None,
            },
            ChatMessage::assistant_text("turn 2"),
            ChatMessage {
                role: "tool".to_string(),
                content: Some(json!("B".repeat(16385))), // 16385 -> FOLDED in Pass 2
                reasoning_content: None,
                tool_calls: None,
                tool_call_id: Some("call_t2".to_string()),
                name: None,
            },
            ChatMessage::assistant_text("turn 3"),
            ChatMessage::plain("tool", "short 3"),
            ChatMessage::assistant_text("turn 4"),
            ChatMessage::plain("tool", "short 4"),
        ],
        ..Default::default()
    };

    // Budget of 1,000 tokens (well below ~8,000 tokens of 32KB payload)
    let (pruned, orig) = prune_request(&mut req, 1_000);
    assert!(pruned < orig);

    // Turn 1 tool (16384 bytes) MUST NOT be folded
    assert_eq!(req.messages[3].text(), "A".repeat(16384));

    // Turn 2 tool (16385 bytes) MUST be folded
    let t2_text = req.messages[5].text();
    assert!(t2_text.starts_with("[Tool Output Digest: id=call_t2"));
    assert!(t2_text.contains("16385 bytes"));
}

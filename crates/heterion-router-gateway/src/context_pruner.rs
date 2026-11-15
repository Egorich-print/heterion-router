//! Zero-Quota Algorithmic Context Pruning (Requirement R2).
//!
//! Pure deterministic Rust (<0.05 ms) folding of intermediate tool outputs
//! to fit 262k/128k context windows without external LLM calls or quota consumption.

use heterion_router_core::{ChatCompletionRequest, ChatMessage};
use serde_json::Value;

/// Number of active trailing interaction turns preserved verbatim.
pub const DEFAULT_RETAIN_TAIL_TURNS: usize = 4;

/// Minimum content size in bytes before a tool result is folded into a digest.
pub const MIN_TOOL_PRUNE_BYTES: usize = 512;

/// Byte length of head preview in the structured digest.
pub const DIGEST_HEAD_BYTES: usize = 160;

/// Byte length of tail preview in the structured digest.
pub const DIGEST_TAIL_BYTES: usize = 160;

/// Target context budget safety factor (90% of model window).
pub const TOKEN_BUDGET_SAFETY_FACTOR: f64 = 0.90;

/// Returns the context window the router plans and prunes against.
///
/// The window is the one the capability catalog holds ([`crate::model_spec`]),
/// so the two can never drift apart: a candidate that the dynamic plan
/// admitted against this number is pruned against the same number, and a
/// request never leaves the router larger than the limit the plan promised.
///
/// The windows here are the *operational* ones — the margin the router
/// deliberately keeps below the platform's physical window (the agentrouter
/// models are planned against 512k, not against the ~870k their platform
/// measured), because a request that only fits the physical window is the one
/// that runs into the platform's balancer timeout.
pub fn model_context_limit(model: &str) -> u32 {
    crate::model_spec::model_spec(model).context_window
}

/// Partition result indicating initial user goal index and active tail cutoff boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PartitionPlan {
    pub initial_user_goal_idx: Option<usize>,
    pub tail_cutoff_idx: usize,
}

/// Identifies the initial user goal and the boundary index of the last `retain_turns`.
pub fn partition_conversation(messages: &[ChatMessage], retain_turns: usize) -> PartitionPlan {
    let len = messages.len();
    if len == 0 {
        return PartitionPlan {
            initial_user_goal_idx: None,
            tail_cutoff_idx: 0,
        };
    }

    // 1. Locate the first user message (Initial Goal)
    let initial_user_goal_idx = messages.iter().position(|m| m.role == "user");

    // 2. Scan backwards from the end to find the cutoff for the last `retain_turns`
    let mut turns_seen = 0usize;
    let mut tail_cutoff_idx = 0usize;

    for i in (0..len).rev() {
        let role = messages[i].role.as_str();
        if role == "assistant" || role == "user" {
            turns_seen += 1;
        }
        // Tool messages belong to the assistant turn they respond to; they do not increment turn counter.

        if turns_seen >= retain_turns {
            tail_cutoff_idx = i;
            break;
        }
    }

    PartitionPlan {
        initial_user_goal_idx,
        tail_cutoff_idx,
    }
}

struct ByteCounter(usize);

impl std::io::Write for ByteCounter {
    #[inline(always)]
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0 += buf.len();
        Ok(buf.len())
    }
    #[inline(always)]
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[inline]
fn json_bytes(val: &Value) -> usize {
    let mut counter = ByteCounter(0);
    let _ = serde_json::to_writer(&mut counter, val);
    counter.0
}

#[inline]
fn count_lines(bytes: &[u8]) -> usize {
    memchr::memchr_iter(b'\n', bytes).count() + 1
}

#[inline]
fn decimal_digits(mut n: usize) -> usize {
    if n == 0 {
        return 1;
    }
    let mut d = 0;
    while n > 0 {
        d += 1;
        n /= 10;
    }
    d
}

#[inline]
fn push_usize(s: &mut String, mut n: usize) {
    if n == 0 {
        s.push('0');
        return;
    }
    let mut buf = [0u8; 20];
    let mut i = 20;
    while n > 0 {
        i -= 1;
        buf[i] = b'0' + (n % 10) as u8;
        n /= 10;
    }
    let num_str = unsafe { std::str::from_utf8_unchecked(&buf[i..]) };
    s.push_str(num_str);
}

#[inline]
fn contains_err_marker(bytes: &[u8]) -> bool {
    let len = bytes.len();
    if len < 4 {
        return false;
    }
    let mut i = 0;
    while i + 4 <= len {
        let b = bytes[i];
        if b.eq_ignore_ascii_case(&b'e') {
            let b1 = bytes[i + 1];
            if b1.eq_ignore_ascii_case(&b'r') && bytes[i + 2].eq_ignore_ascii_case(&b'r') {
                return true;
            }
            if b1.eq_ignore_ascii_case(&b'x')
                && bytes[i + 2].eq_ignore_ascii_case(&b'i')
                && bytes[i + 3].eq_ignore_ascii_case(&b't')
            {
                return true;
            }
        } else if (b.eq_ignore_ascii_case(&b'f')
            && bytes[i + 1].eq_ignore_ascii_case(&b'a')
            && bytes[i + 2].eq_ignore_ascii_case(&b'i')
            && bytes[i + 3].eq_ignore_ascii_case(&b'l'))
            || (b.eq_ignore_ascii_case(&b'p')
                && bytes[i + 1].eq_ignore_ascii_case(&b'a')
                && bytes[i + 2].eq_ignore_ascii_case(&b'n')
                && bytes[i + 3].eq_ignore_ascii_case(&b'i'))
        {
            return true;
        }
        i += 1;
    }
    false
}

/// Folds a raw tool content string in-place into a structured digest with UTF-8 safe boundary slicing.
/// Reuses the existing String allocation to eliminate heap churn.
pub fn fold_tool_string_in_place(s: &mut String, tool_call_id: &str, is_error: bool) -> usize {
    let total_bytes = s.len();
    if s.starts_with("[Tool Output Digest:") || total_bytes <= MIN_TOOL_PRUNE_BYTES {
        return total_bytes;
    }

    // 1. UTF-8 safe boundary slicing with overlap prevention
    let head_limit = DIGEST_HEAD_BYTES.min(total_bytes);
    let head_end = s.floor_char_boundary(head_limit);

    let tail_target = total_bytes.saturating_sub(DIGEST_TAIL_BYTES);
    let raw_tail_start = s.ceil_char_boundary(tail_target);
    let tail_start = raw_tail_start.max(head_end);

    let omitted_bytes = tail_start - head_end;
    if omitted_bytes == 0 {
        return total_bytes;
    }

    let status = if is_error {
        "completed with errors/warnings"
    } else {
        "completed successfully"
    };

    let line_count = count_lines(s.as_bytes());

    let envelope_len = 124
        + tool_call_id.len()
        + status.len()
        + decimal_digits(line_count)
        + decimal_digits(total_bytes)
        + decimal_digits(omitted_bytes);

    if omitted_bytes <= envelope_len {
        return total_bytes;
    }

    let mut head_buf = [0u8; DIGEST_HEAD_BYTES];
    head_buf[..head_end].copy_from_slice(&s.as_bytes()[..head_end]);

    let tail_len = total_bytes - tail_start;
    let mut tail_buf = [0u8; DIGEST_TAIL_BYTES];
    tail_buf[..tail_len].copy_from_slice(&s.as_bytes()[tail_start..]);

    s.clear();
    s.push_str("[Tool Output Digest: id=");
    s.push_str(tool_call_id);
    s.push_str(" | ");
    push_usize(s, line_count);
    s.push_str(" lines, ");
    push_usize(s, total_bytes);
    s.push_str(" bytes | status: ");
    s.push_str(status);
    s.push_str("\nHead:\n");
    // SAFETY: head_buf[..head_end] was validated on UTF-8 char boundary from original valid &str
    s.push_str(unsafe { std::str::from_utf8_unchecked(&head_buf[..head_end]) });
    s.push_str("\n... [omitted ");
    push_usize(s, omitted_bytes);
    s.push_str(" bytes of intermediate tool output] ...\nTail:\n");
    // SAFETY: tail_buf[..tail_len] was validated on UTF-8 char boundary from original valid &str
    s.push_str(unsafe { std::str::from_utf8_unchecked(&tail_buf[..tail_len]) });
    s.push(']');

    s.len()
}

/// Folds a raw tool content string into a structured digest with UTF-8 safe boundary slicing.
pub fn fold_tool_content(raw_text: &str, tool_call_id: &str, is_error: bool) -> String {
    let mut s = raw_text.to_string();
    fold_tool_string_in_place(&mut s, tool_call_id, is_error);
    s
}

/// Zero-allocation prompt token estimator for pruning checks.
pub fn fast_estimate_tokens(req: &ChatCompletionRequest) -> u32 {
    let mut bytes = 0usize;
    for message in &req.messages {
        let text_len = match &message.content {
            None => 0,
            Some(Value::String(s)) => s.len(),
            _ => message.text().len(),
        };
        bytes += message.role.len() + text_len + 16;
        if let Some(calls) = &message.tool_calls {
            bytes += json_bytes(calls);
        }
        if let Some(id) = &message.tool_call_id {
            bytes += id.len();
        }
    }
    if let Some(tools) = &req.tools {
        bytes += json_bytes(tools);
    }
    (bytes / 4 + 1) as u32
}

/// Fast check before doing full inspector analysis to prevent redundant heap allocations.
fn quick_is_tool_error(msg: &ChatMessage) -> bool {
    if let Some(Value::Object(_)) = &msg.content {
        return crate::inspector::is_tool_error(msg);
    }
    if let Some(content) = &msg.content {
        match content {
            Value::String(s) => {
                let bytes = s.as_bytes();
                let len = bytes.len();
                let head = &bytes[..2048.min(len)];
                let tail = &bytes[len.saturating_sub(2048)..];
                if !contains_err_marker(head) && !contains_err_marker(tail) {
                    return false;
                }
            }
            Value::Array(parts) => {
                let has_needle = parts.iter().any(|p| {
                    if let Some(t) = p.get("text").and_then(Value::as_str) {
                        let bytes = t.as_bytes();
                        let len = bytes.len();
                        let head = &bytes[..2048.min(len)];
                        let tail = &bytes[len.saturating_sub(2048)..];
                        contains_err_marker(head) || contains_err_marker(tail)
                    } else {
                        true
                    }
                });
                if !has_needle {
                    return false;
                }
            }
            _ => return false,
        }
    }
    crate::inspector::is_tool_error(msg)
}

/// Prunes intermediate tool messages older than the last 4 interaction turns.
/// Returns (pruned_tokens, original_tokens).
pub fn prune_request(request: &mut ChatCompletionRequest, target_token_limit: u32) -> (u32, u32) {
    let original_tokens = fast_estimate_tokens(request);
    let budget = (target_token_limit as f64 * TOKEN_BUDGET_SAFETY_FACTOR) as u32;

    if original_tokens <= budget {
        return (original_tokens, original_tokens);
    }

    let partition = partition_conversation(&request.messages, DEFAULT_RETAIN_TAIL_TURNS);
    let goal_idx = partition.initial_user_goal_idx;
    let tail_idx = partition.tail_cutoff_idx;

    // Pass 1: Fold intermediate tool outputs older than tail_idx
    let mut modified = false;
    for i in 0..tail_idx {
        if Some(i) == goal_idx {
            continue; // Preserve initial user goal
        }
        if request.messages[i].role == "system" || request.messages[i].role == "developer" {
            continue; // Preserve system prompts
        }
        if request.messages[i].role != "tool" {
            continue; // Only fold tool outputs
        }

        let msg = &mut request.messages[i];
        let is_err = quick_is_tool_error(msg);
        let call_id = msg.tool_call_id.as_deref().unwrap_or_default();

        if let Some(content) = msg.content.as_mut() {
            match content {
                Value::String(s) => {
                    let old_len = s.len();
                    let new_len = fold_tool_string_in_place(s, call_id, is_err);
                    if new_len < old_len {
                        modified = true;
                    }
                }
                Value::Array(_) => {
                    let text = heterion_router_core::content_text(Some(content));
                    let folded = fold_tool_content(&text, call_id, is_err);
                    *content = Value::String(folded);
                    modified = true;
                }
                _ => {}
            }
        }
    }

    let mut current_tokens = if modified {
        fast_estimate_tokens(request)
    } else {
        original_tokens
    };

    // Pass 2: Emergency pruning for oversized single tools inside the tail if still over budget
    if current_tokens > budget {
        let mut pass2_modified = false;
        for i in tail_idx..request.messages.len() {
            if request.messages[i].role == "tool" {
                let msg = &mut request.messages[i];
                let is_err = quick_is_tool_error(msg);
                let call_id = msg.tool_call_id.as_deref().unwrap_or_default();

                if let Some(content) = msg.content.as_mut() {
                    match content {
                        Value::String(s) => {
                            if s.len() > 16384 {
                                fold_tool_string_in_place(s, call_id, is_err);
                                pass2_modified = true;
                            }
                        }
                        Value::Array(_) => {
                            let text = heterion_router_core::content_text(Some(content));
                            if text.len() > 16384 {
                                *content = Value::String(fold_tool_content(&text, call_id, is_err));
                                pass2_modified = true;
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
        if pass2_modified {
            current_tokens = fast_estimate_tokens(request);
        }
    }

    (current_tokens, original_tokens)
}

/// Checks whether zero-quota algorithmic context pruning can compress the prompt
/// to fit within `target_token_limit`. Returns true if already within budget or
/// if pruning successfully brings the token count within budget.
pub fn can_prune_to_fit(request: &ChatCompletionRequest, target_token_limit: u32) -> bool {
    let original_tokens = fast_estimate_tokens(request);
    let budget = (target_token_limit as f64 * TOKEN_BUDGET_SAFETY_FACTOR) as u32;
    if original_tokens <= budget {
        return true;
    }
    let mut cloned = request.clone();
    let (pruned_tokens, _) = prune_request(&mut cloned, target_token_limit);
    pruned_tokens <= target_token_limit
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_partition_conversation_empty() {
        let plan = partition_conversation(&[], 4);
        assert_eq!(plan.initial_user_goal_idx, None);
        assert_eq!(plan.tail_cutoff_idx, 0);
    }

    #[test]
    fn test_partition_conversation_user_only() {
        let msgs = vec![
            ChatMessage::plain("user", "Hello"),
            ChatMessage::plain("user", "World"),
        ];
        let plan = partition_conversation(&msgs, 4);
        assert_eq!(plan.initial_user_goal_idx, Some(0));
        assert_eq!(plan.tail_cutoff_idx, 0);
    }

    #[test]
    fn test_partition_conversation_tail_boundary() {
        let mut msgs = vec![
            ChatMessage::plain("system", "System"),
            ChatMessage::plain("user", "Goal"),
        ];
        // Add 6 turns (each assistant + tool)
        for i in 0..6 {
            let call_id = format!("call_{i}");
            msgs.push(ChatMessage {
                role: "assistant".to_string(),
                content: None,
                reasoning_content: None,
                tool_calls: Some(serde_json::json!([{"id": call_id}])),
                tool_call_id: None,
                name: None,
            });
            msgs.push(ChatMessage {
                role: "tool".to_string(),
                content: Some(serde_json::json!("output")),
                reasoning_content: None,
                tool_calls: None,
                tool_call_id: Some(call_id),
                name: None,
            });
        }

        // msgs has length: 2 (sys+user) + 12 (6 turns * 2) = 14 msgs.
        // Retaining 4 turns:
        // Turns 6, 5, 4, 3 are retained. Turn 3 begins at index: 2 + (2 * 2) = 6.
        let plan = partition_conversation(&msgs, 4);
        assert_eq!(plan.initial_user_goal_idx, Some(1));
        assert_eq!(plan.tail_cutoff_idx, 6);
    }

    #[test]
    fn test_fold_tool_content_small() {
        let small = "a".repeat(100);
        let folded = fold_tool_content(&small, "call_1", false);
        assert_eq!(
            folded, small,
            "Output <= MIN_TOOL_PRUNE_BYTES must not be folded"
        );
    }

    #[test]
    fn test_fold_tool_content_large_clean() {
        let large = "line1\nline2\n".to_string() + &"content ".repeat(500);
        let folded = fold_tool_content(&large, "call_clean", false);

        assert!(folded.starts_with("[Tool Output Digest: id=call_clean"));
        assert!(folded.contains("status: completed successfully"));
        assert!(folded.contains("Head:\nline1\nline2\n"));
        assert!(folded.contains("... [omitted "));
        assert!(folded.ends_with(']'));
    }

    #[test]
    fn test_fold_tool_content_error_markers() {
        let err_output =
            "error[E0432]: unresolved import `foo`\n".to_string() + &"stack ".repeat(500);
        let folded = fold_tool_content(&err_output, "call_err", true);

        assert!(folded.contains("status: completed with errors/warnings"));
    }

    #[test]
    fn test_fold_tool_content_utf8_multibyte() {
        let prefix = "Б".repeat(78); // 156 bytes
        let middle =
            "🎉Тестирование длинного вывода инструмента для проверки UTF-8 границы.\n".repeat(8);
        let raw = format!("{prefix}{middle}");
        let folded = fold_tool_content(&raw, "call_utf8", false);

        assert!(folded.contains("[Tool Output Digest: id=call_utf8"));
    }

    #[test]
    fn test_fast_estimate_tokens_matches_estimate_prompt_tokens() {
        let req = ChatCompletionRequest {
            model: "test-model".to_string(),
            messages: vec![
                ChatMessage::plain("system", "You are an assistant."),
                ChatMessage::plain("user", "Hello there!"),
                ChatMessage {
                    role: "assistant".to_string(),
                    content: None,
                    reasoning_content: None,
                    tool_calls: Some(serde_json::json!([
                        {"id": "call_1", "type": "function", "function": {"name": "test", "arguments": "{}"}}
                    ])),
                    tool_call_id: None,
                    name: None,
                },
                ChatMessage {
                    role: "tool".to_string(),
                    content: Some(serde_json::json!("Tool output test content.")),
                    reasoning_content: None,
                    tool_calls: None,
                    tool_call_id: Some("call_1".to_string()),
                    name: None,
                },
            ],
            ..Default::default()
        };

        assert_eq!(
            fast_estimate_tokens(&req),
            crate::usage_estimate::estimate_prompt_tokens(&req)
        );
    }

    #[test]
    fn test_count_lines_basic_parity() {
        let cases: &[(&[u8], usize)] = &[
            (b"", 1),
            (b"hello", 1),
            (b"hello\n", 2),
            (b"hello\nworld", 2),
            (b"hello\nworld\n", 3),
            (b"\n\n\n\n\n", 6),
        ];
        for (input, expected) in cases {
            assert_eq!(count_lines(input), *expected);
            assert_eq!(
                count_lines(input),
                input.iter().filter(|&&b| b == b'\n').count() + 1
            );
        }
    }

    #[test]
    fn test_count_lines_swar_borrow_cascade_regression() {
        // 1. Auditor specific sequence: [10, 11, 117, 45, 236, 101, 134, 86]
        let mut buf = [b'a'; 64];
        buf[0..8].copy_from_slice(&[10, 11, 117, 45, 236, 101, 134, 86]);
        assert_eq!(count_lines(&buf), 2); // 1 newline + 1 = 2 lines
        assert_eq!(
            count_lines(&buf),
            buf.iter().filter(|&&b| b == b'\n').count() + 1
        );

        // 2. Full 8-byte cascade: 10 (\n) followed by seven 11s (\v)
        let mut cascade_buf = [11u8; 64];
        cascade_buf[0] = 10;
        assert_eq!(count_lines(&cascade_buf), 2);
        assert_eq!(
            count_lines(&cascade_buf),
            cascade_buf.iter().filter(|&&b| b == b'\n').count() + 1
        );

        // 3. Repeating alternating \n and \v pattern
        let repeating: Vec<u8> = (0..200).map(|i| if i % 2 == 0 { 10 } else { 11 }).collect();
        assert_eq!(count_lines(&repeating), 101); // 100 newlines + 1 = 101 lines

        // 4. Control characters mix: only 0x0A is counted
        let ctrl = b"\x00\x01\x02\x03\x04\x05\x06\x07\x08\x09\x0A\x0B\x0C\x0D\x0E\x0F";
        assert_eq!(count_lines(ctrl), 2);
    }

    #[test]
    fn test_count_lines_multibyte_utf8() {
        let s = "Привет, мир!\nЭто вторая строка 🚀.\nИ третья строка с иероглифами: 漢字\n";
        let expected = s.as_bytes().iter().filter(|&&b| b == b'\n').count() + 1;
        assert_eq!(count_lines(s.as_bytes()), expected);
        assert_eq!(count_lines(s.as_bytes()), 4);
    }

    #[test]
    fn test_count_lines_random_fuzz_parity() {
        let mut rng: u64 = 0xDEADBEEFCAFEBABE;
        for len in 0..1024 {
            let mut buf = vec![0u8; len];
            for b in buf.iter_mut() {
                rng = rng.wrapping_mul(6364136223846793005).wrapping_add(1);
                *b = (rng >> 32) as u8;
            }
            let naive = buf.iter().filter(|&&b| b == b'\n').count() + 1;
            assert_eq!(
                count_lines(&buf),
                naive,
                "Fuzz parity failed at buffer length {len}"
            );
        }
    }

    #[test]
    fn test_exact_threshold_boundary_not_folded() {
        let raw = "a".repeat(MIN_TOOL_PRUNE_BYTES);
        let folded = fold_tool_content(&raw, "call_test", false);
        assert_eq!(
            folded, raw,
            "Input == MIN_TOOL_PRUNE_BYTES must remain unaltered"
        );
    }

    #[test]
    fn test_sub_threshold_inputs_never_folded() {
        for len in [0, 1, 15, 64, 128, 256, 320, 384, 511, 512] {
            if len <= MIN_TOOL_PRUNE_BYTES {
                let raw = "z".repeat(len);
                let folded = fold_tool_content(&raw, "call_sub", false);
                assert_eq!(folded.len(), len);
                assert_eq!(folded, raw);
            }
        }
    }

    #[test]
    fn test_strictly_smaller_for_every_folded_input() {
        // Sweep across lengths from MIN_TOOL_PRUNE_BYTES + 1 to 2048 bytes
        for len in (MIN_TOOL_PRUNE_BYTES + 1)..=2048 {
            let raw = "x".repeat(len);
            for is_err in [false, true] {
                for id in [
                    "c1",
                    "call_intermediate_0",
                    "call_123456789012345678901234567890",
                ] {
                    let mut s = raw.clone();
                    let new_len = fold_tool_string_in_place(&mut s, id, is_err);
                    assert_eq!(new_len, s.len());
                    // Invariant: either it folded and strictly shrank, or it remained verbatim!
                    if s.starts_with("[Tool Output Digest:") {
                        assert!(
                            new_len < len,
                            "Folded len ({new_len}) must be strictly less than raw len ({len}) for id '{id}', is_err={is_err}"
                        );
                    } else {
                        assert_eq!(new_len, len);
                        assert_eq!(s, raw);
                    }
                }
            }
        }
    }

    #[test]
    fn test_no_window_overlap_no_duplicate_content() {
        // Construct string with unique, identifiable 8-byte chunks: [000000][000001]...
        let mut raw = String::new();
        for i in 0..100 {
            raw.push_str(&format!("[{:06}]", i)); // 8 bytes each, total 800 bytes
        }
        let folded = fold_tool_content(&raw, "call_overlap_test", false);

        assert!(folded.starts_with("[Tool Output Digest:"));
        let head_marker = "Head:\n";
        let tail_marker = "Tail:\n";
        let head_start = folded.find(head_marker).unwrap() + head_marker.len();
        let head_end = folded.find("\n... [omitted").unwrap();
        let head_str = &folded[head_start..head_end];

        let tail_start = folded.find(tail_marker).unwrap() + tail_marker.len();
        let tail_end = folded.rfind(']').unwrap();
        let tail_str = &folded[tail_start..tail_end];

        // Ensure no 8-byte chunk appears in BOTH head and tail
        for i in 0..100 {
            let chunk = format!("[{:06}]", i);
            let in_head = head_str.contains(&chunk);
            let in_tail = tail_str.contains(&chunk);
            assert!(
                !(in_head && in_tail),
                "Chunk {chunk} appears in both Head and Tail! Window overlap detected."
            );
        }
    }

    #[test]
    fn test_adversarial_long_tool_call_id_never_expands() {
        // 200-byte tool_call_id
        let long_id = "call_".to_string() + &"a".repeat(195);
        let raw = "hello world\n".repeat(45); // ~540 bytes
        let mut s = raw.clone();
        let new_len = fold_tool_string_in_place(&mut s, &long_id, false);

        // Because long_id makes envelope_len > omitted_bytes, non-expansion guard must preserve verbatim
        assert_eq!(new_len, raw.len());
        assert_eq!(s, raw);
    }

    #[test]
    fn test_gemma_context_limit_and_can_prune_to_fit() {
        assert_eq!(model_context_limit("gemma"), 256_000);
        assert_eq!(model_context_limit("gemma-4-26b-a4b-it"), 256_000);

        let small_req = ChatCompletionRequest {
            model: "auto".into(),
            messages: vec![ChatMessage::plain("user", "Hello")],
            ..Default::default()
        };
        assert!(can_prune_to_fit(&small_req, 128_000));
        assert!(can_prune_to_fit(&small_req, 256_000));
    }
}

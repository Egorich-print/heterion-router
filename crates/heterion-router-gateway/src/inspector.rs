//! Request inspector for NVIDIA Switchyard dynamic routing.
//!
//! Inspects incoming [`ChatCompletionRequest`] to extract:
//! - Estimated prompt token count (accounting for text characters/bytes, images at 1120 tok/img, turns framing, and tool definitions)
//! - Tool presence ([`has_tools`])
//! - Multimodal vision / image presence ([`has_images`])
//! - Agent cycle stage ([`AgentStage`]: `Planning`, `MechanicalToolLoop`, `ErrorRecovery`)

use heterion_router_core::{ChatCompletionRequest, ChatMessage};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Detected stage of an agentic workflow execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AgentStage {
    /// Initial conceptualization, system architecture, or plan generation without prior tool history.
    Planning,
    /// Active tool execution loop with successful tool outputs.
    MechanicalToolLoop,
    /// Recovery from tool failure, panic, exception, or non-zero exit code.
    ErrorRecovery,
}

/// Inspection output representing the key structural traits of an incoming request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequestInspection {
    /// Approximate token length of the prompt payload.
    pub estimated_tokens: u32,
    /// Whether the request specifies tools for function calling.
    pub has_tools: bool,
    /// Whether the request contains image inputs (multimodal vision).
    pub has_images: bool,
    /// Classified agent cycle stage.
    pub stage: AgentStage,
}

pub const ERROR_MARKERS: [&str; 6] = [
    "error",
    "panic",
    "failed",
    "exception",
    "traceback",
    "exit code",
];

/// Inspect a chat completion request to determine tokens, tools, vision, and agent stage.
pub fn inspect_request(req: &ChatCompletionRequest) -> RequestInspection {
    let mut text_bytes: usize = 0;
    let mut image_count: usize = 0;

    for msg in &req.messages {
        text_bytes += msg.role.len();
        if let Some(content) = &msg.content {
            match content {
                Value::String(s) => text_bytes += s.len(),
                Value::Array(parts) => {
                    for part in parts {
                        if is_image_part(part) {
                            image_count += 1;
                            if let Some(t) = part.get("text").and_then(Value::as_str) {
                                text_bytes += t.len();
                            }
                        } else if let Some(t) = part.get("text").and_then(Value::as_str) {
                            text_bytes += t.len();
                        } else if let Some(s) = part.as_str() {
                            text_bytes += s.len();
                        }
                    }
                }
                Value::Object(map) => {
                    if is_image_part(content) {
                        image_count += 1;
                    }
                    if let Some(t) = map.get("text").and_then(Value::as_str) {
                        text_bytes += t.len();
                    }
                }
                other => {
                    if let Some(s) = other.as_str() {
                        text_bytes += s.len();
                    }
                }
            }
        }

        if let Some(reasoning) = &msg.reasoning_content {
            text_bytes += reasoning.len();
        }
        if let Some(calls) = &msg.tool_calls {
            text_bytes += calls.to_string().len();
        }
        if let Some(id) = &msg.tool_call_id {
            text_bytes += id.len();
        }
        if let Some(name) = &msg.name {
            text_bytes += name.len();
        }
    }

    let tool_bytes = req.tools.as_ref().map(|t| t.to_string().len()).unwrap_or(0);
    let turns = req.messages.len();

    // Token estimation: text characters/bytes / 4 + 1120 tokens per image + turns * 4 + tool definitions / 4
    let estimated_tokens = (text_bytes / 4) as u32
        + (image_count as u32 * 1120)
        + (turns as u32 * 4)
        + (tool_bytes / 4) as u32;

    let has_tools = req.tools.as_ref().is_some_and(|tools| match tools {
        Value::Array(arr) => !arr.is_empty(),
        Value::Null => false,
        _ => true,
    });

    let has_images = image_count > 0;
    let stage = detect_stage(&req.messages);

    RequestInspection {
        estimated_tokens,
        has_tools,
        has_images,
        stage,
    }
}

/// Check if a JSON content part represents an image (OpenAI `image_url` or AI-SDK `image`).
pub fn is_image_part(part: &Value) -> bool {
    if let Some(obj) = part.as_object() {
        if let Some(t) = obj.get("type").and_then(Value::as_str)
            && (t == "image_url" || t == "image")
        {
            return true;
        }
        if obj.contains_key("image_url") || obj.contains_key("image") {
            return true;
        }
    }
    false
}

/// Detect the active agent cycle stage based on the trailing active turn.
pub fn detect_stage(messages: &[ChatMessage]) -> AgentStage {
    if messages.is_empty() {
        return AgentStage::Planning;
    }

    // 1. Determine active turn from the end of the message list.
    // Trailing assistant messages (e.g. prefill) are skipped to find the preceding stimulus,
    // unless the assistant message carries pending tool calls.
    let mut rev_iter = messages.iter().rev();
    let last = rev_iter.next().unwrap();

    let effective_last = if last.role == "assistant" {
        if last.has_tool_calls() {
            return AgentStage::MechanicalToolLoop;
        }
        rev_iter.find(|m| m.role != "assistant").unwrap_or(last)
    } else {
        last
    };

    match effective_last.role.as_str() {
        "tool" => {
            // Check all contiguous tool messages in the trailing batch (e.g. parallel tool calls)
            let trailing_tools: Vec<&ChatMessage> = messages
                .iter()
                .rev()
                .skip_while(|m| m.role == "assistant")
                .take_while(|m| m.role == "tool")
                .collect();

            let has_error = trailing_tools.iter().any(|m| is_tool_error(m));
            if has_error {
                AgentStage::ErrorRecovery
            } else {
                AgentStage::MechanicalToolLoop
            }
        }
        "user" => {
            if is_user_error_report(effective_last) {
                AgentStage::ErrorRecovery
            } else {
                AgentStage::Planning
            }
        }
        _ => AgentStage::Planning,
    }
}

/// Check if text represents tool search results (e.g. grep, ripgrep).
fn is_search_result_output(lower: &str) -> bool {
    let trimmed = lower.trim_start();

    // 1. Summary prefix: "matched <N> line(s):", "matched <N> matches:", etc.
    if let Some(rest) = trimmed.strip_prefix("matched ") {
        let after_num = rest.trim_start_matches(|c: char| c.is_ascii_digit() || c.is_whitespace());
        if after_num.starts_with("line:")
            || after_num.starts_with("lines:")
            || after_num.starts_with("match:")
            || after_num.starts_with("matches:")
            || after_num.starts_with("result:")
            || after_num.starts_with("results:")
        {
            return true;
        }
    }
    if let Some(rest) = trimmed.strip_prefix("found ") {
        let after_num = rest.trim_start_matches(|c: char| c.is_ascii_digit() || c.is_whitespace());
        if after_num.starts_with("line:")
            || after_num.starts_with("lines:")
            || after_num.starts_with("match:")
            || after_num.starts_with("matches:")
            || after_num.starts_with("result:")
            || after_num.starts_with("results:")
        {
            return true;
        }
    }

    // 2. Summary phrase in first line: "3 lines matched:", "5 matches found", etc.
    let first_line = trimmed.lines().next().unwrap_or("");
    if first_line.contains("lines matched")
        || first_line.contains("line matched")
        || first_line.contains("matches found")
        || first_line.contains("match found")
        || first_line.starts_with("search results")
        || first_line.starts_with("grep results")
    {
        return true;
    }

    // 3. Output containing multiple grep-formatted lines: `<file>:<line_number>:<content>`
    let mut search_line_count = 0;
    for line in trimmed.lines().take(10) {
        let parts: Vec<&str> = line.splitn(3, ':').collect();
        if parts.len() >= 3
            && parts[1].trim().chars().all(|c| c.is_ascii_digit())
            && !parts[1].trim().is_empty()
        {
            search_line_count += 1;
            if search_line_count >= 2 {
                return true;
            }
        }
    }

    false
}

/// Sanitize source code definitions of error types (e.g. `enum Error`, `pub enum CustomError`, `struct ApiError`).
fn sanitize_code_definitions(lower: &str) -> String {
    let mut sanitized = lower.to_string();

    let type_keywords = ["enum ", "struct ", "class ", "type ", "interface "];
    for kw in type_keywords {
        let mut search_idx = 0;
        while let Some(rel_idx) = sanitized[search_idx..].find(kw) {
            let decl_start = search_idx + rel_idx;
            // Check boundary before keyword: must not be preceded by alphanumeric or '_'
            let prev_char_is_ident = decl_start > 0 && {
                let prev_char = sanitized[..decl_start].chars().next_back().unwrap();
                prev_char.is_ascii_alphanumeric() || prev_char == '_'
            };
            if prev_char_is_ident {
                search_idx = decl_start + kw.len();
                continue;
            }

            let after_kw = &sanitized[decl_start + kw.len()..];
            let trimmed_start = after_kw.len() - after_kw.trim_start().len();
            let after_kw_trimmed = &after_kw[trimmed_start..];
            let ident_len = after_kw_trimmed
                .find(|c: char| !c.is_ascii_alphanumeric() && c != '_')
                .unwrap_or(after_kw_trimmed.len());
            let ident = &after_kw_trimmed[..ident_len];
            if ident.contains("error") || ident.contains("exception") {
                let mut remove_start = decl_start;
                let before = &sanitized[..decl_start];
                let line_start = before.rfind('\n').map(|p| p + 1).unwrap_or(0);
                let line_before = &before[line_start..];

                if let Some(pub_rel) = line_before.rfind("pub ") {
                    if line_before[pub_rel + 4..].trim().is_empty() {
                        remove_start = line_start + pub_rel;
                    }
                } else if let Some(pub_rel) = line_before.rfind("pub(")
                    && let Some(close_paren) = line_before[pub_rel..].find(')')
                    && line_before[pub_rel + close_paren + 1..].trim().is_empty()
                {
                    remove_start = line_start + pub_rel;
                }
                let remove_end = decl_start + kw.len() + trimmed_start + ident_len;
                sanitized.replace_range(remove_start..remove_end, "");
                search_idx = remove_start;
            } else {
                search_idx = decl_start + kw.len();
            }
        }
    }

    sanitized
}

/// Check if a tool execution message represents a failure/error.
pub fn is_tool_error(msg: &ChatMessage) -> bool {
    // 1. Structured JSON Object inspection (when msg.content is directly a JSON object)
    if let Some(Value::Object(obj)) = &msg.content {
        if let Some(err_val) = obj.get("error")
            && is_json_error_value(err_val)
        {
            return true;
        }
        if let Some(status) = obj.get("status").and_then(Value::as_str) {
            let s = status.to_ascii_lowercase();
            if s == "error" || s == "failed" || s == "failure" {
                return true;
            }
            if (s == "success" || s == "ok")
                && obj.get("error").is_none_or(|e| !is_json_error_value(e))
            {
                if let Some(code) = obj.get("exit_code").and_then(Value::as_i64)
                    && code != 0
                {
                    return true;
                }
                return false;
            }
        }
        if let Some(true) = obj.get("is_error").and_then(Value::as_bool) {
            return true;
        }
        if let Some(code) = obj.get("exit_code").and_then(Value::as_i64) {
            if code != 0 {
                return true;
            }
            if code == 0 && obj.get("error").is_none_or(|e| !is_json_error_value(e)) {
                return false;
            }
        }
        if let Some(err_val) = obj.get("error")
            && !is_json_error_value(err_val)
        {
            return false;
        }
    }

    let raw_text = message_full_text(msg);
    let lower = raw_text.to_lowercase();

    // 2. Stringified JSON check (e.g. content: "{\"status\": \"success\", \"error\": null, ...}")
    let trimmed = raw_text.trim();
    if (trimmed.starts_with('{') || trimmed.starts_with('['))
        && let Ok(val) = serde_json::from_str::<Value>(trimmed)
        && let Some(obj) = val.as_object()
    {
        if let Some(err_val) = obj.get("error")
            && is_json_error_value(err_val)
        {
            return true;
        }
        if let Some(status) = obj.get("status").and_then(Value::as_str) {
            let s = status.to_ascii_lowercase();
            if s == "error" || s == "failed" || s == "failure" {
                return true;
            }
            if (s == "success" || s == "ok")
                && obj.get("error").is_none_or(|e| !is_json_error_value(e))
            {
                if let Some(code) = obj.get("exit_code").and_then(Value::as_i64)
                    && code != 0
                {
                    return true;
                }
                return false;
            }
        }
        if let Some(true) = obj.get("is_error").and_then(Value::as_bool) {
            return true;
        }
        if let Some(code) = obj.get("exit_code").and_then(Value::as_i64) {
            if code != 0 {
                return true;
            }
            if code == 0 && obj.get("error").is_none_or(|e| !is_json_error_value(e)) {
                return false;
            }
        }
        if let Some(err_val) = obj.get("error")
            && !is_json_error_value(err_val)
        {
            return false;
        }
    }

    // 3. Search tool output (e.g. grep, ripgrep finding code occurrences)
    let is_search = if let Some(tool_name) = &msg.name {
        let n = tool_name.to_ascii_lowercase();
        n == "grep"
            || n == "grep_search"
            || n == "search"
            || n == "search_code"
            || n == "code_search"
            || n == "find_by_name"
            || n == "ripgrep"
            || n == "ag"
            || is_search_result_output(&lower)
    } else {
        is_search_result_output(&lower)
    };
    if is_search {
        let sanitized = sanitize_code_definitions(&lower);
        return has_compiler_or_runtime_error(&sanitized)
            || has_panic_error(&sanitized)
            || has_exception_error(&sanitized)
            || has_nonzero_exit_code(&sanitized)
            || has_test_failures(&sanitized);
    }

    // 4. Suppress Cargo.toml release profile panic config (e.g. `panic = "abort"`)
    if lower.contains("panic = \"abort\"") || lower.contains("panic = 'abort'") {
        let sanitized = lower
            .replace("panic = \"abort\"", "")
            .replace("panic = 'abort'", "");
        return has_compiler_or_runtime_error(&sanitized)
            || has_panic_error(&sanitized)
            || has_nonzero_exit_code(&sanitized)
            || has_test_failures(&sanitized);
    }

    // 5. Suppress source code definitions of error types (e.g. `enum Error`, `pub enum CustomError`, `struct ApiError`)
    let sanitized_code = sanitize_code_definitions(&lower);
    if sanitized_code != lower {
        return has_compiler_or_runtime_error(&sanitized_code)
            || has_panic_error(&sanitized_code)
            || has_nonzero_exit_code(&sanitized_code)
            || has_test_failures(&sanitized_code);
    }

    // 6. Suppress English idioms like "with the exception of"
    if lower.contains("with the exception of") || lower.contains("with exception of") {
        let sanitized = lower
            .replace("with the exception of", "")
            .replace("with exception of", "");
        return has_compiler_or_runtime_error(&sanitized)
            || has_panic_error(&sanitized)
            || has_exception_error(&sanitized)
            || has_nonzero_exit_code(&sanitized)
            || has_test_failures(&sanitized);
    }

    // 7. Embedded JSON error fields
    if has_json_error_field(&lower) {
        return true;
    }

    // 8. Non-zero exit code check (differentiates "exit code 0" from non-zero)
    if has_nonzero_exit_code(&lower) {
        return true;
    }

    // 9. Test runner failure counts (differentiates "0 failed" from positive failures)
    if has_test_failures(&lower) {
        return true;
    }

    // 10. Compiler, traceback, or runtime errors
    if has_compiler_or_runtime_error(&lower) {
        return true;
    }

    // 11. Runtime panics
    if has_panic_error(&lower) {
        return true;
    }

    // 12. Unhandled exceptions
    if has_exception_error(&lower) {
        return true;
    }

    // 13. Colon-delimited error lines (e.g. "error: cannot find file")
    if has_error_colon(&lower) {
        return true;
    }

    false
}

/// Check if a user message is explicitly reporting an error or crash to be debugged.
pub fn is_user_error_report(msg: &ChatMessage) -> bool {
    let text = message_full_text(msg);
    let lower = text.to_lowercase();

    // 0. Sanitize code definitions of error types (e.g. `struct DbError`, `enum RouterError`, `class NetworkException`)
    let code_sanitized = sanitize_code_definitions(&lower);

    // 1. Filter out conceptual questions and architecture design topics
    let sanitized = code_sanitized
        .replace("error handling", "")
        .replace("error-handling", "")
        .replace("exception handling", "")
        .replace("handling errors", "")
        .replace("handling exceptions", "")
        .replace("error boundary", "")
        .replace("error boundaries", "")
        .replace("error definition", "")
        .replace("error definitions", "")
        .replace("error type", "")
        .replace("error types", "")
        .replace("error handler", "")
        .replace("error handlers", "")
        .replace("exit code 0", "")
        .replace("exit code: 0", "")
        .replace("exit status: 0", "")
        .replace("exited with code 0", "")
        .replace("0 failed", "")
        .replace("failed: 0", "");

    // 2. Concrete errors reported in user text (evaluated on code-sanitized text to prevent class/type name leakage)
    if has_nonzero_exit_code(&code_sanitized)
        || has_test_failures(&code_sanitized)
        || has_compiler_or_runtime_error(&code_sanitized)
        || has_panic_error(&code_sanitized)
        || has_exception_error(&code_sanitized)
    {
        return true;
    }

    // 3. User explicit error reporting phrases (e.g. "encountered ... during execution", "crashed with")
    if sanitized.contains("encountered")
        && (sanitized.contains("error")
            || sanitized.contains("panic")
            || sanitized.contains("failed")
            || sanitized.contains("exception")
            || sanitized.contains("traceback")
            || sanitized.contains("exit code"))
    {
        return true;
    }

    if sanitized.contains("failed with")
        || sanitized.contains("crashed with")
        || sanitized.contains("crashed due to")
        || sanitized.contains("process crashed")
        || sanitized.contains("build failed")
        || sanitized.contains("compilation failed")
        || sanitized.contains("tests failed")
        || sanitized.contains("test failed")
    {
        return true;
    }

    // 4. General error markers in sanitized user message, if framed as an incident or report
    for marker in ERROR_MARKERS {
        if sanitized.contains(marker) {
            let trimmed = lower.trim();

            // Check explicit incident remediation intents
            let is_incident_remediation = lower.contains("how do i fix")
                || lower.contains("how to fix")
                || lower.contains("how can i fix")
                || lower.contains("help me fix")
                || lower.contains("help fix")
                || lower.contains("fix this error")
                || lower.contains("fix the error")
                || lower.contains("fix an error")
                || lower.contains("fix my error")
                || lower.contains("resolve this error")
                || lower.contains("resolve the error")
                || lower.contains("resolve an error")
                || lower.contains("solve this error")
                || lower.contains("solve the error")
                || lower.contains("solve an error")
                || lower.contains("debug this error")
                || lower.contains("debug the error")
                || lower.contains("debug an error")
                || lower.contains("troubleshoot this error")
                || lower.contains("why am i getting")
                || lower.contains("why do i get")
                || lower.contains("got this error")
                || lower.contains("got an error")
                || lower.contains("getting this error")
                || lower.contains("getting an error");

            if is_incident_remediation {
                return true;
            }

            // Recognize natural inquiry stems on trimmed text
            let is_question = trimmed.starts_with("how")
                || trimmed.starts_with("what")
                || trimmed.starts_with("why")
                || trimmed.starts_with("where")
                || trimmed.starts_with("when")
                || trimmed.starts_with("who")
                || trimmed.starts_with("which")
                || trimmed.starts_with("can you")
                || trimmed.starts_with("could you")
                || trimmed.starts_with("would you")
                || trimmed.starts_with("explain")
                || trimmed.starts_with("please explain")
                || trimmed.starts_with("describe")
                || trimmed.starts_with("please describe")
                || trimmed.starts_with("tell me")
                || trimmed.starts_with("show me")
                || trimmed.starts_with("design")
                || trimmed.starts_with("plan")
                || trimmed.ends_with('?');

            if is_question {
                continue;
            }
            return true;
        }
    }

    false
}

fn is_json_error_value(val: &Value) -> bool {
    match val {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_i64() != Some(0),
        Value::String(s) => !s.trim().is_empty(),
        Value::Object(map) => !map.is_empty(),
        Value::Array(arr) => !arr.is_empty(),
    }
}

fn has_json_error_field(lower: &str) -> bool {
    if lower.contains("\"status\": \"error\"")
        || lower.contains("\"status\":\"error\"")
        || lower.contains("\"status\": \"failed\"")
        || lower.contains("\"status\":\"failed\"")
        || lower.contains("\"status\": \"failure\"")
        || lower.contains("\"status\":\"failure\"")
        || lower.contains("\"is_error\": true")
        || lower.contains("\"is_error\":true")
    {
        return true;
    }

    for pattern in ["\"error\":", "\"error\" :"] {
        let mut search_idx = 0;
        while let Some(rel_idx) = lower[search_idx..].find(pattern) {
            let start = search_idx + rel_idx + pattern.len();
            let after = lower[start..].trim_start();
            if after.starts_with("null")
                || after.starts_with("false")
                || after.starts_with('0')
                || after.starts_with("\"\"")
                || after.starts_with("{}")
                || after.starts_with("[]")
            {
                search_idx = start;
                continue;
            }
            if after.starts_with("true")
                || after.starts_with('{')
                || after.starts_with('[')
                || after.starts_with('"')
                || after
                    .chars()
                    .next()
                    .is_some_and(|c| c.is_ascii_digit() && c != '0')
            {
                return true;
            }
            search_idx = start;
        }
    }
    false
}

fn has_nonzero_exit_code(lower: &str) -> bool {
    let prefixes = [
        "exit code",
        "exit status",
        "exited with code",
        "exited with status",
        "exit:",
    ];

    for prefix in prefixes {
        let mut search_idx = 0;
        while let Some(rel_idx) = lower[search_idx..].find(prefix) {
            let start = search_idx + rel_idx + prefix.len();
            let remaining = lower[start..]
                .trim_start_matches(|c: char| c == ':' || c == '=' || c.is_whitespace());

            if let Some(first_char) = remaining.chars().next() {
                if first_char == '0' {
                    let after_zero = remaining[1..].chars().next();
                    if after_zero.is_none() || !after_zero.unwrap().is_ascii_digit() {
                        // Exit code 0 -> NOT an error. Advance past this match.
                        search_idx = start;
                        continue;
                    }
                } else if first_char.is_ascii_digit() || first_char == '-' {
                    return true;
                }
            }
            search_idx = start;
        }
    }

    if (lower.contains("non-zero exit code")
        || lower.contains("command failed with exit")
        || lower.contains("failed with exit code")
        || lower.contains("crashed with exit code"))
        && !lower.contains("exit code 0")
        && !lower.contains("exit code: 0")
    {
        return true;
    }

    false
}

fn has_test_failures(lower: &str) -> bool {
    if lower.contains("test result: failed")
        || lower.contains("test result: fail")
        || lower.contains("\nfail:")
        || lower.contains("failed (failures=")
        || lower.contains("failed (errors=")
        || lower.contains("tests failed")
    {
        return true;
    }

    let failure_words = ["failed", "failure", "failures"];
    for word in failure_words {
        let mut search_idx = 0;
        while let Some(rel_idx) = lower[search_idx..].find(word) {
            let word_start = search_idx + rel_idx;
            let word_end = word_start + word.len();

            // Check preceding token (e.g. "0 failed" vs "3 failed")
            let before = lower[..word_start].trim_end();
            if let Some(last_num_str) = before
                .split(|c: char| !c.is_ascii_digit())
                .rfind(|s| !s.is_empty())
            {
                let num_end_pos = before.rfind(last_num_str).unwrap() + last_num_str.len();
                let between = &lower[num_end_pos..word_start];
                if between.chars().all(|c| c.is_whitespace())
                    && let Ok(num) = last_num_str.parse::<u64>()
                {
                    if num > 0 {
                        return true;
                    } else {
                        // "0 failed", "0 failures" -> NOT an error
                        search_idx = word_end;
                        continue;
                    }
                }
            }

            // Check following token (e.g. "failed: 0" vs "failed: 2")
            let after = lower[word_end..]
                .trim_start_matches(|c: char| c == ':' || c == '=' || c.is_whitespace());
            if let Some(first_char) = after.chars().next() {
                if first_char == '0' {
                    let after_zero = after[1..].chars().next();
                    if after_zero.is_none() || !after_zero.unwrap().is_ascii_digit() {
                        // "failed: 0" -> NOT an error
                        search_idx = word_end;
                        continue;
                    }
                } else if first_char.is_ascii_digit() {
                    return true;
                }
            }

            // Check phrases like "command failed", "compilation failed"
            let preceding = &lower[..word_start];
            if preceding.ends_with("command ")
                || preceding.ends_with("build ")
                || preceding.ends_with("compilation ")
                || preceding.ends_with("process ")
                || preceding.ends_with("execution ")
                || preceding.ends_with("assertion ")
            {
                return true;
            }

            let following = &lower[word_end..];
            if following.starts_with(" with ")
                || following.starts_with(" due to ")
                || following.starts_with(" to ")
            {
                return true;
            }

            search_idx = word_end;
        }
    }

    false
}

fn has_compiler_or_runtime_error(lower: &str) -> bool {
    if lower.contains("error[")
        || lower.contains("error: aborting due to")
        || lower.contains("could not compile `")
        || lower.contains(": error:")
        || lower.contains("fatal error:")
        || lower.contains("fatal:")
        || lower.contains("something went wrong")
        || lower.contains("an error occurred")
        || lower.contains("error occurred")
        || lower.contains("task failed")
        || lower.contains("operation failed")
        || lower.contains("execution failed")
        || lower.contains("step failed")
        || lower.contains("action failed")
        || lower.contains("command failed")
        || lower.contains("assertion failed")
        || lower.contains("build failed")
        || lower.contains("compilation failed")
        || lower.contains("traceback (most recent call last)")
        || lower.contains("stack backtrace:")
        || lower.contains("stack trace:")
        || lower.contains("stacktrace:")
        || lower.contains("uncaught exception")
        || lower.contains("unhandled exception")
        || lower.contains("syntaxerror:")
        || lower.contains("typeerror:")
        || lower.contains("referenceerror:")
        || lower.contains("valueerror:")
        || lower.contains("nameerror:")
        || lower.contains("indexerror:")
        || lower.contains("keyerror:")
        || lower.contains("attributeerror:")
        || lower.contains("zerodivisionerror:")
        || lower.contains("runtimeerror:")
        || lower.contains("command not found")
        || lower.contains("no such file or directory")
        || lower.contains("permission denied")
        || lower.contains("connection refused")
        || lower.contains("operation timed out")
        || lower.contains("segmentation fault")
        || lower.contains("core dumped")
        || lower.contains("broken pipe")
        || lower.contains("address already in use")
    {
        return true;
    }
    false
}

fn has_panic_error(lower: &str) -> bool {
    if lower.contains("panicked at")
        || (lower.contains("thread '") && lower.contains("' panicked"))
        || lower.contains("panic: ")
        || lower.contains("fatal panic")
        || lower.contains("kernel panic")
    {
        return true;
    }
    false
}

fn has_exception_error(lower: &str) -> bool {
    if !lower.contains("exception") {
        return false;
    }

    let is_idiom = lower.contains("with the exception of")
        || lower.contains("with exception of")
        || lower.contains("without exception")
        || lower.contains("no exception")
        || lower.contains("exception to the rule")
        || lower.contains("exceptions to the rule");

    if is_idiom {
        let sanitized = lower
            .replace("with the exception of", "")
            .replace("with exception of", "")
            .replace("without exception", "")
            .replace("no exception", "")
            .replace("exception to the rule", "")
            .replace("exceptions to the rule", "");

        sanitized.contains("exception:")
            || sanitized.contains("exception in ")
            || sanitized.contains("threw an exception")
            || sanitized.contains("raised an exception")
            || sanitized.contains("uncaught exception")
            || sanitized.contains("unhandled exception")
    } else {
        lower.contains("exception:")
            || lower.contains("exception in ")
            || lower.contains("threw an exception")
            || lower.contains("raised an exception")
            || lower.contains("uncaught exception")
            || lower.contains("unhandled exception")
    }
}

fn has_error_colon(lower: &str) -> bool {
    // 1. Direct "error:" substring with non-benign payload and word boundary check
    let mut search_idx = 0;
    while let Some(rel_idx) = lower[search_idx..].find("error:") {
        let match_pos = search_idx + rel_idx;
        let is_ident = match_pos > 0 && {
            let prev = lower[..match_pos].chars().next_back().unwrap();
            prev.is_ascii_alphanumeric() || prev == '_'
        };
        let start = match_pos + "error:".len();
        let after = lower[start..].trim_start();
        if !is_ident
            && !after.starts_with("null")
            && !after.starts_with("false")
            && !after.starts_with('0')
            && !after.starts_with("\"\"")
            && !after.starts_with("none")
            && !after.starts_with("ok")
            && !after.starts_with("success")
        {
            return true;
        }
        search_idx = start;
    }

    // 2. Line-leading error indicators with colon (e.g. "error in step 1: ...", "error in worker: ...", "[error] ...: ...")
    for line in lower.lines() {
        let trimmed = line
            .trim()
            .trim_start_matches(|c: char| c == '-' || c == '*' || c == '>' || c.is_whitespace());

        let is_error_lead = if trimmed.starts_with("[error]") || trimmed.starts_with("[error ") {
            true
        } else if let Some(rest) = trimmed.strip_prefix("error") {
            rest.starts_with(|c: char| {
                c == ':' || c == ' ' || c == '\t' || c == '[' || c == '(' || c == '-'
            })
        } else {
            false
        };

        if !is_error_lead {
            continue;
        }

        // Exclude conceptual documentation / design headings
        if trimmed.starts_with("error handling")
            || trimmed.starts_with("error handler")
            || trimmed.starts_with("error type")
            || trimmed.starts_with("error definition")
            || trimmed.starts_with("error boundary")
            || trimmed.starts_with("error boundaries")
        {
            continue;
        }

        if let Some(colon_pos) = trimmed.find(':')
            && colon_pos <= 80
        {
            let after = trimmed[colon_pos + 1..].trim_start();
            if !after.is_empty()
                && !after.starts_with("null")
                && !after.starts_with("false")
                && !after.starts_with('0')
                && !after.starts_with("\"\"")
                && !after.starts_with("none")
                && !after.starts_with("ok")
                && !after.starts_with("success")
            {
                return true;
            }
        }
    }

    false
}

fn message_full_text(msg: &ChatMessage) -> String {
    let mut out = String::new();
    if let Some(c) = &msg.content {
        match c {
            Value::String(s) => out.push_str(s),
            Value::Array(parts) => {
                for part in parts {
                    if let Some(s) = part.get("text").and_then(Value::as_str) {
                        out.push_str(s);
                        out.push(' ');
                    } else if let Some(s) = part.as_str() {
                        out.push_str(s);
                        out.push(' ');
                    }
                }
            }
            Value::Object(map) => {
                if let Some(s) = map.get("text").and_then(Value::as_str) {
                    out.push_str(s);
                }
            }
            _ => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model_spec::model_spec;
    use serde_json::json;

    #[test]
    fn test_stage_detection_planning() {
        let req = ChatCompletionRequest {
            messages: vec![ChatMessage::plain(
                "user",
                "Design a distributed event streaming architecture",
            )],
            ..Default::default()
        };
        let inspection = inspect_request(&req);
        assert_eq!(inspection.stage, AgentStage::Planning);
        assert!(!inspection.has_tools);
        assert!(!inspection.has_images);
    }

    #[test]
    fn test_stage_detection_mechanical_tool_loop() {
        let req = ChatCompletionRequest {
            messages: vec![
                ChatMessage::plain("user", "List repository files"),
                ChatMessage {
                    role: "assistant".to_string(),
                    content: None,
                    reasoning_content: None,
                    tool_calls: Some(json!([
                        {"id": "call_1", "type": "function", "function": {"name": "ls", "arguments": "{}"}}
                    ])),
                    tool_call_id: None,
                    name: None,
                },
                ChatMessage {
                    role: "tool".to_string(),
                    content: Some(json!("Cargo.toml\nsrc/lib.rs\nsrc/main.rs")),
                    reasoning_content: None,
                    tool_calls: None,
                    tool_call_id: Some("call_1".to_string()),
                    name: Some("ls".to_string()),
                },
            ],
            ..Default::default()
        };
        let inspection = inspect_request(&req);
        assert_eq!(inspection.stage, AgentStage::MechanicalToolLoop);
    }

    #[test]
    fn test_stage_detection_error_recovery_from_tool() {
        let req = ChatCompletionRequest {
            messages: vec![
                ChatMessage::plain("user", "Run tests"),
                ChatMessage {
                    role: "tool".to_string(),
                    content: Some(json!("error[E0432]: unresolved import `foo`")),
                    reasoning_content: None,
                    tool_calls: None,
                    tool_call_id: Some("call_2".to_string()),
                    name: Some("cargo_test".to_string()),
                },
            ],
            ..Default::default()
        };
        let inspection = inspect_request(&req);
        assert_eq!(inspection.stage, AgentStage::ErrorRecovery);
    }

    #[test]
    fn test_stage_detection_error_recovery_from_user() {
        let req = ChatCompletionRequest {
            messages: vec![
                ChatMessage::plain("user", "Start server"),
                ChatMessage::plain("assistant", "Server launched"),
                ChatMessage::plain(
                    "user",
                    "The process failed with exit code 1 and panic in worker",
                ),
            ],
            ..Default::default()
        };
        let inspection = inspect_request(&req);
        assert_eq!(inspection.stage, AgentStage::ErrorRecovery);
    }

    #[test]
    fn test_stage_detection_error_recovery_markers() {
        for marker in ERROR_MARKERS {
            let req = ChatCompletionRequest {
                messages: vec![ChatMessage::plain(
                    "user",
                    format!("Encountered {marker} during execution"),
                )],
                ..Default::default()
            };
            let inspection = inspect_request(&req);
            assert_eq!(
                inspection.stage,
                AgentStage::ErrorRecovery,
                "Failed to detect error marker: {marker}"
            );
        }
    }

    #[test]
    fn test_token_estimation_text_and_tools() {
        // text = 40 bytes (role + content) -> 10 tokens
        // turns = 1 -> 4 tokens
        // tools = 80 bytes -> 20 tokens
        // total = 34 tokens
        let tools_json = json!([{"name": "test_tool", "description": "A simple test tool"}]);
        let tools_len = tools_json.to_string().len();

        let req = ChatCompletionRequest {
            messages: vec![ChatMessage::plain("user", "Hello world! This is a test.")],
            tools: Some(tools_json),
            ..Default::default()
        };

        let inspection = inspect_request(&req);
        assert!(inspection.has_tools);
        assert!(!inspection.has_images);

        let role_len = "user".len();
        let content_len = "Hello world! This is a test.".len();
        let turns: u32 = 1;
        let expected_tokens =
            ((role_len + content_len) / 4) as u32 + (turns * 4) + (tools_len / 4) as u32;
        assert_eq!(inspection.estimated_tokens, expected_tokens);
    }

    #[test]
    fn test_token_estimation_with_images() {
        let req = ChatCompletionRequest {
            messages: vec![ChatMessage {
                role: "user".to_string(),
                content: Some(json!([
                    {"type": "text", "text": "Describe this photo"},
                    {"type": "image_url", "image_url": {"url": "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg=="}}
                ])),
                reasoning_content: None,
                tool_calls: None,
                tool_call_id: None,
                name: None,
            }],
            ..Default::default()
        };

        let inspection = inspect_request(&req);
        assert!(inspection.has_images);
        // Prompt includes 1 image (1120 tokens) + 1 turn (4 tokens) + text bytes / 4
        assert!(inspection.estimated_tokens >= 1124);
    }

    /// Capability-table level check: only Gemini models are vision-capable.
    ///
    /// The routing-level proof of the same criterion (a request with an image
    /// is served by a multimodal model and never by a text-only one) lives in
    /// `tests/auto_routing.rs::test_multimodal_vision_routing`, which builds a
    /// real request and runs it through the router.
    #[test]
    fn vision_capability_table_selects_only_gemini_models() {
        let req = ChatCompletionRequest {
            messages: vec![ChatMessage {
                role: "user".to_string(),
                content: Some(json!([
                    {"type": "text", "text": "What is in this architecture diagram?"},
                    {"type": "image_url", "image_url": {"url": "https://example.com/diagram.png"}}
                ])),
                reasoning_content: None,
                tool_calls: None,
                tool_call_id: None,
                name: None,
            }],
            ..Default::default()
        };

        let inspection = inspect_request(&req);
        assert!(
            inspection.has_images,
            "Request with image_url must have has_images == true"
        );

        let candidate_models = [
            "gemini-2.5-pro",
            "gemini-2.5-flash",
            "agentrouter/gpt-5.6-sol",
            "agentrouter/gpt-6.1-sol",
            "codestral-latest",
            "deepseek-chat",
            "morph-v3-large",
        ];

        let vision_models: Vec<&str> = candidate_models
            .into_iter()
            .filter(|model| model_spec(model).supports_vision)
            .collect();

        assert_eq!(
            vision_models,
            vec!["gemini-2.5-pro", "gemini-2.5-flash"],
            "Only Gemini models should be selected as vision-capable"
        );
    }

    #[test]
    fn test_tool_error_json_structures() {
        // Non-error JSON values
        let non_errors = [
            json!({"status": "success", "error": null}),
            json!({"status": "ok", "error": false}),
            json!({"error": 0, "result": "ok"}),
            json!({"error": "", "data": [1, 2]}),
            json!({"error": {}, "status": "success"}),
            json!({"error": [], "exit_code": 0}),
        ];

        for val in non_errors {
            let msg_obj = ChatMessage {
                role: "tool".to_string(),
                content: Some(val.clone()),
                reasoning_content: None,
                tool_calls: None,
                tool_call_id: Some("c1".to_string()),
                name: Some("test_tool".to_string()),
            };
            assert!(
                !is_tool_error(&msg_obj),
                "JSON object should not be an error: {val:?}"
            );

            let msg_str = ChatMessage {
                role: "tool".to_string(),
                content: Some(json!(val.to_string())),
                reasoning_content: None,
                tool_calls: None,
                tool_call_id: Some("c1".to_string()),
                name: Some("test_tool".to_string()),
            };
            assert!(
                !is_tool_error(&msg_str),
                "Stringified JSON should not be an error: {val:?}"
            );
        }

        // Genuine error JSON values
        let errors = [
            json!({"error": "network timeout", "code": 504}),
            json!({"status": "failed", "details": "unknown error"}),
            json!({"status": "error", "message": "bad request"}),
            json!({"is_error": true, "msg": "failed"}),
            json!({"exit_code": 1}),
            json!({"error": {"code": 500, "message": "server error"}}),
        ];

        for val in errors {
            let msg_obj = ChatMessage {
                role: "tool".to_string(),
                content: Some(val.clone()),
                reasoning_content: None,
                tool_calls: None,
                tool_call_id: Some("c1".to_string()),
                name: Some("test_tool".to_string()),
            };
            assert!(
                is_tool_error(&msg_obj),
                "JSON object should be detected as error: {val:?}"
            );

            let msg_str = ChatMessage {
                role: "tool".to_string(),
                content: Some(json!(val.to_string())),
                reasoning_content: None,
                tool_calls: None,
                tool_call_id: Some("c1".to_string()),
                name: Some("test_tool".to_string()),
            };
            assert!(
                is_tool_error(&msg_str),
                "Stringified JSON should be detected as error: {val:?}"
            );
        }
    }

    #[test]
    fn test_tool_error_exit_codes_and_test_counters() {
        let successes = [
            "Process completed successfully with exit code 0",
            "Command exited with code 0",
            "exit status: 0",
            "test result: ok. 40 passed; 0 failed; 0 ignored",
            "Tests passed: 10, failed: 0, skipped: 0",
            "0 failures found in 100 test cases",
        ];

        for text in successes {
            let msg = ChatMessage {
                role: "tool".to_string(),
                content: Some(json!(text)),
                reasoning_content: None,
                tool_calls: None,
                tool_call_id: Some("c1".to_string()),
                name: Some("sh".to_string()),
            };
            assert!(!is_tool_error(&msg), "Should not be error: {text}");
        }

        let failures = [
            "Process failed with exit code 1",
            "Command exited with code 127: command not found",
            "exit status: 2",
            "test result: FAILED. 38 passed; 2 failed; 0 ignored",
            "Tests passed: 8, failed: 2",
            "3 failures encountered during test run",
        ];

        for text in failures {
            let msg = ChatMessage {
                role: "tool".to_string(),
                content: Some(json!(text)),
                reasoning_content: None,
                tool_calls: None,
                tool_call_id: Some("c1".to_string()),
                name: Some("sh".to_string()),
            };
            assert!(is_tool_error(&msg), "Should be detected as error: {text}");
        }
    }

    #[test]
    fn test_tool_error_code_mentions_and_idioms() {
        let benign_snippets = [
            "[package]\nname = \"app\"\n[profile.release]\npanic = \"abort\"",
            "pub enum Error { NotFound, PermissionDenied }",
            "pub enum RouterError { Timeout, Unavailable }",
            "struct CustomError { code: i32 }",
            "class NetworkException {}",
            "export type DbError = { message: string };",
            "All users are verified with the exception of user 42",
            "Matched 3 lines:\nsrc/lib.rs:5: fn handle_error()",
            "Matched 1 line:\nsrc/lib.rs:5: fn handle_error()",
            "5 matches found:\nsrc/core.rs:10: let x = 1;\nsrc/core.rs:15: let y = 2;",
        ];

        for snippet in benign_snippets {
            let msg = ChatMessage {
                role: "tool".to_string(),
                content: Some(json!(snippet)),
                reasoning_content: None,
                tool_calls: None,
                tool_call_id: Some("c1".to_string()),
                name: Some("tool".to_string()),
            };
            assert!(
                !is_tool_error(&msg),
                "Benign code/idiom snippet should not be error: {snippet}"
            );
        }

        // Search outputs with real errors must NOT be masked
        let real_error_snippets = [
            "Matched 3 lines:\nsrc/lib.rs:5: fn handle_error()\nerror[E0425]: cannot find value",
            "Matched 2 lines:\nsrc/lib.rs:5: fn handle_error()\nexit code: 1",
            "pub enum RouterError { Timeout }\ncommand not found: cargo",
            "error in worker: failed to load configuration",
        ];

        for snippet in real_error_snippets {
            let msg = ChatMessage {
                role: "tool".to_string(),
                content: Some(json!(snippet)),
                reasoning_content: None,
                tool_calls: None,
                tool_call_id: Some("c1".to_string()),
                name: Some("tool".to_string()),
            };
            assert!(
                is_tool_error(&msg),
                "Real error mixed in snippet must be detected: {snippet}"
            );
        }
    }

    #[test]
    fn test_user_error_report_conceptual_vs_incident() {
        let conceptual_queries = [
            "How do I design robust error handling in Rust without using unwrap?",
            "Explain how error boundaries work in React",
            "Can you explain error types in TypeScript?",
            "What is the best pattern for handling exceptions in Python?",
        ];

        for q in conceptual_queries {
            let msg = ChatMessage::plain("user", q);
            assert!(
                !is_user_error_report(&msg),
                "Conceptual question should not be error report: {q}"
            );
        }

        let incident_reports = [
            "Encountered error during execution",
            "The worker process crashed with segmentation fault",
            "Build failed with error[E0425]: cannot find value",
            "The test failed with exit code 1",
            "Execution failed due to uncaught exception in thread main",
        ];

        for r in incident_reports {
            let msg = ChatMessage::plain("user", r);
            assert!(
                is_user_error_report(&msg),
                "Incident report should be detected as error: {r}"
            );
        }
    }

    #[test]
    fn test_stage_detection_post_tool_and_prefill() {
        // Multi-turn post-tool transition to Planning
        let req_post_tool = ChatCompletionRequest {
            messages: vec![
                ChatMessage::plain("user", "List files"),
                ChatMessage {
                    role: "assistant".to_string(),
                    content: None,
                    reasoning_content: None,
                    tool_calls: Some(
                        json!([{"id": "c1", "type": "function", "function": {"name": "ls", "arguments": "{}"}}]),
                    ),
                    tool_call_id: None,
                    name: None,
                },
                ChatMessage {
                    role: "tool".to_string(),
                    content: Some(json!("Cargo.toml\nsrc/")),
                    reasoning_content: None,
                    tool_calls: None,
                    tool_call_id: Some("c1".to_string()),
                    name: Some("ls".to_string()),
                },
                ChatMessage::plain("assistant", "Found Cargo.toml and src/."),
                ChatMessage::plain("user", "Now, plan the architecture for our auth service."),
            ],
            ..Default::default()
        };
        assert_eq!(detect_stage(&req_post_tool.messages), AgentStage::Planning);

        // Assistant prefill over tool success
        let req_prefill_tool = ChatCompletionRequest {
            messages: vec![
                ChatMessage::plain("user", "List files"),
                ChatMessage {
                    role: "assistant".to_string(),
                    content: None,
                    reasoning_content: None,
                    tool_calls: Some(
                        json!([{"id": "c1", "type": "function", "function": {"name": "ls", "arguments": "{}"}}]),
                    ),
                    tool_call_id: None,
                    name: None,
                },
                ChatMessage {
                    role: "tool".to_string(),
                    content: Some(json!("file1.txt")),
                    reasoning_content: None,
                    tool_calls: None,
                    tool_call_id: Some("c1".to_string()),
                    name: Some("ls".to_string()),
                },
                ChatMessage::plain("assistant", "The files are:"),
            ],
            ..Default::default()
        };
        assert_eq!(
            detect_stage(&req_prefill_tool.messages),
            AgentStage::MechanicalToolLoop
        );

        // Assistant prefill over tool failure
        let req_prefill_tool_err = ChatCompletionRequest {
            messages: vec![
                ChatMessage::plain("user", "Run build"),
                ChatMessage {
                    role: "assistant".to_string(),
                    content: None,
                    reasoning_content: None,
                    tool_calls: Some(
                        json!([{"id": "c1", "type": "function", "function": {"name": "sh", "arguments": "{}"}}]),
                    ),
                    tool_call_id: None,
                    name: None,
                },
                ChatMessage {
                    role: "tool".to_string(),
                    content: Some(json!("error[E0425]: cannot find value")),
                    reasoning_content: None,
                    tool_calls: None,
                    tool_call_id: Some("c1".to_string()),
                    name: Some("sh".to_string()),
                },
                ChatMessage::plain("assistant", "I see the build failed with:"),
            ],
            ..Default::default()
        };
        assert_eq!(
            detect_stage(&req_prefill_tool_err.messages),
            AgentStage::ErrorRecovery
        );
    }
}

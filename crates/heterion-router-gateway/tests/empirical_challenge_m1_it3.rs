use heterion_router_core::{ChatCompletionRequest, ChatMessage};
use heterion_router_gateway::inspector::{AgentStage, inspect_request, is_tool_error};
use serde_json::json;

fn make_tool_msg(name: &str, content: serde_json::Value) -> ChatMessage {
    ChatMessage {
        role: "tool".to_string(),
        content: Some(content),
        reasoning_content: None,
        tool_calls: None,
        tool_call_id: Some("call_test_1".to_string()),
        name: Some(name.to_string()),
    }
}

fn make_request_with_tool(name: &str, content: &str) -> ChatCompletionRequest {
    ChatCompletionRequest {
        messages: vec![
            ChatMessage::plain("user", "Perform task"),
            ChatMessage {
                role: "assistant".to_string(),
                content: None,
                reasoning_content: None,
                tool_calls: Some(json!([
                    {"id": "call_test_1", "type": "function", "function": {"name": name, "arguments": "{}"}}
                ])),
                tool_call_id: None,
                name: None,
            },
            make_tool_msg(name, json!(content)),
        ],
        ..Default::default()
    }
}

#[test]
fn test_dispatch_required_non_test_variations() {
    // Requirements from Dispatch:
    // - Non-test variations (e.g. `pub enum RouterError`, `struct DbError`, `Matched 1 line:`, `5 matches found:`) work correctly.

    let benign_cases = [
        ("sh", "pub enum RouterError { NotFound, Timeout }"),
        ("sh", "struct DbError { code: u32, message: String }"),
        (
            "grep",
            "Matched 1 line:\nsrc/router.rs:12: pub enum RouterError {",
        ),
        (
            "code_search",
            "5 matches found:\nsrc/db.rs:1: struct DbError;\nsrc/db.rs:5: fn query()",
        ),
        (
            "view_file",
            "pub(crate) enum RouterError { Network, Unavailable }",
        ),
        ("view_file", "pub struct DbError(pub String);"),
        ("view_file", "class NetworkException extends Exception {}"),
        (
            "view_file",
            "export type DbError = { code: number; msg: string };",
        ),
        (
            "grep",
            "Matched 1 line:\nsrc/handler.rs:4: fn handle_error()",
        ),
        (
            "ripgrep",
            "3 lines matched:\nsrc/a.rs:1: fn a()\nsrc/b.rs:2: fn b()\nsrc/c.rs:3: fn c()",
        ),
        (
            "search",
            "found 2 lines:\nsrc/foo.rs:10: let x = 1;\nsrc/bar.rs:20: let y = 2;",
        ),
        ("grep_search", "search results:\nsrc/main.rs:10: fn main()"),
        ("ag", "grep results:\nsrc/lib.rs:1: // library"),
        (
            "find_by_name",
            "found 1 match:\ncrates/heterion-router-gateway/src/inspector.rs",
        ),
        (
            "search_code",
            "1 line matched:\nsrc/main.rs:20: let error_count = 0;",
        ),
    ];

    for (tool_name, text) in benign_cases {
        let msg = make_tool_msg(tool_name, json!(text));
        assert!(
            !is_tool_error(&msg),
            "Expected benign output to NOT be tool error for '{text}' (tool: {tool_name})"
        );

        let req = make_request_with_tool(tool_name, text);
        let insp = inspect_request(&req);
        assert_eq!(
            insp.stage,
            AgentStage::MechanicalToolLoop,
            "Expected MechanicalToolLoop for benign snippet '{text}', got {:?}",
            insp.stage
        );
    }
}

#[test]
fn test_dispatch_required_real_errors_in_search_and_code() {
    // Requirements from Dispatch:
    // - Real errors embedded inside search outputs or code definitions (e.g. `Matched 3 lines:\n...\nerror[E0425]: ...`) correctly trigger `AgentStage::ErrorRecovery`.

    let error_cases = [
        (
            "grep",
            "Matched 3 lines:\nsrc/main.rs:12: pub enum AppError {\nerror[E0425]: cannot find value `foo` in this scope",
        ),
        (
            "code_search",
            "5 matches found:\nsrc/db.rs:1: struct DbError;\nthread 'main' panicked at 'assertion failed'",
        ),
        (
            "sh",
            "pub enum RouterError { Timeout }\nerror[E0425]: cannot find value `bar`",
        ),
        (
            "view_file",
            "struct DbError { reason: String }\ncommand failed with exit code 1",
        ),
        (
            "view_file",
            "export type DbError = { code: string };\nexit code 127",
        ),
        (
            "grep",
            "Matched 1 line:\nsrc/lib.rs:5: fn handle_error()\nexit status: 1",
        ),
        (
            "ripgrep",
            "found 2 lines:\nsrc/a.rs:1: fn a()\ntest result: FAILED. 2 failed",
        ),
        (
            "search",
            "search results:\nsrc/main.rs:10: fn main()\nTraceback (most recent call last):\nValueError: bad",
        ),
        (
            "grep_search",
            "grep results:\nsrc/lib.rs:1: // lib\n/bin/sh: command not found",
        ),
        (
            "find_by_name",
            "found 1 match:\nsrc/main.rs\npermission denied: /root/secure.key",
        ),
        (
            "search_code",
            "1 line matched:\nsrc/lib.rs:1\nfatal error: unable to allocate memory",
        ),
    ];

    for (tool_name, text) in error_cases {
        let msg = make_tool_msg(tool_name, json!(text));
        assert!(
            is_tool_error(&msg),
            "Expected real error to be detected as tool error for '{text}' (tool: {tool_name})"
        );

        let req = make_request_with_tool(tool_name, text);
        let insp = inspect_request(&req);
        assert_eq!(
            insp.stage,
            AgentStage::ErrorRecovery,
            "Expected ErrorRecovery for error snippet '{text}', got {:?}",
            insp.stage
        );
    }
}

#[test]
fn test_multiline_grep_output_without_header() {
    // Direct grep output lines without summary header:
    // `<file>:<line>:<content>`
    let raw_grep_benign = "src/errors.rs:10:pub enum RouterError {\nsrc/errors.rs:11:    Timeout,\nsrc/errors.rs:12:    Unavailable,\nsrc/errors.rs:13:}";
    let msg = make_tool_msg("custom_grep", json!(raw_grep_benign));
    assert!(
        !is_tool_error(&msg),
        "Multi-line raw grep output of enum RouterError should not be error"
    );

    let raw_grep_with_err = "src/errors.rs:10:pub enum RouterError {\nsrc/errors.rs:11:    Timeout,\nerror[E0425]: unresolved import";
    let msg_err = make_tool_msg("custom_grep", json!(raw_grep_with_err));
    assert!(
        is_tool_error(&msg_err),
        "Multi-line raw grep with compiler error must be detected as error"
    );
}

#[test]
fn test_parallel_tool_executions_stage() {
    // Parallel tools: one returns benign code search, the other returns an error
    let req_mixed_parallel = ChatCompletionRequest {
        messages: vec![
            ChatMessage::plain("user", "Check status"),
            ChatMessage {
                role: "assistant".to_string(),
                content: None,
                reasoning_content: None,
                tool_calls: Some(json!([
                    {"id": "c1", "type": "function", "function": {"name": "grep", "arguments": "{}"}},
                    {"id": "c2", "type": "function", "function": {"name": "sh", "arguments": "{}"}}
                ])),
                tool_call_id: None,
                name: None,
            },
            make_tool_msg(
                "grep",
                json!("Matched 1 line:\nsrc/lib.rs:1: pub enum RouterError"),
            ),
            make_tool_msg("sh", json!("exit code 1")),
        ],
        ..Default::default()
    };
    let insp_mixed = inspect_request(&req_mixed_parallel);
    assert_eq!(
        insp_mixed.stage,
        AgentStage::ErrorRecovery,
        "Any tool error in trailing parallel tools must trigger ErrorRecovery"
    );

    // Parallel tools: both return benign code snippets
    let req_all_benign = ChatCompletionRequest {
        messages: vec![
            ChatMessage::plain("user", "Check status"),
            ChatMessage {
                role: "assistant".to_string(),
                content: None,
                reasoning_content: None,
                tool_calls: Some(json!([
                    {"id": "c1", "type": "function", "function": {"name": "grep", "arguments": "{}"}},
                    {"id": "c2", "type": "function", "function": {"name": "view_file", "arguments": "{}"}}
                ])),
                tool_call_id: None,
                name: None,
            },
            make_tool_msg(
                "grep",
                json!("Matched 1 line:\nsrc/lib.rs:1: pub enum RouterError"),
            ),
            make_tool_msg("view_file", json!("struct DbError { id: i64 }")),
        ],
        ..Default::default()
    };
    let insp_all_benign = inspect_request(&req_all_benign);
    assert_eq!(
        insp_all_benign.stage,
        AgentStage::MechanicalToolLoop,
        "All benign parallel tools must remain in MechanicalToolLoop"
    );
}

#[test]
fn test_universal_failure_expressions() {
    // Verification of the universal failure expressions added to replace "error at step "
    let failure_expressions = [
        "something went wrong",
        "an error occurred",
        "error occurred",
        "task failed",
        "operation failed",
        "execution failed",
        "step failed",
        "action failed",
    ];

    for expr in failure_expressions {
        let msg = make_tool_msg(
            "worker",
            json!(format!("Execution status: {expr} in step 3")),
        );
        assert!(
            is_tool_error(&msg),
            "Universal failure expression '{expr}' must be detected as tool error"
        );

        let req = make_request_with_tool("worker", &format!("Worker reported: {expr}"));
        let insp = inspect_request(&req);
        assert_eq!(
            insp.stage,
            AgentStage::ErrorRecovery,
            "Universal failure expression '{expr}' must trigger ErrorRecovery"
        );
    }
}

#[test]
fn test_user_error_recovery_variations() {
    // Conceptual user questions mentioning error types or concepts must classify as Planning
    let conceptual_questions = [
        "Can you explain the structure of `struct DbError`?",
        "Can you explain the structure of struct DbError?",
        "What is an Error?",
        "What is a DbError?",
        "Explain how RouterError works",
        "Explain the structure of RouterError",
        "How do I design robust error handling in Rust?",
        "Could you explain how errors are propagated in Axum?",
        "Please explain how custom error types are defined in Rust",
        "Design an Error enum for the HTTP client",
        "Plan the refactoring of our error handling architecture",
    ];

    for q in conceptual_questions {
        let req = ChatCompletionRequest {
            messages: vec![ChatMessage::plain("user", q)],
            ..Default::default()
        };
        let insp = inspect_request(&req);
        assert_eq!(
            insp.stage,
            AgentStage::Planning,
            "Conceptual user question should be Planning: '{q}', got {:?}",
            insp.stage
        );
    }

    // Concrete incident and crash reports must classify as ErrorRecovery
    let real_incident_reports = [
        "How do I fix error[E0425]: cannot find value `foo` in this scope?",
        "Why did I get error[E0308]: mismatched types expected `usize`, found `u32`?",
        "How can I resolve this panic: thread 'main' panicked at 'index out of bounds'?",
        "The command failed with exit code 127, how do I resolve it?",
        "How do I fix test result: FAILED. 2 failed?",
        "Our build failed with compilation error in src/lib.rs",
        "The worker process crashed with segmentation fault",
        "Encountered error during execution of database migration",
        "Traceback (most recent call last):\nValueError: invalid literal for int()",
        "Execution failed due to uncaught exception in thread main",
        "There is an error in the authentication service preventing login",
    ];

    for r in real_incident_reports {
        let req = ChatCompletionRequest {
            messages: vec![ChatMessage::plain("user", r)],
            ..Default::default()
        };
        let insp = inspect_request(&req);
        assert_eq!(
            insp.stage,
            AgentStage::ErrorRecovery,
            "Real error report should be ErrorRecovery: '{r}', got {:?}",
            insp.stage
        );
    }
}

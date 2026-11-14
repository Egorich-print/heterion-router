use heterion_router_core::ChatMessage;
use heterion_router_gateway::inspector::is_tool_error;
use serde_json::json;

fn make_tool_msg(content: serde_json::Value) -> ChatMessage {
    ChatMessage {
        role: "tool".to_string(),
        content: Some(content),
        reasoning_content: None,
        tool_calls: None,
        tool_call_id: Some("call_test_1".to_string()),
        name: Some("test_tool".to_string()),
    }
}

#[test]
fn test_empirical_false_positives_exact_requirements() {
    // Requirements:
    // `is_tool_error` returns false for:
    // - `exit code 0`
    // - `exit status: 0`
    // - `0 failed`
    // - `failed: 0`
    // - `{"error": null}`
    // - `panic = "abort"`
    // - `pub enum Error`
    // - `with the exception of`

    let false_positive_cases = [
        // 1. exit code 0
        "exit code 0",
        "Command finished with exit code 0",
        "process completed with exit code 0",
        "Process exited with exit code: 0",
        // 2. exit status: 0
        "exit status: 0",
        "process finished with exit status: 0",
        "exit status 0",
        // 3. 0 failed
        "0 failed",
        "test result: ok. 40 passed; 0 failed; 0 ignored",
        "25 passed, 0 failed, 2 skipped",
        // 4. failed: 0
        "failed: 0",
        "tests: 50, passed: 50, failed: 0",
        "failed:0",
        // 5. panic = "abort"
        "panic = \"abort\"",
        "profile.release.panic = \"abort\"",
        "[profile.release]\npanic = \"abort\"\n",
        "panic = 'abort'",
        // 6. pub enum Error
        "pub enum Error",
        "pub enum Error { NotFound, Invalid }",
        "enum Error { Foo, Bar }",
        "pub(crate) enum Error { Baz }",
        // 7. with the exception of
        "with the exception of",
        "All tests succeeded with the exception of none",
        "verified all items with the exception of cached entries",
        "executed without exception",
    ];

    for text in false_positive_cases {
        let msg = make_tool_msg(json!(text));
        assert!(
            !is_tool_error(&msg),
            "Expected false for benign input: '{text}', got true"
        );
    }

    // JSON {"error": null} structured object and stringified
    let null_error_obj = make_tool_msg(json!({"error": null}));
    assert!(
        !is_tool_error(&null_error_obj),
        "Expected false for JSON object {{\"error\": null}}"
    );

    let null_error_str = make_tool_msg(json!("{\"error\": null}"));
    assert!(
        !is_tool_error(&null_error_str),
        "Expected false for stringified JSON {{\"error\": null}}"
    );

    let null_error_extended =
        make_tool_msg(json!({"status": "success", "error": null, "data": [1, 2, 3]}));
    assert!(
        !is_tool_error(&null_error_extended),
        "Expected false for extended JSON with error: null"
    );
}

#[test]
fn test_empirical_true_positives_exact_requirements() {
    // Requirements:
    // `is_tool_error` returns true for:
    // - non-zero exit codes
    // - test failures
    // - compiler errors
    // - panics
    // - tracebacks
    // - system errors

    // 1. Non-zero exit codes
    let nonzero_exit_cases = [
        "exit code 1",
        "exit code 2",
        "exit code 127",
        "exit code 255",
        "exit code -1",
        "exit status: 1",
        "exit status: 137",
        "exited with code 1",
        "Process exited with exit code 1",
        "exit: 2",
    ];
    for text in nonzero_exit_cases {
        let msg = make_tool_msg(json!(text));
        assert!(
            is_tool_error(&msg),
            "Expected true for non-zero exit code: '{text}', got false"
        );
    }

    // 2. Test failures
    let test_failure_cases = [
        "1 failed",
        "2 failed",
        "10 failed",
        "failed: 1",
        "failed: 3",
        "failed:1",
        "test result: FAILED. 3 passed; 1 failed",
        "1 failure detected",
        "failures: 2",
        "failures:1",
        "tests failed",
    ];
    for text in test_failure_cases {
        let msg = make_tool_msg(json!(text));
        assert!(
            is_tool_error(&msg),
            "Expected true for test failure: '{text}', got false"
        );
    }

    // 3. Compiler errors
    let compiler_error_cases = [
        "error[E0425]: cannot find value `foo` in this scope",
        "error: aborting due to 2 previous errors",
        "could not compile `my_crate` (bin \"my_crate\") due to 1 previous error",
        "main.c:14:2: error: expected ';' before '}' token",
        "fatal error: stdio.h: No such file or directory",
        "compilation failed",
        "build failed",
        "assertion failed: x == y",
    ];
    for text in compiler_error_cases {
        let msg = make_tool_msg(json!(text));
        assert!(
            is_tool_error(&msg),
            "Expected true for compiler error: '{text}', got false"
        );
    }

    // 4. Panics
    let panic_cases = [
        "thread 'main' panicked at 'explicit panic', src/main.rs:10:5",
        "panicked at 'index out of bounds: the len is 3 but the index is 5'",
        "panic: runtime error: invalid memory address or nil pointer dereference",
        "fatal panic in worker thread",
        "kernel panic: unable to handle kernel paging request",
    ];
    for text in panic_cases {
        let msg = make_tool_msg(json!(text));
        assert!(
            is_tool_error(&msg),
            "Expected true for panic: '{text}', got false"
        );
    }

    // 5. Tracebacks
    let traceback_cases = [
        "Traceback (most recent call last):\n  File \"app.py\", line 12, in <module>\nValueError: invalid literal",
        "stack backtrace:\n   0: std::panicking::begin_panic\n   1: app::run",
        "stack trace:\nError: connection reset\n    at TCP.onStreamRead",
        "stacktrace:\njava.lang.NullPointerException",
    ];
    for text in traceback_cases {
        let msg = make_tool_msg(json!(text));
        assert!(
            is_tool_error(&msg),
            "Expected true for traceback: '{text}', got false"
        );
    }

    // 6. System errors
    let system_error_cases = [
        "/bin/sh: line 1: foo_cmd: command not found",
        "permission denied: /etc/shadow",
        "connection refused: 127.0.0.1:8080",
        "operation timed out after 30000ms",
        "segmentation fault (core dumped)",
        "broken pipe",
        "address already in use: 0.0.0.0:20128",
        "no such file or directory: /tmp/missing.txt",
    ];
    for text in system_error_cases {
        let msg = make_tool_msg(json!(text));
        assert!(
            is_tool_error(&msg),
            "Expected true for system error: '{text}', got false"
        );
    }
}

#[test]
fn test_empirical_adversarial_combinations() {
    // Combined cases: benign marker mixed with real error must still be detected as TRUE
    let combined_cases = [
        "exit code 0\nerror[E0432]: unresolved import",
        "0 failed\nthread 'main' panicked at 'failed assertion'",
        "pub enum Error { Ok }\ncommand not found: rustc",
        "panic = \"abort\"\nsegmentation fault",
        "All verified with the exception of none\nTraceback (most recent call last):",
    ];
    for text in combined_cases {
        let msg = make_tool_msg(json!(text));
        assert!(
            is_tool_error(&msg),
            "Expected true for benign marker combined with real error: '{text}'"
        );
    }

    // Non-zero exit code combined with JSON
    let json_exit_err = make_tool_msg(json!({"exit_code": 1, "status": "failed"}));
    assert!(is_tool_error(&json_exit_err));

    let json_exit_success =
        make_tool_msg(json!({"exit_code": 0, "status": "success", "error": null}));
    assert!(!is_tool_error(&json_exit_success));
}

#[test]
fn test_empirical_more_edge_cases() {
    // 1. Whitespace and formatting variations
    let spacing_cases = [
        ("exit code:   0", false),
        ("exit status:   0", false),
        ("exit code = 0", false),
        ("exit:   0", false),
        ("0   failed", false),
        ("failed:   0", false),
        ("failed = 0", false),
        ("0   failures", false),
        ("failures:   0", false),
        ("failures = 0", false),
        // Positives
        ("exit code:   1", true),
        ("exit status:   2", true),
        ("exit code = 1", true),
        ("exit:   1", true),
        ("2   failed", true),
        ("failed:   3", true),
        ("failed = 4", true),
        ("3   failures", true),
        ("failures:   2", true),
        ("failures = 1", true),
    ];
    for (text, expected) in spacing_cases {
        let msg = make_tool_msg(json!(text));
        assert_eq!(
            is_tool_error(&msg),
            expected,
            "Mismatch for spacing case '{text}': expected {expected}"
        );
    }

    // 2. Prose mentioning 'exit code' without number
    let prose_cases = [
        ("Please inspect the exit code of the last command", false),
        ("The function inspects exit code values", false),
        ("Exit status is recorded in telemetry", false),
    ];
    for (text, expected) in prose_cases {
        let msg = make_tool_msg(json!(text));
        assert_eq!(
            is_tool_error(&msg),
            expected,
            "Mismatch for prose case '{text}': expected {expected}"
        );
    }

    // 3. Various JSON error value shapes
    let json_cases = [
        (json!({"error": false}), false),
        (json!({"error": 0}), false),
        (json!({"error": ""}), false),
        (json!({"error": []}), false),
        (json!({"error": {}}), false),
        (json!({"error": "something went wrong"}), true),
        (json!({"error": {"code": 404}}), true),
        (json!({"error": [1, 2]}), true),
        (json!({"error": true}), true),
        (json!({"error": 1}), true),
    ];
    for (val, expected) in json_cases {
        let msg_obj = make_tool_msg(val.clone());
        assert_eq!(
            is_tool_error(&msg_obj),
            expected,
            "Mismatch for JSON object: {val:?}"
        );

        let msg_str = make_tool_msg(json!(val.to_string()));
        assert_eq!(
            is_tool_error(&msg_str),
            expected,
            "Mismatch for stringified JSON: {val:?}"
        );
    }
}

#[test]
fn test_comprehensive_error_taxonomy() {
    let system_and_runtime_diagnostics = [
        ("error[E0425]: cannot find value `foo` in this scope", true),
        ("error: aborting due to previous error", true),
        ("could not compile `heterion-router`", true),
        ("main.c:10: error: syntax error", true),
        ("fatal error: stdio.h not found", true),
        ("fatal: not a git repository", true),
        ("command failed with exit code 1", true),
        ("assertion failed: x == 1", true),
        ("build failed", true),
        ("compilation failed", true),
        ("Traceback (most recent call last):", true),
        ("stack backtrace:", true),
        ("stack trace:", true),
        ("stacktrace:", true),
        ("uncaught exception", true),
        ("unhandled exception", true),
        ("SyntaxError: unexpected token", true),
        ("TypeError: undefined is not a function", true),
        ("ReferenceError: x is not defined", true),
        ("ValueError: invalid literal", true),
        ("NameError: name 'x' is not defined", true),
        ("IndexError: list index out of range", true),
        ("KeyError: 'foo'", true),
        ("AttributeError: object has no attribute", true),
        ("ZeroDivisionError: division by zero", true),
        ("RuntimeError: maximum recursion depth exceeded", true),
        ("command not found", true),
        ("no such file or directory", true),
        ("permission denied", true),
        ("connection refused", true),
        ("operation timed out", true),
        ("segmentation fault", true),
        ("core dumped", true),
        ("broken pipe", true),
        ("address already in use", true),
        // Panics
        ("panicked at 'explicit panic'", true),
        ("thread 'worker' panicked at 'bad unwrap'", true),
        ("panic: runtime error: index out of bounds", true),
        ("fatal panic in executor", true),
        ("kernel panic", true),
    ];

    for (text, expected) in system_and_runtime_diagnostics {
        let msg = make_tool_msg(json!(text));
        assert_eq!(
            is_tool_error(&msg),
            expected,
            "Taxonomy test failed for '{text}': expected {expected}"
        );
    }
}

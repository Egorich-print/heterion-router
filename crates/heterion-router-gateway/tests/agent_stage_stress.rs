use heterion_router_core::{ChatCompletionRequest, ChatMessage};
use heterion_router_gateway::inspector::{AgentStage, inspect_request};
use serde_json::{Value, json};

#[test]
fn test_long_multiturn_alternating_tools() {
    let mut messages = Vec::new();
    messages.push(ChatMessage::plain("system", "You are an AI assistant."));
    messages.push(ChatMessage::plain("user", "Start long task"));

    for i in 0..50 {
        messages.push(ChatMessage {
            role: "assistant".to_string(),
            content: None,
            reasoning_content: None,
            tool_calls: Some(json!([
                {"id": format!("call_{i}"), "type": "function", "function": {"name": "read_step", "arguments": format!("{{\"step\": {i}}}")}}
            ])),
            tool_call_id: None,
            name: None,
        });
        messages.push(ChatMessage {
            role: "tool".to_string(),
            content: Some(json!(format!(
                "Result for step {i}: success, processed chunk."
            ))),
            reasoning_content: None,
            tool_calls: None,
            tool_call_id: Some(format!("call_{i}")),
            name: Some("read_step".to_string()),
        });
    }

    let req = ChatCompletionRequest {
        messages,
        ..Default::default()
    };

    let inspection = inspect_request(&req);
    assert_eq!(
        inspection.stage,
        AgentStage::MechanicalToolLoop,
        "50-turn successful tool loop should be MechanicalToolLoop"
    );
    assert!(inspection.estimated_tokens > 500);
}

#[test]
fn test_subtle_errors_vs_success_in_tool_outputs() {
    // 1. Tool output with "exit code 0" (SUCCESS)
    let req_exit_0 = ChatCompletionRequest {
        messages: vec![
            ChatMessage::plain("user", "Run the build"),
            ChatMessage {
                role: "assistant".to_string(),
                content: None,
                reasoning_content: None,
                tool_calls: Some(
                    json!([{"id": "call_1", "type": "function", "function": {"name": "sh", "arguments": "{}"}}]),
                ),
                tool_call_id: None,
                name: None,
            },
            ChatMessage {
                role: "tool".to_string(),
                content: Some(json!("Process completed successfully with exit code 0")),
                reasoning_content: None,
                tool_calls: None,
                tool_call_id: Some("call_1".to_string()),
                name: Some("sh".to_string()),
            },
        ],
        ..Default::default()
    };
    let insp_exit_0 = inspect_request(&req_exit_0);
    // Success: "exit code 0" is correctly classified as MechanicalToolLoop
    assert_eq!(insp_exit_0.stage, AgentStage::MechanicalToolLoop);

    // 2. Tool output with "0 failed" (SUCCESS in test runners like cargo test or pytest)
    let req_0_failed = ChatCompletionRequest {
        messages: vec![
            ChatMessage::plain("user", "Run tests"),
            ChatMessage {
                role: "assistant".to_string(),
                content: None,
                reasoning_content: None,
                tool_calls: Some(
                    json!([{"id": "call_1", "type": "function", "function": {"name": "test", "arguments": "{}"}}]),
                ),
                tool_call_id: None,
                name: None,
            },
            ChatMessage {
                role: "tool".to_string(),
                content: Some(json!(
                    "test result: ok. 40 passed; 0 failed; 0 ignored; finished in 0.05s"
                )),
                reasoning_content: None,
                tool_calls: None,
                tool_call_id: Some("call_1".to_string()),
                name: Some("test".to_string()),
            },
        ],
        ..Default::default()
    };
    let insp_0_failed = inspect_request(&req_0_failed);
    // Success: "0 failed" is correctly classified as MechanicalToolLoop
    assert_eq!(insp_0_failed.stage, AgentStage::MechanicalToolLoop);

    // 3. Tool output with JSON `{"error": null}` or `{"status": "ok"}`
    let req_null_error = ChatCompletionRequest {
        messages: vec![
            ChatMessage::plain("user", "Query API"),
            ChatMessage {
                role: "assistant".to_string(),
                content: None,
                reasoning_content: None,
                tool_calls: Some(
                    json!([{"id": "call_1", "type": "function", "function": {"name": "api", "arguments": "{}"}}]),
                ),
                tool_call_id: None,
                name: None,
            },
            ChatMessage {
                role: "tool".to_string(),
                content: Some(json!(
                    "{\"status\": \"success\", \"error\": null, \"data\": [1, 2, 3]}"
                )),
                reasoning_content: None,
                tool_calls: None,
                tool_call_id: Some("call_1".to_string()),
                name: Some("api".to_string()),
            },
        ],
        ..Default::default()
    };
    let insp_null_error = inspect_request(&req_null_error);
    // Success: `error: null` is correctly classified as MechanicalToolLoop
    assert_eq!(insp_null_error.stage, AgentStage::MechanicalToolLoop);

    // 4. Tool output with grep search results looking for error handling code
    let req_grep = ChatCompletionRequest {
        messages: vec![
            ChatMessage::plain("user", "Find error handlers in codebase"),
            ChatMessage {
                role: "assistant".to_string(),
                content: None,
                reasoning_content: None,
                tool_calls: Some(
                    json!([{"id": "call_1", "type": "function", "function": {"name": "grep", "arguments": "{}"}}]),
                ),
                tool_call_id: None,
                name: None,
            },
            ChatMessage {
                role: "tool".to_string(),
                content: Some(json!(
                    "Matched 3 lines:\nsrc/main.rs:12: pub enum AppError {\nsrc/main.rs:15: fn handle_error()"
                )),
                reasoning_content: None,
                tool_calls: None,
                tool_call_id: Some("call_1".to_string()),
                name: Some("grep".to_string()),
            },
        ],
        ..Default::default()
    };
    let insp_grep = inspect_request(&req_grep);
    // Success: grep search results for error handling code are classified as MechanicalToolLoop
    assert_eq!(insp_grep.stage, AgentStage::MechanicalToolLoop);

    // 5. Tool output with genuine error
    let req_genuine_err = ChatCompletionRequest {
        messages: vec![
            ChatMessage::plain("user", "Run the build"),
            ChatMessage {
                role: "assistant".to_string(),
                content: None,
                reasoning_content: None,
                tool_calls: Some(
                    json!([{"id": "call_1", "type": "function", "function": {"name": "sh", "arguments": "{}"}}]),
                ),
                tool_call_id: None,
                name: None,
            },
            ChatMessage {
                role: "tool".to_string(),
                content: Some(json!("error[E0425]: cannot find value `foo` in this scope")),
                reasoning_content: None,
                tool_calls: None,
                tool_call_id: Some("call_1".to_string()),
                name: Some("sh".to_string()),
            },
        ],
        ..Default::default()
    };
    let insp_genuine_err = inspect_request(&req_genuine_err);
    assert_eq!(insp_genuine_err.stage, AgentStage::ErrorRecovery);

    // 6. Tool reads Cargo.toml which specifies `panic = "abort"`
    let req_cargo_toml = ChatCompletionRequest {
        messages: vec![
            ChatMessage::plain("user", "Check Cargo.toml configuration"),
            ChatMessage {
                role: "assistant".to_string(),
                content: None,
                reasoning_content: None,
                tool_calls: Some(
                    json!([{"id": "call_1", "type": "function", "function": {"name": "view_file", "arguments": "{}"}}]),
                ),
                tool_call_id: None,
                name: None,
            },
            ChatMessage {
                role: "tool".to_string(),
                content: Some(json!(
                    "[package]\nname = \"myapp\"\n\n[profile.release]\npanic = \"abort\"\n"
                )),
                reasoning_content: None,
                tool_calls: None,
                tool_call_id: Some("call_1".to_string()),
                name: Some("view_file".to_string()),
            },
        ],
        ..Default::default()
    };
    let insp_cargo_toml = inspect_request(&req_cargo_toml);
    // Success: reading Cargo.toml with `panic = "abort"` is classified as MechanicalToolLoop
    assert_eq!(insp_cargo_toml.stage, AgentStage::MechanicalToolLoop);

    // 7. Tool reads error.rs containing Rust error definition
    let req_read_error_rs = ChatCompletionRequest {
        messages: vec![
            ChatMessage::plain("user", "Inspect error definitions"),
            ChatMessage {
                role: "assistant".to_string(),
                content: None,
                reasoning_content: None,
                tool_calls: Some(
                    json!([{"id": "call_1", "type": "function", "function": {"name": "view_file", "arguments": "{}"}}]),
                ),
                tool_call_id: None,
                name: None,
            },
            ChatMessage {
                role: "tool".to_string(),
                content: Some(json!("pub enum Error { NotFound, InvalidInput }")),
                reasoning_content: None,
                tool_calls: None,
                tool_call_id: Some("call_1".to_string()),
                name: Some("view_file".to_string()),
            },
        ],
        ..Default::default()
    };
    let insp_read_error_rs = inspect_request(&req_read_error_rs);
    // Success: reading source code with enum Error is classified as MechanicalToolLoop
    assert_eq!(insp_read_error_rs.stage, AgentStage::MechanicalToolLoop);

    // 8. Tool output with idiomatic English "with the exception of"
    let req_exception_idiom = ChatCompletionRequest {
        messages: vec![
            ChatMessage::plain("user", "Summarize user status"),
            ChatMessage {
                role: "assistant".to_string(),
                content: None,
                reasoning_content: None,
                tool_calls: Some(
                    json!([{"id": "call_1", "type": "function", "function": {"name": "query_db", "arguments": "{}"}}]),
                ),
                tool_call_id: None,
                name: None,
            },
            ChatMessage {
                role: "tool".to_string(),
                content: Some(json!(
                    "All users are verified with the exception of user 42"
                )),
                reasoning_content: None,
                tool_calls: None,
                tool_call_id: Some("call_1".to_string()),
                name: Some("query_db".to_string()),
            },
        ],
        ..Default::default()
    };
    let insp_exception_idiom = inspect_request(&req_exception_idiom);
    // Success: English idiom "with the exception of" is classified as MechanicalToolLoop
    assert_eq!(insp_exception_idiom.stage, AgentStage::MechanicalToolLoop);
}

#[test]
fn test_user_intent_and_subsequent_turns() {
    // User asking conceptual question about error handling:
    let req_user_question = ChatCompletionRequest {
        messages: vec![ChatMessage::plain(
            "user",
            "How do I design robust error handling in Rust without using unwrap?",
        )],
        ..Default::default()
    };
    let insp_user_q = inspect_request(&req_user_question);
    // Success: conceptual design question mentioning "error handling" is classified as Planning
    assert_eq!(insp_user_q.stage, AgentStage::Planning);

    // User post-tool planning:
    let req_post_tool_planning = ChatCompletionRequest {
        messages: vec![
            ChatMessage::plain("user", "List files in repo"),
            ChatMessage {
                role: "assistant".to_string(),
                content: None,
                reasoning_content: None,
                tool_calls: Some(
                    json!([{"id": "call_1", "type": "function", "function": {"name": "ls", "arguments": "{}"}}]),
                ),
                tool_call_id: None,
                name: None,
            },
            ChatMessage {
                role: "tool".to_string(),
                content: Some(json!("README.md\nsrc/\ntests/")),
                reasoning_content: None,
                tool_calls: None,
                tool_call_id: Some("call_1".to_string()),
                name: Some("ls".to_string()),
            },
            ChatMessage::plain(
                "assistant",
                "The repo contains README.md, src/, and tests/.",
            ),
            ChatMessage::plain(
                "user",
                "Now, plan the architecture for our new authentication service.",
            ),
        ],
        ..Default::default()
    };
    let insp_post_tool = inspect_request(&req_post_tool_planning);
    // Success: user requesting new architecture planning after tool turn transitions to Planning
    assert_eq!(insp_post_tool.stage, AgentStage::Planning);
}

#[test]
fn test_robustness_and_unusual_payloads() {
    // Empty messages
    let req_empty = ChatCompletionRequest {
        messages: vec![],
        ..Default::default()
    };
    let insp_empty = inspect_request(&req_empty);
    assert_eq!(insp_empty.stage, AgentStage::Planning);
    assert_eq!(insp_empty.estimated_tokens, 0);

    // Malformed JSON types inside messages
    let req_malformed = ChatCompletionRequest {
        messages: vec![
            ChatMessage {
                role: "user".to_string(),
                content: Some(Value::Null),
                reasoning_content: None,
                tool_calls: None,
                tool_call_id: None,
                name: None,
            },
            ChatMessage {
                role: "user".to_string(),
                content: Some(Value::Bool(false)),
                reasoning_content: None,
                tool_calls: None,
                tool_call_id: None,
                name: None,
            },
            ChatMessage {
                role: "user".to_string(),
                content: Some(Value::Number(12345.into())),
                reasoning_content: None,
                tool_calls: None,
                tool_call_id: None,
                name: None,
            },
            ChatMessage {
                role: "user".to_string(),
                content: Some(json!([null, false, 42, {"not_text": 123}])),
                reasoning_content: None,
                tool_calls: None,
                tool_call_id: None,
                name: None,
            },
        ],
        tools: Some(Value::String("not an array".to_string())),
        ..Default::default()
    };
    let insp_malformed = inspect_request(&req_malformed);
    println!(
        "insp_malformed: tokens={}, tools={}, stage={:?}",
        insp_malformed.estimated_tokens, insp_malformed.has_tools, insp_malformed.stage
    );
}

#[test]
fn test_fuzz_and_boundary_conditions_no_panic() {
    // 1. Unicode boundary & multi-byte UTF-8 in message content, roles, tool IDs
    let req_unicode = ChatCompletionRequest {
        messages: vec![
            ChatMessage::plain(
                "user",
                "🦀 Rust 🚀 👩‍👩‍👧‍👦 \u{1F600} \u{0000} \u{FFFF} \u{10FFFF}",
            ),
            ChatMessage {
                role: "assistant".to_string(),
                content: Some(json!([
                    {"type": "text", "text": "Multibyte text: 𝒳 𝒴 𝒵 \n \t \r"},
                    {"type": "image_url", "image_url": {"url": "data:image/png;base64,emoji🦀"}}
                ])),
                reasoning_content: Some(
                    "Reasoning with \u{0007}\u{0008}\u{001B} control chars".to_string(),
                ),
                tool_calls: Some(json!([
                    {"id": "call_🦀", "type": "function", "function": {"name": "func_🚀", "arguments": "{\"arg\": \"👩‍👩‍👧‍👦\"}"}}
                ])),
                tool_call_id: None,
                name: Some("tool_🚀".to_string()),
            },
            ChatMessage {
                role: "tool".to_string(),
                content: Some(json!("Tool result with 0 bytes \0 and emojis 🎉")),
                reasoning_content: None,
                tool_calls: None,
                tool_call_id: Some("call_🦀".to_string()),
                name: Some("func_🚀".to_string()),
            },
        ],
        tools: Some(json!([
            {"type": "function", "function": {"name": "func_🚀", "description": "Emoji func"}}
        ])),
        ..Default::default()
    };
    let insp_unicode = inspect_request(&req_unicode);
    assert!(insp_unicode.has_tools);
    assert!(insp_unicode.has_images);
    assert!(insp_unicode.estimated_tokens > 1120);

    // 2. Huge string payloads to check integer overflow in estimated_tokens
    let huge_text = "a".repeat(1_000_000); // 1 MB text
    let req_huge = ChatCompletionRequest {
        messages: vec![ChatMessage::plain("user", &huge_text)],
        ..Default::default()
    };
    let insp_huge = inspect_request(&req_huge);
    assert_eq!(insp_huge.estimated_tokens, (1_000_004 / 4) + 4);

    // 3. Deeply nested JSON in content
    let mut nested = json!("deep");
    for _ in 0..100 {
        nested = json!([nested]);
    }
    let req_nested = ChatCompletionRequest {
        messages: vec![ChatMessage {
            role: "user".to_string(),
            content: Some(nested),
            reasoning_content: None,
            tool_calls: None,
            tool_call_id: None,
            name: None,
        }],
        ..Default::default()
    };
    let insp_nested = inspect_request(&req_nested);
    assert_eq!(insp_nested.stage, AgentStage::Planning);

    // 4. Repeated tool turns with alternating error recovery
    let mut flip_messages = Vec::new();
    flip_messages.push(ChatMessage::plain("user", "Start flipping loop"));
    for i in 0..20 {
        flip_messages.push(ChatMessage {
            role: "assistant".to_string(),
            content: None,
            reasoning_content: None,
            tool_calls: Some(json!([{"id": format!("c_{i}"), "type": "function", "function": {"name": "step", "arguments": "{}"}}])),
            tool_call_id: None,
            name: None,
        });
        let result_content = if i % 2 == 1 {
            format!("error at step {i}: something went wrong")
        } else {
            format!("step {i} succeeded perfectly")
        };
        flip_messages.push(ChatMessage {
            role: "tool".to_string(),
            content: Some(json!(result_content)),
            reasoning_content: None,
            tool_calls: None,
            tool_call_id: Some(format!("c_{i}")),
            name: Some("step".to_string()),
        });
        let req_flip = ChatCompletionRequest {
            messages: flip_messages.clone(),
            ..Default::default()
        };
        let insp_flip = inspect_request(&req_flip);
        if i % 2 == 1 {
            assert_eq!(
                insp_flip.stage,
                AgentStage::ErrorRecovery,
                "Step {i} must be ErrorRecovery"
            );
        } else {
            assert_eq!(
                insp_flip.stage,
                AgentStage::MechanicalToolLoop,
                "Step {i} must be MechanicalToolLoop"
            );
        }
    }
}

#[test]
fn test_empirical_multiturn_alternating_lifecycle() {
    let mut messages: Vec<ChatMessage> = Vec::new();
    messages.push(ChatMessage::plain(
        "system",
        "You are an autonomous engineering agent.",
    ));

    let cycles = [
        (
            "Please design a high-throughput router architecture with robust error handling",
            "exit code 0",
            "error[E0425]: cannot find value `foo` in this scope",
            "I see the compilation failed. Let me examine the error.",
            "How do I fix this compiler error?",
            "Replaced undefined `foo` with `config.foo`. exit code 0",
        ),
        (
            "Can you explain the structure of `struct DbError` and `enum RouterError`?",
            "Matched 2 lines:\nsrc/db.rs:10: pub struct DbError;\nsrc/router.rs:5: pub enum RouterError {",
            "exit code 127: /bin/sh: cargo-audit: command not found",
            "Tool failed with missing binary. Investigating alternate inspection path.",
            "We got this error in the terminal, please resolve this error.",
            "Installed missing tooling, cargo-audit completed with exit code 0",
        ),
        (
            "How should we organize `class NetworkException` and `interface IServiceError`?",
            "[package]\nname = \"service\"\n[profile.release]\npanic = \"abort\"\n",
            "thread 'main' panicked at 'index out of bounds: the len is 3 but the index is 5'",
            "Encountered panic during boundary testing. Reviewing slice indexing.",
            "Why am I getting this panic in main? Please debug this error.",
            "Added length guard before slice index access. test result: ok. 15 passed; 0 failed",
        ),
        (
            "Plan the database migration ensuring zero downtime",
            "{\"status\": \"success\", \"error\": null, \"rows_migrated\": 15000}",
            "test result: FAILED. 18 passed; 2 failed; 0 ignored; finished in 0.12s",
            "Tests failed after migration dry-run. Inspecting schema mismatch.",
            "The tests failed, please help me fix the failing tests.",
            "Updated foreign key cascade constraints. test result: ok. 20 passed; 0 failed",
        ),
        (
            "Could you describe how errors are propagated through the middleware stack?",
            "All services are operational with the exception of legacy v1 gateway",
            "Traceback (most recent call last):\n  File \"app.py\", line 42\nValueError: invalid literal for int()",
            "Traceback detected in auth token decoding. Reviewing format expectations.",
            "Got an error during execution, please fix the error.",
            "Sanitized input before integer parsing. Process exited with exit code 0",
        ),
        (
            "What is the best way to structure our custom error definitions in Rust?",
            "test result: ok. 45 passed; 0 failed; 0 ignored",
            "fatal error: unable to allocate memory in worker thread",
            "Fatal memory exhaustion detected. Reviewing chunk buffer limits.",
            "Help me fix this fatal error immediately.",
            "Added buffer streaming limit to 64MB. exit code 0",
        ),
    ];

    for (idx, (plan_q, tool_succ, tool_err, prefill, user_fix, rem_succ)) in
        cycles.iter().enumerate()
    {
        // Step A: User conceptual inquiry or planning request -> Planning
        messages.push(ChatMessage::plain("user", *plan_q));
        let insp_a = inspect_request(&ChatCompletionRequest {
            messages: messages.clone(),
            ..Default::default()
        });
        assert_eq!(
            insp_a.stage,
            AgentStage::Planning,
            "Cycle {idx} Step A (User Planning '{plan_q}') must be Planning, got {:?}",
            insp_a.stage
        );

        // Step B: Assistant calls tool -> MechanicalToolLoop
        let call_id_1 = format!("call_{idx}_1");
        messages.push(ChatMessage {
            role: "assistant".to_string(),
            content: None,
            reasoning_content: None,
            tool_calls: Some(json!([
                {"id": &call_id_1, "type": "function", "function": {"name": "inspect", "arguments": "{}"}}
            ])),
            tool_call_id: None,
            name: None,
        });
        let insp_b = inspect_request(&ChatCompletionRequest {
            messages: messages.clone(),
            ..Default::default()
        });
        assert_eq!(
            insp_b.stage,
            AgentStage::MechanicalToolLoop,
            "Cycle {idx} Step B (Assistant Tool Calls) must be MechanicalToolLoop, got {:?}",
            insp_b.stage
        );

        // Step C: Benign tool output -> MechanicalToolLoop
        messages.push(ChatMessage {
            role: "tool".to_string(),
            content: Some(json!(*tool_succ)),
            reasoning_content: None,
            tool_calls: None,
            tool_call_id: Some(call_id_1),
            name: Some("inspect".to_string()),
        });
        let insp_c = inspect_request(&ChatCompletionRequest {
            messages: messages.clone(),
            ..Default::default()
        });
        assert_eq!(
            insp_c.stage,
            AgentStage::MechanicalToolLoop,
            "Cycle {idx} Step C (Benign Tool Output '{tool_succ}') must be MechanicalToolLoop, got {:?}",
            insp_c.stage
        );

        // Step D: Assistant calls next tool, which fails -> ErrorRecovery
        let call_id_2 = format!("call_{idx}_2");
        messages.push(ChatMessage {
            role: "assistant".to_string(),
            content: None,
            reasoning_content: None,
            tool_calls: Some(json!([
                {"id": &call_id_2, "type": "function", "function": {"name": "execute", "arguments": "{}"}}
            ])),
            tool_call_id: None,
            name: None,
        });
        messages.push(ChatMessage {
            role: "tool".to_string(),
            content: Some(json!(*tool_err)),
            reasoning_content: None,
            tool_calls: None,
            tool_call_id: Some(call_id_2),
            name: Some("execute".to_string()),
        });
        let insp_d = inspect_request(&ChatCompletionRequest {
            messages: messages.clone(),
            ..Default::default()
        });
        assert_eq!(
            insp_d.stage,
            AgentStage::ErrorRecovery,
            "Cycle {idx} Step D (Tool Failure '{tool_err}') must be ErrorRecovery, got {:?}",
            insp_d.stage
        );

        // Step E: Assistant prefill (no tool calls) -> ErrorRecovery preserved
        messages.push(ChatMessage::plain("assistant", *prefill));
        let insp_e = inspect_request(&ChatCompletionRequest {
            messages: messages.clone(),
            ..Default::default()
        });
        assert_eq!(
            insp_e.stage,
            AgentStage::ErrorRecovery,
            "Cycle {idx} Step E (Assistant Prefill after error) must preserve ErrorRecovery, got {:?}",
            insp_e.stage
        );

        // Step F: User incident remediation instruction -> ErrorRecovery
        messages.push(ChatMessage::plain("user", *user_fix));
        let insp_f = inspect_request(&ChatCompletionRequest {
            messages: messages.clone(),
            ..Default::default()
        });
        assert_eq!(
            insp_f.stage,
            AgentStage::ErrorRecovery,
            "Cycle {idx} Step F (User Remediation '{user_fix}') must be ErrorRecovery, got {:?}",
            insp_f.stage
        );

        // Step G: Assistant calls remediation tool and tool succeeds -> MechanicalToolLoop
        let call_id_3 = format!("call_{idx}_3");
        messages.push(ChatMessage {
            role: "assistant".to_string(),
            content: None,
            reasoning_content: None,
            tool_calls: Some(json!([
                {"id": &call_id_3, "type": "function", "function": {"name": "remediate", "arguments": "{}"}}
            ])),
            tool_call_id: None,
            name: None,
        });
        messages.push(ChatMessage {
            role: "tool".to_string(),
            content: Some(json!(*rem_succ)),
            reasoning_content: None,
            tool_calls: None,
            tool_call_id: Some(call_id_3),
            name: Some("remediate".to_string()),
        });
        let insp_g = inspect_request(&ChatCompletionRequest {
            messages: messages.clone(),
            ..Default::default()
        });
        assert_eq!(
            insp_g.stage,
            AgentStage::MechanicalToolLoop,
            "Cycle {idx} Step G (Remediation Success '{rem_succ}') must transition to MechanicalToolLoop, got {:?}",
            insp_g.stage
        );

        // Step H: Assistant confirms remediation
        messages.push(ChatMessage::plain(
            "assistant",
            "The remediation step succeeded and verified clean.",
        ));
        let insp_h = inspect_request(&ChatCompletionRequest {
            messages: messages.clone(),
            ..Default::default()
        });
        assert_eq!(
            insp_h.stage,
            AgentStage::MechanicalToolLoop,
            "Cycle {idx} Step H (Assistant prefill after successful tool) must be MechanicalToolLoop, got {:?}",
            insp_h.stage
        );
    }
}

#[test]
fn test_long_history_planning_resistance_to_sticky_error() {
    // Build a noisy, long 60-turn dialogue filled with past tool outputs, panics, errors, and fixes
    let mut history: Vec<ChatMessage> = Vec::new();
    history.push(ChatMessage::plain(
        "system",
        "Autonomous system coordinator",
    ));

    for i in 0..15 {
        history.push(ChatMessage::plain(
            "user",
            format!("Step {i}: execute inspection"),
        ));
        let call_id = format!("h_call_{i}");
        history.push(ChatMessage {
            role: "assistant".to_string(),
            content: None,
            reasoning_content: None,
            tool_calls: Some(json!([
                {"id": &call_id, "type": "function", "function": {"name": "run", "arguments": "{}"}}
            ])),
            tool_call_id: None,
            name: None,
        });
        let tool_content = if i % 2 == 0 {
            format!("error[E0425]: failed operation {i}")
        } else {
            format!("recovered step {i}: exit code 0")
        };
        history.push(ChatMessage {
            role: "tool".to_string(),
            content: Some(json!(tool_content)),
            reasoning_content: None,
            tool_calls: None,
            tool_call_id: Some(call_id),
            name: Some("run".to_string()),
        });
        history.push(ChatMessage::plain(
            "assistant",
            format!("Summary of step {i}"),
        ));
    }

    // Now test a battery of 20 diverse conceptual, planning, and architectural prompts
    let planning_prompts = [
        "Plan the next architectural refactoring phase",
        "Design a high-throughput caching tier for Heterion Router",
        "Can you explain the structure of `struct DbError`?",
        "Can you explain the structure of struct DbError?",
        "Explain how RouterError works in the new gateway",
        "Explain the error definition of `enum RouterError`",
        "What is an Error in the context of Rust standard library?",
        "What is a DbError?",
        "How do I design robust error handling in Rust?",
        "Could you explain how errors are propagated through Axum?",
        "Please explain how custom error types are defined in Rust",
        "Design an Error enum for the HTTP client",
        "Plan the refactoring of our error handling architecture",
        "Design an error handling pipeline where all steps exit with code 0",
        "How should we structure `class NetworkException` in Python?",
        "Describe the interface contract for `interface IServiceError`",
        "Outline the milestone roadmap for switchyard dynamic routing",
        "Show me the blueprint for zero-quota algorithmic context pruning",
        "Tell me how to architect the model fallback chain",
        "Where should we place the new telemetry metrics recorder?",
    ];

    for prompt in planning_prompts {
        let mut test_messages = history.clone();
        test_messages.push(ChatMessage::plain("user", prompt));

        let req = ChatCompletionRequest {
            messages: test_messages,
            ..Default::default()
        };

        let insp = inspect_request(&req);
        assert_eq!(
            insp.stage,
            AgentStage::Planning,
            "Prompt '{prompt}' appended to long noisy history MUST be Planning, got {:?}",
            insp.stage
        );
    }
}

#[test]
fn test_parallel_tools_error_and_recovery_transitions() {
    let mut messages: Vec<ChatMessage> = vec![
        ChatMessage::plain("system", "Test assistant"),
        ChatMessage::plain("user", "Run parallel status checks"),
        ChatMessage {
            role: "assistant".to_string(),
            content: None,
            reasoning_content: None,
            tool_calls: Some(json!([
                {"id": "p1", "type": "function", "function": {"name": "db_check", "arguments": "{}"}},
                {"id": "p2", "type": "function", "function": {"name": "cache_check", "arguments": "{}"}},
                {"id": "p3", "type": "function", "function": {"name": "grep_errors", "arguments": "{}"}}
            ])),
            tool_call_id: None,
            name: None,
        },
        ChatMessage {
            role: "tool".to_string(),
            content: Some(json!("exit code 0")),
            reasoning_content: None,
            tool_calls: None,
            tool_call_id: Some("p1".to_string()),
            name: Some("db_check".to_string()),
        },
        ChatMessage {
            role: "tool".to_string(),
            content: Some(json!("{\"status\": \"ok\", \"error\": null}")),
            reasoning_content: None,
            tool_calls: None,
            tool_call_id: Some("p2".to_string()),
            name: Some("cache_check".to_string()),
        },
        ChatMessage {
            role: "tool".to_string(),
            content: Some(json!("Matched 1 line:\nsrc/err.rs:1: pub enum Error")),
            reasoning_content: None,
            tool_calls: None,
            tool_call_id: Some("p3".to_string()),
            name: Some("grep_errors".to_string()),
        },
    ];

    // All 3 parallel tools benign -> MechanicalToolLoop
    let insp1 = inspect_request(&ChatCompletionRequest {
        messages: messages.clone(),
        ..Default::default()
    });
    assert_eq!(insp1.stage, AgentStage::MechanicalToolLoop);

    // Assistant emits next batch of 3 parallel tools, where 1 fails
    messages.push(ChatMessage {
        role: "assistant".to_string(),
        content: None,
        reasoning_content: None,
        tool_calls: Some(json!([
            {"id": "p4", "type": "function", "function": {"name": "worker_1", "arguments": "{}"}},
            {"id": "p5", "type": "function", "function": {"name": "worker_2", "arguments": "{}"}},
            {"id": "p6", "type": "function", "function": {"name": "worker_3", "arguments": "{}"}}
        ])),
        tool_call_id: None,
        name: None,
    });
    messages.push(ChatMessage {
        role: "tool".to_string(),
        content: Some(json!("exit code 0")),
        reasoning_content: None,
        tool_calls: None,
        tool_call_id: Some("p4".to_string()),
        name: Some("worker_1".to_string()),
    });
    messages.push(ChatMessage {
        role: "tool".to_string(),
        content: Some(json!("fatal error: connection refused to 127.0.0.1:5432")),
        reasoning_content: None,
        tool_calls: None,
        tool_call_id: Some("p5".to_string()),
        name: Some("worker_2".to_string()),
    });
    messages.push(ChatMessage {
        role: "tool".to_string(),
        content: Some(json!("test result: ok. 10 passed; 0 failed")),
        reasoning_content: None,
        tool_calls: None,
        tool_call_id: Some("p6".to_string()),
        name: Some("worker_3".to_string()),
    });

    // One tool failed in batch -> ErrorRecovery
    let insp2 = inspect_request(&ChatCompletionRequest {
        messages: messages.clone(),
        ..Default::default()
    });
    assert_eq!(insp2.stage, AgentStage::ErrorRecovery);

    // Assistant prefill -> ErrorRecovery preserved
    messages.push(ChatMessage::plain(
        "assistant",
        "Database connection refused on worker 2. Assessing options.",
    ));
    let insp3 = inspect_request(&ChatCompletionRequest {
        messages: messages.clone(),
        ..Default::default()
    });
    assert_eq!(insp3.stage, AgentStage::ErrorRecovery);

    // User pivots to planning -> transitions cleanly to Planning
    messages.push(ChatMessage::plain(
        "user",
        "Cancel the database synchronization and plan our fallback strategy.",
    ));
    let insp4 = inspect_request(&ChatCompletionRequest {
        messages: messages.clone(),
        ..Default::default()
    });
    assert_eq!(insp4.stage, AgentStage::Planning);
}

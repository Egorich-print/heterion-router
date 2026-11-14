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

#[test]
fn test_varied_punctuation_around_error_definitions() {
    // Delimiters and punctuation attached immediately to error type identifiers
    let punctuations = [
        ("?", "Can you explain struct DbError?"),
        (".", "Please describe struct DbError."),
        ("!", "Look at struct DbError!"),
        (",", "We need struct DbError, which holds response data."),
        (";", "First inspect struct DbError; then check caller."),
        (":", "Consider struct DbError: what fields are required?"),
        ("...", "Reviewing struct DbError... looks incomplete."),
        (")", "Check the definition of (struct DbError) in lib."),
        ("]", "Look at [struct DbError] in documentation."),
        ("}", "Examine {struct DbError} for memory layout."),
        (">", "Is it wrapped in Option<struct DbError>?"),
        ("'", "Check 'struct DbError' in the module."),
        ("\"", "Search for \"struct DbError\" in repository."),
    ];

    for (punc_desc, prompt) in punctuations {
        let req = ChatCompletionRequest {
            messages: vec![ChatMessage::plain("user", prompt)],
            ..Default::default()
        };
        let insp = inspect_request(&req);
        assert_eq!(
            insp.stage,
            AgentStage::Planning,
            "Prompt with punctuation '{punc_desc}' should classify as Planning: '{prompt}', got {:?}",
            insp.stage
        );
    }
}

#[test]
fn test_markdown_backticks_and_code_blocks() {
    let markdown_cases = [
        // Single backticks
        "Can you explain `struct DbError`?",
        "What does `enum AuthError` represent?",
        "How is `type ResultError = CustomError;` used?",
        "Please describe `interface ApiError` in TypeScript.",
        "Explain `class FileSystemException:` in Python.",
        // Double backticks
        "Can you explain ``struct DbError``?",
        "What is ``enum ValidationError``?",
        // Fenced code blocks
        "Can you review this error type definition?\n```rust\npub struct ServiceError {\n    pub code: u32,\n}\n```\nWhat do you think?",
        "How should I structure this interface?\n```typescript\nexport interface NetworkError {\n    status: number;\n}\n```",
        "Please explain the following class:\n```python\nclass DataException:\n    pass\n```",
        // Multi-backtick inside question
        "Compare `struct ReadError` with `struct WriteError`.",
        "What are the variants of `pub(crate) enum GatewayError`?",
        // Markdown italics, bold, strikethrough
        "Can you explain *struct DbError*?",
        "Can you explain _struct DbError_?",
        "Can you explain **struct DbError**?",
        "Can you explain __struct DbError__?",
        "Can you explain ~~struct DbError~~?",
        "What is ***enum CriticalError***?",
    ];

    for prompt in markdown_cases {
        let req = ChatCompletionRequest {
            messages: vec![ChatMessage::plain("user", prompt)],
            ..Default::default()
        };
        let insp = inspect_request(&req);
        assert_eq!(
            insp.stage,
            AgentStage::Planning,
            "Markdown case should classify as Planning: '{prompt}', got {:?}",
            insp.stage
        );
    }
}

#[test]
fn test_sentence_structures_and_inquiry_stems() {
    let stems = [
        "Can you explain why struct DbError implements Send?",
        "Could you clarify the purpose of enum RouterError?",
        "Would you explain how interface HandlerError is used?",
        "Please explain the design of struct StorageError.",
        "Explain the hierarchy of class NetworkException.",
        "Please describe the fields in struct ConfigError.",
        "Describe how enum StreamError handles timeouts.",
        "Tell me about struct ClientError in this module.",
        "Show me how struct ServerError is constructed.",
        "What is struct MetricError used for?",
        "Why is enum CacheError necessary here?",
        "Where should struct StateError be declared?",
        "When is class SessionException thrown?",
        "Who handles struct DispatchError in the gateway?",
        "Which is preferred: struct FlatError or enum NestedError?",
        "How do error-handling boundaries work in web frameworks?",
        "Design an error type hierarchy for the RPC client.",
        "Plan the migration from old error definitions to thiserror.",
        "The difference between struct DbError and struct IoError?",
    ];

    for prompt in stems {
        let req = ChatCompletionRequest {
            messages: vec![ChatMessage::plain("user", prompt)],
            ..Default::default()
        };
        let insp = inspect_request(&req);
        assert_eq!(
            insp.stage,
            AgentStage::Planning,
            "Inquiry stem should classify as Planning: '{prompt}', got {:?}",
            insp.stage
        );
    }
}

#[test]
fn test_multilingual_type_declarations() {
    let declarations = [
        // Rust
        "struct DbError { code: i32 }",
        "pub struct ApiError;",
        "pub(crate) enum NetError { Timeout }",
        "pub(super) type ServiceError = Result<(), String>;",
        // TypeScript / JavaScript
        "interface DatabaseError { message: string; }",
        "export interface GraphError { node: string; }",
        "type QueryError = string | number;",
        // Python
        "class NetworkException: pass",
        "class AuthException(Exception): pass",
        // C++ / Java / C#
        "class RuntimeException { public: int code; };",
    ];

    for decl in declarations {
        // As a user inquiry asking to explain it
        let prompt = format!("Explain this code: {decl}");
        let req = ChatCompletionRequest {
            messages: vec![ChatMessage::plain("user", prompt.clone())],
            ..Default::default()
        };
        let insp = inspect_request(&req);
        assert_eq!(
            insp.stage,
            AgentStage::Planning,
            "Explaining declaration should classify as Planning: '{prompt}', got {:?}",
            insp.stage
        );

        // As a benign tool search output
        let tool_msg = make_tool_msg(
            "search_code",
            json!(format!("1 match found:\nsrc/lib.rs:5: {decl}")),
        );
        assert!(
            !is_tool_error(&tool_msg),
            "Search output with declaration should not be tool error: '{decl}'"
        );
    }
}

#[test]
fn test_true_positives_real_incidents_must_be_error_recovery() {
    let incident_cases = [
        // Compiler error codes and messages
        "How do I fix error[E0425]: cannot find value `foo` in this scope?",
        "Why did I get error[E0308]: mismatched types expected `usize`, found `u32`?",
        "Got this error: error[E0599]: no method named `bar` found for type `Baz`",
        // Panics and assertions
        "How can I resolve this panic: thread 'main' panicked at 'index out of bounds'?",
        "The background worker panicked at 'called Option::unwrap() on a None value'",
        "Assertion failed: (ptr != NULL), function alloc, file mem.c, line 42.",
        // Crashes and non-zero exits
        "The command failed with exit code 127, how do I resolve it?",
        "Build failed with exit code 1 in step compile",
        "Worker process crashed with segmentation fault",
        "Process crashed due to out of memory",
        // Test failures
        "How do I fix test result: FAILED. 2 failed?",
        "3 tests failed during cargo test execution",
        // Uncaught exceptions and tracebacks
        "Traceback (most recent call last):\n  File \"run.py\", line 42\nValueError: invalid literal for int()",
        "Execution failed due to uncaught exception in thread pool",
        // Incident remediation intents with general error markers
        "How do I fix this error in production?",
        "How to fix the error in the authentication flow?",
        "Help me fix this error: connection refused on port 5432",
        "Why am I getting this error when sending requests?",
        "Why do I get an error after updating dependencies?",
        "Got this error while establishing TLS handshake",
        "Troubleshoot this error in the redis cluster",
        "Debug this error in the routing engine",
        "Resolve this error: database connection pool exhausted",
        // Factual incident statements
        "There is an error in the indexing service preventing user search",
        "Encountered error during execution of database schema migration",
        "The database query failed with timeout error after 30 seconds",
        "Execution failed: unable to bind to port 20128",
    ];

    for inc in incident_cases {
        let req = ChatCompletionRequest {
            messages: vec![ChatMessage::plain("user", inc)],
            ..Default::default()
        };
        let insp = inspect_request(&req);
        assert_eq!(
            insp.stage,
            AgentStage::ErrorRecovery,
            "Real incident must classify as ErrorRecovery: '{inc}', got {:?}",
            insp.stage
        );
    }
}

#[test]
fn test_multiturn_agent_flow_transitions() {
    // 1. Initial user inquiry -> Planning
    let req_step1 = ChatCompletionRequest {
        messages: vec![ChatMessage::plain(
            "user",
            "Can you explain the structure of `struct DbError`?",
        )],
        ..Default::default()
    };
    assert_eq!(inspect_request(&req_step1).stage, AgentStage::Planning);

    // 2. Assistant initiates tool call -> MechanicalToolLoop
    let req_step2 = ChatCompletionRequest {
        messages: vec![
            ChatMessage::plain("user", "Can you explain the structure of `struct DbError`?"),
            ChatMessage {
                role: "assistant".to_string(),
                content: None,
                reasoning_content: None,
                tool_calls: Some(json!([
                    {"id": "c1", "type": "function", "function": {"name": "grep", "arguments": "{\"query\": \"struct DbError\"}"}}
                ])),
                tool_call_id: None,
                name: None,
            },
            make_tool_msg(
                "grep",
                json!("Matched 1 line:\nsrc/db.rs:10: pub struct DbError { pub code: u32 }"),
            ),
        ],
        ..Default::default()
    };
    assert_eq!(
        inspect_request(&req_step2).stage,
        AgentStage::MechanicalToolLoop
    );

    // 3. Tool fails with exit code 1 -> ErrorRecovery
    let req_step3 = ChatCompletionRequest {
        messages: vec![
            ChatMessage::plain("user", "Can you explain the structure of `struct DbError`?"),
            ChatMessage {
                role: "assistant".to_string(),
                content: None,
                reasoning_content: None,
                tool_calls: Some(json!([
                    {"id": "c2", "type": "function", "function": {"name": "sh", "arguments": "{\"cmd\": \"cargo check\"}"}}
                ])),
                tool_call_id: None,
                name: None,
            },
            make_tool_msg(
                "sh",
                json!("error[E0425]: cannot find value `DbError` in this scope\nexit code 1"),
            ),
        ],
        ..Default::default()
    };
    assert_eq!(inspect_request(&req_step3).stage, AgentStage::ErrorRecovery);

    // 4. Follow-up user question asking to explain how to fix the error -> ErrorRecovery
    let req_step4 = ChatCompletionRequest {
        messages: vec![
            ChatMessage::plain("user", "Can you explain the structure of `struct DbError`?"),
            ChatMessage::plain("assistant", "Here is the structure..."),
            ChatMessage::plain(
                "user",
                "How do I fix this error: cannot find value `DbError` in this scope?",
            ),
        ],
        ..Default::default()
    };
    assert_eq!(inspect_request(&req_step4).stage, AgentStage::ErrorRecovery);

    // 5. Follow-up user question asking conceptual inquiry -> Planning
    let req_step5 = ChatCompletionRequest {
        messages: vec![
            ChatMessage::plain(
                "user",
                "How do I fix this error: cannot find value `DbError` in this scope?",
            ),
            ChatMessage::plain("assistant", "You need to add `use crate::db::DbError;`."),
            ChatMessage::plain(
                "user",
                "Could you explain the purpose of `enum RouterError` now?",
            ),
        ],
        ..Default::default()
    };
    assert_eq!(inspect_request(&req_step5).stage, AgentStage::Planning);
}

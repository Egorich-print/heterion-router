//! A request that carries a `developer` turn.
//!
//! Agent runtimes (MCode among them) send their system prompt with
//! `role: "developer"` — the role OpenAI introduced for the o-series models.
//! The Mistral API does not know it and answers `422` with `union_tag_invalid`:
//! "Input tag 'developer' found using 'role' does not match any of the expected
//! tags: <Roles.system>, <Roles.user>, <Roles.assistant>, <Roles.tool>".
//!
//! The router must rename the role for exactly that provider — and leave the
//! conversation verbatim everywhere else, where 253 OpenAI-format providers
//! accept the role as it is.

use futures::StreamExt;
use heterion_router_core::{ChatCompletionRequest, ChatMessage};
use heterion_router_gateway::backend::ChatBackend;
use heterion_router_gateway::openai::ProviderOpenAiBackend;
use serde_json::{Value, json};
use wiremock::{
    Mock, MockServer, Request, ResponseTemplate,
    matchers::{method, path},
};

/// A conversation whose system prompt carries the `developer` role.
fn developer_request(model: &str) -> ChatCompletionRequest {
    ChatCompletionRequest {
        model: model.to_string(),
        messages: vec![
            ChatMessage::plain("developer", "You are terse."),
            ChatMessage::plain("user", "Say exactly: HELLO"),
        ],
        stream: false,
        ..Default::default()
    }
}

/// The role of the first message of a request body.
fn first_role(body: &Value) -> String {
    body["messages"]
        .as_array()
        .and_then(|messages| messages.first())
        .and_then(|message| message["role"].as_str())
        .unwrap_or_default()
        .to_string()
}

/// Every role of a request body, in order.
fn roles(body: &Value) -> Vec<String> {
    body["messages"]
        .as_array()
        .map(|messages| {
            messages
                .iter()
                .filter_map(|message| message["role"].as_str())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// An upstream that behaves like Mistral: a `developer` role is a `422`,
/// anything else is answered.
fn mistral_like() -> Mock {
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(|request: &Request| {
            let body: Value = serde_json::from_slice(&request.body).expect("json body");
            let developer = body["messages"]
                .as_array()
                .map(|messages| {
                    messages
                        .iter()
                        .any(|message| message["role"].as_str() == Some("developer"))
                })
                .unwrap_or(false);
            if developer {
                return ResponseTemplate::new(422).set_body_json(json!({
                    "detail": [{
                        "type": "union_tag_invalid",
                        "loc": ["body", "messages", 0],
                        "msg": "Input tag 'developer' found using 'role' does not match any of the expected tags: <Roles.system: 'system'>, <Roles.user: 'user'>, <Roles.assistant: 'assistant'>, <Roles.tool: 'tool'>",
                    }]
                }));
            }
            if body["stream"].as_bool() == Some(true) {
                let frame = |content: &str, finish: Option<&str>| {
                    format!(
                        "data: {}\n\n",
                        json!({
                            "id": "chatcmpl-1",
                            "object": "chat.completion.chunk",
                            "created": 1,
                            "model": "codestral-latest",
                            "choices": [{
                                "index": 0,
                                "delta": {"role": "assistant", "content": content},
                                "finish_reason": finish,
                            }],
                        })
                    )
                };
                return ResponseTemplate::new(200)
                    .insert_header("content-type", "text/event-stream")
                    .set_body_string(format!(
                        "{}{}data: [DONE]\n\n",
                        frame("HELLO", None),
                        frame("", Some("stop"))
                    ));
            }
            ResponseTemplate::new(200).set_body_json(json!({
                "id": "chatcmpl-1",
                "object": "chat.completion",
                "created": 1,
                "model": "codestral-latest",
                "choices": [{
                    "index": 0,
                    "message": {"role": "assistant", "content": "HELLO"},
                    "finish_reason": "stop"
                }],
                "usage": {"prompt_tokens": 10, "completion_tokens": 2, "total_tokens": 12}
            }))
        })
}

#[tokio::test]
async fn a_mistral_like_provider_gets_a_system_turn() {
    let server = MockServer::start().await;
    mistral_like().expect(1).mount(&server).await;

    let backend = ProviderOpenAiBackend::new(
        "mistral".to_string(),
        format!("{}/chat/completions", server.uri()),
        vec!["key".to_string()],
    )
    .unwrap();
    let response = backend
        .complete(developer_request("codestral-latest"))
        .await
        .expect("the request must not be refused");

    let requests = server.received_requests().await.expect("requests");
    let body: Value = serde_json::from_slice(&requests[0].body).expect("json body");
    assert_eq!(
        first_role(&body),
        "system",
        "the upstream still saw a developer turn: {body}"
    );
    assert_eq!(
        roles(&body),
        vec!["system".to_string(), "user".to_string()],
        "only the role changes — the conversation stays the client's: {body}"
    );
    assert_eq!(response.choices[0].message.text(), "HELLO");
}

#[tokio::test]
async fn a_provider_that_serves_the_developer_role_keeps_the_verbatim_turn() {
    let server = MockServer::start().await;
    mistral_like().expect(1).mount(&server).await;

    let backend = ProviderOpenAiBackend::new(
        "openrouter".to_string(),
        format!("{}/chat/completions", server.uri()),
        vec!["key".to_string()],
    )
    .unwrap();
    let result = backend.complete(developer_request("some-model")).await;

    assert!(
        result.is_err(),
        "a provider that serves the developer role must not be adapted"
    );
    let requests = server.received_requests().await.expect("requests");
    let body: Value = serde_json::from_slice(&requests[0].body).expect("json body");
    assert_eq!(
        first_role(&body),
        "developer",
        "the role was rewritten for a provider that serves it: {body}"
    );
}

#[tokio::test]
async fn the_streaming_path_renames_the_role_too() {
    let server = MockServer::start().await;
    mistral_like().expect(1).mount(&server).await;

    let backend = ProviderOpenAiBackend::new(
        "mistral".to_string(),
        format!("{}/chat/completions", server.uri()),
        vec!["key".to_string()],
    )
    .unwrap();
    let mut request = developer_request("codestral-latest");
    request.stream = true;

    let mut stream = backend.stream(request);
    let mut chunks = Vec::new();
    while let Some(item) = stream.next().await {
        chunks.push(item.expect("chunk ok"));
    }
    assert!(
        !chunks.is_empty(),
        "the stream must deliver a chunk to the client"
    );

    let requests = server.received_requests().await.expect("requests");
    let body: Value = serde_json::from_slice(&requests[0].body).expect("json body");
    assert_eq!(
        first_role(&body),
        "system",
        "the streaming path still sent a developer turn: {body}"
    );
}

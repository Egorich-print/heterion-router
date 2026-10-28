//! A request whose last message is an assistant turn.
//!
//! Some providers refuse that shape outright: Mistral answers `400` with
//! `invalid_request_message_order` (code 3230, "Expected last role User or
//! Tool"), Gemini answers `400` with "Requests ending with a model turn are
//! not supported". The router must adapt such a request for exactly those
//! providers — and leave it alone everywhere else, where a verbatim prefill
//! is a feature (253 OpenAI-format providers serve it today).

use futures::StreamExt;
use heterion_router_core::{ChatCompletionRequest, ChatMessage};
use heterion_router_gateway::backend::ChatBackend;
use heterion_router_gateway::gemini::GeminiBackend;
use heterion_router_gateway::openai::ProviderOpenAiBackend;
use serde_json::{Value, json};
use wiremock::{
    Mock, MockServer, Request, ResponseTemplate,
    matchers::{method, path},
};

/// The unfinished assistant turn a client sends as a prefill.
const PREFIX: &str = "The answer starts like this";

/// A conversation that ends with an assistant turn.
fn trailing_assistant_request(model: &str) -> ChatCompletionRequest {
    ChatCompletionRequest {
        model: model.to_string(),
        messages: vec![
            ChatMessage::plain("user", "Say exactly: HELLO"),
            ChatMessage::plain("assistant", PREFIX),
        ],
        stream: false,
        ..Default::default()
    }
}

/// The role of the last message of a request body.
fn last_role(body: &Value) -> String {
    body["messages"]
        .as_array()
        .and_then(|messages| messages.last())
        .and_then(|message| message["role"].as_str())
        .unwrap_or_default()
        .to_string()
}

/// The text of the last message of a request body.
fn last_text(body: &Value) -> String {
    body["messages"]
        .as_array()
        .and_then(|messages| messages.last())
        .and_then(|message| message["content"].as_str())
        .unwrap_or_default()
        .to_string()
}

/// An upstream that behaves like Mistral: a trailing assistant turn is a
/// `400`, anything else is answered.
fn mistral_like() -> Mock {
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(|request: &Request| {
            let body: Value = serde_json::from_slice(&request.body).expect("json body");
            if last_role(&body) == "assistant" {
                ResponseTemplate::new(400).set_body_json(json!({
                    "object": "error",
                    "message": "Expected last role User or Tool (or Assistant with prefix True) for serving but got assistant",
                    "type": "invalid_request_message_order",
                    "code": "3230",
                }))
            } else {
                ResponseTemplate::new(200).set_body_json(json!({
                    "id": "chatcmpl-1",
                    "object": "chat.completion",
                    "created": 1,
                    "model": "codestral-latest",
                    "choices": [{
                        "index": 0,
                        "message": {"role": "assistant", "content": " and continues here."},
                        "finish_reason": "stop"
                    }],
                    "usage": {"prompt_tokens": 10, "completion_tokens": 4, "total_tokens": 14}
                }))
            }
        })
}

#[tokio::test]
async fn a_mistral_like_provider_gets_a_trailing_user_turn() {
    let server = MockServer::start().await;
    mistral_like().expect(1).mount(&server).await;

    let backend = ProviderOpenAiBackend::new(
        "mistral".to_string(),
        format!("{}/chat/completions", server.uri()),
        vec!["key".to_string()],
    )
    .unwrap();
    let response = backend
        .complete(trailing_assistant_request("codestral-latest"))
        .await
        .expect("the request must not be refused");

    let requests = server.received_requests().await.expect("requests");
    let body: Value = serde_json::from_slice(&requests[0].body).expect("json body");
    assert_eq!(
        last_role(&body),
        "user",
        "the upstream still saw a trailing assistant turn: {body}"
    );
    assert!(
        last_text(&body).to_lowercase().contains("continu"),
        "the appended turn does not ask for a continuation: {body}"
    );
    assert_eq!(
        response.choices[0].message.text(),
        " and continues here.",
        "the answer must be the model's own text: a native prefill returns \
         the new text alone, and the prefix stays the client's"
    );
}

#[tokio::test]
async fn a_provider_that_serves_prefills_keeps_the_verbatim_turn() {
    let server = MockServer::start().await;
    mistral_like().expect(1).mount(&server).await;

    let backend = ProviderOpenAiBackend::new(
        "openrouter".to_string(),
        format!("{}/chat/completions", server.uri()),
        vec!["key".to_string()],
    )
    .unwrap();
    let result = backend
        .complete(trailing_assistant_request("some-model"))
        .await;

    assert!(
        result.is_err(),
        "a provider that serves prefills must not be adapted"
    );
    let requests = server.received_requests().await.expect("requests");
    let body: Value = serde_json::from_slice(&requests[0].body).expect("json body");
    assert_eq!(
        last_role(&body),
        "assistant",
        "the prefill was rewritten for a provider that serves it: {body}"
    );
}

#[tokio::test]
async fn a_gemini_like_provider_gets_a_trailing_user_turn() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/models/gemini-3.8-flash:generateContent"))
        .respond_with(|request: &Request| {
            let body: Value = serde_json::from_slice(&request.body).expect("json body");
            let last = body["contents"]
                .as_array()
                .and_then(|contents| contents.last())
                .and_then(|turn| turn["role"].as_str())
                .unwrap_or_default()
                .to_string();
            if last == "model" {
                ResponseTemplate::new(400).set_body_json(json!({
                    "error": {
                        "code": 400,
                        "message": "Requests ending with a model turn are not supported.",
                        "status": "INVALID_ARGUMENT",
                    }
                }))
            } else {
                ResponseTemplate::new(200).set_body_json(json!({
                    "candidates": [{
                        "content": {"parts": [{"text": " and continues here."}]},
                        "finishReason": "STOP"
                    }],
                    "usageMetadata": {
                        "promptTokenCount": 10,
                        "candidatesTokenCount": 4,
                        "totalTokenCount": 14
                    }
                }))
            }
        })
        .expect(1)
        .mount(&server)
        .await;

    let backend = GeminiBackend::new(format!("{}/models", server.uri()), vec!["key".to_string()])
        .expect("backend");
    let response = backend
        .complete(trailing_assistant_request("gemini-3.8-flash"))
        .await
        .expect("the request must not be refused");

    let requests = server.received_requests().await.expect("requests");
    let body: Value = serde_json::from_slice(&requests[0].body).expect("json body");
    let last_role = body["contents"]
        .as_array()
        .and_then(|contents| contents.last())
        .and_then(|turn| turn["role"].as_str())
        .unwrap_or_default();
    assert_eq!(
        last_role, "user",
        "the upstream still saw a trailing model turn: {body}"
    );
    assert_eq!(
        response.choices[0].message.text(),
        " and continues here.",
        "the answer must be the model's own text"
    );
}

#[tokio::test]
async fn a_streamed_answer_carries_the_model_text() {
    let server = MockServer::start().await;
    let mut frames = String::new();
    for (index, text) in [" and ", "continues here."].iter().enumerate() {
        frames.push_str("data: ");
        frames.push_str(
            &json!({
                "id": "chatcmpl-1",
                "object": "chat.completion.chunk",
                "created": 1,
                "model": "codestral-latest",
                "choices": [{
                    "index": 0,
                    "delta": {"content": text},
                    "finish_reason": Value::Null
                }]
            })
            .to_string(),
        );
        frames.push_str("\n\n");
        let _ = index;
    }
    frames.push_str("data: [DONE]\n\n");

    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_raw(frames, "text/event-stream"),
        )
        .expect(1)
        .mount(&server)
        .await;

    let backend = ProviderOpenAiBackend::new(
        "mistral".to_string(),
        format!("{}/chat/completions", server.uri()),
        vec!["key".to_string()],
    )
    .unwrap();
    let mut request = trailing_assistant_request("codestral-latest");
    request.stream = true;
    let mut stream = backend.stream(request);
    let mut text = String::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.expect("chunk");
        if let Some(content) = chunk.content {
            text.push_str(&content);
        }
    }

    assert_eq!(
        text, " and continues here.",
        "the stream must carry the model's own text"
    );
}

//! End-to-end check of the repetition guard, over the wire.
//!
//! The unit tests in the gateway crate prove the guard cuts a looping answer.
//! This test proves the same over the real HTTP surface: a stub upstream
//! answers with a loop — the way Codestral did on 2026-10-26, when a 110 KB
//! answer printed one lint name 4090 times — and the client must receive the
//! text from before the loop, with the answer marked as truncated.

use axum::body::{Body, to_bytes};
use axum::http::Request;
use heterion_router_gateway::backend::ChatBackend;
use heterion_router_gateway::build_router;
use heterion_router_gateway::openai::OpenAiBackend;
use serde_json::{Value, json};
use std::sync::Arc;
use tower::ServiceExt;
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{method, path},
};

/// The answer a degenerate model produces: one good sentence, then the same
/// lint name over and over.
fn looping_answer() -> String {
    format!(
        "warning: unused variable: `cargo_cmd` at host-test.sh:32:5. {}",
        "(clippy::unused_variables) ".repeat(2000)
    )
}

fn router_for(server: &MockServer) -> axum::Router {
    let backend = OpenAiBackend::new(server.uri(), None).expect("backend");
    build_router(Arc::new(backend) as Arc<dyn ChatBackend>)
}

async fn post(router: axum::Router, body: Value) -> String {
    let response = router
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    String::from_utf8_lossy(&bytes).to_string()
}

#[tokio::test]
async fn a_looping_answer_never_reaches_the_client_whole() {
    let server = MockServer::start().await;
    let answer = looping_answer();
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "chatcmpl-loop",
            "object": "chat.completion",
            "created": 1,
            "model": "codestral-latest",
            "choices": [{
                "index": 0,
                "message": {"role": "assistant", "content": answer},
                "finish_reason": "length"
            }],
            "usage": {"prompt_tokens": 11, "completion_tokens": 32768, "total_tokens": 32779}
        })))
        .mount(&server)
        .await;

    let text = post(
        router_for(&server),
        json!({
            "model": "codestral-latest",
            "messages": [{"role": "user", "content": "hello"}]
        }),
    )
    .await;

    let value: Value = serde_json::from_str(&text).expect("json answer");
    let content = value["choices"][0]["message"]["content"]
        .as_str()
        .unwrap_or_default();
    assert!(
        content.starts_with("warning: unused variable"),
        "the text before the loop was lost: {content}"
    );
    assert!(
        content.len() < 200,
        "the loop reached the client: {} bytes",
        content.len()
    );
    assert_eq!(value["choices"][0]["finish_reason"], json!("length"));
}

#[tokio::test]
async fn a_looping_stream_is_cut_short() {
    let server = MockServer::start().await;
    let mut frames = String::new();
    // The upstream streams the loop in chunks, the way a real provider does:
    // the same text arrives again and again, until the guard stops it.
    let chunk = "(clippy::unused_variables) ".repeat(100);
    for _ in 0..20 {
        frames.push_str("data: ");
        frames.push_str(
            &json!({
                "id": "chatcmpl-loop",
                "object": "chat.completion.chunk",
                "created": 1,
                "model": "codestral-latest",
                "choices": [{
                    "index": 0,
                    "delta": {"content": chunk},
                    "finish_reason": Value::Null
                }]
            })
            .to_string(),
        );
        frames.push_str("\n\n");
    }
    frames.push_str("data: [DONE]\n\n");

    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_raw(frames, "text/event-stream"),
        )
        .mount(&server)
        .await;

    let text = post(
        router_for(&server),
        json!({
            "model": "codestral-latest",
            "messages": [{"role": "user", "content": "hello"}],
            "stream": true
        }),
    )
    .await;

    assert!(
        text.len() < 20000,
        "the loop was streamed through: {} bytes",
        text.len()
    );
    assert!(
        text.contains("\"finish_reason\":\"length\""),
        "the truncated answer was not reported: {text}"
    );
}

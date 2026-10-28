//! agentrouter only answers a client it recognises.
//!
//! The platform routes quota by client type: a request without a recognised
//! client identity is refused with `401 unauthorized_client_error` even when
//! the key is valid (probed live 2026-10-27 — no fingerprint and a browser
//! user-agent both get 401). Its live models speak the Codex Responses
//! protocol on `/v1/responses` and reject `/v1/chat/completions` outright
//! (`400 不支持 /v1/chat/completions 接口，请改用 /v1/responses`).
//!
//! Two things must therefore hold: the registry has to name the models the
//! platform actually serves, and the router has to identify as a Codex
//! client when it talks to them.

use heterion_router_core::{ChatCompletionRequest, ChatMessage};
use heterion_router_gateway::backend::ChatBackend;
use heterion_router_gateway::responses::ResponsesBackend;
use heterion_router_providers::ProviderRegistry;
use serde_json::json;
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{header, method, path},
};

/// The header agentrouter reads to recognise a client.
const ORIGINATOR: &str = "originator";
/// The Codex client name it expects on that header.
const CODEX: &str = "codex_cli_rs";

fn request(model: &str) -> ChatCompletionRequest {
    ChatCompletionRequest {
        model: model.to_string(),
        messages: vec![ChatMessage::plain("user", "Say exactly: OK")],
        stream: false,
        ..Default::default()
    }
}

/// A Responses payload the router's translator understands.
fn responses_body() -> serde_json::Value {
    json!({
        "id": "resp_1",
        "object": "response",
        "status": "completed",
        "output": [{
            "type": "message",
            "role": "assistant",
            "content": [{"type": "output_text", "text": "OK"}],
        }],
        "usage": {"input_tokens": 8, "output_tokens": 5, "total_tokens": 13},
    })
}

#[test]
fn the_registry_names_the_models_the_platform_actually_serves() {
    let registry = ProviderRegistry::load();
    let entry = registry
        .get("agentrouter")
        .expect("agentrouter is registered");
    assert_eq!(
        entry.format.as_deref(),
        Some("openai-responses"),
        "agentrouter speaks the Responses protocol, not chat/completions"
    );
    assert_eq!(
        entry.base_url.as_deref(),
        Some("https://agentrouter.org/v1/responses"),
        "the responses endpoint is the only one the platform serves"
    );
    let models: Vec<&str> = entry.models.iter().map(|model| model.id.as_str()).collect();
    assert_eq!(
        models,
        vec!["gpt-6.1-sol", "gpt-6-astra", "gpt-5.6-sol"],
        "the platform's own model list is the contract — retired Claude ids must not linger"
    );
}

#[tokio::test]
async fn a_request_carries_the_client_fingerprint_the_platform_accepts() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/responses"))
        .and(header(ORIGINATOR, CODEX))
        .respond_with(ResponseTemplate::new(200).set_body_json(responses_body()))
        .expect(1)
        .mount(&server)
        .await;

    let backend = ResponsesBackend::new(
        "agentrouter".to_string(),
        format!("{}/v1/responses", server.uri()),
        vec!["key".to_string()],
    )
    .unwrap();

    let response = backend.complete(request("gpt-6.1-sol")).await.unwrap();
    assert_eq!(response.choices[0].message.text(), "OK");
    server.verify().await;

    let sent = server.received_requests().await.unwrap();
    assert_eq!(
        sent[0].headers[ORIGINATOR], CODEX,
        "the platform refuses a request without a recognised client"
    );
}

#[tokio::test]
async fn other_providers_keep_their_own_headers() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/responses"))
        .respond_with(ResponseTemplate::new(200).set_body_json(responses_body()))
        .expect(1)
        .mount(&server)
        .await;

    let backend = ResponsesBackend::new(
        "deepseek".to_string(),
        format!("{}/responses", server.uri()),
        vec!["key".to_string()],
    )
    .unwrap();
    backend.complete(request("deepseek-v4-pro")).await.unwrap();

    let sent = server.received_requests().await.unwrap();
    assert!(
        !sent[0].headers.contains_key(ORIGINATOR),
        "the Codex fingerprint is agentrouter's, not every provider's"
    );
}

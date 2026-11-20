//! The quota probe must survive what platforms actually refuse.
//!
//! Two refusals have nothing to do with the quota, and both were measured on
//! live platforms (17.11.2026):
//!
//! 1. **The budget.** AI Horde's native text API refuses a token limit below
//!    16 — «Invalid 'max_tokens': the AI Horde refuses a token limit below 16,
//!    the minimum generation length of its native text API, rather than raising
//!    it, but got 1 instead». A probe that only ever asks for one token never
//!    gets an answer there, and the platform is reported as `unknown`.
//! 2. **The model.** llm7 answered «Model 'gpt-4o-mini-2024-07-18' is
//!    currently unavailable» and nvidia answered «The model
//!    'deepseek-ai/deepseek-v4-flash-0731' has reached its end of life on
//!    2026-09-21T08:00:00Z» — both are models the registry still lists. The
//!    platform's own catalogue knows better.
//!
//! Contracts:
//! 1. `a_platform_that_refuses_a_tiny_budget_is_probed_again` — the probe
//!    climbs the token budget when the platform complains about it.
//! 2. `a_probe_falls_back_to_a_model_the_platform_still_lists` — a model the
//!    platform does not serve does not end the measurement.
//! 3. `a_refusal_about_neither_stays_unknown` — a platform that says nothing
//!    about the quota still yields `unknown`, not a guess.

use heterion_router_gateway::provider_sync;
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{body_partial_json, method, path},
};

/// The budget AI Horde refuses to go below.
const AI_HORDE_MINIMUM: u32 = 16;

#[tokio::test]
async fn a_platform_that_refuses_a_tiny_budget_is_probed_again() {
    let upstream = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(body_partial_json(serde_json::json!({"max_tokens": 1})))
        .respond_with(ResponseTemplate::new(400).set_body_string(
            r#"{"error":{"message":"Invalid 'max_tokens': the AI Horde refuses a token limit below 16, the minimum generation length of its native text API, rather than raising it, but got 1 instead.","type":"invalid_request_error"}}"#,
        ))
        .expect(1)
        .mount(&upstream)
        .await;

    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(body_partial_json(
            serde_json::json!({"max_tokens": AI_HORDE_MINIMUM}),
        ))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("x-ratelimit-remaining-requests", "9")
                .insert_header("x-ratelimit-limit-requests", "10")
                .set_body_string(r#"{"id":"x","choices":[]}"#),
        )
        .expect(1)
        .mount(&upstream)
        .await;

    let http = reqwest::Client::new();
    let outcome = provider_sync::sync_connection(
        &http,
        &provider_sync::SyncTarget {
            provider: "aihorde".to_string(),
            format: "openai".to_string(),
            base_url: format!("{}/v1", upstream.uri()),
            model: Some("registry-model".to_string()),
            connection_id: "connection-1".to_string(),
            token: "sk-test".to_string(),
            free_only: false,
        },
    )
    .await;

    let reading = outcome
        .quota
        .expect("the platform answered once the budget was acceptable");
    assert_eq!(
        reading.state.as_str(),
        "available",
        "a refusal about the budget is not a refusal about the quota: {}",
        reading.detail
    );
    assert_eq!(reading.remaining_percentage, Some(90.0));
    upstream.verify().await;
}

#[tokio::test]
async fn a_probe_falls_back_to_a_model_the_platform_still_lists() {
    let upstream = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .respond_with(
            ResponseTemplate::new(200).set_body_string(r#"{"data":[{"id":"live-model"}]}"#),
        )
        .mount(&upstream)
        .await;

    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(body_partial_json(serde_json::json!({"model": "registry-model"})))
        .respond_with(ResponseTemplate::new(400).set_body_string(
            r#"{"error":{"message":"Model 'registry-model' is currently unavailable.","type":"invalid_request_error","code":"model_unavailable"}}"#,
        ))
        .mount(&upstream)
        .await;

    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(body_partial_json(
            serde_json::json!({"model": "live-model"}),
        ))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("x-ratelimit-remaining-requests", "42")
                .insert_header("x-ratelimit-limit-requests", "100")
                .set_body_string(r#"{"id":"x","choices":[]}"#),
        )
        .mount(&upstream)
        .await;

    let http = reqwest::Client::new();
    let outcome = provider_sync::sync_connection(
        &http,
        &provider_sync::SyncTarget {
            provider: "llm7".to_string(),
            format: "openai".to_string(),
            base_url: format!("{}/v1", upstream.uri()),
            model: Some("registry-model".to_string()),
            connection_id: "connection-1".to_string(),
            token: "sk-test".to_string(),
            free_only: false,
        },
    )
    .await;

    let reading = outcome
        .quota
        .expect("the platform answered with a model it still serves");
    assert_eq!(
        reading.state.as_str(),
        "available",
        "a dead model in the registry is not a dead platform: {}",
        reading.detail
    );
    assert_eq!(reading.remaining_percentage, Some(42.0));
    upstream.verify().await;
}

#[tokio::test]
async fn a_refusal_about_neither_stays_unknown() {
    let upstream = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(400).set_body_string(
            r#"{"error":{"message":"Request is malformed.","type":"invalid_request_error"}}"#,
        ))
        .mount(&upstream)
        .await;

    let http = reqwest::Client::new();
    let outcome = provider_sync::sync_connection(
        &http,
        &provider_sync::SyncTarget {
            provider: "mystery".to_string(),
            format: "openai".to_string(),
            base_url: format!("{}/v1", upstream.uri()),
            model: Some("registry-model".to_string()),
            connection_id: "connection-1".to_string(),
            token: "sk-test".to_string(),
            free_only: false,
        },
    )
    .await;

    let reading = outcome.quota.expect("the platform answered");
    assert_eq!(
        reading.state.as_str(),
        "unknown",
        "a refusal that says nothing about the quota must stay unknown"
    );
    assert_eq!(
        reading.remaining_percentage, None,
        "a percentage must never be invented"
    );
    upstream.verify().await;
}

/// A key that may not use the model is a refusal about the model, not about the
/// quota — measured on sealion (17.11.2026): «The requested model '…' is not
/// available for this API key, or the model name is invalid», code
/// `key_model_access_denied`. The same key serves other models of the area.
#[tokio::test]
async fn a_model_the_key_may_not_use_does_not_end_the_measurement() {
    let upstream = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .respond_with(
            ResponseTemplate::new(200).set_body_string(r#"{"data":[{"id":"live-model"}]}"#),
        )
        .mount(&upstream)
        .await;

    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(body_partial_json(serde_json::json!({"model": "registry-model"})))
        .respond_with(ResponseTemplate::new(403).set_body_string(
            r#"{"error":{"message":"The requested model 'registry-model' is not available for this API key, or the model name is invalid. Check the models available to you and try again.","type":"key_model_access_denied"}}"#,
        ))
        .mount(&upstream)
        .await;

    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(body_partial_json(
            serde_json::json!({"model": "live-model"}),
        ))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("x-ratelimit-remaining-requests", "7")
                .insert_header("x-ratelimit-limit-requests", "10")
                .set_body_string(r#"{"id":"x","choices":[]}"#),
        )
        .mount(&upstream)
        .await;

    let http = reqwest::Client::new();
    let outcome = provider_sync::sync_connection(
        &http,
        &provider_sync::SyncTarget {
            provider: "sealion".to_string(),
            format: "openai".to_string(),
            base_url: format!("{}/v1", upstream.uri()),
            model: Some("registry-model".to_string()),
            connection_id: "connection-1".to_string(),
            token: "sk-test".to_string(),
            free_only: false,
        },
    )
    .await;

    let reading = outcome
        .quota
        .expect("the platform answered with a model the key may use");
    assert_eq!(
        reading.state.as_str(),
        "available",
        "a model the key may not use is not a quota problem: {}",
        reading.detail
    );
    assert_eq!(reading.remaining_percentage, Some(70.0));
    upstream.verify().await;
}

/// A model without a worker right now is a refusal about the model — measured
/// on AI Horde (17.11.2026): «No worker is currently serving the model '…'.
/// Retry later.», code `no_eligible_worker`. Another model of the same area
/// may be served.
#[tokio::test]
async fn a_model_without_a_worker_is_probed_with_another_one() {
    let upstream = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .respond_with(
            ResponseTemplate::new(200).set_body_string(r#"{"data":[{"id":"live-model"}]}"#),
        )
        .mount(&upstream)
        .await;

    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(body_partial_json(serde_json::json!({"model": "registry-model"})))
        .respond_with(ResponseTemplate::new(503).set_body_string(
            r#"{"error":{"message":"No worker is currently serving the model 'registry-model'. Retry later.","type":"api_error","code":"no_eligible_worker"}}"#,
        ))
        .mount(&upstream)
        .await;

    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(body_partial_json(
            serde_json::json!({"model": "live-model"}),
        ))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("x-ratelimit-remaining-requests", "1")
                .insert_header("x-ratelimit-limit-requests", "2")
                .set_body_string(r#"{"id":"x","choices":[]}"#),
        )
        .mount(&upstream)
        .await;

    let http = reqwest::Client::new();
    let outcome = provider_sync::sync_connection(
        &http,
        &provider_sync::SyncTarget {
            provider: "aihorde".to_string(),
            format: "openai".to_string(),
            base_url: format!("{}/v1", upstream.uri()),
            model: Some("registry-model".to_string()),
            connection_id: "connection-1".to_string(),
            token: "sk-test".to_string(),
            free_only: false,
        },
    )
    .await;

    let reading = outcome
        .quota
        .expect("the platform answered with a model it serves");
    assert_eq!(
        reading.state.as_str(),
        "available",
        "a model without a worker is not a quota problem: {}",
        reading.detail
    );
    assert_eq!(reading.remaining_percentage, Some(50.0));
    upstream.verify().await;
}

/// A bare `404` from a chat endpoint means the model is not there — measured on
/// nvidia (17.11.2026), where the area answers «404 page not found» in plain
/// text, without naming the model at all.
#[tokio::test]
async fn a_bare_404_means_the_model_is_not_there() {
    let upstream = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .respond_with(
            ResponseTemplate::new(200).set_body_string(r#"{"data":[{"id":"live-model"}]}"#),
        )
        .mount(&upstream)
        .await;

    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(body_partial_json(
            serde_json::json!({"model": "registry-model"}),
        ))
        .respond_with(ResponseTemplate::new(404).set_body_string("404 page not found"))
        .mount(&upstream)
        .await;

    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(body_partial_json(
            serde_json::json!({"model": "live-model"}),
        ))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("x-ratelimit-remaining-requests", "3")
                .insert_header("x-ratelimit-limit-requests", "4")
                .set_body_string(r#"{"id":"x","choices":[]}"#),
        )
        .mount(&upstream)
        .await;

    let http = reqwest::Client::new();
    let outcome = provider_sync::sync_connection(
        &http,
        &provider_sync::SyncTarget {
            provider: "nvidia".to_string(),
            format: "openai".to_string(),
            base_url: format!("{}/v1", upstream.uri()),
            model: Some("registry-model".to_string()),
            connection_id: "connection-1".to_string(),
            token: "sk-test".to_string(),
            free_only: false,
        },
    )
    .await;

    let reading = outcome
        .quota
        .expect("the platform answered with a model it serves");
    assert_eq!(
        reading.state.as_str(),
        "available",
        "a bare 404 is a missing model, not a quota problem: {}",
        reading.detail
    );
    assert_eq!(reading.remaining_percentage, Some(75.0));
    upstream.verify().await;
}

/// An embedding model on a chat endpoint is a refusal about the model —
/// measured on sealion (17.11.2026): their litellm gateway answers «litellm.
/// BadRequestError: Unmapped LLM provider for this endpoint» for
/// `BAAI/bge-m3`. The catalogue opens with such models, so the probe has to
/// step over them.
#[tokio::test]
async fn a_model_that_is_not_a_chat_model_does_not_end_the_measurement() {
    let upstream = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .respond_with(
            ResponseTemplate::new(200).set_body_string(r#"{"data":[{"id":"live-model"}]}"#),
        )
        .mount(&upstream)
        .await;

    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(body_partial_json(serde_json::json!({"model": "registry-model"})))
        .respond_with(ResponseTemplate::new(400).set_body_string(
            r#"{"error":{"message":"litellm.BadRequestError: Unmapped LLM provider for this endpoint","type":"invalid_request_error"}}"#,
        ))
        .mount(&upstream)
        .await;

    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(body_partial_json(
            serde_json::json!({"model": "live-model"}),
        ))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("x-ratelimit-remaining-requests", "5")
                .insert_header("x-ratelimit-limit-requests", "10")
                .set_body_string(r#"{"id":"x","choices":[]}"#),
        )
        .mount(&upstream)
        .await;

    let http = reqwest::Client::new();
    let outcome = provider_sync::sync_connection(
        &http,
        &provider_sync::SyncTarget {
            provider: "sealion".to_string(),
            format: "openai".to_string(),
            base_url: format!("{}/v1", upstream.uri()),
            model: Some("registry-model".to_string()),
            connection_id: "connection-1".to_string(),
            token: "sk-test".to_string(),
            free_only: false,
        },
    )
    .await;

    let reading = outcome
        .quota
        .expect("the platform answered with a chat model it serves");
    assert_eq!(
        reading.state.as_str(),
        "available",
        "a model that is not a chat model is not a quota problem: {}",
        reading.detail
    );
    assert_eq!(reading.remaining_percentage, Some(50.0));
    upstream.verify().await;
}

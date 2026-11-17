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
        "aihorde",
        Some("openai"),
        &format!("{}/v1", upstream.uri()),
        "connection-1",
        "sk-test",
        Some("registry-model"),
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
        "llm7",
        Some("openai"),
        &format!("{}/v1", upstream.uri()),
        "connection-1",
        "sk-test",
        Some("registry-model"),
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
        "mystery",
        Some("openai"),
        &format!("{}/v1", upstream.uri()),
        "connection-1",
        "sk-test",
        Some("registry-model"),
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

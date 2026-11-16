//! The provider matrix: every registry entry gets a *measured* reason.
//!
//! Contracts:
//! 1. `the_reason_comes_from_the_answer_not_from_the_registry` — a provider
//!    the registry calls keyless but the platform answers `401` to needs a
//!    key; one answered `402` to needs credits; one answered `404` with a
//!    router's JSON has no chat route at all.
//! 2. `a_probe_of_a_talking_platform_is_served` — a live probe against a fake
//!    upstream decides by what the upstream actually answered.
//! 3. `the_keyless_pass_takes_only_unconnected_keyless_http_providers` — the
//!    pass that builds a backend without a credential is narrow on purpose.

use heterion_router_gateway::provider_status::{
    ProviderReason, classify, is_keyless, keyless_candidate, probe, probe_body, probe_url,
};
use wiremock::{Mock, MockServer, ResponseTemplate, matchers::path};

/// AC1 — the reason comes from the answer.
#[test]
fn the_reason_comes_from_the_answer_not_from_the_registry() {
    // What the platforms answered on 2026-11-16 (see
    // docs/research/provider-matrix-2026-11-16.md).
    assert_eq!(
        classify(
            401,
            "{\"error\":{\"type\":\"authentication_required\",\"message\":\"Authentication credentials were not provided.\"}}"
        ),
        ProviderReason::NeedsKey,
        "naga-ac wants a key despite auth_type: optional"
    );
    assert_eq!(
        classify(
            401,
            "{\"error\":{\"message\":\"The public beta is closed for now. This endpoint needs an API key, and key registration is closed\"}}"
        ),
        ProviderReason::DeadEndpoint,
        "uncloseai is gone"
    );
    assert_eq!(
        classify(
            402,
            "{\"error\":{\"message\":\"No cake credits. Bake proof-of-work cakes at g4f.dev/chat to earn anonymous usage\"}}"
        ),
        ProviderReason::CreditsRequired,
        "g4f.space wants credits"
    );
    assert_eq!(
        classify(
            404,
            "{\"message\":\"Route POST:/chat/completions not found\",\"error\":\"Not Found\",\"statusCode\":404}"
        ),
        ProviderReason::NotAChatApi,
        "cursor has no OpenAI route"
    );
    assert_eq!(
        classify(
            404,
            "<html lang=\"en\"><head><title>404 Page not found</title></head></html>"
        ),
        ProviderReason::DeadEndpoint,
        "deepai has no chat endpoint at all"
    );
}

/// AC2 — a live probe decides by the upstream's answer.
#[tokio::test]
async fn a_probe_of_a_talking_platform_is_served() {
    let upstream = MockServer::start().await;
    Mock::given(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_string("{\"id\":\"x\"}"))
        .mount(&upstream)
        .await;

    let http = reqwest::Client::new();
    let outcome = probe(
        &http,
        Some("openai"),
        &format!("{}/v1", upstream.uri()),
        Some("m-1"),
    )
    .await
    .expect("a probe");
    assert_eq!(outcome.status, Some(200));
    assert_eq!(outcome.reason, ProviderReason::Served);
    assert_eq!(
        outcome.url,
        format!("{}/v1/chat/completions", upstream.uri())
    );
}

/// A probe of a platform that wants a key says so.
#[tokio::test]
async fn a_probe_of_a_platform_that_wants_a_key_says_so() {
    let upstream = MockServer::start().await;
    Mock::given(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(401).set_body_string("{\"error\":\"unauthorized\"}"))
        .mount(&upstream)
        .await;

    let http = reqwest::Client::new();
    let outcome = probe(
        &http,
        Some("openai"),
        &format!("{}/v1", upstream.uri()),
        Some("m-1"),
    )
    .await
    .expect("a probe");
    assert_eq!(outcome.reason, ProviderReason::NeedsKey);
}

/// The probe body is per format and minimal.
#[test]
fn the_probe_body_is_per_format() {
    let openai = probe_body(Some("openai"), Some("m-1"));
    assert!(openai.contains("\"model\":\"m-1\""));
    assert!(openai.contains("\"max_tokens\":1"));

    let gemini = probe_body(Some("gemini"), Some("m-1"));
    assert!(gemini.contains("\"contents\""));
    assert!(!gemini.contains("\"model\""));

    let responses = probe_body(Some("openai-responses"), Some("m-1"));
    assert!(responses.contains("\"input\""));
}

/// AC3 — the keyless pass is narrow on purpose.
#[test]
fn the_keyless_pass_takes_only_unconnected_keyless_http_providers() {
    assert!(keyless_candidate(
        Some("openai"),
        Some("none"),
        Some("https://api.x/v1"),
        false
    ));
    assert!(!keyless_candidate(
        Some("openai"),
        Some("apikey"),
        Some("https://api.x/v1"),
        false
    ));
    assert!(!keyless_candidate(
        Some("openai"),
        Some("none"),
        Some("https://api.x/v1"),
        true
    ));
    assert!(!keyless_candidate(
        Some("cursor"),
        Some("none"),
        Some("https://api.x"),
        false
    ));
    assert!(!keyless_candidate(
        Some("openai"),
        Some("none"),
        Some("auggie://cli/stdio"),
        false
    ));

    // A provider the registry calls keyless is keyless — the probe decides
    // whether it really is.
    assert!(is_keyless(Some("none")));
    assert!(is_keyless(Some("optional")));
    assert!(!is_keyless(Some("apikey")));
    assert!(!is_keyless(None));
}

/// A format the gateway cannot speak has no probe URL.
#[test]
fn a_format_the_gateway_cannot_speak_has_no_probe_url() {
    assert!(probe_url(Some("cursor"), "https://api2.cursor.sh", None).is_none());
    assert!(probe_url(Some("kiro"), "https://codewhisperer.example", None).is_none());
    assert!(probe_url(Some("clova"), "https://clova.example/v3", None).is_none());
}

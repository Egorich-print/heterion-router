//! Milestone 5 Empirical Challenge Suite: E2E Verification, Adversarial Hardening & Final Gate.
//!
//! The suites of M1–M4 pinned each mechanism in isolation. M5 checks the
//! properties that only show up when the mechanisms are combined, and it
//! checks them through the **observable** surface of the gateway: which
//! upstream was called, with which model, in which order, and how many calls
//! were made at all.
//!
//! Every test here drives the in-repo harness only — the in-process
//! [`RoutingBackend`], the axum application built by
//! [`build_router_with_state`], and mock upstreams. Nothing in this file
//! talks to the live launchd instance on `127.0.0.1:20128` (AGENTS.md:
//! mission changes are not verified against the running service).
//!
//! Invariants pinned here:
//!
//! 1. `test_challenge_every_ladder_position_is_served_in_plan_order` —
//!    every position of the canonical ladder is reachable end to end, and the
//!    order in which the upstreams are actually called is the order of the
//!    plan the router itself built (no hand-written expectation list).
//! 2. `test_challenge_auto_model_serves_over_http_in_both_modes` — the
//!    `heterion-router/auto` model resolves and answers through the gateway
//!    in the plain and in the SSE mode, and the call log names the model
//!    that really served the request.
//! 3. `test_challenge_quality_demotion_changes_the_served_order` — a model
//!    that fails post-flight validation is *demoted in the order in which
//!    requests are served*, not merely in the plan.
//! 4. `test_challenge_429_never_trips_the_breaker` /
//!    `test_challenge_breaker_trip_never_creates_a_cooldown` /
//!    `test_challenge_breaker_trip_does_not_extend_a_cooldown` — the cooldown
//!    set and the breaker set stay orthogonal under real upstream failures.
//! 5. `test_challenge_model_scoped_429_leaves_sibling_models_servable` — a
//!    rate-limited model does not disable its siblings on the same provider.
//! 6. `test_challenge_tier3_protector_absorbs_a_model_scoped_429` — the
//!    Tier-3 high-quota protector still absorbs a Tier-1 rate limit.
//! 7. `test_challenge_dynamic_plan_filters_tools_and_vision` — the plan the
//!    router builds on its own never carries a pseudo-tool model for a
//!    tool-bearing request, and never carries a non-vision model for a
//!    request with an image.
//! 8. `test_challenge_zero_quota_pruning_makes_no_extra_upstream_call` —
//!    pruning a 280k-token prompt costs no upstream quota: the number of
//!    upstream calls equals the number of candidate attempts, and the
//!    compressed request keeps its `tool_call_id` parity and its tail.

use async_trait::async_trait;
use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use futures::StreamExt;
use heterion_router_core::{
    ChatChoice, ChatCompletionRequest, ChatCompletionResponse, ChatMessage, GatewayError,
    StreamChunk, Usage,
};
use heterion_router_db::{Db, repos::recent_calls};
use heterion_router_gateway::{
    AppState,
    backend::{ChatBackend, ChunkStream, stamp_attribution},
    build_router_with_state,
    model_spec::{ToolSupport, is_pseudo_tool_model, model_spec},
    routing::{DEFAULT_AUTO_CANDIDATES, RoutingBackend},
};
use heterion_router_routing::Router;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tower::ServiceExt;

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

/// One upstream attempt: which provider was asked, and for which model.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Attempt {
    provider: String,
    model: String,
}

/// The global order in which the upstreams were called.
///
/// Shared by every mock of one scenario, so the *interleaving* of the calls is
/// observable and not only the per-provider counts.
#[derive(Clone, Default)]
struct Ledger {
    attempts: Arc<Mutex<Vec<Attempt>>>,
}

impl Ledger {
    fn attempts(&self) -> Vec<Attempt> {
        self.attempts.lock().unwrap().clone()
    }
}

/// A mock upstream that records what it was asked to serve.
struct ProbeBackend {
    provider: &'static str,
    responses: Mutex<Vec<Result<ChatCompletionResponse, GatewayError>>>,
    stream_results: Mutex<Vec<Result<StreamChunk, GatewayError>>>,
    ledger: Ledger,
    requests: Mutex<Vec<ChatCompletionRequest>>,
}

impl ProbeBackend {
    fn new(provider: &'static str, ledger: Ledger) -> Self {
        Self {
            provider,
            responses: Mutex::new(Vec::new()),
            stream_results: Mutex::new(Vec::new()),
            ledger,
            requests: Mutex::new(Vec::new()),
        }
    }

    fn push_response(&self, response: Result<ChatCompletionResponse, GatewayError>) {
        self.responses.lock().unwrap().push(response);
    }

    fn push_stream_chunk(&self, chunk: Result<StreamChunk, GatewayError>) {
        self.stream_results.lock().unwrap().push(chunk);
    }

    /// The last request this upstream was asked to serve.
    fn last_request(&self) -> Option<ChatCompletionRequest> {
        self.requests.lock().unwrap().last().cloned()
    }
}

#[async_trait]
impl ChatBackend for ProbeBackend {
    fn name(&self) -> &'static str {
        self.provider
    }

    async fn complete(
        &self,
        request: ChatCompletionRequest,
    ) -> Result<ChatCompletionResponse, GatewayError> {
        self.ledger.attempts.lock().unwrap().push(Attempt {
            provider: self.provider.to_string(),
            model: request.model.clone(),
        });
        self.requests.lock().unwrap().push(request.clone());

        let mut guard = self.responses.lock().unwrap();
        if !guard.is_empty() {
            return guard.remove(0);
        }
        Ok(text_response(
            &request.model,
            &format!("served by {}", self.provider),
        ))
    }

    fn stream(&self, request: ChatCompletionRequest) -> ChunkStream {
        self.ledger.attempts.lock().unwrap().push(Attempt {
            provider: self.provider.to_string(),
            model: request.model.clone(),
        });
        self.requests.lock().unwrap().push(request);

        let mut guard = self.stream_results.lock().unwrap();
        let items: Vec<Result<StreamChunk, GatewayError>> = if !guard.is_empty() {
            guard.drain(..).collect()
        } else {
            vec![Ok(StreamChunk {
                content: Some(format!("streamed by {}", self.provider)),
                finish_reason: Some("stop".into()),
                ..Default::default()
            })]
        };
        Box::pin(futures::stream::iter(items))
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// The backend map of a scenario and the probes behind it.
type Backends = (
    HashMap<String, Arc<dyn ChatBackend>>,
    HashMap<String, Arc<ProbeBackend>>,
);

/// Build the backend map for the given providers plus the probes behind it.
fn setup_backends(ledger: &Ledger, providers: &[&'static str]) -> Backends {
    let mut backends: HashMap<String, Arc<dyn ChatBackend>> = HashMap::new();
    let mut probes: HashMap<String, Arc<ProbeBackend>> = HashMap::new();
    for provider in providers {
        let probe = Arc::new(ProbeBackend::new(provider, ledger.clone()));
        backends.insert((*provider).to_string(), probe.clone());
        probes.insert((*provider).to_string(), probe);
    }
    (backends, probes)
}

/// A plain text answer for `model`.
fn text_response(model: &str, content: &str) -> ChatCompletionResponse {
    ChatCompletionResponse {
        id: format!("resp-{model}"),
        object: "chat.completion",
        created: 1,
        model: model.to_string(),
        usage: Usage::default(),
        choices: vec![ChatChoice {
            index: 0,
            message: ChatMessage::assistant_text(content),
            finish_reason: "stop".into(),
        }],
        raw: None,
    }
}

/// An answer that carries a native structured tool call.
fn tool_call_response(model: &str) -> ChatCompletionResponse {
    ChatCompletionResponse {
        id: format!("resp-{model}"),
        object: "chat.completion",
        created: 1,
        model: model.to_string(),
        usage: Usage::default(),
        choices: vec![ChatChoice {
            index: 0,
            message: ChatMessage {
                role: "assistant".into(),
                content: None,
                reasoning_content: None,
                tool_calls: Some(json!([{
                    "id": "call_native_1",
                    "type": "function",
                    "function": {"name": "read_file", "arguments": "{\"path\":\"main.rs\"}"}
                }])),
                tool_call_id: None,
                name: None,
            },
            finish_reason: "tool_calls".into(),
        }],
        raw: None,
    }
}

/// An answer that pretends to call a tool with unstructured XML.
fn pseudo_tool_response(model: &str) -> ChatCompletionResponse {
    text_response(
        model,
        "Here is the execution: <tool_call>{\"name\": \"read_file\"}</tool_call>",
    )
}

fn upstream_429(retry_after_secs: u64) -> GatewayError {
    GatewayError::Upstream(format!(
        "upstream status 429: rate limit exceeded, retry-after: {retry_after_secs}"
    ))
}

fn upstream_502() -> GatewayError {
    GatewayError::Upstream("upstream status 502: bad gateway".into())
}

/// A plain `heterion-router/auto` request (planning stage).
fn auto_request() -> ChatCompletionRequest {
    ChatCompletionRequest {
        model: "heterion-router/auto".into(),
        messages: vec![ChatMessage::plain(
            "user",
            "Design the storage layout for the response cache",
        )],
        ..Default::default()
    }
}

/// A `heterion-router/auto` request that carries tools.
fn auto_request_with_tools() -> ChatCompletionRequest {
    ChatCompletionRequest {
        model: "heterion-router/auto".into(),
        messages: vec![ChatMessage::plain("user", "Read the project file")],
        tools: Some(json!([{
            "type": "function",
            "function": {
                "name": "read_file",
                "description": "reads a file from the filesystem",
                "parameters": {
                    "type": "object",
                    "properties": {"path": {"type": "string"}},
                    "required": ["path"]
                }
            }
        }])),
        ..Default::default()
    }
}

/// A `heterion-router/auto` request that carries an image.
fn auto_request_with_image() -> ChatCompletionRequest {
    ChatCompletionRequest {
        model: "heterion-router/auto".into(),
        messages: vec![ChatMessage {
            role: "user".into(),
            content: Some(json!([
                {"type": "text", "text": "What does this diagram show?"},
                {"type": "image_url", "image_url": {"url": "https://example.org/diagram.png"}}
            ])),
            reasoning_content: None,
            tool_calls: None,
            tool_call_id: None,
            name: None,
        }],
        ..Default::default()
    }
}

/// A long conversation whose middle is made of big tool outputs: ~280k tokens,
/// above the 256k window of the Tier-2 candidates.
fn large_history_request() -> ChatCompletionRequest {
    let mut messages = vec![
        ChatMessage::plain("system", "You are an autonomous software engineer."),
        ChatMessage::plain(
            "user",
            "Refactor the architecture and optimize performance.",
        ),
    ];

    let large_body = "x".repeat(140_000);
    for i in 0..8 {
        let call_id = format!("call_int_{i}");
        messages.push(ChatMessage {
            role: "assistant".into(),
            content: None,
            reasoning_content: None,
            tool_calls: Some(json!([{
                "id": call_id,
                "type": "function",
                "function": {"name": "grep", "arguments": "{\"query\":\"fn\"}"}
            }])),
            tool_call_id: None,
            name: None,
        });
        messages.push(ChatMessage {
            role: "tool".into(),
            content: Some(Value::String(large_body.clone())),
            reasoning_content: None,
            tool_calls: None,
            tool_call_id: Some(call_id),
            name: None,
        });
    }

    for i in 0..4 {
        let call_id = format!("call_tail_{i}");
        messages.push(ChatMessage {
            role: "assistant".into(),
            content: None,
            reasoning_content: None,
            tool_calls: Some(json!([{
                "id": call_id,
                "type": "function",
                "function": {"name": "read_file", "arguments": "{\"path\":\"main.rs\"}"}
            }])),
            tool_call_id: None,
            name: None,
        });
        messages.push(ChatMessage {
            role: "tool".into(),
            content: Some(Value::String("fn main() { println!(\"ok\"); }".into())),
            reasoning_content: None,
            tool_calls: None,
            tool_call_id: Some(call_id),
            name: None,
        });
    }

    ChatCompletionRequest {
        model: "heterion-router/auto".into(),
        messages,
        ..Default::default()
    }
}

/// The capability tier a model belongs to, read from the canonical ladder.
fn tier_of(model: &str) -> u8 {
    let base = DEFAULT_AUTO_CANDIDATES
        .iter()
        .find(|candidate| candidate.model == model)
        .map(|candidate| candidate.base_priority)
        .unwrap_or_else(|| panic!("{model} is not part of the canonical ladder"));
    match base {
        p if p >= 280.0 => 1,
        p if p >= 190.0 => 2,
        _ => 3,
    }
}

/// The `tool_call_id`s of every tool message of a request, in order.
fn tool_call_ids(request: &ChatCompletionRequest) -> Vec<String> {
    request
        .messages
        .iter()
        .filter(|message| message.role == "tool")
        .filter_map(|message| message.tool_call_id.clone())
        .collect()
}

/// Total number of content bytes carried by a request.
fn content_bytes(request: &ChatCompletionRequest) -> usize {
    request
        .messages
        .iter()
        .map(|message| match &message.content {
            Some(Value::String(text)) => text.len(),
            Some(other) => other.to_string().len(),
            None => 0,
        })
        .sum()
}

// ---------------------------------------------------------------------------
// Challenge 1 — the whole ladder is reachable, and it is served in plan order
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_challenge_every_ladder_position_is_served_in_plan_order() {
    let mut tiers_served: Vec<u8> = Vec::new();

    // The ladder has as many positions as the canonical table; every one of
    // them must be able to serve a request when the ones above it fail.
    let ladder_len = DEFAULT_AUTO_CANDIDATES.len();

    for target in 0..ladder_len {
        let ledger = Ledger::default();
        let (backends, probes) = setup_backends(&ledger, &["gemini", "agentrouter", "mistral"]);
        // A high breaker threshold keeps the ladder stable *inside* one
        // request: this test is about the plan order, the breaker isolation is
        // pinned by its own tests.
        let routing =
            RoutingBackend::with_breaker_settings(backends, Router::new(vec![], vec![]), 100, 60);

        let request = auto_request();
        // The expectation is the plan the router builds itself — never a
        // hand-written list of models.
        let plan = routing.auto_plan_at(&request, Instant::now());
        assert_eq!(
            plan.steps.len(),
            ladder_len,
            "the plan must hold every ladder position"
        );
        for candidate in DEFAULT_AUTO_CANDIDATES {
            assert!(
                plan.steps.iter().any(|step| step.1 == candidate.model),
                "the plan must contain the ladder model {}",
                candidate.model
            );
        }

        // Everything above the target position fails with a 502.
        for (provider, model) in plan.steps.iter().take(target) {
            probes[provider].push_response(Err(upstream_502()));
            let _ = model;
        }
        // The target position answers.
        let (target_provider, target_model) = &plan.steps[target];
        probes[target_provider].push_response(Ok(text_response(
            target_model,
            &format!("served by {target_model}"),
        )));

        let response = routing
            .complete(request)
            .await
            .unwrap_or_else(|error| panic!("ladder position {target} must serve: {error}"));
        assert_eq!(
            response.model, *target_model,
            "ladder position {target} must be served by the candidate that answered"
        );

        // The upstreams were called in the order of the plan, up to and
        // including the one that answered.
        let expected: Vec<Attempt> = plan
            .steps
            .iter()
            .take(target + 1)
            .map(|(provider, model)| Attempt {
                provider: provider.clone(),
                model: model.clone(),
            })
            .collect();
        assert_eq!(
            ledger.attempts(),
            expected,
            "the upstreams must be called in the order of the plan"
        );

        tiers_served.push(tier_of(target_model));
    }

    for tier in [1u8, 2, 3] {
        assert!(
            tiers_served.contains(&tier),
            "tier {tier} must be reachable through the ladder, got {tiers_served:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// Challenge 2 — `heterion-router/auto` answers over HTTP, plain and SSE
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_challenge_auto_model_serves_over_http_in_both_modes() {
    let db = Arc::new(Db::open_in_memory().unwrap());
    db.migrate().unwrap();

    let ledger = Ledger::default();
    let (backends, probes) = setup_backends(&ledger, &["gemini"]);
    probes["gemini"].push_response(Ok(text_response("gemini-3-flash-preview", "plain answer")));
    probes["gemini"].push_stream_chunk(Ok(StreamChunk {
        content: Some("streamed answer".into()),
        finish_reason: Some("stop".into()),
        ..Default::default()
    }));

    // The rotation layer is off: this contract pins the *ladder* — which model
    // is the head and how a failure moves the head — and the rotation
    // deliberately moves the head after every answer.
    let routing =
        Arc::new(RoutingBackend::new(backends, Router::new(vec![], vec![])).with_rotation(false));
    let router = build_router_with_state(AppState::with_db(routing, db.clone()));

    // --- plain mode -------------------------------------------------------
    let http_request = Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header("content-type", "application/json")
        .body(Body::from(
            serde_json::to_vec(&json!({
                "model": "heterion-router/auto",
                "messages": [{"role": "user", "content": "Design the cache layout"}]
            }))
            .unwrap(),
        ))
        .unwrap();

    let http_response = router.clone().oneshot(http_request).await.unwrap();
    assert_eq!(http_response.status(), StatusCode::OK);
    // The header names the model that really served the request, even when
    // the body keeps the name the client asked for.
    let served_model = http_response
        .headers()
        .get("x-router-model-used")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    assert_eq!(
        served_model.as_deref(),
        Some("gemini-3-flash-preview"),
        "the served model must be visible to the client"
    );
    let bytes = http_response
        .into_body()
        .collect()
        .await
        .unwrap()
        .to_bytes();
    let body: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(body["choices"][0]["message"]["content"], "plain answer");
    assert!(body["choices"][0]["finish_reason"].is_string());

    // --- streaming mode ----------------------------------------------------
    let http_request = Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header("content-type", "application/json")
        .body(Body::from(
            serde_json::to_vec(&json!({
                "model": "auto",
                "stream": true,
                "messages": [{"role": "user", "content": "Design the cache layout"}]
            }))
            .unwrap(),
        ))
        .unwrap();

    let http_response = router.oneshot(http_request).await.unwrap();
    assert_eq!(http_response.status(), StatusCode::OK);

    let mut body_stream = http_response.into_body().into_data_stream();
    let mut chunks: Vec<Value> = Vec::new();
    while let Some(chunk) = body_stream.next().await {
        let bytes = chunk.unwrap();
        let text = String::from_utf8_lossy(&bytes);
        for line in text.lines() {
            if let Some(data) = line.strip_prefix("data: ")
                && data != "[DONE]"
                && let Ok(value) = serde_json::from_str::<Value>(data)
            {
                chunks.push(value);
            }
        }
    }
    assert!(!chunks.is_empty(), "the SSE mode must deliver chunks");
    assert!(
        chunks
            .iter()
            .any(|chunk| chunk["choices"][0]["delta"]["content"] == "streamed answer"),
        "the SSE chunks must carry the answer, got {chunks:?}"
    );
    for chunk in &chunks {
        assert_eq!(
            chunk["model"], "auto",
            "a streamed chunk must keep the requested model name"
        );
    }

    // --- what the operator sees -------------------------------------------
    let guard = db.connection();
    let logs = recent_calls(&guard, 10).expect("call logs must be readable");
    assert_eq!(logs.len(), 2, "both requests must be logged");
    for log in &logs {
        assert_eq!(log.provider.as_deref(), Some("gemini"));
        assert_eq!(log.model.as_deref(), Some("gemini-3-flash-preview"));
        assert_eq!(log.combo_name.as_deref(), Some("heterion-router/auto"));
    }
}

// ---------------------------------------------------------------------------
// Challenge 3 — a demoted model changes the order in which requests are served
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_challenge_quality_demotion_changes_the_served_order() {
    let ledger = Ledger::default();
    let (backends, probes) = setup_backends(&ledger, &["gemini", "agentrouter"]);
    // The rotation layer is off: this contract pins the ladder order (the
    // demoted model loses the head), not the rotation.
    let routing = RoutingBackend::new(backends, Router::new(vec![], vec![])).with_rotation(false);

    // The head of the ladder answers with a pseudo XML "tool call": the
    // post-flight validation must reject it and fail over.
    probes["gemini"].push_response(Ok(pseudo_tool_response("gemini-3-flash-preview")));
    probes["agentrouter"].push_response(Ok(tool_call_response("gpt-6.1-sol")));

    let mut request = auto_request_with_tools();
    let token = stamp_attribution(&mut request);
    let response = routing
        .complete(request)
        .await
        .expect("the request must fail over to the next candidate");
    assert_eq!(response.choices[0].finish_reason, "tool_calls");
    assert_eq!(
        routing.attribution(&token).model.as_deref(),
        Some("gpt-6.1-sol")
    );
    assert_eq!(
        ledger.attempts(),
        vec![
            Attempt {
                provider: "gemini".into(),
                model: "gemini-3-flash-preview".into()
            },
            Attempt {
                provider: "agentrouter".into(),
                model: "gpt-6.1-sol".into()
            },
        ],
        "the defective model must be tried first and replaced by the next one"
    );

    // The next identical request must not be served by the demoted model
    // any more: its quality score dropped, so it lost the head of the ladder.
    probes["agentrouter"].push_response(Ok(tool_call_response("gpt-6.1-sol")));

    let mut request = auto_request_with_tools();
    let token = stamp_attribution(&mut request);
    let _response = routing
        .complete(request)
        .await
        .expect("the second request must be served");
    assert_eq!(
        routing.attribution(&token).model.as_deref(),
        Some("gpt-6.1-sol"),
        "the demoted model must not lead the ladder any more"
    );
    assert_eq!(
        ledger.attempts(),
        vec![
            Attempt {
                provider: "gemini".into(),
                model: "gemini-3-flash-preview".into()
            },
            Attempt {
                provider: "agentrouter".into(),
                model: "gpt-6.1-sol".into()
            },
            Attempt {
                provider: "agentrouter".into(),
                model: "gpt-6.1-sol".into()
            },
        ],
        "the demoted model must not be called again for the second request"
    );
}

// ---------------------------------------------------------------------------
// Challenge 4 — cooldown and breaker stay orthogonal under real failures
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_challenge_429_never_trips_the_breaker() {
    let ledger = Ledger::default();
    let (backends, probes) = setup_backends(&ledger, &["gemini", "agentrouter"]);
    // A breaker threshold of 1: a single recorded failure would open it.
    let routing =
        RoutingBackend::with_breaker_settings(backends, Router::new(vec![], vec![]), 1, 3600);

    probes["gemini"].push_response(Err(upstream_429(30)));
    probes["agentrouter"].push_response(Ok(text_response("gpt-6.1-sol", "served")));

    let response = routing
        .complete(auto_request())
        .await
        .expect("a rate limit must fail over, not fail the request");
    assert_eq!(response.model, "gpt-6.1-sol");

    assert!(
        routing
            .breakers()
            .lock()
            .unwrap()
            .allow("gemini", Instant::now()),
        "a 429 must not trip the breaker of the provider"
    );
    assert!(
        routing
            .cooldowns()
            .is_in_cooldown("gemini", Some("gemini-3-flash-preview")),
        "the rate-limited model must be in cooldown"
    );
}

#[tokio::test]
async fn test_challenge_breaker_trip_never_creates_a_cooldown() {
    let ledger = Ledger::default();
    let (backends, probes) = setup_backends(&ledger, &["gemini", "agentrouter"]);
    let routing =
        RoutingBackend::with_breaker_settings(backends, Router::new(vec![], vec![]), 1, 3600);

    probes["gemini"].push_response(Err(upstream_502()));
    probes["agentrouter"].push_response(Ok(text_response("gpt-6.1-sol", "served")));

    let response = routing
        .complete(auto_request())
        .await
        .expect("a broken backend must fail over");
    assert_eq!(response.model, "gpt-6.1-sol");

    assert!(
        !routing
            .breakers()
            .lock()
            .unwrap()
            .allow("gemini", Instant::now()),
        "a 502 must trip the breaker of the provider"
    );
    assert!(
        !routing.cooldowns().is_in_cooldown("gemini", None),
        "a breaker trip must not create a provider cooldown"
    );
    assert!(
        routing
            .cooldowns()
            .get_cooldown_remaining("gemini", None)
            .is_none(),
        "a breaker trip must not leave a cooldown behind"
    );
}

#[tokio::test]
async fn test_challenge_breaker_trip_does_not_extend_a_cooldown() {
    let ledger = Ledger::default();
    // Only one provider: the rate-limited model and the broken model sit on
    // the same backend, so the two states touch each other.
    let (backends, probes) = setup_backends(&ledger, &["gemini"]);
    let routing =
        RoutingBackend::with_breaker_settings(backends, Router::new(vec![], vec![]), 1, 3600);

    // The head of the ladder is rate limited (30s cooldown on that model),
    // the next candidate of the same provider is broken (breaker trip).
    probes["gemini"].push_response(Err(upstream_429(30)));
    probes["gemini"].push_response(Err(upstream_502()));

    // Every remaining candidate of that provider is skipped because the
    // breaker is open: the request ends in an error, and that is fine — what
    // matters is the state the two mechanisms leave behind.
    let error = routing
        .complete(auto_request())
        .await
        .expect_err("the request must fail when the only provider is broken");
    assert!(
        error.to_string().contains("502"),
        "the failure must be the broken upstream, got: {error}"
    );

    let remaining = routing
        .cooldowns()
        .get_cooldown_remaining("gemini", Some("gemini-3-flash-preview"))
        .expect("the rate limit cooldown must still be there");
    assert!(
        remaining <= Duration::from_secs(30),
        "the breaker trip must not extend the cooldown, got {remaining:?}"
    );
    assert!(
        remaining >= Duration::from_secs(25),
        "the cooldown must still be the original one, got {remaining:?}"
    );
    assert!(
        !routing.cooldowns().is_in_cooldown("gemini", None),
        "the breaker trip must not widen the model cooldown to the whole provider"
    );
    assert!(
        !routing
            .breakers()
            .lock()
            .unwrap()
            .allow("gemini", Instant::now()),
        "the breaker must be open after the 502"
    );
}

// ---------------------------------------------------------------------------
// Challenge 5 — a rate-limited model leaves its siblings alone
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_challenge_model_scoped_429_leaves_sibling_models_servable() {
    let ledger = Ledger::default();
    // One provider, several models: the siblings of the rate-limited model
    // live on the same backend.
    let (backends, probes) = setup_backends(&ledger, &["gemini"]);
    // The rotation layer is off: this contract pins the ladder order around a
    // model-scoped cooldown, not the rotation.
    let routing = RoutingBackend::new(backends, Router::new(vec![], vec![])).with_rotation(false);

    probes["gemini"].push_response(Err(upstream_429(30)));
    probes["gemini"].push_response(Ok(text_response(
        "gemma-4-26b-a4b-it",
        "served by the sibling",
    )));

    let response = routing
        .complete(auto_request())
        .await
        .expect("the sibling model must serve the request");
    assert_eq!(
        response.model, "gemma-4-26b-a4b-it",
        "the sibling of the rate-limited model must serve the request"
    );
    assert_eq!(
        ledger.attempts(),
        vec![
            Attempt {
                provider: "gemini".into(),
                model: "gemini-3-flash-preview".into()
            },
            Attempt {
                provider: "gemini".into(),
                model: "gemma-4-26b-a4b-it".into()
            },
        ],
        "the rate-limited model must be skipped, its sibling must be tried"
    );

    // The cooldown belongs to the rate-limited model only.
    assert!(
        routing
            .cooldowns()
            .is_in_cooldown("gemini", Some("gemini-3-flash-preview"))
    );
    assert!(
        !routing.cooldowns().is_in_cooldown("gemini", None),
        "a per-model 429 must not put the whole provider in cooldown"
    );
    assert_eq!(
        routing
            .cooldowns()
            .availability("gemini", Some("gemma-4-26b-a4b-it")),
        1.0,
        "the sibling must stay fully available"
    );

    // The next request must skip the rate-limited model entirely and be
    // served by the sibling again.
    probes["gemini"].push_response(Ok(text_response(
        "gemma-4-26b-a4b-it",
        "served by the sibling again",
    )));
    let response = routing
        .complete(auto_request())
        .await
        .expect("the sibling must serve the second request");
    assert_eq!(response.model, "gemma-4-26b-a4b-it");
    assert_eq!(
        ledger.attempts(),
        vec![
            Attempt {
                provider: "gemini".into(),
                model: "gemini-3-flash-preview".into()
            },
            Attempt {
                provider: "gemini".into(),
                model: "gemma-4-26b-a4b-it".into()
            },
            Attempt {
                provider: "gemini".into(),
                model: "gemma-4-26b-a4b-it".into()
            },
        ],
        "the rate-limited model must not be called again"
    );
}

// ---------------------------------------------------------------------------
// Challenge 6 — the Tier-3 high-quota protector absorbs a rate limit
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_challenge_tier3_protector_absorbs_a_model_scoped_429() {
    let ledger = Ledger::default();
    let (backends, probes) = setup_backends(&ledger, &["gemini"]);
    let routing = RoutingBackend::new(backends, Router::new(vec![], vec![]));

    // Both models above the Tier-3 protector are rate limited, each on its
    // own model scope.
    probes["gemini"].push_response(Err(upstream_429(30)));
    probes["gemini"].push_response(Err(upstream_429(30)));
    probes["gemini"].push_response(Ok(text_response(
        "gemini-3.5-flash-lite",
        "served by the high-quota protector",
    )));

    let response = routing
        .complete(auto_request())
        .await
        .expect("the Tier-3 protector must absorb the rate limits");
    assert_eq!(
        response.model, "gemini-3.5-flash-lite",
        "the Tier-3 protector must serve the request"
    );
    assert_eq!(tier_of("gemini-3.5-flash-lite"), 3);
    assert_eq!(
        ledger.attempts(),
        vec![
            Attempt {
                provider: "gemini".into(),
                model: "gemini-3-flash-preview".into()
            },
            Attempt {
                provider: "gemini".into(),
                model: "gemma-4-26b-a4b-it".into()
            },
            Attempt {
                provider: "gemini".into(),
                model: "gemini-3.5-flash-lite".into()
            },
        ]
    );

    for rate_limited in ["gemini-3-flash-preview", "gemma-4-26b-a4b-it"] {
        assert!(
            routing
                .cooldowns()
                .is_in_cooldown("gemini", Some(rate_limited)),
            "{rate_limited} must be in cooldown"
        );
    }
    assert!(
        routing
            .cooldowns()
            .is_available("gemini", Some("gemini-3.5-flash-lite")),
        "the protector must stay available"
    );
}

// ---------------------------------------------------------------------------
// Challenge 7 — the dynamic plan filters capabilities
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_challenge_dynamic_plan_filters_tools_and_vision() {
    let ledger = Ledger::default();
    // A pseudo-tool provider is configured: it must never be reached.
    let (backends, probes) = setup_backends(&ledger, &["gemini", "agentrouter", "morph"]);
    let routing = RoutingBackend::new(backends, Router::new(vec![], vec![]));

    // --- a tool-bearing request -------------------------------------------
    // The predicate must have teeth: these ids are the pseudo-tool models the
    // pre-flight filter exists for.
    assert!(is_pseudo_tool_model("morph", "morph-v3-large"));
    assert!(is_pseudo_tool_model("upstage", "syn-pro"));

    let tool_plan = routing.auto_plan_at(&auto_request_with_tools(), Instant::now());
    assert!(!tool_plan.steps.is_empty(), "the plan must not be empty");
    for (provider, model) in &tool_plan.steps {
        assert!(
            !is_pseudo_tool_model(provider, model),
            "{provider}/{model} is a pseudo-tool model and must not be planned"
        );
        assert_eq!(
            model_spec(model).tool_support,
            ToolSupport::Native,
            "{model} does not support tools natively and must not be planned"
        );
    }

    // --- a request with an image ------------------------------------------
    let vision_plan = routing.auto_plan_at(&auto_request_with_image(), Instant::now());
    assert!(!vision_plan.steps.is_empty(), "the plan must not be empty");
    for (_, model) in &vision_plan.steps {
        assert!(
            model_spec(model).supports_vision,
            "{model} cannot read images and must not be planned"
        );
    }
    let text_plan = routing.auto_plan_at(&auto_request(), Instant::now());
    assert!(
        vision_plan.steps.len() < text_plan.steps.len(),
        "the image plan must be a strict subset of the text plan"
    );

    // --- and the same holds for what is really served ----------------------
    probes["gemini"].push_response(Ok(tool_call_response("gemini-3-flash-preview")));
    let response = routing
        .complete(auto_request_with_tools())
        .await
        .expect("the tool-bearing request must be served");
    assert_eq!(response.model, "gemini-3-flash-preview");

    probes["gemini"].push_response(Ok(text_response(
        "gemini-3-flash-preview",
        "the image is a diagram",
    )));
    let response = routing
        .complete(auto_request_with_image())
        .await
        .expect("the image request must be served");
    assert_eq!(response.model, "gemini-3-flash-preview");
    for attempt in ledger.attempts() {
        assert!(
            !is_pseudo_tool_model(&attempt.provider, &attempt.model),
            "the pseudo-tool provider was reached: {attempt:?}"
        );
        assert!(
            !attempt.provider.contains("morph"),
            "the pseudo-tool provider was reached: {attempt:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// Challenge 9 — the catalog never advertises the same model twice
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_challenge_catalog_never_lists_the_same_model_twice() {
    let db = Arc::new(Db::open_in_memory().unwrap());
    db.migrate().unwrap();

    // A combo whose name collides with a model id of the catalog, and one
    // that does not.
    {
        let guard = db.connection();
        for (name, id) in [("auto", "combo-auto"), ("m5-non-colliding", "combo-plain")] {
            heterion_router_db::repos::upsert_combo(
                &guard,
                &heterion_router_db::repos::ComboRow {
                    id: id.into(),
                    name: name.into(),
                    data: json!({"isHidden": false}).to_string(),
                    sort_order: 10,
                    active: true,
                },
            )
            .unwrap();
        }
    }

    let ledger = Ledger::default();
    let (backends, _) = setup_backends(&ledger, &["gemini"]);
    let routing = Arc::new(RoutingBackend::new(backends, Router::new(vec![], vec![])));
    let router = build_router_with_state(AppState::with_db(routing, db));

    let request = Request::builder()
        .method("GET")
        .uri("/v1/models")
        .body(Body::empty())
        .unwrap();
    let response = router.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let catalog: Value = serde_json::from_slice(&bytes).unwrap();
    let data = catalog["data"].as_array().expect("data must be an array");
    assert!(!data.is_empty(), "the catalog must not be empty");

    let ids: Vec<&str> = data
        .iter()
        .filter_map(|entry| entry["id"].as_str())
        .collect();
    let mut unique = ids.clone();
    unique.sort_unstable();
    unique.dedup();
    assert_eq!(
        ids.len(),
        unique.len(),
        "the catalog must not advertise the same id twice: {ids:?}"
    );

    for auto_id in ["heterion-router/auto", "auto"] {
        assert_eq!(
            ids.iter().filter(|id| **id == auto_id).count(),
            1,
            "{auto_id} must be advertised exactly once"
        );
    }

    // The combo listing really ran: a combo that does not collide is
    // advertised, so the collision above was removed by the dedup and not by
    // the combos being dropped from the catalog altogether.
    assert!(
        ids.contains(&"m5-non-colliding"),
        "a combo that does not collide must be advertised, got {ids:?}"
    );
}

// ---------------------------------------------------------------------------
// Challenge 8 — pruning costs no upstream quota and no extra LLM call
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_challenge_zero_quota_pruning_makes_no_extra_upstream_call() {
    let ledger = Ledger::default();
    let (backends, probes) = setup_backends(&ledger, &["gemini", "agentrouter", "mistral"]);
    let routing = RoutingBackend::new(backends, Router::new(vec![], vec![]));

    let request = large_history_request();
    let original_ids = tool_call_ids(&request);
    let original_bytes = content_bytes(&request);
    assert!(
        original_bytes > 1_000_000,
        "the fixture must be big enough to need pruning, got {original_bytes} bytes"
    );

    // The plan the router builds itself decides which candidate has to fail
    // before the Tier-2 candidate with the 256k window is reached.
    let plan = routing.auto_plan_at(&request, Instant::now());
    let target_index = plan
        .steps
        .iter()
        .position(|(_, model)| model == "codestral-latest")
        .expect("the Tier-2 candidate must be part of the plan");
    for (provider, _) in plan.steps.iter().take(target_index) {
        probes[provider].push_response(Err(upstream_502()));
    }
    probes["mistral"].push_response(Ok(text_response(
        "codestral-latest",
        "served with a compressed prompt",
    )));

    let mut request = request;
    let token = stamp_attribution(&mut request);
    let response = routing
        .complete(request)
        .await
        .expect("the pruned candidate must serve the request");
    assert_eq!(response.model, "codestral-latest");

    // Zero quota: one upstream call per candidate attempt, and not one call
    // more. A summarising LLM would show up as an extra attempt here.
    let expected: Vec<Attempt> = plan
        .steps
        .iter()
        .take(target_index + 1)
        .map(|(provider, model)| Attempt {
            provider: provider.clone(),
            model: model.clone(),
        })
        .collect();
    assert_eq!(
        ledger.attempts(),
        expected,
        "pruning must not cost an extra upstream call"
    );

    // The compressed prompt is the one that reached the upstream.
    let served = probes["mistral"].last_request();
    let served = served.expect("the Tier-2 candidate must have been called");
    assert_eq!(served.model, "codestral-latest");
    assert!(
        content_bytes(&served) * 10 < original_bytes,
        "the prompt must have been compressed, {} bytes instead of {original_bytes}",
        content_bytes(&served)
    );
    assert!(
        served
            .messages
            .iter()
            .any(|message| message.text().contains("[Tool Output Digest")),
        "the folded tool outputs must be replaced by structured digests"
    );

    // The compression keeps the contract: 1-to-1 `tool_call_id` parity and
    // the active tail verbatim.
    assert_eq!(
        tool_call_ids(&served),
        original_ids,
        "the compressed request must keep the tool_call_id parity"
    );
    assert_eq!(
        served.messages.len(),
        26,
        "the compression folds the content, it does not drop messages"
    );

    let attribution = routing.attribution(&token);
    assert_eq!(attribution.model.as_deref(), Some("codestral-latest"));
    assert!(
        attribution.tokens_compressed.unwrap_or(0) > 0,
        "the attribution must report the compressed tokens"
    );
}

// ---------------------------------------------------------------------------
// Challenge 10 — KNOWN DEFECT (reported, not fixed): the `Retry-After` header
// of a real upstream never reaches the cooldown
// ---------------------------------------------------------------------------

/// A backend that really talks to an HTTP upstream, the way the
/// OpenAI-format executor of the gateway does
/// (`crates/heterion-router-gateway/src/openai.rs:190`:
///
/// ```text
/// .map_err(|error| GatewayError::Upstream(error.to_string()))?
/// ```
/// ).
struct HttpProbeBackend {
    url: String,
    client: heterion_router_http::client::HttpClient,
}

#[async_trait]
impl ChatBackend for HttpProbeBackend {
    fn name(&self) -> &'static str {
        "gemini"
    }

    async fn complete(
        &self,
        request: ChatCompletionRequest,
    ) -> Result<ChatCompletionResponse, GatewayError> {
        let body = json!({
            "model": request.model,
            "messages": [{"role": "user", "content": "hello"}],
        });
        self.client
            .post_json(&self.url, None, &body)
            .await
            .map_err(|error| GatewayError::Upstream(error.to_string()))?;
        Ok(text_response(&request.model, "never reached"))
    }

    fn stream(&self, _request: ChatCompletionRequest) -> ChunkStream {
        Box::pin(futures::stream::empty())
    }
}

/// The `Retry-After` header of a real upstream must set the length of the
/// cooldown — ORIGINAL_REQUEST §R4 ("При 429: учёт `Retry-After`") and the
/// Interface Contracts of `PROJECT.md` ("`Retry-After` header or exponential
/// backoff").
///
/// The defect this test was born as a marker for (reported in
/// `.agents/teamwork/worker_m5_1/handoff.md`) is **fixed**: the structured
/// `retry_after` of `HttpError::Status` used to be dropped by its `Display`
/// (`crates/heterion-router-http/src/client.rs`), the executor turned the
/// error into a plain message, and the router reads the cooldown length back
/// out of that message
/// (`crates/heterion-router-gateway/src/routing.rs` →
/// `crates/heterion-router-gateway/src/resilience.rs`). The header is now
/// carried in the message, so the cooldown is the 300 s the provider asked
/// for — not the 30 s first backoff step.
#[tokio::test]
async fn test_the_retry_after_header_of_a_real_upstream_reaches_the_cooldown() {
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{method, path},
    };

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(
            ResponseTemplate::new(429)
                .insert_header("retry-after", "300")
                .set_body_json(json!({"error": {"message": "slow down"}})),
        )
        .mount(&server)
        .await;

    let probe = Arc::new(HttpProbeBackend {
        url: format!("{}/v1/chat/completions", server.uri()),
        client: heterion_router_http::client::HttpClient::new(10).unwrap(),
    });
    let mut backends: HashMap<String, Arc<dyn ChatBackend>> = HashMap::new();
    backends.insert("gemini".into(), probe);
    let routing = RoutingBackend::new(backends, Router::new(vec![], vec![]));

    // The upstream answered 429 — the request must fail over to the next
    // candidate, and the rate-limited model must land in a cooldown.
    let error = routing
        .complete(auto_request())
        .await
        .expect_err("the only upstream is rate limited");
    assert!(
        error.to_string().contains("429"),
        "the failure must be the rate limit, got: {error}"
    );

    let remaining = routing
        .cooldowns()
        .get_cooldown_remaining("gemini", Some("gemini-3-flash-preview"))
        .expect("the rate-limited model must be in cooldown");
    assert!(
        remaining >= Duration::from_secs(295),
        "the upstream asked for `Retry-After: 300`, but the cooldown lasts only \
         {remaining:?}: the header must reach the router"
    );
    assert!(
        remaining > Duration::from_secs(120),
        "the cooldown must not fall back to the exponential backoff step \
         (30s -> 60s -> 120s): {remaining:?}"
    );

    // The upstream really was called with the header in place.
    server.verify().await;
}

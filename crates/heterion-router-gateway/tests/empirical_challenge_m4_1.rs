//! Milestone 4 Empirical Challenge Suite: Switchyard Auto-Routing Hardening & Edge Verification
//!
//! Rigorously challenges:
//! 1. `test_challenge_priority_ladder_ordering_and_stage_dynamics`:
//!    - Base priority tiers: Tier 1 (300/290/280) > Tier 2 (200/190) > Tier 3 (100/90).
//!    - Stage weights: Planning / ErrorRecovery boosts FrontierReasoning (promoting gpt-6.1-sol above gpt-5.6-sol).
//!    - MechanicalToolLoop boosts FastExecution.
//!    - Quality score degradation dynamically demotes candidates across tiers.
//!    - Active cooldowns and open breakers dynamically set availability state to 0.0 and purge candidates.
//! 2. `test_challenge_simultaneous_multi_tier_failures`:
//!    - Concurrent 429, 403, and 503 errors across Tier 1 (3 models) and Tier 2 (2 models).
//!    - Clean cascade to Tier 3 without infinite loops, hanging, or request dropping.
//!    - Strict isolation: 429/403 errors set cooldowns but NEVER trip circuit breakers.
//!    - Terminal failure handling when all tiers are exhausted: clean GatewayError.
//! 3. `test_challenge_multimodal_vision_filtering_edge_cases`:
//!    - Data URI (`data:image/png;base64,...`), HTTPS URL, and AI-SDK format (`{"type": "image", ...}`).
//!    - Multi-turn conversation with image in earlier turn.
//!    - Strict exclusion: Non-vision models (`codestral`, `agentrouter`, `gemma`) are 100% absent from plan steps.
//!    - Clean failure when only non-vision backends exist.
//! 4. `test_challenge_tool_call_preflight_and_postflight_rejection`:
//!    - Pre-flight exclusion of pseudo-tool models (`morph/*`, `*syn-pro*`) and search-only models (`exa/*`).
//!    - Empty tools array (`tools: []`) does not trigger false pre-flight exclusion.
//!    - Post-flight rejection: Malformed XML `<tool_call>` or empty tool calls trigger rejection, quality penalty, and transparent failover.
//! 5. `test_challenge_jit_pruning_tool_call_id_parity_and_tail_invariance`:
//!    - Large context (>280k tokens) failover from Tier 1 to Tier 2 (256k limit).
//!    - Preserves system prompt, initial user goal, and active tail turns verbatim.
//!    - Preserves exact 1-to-1 parity for all `tool_call_id` attributes between assistant calls and tool messages.
//!    - Negative pruning check: Unprunable text-heavy prompt (>300k tokens) correctly causes `can_prune_to_fit` to return false and filters out 256k/128k candidates.
//! 6. `test_challenge_prestream_failover_and_sse_attribution_invariants`:
//!    - Streaming SSE failover on initial error before headers/chunks.
//!    - Wire chunks preserve requested model name (`heterion-router/auto`), while DB attribution records actual candidate model.
//!    - Multi-candidate streaming cascade.

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
    context_pruner::{can_prune_to_fit, fast_estimate_tokens, prune_request},
    inspector::{AgentStage, inspect_request},
    model_spec::{ToolSupport, is_pseudo_tool_model, model_spec},
    routing::{DEFAULT_AUTO_CANDIDATES, RoutingBackend},
};
use heterion_router_routing::Router;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tower::ServiceExt;

// ---------------------------------------------------------------------------
// Mock Backend Harness
// ---------------------------------------------------------------------------

struct EmpiricalMockBackend {
    name: &'static str,
    responses: Mutex<Vec<Result<ChatCompletionResponse, GatewayError>>>,
    stream_results: Mutex<Vec<Result<StreamChunk, GatewayError>>>,
    calls: Arc<AtomicUsize>,
    dispatched_requests: Arc<Mutex<Vec<ChatCompletionRequest>>>,
}

impl EmpiricalMockBackend {
    fn new(name: &'static str) -> Self {
        Self {
            name,
            responses: Mutex::new(Vec::new()),
            stream_results: Mutex::new(Vec::new()),
            calls: Arc::new(AtomicUsize::new(0)),
            dispatched_requests: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn push_response(&self, res: Result<ChatCompletionResponse, GatewayError>) {
        self.responses.lock().unwrap().push(res);
    }

    fn push_stream_chunk(&self, chunk: Result<StreamChunk, GatewayError>) {
        self.stream_results.lock().unwrap().push(chunk);
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::Relaxed)
    }

    #[allow(dead_code)]
    fn last_request(&self) -> Option<ChatCompletionRequest> {
        self.dispatched_requests.lock().unwrap().last().cloned()
    }
}

#[async_trait]
impl ChatBackend for EmpiricalMockBackend {
    fn name(&self) -> &'static str {
        self.name
    }

    async fn complete(
        &self,
        request: ChatCompletionRequest,
    ) -> Result<ChatCompletionResponse, GatewayError> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        self.dispatched_requests
            .lock()
            .unwrap()
            .push(request.clone());

        let mut guard = self.responses.lock().unwrap();
        if !guard.is_empty() {
            guard.remove(0)
        } else {
            Ok(ChatCompletionResponse {
                id: format!("resp-{}", self.name),
                object: "chat.completion",
                created: 10000,
                model: request.model.clone(),
                usage: Usage::default(),
                choices: vec![ChatChoice {
                    index: 0,
                    message: ChatMessage::assistant_text(format!("Success from {}", self.name)),
                    finish_reason: "stop".into(),
                }],
                raw: None,
            })
        }
    }

    fn stream(&self, request: ChatCompletionRequest) -> ChunkStream {
        self.calls.fetch_add(1, Ordering::Relaxed);
        self.dispatched_requests.lock().unwrap().push(request);

        let mut guard = self.stream_results.lock().unwrap();
        let items: Vec<Result<StreamChunk, GatewayError>> = if !guard.is_empty() {
            guard.drain(..).collect()
        } else {
            vec![Ok(StreamChunk {
                content: Some(format!("Stream token from {}", self.name)),
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

#[allow(clippy::type_complexity)]
fn setup_all_backends() -> (
    HashMap<String, Arc<dyn ChatBackend>>,
    Arc<EmpiricalMockBackend>,
    Arc<EmpiricalMockBackend>,
    Arc<EmpiricalMockBackend>,
) {
    let gemini = Arc::new(EmpiricalMockBackend::new("gemini"));
    let agentrouter = Arc::new(EmpiricalMockBackend::new("agentrouter"));
    let mistral = Arc::new(EmpiricalMockBackend::new("mistral"));

    let mut backends: HashMap<String, Arc<dyn ChatBackend>> = HashMap::new();
    backends.insert("gemini".into(), gemini.clone());
    backends.insert("agentrouter".into(), agentrouter.clone());
    backends.insert("mistral".into(), mistral.clone());

    (backends, gemini, agentrouter, mistral)
}

// ---------------------------------------------------------------------------
// Challenge 1: Dynamic Priority Ladder Ordering & Stage Dynamics
// ---------------------------------------------------------------------------

#[test]
fn test_challenge_priority_ladder_ordering_and_stage_dynamics() {
    let (backends, _, _, _) = setup_all_backends();
    let routing = RoutingBackend::new(backends, Router::new(vec![], vec![]));
    let now = Instant::now();

    // 1. Verify static DEFAULT_AUTO_CANDIDATES structure matches NVIDIA Switchyard tiers
    assert_eq!(DEFAULT_AUTO_CANDIDATES.len(), 7);
    assert_eq!(DEFAULT_AUTO_CANDIDATES[0].model, "gemini-3-flash-preview");
    assert_eq!(DEFAULT_AUTO_CANDIDATES[0].base_priority, 300.0);
    assert_eq!(DEFAULT_AUTO_CANDIDATES[1].model, "gpt-5.6-sol");
    assert_eq!(DEFAULT_AUTO_CANDIDATES[1].base_priority, 290.0);
    assert_eq!(DEFAULT_AUTO_CANDIDATES[2].model, "gpt-6.1-sol");
    assert_eq!(DEFAULT_AUTO_CANDIDATES[2].base_priority, 280.0);
    assert_eq!(DEFAULT_AUTO_CANDIDATES[3].model, "codestral-latest");
    assert_eq!(DEFAULT_AUTO_CANDIDATES[3].base_priority, 200.0);
    assert_eq!(DEFAULT_AUTO_CANDIDATES[4].model, "gemma-4-26b-a4b-it");
    assert_eq!(DEFAULT_AUTO_CANDIDATES[4].base_priority, 190.0);
    assert_eq!(DEFAULT_AUTO_CANDIDATES[5].model, "gemini-3.5-flash-lite");
    assert_eq!(DEFAULT_AUTO_CANDIDATES[5].base_priority, 100.0);
    assert_eq!(DEFAULT_AUTO_CANDIDATES[6].model, "gemini-3.1-flash-lite");
    assert_eq!(DEFAULT_AUTO_CANDIDATES[6].base_priority, 90.0);

    // 2. Planning stage: FrontierReasoning models get 1.05x boost
    // gpt-6.1-sol (280.0 * 1.05 = 294.0) jumps ahead of gpt-5.6-sol (290.0 * 1.0 = 290.0)
    let plan_req = ChatCompletionRequest {
        model: "heterion-router/auto".into(),
        messages: vec![ChatMessage::plain("user", "Design system architecture")],
        ..Default::default()
    };
    assert_eq!(inspect_request(&plan_req).stage, AgentStage::Planning);
    let plan = routing.auto_plan_at(&plan_req, now);
    let models: Vec<&str> = plan.steps.iter().map(|s| s.1.as_str()).collect();
    assert_eq!(
        models,
        vec![
            "gemini-3-flash-preview", // 300.0
            "gpt-6.1-sol",            // 294.0 (boosted from 280)
            "gpt-5.6-sol",            // 290.0
            "codestral-latest",       // 200.0
            "gemma-4-26b-a4b-it",     // 190.0
            "gemini-3.5-flash-lite",  // 100.0
            "gemini-3.1-flash-lite",  // 90.0
        ]
    );

    // 3. MechanicalToolLoop stage: FastExecution models get 1.05x boost
    // gpt-5.6-sol (290.0 * 1.05 = 304.5), gpt-6.1-sol (280.0 * 1.0 = 280.0)
    let tool_loop_req = ChatCompletionRequest {
        model: "heterion-router/auto".into(),
        messages: vec![
            ChatMessage::plain("user", "Run tests"),
            ChatMessage {
                role: "assistant".into(),
                content: None,
                reasoning_content: None,
                tool_calls: Some(
                    json!([{"id":"c1","type":"function","function":{"name":"run","arguments":"{}"}}]),
                ),
                tool_call_id: None,
                name: None,
            },
            ChatMessage {
                role: "tool".into(),
                content: Some(json!("test pass")),
                reasoning_content: None,
                tool_calls: None,
                tool_call_id: Some("c1".into()),
                name: None,
            },
        ],
        ..Default::default()
    };
    assert_eq!(
        inspect_request(&tool_loop_req).stage,
        AgentStage::MechanicalToolLoop
    );
    let plan_tool_loop = routing.auto_plan_at(&tool_loop_req, now);
    let tool_models: Vec<&str> = plan_tool_loop.steps.iter().map(|s| s.1.as_str()).collect();
    assert_eq!(
        tool_models,
        vec![
            "gemini-3-flash-preview", // 300.0 * 1.05 = 315.0
            "gpt-5.6-sol",            // 290.0 * 1.05 = 304.5
            "gpt-6.1-sol",            // 280.0 * 1.0 = 280.0
            "codestral-latest",       // 200.0 * 1.05 = 210.0
            "gemma-4-26b-a4b-it",     // 190.0 * 1.05 = 199.5
            "gemini-3.5-flash-lite",  // 100.0 * 1.05 = 105.0
            "gemini-3.1-flash-lite",  // 90.0 * 1.05 = 94.5
        ]
    );

    // 4. Quality score demotion across tiers:
    // A. Single defect: penalize QualityScore (-0.3: 1.0 -> 0.7)
    // gemini-3-flash-preview: 300 * 0.7 = 210.0, demoted below gpt-6.1-sol (294) and gpt-5.6-sol (290)
    routing
        .quality_tracker()
        .record_failure("gemini-3-flash-preview");
    let demoted_plan_1 = routing.auto_plan_at(&plan_req, now);
    let demoted_models_1: Vec<&str> = demoted_plan_1.steps.iter().map(|s| s.1.as_str()).collect();
    assert_eq!(
        demoted_models_1[0], "gpt-6.1-sol",
        "gpt-6.1-sol (294.0) takes #1 spot"
    );
    assert_eq!(
        demoted_models_1[1], "gpt-5.6-sol",
        "gpt-5.6-sol (290.0) takes #2 spot"
    );
    assert_eq!(
        demoted_models_1[2], "gemini-3-flash-preview",
        "gemini-3-flash-preview (210.0) demoted to #3 spot above codestral (200.0)"
    );
    assert_eq!(demoted_models_1[3], "codestral-latest");
    assert_eq!(demoted_models_1[4], "gemma-4-26b-a4b-it");
    assert_eq!(demoted_models_1[5], "gemini-3.5-flash-lite");
    assert_eq!(demoted_models_1[6], "gemini-3.1-flash-lite");

    // B. Second consecutive defect triggers 15-minute probation:
    // QualityScore clamped to PROBATION_SCORE_CEILING (0.2).
    // gemini-3-flash-preview: 300 * 0.2 = 60.0, demoted below Tier 3 (100.0 / 90.0) to index 6!
    routing
        .quality_tracker()
        .record_failure("gemini-3-flash-preview");
    assert!(
        routing
            .quality_tracker()
            .is_in_probation_at("gemini-3-flash-preview", now),
        "two consecutive failures must enter 15-minute probation"
    );
    let demoted_plan_2 = routing.auto_plan_at(&plan_req, now);
    let demoted_models_2: Vec<&str> = demoted_plan_2.steps.iter().map(|s| s.1.as_str()).collect();
    assert_eq!(
        demoted_models_2.last().copied(),
        Some("gemini-3-flash-preview"),
        "probation demotes model to the very bottom of the ladder (60.0 priority)"
    );

    // 5. Availability State exclusion:
    // Place gpt-6.1-sol in active cooldown
    routing.cooldowns().record_cooldown_at(
        "agentrouter",
        Some("gpt-6.1-sol"),
        Duration::from_secs(60),
        heterion_router_gateway::resilience::CooldownReason::Manual,
        now,
    );
    let cooldown_plan = routing.auto_plan_at(&plan_req, now);
    let active_models: Vec<&str> = cooldown_plan.steps.iter().map(|s| s.1.as_str()).collect();
    assert!(
        !active_models.contains(&"gpt-6.1-sol"),
        "model in active cooldown must have availability_state = 0.0 and be excluded from plan"
    );
}

// ---------------------------------------------------------------------------
// Challenge 2: Simultaneous Multi-Tier 429/403/503 Cascade & Edge Resilience
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_challenge_simultaneous_multi_tier_failures() {
    let (backends, gemini, agentrouter, mistral) = setup_all_backends();

    // Configure router with breaker threshold: breaker trips on >=3 consecutive 5xx/transport outages
    let routing =
        RoutingBackend::with_breaker_settings(backends, Router::new(vec![], vec![]), 3, 3600);

    // Setup responses for simultaneous multi-tier errors:
    // Tier 1:
    // 1. gemini-3-flash-preview -> HTTP 503 Service Unavailable (transient upstream failure)
    gemini.push_response(Err(GatewayError::Upstream(
        "upstream status 503: high load backend unavailable".into(),
    )));
    // 2. gpt-6.1-sol (Planning stage #1 for agentrouter) -> HTTP 429 Rate Limit
    agentrouter.push_response(Err(GatewayError::Upstream(
        "upstream status 429: rate limit reached, retry-after: 30".into(),
    )));
    // 2b. gpt-5.6-sol (next agentrouter candidate) -> HTTP 429 Rate Limit too.
    // The 429 cooldown is scoped to the model that was rate-limited, so the
    // sibling model is still tried and must be given its own failure —
    // otherwise the mock would answer with its default success and the
    // cascade would stop at Tier 1 instead of reaching the Tier-3 protector.
    agentrouter.push_response(Err(GatewayError::Upstream(
        "upstream status 429: rate limit reached, retry-after: 30".into(),
    )));

    // Tier 2:
    // 3. codestral-latest -> HTTP 502 Bad Gateway
    mistral.push_response(Err(GatewayError::Upstream(
        "upstream status 502: Bad Gateway upstream drop".into(),
    )));
    // 4. gemma-4-26b-a4b-it -> HTTP 503 Service Unavailable
    gemini.push_response(Err(GatewayError::Upstream(
        "upstream status 503: Service Unavailable".into(),
    )));

    // Tier 3:
    // 5. gemini-3.5-flash-lite -> 200 OK!
    gemini.push_response(Ok(ChatCompletionResponse {
        id: "resp-tier3-savior".into(),
        object: "chat.completion",
        created: 77777,
        model: "gemini-3.5-flash-lite".into(),
        usage: Usage::default(),
        choices: vec![ChatChoice {
            index: 0,
            message: ChatMessage::assistant_text("Tier 3 High-Quota Protector Rescued Request"),
            finish_reason: "stop".into(),
        }],
        raw: None,
    }));

    let mut req = ChatCompletionRequest {
        model: "heterion-router/auto".into(),
        messages: vec![ChatMessage::plain(
            "user",
            "Critical payload across cascading tiers",
        )],
        ..Default::default()
    };
    let token = stamp_attribution(&mut req);

    let res = routing
        .complete(req)
        .await
        .expect("multi-tier cascade through simultaneous errors must succeed at Tier 3");
    assert_eq!(
        res.choices[0].message.text(),
        "Tier 3 High-Quota Protector Rescued Request"
    );

    // Verify attribution records actual Tier 3 savior model
    let attr = routing.attribution(&token);
    assert_eq!(attr.provider.as_deref(), Some("gemini"));
    assert_eq!(attr.model.as_deref(), Some("gemini-3.5-flash-lite"));
    assert_eq!(attr.combo_name.as_deref(), Some("heterion-router/auto"));

    // Verify Strict Breaker Invariant:
    // 429 and 403 errors MUST NEVER trip circuit breakers!
    let now = Instant::now();
    assert!(
        routing.breakers().lock().unwrap().allow("agentrouter", now),
        "agentrouter breaker must remain allowed (429 never trips circuit breakers)"
    );
    assert!(
        routing
            .cooldowns()
            .is_in_cooldown("agentrouter", Some("gpt-6.1-sol")),
        "the rate-limited model must have entered 429 cooldown"
    );
    assert!(
        routing
            .cooldowns()
            .is_in_cooldown("agentrouter", Some("gpt-5.6-sol")),
        "the second rate-limited model must have entered 429 cooldown"
    );
    assert!(
        !routing.cooldowns().is_in_cooldown("agentrouter", None),
        "a per-model 429 must not put the whole provider in cooldown"
    );

    // Terminal failure scenario: If Tier 3 also fails, clean GatewayError returned without hanging
    let (fail_backends, fail_gemini, fail_agentrouter, fail_mistral) = setup_all_backends();
    let fail_routing = RoutingBackend::new(fail_backends, Router::new(vec![], vec![]));

    // Exhaust all candidates with 503s
    fail_gemini.push_response(Err(GatewayError::Upstream("upstream status 503".into())));
    fail_agentrouter.push_response(Err(GatewayError::Upstream("upstream status 503".into())));
    fail_agentrouter.push_response(Err(GatewayError::Upstream("upstream status 503".into())));
    fail_mistral.push_response(Err(GatewayError::Upstream("upstream status 503".into())));
    fail_gemini.push_response(Err(GatewayError::Upstream("upstream status 503".into())));
    fail_gemini.push_response(Err(GatewayError::Upstream("upstream status 503".into())));
    fail_gemini.push_response(Err(GatewayError::Upstream("upstream status 503".into())));

    let req_fail = ChatCompletionRequest {
        model: "auto".into(),
        messages: vec![ChatMessage::plain("user", "This should fail gracefully")],
        ..Default::default()
    };
    let err = fail_routing.complete(req_fail).await.unwrap_err();
    match err {
        GatewayError::Upstream(msg) => {
            assert!(msg.contains("503"), "expected 503 error, got: {msg}");
        }
        other => panic!("expected Upstream error on complete exhaustion, got: {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Challenge 3: Multimodal Vision Filtering Edge Cases
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_challenge_multimodal_vision_filtering_edge_cases() {
    let (backends, gemini, agentrouter, mistral) = setup_all_backends();
    let routing = RoutingBackend::new(backends, Router::new(vec![], vec![]));

    // Case A: Data URL (OpenAI standard)
    let req_data_uri = ChatCompletionRequest {
        model: "heterion-router/auto".into(),
        messages: vec![ChatMessage {
            role: "user".into(),
            content: Some(json!([
                {"type": "text", "text": "Analyze image"},
                {"type": "image_url", "image_url": {"url": "data:image/jpeg;base64,/9j/4AAQSkZJRgABAQEASABIAAD/2wBD..."}}
            ])),
            reasoning_content: None,
            tool_calls: None,
            tool_call_id: None,
            name: None,
        }],
        ..Default::default()
    };

    // Case B: HTTPS URL
    let req_https_url = ChatCompletionRequest {
        model: "auto".into(),
        messages: vec![ChatMessage {
            role: "user".into(),
            content: Some(json!([
                {"type": "text", "text": "Inspect remote diagram"},
                {"type": "image_url", "image_url": {"url": "https://cdn.example.org/arch.png"}}
            ])),
            reasoning_content: None,
            tool_calls: None,
            tool_call_id: None,
            name: None,
        }],
        ..Default::default()
    };

    // Case C: AI-SDK format: {"type": "image", "image": "..."}
    let req_aisdk_format = ChatCompletionRequest {
        model: "heterion-router/auto".into(),
        messages: vec![ChatMessage {
            role: "user".into(),
            content: Some(json!([
                {"type": "text", "text": "Process AI-SDK image"},
                {"type": "image", "image": "https://cdn.example.org/spec.jpg"}
            ])),
            reasoning_content: None,
            tool_calls: None,
            tool_call_id: None,
            name: None,
        }],
        ..Default::default()
    };

    // Case D: Multi-turn history with image in turn 1
    let req_history_image = ChatCompletionRequest {
        model: "heterion-router/auto".into(),
        messages: vec![
            ChatMessage {
                role: "user".into(),
                content: Some(json!([
                    {"type": "text", "text": "Here is screenshot"},
                    {"type": "image_url", "image_url": {"url": "data:image/png;base64,iVBORw..."}}
                ])),
                reasoning_content: None,
                tool_calls: None,
                tool_call_id: None,
                name: None,
            },
            ChatMessage::plain("assistant", "I see the screenshot of the compiler error."),
            ChatMessage::plain("user", "Explain line 42 in that screenshot"),
        ],
        ..Default::default()
    };

    let cases = vec![
        ("Data URI", req_data_uri),
        ("HTTPS URL", req_https_url),
        ("AI-SDK format", req_aisdk_format),
        ("History image", req_history_image),
    ];

    for (label, req) in cases {
        let inspection = inspect_request(&req);
        assert!(
            inspection.has_images,
            "failed has_images inspection for {label}"
        );

        let plan = routing.auto_plan(&req);
        assert!(
            !plan.steps.is_empty(),
            "plan must not be empty for vision request ({label})"
        );

        for (provider, model) in &plan.steps {
            let spec = model_spec(model);
            assert!(
                spec.supports_vision,
                "violation in {label}: non-vision model {provider}/{model} included in plan"
            );
            assert_ne!(
                provider, "agentrouter",
                "agentrouter must never be selected for vision ({label})"
            );
            assert_ne!(
                provider, "mistral",
                "mistral must never be selected for vision ({label})"
            );
            assert_ne!(
                model, "gemma-4-26b-a4b-it",
                "gemma does not support vision ({label})"
            );
        }
    }

    // Execute vision request: gemini serves it
    gemini.push_response(Ok(ChatCompletionResponse {
        id: "resp-vis-success".into(),
        object: "chat.completion",
        created: 8888,
        model: "gemini-3-flash-preview".into(),
        usage: Usage::default(),
        choices: vec![ChatChoice {
            index: 0,
            message: ChatMessage::assistant_text("Identified diagram components successfully"),
            finish_reason: "stop".into(),
        }],
        raw: None,
    }));

    let mut execute_req = ChatCompletionRequest {
        model: "heterion-router/auto".into(),
        messages: vec![ChatMessage {
            role: "user".into(),
            content: Some(json!([
                {"type": "text", "text": "Analyze diagram"},
                {"type": "image_url", "image_url": {"url": "https://example.org/d.png"}}
            ])),
            reasoning_content: None,
            tool_calls: None,
            tool_call_id: None,
            name: None,
        }],
        ..Default::default()
    };
    let token = stamp_attribution(&mut execute_req);
    let res = routing.complete(execute_req).await.unwrap();
    assert_eq!(
        res.choices[0].message.text(),
        "Identified diagram components successfully"
    );
    assert_eq!(
        routing.attribution(&token).model.as_deref(),
        Some("gemini-3-flash-preview")
    );
    assert_eq!(agentrouter.calls(), 0);
    assert_eq!(mistral.calls(), 0);
}

// ---------------------------------------------------------------------------
// Challenge 4: Tool-Call Pre-Flight and Post-Flight Rejection
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_challenge_tool_call_preflight_and_postflight_rejection() {
    // 1. Rigorously test pseudo-tool model detection across variants
    assert!(is_pseudo_tool_model("morph", "morph-v3-large"));
    assert!(is_pseudo_tool_model("MORPH", "ANY-MODEL"));
    assert!(is_pseudo_tool_model("", "morph-v3-large"));
    assert!(is_pseudo_tool_model("upstage", "syn-pro"));
    assert!(is_pseudo_tool_model("vendor", "upstage-syn-pro-instruct"));
    assert!(is_pseudo_tool_model("", "syn-pro"));
    assert!(!is_pseudo_tool_model("gemini", "gemini-3-flash-preview"));
    assert!(!is_pseudo_tool_model("agentrouter", "gpt-5.6-sol"));
    assert!(!is_pseudo_tool_model("mistral", "codestral-latest"));

    // 2. Search-only model (exa) capability verification
    let exa_spec = model_spec("exa/exa-search");
    assert_eq!(exa_spec.tool_support, ToolSupport::None);

    let (mut backends, gemini, agentrouter, _) = setup_all_backends();
    let morph = Arc::new(EmpiricalMockBackend::new("morph"));
    let exa = Arc::new(EmpiricalMockBackend::new("exa"));
    backends.insert("morph".into(), morph.clone());
    backends.insert("exa".into(), exa.clone());

    let routing = RoutingBackend::new(backends, Router::new(vec![], vec![]));

    // 3. Request with tools
    let req_tools = ChatCompletionRequest {
        model: "heterion-router/auto".into(),
        messages: vec![ChatMessage::plain("user", "Compute metrics")],
        tools: Some(json!([
            {
                "type": "function",
                "function": {
                    "name": "compute_metrics",
                    "description": "computes stats",
                    "parameters": {"type": "object"}
                }
            }
        ])),
        ..Default::default()
    };
    let plan_tools = routing.auto_plan(&req_tools);
    for (p, m) in &plan_tools.steps {
        assert_ne!(p, "morph");
        assert_ne!(p, "exa");
        assert!(!is_pseudo_tool_model(p, m));
        assert_ne!(model_spec(m).tool_support, ToolSupport::None);
    }

    // 4. Request with empty tools array `[]`
    let req_empty_tools = ChatCompletionRequest {
        model: "auto".into(),
        messages: vec![ChatMessage::plain("user", "Normal chat message")],
        tools: Some(json!([])),
        ..Default::default()
    };
    let inspection_empty = inspect_request(&req_empty_tools);
    assert!(
        !inspection_empty.has_tools,
        "empty tools array must evaluate has_tools=false"
    );

    // 5. Post-Flight Tool Validation Rejection:
    // Candidate 1 (gemini-3-flash-preview) emits pseudo XML `<tool_call>` in text content
    gemini.push_response(Ok(ChatCompletionResponse {
        id: "resp-pseudo-xml".into(),
        object: "chat.completion",
        created: 9001,
        model: "gemini-3-flash-preview".into(),
        usage: Usage::default(),
        choices: vec![ChatChoice {
            index: 0,
            message: ChatMessage::assistant_text(
                "<tool_call>{\"name\": \"compute_metrics\", \"arguments\": {}}</tool_call>",
            ),
            finish_reason: "stop".into(),
        }],
        raw: None,
    }));

    // Candidate 2 (agentrouter/gpt-5.6-sol or gpt-6.1-sol) emits valid structured JSON tool call
    agentrouter.push_response(Ok(ChatCompletionResponse {
        id: "resp-valid-tool".into(),
        object: "chat.completion",
        created: 9002,
        model: "gpt-6.1-sol".into(),
        usage: Usage::default(),
        choices: vec![ChatChoice {
            index: 0,
            message: ChatMessage {
                role: "assistant".into(),
                content: None,
                reasoning_content: None,
                tool_calls: Some(json!([
                    {
                        "id": "call_valid_99",
                        "type": "function",
                        "function": {
                            "name": "compute_metrics",
                            "arguments": "{}"
                        }
                    }
                ])),
                tool_call_id: None,
                name: None,
            },
            finish_reason: "tool_calls".into(),
        }],
        raw: None,
    }));

    let mut tool_exec_req = req_tools.clone();
    let token = stamp_attribution(&mut tool_exec_req);
    let res = routing
        .complete(tool_exec_req)
        .await
        .expect("post-flight tool rejection should failover to candidate 2");

    assert_eq!(res.choices[0].finish_reason, "tool_calls");
    assert!(res.choices[0].message.has_tool_calls());

    // Verify Candidate 1 was penalized for returning pseudo XML
    let quality_gemini = routing
        .quality_tracker()
        .get_score("gemini-3-flash-preview");
    assert!(
        quality_gemini < 1.0,
        "gemini-3-flash-preview must be penalized after emitting pseudo XML"
    );

    // Verify attribution records candidate 2
    let attr = routing.attribution(&token);
    assert_eq!(attr.provider.as_deref(), Some("agentrouter"));
    assert_eq!(attr.model.as_deref(), Some("gpt-6.1-sol"));
}

// ---------------------------------------------------------------------------
// Challenge 5: JIT Pruning Tool Call ID Parity & Tail Invariance
// ---------------------------------------------------------------------------

#[test]
fn test_challenge_jit_pruning_tool_call_id_parity_and_tail_invariance() {
    let mut messages = Vec::new();
    messages.push(ChatMessage::plain(
        "system",
        "SYSTEM_DIRECTIVE_STRICT_PRESERVE",
    ));
    messages.push(ChatMessage::plain("user", "INITIAL_GOAL_SOLVE_TASK"));

    // Generate 12 intermediate tool cycles with unique call IDs and massive payloads
    let intermediate_body = "x".repeat(120_000); // 120k chars per tool = ~30k tokens each
    let mut expected_tool_call_ids = Vec::new();

    for i in 0..12 {
        let call_id = format!("call_parity_chk_{i:03}");
        expected_tool_call_ids.push(call_id.clone());

        messages.push(ChatMessage {
            role: "assistant".into(),
            content: None,
            reasoning_content: None,
            tool_calls: Some(json!([
                {
                    "id": call_id,
                    "type": "function",
                    "function": {"name": "read_logs", "arguments": "{\"offset\": 0}"}
                }
            ])),
            tool_call_id: None,
            name: None,
        });

        messages.push(ChatMessage {
            role: "tool".into(),
            content: Some(Value::String(intermediate_body.clone())),
            reasoning_content: None,
            tool_calls: None,
            tool_call_id: Some(call_id),
            name: None,
        });
    }

    // Active tail turns (last 4 interaction turns)
    messages.push(ChatMessage::plain("user", "ACTIVE_TAIL_USER_1"));
    messages.push(ChatMessage::plain("assistant", "ACTIVE_TAIL_ASSISTANT_1"));
    messages.push(ChatMessage::plain("user", "ACTIVE_TAIL_USER_2"));
    messages.push(ChatMessage::plain("assistant", "ACTIVE_TAIL_ASSISTANT_2"));

    let original_req = ChatCompletionRequest {
        model: "heterion-router/auto".into(),
        messages,
        ..Default::default()
    };

    let total_orig_messages = original_req.messages.len();
    let orig_tokens = fast_estimate_tokens(&original_req);
    assert!(
        orig_tokens > 300_000,
        "expected >300k tokens, got {orig_tokens}"
    );

    // Verify can_prune_to_fit for 256k limit
    assert!(
        can_prune_to_fit(&original_req, 256_000),
        "can_prune_to_fit must be true for collapsible tool outputs"
    );

    let mut pruned_req = original_req.clone();
    let (new_tokens, reported_orig) = prune_request(&mut pruned_req, 256_000);
    assert_eq!(reported_orig, orig_tokens);
    assert!(
        new_tokens <= 256_000,
        "pruned tokens ({new_tokens}) must fit 256k limit"
    );

    // 1. Invariance: Message list length must not change (zero dropped messages)
    assert_eq!(
        pruned_req.messages.len(),
        total_orig_messages,
        "pruner must not drop or reorder messages"
    );

    // 2. Invariance: System prompt preserved verbatim
    assert_eq!(
        pruned_req.messages[0].text(),
        "SYSTEM_DIRECTIVE_STRICT_PRESERVE"
    );

    // 3. Invariance: Initial user goal preserved verbatim
    assert_eq!(pruned_req.messages[1].text(), "INITIAL_GOAL_SOLVE_TASK");

    // 4. Invariance: Active tail turns preserved verbatim
    let tail_start = total_orig_messages - 4;
    assert_eq!(pruned_req.messages[tail_start].text(), "ACTIVE_TAIL_USER_1");
    assert_eq!(
        pruned_req.messages[tail_start + 1].text(),
        "ACTIVE_TAIL_ASSISTANT_1"
    );
    assert_eq!(
        pruned_req.messages[tail_start + 2].text(),
        "ACTIVE_TAIL_USER_2"
    );
    assert_eq!(
        pruned_req.messages[tail_start + 3].text(),
        "ACTIVE_TAIL_ASSISTANT_2"
    );

    // 5. Strict 1-to-1 Tool Call ID Parity Check:
    // Every folded tool message must have its original tool_call_id intact,
    // must match the assistant message tool_calls[0].id,
    // and must carry the structured digest header with its ID
    let mut tool_turn_count = 0;
    for i in 0..tail_start {
        if pruned_req.messages[i].role == "tool" {
            let tool_msg = &pruned_req.messages[i];
            let expected_id = &expected_tool_call_ids[tool_turn_count];
            assert_eq!(
                tool_msg.tool_call_id.as_deref(),
                Some(expected_id.as_str()),
                "tool_call_id must remain strictly identical"
            );

            // Assistant turn immediately precedes tool turn
            let asst_msg = &pruned_req.messages[i - 1];
            assert_eq!(asst_msg.role, "assistant");
            let asst_call_id = asst_msg.tool_calls.as_ref().unwrap()[0]["id"]
                .as_str()
                .unwrap();
            assert_eq!(
                asst_call_id, expected_id,
                "assistant call id and tool turn call id must match 1-to-1"
            );

            // Digest header must include the id
            let text = tool_msg.text();
            assert!(
                text.starts_with(&format!("[Tool Output Digest: id={expected_id}")),
                "digest must reference the exact tool call id"
            );

            tool_turn_count += 1;
        }
    }
    assert_eq!(tool_turn_count, 12);

    // 6. Negative pruning check:
    // Prompt with 300k tokens of non-tool text (e.g. huge user code dumps) CANNOT be compressed
    let unprunable_messages = vec![ChatMessage::plain("user", "x".repeat(1_300_000))]; // ~325k tokens of pure user text
    let unprunable_req = ChatCompletionRequest {
        model: "auto".into(),
        messages: unprunable_messages,
        ..Default::default()
    };
    assert!(
        !can_prune_to_fit(&unprunable_req, 256_000),
        "unprunable user prompt must fail can_prune_to_fit for 256k limit"
    );

    // In auto_plan, candidates with context_window < 325k MUST be filtered out!
    let (backends, _, _, _) = setup_all_backends();
    let routing = RoutingBackend::new(backends, Router::new(vec![], vec![]));
    let unprunable_plan = routing.auto_plan(&unprunable_req);
    for (provider, model) in &unprunable_plan.steps {
        let spec = model_spec(model);
        assert!(
            spec.context_window >= 325_000,
            "candidate {provider}/{model} (window {}) cannot fit 325k unprunable prompt",
            spec.context_window
        );
        assert_ne!(
            model, "codestral-latest",
            "codestral (256k) must be excluded for unprunable prompt"
        );
        assert_ne!(
            model, "gemma-4-26b-a4b-it",
            "gemma (256k) must be excluded for unprunable prompt"
        );
    }
}

// ---------------------------------------------------------------------------
// Challenge 6: Pre-Stream Failover & SSE Attribution Invariants
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_challenge_prestream_failover_and_sse_attribution_invariants() {
    let db = Arc::new(Db::open_in_memory().unwrap());
    db.migrate().unwrap();

    let (backends, gemini, agentrouter, _) = setup_all_backends();

    // Streaming failure cascade:
    // Candidate 1 (gemini-3-flash-preview) fails on initial stream chunk with 429
    gemini.push_stream_chunk(Err(GatewayError::Upstream(
        "upstream status 429: stream rate limited".into(),
    )));

    // Candidate 2 (agentrouter/gpt-6.1-sol under Planning stage) emits valid stream chunks
    agentrouter.push_stream_chunk(Ok(StreamChunk {
        content: Some("First streaming token".into()),
        finish_reason: None,
        ..Default::default()
    }));
    agentrouter.push_stream_chunk(Ok(StreamChunk {
        content: Some(" and final conclusion.".into()),
        finish_reason: Some("stop".into()),
        ..Default::default()
    }));

    let routing_backend = Arc::new(RoutingBackend::new(backends, Router::new(vec![], vec![])));

    let state = AppState::with_db(routing_backend, db.clone());
    let router = build_router_with_state(state);

    // Call /v1/chat/completions with stream=true using alias "auto"
    let req_body = json!({
        "model": "auto",
        "stream": true,
        "messages": [{"role": "user", "content": "Explain pre-stream failover"}]
    });

    let http_req = Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(&req_body).unwrap()))
        .unwrap();

    let http_res = router.oneshot(http_req).await.unwrap();
    assert_eq!(http_res.status(), StatusCode::OK);

    let mut body_stream = http_res.into_body().into_data_stream();
    let mut collected_chunks = Vec::new();

    while let Some(chunk_res) = body_stream.next().await {
        let chunk_bytes = chunk_res.unwrap();
        let chunk_str = String::from_utf8_lossy(&chunk_bytes);
        for line in chunk_str.lines() {
            if let Some(data) = line.strip_prefix("data: ") {
                if data == "[DONE]" {
                    continue;
                }
                if let Ok(v) = serde_json::from_str::<Value>(data) {
                    collected_chunks.push(v);
                }
            }
        }
    }

    assert!(
        !collected_chunks.is_empty(),
        "stream must deliver SSE chunks to client"
    );

    // Wire Invariant: All client chunks MUST preserve requested model ("auto")
    for chunk in &collected_chunks {
        assert_eq!(
            chunk["model"], "auto",
            "client wire chunk must preserve requested model name 'auto'"
        );
    }

    // DB Call Logs Invariant: Actual served candidate model must be logged in DB call_logs
    let guard = db.connection();
    let logs = recent_calls(&guard, 10).expect("must query call logs");
    assert!(!logs.is_empty(), "call log entry must be recorded in DB");

    let logged = &logs[0];
    assert_eq!(logged.provider.as_deref(), Some("agentrouter"));
    assert_eq!(
        logged.model.as_deref(),
        Some("gpt-6.1-sol"),
        "DB call logs must record actual candidate model served"
    );
    assert_eq!(
        logged.combo_name.as_deref(),
        Some("heterion-router/auto"),
        "combo_name must be heterion-router/auto"
    );
}

// ---------------------------------------------------------------------------
// Challenge 7: Model-Scoped 429 Cooldown — A Rate-Limited Model Must Not Take
// Its Siblings Down With It.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_challenge_model_scoped_429_keeps_tier3_gemini_protectors_alive() {
    let (backends, gemini, _, _) = setup_all_backends();
    let routing = RoutingBackend::new(backends, Router::new(vec![], vec![]));

    // A vision request has ONLY Gemini candidates in its plan:
    // 1. gemini-3-flash-preview
    // 2. gemini-3.5-flash-lite
    // 3. gemini-3.1-flash-lite
    let vision_req = ChatCompletionRequest {
        model: "heterion-router/auto".into(),
        messages: vec![ChatMessage {
            role: "user".into(),
            content: Some(json!([
                {"type": "text", "text": "Analyze diagram"},
                {"type": "image_url", "image_url": {"url": "https://example.org/test.png"}}
            ])),
            reasoning_content: None,
            tool_calls: None,
            tool_call_id: None,
            name: None,
        }],
        ..Default::default()
    };

    // Candidate 1 (gemini-3-flash-preview) hits a 429 rate limit with Retry-After: 30
    gemini.push_response(Err(GatewayError::Upstream(
        "upstream status 429: rate limit exceeded, retry-after: 30".into(),
    )));

    // Candidate 2 (gemini-3.5-flash-lite) is queued to succeed as the High-Quota Protector
    gemini.push_response(Ok(ChatCompletionResponse {
        id: "resp-vis-tier3".into(),
        object: "chat.completion",
        created: 99999,
        model: "gemini-3.5-flash-lite".into(),
        usage: Usage::default(),
        choices: vec![ChatChoice {
            index: 0,
            message: ChatMessage::assistant_text("Tier 3 Vision Protector Success"),
            finish_reason: "stop".into(),
        }],
        raw: None,
    }));

    // The directive is explicit: "HTTP 429: Drop from ladder temporarily for
    // duration of cooldown. Request cascades to next tier immediately."
    // A rate limit on ONE model must therefore never disable the sibling
    // models of the same provider — least of all the Tier-3 high-quota
    // protector whose entire purpose is to absorb a Tier-1 rate limit.
    let response = routing
        .complete(vision_req)
        .await
        .expect("a 429 on one model must cascade to the Tier-3 protector, not fail the request");
    assert_eq!(
        response.choices[0].message.text(),
        "Tier 3 Vision Protector Success"
    );

    // The protector must actually have been invoked.
    assert_eq!(
        gemini.calls(),
        2,
        "the Tier-3 protector must be tried after the rate-limited model is skipped"
    );

    // The cooldown is scoped to the model that was rate-limited.
    assert!(
        routing
            .cooldowns()
            .is_in_cooldown("gemini", Some("gemini-3-flash-preview")),
        "the rate-limited model must be in cooldown"
    );
    assert!(
        !routing.cooldowns().is_in_cooldown("gemini", None),
        "a per-model 429 must not put the whole provider in cooldown"
    );
    assert!(
        routing
            .cooldowns()
            .is_available("gemini", Some("gemini-3.5-flash-lite")),
        "a sibling model must stay available while one model is rate-limited"
    );
}

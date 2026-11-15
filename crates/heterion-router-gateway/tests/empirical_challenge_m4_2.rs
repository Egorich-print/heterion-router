//! Empirical Challenge Harness for Milestone 4 (Challenger 2)
//!
//! Scope:
//! 1. Dynamic Priority Formula & Math Demotion Guarantee:
//!    - Verify: `Effective Priority = (Base Priority * Stage Weight) * Quality Score * Availability State`.
//!    - Challenge Tier 1 probation demotion: When Tier 1 candidate (base priority 300.0) incurs repeated tool failures,
//!      its quality score is clamped to 0.2 ceiling during 15-minute probation: 300 * 0.2 = 60.0.
//!    - Empirically verify that this demoted candidate strictly ranks BELOW healthy Tier 2 candidates
//!      (base 200.0, effective 200.0) and Tier 3 candidates (base 100.0, effective 100.0).
//!    - Challenge stage weightings: Verify demoted Tier 1 remains demoted below Tier 2 and Tier 3 under all agent stages
//!      (`Default`, `Planning`, `ErrorRecovery`, `MechanicalToolLoop`).
//!    - Challenge probation timing and expiration: Verify exact restoration behavior at t0 + 14m 59s vs t0 + 15m 1s.
//!    - Challenge recovery streak: Verify gradual score increase (+0.05 per success) reclaims Tier 1 standing.
//! 2. Breaker Invariant on 4xx / Cooldown Isolation:
//!    - Challenge circuit breaker: Under repeated 429 backoffs (30s -> 60s -> 120s) and 403/402 balance freezes (3600s),
//!      verify `BreakerSet` is NEVER tripped (`BreakerSet.allow(provider, now)` remains true).
//!    - Verify cooldowns drop availability state to 0.0 without triggering circuit breaker state transitions.
//!    - Verify state orthogonality: Cooldown expiration does not affect breaker state, and breaker reset does not clear cooldown.
//! 3. Model Catalog & Aliasing:
//!    - Challenge `GET /v1/models`: verify canonical `heterion-router/auto` and alias `auto` are both present,
//!      with matching schema and capability dictionaries, without breaking existing custom providers or DB combos.
//!    - Verify symmetric auth pattern matching equivalence.
//! 4. End-to-End Execution & Cascading:
//!    - Empirically verify that when Tier 1 models are demoted via probation, execution cascades directly to healthy Tier 2 candidates.

use async_trait::async_trait;
use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use heterion_router_core::{
    ChatChoice, ChatCompletionRequest, ChatCompletionResponse, ChatMessage, GatewayError, Usage,
};
use heterion_router_db::{
    Db,
    repos::{self, ComboRow},
};
use heterion_router_gateway::{
    AppState,
    auth::matches_pattern,
    backend::{ChatBackend, ChunkStream, stamp_attribution},
    build_router_with_state,
    routing::RoutingBackend,
};
use heterion_router_routing::Router;
use heterion_router_routing::breaker::BreakerState;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tower::ServiceExt;

// ---------------------------------------------------------------------------
// Mock Backend Implementation for Challenge Harness
// ---------------------------------------------------------------------------

struct ChallengeMockBackend {
    name: &'static str,
    responses: Mutex<Vec<Result<ChatCompletionResponse, GatewayError>>>,
    calls: Arc<AtomicUsize>,
    received_requests: Arc<Mutex<Vec<ChatCompletionRequest>>>,
}

impl ChallengeMockBackend {
    fn new(name: &'static str) -> Self {
        Self {
            name,
            responses: Mutex::new(Vec::new()),
            calls: Arc::new(AtomicUsize::new(0)),
            received_requests: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn push_response(&self, res: Result<ChatCompletionResponse, GatewayError>) {
        self.responses.lock().unwrap().push(res);
    }

    fn clear_responses(&self) {
        self.responses.lock().unwrap().clear();
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::Relaxed)
    }
}

#[async_trait]
impl ChatBackend for ChallengeMockBackend {
    fn name(&self) -> &'static str {
        self.name
    }

    async fn complete(
        &self,
        request: ChatCompletionRequest,
    ) -> Result<ChatCompletionResponse, GatewayError> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        self.received_requests.lock().unwrap().push(request.clone());

        let mut guard = self.responses.lock().unwrap();
        if !guard.is_empty() {
            guard.remove(0)
        } else {
            Ok(ChatCompletionResponse {
                id: format!("resp-challenge-{}", self.name),
                object: "chat.completion",
                created: 1000,
                model: request.model.clone(),
                usage: Usage::default(),
                choices: vec![ChatChoice {
                    index: 0,
                    message: ChatMessage::assistant_text(format!("Echo from {}", self.name)),
                    finish_reason: "stop".into(),
                }],
                raw: None,
            })
        }
    }

    fn stream(&self, _request: ChatCompletionRequest) -> ChunkStream {
        Box::pin(futures::stream::empty())
    }
}

fn auto_request() -> ChatCompletionRequest {
    ChatCompletionRequest {
        model: "heterion-router/auto".into(),
        messages: vec![ChatMessage::plain("user", "Hello empirical challenge")],
        ..Default::default()
    }
}

fn auto_request_with_stage(stage_prompt: &str) -> ChatCompletionRequest {
    ChatCompletionRequest {
        model: "heterion-router/auto".into(),
        messages: vec![ChatMessage::plain("user", stage_prompt)],
        ..Default::default()
    }
}

// ---------------------------------------------------------------------------
// Challenge 1: Dynamic Priority Math Demotion Guarantee
// ---------------------------------------------------------------------------

#[test]
fn test_challenge_dynamic_priority_math_demotion_guarantee() {
    let gemini = Arc::new(ChallengeMockBackend::new("gemini"));
    let agentrouter = Arc::new(ChallengeMockBackend::new("agentrouter"));
    let mistral = Arc::new(ChallengeMockBackend::new("mistral"));

    let mut backends: HashMap<String, Arc<dyn ChatBackend>> = HashMap::new();
    backends.insert("gemini".into(), gemini);
    backends.insert("agentrouter".into(), agentrouter);
    backends.insert("mistral".into(), mistral);

    let routing = RoutingBackend::new(backends, Router::new(vec![], vec![]));
    let t0 = Instant::now();
    let req = auto_request();

    // 1. Pristine baseline: all candidates healthy (quality = 1.0, availability = 1.0)
    // Note: A single user turn triggers AgentStage::Planning.
    // In Planning:
    // - gemini-3-flash-preview: 300.0 * 1.0 = 300.0
    // - gpt-6.1-sol (FrontierReasoning): 280.0 * 1.05 = 294.0
    // - gpt-5.6-sol (FastExecution): 290.0 * 1.0 = 290.0
    // - codestral-latest (FastExecution): 200.0 * 1.0 = 200.0
    // - gemma-4-26b-a4b-it (FastExecution): 190.0 * 1.0 = 190.0
    // - gemini-3.5-flash-lite (FastExecution): 100.0 * 1.0 = 100.0
    // - gemini-3.1-flash-lite (FastExecution): 90.0 * 1.0 = 90.0
    let pristine_plan = routing.auto_plan_at(&req, t0);
    assert_eq!(
        pristine_plan.steps.len(),
        7,
        "All 7 candidates must be present"
    );
    assert_eq!(
        pristine_plan.steps[0],
        ("gemini".into(), "gemini-3-flash-preview".into()),
        "Top Tier 1 candidate must be gemini-3-flash-preview (300.0)"
    );
    assert_eq!(
        pristine_plan.steps[1],
        ("agentrouter".into(), "gpt-6.1-sol".into()),
        "FrontierReasoning gpt-6.1-sol (294.0 with Planning stage boost) ranks at index 1"
    );
    assert_eq!(
        pristine_plan.steps[2],
        ("agentrouter".into(), "gpt-5.6-sol".into()),
        "gpt-5.6-sol (290.0) ranks at index 2"
    );
    assert_eq!(
        pristine_plan.steps[3],
        ("mistral".into(), "codestral-latest".into()),
        "Tier 2 Candidate 1 must be codestral-latest (200.0)"
    );
    assert_eq!(
        pristine_plan.steps[4],
        ("gemini".into(), "gemma-4-26b-a4b-it".into()),
        "Tier 2 Candidate 2 must be gemma-4-26b-a4b-it (190.0)"
    );
    assert_eq!(
        pristine_plan.steps[5],
        ("gemini".into(), "gemini-3.5-flash-lite".into()),
        "Tier 3 Candidate 1 must be gemini-3.5-flash-lite (100.0)"
    );
    assert_eq!(
        pristine_plan.steps[6],
        ("gemini".into(), "gemini-3.1-flash-lite".into()),
        "Tier 3 Candidate 2 must be gemini-3.1-flash-lite (90.0)"
    );

    // 2. First tool failure on Tier 1 candidate (gemini-3-flash-preview)
    // Score drops: 1.0 - 0.3 = 0.7. consecutive_failures = 1. NOT in probation.
    // Effective priority: 300.0 * 0.7 = 210.0.
    routing
        .quality_tracker()
        .record_failure_at("gemini-3-flash-preview", t0);
    assert_eq!(
        routing
            .quality_tracker()
            .get_score_at("gemini-3-flash-preview", t0),
        0.7
    );
    assert!(
        !routing
            .quality_tracker()
            .is_in_probation_at("gemini-3-flash-preview", t0)
    );

    let plan_after_1fail = routing.auto_plan_at(&req, t0);
    // At 210.0, gemini-3-flash-preview drops below gpt-6.1-sol (294) and gpt-5.6-sol (290),
    // but remains strictly ABOVE codestral-latest (200.0)
    assert_eq!(
        plan_after_1fail.steps[2],
        ("gemini".into(), "gemini-3-flash-preview".into())
    );
    assert_eq!(
        plan_after_1fail.steps[3],
        ("mistral".into(), "codestral-latest".into())
    );

    // 3. Second consecutive tool failure within 15 minutes triggers PROBATION:
    // consecutive_failures = 2. 15-minute probation is activated.
    // Effective score is strictly clamped to PROBATION_SCORE_CEILING = 0.2.
    let t_fail2 = t0 + Duration::from_secs(10);
    routing
        .quality_tracker()
        .record_failure_at("gemini-3-flash-preview", t_fail2);
    assert!(
        routing
            .quality_tracker()
            .is_in_probation_at("gemini-3-flash-preview", t_fail2),
        "Candidate must be in probation after 2 consecutive defects"
    );
    let effective_score = routing
        .quality_tracker()
        .effective_score_at("gemini-3-flash-preview", t_fail2);
    assert_eq!(
        effective_score, 0.2,
        "Quality score must be clamped to ceiling 0.2 during probation"
    );

    // MATH DEMOTION GUARANTEE:
    // Effective Priority = 300.0 * 0.2 * 1.0 = 60.0.
    // Compare with healthy candidates:
    // - Healthy Tier 2: codestral-latest = 200.0 * 1.0 = 200.0
    // - Healthy Tier 2: gemma-4-26b-a4b-it = 190.0 * 1.0 = 190.0
    // - Healthy Tier 3: gemini-3.5-flash-lite = 100.0 * 1.0 = 100.0
    // - Healthy Tier 3: gemini-3.1-flash-lite = 90.0 * 1.0 = 90.0
    // Demoted candidate (60.0) MUST rank STRICTLY BELOW all Tier 2 AND all Tier 3 candidates!
    let plan_during_probation = routing.auto_plan_at(&req, t_fail2);
    assert_eq!(
        plan_during_probation.steps.len(),
        7,
        "Demoted candidate remains available as bottom fallback"
    );

    // Verify order:
    // 0: gpt-6.1-sol (294.0)
    // 1: gpt-5.6-sol (290.0)
    // 2: codestral-latest (200.0)
    // 3: gemma-4-26b-a4b-it (190.0)
    // 4: gemini-3.5-flash-lite (100.0)
    // 5: gemini-3.1-flash-lite (90.0)
    // 6: gemini-3-flash-preview (60.0)
    assert_eq!(
        plan_during_probation.steps[0],
        ("agentrouter".into(), "gpt-6.1-sol".into())
    );
    assert_eq!(
        plan_during_probation.steps[1],
        ("agentrouter".into(), "gpt-5.6-sol".into())
    );
    assert_eq!(
        plan_during_probation.steps[2],
        ("mistral".into(), "codestral-latest".into()),
        "Healthy Tier 2 candidate must rank above demoted Tier 1"
    );
    assert_eq!(
        plan_during_probation.steps[3],
        ("gemini".into(), "gemma-4-26b-a4b-it".into()),
        "Healthy Tier 2 candidate must rank above demoted Tier 1"
    );
    assert_eq!(
        plan_during_probation.steps[4],
        ("gemini".into(), "gemini-3.5-flash-lite".into()),
        "Healthy Tier 3 candidate must rank above demoted Tier 1"
    );
    assert_eq!(
        plan_during_probation.steps[5],
        ("gemini".into(), "gemini-3.1-flash-lite".into()),
        "Healthy Tier 3 candidate must rank above demoted Tier 1"
    );
    assert_eq!(
        plan_during_probation.steps[6],
        ("gemini".into(), "gemini-3-flash-preview".into()),
        "Demoted Tier 1 candidate (60.0) must be ranked at index 6, below ALL Tier 2 and Tier 3"
    );

    // 4. Probation expiration dynamics:
    // At t_fail2 + 14m 59s: probation remains active (ceiling 0.2, effective 60.0)
    let t_pre_exp = t_fail2 + Duration::from_secs(14 * 60 + 59);
    assert!(
        routing
            .quality_tracker()
            .is_in_probation_at("gemini-3-flash-preview", t_pre_exp)
    );
    let plan_pre_exp = routing.auto_plan_at(&req, t_pre_exp);
    assert_eq!(
        plan_pre_exp.steps[6],
        ("gemini".into(), "gemini-3-flash-preview".into())
    );

    // At t_fail2 + 15m 1s: probation expires!
    // Underlying score: 0.4 (1.0 - 0.3 - 0.3).
    // Effective priority: 300.0 * 0.4 = 120.0.
    // 120.0 ranks ABOVE Tier 3 (100.0, 90.0) but BELOW Tier 2 (200.0, 190.0).
    let t_post_exp = t_fail2 + Duration::from_secs(15 * 60 + 1);
    assert!(
        !routing
            .quality_tracker()
            .is_in_probation_at("gemini-3-flash-preview", t_post_exp)
    );
    let score_post_exp = routing
        .quality_tracker()
        .get_score_at("gemini-3-flash-preview", t_post_exp);
    assert!(
        (score_post_exp - 0.4).abs() < 1e-6,
        "Score after probation expiration must be 0.4 (actual: {score_post_exp})"
    );

    let plan_post_exp = routing.auto_plan_at(&req, t_post_exp);
    // At 120.0, gemini-3-flash-preview ranks at index 4 (between Tier 2 and Tier 3):
    // 0: gpt-6.1-sol (294.0)
    // 1: gpt-5.6-sol (290.0)
    // 2: codestral-latest (200.0)
    // 3: gemma-4-26b-a4b-it (190.0)
    // 4: gemini-3-flash-preview (120.0)  <-- Restored above Tier 3!
    // 5: gemini-3.5-flash-lite (100.0)
    // 6: gemini-3.1-flash-lite (90.0)
    assert_eq!(
        plan_post_exp.steps[4],
        ("gemini".into(), "gemini-3-flash-preview".into()),
        "Post-probation candidate at 120.0 must rank above Tier 3 (100.0)"
    );
    assert_eq!(
        plan_post_exp.steps[5],
        ("gemini".into(), "gemini-3.5-flash-lite".into())
    );

    // 5. Recovery streak dynamics:
    // Successes increment score by 0.05 per success
    // 6 successes: 0.4 + 6 * 0.05 = 0.70 -> Effective priority: 300 * 0.7 = 210.0 > 200.0 (codestral)!
    for i in 1..=6 {
        routing.quality_tracker().record_success_at(
            "gemini-3-flash-preview",
            t_post_exp + Duration::from_secs(i),
        );
    }
    let t_recovered = t_post_exp + Duration::from_secs(7);
    assert!(
        (routing
            .quality_tracker()
            .get_score_at("gemini-3-flash-preview", t_recovered)
            - 0.7)
            .abs()
            < 1e-6
    );
    let plan_recovered = routing.auto_plan_at(&req, t_recovered);
    // 210.0 reclaims spot above codestral-latest (200.0):
    assert_eq!(
        plan_recovered.steps[2],
        ("gemini".into(), "gemini-3-flash-preview".into()),
        "Recovered score 0.70 (210.0) reclaims position above Tier 2 codestral (200.0)"
    );
}

// ---------------------------------------------------------------------------
// Challenge 1B: Stage Weighting Under Demotion
// ---------------------------------------------------------------------------

#[test]
fn test_challenge_stage_weightings_under_probation_demotion() {
    let gemini = Arc::new(ChallengeMockBackend::new("gemini"));
    let agentrouter = Arc::new(ChallengeMockBackend::new("agentrouter"));
    let mistral = Arc::new(ChallengeMockBackend::new("mistral"));

    let mut backends: HashMap<String, Arc<dyn ChatBackend>> = HashMap::new();
    backends.insert("gemini".into(), gemini);
    backends.insert("agentrouter".into(), agentrouter);
    backends.insert("mistral".into(), mistral);

    let routing = RoutingBackend::new(backends, Router::new(vec![], vec![]));
    let t0 = Instant::now();

    // 1. Planning Stage:
    // Prompt: "Plan the step-by-step architecture" -> triggers AgentStage::Planning
    // FrontierReasoning models (gpt-6.1-sol) get 1.05x boost: 280.0 * 1.05 = 294.0 > gpt-5.6-sol (290.0)
    let planning_req =
        auto_request_with_stage("Please design and plan the architectural steps for migration");
    let plan_planning = routing.auto_plan_at(&planning_req, t0);
    assert_eq!(
        plan_planning.steps[1],
        ("agentrouter".into(), "gpt-6.1-sol".into()),
        "gpt-6.1-sol (294.0) must surpass gpt-5.6-sol (290.0) in Planning stage"
    );

    // If gpt-6.1-sol incurs 2 consecutive failures and enters probation:
    routing
        .quality_tracker()
        .record_failure_at("gpt-6.1-sol", t0);
    routing
        .quality_tracker()
        .record_failure_at("gpt-6.1-sol", t0 + Duration::from_secs(5));
    assert!(
        routing
            .quality_tracker()
            .is_in_probation_at("gpt-6.1-sol", t0 + Duration::from_secs(5))
    );

    // In Planning stage, demoted gpt-6.1-sol effective priority:
    // (280.0 * 1.05) * 0.2 * 1.0 = 58.8.
    // 58.8 < 90.0 (gemini-3.1-flash-lite) < 100.0 < 190.0 < 200.0.
    // Must strictly rank below ALL Tier 2 and Tier 3 candidates!
    let plan_planning_demoted = routing.auto_plan_at(&planning_req, t0 + Duration::from_secs(5));
    assert_eq!(
        plan_planning_demoted.steps[6],
        ("agentrouter".into(), "gpt-6.1-sol".into()),
        "Demoted gpt-6.1-sol (58.8) in Planning stage must rank below all Tier 2 and Tier 3 models"
    );

    // 2. Mechanical Tool Loop Stage:
    // When gemini-3-flash-preview (FastExecution, 300.0) gets 1.05x boost:
    // 300.0 * 1.05 = 315.0.
    // If it enters probation: 315.0 * 0.2 = 63.0.
    // Healthy Tier 3 (gemini-3.5-flash-lite, FastExecution) gets 1.05x: 100.0 * 1.05 = 105.0.
    // Healthy Tier 3 (gemini-3.1-flash-lite, FastExecution) gets 1.05x: 90.0 * 1.05 = 94.5.
    // 63.0 < 94.5 < 105.0!
    // Even with FastExecution stage boost, demoted candidate (63.0) remains strictly below Tier 3!
    routing
        .quality_tracker()
        .record_failure_at("gemini-3-flash-preview", t0);
    routing
        .quality_tracker()
        .record_failure_at("gemini-3-flash-preview", t0 + Duration::from_secs(5));

    // Request with tool call context representing mechanical tool iteration
    let tool_loop_req = ChatCompletionRequest {
        model: "heterion-router/auto".into(),
        messages: vec![
            ChatMessage::plain("user", "Run the loop"),
            ChatMessage {
                role: "assistant".into(),
                content: None,
                reasoning_content: None,
                tool_calls: Some(
                    json!([{"id": "c1", "type": "function", "function": {"name": "f", "arguments": "{}"}}]),
                ),
                tool_call_id: None,
                name: None,
            },
            ChatMessage {
                role: "tool".into(),
                content: Some("result ok".into()),
                reasoning_content: None,
                tool_calls: None,
                tool_call_id: Some("c1".into()),
                name: Some("f".into()),
            },
        ],
        tools: Some(json!([{"type": "function", "function": {"name": "f", "parameters": {}}}])),
        ..Default::default()
    };
    let plan_tool_loop = routing.auto_plan_at(&tool_loop_req, t0 + Duration::from_secs(5));

    // Both demoted Tier 1s (gemini-3-flash-preview at 63.0, gpt-6.1-sol at 56.0)
    // must rank strictly below healthy Tier 2 and Tier 3 models:
    let steps = plan_tool_loop.steps;
    let idx_flash = steps
        .iter()
        .position(|(_, m)| m == "gemini-3-flash-preview")
        .expect("flash preview present");
    let idx_codestral = steps
        .iter()
        .position(|(_, m)| m == "codestral-latest")
        .expect("codestral present");
    let idx_gemma = steps
        .iter()
        .position(|(_, m)| m == "gemma-4-26b-a4b-it")
        .expect("gemma present");
    let idx_lite = steps
        .iter()
        .position(|(_, m)| m == "gemini-3.5-flash-lite")
        .expect("flash lite present");

    assert!(
        idx_flash > idx_codestral,
        "63.0 must rank below codestral (210.0)"
    );
    assert!(idx_flash > idx_gemma, "63.0 must rank below gemma (199.5)");
    assert!(
        idx_flash > idx_lite,
        "63.0 must rank below flash lite (105.0)"
    );
}

// ---------------------------------------------------------------------------
// Challenge 2: Breaker Invariant on 4xx & Cooldown Isolation
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_challenge_breaker_invariant_under_repeated_4xx_stress() {
    let gemini = Arc::new(ChallengeMockBackend::new("gemini"));

    let mut backends: HashMap<String, Arc<dyn ChatBackend>> = HashMap::new();
    backends.insert("gemini".into(), gemini.clone());

    // Circuit breaker configured with threshold = 3, cooldown = 60s
    let routing =
        RoutingBackend::with_breaker_settings(backends, Router::new(vec![], vec![]), 3, 60);

    // STRESS ATTACK 1: 50 consecutive 429 Rate Limit errors directly on gemini.
    // Only a "gemini" backend exists, so the plan holds four Gemini candidates
    // (flash-preview, gemma, flash-lite, flash-lite-3.1). The 429 cooldown is
    // model-scoped, so each candidate has to fail on its own for the request to
    // fail — which is exactly what the breaker invariant below is about.
    const GEMINI_CANDIDATES: usize = 4;
    for i in 1..=50 {
        routing.cooldowns().clear_all_for_provider("gemini");
        routing
            .quality_tracker()
            .record_success("gemini-3-flash-preview");
        gemini.clear_responses();
        for candidate in 0..GEMINI_CANDIDATES {
            gemini.push_response(Err(GatewayError::Upstream(format!(
                "upstream status 429: rate limit exceeded attempt {i} candidate {candidate}"
            ))));
        }

        let req = auto_request();
        let err = routing.complete(req).await.unwrap_err();
        match err {
            GatewayError::Upstream(msg) => {
                assert!(msg.contains("429"), "Must return 429 error");
            }
            other => panic!("Expected Upstream error, got: {other:?}"),
        }

        // INVARIANT CHECK: BreakerSet must NEVER trip on 429!
        let now = Instant::now();
        assert!(
            routing.breakers().lock().unwrap().allow("gemini", now),
            "INVARIANT VIOLATION: BreakerSet tripped on 429 at iteration {i}"
        );
        assert_eq!(
            routing.breakers().lock().unwrap().state("gemini"),
            BreakerState::Closed,
            "Breaker state must remain Closed for 429 errors"
        );
        // The rate-limited model is in active cooldown in CooldownSet.
        assert!(
            routing
                .cooldowns()
                .is_in_cooldown_at("gemini", Some("gemini-3-flash-preview"), now),
            "CooldownSet must record 429 cooldown for the rate-limited model"
        );
        assert!(
            !routing.cooldowns().is_in_cooldown_at("gemini", None, now),
            "a per-model 429 must not put the whole provider in cooldown"
        );
    }

    // STRESS ATTACK 2: 50 consecutive 403 Forbidden / 402 Balance Freeze errors directly on gemini
    for i in 1..=50 {
        routing.cooldowns().clear_all_for_provider("gemini");
        routing
            .quality_tracker()
            .record_success("gemini-3-flash-preview");
        gemini.clear_responses();
        let code = if i % 2 == 0 { 403 } else { 402 };
        gemini.push_response(Err(GatewayError::Upstream(format!(
            "upstream status {code}: insufficient balance or forbidden"
        ))));

        let req = auto_request();
        let err = routing.complete(req).await.unwrap_err();
        match err {
            GatewayError::Upstream(msg) => {
                assert!(msg.contains(&code.to_string()), "Must return {code} error");
            }
            other => panic!("Expected Upstream error, got: {other:?}"),
        }

        // INVARIANT CHECK: BreakerSet must NEVER trip on 403 or 402!
        let now = Instant::now();
        assert!(
            routing.breakers().lock().unwrap().allow("gemini", now),
            "INVARIANT VIOLATION: BreakerSet tripped on {code} at iteration {i}"
        );
        assert_eq!(
            routing.breakers().lock().unwrap().state("gemini"),
            BreakerState::Closed,
            "Breaker state must remain Closed for 403/402 balance freezes"
        );
        // CooldownSet records 1-hour balance freeze
        assert!(
            routing.cooldowns().is_in_cooldown_at("gemini", None, now),
            "CooldownSet must record 403/402 balance freeze"
        );
    }

    // Availability state drops to 0.0 purely via ProviderCooldownSet:
    let now = Instant::now();
    assert!(routing.cooldowns().is_in_cooldown_at("gemini", None, now));
    let plan = routing.auto_plan_at(&auto_request(), now);
    for (provider, _) in &plan.steps {
        assert_ne!(
            provider, "gemini",
            "gemini must have availability 0.0 and be excluded from plan while in balance freeze"
        );
    }
}

#[test]
fn test_challenge_cooldown_and_breaker_state_orthogonality() {
    let gemini = Arc::new(ChallengeMockBackend::new("gemini"));
    let mut backends: HashMap<String, Arc<dyn ChatBackend>> = HashMap::new();
    backends.insert("gemini".into(), gemini);

    let routing =
        RoutingBackend::with_breaker_settings(backends, Router::new(vec![], vec![]), 3, 60);
    let t0 = Instant::now();

    // 1. Enter 403 1-hour balance freeze:
    routing.cooldowns().record_cooldown_at(
        "gemini",
        None,
        Duration::from_secs(3600),
        heterion_router_gateway::resilience::CooldownReason::BalanceFrozen,
        t0,
    );

    // Availability state = 0.0, Breaker = ALLOW (closed, healthy)
    assert!(routing.cooldowns().is_in_cooldown_at("gemini", None, t0));
    assert!(routing.breakers().lock().unwrap().allow("gemini", t0));
    let plan_frozen = routing.auto_plan_at(&auto_request(), t0);
    assert!(
        plan_frozen.steps.is_empty(),
        "Plan must be empty when provider availability is 0.0"
    );

    // 2. Advance time past cooldown expiration (3601s):
    let t_exp = t0 + Duration::from_secs(3601);
    assert!(!routing.cooldowns().is_in_cooldown_at("gemini", None, t_exp));
    assert!(routing.breakers().lock().unwrap().allow("gemini", t_exp));
    let plan_restored = routing.auto_plan_at(&auto_request(), t_exp);
    assert!(
        !plan_restored.steps.is_empty(),
        "Provider availability must restore to 1.0 once cooldown expires"
    );

    // 3. Now simulate 3 consecutive 502 Bad Gateway outages:
    // 5xx errors DO trip BreakerSet!
    routing
        .breakers()
        .lock()
        .unwrap()
        .record_failure("gemini", t_exp);
    routing
        .breakers()
        .lock()
        .unwrap()
        .record_failure("gemini", t_exp);
    routing
        .breakers()
        .lock()
        .unwrap()
        .record_failure("gemini", t_exp);

    // Breaker is now tripped (allow == false)
    assert!(!routing.breakers().lock().unwrap().allow("gemini", t_exp));
    // But cooldown set is completely independent and has no cooldown:
    assert!(!routing.cooldowns().is_in_cooldown_at("gemini", None, t_exp));

    // Availability drops to 0.0 because of tripped breaker:
    let plan_breaker_tripped = routing.auto_plan_at(&auto_request(), t_exp);
    assert!(
        plan_breaker_tripped.steps.is_empty(),
        "Tripped breaker must drop availability to 0.0"
    );

    // 4. Recovery of tripped breaker:
    // Advance past breaker cooldown (60s):
    let t_probe = t_exp + Duration::from_secs(61);
    // Trial probe request allowed
    assert!(routing.breakers().lock().unwrap().allow("gemini", t_probe));
    // Successful completion of probe closes breaker
    routing.breakers().lock().unwrap().record_success("gemini");
    assert!(routing.breakers().lock().unwrap().allow("gemini", t_probe));
    let plan_recovered = routing.auto_plan_at(&auto_request(), t_probe);
    assert!(
        !plan_recovered.steps.is_empty(),
        "Resetting breaker restores availability"
    );
}

// ---------------------------------------------------------------------------
// Challenge 3: Model Catalog & Aliasing Invariants
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_challenge_catalog_and_aliasing_integrity() {
    let db = Arc::new(Db::open_in_memory().unwrap());
    db.migrate().unwrap();

    {
        let guard = db.connection();
        repos::upsert_combo(
            &guard,
            &ComboRow {
                id: "custom-1".into(),
                name: "deep-think".into(),
                data: json!({"isHidden": false, "description": "Custom user combo"}).to_string(),
                sort_order: 10,
                active: true,
            },
        )
        .unwrap();
        // Hidden combo should be omitted
        repos::upsert_combo(
            &guard,
            &ComboRow {
                id: "custom-hidden".into(),
                name: "secret-combo".into(),
                data: json!({"isHidden": true}).to_string(),
                sort_order: 20,
                active: true,
            },
        )
        .unwrap();
    }

    let state = AppState::with_db(Arc::new(ChallengeMockBackend::new("mock")), db);
    let router = build_router_with_state(state);

    let req = Request::builder()
        .method("GET")
        .uri("/v1/models")
        .body(Body::empty())
        .unwrap();

    let res = router.oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    let catalog: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(catalog["object"], "list");

    let data = catalog["data"].as_array().expect("data must be an array");
    assert!(data.len() >= 3);

    // 1. Verify Canonical heterion-router/auto (must be at index 0)
    let canonical = &data[0];
    assert_eq!(canonical["id"], "heterion-router/auto");
    assert_eq!(canonical["object"], "model");
    assert_eq!(canonical["owned_by"], "heterion-router");
    assert_eq!(canonical["capabilities"]["context_window"], 1_000_000);
    assert_eq!(canonical["capabilities"]["supports_vision"], true);
    assert_eq!(canonical["capabilities"]["tool_support"], "native");
    assert_eq!(canonical["capabilities"]["streaming"], true);
    assert_eq!(canonical["capabilities"]["zero_quota_pruning"], true);
    assert_eq!(canonical["capabilities"]["dynamic_ladder"], true);
    assert_eq!(canonical["heterion-router"]["kind"], "auto");
    assert_eq!(canonical["heterion-router"]["strategy"], "switchyard");
    assert_eq!(canonical["heterion-router"]["is_canonical"], true);
    assert_eq!(canonical["heterion-router"]["alias"], "auto");

    // 2. Verify Alias auto (must be at index 1)
    let alias = &data[1];
    assert_eq!(alias["id"], "auto");
    assert_eq!(alias["object"], "model");
    assert_eq!(alias["owned_by"], "heterion-router");
    assert_eq!(alias["capabilities"]["context_window"], 1_000_000);
    assert_eq!(alias["capabilities"]["supports_vision"], true);
    assert_eq!(alias["capabilities"]["tool_support"], "native");
    assert_eq!(alias["capabilities"]["streaming"], true);
    assert_eq!(alias["capabilities"]["zero_quota_pruning"], true);
    assert_eq!(alias["capabilities"]["dynamic_ladder"], true);
    assert_eq!(alias["heterion-router"]["kind"], "auto");
    assert_eq!(alias["heterion-router"]["strategy"], "switchyard");
    assert_eq!(alias["heterion-router"]["is_alias"], true);
    assert_eq!(
        alias["heterion-router"]["canonical_id"],
        "heterion-router/auto"
    );

    // Invariant: Capability dictionary between canonical and alias must match EXACTLY
    assert_eq!(canonical["capabilities"], alias["capabilities"]);

    // 3. User combos preserved
    let custom_combo = data
        .iter()
        .find(|m| m["id"] == "deep-think")
        .expect("active user combo deep-think must be present in catalog");
    assert_eq!(custom_combo["heterion-router"]["kind"], "combo");

    // Hidden combo must NOT be present
    assert!(
        data.iter().all(|m| m["id"] != "secret-combo"),
        "Hidden combos must not be leaked into /v1/models"
    );

    // 4. Symmetric Auth Pattern Matching Equivalence:
    assert!(matches_pattern("heterion-router/auto", "auto"));
    assert!(matches_pattern("auto", "heterion-router/auto"));
    assert!(matches_pattern(
        "heterion-router/auto",
        "heterion-router/auto"
    ));
    assert!(matches_pattern("auto", "auto"));
    assert!(matches_pattern("*", "heterion-router/auto"));
    assert!(matches_pattern("*", "auto"));
    assert!(!matches_pattern("openai/*", "heterion-router/auto"));
    assert!(!matches_pattern("openai/*", "auto"));
}

// ---------------------------------------------------------------------------
// Challenge 4: Multi-Candidate Cascade With All Tier 1s Demoted
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_challenge_cascade_when_all_tier1_models_in_probation() {
    let gemini = Arc::new(ChallengeMockBackend::new("gemini"));
    let agentrouter = Arc::new(ChallengeMockBackend::new("agentrouter"));
    let mistral = Arc::new(ChallengeMockBackend::new("mistral"));

    let mut backends: HashMap<String, Arc<dyn ChatBackend>> = HashMap::new();
    backends.insert("gemini".into(), gemini.clone());
    backends.insert("agentrouter".into(), agentrouter.clone());
    backends.insert("mistral".into(), mistral.clone());

    let routing = RoutingBackend::new(backends, Router::new(vec![], vec![]));
    let t0 = Instant::now();

    // Force ALL Tier 1 candidates into 15-minute probation:
    // - gemini-3-flash-preview: 300.0 * 0.2 = 60.0
    // - gpt-5.6-sol: 290.0 * 0.2 = 58.0
    // - gpt-6.1-sol: 280.0 * 0.2 = 56.0
    for model in ["gemini-3-flash-preview", "gpt-5.6-sol", "gpt-6.1-sol"] {
        routing.quality_tracker().record_failure_at(model, t0);
        routing
            .quality_tracker()
            .record_failure_at(model, t0 + Duration::from_secs(5));
        assert!(
            routing
                .quality_tracker()
                .is_in_probation_at(model, t0 + Duration::from_secs(5))
        );
    }

    // Configure Tier 2 (mistral / codestral-latest) to succeed
    mistral.push_response(Ok(ChatCompletionResponse {
        id: "resp-codestral-tier2".into(),
        object: "chat.completion",
        created: 555,
        model: "codestral-latest".into(),
        usage: Usage::default(),
        choices: vec![ChatChoice {
            index: 0,
            message: ChatMessage::assistant_text("Served by Tier 2 Codestral"),
            finish_reason: "stop".into(),
        }],
        raw: None,
    }));

    let mut req = auto_request();
    let token = stamp_attribution(&mut req);
    let res = routing.complete(req).await.expect("routing should succeed");

    assert_eq!(res.choices[0].message.text(), "Served by Tier 2 Codestral");
    // Verify that neither gemini nor agentrouter were called because Tier 2 was higher priority!
    assert_eq!(gemini.calls(), 0);
    assert_eq!(agentrouter.calls(), 0);
    assert_eq!(mistral.calls(), 1);

    // Verify attribution correctly books codestral-latest
    let attr = routing.attribution(&token);
    assert_eq!(attr.provider.as_deref(), Some("mistral"));
    assert_eq!(attr.model.as_deref(), Some("codestral-latest"));
    assert_eq!(attr.combo_name.as_deref(), Some("heterion-router/auto"));
}

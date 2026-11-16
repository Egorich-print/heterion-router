//! Empirical Challenge Harness for Milestone 5 (Challenger 2)
//!
//! Attacks the **final integrated** Switchyard behaviour, with the emphasis on the
//! two uncommitted behaviour changes that ship with this milestone:
//!
//! 1. `src/lib.rs` — `GET /v1/models` no longer lists the same id twice
//!    (`push_catalog_entry`), and the `auto` / `heterion-router/auto` entries keep
//!    their capabilities block.
//! 2. `src/routing.rs` — a 429 cooldown is recorded **per model**
//!    (`record_429(name, Some(target_model), …)`) instead of per provider, so a
//!    rate-limited model no longer disables its siblings on the same provider. The
//!    403/402 balance freeze stays provider-wide on purpose.
//!
//! Suites:
//! - A: model-scoped 429 vs. the provider-wide 403/402 freeze, and the breaker.
//! - B: catalog integrity (duplicate ids from combos, `provider/model` ids, aliases).
//! - C: ladder integrity under a combined 429 storm + 403 freeze + 5xx outage.
//! - D: zero-quota guarantee (pruning burns no quota, issues no extra call).
//! - E: regression traps (a 4xx never trips the breaker, never takes down siblings).
//! - F: concurrency (cooldown / quality / attribution integrity under load).
//!
//! Every invariant in this file was proved RED against a deliberately broken variant
//! of the product logic (temporarily patched, then reverted) before it was confirmed
//! green against the real code. The red-proof log lives in
//! `.agents/teamwork/challenger_m5_1/handoff.md`.
//!
//! Harness scope: in-process router + scripted `ChatBackend`s only. Nothing here
//! touches the live launchd instance on `127.0.0.1:20128` (AGENTS.md).

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use heterion_router_core::{
    ChatChoice, ChatCompletionRequest, ChatCompletionResponse, ChatMessage, GatewayError,
    StreamChunk, Usage,
};
use heterion_router_db::{
    Db,
    repos::{self, ComboRow},
};
use heterion_router_gateway::{
    AppState,
    backend::{ChatBackend, ChunkStream, stamp_attribution},
    build_router_with_state,
    routing::RoutingBackend,
};
use heterion_router_providers::ProviderRegistry;
use heterion_router_routing::Router;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;

// ---------------------------------------------------------------------------
// Scripted, model-aware backend
// ---------------------------------------------------------------------------

type Outcome = Result<ChatCompletionResponse, GatewayError>;

/// A backend that answers per **model id**, so a provider that serves several
/// models can be driven differently for each of them.
///
/// Each model has a **persistent default** answer and an optional **queue** of
/// one-shot answers that is consumed before the default applies.
struct ScriptedBackend {
    name: &'static str,
    /// model id -> queued outcomes (consumed front to back)
    outcomes: Mutex<HashMap<String, VecDeque<Outcome>>>,
    /// model id -> the answer used once the queue is empty
    defaults: Mutex<HashMap<String, Outcome>>,
    /// model id -> queued stream outcomes (consumed front to back)
    stream_outcomes: Mutex<HashMap<String, VecDeque<Result<StreamChunk, GatewayError>>>>,
    /// model id -> how often that model was dispatched
    calls_by_model: Mutex<HashMap<String, usize>>,
    /// every request the backend received, in dispatch order
    received: Mutex<Vec<(String, ChatCompletionRequest)>>,
    calls: AtomicUsize,
}

impl ScriptedBackend {
    fn new(name: &'static str) -> Self {
        Self {
            name,
            outcomes: Mutex::new(HashMap::new()),
            defaults: Mutex::new(HashMap::new()),
            stream_outcomes: Mutex::new(HashMap::new()),
            calls_by_model: Mutex::new(HashMap::new()),
            received: Mutex::new(Vec::new()),
            calls: AtomicUsize::new(0),
        }
    }

    fn push(&self, model: &str, outcome: Outcome) {
        self.outcomes
            .lock()
            .unwrap()
            .entry(model.to_string())
            .or_default()
            .push_back(outcome);
    }

    /// Answer `model` with `status` (a well-formed upstream refusal), once.
    fn fail_status(&self, model: &str, status: u16) {
        self.push(
            model,
            Err(GatewayError::Upstream(format!(
                "upstream status {status}: scripted refusal for {model}"
            ))),
        );
    }

    /// Answer `model` with `status`, `times` times in a row.
    fn fail_status_times(&self, model: &str, status: u16, times: usize) {
        for _ in 0..times {
            self.fail_status(model, status);
        }
    }

    /// Answer `model` with a free-form upstream message that carries **no**
    /// recoverable status (no `upstream status N:` prefix, no JSON payload).
    fn fail_bare(&self, model: &str, message: &str) {
        self.push(model, Err(GatewayError::Upstream(message.to_string())));
    }

    /// Answer `model` with `text` — for every call, not only the first.
    fn succeed(&self, model: &str, text: &str) {
        self.defaults
            .lock()
            .unwrap()
            .insert(model.to_string(), Ok(ok_response(model, text)));
    }

    /// Queue one stream outcome for `model`.
    fn push_stream(&self, model: &str, outcome: Result<StreamChunk, GatewayError>) {
        self.stream_outcomes
            .lock()
            .unwrap()
            .entry(model.to_string())
            .or_default()
            .push_back(outcome);
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::Relaxed)
    }

    fn calls_for(&self, model: &str) -> usize {
        self.calls_by_model
            .lock()
            .unwrap()
            .get(model)
            .copied()
            .unwrap_or(0)
    }

    fn requests_for(&self, model: &str) -> Vec<ChatCompletionRequest> {
        self.received
            .lock()
            .unwrap()
            .iter()
            .filter(|(m, _)| m == model)
            .map(|(_, r)| r.clone())
            .collect()
    }
}

#[async_trait]
impl ChatBackend for ScriptedBackend {
    fn name(&self) -> &'static str {
        self.name
    }

    async fn complete(
        &self,
        request: ChatCompletionRequest,
    ) -> Result<ChatCompletionResponse, GatewayError> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        let model = request.model.clone();
        {
            let mut counts = self.calls_by_model.lock().unwrap();
            *counts.entry(model.clone()).or_insert(0) += 1;
        }
        self.received
            .lock()
            .unwrap()
            .push((model.clone(), request.clone()));

        let mut guard = self.outcomes.lock().unwrap();
        let next = guard.get_mut(&model).and_then(VecDeque::pop_front);
        match next {
            Some(outcome) => outcome,
            None => self
                .defaults
                .lock()
                .unwrap()
                .get(&model)
                .cloned()
                .unwrap_or_else(|| {
                    Ok(ok_response(
                        &model,
                        &format!("Echo from {} for {model}", self.name),
                    ))
                }),
        }
    }

    fn stream(&self, request: ChatCompletionRequest) -> ChunkStream {
        self.calls.fetch_add(1, Ordering::Relaxed);
        let model = request.model.clone();
        {
            let mut counts = self.calls_by_model.lock().unwrap();
            *counts.entry(model.clone()).or_insert(0) += 1;
        }
        let mut guard = self.stream_outcomes.lock().unwrap();
        let items: Vec<Result<StreamChunk, GatewayError>> = match guard.get_mut(&model) {
            Some(queue) => queue.drain(..).collect(),
            None => vec![Ok(StreamChunk {
                content: Some(format!("Stream from {}", self.name)),
                finish_reason: Some("stop".into()),
                ..Default::default()
            })],
        };
        Box::pin(futures::stream::iter(items))
    }
}

fn ok_response(model: &str, text: &str) -> ChatCompletionResponse {
    ChatCompletionResponse {
        id: "resp-m5-2".into(),
        object: "chat.completion",
        created: 1,
        model: model.to_string(),
        usage: Usage::default(),
        choices: vec![ChatChoice {
            index: 0,
            message: ChatMessage::assistant_text(text.to_string()),
            finish_reason: "stop".into(),
        }],
        raw: None,
    }
}

/// A response that carries a pseudo-XML tool call instead of a structured one.
fn pseudo_xml_response(model: &str) -> ChatCompletionResponse {
    ChatCompletionResponse {
        id: "resp-m5-2-pseudo".into(),
        object: "chat.completion",
        created: 1,
        model: model.to_string(),
        usage: Usage::default(),
        choices: vec![ChatChoice {
            index: 0,
            message: ChatMessage::assistant_text(
                "<tool_call>{\"name\":\"read_file\",\"arguments\":{}}</tool_call>",
            ),
            finish_reason: "stop".into(),
        }],
        raw: None,
    }
}

/// A response carrying a structured tool call.
fn tool_call_response(model: &str) -> ChatCompletionResponse {
    let mut response = ok_response(model, "");
    response.choices[0].message = ChatMessage {
        role: "assistant".into(),
        content: None,
        reasoning_content: None,
        tool_calls: Some(json!([{
            "id": "call_m5_2",
            "type": "function",
            "function": {"name": "read_file", "arguments": "{}"}
        }])),
        tool_call_id: None,
        name: None,
    };
    response.choices[0].finish_reason = "tool_calls".into();
    response
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn auto_request() -> ChatCompletionRequest {
    ChatCompletionRequest {
        model: "heterion-router/auto".into(),
        messages: vec![ChatMessage::plain("user", "challenge request")],
        ..Default::default()
    }
}

fn auto_request_with_tools() -> ChatCompletionRequest {
    ChatCompletionRequest {
        model: "heterion-router/auto".into(),
        messages: vec![ChatMessage::plain("user", "read the file")],
        tools: Some(json!([{
            "type": "function",
            "function": {
                "name": "read_file",
                "description": "reads a file",
                "parameters": {"type": "object", "properties": {"path": {"type": "string"}}}
            }
        }])),
        ..Default::default()
    }
}

/// Build the backend map for the auto ladder providers used across the suite.
///
/// The rotation layer is switched off: this suite pins the *ladder* — which
/// model is tried first, what happens to the head after a failure — and the
/// rotation deliberately moves the head after every answer. The rotation has
/// its own contract (`tests/model_ladder.rs`).
fn ladder(backends: Vec<Arc<ScriptedBackend>>) -> RoutingBackend {
    let mut map: HashMap<String, Arc<dyn ChatBackend>> = HashMap::new();
    for backend in backends {
        map.insert(backend.name.to_string(), backend);
    }
    RoutingBackend::new(map, Router::new(vec![], vec![])).with_rotation(false)
}

fn gemini() -> Arc<ScriptedBackend> {
    Arc::new(ScriptedBackend::new("gemini"))
}

fn agentrouter() -> Arc<ScriptedBackend> {
    Arc::new(ScriptedBackend::new("agentrouter"))
}

fn mistral() -> Arc<ScriptedBackend> {
    Arc::new(ScriptedBackend::new("mistral"))
}

/// Position of `model` in a plan, if present.
fn plan_position(plan: &heterion_router_gateway::routing::Plan, model: &str) -> Option<usize> {
    plan.steps.iter().position(|(_, m)| m == model)
}

/// Total upstream calls across the given backends.
fn total_calls(backends: &[Arc<ScriptedBackend>]) -> usize {
    backends.iter().map(|b| b.calls()).sum()
}

// ---------------------------------------------------------------------------
// Suite A: the model-scoped 429 rule
// ---------------------------------------------------------------------------

/// I-1 — a 429 on one model must leave its siblings on the same provider usable.
#[tokio::test]
async fn test_429_on_one_model_leaves_siblings_on_the_same_provider_usable() {
    let gemini = gemini();
    // The Tier-1 model is rate limited; its Tier-2 sibling on the same provider
    // answers normally.
    gemini.fail_status("gemini-3-flash-preview", 429);
    gemini.succeed("gemma-4-26b-a4b-it", "sibling answered");

    let routing = ladder(vec![gemini.clone()]);

    let plan = routing.auto_plan(&auto_request());
    let flash = plan_position(&plan, "gemini-3-flash-preview").expect("tier 1 in plan");
    let gemma = plan_position(&plan, "gemma-4-26b-a4b-it").expect("tier 2 in plan");
    assert!(
        flash < gemma,
        "the tier-1 model must be tried before its tier-2 sibling"
    );

    let mut request = auto_request();
    let token = stamp_attribution(&mut request);
    let response = routing
        .complete(request)
        .await
        .expect("the request must succeed through the sibling model");

    assert_eq!(
        response.choices[0].message.text(),
        "sibling answered",
        "the sibling of the rate-limited model must serve the request"
    );
    assert_eq!(gemini.calls_for("gemini-3-flash-preview"), 1);
    assert_eq!(
        gemini.calls_for("gemma-4-26b-a4b-it"),
        1,
        "the sibling must actually have been dispatched"
    );

    // The cooldown is scoped to the model, not to the provider.
    let cooldowns = routing.cooldowns();
    assert!(
        cooldowns.is_in_cooldown("gemini", Some("gemini-3-flash-preview")),
        "the rate-limited model must be in cooldown"
    );
    assert!(
        !cooldowns.is_in_cooldown("gemini", Some("gemma-4-26b-a4b-it")),
        "the sibling must NOT be in cooldown"
    );
    assert!(
        !cooldowns.is_in_cooldown("gemini", None),
        "the provider must NOT be in cooldown: a rate limit on one model is not a provider fault"
    );

    // The next plan drops the rate-limited model and keeps the sibling.
    let next_plan = routing.auto_plan(&auto_request());
    assert!(
        plan_position(&next_plan, "gemini-3-flash-preview").is_none(),
        "the rate-limited model must leave the ladder"
    );
    assert!(
        plan_position(&next_plan, "gemma-4-26b-a4b-it").is_some(),
        "the sibling must stay in the ladder"
    );

    // Attribution names the model that actually served.
    let attribution = routing.attribution(&token);
    assert_eq!(attribution.provider.as_deref(), Some("gemini"));
    assert_eq!(attribution.model.as_deref(), Some("gemma-4-26b-a4b-it"));
}

/// I-2 — the Tier-3 high-quota protector must absorb a Tier-1 rate limit.
#[tokio::test]
async fn test_tier3_high_quota_protector_absorbs_a_tier1_rate_limit() {
    let gemini = gemini();
    let agentrouter = agentrouter();
    let mistral = mistral();

    // Every Tier-1 model is rate limited, and the Tier-2 models are too.
    gemini.fail_status("gemini-3-flash-preview", 429);
    gemini.fail_status("gemma-4-26b-a4b-it", 429);
    agentrouter.fail_status("gpt-6.1-sol", 429);
    agentrouter.fail_status("gpt-5.6-sol", 429);
    mistral.fail_status("codestral-latest", 429);
    // Only the Tier-3 high-quota protector is left standing.
    gemini.succeed("gemini-3.5-flash-lite", "tier 3 protector answered");

    let routing = ladder(vec![gemini.clone(), agentrouter.clone(), mistral.clone()]);

    let mut request = auto_request();
    let token = stamp_attribution(&mut request);
    let response = routing
        .complete(request)
        .await
        .expect("the tier-3 protector must answer when every tier-1 model is rate limited");

    assert_eq!(
        response.choices[0].message.text(),
        "tier 3 protector answered",
        "the tier-3 high-quota protector must absorb the tier-1 rate limit"
    );
    assert_eq!(gemini.calls_for("gemini-3.5-flash-lite"), 1);

    let attribution = routing.attribution(&token);
    assert_eq!(
        attribution.model.as_deref(),
        Some("gemini-3.5-flash-lite"),
        "the protector must be the attributed model"
    );

    // The rate limits must not have frozen the provider that owns the protector.
    assert!(
        !routing.cooldowns().is_in_cooldown("gemini", None),
        "a 429 storm on two gemini models must not freeze the whole provider"
    );
}

/// I-3 — a provider whose every model is rate limited must fall over to another
/// provider, and must not be frozen as a whole.
#[tokio::test]
async fn test_provider_with_every_model_rate_limited_falls_over_to_another_provider() {
    let gemini = gemini();
    let mistral = mistral();

    for model in [
        "gemini-3-flash-preview",
        "gemma-4-26b-a4b-it",
        "gemini-3.5-flash-lite",
        "gemini-3.1-flash-lite",
    ] {
        gemini.fail_status(model, 429);
    }
    mistral.succeed("codestral-latest", "other provider answered");

    let routing = ladder(vec![gemini.clone(), mistral.clone()]);

    let mut request = auto_request();
    let token = stamp_attribution(&mut request);
    let response = routing
        .complete(request)
        .await
        .expect("the request must fall over to the healthy provider");

    assert_eq!(
        response.choices[0].message.text(),
        "other provider answered"
    );
    let attribution = routing.attribution(&token);
    assert_eq!(attribution.provider.as_deref(), Some("mistral"));
    assert_eq!(attribution.model.as_deref(), Some("codestral-latest"));

    // Each gemini model carries its own cooldown; the provider as a whole is not
    // frozen, because no account-level fault was reported.
    assert!(
        !routing.cooldowns().is_in_cooldown("gemini", None),
        "per-model rate limits must not become a provider-wide freeze"
    );
    assert!(
        routing
            .cooldowns()
            .is_in_cooldown("gemini", Some("gemini-3-flash-preview")),
        "the rate-limited model must carry its own cooldown"
    );
}

/// I-4 — a 429 must never trip the breaker, however many arrive.
#[tokio::test]
async fn test_429_storm_never_trips_the_breaker() {
    let gemini = gemini();
    let mistral = mistral();
    for model in [
        "gemini-3-flash-preview",
        "gemma-4-26b-a4b-it",
        "gemini-3.5-flash-lite",
        "gemini-3.1-flash-lite",
    ] {
        gemini.fail_status(model, 429);
    }
    // The other provider is rate limited as well, so that all four gemini
    // models are really tried: four refusals on one provider would trip a
    // breaker that counted 429s.
    mistral.fail_status("codestral-latest", 429);

    let routing = ladder(vec![gemini.clone(), mistral.clone()]);

    let mut request = auto_request();
    let _ = stamp_attribution(&mut request);
    let outcome = routing.complete(request).await;
    assert!(
        outcome.is_err(),
        "with every candidate rate limited the request must fail"
    );
    assert_eq!(
        gemini.calls(),
        4,
        "every rate-limited model of the provider must have been tried"
    );

    let now = Instant::now();
    let mut breakers = routing.breakers().lock().unwrap();
    assert!(
        breakers.allow("gemini", now),
        "a rate-limited provider must not have its breaker tripped"
    );
    assert_eq!(
        breakers.state("gemini"),
        heterion_router_routing::breaker::BreakerState::Closed,
        "the breaker of a rate-limited provider must stay closed"
    );
    assert_eq!(
        breakers.state("mistral"),
        heterion_router_routing::breaker::BreakerState::Closed,
        "the breaker of a rate-limited provider must stay closed"
    );
    drop(breakers);

    // The rate limit is recorded per model, and never as a provider freeze.
    for model in [
        "gemini-3-flash-preview",
        "gemma-4-26b-a4b-it",
        "gemini-3.5-flash-lite",
        "gemini-3.1-flash-lite",
    ] {
        assert!(
            routing.cooldowns().is_in_cooldown("gemini", Some(model)),
            "model {model} must carry its own rate-limit cooldown"
        );
    }
    assert!(
        !routing.cooldowns().is_in_cooldown("gemini", None),
        "a 429 must never become a provider-wide freeze"
    );
}

/// I-5 — the 403 balance freeze stays provider-wide.
#[tokio::test]
async fn test_403_balance_freeze_stays_provider_wide() {
    let agentrouter = agentrouter();
    let mistral = mistral();
    agentrouter.fail_status("gpt-5.6-sol", 403);
    agentrouter.fail_status("gpt-6.1-sol", 403);
    mistral.succeed("codestral-latest", "other provider answered");

    let routing = ladder(vec![agentrouter.clone(), mistral.clone()]);

    let mut request = auto_request();
    let _ = stamp_attribution(&mut request);
    let response = routing
        .complete(request)
        .await
        .expect("the request must fail over to another provider");

    assert_eq!(
        response.choices[0].message.text(),
        "other provider answered"
    );
    assert_eq!(
        agentrouter.calls(),
        1,
        "the sibling model of a frozen provider must not be dispatched"
    );

    let cooldowns = routing.cooldowns();
    assert!(
        cooldowns.is_in_cooldown("agentrouter", None),
        "the balance freeze must be recorded at provider level"
    );
    for sibling in ["gpt-6.1-sol", "gpt-5.6-sol"] {
        assert!(
            cooldowns.is_in_cooldown("agentrouter", Some(sibling)),
            "the balance freeze must cover the sibling model {sibling}"
        );
    }
}

/// I-5b — the 402 balance freeze stays provider-wide too.
#[tokio::test]
async fn test_402_balance_freeze_stays_provider_wide() {
    let agentrouter = agentrouter();
    let mistral = mistral();
    agentrouter.fail_status("gpt-5.6-sol", 402);
    agentrouter.fail_status("gpt-6.1-sol", 402);
    mistral.succeed("codestral-latest", "other provider answered");

    let routing = ladder(vec![agentrouter.clone(), mistral.clone()]);

    let mut request = auto_request();
    let _ = stamp_attribution(&mut request);
    routing.complete(request).await.expect("failover must work");

    assert_eq!(agentrouter.calls(), 1);
    assert!(
        routing.cooldowns().is_in_cooldown("agentrouter", None),
        "a 402 must freeze the provider, not one model"
    );
    assert!(
        routing
            .cooldowns()
            .is_in_cooldown("agentrouter", Some("gpt-6.1-sol")),
        "a 402 must cover the sibling model as well"
    );
}

// ---------------------------------------------------------------------------
// Suite B: catalog integrity
// ---------------------------------------------------------------------------

/// I-6 — `GET /v1/models` must never contain the same id twice.
///
/// The shipped registry already carries a duplicate model id (`openai` lists
/// `gpt-4o` twice), so the dedupe is load-bearing on real data — not only on
/// hand-made collisions.
#[tokio::test]
async fn test_catalog_never_lists_the_same_id_twice() {
    let db = Arc::new(Db::open_in_memory().unwrap());
    db.migrate().unwrap();
    {
        let guard = db.connection();
        // A combo named like the router's own alias, and one named like a
        // `provider/model` id of the live registry.
        for (id, name) in [
            ("combo-auto", "auto"),
            ("combo-canonical", "heterion-router/auto"),
            ("combo-provider-model", "openai/gpt-4o"),
        ] {
            repos::upsert_combo(
                &guard,
                &ComboRow {
                    id: id.to_string(),
                    name: name.to_string(),
                    data: json!({"isHidden": false, "models": []}).to_string(),
                    sort_order: 0,
                    active: true,
                },
            )
            .unwrap();
        }
    }

    let state = AppState::with_db(Arc::new(ScriptedBackend::new("mock")), db).with_catalog(
        Arc::new(ProviderRegistry::load()),
        vec!["openai".to_string(), "gemini".to_string()],
    );
    let router = build_router_with_state(state);

    let response = router
        .oneshot(
            Request::builder()
                .uri("/v1/models")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let catalog: Value = serde_json::from_slice(&bytes).unwrap();
    let entries = catalog["data"].as_array().expect("data must be an array");

    let mut seen = std::collections::HashSet::new();
    for entry in entries {
        let id = entry["id"].as_str().expect("every entry needs an id");
        assert!(seen.insert(id.to_string()), "id `{id}` is listed twice");
    }

    // The registry really does ship a duplicated model id — prove the case is
    // not vacuous.
    assert!(
        entries.iter().any(|e| e["id"] == "openai/gpt-4o"),
        "the live registry's provider/model ids must be present in the catalog"
    );
}

/// I-7 — the `auto` and `heterion-router/auto` entries keep their capabilities
/// block, even when a combo carries the same name.
#[tokio::test]
async fn test_auto_entries_keep_their_capabilities_block() {
    let db = Arc::new(Db::open_in_memory().unwrap());
    db.migrate().unwrap();
    {
        let guard = db.connection();
        for name in ["auto", "heterion-router/auto"] {
            repos::upsert_combo(
                &guard,
                &ComboRow {
                    id: format!("combo-{name}"),
                    name: name.to_string(),
                    data: json!({"isHidden": false, "models": []}).to_string(),
                    sort_order: 0,
                    active: true,
                },
            )
            .unwrap();
        }
    }

    let state = AppState::with_db(Arc::new(ScriptedBackend::new("mock")), db);
    let router = build_router_with_state(state);

    let response = router
        .oneshot(
            Request::builder()
                .uri("/v1/models")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let catalog: Value = serde_json::from_slice(&bytes).unwrap();
    let entries = catalog["data"].as_array().unwrap();

    for id in ["auto", "heterion-router/auto"] {
        let entry = entries
            .iter()
            .find(|entry| entry["id"] == id)
            .unwrap_or_else(|| panic!("{id} must be in the catalog"));
        assert_eq!(
            entry["heterion-router"]["kind"], "auto",
            "{id} must be advertised as an auto model, not as a combo stub"
        );
        assert_eq!(entry["capabilities"]["context_window"], 1_000_000);
        assert_eq!(entry["capabilities"]["supports_vision"], true);
        assert_eq!(entry["capabilities"]["tool_support"], "native");
        assert_eq!(entry["capabilities"]["streaming"], true);
        assert_eq!(entry["capabilities"]["zero_quota_pruning"], true);
        assert_eq!(entry["capabilities"]["dynamic_ladder"], true);
        assert_eq!(
            entry["heterion-router"]["capabilities"]["dynamic_ladder"], true,
            "{id} must keep the nested capabilities block"
        );
    }
}

// ---------------------------------------------------------------------------
// Suite C: ladder integrity under stress
// ---------------------------------------------------------------------------

/// I-8 — a 429 storm, a 403 freeze and a 5xx outage at the same time must still
/// leave the router with a healthy candidate.
#[tokio::test]
async fn test_429_storm_plus_403_plus_5xx_still_answers_with_a_healthy_candidate() {
    let gemini = gemini();
    let agentrouter = agentrouter();
    let mistral = mistral();

    // 429 storm over the Tier-1 gemini model and the Tier-2 gemini model.
    gemini.fail_status("gemini-3-flash-preview", 429);
    gemini.fail_status("gemma-4-26b-a4b-it", 429);
    // A 5xx outage on the other tier-3 gemini protector.
    gemini.fail_status("gemini-3.5-flash-lite", 502);
    // A balance freeze on one agentrouter model — provider-wide, so the other
    // agentrouter model is frozen with it.
    agentrouter.fail_status("gpt-6.1-sol", 403);
    // A 5xx outage on the third provider.
    mistral.fail_status("codestral-latest", 502);
    // The only healthy candidate left: the second tier-3 protector, a **sibling
    // of the rate-limited models on the same provider**.
    gemini.succeed("gemini-3.1-flash-lite", "healthy candidate answered");

    let routing = ladder(vec![gemini.clone(), agentrouter.clone(), mistral.clone()]);

    let mut request = auto_request();
    let token = stamp_attribution(&mut request);
    let response = routing
        .complete(request)
        .await
        .expect("the router must still answer under a combined fault storm");

    assert_eq!(
        response.choices[0].message.text(),
        "healthy candidate answered",
        "the answer must come from a healthy candidate"
    );
    let attribution = routing.attribution(&token);
    assert_eq!(attribution.provider.as_deref(), Some("gemini"));
    assert_eq!(
        attribution.model.as_deref(),
        Some("gemini-3.1-flash-lite"),
        "the surviving tier-3 protector must serve the request"
    );
}

/// I-9 — a tool-bearing request is never served a pseudo-XML answer: the
/// post-flight check must reject it and fail over.
#[tokio::test]
async fn test_tool_bearing_request_is_never_served_a_pseudo_xml_answer() {
    let gemini = gemini();
    gemini.push(
        "gemini-3-flash-preview",
        Ok(pseudo_xml_response("gemini-3-flash-preview")),
    );
    gemini.succeed("gemma-4-26b-a4b-it", "structured answer");

    let routing = ladder(vec![gemini.clone()]);

    let mut request = auto_request_with_tools();
    let token = stamp_attribution(&mut request);
    let response = routing
        .complete(request)
        .await
        .expect("the pseudo-XML answer must be rejected and the request failed over");

    let text = response.choices[0].message.text().to_lowercase();
    assert!(
        !text.contains("<tool_call"),
        "a pseudo-XML answer must never reach the client: {text}"
    );
    assert_eq!(
        response.choices[0].message.text(),
        "structured answer",
        "the failover candidate must serve the request"
    );
    assert_eq!(gemini.calls_for("gemini-3-flash-preview"), 1);
    assert_eq!(gemini.calls_for("gemma-4-26b-a4b-it"), 1);

    let attribution = routing.attribution(&token);
    assert_eq!(attribution.model.as_deref(), Some("gemma-4-26b-a4b-it"));

    // The rejected model is penalised, so it leaves the ladder.
    assert!(
        routing
            .quality_tracker()
            .get_score("gemini-3-flash-preview")
            < 1.0,
        "the pseudo-XML model must be penalised"
    );
}

/// I-9b — a structured tool call is passed through untouched (the post-flight
/// check must not reject a legitimate tool call).
#[tokio::test]
async fn test_structured_tool_call_is_served_unchanged() {
    let gemini = gemini();
    gemini.push(
        "gemini-3-flash-preview",
        Ok(tool_call_response("gemini-3-flash-preview")),
    );

    let routing = ladder(vec![gemini.clone()]);

    let mut request = auto_request_with_tools();
    let token = stamp_attribution(&mut request);
    let response = routing
        .complete(request)
        .await
        .expect("a structured tool call must be served");

    assert!(
        response.choices[0].message.has_tool_calls(),
        "the structured tool call must survive"
    );
    assert_eq!(gemini.calls(), 1);
    assert_eq!(
        routing.attribution(&token).model.as_deref(),
        Some("gemini-3-flash-preview")
    );
}

// ---------------------------------------------------------------------------
// Suite D: the zero-quota guarantee
// ---------------------------------------------------------------------------

/// I-10 — pruning consumes no upstream quota and issues no extra call.
#[tokio::test]
async fn test_pruning_burns_no_quota_and_issues_no_extra_call() {
    let mistral = mistral();
    mistral.succeed("codestral-latest", "compressed answer");

    let routing = ladder(vec![mistral.clone()]);

    // A history far beyond the 256k window of `codestral-latest`.
    let mut messages = Vec::new();
    messages.push(ChatMessage::plain("system", "You are an expert engineer."));
    messages.push(ChatMessage::plain("user", "Refactor the whole repository."));
    let body = "x".repeat(150_000);
    for index in 0..8 {
        let call_id = format!("call_{index}");
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
            content: Some(Value::String(body.clone())),
            reasoning_content: None,
            tool_calls: None,
            tool_call_id: Some(call_id),
            name: None,
        });
    }
    messages.push(ChatMessage::plain("user", "Execute the last step"));
    messages.push(ChatMessage::plain("assistant", "Step done"));
    messages.push(ChatMessage::plain("user", "Execute the final step"));
    messages.push(ChatMessage::plain("assistant", "Proceeding"));

    let request = ChatCompletionRequest {
        model: "heterion-router/auto".into(),
        messages,
        ..Default::default()
    };

    let original = heterion_router_gateway::context_pruner::fast_estimate_tokens(&request);
    assert!(
        original > 256_000,
        "the fixture must exceed the candidate window (was {original})"
    );

    let original_messages = request.messages.clone();
    let mut request = request;
    let token = stamp_attribution(&mut request);
    let response = routing
        .complete(request.clone())
        .await
        .expect("the pruned request must be served");

    assert_eq!(response.choices[0].message.text(), "compressed answer");
    assert_eq!(
        mistral.calls(),
        1,
        "pruning must not issue an extra upstream call"
    );

    let sent = mistral.requests_for("codestral-latest");
    assert_eq!(sent.len(), 1, "exactly one upstream request must be sent");
    let sent_tokens = heterion_router_gateway::context_pruner::fast_estimate_tokens(&sent[0]);
    assert!(
        sent_tokens < original,
        "the upstream prompt must be smaller than the original ({sent_tokens} vs {original})"
    );
    assert!(
        sent_tokens <= 256_000,
        "the pruned prompt must fit the candidate window (was {sent_tokens})"
    );

    // The tool_call_id parity survives the fold.
    let ids = |messages: &[ChatMessage]| -> Vec<String> {
        messages
            .iter()
            .filter_map(|m| m.tool_call_id.clone())
            .collect()
    };
    assert_eq!(
        ids(&sent[0].messages),
        ids(&original_messages),
        "tool_call_id parity must survive the pruning"
    );

    // And the router booked the compression.
    let attribution = routing.attribution(&token);
    assert_eq!(attribution.provider.as_deref(), Some("mistral"));
    assert_eq!(
        attribution.tokens_compressed,
        Some(original - sent_tokens),
        "the compressed token count must be attributed"
    );
}

/// I-11 — one client request produces exactly one upstream call when the first
/// candidate succeeds.
#[tokio::test]
async fn test_one_client_request_produces_exactly_one_upstream_call() {
    let gemini = gemini();
    let agentrouter = agentrouter();
    let mistral = mistral();

    let routing = ladder(vec![gemini.clone(), agentrouter.clone(), mistral.clone()]);

    let mut request = auto_request();
    let token = stamp_attribution(&mut request);
    let response = routing.complete(request).await.expect("must succeed");

    assert_eq!(
        response.choices[0].message.text(),
        "Echo from gemini for gemini-3-flash-preview"
    );
    assert_eq!(
        total_calls(&[gemini.clone(), agentrouter.clone(), mistral.clone()]),
        1,
        "a successful first candidate must not trigger any further upstream call"
    );
    assert_eq!(gemini.calls(), 1);
    assert_eq!(agentrouter.calls(), 0);
    assert_eq!(mistral.calls(), 0);
    assert_eq!(
        routing.attribution(&token).model.as_deref(),
        Some("gemini-3-flash-preview")
    );
}

// ---------------------------------------------------------------------------
// Suite E: regression traps
// ---------------------------------------------------------------------------

/// I-12 — a 4xx must not trip the breaker and must not take down the provider's
/// other models.
#[tokio::test]
async fn test_4xx_never_trips_the_breaker_and_never_takes_down_siblings() {
    let gemini = gemini();
    let mistral = mistral();
    for _ in 0..5 {
        gemini.fail_status("gemini-3-flash-preview", 404);
    }
    mistral.succeed("codestral-latest", "backup answered");

    let routing = ladder(vec![gemini.clone(), mistral.clone()]);

    for _ in 0..5 {
        let mut request = auto_request();
        let _ = stamp_attribution(&mut request);
        routing
            .complete(request)
            .await
            .expect("a 4xx must fail over, not break the request");
    }

    {
        let now = Instant::now();
        let mut breakers = routing.breakers().lock().unwrap();
        assert!(
            breakers.allow("gemini", now),
            "a 4xx must never trip the breaker"
        );
        assert_eq!(
            breakers.state("gemini"),
            heterion_router_routing::breaker::BreakerState::Closed,
            "a 4xx must never open the breaker"
        );
    }

    // The sibling model of the same provider is still usable.
    let mut request = auto_request();
    let _ = stamp_attribution(&mut request);
    let response = routing.complete(request).await.expect("must succeed");
    assert!(
        response.choices[0]
            .message
            .text()
            .contains("Echo from gemini"),
        "the sibling model of the provider must still serve, got: {}",
        response.choices[0].message.text()
    );
}

/// I-13 — a 5xx outage isolates the backend through the breaker.
#[tokio::test]
async fn test_5xx_outage_isolates_the_backend_via_the_breaker() {
    let gemini = gemini();
    let mistral = mistral();
    for model in [
        "gemini-3-flash-preview",
        "gemma-4-26b-a4b-it",
        "gemini-3.5-flash-lite",
        "gemini-3.1-flash-lite",
    ] {
        gemini.fail_status(model, 502);
    }
    mistral.fail_status("codestral-latest", 502);

    let routing = ladder(vec![gemini.clone(), mistral.clone()]);

    let mut request = auto_request();
    let _ = stamp_attribution(&mut request);
    let outcome = routing.complete(request).await;
    assert!(
        outcome.is_err(),
        "with every candidate dead the request must fail"
    );

    let now = Instant::now();
    let mut breakers = routing.breakers().lock().unwrap();
    assert_eq!(
        breakers.state("gemini"),
        heterion_router_routing::breaker::BreakerState::Open,
        "repeated 5xx must open the breaker of the dead backend"
    );
    assert!(!breakers.allow("gemini", now));
    drop(breakers);

    assert!(
        !routing.cooldowns().is_in_cooldown("gemini", None),
        "a 5xx outage is a breaker matter, not a cooldown matter"
    );
}

// ---------------------------------------------------------------------------
// Suite F: concurrency
// ---------------------------------------------------------------------------

/// I-14 — many simultaneous requests must not corrupt the cooldown state, the
/// quality scores or the attribution map.
#[tokio::test]
async fn test_concurrent_requests_keep_cooldown_quality_and_attribution_intact() {
    let gemini = gemini();
    // The Tier-1 model is rate limited, its Tier-2 sibling answers. Every
    // concurrent request must hit the scripted refusal, so the queue is filled
    // for all of them.
    gemini.fail_status_times("gemini-3-flash-preview", 429, 64);
    gemini.succeed("gemma-4-26b-a4b-it", "sibling answered");

    let routing = Arc::new(ladder(vec![gemini.clone()]));

    const CONCURRENT: usize = 32;
    let mut handles = Vec::with_capacity(CONCURRENT);
    for index in 0..CONCURRENT {
        let routing = routing.clone();
        handles.push(tokio::spawn(async move {
            let mut request = auto_request();
            let token = stamp_attribution(&mut request);
            let response = routing.complete(request).await?;
            let attribution = routing.attribution(&token);
            Ok::<_, GatewayError>((index, response, attribution))
        }));
    }

    for handle in handles {
        let (index, response, attribution) = handle.await.unwrap().unwrap();
        assert_eq!(
            response.choices[0].message.text(),
            "sibling answered",
            "request {index} must be served by the sibling of the rate-limited model"
        );
        assert_eq!(
            attribution.provider.as_deref(),
            Some("gemini"),
            "request {index} must be attributed to its own provider"
        );
        assert_eq!(
            attribution.model.as_deref(),
            Some("gemma-4-26b-a4b-it"),
            "request {index} must be attributed to its own model"
        );
    }

    // The cooldown state is coherent: the rate-limited model is cooled down, the
    // provider is not frozen.
    assert!(
        routing
            .cooldowns()
            .is_in_cooldown("gemini", Some("gemini-3-flash-preview")),
        "the rate-limited model must carry a cooldown"
    );
    assert!(
        !routing.cooldowns().is_in_cooldown("gemini", None),
        "a per-model rate limit must never become a provider-wide freeze"
    );
    assert!(
        !routing
            .cooldowns()
            .is_in_cooldown("gemini", Some("gemma-4-26b-a4b-it")),
        "the healthy sibling must not be cooled down"
    );

    // The quality score of the healthy model is untouched.
    assert_eq!(
        routing.quality_tracker().get_score("gemma-4-26b-a4b-it"),
        1.0,
        "the quality score of the healthy model must not drift"
    );
}

/// I-15 — concurrent rate limits on one model must not leak into the sibling.
#[tokio::test]
async fn test_concurrent_rate_limits_never_disable_the_sibling_model() {
    let gemini = gemini();
    gemini.fail_status_times("gemini-3-flash-preview", 429, 64);
    gemini.succeed("gemma-4-26b-a4b-it", "sibling answered");

    let routing = Arc::new(ladder(vec![gemini.clone()]));

    const CONCURRENT: usize = 32;
    let mut handles = Vec::with_capacity(CONCURRENT);
    for _ in 0..CONCURRENT {
        let routing = routing.clone();
        handles.push(tokio::spawn(async move {
            let mut request = auto_request();
            let token = stamp_attribution(&mut request);
            let response = routing.complete(request).await?;
            let attribution = routing.attribution(&token);
            Ok::<_, GatewayError>((response, attribution))
        }));
    }

    for handle in handles {
        let (response, attribution) = handle.await.unwrap().unwrap();
        assert_eq!(
            response.choices[0].message.text(),
            "sibling answered",
            "every concurrent request must be served by the sibling model"
        );
        assert_eq!(
            attribution.model.as_deref(),
            Some("gemma-4-26b-a4b-it"),
            "every concurrent request must be attributed to the sibling model"
        );
    }

    assert!(
        !routing.cooldowns().is_in_cooldown("gemini", None),
        "concurrent per-model rate limits must not freeze the provider"
    );
}

// ---------------------------------------------------------------------------
// Suite G: the streaming path, Retry-After, and status-less errors
// ---------------------------------------------------------------------------

/// I-16 — the streaming path honours the model-scoped 429 as well: the sibling
/// of a rate-limited model must serve the stream.
#[tokio::test]
async fn test_streaming_path_honours_the_model_scoped_429() {
    let gemini = gemini();
    gemini.push_stream(
        "gemini-3-flash-preview",
        Err(GatewayError::Upstream(
            "upstream status 429: slow down".into(),
        )),
    );
    gemini.push_stream(
        "gemma-4-26b-a4b-it",
        Ok(StreamChunk {
            content: Some("stream from the sibling".into()),
            finish_reason: Some("stop".into()),
            ..Default::default()
        }),
    );

    let routing = ladder(vec![gemini.clone()]);

    let mut request = auto_request();
    request.stream = true;
    let token = stamp_attribution(&mut request);

    let mut stream = routing.stream(request);
    let mut text = String::new();
    let mut error = None;
    while let Some(item) = futures::StreamExt::next(&mut stream).await {
        match item {
            Ok(chunk) => {
                if let Some(content) = chunk.content {
                    text.push_str(&content);
                }
            }
            Err(err) => {
                error = Some(err);
                break;
            }
        }
    }

    assert!(error.is_none(), "the stream must not fail: {error:?}");
    assert_eq!(text, "stream from the sibling");
    assert_eq!(gemini.calls_for("gemini-3-flash-preview"), 1);
    assert_eq!(gemini.calls_for("gemma-4-26b-a4b-it"), 1);

    // The cooldown is model-scoped here too.
    assert!(
        routing
            .cooldowns()
            .is_in_cooldown("gemini", Some("gemini-3-flash-preview")),
        "the streamed rate limit must be recorded for the model"
    );
    assert!(
        !routing.cooldowns().is_in_cooldown("gemini", None),
        "a streamed rate limit must not freeze the provider"
    );
    assert!(!routing.cooldowns().is_in_cooldown("gemini", None));
    let attribution = routing.attribution(&token);
    assert_eq!(attribution.model.as_deref(), Some("gemma-4-26b-a4b-it"));
}

/// I-17 — a `Retry-After` on a 429 sets the model-scoped cooldown to that
/// duration and leaves the sibling alone.
#[tokio::test]
async fn test_retry_after_sets_the_model_scoped_cooldown_only() {
    let gemini = gemini();
    gemini.fail_status("gemini-3-flash-preview", 429);
    gemini.succeed("gemma-4-26b-a4b-it", "sibling answered");

    let routing = ladder(vec![gemini.clone()]);

    // The upstream names its own retry delay.
    gemini.outcomes.lock().unwrap().clear();
    gemini.push(
        "gemini-3-flash-preview",
        Err(GatewayError::Upstream(
            "upstream status 429: retry-after: 90 slow down".into(),
        )),
    );

    let mut request = auto_request();
    let _ = stamp_attribution(&mut request);
    let response = routing.complete(request).await.expect("must succeed");
    assert_eq!(response.choices[0].message.text(), "sibling answered");

    let remaining = routing
        .cooldowns()
        .get_cooldown_remaining("gemini", Some("gemini-3-flash-preview"));
    let remaining = remaining.expect("the rate-limited model must be in cooldown");
    assert!(
        remaining >= Duration::from_secs(85) && remaining <= Duration::from_secs(90),
        "the Retry-After delay must be honoured, got {remaining:?}"
    );
    assert!(
        routing
            .cooldowns()
            .get_cooldown_remaining("gemini", Some("gemma-4-26b-a4b-it"))
            .is_none(),
        "the sibling must carry no cooldown"
    );
    assert!(
        routing
            .cooldowns()
            .get_cooldown_remaining("gemini", None)
            .is_none(),
        "the provider must carry no cooldown"
    );
}

/// I-18 — an upstream error that carries no recoverable status is a backend
/// outage: the breaker isolates that backend.
///
/// This is the documented rule of `is_backend_outage` ("no status means the
/// call never landed"), and it is the reason the 429/403 branches must be able
/// to recover a status: an error without one is treated as a dead backend.
#[tokio::test]
async fn test_upstream_error_without_a_status_is_treated_as_a_backend_outage() {
    let gemini = gemini();
    let mistral = mistral();
    for _ in 0..4 {
        gemini.fail_bare("gemini-3-flash-preview", "connection reset by peer");
    }
    mistral.succeed("codestral-latest", "other provider answered");

    let routing = ladder(vec![gemini.clone(), mistral.clone()]);

    for _ in 0..4 {
        let mut request = auto_request();
        let _ = stamp_attribution(&mut request);
        routing.complete(request).await.expect("failover must work");
    }

    let now = Instant::now();
    let mut breakers = routing.breakers().lock().unwrap();
    assert!(
        !breakers.allow("gemini", now),
        "an unreachable backend must be isolated by the breaker"
    );
    drop(breakers);
    assert!(
        !routing.cooldowns().is_in_cooldown("gemini", None),
        "an outage is a breaker matter, not a cooldown matter"
    );
}

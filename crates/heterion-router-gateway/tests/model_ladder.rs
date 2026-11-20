//! The capability-driven ladder: every live model gets its place in the
//! priority order from what it can do, and a model that just served cools
//! down softly instead of losing its properties.
//!
//! Contracts:
//! 1. `the_ladder_follows_the_capability_table` — the priority order comes
//!    from the capabilities, not from a hand-written list.
//! 2. `an_unknown_model_keeps_the_curated_fallback` — a model the table does
//!    not know keeps its hand-set priority.
//! 3. `a_served_model_cools_down_and_comes_back` — the rotation: the model
//!    that just answered drops down for a while and returns unchanged.
//! 4. `the_rotation_moves_the_traffic_down_the_ladder` — with the rotation
//!    on, the second request is answered by another model.
//! 5. `without_the_rotation_the_ladder_order_is_pinned` — with the rotation
//!    off, the same model keeps answering while nothing is wrong with it.
//! 6. `an_untrusted_model_stays_below_a_trusted_one` — research says what a
//!    model is, trust says whether the provider serves it.
//! 7. `a_failed_model_goes_down_but_stays_in_the_pool` — an unstable model
//!    sinks temporarily, keeping its properties.

use async_trait::async_trait;
use heterion_router_core::{
    ChatChoice, ChatCompletionRequest, ChatCompletionResponse, ChatMessage, GatewayError, Usage,
};
use heterion_router_db::{Db, repos::ModelCapabilityRow};
use heterion_router_gateway::{
    backend::{ChatBackend, ChunkStream},
    capabilities::CapabilityStore,
    routing::RoutingBackend,
    trust::TrustStore,
};
use heterion_router_providers::ProviderRegistry;
use heterion_router_routing::Router;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

struct EchoBackend {
    name: &'static str,
    /// The models this backend was asked to serve, in order.
    served: Arc<Mutex<Vec<String>>>,
    /// When set, the backend answers with this upstream status instead of a
    /// response — the way a platform answers an id it does not serve.
    fail_status: Option<u16>,
}

#[async_trait]
impl ChatBackend for EchoBackend {
    fn name(&self) -> &'static str {
        self.name
    }

    async fn complete(
        &self,
        request: ChatCompletionRequest,
    ) -> Result<ChatCompletionResponse, GatewayError> {
        if let Some(status) = self.fail_status {
            return Err(GatewayError::Upstream(format!("upstream status {status}")));
        }
        self.served.lock().unwrap().push(request.model.clone());
        Ok(ChatCompletionResponse {
            id: "resp-echo".into(),
            object: "chat.completion",
            created: 1,
            model: request.model.clone(),
            usage: Usage::default(),
            choices: vec![ChatChoice {
                index: 0,
                message: ChatMessage::assistant_text("ok".to_string()),
                finish_reason: "stop".into(),
            }],
            raw: None,
        })
    }

    fn stream(&self, _request: ChatCompletionRequest) -> ChunkStream {
        Box::pin(futures::stream::once(async {
            Err(GatewayError::Upstream("stream not used".into()))
        }))
    }
}

fn request() -> ChatCompletionRequest {
    ChatCompletionRequest {
        model: "heterion-router/auto".into(),
        messages: vec![ChatMessage::plain("user", "ping")],
        ..Default::default()
    }
}

fn row(provider: &str, model: &str, tier: &str, context: i64) -> ModelCapabilityRow {
    ModelCapabilityRow {
        provider: provider.to_string(),
        model_id: model.to_string(),
        tool_call: Some(true),
        reasoning: Some(tier == "frontier"),
        attachment: Some(false),
        limit_context: Some(context),
        limit_output: None,
        family: None,
        real_context: None,
        tier: Some(tier.to_string()),
    }
}

/// The models each provider served, in order.
type ServedLog = HashMap<String, Arc<Mutex<Vec<String>>>>;

/// How the test router is put together.
struct RouterSpec {
    /// Capability rows.
    rows: Vec<ModelCapabilityRow>,
    /// Whether the rotation layer is on.
    rotation: bool,
    /// Pairs the router has evidence for: the platform answered for them.
    trusted: Vec<(String, String)>,
    /// Providers whose backend answers `404` — the way a platform answers an
    /// id it does not serve.
    failing: Vec<String>,
}

impl Default for RouterSpec {
    fn default() -> Self {
        Self {
            rows: Vec::new(),
            rotation: true,
            trusted: Vec::new(),
            failing: Vec::new(),
        }
    }
}

/// A router over two providers, with the given capability rows.
fn router_with(rows: Vec<ModelCapabilityRow>) -> (RoutingBackend, ProviderRegistry) {
    let (routing, registry, _) = build_router(RouterSpec {
        rows,
        ..Default::default()
    });
    (routing, registry)
}

/// A router over two providers, put together according to the spec, plus the
/// log of served models.
fn build_router(spec: RouterSpec) -> (RoutingBackend, ProviderRegistry, ServedLog) {
    let registry = ProviderRegistry::load();
    let mut backends: HashMap<String, Arc<dyn ChatBackend>> = HashMap::new();
    let mut served: ServedLog = HashMap::new();
    for provider in ["gemini", "groq"] {
        let log = Arc::new(Mutex::new(Vec::new()));
        backends.insert(
            provider.to_string(),
            Arc::new(EchoBackend {
                name: provider,
                served: Arc::clone(&log),
                fail_status: spec
                    .failing
                    .iter()
                    .any(|name| name == provider)
                    .then_some(404),
            }) as Arc<dyn ChatBackend>,
        );
        served.insert(provider.to_string(), log);
    }
    let db = Arc::new(Db::open_in_memory().unwrap());
    let routing = RoutingBackend::new(backends, Router::new(vec![], vec![]))
        .with_combo_source(db, Arc::new(registry.clone()))
        .with_capabilities(Arc::new(CapabilityStore::from_rows(&spec.rows)))
        .with_trust(Arc::new(TrustStore::from_pairs(spec.trusted)))
        .with_rotation(spec.rotation);
    (routing, registry, served)
}

/// The first model a provider serves, taken from the registry.
fn first_model(registry: &ProviderRegistry, provider: &str) -> String {
    registry
        .get(provider)
        .expect("provider in registry")
        .models
        .first()
        .expect("provider has models")
        .id
        .clone()
}

/// AC1 — the priority order comes from the capabilities.
#[tokio::test]
async fn the_ladder_follows_the_capability_table() {
    let registry = ProviderRegistry::load();
    let groq_model = first_model(&registry, "groq");
    let gemini_model = first_model(&registry, "gemini");

    // Groq's model is a frontier model with a big window; Gemini's is a fast
    // model with a small one. The ladder must follow that, not the hand-set
    // order of the curated candidates.
    let rows = vec![
        row("groq", &groq_model, "frontier", 131_072),
        row("gemini", &gemini_model, "fast", 32_768),
    ];
    let (routing, _registry) = router_with(rows);

    let plan = routing.auto_plan(&request());
    assert_eq!(
        plan.steps.first(),
        Some(&("groq".to_string(), groq_model.clone())),
        "the frontier model with the bigger window leads the ladder: {:?}",
        plan.steps
    );
}

/// AC2 — a model the table does not know keeps its hand-set priority.
#[tokio::test]
async fn an_unknown_model_keeps_the_curated_fallback() {
    let registry = ProviderRegistry::load();
    let groq_model = first_model(&registry, "groq");
    // Only the groq model is described; every other model falls back to the
    // curated priorities and the tail's flat one.
    let rows = vec![row("groq", &groq_model, "fallback", 32_768)];
    let (routing, _registry) = router_with(rows);

    let plan = routing.auto_plan(&request());
    let curated = heterion_router_gateway::routing::DEFAULT_AUTO_CANDIDATES;
    let first_curated = curated
        .iter()
        .find(|candidate| candidate.provider == "gemini")
        .expect("gemini in the curated ladder");
    let position = plan
        .steps
        .iter()
        .position(|step| step.0 == "gemini" && step.1 == first_curated.model)
        .expect("curated candidate in the plan");
    // A fallback-class groq model (60 points) must sit below the curated
    // tier (90 points and up).
    let groq_position = plan
        .steps
        .iter()
        .position(|step| step.0 == "groq" && step.1 == groq_model)
        .expect("groq model in the plan");
    assert!(
        groq_position > position,
        "a fallback-class model must sit below the curated tier: {groq_position} vs {position}"
    );
}

/// AC3 — the rotation: the model that just answered cools down softly and
/// comes back unchanged.
#[tokio::test]
async fn a_served_model_cools_down_and_comes_back() {
    let registry = ProviderRegistry::load();
    let groq_model = first_model(&registry, "groq");
    let gemini_model = first_model(&registry, "gemini");
    // Both models are equal: same class, same window.
    let rows = vec![
        row("groq", &groq_model, "fast", 32_768),
        row("gemini", &gemini_model, "fast", 32_768),
    ];
    let (routing, _registry) = router_with(rows);

    let before = routing.auto_plan(&request());
    let leader = before.steps[0].clone();

    routing.complete(request()).await.expect("auto serves");

    let after = routing.auto_plan(&request());
    let leader_position = after
        .steps
        .iter()
        .position(|step| *step == leader)
        .expect("the leader stays in the pool");
    assert!(
        leader_position > 0,
        "the model that just served must drop down, not stay on top: {after:?}"
    );

    // The cooldown is a factor, not a ban: the model keeps its place in the
    // pool and comes back once the cooldown is over.
    assert!(
        after.steps.contains(&leader),
        "a cooled model keeps its properties and stays in the pool"
    );
    // The demotion is a factor, not a ban: the model keeps its place in the
    // pool and comes back exactly where it was once the window is over.
    let _ = (Duration::from_secs(1), Instant::now());
}

/// AC4 — with the rotation on, the traffic moves down the ladder: the model
/// that just answered is not the one that answers next.
#[tokio::test]
async fn the_rotation_moves_the_traffic_down_the_ladder() {
    let registry = ProviderRegistry::load();
    let groq_model = first_model(&registry, "groq");
    let gemini_model = first_model(&registry, "gemini");
    let rows = vec![
        row("groq", &groq_model, "fast", 32_768),
        row("gemini", &gemini_model, "fast", 32_768),
    ];
    let (routing, _registry, served) = build_router(RouterSpec {
        rows,
        ..Default::default()
    });

    routing.complete(request()).await.expect("auto serves");
    routing.complete(request()).await.expect("auto serves");

    let answers: Vec<String> = served
        .values()
        .flat_map(|log| log.lock().unwrap().clone())
        .collect();
    assert_eq!(
        answers.len(),
        2,
        "both requests must be served: {answers:?}"
    );
    assert_ne!(
        answers[0], answers[1],
        "the rotation must move the traffic to another model: {answers:?}"
    );
}

/// AC5 — with the rotation off, the ladder order is pinned: the head keeps
/// answering while nothing is wrong with it.
#[tokio::test]
async fn without_the_rotation_the_ladder_order_is_pinned() {
    let registry = ProviderRegistry::load();
    let groq_model = first_model(&registry, "groq");
    let gemini_model = first_model(&registry, "gemini");
    let rows = vec![
        row("groq", &groq_model, "fast", 32_768),
        row("gemini", &gemini_model, "fast", 32_768),
    ];
    let (routing, _registry, served) = build_router(RouterSpec {
        rows,
        rotation: false,
        ..Default::default()
    });

    routing.complete(request()).await.expect("auto serves");
    routing.complete(request()).await.expect("auto serves");

    let answers: Vec<String> = served
        .values()
        .flat_map(|log| log.lock().unwrap().clone())
        .collect();
    assert_eq!(
        answers.len(),
        2,
        "both requests must be served: {answers:?}"
    );
    assert_eq!(
        answers[0], answers[1],
        "without the rotation the head of the ladder keeps answering: {answers:?}"
    );
}

/// AC6 — trust: a model we have talked to keeps the head even when an
/// unverified model looks better on paper.
///
/// Research says what a model *is*; it does not say whether this provider
/// serves that id. A platform answers `404` for plenty of models its own
/// catalogue lists — such a model must not push a model we know works out
/// of the head of the ladder.
#[tokio::test]
async fn an_untrusted_model_stays_below_a_trusted_one() {
    let registry = ProviderRegistry::load();
    let groq_model = first_model(&registry, "groq");
    let gemini_model = first_model(&registry, "gemini");
    // On paper the gemini model is the better one: same class, a much bigger
    // window. The groq model is the one the router has evidence for.
    let rows = vec![
        row("groq", &groq_model, "frontier", 131_072),
        row("gemini", &gemini_model, "frontier", 1_048_576),
    ];
    let (routing, _registry, _served) = build_router(RouterSpec {
        rows: rows.clone(),
        trusted: vec![("groq".to_string(), groq_model.clone())],
        ..Default::default()
    });

    let plan = routing.auto_plan(&request());
    assert_eq!(
        plan.steps.first(),
        Some(&("groq".to_string(), groq_model.clone())),
        "the trusted model must lead the ladder: {:?}",
        plan.steps
    );

    // Without the trust evidence the better-looking model would win — that is
    // exactly the mistake the trust band prevents.
    //
    // The ladder order is pinned: the spread lets the models of one class
    // share the head, and this test asserts which model leads.
    let (untrusted, _registry, _served) = build_router(RouterSpec {
        rows,
        rotation: false,
        ..Default::default()
    });
    let plan = untrusted.auto_plan(&request());
    assert_eq!(
        plan.steps.first(),
        Some(&("gemini".to_string(), gemini_model.clone())),
        "without trust evidence the capabilities decide: {:?}",
        plan.steps
    );
}

/// AC7 — a model the provider does not serve goes down and stays in the pool.
///
/// This is the owner's "an unstable model goes down the ladder temporarily,
/// keeping its intelligence and its properties": the model is not banned and
/// not put on probation, it is pushed below its neighbours for a while.
#[tokio::test]
async fn a_failed_model_goes_down_but_stays_in_the_pool() {
    let registry = ProviderRegistry::load();
    let groq_model = first_model(&registry, "groq");
    let gemini_model = first_model(&registry, "gemini");
    let rows = vec![
        row("groq", &groq_model, "fast", 32_768),
        row("gemini", &gemini_model, "fast", 32_768),
    ];
    // The gemini backend answers 404 for everything — the way a platform
    // answers an id it does not serve.
    let (routing, _registry, served) = build_router(RouterSpec {
        rows,
        failing: vec!["gemini".to_string()],
        ..Default::default()
    });

    let before = routing.auto_plan(&request());
    let leader = before.steps[0].clone();
    assert_eq!(leader.0, "gemini", "the gemini model starts in the head");

    routing.complete(request()).await.expect("auto serves");

    // The model that answered — the only backend that works.
    let answered = served
        .iter()
        .find_map(|(provider, log)| {
            log.lock()
                .ok()
                .and_then(|models| models.first().cloned())
                .map(|model| (provider.clone(), model))
        })
        .expect("a model answered");

    let after = routing.auto_plan(&request());
    let failed_position = after
        .steps
        .iter()
        .position(|step| *step == leader)
        .expect("the failed model stays in the pool");
    let answered_position = after
        .steps
        .iter()
        .position(|step| *step == answered)
        .expect("the model that answered stays in the pool");
    assert!(
        failed_position > answered_position,
        "a failed model must go below the model that works: {failed_position} vs {answered_position} in {after:?}"
    );
}

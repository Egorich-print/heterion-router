//! The spread: a combo (and the `auto` ladder) shares its traffic over the
//! models it contains instead of always starting at the same entry.
//!
//! Contracts:
//! 1. `a_combo_spreads_its_traffic_over_its_models` — two requests against
//!    the same combo do not always start at the same entry.
//! 2. `a_combo_with_one_servable_entry_always_uses_it` — a combo with a
//!    single servable entry is not spread at all.
//! 3. `the_auto_ladder_keeps_a_frontier_model_above_a_fast_one` — the
//!    spread mixes the models of one class, never the classes themselves.
//! 4. `without_the_rotation_the_ladder_order_is_pinned` — with the rotation
//!    layer off the ladder order is stable, spread included.

use async_trait::async_trait;
use heterion_router_core::{
    ChatChoice, ChatCompletionRequest, ChatCompletionResponse, ChatMessage, GatewayError, Usage,
};
use heterion_router_db::{
    Db,
    repos::{ComboRow, ModelCapabilityRow},
};
use heterion_router_gateway::{
    backend::{ChatBackend, ChunkStream},
    capabilities::CapabilityStore,
    routing::RoutingBackend,
};
use heterion_router_providers::ProviderRegistry;
use heterion_router_routing::Router;
use serde_json::json;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// The models the backends served, in the order the requests arrived.
type ServedLog = Arc<Mutex<Vec<String>>>;

/// A backend that answers with a canned text and remembers what it served.
struct EchoBackend {
    name: &'static str,
    /// The models this backend was asked to serve, in order.
    served: ServedLog,
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

/// A request for the given model.
fn request(model: &str) -> ChatCompletionRequest {
    ChatCompletionRequest {
        model: model.to_string(),
        messages: vec![ChatMessage::plain("user", "ping")],
        ..Default::default()
    }
}

/// A capability row: what the ladder knows about one model.
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

/// The combo row: three models with the same weight.
fn combo_data() -> String {
    json!({
        "strategy": "priority",
        "models": [
            {"kind": "model", "providerId": "gemini", "model": "gemini-3-flash-preview", "weight": 50},
            {"kind": "model", "providerId": "groq", "model": "openai/gpt-oss-20b", "weight": 50},
            {"kind": "model", "providerId": "mistral", "model": "codestral-latest", "weight": 50},
        ],
    })
    .to_string()
}

/// A router over the given providers, with the combo `name` in the store.
fn combo_router(
    combo_name: &str,
    providers: &[&'static str],
    data: String,
) -> (RoutingBackend, ServedLog) {
    let served: ServedLog = Arc::new(Mutex::new(Vec::new()));
    let mut backends: HashMap<String, Arc<dyn ChatBackend>> = HashMap::new();
    for provider in providers {
        backends.insert(
            (*provider).to_string(),
            Arc::new(EchoBackend {
                name: provider,
                served: Arc::clone(&served),
            }) as Arc<dyn ChatBackend>,
        );
    }
    let db = Arc::new(Db::open_in_memory().unwrap());
    db.migrate().unwrap();
    {
        let guard = db.connection();
        heterion_router_db::repos::upsert_combo(
            &guard,
            &ComboRow {
                id: "combo-spread".to_string(),
                name: combo_name.to_string(),
                data,
                sort_order: 0,
                active: true,
            },
        )
        .unwrap();
    }
    let routing = RoutingBackend::new(backends, Router::new(vec![], vec![]))
        .with_combo_source(db, Arc::new(ProviderRegistry::load()));
    (routing, served)
}

/// A router over the given providers, with the whole catalogue in the pool.
fn ladder_router(
    providers: &[&'static str],
    rows: Vec<ModelCapabilityRow>,
    rotation: bool,
) -> RoutingBackend {
    let mut backends: HashMap<String, Arc<dyn ChatBackend>> = HashMap::new();
    for provider in providers {
        backends.insert(
            (*provider).to_string(),
            Arc::new(EchoBackend {
                name: provider,
                served: Arc::new(Mutex::new(Vec::new())),
            }) as Arc<dyn ChatBackend>,
        );
    }
    let db = Arc::new(Db::open_in_memory().unwrap());
    RoutingBackend::new(backends, Router::new(vec![], vec![]))
        .with_combo_source(db, Arc::new(ProviderRegistry::load()))
        .with_capabilities(Arc::new(CapabilityStore::from_rows(&rows)))
        .with_rotation(rotation)
}

/// AC1 — the traffic of a combo is shared by the models it contains.
#[tokio::test]
async fn a_combo_spreads_its_traffic_over_its_models() {
    let (routing, served) =
        combo_router("spread-combo", &["gemini", "groq", "mistral"], combo_data());

    // 24 draws: with three equal entries the chance that every draw lands on
    // the same one is 3 * (1/3)^24 — far below a CI flake.
    for _ in 0..24 {
        routing
            .complete(request("spread-combo"))
            .await
            .expect("the combo serves");
    }

    let heads = served.lock().unwrap().clone();
    assert_eq!(heads.len(), 24, "every request must be served: {heads:?}");
    let distinct: std::collections::HashSet<&String> = heads.iter().collect();
    assert!(
        distinct.len() > 1,
        "the combo must not always start at the same entry: {heads:?}"
    );
}

/// AC2 — a combo with a single servable entry keeps it.
#[tokio::test]
async fn a_combo_with_one_servable_entry_always_uses_it() {
    // Only the gemini backend exists: the two other entries of the combo
    // cannot serve anything.
    let (routing, served) = combo_router("spread-combo", &["gemini"], combo_data());

    for _ in 0..8 {
        routing
            .complete(request("spread-combo"))
            .await
            .expect("the combo serves");
    }

    let asked = served.lock().unwrap().clone();
    assert_eq!(
        asked,
        vec!["gemini-3-flash-preview".to_string(); 8],
        "the only servable entry must serve every request: {asked:?}"
    );
}

/// AC3 — the spread mixes the models of one class, never the classes.
#[tokio::test]
async fn the_auto_ladder_keeps_a_frontier_model_above_a_fast_one() {
    let registry = ProviderRegistry::load();
    let frontier = registry
        .get("groq")
        .expect("groq in registry")
        .models
        .first()
        .expect("groq has models")
        .id
        .clone();
    let fast = registry
        .get("mistral")
        .expect("mistral in registry")
        .models
        .first()
        .expect("mistral has models")
        .id
        .clone();

    // Two frontier models of the same standing and one fast model below
    // them: the spread may swap the frontier models, never lift the fast
    // one above them.
    let rows = vec![
        row("groq", &frontier, "frontier", 131_072),
        row("gemini", "gemini-3-flash-preview", "frontier", 131_072),
        row("mistral", &fast, "fast", 32_768),
    ];
    let routing = ladder_router(&["groq", "gemini", "mistral"], rows, true);

    let mut heads: Vec<String> = Vec::new();
    // 24 plans: the head is drawn by weight, so the chance that every draw
    // lands on the same model is far below a CI flake.
    for _ in 0..24 {
        let plan = routing.auto_plan(&request("heterion-router/auto"));
        let fast_position = plan
            .steps
            .iter()
            .position(|step| step.0 == "mistral" && step.1 == fast)
            .expect("the fast model is in the plan");
        for (provider, model) in [
            ("groq", frontier.as_str()),
            ("gemini", "gemini-3-flash-preview"),
        ] {
            let position = plan
                .steps
                .iter()
                .position(|step| step.0 == provider && step.1 == model)
                .expect("the frontier model is in the plan");
            assert!(
                position < fast_position,
                "a frontier model must stay above a fast one: {position} vs {fast_position} in {:?}",
                plan.steps
            );
        }
        heads.push(plan.steps[0].1.clone());
    }

    let distinct: std::collections::HashSet<&String> = heads.iter().collect();
    assert!(
        distinct.len() > 1,
        "the models of one class must share the head: {heads:?}"
    );
}

/// AC4 — with the rotation off the ladder order is pinned.
#[tokio::test]
async fn without_the_rotation_the_ladder_order_is_pinned() {
    let registry = ProviderRegistry::load();
    let groq_model = registry
        .get("groq")
        .expect("groq in registry")
        .models
        .first()
        .expect("groq has models")
        .id
        .clone();
    let rows = vec![
        row("groq", &groq_model, "frontier", 131_072),
        row("gemini", "gemini-3-flash-preview", "frontier", 131_072),
    ];
    let routing = ladder_router(&["groq", "gemini"], rows, false);

    let first = routing.auto_plan(&request("heterion-router/auto"));
    let second = routing.auto_plan(&request("heterion-router/auto"));
    assert_eq!(
        first.steps, second.steps,
        "without the rotation the ladder order must not move: {:?}",
        first.steps
    );
}

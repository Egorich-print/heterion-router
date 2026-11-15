//! The unified Switchyard pool: `auto` serves every live model, not the
//! seven curated candidates alone.
//!
//! Contracts:
//! 1. `the_pool_covers_every_live_provider_model` — every model of every
//!    provider that has a live backend is reachable through `auto`.
//! 2. `the_curated_ladder_stays_on_top_of_the_tail` — the curated tier keeps
//!    its order and its priorities; the tail follows below.
//! 3. `web_executors_stay_out_of_tool_requests` — the web executor has no
//!    tool head at all, so it never takes a tool-bearing request.
//! 4. `auto_serves_a_model_the_curated_ladder_does_not_know` — a provider
//!    outside the ladder is served through `auto` all the same.
//! 5. `an_auto_request_stops_at_the_attempt_budget` — the pool is the whole
//!    catalogue, but one request may not walk all of it.

use async_trait::async_trait;
use heterion_router_core::{
    ChatChoice, ChatCompletionRequest, ChatCompletionResponse, ChatMessage, GatewayError, Usage,
};
use heterion_router_db::Db;
use heterion_router_gateway::{
    backend::{ChatBackend, ChunkStream, stamp_attribution},
    routing::{AUTO_MAX_ATTEMPTS, RoutingBackend},
};
use heterion_router_providers::ProviderRegistry;
use heterion_router_routing::Router;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

/// A backend that answers with a canned text or fails with an upstream 500.
struct MockBackend {
    calls: AtomicUsize,
    fail: bool,
}

impl MockBackend {
    fn ok() -> Self {
        Self {
            calls: AtomicUsize::new(0),
            fail: false,
        }
    }

    fn failing() -> Self {
        Self {
            calls: AtomicUsize::new(0),
            fail: true,
        }
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::Relaxed)
    }
}

#[async_trait]
impl ChatBackend for MockBackend {
    fn name(&self) -> &'static str {
        "mock"
    }

    async fn complete(
        &self,
        request: ChatCompletionRequest,
    ) -> Result<ChatCompletionResponse, GatewayError> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        if self.fail {
            return Err(GatewayError::Upstream(
                "upstream status 500: mock failure".to_string(),
            ));
        }
        Ok(ChatCompletionResponse {
            id: "resp-mock".into(),
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
            Err(GatewayError::Upstream(
                "upstream status 500: mock failure".into(),
            ))
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

fn request_with_tools() -> ChatCompletionRequest {
    ChatCompletionRequest {
        model: "heterion-router/auto".into(),
        messages: vec![ChatMessage::plain("user", "read a file")],
        tools: Some(serde_json::json!([
            {
                "type": "function",
                "function": {
                    "name": "read_file",
                    "description": "reads a file",
                    "parameters": { "type": "object", "properties": { "path": { "type": "string" } } }
                }
            }
        ])),
        ..Default::default()
    }
}

/// A router whose pool is built from the real registry.
fn router_with(
    backends: HashMap<String, Arc<dyn ChatBackend>>,
) -> (RoutingBackend, ProviderRegistry) {
    let registry = ProviderRegistry::load();
    let db = Arc::new(Db::open_in_memory().unwrap());
    let routing = RoutingBackend::new(backends, Router::new(vec![], vec![]))
        .with_combo_source(db, Arc::new(registry.clone()));
    (routing, registry)
}

fn insert(
    backends: &mut HashMap<String, Arc<dyn ChatBackend>>,
    provider: &str,
    mock: Arc<MockBackend>,
) {
    backends.insert(provider.to_string(), mock as Arc<dyn ChatBackend>);
}

/// AC1 — every model of every provider with a live backend is in the plan.
#[tokio::test]
async fn the_pool_covers_every_live_provider_model() {
    let mut backends: HashMap<String, Arc<dyn ChatBackend>> = HashMap::new();
    insert(&mut backends, "groq", Arc::new(MockBackend::ok()));
    insert(&mut backends, "pollinations", Arc::new(MockBackend::ok()));
    let (routing, registry) = router_with(backends);

    let plan = routing.auto_plan(&request());

    for provider in ["groq", "pollinations"] {
        let entry = registry.get(provider).expect("provider in registry");
        assert!(
            !entry.models.is_empty(),
            "{provider} is expected to carry models"
        );
        for model in &entry.models {
            let step = (provider.to_string(), model.id.clone());
            assert!(
                plan.steps.contains(&step),
                "{provider}/{} must be reachable through auto",
                model.id
            );
        }
    }
}

/// AC2 — the curated ladder keeps its order; the tail comes after it.
#[tokio::test]
async fn the_curated_ladder_stays_on_top_of_the_tail() {
    let mut backends: HashMap<String, Arc<dyn ChatBackend>> = HashMap::new();
    for provider in ["gemini", "agentrouter", "mistral", "groq"] {
        insert(&mut backends, provider, Arc::new(MockBackend::ok()));
    }
    let (routing, _registry) = router_with(backends);

    let plan = routing.auto_plan(&request());
    let curated: Vec<(String, String)> = vec![
        ("gemini".into(), "gemini-3-flash-preview".into()),
        ("agentrouter".into(), "gpt-5.6-sol".into()),
        ("agentrouter".into(), "gpt-6.1-sol".into()),
        ("mistral".into(), "codestral-latest".into()),
        ("gemini".into(), "gemma-4-26b-a4b-it".into()),
        ("gemini".into(), "gemini-3.5-flash-lite".into()),
        ("gemini".into(), "gemini-3.1-flash-lite".into()),
    ];

    // The curated tier comes first, in the curated order of the *plan*: the
    // stage weight may lift a reasoning candidate above a higher base
    // priority, but the tier never mixes with the tail.
    let head: Vec<(String, String)> = plan.steps.iter().take(curated.len()).cloned().collect();
    for expected in &curated {
        assert!(
            head.contains(expected),
            "{expected:?} must be in the curated head: {head:?}"
        );
    }
    let tail: Vec<(String, String)> = plan.steps.iter().skip(curated.len()).cloned().collect();
    for step in &tail {
        assert!(
            !curated.contains(step),
            "the tail must not repeat a curated candidate: {step:?}"
        );
    }
}

/// AC3 — the web executor never takes a tool-bearing request.
#[tokio::test]
async fn web_executors_stay_out_of_tool_requests() {
    let mut backends: HashMap<String, Arc<dyn ChatBackend>> = HashMap::new();
    insert(&mut backends, "chatgpt-web", Arc::new(MockBackend::ok()));
    insert(&mut backends, "groq", Arc::new(MockBackend::ok()));
    let (routing, _registry) = router_with(backends);

    let with_tools = routing.auto_plan(&request_with_tools());
    assert!(
        !with_tools
            .steps
            .iter()
            .any(|(provider, _)| provider == "chatgpt-web"),
        "the web executor has no tool head and must not take a tool request"
    );

    let plain = routing.auto_plan(&request());
    assert!(
        plain
            .steps
            .iter()
            .any(|(provider, _)| provider == "chatgpt-web"),
        "without tools the web executor is part of the pool"
    );
}

/// AC4 — a provider outside the curated ladder is served through `auto`.
#[tokio::test]
async fn auto_serves_a_model_the_curated_ladder_does_not_know() {
    let groq = Arc::new(MockBackend::ok());
    let mut backends: HashMap<String, Arc<dyn ChatBackend>> = HashMap::new();
    // Only the tail provider exists: no gemini, no agentrouter, no mistral.
    insert(&mut backends, "groq", groq.clone());
    let (routing, registry) = router_with(backends);

    let groq_models: Vec<String> = registry
        .get("groq")
        .expect("groq in registry")
        .models
        .iter()
        .map(|model| model.id.clone())
        .collect();

    let mut req = request();
    let token = stamp_attribution(&mut req);
    routing.complete(req).await.expect("auto serves groq");

    assert_eq!(groq.calls(), 1);
    assert_eq!(
        routing.attribution(&token).provider.as_deref(),
        Some("groq")
    );
    let served = routing.attribution(&token).model.clone().expect("model");
    assert!(
        groq_models.contains(&served),
        "the served model {served} must be one of {groq_models:?}"
    );
}

/// AC5 — the pool is the whole catalogue, but a request stops at the budget.
#[tokio::test]
async fn an_auto_request_stops_at_the_attempt_budget() {
    let registry = ProviderRegistry::load();
    let mut backends: HashMap<String, Arc<dyn ChatBackend>> = HashMap::new();
    let mut mocks: Vec<Arc<MockBackend>> = Vec::new();
    let mut candidates = 0usize;
    for entry in registry.iter() {
        if entry.models.is_empty() {
            continue;
        }
        // 40 providers with models: far more than the budget allows.
        if candidates >= 40 {
            break;
        }
        candidates += entry.models.len();
        let mock = Arc::new(MockBackend::failing());
        mocks.push(mock.clone());
        insert(&mut backends, entry.id.as_str(), mock);
    }

    let db = Arc::new(Db::open_in_memory().unwrap());
    let routing = RoutingBackend::new(backends, Router::new(vec![], vec![]))
        .with_combo_source(db, Arc::new(registry));
    let plan = routing.auto_plan(&request());
    assert!(
        plan.steps.len() > AUTO_MAX_ATTEMPTS,
        "the pool must be bigger than the attempt budget, got {}",
        plan.steps.len()
    );

    let mut req = request();
    let _ = stamp_attribution(&mut req);
    let error = routing
        .complete(req)
        .await
        .expect_err("every candidate fails");
    assert!(
        error.to_string().contains("500"),
        "the last upstream error is reported, got: {error}"
    );

    let total: usize = mocks.iter().map(|mock| mock.calls()).sum();
    assert!(
        total <= AUTO_MAX_ATTEMPTS,
        "the request tried {total} candidates, the budget is {AUTO_MAX_ATTEMPTS}"
    );
}

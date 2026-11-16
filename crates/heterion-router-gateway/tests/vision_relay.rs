//! The vision relay: the router borrows eyes for a model that has none.
//!
//! Contracts:
//! 1. `a_blind_target_is_served_through_a_borrowed_description` — a request
//!    with an image that lands on a model without vision is served: another
//!    model looks at the picture, and the target receives its description.
//! 2. `without_the_relay_the_picture_stays_in_the_request` — with the relay
//!    off, nothing changes: the target still gets the image.
//! 3. `a_vision_target_looks_at_the_picture_itself` — a model that can see
//!    the picture is asked directly; nobody describes it for it.
//! 4. `the_saving_variant_lends_the_picture_to_a_cheaper_model` — with the
//!    saving variant on, a frontier model hands the picture to a cheaper
//!    vision model and receives the description instead.
//! 5. `a_failed_relay_leaves_the_request_unchanged` — a describer that
//!    fails changes nothing: the target still gets the picture.

use async_trait::async_trait;
use heterion_router_core::{
    ChatChoice, ChatCompletionRequest, ChatCompletionResponse, ChatMessage, GatewayError, Usage,
};
use heterion_router_db::{Db, repos::ModelCapabilityRow};
use heterion_router_gateway::{
    backend::{ChatBackend, ChunkStream},
    capabilities::CapabilityStore,
    routing::RoutingBackend,
};
use heterion_router_providers::ProviderRegistry;
use heterion_router_routing::Router;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// A backend that records what it was asked to serve.
struct RecordingBackend {
    name: &'static str,
    /// The requests this backend was asked to serve.
    served: Arc<Mutex<Vec<ChatCompletionRequest>>>,
    /// What it answers with.
    answer: &'static str,
    /// When set, the backend fails instead of answering.
    fail: bool,
}

#[async_trait]
impl ChatBackend for RecordingBackend {
    fn name(&self) -> &'static str {
        self.name
    }

    async fn complete(
        &self,
        request: ChatCompletionRequest,
    ) -> Result<ChatCompletionResponse, GatewayError> {
        if self.fail {
            return Err(GatewayError::Upstream("upstream status 500".into()));
        }
        let model = request.model.clone();
        self.served.lock().unwrap().push(request);
        Ok(ChatCompletionResponse {
            id: "resp-recording".into(),
            object: "chat.completion",
            created: 1,
            model,
            usage: Usage::default(),
            choices: vec![ChatChoice {
                index: 0,
                message: ChatMessage::assistant_text(self.answer.to_string()),
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

/// The requests a backend was asked to serve.
type ServedLog = HashMap<String, Arc<Mutex<Vec<ChatCompletionRequest>>>>;

/// How the test router is put together.
struct RouterSpec {
    /// Whether the relay is on.
    relay: bool,
    /// Whether the token-saving variant is on.
    saving: bool,
    /// Whether the describer fails.
    describer_fails: bool,
}

impl Default for RouterSpec {
    fn default() -> Self {
        Self {
            relay: true,
            saving: false,
            describer_fails: false,
        }
    }
}

/// A router over one vision provider (`gemini`) and one blind one (`groq`).
fn build_router(spec: RouterSpec) -> (RoutingBackend, ProviderRegistry, ServedLog, String, String) {
    let registry = ProviderRegistry::load();
    let mut backends: HashMap<String, Arc<dyn ChatBackend>> = HashMap::new();
    let mut served: ServedLog = HashMap::new();
    for provider in ["gemini", "groq"] {
        let log = Arc::new(Mutex::new(Vec::new()));
        backends.insert(
            provider.to_string(),
            Arc::new(RecordingBackend {
                name: provider,
                served: Arc::clone(&log),
                answer: if provider == "gemini" {
                    "a red button labelled OK"
                } else {
                    "ok"
                },
                fail: provider == "gemini" && spec.describer_fails,
            }) as Arc<dyn ChatBackend>,
        );
        served.insert(provider.to_string(), log);
    }

    // Every model of the vision provider sees images; every model of the
    // blind one does not. The first gemini model is the frontier one — the
    // expensive eyes the saving variant borrows from.
    let mut rows: Vec<ModelCapabilityRow> = Vec::new();
    for (index, model) in registry
        .get("gemini")
        .expect("gemini in the registry")
        .models
        .iter()
        .enumerate()
    {
        rows.push(row(
            "gemini",
            &model.id,
            if index == 0 { "frontier" } else { "fast" },
            true,
        ));
    }
    for model in &registry.get("groq").expect("groq in the registry").models {
        rows.push(row("groq", &model.id, "frontier", false));
    }
    let frontier_gemini = registry.get("gemini").unwrap().models[0].id.clone();
    let blind_groq = registry.get("groq").unwrap().models[0].id.clone();

    let db = Arc::new(Db::open_in_memory().unwrap());
    let routing = RoutingBackend::new(backends, Router::new(vec![], vec![]))
        .with_combo_source(db, Arc::new(registry.clone()))
        .with_capabilities(Arc::new(CapabilityStore::from_rows(&rows)))
        .with_vision_relay(spec.relay)
        .with_vision_saving(spec.saving);
    (routing, registry, served, frontier_gemini, blind_groq)
}

fn row(provider: &str, model: &str, tier: &str, sees: bool) -> ModelCapabilityRow {
    ModelCapabilityRow {
        provider: provider.to_string(),
        model_id: model.to_string(),
        tool_call: Some(true),
        reasoning: Some(tier == "frontier"),
        attachment: Some(sees),
        limit_context: Some(32_768),
        limit_output: None,
        family: None,
        real_context: None,
        tier: Some(tier.to_string()),
    }
}

/// A request that carries a picture.
fn image_request(model: &str) -> ChatCompletionRequest {
    ChatCompletionRequest {
        model: model.to_string(),
        messages: vec![ChatMessage {
            role: "user".into(),
            content: Some(json!([
                {"type": "text", "text": "что на картинке?"},
                {"type": "image_url", "image_url": {"url": "data:image/png;base64,AAA"}},
            ])),
            reasoning_content: None,
            tool_calls: None,
            tool_call_id: None,
            name: None,
        }],
        ..Default::default()
    }
}

/// Whether the request still carries a picture.
fn carries_image(request: &ChatCompletionRequest) -> bool {
    request.messages.iter().any(|message| {
        message
            .content
            .as_ref()
            .and_then(Value::as_array)
            .is_some_and(|parts| {
                parts
                    .iter()
                    .any(|part| part.get("type").and_then(Value::as_str) == Some("image_url"))
            })
    })
}

/// The text a request carries.
fn request_text(request: &ChatCompletionRequest) -> String {
    request
        .messages
        .iter()
        .map(|message| message.text())
        .collect::<Vec<_>>()
        .join("\n")
}

/// AC1 — a blind target is served through a borrowed description.
#[tokio::test]
async fn a_blind_target_is_served_through_a_borrowed_description() {
    let (routing, _registry, served, _frontier, blind) = build_router(RouterSpec::default());
    let model = format!("groq/{blind}");

    let response = routing
        .complete(image_request(&model))
        .await
        .expect("the request is served");

    let described = served["gemini"].lock().unwrap().clone();
    assert_eq!(
        described.len(),
        1,
        "a model that can see the picture must look at it: {described:?}"
    );
    assert!(
        carries_image(&described[0]),
        "the describer must receive the picture itself"
    );

    let answered = served["groq"].lock().unwrap().clone();
    assert_eq!(answered.len(), 1, "the target must be served: {answered:?}");
    assert!(
        !carries_image(&answered[0]),
        "the picture must not reach the blind target"
    );
    let text = request_text(&answered[0]);
    assert!(
        text.contains("a red button labelled OK"),
        "the target must receive the description: {text}"
    );
    assert!(
        text.contains("gemini/"),
        "the description must name the model that wrote it: {text}"
    );
    assert!(!response.choices.is_empty());
}

/// AC2 — with the relay off, nothing changes.
#[tokio::test]
async fn without_the_relay_the_picture_stays_in_the_request() {
    let (routing, _registry, served, _frontier, blind) = build_router(RouterSpec {
        relay: false,
        ..Default::default()
    });
    let model = format!("groq/{blind}");

    routing
        .complete(image_request(&model))
        .await
        .expect("served");

    assert!(
        served["gemini"].lock().unwrap().is_empty(),
        "nobody may describe the picture when the relay is off"
    );
    let answered = served["groq"].lock().unwrap().clone();
    assert_eq!(answered.len(), 1);
    assert!(
        carries_image(&answered[0]),
        "the target must still get the picture"
    );
}

/// AC3 — a model that can see the picture looks at it itself.
#[tokio::test]
async fn a_vision_target_looks_at_the_picture_itself() {
    let (routing, _registry, served, frontier, _blind) = build_router(RouterSpec::default());
    let model = format!("gemini/{frontier}");

    routing
        .complete(image_request(&model))
        .await
        .expect("served");

    let answered = served["gemini"].lock().unwrap().clone();
    assert_eq!(answered.len(), 1);
    assert!(
        carries_image(&answered[0]),
        "the target must see the picture itself: {answered:?}"
    );
}

/// AC4 — the saving variant lends the picture to a cheaper model.
#[tokio::test]
async fn the_saving_variant_lends_the_picture_to_a_cheaper_model() {
    let (routing, _registry, served, frontier, _blind) = build_router(RouterSpec {
        saving: true,
        ..Default::default()
    });
    let model = format!("gemini/{frontier}");

    routing
        .complete(image_request(&model))
        .await
        .expect("served");

    let answered = served["gemini"].lock().unwrap().clone();
    assert_eq!(
        answered.len(),
        2,
        "one call describes the picture, the other answers: {answered:?}"
    );
    let described = answered
        .iter()
        .find(|request| carries_image(request))
        .expect("the describer gets the picture");
    let answered = answered
        .iter()
        .find(|request| !carries_image(request))
        .expect("the target gets the description");
    assert_ne!(
        described.model, answered.model,
        "a different, cheaper model must do the looking"
    );
    assert!(
        request_text(answered).contains("a red button labelled OK"),
        "the target must receive the description: {:?}",
        request_text(answered)
    );
}

/// AC5 — a failed relay changes nothing.
#[tokio::test]
async fn a_failed_relay_leaves_the_request_unchanged() {
    let (routing, _registry, served, _frontier, blind) = build_router(RouterSpec {
        describer_fails: true,
        ..Default::default()
    });
    let model = format!("groq/{blind}");

    routing
        .complete(image_request(&model))
        .await
        .expect("served");

    let answered = served["groq"].lock().unwrap().clone();
    assert_eq!(answered.len(), 1);
    assert!(
        carries_image(&answered[0]),
        "a failed relay must leave the request as it was"
    );
}

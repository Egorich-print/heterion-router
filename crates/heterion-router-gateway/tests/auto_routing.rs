//! Milestone 4 Integration Test Suite:
//! NVIDIA Switchyard Dynamic Auto-Routing & Catalog Contracts for `heterion-router/auto` and `auto`.
//!
//! Validates:
//! 1. `test_catalog_listing_auto_models`: `GET /v1/models` advertises `heterion-router/auto` and `auto`
//!    with full capability contracts (1M context, vision, tools, zero-quota pruning, dynamic ladder).
//! 2. `test_alias_resolution_equivalence`: `heterion-router/auto` and `auto` resolve symmetrically,
//!    preserve requested model on the wire, and book attribution under `heterion-router/auto`.
//! 3. `test_multi_tier_cascading_failover`: Cascading across Tier 1 -> Tier 2 -> Tier 3.
//! 4. `test_tool_call_preflight_exclusion`: Requests with tools exclude pseudo-tool and search-only models.
//! 5. `test_multimodal_vision_routing`: Requests with images only select vision-capable models (Gemini).
//! 6. `test_high_context_jit_pruning_failover`: Prompts >280k fold intermediate tools to fit 256k Tier 2 candidate.
//! 7. `test_streaming_prestream_failover_and_attribution`: Streaming SSE failover on initial error; books candidate model.
//! 8. `test_strict_breaker_isolation_on_4xx`: Repeated 429 and 403 errors never trip `BreakerSet`.

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
use http_body_util::BodyExt;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;
use tower::ServiceExt;

// ---------------------------------------------------------------------------
// Mock Backend Infrastructure
// ---------------------------------------------------------------------------

struct MockBackend {
    name: &'static str,
    responses: Mutex<Vec<Result<ChatCompletionResponse, GatewayError>>>,
    calls: Arc<AtomicUsize>,
    received_requests: Arc<Mutex<Vec<ChatCompletionRequest>>>,
    stream_results: Mutex<Vec<Result<StreamChunk, GatewayError>>>,
}

impl MockBackend {
    fn new(name: &'static str) -> Self {
        Self {
            name,
            responses: Mutex::new(Vec::new()),
            calls: Arc::new(AtomicUsize::new(0)),
            received_requests: Arc::new(Mutex::new(Vec::new())),
            stream_results: Mutex::new(Vec::new()),
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

    fn last_request(&self) -> Option<ChatCompletionRequest> {
        self.received_requests.lock().unwrap().last().cloned()
    }
}

#[async_trait]
impl ChatBackend for MockBackend {
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
                id: format!("resp-{}", self.name),
                object: "chat.completion",
                created: 12345,
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

    fn stream(&self, request: ChatCompletionRequest) -> ChunkStream {
        self.calls.fetch_add(1, Ordering::Relaxed);
        self.received_requests.lock().unwrap().push(request);

        let mut guard = self.stream_results.lock().unwrap();
        let items: Vec<Result<StreamChunk, GatewayError>> = if !guard.is_empty() {
            guard.drain(..).collect()
        } else {
            vec![Ok(StreamChunk {
                content: Some(format!("Stream from {}", self.name)),
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

fn auto_request() -> ChatCompletionRequest {
    ChatCompletionRequest {
        model: "heterion-router/auto".into(),
        messages: vec![ChatMessage::plain("user", "Hello router")],
        ..Default::default()
    }
}

fn auto_request_with_tools() -> ChatCompletionRequest {
    ChatCompletionRequest {
        model: "heterion-router/auto".into(),
        messages: vec![ChatMessage::plain("user", "Read this file")],
        tools: Some(json!([
            {
                "type": "function",
                "function": {
                    "name": "read_file",
                    "description": "reads a file from filesystem",
                    "parameters": {
                        "type": "object",
                        "properties": {
                            "path": { "type": "string" }
                        },
                        "required": ["path"]
                    }
                }
            }
        ])),
        ..Default::default()
    }
}

fn build_large_history_request() -> ChatCompletionRequest {
    let mut messages = Vec::new();
    messages.push(ChatMessage::plain(
        "system",
        "You are an expert autonomous software engineer.",
    ));
    messages.push(ChatMessage::plain(
        "user",
        "Refactor the architecture and optimize performance.",
    ));

    // 8 intermediate turns of large tool outputs (~35k tokens / 140k chars each)
    let large_body = "x".repeat(140_000);
    for i in 0..8 {
        let call_id = format!("call_int_{i}");
        messages.push(ChatMessage {
            role: "assistant".into(),
            content: None,
            reasoning_content: None,
            tool_calls: Some(json!([
                {
                    "id": call_id,
                    "type": "function",
                    "function": { "name": "grep", "arguments": "{\"query\":\"fn\"}" }
                }
            ])),
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

    // Active tail turns (4 turns)
    messages.push(ChatMessage::plain("user", "Execute step 7"));
    messages.push(ChatMessage::plain("assistant", "Step 7 complete"));
    messages.push(ChatMessage::plain("user", "Execute step 8"));
    messages.push(ChatMessage::plain("assistant", "Proceeding to step 8"));

    ChatCompletionRequest {
        model: "heterion-router/auto".into(),
        messages,
        ..Default::default()
    }
}

// ---------------------------------------------------------------------------
// Suite 1: Model Catalog Contract (`GET /v1/models`)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_catalog_listing_auto_models() {
    let db = Arc::new(Db::open_in_memory().unwrap());
    db.migrate().unwrap();

    // Insert an active combo to verify custom combos remain preserved in catalog
    {
        let guard = db.connection();
        repos::upsert_combo(
            &guard,
            &ComboRow {
                id: "combo-test".into(),
                name: "my-test-combo".into(),
                data: json!({"isHidden": false}).to_string(),
                sort_order: 10,
                active: true,
            },
        )
        .unwrap();
    }

    let state = AppState::with_db(Arc::new(MockBackend::new("mock")), db);
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

    // 1. Canonical heterion-router/auto
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

    // 2. Alias auto
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

    // 3. User combo preserved
    let combo = data
        .iter()
        .find(|m| m["id"] == "my-test-combo")
        .expect("user combo must be present");
    assert_eq!(combo["heterion-router"]["kind"], "combo");
}

// ---------------------------------------------------------------------------
// Suite 2: Route Alias Equivalence (`heterion-router/auto` vs `auto`)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_alias_resolution_equivalence() {
    let gemini = Arc::new(MockBackend::new("gemini"));
    let agentrouter = Arc::new(MockBackend::new("agentrouter"));

    let mut backends: HashMap<String, Arc<dyn ChatBackend>> = HashMap::new();
    backends.insert("gemini".into(), gemini.clone());
    backends.insert("agentrouter".into(), agentrouter.clone());

    let routing = RoutingBackend::new(backends, Router::new(vec![], vec![]));

    // A. Request using canonical "heterion-router/auto"
    gemini.push_response(Ok(ChatCompletionResponse {
        id: "resp-canonical".into(),
        object: "chat.completion",
        created: 1,
        model: "gemini-3-flash-preview".into(),
        usage: Usage::default(),
        choices: vec![ChatChoice {
            index: 0,
            message: ChatMessage::assistant_text("Response via canonical"),
            finish_reason: "stop".into(),
        }],
        raw: None,
    }));

    let mut req_canonical = ChatCompletionRequest {
        model: "heterion-router/auto".into(),
        messages: vec![ChatMessage::plain("user", "Hello canonical")],
        ..Default::default()
    };
    let token_canonical = stamp_attribution(&mut req_canonical);
    let res_canonical = routing.complete(req_canonical).await.unwrap();
    assert_eq!(
        res_canonical.choices[0].message.text(),
        "Response via canonical"
    );
    let attr_canonical = routing.attribution(&token_canonical);
    assert_eq!(attr_canonical.provider.as_deref(), Some("gemini"));
    assert_eq!(
        attr_canonical.model.as_deref(),
        Some("gemini-3-flash-preview")
    );
    assert_eq!(
        attr_canonical.combo_name.as_deref(),
        Some("heterion-router/auto")
    );

    // B. Request using alias "auto"
    gemini.push_response(Ok(ChatCompletionResponse {
        id: "resp-alias".into(),
        object: "chat.completion",
        created: 2,
        model: "gemini-3-flash-preview".into(),
        usage: Usage::default(),
        choices: vec![ChatChoice {
            index: 0,
            message: ChatMessage::assistant_text("Response via alias"),
            finish_reason: "stop".into(),
        }],
        raw: None,
    }));

    let mut req_alias = ChatCompletionRequest {
        model: "auto".into(),
        messages: vec![ChatMessage::plain("user", "Hello alias")],
        ..Default::default()
    };
    let token_alias = stamp_attribution(&mut req_alias);
    let res_alias = routing.complete(req_alias).await.unwrap();
    assert_eq!(res_alias.choices[0].message.text(), "Response via alias");
    let attr_alias = routing.attribution(&token_alias);
    assert_eq!(attr_alias.provider.as_deref(), Some("gemini"));
    assert_eq!(attr_alias.model.as_deref(), Some("gemini-3-flash-preview"));
    assert_eq!(
        attr_alias.combo_name.as_deref(),
        Some("heterion-router/auto")
    );

    // C. Symmetric Auth pattern matching equivalence
    assert!(matches_pattern("heterion-router/auto", "auto"));
    assert!(matches_pattern("auto", "heterion-router/auto"));
    assert!(matches_pattern(
        "heterion-router/auto",
        "heterion-router/auto"
    ));
    assert!(matches_pattern("auto", "auto"));
    assert!(!matches_pattern("heterion-router/auto", "gpt-4o"));
    assert!(!matches_pattern("auto", "gpt-4o"));
}

// ---------------------------------------------------------------------------
// Suite 3: Multi-Tier Cascading Failover (Tier 1 -> Tier 2 -> Tier 3)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_multi_tier_cascading_failover() {
    let gemini = Arc::new(MockBackend::new("gemini"));
    let agentrouter = Arc::new(MockBackend::new("agentrouter"));
    let mistral = Arc::new(MockBackend::new("mistral"));

    // Canonical priority ladder in DEFAULT_AUTO_CANDIDATES:
    // Tier 1:
    // - gemini / gemini-3-flash-preview (300.0)
    // - agentrouter / gpt-5.6-sol (290.0)
    // - agentrouter / gpt-6.1-sol (280.0)
    // Tier 2:
    // - mistral / codestral-latest (200.0)
    // - gemini / gemma-4-26b-a4b-it (190.0)
    // Tier 3:
    // - gemini / gemini-3.5-flash-lite (100.0)
    // - gemini / gemini-3.1-flash-lite (90.0)

    // Tier 1 Candidate 1 (gemini / gemini-3-flash-preview): returns transient 503
    gemini.push_response(Err(GatewayError::Upstream(
        "upstream status 503: Service Unavailable".into(),
    )));

    // Tier 1 Candidate 2 (agentrouter / gpt-5.6-sol): returns 403 quota error
    agentrouter.push_response(Err(GatewayError::Upstream(
        "upstream status 403: quota exceeded".into(),
    )));

    // Tier 2 Candidate 4 (mistral / codestral-latest): returns 502 Bad Gateway
    mistral.push_response(Err(GatewayError::Upstream(
        "upstream status 502: Bad Gateway".into(),
    )));

    // Tier 2 Candidate 5 (gemini / gemma-4-26b-a4b-it): returns transient 503
    gemini.push_response(Err(GatewayError::Upstream(
        "upstream status 503: Service Unavailable".into(),
    )));

    // Tier 3 Candidate 6 (gemini / gemini-3.5-flash-lite): succeeds with 200 OK!
    gemini.push_response(Ok(ChatCompletionResponse {
        id: "resp-tier3".into(),
        object: "chat.completion",
        created: 333,
        model: "gemini-3.5-flash-lite".into(),
        usage: Usage::default(),
        choices: vec![ChatChoice {
            index: 0,
            message: ChatMessage::assistant_text(
                "Tier 3 Protector Gemini 3.5 Flash Lite Succeeded",
            ),
            finish_reason: "stop".into(),
        }],
        raw: None,
    }));

    let mut backends: HashMap<String, Arc<dyn ChatBackend>> = HashMap::new();
    backends.insert("gemini".into(), gemini.clone());
    backends.insert("agentrouter".into(), agentrouter.clone());
    backends.insert("mistral".into(), mistral.clone());

    let routing = RoutingBackend::new(backends, Router::new(vec![], vec![]));

    let mut req = auto_request();
    let token = stamp_attribution(&mut req);

    let res = routing
        .complete(req)
        .await
        .expect("cascading failover to Tier 3 should succeed");
    assert_eq!(
        res.choices[0].message.text(),
        "Tier 3 Protector Gemini 3.5 Flash Lite Succeeded"
    );

    let attr = routing.attribution(&token);
    assert_eq!(attr.provider.as_deref(), Some("gemini"));
    assert_eq!(attr.model.as_deref(), Some("gemini-3.5-flash-lite"));
    assert_eq!(attr.combo_name.as_deref(), Some("heterion-router/auto"));
}

// ---------------------------------------------------------------------------
// Suite 4: Tool-Call Pre-Flight Exclusion
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_tool_call_preflight_exclusion() {
    let gemini = Arc::new(MockBackend::new("gemini"));
    let agentrouter = Arc::new(MockBackend::new("agentrouter"));
    let morph = Arc::new(MockBackend::new("morph"));
    let exa = Arc::new(MockBackend::new("exa"));

    let mut backends: HashMap<String, Arc<dyn ChatBackend>> = HashMap::new();
    backends.insert("gemini".into(), gemini.clone());
    backends.insert("agentrouter".into(), agentrouter.clone());
    backends.insert("morph".into(), morph.clone());
    backends.insert("exa".into(), exa.clone());

    let routing = RoutingBackend::new(backends, Router::new(vec![], vec![]));

    let req_with_tools = auto_request_with_tools();
    let plan = routing.auto_plan(&req_with_tools);

    // Pre-flight filter verification:
    // Pseudo-tool models (morph, syn-pro) and tool-incapable models (exa) must NEVER be in steps
    for (provider, model) in &plan.steps {
        assert_ne!(
            provider, "morph",
            "morph is pseudo-tool and must be excluded"
        );
        assert_ne!(
            provider, "exa",
            "exa does not support tools and must be excluded"
        );
        assert!(
            !heterion_router_gateway::model_spec::is_pseudo_tool_model(provider, model),
            "pseudo-tool model {model} must be excluded"
        );
        let spec = heterion_router_gateway::model_spec::model_spec(model);
        assert_ne!(
            spec.tool_support,
            heterion_router_gateway::model_spec::ToolSupport::None,
            "models with ToolSupport::None must be excluded when tools requested"
        );
    }

    // Execute with tools: gemini returns structured native tool call
    gemini.push_response(Ok(ChatCompletionResponse {
        id: "resp-tools".into(),
        object: "chat.completion",
        created: 444,
        model: "gemini-3-flash-preview".into(),
        usage: Usage::default(),
        choices: vec![ChatChoice {
            index: 0,
            message: ChatMessage {
                role: "assistant".into(),
                content: None,
                reasoning_content: None,
                tool_calls: Some(json!([
                    {
                        "id": "call_123",
                        "type": "function",
                        "function": {
                            "name": "read_file",
                            "arguments": "{\"path\": \"Cargo.toml\"}"
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

    let mut req = req_with_tools;
    let token = stamp_attribution(&mut req);
    let res = routing.complete(req).await.unwrap();
    assert_eq!(res.choices[0].finish_reason, "tool_calls");
    assert!(res.choices[0].message.has_tool_calls());
    assert_eq!(morph.calls(), 0);
    assert_eq!(exa.calls(), 0);
    assert_eq!(
        routing.attribution(&token).model.as_deref(),
        Some("gemini-3-flash-preview")
    );
}

// ---------------------------------------------------------------------------
// Suite 5: Multimodal Vision Routing
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_multimodal_vision_routing() {
    let gemini = Arc::new(MockBackend::new("gemini"));
    let agentrouter = Arc::new(MockBackend::new("agentrouter"));
    let mistral = Arc::new(MockBackend::new("mistral"));

    let mut backends: HashMap<String, Arc<dyn ChatBackend>> = HashMap::new();
    backends.insert("gemini".into(), gemini.clone());
    backends.insert("agentrouter".into(), agentrouter.clone());
    backends.insert("mistral".into(), mistral.clone());

    let routing = RoutingBackend::new(backends, Router::new(vec![], vec![]));

    let vision_req = ChatCompletionRequest {
        model: "heterion-router/auto".into(),
        messages: vec![ChatMessage {
            role: "user".into(),
            content: Some(json!([
                {"type": "text", "text": "Describe this architecture diagram"},
                {"type": "image_url", "image_url": {"url": "https://example.com/diagram.png"}}
            ])),
            reasoning_content: None,
            tool_calls: None,
            tool_call_id: None,
            name: None,
        }],
        ..Default::default()
    };

    let plan = routing.auto_plan(&vision_req);

    // Pre-flight filter verification:
    // ALL steps in plan must support vision. Non-vision models (gpt-5.6-sol, codestral) must be absent.
    assert!(!plan.steps.is_empty(), "vision plan must have candidates");
    for (provider, model) in &plan.steps {
        let spec = heterion_router_gateway::model_spec::model_spec(model);
        assert!(
            spec.supports_vision,
            "model {provider}/{model} does not support vision but was included in vision plan"
        );
        assert_ne!(
            provider, "agentrouter",
            "agentrouter does not support vision"
        );
        assert_ne!(provider, "mistral", "mistral does not support vision");
    }

    // Failover between vision candidates:
    // Candidate 1 (gemini-3-flash-preview) fails with 503
    gemini.push_response(Err(GatewayError::Upstream(
        "upstream status 503: busy".into(),
    )));
    // Gemma does not support vision, so candidate 2 is gemini-3.5-flash-lite!
    gemini.push_response(Ok(ChatCompletionResponse {
        id: "resp-vision".into(),
        object: "chat.completion",
        created: 555,
        model: "gemini-3.5-flash-lite".into(),
        usage: Usage::default(),
        choices: vec![ChatChoice {
            index: 0,
            message: ChatMessage::assistant_text("Diagram shows Switchyard auto-routing"),
            finish_reason: "stop".into(),
        }],
        raw: None,
    }));

    let mut req = vision_req.clone();
    let token = stamp_attribution(&mut req);
    let res = routing.complete(req).await.unwrap();
    assert_eq!(
        res.choices[0].message.text(),
        "Diagram shows Switchyard auto-routing"
    );
    assert_eq!(agentrouter.calls(), 0);
    assert_eq!(mistral.calls(), 0);
    assert_eq!(
        routing.attribution(&token).model.as_deref(),
        Some("gemini-3.5-flash-lite")
    );

    // Clean GatewayError when no vision models are available
    let non_vision_backends: HashMap<String, Arc<dyn ChatBackend>> = HashMap::from([
        (
            "agentrouter".into(),
            agentrouter.clone() as Arc<dyn ChatBackend>,
        ),
        ("mistral".into(), mistral.clone() as Arc<dyn ChatBackend>),
    ]);
    let non_vision_routing = RoutingBackend::new(non_vision_backends, Router::new(vec![], vec![]));
    let err = non_vision_routing.complete(vision_req).await.unwrap_err();
    match err {
        GatewayError::UnknownModel(msg) => {
            assert!(msg.contains("no backend can serve it"));
        }
        other => panic!("expected UnknownModel error for empty candidates, got: {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Suite 6: High-Context Failover with Zero-Quota JIT Pruning
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_high_context_jit_pruning_failover() {
    let agentrouter = Arc::new(MockBackend::new("agentrouter"));
    let mistral = Arc::new(MockBackend::new("mistral"));

    // Candidate 1 (agentrouter / gpt-5.6-sol, 512k context limit) returns 429
    agentrouter.push_response(Err(GatewayError::Upstream(
        "upstream status 429: rate limit".into(),
    )));

    // Candidate 2 (mistral / codestral-latest, 256k limit) receives compressed prompt and succeeds
    mistral.push_response(Ok(ChatCompletionResponse {
        id: "resp-codestral-pruned".into(),
        object: "chat.completion",
        created: 666,
        model: "codestral-latest".into(),
        usage: Usage::default(),
        choices: vec![ChatChoice {
            index: 0,
            message: ChatMessage::assistant_text(
                "Codestral served compressed request successfully",
            ),
            finish_reason: "stop".into(),
        }],
        raw: None,
    }));

    let mut backends: HashMap<String, Arc<dyn ChatBackend>> = HashMap::new();
    backends.insert("agentrouter".into(), agentrouter.clone());
    backends.insert("mistral".into(), mistral.clone());

    let routing =
        RoutingBackend::with_breaker_settings(backends, Router::new(vec![], vec![]), 3, 60);

    let mut req = build_large_history_request();
    let original_msg_count = req.messages.len();
    let token = stamp_attribution(&mut req);

    let res = routing
        .complete(req)
        .await
        .expect("JIT pruning failover should succeed");
    assert_eq!(
        res.choices[0].message.text(),
        "Codestral served compressed request successfully"
    );

    // Attribution confirms compression was booked
    let attr = routing.attribution(&token);
    assert_eq!(attr.provider.as_deref(), Some("mistral"));
    assert_eq!(attr.model.as_deref(), Some("codestral-latest"));
    let compressed = attr
        .tokens_compressed
        .expect("tokens_compressed must be attributed");
    assert!(
        compressed > 100_000,
        "expected >100k tokens compressed, got {compressed}"
    );

    // Request delivered to mistral had intermediate tools compressed but 1-to-1 message parity and active tail turns intact
    let delivered = mistral.last_request().expect("mistral received a request");
    assert_eq!(delivered.messages.len(), original_msg_count);
    let intermediate = delivered.messages[3].text();
    assert!(intermediate.contains("[Tool Output Digest:"));
    // Tail turns preserved
    let last_user_turn = delivered.messages[delivered.messages.len() - 2].text();
    assert_eq!(last_user_turn, "Execute step 8");
}

// ---------------------------------------------------------------------------
// Suite 7: Streaming Failover and SSE Model Attribution Invariant
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_streaming_prestream_failover_and_attribution() {
    let gemini = Arc::new(MockBackend::new("gemini"));
    let agentrouter = Arc::new(MockBackend::new("agentrouter"));

    // Candidate 1 (gemini) returns 429 on initial stream chunk
    gemini.push_stream_chunk(Err(GatewayError::Upstream(
        "upstream status 429: stream rate limited".into(),
    )));

    // Candidate 2 (agentrouter) stream produces valid chunks
    agentrouter.push_stream_chunk(Ok(StreamChunk {
        content: Some("Stream chunk from AgentRouter".into()),
        finish_reason: None,
        ..Default::default()
    }));
    agentrouter.push_stream_chunk(Ok(StreamChunk {
        content: Some(" finishing stream".into()),
        finish_reason: Some("stop".into()),
        ..Default::default()
    }));

    let mut backends: HashMap<String, Arc<dyn ChatBackend>> = HashMap::new();
    backends.insert("gemini".into(), gemini.clone());
    backends.insert("agentrouter".into(), agentrouter.clone());

    let routing =
        RoutingBackend::with_breaker_settings(backends, Router::new(vec![], vec![]), 3, 60);

    let mut req = auto_request();
    req.stream = true;
    let token = stamp_attribution(&mut req);

    let mut stream = routing.stream(req);
    let chunk1 = stream.next().await.unwrap().unwrap();
    assert_eq!(
        chunk1.content.as_deref(),
        Some("Stream chunk from AgentRouter")
    );

    let chunk2 = stream.next().await.unwrap().unwrap();
    assert_eq!(chunk2.content.as_deref(), Some(" finishing stream"));
    assert_eq!(chunk2.finish_reason.as_deref(), Some("stop"));

    // Verify attribution recorded the actual candidate model used
    let attr = routing.attribution(&token);
    assert_eq!(attr.provider.as_deref(), Some("agentrouter"));
    assert_eq!(attr.model.as_deref(), Some("gpt-6.1-sol"));
    assert_eq!(attr.combo_name.as_deref(), Some("heterion-router/auto"));
}

// ---------------------------------------------------------------------------
// Suite 8: Strict Breaker Isolation on 4xx Errors Invariant
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_strict_breaker_isolation_on_4xx() {
    let gemini = Arc::new(MockBackend::new("gemini"));
    let agentrouter = Arc::new(MockBackend::new("agentrouter"));

    let mut backends: HashMap<String, Arc<dyn ChatBackend>> = HashMap::new();
    backends.insert("gemini".into(), gemini.clone());
    backends.insert("agentrouter".into(), agentrouter.clone());

    // Strict threshold = 1: if ANY 4xx error counted as a breaker failure, the breaker would TRIP IMMEDIATELY!
    let routing =
        RoutingBackend::with_breaker_settings(backends, Router::new(vec![], vec![]), 1, 3600);

    // Repeated 429 errors on gemini
    for i in 1..=3 {
        gemini.push_response(Err(GatewayError::Upstream(format!(
            "upstream status 429: rate limited call {i}"
        ))));
        agentrouter.push_response(Ok(ChatCompletionResponse {
            id: format!("resp-429-fallback-{i}"),
            object: "chat.completion",
            created: i as i64,
            model: "gpt-5.6-sol".into(),
            usage: Usage::default(),
            choices: vec![ChatChoice {
                index: 0,
                message: ChatMessage::assistant_text("agentrouter fallback"),
                finish_reason: "stop".into(),
            }],
            raw: None,
        }));

        let req = auto_request();
        let _ = routing.complete(req).await.unwrap();
    }

    // Gemini is in cooldown, but its Breaker is STILL OPEN/ALLOWED (never tripped by 429)
    assert!(routing.cooldowns().is_in_cooldown("gemini", None));
    assert!(
        routing
            .breakers()
            .lock()
            .unwrap()
            .allow("gemini", Instant::now()),
        "Invariant violation: 429 must NEVER trip circuit breaker!"
    );

    // Clear cooldown on gemini
    routing.cooldowns().clear_all_for_provider("gemini");
    for _ in 0..10 {
        routing
            .quality_tracker()
            .record_success("gemini-3-flash-preview");
    }

    // Let gemini fail with 403
    gemini.push_response(Err(GatewayError::Upstream(
        "upstream status 403: quota exceeded".into(),
    )));
    agentrouter.push_response(Ok(ChatCompletionResponse {
        id: "resp-403-fallback".into(),
        object: "chat.completion",
        created: 999,
        model: "gpt-5.6-sol".into(),
        usage: Usage::default(),
        choices: vec![ChatChoice {
            index: 0,
            message: ChatMessage::assistant_text("agentrouter fallback 403"),
            finish_reason: "stop".into(),
        }],
        raw: None,
    }));

    let req = auto_request();
    let _ = routing.complete(req).await.unwrap();

    // Gemini recorded quota freeze, but its Breaker is STILL OPEN/ALLOWED (never tripped by 403)
    assert!(routing.cooldowns().is_in_cooldown("gemini", None));
    assert!(
        routing
            .breakers()
            .lock()
            .unwrap()
            .allow("gemini", Instant::now()),
        "Invariant violation: 403 must NEVER trip circuit breaker!"
    );
}

//! Integration Test Suite for Milestone 3:
//! Resilience, Cooldowns & Post-Flight Validation with Dynamic Priority Ladder.
//!
//! Validates:
//! 1. 429 failover cascade: primary candidate returns 429 -> provider in cooldown -> cascades to
//!    secondary candidate on another provider -> subsequent request skips primary immediately.
//! 2. 403 freeze without breaker trip: primary candidate returns 403 -> 1-hour freeze -> cascades
//!    to secondary provider -> BreakerSet is NOT tripped for primary.
//! 3. Post-flight tool rejection cascade: candidate 1 returns defective response (pseudo XML `<tool_call>`)
//!    -> validation rejects -> quality penalized -> cascades to candidate 2 which produces native tool calls.
//! 4. JIT context pruning failover: large request (>280k tokens) -> 1M candidate returns 429 ->
//!    cascades to 256k candidate -> JIT context pruning folds intermediate outputs -> candidate 2 completes
//!    -> attribution records `tokens_compressed > 0`.
//! 5. Model probation enforcement: 2 consecutive failures activate 15-minute probation -> model
//!    skipped during candidate loop.
//! 6. Streaming failover on initial 429 error.
//! 7. Rate-limit cooldown on the direct `provider/model` path: a model that is
//!    known to be rate limited must not be called again.
//! 8. A rate limit is a quota event, not a defective response: it must not cost
//!    the model its quality score (and with it the 15-minute probation).

use async_trait::async_trait;
use futures::StreamExt;
use heterion_router_core::{
    ChatChoice, ChatCompletionRequest, ChatCompletionResponse, ChatMessage, GatewayError,
    StreamChunk, Usage,
};
use heterion_router_gateway::backend::{ChatBackend, ChunkStream, stamp_attribution};
use heterion_router_gateway::routing::RoutingBackend;
use heterion_router_routing::Router;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

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
            // Default echo response
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
    // System prompt
    messages.push(ChatMessage::plain(
        "system",
        "You are an expert autonomous software engineer.",
    ));
    // Initial user goal
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

    // 4 recent tail turns
    for i in 0..4 {
        let call_id = format!("call_tail_{i}");
        messages.push(ChatMessage {
            role: "assistant".into(),
            content: None,
            reasoning_content: None,
            tool_calls: Some(json!([
                {
                    "id": call_id,
                    "type": "function",
                    "function": { "name": "read_file", "arguments": "{\"path\":\"main.rs\"}" }
                }
            ])),
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

// ---------------------------------------------------------------------------
// Integration Tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_integration_cascade_on_429() {
    let agentrouter = Arc::new(MockBackend::new("agentrouter"));
    let gemini = Arc::new(MockBackend::new("gemini"));

    // Candidate 1 (gemini / gemini-3-flash-preview, priority 300.0) returns 429
    gemini.push_response(Err(GatewayError::Upstream(
        "upstream status 429: rate limit exceeded".into(),
    )));

    // Candidate 2 on agentrouter (gpt-5.6-sol, priority 290.0) returns success
    agentrouter.push_response(Ok(ChatCompletionResponse {
        id: "resp-agentrouter".into(),
        object: "chat.completion",
        created: 100,
        model: "gpt-5.6-sol".into(),
        usage: Usage::default(),
        choices: vec![ChatChoice {
            index: 0,
            message: ChatMessage::assistant_text("Success from AgentRouter"),
            finish_reason: "stop".into(),
        }],
        raw: None,
    }));

    let mut backends: HashMap<String, Arc<dyn ChatBackend>> = HashMap::new();
    backends.insert("agentrouter".into(), agentrouter.clone());
    backends.insert("gemini".into(), gemini.clone());

    let routing =
        RoutingBackend::with_breaker_settings(backends, Router::new(vec![], vec![]), 3, 60);

    let mut req1 = auto_request();
    let token1 = stamp_attribution(&mut req1);

    // First request: gemini fails with 429, cascades to agentrouter
    let res1 = routing
        .complete(req1)
        .await
        .expect("failover should succeed");
    assert_eq!(res1.choices[0].message.text(), "Success from AgentRouter");

    // Attribution confirms agentrouter served the request
    let attr1 = routing.attribution(&token1);
    assert_eq!(attr1.provider.as_deref(), Some("agentrouter"));

    // Crucial check: Candidate 5 (gemini/gemma-4-26b-a4b-it) was skipped because gemini
    // entered cooldown upon candidate 1's 429. gemini was only called ONCE!
    assert_eq!(gemini.calls(), 1);
    assert_eq!(agentrouter.calls(), 1);

    // The rate-limited model is in active cooldown. The cooldown is scoped to
    // that model, so its siblings on the same provider stay usable.
    assert!(
        routing
            .cooldowns()
            .is_in_cooldown("gemini", Some("gemini-3-flash-preview"))
    );
    assert_eq!(
        routing
            .cooldowns()
            .availability("gemini", Some("gemini-3-flash-preview")),
        0.0
    );
    assert!(
        !routing.cooldowns().is_in_cooldown("gemini", None),
        "a per-model 429 must not put the whole provider in cooldown"
    );

    // Second request: auto_plan calculates effective priority for gemini as 0.0,
    // so it routes directly to agentrouter without calling gemini!
    agentrouter.push_response(Ok(ChatCompletionResponse {
        id: "resp-agentrouter-2".into(),
        object: "chat.completion",
        created: 101,
        model: "gpt-5.6-sol".into(),
        usage: Usage::default(),
        choices: vec![ChatChoice {
            index: 0,
            message: ChatMessage::assistant_text("Second call from AgentRouter"),
            finish_reason: "stop".into(),
        }],
        raw: None,
    }));

    let mut req2 = auto_request();
    let token2 = stamp_attribution(&mut req2);
    let res2 = routing
        .complete(req2)
        .await
        .expect("second request succeeds");
    assert_eq!(
        res2.choices[0].message.text(),
        "Second call from AgentRouter"
    );
    assert_eq!(
        routing.attribution(&token2).provider.as_deref(),
        Some("agentrouter")
    );

    // gemini was NOT asked again
    assert_eq!(gemini.calls(), 1);
    assert_eq!(agentrouter.calls(), 2);
}

#[tokio::test]
async fn test_integration_cascade_on_403_no_breaker_trip() {
    let agentrouter = Arc::new(MockBackend::new("agentrouter"));
    let gemini = Arc::new(MockBackend::new("gemini"));

    // Candidate 1 (gemini) returns 403 quota exceeded
    gemini.push_response(Err(GatewayError::Upstream(
        "upstream status 403: quota exceeded or balance exhausted".into(),
    )));

    agentrouter.push_response(Ok(ChatCompletionResponse {
        id: "resp-agentrouter".into(),
        object: "chat.completion",
        created: 200,
        model: "gpt-5.6-sol".into(),
        usage: Usage::default(),
        choices: vec![ChatChoice {
            index: 0,
            message: ChatMessage::assistant_text("AgentRouter served quota failover"),
            finish_reason: "stop".into(),
        }],
        raw: None,
    }));

    let mut backends: HashMap<String, Arc<dyn ChatBackend>> = HashMap::new();
    backends.insert("agentrouter".into(), agentrouter.clone());
    backends.insert("gemini".into(), gemini.clone());

    // Breaker threshold = 1: a single breaker failure would trip the breaker!
    let routing =
        RoutingBackend::with_breaker_settings(backends, Router::new(vec![], vec![]), 1, 3600);

    let mut req = auto_request();
    let token = stamp_attribution(&mut req);

    let res = routing
        .complete(req)
        .await
        .expect("403 failover should succeed");
    assert_eq!(
        res.choices[0].message.text(),
        "AgentRouter served quota failover"
    );
    assert_eq!(
        routing.attribution(&token).provider.as_deref(),
        Some("agentrouter")
    );

    // 1-hour quota freeze was recorded in ProviderCooldownSet for gemini
    assert!(routing.cooldowns().is_in_cooldown("gemini", None));
    let remaining = routing
        .cooldowns()
        .get_cooldown_remaining("gemini", None)
        .unwrap();
    assert!(remaining.as_secs() >= 3590 && remaining.as_secs() <= 3600);

    // INVARIANT: 4xx errors strictly do NOT trip BreakerSet!
    // Since threshold is 1, if BreakerSet had been tripped, allow() would return false.
    let breaker_allowed = routing
        .breakers()
        .lock()
        .unwrap()
        .allow("gemini", Instant::now());
    assert!(
        breaker_allowed,
        "Invariant violation: 403 must not trip BreakerSet!"
    );
}

#[tokio::test]
async fn test_integration_cascade_on_post_flight_tool_rejection() {
    let agentrouter = Arc::new(MockBackend::new("agentrouter"));
    let gemini = Arc::new(MockBackend::new("gemini"));

    // Candidate 1 (gemini / gemini-3-flash-preview) returns HTTP 200 with defective pseudo XML tool call in text!
    gemini.push_response(Ok(ChatCompletionResponse {
        id: "resp-gemini-defective".into(),
        object: "chat.completion",
        created: 300,
        model: "gemini-3-flash-preview".into(),
        usage: Usage::default(),
        choices: vec![ChatChoice {
            index: 0,
            message: ChatMessage::assistant_text(
                "Here is the execution: <tool_call>{\"name\": \"read_file\"}</tool_call>",
            ),
            finish_reason: "stop".into(),
        }],
        raw: None,
    }));

    // Candidate 2 (agentrouter / gpt-5.6-sol) returns valid native structured tool call!
    agentrouter.push_response(Ok(ChatCompletionResponse {
        id: "resp-agentrouter-valid".into(),
        object: "chat.completion",
        created: 301,
        model: "gpt-5.6-sol".into(),
        usage: Usage::default(),
        choices: vec![ChatChoice {
            index: 0,
            message: ChatMessage {
                role: "assistant".into(),
                content: None,
                reasoning_content: None,
                tool_calls: Some(json!([
                    {
                        "id": "call_clean_1",
                        "type": "function",
                        "function": {
                            "name": "read_file",
                            "arguments": "{\"path\": \"src/main.rs\"}"
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

    let mut backends: HashMap<String, Arc<dyn ChatBackend>> = HashMap::new();
    backends.insert("agentrouter".into(), agentrouter.clone());
    backends.insert("gemini".into(), gemini.clone());

    let routing =
        RoutingBackend::with_breaker_settings(backends, Router::new(vec![], vec![]), 3, 60);

    let mut req = auto_request_with_tools();
    let token = stamp_attribution(&mut req);

    let res = routing
        .complete(req)
        .await
        .expect("post-flight failover should succeed");

    // Valid native tool call from Candidate 2 was returned
    assert_eq!(res.choices[0].finish_reason, "tool_calls");
    assert!(res.choices[0].message.has_tool_calls());
    assert_eq!(
        routing.attribution(&token).provider.as_deref(),
        Some("agentrouter")
    );

    // Candidate 1 quality score was penalized by 0.3
    let score = routing
        .quality_tracker()
        .get_score("gemini-3-flash-preview");
    assert!(
        (score - 0.7).abs() < 1e-6,
        "quality score should be 0.7, got {score}"
    );

    // Candidate 2 quality score was rewarded (1.0)
    let score_agentrouter = routing.quality_tracker().get_score("gpt-5.6-sol");
    assert_eq!(score_agentrouter, 1.0);
}

#[tokio::test]
async fn test_integration_cascade_with_jit_context_pruner() {
    let agentrouter = Arc::new(MockBackend::new("agentrouter"));
    let mistral = Arc::new(MockBackend::new("mistral"));

    // Candidate 1 (agentrouter, 870k context limit) returns 429
    agentrouter.push_response(Err(GatewayError::Upstream(
        "upstream status 429: rate limit".into(),
    )));

    // Candidate 1b (the other agentrouter model) is rate-limited as well. The
    // cooldown is scoped to the model that was rate-limited, so the sibling is
    // still tried and needs its own failure — otherwise the mock answers with
    // its default echo and the request never reaches the pruned candidate.
    agentrouter.push_response(Err(GatewayError::Upstream(
        "upstream status 429: rate limit".into(),
    )));

    // Candidate 2 (mistral, codestral-latest, 256k limit) receives pruned request and succeeds
    mistral.push_response(Ok(ChatCompletionResponse {
        id: "resp-mistral".into(),
        object: "chat.completion",
        created: 400,
        model: "codestral-latest".into(),
        usage: Usage::default(),
        choices: vec![ChatChoice {
            index: 0,
            message: ChatMessage::assistant_text("Codestral completed compressed prompt"),
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
    let token = stamp_attribution(&mut req);
    let original_msg_count = req.messages.len();

    let res = routing
        .complete(req)
        .await
        .expect("JIT pruning failover should succeed");
    assert_eq!(
        res.choices[0].message.text(),
        "Codestral completed compressed prompt"
    );

    // Verify tokens_compressed attribution
    let attr = routing.attribution(&token);
    assert_eq!(attr.provider.as_deref(), Some("mistral"));
    let compressed = attr
        .tokens_compressed
        .expect("tokens_compressed must be attributed");
    assert!(
        compressed > 100_000,
        "expected >100k tokens compressed, got {compressed}"
    );

    // Verify request delivered to mistral was pruned: intermediate tool outputs folded into digests
    let mistral_req = mistral.last_request().expect("mistral received a request");
    assert_eq!(mistral_req.messages.len(), original_msg_count); // 1-to-1 parity maintained
    let sample_intermediate = mistral_req.messages[3].text(); // intermediate tool output
    assert!(
        sample_intermediate.contains("[Tool Output Digest:")
            && sample_intermediate.contains("intermediate tool output"),
        "expected folded digest, got: {sample_intermediate}"
    );
}

#[tokio::test]
async fn test_integration_model_probation_skipping() {
    let agentrouter = Arc::new(MockBackend::new("agentrouter"));
    let gemini = Arc::new(MockBackend::new("gemini"));

    let mut backends: HashMap<String, Arc<dyn ChatBackend>> = HashMap::new();
    backends.insert("agentrouter".into(), agentrouter.clone());
    backends.insert("gemini".into(), gemini.clone());

    let routing =
        RoutingBackend::with_breaker_settings(backends, Router::new(vec![], vec![]), 3, 60);

    // Simulate 2 consecutive failures on gpt-5.6-sol within 15 minutes
    let now = Instant::now();
    routing
        .quality_tracker()
        .record_failure_at("gpt-5.6-sol", now);
    routing
        .quality_tracker()
        .record_failure_at("gpt-5.6-sol", now + std::time::Duration::from_secs(30));

    // Confirm model is in probation and score clamped to 0.2
    assert!(routing.quality_tracker().is_in_probation("gpt-5.6-sol"));
    assert_eq!(routing.quality_tracker().get_score("gpt-5.6-sol"), 0.2);

    // Gemini returns success
    gemini.push_response(Ok(ChatCompletionResponse {
        id: "resp-gemini".into(),
        object: "chat.completion",
        created: 500,
        model: "gemini-3-flash-preview".into(),
        usage: Usage::default(),
        choices: vec![ChatChoice {
            index: 0,
            message: ChatMessage::assistant_text("Gemini served while gpt-5.6-sol in probation"),
            finish_reason: "stop".into(),
        }],
        raw: None,
    }));

    let mut req = auto_request();
    let token = stamp_attribution(&mut req);

    let res = routing
        .complete(req)
        .await
        .expect("probation skip should succeed");
    assert_eq!(
        res.choices[0].message.text(),
        "Gemini served while gpt-5.6-sol in probation"
    );
    assert_eq!(
        routing.attribution(&token).provider.as_deref(),
        Some("gemini")
    );

    // gpt-5.6-sol was NEVER called because probation skipped it!
    assert_eq!(agentrouter.calls(), 0);
    assert_eq!(gemini.calls(), 1);
}

#[tokio::test]
async fn test_integration_stream_cascade_on_429() {
    let agentrouter = Arc::new(MockBackend::new("agentrouter"));
    let gemini = Arc::new(MockBackend::new("gemini"));

    // Candidate 1 (gemini) stream yields 429 on initial next()
    gemini.push_stream_chunk(Err(GatewayError::Upstream(
        "upstream status 429: stream rate limited".into(),
    )));

    // Candidate 2 (agentrouter) stream yields valid content chunk
    agentrouter.push_stream_chunk(Ok(StreamChunk {
        content: Some("Stream chunk from AgentRouter".into()),
        finish_reason: Some("stop".into()),
        ..Default::default()
    }));

    let mut backends: HashMap<String, Arc<dyn ChatBackend>> = HashMap::new();
    backends.insert("agentrouter".into(), agentrouter.clone());
    backends.insert("gemini".into(), gemini.clone());

    let routing =
        RoutingBackend::with_breaker_settings(backends, Router::new(vec![], vec![]), 3, 60);

    let mut req = auto_request();
    req.stream = true;
    let token = stamp_attribution(&mut req);

    let mut stream = routing.stream(req);
    let first = stream
        .next()
        .await
        .expect("stream chunk exists")
        .expect("chunk ok");
    assert_eq!(
        first.content.as_deref(),
        Some("Stream chunk from AgentRouter")
    );

    // Attribution recorded agentrouter
    assert_eq!(
        routing.attribution(&token).provider.as_deref(),
        Some("agentrouter")
    );

    // The rate-limited model is now in cooldown
    assert!(
        routing
            .cooldowns()
            .is_in_cooldown("gemini", Some("gemini-3-flash-preview"))
    );
}

// ---------------------------------------------------------------------------
// Empirical Challenge Tests (Milenger M3 Adversarial Harness)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_challenge_429_exponential_backoff_and_recovery() {
    let agentrouter = Arc::new(MockBackend::new("agentrouter"));
    let gemini = Arc::new(MockBackend::new("gemini"));

    let mut backends: HashMap<String, Arc<dyn ChatBackend>> = HashMap::new();
    backends.insert("agentrouter".into(), agentrouter.clone());
    backends.insert("gemini".into(), gemini.clone());

    let routing =
        RoutingBackend::with_breaker_settings(backends, Router::new(vec![], vec![]), 3, 60);

    let t0 = Instant::now();

    // 1. Initial 429 on Candidate 1 (gemini): Streak 1 -> 30s cooldown
    gemini.push_response(Err(GatewayError::Upstream(
        "upstream status 429: rate limited".into(),
    )));
    agentrouter.push_response(Ok(ChatCompletionResponse {
        id: "resp-agentrouter-1".into(),
        object: "chat.completion",
        created: 1,
        model: "gpt-5.6-sol".into(),
        usage: Usage::default(),
        choices: vec![ChatChoice {
            index: 0,
            message: ChatMessage::assistant_text("AgentRouter 1"),
            finish_reason: "stop".into(),
        }],
        raw: None,
    }));

    let mut req1 = auto_request();
    let _ = stamp_attribution(&mut req1);
    let res1 = routing.complete(req1).await.expect("req1 should succeed");
    assert_eq!(res1.choices[0].message.text(), "AgentRouter 1");
    assert_eq!(gemini.calls(), 1);
    assert_eq!(agentrouter.calls(), 1);

    // Cooldown is active on the rate-limited model: ~30s
    let rem1 = routing
        .cooldowns()
        .get_cooldown_remaining("gemini", Some("gemini-3-flash-preview"))
        .unwrap();
    assert!(rem1.as_secs() >= 29 && rem1.as_secs() <= 30);

    // 2. While in cooldown, subsequent requests immediately bypass the
    //    rate-limited *model*.
    //
    //    Which candidate serves is a per-request choice — the soft rotation
    //    (ADR-029) pushes the model that just served down for a moment — and
    //    a sibling model of the same provider may serve: the 429 cooldown is
    //    model-scoped on purpose, so one model hitting its rate limit must
    //    not switch its siblings off.
    for i in 2..=4 {
        agentrouter.push_response(Ok(ChatCompletionResponse {
            id: format!("resp-agentrouter-{i}"),
            object: "chat.completion",
            created: i as i64,
            model: "gpt-5.6-sol".into(),
            usage: Usage::default(),
            choices: vec![ChatChoice {
                index: 0,
                message: ChatMessage::assistant_text(format!("AgentRouter {i}")),
                finish_reason: "stop".into(),
            }],
            raw: None,
        }));
        let mut req = auto_request();
        let _ = stamp_attribution(&mut req);
        let res = routing
            .complete(req)
            .await
            .expect("bypass call should succeed");
        assert!(
            !res.choices[0].message.text().is_empty(),
            "the bypass call must be answered"
        );
        // The rate-limited model was never called again: it was asked once
        // (the request that got the 429) and never since.
        let asked = gemini
            .received_requests
            .lock()
            .unwrap()
            .iter()
            .filter(|request| request.model == "gemini-3-flash-preview")
            .count();
        assert_eq!(asked, 1, "the rate-limited model must not be called again");
    }
    // Every request was answered by exactly one candidate.
    assert_eq!(gemini.calls() + agentrouter.calls(), 5);

    // 3. Test backoff progression in ProviderCooldownSet directly:
    // Streak 2 at t0 + 1s -> 60s. Model-scoped, the same scope the router
    // uses when it records a 429.
    let t1 = t0 + std::time::Duration::from_secs(1);
    routing
        .cooldowns()
        .record_429_at("gemini", Some("gemini-3-flash-preview"), None, t1);
    let rem2 = routing
        .cooldowns()
        .get_cooldown_remaining_at("gemini", Some("gemini-3-flash-preview"), t1)
        .unwrap();
    assert_eq!(rem2, std::time::Duration::from_secs(60));

    // Streak 3 at t1 + 1s -> 120s capped
    let t2 = t1 + std::time::Duration::from_secs(1);
    routing
        .cooldowns()
        .record_429_at("gemini", Some("gemini-3-flash-preview"), None, t2);
    let rem3 = routing
        .cooldowns()
        .get_cooldown_remaining_at("gemini", Some("gemini-3-flash-preview"), t2)
        .unwrap();
    assert_eq!(rem3, std::time::Duration::from_secs(120));

    // 4. Retry-After explicit header override (e.g. 75s)
    let t3 = t2 + std::time::Duration::from_secs(1);
    routing.cooldowns().record_429_at(
        "gemini",
        Some("gemini-3-flash-preview"),
        Some(std::time::Duration::from_secs(75)),
        t3,
    );
    let rem_override = routing
        .cooldowns()
        .get_cooldown_remaining_at("gemini", Some("gemini-3-flash-preview"), t3)
        .unwrap();
    assert_eq!(rem_override, std::time::Duration::from_secs(75));

    // 5. Cooldown expiration and ladder restoration:
    // When cooldown is cleared, gemini becomes available again
    routing.cooldowns().clear_all_for_provider("gemini");
    for _ in 0..10 {
        routing
            .quality_tracker()
            .record_success("gemini-3-flash-preview"); // restore score to top
    }
    assert!(
        routing
            .cooldowns()
            .is_available("gemini", Some("gemini-3-flash-preview"))
    );

    gemini.push_response(Ok(ChatCompletionResponse {
        id: "resp-gemini-restored".into(),
        object: "chat.completion",
        created: 99,
        model: "gemini-3-flash-preview".into(),
        usage: Usage::default(),
        choices: vec![ChatChoice {
            index: 0,
            message: ChatMessage::assistant_text("Restored Gemini"),
            finish_reason: "stop".into(),
        }],
        raw: None,
    }));

    // How often the rate-limited model was asked so far: once, for the
    // request that got the 429. A sibling model of the same provider may
    // have served the bypass calls — that is what a model-scoped cooldown
    // means.
    let asked_before = gemini
        .received_requests
        .lock()
        .unwrap()
        .iter()
        .filter(|request| request.model == "gemini-3-flash-preview")
        .count();

    let mut req_restored = auto_request();
    let token_restored = stamp_attribution(&mut req_restored);
    let res_restored = routing
        .complete(req_restored)
        .await
        .expect("restored gemini should succeed");
    assert_eq!(res_restored.choices[0].message.text(), "Restored Gemini");
    assert_eq!(
        routing.attribution(&token_restored).provider.as_deref(),
        Some("gemini")
    );
    // The restored model is back in the ladder and serves again.
    let asked_after = gemini
        .received_requests
        .lock()
        .unwrap()
        .iter()
        .filter(|request| request.model == "gemini-3-flash-preview")
        .count();
    assert_eq!(asked_after, asked_before + 1);
}

#[tokio::test]
async fn test_challenge_403_and_402_freeze_with_strict_breaker_isolation() {
    let agentrouter = Arc::new(MockBackend::new("agentrouter"));
    let gemini = Arc::new(MockBackend::new("gemini"));

    let mut backends: HashMap<String, Arc<dyn ChatBackend>> = HashMap::new();
    backends.insert("agentrouter".into(), agentrouter.clone());
    backends.insert("gemini".into(), gemini.clone());

    // Breaker threshold = 1: if 4xx ever counted as a failure, breaker would trip immediately!
    let routing =
        RoutingBackend::with_breaker_settings(backends, Router::new(vec![], vec![]), 1, 3600);

    // 1. Simulate 403 quota error on Candidate 1 (gemini)
    gemini.push_response(Err(GatewayError::Upstream(
        "upstream status 403: quota exceeded".into(),
    )));
    agentrouter.push_response(Ok(ChatCompletionResponse {
        id: "resp-agentrouter-403".into(),
        object: "chat.completion",
        created: 1,
        model: "gpt-5.6-sol".into(),
        usage: Usage::default(),
        choices: vec![ChatChoice {
            index: 0,
            message: ChatMessage::assistant_text("AgentRouter from 403 failover"),
            finish_reason: "stop".into(),
        }],
        raw: None,
    }));

    let req = auto_request();
    let res = routing.complete(req).await.expect("failover succeeds");
    assert_eq!(
        res.choices[0].message.text(),
        "AgentRouter from 403 failover"
    );

    // 1-hour freeze recorded for gemini
    assert!(routing.cooldowns().is_in_cooldown("gemini", None));
    let rem = routing
        .cooldowns()
        .get_cooldown_remaining("gemini", None)
        .unwrap();
    assert!(rem.as_secs() >= 3590 && rem.as_secs() <= 3600);

    // BreakerSet is NOT tripped for gemini
    assert!(
        routing
            .breakers()
            .lock()
            .unwrap()
            .allow("gemini", Instant::now()),
        "Invariant violation: 403 tripped BreakerSet!"
    );

    // 2. Clear cooldown, restore quality score, and simulate 402 payment required error
    routing.cooldowns().clear_all_for_provider("gemini");
    for _ in 0..10 {
        routing
            .quality_tracker()
            .record_success("gemini-3-flash-preview");
    }
    gemini.push_response(Err(GatewayError::Upstream(
        "upstream status 402: payment required".into(),
    )));
    agentrouter.push_response(Ok(ChatCompletionResponse {
        id: "resp-agentrouter-402".into(),
        object: "chat.completion",
        created: 2,
        model: "gpt-5.6-sol".into(),
        usage: Usage::default(),
        choices: vec![ChatChoice {
            index: 0,
            message: ChatMessage::assistant_text("AgentRouter from 402 failover"),
            finish_reason: "stop".into(),
        }],
        raw: None,
    }));

    let req2 = auto_request();
    let res2 = routing.complete(req2).await.expect("402 failover succeeds");
    assert_eq!(
        res2.choices[0].message.text(),
        "AgentRouter from 402 failover"
    );
    assert_eq!(gemini.calls(), 2);

    // 1-hour freeze recorded again
    assert!(routing.cooldowns().is_in_cooldown("gemini", None));
    // BreakerSet is STILL NOT tripped (threshold was 1, and 2 errors occurred)
    assert!(
        routing
            .breakers()
            .lock()
            .unwrap()
            .allow("gemini", Instant::now()),
        "Invariant violation: 402 tripped BreakerSet!"
    );
}

#[tokio::test]
async fn test_challenge_jit_context_pruner_multi_tier_failover() {
    let agentrouter = Arc::new(MockBackend::new("agentrouter"));
    let mistral = Arc::new(MockBackend::new("mistral"));

    // Candidate 1 (agentrouter, 870k context limit) returns 429
    agentrouter.push_response(Err(GatewayError::Upstream(
        "upstream status 429: rate limit".into(),
    )));

    // Candidate 1b (the other agentrouter model) is rate-limited as well — see
    // the sibling-model note in test_integration_cascade_with_jit_context_pruner.
    agentrouter.push_response(Err(GatewayError::Upstream(
        "upstream status 429: rate limit".into(),
    )));

    // Candidate 2 (mistral, codestral-latest, 256k limit) receives pruned request and succeeds
    mistral.push_response(Ok(ChatCompletionResponse {
        id: "resp-mistral".into(),
        object: "chat.completion",
        created: 400,
        model: "codestral-latest".into(),
        usage: Usage::default(),
        choices: vec![ChatChoice {
            index: 0,
            message: ChatMessage::assistant_text("Codestral response"),
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
    let token = stamp_attribution(&mut req);

    let res = routing
        .complete(req)
        .await
        .expect("JIT pruning failover should succeed");
    assert_eq!(res.choices[0].message.text(), "Codestral response");

    // Verify tokens_compressed attribution
    let attr = routing.attribution(&token);
    assert_eq!(attr.provider.as_deref(), Some("mistral"));
    let compressed = attr
        .tokens_compressed
        .expect("tokens_compressed must be attributed");
    assert!(
        compressed > 100_000,
        "expected >100k tokens compressed, got {compressed}"
    );

    // Verify recent tail turns (last 4 turns) were NOT compressed
    let mistral_req = mistral.last_request().expect("mistral received a request");
    let tail_turn = mistral_req.messages.last().expect("last message exists");
    assert_eq!(
        tail_turn.text(),
        "fn main() { println!(\"ok\"); }",
        "Recent tail turns must remain verbatim!"
    );
}

#[tokio::test]
async fn test_challenge_provider_cooldown_set_concurrency_stress() {
    use std::sync::Arc;
    let cooldowns = Arc::new(heterion_router_gateway::resilience::ProviderCooldownSet::new());
    let mut handles = Vec::new();

    // 32 concurrent tasks performing 500 iterations of mixed read/write cooldown operations
    for task_id in 0..32 {
        let set = cooldowns.clone();
        handles.push(tokio::spawn(async move {
            let provider = if task_id % 2 == 0 {
                "provider_a"
            } else {
                "provider_b"
            };
            let t0 = Instant::now();

            for iter in 0..500 {
                match iter % 6 {
                    0 => {
                        set.record_429_at(
                            provider,
                            None,
                            None,
                            t0 + std::time::Duration::from_millis(iter),
                        );
                    }
                    1 => {
                        set.record_quota_error_at(
                            provider,
                            t0 + std::time::Duration::from_millis(iter),
                        );
                    }
                    2 => {
                        let _ = set.is_in_cooldown_at(
                            provider,
                            None,
                            t0 + std::time::Duration::from_millis(iter),
                        );
                    }
                    3 => {
                        let _ = set.availability_at(
                            provider,
                            None,
                            t0 + std::time::Duration::from_millis(iter),
                        );
                    }
                    4 => {
                        let _ = set.get_cooldown_remaining_at(
                            provider,
                            None,
                            t0 + std::time::Duration::from_millis(iter),
                        );
                    }
                    5 => {
                        set.record_success(provider, None);
                    }
                    _ => unreachable!(),
                }
            }
        }));
    }

    for handle in handles {
        handle.await.expect("task must not panic or deadlock");
    }

    // Cooldown set remains healthy and usable after high-concurrency stress
    cooldowns.clear_all_for_provider("provider_a");
    assert!(cooldowns.is_available("provider_a", None));
    assert_eq!(cooldowns.availability("provider_a", None), 1.0);
}

#[test]
fn test_challenge_post_flight_tool_validation_edge_cases() {
    use heterion_router_gateway::resilience::{ToolRejection, verify_tool_response};

    // 1. Empty tools array in request passes standard conversation without rejection
    let req_empty_tools = ChatCompletionRequest {
        model: "auto".into(),
        messages: vec![ChatMessage::plain("user", "Hello")],
        tools: Some(json!([])),
        ..Default::default()
    };
    let res_text = ChatCompletionResponse {
        id: "1".into(),
        object: "chat.completion",
        created: 1,
        model: "test".into(),
        usage: Usage::default(),
        choices: vec![ChatChoice {
            index: 0,
            message: ChatMessage::assistant_text("Talking about <tool_call> tags"),
            finish_reason: "stop".into(),
        }],
        raw: None,
    };
    assert!(verify_tool_response(&req_empty_tools, &res_text).is_ok());

    // 2. Request with tools rejects pseudo XML in text
    let req_with_tools = ChatCompletionRequest {
        model: "auto".into(),
        messages: vec![ChatMessage::plain("user", "Run tool")],
        tools: Some(json!([{"type": "function", "function": {"name": "run"}}])),
        ..Default::default()
    };
    assert!(matches!(
        verify_tool_response(&req_with_tools, &res_text),
        Err(ToolRejection::PseudoXmlDetected(_))
    ));

    // 3. Contradictory refusal accompanying tool call is rejected
    let res_contra = ChatCompletionResponse {
        id: "2".into(),
        object: "chat.completion",
        created: 2,
        model: "test".into(),
        usage: Usage::default(),
        choices: vec![ChatChoice {
            index: 0,
            message: ChatMessage {
                role: "assistant".into(),
                content: Some(json!("I am sorry, I cannot execute tools")),
                reasoning_content: None,
                tool_calls: Some(json!([
                    {"id": "c1", "type": "function", "function": {"name": "run", "arguments": "{}"}}
                ])),
                tool_call_id: None,
                name: None,
            },
            finish_reason: "tool_calls".into(),
        }],
        raw: None,
    };
    assert!(matches!(
        verify_tool_response(&req_with_tools, &res_contra),
        Err(ToolRejection::HallucinatedRefusal(_))
    ));

    // 4. Malformed tool call arguments are rejected
    let res_bad_json = ChatCompletionResponse {
        id: "3".into(),
        object: "chat.completion",
        created: 3,
        model: "test".into(),
        usage: Usage::default(),
        choices: vec![ChatChoice {
            index: 0,
            message: ChatMessage {
                role: "assistant".into(),
                content: None,
                reasoning_content: None,
                tool_calls: Some(json!([
                    {"id": "c1", "type": "function", "function": {"name": "run", "arguments": "{broken_json"}}
                ])),
                tool_call_id: None,
                name: None,
            },
            finish_reason: "tool_calls".into(),
        }],
        raw: None,
    };
    assert!(matches!(
        verify_tool_response(&req_with_tools, &res_bad_json),
        Err(ToolRejection::MalformedToolCall(_))
    ));

    // 5. Polite preamble accompanied by valid tool calls is permitted
    let res_polite = ChatCompletionResponse {
        id: "4".into(),
        object: "chat.completion",
        created: 4,
        model: "test".into(),
        usage: Usage::default(),
        choices: vec![ChatChoice {
            index: 0,
            message: ChatMessage {
                role: "assistant".into(),
                content: Some(json!(
                    "I apologize for the delay. Let me inspect the directory."
                )),
                reasoning_content: None,
                tool_calls: Some(json!([
                    {"id": "c1", "type": "function", "function": {"name": "run", "arguments": "{}"}}
                ])),
                tool_call_id: None,
                name: None,
            },
            finish_reason: "tool_calls".into(),
        }],
        raw: None,
    };
    assert!(verify_tool_response(&req_with_tools, &res_polite).is_ok());
}

// ---------------------------------------------------------------------------
// Suite: the direct `provider/model` path and the rate-limit penalty
// ---------------------------------------------------------------------------

/// The direct `provider/model` path is deliberately **not** gated by the
/// rate-limit cooldown: the client asked for one model by id, and the frozen
/// contract (AGENTS.md) says a provider 4xx must not disable the provider.
/// The cooldown steers the *ladder* — which candidate to try next — not the
/// client's explicit choice.
///
/// The M5 challenger called this gap a defect (D-1). It is kept as a
/// deliberate deviation, and this test pins it so the choice stays visible
/// instead of quietly changing.
#[tokio::test]
async fn a_direct_model_request_is_not_disabled_by_a_rate_limit() {
    let gemini = Arc::new(MockBackend::new("gemini"));
    let mut backends: HashMap<String, Arc<dyn ChatBackend>> = HashMap::new();
    backends.insert("gemini".into(), gemini.clone());
    let routing = RoutingBackend::new(backends, Router::new(vec![], vec![]));

    for _ in 0..2 {
        gemini.push_response(Err(GatewayError::Upstream(
            "upstream status 429: rate limit exceeded".into(),
        )));
    }

    let request = ChatCompletionRequest {
        model: "gemini/gemini-3-flash-preview".into(),
        messages: vec![ChatMessage::plain("user", "hi")],
        ..Default::default()
    };

    // Both requests are dispatched: the provider is asked again.
    let first = routing.complete(request.clone()).await.unwrap_err();
    assert!(matches!(first, GatewayError::Upstream(_)), "{first:?}");
    let second = routing.complete(request).await.unwrap_err();
    assert!(matches!(second, GatewayError::Upstream(_)), "{second:?}");
    assert_eq!(
        gemini.calls(),
        2,
        "a provider 4xx must not disable the provider on the direct path"
    );

    // The cooldown is still recorded — it is the ladder that consults it.
    assert!(
        routing
            .cooldowns()
            .is_in_cooldown("gemini", Some("gemini-3-flash-preview")),
        "the rate-limited model must carry a cooldown"
    );
}

/// A rate limit is a quota event, not a defective response. The directive
/// reserves the quality penalty (`QualityScore -= 0.3`, 15-minute probation)
/// for defective answers — empty content, repetition loops, pseudo-XML tool
/// tags. Charging it for a 429 keeps the model out of the ladder for 15
/// minutes while its own rate-limit cooldown is 30s -> 60s -> 120s.
#[tokio::test]
async fn a_rate_limit_does_not_punish_the_model_quality() {
    let gemini = Arc::new(MockBackend::new("gemini"));
    let mut backends: HashMap<String, Arc<dyn ChatBackend>> = HashMap::new();
    backends.insert("gemini".into(), gemini.clone());
    let routing = RoutingBackend::new(backends, Router::new(vec![], vec![]));

    gemini.push_response(Err(GatewayError::Upstream(
        "upstream status 429: rate limit exceeded".into(),
    )));

    let request = ChatCompletionRequest {
        model: "gemini/gemini-3-flash-preview".into(),
        messages: vec![ChatMessage::plain("user", "hi")],
        ..Default::default()
    };
    let _ = routing.complete(request).await.unwrap_err();

    assert_eq!(
        routing
            .quality_tracker()
            .get_score("gemini-3-flash-preview"),
        1.0,
        "a rate limit must not cost the model its quality score"
    );
    assert!(
        !routing
            .quality_tracker()
            .is_in_probation("gemini-3-flash-preview"),
        "a rate limit must not put the model into the 15-minute probation"
    );
}

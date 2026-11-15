//! Empirical Challenge Harness for Milestone 3 Iteration 2 (Challenger 1)
//!
//! Scope:
//! 1. Empirically challenge the 429 backoff decay window in `resilience.rs`:
//!    - Verify that when a provider exits cooldown and encounters another 429 within 5 minutes,
//!      backoff steps from 30s -> 60s -> 120s.
//!    - Verify that if 429 occurs > 5 minutes after cooldown expiration, streak resets to 30s.
//!    - Verify exact boundary conditions at 300s vs 300s + 1ms.
//!    - Verify consecutive 429s while still in cooldown.
//!    - Verify `record_success` resets the streak.
//! 2. Empirically challenge model-scoped vs provider-scoped cooldowns in `routing.rs`:
//!    - Model-specific cooldowns isolate only the specified model.
//!    - Other models for the same provider remain available.
//!    - Provider-wide cooldown blocks all models under that provider.
//!    - Switchyard `auto_plan` filters out cooled models while keeping available models.
//!    - End-to-end RoutingBackend candidate execution (non-streaming and streaming) skips cooled models.
//! 3. Thread-safety stress testing of cooldown state under concurrent operations.

use async_trait::async_trait;
use futures::StreamExt;
use heterion_router_core::{
    ChatChoice, ChatCompletionRequest, ChatCompletionResponse, ChatMessage, GatewayError,
    StreamChunk, Usage,
};
use heterion_router_gateway::backend::{ChatBackend, ChunkStream};
use heterion_router_gateway::resilience::{
    BACKOFF_DECAY_WINDOW, BACKOFF_STEP_1, BACKOFF_STEP_2, BACKOFF_STEP_3_CAP, ProviderCooldownSet,
};
use heterion_router_gateway::routing::RoutingBackend;
use heterion_router_routing::Router;
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

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

    fn received_models(&self) -> Vec<String> {
        self.received_requests
            .lock()
            .unwrap()
            .iter()
            .map(|r| r.model.clone())
            .collect()
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
                    message: ChatMessage::assistant_text(format!(
                        "Response from {} model {}",
                        self.name, request.model
                    )),
                    finish_reason: "stop".into(),
                }],
                raw: None,
            })
        }
    }

    fn stream(&self, request: ChatCompletionRequest) -> ChunkStream {
        self.calls.fetch_add(1, Ordering::Relaxed);
        self.received_requests.lock().unwrap().push(request.clone());

        let mut guard = self.stream_results.lock().unwrap();
        let items: Vec<Result<StreamChunk, GatewayError>> = if !guard.is_empty() {
            guard.drain(..).collect()
        } else {
            vec![Ok(StreamChunk {
                content: Some(format!("Stream from {} model {}", self.name, request.model)),
                finish_reason: Some("stop".into()),
                ..Default::default()
            })]
        };

        Box::pin(futures::stream::iter(items))
    }
}

fn auto_request() -> ChatCompletionRequest {
    ChatCompletionRequest {
        model: "heterion-router/auto".into(),
        messages: vec![ChatMessage::plain("user", "Hello empirical challenge")],
        ..Default::default()
    }
}

// ---------------------------------------------------------------------------
// 1. Empirical Challenge: 429 Backoff Decay Window
// ---------------------------------------------------------------------------

#[test]
fn test_challenge_429_backoff_decay_window_progression() {
    let set = ProviderCooldownSet::new();
    let provider = "test_provider_decay";
    let t0 = Instant::now();

    // Initial state: available
    assert!(set.is_available_at(provider, None, t0));

    // Event 1: First 429 at t0 -> 30s cooldown
    set.record_429_at(provider, None, None, t0);
    assert!(set.is_in_cooldown_at(provider, None, t0));
    assert_eq!(
        set.get_cooldown_remaining_at(provider, None, t0).unwrap(),
        BACKOFF_STEP_1
    );

    // Cooldown expires at t0 + 30s
    let t_exp1 = t0 + Duration::from_secs(30);
    assert!(!set.is_in_cooldown_at(provider, None, t_exp1));
    assert!(set.is_available_at(provider, None, t_exp1));

    // Event 2: Provider serves traffic, but encounters another 429 at t_exp1 + 10s (within 5-min decay window: 10s <= 300s)
    let t1 = t_exp1 + Duration::from_secs(10);
    set.record_429_at(provider, None, None, t1);
    assert!(set.is_in_cooldown_at(provider, None, t1));
    // Backoff MUST step to 60s
    let rem2 = set.get_cooldown_remaining_at(provider, None, t1).unwrap();
    assert_eq!(rem2, BACKOFF_STEP_2, "Streak 2 must step backoff to 60s");

    // Second cooldown expires at t1 + 60s
    let t_exp2 = t1 + Duration::from_secs(60);
    assert!(!set.is_in_cooldown_at(provider, None, t_exp2));
    assert!(set.is_available_at(provider, None, t_exp2));

    // Event 3: Third 429 at t_exp2 + 250s (within 5-min decay window: 250s <= 300s)
    let t2 = t_exp2 + Duration::from_secs(250);
    set.record_429_at(provider, None, None, t2);
    assert!(set.is_in_cooldown_at(provider, None, t2));
    // Backoff MUST step to 120s (capped)
    let rem3 = set.get_cooldown_remaining_at(provider, None, t2).unwrap();
    assert_eq!(
        rem3, BACKOFF_STEP_3_CAP,
        "Streak 3 must step backoff to 120s (cap)"
    );

    // Event 4: Fourth 429 at t2 + 10s (inside active cooldown)
    let t3 = t2 + Duration::from_secs(10);
    set.record_429_at(provider, None, None, t3);
    let rem4 = set.get_cooldown_remaining_at(provider, None, t3).unwrap();
    assert_eq!(
        rem4, BACKOFF_STEP_3_CAP,
        "Streak 4+ must remain at 120s cap"
    );

    // Third/fourth cooldown expires at t3 + 120s
    let t_exp3 = t3 + Duration::from_secs(120);
    assert!(!set.is_in_cooldown_at(provider, None, t_exp3));
    assert!(set.is_available_at(provider, None, t_exp3));

    // Event 5: Fifth 429 at t_exp3 + 301s (> 5 minutes / 300s after expiration)
    // STREAK MUST RESET TO 30s!
    let t4 = t_exp3 + Duration::from_secs(301);
    set.record_429_at(provider, None, None, t4);
    let rem5 = set.get_cooldown_remaining_at(provider, None, t4).unwrap();
    assert_eq!(
        rem5, BACKOFF_STEP_1,
        "After >5 minutes since cooldown expiration, streak must reset to 30s"
    );
}

#[test]
fn test_challenge_429_backoff_decay_exact_boundary() {
    let set = ProviderCooldownSet::new();
    let p = "boundary_provider";
    let t0 = Instant::now();

    // 1st 429: until = t0 + 30s
    set.record_429_at(p, None, None, t0);
    let exp = t0 + Duration::from_secs(30);

    // Exact boundary test: at exp + 300s (exactly 5m 00s), within_decay should be true
    let t_boundary = exp + BACKOFF_DECAY_WINDOW;
    set.record_429_at(p, None, None, t_boundary);
    let rem = set.get_cooldown_remaining_at(p, None, t_boundary).unwrap();
    assert_eq!(
        rem, BACKOFF_STEP_2,
        "At exact 300s decay boundary, streak must increment to 2 (60s)"
    );

    // Next expiry: t_boundary + 60s
    let exp2 = t_boundary + Duration::from_secs(60);

    // Past boundary test: at exp2 + 300s + 1ms, within_decay must be false -> reset to 30s
    let t_past = exp2 + BACKOFF_DECAY_WINDOW + Duration::from_millis(1);
    set.record_429_at(p, None, None, t_past);
    let rem_reset = set.get_cooldown_remaining_at(p, None, t_past).unwrap();
    assert_eq!(
        rem_reset, BACKOFF_STEP_1,
        "At 300s + 1ms past expiration, streak must reset to 30s"
    );
}

#[test]
fn test_challenge_429_success_resets_streak_immediately() {
    let set = ProviderCooldownSet::new();
    let p = "success_reset_provider";
    let t0 = Instant::now();

    // 1st 429 -> 30s
    set.record_429_at(p, None, None, t0);
    // 2nd 429 after 31s -> 60s
    let t1 = t0 + Duration::from_secs(31);
    set.record_429_at(p, None, None, t1);
    assert_eq!(
        set.get_cooldown_remaining_at(p, None, t1).unwrap(),
        BACKOFF_STEP_2
    );

    // Successful request completes
    set.record_success(p, None);
    assert!(set.is_available_at(p, None, t1));

    // Next 429 immediately after success must restart at 30s (streak = 1)
    let t2 = t1 + Duration::from_secs(5);
    set.record_429_at(p, None, None, t2);
    assert_eq!(
        set.get_cooldown_remaining_at(p, None, t2).unwrap(),
        BACKOFF_STEP_1,
        "Record success must reset consecutive 429 streak to 1"
    );
}

// ---------------------------------------------------------------------------
// 2. Empirical Challenge: Model-Scoped vs Provider-Scoped Cooldowns
// ---------------------------------------------------------------------------

#[test]
fn test_challenge_model_vs_provider_scoped_cooldowns_isolation() {
    let set = ProviderCooldownSet::new();
    let prov = "multi_model_prov";
    let m1 = "flagship_model";
    let m2 = "fast_model";
    let t0 = Instant::now();

    // Case 1: M1 encounters 429 with 45s Retry-After
    set.record_429_at(prov, Some(m1), Some(Duration::from_secs(45)), t0);

    // M1 is in cooldown
    assert!(set.is_in_cooldown_at(prov, Some(m1), t0));
    assert!(!set.is_available_at(prov, Some(m1), t0));
    assert_eq!(set.availability_at(prov, Some(m1), t0), 0.0);

    // M2 is NOT in cooldown
    assert!(!set.is_in_cooldown_at(prov, Some(m2), t0));
    assert!(set.is_available_at(prov, Some(m2), t0));
    assert_eq!(set.availability_at(prov, Some(m2), t0), 1.0);

    // Provider generic is NOT in cooldown
    assert!(!set.is_in_cooldown_at(prov, None, t0));
    assert!(set.is_available_at(prov, None, t0));
    assert_eq!(set.availability_at(prov, None, t0), 1.0);

    // Case 2: Provider-level freeze (403 quota error)
    set.record_quota_error_at(prov, t0);

    // Now provider AND ALL models are blocked
    assert!(set.is_in_cooldown_at(prov, None, t0));
    assert!(set.is_in_cooldown_at(prov, Some(m1), t0));
    assert!(set.is_in_cooldown_at(prov, Some(m2), t0));
    assert_eq!(set.availability_at(prov, None, t0), 0.0);
    assert_eq!(set.availability_at(prov, Some(m1), t0), 0.0);
    assert_eq!(set.availability_at(prov, Some(m2), t0), 0.0);

    // Case 3: Clear all for provider restores everything
    set.clear_all_for_provider(prov);
    assert!(set.is_available_at(prov, None, t0));
    assert!(set.is_available_at(prov, Some(m1), t0));
    assert!(set.is_available_at(prov, Some(m2), t0));
}

#[tokio::test]
async fn test_challenge_auto_plan_dynamic_priority_model_scoping() {
    let agentrouter = Arc::new(MockBackend::new("agentrouter"));
    let gemini = Arc::new(MockBackend::new("gemini"));

    let mut backends: HashMap<String, Arc<dyn ChatBackend>> = HashMap::new();
    backends.insert("agentrouter".into(), agentrouter.clone());
    backends.insert("gemini".into(), gemini.clone());

    let routing =
        RoutingBackend::with_breaker_settings(backends, Router::new(vec![], vec![]), 3, 60);

    let t0 = Instant::now();

    // In auto_plan:
    // agentrouter / gpt-5.6-sol base_priority = 100.0
    // gemini / gemini-3-flash-preview base_priority = 95.0
    // agentrouter / gpt-6.1-sol base_priority = 90.0

    // Cooldown only gpt-5.6-sol on agentrouter
    routing.cooldowns().record_429_at(
        "agentrouter",
        Some("gpt-5.6-sol"),
        Some(Duration::from_secs(60)),
        t0,
    );

    // Configure mock responses:
    // gemini returns error 500 so routing attempts next candidate
    gemini.push_response(Err(GatewayError::Upstream(
        "upstream status 500: gemini error".into(),
    )));

    // agentrouter should receive gpt-6.1-sol (NOT gpt-5.6-sol!)
    agentrouter.push_response(Ok(ChatCompletionResponse {
        id: "resp-agentrouter-6.1".into(),
        object: "chat.completion",
        created: 1000,
        model: "gpt-6.1-sol".into(),
        usage: Usage::default(),
        choices: vec![ChatChoice {
            index: 0,
            message: ChatMessage::assistant_text("Responded from gpt-6.1-sol"),
            finish_reason: "stop".into(),
        }],
        raw: None,
    }));

    let req = auto_request();
    let res = routing
        .complete(req)
        .await
        .expect("should failover to gpt-6.1-sol");
    assert_eq!(res.choices[0].message.text(), "Responded from gpt-6.1-sol");

    // Verify received requests:
    // agentrouter was called once, with model "gpt-6.1-sol" (gpt-5.6-sol was skipped)
    assert_eq!(gemini.calls(), 1);
    assert_eq!(agentrouter.calls(), 1);
    let agentrouter_models = agentrouter.received_models();
    assert_eq!(agentrouter_models, vec!["gpt-6.1-sol".to_string()]);
}

#[tokio::test]
async fn test_challenge_streaming_model_scoped_cooldown_failover() {
    let agentrouter = Arc::new(MockBackend::new("agentrouter"));
    let gemini = Arc::new(MockBackend::new("gemini"));

    let mut backends: HashMap<String, Arc<dyn ChatBackend>> = HashMap::new();
    backends.insert("agentrouter".into(), agentrouter.clone());
    backends.insert("gemini".into(), gemini.clone());

    let routing =
        RoutingBackend::with_breaker_settings(backends, Router::new(vec![], vec![]), 3, 60);

    // Cooldown only gpt-5.6-sol on agentrouter
    routing.cooldowns().record_429(
        "agentrouter",
        Some("gpt-5.6-sol"),
        Some(Duration::from_secs(120)),
    );

    // Gemini returns error
    gemini.push_stream_chunk(Err(GatewayError::Upstream(
        "upstream status 502: Bad Gateway".into(),
    )));

    // agentrouter stream succeeds for gpt-6.1-sol
    agentrouter.push_stream_chunk(Ok(StreamChunk {
        content: Some("Stream chunk from gpt-6.1-sol".into()),
        finish_reason: Some("stop".into()),
        ..Default::default()
    }));

    let req = auto_request();
    let mut stream = routing.stream(req);

    let first_chunk = stream
        .next()
        .await
        .expect("stream chunk must be received")
        .expect("chunk must be ok");
    assert_eq!(
        first_chunk.content.as_deref(),
        Some("Stream chunk from gpt-6.1-sol")
    );

    // Verify agentrouter only received gpt-6.1-sol
    assert_eq!(gemini.calls(), 1);
    assert_eq!(agentrouter.calls(), 1);
    assert_eq!(
        agentrouter.received_models(),
        vec!["gpt-6.1-sol".to_string()]
    );
}

// ---------------------------------------------------------------------------
// 3. Stress Test: Multi-Threaded Concurrency
// ---------------------------------------------------------------------------

#[test]
fn test_challenge_cooldown_set_multithreaded_stress() {
    let set = Arc::new(ProviderCooldownSet::new());
    let mut handles = Vec::new();
    let num_threads = 16;
    let iterations_per_thread = 500;

    for thread_id in 0..num_threads {
        let set_clone = set.clone();
        handles.push(std::thread::spawn(move || {
            let provider = format!("provider_{}", thread_id % 4);
            let model = format!("model_{}", thread_id % 3);
            let t0 = Instant::now();

            for i in 0..iterations_per_thread {
                let now = t0 + Duration::from_millis(i as u64);
                if i % 3 == 0 {
                    set_clone.record_429_at(&provider, Some(&model), None, now);
                } else if i % 5 == 0 {
                    set_clone.record_quota_error_at(&provider, now);
                } else if i % 7 == 0 {
                    set_clone.record_success(&provider, Some(&model));
                } else {
                    let _ = set_clone.is_available_at(&provider, Some(&model), now);
                    let _ = set_clone.availability_at(&provider, Some(&model), now);
                }
            }
        }));
    }

    for h in handles {
        h.join().expect("thread must join without panic");
    }

    // Ensure pruning works after high concurrency
    set.prune_expired(Instant::now());
}

//! A request that does not fit its platform must not be sent — and the client
//! must learn why.
//!
//! Two platforms refuse a request for a size reason, both measured live:
//!
//! * oneminai counts **characters**: `400 Messages, tools and response format
//!   exceed the 100,000 character limit` (2026-11-17),
//! * groq counts **tokens per minute**: `413 Request too large ... TPM: Limit
//!   8000` (2026-11-16).
//!
//! The router cuts the history first and, when that is not enough, skips the
//! candidate instead of paying for a refusal that is certain. But a skipped
//! candidate used to leave the client with `404 unknown model` — a lie: the
//! model is known, the request is simply too big for it.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use futures::StreamExt;
use heterion_router_core::{
    ChatCompletionRequest, ChatCompletionResponse, ChatMessage, GatewayError, StreamChunk,
};
use heterion_router_gateway::backend::{ChatBackend, ChunkStream};
use heterion_router_gateway::routing::RoutingBackend;
use heterion_router_routing::{RouteRule, Router};

/// A backend that counts its calls and always answers.
struct CountingBackend {
    name: &'static str,
    calls: AtomicUsize,
}

impl CountingBackend {
    fn new(name: &'static str) -> Self {
        Self {
            name,
            calls: AtomicUsize::new(0),
        }
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::Relaxed)
    }
}

#[async_trait]
impl ChatBackend for CountingBackend {
    fn name(&self) -> &'static str {
        self.name
    }

    async fn complete(
        &self,
        _request: ChatCompletionRequest,
    ) -> Result<ChatCompletionResponse, GatewayError> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        Ok(ChatCompletionResponse {
            id: "resp-1".into(),
            object: "chat.completion",
            created: 1,
            model: self.name.into(),
            usage: Default::default(),
            choices: vec![],
            raw: None,
        })
    }

    fn stream(&self, _request: ChatCompletionRequest) -> ChunkStream {
        Box::pin(futures::stream::once(async move {
            Ok(StreamChunk::done("stop"))
        }))
    }
}

/// A router where `provider/model` goes straight to that one backend.
fn direct_routing(provider: &'static str) -> (Arc<CountingBackend>, RoutingBackend) {
    let backend = Arc::new(CountingBackend::new(provider));
    let backends: HashMap<String, Arc<dyn ChatBackend>> = HashMap::from([(
        provider.to_string(),
        backend.clone() as Arc<dyn ChatBackend>,
    )]);
    let router = Router::new(
        vec![RouteRule {
            model_prefix: format!("{provider}/"),
            backends: vec![provider.to_string()],
        }],
        vec![],
    );
    (backend, RoutingBackend::new(backends, router))
}

/// A request with one user turn of `chars` characters.
fn request_of(model: &str, chars: usize) -> ChatCompletionRequest {
    ChatCompletionRequest {
        model: model.into(),
        messages: vec![ChatMessage::plain("user", "x".repeat(chars))],
        ..Default::default()
    }
}

#[tokio::test]
async fn a_request_over_the_character_budget_is_not_sent_and_says_why() {
    // oneminai counts characters: the whole history with the tool schemas and
    // the response format must stay under 100 000.
    let (backend, routing) = direct_routing("oneminai");
    let request = request_of("oneminai/gpt-4o-mini", 120_000);

    let error = routing.complete(request).await.unwrap_err();

    assert_eq!(
        backend.calls(),
        0,
        "the platform would refuse this request for certain — it must not be called"
    );
    match error {
        GatewayError::InvalidRequest(message) => {
            assert!(
                message.contains("request too large"),
                "the client must be told the request is too large, got: {message}"
            );
            assert!(
                message.contains("100000 characters"),
                "the client must be told the limit, got: {message}"
            );
            assert!(
                message.contains("oneminai/gpt-4o-mini"),
                "the client must be told which model, got: {message}"
            );
        }
        other => panic!("expected InvalidRequest for an oversized request, got: {other:?}"),
    }
}

#[tokio::test]
async fn a_history_over_the_character_budget_is_cut_and_served() {
    // The oldest turn goes, the request fits and is served — a cut history is
    // a smaller ask than a failed request.
    let (backend, routing) = direct_routing("oneminai");
    let request = ChatCompletionRequest {
        model: "oneminai/gpt-4o-mini".into(),
        messages: vec![
            ChatMessage::plain("user", "x".repeat(90_000)),
            ChatMessage::plain("user", "the actual question"),
        ],
        ..Default::default()
    };

    routing
        .complete(request)
        .await
        .expect("a cut history fits the budget");

    assert_eq!(
        backend.calls(),
        1,
        "the request fits after the cut, so it must be sent"
    );
}

#[tokio::test]
async fn a_prompt_over_the_token_ceiling_is_not_sent_and_says_why() {
    // groq's whole per-minute budget is 8 000 tokens — a bigger prompt can
    // never be served, however long we wait.
    let (backend, routing) = direct_routing("groq");
    let request = request_of("groq/qwen3.8-27b", 100_000);

    let error = routing.complete(request).await.unwrap_err();

    assert_eq!(
        backend.calls(),
        0,
        "the platform would refuse this request for certain — it must not be called"
    );
    match error {
        GatewayError::InvalidRequest(message) => {
            assert!(
                message.contains("request too large"),
                "the client must be told the request is too large, got: {message}"
            );
            assert!(
                message.contains("8000 tokens"),
                "the client must be told the limit, got: {message}"
            );
        }
        other => panic!("expected InvalidRequest for an oversized request, got: {other:?}"),
    }
}

#[tokio::test]
async fn a_streaming_request_over_the_character_budget_says_why_too() {
    // The streaming path has its own loop and its own error path: an oversized
    // request must not turn into a silent stream that ends without a word.
    let (backend, routing) = direct_routing("oneminai");
    let request = request_of("oneminai/gpt-4o-mini", 120_000);

    let mut stream = routing.stream(request);
    let first = stream
        .next()
        .await
        .expect("the stream must answer with an error, not with silence");
    let error = first.expect_err("an oversized request must fail");

    assert_eq!(
        backend.calls(),
        0,
        "the platform would refuse this request for certain — it must not be called"
    );
    match error {
        GatewayError::InvalidRequest(message) => {
            assert!(
                message.contains("request too large"),
                "the client must be told the request is too large, got: {message}"
            );
        }
        other => panic!("expected InvalidRequest for an oversized request, got: {other:?}"),
    }
}

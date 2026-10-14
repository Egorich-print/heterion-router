//! Routing backend: model-based selection with failover.
//!
//! A [`RoutingBackend`] holds named backends, asks the [`Router`] which to
//! try for a model, skips tripped breakers, and falls back on failure for
//! non-streaming requests. Streaming uses the first allowed backend (no
//! mid-stream failover yet). Because the router dispatches to backends that
//! do the real serving, it also records which one served each request, keyed
//! by the gateway's request token, so usage rows name a provider instead of
//! `routing`.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use async_trait::async_trait;
use futures::StreamExt;
use heterion_router_core::{ChatCompletionRequest, ChatCompletionResponse, GatewayError};
use heterion_router_db::Db;
use heterion_router_providers::ProviderRegistry;
use heterion_router_routing::{BreakerSet, Router};
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;

use crate::backend::{ATTRIBUTION_TOKEN, Attribution, ChatBackend, ChunkStream, request_token};
use crate::combos::{ComboConfig, load_combos, plan_steps};
use crate::upstream_status_or_recovered;

/// How many finished requests keep an attribution for. Bounded so a request
/// that is never booked (a `no_log` key) cannot grow the ring without limit.
const ATTRIBUTION_SLOTS: usize = 256;

/// Everything one request needs before the first attempt.
struct Plan {
    /// Retry/failover settings from the combo row, defaults otherwise.
    config: ComboConfig,
    /// Ordered `(backend, model)` attempts; empty means "nothing can serve it".
    steps: Vec<(String, String)>,
    /// Combo the request is served through, when it is served through one.
    combo_name: Option<String>,
}

/// Backend router with failover.
#[derive(Clone)]
pub struct RoutingBackend {
    backends: Arc<HashMap<String, Arc<dyn ChatBackend>>>,
    router: Arc<Router>,
    breakers: Arc<Mutex<BreakerSet>>,
    db: Option<Arc<Db>>,
    registry: Option<Arc<ProviderRegistry>>,
    /// Who served which request: `request token -> Attribution`.
    ///
    /// Keyed rather than a single "last served" slot, because concurrent
    /// requests would otherwise read each other's provider.
    attributions: Arc<Mutex<Vec<(String, Attribution)>>>,
}

/// Whether a failure means the backend itself is unhealthy.
///
/// A provider answering 4xx — "model not found", "no access", "bad payload" —
/// is healthy: it served the request, it just refused it. Tripping its breaker
/// over one dead model id takes every *other* model on that provider down for
/// the whole cooldown, which is how a single stale combo entry used to disable
/// a working provider.
fn is_backend_outage(error: &GatewayError) -> bool {
    match error {
        GatewayError::Upstream(message) => match upstream_status_or_recovered(message) {
            Some(status) => status >= 500,
            // No status means the call never landed (transport, DNS, TLS,
            // exhausted credentials): the backend is unreachable.
            None => true,
        },
        // Nothing-to-serve is a routing outcome, not a backend fault.
        GatewayError::UnknownModel(_) | GatewayError::InvalidRequest(_) => false,
    }
}

/// Error for a plan that exists but is entirely in breaker cooldown.
fn all_candidates_tripped(model: &str) -> GatewayError {
    GatewayError::Upstream(format!(
        "{model}: every candidate backend is in breaker cooldown after recent failures; retry shortly"
    ))
}

/// Error for a model no configured backend can serve at all.
fn no_candidate(model: &str) -> GatewayError {
    GatewayError::UnknownModel(format!(
        "{model} (no backend can serve it; check the combo entries or provider credentials)"
    ))
}

/// One attempt against a named backend: the request leaves without the
/// internal attribution token, which no upstream may ever see.
fn attempt(request: &ChatCompletionRequest, model: &str) -> ChatCompletionRequest {
    let mut attempt = request.clone();
    attempt.model = model.to_string();
    attempt.extra.remove(ATTRIBUTION_TOKEN);
    attempt
}

impl RoutingBackend {
    /// Build with default breaker settings (3 failures, 60s cooldown).
    pub fn new(backends: HashMap<String, Arc<dyn ChatBackend>>, router: Router) -> Self {
        Self::with_breaker_settings(backends, router, 3, 60)
    }

    /// Build with explicit breaker settings.
    pub fn with_breaker_settings(
        backends: HashMap<String, Arc<dyn ChatBackend>>,
        router: Router,
        threshold: u32,
        cooldown_secs: u64,
    ) -> Self {
        Self {
            backends: Arc::new(backends),
            router: Arc::new(router),
            breakers: Arc::new(Mutex::new(BreakerSet::new(threshold, cooldown_secs))),
            db: None,
            registry: None,
            attributions: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Attach a database + registry for combo-name expansion.
    pub fn with_combo_source(mut self, db: Arc<Db>, registry: Arc<ProviderRegistry>) -> Self {
        self.db = Some(db);
        self.registry = Some(registry);
        self
    }

    fn lock_breakers(&self) -> std::sync::MutexGuard<'_, BreakerSet> {
        self.breakers
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn lock_attributions(&self) -> std::sync::MutexGuard<'_, Vec<(String, Attribution)>> {
        self.attributions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Remember that `backend` handled the request stamped `token`.
    ///
    /// The provider id is the key this router dispatches under, which is the
    /// canonical provider id; the connection id comes from the executor that
    /// actually ran, and only from
    /// [`Self::dispatch_head_connection_id`] when the executor cannot name
    /// one. Failures are attributed too: "which provider is rate-limiting us"
    /// is the question this data exists to answer.
    fn attribute(
        &self,
        token: Option<&str>,
        provider: &str,
        backend: &dyn ChatBackend,
        combo_name: Option<&str>,
    ) {
        let Some(token) = token else {
            return;
        };
        let connection_id = backend
            .attribution(token)
            .connection_id
            .or_else(|| self.dispatch_head_connection_id(provider));
        let attribution = Attribution {
            provider: Some(provider.to_string()),
            connection_id,
            combo_name: combo_name.map(str::to_string),
        };
        let mut entries = self.lock_attributions();
        if entries.len() >= ATTRIBUTION_SLOTS {
            entries.remove(0);
        }
        entries.retain(|(key, _)| key != token);
        entries.push((token.to_string(), attribution));
    }

    /// The `provider_connections` row to name when the executor that served
    /// the request could not name one itself.
    ///
    /// It is the head of [`heterion_router_db::repos::active_connection_ids`]
    /// — `priority DESC, id ASC` — which is the exact order
    /// [`crate::credentials::load_credentials`] builds the dispatch pool in and
    /// the exact order [`crate::backend::next_token`] rotates over, so the id
    /// recorded is always a row that was in the pool and that dispatch tries
    /// before any other: the request's first credential attempt. For a
    /// single-row provider that is the row that really served, exactly; for a
    /// rotating multi-row pool it is the row a per-request rotation cannot be
    /// asked to name, and the head is the only honest deterministic answer.
    ///
    /// Why not leave it `None`, the way this used to for any provider with two
    /// or more accounts: `None` is what made the multi-account case useless,
    /// and a multi-account provider is precisely the case an operator is
    /// asking about — "which of my four Grok accounts just spent the quota?"
    /// In the dashboard a `NULL` `connection_id` leaves every per-account
    /// rollup empty and is indistinguishable from the gateway having lost
    /// track of the provider entirely, while naming the dispatch head keeps
    /// the column joinable, keeps the number consistent with what the gateway
    /// sent, and is right whenever the pool was not mid-rotation. A recorded
    /// row that is one rotation step off is a fact about the pool; a missing
    /// row is a fact about nothing.
    ///
    /// A provider with no active row at all — a keyless one such as
    /// `heterion-local`, or one whose credentials were all disabled — returns
    /// `None`, because there is no row to name.
    fn dispatch_head_connection_id(&self, provider: &str) -> Option<String> {
        let db = self.db.as_ref()?;
        let guard = db.connection();
        heterion_router_db::repos::active_connection_ids(&guard, provider)
            .ok()?
            .into_iter()
            .next()
    }

    /// Whether the error is transient and worth retrying.
    fn is_transient(error: &GatewayError) -> bool {
        match error {
            GatewayError::Upstream(msg) => {
                if let Some(status) = upstream_status_or_recovered(msg) {
                    status == 429 || status >= 500
                } else {
                    true
                }
            }
            _ => false,
        }
    }

    /// Retry config, candidate plan and combo name for a requested model.
    ///
    /// Resolution order: `provider/model` id → combo name → prefix rule. The
    /// combo store is consulted at most once per request, and only when the
    /// request is not already a `provider/model` id. A store that cannot be
    /// read is reported as such: clients must not be sent hunting for a model
    /// typo that does not exist.
    fn resolve(&self, model: &str) -> Result<Plan, GatewayError> {
        let prefix_plan = || Plan {
            config: ComboConfig::default(),
            steps: self.prefix_plan(model),
            combo_name: None,
        };

        // `provider/model` ids map straight to a provider backend, which is
        // what `/v1/models` advertises. The prefix may be an alias (`ds/…`),
        // so it is canonicalized before the backend lookup.
        if let Some((prefix, rest)) = model.split_once('/')
            && !rest.is_empty()
        {
            let provider = self
                .registry
                .as_ref()
                .and_then(|registry| registry.canonical_id(prefix))
                .unwrap_or(prefix);
            if self.backends.contains_key(provider) {
                return Ok(Plan {
                    config: ComboConfig::default(),
                    steps: vec![(provider.to_string(), rest.to_string())],
                    combo_name: None,
                });
            }
        }

        let (Some(db), Some(registry)) = (self.db.as_ref(), self.registry.as_ref()) else {
            return Ok(prefix_plan());
        };
        let guard = db.connection();
        let combos = load_combos(&guard).map_err(|error| {
            tracing::warn!("combo store read failed: {error}");
            GatewayError::Upstream("combo store unavailable".to_string())
        })?;
        let Some(combo) = combos.get(model) else {
            return Ok(prefix_plan());
        };
        let available: HashSet<String> = self.backends.keys().cloned().collect();
        let steps = plan_steps(combo, registry, &available);
        let config = combo.config.clone();
        // No servable entry: fall through to prefix routing so an
        // under-provisioned combo degrades like an unknown model.
        if steps.is_empty() {
            return Ok(Plan {
                config,
                steps: self.prefix_plan(model),
                combo_name: None,
            });
        }
        Ok(Plan {
            config,
            steps,
            combo_name: Some(model.to_string()),
        })
    }

    /// Candidates from the explicit route rules (no combos involved).
    fn prefix_plan(&self, model: &str) -> Vec<(String, String)> {
        self.router
            .route(model)
            .into_iter()
            .map(|backend| (backend, model.to_string()))
            .collect()
    }
}

#[async_trait]
impl ChatBackend for RoutingBackend {
    fn name(&self) -> &'static str {
        "routing"
    }

    fn attribution(&self, token: &str) -> Attribution {
        self.lock_attributions()
            .iter()
            .find(|(key, _)| key == token)
            .map(|(_, attribution)| attribution.clone())
            // Nothing was dispatched for this token — the request never
            // reached a provider (unknown model, every combo step failed), so
            // there is no provider to name.
            .unwrap_or_else(|| Attribution {
                provider: None,
                connection_id: None,
                combo_name: None,
            })
    }

    async fn complete(
        &self,
        request: ChatCompletionRequest,
    ) -> Result<ChatCompletionResponse, GatewayError> {
        let model = request.model.clone();
        let token = request_token(&request);
        let Plan {
            config,
            steps: plan,
            combo_name,
        } = self.resolve(&model)?;
        let mut last_error: Option<GatewayError> = None;
        let mut tripped = false;

        for (name, target_model) in &plan {
            let mut retry_count = 0;
            loop {
                if !self.lock_breakers().allow(name, Instant::now()) {
                    tripped = true;
                    break;
                }
                let Some(backend) = self.backends.get(name) else {
                    break;
                };
                match backend.complete(attempt(&request, target_model)).await {
                    Ok(response) => {
                        self.lock_breakers().record_success(name);
                        self.attribute(
                            token.as_deref(),
                            name,
                            backend.as_ref(),
                            combo_name.as_deref(),
                        );
                        return Ok(response);
                    }
                    Err(error) => {
                        if is_backend_outage(&error) {
                            self.lock_breakers().record_failure(name, Instant::now());
                        }
                        self.attribute(
                            token.as_deref(),
                            name,
                            backend.as_ref(),
                            combo_name.as_deref(),
                        );
                        let transient = Self::is_transient(&error);
                        last_error = Some(error);
                        if retry_count < config.max_retries && transient {
                            retry_count += 1;
                            if config.failover_before_retry {
                                break;
                            }
                            if config.retry_delay_ms > 0 {
                                tokio::time::sleep(std::time::Duration::from_millis(
                                    config.retry_delay_ms,
                                ))
                                .await;
                            }
                            continue;
                        }
                        break;
                    }
                }
            }
        }

        Err(last_error.unwrap_or_else(|| {
            if tripped {
                all_candidates_tripped(&model)
            } else {
                no_candidate(&model)
            }
        }))
    }

    fn stream(&self, request: ChatCompletionRequest) -> ChunkStream {
        let this = self.clone();
        let model = request.model.clone();
        let token = request_token(&request);
        let (tx, rx) = mpsc::channel(64);

        tokio::spawn(async move {
            let Plan {
                config,
                steps: plan,
                combo_name,
            } = match this.resolve(&model) {
                Ok(resolved) => resolved,
                // Say the combo store is unavailable instead of streaming a
                // fabricated "unknown model".
                Err(error) => {
                    let _ = tx.send(Err(error)).await;
                    return;
                }
            };
            let mut last_error: Option<GatewayError> = None;
            let mut tripped = false;

            for (name, target_model) in &plan {
                let mut retry_count = 0;
                loop {
                    if !this.lock_breakers().allow(name, Instant::now()) {
                        tripped = true;
                        break;
                    }
                    let Some(backend) = this.backends.get(name).cloned() else {
                        break;
                    };
                    let mut stream = backend.stream(attempt(&request, target_model));

                    match stream.next().await {
                        Some(Ok(first)) => {
                            this.lock_breakers().record_success(name);
                            this.attribute(
                                token.as_deref(),
                                name,
                                backend.as_ref(),
                                combo_name.as_deref(),
                            );
                            if tx.send(Ok(first)).await.is_err() {
                                return;
                            }
                            while let Some(item) = stream.next().await {
                                if tx.send(item).await.is_err() {
                                    return;
                                }
                            }
                            return;
                        }
                        Some(Err(error)) => {
                            if is_backend_outage(&error) {
                                this.lock_breakers().record_failure(name, Instant::now());
                            }
                            this.attribute(
                                token.as_deref(),
                                name,
                                backend.as_ref(),
                                combo_name.as_deref(),
                            );
                            let transient = Self::is_transient(&error);
                            last_error = Some(error);
                            if retry_count < config.max_retries && transient {
                                retry_count += 1;
                                if config.failover_before_retry {
                                    break;
                                }
                                if config.retry_delay_ms > 0 {
                                    tokio::time::sleep(std::time::Duration::from_millis(
                                        config.retry_delay_ms,
                                    ))
                                    .await;
                                }
                                continue;
                            }
                            break;
                        }
                        None => {
                            this.lock_breakers().record_success(name);
                            this.attribute(
                                token.as_deref(),
                                name,
                                backend.as_ref(),
                                combo_name.as_deref(),
                            );
                            return;
                        }
                    }
                }
            }

            let error = last_error.unwrap_or_else(|| {
                if tripped {
                    all_candidates_tripped(&model)
                } else {
                    no_candidate(&model)
                }
            });
            let _ = tx.send(Err(error)).await;
        });

        Box::pin(ReceiverStream::new(rx))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::EchoBackend;
    use futures::StreamExt;
    use heterion_router_core::ChatMessage;
    use heterion_router_providers::ProviderRegistry;
    use heterion_router_routing::RouteRule;
    use serde_json::json;

    #[derive(Debug, Clone)]
    struct FailBackend;
    #[async_trait]
    impl ChatBackend for FailBackend {
        fn name(&self) -> &'static str {
            "fail"
        }
        async fn complete(
            &self,
            _request: ChatCompletionRequest,
        ) -> Result<ChatCompletionResponse, GatewayError> {
            Err(GatewayError::Upstream("boom".to_string()))
        }
        fn stream(&self, _request: ChatCompletionRequest) -> ChunkStream {
            Box::pin(futures::stream::once(async {
                Err(GatewayError::Upstream("boom".to_string()))
            }))
        }
    }

    /// A backend that always answers with one upstream status.
    #[derive(Debug, Clone)]
    struct StatusBackend {
        status: u16,
        name: &'static str,
    }

    #[async_trait]
    impl ChatBackend for StatusBackend {
        fn name(&self) -> &'static str {
            self.name
        }
        async fn complete(
            &self,
            _request: ChatCompletionRequest,
        ) -> Result<ChatCompletionResponse, GatewayError> {
            Err(GatewayError::Upstream(format!(
                "upstream status {}: nope",
                self.status
            )))
        }
        fn stream(&self, _request: ChatCompletionRequest) -> ChunkStream {
            let error = GatewayError::Upstream(format!("upstream status {}: nope", self.status));
            Box::pin(futures::stream::once(async move { Err(error) }))
        }
    }

    fn request(model: &str) -> ChatCompletionRequest {
        ChatCompletionRequest {
            model: model.to_string(),
            messages: vec![ChatMessage::plain("user".to_string(), "hi".to_string())],
            stream: false,
            max_tokens: None,
            temperature: None,
            top_p: None,
            tools: None,
            tool_choice: None,
            ..Default::default()
        }
    }

    fn routing() -> RoutingBackend {
        let mut backends: HashMap<String, std::sync::Arc<dyn ChatBackend>> = HashMap::new();
        backends.insert("fail".to_string(), std::sync::Arc::new(FailBackend));
        backends.insert("echo".to_string(), std::sync::Arc::new(EchoBackend));
        let router = Router::new(
            vec![RouteRule {
                model_prefix: "x-".to_string(),
                backends: vec!["fail".to_string(), "echo".to_string()],
            }],
            vec!["echo".to_string()],
        );
        RoutingBackend::with_breaker_settings(backends, router, 1, 3600)
    }

    #[test]
    fn only_provider_outages_are_breaker_failures() {
        // 4xx: the provider answered, it just refused this one request.
        assert!(!is_backend_outage(&GatewayError::Upstream(
            "upstream status 404: {\"error\":\"model not found\"}".to_string()
        )));
        assert!(!is_backend_outage(&GatewayError::Upstream(
            "upstream status 410: gone".to_string()
        )));
        assert!(!is_backend_outage(&GatewayError::Upstream(
            "upstream status 400: bad payload".to_string()
        )));
        assert!(!is_backend_outage(&GatewayError::Upstream(
            "upstream status 429: slow down".to_string()
        )));
        // 5xx and transport failures: the backend is unhealthy.
        assert!(is_backend_outage(&GatewayError::Upstream(
            "upstream status 503: overloaded".to_string()
        )));
        assert!(is_backend_outage(&GatewayError::Upstream(
            "upstream status 500: boom".to_string()
        )));
        assert!(is_backend_outage(&GatewayError::Upstream(
            "grok-cli: all credentials failed".to_string()
        )));
        // Routing outcomes are never backend faults.
        assert!(!is_backend_outage(&GatewayError::UnknownModel(
            "nope".to_string()
        )));
        assert!(!is_backend_outage(&GatewayError::InvalidRequest(
            "messages is empty".to_string()
        )));
    }

    #[test]
    fn upstream_status_is_parsed_from_the_client_message() {
        assert_eq!(
            upstream_status_or_recovered("upstream status 404: nope"),
            Some(404)
        );
        assert_eq!(
            upstream_status_or_recovered("upstream status 503: busy"),
            Some(503)
        );
        assert_eq!(
            upstream_status_or_recovered("upstream status : weird"),
            None
        );
        assert_eq!(upstream_status_or_recovered("transport error"), None);
    }

    #[tokio::test]
    async fn dead_model_does_not_trip_the_whole_backend() {
        // `fail` answers like a provider that does not serve the model; a
        // second model on the same backend must still be served.
        let mut backends: HashMap<String, std::sync::Arc<dyn ChatBackend>> = HashMap::new();
        backends.insert(
            "gone".to_string(),
            std::sync::Arc::new(StatusBackend {
                status: 404,
                name: "gone",
            }),
        );
        backends.insert("echo".to_string(), std::sync::Arc::new(EchoBackend));
        let router = Router::new(
            vec![RouteRule {
                model_prefix: "x-".to_string(),
                backends: vec!["gone".to_string(), "echo".to_string()],
            }],
            vec!["echo".to_string()],
        );
        // Threshold 1: a single counted failure would trip the backend.
        let routing = RoutingBackend::with_breaker_settings(backends, router, 1, 3600);

        for _ in 0..3 {
            let response = routing.complete(request("x-model")).await.unwrap();
            assert_eq!(response.choices[0].message.text(), "echo: hi");
        }
    }

    #[tokio::test]
    async fn tripped_candidates_report_cooldown_not_an_unknown_model() {
        let mut backends: HashMap<String, std::sync::Arc<dyn ChatBackend>> = HashMap::new();
        backends.insert(
            "down".to_string(),
            std::sync::Arc::new(StatusBackend {
                status: 503,
                name: "down",
            }),
        );
        let router = Router::new(
            vec![RouteRule {
                model_prefix: "x-".to_string(),
                backends: vec!["down".to_string()],
            }],
            vec![],
        );
        let routing = RoutingBackend::with_breaker_settings(backends, router, 1, 3600);

        // First call trips the breaker on the 503.
        let first = routing.complete(request("x-model")).await.unwrap_err();
        assert!(matches!(first, GatewayError::Upstream(_)), "{first:?}");

        // The next call has no allowed candidate: say so instead of pretending
        // the model is unknown.
        let second = routing.complete(request("x-model")).await.unwrap_err();
        let message = second.to_string();
        assert!(message.contains("breaker cooldown"), "{message}");
    }

    #[tokio::test]
    async fn falls_back_on_primary_failure() {
        let routing = routing();
        let response = routing.complete(request("x-model")).await.unwrap();
        assert_eq!(response.choices[0].message.text(), "echo: hi");
    }

    #[tokio::test]
    async fn breaker_skips_tripped_primary() {
        let routing = routing();
        // First call trips "fail" (threshold 1) then serves via echo.
        routing.complete(request("x-model")).await.unwrap();
        // Second call must not touch "fail" at all (breaker open, long cooldown).
        let response = routing.complete(request("x-model")).await.unwrap();
        assert_eq!(response.choices[0].message.text(), "echo: hi");
    }

    #[tokio::test]
    async fn unknown_model_without_route_errors() {
        let mut backends: HashMap<String, std::sync::Arc<dyn ChatBackend>> = HashMap::new();
        backends.insert("echo".to_string(), std::sync::Arc::new(EchoBackend));
        let routing = RoutingBackend::new(backends, Router::new(vec![], vec![]));
        let error = routing.complete(request("x-model")).await.unwrap_err();
        assert!(matches!(error, GatewayError::UnknownModel(_)));
    }

    #[tokio::test]
    async fn stream_uses_first_allowed_backend() {
        let routing = routing();
        let chunks: Vec<_> = routing
            .stream(request("other"))
            .collect::<Vec<_>>()
            .await
            .into_iter()
            .map(Result::unwrap)
            .collect();
        let text: String = chunks.iter().filter_map(|c| c.content.clone()).collect();
        assert_eq!(text, "echo: hi");
    }

    #[tokio::test]
    async fn provider_model_id_routes_to_provider_backend() {
        let mut backends: HashMap<String, Arc<dyn ChatBackend>> = HashMap::new();
        backends.insert("openrouter".to_string(), Arc::new(EchoBackend));
        let routing = RoutingBackend::new(backends, Router::new(vec![], vec![]));

        // `provider/model` ids are what /v1/models advertises; they must route
        // to the provider backend with the prefix stripped.
        let response = routing
            .complete(request("openrouter/anthropic/claude-3-haiku"))
            .await
            .unwrap();
        assert_eq!(response.model, "anthropic/claude-3-haiku");
    }

    #[tokio::test]
    async fn unservable_model_errors_instead_of_fabricating() {
        let mut backends: HashMap<String, Arc<dyn ChatBackend>> = HashMap::new();
        backends.insert("openrouter".to_string(), Arc::new(EchoBackend));
        let routing = RoutingBackend::new(backends, Router::new(vec![], vec![]));
        let error = routing
            .complete(request("gemini/gemini-3.8-flash"))
            .await
            .unwrap_err();
        assert!(matches!(error, GatewayError::UnknownModel(_)));
        assert!(error.to_string().contains("no backend can serve"));
    }

    #[tokio::test]
    async fn stream_fails_over_when_first_backend_errors() {
        let routing = routing();
        // The `x-` rule tries "fail" first; streaming must now hop to echo
        // instead of surfacing the error, which is what free tiers need.
        let chunks: Vec<_> = routing.stream(request("x-model")).collect::<Vec<_>>().await;
        let text: String = chunks
            .iter()
            .map(|item| item.as_ref().expect("no error after failover"))
            .filter_map(|chunk| chunk.content.clone())
            .collect();
        assert_eq!(text, "echo: hi");
    }

    #[tokio::test]
    async fn alias_prefixed_model_reaches_the_canonical_backend() {
        use heterion_router_db::Db;
        use heterion_router_providers::ProviderRegistry;

        let db = std::sync::Arc::new(Db::open_in_memory().unwrap());
        db.migrate().unwrap();

        let mut backends: HashMap<String, std::sync::Arc<dyn ChatBackend>> = HashMap::new();
        // `kg` is the alias of `kilo-gateway`; the backend is keyed by id.
        backends.insert("kilo-gateway".to_string(), std::sync::Arc::new(EchoBackend));
        backends.insert("echo".to_string(), std::sync::Arc::new(EchoBackend));
        let routing = RoutingBackend::new(backends, Router::new(vec![], vec![]))
            .with_combo_source(db, std::sync::Arc::new(ProviderRegistry::load()));

        let response = routing
            .complete(request("kg/kilo-auto/balanced"))
            .await
            .unwrap();
        assert_eq!(response.model, "kilo-auto/balanced");
        assert_eq!(response.choices[0].message.text(), "echo: hi");
    }

    /// A backend that names its own provider and connection, the way the
    /// registry-driven executors do.
    #[derive(Clone)]
    struct AttributedBackend {
        provider: &'static str,
        connection: &'static str,
    }

    #[async_trait]
    impl ChatBackend for AttributedBackend {
        fn name(&self) -> &'static str {
            "provider"
        }

        fn attribution(&self, _token: &str) -> Attribution {
            Attribution {
                provider: Some(self.provider.to_string()),
                connection_id: Some(self.connection.to_string()),
                combo_name: None,
            }
        }

        async fn complete(
            &self,
            request: ChatCompletionRequest,
        ) -> Result<ChatCompletionResponse, GatewayError> {
            Ok(ChatCompletionResponse {
                id: "chatcmpl-attributed".to_string(),
                object: "chat.completion",
                created: 0,
                model: request.model,
                choices: vec![heterion_router_core::ChatChoice {
                    index: 0,
                    message: ChatMessage::plain("assistant".to_string(), "ok".to_string()),
                    finish_reason: "stop".to_string(),
                }],
                usage: heterion_router_core::Usage::default(),
                raw: None,
            })
        }

        fn stream(&self, _request: ChatCompletionRequest) -> ChunkStream {
            Box::pin(futures::stream::once(async {
                Ok(heterion_router_core::StreamChunk::done("stop"))
            }))
        }
    }

    #[tokio::test]
    async fn attribution_is_keyed_per_request_not_per_backend() {
        // Two providers, two requests in flight: a shared "last served" slot
        // would hand one request the other's provider.
        let mut backends: HashMap<String, Arc<dyn ChatBackend>> = HashMap::new();
        for (provider, connection) in [("openrouter", "conn-a"), ("pollinations", "conn-b")] {
            backends.insert(
                provider.to_string(),
                Arc::new(AttributedBackend {
                    provider,
                    connection,
                }),
            );
        }
        let routing = RoutingBackend::new(backends, Router::new(vec![], vec![]));

        let mut first = request("openrouter/model");
        first
            .extra
            .insert(ATTRIBUTION_TOKEN.to_string(), json!("t1"));
        let mut second = request("pollinations/model");
        second
            .extra
            .insert(ATTRIBUTION_TOKEN.to_string(), json!("t2"));

        let (a, b) = tokio::join!(routing.complete(first), routing.complete(second));
        a.unwrap();
        b.unwrap();

        let one = routing.attribution("t1");
        assert_eq!(one.provider.as_deref(), Some("openrouter"));
        assert_eq!(one.connection_id.as_deref(), Some("conn-a"));
        let two = routing.attribution("t2");
        assert_eq!(two.provider.as_deref(), Some("pollinations"));
        assert_eq!(two.connection_id.as_deref(), Some("conn-b"));
        // An unknown token names no provider at all — never a stale one, and
        // never the router, which is not a provider and would otherwise land
        // in `usage_history.provider` as if it were.
        assert_eq!(routing.attribution("t3").provider, None);
    }

    /// An active `provider_connections` row for `provider`. No secrets: the
    /// routing fallback only ever reads ids.
    fn connection(db: &heterion_router_db::Db, id: &str, provider: &str, priority: i64) {
        use heterion_router_db::repos;
        let guard = db.connection();
        repos::upsert_connection(
            &guard,
            &repos::ConnectionRow {
                id: id.to_string(),
                provider: provider.to_string(),
                auth_type: Some("apikey".to_string()),
                name: None,
                email: None,
                priority,
                is_active: true,
                access_token: None,
                refresh_token: None,
                expires_at: None,
                api_key: Some(format!("secret-for-{id}")),
                provider_specific_data: None,
            },
        )
        .unwrap();
    }

    /// An in-memory database plus a routing backend whose per-provider
    /// executors name nothing, which is how the real pool-less executors
    /// behave — the DB fallback is the only source of a connection id.
    fn routing_with_db(db: &Arc<heterion_router_db::Db>, providers: &[&str]) -> RoutingBackend {
        let mut backends: HashMap<String, Arc<dyn ChatBackend>> = HashMap::new();
        for provider in providers {
            backends.insert((*provider).to_string(), Arc::new(AnonymousBackend));
        }
        RoutingBackend::new(backends, Router::new(vec![], vec![]))
            .with_combo_source(Arc::clone(db), Arc::new(ProviderRegistry::load()))
    }

    fn memory_db() -> Arc<heterion_router_db::Db> {
        let db = Arc::new(heterion_router_db::Db::open_in_memory().unwrap());
        db.migrate().unwrap();
        db
    }

    fn stamped(model: &str, token: &str) -> ChatCompletionRequest {
        let mut request = request(model);
        request
            .extra
            .insert(ATTRIBUTION_TOKEN.to_string(), json!(token));
        request
    }

    /// An executor that uses the default attribution: it names its provider
    /// and no connection, so the routing fallback decides.
    #[derive(Debug, Clone)]
    struct AnonymousBackend;

    #[async_trait]
    impl ChatBackend for AnonymousBackend {
        fn name(&self) -> &'static str {
            "provider"
        }

        async fn complete(
            &self,
            request: ChatCompletionRequest,
        ) -> Result<ChatCompletionResponse, GatewayError> {
            Ok(ChatCompletionResponse {
                id: "chatcmpl-anonymous".to_string(),
                object: "chat.completion",
                created: 0,
                model: request.model,
                choices: vec![heterion_router_core::ChatChoice {
                    index: 0,
                    message: ChatMessage::plain("assistant".to_string(), "ok".to_string()),
                    finish_reason: "stop".to_string(),
                }],
                usage: heterion_router_core::Usage::default(),
                raw: None,
            })
        }

        fn stream(&self, _request: ChatCompletionRequest) -> ChunkStream {
            Box::pin(futures::stream::once(async {
                Ok(heterion_router_core::StreamChunk::done("stop"))
            }))
        }
    }

    /// One active row: the id is that row, exactly.
    #[tokio::test]
    async fn sole_active_connection_is_the_attributed_row() {
        let db = memory_db();
        connection(&db, "conn-solo", "openrouter", 5);
        let routing = routing_with_db(&db, &["openrouter"]);

        routing
            .complete(stamped("openrouter/model", "t1"))
            .await
            .unwrap();

        let attributed = routing.attribution("t1");
        assert_eq!(attributed.provider.as_deref(), Some("openrouter"));
        assert_eq!(attributed.connection_id.as_deref(), Some("conn-solo"));
    }

    /// Several active rows: the recorded id is the head of dispatch order
    /// (`priority DESC, id ASC`) — the row the credential pool puts at index
    /// zero — so the dashboard names the account this request would be
    /// charged to, not a blank and not an arbitrary row.
    #[tokio::test]
    async fn multi_connection_provider_names_the_dispatch_head() {
        let db = memory_db();
        connection(&db, "conn-low", "grok-cli", 1);
        connection(&db, "conn-top", "grok-cli", 90);
        connection(&db, "conn-mid", "grok-cli", 50);
        let routing = routing_with_db(&db, &["grok-cli"]);

        routing
            .complete(stamped("grok-cli/grok-4.6", "t1"))
            .await
            .unwrap();

        assert_eq!(
            routing.attribution("t1").connection_id.as_deref(),
            Some("conn-top"),
            "highest priority wins, and the id is a row the request could use"
        );
    }

    /// A priority tie is broken by id, so the head is stable across calls.
    #[tokio::test]
    async fn tied_priorities_break_the_tie_by_id() {
        let db = memory_db();
        connection(&db, "conn-b", "grok-cli", 7);
        connection(&db, "conn-a", "grok-cli", 7);
        let routing = routing_with_db(&db, &["grok-cli"]);

        for token in ["t1", "t2", "t3"] {
            routing
                .complete(stamped("grok-cli/grok-4.6", token))
                .await
                .unwrap();
            assert_eq!(
                routing.attribution(token).connection_id.as_deref(),
                Some("conn-a"),
                "id ASC is the second key, so the head does not drift"
            );
        }
    }

    /// An inactive row is not in the pool, so it can never be named; no
    /// active row means no row at all rather than a dangling id.
    #[tokio::test]
    async fn a_provider_without_active_connections_names_no_row() {
        use heterion_router_db::repos;

        let db = memory_db();
        connection(&db, "conn-off", "pollinations", 100);
        {
            let guard = db.connection();
            let mut row = repos::get_connection(&guard, "conn-off").unwrap().unwrap();
            row.is_active = false;
            repos::upsert_connection(&guard, &row).unwrap();
        }
        let routing = routing_with_db(&db, &["pollinations"]);

        routing
            .complete(stamped("pollinations/model", "t1"))
            .await
            .unwrap();

        let attributed = routing.attribution("t1");
        assert_eq!(attributed.provider.as_deref(), Some("pollinations"));
        assert_eq!(attributed.connection_id, None);
    }

    /// An alias-keyed id (`grok-cli::acct-9f2a`) belongs to its own provider
    /// and must never be handed to a provider whose name merely starts with
    /// the same characters, in either direction.
    #[tokio::test]
    async fn alias_keyed_connections_stay_scoped_to_their_provider() {
        let db = memory_db();
        connection(&db, "grok-cli::acct-9f2a", "grok-cli", 3);
        connection(&db, "grok-cli-acct-9f2a", "grok", 3);
        let routing = routing_with_db(&db, &["grok-cli", "grok"]);

        routing
            .complete(stamped("grok-cli/grok-4.6", "t1"))
            .await
            .unwrap();
        routing
            .complete(stamped("grok/grok-4.6", "t2"))
            .await
            .unwrap();

        assert_eq!(
            routing.attribution("t1").connection_id.as_deref(),
            Some("grok-cli::acct-9f2a"),
            "the alias id comes back verbatim for its own provider"
        );
        assert_eq!(
            routing.attribution("t2").connection_id.as_deref(),
            Some("grok-cli-acct-9f2a"),
            "and the other provider never sees the `::` row"
        );
    }

    /// The row an executor reports is the one that really served; the
    /// database is only consulted when the executor declines to say.
    #[tokio::test]
    async fn a_reported_connection_beats_the_database_head() {
        let db = memory_db();
        connection(&db, "conn-top", "openrouter", 90);
        connection(&db, "conn-served", "openrouter", 1);
        let mut backends: HashMap<String, Arc<dyn ChatBackend>> = HashMap::new();
        backends.insert(
            "openrouter".to_string(),
            Arc::new(AttributedBackend {
                provider: "openrouter",
                connection: "conn-served",
            }),
        );
        let routing = RoutingBackend::new(backends, Router::new(vec![], vec![]))
            .with_combo_source(Arc::clone(&db), Arc::new(ProviderRegistry::load()));

        routing
            .complete(stamped("openrouter/model", "t1"))
            .await
            .unwrap();

        assert_eq!(
            routing.attribution("t1").connection_id.as_deref(),
            Some("conn-served"),
            "the lower-priority row that really served wins over the head"
        );
    }

    /// Regression guard for the fallback path itself: two providers, two
    /// rows each, two requests in flight. A shared slot would hand one
    /// request the other's provider *and* the other's account.
    #[tokio::test]
    async fn concurrent_requests_keep_their_own_connection() {
        let db = memory_db();
        connection(&db, "conn-a1", "openrouter", 90);
        connection(&db, "conn-a2", "openrouter", 1);
        connection(&db, "conn-b1", "pollinations", 90);
        connection(&db, "conn-b2", "pollinations", 1);
        let routing = routing_with_db(&db, &["openrouter", "pollinations"]);

        let (a, b) = tokio::join!(
            routing.complete(stamped("openrouter/model", "t1")),
            routing.complete(stamped("pollinations/model", "t2"))
        );
        a.unwrap();
        b.unwrap();

        let one = routing.attribution("t1");
        assert_eq!(one.provider.as_deref(), Some("openrouter"));
        assert_eq!(one.connection_id.as_deref(), Some("conn-a1"));
        let two = routing.attribution("t2");
        assert_eq!(two.provider.as_deref(), Some("pollinations"));
        assert_eq!(two.connection_id.as_deref(), Some("conn-b1"));
    }

    /// A keyless provider has no `provider_connections` row and no credential
    /// at all, so it serves normally and records no row.
    #[tokio::test]
    async fn keyless_provider_serves_and_names_no_connection() {
        let db = memory_db();
        let routing = routing_with_db(&db, &["heterion-local"]);

        let response = routing
            .complete(stamped("heterion-local/grok-4.6", "t1"))
            .await
            .unwrap();
        assert_eq!(response.choices[0].message.text(), "ok");

        let attributed = routing.attribution("t1");
        assert_eq!(attributed.provider.as_deref(), Some("heterion-local"));
        assert_eq!(attributed.connection_id, None);
    }

    #[tokio::test]
    async fn combo_name_expands_and_rewrites_model() {
        use heterion_router_db::{Db, repos};
        use heterion_router_providers::ProviderRegistry;

        let db = std::sync::Arc::new(Db::open_in_memory().unwrap());
        db.migrate().unwrap();
        {
            let guard = db.connection();
            repos::upsert_combo(
                &guard,
                 &repos::ComboRow {
                     id: "c1".to_string(),
                     name: "test-combo".to_string(),
                     data: serde_json::json!({
                         "strategy": "priority",
                         "models": [
                             {"kind": "model", "model": "grok-cli/grok-4.6", "providerId": "grok-cli"},
                         ],
                     })
                     .to_string(),
                     sort_order: 0,
                     active: true,
                 },
            )
            .unwrap();
        }

        let mut backends: HashMap<String, std::sync::Arc<dyn ChatBackend>> = HashMap::new();
        // Named like the real backend so the plan selects it; echo lets us
        // observe the rewritten model id in the response.
        backends.insert("grok-cli".to_string(), std::sync::Arc::new(EchoBackend));
        backends.insert("echo".to_string(), std::sync::Arc::new(EchoBackend));
        let routing = RoutingBackend::new(backends, Router::new(vec![], vec![]))
            .with_combo_source(db, std::sync::Arc::new(ProviderRegistry::load()));

        let response = routing.complete(request("test-combo")).await.unwrap();
        assert_eq!(response.model, "grok-4.6");
        assert_eq!(response.choices[0].message.text(), "echo: hi");
    }
}

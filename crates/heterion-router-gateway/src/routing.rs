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
use std::time::{Duration, Instant};

use async_trait::async_trait;
use dashmap::DashMap;
use futures::StreamExt;
use heterion_router_core::{ChatCompletionRequest, ChatCompletionResponse, GatewayError};
use heterion_router_db::Db;
use heterion_router_providers::ProviderRegistry;
use heterion_router_routing::{BreakerSet, Router};
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;

use crate::backend::{
    ATTRIBUTION_TOKEN, Attribution, ChatBackend, ChunkStream, FailedAttempt, request_token,
};
use crate::capabilities::{CapabilityStore, ModelCapabilities, ladder_priority};
use crate::combos::{ComboConfig, WeightedStep, load_combos, plan_weighted_steps};
use crate::model_spec::{ModelSpec, ModelTier, ToolSupport};
use crate::resilience::{ModelQualityTracker, ProviderCooldownSet};
use crate::spread;
use crate::trust::TrustStore;
use crate::upstream_status_or_recovered;

/// How many finished requests keep an attribution for. Bounded so a request
/// that is never booked (a `no_log` key) cannot grow the ring without limit.
const ATTRIBUTION_SLOTS: usize = 256;

/// The model ids the Switchyard router serves itself: the canonical
/// `heterion-router/auto` and its `auto` alias.
///
/// Both are virtual: the router picks a real candidate per request, so the
/// model that answered is reported in the `x-router-model-used` header and in
/// `call_logs`, while the body carries the name the client asked for.
pub fn is_auto_model(model: &str) -> bool {
    model == "heterion-router/auto" || model == "auto"
}

/// Everything one request needs before the first attempt.
#[derive(Debug, Clone)]
pub struct Plan {
    /// Retry/failover settings from the combo row, defaults otherwise.
    pub config: ComboConfig,
    /// Ordered `(backend, model)` attempts; empty means "nothing can serve it".
    pub steps: Vec<(String, String)>,
    /// Combo the request is served through, when it is served through one.
    pub combo_name: Option<String>,
}

/// Candidate specification for Switchyard dynamic auto-routing.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AutoCandidate {
    /// Provider identifier matching backend map keys (e.g. "gemini", "agentrouter", "mistral").
    pub provider: &'static str,
    /// Model identifier passed to upstream provider (e.g. "gemini-3-flash-preview").
    pub model: &'static str,
    /// Base priority score according to capability tier:
    /// - Tier 1: 300, 290, 280
    /// - Tier 2: 200, 190
    /// - Tier 3: 100, 90
    pub base_priority: f64,
}

/// One candidate of the `auto` plan, with the numbers that decided it.
///
/// The plan says which models are tried and in which order; this says *why*.
/// An operator who asks "why does this model never answer" gets the base
/// priority, the quality score, the availability state, the soft cooldown
/// factor and — when the candidate was dropped — the reason.
#[derive(Debug, Clone, serde::Serialize)]
pub struct PlanCandidate {
    /// Provider id, as the backend map knows it.
    pub provider: String,
    /// Model id, as the provider knows it.
    pub model: String,
    /// The intrinsic priority of the model, after the stage weight.
    pub base_priority: f64,
    /// The quality score the tracker holds for the model.
    pub quality_score: f64,
    /// 1.0 when the model can serve right now, 0.0 in cooldown or breaker.
    pub availability: f64,
    /// The rotation factor: a model that just served ranks lower for a while.
    pub soft_cooldown: f64,
    /// The resulting effective priority — what the plan sorts by.
    pub effective: f64,
    /// Why the candidate is not in the plan; empty when it is.
    ///
    /// `no_backend`, `no_vision`, `no_tools`, `pseudo_tools`, `context`,
    /// `cooldown`, `breaker`, `zero_score`.
    pub excluded: String,
}

/// Canonical dynamic priority ladder table according to NVIDIA Switchyard specifications.
pub const DEFAULT_AUTO_CANDIDATES: &[AutoCandidate] = &[
    // Tier 1: Flagships / Deep Reasoning & High Context
    AutoCandidate {
        provider: "gemini",
        model: "gemini-3-flash-preview",
        base_priority: 300.0,
    },
    AutoCandidate {
        provider: "agentrouter",
        model: "gpt-5.6-sol",
        base_priority: 290.0,
    },
    AutoCandidate {
        provider: "agentrouter",
        model: "gpt-6.1-sol",
        base_priority: 280.0,
    },
    // Tier 2: Workhorses / Specialized Coding & 262k
    AutoCandidate {
        provider: "mistral",
        model: "codestral-latest",
        base_priority: 200.0,
    },
    AutoCandidate {
        provider: "gemini",
        model: "gemma-4-26b-a4b-it",
        base_priority: 190.0,
    },
    // Tier 3: Fast Execution / High-Quota Protectors
    AutoCandidate {
        provider: "gemini",
        model: "gemini-3.5-flash-lite",
        base_priority: 100.0,
    },
    AutoCandidate {
        provider: "gemini",
        model: "gemini-3.1-flash-lite",
        base_priority: 90.0,
    },
];

/// The combo name a Switchyard plan books its attempts under.
pub const AUTO_COMBO_NAME: &str = "heterion-router/auto";

/// Base priority of the unified pool's long tail: every live provider model
/// that is not part of the curated ladder.
///
/// The tail sits below Tier 3 (90) on purpose: the curated ladder keeps its
/// contract, and the tail is what makes the pool *unified* — a model the
/// operator connected but nobody curated is still reachable through `auto`,
/// ranked last and demoted by the quality tracker the moment it misbehaves.
pub const AUTO_TAIL_BASE_PRIORITY: f64 = 50.0;

/// How long a model keeps a reduced priority after it served a request.
///
/// The soft cooldown is the rotation: the best model answers, cools down for
/// a moment, and the next one in the ladder takes the next request. Nothing
/// about the model changes — its class, context and capabilities stay — it is
/// only temporarily pushed down, so it comes back exactly where it was.
pub const AUTO_SOFT_COOLDOWN_SECS: u64 = 45;

/// How long a model that failed keeps its demotion.
///
/// Twice the rotation window: a model whose id the provider does not serve
/// (or whose answers are broken) stays down long enough that the next
/// requests do not start with it — but it comes back, with all its
/// properties, when the window is over.
pub const AUTO_FAILURE_COOLDOWN_SECS: u64 = 120;

/// How much of its priority a failed model keeps on its first failure.
///
/// A tenth: enough to put the model below everything else in the pool — the
/// plain tail sits at 50, so a failed candidate of 300 points lands at 30.
/// The model is not thrown out of the pool: it comes back when the window is
/// over, with all its properties.
pub const AUTO_FAILURE_FACTOR: f64 = 0.1;

/// How many upstream calls one `auto` request may make.
///
/// The pool is the whole live catalogue; the budget is what keeps a request
/// from walking every dead model in it one by one. A model that fails decays
/// in the quality tracker, so the next request starts further down the tail —
/// the pool rotates instead of hammering the same head.
pub const AUTO_MAX_ATTEMPTS: usize = 24;

/// How close a candidate must stand to the head of the ladder to share the
/// traffic with it.
///
/// The peers of the head are the models of its class that are almost as good
/// as it. A candidate far below the head is a fallback, not a peer: it must
/// not take the head away from a better model just because they belong to
/// the same class.
pub const AUTO_SPREAD_PEER_RATIO: f64 = 0.8;

/// When a model served, and how often inside the cooldown window.
#[derive(Debug, Clone, Copy)]
struct ServeRecord {
    /// When it served last.
    last: Instant,
    /// How many times it served inside the current window.
    count: u32,
    /// How many times it failed inside the failure window.
    fails: u32,
    /// When it failed last.
    last_fail: Instant,
}

/// One entry of the unified Switchyard pool.
#[derive(Debug, Clone, PartialEq)]
struct PoolCandidate {
    /// Backend name (provider id) the candidate dispatches to.
    provider: String,
    /// Bare model id passed upstream.
    model: String,
    /// Priority before quality and availability are applied.
    base_priority: f64,
    /// Served by the web executor: a browser session that answers with page
    /// text and has no tool head at all.
    web_executor: bool,
}

/// One scored candidate of the `auto` plan: the candidate plus the numbers
/// that decided its place.
///
/// The plan says which models are tried and in which order; this carries the
/// numbers the spread needs — what the candidate is worth, what it is worth
/// intrinsically, which class it belongs to and whether it just served.
#[derive(Debug, Clone)]
struct Scored<T> {
    /// The candidate itself: a step for the plan, a row for the debug view.
    item: T,
    /// What the candidate is worth right now.
    effective: f64,
    /// The intrinsic priority of the candidate.
    base_priority: f64,
    /// The class of the candidate.
    tier: ModelTier,
    /// Whether the candidate served recently.
    ///
    /// The rotation demotes such a candidate, and the traffic must not go
    /// straight back to it.
    cooling: bool,
}

/// Backend router with failover.
#[derive(Clone)]
pub struct RoutingBackend {
    backends: Arc<HashMap<String, Arc<dyn ChatBackend>>>,
    router: Arc<Router>,
    breakers: Arc<Mutex<BreakerSet>>,
    db: Option<Arc<Db>>,
    registry: Option<Arc<ProviderRegistry>>,
    /// What each model can do — the ladder's intrinsic input.
    capabilities: Option<Arc<CapabilityStore>>,
    /// Which models the router has actually talked to.
    ///
    /// A model the platform merely *lists* may answer `404`; the trust store
    /// keeps such an unverified model from pushing a model we know works out
    /// of the head of the ladder.
    trust: Option<Arc<TrustStore>>,
    /// Whether the rotation layer is on: a model that just served drops down
    /// softly, so the next request goes to the next rung of the ladder.
    ///
    /// The layer is orthogonal to the ladder itself: the ladder says which
    /// model is *best suited*, the rotation says who answers *now*. Contracts
    /// that pin the ladder order switch the rotation off explicitly; the
    /// running gateway always has it on.
    rotation: bool,
    /// Whether the vision relay is on: a request with images that lands on a
    /// model which cannot see them is served through a borrowed description.
    vision_relay: bool,
    /// Whether the relay may also *save* tokens: a model that can see the
    /// picture hands it to a cheaper vision model first and receives the
    /// description instead of the image.
    ///
    /// Off by default — the target then loses the picture's detail, which is
    /// a trade the operator has to opt into.
    vision_saving: bool,
    /// When each model served and how often (`provider/model` → record).
    served_at: Arc<DashMap<String, ServeRecord>>,
    /// Who served which request: `request token -> Attribution`.
    ///
    /// Keyed rather than a single "last served" slot, because concurrent
    /// requests would otherwise read each other's provider.
    attributions: Arc<Mutex<Vec<(String, Attribution)>>>,
    /// The upstream calls that failed inside a failover plan:
    /// `request token -> the attempts, in the order they failed`.
    ///
    /// The journal writes one row per failed call, so "which provider refused
    /// us and with what" is readable from `call_logs` alone. The request that
    /// eventually succeeded keeps its own row; a request that failed entirely
    /// is described by these rows instead of one summary row.
    ///
    /// Bounded like the attributions: a request that is never booked (a
    /// `no_log` key) must not grow the ring without limit.
    failed_attempts: Arc<Mutex<FailedAttempts>>,
    cooldowns: Arc<ProviderCooldownSet>,
    quality_tracker: Arc<ModelQualityTracker>,
    /// The model catalogue each platform published, as measured.
    ///
    /// The registry is the curated list; this is what the platforms said
    /// today (`provider_catalog` in the database). A model the platform lists
    /// and the registry has never heard of is reachable through `auto` — but
    /// only as an unverified tail candidate, so a model that answers `404`
    /// cannot push a model we know works out of the head of the ladder.
    catalog: Arc<std::sync::RwLock<HashMap<String, Vec<String>>>>,
}

/// The ladder's class as a number: the lower the class, the cheaper the model.
fn class_rank(tier: ModelTier) -> u8 {
    match tier {
        ModelTier::Fallback => 0,
        ModelTier::FastExecution => 1,
        ModelTier::FrontierReasoning => 2,
    }
}

/// The smallest window a model may have and still be a model.
///
/// Below it the id is a router alias (`auto`, `auto:free`), not a model: the
/// platform picks a model per request, and for some of them an image is
/// refused outright.
const MIN_MODEL_WINDOW: u32 = 4096;

/// How many models the relay may ask to describe a picture.
///
/// One failure must not cost the relay: the next candidate gets the picture.
const VISION_RELAY_DESCRIBERS: usize = 3;

/// How many models one platform's measured catalogue may add to the ladder.
///
/// A platform that lists hundreds of models would otherwise bury the curated
/// candidates; the tail is a fallback, not a catalogue.
const CATALOG_TAIL_LIMIT: usize = 50;

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

/// Error for a request that does not fit the platform it named.
///
/// The candidate was skipped on purpose: the refusal was certain, so the
/// router did not pay for it. Saying "unknown model" here would be a lie —
/// the model is known, the request is simply too big for it, and the client
/// has to be told which limit it hit.
fn too_large_error(provider: &str, model: &str, detail: &str) -> GatewayError {
    GatewayError::InvalidRequest(format!(
        "request too large for {provider}/{model}: {detail}"
    ))
}

/// Error for a plan whose every candidate is in rate-limit cooldown.
fn all_candidates_cooling(model: &str) -> GatewayError {
    GatewayError::Upstream(format!(
        "{model}: every candidate model is in rate-limit cooldown; retry shortly"
    ))
}

/// The upstream calls that failed inside a request, keyed by request token.
type FailedAttempts = Vec<(String, Vec<FailedAttempt>)>;

/// How one upstream call ended when it refused.
///
/// Kept apart from the call site so the facts travel together: the status,
/// the time it cost and the text it answered with describe the same event.
#[derive(Debug, Clone)]
struct Refusal {
    /// Status the client saw.
    status: u16,
    /// How long the call took, in milliseconds.
    duration_ms: u32,
    /// What the call answered with, verbatim.
    error: Option<String>,
}

/// Milliseconds since `started`, capped at `u32::MAX`.
///
/// A call that outlives the cap is a call the operator wants to see as
/// "very long", not as a wrapped number that reads like a fast one.
fn elapsed_ms(started: std::time::Instant) -> u32 {
    started.elapsed().as_millis().min(u128::from(u32::MAX)) as u32
}

/// How many characters the request body carries, the way a platform that
/// counts characters instead of tokens measures it.
///
/// The platform sums the history with the tool schemas and the response
/// format: oneminai answers `400 Messages, tools and response format exceed
/// the 100,000 character limit` (probed live 2026-11-17).
fn payload_chars(request: &ChatCompletionRequest) -> usize {
    let mut chars = 0usize;
    for message in &request.messages {
        chars += message.role.chars().count();
        match message.content.as_ref() {
            Some(serde_json::Value::String(text)) => chars += text.chars().count(),
            Some(other) => chars += other.to_string().chars().count(),
            None => {}
        }
        if let Some(calls) = &message.tool_calls {
            chars += calls.to_string().chars().count();
        }
        if let Some(id) = &message.tool_call_id {
            chars += id.chars().count();
        }
    }
    if let Some(tools) = &request.tools {
        chars += tools.to_string().chars().count();
    }
    if let Some(format) = &request.response_format {
        chars += format.to_string().chars().count();
    }
    chars
}

/// One attempt against a named backend: the request leaves without the
/// internal attribution token, which no upstream may ever see, and without
/// fields that only the OpenAI platform API defines. `store` (server-side
/// conversation retention) is the known offender: OpenCode sends it on every
/// completion and Mistral's strict validation rejects the whole request with
/// `extra_forbidden` 422, while every other provider merely ignores it.
///
/// The attempt is also where the request is brought inside what the *named
/// provider* accepts: a platform with a narrower set of levels, a smaller
/// output budget or no place for the thinking sidecar kills the whole
/// request otherwise. The provider id is part of the decision on purpose: the
/// same model id behind two providers is served by two different APIs.
fn attempt(
    request: &ChatCompletionRequest,
    provider: &str,
    model: &str,
    context_limit: Option<u32>,
) -> ChatCompletionRequest {
    const OPENAI_ONLY_FIELDS: [&str; 1] = ["store"];
    let mut attempt = request.clone();
    attempt.model = model.to_string();
    attempt.extra.remove(ATTRIBUTION_TOKEN);
    for field in OPENAI_ONLY_FIELDS {
        attempt.extra.remove(field);
    }
    // Strict validators (Mistral) reject the whole request over this field.
    if crate::reasoning::rejects_reasoning_effort(model) {
        attempt.reasoning_effort = None;
    }
    // Reasoning models with fixed sampling knobs 400 on `temperature`
    // (probed live 2026-11-15) — the field cannot ride along.
    if crate::reasoning::rejects_temperature(model) {
        attempt.temperature = None;
    }
    // A provider whose set of levels is narrower than the client's: the
    // request is pulled down to the strongest level it accepts (probed live
    // 2026-11-16 on groq, which takes `low`, `medium` and `high` only).
    if let Some(allowed) = crate::provider_caps::allowed_efforts(provider, model) {
        let clamped =
            crate::provider_caps::clamp_effort(attempt.reasoning_effort.as_deref(), allowed);
        if attempt.reasoning_effort.is_some() && clamped != attempt.reasoning_effort {
            tracing::debug!(
                provider = %provider,
                model = %model,
                requested = %attempt.reasoning_effort.as_deref().unwrap_or("-"),
                sent = %clamped.as_deref().unwrap_or("-"),
                "provider caps: reasoning effort clamped to the platform's set"
            );
        }
        attempt.reasoning_effort = clamped;
    }
    // A model whose platform refuses the field outright (probed live
    // 2026-11-16 on groq's Qwen3.8 models).
    if crate::provider_caps::rejects_reasoning_effort_field(provider, model) {
        attempt.reasoning_effort = None;
    }
    // The thinking sidecar of an earlier assistant turn has no place in the
    // history of some platforms (probed live 2026-11-16 on groq and
    // oneminai). The visible text of the turn stays untouched.
    if crate::provider_caps::rejects_reasoning_content(provider) {
        for message in &mut attempt.messages {
            message.reasoning_content = None;
        }
    }
    // Cloudflare Workers AI validates the body against its own schema: a
    // content array of parts is not a string there ("Type mismatch of
    // '/messages/0/content', 'array' not in 'string'"), and a turn without
    // content is refused outright ("required properties at '/messages/5' are
    // 'role,content'" — probed live 2026-11-16, 326 refusals).
    //
    // The parts are joined into the text they carry, and a turn that has no
    // text at all (an assistant turn with tool calls only) gets an empty
    // string — the field has to be there.
    if crate::provider_caps::wants_string_content(provider) {
        for message in &mut attempt.messages {
            let as_array = message
                .content
                .as_ref()
                .is_some_and(|content| content.is_array());
            if as_array {
                let text = heterion_router_core::content_text(message.content.as_ref());
                message.content = Some(serde_json::Value::String(text));
            } else if message.content.is_none() {
                message.content = Some(serde_json::Value::String(String::new()));
            }
        }
    }
    // Some platforms validate the length of the history themselves ("List
    // should have at most 50 items after validation, not 69" — probed live
    // 2026-11-16 on typhoon). The oldest turns go first: the recent context
    // is what the model needs, and the request is served instead of refused.
    if let Some(cap) = crate::provider_caps::max_messages(provider)
        && attempt.messages.len() > cap
    {
        let cut = attempt.messages.len() - cap;
        attempt.messages.drain(0..cut);
    }
    // A platform that counts the request in characters, not tokens: oneminai
    // answers `400 Messages, tools and response format exceed the 100,000
    // character limit` (probed live 2026-11-17). The oldest **turns** go
    // first — cut at a turn boundary, so a tool call never loses the result
    // that answers it.
    if let Some(limit) = crate::provider_caps::max_payload_chars(provider) {
        while attempt.messages.len() > 1 && payload_chars(&attempt) > limit {
            let cut = attempt
                .messages
                .iter()
                .enumerate()
                .skip(1)
                .find(|(_, message)| message.role == "user")
                .map(|(index, _)| index)
                .unwrap_or(1);
            attempt.messages.drain(0..cut);
        }
    }
    // The output budget the platform publishes (probed live 2026-11-16 on
    // typhoon and oneminai).
    if let Some(cap) = crate::provider_caps::max_output_tokens(provider, model) {
        if attempt.max_tokens.is_some_and(|tokens| tokens > cap) {
            attempt.max_tokens = Some(cap);
        }
        if attempt
            .max_completion_tokens
            .is_some_and(|tokens| tokens > cap)
        {
            attempt.max_completion_tokens = Some(cap);
        }
    }
    // The output budget shares the context window with the prompt: the
    // platform counts them together ("This model's maximum context length is
    // 32000 tokens. However, you requested 32000 output tokens and your
    // prompt contains ..." — probed live 2026-11-16 on sealion's 32B model).
    // So the room left for the answer is the window minus the prompt.
    if let Some(limit) = context_limit.filter(|limit| *limit > 0) {
        let prompt = crate::context_pruner::fast_estimate_tokens(&attempt);
        let room = limit.saturating_sub(prompt);
        if room > 0 {
            if attempt.max_tokens.is_some_and(|tokens| tokens > room) {
                attempt.max_tokens = Some(room);
            }
            if attempt
                .max_completion_tokens
                .is_some_and(|tokens| tokens > room)
            {
                attempt.max_completion_tokens = Some(room);
            }
        }
    }
    attempt
}

#[cfg(test)]
mod attempt_tests {
    use super::attempt;
    use heterion_router_core::{ChatCompletionRequest, ChatMessage};

    #[test]
    fn attempt_strips_store_and_attribution_but_keeps_known_fields() {
        let mut request = ChatCompletionRequest {
            model: "combo".into(),
            messages: vec![ChatMessage::plain("user", "hi")],
            stream: true,
            ..Default::default()
        };
        request.extra.insert(
            "_heterion_router_attribution".into(),
            serde_json::json!("t"),
        );
        request
            .extra
            .insert("store".into(), serde_json::json!(false));
        request
            .extra
            .insert("logprobs".into(), serde_json::json!(true));

        let out = attempt(&request, "mistral", "codestral-latest", None);
        assert_eq!(out.model, "codestral-latest");
        assert!(!out.extra.contains_key("_heterion_router_attribution"));
        assert!(!out.extra.contains_key("store"));
        // Anything else the client sent still rides along.
        assert_eq!(out.extra.get("logprobs"), Some(&serde_json::json!(true)));
    }

    #[test]
    fn attempt_drops_reasoning_effort_for_models_that_reject_it() {
        let mut request = ChatCompletionRequest {
            model: "combo".into(),
            messages: vec![ChatMessage::plain("user", "hi")],
            reasoning_effort: Some("high".into()),
            ..Default::default()
        };
        // The client's field survives dispatch to a family that accepts it.
        let glm = attempt(&request, "zai", "glm-5.3-flash", None);
        assert_eq!(glm.reasoning_effort.as_deref(), Some("high"));
        // Mistral 400s on the field, so the attempt must not carry it.
        request.reasoning_effort = Some("medium".into());
        let codestral = attempt(&request, "mistral", "codestral-latest", None);
        assert_eq!(codestral.reasoning_effort, None);
    }

    /// A reasoning model that rejects `temperature` must not see it: the
    /// platform answers 400 for the whole request (probed live 2026-11-15).
    #[test]
    fn attempt_drops_temperature_for_models_that_reject_it() {
        let request = ChatCompletionRequest {
            model: "combo".into(),
            messages: vec![ChatMessage::plain("user", "hi")],
            temperature: Some(0.2),
            ..Default::default()
        };
        let sol = attempt(&request, "agentrouter", "gpt-5.6-sol", None);
        assert_eq!(sol.temperature, None);
        // A family that accepts sampling parameters keeps the client's value.
        let codestral = attempt(&request, "mistral", "codestral-latest", None);
        assert_eq!(codestral.temperature, Some(0.2));
    }

    /// A provider with a narrower set of levels gets the strongest level it
    /// accepts (probed live 2026-11-16: groq takes `low`, `medium`, `high`).
    #[test]
    fn attempt_clamps_reasoning_effort_to_what_the_provider_accepts() {
        let request = ChatCompletionRequest {
            model: "combo".into(),
            messages: vec![ChatMessage::plain("user", "hi")],
            reasoning_effort: Some("max".into()),
            ..Default::default()
        };
        let groq = attempt(&request, "groq", "openai/gpt-oss-120b", None);
        assert_eq!(groq.reasoning_effort.as_deref(), Some("high"));
        // Another provider keeps the client's value: groq's set is groq's.
        let gemini = attempt(&request, "gemini", "gemini-3.5-flash", None);
        assert_eq!(gemini.reasoning_effort.as_deref(), Some("max"));
    }

    /// A model whose platform refuses the field outright must not see it
    /// (probed live 2026-11-16: groq's Qwen3.8 models).
    #[test]
    fn attempt_drops_reasoning_effort_for_a_model_that_refuses_the_field() {
        let request = ChatCompletionRequest {
            model: "combo".into(),
            messages: vec![ChatMessage::plain("user", "hi")],
            reasoning_effort: Some("high".into()),
            ..Default::default()
        };
        let qwen = attempt(&request, "groq", "qwen/qwen3.8-27b", None);
        assert_eq!(qwen.reasoning_effort, None);
        // The same field is fine for another model of the same provider.
        let oss = attempt(&request, "groq", "openai/gpt-oss-120b", None);
        assert_eq!(oss.reasoning_effort.as_deref(), Some("high"));
    }

    /// The thinking sidecar of an earlier assistant turn has no place in the
    /// history of some platforms (probed live 2026-11-16: groq, oneminai).
    #[test]
    fn attempt_strips_reasoning_content_where_the_platform_refuses_it() {
        let mut assistant = ChatMessage::plain("assistant", "the answer");
        assistant.reasoning_content = Some("the thinking".to_string());
        let request = ChatCompletionRequest {
            model: "combo".into(),
            messages: vec![ChatMessage::plain("user", "hi"), assistant],
            ..Default::default()
        };
        let groq = attempt(&request, "groq", "openai/gpt-oss-120b", None);
        assert_eq!(groq.messages[1].reasoning_content, None);
        // The visible text of the turn is untouched.
        assert_eq!(
            groq.messages[1].content,
            Some(serde_json::json!("the answer")),
            "{:?}",
            groq.messages[1]
        );
        // A provider that takes the sidecar keeps it.
        let gemini = attempt(&request, "gemini", "gemini-3.5-flash", None);
        assert_eq!(
            gemini.messages[1].reasoning_content.as_deref(),
            Some("the thinking")
        );
    }

    /// The output budget shares the context window with the prompt (probed
    /// live 2026-11-16 on sealion's 32B model: "This model's maximum context
    /// length is 32000 tokens. However, you requested 32000 output tokens
    /// and your prompt contains ...").
    #[test]
    fn attempt_caps_the_output_budget_to_the_room_left_in_the_window() {
        let request = ChatCompletionRequest {
            model: "combo".into(),
            messages: vec![ChatMessage::plain("user", "x".repeat(4000))],
            max_completion_tokens: Some(60_000),
            ..Default::default()
        };
        let out = attempt(
            &request,
            "sealion",
            "aisingapore/Qwen-SEA-LION-v4-32B-IT",
            Some(32_000),
        );
        let prompt = crate::context_pruner::fast_estimate_tokens(&request);
        assert!(
            prompt > 0,
            "the prompt must be measured for the room to shrink"
        );
        assert_eq!(out.max_completion_tokens, Some(32_000 - prompt));
        // Without a known window the client's budget rides along unchanged
        // (a provider with a cap of its own is clamped by that cap instead).
        let unknown = attempt(&request, "gemini", "gemini-3.5-flash", None);
        assert_eq!(unknown.max_completion_tokens, Some(60_000));
    }

    /// The output budget the platform publishes is a cap, not a policy
    /// (probed live 2026-11-16: typhoon 16384, oneminai 32768).
    #[test]
    fn attempt_caps_the_output_budget_to_what_the_provider_accepts() {
        let request = ChatCompletionRequest {
            model: "combo".into(),
            messages: vec![ChatMessage::plain("user", "hi")],
            max_tokens: Some(64_000),
            max_completion_tokens: Some(64_000),
            ..Default::default()
        };
        let typhoon = attempt(&request, "typhoon", "typhoon-v2.5-30b-a3b-instruct", None);
        assert_eq!(typhoon.max_tokens, Some(16_384));
        assert_eq!(typhoon.max_completion_tokens, Some(16_384));
        // A budget inside the cap rides along unchanged.
        let small = attempt(
            &ChatCompletionRequest {
                max_tokens: Some(1_000),
                max_completion_tokens: Some(1_000),
                ..request.clone()
            },
            "typhoon",
            "typhoon-v2.5-30b-a3b-instruct",
            None,
        );
        assert_eq!(small.max_tokens, Some(1_000));
        // A provider without a published cap keeps the client's budget.
        let gemini = attempt(&request, "gemini", "gemini-3.5-flash", None);
        assert_eq!(gemini.max_tokens, Some(64_000));
    }

    /// Cloudflare validates the body against its own schema: a content array
    /// is not a string there, and a turn without content is refused outright
    /// (probed live 2026-11-16).
    #[test]
    fn attempt_writes_cloudflare_content_as_plain_text() {
        let request = ChatCompletionRequest {
            model: "combo".into(),
            messages: vec![
                ChatMessage::plain("user", "hello"),
                ChatMessage {
                    role: "assistant".into(),
                    content: Some(serde_json::json!([
                        {"type": "text", "text": "part one"},
                        {"type": "text", "text": "part two"}
                    ])),
                    reasoning_content: None,
                    tool_calls: None,
                    tool_call_id: None,
                    name: None,
                },
                ChatMessage {
                    role: "assistant".into(),
                    content: None,
                    reasoning_content: None,
                    tool_calls: Some(serde_json::json!([{"id": "c1"}])),
                    tool_call_id: None,
                    name: None,
                },
            ],
            ..Default::default()
        };
        let out = attempt(&request, "cloudflare-ai", "@cf/openai/gpt-oss-120b", None);
        // The parts are joined into the text they carry.
        assert_eq!(
            out.messages[1].content,
            Some(serde_json::json!("part onepart two"))
        );
        // A turn without content still has the field — the schema wants it.
        assert_eq!(out.messages[2].content, Some(serde_json::json!("")));
        // Another provider keeps the parts as they were sent.
        let gemini = attempt(&request, "gemini", "gemini-3.5-flash", None);
        assert!(gemini.messages[1].content.as_ref().unwrap().is_array());
    }

    /// typhoon validates the length of the history itself ("List should have
    /// at most 50 items after validation, not 69" — probed live 2026-11-16).
    #[test]
    fn attempt_cuts_the_history_to_what_the_provider_accepts() {
        let messages: Vec<ChatMessage> = (0..69)
            .map(|index| ChatMessage::plain("user", format!("turn {index}")))
            .collect();
        let request = ChatCompletionRequest {
            model: "combo".into(),
            messages,
            ..Default::default()
        };
        let out = attempt(&request, "typhoon", "typhoon-v2.5-30b-a3b-instruct", None);
        assert_eq!(out.messages.len(), 50);
        // The oldest turns go first: the recent context stays.
        assert_eq!(out.messages[49].content, Some(serde_json::json!("turn 68")));
        // A provider without a limit keeps the whole history.
        let gemini = attempt(&request, "gemini", "gemini-3.5-flash", None);
        assert_eq!(gemini.messages.len(), 69);
    }

    /// A platform that counts the request in characters refuses the whole
    /// request when it is too big ("Messages, tools and response format exceed
    /// the 100,000 character limit" — probed live 2026-11-17). The oldest
    /// turns go first, as for the history-length rule.
    #[test]
    fn attempt_cuts_the_history_to_the_character_budget() {
        let big = "x".repeat(40_000);
        let messages = vec![
            ChatMessage::plain("user", big.clone()),
            ChatMessage::plain("assistant", big.clone()),
            ChatMessage::plain("user", big.clone()),
        ];
        let request = ChatCompletionRequest {
            model: "combo".into(),
            messages,
            ..Default::default()
        };
        let out = attempt(&request, "oneminai", "gpt-4o-mini", None);
        // Two turns of 40 000 characters are over the budget. The cut is at a
        // turn boundary, so the oldest user turn goes together with the
        // answer it produced.
        assert_eq!(out.messages.len(), 1);
        // The recent context stays: the newest turn is kept.
        assert_eq!(
            out.messages[0].content,
            Some(serde_json::json!(big)),
            "the newest turn is kept"
        );
        // Another platform keeps the whole history.
        let gemini = attempt(&request, "gemini", "gemini-3.5-flash", None);
        assert_eq!(gemini.messages.len(), 3);
    }

    /// The cut is made at a turn boundary: a tool call never loses the result
    /// that answers it (a tool message without its call is refused by every
    /// platform).
    #[test]
    fn attempt_cuts_the_history_at_a_turn_boundary() {
        let big = "x".repeat(40_000);
        let messages = vec![
            ChatMessage::plain("user", big.clone()),
            ChatMessage {
                role: "assistant".into(),
                content: None,
                reasoning_content: None,
                tool_calls: Some(serde_json::json!([{"id": "c1"}])),
                tool_call_id: None,
                name: None,
            },
            ChatMessage {
                role: "tool".into(),
                content: Some(serde_json::json!(big.clone())),
                reasoning_content: None,
                tool_calls: None,
                tool_call_id: Some("c1".into()),
                name: None,
            },
            ChatMessage::plain("user", big.clone()),
        ];
        let request = ChatCompletionRequest {
            model: "combo".into(),
            messages,
            ..Default::default()
        };
        let out = attempt(&request, "oneminai", "gpt-4o-mini", None);
        // The tool call and its result went together with the turn they
        // belong to.
        assert!(out.messages.iter().all(|message| message.role != "tool"));
        assert!(
            out.messages
                .iter()
                .all(|message| message.role != "assistant")
        );
        assert_eq!(out.messages.len(), 1);
        assert_eq!(out.messages[0].content, Some(serde_json::json!(big)));
    }
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
            capabilities: None,
            trust: None,
            rotation: true,
            vision_relay: true,
            vision_saving: false,
            served_at: Arc::new(DashMap::new()),
            attributions: Arc::new(Mutex::new(Vec::new())),
            failed_attempts: Arc::new(Mutex::new(Vec::new())),
            cooldowns: Arc::new(ProviderCooldownSet::new()),
            quality_tracker: Arc::new(ModelQualityTracker::new()),
            catalog: Arc::new(std::sync::RwLock::new(HashMap::new())),
        }
    }

    /// Attach the measured catalogue cache.
    ///
    /// The cache is shared with the sync pass: it fills it while the gateway
    /// runs, and the ladder reads it on every plan.
    pub fn with_catalog_cache(
        mut self,
        cache: Arc<std::sync::RwLock<HashMap<String, Vec<String>>>>,
    ) -> Self {
        self.catalog = cache;
        self
    }

    /// The measured catalogue: provider → the model ids it published.
    pub fn catalog(&self) -> HashMap<String, Vec<String>> {
        self.catalog
            .read()
            .map(|cache| cache.clone())
            .unwrap_or_default()
    }

    /// Attach a database + registry for combo-name expansion.
    pub fn with_combo_source(mut self, db: Arc<Db>, registry: Arc<ProviderRegistry>) -> Self {
        self.db = Some(db);
        self.registry = Some(registry);
        self
    }

    /// Attach the capability table: the ladder's intrinsic priorities and the
    /// capability filters read from it.
    pub fn with_capabilities(mut self, capabilities: Arc<CapabilityStore>) -> Self {
        self.capabilities = Some(capabilities);
        self
    }

    /// Attach the trust store: the evidence that a provider really serves a
    /// model.
    ///
    /// Without it the router trusts everything — the capability table then
    /// decides the ladder alone, which is what a router without a database
    /// (tests, tools) can honestly do.
    pub fn with_trust(mut self, trust: Arc<TrustStore>) -> Self {
        self.trust = Some(trust);
        self
    }

    /// Switch the rotation layer on or off.
    ///
    /// On (the default, and what the running gateway uses): a model that
    /// answered cools down softly, so the next request walks down the ladder
    /// and every available model takes part in the traffic. Off: the ladder
    /// order is stable — the best-suited model keeps the head until quality,
    /// quota or a breaker says otherwise. The ladder contracts pin the order
    /// and therefore switch the rotation off; the rotation contract
    /// (`tests/model_ladder.rs`) switches it on.
    pub fn with_rotation(mut self, enabled: bool) -> Self {
        self.rotation = enabled;
        self
    }

    /// Switch the vision relay on or off.
    ///
    /// On (the default): a request that carries images and lands on a model
    /// that cannot see them is served through a borrowed description instead
    /// of dying on the upstream's `400`. Off: such a request keeps failing
    /// the way it did before the relay existed.
    pub fn with_vision_relay(mut self, enabled: bool) -> Self {
        self.vision_relay = enabled;
        self
    }

    /// Switch the token-saving variant of the relay on or off.
    ///
    /// Off (the default): only a model that *cannot* see the picture gets a
    /// borrowed description. On: a model that can see it hands the picture to
    /// a cheaper vision model first and receives the description instead —
    /// cheaper in image tokens, but the target no longer sees the picture
    /// itself. That trade is the operator's to make.
    pub fn with_vision_saving(mut self, enabled: bool) -> Self {
        self.vision_saving = enabled;
        self
    }

    /// Access the provider cooldown set.
    pub fn cooldowns(&self) -> &Arc<ProviderCooldownSet> {
        &self.cooldowns
    }

    /// Access the model quality tracker.
    pub fn quality_tracker(&self) -> &Arc<ModelQualityTracker> {
        &self.quality_tracker
    }

    /// Access the circuit breakers.
    pub fn breakers(&self) -> &Arc<Mutex<BreakerSet>> {
        &self.breakers
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

    fn lock_failed_attempts(&self) -> std::sync::MutexGuard<'_, FailedAttempts> {
        self.failed_attempts
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Remember one upstream call that failed inside the request stamped
    /// `token`.
    ///
    /// The journal writes one row per failed call, so a provider that refused
    /// the first three candidates and served the fourth is visible as three
    /// refusals — instead of one row naming only the last attempt.
    fn note_failed_attempt(
        &self,
        token: Option<&str>,
        provider: &str,
        backend: &dyn ChatBackend,
        model: &str,
        refusal: Refusal,
    ) {
        let Some(token) = token else {
            return;
        };
        let connection_id = backend
            .attribution(token)
            .connection_id
            .or_else(|| self.dispatch_head_connection_id(provider));
        let mut entries = self.lock_failed_attempts();
        if entries.len() >= ATTRIBUTION_SLOTS {
            entries.remove(0);
        }
        let attempt = FailedAttempt {
            provider: provider.to_string(),
            model: model.to_string(),
            connection_id,
            status: refusal.status,
            duration_ms: refusal.duration_ms,
            error: refusal.error,
        };
        match entries.iter_mut().find(|(key, _)| key == token) {
            Some((_, attempts)) => attempts.push(attempt),
            None => entries.push((token.to_string(), vec![attempt])),
        }
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
        tokens_compressed: Option<u32>,
        target_model: Option<&str>,
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
            tokens_compressed,
            model: target_model.map(str::to_string),
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
    fn resolve(&self, model: &str, request: &ChatCompletionRequest) -> Result<Plan, GatewayError> {
        if is_auto_model(model) {
            return Ok(self.auto_plan(request));
        }

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
        let weighted = plan_weighted_steps(combo, registry, &available);
        let config = combo.config.clone();
        // No servable entry: fall through to prefix routing so an
        // under-provisioned combo degrades like an unknown model.
        if weighted.is_empty() {
            return Ok(Plan {
                config,
                steps: self.prefix_plan(model),
                combo_name: None,
            });
        }
        Ok(Plan {
            config,
            steps: self.spread_combo(weighted, request),
            combo_name: Some(model.to_string()),
        })
    }

    /// The order a combo's entries are tried in.
    ///
    /// The entries are spread by the weight the author gave them: the
    /// traffic of a combo is shared by the models it contains, instead of
    /// always starting at the same entry until that model runs out of
    /// quota. The set of entries never changes — only the order they are
    /// tried in — and an entry without a weight stays at the back as a
    /// fallback.
    fn spread_combo(
        &self,
        steps: Vec<WeightedStep>,
        request: &ChatCompletionRequest,
    ) -> Vec<(String, String)> {
        let weighted: Vec<spread::Weighted<(String, String)>> = steps
            .into_iter()
            .map(|step| {
                spread::Weighted::new((step.backend, step.model), step.weight.max(0) as f64)
            })
            .collect();
        spread::spread_by_weight(&weighted, spread::request_seed(request))
    }

    /// Candidates from the explicit route rules (no combos involved).
    fn prefix_plan(&self, model: &str) -> Vec<(String, String)> {
        self.router
            .route(model)
            .into_iter()
            .map(|backend| (backend, model.to_string()))
            .collect()
    }

    /// Build a dynamic Switchyard candidate plan for `heterion-router/auto` or `auto`.
    pub fn auto_plan(&self, request: &ChatCompletionRequest) -> Plan {
        self.auto_plan_at(request, Instant::now())
    }

    /// The unified Switchyard pool: the curated ladder plus every live model
    /// of every provider that currently has a backend.
    ///
    /// The curated tier keeps its contract (seven entries, fixed order and
    /// priorities). The tail is the whole live catalogue — a model the
    /// operator connected but nobody curated is still reachable through `auto`.
    fn auto_pool(&self) -> Vec<PoolCandidate> {
        let curated: HashSet<(String, String)> = DEFAULT_AUTO_CANDIDATES
            .iter()
            .map(|candidate| (candidate.provider.to_string(), candidate.model.to_string()))
            .collect();

        let mut pool: Vec<PoolCandidate> = DEFAULT_AUTO_CANDIDATES
            .iter()
            .map(|candidate| PoolCandidate {
                provider: candidate.provider.to_string(),
                model: candidate.model.to_string(),
                base_priority: self.base_priority_for(
                    candidate.provider,
                    candidate.model,
                    candidate.base_priority,
                    true,
                ),
                web_executor: self.is_web_executor(candidate.provider),
            })
            .collect();

        let Some(registry) = self.registry.as_ref() else {
            return pool;
        };

        let mut tail: Vec<PoolCandidate> = Vec::new();
        // What the registry already lists: the measured catalogue must not
        // offer the same model twice.
        let mut listed: HashSet<(String, String)> = HashSet::new();
        for entry in registry.iter() {
            // Only providers with a live backend can serve anything.
            if !self.backends.contains_key(&entry.id) {
                continue;
            }
            let web_executor = entry.executor.as_deref() == Some("web");
            for model in &entry.models {
                listed.insert((entry.id.clone(), model.id.clone()));
                // The curated ladder already carries these ids.
                if curated.contains(&(entry.id.clone(), model.id.clone())) {
                    continue;
                }
                tail.push(PoolCandidate {
                    provider: entry.id.clone(),
                    model: model.id.clone(),
                    base_priority: self.base_priority_for(
                        &entry.id,
                        &model.id,
                        AUTO_TAIL_BASE_PRIORITY,
                        false,
                    ),
                    web_executor,
                });
            }
        }

        // What the platforms themselves published: a model the area lists and
        // the registry has never heard of is still reachable through `auto` —
        // as an unverified tail candidate, so a model that answers `404`
        // cannot push a model we know works out of the head of the ladder.
        for (provider, models) in self.catalog() {
            if !self.backends.contains_key(&provider) {
                continue;
            }
            let web_executor = self.is_web_executor(&provider);
            let mut added = 0usize;
            for model in models {
                if added >= CATALOG_TAIL_LIMIT {
                    break;
                }
                if curated.contains(&(provider.clone(), model.clone()))
                    || listed.contains(&(provider.clone(), model.clone()))
                {
                    continue;
                }
                added += 1;
                tail.push(PoolCandidate {
                    provider: provider.clone(),
                    model: model.clone(),
                    base_priority: self.base_priority_for(
                        &provider,
                        &model,
                        AUTO_TAIL_BASE_PRIORITY,
                        false,
                    ),
                    web_executor,
                });
            }
        }
        // Deterministic order: provider id, then model id. The quality
        // tracker moves a candidate up or down from there.
        tail.sort_by(|a, b| {
            a.provider
                .cmp(&b.provider)
                .then_with(|| a.model.cmp(&b.model))
        });
        pool.extend(tail);
        pool
    }

    /// Intrinsic priority of a model: what its capabilities are worth, or the
    /// fallback when the capability table does not know the model.
    ///
    /// The intrinsic priority never changes at runtime — an unstable model
    /// sinks through the dynamic part (quality score, cooldown), not through
    /// this number.
    ///
    /// An **untrusted** model is discounted: research says what a model is,
    /// but not whether this provider serves it, and a model that answers
    /// `404` must not push a model we have talked to out of the head of the
    /// ladder. The curated candidates are hand-verified and keep their
    /// priority as they are.
    fn base_priority_for(&self, provider: &str, model: &str, fallback: f64, curated: bool) -> f64 {
        match self.capabilities_of(provider, model) {
            Some(capabilities) => {
                let computed = ladder_priority(&capabilities);
                if curated || self.is_trusted(provider, model) {
                    return computed;
                }
                // Unverified research: the model may be described accurately
                // and still not be served by this provider. It keeps its place
                // in the pool — behind every model we have evidence for — and
                // climbs out of the discount the moment it answers once.
                if self.trust_evidence_exists() {
                    computed * crate::trust::UNTRUSTED_DISCOUNT
                } else {
                    computed
                }
            }
            // No research claim about this model: the hand-set priority is all
            // there is, and there is nothing to discount.
            None => fallback,
        }
    }

    /// Whether the router has any trust evidence at all.
    ///
    /// Without a single proven model there is nothing to be distrustful
    /// *relative to*: the capabilities decide alone.
    fn trust_evidence_exists(&self) -> bool {
        self.trust.as_ref().is_some_and(|trust| !trust.is_empty())
    }

    /// Whether the router has evidence that this provider serves this model.
    fn is_trusted(&self, provider: &str, model: &str) -> bool {
        self.trust
            .as_ref()
            .is_some_and(|trust| trust.is_trusted(provider, model))
    }

    /// Capabilities of one model, if the capability table knows it.
    fn capabilities_of(&self, provider: &str, model: &str) -> Option<ModelCapabilities> {
        self.capabilities
            .as_ref()
            .and_then(|store| store.get(provider, model))
    }

    /// The spec the capability filters use: the capability table when it
    /// knows the model, the static catalogue otherwise.
    fn spec_for(&self, provider: &str, model: &str) -> ModelSpec {
        match self.capabilities_of(provider, model) {
            Some(capabilities) => ModelSpec {
                context_window: if capabilities.context_window > 0 {
                    capabilities.context_window
                } else {
                    crate::model_spec::model_spec(model).context_window
                },
                supports_vision: capabilities.supports_vision,
                tool_support: capabilities.tool_support,
                tier: capabilities.tier,
            },
            None => crate::model_spec::model_spec(model),
        }
    }

    /// The models that can look at a picture, cheapest first.
    ///
    /// "Cheapest" is the ladder's own currency: the lower class first, the
    /// lower priority inside the class. A model the router has never talked
    /// to comes after every proven one — a description from a model that
    /// answers `404` is no description at all.
    ///
    /// A candidate must look like a model: a window of a few thousand tokens
    /// at least. The virtual aliases (`auto`, `auto:free`) are routers that
    /// pick a model per request, and the platform refuses an image for some
    /// of them — they are not describers.
    fn vision_describers(&self, exclude: Option<(&str, &str)>) -> Vec<(String, String)> {
        let mut proven: Vec<((String, String), (u8, f64))> = Vec::new();
        let mut unproven: Vec<((String, String), (u8, f64))> = Vec::new();
        for candidate in self.auto_pool() {
            if !self.backends.contains_key(&candidate.provider) {
                continue;
            }
            if let Some((provider, model)) = exclude
                && candidate.provider == provider
                && candidate.model == model
            {
                continue;
            }
            let spec = self.spec_for(&candidate.provider, &candidate.model);
            if !spec.supports_vision || spec.context_window < MIN_MODEL_WINDOW {
                continue;
            }
            if !self
                .cooldowns
                .is_available(&candidate.provider, Some(&candidate.model))
            {
                continue;
            }
            let rank = (class_rank(spec.tier), candidate.base_priority);
            let entry = ((candidate.provider.clone(), candidate.model.clone()), rank);
            if self.is_trusted(&candidate.provider, &candidate.model) {
                proven.push(entry);
            } else {
                unproven.push(entry);
            }
        }
        proven.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
        unproven.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
        tracing::debug!(
            proven = proven.len(),
            unproven = unproven.len(),
            "vision relay: models in the pool that can look at a picture"
        );
        proven
            .into_iter()
            .chain(unproven)
            .map(|((provider, model), _)| (provider, model))
            .collect()
    }

    /// Borrow eyes for a target that cannot see the picture.
    ///
    /// Returns the request the target should actually receive: the same
    /// request with the images replaced by a description another model wrote.
    /// `None` means "send the original request" — the relay never makes a
    /// failing request worse.
    async fn relay_vision(
        &self,
        request: &ChatCompletionRequest,
        plan: &[(String, String)],
    ) -> Option<ChatCompletionRequest> {
        let (target_provider, target_model) = plan.first()?;
        if !crate::inspector::inspect_request(request).has_images {
            return None;
        }
        let target_spec = self.spec_for(target_provider, target_model);
        // A target that cannot see the picture gets a borrowed description —
        // otherwise the upstream refuses the image. A target that *can* see
        // it only does so when the operator asked for the token-saving
        // variant.
        let blind = !target_spec.supports_vision;
        if !blind && !self.vision_saving {
            return None;
        }
        let describers =
            self.vision_describers(Some((target_provider.as_str(), target_model.as_str())));
        if describers.is_empty() {
            tracing::debug!(
                target = %target_model,
                "vision relay: no model in the pool can look at the picture"
            );
            return None;
        }
        for (provider, model) in describers.into_iter().take(VISION_RELAY_DESCRIBERS) {
            if !blind {
                // The borrowed description must not cost more than the picture
                // it replaces: only a strictly cheaper class may do the
                // looking.
                let describer_spec = self.spec_for(&provider, &model);
                if class_rank(describer_spec.tier) >= class_rank(target_spec.tier) {
                    continue;
                }
            }
            let describer_request = crate::vision_relay::describer_request(request, &model)?;
            let Some(backend) = self.backends.get(&provider) else {
                continue;
            };
            let response = match backend
                .complete(attempt(&describer_request, &provider, &model, None))
                .await
            {
                Ok(response) => response,
                Err(error) => {
                    tracing::warn!(
                        provider = %provider,
                        model = %model,
                        error = %error,
                        "vision relay: the describer failed, trying the next one"
                    );
                    continue;
                }
            };
            let description = response
                .choices
                .first()
                .map(|choice| choice.message.text())
                .unwrap_or_default();
            if description.trim().is_empty() {
                tracing::warn!(
                    provider = %provider,
                    model = %model,
                    "vision relay: the describer answered with nothing, trying the next one"
                );
                continue;
            }
            // The describer answered — the platform really serves that model.
            self.note_trusted(&provider, &model);
            self.note_served(&provider, &model);
            tracing::info!(
                provider = %provider,
                model = %model,
                target = %target_model,
                "vision relay: the picture was described by a borrowed model"
            );
            return Some(crate::vision_relay::with_description(
                request,
                &description,
                &format!("{provider}/{model}"),
            ));
        }
        tracing::warn!(
            target = %target_model,
            "vision relay: nobody could describe the picture, the request goes out unchanged"
        );
        None
    }

    /// Context window of a model: the measured one when the table knows it.
    fn context_limit_for(&self, provider: &str, model: &str) -> u32 {
        match self.capabilities_of(provider, model) {
            Some(capabilities) if capabilities.context_window > 0 => capabilities.context_window,
            _ => crate::context_pruner::model_context_limit(model),
        }
    }

    /// Remember that a model answered us: it is trusted from now on.
    ///
    /// Trust grows with use: the first successful answer is the evidence that
    /// this provider really serves this model, and from that moment the
    /// model competes on its capabilities alone.
    fn note_trusted(&self, provider: &str, model: &str) {
        if let Some(trust) = self.trust.as_ref() {
            trust.mark_trusted(provider, model);
        }
    }

    /// Remember that a model served a request: it cools down softly.
    ///
    /// With the rotation layer off the ladder order is pinned, so there is
    /// nothing to record: the served model keeps the head until quality,
    /// quota or a breaker moves it.
    fn note_served(&self, provider: &str, model: &str) {
        if !self.rotation {
            return;
        }
        let key = format!("{provider}/{model}");
        let now = Instant::now();
        self.served_at
            .entry(key)
            .and_modify(|record| {
                if now.saturating_duration_since(record.last)
                    < Duration::from_secs(AUTO_SOFT_COOLDOWN_SECS)
                {
                    record.count += 1;
                } else {
                    // The window is over: the model starts a fresh count.
                    record.count = 1;
                }
                record.last = now;
            })
            .or_insert(ServeRecord {
                last: now,
                count: 1,
                fails: 0,
                last_fail: now,
            });
    }

    /// Remember that a model failed: it goes down for a while.
    ///
    /// This is the owner's "unstable model goes down the ladder temporarily,
    /// keeping its intelligence and its properties": the model stays in the
    /// pool with all its capabilities, it is only pushed below its neighbours
    /// until the failure window is over.
    ///
    /// It is deliberately *not* the quality tracker's penalty: a 4xx must not
    /// put a model into the 15-minute probation (contract I-12) — it must not
    /// take the model out of the ladder at all.
    fn note_failed(&self, provider: &str, model: &str) {
        if !self.rotation {
            return;
        }
        let key = format!("{provider}/{model}");
        let now = Instant::now();
        self.served_at
            .entry(key)
            .and_modify(|record| {
                if now.saturating_duration_since(record.last_fail)
                    < Duration::from_secs(AUTO_FAILURE_COOLDOWN_SECS)
                {
                    record.fails += 1;
                } else {
                    record.fails = 1;
                }
                record.last_fail = now;
            })
            .or_insert(ServeRecord {
                last: now,
                count: 0,
                fails: 1,
                last_fail: now,
            });
    }

    /// The soft-cooldown factor of a model.
    ///
    /// 1.0 while the model has not served inside the cooldown window; the
    /// more it served inside it, the lower it ranks — one serving halves its
    /// priority, two servings cut it to a third. The ladder therefore walks
    /// down one rung per request and comes back to the head once the window
    /// is over: a rotation, not a ban.
    ///
    /// A model that *failed* sinks further and for longer: the failure window
    /// is twice the rotation window, and the factor is a quarter of the
    /// remaining priority on the first failure. A model whose id the provider
    /// does not serve must not be the first candidate of every request.
    ///
    /// With the rotation layer off the factor is always 1.0: the ladder order
    /// is then decided by the capabilities alone.
    fn soft_cooldown_factor(&self, provider: &str, model: &str, now: Instant) -> f64 {
        if !self.rotation {
            return 1.0;
        }
        let key = format!("{provider}/{model}");
        let Some(record) = self.served_at.get(&key) else {
            return 1.0;
        };
        let mut factor = 1.0;
        if now.saturating_duration_since(record.last) < Duration::from_secs(AUTO_SOFT_COOLDOWN_SECS)
        {
            factor *= 1.0 / (1.0 + record.count as f64);
        }
        if record.fails > 0
            && now.saturating_duration_since(record.last_fail)
                < Duration::from_secs(AUTO_FAILURE_COOLDOWN_SECS)
        {
            factor *= AUTO_FAILURE_FACTOR / (1.0 + record.fails.saturating_sub(1) as f64);
        }
        factor
    }

    /// Whether a provider is served by the web executor (a browser session
    /// instead of an API): such a backend answers with plain text and never
    /// emits tool calls.
    fn is_web_executor(&self, provider: &str) -> bool {
        self.registry
            .as_ref()
            .and_then(|registry| registry.get(provider))
            .and_then(|entry| entry.executor.as_deref())
            .is_some_and(|executor| executor == "web")
    }

    /// Build a dynamic Switchyard candidate plan with deterministic time injection.
    pub fn auto_plan_at(&self, request: &ChatCompletionRequest, now: Instant) -> Plan {
        let inspection = crate::inspector::inspect_request(request);
        let mut scored_candidates: Vec<Scored<(String, String)>> = Vec::new();

        for candidate in self.auto_pool() {
            let row = self.evaluate_candidate(&candidate, request, &inspection, now);
            if row.excluded.is_empty() && row.effective > 0.0 {
                let tier = self.spec_for(&row.provider, &row.model).tier;
                scored_candidates.push(Scored {
                    item: (row.provider, row.model),
                    effective: row.effective,
                    base_priority: row.base_priority,
                    tier,
                    cooling: row.soft_cooldown < 1.0,
                });
            }
        }

        // Sort by effective priority descending
        scored_candidates.sort_by(|a, b| {
            b.effective
                .partial_cmp(&a.effective)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        // The head of the ladder is shared by the models of its own class, so
        // one model does not burn the quota of the account while its
        // siblings stand idle.
        let scored_candidates = self.spread_head(scored_candidates, request);

        let steps: Vec<(String, String)> = scored_candidates
            .into_iter()
            .map(|candidate| candidate.item)
            .collect();

        Plan {
            config: ComboConfig::default(),
            steps,
            combo_name: Some(AUTO_COMBO_NAME.to_string()),
        }
    }

    /// Draw the head of the ladder among the models of its own class.
    ///
    /// The ladder says which model is *best suited*; the head says who
    /// answers *now*. Without the spread the same model takes every request
    /// until its quota is gone, while the models of its class stand idle.
    /// The spread lets the weight of the class decide who takes the head.
    ///
    /// Three rules keep the contracts:
    ///
    /// - only the head's own class takes part, and only the models that
    ///   stand right next to it: a model of another class between them
    ///   keeps its place, so the order of the classes never changes;
    /// - a candidate that just served is no peer: the rotation has already
    ///   demoted it, and the traffic must not go straight back to it;
    /// - a peer stands next to the head: a candidate far below the head is a
    ///   fallback, not a peer, and must not take the head from a better
    ///   model.
    ///
    /// With the rotation layer off the ladder order is pinned, so the spread
    /// is off as well.
    fn spread_head<T: Clone>(
        &self,
        scored: Vec<Scored<T>>,
        request: &ChatCompletionRequest,
    ) -> Vec<Scored<T>> {
        if !self.rotation || scored.len() < 2 {
            return scored;
        }
        let head = &scored[0];
        let floor = head.effective * AUTO_SPREAD_PEER_RATIO;
        // The peers of the head: the run of models that stand next to it and
        // belong to its class.
        let mut peers: Vec<usize> = Vec::new();
        for (index, candidate) in scored.iter().enumerate() {
            if candidate.tier != head.tier || candidate.effective < floor || candidate.cooling {
                break;
            }
            peers.push(index);
        }
        if peers.len() < 2 {
            return scored;
        }
        let weighted: Vec<spread::Weighted<usize>> = peers
            .iter()
            .map(|&index| spread::Weighted::new(index, scored[index].base_priority.max(0.0)))
            .collect();
        let order = spread::weighted_shuffle(&weighted, spread::request_seed(request));
        let chosen = order[0];
        let mut spread: Vec<Scored<T>> = Vec::with_capacity(scored.len());
        spread.push(scored[chosen].clone());
        for (index, candidate) in scored.into_iter().enumerate() {
            if index != chosen {
                spread.push(candidate);
            }
        }
        spread
    }

    /// The `auto` plan with the numbers that decided every candidate.
    ///
    /// The plan itself only says which models are tried and in which order —
    /// it never says why a model that the registry lists is *not* in it. This
    /// method answers that: for every candidate of the pool it reports the
    /// base priority, the quality score, the availability state, the soft
    /// cooldown factor, the resulting effective priority and, when the
    /// candidate was dropped, the reason (`no_backend`, `no_vision`,
    /// `no_tools`, `context`, `cooldown`, `breaker`, `zero_score`).
    pub fn auto_plan_debug(&self, request: &ChatCompletionRequest) -> Vec<PlanCandidate> {
        let inspection = crate::inspector::inspect_request(request);
        let now = Instant::now();
        let mut rows: Vec<Scored<PlanCandidate>> = self
            .auto_pool()
            .iter()
            .map(|candidate| {
                let row = self.evaluate_candidate(candidate, request, &inspection, now);
                Scored {
                    tier: self.spec_for(&row.provider, &row.model).tier,
                    cooling: row.soft_cooldown < 1.0,
                    effective: row.effective,
                    base_priority: row.base_priority,
                    item: row,
                }
            })
            .collect();
        rows.sort_by(|a, b| {
            b.effective
                .partial_cmp(&a.effective)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        // The same spread the plan uses, so the debug view and the plan do
        // not disagree about who answers.
        self.spread_head(rows, request)
            .into_iter()
            .map(|row| row.item)
            .collect()
    }

    /// Score one candidate of the pool — or say why it cannot be scored.
    ///
    /// The plan and the debug view must never disagree, so both read the
    /// numbers from here.
    fn evaluate_candidate(
        &self,
        candidate: &PoolCandidate,
        request: &ChatCompletionRequest,
        inspection: &crate::inspector::RequestInspection,
        now: Instant,
    ) -> PlanCandidate {
        let mut row = PlanCandidate {
            provider: candidate.provider.clone(),
            model: candidate.model.clone(),
            base_priority: candidate.base_priority,
            quality_score: 0.0,
            availability: 0.0,
            soft_cooldown: 0.0,
            effective: 0.0,
            excluded: String::new(),
        };

        // 1. Check if backend exists
        if !self.backends.contains_key(&candidate.provider) {
            row.excluded = "no_backend".to_string();
            return row;
        }

        // 2. Load model spec
        let spec = self.spec_for(&candidate.provider, &candidate.model);

        // 3. Vision constraint
        if inspection.has_images && !spec.supports_vision {
            row.excluded = "no_vision".to_string();
            return row;
        }

        // 4. Tool constraint & pseudo-tool check
        if inspection.has_tools {
            if spec.tool_support == ToolSupport::None {
                row.excluded = "no_tools".to_string();
                return row;
            }
            if candidate.web_executor {
                // The web executor has no tool head at all: its adapter
                // answers with page text and the backend hardcodes
                // `tool_calls: None`.
                row.excluded = "no_tools".to_string();
                return row;
            }
            if crate::model_spec::is_pseudo_tool_model(&candidate.provider, &candidate.model) {
                // Exclude pseudo-tool models when tools are requested
                row.excluded = "pseudo_tools".to_string();
                return row;
            }
        }

        // 5. Context filter vs JIT pruning feasibility
        if inspection.estimated_tokens > spec.context_window
            && !crate::context_pruner::can_prune_to_fit(request, spec.context_window)
        {
            row.excluded = "context".to_string();
            return row;
        }

        // 6. Factor in inspection.stage weighting if applicable
        let stage_weight = match (inspection.stage, spec.tier) {
            (
                crate::inspector::AgentStage::Planning,
                crate::model_spec::ModelTier::FrontierReasoning,
            ) => 1.05,
            (
                crate::inspector::AgentStage::ErrorRecovery,
                crate::model_spec::ModelTier::FrontierReasoning,
            ) => 1.05,
            (
                crate::inspector::AgentStage::MechanicalToolLoop,
                crate::model_spec::ModelTier::FastExecution,
            ) => 1.05,
            _ => 1.0,
        };
        let adjusted_base = candidate.base_priority * stage_weight;

        // 7. Dynamic Priority Formula:
        // Effective Priority = Base Priority * Quality Score * Availability State
        let quality_score = self
            .quality_tracker
            .effective_score_at(&candidate.model, now);

        // Availability State: 0.0 if in active cooldown or breaker is tripped; 1.0 if healthy.
        let in_cooldown =
            self.cooldowns
                .is_in_cooldown_at(&candidate.provider, Some(&candidate.model), now);
        let breaker_open = !self.lock_breakers().allow(&candidate.provider, now);
        let availability_state = if in_cooldown || breaker_open {
            0.0
        } else {
            1.0
        };

        let soft_cooldown = self.soft_cooldown_factor(&candidate.provider, &candidate.model, now);

        let effective = adjusted_base * quality_score * availability_state * soft_cooldown;

        row.base_priority = adjusted_base;
        row.quality_score = quality_score;
        row.availability = availability_state;
        row.soft_cooldown = soft_cooldown;
        row.effective = effective;

        if effective <= 0.0 {
            row.excluded = if in_cooldown {
                "cooldown".to_string()
            } else if breaker_open {
                "breaker".to_string()
            } else {
                "zero_score".to_string()
            };
        }

        row
    }
}

#[async_trait]
impl ChatBackend for RoutingBackend {
    fn name(&self) -> &'static str {
        "routing"
    }

    fn failed_attempts(&self, token: &str) -> Vec<FailedAttempt> {
        self.lock_failed_attempts()
            .iter()
            .find(|(key, _)| key == token)
            .map(|(_, attempts)| attempts.clone())
            .unwrap_or_default()
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
                tokens_compressed: None,
                model: None,
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
        } = self.resolve(&model, &request)?;
        // A model that cannot see the picture is served through a borrowed
        // description of it. The relay borrows one backend directly, so it
        // cannot recurse into the ladder, and a relay that fails leaves the
        // request exactly as it was.
        let relayed = if self.vision_relay {
            self.relay_vision(&request, &plan).await
        } else {
            None
        };
        let request = relayed.unwrap_or(request);
        let mut last_error: Option<GatewayError> = None;
        let mut tripped = false;
        // Why a candidate was never asked. The request itself does not fit the
        // platform, so the refusal was certain; the client still has to learn
        // that, instead of "unknown model".
        let mut skipped_too_large: Option<GatewayError> = None;

        let is_failover_plan = combo_name.is_some() || plan.len() > 1;
        let mut cooled = false;
        // The unified pool holds the whole live catalogue; the budget keeps a
        // request from walking every dead model in it one by one.
        let attempt_budget = if combo_name.as_deref() == Some(AUTO_COMBO_NAME) {
            AUTO_MAX_ATTEMPTS
        } else {
            usize::MAX
        };
        let mut attempts = 0usize;

        'candidates: for (name, target_model) in &plan {
            // The cooldown and the probation steer the *ladder*: they decide
            // which candidate to try next. The direct `provider/model` path is
            // left alone on purpose — a client that asked for one model by id
            // gets that model asked for, and a provider 4xx must not disable
            // the provider (frozen contract, AGENTS.md).
            if is_failover_plan {
                if !self.cooldowns.is_available(name, Some(target_model)) {
                    tracing::debug!(
                        provider = name,
                        model = target_model,
                        "skipping candidate: candidate is in active cooldown"
                    );
                    cooled = true;
                    continue;
                }
                // The probation is a quality signal, not a quota signal, so it
                // only demotes a candidate inside a failover plan.
                if self.quality_tracker.is_in_probation(target_model) {
                    tracing::debug!(
                        model = target_model,
                        "skipping candidate: model is in 15-minute probation"
                    );
                    continue;
                }
            }

            let target_limit = self.context_limit_for(name, target_model);
            let budget =
                (target_limit as f64 * crate::context_pruner::TOKEN_BUDGET_SAFETY_FACTOR) as u32;
            let prompt_tokens = crate::context_pruner::fast_estimate_tokens(&request);
            let (candidate_req, tokens_compressed) = if prompt_tokens > budget {
                let mut pruned = request.clone();
                let (new_tokens, orig_tokens) =
                    crate::context_pruner::prune_request(&mut pruned, target_limit);
                if new_tokens > target_limit {
                    tracing::warn!(
                        model = target_model,
                        prompt = new_tokens,
                        limit = target_limit,
                        "prompt exceeds context window after pruning; skipping candidate"
                    );
                    skipped_too_large = Some(too_large_error(
                        name,
                        target_model,
                        &format!(
                            "the prompt is {new_tokens} tokens after pruning, the platform's context window is {target_limit} tokens"
                        ),
                    ));
                    continue;
                }
                let compressed = orig_tokens.saturating_sub(new_tokens);
                (
                    pruned,
                    if compressed > 0 {
                        Some(compressed)
                    } else {
                        None
                    },
                )
            } else {
                (request.clone(), None)
            };

            // A provider whose account budget is smaller than the prompt can
            // never serve it — the refusal is certain, so the ladder moves on
            // instead of paying for it (measured live 2026-11-16: groq's
            // per-minute budget is 8000 tokens, the agent prompts are tens of
            // thousands).
            if let Some(ceiling) = crate::provider_caps::max_prompt_tokens(name)
                && crate::context_pruner::fast_estimate_tokens(&candidate_req) > ceiling
            {
                tracing::debug!(
                    provider = %name,
                    model = %target_model,
                    ceiling = ceiling,
                    "skipping candidate: the prompt is bigger than the provider's budget"
                );
                skipped_too_large = Some(too_large_error(
                    name,
                    target_model,
                    &format!(
                        "the prompt is about {} tokens, the platform's per-minute budget is {ceiling} tokens",
                        crate::context_pruner::fast_estimate_tokens(&candidate_req)
                    ),
                ));
                continue;
            }

            // The request as this provider will see it: the rules of
            // `attempt()` have already brought it inside the platform's
            // limits.
            let attempt_request = attempt(&candidate_req, name, target_model, Some(target_limit));
            // A platform that counts the request in characters refuses the
            // whole request when it is too big, and cutting the history is not
            // always enough — one huge turn or the tool schemas alone are
            // already over the budget. The refusal is certain, so the ladder
            // moves on instead of paying for it.
            if let Some(limit) = crate::provider_caps::max_payload_chars(name)
                && payload_chars(&attempt_request) > limit
            {
                tracing::debug!(
                    provider = %name,
                    model = %target_model,
                    limit,
                    "skipping candidate: the request is bigger than the platform's character budget"
                );
                skipped_too_large = Some(too_large_error(
                    name,
                    target_model,
                    &format!(
                        "the request is {} characters, the platform's limit is {limit} characters",
                        payload_chars(&attempt_request)
                    ),
                ));
                continue;
            }

            let mut retry_count = 0;
            loop {
                if !self.lock_breakers().allow(name, Instant::now()) {
                    tripped = true;
                    break;
                }
                let Some(backend) = self.backends.get(name) else {
                    break;
                };
                attempts += 1;
                let attempt_started = std::time::Instant::now();
                match backend.complete(attempt_request.clone()).await {
                    Ok(response) => {
                        self.lock_breakers().record_success(name);
                        // Post-flight tool validation.
                        if let Err(e) = crate::resilience::verify_tool_response(&request, &response)
                        {
                            tracing::warn!(
                                model = target_model,
                                provider = name,
                                error = %e,
                                "auto: post-flight tool validation failed, penalising and failing over"
                            );
                            self.quality_tracker.record_failure(target_model);
                            // The upstream call happened and the answer was
                            // rejected: it is a failed candidate, so it gets
                            // its own journal row like any other refusal.
                            let validation_error = GatewayError::Upstream(format!(
                                "upstream status 200: tool validation failed ({e})"
                            ));
                            self.note_failed_attempt(
                                token.as_deref(),
                                name,
                                backend.as_ref(),
                                target_model,
                                Refusal {
                                    status: crate::error_status(&validation_error).as_u16(),
                                    duration_ms: elapsed_ms(attempt_started),
                                    error: Some(validation_error.to_string()),
                                },
                            );
                            // Don't attribute — we're failing over.
                            last_error = Some(validation_error);
                            if attempts >= attempt_budget {
                                break 'candidates;
                            }
                            break;
                        }
                        self.quality_tracker.record_success(target_model);
                        self.note_trusted(name, target_model);
                        self.note_served(name, target_model);
                        self.attribute(
                            token.as_deref(),
                            name,
                            backend.as_ref(),
                            combo_name.as_deref(),
                            tokens_compressed,
                            Some(target_model),
                        );
                        return Ok(response);
                    }
                    Err(error) => {
                        if is_backend_outage(&error) {
                            self.lock_breakers().record_failure(name, Instant::now());
                        }
                        // Update cooldowns/quality for dynamic routing decisions.
                        if let GatewayError::Upstream(ref msg) = error {
                            match upstream_status_or_recovered(msg) {
                                Some(429) => {
                                    let retry_after =
                                        crate::resilience::extract_retry_after_from_error_message(
                                            msg,
                                        );
                                    // Scoped to the model, not the provider: a rate
                                    // limit on one model must not disable its
                                    // siblings — least of all the Tier-3 high-quota
                                    // protector that exists to absorb exactly this.
                                    self.cooldowns.record_429(
                                        name,
                                        Some(target_model),
                                        retry_after,
                                    );
                                    // A rate limit is a quota event, not a
                                    // defective response: the quality penalty
                                    // (and with it the 15-minute probation) is
                                    // reserved for defective answers. Charging
                                    // it here would keep the model out of the
                                    // ladder 7.5x longer than its own cooldown
                                    // (30s -> 60s -> 120s).
                                }
                                Some(402) | Some(403) => {
                                    self.cooldowns.record_quota_error(name);
                                    self.quality_tracker.record_failure(target_model);
                                }
                                Some(s) if s >= 500 => {
                                    self.quality_tracker.record_failure(target_model);
                                    self.note_failed(name, target_model);
                                }
                                Some(400) | Some(401) | Some(404) | Some(422) => {
                                    // The candidate itself is the problem: this
                                    // id is not served, or the platform refuses
                                    // it. The model goes down for a while — the
                                    // owner's "an unstable model sinks
                                    // temporarily, keeping its intelligence and
                                    // its properties". Deliberately *not* the
                                    // quality tracker's penalty: a 4xx must not
                                    // put the model into the 15-minute
                                    // probation (contract I-12), and it must
                                    // not take the model out of the ladder.
                                    self.note_failed(name, target_model);
                                }
                                _ => {}
                            }
                        }
                        self.attribute(
                            token.as_deref(),
                            name,
                            backend.as_ref(),
                            combo_name.as_deref(),
                            tokens_compressed,
                            Some(target_model),
                        );
                        // One failed upstream call, one journal row. The
                        // request-outcome row names only the last attempt, so
                        // without this the earlier refusals are invisible.
                        self.note_failed_attempt(
                            token.as_deref(),
                            name,
                            backend.as_ref(),
                            target_model,
                            Refusal {
                                status: crate::error_status(&error).as_u16(),
                                duration_ms: elapsed_ms(attempt_started),
                                error: Some(error.to_string()),
                            },
                        );
                        let transient = Self::is_transient(&error);
                        last_error = Some(error);
                        if attempts >= attempt_budget {
                            break 'candidates;
                        }
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
            } else if cooled {
                all_candidates_cooling(&model)
            } else if let Some(error) = skipped_too_large {
                error
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
            } = match this.resolve(&model, &request) {
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
            // Same reason as in `complete`: the request does not fit the
            // platform, so the client is told the limit it hit.
            let mut skipped_too_large: Option<GatewayError> = None;
            let is_failover_plan = combo_name.is_some() || plan.len() > 1;
            let mut cooled = false;
            // Same budget as in `complete`: the pool is the whole catalogue,
            // the request may not walk all of it.
            let attempt_budget = if combo_name.as_deref() == Some(AUTO_COMBO_NAME) {
                AUTO_MAX_ATTEMPTS
            } else {
                usize::MAX
            };
            let mut attempts = 0usize;

            'candidates: for (name, target_model) in &plan {
                // Same rule as in `complete`: the cooldown and the probation
                // steer the ladder, not the direct `provider/model` path.
                if is_failover_plan {
                    if !this.cooldowns.is_available(name, Some(target_model)) {
                        tracing::debug!(
                            provider = name,
                            model = target_model,
                            "skipping candidate: candidate is in active cooldown"
                        );
                        cooled = true;
                        continue;
                    }
                    if this.quality_tracker.is_in_probation(target_model) {
                        tracing::debug!(
                            model = target_model,
                            "skipping candidate: model is in 15-minute probation"
                        );
                        continue;
                    }
                }

                let target_limit = this.context_limit_for(name, target_model);
                let budget = (target_limit as f64
                    * crate::context_pruner::TOKEN_BUDGET_SAFETY_FACTOR)
                    as u32;
                let prompt_tokens = crate::context_pruner::fast_estimate_tokens(&request);
                let (candidate_req, tokens_compressed) = if prompt_tokens > budget {
                    let mut pruned = request.clone();
                    let (new_tokens, orig_tokens) =
                        crate::context_pruner::prune_request(&mut pruned, target_limit);
                    if new_tokens > target_limit {
                        tracing::warn!(
                            model = target_model,
                            prompt = new_tokens,
                            limit = target_limit,
                            "prompt exceeds context window after pruning; skipping candidate"
                        );
                        skipped_too_large = Some(too_large_error(
                            name,
                            target_model,
                            &format!(
                                "the prompt is {new_tokens} tokens after pruning, the platform's context window is {target_limit} tokens"
                            ),
                        ));
                        continue;
                    }
                    let compressed = orig_tokens.saturating_sub(new_tokens);
                    (
                        pruned,
                        if compressed > 0 {
                            Some(compressed)
                        } else {
                            None
                        },
                    )
                } else {
                    (request.clone(), None)
                };

                // The same rule as in the non-streaming path: a prompt bigger
                // than the provider's account budget can never be served.
                if let Some(ceiling) = crate::provider_caps::max_prompt_tokens(name)
                    && crate::context_pruner::fast_estimate_tokens(&candidate_req) > ceiling
                {
                    tracing::debug!(
                        provider = %name,
                        model = %target_model,
                        ceiling = ceiling,
                        "skipping candidate: the prompt is bigger than the provider's budget"
                    );
                    skipped_too_large = Some(too_large_error(
                        name,
                        target_model,
                        &format!(
                            "the prompt is about {} tokens, the platform's per-minute budget is {ceiling} tokens",
                            crate::context_pruner::fast_estimate_tokens(&candidate_req)
                        ),
                    ));
                    continue;
                }

                // The request as this provider will see it, and the same
                // character budget as in the non-streaming path.
                let attempt_request =
                    attempt(&candidate_req, name, target_model, Some(target_limit));
                if let Some(limit) = crate::provider_caps::max_payload_chars(name)
                    && payload_chars(&attempt_request) > limit
                {
                    tracing::debug!(
                        provider = %name,
                        model = %target_model,
                        limit,
                        "skipping candidate: the request is bigger than the platform's character budget"
                    );
                    skipped_too_large = Some(too_large_error(
                        name,
                        target_model,
                        &format!(
                            "the request is {} characters, the platform's limit is {limit} characters",
                            payload_chars(&attempt_request)
                        ),
                    ));
                    continue;
                }

                let mut retry_count = 0;
                loop {
                    if !this.lock_breakers().allow(name, Instant::now()) {
                        tripped = true;
                        break;
                    }
                    let Some(backend) = this.backends.get(name).cloned() else {
                        break;
                    };
                    attempts += 1;
                    let attempt_started = std::time::Instant::now();
                    let mut stream = backend.stream(attempt_request.clone());

                    match stream.next().await {
                        Some(Ok(first)) => {
                            this.lock_breakers().record_success(name);
                            this.quality_tracker.record_success(target_model);
                            this.note_trusted(name, target_model);
                            this.note_served(name, target_model);
                            this.attribute(
                                token.as_deref(),
                                name,
                                backend.as_ref(),
                                combo_name.as_deref(),
                                tokens_compressed,
                                Some(target_model),
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
                            // Update cooldowns/quality for dynamic routing decisions.
                            if let GatewayError::Upstream(ref msg) = error {
                                match upstream_status_or_recovered(msg) {
                                    Some(429) => {
                                        let retry_after =
                                            crate::resilience::extract_retry_after_from_error_message(msg);
                                        // Model-scoped, as in the non-streaming path:
                                        // one rate-limited model must not take its
                                        // provider siblings down with it. A rate limit
                                        // is a quota event, not a model defect, so the
                                        // quality penalty stays out of it.
                                        this.cooldowns.record_429(
                                            name,
                                            Some(target_model),
                                            retry_after,
                                        );
                                    }
                                    Some(402) | Some(403) => {
                                        this.cooldowns.record_quota_error(name);
                                        this.quality_tracker.record_failure(target_model);
                                    }
                                    Some(s) if s >= 500 => {
                                        this.quality_tracker.record_failure(target_model);
                                        this.note_failed(name, target_model);
                                    }
                                    Some(400) | Some(401) | Some(404) | Some(422) => {
                                        // Model-scoped demotion, as in the
                                        // non-streaming path: the id is not
                                        // served or the platform refuses it, so
                                        // the model goes down for a while —
                                        // without the quality penalty that
                                        // would put it into the 15-minute
                                        // probation (contract I-12).
                                        this.note_failed(name, target_model);
                                    }
                                    _ => {}
                                }
                            }
                            this.attribute(
                                token.as_deref(),
                                name,
                                backend.as_ref(),
                                combo_name.as_deref(),
                                tokens_compressed,
                                Some(target_model),
                            );
                            // One failed upstream call, one journal row — the
                            // same rule as the non-streaming path.
                            this.note_failed_attempt(
                                token.as_deref(),
                                name,
                                backend.as_ref(),
                                target_model,
                                Refusal {
                                    status: crate::error_status(&error).as_u16(),
                                    duration_ms: elapsed_ms(attempt_started),
                                    error: Some(error.to_string()),
                                },
                            );
                            let transient = Self::is_transient(&error);
                            last_error = Some(error);
                            if attempts >= attempt_budget {
                                break 'candidates;
                            }
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
                            this.quality_tracker.record_success(target_model);
                            this.note_trusted(name, target_model);
                            this.note_served(name, target_model);
                            this.attribute(
                                token.as_deref(),
                                name,
                                backend.as_ref(),
                                combo_name.as_deref(),
                                tokens_compressed,
                                Some(target_model),
                            );
                            return;
                        }
                    }
                }
            }

            let error = last_error.unwrap_or_else(|| {
                if tripped {
                    all_candidates_tripped(&model)
                } else if cooled {
                    all_candidates_cooling(&model)
                } else if let Some(error) = skipped_too_large {
                    error
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
    fn the_debug_plan_explains_every_candidate() {
        // A candidate that is not in the plan must say why: an operator who
        // asks "why is this model not answering" gets an answer, not a guess.
        let mut backends: HashMap<String, std::sync::Arc<dyn ChatBackend>> = HashMap::new();
        backends.insert("gemini".to_string(), std::sync::Arc::new(EchoBackend));
        let routing = RoutingBackend::new(backends, Router::new(vec![], vec![]));

        let plan = routing.auto_plan_debug(&request("heterion-router/auto"));

        assert!(!plan.is_empty(), "the plan must list candidates");
        assert_eq!(plan[0].provider, "gemini", "the curated head comes first");
        assert!(plan[0].effective > 0.0, "the head must be selectable");

        for row in &plan {
            if row.effective <= 0.0 {
                assert!(
                    !row.excluded.is_empty(),
                    "excluded candidate without a reason: {}/{}",
                    row.provider,
                    row.model
                );
            }
        }
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
                tokens_compressed: None,
                model: None,
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

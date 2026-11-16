//! Why a provider is (not) served — measured, not declared.
//!
//! The registry says what a platform *looks* like; only a live answer says
//! what it *is*. A keyless provider that answers `401` wants a credential, one
//! that answers `402` wants credits, and one that answers `404` has no chat
//! route at all. The reasons here are what the admin API reports and what the
//! keyless pass gates on: a provider enters the catalogue only after a probe
//! proved that it answers, because a catalogue entry that always fails is
//! worse than an absent one (ADR-025).

use serde::{Deserialize, Serialize};

/// Why a provider is served — or why it is not.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderReason {
    /// The provider has a live backend and answers.
    Served,
    /// The platform answered `401`/`403`: it wants a credential.
    NeedsKey,
    /// The platform answered `402`: it wants credits or a paid plan.
    CreditsRequired,
    /// The endpoint is not there (an HTML `404`, a gone host).
    DeadEndpoint,
    /// The host answers, but it has no chat route (a router's `404`).
    NotAChatApi,
    /// The registry declares a format the gateway cannot speak.
    UnsupportedFormat,
    /// A web executor without an adapter in the driver.
    NoWebAdapter,
    /// Nothing is connected and nothing probes as keyless.
    NoConnectionRow,
    /// Connected, but the backend did not come up.
    NotServed,
    /// The answer says nothing conclusive (timeout, `429`, `5xx`).
    Unknown,
}

impl ProviderReason {
    /// Stable string for logs, the API and the database.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Served => "served",
            Self::NeedsKey => "needs_key",
            Self::CreditsRequired => "credits_required",
            Self::DeadEndpoint => "dead_endpoint",
            Self::NotAChatApi => "not_a_chat_api",
            Self::UnsupportedFormat => "unsupported_format",
            Self::NoWebAdapter => "no_web_adapter",
            Self::NoConnectionRow => "no_connection_row",
            Self::NotServed => "not_served",
            Self::Unknown => "unknown",
        }
    }

    /// Parse a stored reason.
    pub fn parse(value: &str) -> Self {
        match value {
            "served" => Self::Served,
            "needs_key" => Self::NeedsKey,
            "credits_required" => Self::CreditsRequired,
            "dead_endpoint" => Self::DeadEndpoint,
            "not_a_chat_api" => Self::NotAChatApi,
            "unsupported_format" => Self::UnsupportedFormat,
            "no_web_adapter" => Self::NoWebAdapter,
            "no_connection_row" => Self::NoConnectionRow,
            "not_served" => Self::NotServed,
            _ => Self::Unknown,
        }
    }

    /// Whether the provider is being served.
    pub const fn is_served(self) -> bool {
        matches!(self, Self::Served)
    }
}

/// What is known about one provider, from the registry and from the router.
#[derive(Debug, Clone, Default)]
pub struct ProviderFacts {
    /// The provider has a live backend.
    pub served: bool,
    /// The provider has a connection row (active or not).
    pub connected: bool,
    /// The format the registry declares.
    pub format: Option<String>,
    /// The executor the registry declares.
    pub executor: Option<String>,
    /// The auth type the registry declares.
    pub auth_type: Option<String>,
    /// The gateway can speak this format (or has an executor of its own).
    pub modelled: bool,
    /// The web driver has an adapter for this provider.
    pub web_adapter: bool,
    /// What the last probe measured.
    pub probe: Option<ProviderReason>,
}

/// The reason a provider is (not) served.
///
/// The order matters: the *actionable* reason comes first. A structural gap
/// (the gateway cannot speak the format, the driver has no adapter) is
/// answered before a measured one, because no key and no credit will ever fix
/// a missing implementation — while a measured reason is what tells the
/// operator what to do next.
pub fn reason_for(facts: &ProviderFacts) -> ProviderReason {
    if facts.served {
        return ProviderReason::Served;
    }
    if !facts.modelled && facts.executor.as_deref() != Some("web") {
        return ProviderReason::UnsupportedFormat;
    }
    if facts.executor.as_deref() == Some("web") && !facts.web_adapter {
        return ProviderReason::NoWebAdapter;
    }
    if let Some(probe) = facts.probe
        && probe != ProviderReason::Unknown
    {
        return probe;
    }
    if !facts.connected {
        return if is_keyless(facts.auth_type.as_deref()) {
            ProviderReason::NoConnectionRow
        } else {
            ProviderReason::NeedsKey
        };
    }
    ProviderReason::NotServed
}

/// Classify one upstream answer into a reason.
///
/// The rules are read off the answer, not off the registry:
///
/// * `2xx` — the platform talks: served.
/// * `401`/`403` — it wants a credential; a body that says the beta is *closed*
///   means the platform itself is gone.
/// * `402` — it wants credits.
/// * `404`/`410` — a JSON body means the host is alive but has no such route
///   (not a chat API); anything else means the endpoint is not there.
/// * `405` — the route exists, but it is not the chat route.
/// * everything else (`429`, `5xx`, a timeout) says nothing conclusive.
pub fn classify(status: u16, body: &str) -> ProviderReason {
    let lowered = body.to_ascii_lowercase();
    match status {
        200..=299 => ProviderReason::Served,
        401 | 403 => {
            if lowered.contains("closed") || lowered.contains("no longer") {
                ProviderReason::DeadEndpoint
            } else {
                ProviderReason::NeedsKey
            }
        }
        402 => ProviderReason::CreditsRequired,
        404 | 410 => {
            if body.trim_start().starts_with('{') {
                ProviderReason::NotAChatApi
            } else {
                ProviderReason::DeadEndpoint
            }
        }
        405 => ProviderReason::NotAChatApi,
        _ => ProviderReason::Unknown,
    }
}

/// The URL a probe hits for one provider.
///
/// The registry declares either an API root or the endpoint itself, so the
/// path is resolved the same way the backends resolve it.
pub fn probe_url(format: Option<&str>, base_url: &str, model: Option<&str>) -> Option<String> {
    let base = base_url.trim();
    if !(base.starts_with("http://") || base.starts_with("https://")) {
        return None;
    }
    let trimmed = base.trim_end_matches('/');
    match format.unwrap_or("openai") {
        "openai" => Some(crate::openai::resolve_chat_url(trimmed)),
        "openai-responses" => Some(crate::openai::resolve_responses_url(trimmed)),
        // The registry declares the full `/v1/messages` endpoint.
        "claude" => Some(trimmed.to_string()),
        // Gemini embeds the model in the path.
        "gemini" => Some(format!(
            "{trimmed}/{}:generateContent",
            model.unwrap_or("gemini-2.0-flash")
        )),
        _ => None,
    }
}

/// The smallest request body a probe sends, per format.
///
/// It is deliberately trivial: the probe asks whether the platform *talks*,
/// not whether it can write a poem.
pub fn probe_body(format: Option<&str>, model: Option<&str>) -> String {
    let model = model.unwrap_or("probe");
    match format.unwrap_or("openai") {
        "openai-responses" => serde_json::json!({
            "model": model,
            "input": "ping",
            "max_output_tokens": 1,
        })
        .to_string(),
        "claude" => serde_json::json!({
            "model": model,
            "max_tokens": 1,
            "messages": [{"role": "user", "content": "ping"}],
        })
        .to_string(),
        "gemini" => serde_json::json!({
            "contents": [{"parts": [{"text": "ping"}]}],
            "generationConfig": {"maxOutputTokens": 1},
        })
        .to_string(),
        _ => serde_json::json!({
            "model": model,
            "max_tokens": 1,
            "messages": [{"role": "user", "content": "ping"}],
        })
        .to_string(),
    }
}

/// Whether a provider may be built without a credential.
///
/// A keyless provider is one the registry declares as usable without a key
/// (`auth_type: none` or `optional`). The declaration is a claim: only a probe
/// can confirm it.
pub fn is_keyless(auth_type: Option<&str>) -> bool {
    matches!(auth_type, Some("none" | "optional"))
}

/// Whether a provider may take part in the keyless pass.
///
/// It must speak the OpenAI dialect, live on HTTP(S), need no credential and
/// have no connection of its own — a connected provider is already built.
///
/// Only the OpenAI dialect qualifies: the other dialects are credential-bound
/// by construction (Gemini wants an API key in a header, the Anthropic and
/// Responses dialects want a token), so a keyless provider of those kinds
/// would be a declaration nobody can honour.
///
/// A **web executor** never qualifies. Its "endpoint" is a chat page: it
/// answers `200` to any POST, so a probe there measures nothing, and the
/// answer still comes from the browser driver — which needs an adapter
/// (ADR-025). Such a provider is served by the web pass or by nobody.
pub fn keyless_candidate(
    format: Option<&str>,
    auth_type: Option<&str>,
    base_url: Option<&str>,
    executor: Option<&str>,
    has_connection: bool,
) -> bool {
    if has_connection || !is_keyless(auth_type) {
        return false;
    }
    if executor == Some("web") {
        return false;
    }
    let Some(base_url) = base_url else {
        return false;
    };
    if !(base_url.starts_with("http://") || base_url.starts_with("https://")) {
        return false;
    }
    format == Some("openai")
}

/// The outcome of one probe.
#[derive(Debug, Clone)]
pub struct ProbeOutcome {
    /// The URL that was probed.
    pub url: String,
    /// The HTTP status, when the platform answered at all.
    pub status: Option<u16>,
    /// A short excerpt of the answer, for the report.
    pub body: String,
    /// What the answer means.
    pub reason: ProviderReason,
}

impl ProbeOutcome {
    /// A probe that never reached the platform.
    pub fn unreachable(url: String) -> Self {
        Self {
            url,
            status: None,
            body: String::new(),
            reason: ProviderReason::Unknown,
        }
    }
}

/// How much of the answer the report keeps.
const BODY_EXCERPT: usize = 240;

/// Probe one provider and classify the answer.
///
/// No credential is sent: the probe asks whether the platform answers without
/// one, which is exactly the question the keyless pass needs answered.
pub async fn probe(
    http: &reqwest::Client,
    format: Option<&str>,
    base_url: &str,
    model: Option<&str>,
) -> Option<ProbeOutcome> {
    let url = probe_url(format, base_url, model)?;
    let body = probe_body(format, model);
    let response = http.post(&url).body(body).send().await;
    let Ok(response) = response else {
        return Some(ProbeOutcome::unreachable(url));
    };
    let status = response.status().as_u16();
    let text = response.text().await.unwrap_or_default();
    let excerpt: String = text.chars().take(BODY_EXCERPT).collect();
    Some(ProbeOutcome {
        url,
        status: Some(status),
        reason: classify(status, &text),
        body: excerpt,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_talking_platform_is_served() {
        assert_eq!(classify(200, "{}"), ProviderReason::Served);
        assert_eq!(classify(201, "{}"), ProviderReason::Served);
    }

    #[test]
    fn a_credential_demanded_platform_needs_a_key() {
        assert_eq!(
            classify(
                401,
                "{\"error\":\"Authentication credentials were not provided\"}"
            ),
            ProviderReason::NeedsKey
        );
        assert_eq!(classify(403, "forbidden"), ProviderReason::NeedsKey);
    }

    #[test]
    fn a_closed_platform_is_dead_not_a_key_problem() {
        assert_eq!(
            classify(401, "The public beta is closed for now"),
            ProviderReason::DeadEndpoint
        );
    }

    #[test]
    fn credits_are_not_a_key_problem() {
        assert_eq!(
            classify(402, "{\"error\":{\"type\":\"insufficient_credits\"}}"),
            ProviderReason::CreditsRequired
        );
    }

    #[test]
    fn a_json_not_found_means_the_host_has_no_chat_route() {
        assert_eq!(
            classify(
                404,
                "{\"message\":\"Route POST:/chat/completions not found\"}"
            ),
            ProviderReason::NotAChatApi
        );
    }

    #[test]
    fn an_html_not_found_means_the_endpoint_is_not_there() {
        assert_eq!(
            classify(
                404,
                "<html lang=\"en\"><head><title>404</title></head></html>"
            ),
            ProviderReason::DeadEndpoint
        );
    }

    #[test]
    fn a_method_rejection_means_the_route_is_not_the_chat_route() {
        assert_eq!(classify(405, ""), ProviderReason::NotAChatApi);
    }

    #[test]
    fn a_rate_limit_and_an_outage_say_nothing() {
        assert_eq!(classify(429, "slow down"), ProviderReason::Unknown);
        assert_eq!(classify(503, "maintenance"), ProviderReason::Unknown);
    }

    #[test]
    fn the_probe_url_follows_the_registry_shape() {
        assert_eq!(
            probe_url(Some("openai"), "https://api.x/v1", None).as_deref(),
            Some("https://api.x/v1/chat/completions")
        );
        assert_eq!(
            probe_url(Some("openai"), "https://api.x/v1/chat/completions", None).as_deref(),
            Some("https://api.x/v1/chat/completions")
        );
        assert_eq!(
            probe_url(Some("openai-responses"), "https://api.x/v1/responses", None).as_deref(),
            Some("https://api.x/v1/responses")
        );
        assert_eq!(
            probe_url(
                Some("claude"),
                "https://api.x/api/anthropic/v1/messages",
                None
            )
            .as_deref(),
            Some("https://api.x/api/anthropic/v1/messages")
        );
        assert_eq!(
            probe_url(Some("gemini"), "https://gen.x/v1beta/models", Some("m-1")).as_deref(),
            Some("https://gen.x/v1beta/models/m-1:generateContent")
        );
        assert!(probe_url(Some("cursor"), "https://api2.cursor.sh", None).is_none());
        assert!(probe_url(Some("openai"), "auggie://cli/stdio", None).is_none());
    }

    #[test]
    fn a_structural_gap_outranks_a_measured_one() {
        // A format the gateway cannot speak: no key would ever fix it.
        let facts = ProviderFacts {
            served: false,
            connected: true,
            format: Some("cursor".to_string()),
            executor: None,
            auth_type: Some("apikey".to_string()),
            modelled: false,
            web_adapter: false,
            probe: Some(ProviderReason::NeedsKey),
        };
        assert_eq!(reason_for(&facts), ProviderReason::UnsupportedFormat);

        // A web executor without an adapter.
        let facts = ProviderFacts {
            format: Some("openai".to_string()),
            executor: Some("web".to_string()),
            modelled: true,
            ..Default::default()
        };
        assert_eq!(reason_for(&facts), ProviderReason::NoWebAdapter);
    }

    #[test]
    fn a_measured_reason_is_used_when_the_implementation_exists() {
        let facts = ProviderFacts {
            served: false,
            connected: false,
            format: Some("openai".to_string()),
            executor: None,
            auth_type: Some("optional".to_string()),
            modelled: true,
            web_adapter: false,
            probe: Some(ProviderReason::NeedsKey),
        };
        assert_eq!(reason_for(&facts), ProviderReason::NeedsKey);
    }

    #[test]
    fn a_provider_without_a_key_and_without_a_connection_waits_for_a_key() {
        let facts = ProviderFacts {
            served: false,
            connected: false,
            format: Some("openai".to_string()),
            executor: None,
            auth_type: Some("apikey".to_string()),
            modelled: true,
            web_adapter: false,
            probe: None,
        };
        assert_eq!(reason_for(&facts), ProviderReason::NeedsKey);
    }

    #[test]
    fn a_keyless_provider_without_a_connection_has_nothing_to_connect() {
        let facts = ProviderFacts {
            served: false,
            connected: false,
            format: Some("openai".to_string()),
            executor: None,
            auth_type: Some("none".to_string()),
            modelled: true,
            web_adapter: false,
            probe: None,
        };
        assert_eq!(reason_for(&facts), ProviderReason::NoConnectionRow);
    }

    #[test]
    fn a_served_provider_is_served_whatever_else_is_true() {
        let facts = ProviderFacts {
            served: true,
            connected: false,
            format: Some("openai".to_string()),
            executor: None,
            auth_type: Some("apikey".to_string()),
            modelled: true,
            web_adapter: false,
            probe: Some(ProviderReason::NeedsKey),
        };
        assert_eq!(reason_for(&facts), ProviderReason::Served);
    }

    #[test]
    fn the_keyless_pass_takes_only_unconnected_keyless_http_providers() {
        // A keyless provider on HTTP with a modelled format qualifies.
        assert!(keyless_candidate(
            Some("openai"),
            Some("none"),
            Some("https://api.x/v1"),
            None,
            false
        ));
        // A provider that needs a key is not keyless.
        assert!(!keyless_candidate(
            Some("openai"),
            Some("apikey"),
            Some("https://api.x/v1"),
            None,
            false
        ));
        // A connected provider is already built.
        assert!(!keyless_candidate(
            Some("openai"),
            Some("none"),
            Some("https://api.x/v1"),
            None,
            true
        ));
        // A format the gateway cannot speak is out.
        assert!(!keyless_candidate(
            Some("cursor"),
            Some("none"),
            Some("https://api.x"),
            None,
            false
        ));
        // A dialect that is credential-bound by construction is out too.
        assert!(!keyless_candidate(
            Some("claude"),
            Some("none"),
            Some("https://api.x/v1/messages"),
            None,
            false
        ));
        // A local CLI is not an HTTP provider.
        assert!(!keyless_candidate(
            Some("openai"),
            Some("none"),
            Some("auggie://cli/stdio"),
            None,
            false
        ));
        // A web executor answers 200 to any POST: a probe there measures
        // nothing, and the answer comes from the browser driver.
        assert!(!keyless_candidate(
            Some("openai"),
            Some("none"),
            Some("https://duck.ai/duckchat/v1/chat"),
            Some("web"),
            false
        ));
    }
}

//! Client fingerprints for upstreams that only answer a recognised client.
//!
//! Some gateways meter quota by *client type* and refuse a request that
//! carries no recognised client identity — agentrouter answers
//! `401 unauthorized_client_error` ("unauthorized client detected") even when
//! the key is valid. Probed live 2026-10-27: a request without a fingerprint
//! and a request with a browser user-agent are both refused; a request with
//! `originator: codex_cli_rs` is accepted.
//!
//! The identity is per provider, not global: a fingerprint that one upstream
//! needs must not leak to the 250+ others, so it is looked up by provider id
//! and defaults to "no extra headers".

use heterion_router_core::ChatCompletionRequest;
use sha2::{Digest, Sha256};

/// Extra request headers that identify the client to an upstream.
pub type ClientHeaders = &'static [(&'static str, &'static str)];

/// The client fingerprint for `provider`; empty when it needs none.
///
/// The provider id is matched exactly: only the upstream that demands a
/// fingerprint gets one.
pub fn headers_for(provider: &str) -> ClientHeaders {
    match provider {
        "agentrouter" => CODEX,
        _ => &[],
    }
}

/// Codex client identity.
///
/// agentrouter's live models serve the Codex Responses protocol
/// (`/v1/responses`; `/v1/chat/completions` is refused with
/// `400 不支持 /v1/chat/completions 接口，请改用 /v1/responses`), and `codex`
/// is one of the clients the platform lists as supported. `originator` is the
/// header the platform reads, and it is sufficient on its own — probed live
/// 2026-10-27, with and without a user-agent.
///
/// The user-agent names the router instead of pretending to be the Codex CLI:
/// the account's group (and with it the price multiplier) does not depend on
/// it — every variant probed on 2026-10-27 reported the same `default` group —
/// so there is nothing to gain from a borrowed version string.
const CODEX: ClientHeaders = &[
    ("originator", "codex_cli_rs"),
    ("user-agent", ROUTER_USER_AGENT),
];

/// The router's own user-agent, versioned with the crate.
const ROUTER_USER_AGENT: &str = concat!(
    "heterion-router/",
    env!("CARGO_PKG_VERSION"),
    " (codex-responses client)"
);

/// Headers for one request to `provider`: the static client fingerprint plus
/// a session id where the upstream routes by conversation.
///
/// The static part is the same for every request; the session id is not, so
/// it is built here instead of in [`headers_for`].
pub fn request_headers(provider: &str, request: &ChatCompletionRequest) -> Vec<(String, String)> {
    let mut headers: Vec<(String, String)> = headers_for(provider)
        .iter()
        .map(|(name, value)| ((*name).to_string(), (*value).to_string()))
        .collect();
    if let Some(session) = session_id(provider, request) {
        headers.push(("x-opencode-session".to_string(), session));
    }
    headers
}

/// A stable session id for the conversation, where the upstream asks for one.
///
/// OpenCode's Console gateway routes and caches by conversation: a request
/// without `x-opencode-session` is refused outright — `MissingSessionID`,
/// "Request is missing x-opencode-session and cannot be routed efficiently"
/// (probed live 2026-11-17). The id has to be *stable*: a fresh one per
/// request would defeat the prompt cache the platform keeps on it. So it is
/// derived from the conversation itself — the first user turn, which does not
/// change while the conversation grows.
pub fn session_id(provider: &str, request: &ChatCompletionRequest) -> Option<String> {
    if !provider.starts_with("opencode") {
        return None;
    }
    let first = request
        .messages
        .iter()
        .find(|message| message.role == "user")
        .or_else(|| request.messages.first())?;
    let mut hasher = Sha256::new();
    hasher.update(first.text().as_bytes());
    Some(format!("ses_{}", hex::encode(&hasher.finalize()[..16])))
}

#[cfg(test)]
mod tests {
    use super::*;
    use heterion_router_core::ChatMessage;

    fn request(first_turn: &str) -> ChatCompletionRequest {
        ChatCompletionRequest {
            model: "opencode-go/glm-5.2".into(),
            messages: vec![ChatMessage::plain("user", first_turn)],
            ..Default::default()
        }
    }

    #[test]
    fn agentrouter_identifies_as_a_codex_client() {
        let headers = headers_for("agentrouter");
        assert_eq!(
            headers.first(),
            Some(&("originator", "codex_cli_rs")),
            "agentrouter refuses a request without a recognised client"
        );
        assert!(
            headers
                .iter()
                .any(|(name, value)| *name == "user-agent" && value.contains("heterion-router")),
            "the router must not borrow another client's version string: {headers:?}"
        );
    }

    #[test]
    fn other_providers_get_no_fingerprint() {
        assert!(headers_for("deepseek").is_empty());
        assert!(headers_for("openrouter").is_empty());
        assert!(headers_for("").is_empty());
    }

    #[test]
    fn an_opencode_provider_gets_a_session_id() {
        let session = session_id("opencode-go", &request("hello")).expect("a session id");
        assert!(
            session.starts_with("ses_"),
            "unexpected session id: {session}"
        );
        let headers = request_headers("opencode-go", &request("hello"));
        assert!(
            headers
                .iter()
                .any(|(name, value)| name == "x-opencode-session" && *value == session),
            "the upstream refuses a request without x-opencode-session: {headers:?}"
        );
    }

    #[test]
    fn the_session_id_is_stable_for_one_conversation() {
        let first = session_id("opencode-go", &request("hello")).expect("a session id");
        // The conversation grows, the first turn does not change.
        let mut grown = request("hello");
        grown.messages.push(ChatMessage::assistant_text("hi"));
        let second = session_id("opencode-go", &grown).expect("a session id");
        assert_eq!(
            first, second,
            "a fresh id per request would defeat the upstream's prompt cache"
        );
        let other = session_id("opencode-go", &request("a different task")).expect("a session id");
        assert_ne!(
            first, other,
            "two conversations must not share a session id"
        );
    }

    #[test]
    fn other_providers_get_no_session_id() {
        assert!(session_id("deepseek", &request("hello")).is_none());
        assert!(session_id("", &request("hello")).is_none());
    }
}

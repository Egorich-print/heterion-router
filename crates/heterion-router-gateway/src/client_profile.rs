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

#[cfg(test)]
mod tests {
    use super::*;

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
}

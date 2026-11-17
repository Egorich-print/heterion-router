//! The address a platform is measured at is the address its backend uses.
//!
//! A registry URL may carry an `{env:NAME}` placeholder (Cloudflare Workers AI
//! wants the account id inside the path), and the value belongs to the
//! operator. A measurement that went out with the placeholder unresolved
//! measured nothing — measured 17.11.2026: Cloudflare answered «Could not
//! route to /client/v4/accounts/%7Benv:HETERION_ROUTER_CLOUDFLARE_ACCOUNT_ID%7D
//! /ai/v1/chat/completions».

use heterion_router_gateway::placeholders::{expand_placeholders, expand_placeholders_with};
use heterion_router_providers::ProviderRegistry;

#[test]
fn a_placeholder_is_filled_from_the_environment() {
    let filled = expand_placeholders_with(
        "https://api.example/client/v4/accounts/{env:ACCOUNT}/ai/v1",
        |name| (name == "ACCOUNT").then(|| "acct-42".to_string()),
    );
    assert_eq!(
        filled.as_deref(),
        Some("https://api.example/client/v4/accounts/acct-42/ai/v1")
    );
}

#[test]
fn an_unfilled_placeholder_is_no_address() {
    assert_eq!(
        expand_placeholders_with("https://api.example/accounts/{env:MISSING}/ai/v1", |_| None),
        None
    );
    assert_eq!(
        expand_placeholders_with("https://api.example/accounts/{env:EMPTY}", |_| {
            Some("   ".to_string())
        }),
        None,
        "an empty value is not a value"
    );
}

#[test]
fn the_registry_keeps_the_placeholder_and_the_environment_carries_the_value() {
    let registry = ProviderRegistry::load();
    let entry = registry
        .get("cloudflare-ai")
        .expect("cloudflare-ai is registered");
    let raw = entry.base_url.as_deref().expect("a base url");
    assert!(
        raw.contains("{env:HETERION_ROUTER_CLOUDFLARE_ACCOUNT_ID}"),
        "the registry must not carry the account id: {raw}"
    );
    // Without the variable in the environment there is no address at all — and
    // an address half written is worse than none.
    let resolved = expand_placeholders(raw);
    assert!(
        resolved.is_none() || !resolved.as_deref().unwrap_or_default().contains("{env:"),
        "a resolved address must not keep a placeholder: {resolved:?}"
    );
}

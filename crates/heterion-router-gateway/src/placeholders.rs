//! `{env:NAME}` placeholders in a provider's address.
//!
//! Some providers address an account-scoped endpoint: Cloudflare Workers AI
//! wants the account id inside the path
//! (`/client/v4/accounts/<account_id>/ai/v1`), and that id belongs to the
//! operator's account, not to the provider — so it cannot live in the static
//! registry. The registry keeps the placeholder, the environment carries the
//! value.
//!
//! A placeholder that the environment does not fill yields `None`: a provider
//! whose address is half written cannot answer, and a half-filled address
//! would fail later as an opaque 404 instead of "not configured".
//!
//! The expansion lives in the library, not in the binary, because the
//! measurement of a platform (its catalogue and its quota) must reach the
//! same address the backend reaches — a probe that went out with an
//! unresolved placeholder measured nothing (measured 17.11.2026: Cloudflare
//! answered «Could not route to /client/v4/accounts/{env:…}/ai/v1/
//! chat/completions»).

/// Fill `{env:NAME}` placeholders in a provider base URL from the environment.
pub fn expand_placeholders(url: &str) -> Option<String> {
    expand_placeholders_with(url, |name| std::env::var(name).ok())
}

/// The placeholder expansion with an injected lookup, so the tests do not
/// have to mutate the process environment.
pub fn expand_placeholders_with(
    url: &str,
    lookup: impl Fn(&str) -> Option<String>,
) -> Option<String> {
    let mut out = String::with_capacity(url.len());
    let mut rest = url;
    while let Some(start) = rest.find("{env:") {
        out.push_str(&rest[..start]);
        let tail = &rest[start..];
        let Some(end) = tail.find('}') else {
            // A brace that never closes is not a placeholder: keep the text.
            out.push_str(tail);
            return Some(out);
        };
        let name = &tail["{env:".len()..end];
        let value = lookup(name).filter(|value| !value.trim().is_empty())?;
        out.push_str(&value);
        rest = &tail[end + 1..];
    }
    out.push_str(rest);
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

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
                Some("  ".to_string())
            }),
            None,
            "an empty value is not a value"
        );
    }

    #[test]
    fn a_url_without_placeholders_is_itself() {
        assert_eq!(
            expand_placeholders_with("https://api.example/v1", |_| None).as_deref(),
            Some("https://api.example/v1")
        );
    }

    #[test]
    fn several_placeholders_are_filled_in_order() {
        let filled = expand_placeholders_with("https://{env:HOST}/{env:PATH}", |name| match name {
            "HOST" => Some("api.example".to_string()),
            "PATH" => Some("v1".to_string()),
            _ => None,
        });
        assert_eq!(filled.as_deref(), Some("https://api.example/v1"));
    }
}

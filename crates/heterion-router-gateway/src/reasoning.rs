//! Predefined reasoning defaults per model family (ADR-014).
//!
//! OpenCode no longer controls reasoning for heterion-router models; the
//! router applies an evidence-backed default instead. A request may always
//! override explicitly (`reasoning_effort` / `reasoning.effort` wins over
//! this table). Evidence: provider docs + Artificial Analysis snapshots
//! recorded in `docs/research/mission-2026-10/model_inventory_2026-10-14.md`.

/// Predefined default `reasoning_effort` for a model id, or `None` when the
/// family has no evidence-backed default (the provider default applies).
///
/// `provider/model` ids are matched on the bare model part, so combo steps
/// rewritten to `grok-cli/grok-4.6` resolve the same as a bare `grok-4.6`.
pub fn default_effort(model: &str) -> Option<&'static str> {
    let bare = model.rsplit('/').next().unwrap_or(model);
    let bare = bare.to_ascii_lowercase();
    if bare.starts_with("claude-opus") {
        // Complex agentic tasks reward deep reasoning (provider docs).
        Some("high")
    } else if bare.starts_with("grok") {
        // Efficient at medium; high rarely pays for itself.
        Some("medium")
    } else if bare.starts_with("deepseek") {
        // Varies by task; medium is the balanced default.
        Some("medium")
    } else if bare.starts_with("gemini") {
        // Rate-limited families: keep thinking cheap.
        Some("low")
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::default_effort;

    #[test]
    fn evidenced_families_get_their_predefined_level() {
        assert_eq!(default_effort("claude-opus-4"), Some("high"));
        assert_eq!(default_effort("claude-opus-4-8"), Some("high"));
        assert_eq!(default_effort("grok-4.6"), Some("medium"));
        assert_eq!(default_effort("deepseek-v4-flash"), Some("medium"));
        assert_eq!(default_effort("gemini-3.7-flash"), Some("low"));
    }

    #[test]
    fn provider_prefixed_ids_match_on_the_bare_model() {
        assert_eq!(default_effort("grok-cli/grok-4.6"), Some("medium"));
        assert_eq!(
            default_effort("openrouter/deepseek-v4-flash"),
            Some("medium")
        );
        assert_eq!(default_effort("heterion-local/grok-4.6"), Some("medium"));
    }

    #[test]
    fn matching_is_case_insensitive() {
        assert_eq!(default_effort("GROK-4.6"), Some("medium"));
        assert_eq!(default_effort("Claude-Opus-4"), Some("high"));
    }

    #[test]
    fn unevidenced_families_stay_on_the_provider_default() {
        assert_eq!(default_effort("gpt-5.6-sol"), None);
        assert_eq!(default_effort("claude-haiku-4"), None);
        assert_eq!(default_effort("mistral-large"), None);
        assert_eq!(default_effort("exa-search"), None);
        assert_eq!(default_effort(""), None);
    }
}

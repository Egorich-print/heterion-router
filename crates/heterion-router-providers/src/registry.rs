//! Static provider registry derived from the JS `REGISTRY`.

use std::collections::HashMap;

use heterion_router_core::WireFormat;
use serde::{Deserialize, Serialize};

/// One model entry. `target_format` is `None` when the model inherits the
/// provider's `format` (the same fallback `getTargetFormat` applies in JS).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegistryModel {
    pub id: String,
    pub target_format: Option<String>,
}

/// One provider entry. Function fields of the JS `RegistryEntry`
/// (`urlBuilder`, header profiles) are intentionally not carried here — they
/// are ported as code per executor, not as data.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderEntry {
    pub id: String,
    pub alias: Option<String>,
    pub format: Option<String>,
    pub executor: Option<String>,
    pub auth_type: Option<String>,
    /// Upstream chat-completions URL, when the provider declares one.
    pub base_url: Option<String>,
    /// Whether only the models the platform prices at zero are usable.
    ///
    /// An anonymous area (OpenCode Zen's `public` access) serves the free
    /// models and refuses the rest with `401 Missing API key` (probed live
    /// 2026-11-20). For such a provider the measured catalogue keeps only the
    /// zero-priced models, so the ladder does not offer models that cannot
    /// answer.
    #[serde(default)]
    pub free_only: Option<bool>,
    #[serde(default)]
    pub models: Vec<RegistryModel>,
}

/// Map a registry format string to the core wire format.
///
/// Returns `None` for formats the core does not model yet (`clova`, `codex`,
/// `kiro`, `cursor`, …) — callers must handle those explicitly rather than
/// guessing.
pub fn parse_wire_format(value: &str) -> Option<WireFormat> {
    match value {
        "openai" => Some(WireFormat::OpenAi),
        "openai-responses" => Some(WireFormat::OpenAiResponses),
        "claude" => Some(WireFormat::Claude),
        "gemini" => Some(WireFormat::Gemini),
        "antigravity" => Some(WireFormat::Antigravity),
        _ => None,
    }
}

/// The embedded provider registry.
#[derive(Debug, Clone)]
pub struct ProviderRegistry {
    entries: Vec<ProviderEntry>,
    by_id: HashMap<String, usize>,
}

impl ProviderRegistry {
    /// Load the registry embedded at compile time.
    ///
    /// # Panics
    ///
    /// Panics if the embedded `registry.json` is malformed. The file is
    /// generated from the JS source and validated by the contract tests, so a
    /// malformed file is a build-time bug, not a runtime condition.
    pub fn load() -> Self {
        let entries: Vec<ProviderEntry> =
            serde_json::from_str(include_str!("../registry.json")).expect("registry.json valid");
        Self::from_entries(entries)
    }

    fn from_entries(entries: Vec<ProviderEntry>) -> Self {
        let mut by_id: HashMap<String, usize> = entries
            .iter()
            .enumerate()
            .map(|(index, entry)| (entry.id.clone(), index))
            .collect();
        // Stored combos and clients address some providers by alias
        // (`ds/…`, `pol/…`, `kg/…`), so aliases resolve too. A real id always
        // wins over an alias that collides with one.
        for (index, entry) in entries.iter().enumerate() {
            if let Some(alias) = entry.alias.as_deref().filter(|alias| !alias.is_empty()) {
                by_id.entry(alias.to_string()).or_insert(index);
            }
        }
        Self { entries, by_id }
    }

    /// Number of registered providers.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the registry is empty.
    ///
    /// Load-bearing API, not dead code: `clippy::len_without_is_empty` rejects
    /// [`Self::len`] without it, so removing this fails the build.
    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Iterate over all entries in registry order.
    pub fn iter(&self) -> impl Iterator<Item = &ProviderEntry> {
        self.entries.iter()
    }

    /// Iterate over all provider ids in registry order.
    pub fn ids(&self) -> impl Iterator<Item = &str> {
        self.entries.iter().map(|entry| entry.id.as_str())
    }

    /// Look up a provider by id.
    pub fn get(&self, id: &str) -> Option<&ProviderEntry> {
        self.by_id
            .get(id)
            .and_then(|index| self.entries.get(*index))
    }

    /// Canonical id for a provider id or alias.
    ///
    /// Backends are keyed by canonical id, so any provider reference coming
    /// from a combo or a `provider/model` request has to be resolved first.
    pub fn canonical_id(&self, provider: &str) -> Option<&str> {
        self.get(provider).map(|entry| entry.id.as_str())
    }

    /// Whether the provider is usable without a credential.
    ///
    /// `auth_type: optional` providers (public gateways) answer without a
    /// key; requiring one would silently drop them from the backend set.
    pub fn allows_keyless(&self, provider: &str) -> bool {
        self.get(provider)
            .and_then(|entry| entry.auth_type.as_deref())
            .is_some_and(|auth_type| matches!(auth_type, "optional" | "none"))
    }

    /// Whether only the free models of this platform are usable.
    ///
    /// An anonymous area serves the models it prices at zero and refuses the
    /// rest, so its measured catalogue keeps only those.
    pub fn is_free_only(&self, provider: &str) -> bool {
        self.get(provider)
            .and_then(|entry| entry.free_only)
            .unwrap_or(false)
    }

    /// Effective target format for a model: the model's explicit
    /// `target_format`, falling back to the provider `format`.
    pub fn model_target_format(&self, provider_id: &str, model_id: &str) -> Option<&str> {
        let provider = self.get(provider_id)?;
        let model = provider.models.iter().find(|m| m.id == model_id)?;
        model
            .target_format
            .as_deref()
            .or(provider.format.as_deref())
    }

    /// The effective target format as a core wire format, if it is modelled.
    pub fn model_wire_format(&self, provider_id: &str, model_id: &str) -> Option<WireFormat> {
        self.model_target_format(provider_id, model_id)
            .and_then(parse_wire_format)
    }

    /// Whether a provider speaks the OpenAI wire format.
    pub fn is_openai_format(&self, provider_id: &str) -> bool {
        self.get(provider_id)
            .and_then(|provider| provider.format.as_deref())
            == Some("openai")
    }

    /// Upstream chat URL for a provider, if declared.
    pub fn base_url(&self, provider_id: &str) -> Option<&str> {
        self.get(provider_id)
            .and_then(|provider| provider.base_url.as_deref())
            .filter(|url| !url.is_empty())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn registry() -> ProviderRegistry {
        ProviderRegistry::load()
    }

    #[test]
    fn aliases_resolve_to_the_provider_entry() {
        let registry = registry();
        for (alias, id) in [
            ("ds", "deepseek"),
            ("pol", "pollinations"),
            ("kg", "kilo-gateway"),
            // Clients address Google's models as `google/…` (probed live
            // 2026-11-16: `google/gemini-3.6-flash` answered 404 while
            // `gemini/gemini-3.6-flash` answered 200).
            ("google", "gemini"),
        ] {
            let entry = registry
                .get(alias)
                .unwrap_or_else(|| panic!("alias {alias} did not resolve"));
            assert_eq!(entry.id, id, "alias {alias} resolved to {}", entry.id);
            assert_eq!(registry.canonical_id(alias), Some(id));
            // Everything that reads the entry works through the alias too.
            assert!(
                registry.base_url(alias).is_some(),
                "{alias} has no base_url"
            );
            assert_eq!(
                registry.is_openai_format(alias),
                registry.is_openai_format(id),
                "{alias} format diverged from {id}"
            );
        }
    }

    #[test]
    fn a_real_id_wins_over_a_colliding_alias() {
        let entries = vec![
            ProviderEntry {
                id: "alpha".to_string(),
                alias: Some("beta".to_string()),
                format: Some("openai".to_string()),
                executor: Some("default".to_string()),
                auth_type: Some("apikey".to_string()),
                base_url: Some("https://alpha.example/v1".to_string()),
                free_only: None,
                models: Vec::new(),
            },
            ProviderEntry {
                id: "beta".to_string(),
                alias: Some("gamma".to_string()),
                format: Some("openai".to_string()),
                executor: Some("default".to_string()),
                auth_type: Some("apikey".to_string()),
                base_url: Some("https://beta.example/v1".to_string()),
                free_only: None,
                models: Vec::new(),
            },
        ];
        let registry = ProviderRegistry::from_entries(entries);
        assert_eq!(registry.canonical_id("beta"), Some("beta"));
        assert_eq!(registry.base_url("beta"), Some("https://beta.example/v1"));
        assert_eq!(registry.canonical_id("gamma"), Some("beta"));
    }

    #[test]
    fn keyless_is_reported_for_optional_auth() {
        let registry = registry();
        assert!(registry.allows_keyless("pollinations"));
        assert!(!registry.allows_keyless("openrouter"));
        assert!(!registry.allows_keyless("nope"));
    }

    #[test]
    fn heterion_local_is_a_keyless_openai_sibling() {
        let registry = registry();
        let entry = registry
            .get("heterion-local")
            .expect("heterion-local registered");
        assert_eq!(entry.format.as_deref(), Some("openai"));
        assert_eq!(entry.executor.as_deref(), Some("default"));
        assert!(registry.allows_keyless("heterion-local"));
        assert!(registry.is_openai_format("heterion-local"));
        assert_eq!(registry.canonical_id("heterion"), Some("heterion-local"));
        assert_eq!(
            registry.base_url("heterion-local"),
            Some("http://127.0.0.1:8080/v1")
        );
    }

    #[test]
    fn registry_loads_and_is_nontrivial() {
        let registry = registry();
        assert!(registry.len() >= 200, "len = {}", registry.len());
    }

    #[test]
    fn provider_ids_are_unique() {
        let registry = registry();
        let mut seen = HashSet::new();
        for id in registry.ids() {
            assert!(seen.insert(id), "duplicate provider id: {id}");
        }
    }

    #[test]
    fn every_provider_has_executor_and_format() {
        for entry in registry().iter() {
            assert!(
                entry.executor.as_deref().is_some_and(|s| !s.is_empty()),
                "missing executor: {}",
                entry.id
            );
            assert!(
                entry.format.as_deref().is_some_and(|s| !s.is_empty()),
                "missing format: {}",
                entry.id
            );
        }
    }

    #[test]
    fn every_model_has_an_id() {
        for entry in registry().iter() {
            for model in &entry.models {
                assert!(!model.id.is_empty(), "empty model id under {}", entry.id);
            }
        }
    }

    #[test]
    fn grok_cli_model_uses_responses_format() {
        let registry = registry();
        assert_eq!(
            registry.model_target_format("grok-cli", "grok-4.6"),
            Some("openai-responses")
        );
        assert_eq!(
            registry.model_wire_format("grok-cli", "grok-4.6"),
            Some(WireFormat::OpenAiResponses)
        );
    }

    #[test]
    fn xai_reasoning_models_use_responses_format() {
        let registry = registry();
        assert_eq!(
            registry.model_wire_format("xai", "grok-4.7"),
            Some(WireFormat::OpenAiResponses)
        );
        assert_eq!(
            registry.model_wire_format("xai-oauth", "grok-4.7"),
            Some(WireFormat::OpenAiResponses)
        );
    }

    #[test]
    fn model_without_explicit_target_inherits_provider_format() {
        let registry = registry();
        assert_eq!(
            registry.model_target_format("adapta-web", "adapta-one"),
            Some("openai")
        );
    }

    #[test]
    fn unknown_provider_or_model_returns_none() {
        let registry = registry();
        assert_eq!(registry.model_target_format("nope", "x"), None);
        assert_eq!(registry.model_target_format("grok-cli", "nope"), None);
    }

    #[test]
    fn unmodelled_formats_return_none() {
        assert_eq!(parse_wire_format("clova"), None);
        assert_eq!(parse_wire_format("kiro"), None);
        assert_eq!(parse_wire_format(""), None);
    }

    /// The anonymous Zen pool is a *separate* provider entry.
    ///
    /// The platform serves it without a key — the opencode client sends the
    /// literal `public` — but only the free models: a paid model answers
    /// `401 Missing API key` (probed live 2026-11-20, 12 of 103 models cost
    /// zero). Keeping the free models in their own entry means the paid Zen
    /// models stay reachable for a connection that has a real key.
    #[test]
    fn the_anonymous_zen_pool_is_keyless_and_free_only() {
        let registry = registry();
        let entry = registry
            .get("opencode-free")
            .expect("opencode-free registered");
        assert_eq!(entry.format.as_deref(), Some("openai"));
        assert_eq!(entry.auth_type.as_deref(), Some("optional"));
        assert!(registry.allows_keyless("opencode-free"));
        assert!(
            registry.is_free_only("opencode-free"),
            "the measured catalogue of an anonymous area keeps only the free models"
        );
        assert!(!registry.is_free_only("opencode"));
        assert!(!registry.is_free_only("nope"));
        assert_eq!(
            registry.base_url("opencode-free"),
            Some("https://opencode.ai/inference/openai/v1")
        );
        assert_eq!(
            entry.models.len(),
            12,
            "the free models measured on 2026-11-20"
        );
        for model in &entry.models {
            assert!(
                model.id == "big-pickle" || model.id.ends_with("-free"),
                "a paid model would answer 401 Missing API key: {}",
                model.id
            );
        }
    }

    #[test]
    fn provider_id_list_snapshot() {
        let registry = registry();
        let ids: Vec<&str> = registry.ids().collect();
        insta::assert_debug_snapshot!(ids);
    }
}

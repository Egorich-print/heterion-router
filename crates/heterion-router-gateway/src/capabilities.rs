//! Per-model capabilities — what a model can do, not just what it is called.
//!
//! The registry knows *which* models a platform serves; the capability table
//! knows *what each of them can do*: how much context it takes, whether it
//! sees images, whether it has a native tool head, whether it reasons — and
//! which class of intelligence it belongs to (`frontier` / `fast` /
//! `fallback`). That class is the one thing a platform cannot tell us, so it
//! lives in `model_capability_overrides` under the `tier` key.
//!
//! Two sources are merged, in this order:
//!
//! 1. `model_capabilities` — declared capabilities (from the platform's
//!    catalogue or from research);
//! 2. `model_context_overrides` — the *measured* context window, which wins
//!    over any declared number: a platform that answers "maximum context
//!    length is 131072" knows better than a catalogue.
//!
//! The intrinsic priority a capability set yields never changes. What changes
//! at runtime — an unstable model, an exhausted quota — is the router's
//! dynamic part (quality score, cooldown), so a model that misbehaves sinks
//! for a while and comes back with its properties intact.

use std::collections::HashMap;

use heterion_router_db::repos::ModelCapabilityRow;

use crate::model_spec::{ModelTier, ToolSupport};

/// What one model can do.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ModelCapabilities {
    /// Context window in tokens.
    pub context_window: u32,
    /// Whether the model takes images.
    pub supports_vision: bool,
    /// Whether the model has a native tool head.
    pub tool_support: ToolSupport,
    /// Whether the model has a reasoning/thinking mode.
    pub reasoning: bool,
    /// Class of intelligence: the ladder's coarse rung.
    pub tier: ModelTier,
}

/// Capabilities of every model the router knows about.
///
/// Keyed by `provider/model`, because the same model served by two platforms
/// can differ (a gateway may cap the context it forwards).
#[derive(Debug, Clone, Default)]
pub struct CapabilityStore {
    rows: HashMap<String, ModelCapabilities>,
}

impl CapabilityStore {
    /// An empty store: every model falls back to the static spec.
    pub fn empty() -> Self {
        Self::default()
    }

    /// Load the capabilities from the database.
    ///
    /// A store that cannot be read is an empty store: the router then behaves
    /// exactly as before this table existed.
    pub fn load(conn: &rusqlite::Connection) -> Self {
        let rows = heterion_router_db::repos::list_model_capabilities(conn);
        Self::from_rows(&rows)
    }

    /// Build a store from already-read rows.
    pub fn from_rows(rows: &[ModelCapabilityRow]) -> Self {
        let mut map = HashMap::with_capacity(rows.len());
        for row in rows {
            let capabilities = ModelCapabilities {
                context_window: row
                    .real_context
                    .or(row.limit_context)
                    .filter(|value| *value > 0)
                    .unwrap_or(0) as u32,
                supports_vision: row.attachment.unwrap_or(false),
                tool_support: match row.tool_call {
                    Some(true) => ToolSupport::Native,
                    Some(false) => ToolSupport::None,
                    // Unknown: a model with no declaration is assumed to have
                    // a native tool head only when the platform says so —
                    // an absent answer must not disable tools for everyone.
                    None => ToolSupport::Native,
                },
                reasoning: row.reasoning.unwrap_or(false),
                tier: parse_tier(row.tier.as_deref()),
            };
            map.insert(row.key(), capabilities);
        }
        Self { rows: map }
    }

    /// Capabilities of one model, if the table knows it.
    pub fn get(&self, provider: &str, model: &str) -> Option<ModelCapabilities> {
        self.rows.get(&format!("{provider}/{model}")).copied()
    }

    /// Number of models the store knows.
    pub fn len(&self) -> usize {
        self.rows.len()
    }

    /// Whether the store knows no model at all.
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }
}

/// Ladder class of a model, as stored in `model_capability_overrides`.
///
/// An unknown or missing class is a `fallback`: a model nobody researched
/// must not outrank a model somebody did.
fn parse_tier(value: Option<&str>) -> ModelTier {
    match value {
        Some("frontier") => ModelTier::FrontierReasoning,
        Some("fast") => ModelTier::FastExecution,
        _ => ModelTier::Fallback,
    }
}

/// Priority of a model in the unified ladder, derived from its capabilities.
///
/// The formula is monotone on purpose: a model is worth more the stronger its
/// class, the larger its context, and the more capabilities it brings. The
/// numbers are a scale, not a ranking — the router sorts by them.
///
/// | ingredient            | points |
/// |-----------------------|--------|
/// | class `frontier`      | 240    |
/// | class `fast`          | 140    |
/// | class `fallback`      | 60     |
/// | context               | 0..60  |
/// | native tool head      | 25     |
/// | vision                | 15     |
/// | reasoning             | 20     |
pub fn ladder_priority(capabilities: &ModelCapabilities) -> f64 {
    let tier = match capabilities.tier {
        ModelTier::FrontierReasoning => 240.0,
        ModelTier::FastExecution => 140.0,
        ModelTier::Fallback => 60.0,
    };
    tier + context_bonus(capabilities.context_window)
        + if capabilities.tool_support == ToolSupport::Native {
            25.0
        } else {
            0.0
        }
        + if capabilities.supports_vision {
            15.0
        } else {
            0.0
        }
        + if capabilities.reasoning { 20.0 } else { 0.0 }
}

/// Context bonus: nothing up to 32k, then 20 points per doubling, capped at 60.
///
/// Logarithmic on purpose: the difference between 32k and 64k matters more
/// than the difference between 512k and 1M, and the cap keeps a huge window
/// from burying every other property.
fn context_bonus(context_window: u32) -> f64 {
    if context_window <= 32_768 {
        return 0.0;
    }
    let doublings = (context_window as f64 / 32_768.0).log2();
    (20.0 * doublings).min(60.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn caps(
        context_window: u32,
        supports_vision: bool,
        tool_support: ToolSupport,
        reasoning: bool,
        tier: ModelTier,
    ) -> ModelCapabilities {
        ModelCapabilities {
            context_window,
            supports_vision,
            tool_support,
            reasoning,
            tier,
        }
    }

    #[test]
    fn a_frontier_model_outranks_a_fast_one_with_the_same_properties() {
        let frontier = ladder_priority(&caps(
            512_000,
            false,
            ToolSupport::Native,
            true,
            ModelTier::FrontierReasoning,
        ));
        let fast = ladder_priority(&caps(
            512_000,
            false,
            ToolSupport::Native,
            true,
            ModelTier::FastExecution,
        ));
        assert!(frontier > fast);
    }

    #[test]
    fn capabilities_add_up_and_never_subtract() {
        let bare = ladder_priority(&caps(
            32_768,
            false,
            ToolSupport::None,
            false,
            ModelTier::Fallback,
        ));
        assert_eq!(bare, 60.0);
        let full = ladder_priority(&caps(
            1_000_000,
            true,
            ToolSupport::Native,
            true,
            ModelTier::FrontierReasoning,
        ));
        assert_eq!(full, 360.0);
    }

    #[test]
    fn the_context_bonus_is_capped() {
        // 1M and 8M windows must not differ: the cap keeps a huge window from
        // burying every other property.
        assert_eq!(context_bonus(1_000_000), 60.0);
        assert_eq!(context_bonus(8_000_000), 60.0);
        assert_eq!(context_bonus(131_072), 40.0);
        assert_eq!(context_bonus(32_768), 0.0);
    }

    #[test]
    fn a_measured_context_wins_over_the_declared_one() {
        let rows = vec![ModelCapabilityRow {
            provider: "gemini".to_string(),
            model_id: "gemini-3-flash-preview".to_string(),
            tool_call: Some(true),
            reasoning: Some(true),
            attachment: Some(true),
            limit_context: Some(128_000),
            limit_output: None,
            family: None,
            real_context: Some(1_048_576),
            tier: Some("fast".to_string()),
        }];
        let store = CapabilityStore::from_rows(&rows);
        let capabilities = store
            .get("gemini", "gemini-3-flash-preview")
            .expect("capabilities");
        assert_eq!(capabilities.context_window, 1_048_576);
        assert!(capabilities.supports_vision);
        assert_eq!(capabilities.tool_support, ToolSupport::Native);
        assert!(capabilities.reasoning);
        assert_eq!(capabilities.tier, ModelTier::FastExecution);
    }

    #[test]
    fn an_unknown_model_has_no_capabilities() {
        let store = CapabilityStore::empty();
        assert!(store.is_empty());
        assert!(store.get("gemini", "gemini-3-flash-preview").is_none());
    }
}

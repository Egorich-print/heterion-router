//! Trust — which models the router has actually talked to.
//!
//! The capability table says what a model *is*: how much context it takes,
//! whether it sees images, whether it reasons. It says nothing about whether
//! *this* provider serves that id: a catalogue lists plenty of models whose
//! endpoint answers `404` (or whose key answers `401`).
//!
//! So the ladder needs a second, empirical input: **trust**. A model is
//! trusted when the router has evidence that it answered us:
//!
//! 1. `call_logs` with `status = 200` — we got a good answer from exactly
//!    this provider/model pair;
//! 2. `model_context_overrides` with a measured `real_context` — the platform
//!    confirmed the model's window during a probe, which only happens for a
//!    model it really serves;
//! 3. the hand-verified curated ladder (`DEFAULT_AUTO_CANDIDATES`).
//!
//! A trusted model keeps its full capability priority. An untrusted one —
//! research says it is a frontier model, but nobody has ever talked to it —
//! is discounted, so it cannot push a model we *know* works out of the head
//! of the ladder. It stays in the pool: the first successful call makes it
//! trusted, and from then on it competes on its merits.
//!
//! Trust is not a verdict: it is a prior that the live traffic corrects.

use dashmap::DashSet;

/// The discount an untrusted model's priority is multiplied by.
///
/// A tenth: enough to put an unverified model below every model the router
/// has evidence for — the plain tail sits at 50, so an unverified research
/// model (360 points before the discount) lands at 36. The model stays in the
/// pool and climbs out of the discount as soon as it answers once.
pub const UNTRUSTED_DISCOUNT: f64 = 0.1;

/// Models the router has evidence for.
#[derive(Debug, Clone, Default)]
pub struct TrustStore {
    trusted: DashSet<String>,
}

impl TrustStore {
    /// An empty store: nothing is trusted yet.
    pub fn empty() -> Self {
        Self::default()
    }

    /// Load the trust evidence from the database.
    ///
    /// Two sources, both "the platform answered us":
    ///
    /// - `call_logs` with `status = 200`;
    /// - `model_context_overrides` with a measured context window.
    ///
    /// The `model` column of `call_logs` sometimes holds a full reference
    /// (`agentrouter/gpt-5.6-sol`) instead of a bare id, so the provider
    /// prefix is stripped before the pair is recorded.
    pub fn load(conn: &rusqlite::Connection) -> Self {
        let store = Self::empty();
        for (provider, model) in pairs(
            conn,
            "SELECT DISTINCT provider, model FROM call_logs WHERE status = 200 AND model IS NOT NULL",
        ) {
            store.mark_trusted(&provider, &model);
        }
        for (provider, model) in pairs(
            conn,
            "SELECT provider, model_id FROM model_context_overrides WHERE real_context > 0",
        ) {
            store.mark_trusted(&provider, &model);
        }
        store
    }

    /// Build a store from explicit pairs.
    pub fn from_pairs<I>(pairs: I) -> Self
    where
        I: IntoIterator<Item = (String, String)>,
    {
        let store = Self::empty();
        for (provider, model) in pairs {
            store.mark_trusted(&provider, &model);
        }
        store
    }

    /// Record that a model answered us: it is trusted from now on.
    pub fn mark_trusted(&self, provider: &str, model: &str) {
        self.trusted.insert(key(provider, model));
    }

    /// Whether the router has evidence that this provider serves this model.
    pub fn is_trusted(&self, provider: &str, model: &str) -> bool {
        self.trusted.contains(&key(provider, model))
    }

    /// How many models are trusted.
    pub fn len(&self) -> usize {
        self.trusted.len()
    }

    /// Whether nothing is trusted.
    pub fn is_empty(&self) -> bool {
        self.trusted.is_empty()
    }
}

/// The pairs a query returns, or nothing when the table is not there.
fn pairs(conn: &rusqlite::Connection, sql: &str) -> Vec<(String, String)> {
    let Ok(mut rows) = conn.prepare(sql) else {
        return Vec::new();
    };
    let Ok(mapped) = rows.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    }) else {
        return Vec::new();
    };
    mapped.flatten().collect()
}

/// The key of a provider/model pair.///
/// The model may arrive with its provider prefix (`agentrouter/gpt-5.6-sol`);
/// the prefix is stripped so that the pair matches the registry's bare id.
fn key(provider: &str, model: &str) -> String {
    let bare = model
        .strip_prefix(provider)
        .and_then(|rest| rest.strip_prefix('/'));
    match bare {
        Some(bare) => format!("{provider}/{bare}"),
        None => format!("{provider}/{model}"),
    }
}

/// A store that trusts nothing — for tests and for a router without a
/// database. With no trust evidence at all, no model is discounted.
pub fn empty_store() -> TrustStore {
    TrustStore::empty()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pair_is_trusted_after_being_marked() {
        let store = TrustStore::empty();
        assert!(!store.is_trusted("gemini", "gemini-3-flash-preview"));
        store.mark_trusted("gemini", "gemini-3-flash-preview");
        assert!(store.is_trusted("gemini", "gemini-3-flash-preview"));
        assert!(!store.is_trusted("gemini", "gemma-4-26b-a4b-it"));
        assert_eq!(store.len(), 1);
    }

    #[test]
    fn a_full_reference_is_the_same_pair_as_a_bare_id() {
        let store = TrustStore::from_pairs([(
            "agentrouter".to_string(),
            "agentrouter/gpt-5.6-sol".to_string(),
        )]);
        assert!(
            store.is_trusted("agentrouter", "gpt-5.6-sol"),
            "the provider prefix must be stripped"
        );
    }

    #[test]
    fn a_model_id_with_a_slash_keeps_its_own_path() {
        let store = TrustStore::from_pairs([(
            "siliconflow".to_string(),
            "Qwen/Qwen3.5-397B-A17B".to_string(),
        )]);
        assert!(store.is_trusted("siliconflow", "Qwen/Qwen3.5-397B-A17B"));
        assert!(!store.is_trusted("siliconflow", "Qwen3.5-397B-A17B"));
    }
}

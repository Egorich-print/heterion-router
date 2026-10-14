//! Model pricing in USD per 1M tokens.
//!
//! Resolution order mirrors the JS server: user overrides stored under the
//! `pricing` `key_value` namespace win, then the built-in seed below. Models
//! missing from both resolve to `None` — unknown cost is reported as unknown,
//! never fabricated.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// Per-1M-token prices for one model.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ModelPrice {
    pub input: f64,
    pub output: f64,
    #[serde(default)]
    pub cached: f64,
    #[serde(default)]
    pub reasoning: f64,
    #[serde(default)]
    pub cache_creation: f64,
}

impl ModelPrice {
    /// Published cache-read rate, else the input rate.
    ///
    /// `cached` is `#[serde(default)]`, so a price row that only publishes
    /// input/output leaves it at zero — and zero has to mean "unpublished",
    /// never "free". Falling back to `input` keeps a cache bill at or below
    /// the fresh-input bill instead of silently giving the tokens away.
    fn cache_read_rate(&self) -> f64 {
        if self.cached > 0.0 {
            self.cached
        } else {
            self.input
        }
    }

    /// Published cache-write rate, else the input rate. Providers that bill
    /// a write at a premium publish a higher number than `input`; one that
    /// publishes none is billed as ordinary input.
    fn cache_creation_rate(&self) -> f64 {
        if self.cache_creation > 0.0 {
            self.cache_creation
        } else {
            self.input
        }
    }

    /// Reasoning premium over the output rate.
    ///
    /// Reasoning tokens are counted *inside* `output`, so only the
    /// difference between the two rates is additive; the clamp keeps the
    /// premium non-negative, and an unpublished reasoning rate (zero) leaves
    /// those tokens billed as output rather than as a rebate.
    fn reasoning_premium(&self) -> f64 {
        let rate = if self.reasoning > 0.0 {
            self.reasoning
        } else {
            self.output
        };
        (rate - self.output).max(0.0)
    }
}

/// Price catalog: built-in seed plus user overrides.
#[derive(Debug, Clone, Default)]
pub struct PriceTable {
    prices: HashMap<String, ModelPrice>,
}

impl PriceTable {
    /// Seed with the reference models (values verbatim from the JS catalog).
    pub fn with_defaults() -> Self {
        let mut table = Self::default();
        // xAI (frontier-labs.ts / inference-hosts.ts).
        table.insert(
            "grok-4.6",
            ModelPrice {
                input: 2.0,
                output: 6.0,
                cached: 0.5,
                reasoning: 6.0,
                cache_creation: 2.0,
            },
        );
        table.insert(
            "grok-4.5",
            ModelPrice {
                input: 1.4,
                output: 4.2,
                cached: 0.35,
                reasoning: 4.2,
                cache_creation: 1.4,
            },
        );
        // OpenAI (frontier-labs.ts).
        table.insert(
            "gpt-4o-mini",
            ModelPrice {
                input: 0.15,
                output: 0.6,
                cached: 0.075,
                reasoning: 0.9,
                cache_creation: 0.15,
            },
        );
        // Anthropic (frontier-labs.ts).
        table.insert(
            "claude-sonnet-4.6",
            ModelPrice {
                input: 3.0,
                output: 15.0,
                cached: 1.5,
                reasoning: 22.5,
                cache_creation: 3.0,
            },
        );
        // Google (frontier-labs.ts).
        table.insert(
            "gemini-2.5-flash",
            ModelPrice {
                input: 0.3,
                output: 2.5,
                cached: 0.03,
                reasoning: 3.75,
                cache_creation: 0.3,
            },
        );
        table
    }

    fn insert(&mut self, model: &str, price: ModelPrice) {
        self.prices.insert(model.to_string(), price);
    }

    /// Apply a user override (wins over the seed).
    pub fn set_override(&mut self, model: &str, price: ModelPrice) {
        self.insert(model, price);
    }

    /// Look up the price for a model.
    pub fn price_for(&self, model: &str) -> Option<&ModelPrice> {
        self.prices.get(model)
    }

    /// Cost in USD for one request, mirroring `costCalculator.ts`.
    ///
    /// `input` covers the whole prompt — providers count cached input
    /// *inside* `prompt_tokens` — so the cached share is moved to its own
    /// rate, reasoning pays the premium over the output rate, and a cache
    /// write pays its own rate.
    ///
    /// Discount policy: only a rate the price table actually publishes is
    /// applied ([`ModelPrice::cache_read_rate`], [`ModelPrice::
    /// cache_creation_rate`], [`ModelPrice::reasoning_premium`]). A model
    /// that publishes none is billed as if every token were fresh, so the
    /// three counts being zero — every row written before the port, and
    /// every provider that never reports them — prices to the same `f64`
    /// it always did, and a published discount can only ever lower a bill.
    ///
    /// Precision: no rounding here. Per-token rates are summed in `f64` and
    /// divided by 1e6 once; the only rounding is the caller's `{:.6}`
    /// header format.
    ///
    /// Returns `None` for unpriced models.
    pub fn cost_usd(
        &self,
        model: &str,
        input: u32,
        output: u32,
        cached: u32,
        reasoning: u32,
        cache_creation: u32,
    ) -> Option<f64> {
        let price = self.price_for(model)?;
        let non_cached = input.saturating_sub(cached) as f64;
        let total = non_cached * price.input
            + cached as f64 * price.cache_read_rate()
            + output as f64 * price.output
            + reasoning as f64 * price.reasoning_premium()
            + cache_creation as f64 * price.cache_creation_rate();
        Some(total / 1_000_000.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider_usage::CachedUsage;
    use serde_json::json;

    /// A price row with only input/output published: the three detail
    /// fields stay at their `serde(default)` zero.
    fn no_cache_price() -> ModelPrice {
        ModelPrice {
            input: 2.0,
            output: 6.0,
            cached: 0.0,
            reasoning: 0.0,
            cache_creation: 0.0,
        }
    }

    #[test]
    fn grok_pricing_matches_js_catalog() {
        let table = PriceTable::with_defaults();
        let price = table.price_for("grok-4.6").unwrap();
        assert_eq!(price.input, 2.0);
        assert_eq!(price.output, 6.0);
    }

    #[test]
    fn cost_formula_matches_js() {
        let table = PriceTable::with_defaults();
        // 1000 input (200 cached), 500 output, 100 reasoning.
        let cost = table.cost_usd("grok-4.6", 1000, 500, 200, 100, 0).unwrap();
        let expected = (800.0 * 2.0 + 200.0 * 0.5 + 500.0 * 6.0 + 100.0 * (6.0 - 6.0)) / 1e6;
        assert!((cost - expected).abs() < 1e-12, "{cost} vs {expected}");
    }

    #[test]
    fn unknown_model_has_no_price() {
        let table = PriceTable::with_defaults();
        assert_eq!(table.price_for("nope-1"), None);
        assert_eq!(table.cost_usd("nope-1", 1, 1, 0, 0, 0), None);
    }

    /// A provider that publishes no cache price must keep billing cached
    /// input as ordinary input. Zero means "unpublished", not "free".
    #[test]
    fn unpublished_cache_price_falls_back_to_the_input_rate() {
        let mut table = PriceTable::with_defaults();
        table.set_override("no-cache-price", no_cache_price());
        // 1000 input of which 200 were served from cache: 1000 * 2.0.
        let cost = table
            .cost_usd("no-cache-price", 1000, 0, 200, 0, 0)
            .unwrap();
        assert!(
            (cost - 0.002).abs() < 1e-12,
            "cached input billed {cost}, want 0.002"
        );
    }

    /// Same rule for a cache write: unpublished is not a giveaway.
    #[test]
    fn unpublished_cache_creation_price_falls_back_to_the_input_rate() {
        let mut table = PriceTable::with_defaults();
        table.set_override("no-cache-price", no_cache_price());
        let cost = table.cost_usd("no-cache-price", 0, 0, 0, 0, 300).unwrap();
        assert!(
            (cost - 0.0006).abs() < 1e-12,
            "cache write billed {cost}, want 0.0006"
        );
    }

    /// An unpublished reasoning rate must not turn reasoning tokens into a
    /// rebate: the premium over the output rate is the only addable part.
    #[test]
    fn unpublished_reasoning_price_never_subtracts_from_the_cost() {
        let mut table = PriceTable::with_defaults();
        table.set_override("no-cache-price", no_cache_price());
        let cost = table.cost_usd("no-cache-price", 0, 500, 0, 100, 0).unwrap();
        assert!(
            (cost - 0.003).abs() < 1e-12,
            "reasoning billed {cost}, want 0.003"
        );
    }

    /// A published cache rate is a discount on the cached share only.
    #[test]
    fn published_cache_price_is_applied_to_the_cached_share() {
        let table = PriceTable::with_defaults();
        // grok-4.6: input 2.0, cached 0.5 → 800*2.0 + 200*0.5 = 1700.
        let cost = table.cost_usd("grok-4.6", 1000, 0, 200, 0, 0).unwrap();
        assert!((cost - 0.0017).abs() < 1e-12, "got {cost}, want 0.0017");
    }

    /// The common path is untouched: no cache detail, no discount, and the
    /// same `f64` the header was formatted from before.
    #[test]
    fn a_request_without_cache_detail_prices_exactly_as_before() {
        let table = PriceTable::with_defaults();
        let cost = table.cost_usd("grok-4.6", 1000, 500, 0, 0, 0).unwrap();
        assert_eq!(cost, 5000.0 / 1_000_000.0);
    }

    /// One table over the four upstream spellings: whatever the provider
    /// sent, a published cache rate discounts the bill, and a model with no
    /// cache price published prices it exactly as the zeros did.
    #[test]
    fn each_upstream_spelling_prices_through_the_discount_policy() {
        let payloads = [
            (
                "chat completions",
                json!({"usage": {"prompt_tokens_details": {"cached_tokens": 200}}}),
            ),
            (
                "responses",
                json!({"usage": {"input_tokens_details": {"cached_tokens": 200}}}),
            ),
            (
                "anthropic messages",
                json!({"usage": {"cache_read_input_tokens": 200}}),
            ),
            (
                "gemini",
                json!({"usageMetadata": {"cachedContentTokenCount": 200}}),
            ),
        ];

        for (family, payload) in payloads {
            let counts = CachedUsage::from_payload(&payload);
            assert_eq!(counts.cache_read, 200, "{family} spelling");

            // grok-4.6 publishes cached 0.5 against input 2.0:
            // 800*2.0 + 200*0.5 + 500*6.0 = 4700 → 0.0047.
            let discounted = PriceTable::with_defaults()
                .cost_usd(
                    "grok-4.6",
                    1000,
                    500,
                    counts.cache_read,
                    counts.reasoning,
                    counts.cache_creation,
                )
                .unwrap();
            assert!(
                (discounted - 0.0047).abs() < 1e-12,
                "{family}: got {discounted}, want the discounted 0.0047"
            );

            // No published cache price: identical to the pre-fix zeros.
            let mut table = PriceTable::with_defaults();
            table.set_override("no-cache-price", no_cache_price());
            let as_zeros = table
                .cost_usd("no-cache-price", 1000, 500, 0, 0, 0)
                .unwrap();
            let from_payload = table
                .cost_usd(
                    "no-cache-price",
                    1000,
                    500,
                    counts.cache_read,
                    counts.reasoning,
                    counts.cache_creation,
                )
                .unwrap();
            assert_eq!(
                from_payload, as_zeros,
                "{family}: a model without cache pricing must not move"
            );
        }
    }

    /// The 18k legacy rows: all three counts zero, priced as they always
    /// were. `cost_usd` is read live, so this is the compatibility promise.
    #[test]
    fn legacy_zero_rows_price_as_they_did() {
        let table = PriceTable::with_defaults();
        for (model, input, output) in [
            ("grok-4.6", 1000, 500),
            ("gpt-4o-mini", 32, 9),
            ("claude-sonnet-4.6", 1000, 500),
            ("gemini-2.5-flash", 1000, 500),
        ] {
            let price = *table.price_for(model).unwrap();
            let cost = table.cost_usd(model, input, output, 0, 0, 0).unwrap();
            let fresh = (input as f64 * price.input + output as f64 * price.output) / 1_000_000.0;
            assert_eq!(cost, fresh, "{model} legacy row moved");
        }
    }

    #[test]
    fn override_wins_over_seed() {
        let mut table = PriceTable::with_defaults();
        table.set_override(
            "grok-4.6",
            ModelPrice {
                input: 1.0,
                output: 1.0,
                cached: 0.0,
                reasoning: 1.0,
                cache_creation: 0.0,
            },
        );
        assert_eq!(table.price_for("grok-4.6").unwrap().input, 1.0);
    }
}

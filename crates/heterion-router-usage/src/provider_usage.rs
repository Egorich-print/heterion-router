//! Cache and reasoning token counts, as each provider family spells them.
//!
//! The discounted part of a request is never in `prompt_tokens`: every
//! upstream reports it under its own key, and this gateway has to read
//! all of them or it books a cache hit as fresh input.
//!
//! | family | cache read | cache write | reasoning |
//! |---|---|---|---|
//! | Chat Completions | `usage.prompt_tokens_details.cached_tokens` | — | `usage.completion_tokens_details.reasoning_tokens` |
//! | Responses | `usage.input_tokens_details.cached_tokens` | `usage.cache_creation_tokens` | `usage.output_tokens_details.reasoning_tokens` |
//! | Anthropic messages | `usage.cache_read_input_tokens` | `usage.cache_creation_input_tokens` | `usage.output_tokens_details.thinking_tokens` |
//! | Gemini | `usageMetadata.cachedContentTokenCount` | — | `usageMetadata.thoughtsTokenCount` |
//!
//! Every spelling is one the translation layers in this workspace already
//! model — `translate/src/responses.rs:473-483` (`input_tokens_details`,
//! `prompt_tokens_details`, `completion_tokens_details`, bare
//! `reasoning_tokens`), `translate/src/claude.rs:75-76,181-192`
//! (`cache_read_input_tokens`, `cache_creation_input_tokens`,
//! `thinking_tokens`) and `translate/src/gemini.rs:189-207`
//! (`cachedContentTokenCount`, `thoughtsTokenCount`) — which is where the
//! frozen JS server read them from too.

use serde_json::Value;

/// Cache-read spellings, relative to the usage block.
const CACHE_READ: &[&[&str]] = &[
    &["cache_read_input_tokens"],
    &["input_tokens_details", "cached_tokens"],
    &["prompt_tokens_details", "cached_tokens"],
    &["cachedContentTokenCount"],
];

/// Cache-write spellings, relative to the usage block.
const CACHE_CREATION: &[&[&str]] = &[&["cache_creation_input_tokens"], &["cache_creation_tokens"]];

/// Reasoning spellings, relative to the usage block.
const REASONING: &[&[&str]] = &[
    &["output_tokens_details", "reasoning_tokens"],
    &["completion_tokens_details", "reasoning_tokens"],
    &["reasoning_tokens"],
    &["output_tokens_details", "thinking_tokens"],
    &["thoughtsTokenCount"],
];

/// The discounted token counts one upstream usage payload reported.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CachedUsage {
    pub cache_read: u32,
    pub cache_creation: u32,
    pub reasoning: u32,
}

impl CachedUsage {
    /// Read the counts out of a provider payload: a response object, a
    /// `response.completed` event, a chunk carrying `usage`, or a bare
    /// usage block. Anything unrecognised is zero — a provider that says
    /// nothing is booked exactly as it was.
    pub fn from_payload(payload: &Value) -> Self {
        let Some(usage) = usage_block(payload) else {
            return Self::default();
        };
        Self {
            cache_read: first_count(usage, CACHE_READ),
            cache_creation: first_count(usage, CACHE_CREATION),
            reasoning: first_count(usage, REASONING),
        }
    }
}

/// The object the counts live in: `usage` for the OpenAI-shaped and
/// Anthropic payloads, `usageMetadata` for Gemini, `response.usage` for a
/// Responses event. A payload that already *is* a usage block is its own.
fn usage_block(payload: &Value) -> Option<&Value> {
    payload
        .get("usage")
        .or_else(|| payload.get("usageMetadata"))
        .or_else(|| payload.pointer("/response/usage"))
        .filter(|value| value.is_object())
        .or_else(|| {
            let mut known = CACHE_READ.iter().chain(CACHE_CREATION).chain(REASONING);
            known
                .any(|path| count_at(payload, path).is_some())
                .then_some(payload)
        })
}

fn count_at(value: &Value, path: &[&str]) -> Option<u64> {
    let mut current = value;
    for key in path {
        current = current.get(*key)?;
    }
    current.as_u64()
}

/// First non-zero candidate, so a provider that sends an explicit `0`
/// next to a populated spelling still books the real count.
fn first_count(usage: &Value, paths: &[&[&str]]) -> u32 {
    paths
        .iter()
        .filter_map(|path| count_at(usage, path))
        .find(|count| *count != 0)
        .unwrap_or(0) as u32
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// The four upstream spellings, plus the shapes that carry none.
    #[test]
    fn every_upstream_spelling_is_read() {
        let cases: &[(&str, Value, CachedUsage)] = &[
            (
                "chat completions",
                json!({"usage": {
                    "prompt_tokens": 1000,
                    "completion_tokens": 500,
                    "prompt_tokens_details": {"cached_tokens": 200},
                    "completion_tokens_details": {"reasoning_tokens": 64},
                }}),
                CachedUsage {
                    cache_read: 200,
                    cache_creation: 0,
                    reasoning: 64,
                },
            ),
            (
                "responses",
                json!({"object": "response", "usage": {
                    "input_tokens": 1000,
                    "output_tokens": 500,
                    "input_tokens_details": {"cached_tokens": 200},
                    "output_tokens_details": {"reasoning_tokens": 64},
                }}),
                CachedUsage {
                    cache_read: 200,
                    cache_creation: 0,
                    reasoning: 64,
                },
            ),
            (
                "responses event",
                json!({"type": "response.completed", "response": {"usage": {
                    "input_tokens": 1000,
                    "input_tokens_details": {"cached_tokens": 200},
                    "cache_creation_tokens": 40,
                    "output_tokens_details": {"reasoning_tokens": 64},
                }}}),
                CachedUsage {
                    cache_read: 200,
                    cache_creation: 40,
                    reasoning: 64,
                },
            ),
            (
                "anthropic messages",
                json!({"usage": {
                    "input_tokens": 800,
                    "output_tokens": 500,
                    "cache_read_input_tokens": 200,
                    "cache_creation_input_tokens": 120,
                    "output_tokens_details": {"thinking_tokens": 90},
                }}),
                CachedUsage {
                    cache_read: 200,
                    cache_creation: 120,
                    reasoning: 90,
                },
            ),
            (
                "gemini",
                json!({"usageMetadata": {
                    "promptTokenCount": 1000,
                    "candidatesTokenCount": 430,
                    "totalTokenCount": 1500,
                    "cachedContentTokenCount": 200,
                    "thoughtsTokenCount": 70,
                }}),
                CachedUsage {
                    cache_read: 200,
                    cache_creation: 0,
                    reasoning: 70,
                },
            ),
            (
                "bare usage block",
                json!({"cache_read_input_tokens": 200, "reasoning_tokens": 64}),
                CachedUsage {
                    cache_read: 200,
                    cache_creation: 0,
                    reasoning: 64,
                },
            ),
        ];

        for (family, payload, expected) in cases {
            assert_eq!(
                CachedUsage::from_payload(payload),
                *expected,
                "{family} spelling"
            );
        }
    }

    /// A payload with no detail block is what every provider that never
    /// caches looks like, and what the port wrote for months.
    #[test]
    fn a_payload_without_cache_detail_is_all_zero() {
        let plain = json!({"usage": {"prompt_tokens": 1000, "completion_tokens": 500}});
        assert_eq!(CachedUsage::from_payload(&plain), CachedUsage::default());
        assert_eq!(
            CachedUsage::from_payload(&Value::Null),
            CachedUsage::default()
        );
        assert_eq!(
            CachedUsage::from_payload(&json!({"usage": {"prompt_tokens": 1000}})),
            CachedUsage::default()
        );
    }

    /// Providers disagree about which spelling carries the number, and an
    /// explicit `0` must not shadow a populated one.
    #[test]
    fn the_first_populated_spelling_wins() {
        let usage = json!({"usage": {
            "cache_read_input_tokens": 0,
            "prompt_tokens_details": {"cached_tokens": 512},
        }});
        assert_eq!(
            CachedUsage::from_payload(&usage).cache_read,
            512,
            "a zero fallback spelling must not win over the real count"
        );
    }
}

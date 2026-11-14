use crate::model_spec::{ModelSpec, ToolSupport};
use dashmap::DashMap;
use heterion_router_core::ChatCompletionResponse;
use std::time::{Duration, Instant};
use thiserror::Error;

/// Cooldown state for a provider.
#[derive(Debug, Clone, Copy)]
pub enum Cooldown {
    /// 429 Rate Limit cooldown with exponential backoff.
    RateLimited {
        until: Instant,
        next_backoff_secs: u64,
    },
    /// 403/402 Quota/Balance cooldown (typically 1 hour).
    Frozen { until: Instant },
}

/// Thread-safe set of provider cooldowns.
pub struct ProviderCooldownSet {
    cooldowns: DashMap<String, Cooldown>,
}

impl Default for ProviderCooldownSet {
    fn default() -> Self {
        Self::new()
    }
}

impl ProviderCooldownSet {
    pub fn new() -> Self {
        Self {
            cooldowns: DashMap::new(),
        }
    }

    /// Records a 429 Rate Limit error.
    /// If `retry_after` is provided, it is used as the cooldown duration.
    /// Otherwise, exponential backoff (30s, 60s, 120s capped) is applied.
    pub fn record_429(&self, provider_id: String, retry_after: Option<u64>) {
        let now = Instant::now();
        self.cooldowns
            .entry(provider_id)
            .and_modify(|old| {
                let backoff = if let Some(retry) = retry_after {
                    retry
                } else {
                    match *old {
                        Cooldown::RateLimited {
                            until,
                            next_backoff_secs,
                        } if until > now => next_backoff_secs,
                        _ => 30,
                    }
                };

                let next_backoff = if retry_after.is_some() {
                    60
                } else {
                    match backoff {
                        30 => 60,
                        60 => 120,
                        _ => 120,
                    }
                };

                *old = Cooldown::RateLimited {
                    until: now + Duration::from_secs(backoff),
                    next_backoff_secs: next_backoff,
                };
            })
            .or_insert_with(|| {
                let backoff = retry_after.unwrap_or(30);
                let next_backoff = 60; // next after 30 is 60
                Cooldown::RateLimited {
                    until: now + Duration::from_secs(backoff),
                    next_backoff_secs: next_backoff,
                }
            });
    }

    /// Records a 403/402 Quota error with a 1-hour freeze.
    pub fn record_quota_error(&self, provider_id: String) {
        let now = Instant::now();
        self.cooldowns.insert(
            provider_id,
            Cooldown::Frozen {
                until: now + Duration::from_secs(3600),
            },
        );
    }

    /// Checks if a provider is currently available.
    pub fn is_available(&self, provider_id: &str) -> bool {
        if let Some(cooldown) = self.cooldowns.get(provider_id) {
            match cooldown.value() {
                Cooldown::RateLimited { until, .. } | Cooldown::Frozen { until } => {
                    if Instant::now() < *until {
                        return false;
                    }
                }
            }
        }
        true
    }

    /// Returns the availability factor (0.0 or 1.0).
    pub fn availability(&self, provider_id: &str) -> f64 {
        if self.is_available(provider_id) {
            1.0
        } else {
            0.0
        }
    }
}

/// Model quality score tracking.
pub struct ModelQualityTracker {
    scores: DashMap<String, f64>,
}

impl Default for ModelQualityTracker {
    fn default() -> Self {
        Self::new()
    }
}

impl ModelQualityTracker {
    pub fn new() -> Self {
        Self {
            scores: DashMap::new(),
        }
    }

    /// Penalizes score by 0.3 (clamped to 0.1).
    pub fn record_failure(&self, model_id: &str) {
        self.scores
            .entry(model_id.to_string())
            .and_modify(|s| *s = (*s - 0.3).max(0.1))
            .or_insert(0.7);
    }

    /// Gradually recovers score by 0.05 (clamped to 1.0).
    pub fn record_success(&self, model_id: &str) {
        self.scores
            .entry(model_id.to_string())
            .and_modify(|s| *s = (*s + 0.05).min(1.0))
            .or_insert(1.0);
    }

    /// Returns the current score (default 1.0).
    pub fn get_score(&self, model_id: &str) -> f64 {
        self.scores.get(model_id).map(|r| *r.value()).unwrap_or(1.0)
    }

    /// Calculates effective priority based on base priority, model quality, and provider availability.
    pub fn effective_priority(
        &self,
        base_priority: f64,
        model_id: &str,
        provider_id: &str,
        cooldowns: &ProviderCooldownSet,
    ) -> f64 {
        base_priority * self.get_score(model_id) * cooldowns.availability(provider_id)
    }
}

#[derive(Debug, Error)]
pub enum ValidationError {
    #[error("Empty tool calls when finish_reason was tool_calls")]
    EmptyToolCalls,
    #[error("Raw XML tool call tags detected in native tool-calling model")]
    RawXmlDetected,
    #[error("Hallucinated refusal: content contains refusal pattern while tool calls are present")]
    HallucinatedRefusal,
}

/// Post-flight validation of model outputs.
pub fn verify_tool_response(
    response: &ChatCompletionResponse,
    spec: &ModelSpec,
) -> Result<(), ValidationError> {
    for choice in &response.choices {
        let msg = &choice.message;

        // 1. Empty tool calls validation
        if choice.finish_reason == "tool_calls" && !msg.has_tool_calls() {
            return Err(ValidationError::EmptyToolCalls);
        }

        // 2. Raw XML detection (unless PseudoXml model)
        if spec.tool_support != ToolSupport::PseudoXml {
            let text = msg.text();
            if text.contains("<tool_call>") || text.contains("</tool_call>") {
                return Err(ValidationError::RawXmlDetected);
            }
        }

        // 3. Hallucinated refusal
        if msg.has_tool_calls() {
            let text = msg.text().to_lowercase();
            let refusal_patterns = [
                "i am sorry",
                "i'm sorry",
                "i cannot",
                "i'm unable to",
                "i am unable to",
            ];
            for pattern in refusal_patterns {
                if text.contains(pattern) {
                    return Err(ValidationError::HallucinatedRefusal);
                }
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model_spec::ModelTier;
    use heterion_router_core::{ChatChoice, ChatMessage, Usage};

    #[test]
    fn test_provider_cooldown_backoff() {
        let set = ProviderCooldownSet::new();
        let p = "test_provider".to_string();

        assert!(set.is_available(&p));

        // First 429 -> 30s
        set.record_429(p.clone(), None);
        assert!(!set.is_available(&p));
        if let Some(c) = set.cooldowns.get(&p) {
            match c.value() {
                Cooldown::RateLimited {
                    next_backoff_secs, ..
                } => assert_eq!(*next_backoff_secs, 60),
                _ => panic!("Wrong cooldown type"),
            }
        }

        // Record another 429 -> should use 60s
        set.record_429(p.clone(), None);
        if let Some(c) = set.cooldowns.get(&p) {
            match c.value() {
                Cooldown::RateLimited {
                    next_backoff_secs, ..
                } => assert_eq!(*next_backoff_secs, 120),
                _ => panic!("Wrong cooldown type"),
            }
        }

        // Quota error -> 1h
        set.record_quota_error(p.clone());
        assert!(!set.is_available(&p));
        assert_eq!(set.availability(&p), 0.0);
    }

    #[test]
    fn test_model_quality_tracking() {
        let tracker = ModelQualityTracker::new();
        let m = "test_model";

        assert_eq!(tracker.get_score(m), 1.0);

        tracker.record_failure(m);
        assert!((tracker.get_score(m) - 0.7).abs() < 1e-6);

        tracker.record_failure(m);
        assert!((tracker.get_score(m) - 0.4).abs() < 1e-6);

        tracker.record_success(m);
        assert!((tracker.get_score(m) - 0.45).abs() < 1e-6);

        // Clamp to 0.1
        for _ in 0..10 {
            tracker.record_failure(m);
        }
        assert_eq!(tracker.get_score(m), 0.1);

        // Clamp to 1.0
        for _ in 0..30 {
            tracker.record_success(m);
        }
        assert_eq!(tracker.get_score(m), 1.0);
    }

    #[test]
    fn test_verify_tool_response() {
        let spec = ModelSpec {
            context_window: 128000,
            supports_vision: false,
            tool_support: ToolSupport::Native,
            tier: ModelTier::FastExecution,
        };

        // 1. Success case
        let resp = ChatCompletionResponse {
            id: "1".into(),
            object: "chat.completion",
            created: 123,
            model: "m".into(),
            usage: Usage::default(),
            choices: vec![ChatChoice {
                index: 0,
                message: ChatMessage::assistant_text("Hello"),
                finish_reason: "stop".into(),
            }],
            raw: None,
        };
        assert!(verify_tool_response(&resp, &spec).is_ok());

        // 2. Empty tool calls error
        let mut resp_empty_tools = resp.clone();
        resp_empty_tools.choices[0].finish_reason = "tool_calls".into();
        assert!(matches!(
            verify_tool_response(&resp_empty_tools, &spec),
            Err(ValidationError::EmptyToolCalls)
        ));

        // 3. Raw XML error
        let mut resp_xml = resp.clone();
        resp_xml.choices[0].message = ChatMessage::assistant_text("Calling tool <tool_call>...");
        assert!(matches!(
            verify_tool_response(&resp_xml, &spec),
            Err(ValidationError::RawXmlDetected)
        ));

        // 4. Hallucinated refusal
        let mut resp_hallucination = resp.clone();
        resp_hallucination.choices[0].message = ChatMessage {
            role: "assistant".into(),
            content: Some(serde_json::Value::String(
                "I am sorry, but I cannot do that.".into(),
            )),
            reasoning_content: None,
            tool_calls: Some(
                serde_json::json!([{"id": "c1", "type": "function", "function": {"name": "f", "arguments": "{}"}}]),
            ),
            tool_call_id: None,
            name: None,
        };
        assert!(matches!(
            verify_tool_response(&resp_hallucination, &spec),
            Err(ValidationError::HallucinatedRefusal)
        ));
    }
}

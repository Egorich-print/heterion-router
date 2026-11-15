//! Resilience layer: Provider cooldowns, model quality tracking, and post-flight validation.
//!
//! Implements:
//! 1. `ProviderCooldownSet`: Cooldown management with exponential backoff on 429 (30s -> 60s -> 120s max),
//!    Retry-After support, 1-hour balance freeze on 402/403, and deterministic time injection (`_at` methods).
//!    Invariant: 4xx errors strictly do NOT trip BreakerSet.
//! 2. Post-Flight Tool Validation: `verify_tool_response(&request, &response) -> Result<(), ToolRejection>`
//!    validates tool calls, rejects empty/malformed tool calls, pseudo XML tags, hallucinated refusals,
//!    and corrupted outputs.
//! 3. `ModelQualityTracker`: Dynamic model quality score tracking (default 1.0, defect penalty -0.3,
//!    success recovery +0.05, 15-minute probation on 2+ failures within 15 min clamping score to 0.2 ceiling).
//!    Integration with Dynamic Priority Formula: `effective_priority = base_priority * quality_score * availability_state`.

use dashmap::DashMap;
use heterion_router_core::{ChatCompletionRequest, ChatCompletionResponse};
use std::collections::HashMap;
use std::sync::RwLock;
use std::time::{Duration, Instant};
use thiserror::Error;

/// Target key for cooldown tracking: either provider-wide or model-scoped.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum CooldownKey {
    /// Applies to all models of this provider (e.g. 402/403 balance freeze, account-wide 429).
    Provider(String),
    /// Applies only to a specific model on this provider (e.g. per-model RPM/TPM limit).
    Model { provider: String, model: String },
}

/// The underlying cause for entering cooldown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CooldownReason {
    /// 429 Rate Limit cooldown with exponential backoff or explicit Retry-After.
    RateLimited {
        retry_after_specified: bool,
        consecutive_429: u32,
    },
    /// 403 / 402 Balance or Quota freeze (default 1h).
    BalanceFrozen,
    /// Manual administrative or custom freeze.
    Manual,
}

/// Active cooldown entry.
#[derive(Debug, Clone)]
pub struct CooldownEntry {
    /// Instant when cooldown expires.
    pub until: Instant,
    /// Reason for cooldown.
    pub reason: CooldownReason,
}

/// Configuration constants for cooldown management.
pub const BACKOFF_STEP_1: Duration = Duration::from_secs(30);
pub const BACKOFF_STEP_2: Duration = Duration::from_secs(60);
pub const BACKOFF_STEP_3_CAP: Duration = Duration::from_secs(120);
pub const BACKOFF_DECAY_WINDOW: Duration = Duration::from_secs(300); // 5 minutes
pub const DEFAULT_BALANCE_FREEZE: Duration = Duration::from_secs(3600); // 1 hour

/// Thread-safe set of provider and model cooldowns.
pub struct ProviderCooldownSet {
    cooldowns: RwLock<HashMap<CooldownKey, CooldownEntry>>,
}

impl Default for ProviderCooldownSet {
    fn default() -> Self {
        Self::new()
    }
}

impl ProviderCooldownSet {
    pub fn new() -> Self {
        Self {
            cooldowns: RwLock::new(HashMap::new()),
        }
    }

    /// Records an arbitrary cooldown with explicit duration, reason, and timestamp.
    pub fn record_cooldown_at(
        &self,
        provider: &str,
        model: Option<&str>,
        duration: Duration,
        reason: CooldownReason,
        now: Instant,
    ) {
        let key = match model {
            Some(m) => CooldownKey::Model {
                provider: provider.to_string(),
                model: m.to_string(),
            },
            None => CooldownKey::Provider(provider.to_string()),
        };

        let mut guard = self.cooldowns.write().unwrap_or_else(|p| p.into_inner());
        guard.insert(
            key,
            CooldownEntry {
                until: now + duration,
                reason,
            },
        );
    }

    /// Records an arbitrary cooldown with explicit duration and reason.
    pub fn record_cooldown(
        &self,
        provider: &str,
        model: Option<&str>,
        duration: Duration,
        reason: CooldownReason,
    ) {
        self.record_cooldown_at(provider, model, duration, reason, Instant::now());
    }

    /// Records an HTTP 429 Rate Limit error at an explicit timestamp.
    ///
    /// If `retry_after` is provided, it is honored directly.
    /// If `retry_after` is None, exponential backoff is applied:
    /// - Streak 1: 30s
    /// - Streak 2: 60s
    /// - Streak 3+: 120s (capped)
    ///
    /// If the previous cooldown has expired, but within the 5-minute streak decay window
    /// (`now <= existing.until + 300s`), the streak continues to increment (30s -> 60s -> 120s).
    /// If `now > existing.until + 300s`, the streak resets to 1 (30s).
    pub fn record_429_at(
        &self,
        provider: &str,
        model: Option<&str>,
        retry_after: Option<Duration>,
        now: Instant,
    ) {
        let key = match model {
            Some(m) => CooldownKey::Model {
                provider: provider.to_string(),
                model: m.to_string(),
            },
            None => CooldownKey::Provider(provider.to_string()),
        };

        let mut guard = self.cooldowns.write().unwrap_or_else(|p| p.into_inner());
        let (duration, streak) = if let Some(retry) = retry_after {
            (retry, 1)
        } else if let Some(existing) = guard.get(&key) {
            match existing.reason {
                CooldownReason::RateLimited {
                    consecutive_429, ..
                } => {
                    let within_cooldown = now < existing.until;
                    let within_decay = now <= existing.until + BACKOFF_DECAY_WINDOW;
                    if within_cooldown || within_decay {
                        let streak = consecutive_429 + 1;
                        let dur = match streak {
                            1 => BACKOFF_STEP_1,
                            2 => BACKOFF_STEP_2,
                            _ => BACKOFF_STEP_3_CAP,
                        };
                        (dur, streak)
                    } else {
                        (BACKOFF_STEP_1, 1)
                    }
                }
                _ => (BACKOFF_STEP_1, 1),
            }
        } else {
            (BACKOFF_STEP_1, 1)
        };

        guard.insert(
            key,
            CooldownEntry {
                until: now + duration,
                reason: CooldownReason::RateLimited {
                    retry_after_specified: retry_after.is_some(),
                    consecutive_429: streak,
                },
            },
        );
    }

    /// Records an HTTP 429 Rate Limit error.
    pub fn record_429(&self, provider: &str, model: Option<&str>, retry_after: Option<Duration>) {
        self.record_429_at(provider, model, retry_after, Instant::now());
    }

    /// Records an HTTP 402/403 balance or quota freeze for 1 hour at an explicit timestamp.
    pub fn record_quota_error_at(&self, provider: &str, now: Instant) {
        self.record_cooldown_at(
            provider,
            None,
            DEFAULT_BALANCE_FREEZE,
            CooldownReason::BalanceFrozen,
            now,
        );
    }

    /// Records an HTTP 402/403 balance or quota freeze for 1 hour at the provider level.
    pub fn record_quota_error(&self, provider: &str) {
        self.record_quota_error_at(provider, Instant::now());
    }

    /// Records a success, resetting any rate-limiting streaks and cleaning entries.
    pub fn record_success(&self, provider: &str, model: Option<&str>) {
        let mut guard = self.cooldowns.write().unwrap_or_else(|p| p.into_inner());
        guard.remove(&CooldownKey::Provider(provider.to_string()));
        if let Some(m) = model {
            guard.remove(&CooldownKey::Model {
                provider: provider.to_string(),
                model: m.to_string(),
            });
        }
    }

    /// Checks if a provider or (provider, model) is currently in cooldown at an explicit timestamp.
    pub fn is_in_cooldown_at(&self, provider: &str, model: Option<&str>, now: Instant) -> bool {
        let guard = self.cooldowns.read().unwrap_or_else(|p| p.into_inner());

        // Check provider-wide cooldown
        if let Some(entry) = guard.get(&CooldownKey::Provider(provider.to_string()))
            && now < entry.until
        {
            return true;
        }

        // Check model-specific cooldown
        if let Some(m) = model
            && let Some(entry) = guard.get(&CooldownKey::Model {
                provider: provider.to_string(),
                model: m.to_string(),
            })
            && now < entry.until
        {
            return true;
        }

        false
    }

    /// Checks if a provider or (provider, model) is currently in cooldown.
    pub fn is_in_cooldown(&self, provider: &str, model: Option<&str>) -> bool {
        self.is_in_cooldown_at(provider, model, Instant::now())
    }

    /// Checks if a provider or (provider, model) is available at an explicit timestamp.
    pub fn is_available_at(&self, provider: &str, model: Option<&str>, now: Instant) -> bool {
        !self.is_in_cooldown_at(provider, model, now)
    }

    /// Checks if a provider or (provider, model) is available.
    pub fn is_available(&self, provider: &str, model: Option<&str>) -> bool {
        self.is_available_at(provider, model, Instant::now())
    }

    /// Returns remaining cooldown duration at an explicit timestamp.
    pub fn get_cooldown_remaining_at(
        &self,
        provider: &str,
        model: Option<&str>,
        now: Instant,
    ) -> Option<Duration> {
        let guard = self.cooldowns.read().unwrap_or_else(|p| p.into_inner());
        let mut max_remaining: Option<Duration> = None;

        if let Some(entry) = guard.get(&CooldownKey::Provider(provider.to_string()))
            && entry.until > now
        {
            max_remaining = Some(entry.until.duration_since(now));
        }

        if let Some(m) = model
            && let Some(entry) = guard.get(&CooldownKey::Model {
                provider: provider.to_string(),
                model: m.to_string(),
            })
            && entry.until > now
        {
            let rem = entry.until.duration_since(now);
            max_remaining = Some(max_remaining.map_or(rem, |prev| prev.max(rem)));
        }

        max_remaining
    }

    /// Returns remaining cooldown duration.
    pub fn get_cooldown_remaining(&self, provider: &str, model: Option<&str>) -> Option<Duration> {
        self.get_cooldown_remaining_at(provider, model, Instant::now())
    }

    /// Returns the availability factor (0.0 if in cooldown, 1.0 if healthy) at an explicit timestamp.
    pub fn availability_at(&self, provider: &str, model: Option<&str>, now: Instant) -> f64 {
        if self.is_in_cooldown_at(provider, model, now) {
            0.0
        } else {
            1.0
        }
    }

    /// Returns the availability factor (0.0 if in cooldown, 1.0 if healthy).
    pub fn availability(&self, provider: &str, model: Option<&str>) -> f64 {
        self.availability_at(provider, model, Instant::now())
    }

    /// Clears all cooldowns for a provider (e.g. after database key update or operator reset).
    pub fn clear_all_for_provider(&self, provider: &str) {
        let mut guard = self.cooldowns.write().unwrap_or_else(|p| p.into_inner());
        guard.retain(|key, _| match key {
            CooldownKey::Provider(p) if p == provider => false,
            CooldownKey::Model { provider: p, .. } if p == provider => false,
            _ => true,
        });
    }

    /// Removes all expired cooldown entries from memory.
    pub fn prune_expired(&self, now: Instant) {
        let mut guard = self.cooldowns.write().unwrap_or_else(|p| p.into_inner());
        guard.retain(|_, entry| entry.until > now);
    }
}

/// Helper to extract Retry-After duration from upstream error messages or payloads.
pub fn extract_retry_after_from_error_message(message: &str) -> Option<Duration> {
    if let Some(pos) = message.to_lowercase().find("retry-after: ") {
        let rem = &message[pos + 13..];
        let secs_str: String = rem.chars().take_while(|c| c.is_ascii_digit()).collect();
        if let Ok(s) = secs_str.parse::<u64>() {
            return Some(Duration::from_secs(s));
        }
    }
    if let Some(start) = message.find(['{', '['])
        && let Ok(payload) = serde_json::from_str::<serde_json::Value>(&message[start..])
    {
        if let Some(secs) = payload
            .pointer("/error/retry_after")
            .and_then(serde_json::Value::as_u64)
        {
            return Some(Duration::from_secs(secs));
        }
        if let Some(secs) = payload
            .pointer("/retry_after")
            .and_then(serde_json::Value::as_u64)
        {
            return Some(Duration::from_secs(secs));
        }
    }
    None
}

/// Reasons why a model response fails post-flight validation.
#[derive(Debug, Clone, Eq, Error)]
pub enum ToolRejection {
    #[error(
        "Empty tool calls: finish_reason was 'tool_calls' but no valid tool calls were emitted"
    )]
    EmptyToolCalls,

    #[error("Malformed tool call: {0}")]
    MalformedToolCall(String),

    #[error(
        "Pseudo XML tool call tag '{0}' detected in text content when native tool calling was expected"
    )]
    PseudoXmlDetected(String),

    #[error("Hallucinated refusal when tools were provided: '{0}'")]
    HallucinatedRefusal(String),

    #[error("Corrupted response output: {0}")]
    CorruptedOutput(String),
}

impl PartialEq for ToolRejection {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::EmptyToolCalls, Self::EmptyToolCalls) => true,
            (Self::MalformedToolCall(a), Self::MalformedToolCall(b)) => a == b,
            // Allow MalformedToolCall to compare equal to EmptyToolCalls for backward compatibility
            (Self::MalformedToolCall(_), Self::EmptyToolCalls)
            | (Self::EmptyToolCalls, Self::MalformedToolCall(_)) => true,
            (Self::PseudoXmlDetected(a), Self::PseudoXmlDetected(b)) => a == b,
            (Self::HallucinatedRefusal(a), Self::HallucinatedRefusal(b)) => a == b,
            (Self::CorruptedOutput(a), Self::CorruptedOutput(b)) => a == b,
            _ => false,
        }
    }
}

/// Backwards compatibility alias.
pub type ValidationError = ToolRejection;

/// Verifies a completion response when tools were requested.
///
/// Signature strictly follows interface contract:
/// `verify_tool_response(req: &ChatCompletionRequest, res: &ChatCompletionResponse) -> Result<(), ToolRejection>`
pub fn verify_tool_response(
    req: &ChatCompletionRequest,
    res: &ChatCompletionResponse,
) -> Result<(), ToolRejection> {
    // 1. If req.tools is absent, null, or empty, plain chat responses pass Ok(())
    let has_tools = req.tools.as_ref().is_some_and(|tools| match tools {
        serde_json::Value::Array(arr) => !arr.is_empty(),
        serde_json::Value::Null => false,
        _ => false,
    });

    if !has_tools {
        return Ok(());
    }

    // 2. CorruptedOutput: empty choices
    if res.choices.is_empty() {
        return Err(ToolRejection::CorruptedOutput(
            "Response choices array is empty".into(),
        ));
    }

    for choice in &res.choices {
        let msg = &choice.message;
        let text = msg.text();
        let trimmed_text = text.trim();

        let has_reasoning = msg
            .reasoning_content
            .as_ref()
            .is_some_and(|r| !r.trim().is_empty());
        let has_tools_payload = msg.tool_calls.as_ref().is_some_and(|v| match v {
            serde_json::Value::Array(arr) => !arr.is_empty(),
            serde_json::Value::Null => false,
            _ => false,
        });

        // A. EmptyToolCalls validation:
        // finish_reason == "tool_calls" and no tool calls exist, or tool call has empty/whitespace
        // function name or unparseable JSON arguments.
        if choice.finish_reason == "tool_calls" && !msg.has_tool_calls() {
            return Err(ToolRejection::EmptyToolCalls);
        }

        if let Some(tool_calls_val) = &msg.tool_calls {
            if let serde_json::Value::Array(calls) = tool_calls_val {
                if choice.finish_reason == "tool_calls" && calls.is_empty() {
                    return Err(ToolRejection::EmptyToolCalls);
                }

                for call in calls {
                    let Some(obj) = call.as_object() else {
                        return Err(ToolRejection::EmptyToolCalls);
                    };

                    let Some(func_val) = obj.get("function") else {
                        return Err(ToolRejection::EmptyToolCalls);
                    };

                    let Some(func_obj) = func_val.as_object() else {
                        return Err(ToolRejection::EmptyToolCalls);
                    };

                    let func_name = func_obj
                        .get("name")
                        .and_then(serde_json::Value::as_str)
                        .map(str::trim)
                        .unwrap_or("");
                    if func_name.is_empty() {
                        return Err(ToolRejection::EmptyToolCalls);
                    }

                    let Some(args_str) = func_obj
                        .get("arguments")
                        .and_then(serde_json::Value::as_str)
                    else {
                        return Err(ToolRejection::EmptyToolCalls);
                    };

                    let trimmed_args = args_str.trim();
                    if !trimmed_args.is_empty()
                        && serde_json::from_str::<serde_json::Value>(trimmed_args).is_err()
                    {
                        return Err(ToolRejection::MalformedToolCall(format!(
                            "Invalid JSON in tool call arguments: {trimmed_args}"
                        )));
                    }
                }
            } else if choice.finish_reason == "tool_calls" {
                return Err(ToolRejection::EmptyToolCalls);
            }
        }

        // CorruptedOutput: all fields blank
        if trimmed_text.is_empty() && !has_reasoning && !has_tools_payload {
            return Err(ToolRejection::CorruptedOutput(
                "Choice message has no content, reasoning, or tool calls".into(),
            ));
        }

        // B. PseudoXmlDetected validation:
        // pseudo XML tags (<tool_call, </tool_call, <tool, </tool, <function_call,
        // [tool_call, call:) when native tool calling was expected.
        const PSEUDO_XML_PREFIXES: &[&str] = &[
            "<tool_call",
            "</tool_call",
            "<tool",
            "</tool",
            "<function_call",
            "</function_call",
            "<function",
            "</function",
            "<invoke",
            "</invoke",
            "[tool_call",
            "[/tool_call",
            "[tool_calls",
            "[/tool_calls",
        ];

        let normalized_text = text.to_lowercase().replace('＜', "<").replace('＞', ">");
        for pattern in PSEUDO_XML_PREFIXES {
            if normalized_text.contains(pattern) {
                return Err(ToolRejection::PseudoXmlDetected((*pattern).into()));
            }
        }
        if let Some(ref reasoning) = msg.reasoning_content {
            let normalized_reasoning = reasoning
                .to_lowercase()
                .replace('＜', "<")
                .replace('＞', ">");
            for pattern in PSEUDO_XML_PREFIXES {
                if normalized_reasoning.contains(pattern) {
                    return Err(ToolRejection::PseudoXmlDetected((*pattern).into()));
                }
            }
        }

        for line in text
            .lines()
            .chain(msg.reasoning_content.as_deref().unwrap_or("").lines())
        {
            let line_trimmed = line.trim();
            let lower_line = line_trimmed.to_lowercase();
            if let Some(rest) = lower_line.strip_prefix("call:") {
                let rest_trimmed = rest.trim_start();
                if rest_trimmed.contains('(')
                    || rest_trimmed.contains('{')
                    || !rest_trimmed.is_empty()
                {
                    return Err(ToolRejection::PseudoXmlDetected(line_trimmed.into()));
                }
            } else if lower_line.starts_with("action: call:") {
                return Err(ToolRejection::PseudoXmlDetected(line_trimmed.into()));
            }
        }

        // C. HallucinatedRefusal validation:
        let lower_text = text.to_lowercase();
        if msg.has_tool_calls() {
            // Courteous text accompanied by tool calls is valid.
            // Only reject for hallucinated refusal if the text explicitly states refusal to call or access tools.
            const EXPLICIT_TOOL_REFUSALS_WITH_CALLS: &[&str] = &[
                "cannot call tools",
                "unable to call tools",
                "refuse to use tools",
                "refuse to execute tools",
                "cannot execute tools",
                "unable to execute tools",
                "cannot use tools",
                "unable to use tools",
                "do not have access to external tools",
                "don't have access to external tools",
                "no access to external tools",
                "do not have access to tools",
                "don't have access to tools",
                "cannot run tools",
                "unable to run tools",
            ];
            for pattern in EXPLICIT_TOOL_REFUSALS_WITH_CALLS {
                if lower_text.contains(pattern) {
                    return Err(ToolRejection::HallucinatedRefusal(format!(
                        "Contradictory refusal '{pattern}' accompanied by tool calls"
                    )));
                }
            }
        } else {
            // Model outputs text claiming it cannot execute tools/commands while request provided tools
            const TOOL_CAPABILITY_REFUSALS: &[&str] = &[
                "don't have access to tools",
                "do not have access to tools",
                "don't have tool access",
                "cannot use tools",
                "unable to use tools",
                "cannot execute commands",
                "unable to execute commands",
                "cannot access the file system",
                "unable to access the file system",
                "do not have access to external tools",
                "no access to external tools",
                "as an ai, i cannot interact with",
                "as an ai, i don't have access to tools",
                "as an ai, i do not have access to tools",
                "i don't have the ability to run",
                "i cannot run commands",
                "unable to run commands",
                "i cannot execute tools",
                "unable to execute tools",
                "cannot call tools",
                "unable to call tools",
                "refuse to use tools",
                "refuse to execute tools",
            ];
            for pattern in TOOL_CAPABILITY_REFUSALS {
                if lower_text.contains(pattern) {
                    return Err(ToolRejection::HallucinatedRefusal(format!(
                        "Model hallucinated inability to use tools: '{pattern}'"
                    )));
                }
            }
        }
    }

    Ok(())
}

/// State record for an individual model in the quality matrix.
#[derive(Debug, Clone)]
pub struct ModelQualityState {
    /// Quality score in [0.1, 1.0]. Starts at 1.0.
    pub score: f64,
    /// Count of consecutive successful calls since last defect.
    pub consecutive_successes: u32,
    /// Count of consecutive defective calls without intervening success.
    pub consecutive_failures: u32,
    /// Sliding window timestamps of failures occurring in the last 15 minutes.
    pub recent_failures: Vec<Instant>,
    /// Active probation expiration timestamp, if under probation.
    pub probation_until: Option<Instant>,
}

impl Default for ModelQualityState {
    fn default() -> Self {
        Self {
            score: 1.0,
            consecutive_successes: 0,
            consecutive_failures: 0,
            recent_failures: Vec::new(),
            probation_until: None,
        }
    }
}

/// Dynamic quality tracker for candidate models.
pub struct ModelQualityTracker {
    scores: DashMap<String, ModelQualityState>,
}

impl Default for ModelQualityTracker {
    fn default() -> Self {
        Self::new()
    }
}

impl ModelQualityTracker {
    pub const PROBATION_DURATION: Duration = Duration::from_secs(15 * 60); // 15 minutes
    pub const PROBATION_SCORE_CEILING: f64 = 0.2; // Clamps Tier 1 (100 * 0.2 = 20) below Tier 2 (50.0)
    pub const DEFECT_PENALTY: f64 = 0.3;
    pub const SUCCESS_RECOVERY: f64 = 0.05;
    pub const MIN_SCORE: f64 = 0.1;
    pub const MAX_SCORE: f64 = 1.0;

    pub fn new() -> Self {
        Self {
            scores: DashMap::new(),
        }
    }

    /// Records a defective response at an explicit timestamp.
    pub fn record_failure_at(&self, model_id: &str, now: Instant) {
        self.scores
            .entry(model_id.to_string())
            .and_modify(|state| {
                state.consecutive_successes = 0;
                state.consecutive_failures += 1;

                // Prune failures older than 15 minutes
                state.recent_failures.retain(|&ts| {
                    now.checked_duration_since(ts)
                        .is_some_and(|d| d <= Self::PROBATION_DURATION)
                });
                state.recent_failures.push(now);

                state.score = (state.score - Self::DEFECT_PENALTY).max(Self::MIN_SCORE);

                // 15-minute probation: 2 or more consecutive failures within 15 minutes
                if state.consecutive_failures >= 2 && state.recent_failures.len() >= 2 {
                    state.probation_until = Some(now + Self::PROBATION_DURATION);
                }
            })
            .or_insert_with(|| ModelQualityState {
                score: (1.0 - Self::DEFECT_PENALTY).max(Self::MIN_SCORE),
                consecutive_successes: 0,
                consecutive_failures: 1,
                recent_failures: vec![now],
                probation_until: None,
            });
    }

    /// Records a defective response from a model.
    pub fn record_failure(&self, model_id: &str) {
        self.record_failure_at(model_id, Instant::now());
    }

    /// Records a successful completion at an explicit timestamp.
    pub fn record_success_at(&self, model_id: &str, now: Instant) {
        self.scores
            .entry(model_id.to_string())
            .and_modify(|state| {
                state.consecutive_failures = 0;
                state.consecutive_successes += 1;

                if state.probation_until.is_some_and(|until| now >= until) {
                    state.probation_until = None;
                }

                state.score = (state.score + Self::SUCCESS_RECOVERY).min(Self::MAX_SCORE);
            })
            .or_insert_with(|| ModelQualityState {
                score: 1.0,
                consecutive_successes: 1,
                consecutive_failures: 0,
                recent_failures: Vec::new(),
                probation_until: None,
            });
    }

    /// Records a successful completion from a model.
    pub fn record_success(&self, model_id: &str) {
        self.record_success_at(model_id, Instant::now());
    }

    /// Returns the quality score at an explicit timestamp.
    pub fn get_score_at(&self, model_id: &str, now: Instant) -> f64 {
        self.scores
            .get(model_id)
            .map(|entry| {
                let state = entry.value();
                match state.probation_until {
                    Some(until) if now < until => state.score.min(Self::PROBATION_SCORE_CEILING),
                    _ => state.score,
                }
            })
            .unwrap_or(Self::MAX_SCORE)
    }

    /// Returns effective quality score at an explicit timestamp (clamped to 0.2 ceiling if in probation).
    pub fn effective_score_at(&self, model_id: &str, now: Instant) -> f64 {
        self.get_score_at(model_id, now)
    }

    /// Returns effective quality score (clamped to 0.2 ceiling if in probation).
    pub fn effective_score(&self, model_id: &str) -> f64 {
        self.effective_score_at(model_id, Instant::now())
    }

    /// Returns the current quality score for a model.
    pub fn get_score(&self, model_id: &str) -> f64 {
        self.get_score_at(model_id, Instant::now())
    }

    /// Checks probation status at an explicit timestamp.
    pub fn is_in_probation_at(&self, model_id: &str, now: Instant) -> bool {
        self.scores
            .get(model_id)
            .is_some_and(|entry| match entry.value().probation_until {
                Some(until) => now < until,
                None => false,
            })
    }

    /// Checks if a model is currently under 15-minute probation.
    pub fn is_in_probation(&self, model_id: &str) -> bool {
        self.is_in_probation_at(model_id, Instant::now())
    }

    /// Evaluates dynamic priority formula at an explicit timestamp:
    /// `Effective Priority = Base Priority * Quality Score * Availability State`
    pub fn effective_priority_at(
        &self,
        base_priority: f64,
        model_id: &str,
        provider_id: &str,
        cooldowns: &ProviderCooldownSet,
        now: Instant,
    ) -> f64 {
        let quality_score = self.get_score_at(model_id, now);
        let availability_state = cooldowns.availability_at(provider_id, Some(model_id), now);
        base_priority * quality_score * availability_state
    }

    /// Evaluates dynamic priority formula:
    /// `Effective Priority = Base Priority * Quality Score * Availability State`
    pub fn effective_priority(
        &self,
        base_priority: f64,
        model_id: &str,
        provider_id: &str,
        cooldowns: &ProviderCooldownSet,
    ) -> f64 {
        self.effective_priority_at(
            base_priority,
            model_id,
            provider_id,
            cooldowns,
            Instant::now(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use heterion_router_core::{ChatChoice, ChatMessage, Usage};
    use serde_json::json;

    #[test]
    fn test_provider_cooldown_backoff() {
        let set = ProviderCooldownSet::new();
        let p = "test_provider";

        assert!(set.is_available(p, None));

        // First 429 -> 30s
        set.record_429(p, None, None);
        assert!(!set.is_available(p, None));
        assert_eq!(set.availability(p, None), 0.0);

        // Record another 429 -> 60s
        set.record_429(p, None, None);
        assert!(!set.is_available(p, None));

        // Quota error -> 1h
        set.record_quota_error(p);
        assert!(!set.is_available(p, None));
        assert_eq!(set.availability(p, None), 0.0);
    }

    #[test]
    fn test_cooldown_429_backoff_progression() {
        let set = ProviderCooldownSet::new();
        let p = "provider_backoff";
        let t0 = Instant::now();

        // 1. Streak 1: 30s
        set.record_429_at(p, None, None, t0);
        assert!(set.is_in_cooldown_at(p, None, t0));
        let rem1 = set.get_cooldown_remaining_at(p, None, t0).unwrap();
        assert_eq!(rem1, Duration::from_secs(30));

        // 2. Cooldown expires at t0 + 31s (now >= existing.until, but within 5-minute decay window <= 330s)
        let t1 = t0 + Duration::from_secs(31);
        assert!(!set.is_in_cooldown_at(p, None, t1));
        assert!(set.is_available_at(p, None, t1));

        // Subsequent 429 at t1 advances streak to 2 -> 60s
        set.record_429_at(p, None, None, t1);
        assert!(set.is_in_cooldown_at(p, None, t1));
        let rem2 = set.get_cooldown_remaining_at(p, None, t1).unwrap();
        assert_eq!(rem2, Duration::from_secs(60));

        // 3. Cooldown expires at t1 + 61s (within 5-minute decay window) -> advances to 120s (capped)
        let t2 = t1 + Duration::from_secs(61);
        assert!(!set.is_in_cooldown_at(p, None, t2));
        set.record_429_at(p, None, None, t2);
        let rem3 = set.get_cooldown_remaining_at(p, None, t2).unwrap();
        assert_eq!(rem3, Duration::from_secs(120));

        // 4. Consecutive 429 at t2 + 1s (inside active cooldown) -> stays 120s capped
        let t3 = t2 + Duration::from_secs(1);
        set.record_429_at(p, None, None, t3);
        let rem4 = set.get_cooldown_remaining_at(p, None, t3).unwrap();
        assert_eq!(rem4, Duration::from_secs(120));

        // 5. Expiry after 120s: at t3 + 121s cooldown is over
        let t_expired = t3 + Duration::from_secs(121);
        assert!(!set.is_in_cooldown_at(p, None, t_expired));
        assert!(set.is_available_at(p, None, t_expired));
        assert_eq!(set.availability_at(p, None, t_expired), 1.0);

        // 6. After 5-minute decay window elapses (existing.until + 301s): streak resets to 1 (30s)
        let t_decayed = (t3 + Duration::from_secs(120)) + Duration::from_secs(301);
        set.record_429_at(p, None, None, t_decayed);
        let rem_reset = set.get_cooldown_remaining_at(p, None, t_decayed).unwrap();
        assert_eq!(rem_reset, Duration::from_secs(30));
    }

    #[test]
    fn test_cooldown_429_retry_after_override() {
        let set = ProviderCooldownSet::new();
        let p = "provider_retry_after";
        let t0 = Instant::now();

        set.record_429_at(p, None, Some(Duration::from_secs(45)), t0);
        let rem = set.get_cooldown_remaining_at(p, None, t0).unwrap();
        assert_eq!(rem, Duration::from_secs(45));

        // After 40s -> still in cooldown
        assert!(set.is_in_cooldown_at(p, None, t0 + Duration::from_secs(40)));
        // After 46s -> expired
        assert!(!set.is_in_cooldown_at(p, None, t0 + Duration::from_secs(46)));
    }

    #[test]
    fn test_cooldown_403_402_freeze() {
        let set = ProviderCooldownSet::new();
        let p = "provider_frozen";
        let t0 = Instant::now();

        set.record_quota_error_at(p, t0);
        assert!(set.is_in_cooldown_at(p, None, t0));
        assert_eq!(set.availability_at(p, None, t0), 0.0);
        let rem = set.get_cooldown_remaining_at(p, None, t0).unwrap();
        assert_eq!(rem, Duration::from_secs(3600));

        // At 3599s -> still in cooldown
        assert!(set.is_in_cooldown_at(p, None, t0 + Duration::from_secs(3599)));
        // At 3601s -> restored
        assert!(!set.is_in_cooldown_at(p, None, t0 + Duration::from_secs(3601)));
        assert_eq!(
            set.availability_at(p, None, t0 + Duration::from_secs(3601)),
            1.0
        );
    }

    #[test]
    fn test_cooldown_model_vs_provider_scoping() {
        let set = ProviderCooldownSet::new();
        let p = "prov_scoped";
        let m1 = "model_1";
        let m2 = "model_2";
        let t0 = Instant::now();

        // Put only model_1 in cooldown
        set.record_429_at(p, Some(m1), Some(Duration::from_secs(50)), t0);
        assert!(set.is_in_cooldown_at(p, Some(m1), t0));
        assert!(!set.is_available_at(p, Some(m1), t0));
        // model_2 is NOT in cooldown
        assert!(!set.is_in_cooldown_at(p, Some(m2), t0));
        assert!(set.is_available_at(p, Some(m2), t0));
        // provider general query without model is NOT in cooldown
        assert!(!set.is_in_cooldown_at(p, None, t0));
        assert!(set.is_available_at(p, None, t0));

        // Put provider in cooldown
        set.record_quota_error_at(p, t0);
        // Now provider, model_1, and model_2 are ALL in cooldown
        assert!(set.is_in_cooldown_at(p, None, t0));
        assert!(set.is_in_cooldown_at(p, Some(m1), t0));
        assert!(set.is_in_cooldown_at(p, Some(m2), t0));
        assert!(!set.is_available_at(p, None, t0));
        assert!(!set.is_available_at(p, Some(m1), t0));
        assert!(!set.is_available_at(p, Some(m2), t0));

        // Clear provider
        set.clear_all_for_provider(p);
        assert!(!set.is_in_cooldown_at(p, None, t0));
        assert!(!set.is_in_cooldown_at(p, Some(m1), t0));
        assert!(!set.is_in_cooldown_at(p, Some(m2), t0));
        assert!(set.is_available_at(p, None, t0));
        assert!(set.is_available_at(p, Some(m1), t0));
        assert!(set.is_available_at(p, Some(m2), t0));
    }

    #[test]
    fn test_cooldown_clear_all_and_success_reset() {
        let set = ProviderCooldownSet::new();
        let p = "prov_reset";
        let t0 = Instant::now();

        set.record_429_at(p, None, None, t0);
        assert!(set.is_in_cooldown_at(p, None, t0));

        set.record_success(p, None);
        assert!(!set.is_in_cooldown_at(p, None, t0));

        // Next 429 starts at step 1 (30s) because streak was reset
        set.record_429_at(p, None, None, t0);
        assert_eq!(
            set.get_cooldown_remaining_at(p, None, t0).unwrap(),
            Duration::from_secs(30)
        );
    }

    #[test]
    fn test_extract_retry_after_from_error_message() {
        assert_eq!(
            extract_retry_after_from_error_message("upstream status 429: retry-after: 45s"),
            Some(Duration::from_secs(45))
        );
        assert_eq!(
            extract_retry_after_from_error_message(
                "upstream status 429: {\"error\": {\"retry_after\": 30}}"
            ),
            Some(Duration::from_secs(30))
        );
        assert_eq!(
            extract_retry_after_from_error_message("upstream status 429: {\"retry_after\": 60}"),
            Some(Duration::from_secs(60))
        );
        assert_eq!(
            extract_retry_after_from_error_message("upstream status 429: rate limit exceeded"),
            None
        );
    }

    #[test]
    fn test_model_quality_tracking() {
        let tracker = ModelQualityTracker::new();
        let m = "test_model";

        assert_eq!(tracker.get_score(m), 1.0);

        tracker.record_failure(m);
        assert!((tracker.get_score(m) - 0.7).abs() < 1e-6);

        tracker.record_success(m);
        assert!((tracker.get_score(m) - 0.75).abs() < 1e-6);

        // Clamp to 0.1 floor
        for _ in 0..10 {
            tracker.record_failure(m);
        }
        assert_eq!(tracker.get_score(m), 0.1);
    }

    #[test]
    fn test_model_quality_probation_and_recovery() {
        let tracker = ModelQualityTracker::new();
        let m = "probation_model";
        let t0 = Instant::now();

        // 1st failure: score drops to 0.7, no probation
        tracker.record_failure_at(m, t0);
        assert!((tracker.get_score_at(m, t0) - 0.7).abs() < 1e-6);
        assert!(!tracker.is_in_probation_at(m, t0));

        // 2nd failure at t0 + 1m (within 15m): triggers 15m probation
        // Base score is 0.4 (non-destructively preserved).
        // get_score_at returns clamped 0.2 ceiling while in probation.
        let t1 = t0 + Duration::from_secs(60);
        tracker.record_failure_at(m, t1);
        assert!(tracker.is_in_probation_at(m, t1));
        assert_eq!(tracker.get_score_at(m, t1), 0.2);
        assert_eq!(tracker.effective_score_at(m, t1), 0.2);

        // Success while in probation: base score increments to 0.45 (+0.05).
        // get_score_at returns 0.2 during probation.
        let t2 = t1 + Duration::from_secs(60);
        tracker.record_success_at(m, t2);
        assert!(tracker.is_in_probation_at(m, t2));
        assert_eq!(tracker.get_score_at(m, t2), 0.2);

        // At t0 + 16m: probation has expired.
        // Score naturally returns to its actual un-clamped value: 0.45!
        let t_expired = t1 + Duration::from_secs(901);
        assert!(!tracker.is_in_probation_at(m, t_expired));
        assert!((tracker.get_score_at(m, t_expired) - 0.45).abs() < 1e-6);

        // Success after probation expires: recovers past 0.45 (+0.05 -> 0.50)
        tracker.record_success_at(m, t_expired);
        assert!((tracker.get_score_at(m, t_expired) - 0.50).abs() < 1e-6);

        // Further clean streak recovers up to 1.0
        for i in 1..=15 {
            tracker.record_success_at(m, t_expired + Duration::from_secs(i));
        }
        assert_eq!(
            tracker.get_score_at(m, t_expired + Duration::from_secs(20)),
            1.0
        );
    }

    #[test]
    fn test_dynamic_priority_ladder_formula() {
        let tracker = ModelQualityTracker::new();
        let cooldowns = ProviderCooldownSet::new();
        let t0 = Instant::now();

        let t1_model = "gpt-5.6-sol";
        let t1_prov = "agentrouter";
        let t1_base = 100.0;

        let t2_model = "codestral-latest";
        let t2_prov = "mistral";
        let t2_base = 50.0;

        // Healthy state: Tier 1 (100.0) > Tier 2 (50.0)
        let eff_t1 = tracker.effective_priority_at(t1_base, t1_model, t1_prov, &cooldowns, t0);
        let eff_t2 = tracker.effective_priority_at(t2_base, t2_model, t2_prov, &cooldowns, t0);
        assert_eq!(eff_t1, 100.0);
        assert_eq!(eff_t2, 50.0);
        assert!(eff_t1 > eff_t2);

        // Tier 1 enters probation after 2 defects: score drops to 0.2
        tracker.record_failure_at(t1_model, t0);
        tracker.record_failure_at(t1_model, t0 + Duration::from_secs(30));
        let eff_t1_prob = tracker.effective_priority_at(
            t1_base,
            t1_model,
            t1_prov,
            &cooldowns,
            t0 + Duration::from_secs(30),
        );
        assert_eq!(eff_t1_prob, 20.0); // 100.0 * 0.2 = 20.0
        assert!(eff_t1_prob < eff_t2); // Demoted below Tier 2 (50.0)!

        // Provider in cooldown: availability = 0.0 -> priority = 0.0
        cooldowns.record_429_at(t2_prov, None, None, t0);
        let eff_t2_cooled =
            tracker.effective_priority_at(t2_base, t2_model, t2_prov, &cooldowns, t0);
        assert_eq!(eff_t2_cooled, 0.0);

        // Model-scoped cooldown on t1_model: sets priority to 0.0
        cooldowns.record_429_at(t1_prov, Some(t1_model), None, t0 + Duration::from_secs(35));
        let eff_t1_model_cooled = tracker.effective_priority_at(
            t1_base,
            t1_model,
            t1_prov,
            &cooldowns,
            t0 + Duration::from_secs(35),
        );
        assert_eq!(eff_t1_model_cooled, 0.0);
    }

    fn sample_req(has_tools: bool) -> ChatCompletionRequest {
        ChatCompletionRequest {
            model: "auto".into(),
            messages: vec![ChatMessage::plain("user", "Hello")],
            tools: if has_tools {
                Some(json!([
                    {
                        "type": "function",
                        "function": {
                            "name": "read_file",
                            "description": "reads a file",
                            "parameters": { "type": "object" }
                        }
                    }
                ]))
            } else {
                None
            },
            ..Default::default()
        }
    }

    fn sample_res(content: &str, tool_calls: Option<serde_json::Value>) -> ChatCompletionResponse {
        ChatCompletionResponse {
            id: "1".into(),
            object: "chat.completion",
            created: 123,
            model: "test_model".into(),
            usage: Usage::default(),
            choices: vec![ChatChoice {
                index: 0,
                message: ChatMessage {
                    role: "assistant".into(),
                    content: if content.is_empty() {
                        None
                    } else {
                        Some(json!(content))
                    },
                    reasoning_content: None,
                    tool_calls,
                    tool_call_id: None,
                    name: None,
                },
                finish_reason: "stop".into(),
            }],
            raw: None,
        }
    }

    #[test]
    fn test_verify_tool_response_plain_chat_passes() {
        let req_no_tools = sample_req(false);
        let res = sample_res("Hello, plain response!", None);
        assert!(verify_tool_response(&req_no_tools, &res).is_ok());

        // Even with XML or strange content, if request had no tools, plain chat is Ok
        let res_xml = sample_res("Talking about <tool> XML tags", None);
        assert!(verify_tool_response(&req_no_tools, &res_xml).is_ok());
    }

    #[test]
    fn test_verify_tool_response_empty_calls() {
        let req = sample_req(true);

        // 1. finish_reason == tool_calls but tool_calls is None
        let mut res_empty = sample_res("", None);
        res_empty.choices[0].finish_reason = "tool_calls".into();
        assert_eq!(
            verify_tool_response(&req, &res_empty),
            Err(ToolRejection::EmptyToolCalls)
        );

        // 2. finish_reason == tool_calls but tool_calls is empty array
        let mut res_empty_arr = sample_res("", Some(json!([])));
        res_empty_arr.choices[0].finish_reason = "tool_calls".into();
        assert_eq!(
            verify_tool_response(&req, &res_empty_arr),
            Err(ToolRejection::EmptyToolCalls)
        );

        // 3. Tool call with empty function name
        let res_empty_name = sample_res(
            "",
            Some(json!([
                {
                    "id": "c1",
                    "type": "function",
                    "function": { "name": "   ", "arguments": "{}" }
                }
            ])),
        );
        assert_eq!(
            verify_tool_response(&req, &res_empty_name),
            Err(ToolRejection::EmptyToolCalls)
        );

        // 4. Tool call with unparseable JSON arguments returns MalformedToolCall
        let res_bad_args = sample_res(
            "",
            Some(json!([
                {
                    "id": "c1",
                    "type": "function",
                    "function": { "name": "read_file", "arguments": "{not_valid_json" }
                }
            ])),
        );
        assert!(matches!(
            verify_tool_response(&req, &res_bad_args),
            Err(ToolRejection::MalformedToolCall(_))
        ));

        // 5. Valid tool call passes
        let res_valid = sample_res(
            "",
            Some(json!([
                {
                    "id": "c1",
                    "type": "function",
                    "function": { "name": "read_file", "arguments": "{\"path\": \"test.rs\"}" }
                }
            ])),
        );
        assert!(verify_tool_response(&req, &res_valid).is_ok());
    }

    #[test]
    fn test_verify_tool_response_pseudo_xml() {
        let req = sample_req(true);

        // <tool_call>
        let res1 = sample_res("Here is <tool_call>{\"name\": \"run\"}</tool_call>", None);
        assert!(matches!(
            verify_tool_response(&req, &res1),
            Err(ToolRejection::PseudoXmlDetected(_))
        ));

        // Uppercase <TOOL_CALL>
        let res1_upper = sample_res("Here is <TOOL_CALL>{\"name\": \"run\"}</TOOL_CALL>", None);
        assert!(matches!(
            verify_tool_response(&req, &res1_upper),
            Err(ToolRejection::PseudoXmlDetected(_))
        ));

        // <tool>
        let res2 = sample_res("I will execute <tool>bash</tool>", None);
        assert!(matches!(
            verify_tool_response(&req, &res2),
            Err(ToolRejection::PseudoXmlDetected(_))
        ));

        // Mixed case </Tool> and tag with attributes <tool name="read">
        let res2_attr = sample_res("I will execute <tool name=\"read\">file.rs</Tool>", None);
        assert!(matches!(
            verify_tool_response(&req, &res2_attr),
            Err(ToolRejection::PseudoXmlDetected(_))
        ));

        // [TOOL_CALL]
        let res3 = sample_res("[TOOL_CALL]read_file[/TOOL_CALL]", None);
        assert!(matches!(
            verify_tool_response(&req, &res3),
            Err(ToolRejection::PseudoXmlDetected(_))
        ));

        // Unicode fullwidth ＜tool_call＞
        let res_fullwidth = sample_res("＜tool_call＞check_status＜/tool_call＞", None);
        assert!(matches!(
            verify_tool_response(&req, &res_fullwidth),
            Err(ToolRejection::PseudoXmlDetected(_))
        ));

        // call: prefix
        let res4 = sample_res("call:read_file{\"path\": \"foo.rs\"}", None);
        assert!(matches!(
            verify_tool_response(&req, &res4),
            Err(ToolRejection::PseudoXmlDetected(_))
        ));
    }

    #[test]
    fn test_verify_tool_response_hallucinated_refusals() {
        let req = sample_req(true);

        // Case A: Courteous apology accompanied by legitimate tool calls MUST PASS
        let res_courteous = sample_res(
            "I apologize for the confusion, let me inspect the directory.",
            Some(json!([
                {
                    "id": "c1",
                    "type": "function",
                    "function": { "name": "read_file", "arguments": "{}" }
                }
            ])),
        );
        assert!(
            verify_tool_response(&req, &res_courteous).is_ok(),
            "Polite apology accompanied by valid tool calls should NOT be rejected"
        );

        let res_courteous_sorry = sample_res(
            "I am sorry about that error, running the tool now.",
            Some(json!([
                {
                    "id": "c1",
                    "type": "function",
                    "function": { "name": "read_file", "arguments": "{}" }
                }
            ])),
        );
        assert!(
            verify_tool_response(&req, &res_courteous_sorry).is_ok(),
            "Polite 'I am sorry' accompanied by valid tool calls should NOT be rejected"
        );

        // Case B: Explicit tool refusal while tool calls exist MUST BE REJECTED
        let res_contra = sample_res(
            "I refuse to use tools and cannot execute tools.",
            Some(json!([
                {
                    "id": "c1",
                    "type": "function",
                    "function": { "name": "read_file", "arguments": "{}" }
                }
            ])),
        );
        assert!(matches!(
            verify_tool_response(&req, &res_contra),
            Err(ToolRejection::HallucinatedRefusal(_))
        ));

        // Case C: Model text claims it cannot access tools/execute commands when tools provided
        let res_tool_refusal = sample_res(
            "I do not have access to tools or the ability to execute commands in this environment.",
            None,
        );
        assert!(matches!(
            verify_tool_response(&req, &res_tool_refusal),
            Err(ToolRejection::HallucinatedRefusal(_))
        ));

        // Normal text response without tool calls passes
        let res_plain_answer = sample_res("Paris is the capital of France.", None);
        assert!(verify_tool_response(&req, &res_plain_answer).is_ok());
    }

    #[test]
    fn test_verify_tool_response_corrupted() {
        let req = sample_req(true);

        // Empty choices
        let mut res_no_choices = sample_res("Hello", None);
        res_no_choices.choices.clear();
        assert!(matches!(
            verify_tool_response(&req, &res_no_choices),
            Err(ToolRejection::CorruptedOutput(_))
        ));

        // All fields blank
        let res_blank = sample_res("   ", None);
        assert!(matches!(
            verify_tool_response(&req, &res_blank),
            Err(ToolRejection::CorruptedOutput(_))
        ));
    }
}

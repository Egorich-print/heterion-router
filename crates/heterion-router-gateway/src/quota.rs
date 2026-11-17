//! What is left of a provider's quota — measured, not declared.
//!
//! A provider rarely publishes a quota endpoint, but almost every answer
//! carries a trace of it: the rate-limit headers, the `retry-after`, or the
//! text of the refusal itself. agentrouter says it outright — «user quota:
//! $0.004456, need quota: $0.032528» — and that sentence is the whole
//! remaining budget of the account.
//!
//! The reading here is therefore assembled from what the platform said:
//! the headers first, the body second. A provider that says nothing
//! conclusive yields `unknown`, not a guess.

use std::time::{SystemTime, UNIX_EPOCH};

/// What a measurement says about the provider's quota.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuotaState {
    /// The platform answered and the quota is not spent.
    Available,
    /// The quota is spent — the platform refuses for that reason.
    Exhausted,
    /// The account has no money; the platform asks for credits.
    CreditsRequired,
    /// The platform refused the credential itself.
    NeedsKey,
    /// The answer says nothing about the quota.
    Unknown,
}

impl QuotaState {
    /// Stable string for logs, the API and the database.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Available => "available",
            Self::Exhausted => "exhausted",
            Self::CreditsRequired => "credits_required",
            Self::NeedsKey => "needs_key",
            Self::Unknown => "unknown",
        }
    }
}

/// One measurement of a provider's quota.
#[derive(Debug, Clone, PartialEq)]
pub struct QuotaReading {
    /// What the measurement says.
    pub state: QuotaState,
    /// Which window it describes (`credits`, `rate_limit`, `daily`, …).
    pub window_key: String,
    /// How much of the window is left, in percent, when the platform says.
    pub remaining_percentage: Option<f64>,
    /// When the window resets, when the platform says.
    pub next_reset_at: Option<String>,
    /// The window length, when the platform publishes it.
    pub window_duration_ms: Option<i64>,
    /// The platform's own words, for the operator.
    pub detail: String,
}

impl QuotaReading {
    /// A reading that says nothing.
    fn unknown(detail: impl Into<String>) -> Self {
        Self {
            state: QuotaState::Unknown,
            window_key: "unknown".to_string(),
            remaining_percentage: None,
            next_reset_at: None,
            window_duration_ms: None,
            detail: detail.into(),
        }
    }
}

/// Read the quota out of one upstream answer.
///
/// The headers come first: a platform that publishes `x-ratelimit-remaining`
/// and `x-ratelimit-limit` says exactly how much of the window is left. The
/// body is the fallback, and the *status* is the last word — a body that
/// names a quota outranks a `403`, because agentrouter refuses an empty
/// account with `403`, not with `402`.
pub fn reading_from_response(
    status: u16,
    headers: &[(String, String)],
    body: &str,
) -> QuotaReading {
    let lowered = body.to_ascii_lowercase();
    let header = |name: &str| {
        headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    };

    let remaining = header("x-ratelimit-remaining-requests")
        .or_else(|| header("x-ratelimit-remaining-tokens"))
        .or_else(|| header("x-ratelimit-remaining"))
        .or_else(|| header("ratelimit-remaining"))
        .and_then(parse_number);
    let limit = header("x-ratelimit-limit-requests")
        .or_else(|| header("x-ratelimit-limit-tokens"))
        .or_else(|| header("x-ratelimit-limit"))
        .or_else(|| header("ratelimit-limit"))
        .and_then(parse_number);

    let reset = header("x-ratelimit-reset")
        .or_else(|| header("ratelimit-reset"))
        .map(|value| value.to_string())
        .or_else(|| {
            header("retry-after")
                .and_then(parse_number)
                .map(|seconds| (now_unix() as i64 + seconds as i64).to_string())
        });

    // The body of a refusal sometimes names the budget outright.
    let body_remaining = number_after(&lowered, &["user quota:", "remaining quota:", "quota:"]);
    let body_needed = number_after(&lowered, &["need quota:", "required quota:", "needed:"]);

    let mut reading = match (body_remaining, body_needed) {
        (Some(left), Some(needed)) if needed > 0.0 => QuotaReading {
            state: if left < needed {
                QuotaState::Exhausted
            } else {
                QuotaState::Available
            },
            window_key: "credits".to_string(),
            remaining_percentage: Some((left / needed * 100.0).clamp(0.0, 100.0)),
            next_reset_at: reset.clone(),
            window_duration_ms: None,
            detail: format!("quota {left} of {needed}"),
        },
        _ => match status {
            200..=299 => QuotaReading {
                state: QuotaState::Available,
                window_key: "rate_limit".to_string(),
                remaining_percentage: percentage(remaining, limit),
                next_reset_at: reset.clone(),
                window_duration_ms: None,
                detail: "answered".to_string(),
            },
            402 => QuotaReading {
                state: QuotaState::CreditsRequired,
                window_key: "credits".to_string(),
                remaining_percentage: percentage(remaining, limit),
                next_reset_at: reset.clone(),
                window_duration_ms: None,
                detail: excerpt(body),
            },
            429 => QuotaReading {
                state: QuotaState::Exhausted,
                window_key: "rate_limit".to_string(),
                remaining_percentage: percentage(remaining, limit),
                next_reset_at: reset.clone(),
                window_duration_ms: None,
                detail: excerpt(body),
            },
            401 | 403 if lowered.contains("quota") => QuotaReading {
                state: QuotaState::Exhausted,
                window_key: "credits".to_string(),
                remaining_percentage: percentage(remaining, limit),
                next_reset_at: reset.clone(),
                window_duration_ms: None,
                detail: excerpt(body),
            },
            401 | 403 => QuotaReading {
                state: QuotaState::NeedsKey,
                window_key: "credential".to_string(),
                remaining_percentage: percentage(remaining, limit),
                next_reset_at: reset.clone(),
                window_duration_ms: None,
                detail: excerpt(body),
            },
            _ => QuotaReading::unknown(excerpt(body)),
        },
    };

    // A daily allocation is a window of its own: it resets at midnight, not
    // when the counter does.
    if lowered.contains("daily") && reading.window_key == "rate_limit" {
        reading.window_key = "daily".to_string();
    }
    reading
}

/// The remaining share of a window, when both ends of it are published.
fn percentage(remaining: Option<f64>, limit: Option<f64>) -> Option<f64> {
    match (remaining, limit) {
        (Some(left), Some(total)) if total > 0.0 => Some((left / total * 100.0).clamp(0.0, 100.0)),
        _ => None,
    }
}

/// The first number that follows one of the given markers.
///
/// The platform puts a currency sign and a space between the marker and the
/// number («user quota: $0.004456»), so the scan skips to the first digit
/// rather than expecting one right away.
fn number_after(body: &str, markers: &[&str]) -> Option<f64> {
    for marker in markers {
        let Some(position) = body.find(marker) else {
            continue;
        };
        let tail = &body[position + marker.len()..];
        let Some(start) = tail.find(|character: char| character.is_ascii_digit()) else {
            continue;
        };
        let digits: String = tail[start..]
            .chars()
            .take_while(|character| character.is_ascii_digit() || *character == '.')
            .collect();
        if let Ok(value) = digits.parse::<f64>() {
            return Some(value);
        }
    }
    None
}

/// A number in a header value.
fn parse_number(value: &str) -> Option<f64> {
    let trimmed = value.trim();
    let digits: String = trimmed
        .chars()
        .take_while(|character| character.is_ascii_digit() || *character == '.')
        .collect();
    digits.parse::<f64>().ok()
}

/// A short excerpt of the platform's own words.
fn excerpt(body: &str) -> String {
    let trimmed = body.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    trimmed.chars().take(240).collect()
}

/// Seconds since the unix epoch.
fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header(name: &str, value: &str) -> (String, String) {
        (name.to_string(), value.to_string())
    }

    #[test]
    fn the_body_of_a_refusal_names_the_remaining_budget() {
        let body = r#"{"error":{"message":"pre-consume quota failed, user quota: $0.004456, need quota: $0.032528"}}"#;
        let reading = reading_from_response(403, &[], body);
        assert_eq!(reading.state, QuotaState::Exhausted);
        assert_eq!(reading.window_key, "credits");
        let percentage = reading.remaining_percentage.expect("a percentage");
        assert!(
            (13.0..14.0).contains(&percentage),
            "expected ~13.7% left, got {percentage}"
        );
    }

    #[test]
    fn a_402_asks_for_credits() {
        let reading = reading_from_response(402, &[], "Add usage credits or subscribe");
        assert_eq!(reading.state, QuotaState::CreditsRequired);
        assert_eq!(reading.window_key, "credits");
    }

    #[test]
    fn a_429_is_an_exhausted_window() {
        let reading = reading_from_response(429, &[], "rate limit reached");
        assert_eq!(reading.state, QuotaState::Exhausted);
        assert_eq!(reading.window_key, "rate_limit");
    }

    #[test]
    fn a_401_is_a_credential_problem_not_a_quota_problem() {
        let reading = reading_from_response(401, &[], "Incorrect API key provided");
        assert_eq!(reading.state, QuotaState::NeedsKey);
    }

    #[test]
    fn rate_limit_headers_give_the_remaining_share() {
        let headers = vec![
            header("x-ratelimit-remaining-requests", "42"),
            header("x-ratelimit-limit-requests", "100"),
        ];
        let reading = reading_from_response(200, &headers, "");
        assert_eq!(reading.state, QuotaState::Available);
        assert_eq!(reading.remaining_percentage, Some(42.0));
    }

    #[test]
    fn a_retry_after_becomes_a_reset_moment() {
        let headers = vec![header("retry-after", "30")];
        let reading = reading_from_response(429, &headers, "slow down");
        let reset = reading.next_reset_at.expect("a reset moment");
        let reset: i64 = reset.parse().expect("a unix timestamp");
        assert!(reset > now_unix() as i64 + 20, "unexpected reset: {reset}");
    }

    #[test]
    fn a_daily_allocation_is_its_own_window() {
        let body = "you have used up your daily free allocation of 10,000 neurons";
        let reading = reading_from_response(429, &[], body);
        assert_eq!(reading.window_key, "daily");
        assert_eq!(reading.state, QuotaState::Exhausted);
    }

    #[test]
    fn a_silent_answer_is_unknown() {
        let reading = reading_from_response(500, &[], "");
        assert_eq!(reading.state, QuotaState::Unknown);
    }
}

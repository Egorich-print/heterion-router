//! Per-provider limits, measured against live upstreams.
//!
//! A request that leaves the router carries what the client asked for. Some
//! platforms accept a narrower set than the OpenAI one, and the whole request
//! dies when the difference shows up: the provider answers `400` and the
//! ladder moves on to the next candidate. The facts here were measured on
//! the live run of 2026-11-16 (100 subagents, MiniMax orchestrator) and are
//! only widened by a measurement, never by a guess.
//!
//! Every rule here is a *cap*, not a policy: the router does not decide what
//! the client wants, it only keeps the request inside what the platform can
//! serve. What the client asked for stays in the journal.

/// The `reasoning_effort` values a provider accepts, when its set is narrower
/// than the OpenAI one.
///
/// groq answers `400 invalid_request_error: reasoning_effort must be one of
/// `low`, `medium`, or `high`` — a client that sends `xhigh` or `max` loses
/// the whole turn (measured live 2026-11-16, 50 refusals on
/// `openai/gpt-oss-120b` and `openai/gpt-oss-20b`).
pub fn allowed_efforts(provider: &str) -> Option<&'static [&'static str]> {
    match provider {
        "groq" => Some(&["low", "medium", "high"]),
        _ => None,
    }
}

/// Models whose upstream refuses the `reasoning_effort` field outright.
///
/// groq answers `400 invalid_request_error: invalid Qwen3.8
/// reasoning_effort` for its Qwen3.8 models — the field is not merely
/// misvalued there, it is not part of that model's request shape (measured
/// live 2026-11-16, 11 refusals on `qwen/qwen3.8-27b`).
pub fn rejects_reasoning_effort_field(provider: &str, model: &str) -> bool {
    provider == "groq" && model.to_ascii_lowercase().contains("qwen3.8")
}

/// Providers whose API refuses an assistant message that carries
/// `reasoning_content`.
///
/// The field is the model's own thinking from an earlier turn, echoed back by
/// the client with the history. groq answers `400 invalid_request_error:
/// 'messages.11' : for 'role:assistant' the following must be satisfied[(
/// 'messages.11' : property 'reasoning_content' is unsupported)]`, oneminai
/// answers `400 Unrecognized key: "reasoning_content"`, and Mistral answers
/// `422 extra_forbidden` with the path `body.messages.6.assistant.
/// reasoning_content` (measured live 2026-11-16, 176 refusals between the
/// three).
///
/// The visible text of the assistant turn is not touched — only the thinking
/// sidecar, which the platform refuses to accept in the history.
pub fn rejects_reasoning_content(provider: &str) -> bool {
    matches!(provider, "groq" | "oneminai" | "mistral")
}

/// The largest `max_tokens` / `max_completion_tokens` a provider accepts.
///
/// typhoon answers `400 max_completion_tokens must be at most 16384` and
/// oneminai answers `400 Too big: expected number to be <=32768` (measured
/// live 2026-11-16, 34 refusals between the two).
///
/// The limit can be a property of the model rather than of the platform:
/// sealion's 32B model answers `400 max_completion_tokens=65536 cannot be
/// greater than max_model_len=32000` while its 27B model takes the same
/// budget happily. Such a model is named here, and the cap applies to that
/// model only.
pub fn max_output_tokens(provider: &str, model: &str) -> Option<u32> {
    match provider {
        "typhoon" => Some(16_384),
        "oneminai" => Some(32_768),
        "sealion" if model.contains("SEA-LION-v4-32B") => Some(32_000),
        _ => None,
    }
}

/// The reasoning levels OpenAI's surface knows, from the weakest to the
/// strongest.
///
/// A provider with a narrower set gets the closest level that is not stronger
/// than the one the client asked for.
const EFFORT_LADDER: [&str; 6] = ["minimal", "low", "medium", "high", "xhigh", "max"];

/// The effort to send, given what the client asked for and what the provider
/// accepts.
///
/// - a value the provider accepts rides along unchanged;
/// - a stronger value (`xhigh`, `max`) is pulled down to the strongest level
///   the provider has;
/// - a weaker value (`minimal`, `none`) is lifted to the weakest level the
///   provider has — the platform offers nothing below it;
/// - a value outside the ladder is dropped: the provider's own default is a
///   better guess than a wrong guess.
///
/// `None` means "send no effort at all".
pub fn clamp_effort(requested: Option<&str>, allowed: &[&str]) -> Option<String> {
    let requested = requested?;
    let requested_rank = EFFORT_LADDER
        .iter()
        .position(|level| *level == requested)
        .or_else(|| if requested == "none" { Some(0) } else { None })?;
    // The allowed set, ranked the same way.
    let mut ranked: Vec<(usize, &str)> = allowed
        .iter()
        .filter_map(|level| {
            EFFORT_LADDER
                .iter()
                .position(|known| known == level)
                .map(|rank| (rank, *level))
        })
        .collect();
    ranked.sort();
    if ranked.is_empty() {
        return None;
    }
    // The strongest allowed level that is not stronger than the request.
    let fit = ranked
        .iter()
        .rev()
        .find(|(rank, _)| *rank <= requested_rank)
        .or_else(|| ranked.first());
    fit.map(|(_, level)| (*level).to_string())
}

#[cfg(test)]
mod tests {
    use super::allowed_efforts;
    use super::clamp_effort;
    use super::max_output_tokens;
    use super::rejects_reasoning_content;
    use super::rejects_reasoning_effort_field;

    /// groq accepts three levels; a stronger request is pulled down to the
    /// strongest of them.
    #[test]
    fn a_stronger_effort_is_pulled_down_to_the_provider_ceiling() {
        let allowed = allowed_efforts("groq").expect("groq has its own set");
        assert_eq!(
            clamp_effort(Some("xhigh"), allowed).as_deref(),
            Some("high")
        );
        assert_eq!(clamp_effort(Some("max"), allowed).as_deref(), Some("high"));
    }

    /// A level the provider accepts rides along unchanged.
    #[test]
    fn an_accepted_effort_is_left_alone() {
        let allowed = allowed_efforts("groq").expect("groq has its own set");
        assert_eq!(clamp_effort(Some("low"), allowed).as_deref(), Some("low"));
        assert_eq!(
            clamp_effort(Some("medium"), allowed).as_deref(),
            Some("medium")
        );
        assert_eq!(clamp_effort(Some("high"), allowed).as_deref(), Some("high"));
    }

    /// A request below the provider's weakest level is lifted to it: the
    /// platform offers nothing below, and sending nothing would leave the
    /// client with the provider default instead of the level it asked for.
    #[test]
    fn a_weaker_effort_is_lifted_to_the_provider_floor() {
        let allowed = allowed_efforts("groq").expect("groq has its own set");
        assert_eq!(
            clamp_effort(Some("minimal"), allowed).as_deref(),
            Some("low")
        );
        assert_eq!(clamp_effort(Some("none"), allowed).as_deref(), Some("low"));
    }

    /// A value the router does not know is dropped rather than guessed.
    #[test]
    fn an_unknown_effort_is_dropped() {
        let allowed = allowed_efforts("groq").expect("groq has its own set");
        assert_eq!(clamp_effort(Some("extreme"), allowed), None);
        assert_eq!(clamp_effort(None, allowed), None);
    }

    /// A provider without its own set keeps the client's value untouched.
    #[test]
    fn a_provider_without_a_set_is_not_touched() {
        assert!(allowed_efforts("gemini").is_none());
        assert!(allowed_efforts("pollinations").is_none());
    }

    /// groq refuses the field outright for its Qwen3.8 models.
    #[test]
    fn groq_refuses_the_field_for_qwen38() {
        assert!(rejects_reasoning_effort_field("groq", "qwen/qwen3.8-27b"));
        assert!(rejects_reasoning_effort_field("groq", "qwen3.8-27b"));
        assert!(!rejects_reasoning_effort_field(
            "groq",
            "openai/gpt-oss-120b"
        ));
        assert!(!rejects_reasoning_effort_field(
            "gemini",
            "gemini-3.5-flash"
        ));
    }

    /// The providers that refuse `reasoning_content` in the history.
    #[test]
    fn groq_oneminai_and_mistral_refuse_reasoning_content() {
        assert!(rejects_reasoning_content("groq"));
        assert!(rejects_reasoning_content("oneminai"));
        assert!(rejects_reasoning_content("mistral"));
        assert!(!rejects_reasoning_content("gemini"));
        assert!(!rejects_reasoning_content("pollinations"));
    }

    /// The output caps the platforms publish.
    #[test]
    fn the_output_caps_are_the_measured_ones() {
        assert_eq!(
            max_output_tokens("typhoon", "typhoon-v2.5-30b-a3b-instruct"),
            Some(16_384)
        );
        assert_eq!(max_output_tokens("oneminai", "gpt-4o-mini"), Some(32_768));
        assert_eq!(max_output_tokens("gemini", "gemini-3.5-flash"), None);
    }

    /// A cap that belongs to one model does not touch its neighbour.
    #[test]
    fn a_model_cap_does_not_reach_the_other_model_of_the_provider() {
        assert_eq!(
            max_output_tokens("sealion", "aisingapore/Qwen-SEA-LION-v4-32B-IT"),
            Some(32_000)
        );
        assert_eq!(
            max_output_tokens("sealion", "aisingapore/Qwen-SEA-LION-v4.5-27B-IT"),
            None
        );
    }
}

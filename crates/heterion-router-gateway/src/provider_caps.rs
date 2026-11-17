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
///
/// The set is sometimes the **model's**, not the platform's: Cloudflare
/// Workers AI routes to different engines behind the same endpoint, and each
/// engine names its own levels — `gpt-oss` on Harmony takes `high`, `medium`,
/// `low` («reasoning_effort='max' is not supported by Harmony. Supported
/// values are: high, medium, low» — 54 refusals on 2026-11-17), while
/// `qwen3.8` takes `xhigh`, `medium`, `low` and rejects `high` outright
/// («Unexpected reasoning effort high. Supported types are xhigh (default),
/// medium, and low» — 60 refusals the same day). The same Qwen3.8 engine
/// behind sealion answers with that identical wording («Unexpected reasoning
/// effort max. Supported types are xhigh (default), medium, and low» — 8
/// refusals), so the rule names the model id, not the platform.
pub fn allowed_efforts(provider: &str, model: &str) -> Option<&'static [&'static str]> {
    let model = model.to_ascii_lowercase();
    match provider {
        "groq" => Some(&["low", "medium", "high"]),
        // oneminai knows every OpenAI level except `max` (probed live
        // 2026-11-16: "Invalid option: expected one of none|minimal|low|
        // medium|high|xhigh").
        "oneminai" => Some(&["none", "minimal", "low", "medium", "high", "xhigh"]),
        // pollinations passes the body to an OpenAI-shaped deserializer that
        // knows five levels only (measured live 2026-11-16: "reasoning_effort:
        // unknown variant `max`, expected one of `none`, `high`, `medium`,
        // `low`, `minimal`" — 114 refusals).
        "pollinations" => Some(&["none", "minimal", "low", "medium", "high"]),
        // Cloudflare Workers AI: the engine behind the id names the levels.
        "cloudflare-ai" if model.contains("qwen3.8") => Some(&["low", "medium", "xhigh"]),
        "cloudflare-ai" if model.contains("gpt-oss") => Some(&["low", "medium", "high"]),
        // The same Qwen3.8 engine behind sealion.
        "sealion" if model.contains("sea-lion-v4-32b") => Some(&["low", "medium", "xhigh"]),
        _ => None,
    }
}

/// Models whose upstream refuses the `reasoning_effort` field outright.
///
/// groq answers `400 invalid_request_error: invalid Qwen3.8
/// reasoning_effort` for its Qwen3.8 models — the field is not merely
/// misvalued there, it is not part of that model's request shape (measured
/// live 2026-11-16, 11 refusals on `qwen/qwen3.8-27b`).
///
/// ovhcloud answers `400 {"message":"feature 'reasoning_effort for this
/// model' is not currently supported"}` for `Meta-Llama-3_3-70B-Instruct`
/// (measured live 2026-11-17): the field is not part of that model's request
/// shape at all, so it is removed instead of being valued.
pub fn rejects_reasoning_effort_field(provider: &str, model: &str) -> bool {
    let model = model.to_ascii_lowercase();
    (provider == "groq" && model.contains("qwen3.8"))
        || (provider == "ovhcloud" && model.contains("meta-llama-3_3-70b"))
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
/// budget happily, and oneminai's `gpt-4o-mini` answers `400 Output limit
/// exceeds the model maximum of 16384` — a lower ceiling than the platform's
/// own. Such a model is named here, and the cap applies to that model only.
pub fn max_output_tokens(provider: &str, model: &str) -> Option<u32> {
    match provider {
        "typhoon" => Some(16_384),
        "oneminai" if model == "gpt-4o-mini" => Some(16_384),
        "oneminai" => Some(32_768),
        // groq's Qwen3.8 answers "max_completion_tokens must be less than or
        // equal to 16384, the maximum value for max_completion_tokens is
        // less than the context_window for this model" (measured live
        // 2026-11-16).
        "groq" if model.contains("qwen3.8") => Some(16_384),
        "sealion" if model.contains("SEA-LION-v4-32B") => Some(32_000),
        _ => None,
    }
}

/// The largest prompt a provider can take in one call.
///
/// groq answers `413 Request too large ... on tokens per minute (TPM): Limit
/// 8000, Requested 96330, please reduce your message size and try again` —
/// the account's whole per-minute token budget is smaller than the request,
/// so a prompt above the ceiling can never be served, however long we wait
/// (measured live 2026-11-16, 924 refusals on groq alone).
///
/// The router skips such a candidate instead of paying for the refusal: the
/// ladder moves on to a provider that can take the prompt.
pub fn max_prompt_tokens(provider: &str) -> Option<u32> {
    match provider {
        "groq" => Some(8_000),
        _ => None,
    }
}

/// The largest request a platform accepts, counted in **characters**.
///
/// Some platforms count characters, not tokens, and sum the history with the
/// tool schemas and the response format. oneminai answers `400
/// invalid_request_error: Messages, tools and response format exceed the
/// 100,000 character limit` (measured live 2026-11-17: 2 refusals in the agent
/// run, plus a probe — ~110 000 characters of tool schemas alone were refused
/// the same way).
///
/// The same platform caps a single message: `400 Too big: expected string to
/// have <=100000 characters` with the path `messages.0.content`.
///
/// The history is cut first (the oldest turns go, as for typhoon). When even
/// that is not enough — one huge turn or the tool schemas alone — the ladder
/// skips the candidate instead of paying for the refusal.
pub fn max_payload_chars(provider: &str) -> Option<usize> {
    match provider {
        "oneminai" => Some(100_000),
        _ => None,
    }
}

/// The largest number of messages a provider's API accepts in one request.
///
/// typhoon answers `422 {"detail":[{"type":"too_long","loc":["body",
/// "messages"],"msg":"List should have at most 50 items after validation, not
/// 69"}]}` — the platform validates the history length itself and refuses the
/// whole request (measured live 2026-11-16, 7 refusals in ten minutes of the
/// agent run).
///
/// The history is cut, not the request refused: the conversation keeps its
/// oldest turn and the platform gets a list it can validate. A cut history is
/// a smaller ask than a failed request.
pub fn max_messages(provider: &str) -> Option<usize> {
    match provider {
        "typhoon" => Some(50),
        _ => None,
    }
}

/// Whether the platform's schema wants the message content as plain text.
///
/// Cloudflare Workers AI validates the body against its own schema before the
/// model sees it, and answers `400 AiError: Bad input: Error: oneOf at '/' not
/// met, 0 matches: ... Type mismatch of '/messages/0/content', 'array' not in
/// 'string'` — a content array of parts is not a string there, and the schema
/// wants one shape for the whole list (measured live 2026-11-16, 326 refusals
/// across the `@cf/*` models).
///
/// The same answer names the second half of the rule: `required properties at
/// '/messages/5' are 'role,content'` — a turn without content (an assistant
/// turn that carries only tool calls) must still have the field.
pub fn wants_string_content(provider: &str) -> bool {
    provider == "cloudflare-ai"
}

/// The reasoning levels OpenAI's surface knows, from the weakest to the
/// strongest.
///
/// A provider with a narrower set gets the closest level that is not stronger
/// than the one the client asked for.
const EFFORT_LADDER: [&str; 7] = ["none", "minimal", "low", "medium", "high", "xhigh", "max"];

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
    let requested_rank = EFFORT_LADDER.iter().position(|level| *level == requested)?;
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
    use super::max_messages;
    use super::max_output_tokens;
    use super::max_payload_chars;
    use super::max_prompt_tokens;
    use super::rejects_reasoning_content;
    use super::rejects_reasoning_effort_field;
    use super::wants_string_content;

    /// groq accepts three levels; a stronger request is pulled down to the
    /// strongest of them.
    #[test]
    fn a_stronger_effort_is_pulled_down_to_the_provider_ceiling() {
        let allowed = allowed_efforts("groq", "openai/gpt-oss-120b").expect("groq has its own set");
        assert_eq!(
            clamp_effort(Some("xhigh"), allowed).as_deref(),
            Some("high")
        );
        assert_eq!(clamp_effort(Some("max"), allowed).as_deref(), Some("high"));
    }

    /// A level the provider accepts rides along unchanged.
    #[test]
    fn an_accepted_effort_is_left_alone() {
        let allowed = allowed_efforts("groq", "openai/gpt-oss-120b").expect("groq has its own set");
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
        let allowed = allowed_efforts("groq", "openai/gpt-oss-120b").expect("groq has its own set");
        assert_eq!(
            clamp_effort(Some("minimal"), allowed).as_deref(),
            Some("low")
        );
        assert_eq!(clamp_effort(Some("none"), allowed).as_deref(), Some("low"));
    }

    /// A value the router does not know is dropped rather than guessed.
    #[test]
    fn an_unknown_effort_is_dropped() {
        let allowed = allowed_efforts("groq", "openai/gpt-oss-120b").expect("groq has its own set");
        assert_eq!(clamp_effort(Some("extreme"), allowed), None);
        assert_eq!(clamp_effort(None, allowed), None);
    }

    /// A provider without its own set keeps the client's value untouched.
    #[test]
    fn a_provider_without_a_set_is_not_touched() {
        assert!(allowed_efforts("gemini", "gemini-3.5-flash").is_none());
        assert!(allowed_efforts("kilocode", "stealth/glyph-cluster").is_none());
    }

    /// pollinations knows five levels: none, minimal, low, medium, high.
    /// A stronger request is pulled down to the strongest of them.
    #[test]
    fn pollinations_knows_five_levels() {
        let allowed =
            allowed_efforts("pollinations", "openai-fast").expect("pollinations has its own set");
        assert_eq!(clamp_effort(Some("max"), allowed).as_deref(), Some("high"));
        assert_eq!(
            clamp_effort(Some("xhigh"), allowed).as_deref(),
            Some("high")
        );
        assert_eq!(clamp_effort(Some("low"), allowed).as_deref(), Some("low"));
        assert_eq!(clamp_effort(Some("none"), allowed).as_deref(), Some("none"));
    }

    /// typhoon validates the length of the history itself.
    #[test]
    fn typhoon_takes_at_most_fifty_messages() {
        assert_eq!(max_messages("typhoon"), Some(50));
        assert_eq!(max_messages("gemini"), None);
    }

    /// Cloudflare's schema wants plain text in the content field.
    #[test]
    fn cloudflare_wants_plain_text() {
        assert!(wants_string_content("cloudflare-ai"));
        assert!(!wants_string_content("gemini"));
        assert!(!wants_string_content("pollinations"));
    }

    /// Cloudflare's Qwen3.8 engine takes xhigh, medium and low — and refuses
    /// `high` outright ("Unexpected reasoning effort high. Supported types are
    /// xhigh (default), medium, and low", measured live 2026-11-17).
    #[test]
    fn cloudflare_qwen38_takes_xhigh_medium_low() {
        let allowed = allowed_efforts("cloudflare-ai", "@cf/qwen/qwen3.8-27b")
            .expect("the engine has its own set");
        assert_eq!(clamp_effort(Some("max"), allowed).as_deref(), Some("xhigh"));
        assert_eq!(
            clamp_effort(Some("xhigh"), allowed).as_deref(),
            Some("xhigh")
        );
        // `high` is stronger than the platform's `medium`, so it is pulled
        // down to the strongest level the engine has.
        assert_eq!(
            clamp_effort(Some("high"), allowed).as_deref(),
            Some("medium")
        );
        assert_eq!(clamp_effort(Some("low"), allowed).as_deref(), Some("low"));
    }

    /// Cloudflare's gpt-oss runs on Harmony, which takes high, medium and low
    /// ("reasoning_effort='max' is not supported by Harmony. Supported values
    /// are: high, medium, low", measured live 2026-11-17).
    #[test]
    fn cloudflare_gpt_oss_takes_high_medium_low() {
        let allowed = allowed_efforts("cloudflare-ai", "@cf/openai/gpt-oss-120b")
            .expect("the engine has its own set");
        assert_eq!(clamp_effort(Some("max"), allowed).as_deref(), Some("high"));
        assert_eq!(clamp_effort(Some("high"), allowed).as_deref(), Some("high"));
        assert_eq!(clamp_effort(Some("low"), allowed).as_deref(), Some("low"));
    }

    /// The same Qwen3.8 engine behind sealion answers with that identical
    /// wording ("Unexpected reasoning effort max. Supported types are xhigh
    /// (default), medium, and low", measured live 2026-11-17).
    #[test]
    fn sealion_32b_takes_xhigh_medium_low() {
        let allowed = allowed_efforts("sealion", "aisingapore/Qwen-SEA-LION-v4-32B-IT")
            .expect("the engine has its own set");
        assert_eq!(clamp_effort(Some("max"), allowed).as_deref(), Some("xhigh"));
        assert_eq!(
            clamp_effort(Some("high"), allowed).as_deref(),
            Some("medium")
        );
    }

    /// ovhcloud's Llama does not have the field at all ("feature
    /// 'reasoning_effort for this model' is not currently supported", measured
    /// live 2026-11-17).
    #[test]
    fn ovhcloud_llama_refuses_the_field_entirely() {
        assert!(rejects_reasoning_effort_field(
            "ovhcloud",
            "Meta-Llama-3_3-70B-Instruct"
        ));
        assert!(!rejects_reasoning_effort_field(
            "ovhcloud",
            "Mistral-Small-3.2-24B-Instruct-2506"
        ));
    }

    /// oneminai counts the request in characters.
    #[test]
    fn oneminai_counts_characters() {
        assert_eq!(max_payload_chars("oneminai"), Some(100_000));
        assert_eq!(max_payload_chars("gemini"), None);
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

    /// oneminai knows every level except `max`.
    #[test]
    fn oneminai_knows_every_level_but_max() {
        let allowed = allowed_efforts("oneminai", "gpt-4o-mini").expect("oneminai has its own set");
        assert_eq!(clamp_effort(Some("max"), allowed).as_deref(), Some("xhigh"));
        assert_eq!(
            clamp_effort(Some("xhigh"), allowed).as_deref(),
            Some("xhigh")
        );
        assert_eq!(clamp_effort(Some("none"), allowed).as_deref(), Some("none"));
    }

    /// groq's Qwen3.8 has its own output ceiling.
    #[test]
    fn groq_qwen38_has_its_own_output_ceiling() {
        assert_eq!(max_output_tokens("groq", "qwen/qwen3.8-27b"), Some(16_384));
        // Its neighbours keep the context-based rule.
        assert_eq!(max_output_tokens("groq", "openai/gpt-oss-120b"), None);
    }

    /// The per-minute token budget of the account is a ceiling for one call.
    #[test]
    fn groq_has_a_prompt_ceiling_of_its_budget() {
        assert_eq!(max_prompt_tokens("groq"), Some(8_000));
        assert_eq!(max_prompt_tokens("gemini"), None);
    }

    /// The output caps the platforms publish.
    #[test]
    fn the_output_caps_are_the_measured_ones() {
        assert_eq!(
            max_output_tokens("typhoon", "typhoon-v2.5-30b-a3b-instruct"),
            Some(16_384)
        );
        // gpt-4o-mini has its own, lower ceiling than the platform's.
        assert_eq!(max_output_tokens("oneminai", "gpt-4o-mini"), Some(16_384));
        assert_eq!(max_output_tokens("oneminai", "other-model"), Some(32_768));
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

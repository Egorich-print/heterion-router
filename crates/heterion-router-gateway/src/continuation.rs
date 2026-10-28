//! Adaptation of a request whose last message is an assistant turn.
//!
//! Some providers refuse that shape outright:
//!
//! - Mistral answers `400` with `invalid_request_message_order` (code 3230):
//!   "Expected last role User or Tool (or Assistant with prefix True)".
//! - Gemini answers `400`: "Requests ending with a model turn are not
//!   supported".
//!
//! A client that sends such a request means it as a prefill: the trailing
//! assistant turn is the beginning of an answer the model has to finish. Only
//! Mistral serves that shape natively, and only behind a `prefix` flag the
//! OpenAI surface does not carry, so the router adapts the request for the
//! providers that refuse it — the assistant turn stays in the history and one
//! user turn is appended, asking the model to continue it.
//!
//! The adaptation is deliberately narrow:
//!
//! - it runs for the providers that refuse the shape (Mistral, and every
//!   Gemini-format provider), never for the OpenAI-format ones, where a
//!   verbatim prefill is a feature that hundreds of providers serve today;
//! - it leaves an assistant turn that carries tool calls alone: such a
//!   conversation is missing its tool result, which is malformed for every
//!   provider (the OpenAI API refuses it too);
//! - it does not rewrite the answer. The client's own text stays the
//!   client's: the model answers as it would after any other user turn, and
//!   only the model's own text comes back — the same contract a native
//!   prefill has, where the response carries the new text alone.

use heterion_router_core::ChatMessage;

/// The instruction appended in place of the refused trailing assistant turn.
///
/// It asks for a continuation, because a trailing assistant turn is normally
/// an unfinished answer, and falls back to a fresh answer when the turn the
/// client sent was already complete (the runtime's "answer once more" flow).
pub(crate) const CONTINUATION_INSTRUCTION: &str = concat!(
    "The previous assistant message is your own output. ",
    "Continue it from exactly where it stops, without repeating what is already written. ",
    "If it is already complete, answer the request again instead."
);

/// Does this provider's API refuse a request whose last message is an
/// assistant turn?
///
/// Only the provider's own API counts. A gateway that proxies Mistral models
/// under the OpenAI API (OpenRouter and friends) serves the shape fine, so
/// the decision is made per provider, not per model family.
///
/// The list holds only providers whose refusal was measured live (2026-10-26:
/// `mistral` → 400 code 3230). The registry's second Mistral-API provider
/// (`codestral`, `codestral.mistral.ai`) speaks the same protocol and will
/// refuse the shape too, but it had no credential to check against — so it
/// joins the list only after its own refusal is measured, not before.
pub(crate) fn provider_rejects_trailing_assistant(provider: &str) -> bool {
    matches!(provider, "mistral")
}

/// Adapt a trailing assistant turn, returning the text it carried.
///
/// Returns `None` — and changes nothing — when the last message is not an
/// assistant turn, when that turn carries tool calls, or when it carries no
/// text at all.
pub(crate) fn adapt(messages: &mut Vec<ChatMessage>) -> Option<String> {
    let prefix = trailing_prefill(messages)?;
    messages.push(ChatMessage::plain("user", CONTINUATION_INSTRUCTION));
    Some(prefix)
}

/// The text of a trailing assistant turn that reads as an unfinished prefill.
fn trailing_prefill(messages: &[ChatMessage]) -> Option<String> {
    let last = messages.last()?;
    if last.role != "assistant" {
        return None;
    }
    // An assistant turn with tool calls is a call waiting for its result,
    // not a prefill: no provider accepts it as the last message.
    if last.has_tool_calls() {
        return None;
    }
    let text = last.text();
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return None;
    }
    Some(trimmed.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn messages() -> Vec<ChatMessage> {
        vec![
            ChatMessage::plain("user", "Say exactly: HELLO"),
            ChatMessage::plain("assistant", "The answer starts like this"),
        ]
    }

    #[test]
    fn a_trailing_assistant_turn_gets_a_continuation_turn() {
        let mut turns = messages();
        let prefix = adapt(&mut turns).expect("the turn is adapted");

        assert_eq!(prefix, "The answer starts like this");
        assert_eq!(turns.len(), 3);
        assert_eq!(turns[2].role, "user");
        assert_eq!(turns[2].text(), CONTINUATION_INSTRUCTION);
        assert_eq!(
            turns[1].text(),
            "The answer starts like this",
            "the client's own turn was rewritten"
        );
    }

    #[test]
    fn a_conversation_that_ends_with_a_user_turn_is_untouched() {
        let mut turns = vec![ChatMessage::plain("user", "hello")];
        assert!(adapt(&mut turns).is_none());
        assert_eq!(turns.len(), 1);
    }

    #[test]
    fn an_assistant_turn_with_tool_calls_is_untouched() {
        let mut turns = vec![
            ChatMessage::plain("user", "read the file"),
            ChatMessage {
                role: "assistant".to_string(),
                content: None,
                reasoning_content: None,
                tool_calls: Some(json!([{
                    "id": "call_1",
                    "type": "function",
                    "function": {"name": "read", "arguments": "{}"},
                }])),
                tool_call_id: None,
                name: None,
            },
        ];
        assert!(adapt(&mut turns).is_none());
        assert_eq!(turns.len(), 2);
    }

    #[test]
    fn an_empty_assistant_turn_is_untouched() {
        let mut turns = vec![
            ChatMessage::plain("user", "hello"),
            ChatMessage::plain("assistant", "   "),
        ];
        assert!(adapt(&mut turns).is_none());
        assert_eq!(turns.len(), 2);
    }

    #[test]
    fn only_the_mistral_openai_provider_refuses_the_shape() {
        // Gemini-format providers refuse it too, but they never reach this
        // path: they speak their own protocol and adapt unconditionally.
        assert!(provider_rejects_trailing_assistant("mistral"));
        assert!(!provider_rejects_trailing_assistant("openrouter"));
        assert!(!provider_rejects_trailing_assistant("gemini"));
    }
}

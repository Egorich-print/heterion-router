//! Adaptation of the `developer` system role for providers that refuse it.
//!
//! Agent runtimes (MCode among them) send their system prompt as a message
//! with `role: "developer"` — the role OpenAI introduced for the o-series
//! models. The Mistral API does not know that role and answers `422` with
//! `union_tag_invalid`:
//!
//! ```text
//! Input tag 'developer' found using 'role' does not match any of the
//! expected tags: <Roles.system: 'system'>, <Roles.user: 'user'>,
//! <Roles.assistant: 'assistant'>, <Roles.tool: 'tool'>
//! ```
//!
//! Measured live 2026-11-15 against `mistral/codestral-latest`: the same
//! conversation with `role: "system"` is answered (`200`), with
//! `role: "developer"` it is refused (`422`). That refusal is not a soft
//! failure — it takes the whole Tier-2 workhorse of the Switchyard ladder out
//! of service for every agent request, because every agent runtime sends its
//! prompt that way.
//!
//! The adaptation is deliberately narrow:
//!
//! - it runs only for the providers whose refusal was measured (Mistral's own
//!   API), never for the OpenAI-format gateways that accept the role;
//! - it renames the role and nothing else: the text, the order and the
//!   message count stay exactly as the client sent them. A `developer` turn
//!   means a system prompt, and `system` is the role every provider in the
//!   registry understands;
//! - it is not applied to the other formats: the Responses, Claude and Gemini
//!   translators already map `developer` to their own system shape.

use heterion_router_core::ChatMessage;

/// Does this provider's API refuse a `developer` role?
///
/// Only the provider's own API counts. A gateway that proxies Mistral models
/// under the OpenAI API (OpenRouter and friends) serves the role fine, so the
/// decision is made per provider, not per model family.
///
/// The list holds only providers whose refusal was measured live (2026-11-15:
/// `mistral` → 422 `union_tag_invalid`; 2026-11-16: `mixlayer` → 400
/// `messages[0].role: unknown variant 'developer', expected one of 'system',
/// 'user', 'assistant', 'tool'`). The registry's second Mistral-API provider
/// (`codestral`, `codestral.mistral.ai`) speaks the same protocol and will
/// refuse the role too, but it had no credential to check against — so it
/// joins the list only after its own refusal is measured, not before.
pub(crate) fn provider_rejects_developer_role(provider: &str) -> bool {
    matches!(provider, "mistral" | "mixlayer")
}

/// Rename every `developer` turn to a `system` turn.
///
/// Returns how many turns were renamed, so the caller can log the change
/// instead of silently altering the client's conversation.
pub(crate) fn adapt(messages: &mut [ChatMessage]) -> usize {
    let mut renamed = 0;
    for message in messages.iter_mut() {
        if message.role == "developer" {
            message.role = "system".to_string();
            renamed += 1;
        }
    }
    renamed
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn turn(role: &str) -> ChatMessage {
        ChatMessage {
            role: role.to_string(),
            content: Some(json!("text")),
            reasoning_content: None,
            tool_calls: None,
            tool_call_id: None,
            name: None,
        }
    }

    #[test]
    fn a_developer_turn_becomes_a_system_turn() {
        let mut turns = vec![turn("developer"), turn("user"), turn("assistant")];
        assert_eq!(adapt(&mut turns), 1);
        assert_eq!(turns[0].role, "system");
        // Nothing else moves: the conversation stays the client's.
        assert_eq!(turns[1].role, "user");
        assert_eq!(turns[2].role, "assistant");
        assert_eq!(turns[0].content, Some(json!("text")));
    }

    #[test]
    fn a_conversation_without_developer_turns_is_untouched() {
        let mut turns = vec![turn("system"), turn("user")];
        assert_eq!(adapt(&mut turns), 0);
        assert_eq!(turns[0].role, "system");
        assert_eq!(turns[1].role, "user");
    }

    #[test]
    fn every_developer_turn_is_renamed() {
        let mut turns = vec![turn("developer"), turn("developer"), turn("user")];
        assert_eq!(adapt(&mut turns), 2);
        assert!(turns.iter().take(2).all(|turn| turn.role == "system"));
    }

    #[test]
    fn only_the_measured_providers_are_adapted() {
        assert!(provider_rejects_developer_role("mistral"));
        // mixlayer answers 400 "unknown variant `developer`" (measured live
        // 2026-11-16).
        assert!(provider_rejects_developer_role("mixlayer"));
        assert!(!provider_rejects_developer_role("openrouter"));
        assert!(!provider_rejects_developer_role("gemini"));
        assert!(!provider_rejects_developer_role("agentrouter"));
    }
}

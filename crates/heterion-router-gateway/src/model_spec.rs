//! Model specification and capability catalog for NVIDIA Switchyard dynamic routing.
//!
//! Provides static capability detection for context windows, multimodal vision support,
//! tool calling support (native JSON vs pseudo XML vs none), and latency/reasoning tiers.

use serde::{Deserialize, Serialize};

/// Tool calling support capability of a model.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ToolSupport {
    /// Native structured JSON tool calling (OpenAI function/tool calling head).
    Native,
    /// Emits unstructured XML or text `<tool_call>` tags instead of JSON heads.
    /// Must be strictly filtered out from tool-bearing requests.
    PseudoXml,
    /// Model does not support tools (e.g. search-only or pure text models).
    None,
}

/// Execution tier of a model for latency vs deep reasoning tradeoffs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ModelTier {
    /// High-reasoning frontier models suitable for planning and complex debugging.
    FrontierReasoning,
    /// High-throughput, low-latency execution models for fast mechanical tool loops.
    FastExecution,
    /// Generic fallback tier for unspecified models.
    Fallback,
}

/// Specifications and operational capabilities of a language model.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelSpec {
    /// Context window limit in tokens.
    pub context_window: u32,
    /// Whether the model natively accepts multimodal image inputs.
    pub supports_vision: bool,
    /// Tool calling capability.
    pub tool_support: ToolSupport,
    /// Latency / reasoning tier.
    pub tier: ModelTier,
}

/// Returns the capability specification for a given model identifier.
pub fn model_spec(model: &str) -> ModelSpec {
    let lower = model.to_lowercase();
    let bare = lower.rsplit('/').next().unwrap_or(&lower);

    // 1. Pseudo-tool models (Morph, Syn-Pro)
    if is_pseudo_tool_model("", &lower) {
        return ModelSpec {
            context_window: 128_000,
            supports_vision: false,
            tool_support: ToolSupport::PseudoXml,
            tier: ModelTier::Fallback,
        };
    }

    // 2. Search / text-only models without tool heads
    if bare.starts_with("exa") || bare.contains("search") {
        return ModelSpec {
            context_window: 128_000,
            supports_vision: false,
            tool_support: ToolSupport::None,
            tier: ModelTier::Fallback,
        };
    }

    // 3. Gemini models (1M context window, native vision & tools)
    if bare.starts_with("gemini") {
        let tier = if bare.contains("pro") {
            ModelTier::FrontierReasoning
        } else {
            ModelTier::FastExecution
        };
        return ModelSpec {
            context_window: 1_000_000,
            supports_vision: true,
            tool_support: ToolSupport::Native,
            tier,
        };
    }

    // 4. AgentRouter reasoning models: gpt-6.1-sol, gpt-6-astra (512k context)
    if bare.contains("gpt-6.1-sol") || bare.contains("gpt-6-astra") {
        return ModelSpec {
            context_window: 512_000,
            supports_vision: false,
            tool_support: ToolSupport::Native,
            tier: ModelTier::FrontierReasoning,
        };
    }

    // 5. AgentRouter execution models: gpt-5.6-sol (512k context)
    if bare.contains("gpt-5.6-sol") {
        return ModelSpec {
            context_window: 512_000,
            supports_vision: false,
            tool_support: ToolSupport::Native,
            tier: ModelTier::FastExecution,
        };
    }

    // 6. Codestral models (256k context, native tools, fast execution)
    if bare.starts_with("codestral") {
        return ModelSpec {
            context_window: 256_000,
            supports_vision: false,
            tool_support: ToolSupport::Native,
            tier: ModelTier::FastExecution,
        };
    }

    // 7. Gemma models: gemma-4-26b-a4b-it (256k context, native tools, fast execution)
    if bare.starts_with("gemma") {
        return ModelSpec {
            context_window: 256_000,
            supports_vision: false,
            tool_support: ToolSupport::Native,
            tier: ModelTier::FastExecution,
        };
    }

    // 8. DeepSeek models (128k context)
    if bare.starts_with("deepseek") {
        let tier = if bare.contains("reasoner") || bare.contains("r1") {
            ModelTier::FrontierReasoning
        } else {
            ModelTier::FastExecution
        };
        return ModelSpec {
            context_window: 128_000,
            supports_vision: false,
            tool_support: ToolSupport::Native,
            tier,
        };
    }

    // 8. Default fallback for general OpenAI-compatible / unknown models
    ModelSpec {
        context_window: 128_000,
        supports_vision: false,
        tool_support: ToolSupport::Native,
        tier: ModelTier::Fallback,
    }
}

/// Checks whether a provider or model emits unstructured XML pseudo-tools (`<tool_call>`)
/// instead of standard OpenAI structured `tool_calls`.
///
/// Matches all models under provider `morph`, model IDs starting with `morph-`,
/// and models containing `syn-pro` (e.g. `upstage/syn-pro`).
pub fn is_pseudo_tool_model(provider: &str, model: &str) -> bool {
    let p = provider.to_lowercase();
    let m = model.to_lowercase();

    p.contains("morph") || m.contains("morph") || p.contains("syn-pro") || m.contains("syn-pro")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_native_tool_filtering() {
        // Assert pseudo-tool models return true
        assert!(is_pseudo_tool_model("", "morph-v3-large"));
        assert!(is_pseudo_tool_model("morph", "morph-v3-large"));
        assert!(is_pseudo_tool_model("morph", "any-model"));
        assert!(is_pseudo_tool_model("upstage", "syn-pro"));
        assert!(is_pseudo_tool_model("", "upstage/syn-pro"));

        // Assert native tool models return false
        assert!(!is_pseudo_tool_model("", "gpt-5.6-sol"));
        assert!(!is_pseudo_tool_model("agentrouter", "gpt-5.6-sol"));
        assert!(!is_pseudo_tool_model("", "gemini-2.5-pro"));
        assert!(!is_pseudo_tool_model("google", "gemini-2.5-pro"));
        assert!(!is_pseudo_tool_model("", "codestral"));
        assert!(!is_pseudo_tool_model("mistral", "codestral"));
    }

    #[test]
    fn test_model_specs() {
        // Gemini
        let gemini_pro = model_spec("gemini-2.5-pro");
        assert_eq!(gemini_pro.context_window, 1_000_000);
        assert!(gemini_pro.supports_vision);
        assert_eq!(gemini_pro.tool_support, ToolSupport::Native);
        assert_eq!(gemini_pro.tier, ModelTier::FrontierReasoning);

        let gemini_flash = model_spec("google/gemini-2.5-flash");
        assert_eq!(gemini_flash.context_window, 1_000_000);
        assert!(gemini_flash.supports_vision);
        assert_eq!(gemini_flash.tool_support, ToolSupport::Native);
        assert_eq!(gemini_flash.tier, ModelTier::FastExecution);

        // AgentRouter models
        let gpt56 = model_spec("agentrouter/gpt-5.6-sol");
        assert_eq!(gpt56.context_window, 512_000);
        assert!(!gpt56.supports_vision);
        assert_eq!(gpt56.tool_support, ToolSupport::Native);
        assert_eq!(gpt56.tier, ModelTier::FastExecution);

        let gpt61 = model_spec("agentrouter/gpt-6.1-sol");
        assert_eq!(gpt61.context_window, 512_000);
        assert!(!gpt61.supports_vision);
        assert_eq!(gpt61.tool_support, ToolSupport::Native);
        assert_eq!(gpt61.tier, ModelTier::FrontierReasoning);

        let astra = model_spec("gpt-6-astra");
        assert_eq!(astra.context_window, 512_000);
        assert_eq!(astra.tier, ModelTier::FrontierReasoning);

        // Codestral
        let codestral = model_spec("codestral-latest");
        assert_eq!(codestral.context_window, 256_000);
        assert!(!codestral.supports_vision);
        assert_eq!(codestral.tool_support, ToolSupport::Native);
        assert_eq!(codestral.tier, ModelTier::FastExecution);

        // Gemma
        let gemma = model_spec("gemini/gemma-4-26b-a4b-it");
        assert_eq!(gemma.context_window, 256_000);
        assert!(!gemma.supports_vision);
        assert_eq!(gemma.tool_support, ToolSupport::Native);
        assert_eq!(gemma.tier, ModelTier::FastExecution);

        // DeepSeek
        let ds_chat = model_spec("deepseek-chat");
        assert_eq!(ds_chat.context_window, 128_000);
        assert_eq!(ds_chat.tier, ModelTier::FastExecution);

        let ds_reasoner = model_spec("deepseek-reasoner");
        assert_eq!(ds_reasoner.context_window, 128_000);
        assert_eq!(ds_reasoner.tier, ModelTier::FrontierReasoning);

        // Pseudo-tool models
        let morph = model_spec("morph-v3-large");
        assert_eq!(morph.context_window, 128_000);
        assert!(!morph.supports_vision);
        assert_eq!(morph.tool_support, ToolSupport::PseudoXml);

        let syn_pro = model_spec("upstage/syn-pro");
        assert_eq!(syn_pro.context_window, 128_000);
        assert_eq!(syn_pro.tool_support, ToolSupport::PseudoXml);

        // Exa Search
        let exa = model_spec("exa/exa-search");
        assert_eq!(exa.tool_support, ToolSupport::None);
    }
}

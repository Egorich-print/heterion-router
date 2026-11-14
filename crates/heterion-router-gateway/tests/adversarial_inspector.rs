use heterion_router_core::{ChatCompletionRequest, ChatMessage};
use heterion_router_gateway::inspector::{AgentStage, inspect_request, is_image_part};
use heterion_router_gateway::model_spec::{
    ModelTier, ToolSupport, is_pseudo_tool_model, model_spec,
};
use serde_json::json;

#[test]
fn test_case_sensitivity_models_and_providers() {
    // 1. Morph in various cases and formats
    let morph_cases = [
        ("morph-v3-large", ""),
        ("MORPH-V3-LARGE", ""),
        ("Morph-V3-Large", ""),
        ("morph", "morph-v3-large"),
        ("MORPH", "V3-LARGE"),
        ("Morph", "v3-large"),
        ("", "morph-v3-large"),
        ("", "MORPH-V3-LARGE"),
    ];

    for (prov, mdl) in morph_cases {
        assert!(
            is_pseudo_tool_model(prov, mdl),
            "is_pseudo_tool_model should match morph case-insensitively: prov='{prov}', mdl='{mdl}'"
        );
        let formatted = format!("{prov}/{mdl}");
        let model_name = if prov.is_empty() { mdl } else { &formatted };
        let spec = model_spec(model_name);
        assert_eq!(
            spec.tool_support,
            ToolSupport::PseudoXml,
            "Morph model must have PseudoXml tool support: prov='{prov}', mdl='{mdl}'"
        );
    }

    // 2. Upstage Syn-Pro in various cases and formats
    let syn_cases = [
        ("upstage/syn-pro", ""),
        ("upstage/Syn-Pro", ""),
        ("UPSTAGE/SYN-PRO", ""),
        ("UpStage/Syn-Pro", ""),
        ("upstage", "syn-pro"),
        ("UPSTAGE", "SYN-PRO"),
        ("Upstage", "Syn-Pro"),
        ("", "upstage/syn-pro"),
        ("", "UPSTAGE/SYN-PRO"),
    ];

    for (prov, mdl) in syn_cases {
        assert!(
            is_pseudo_tool_model(prov, mdl),
            "is_pseudo_tool_model should match syn-pro case-insensitively: prov='{prov}', mdl='{mdl}'"
        );
        let formatted = format!("{prov}/{mdl}");
        let model_name = if prov.is_empty() { mdl } else { &formatted };
        let spec = model_spec(model_name);
        assert_eq!(
            spec.tool_support,
            ToolSupport::PseudoXml,
            "Syn-Pro model must have PseudoXml tool support: prov='{prov}', mdl='{mdl}'"
        );
    }

    // 3. Gemini in various cases and tiers
    let gemini_cases = [
        ("gemini-2.5-pro", ModelTier::FrontierReasoning),
        ("GEMINI-2.5-PRO", ModelTier::FrontierReasoning),
        ("Gemini-2.5-Pro", ModelTier::FrontierReasoning),
        ("google/gemini-2.5-pro", ModelTier::FrontierReasoning),
        ("GOOGLE/GEMINI-2.5-PRO", ModelTier::FrontierReasoning),
        ("gemini-2.5-flash", ModelTier::FastExecution),
        ("GEMINI-2.5-FLASH", ModelTier::FastExecution),
        ("google/gemini-2.5-flash", ModelTier::FastExecution),
    ];

    for (mdl, expected_tier) in gemini_cases {
        let spec = model_spec(mdl);
        assert_eq!(
            spec.context_window, 1_000_000,
            "Gemini context window must be 1M: {mdl}"
        );
        assert!(spec.supports_vision, "Gemini must support vision: {mdl}");
        assert_eq!(
            spec.tool_support,
            ToolSupport::Native,
            "Gemini must have Native tools: {mdl}"
        );
        assert_eq!(spec.tier, expected_tier, "Gemini tier mismatch: {mdl}");
        assert!(
            !is_pseudo_tool_model("", mdl),
            "Gemini must NOT be pseudo tool model: {mdl}"
        );
    }

    // 4. AgentRouter models in various cases
    let agentrouter_cases = [
        ("agentrouter/gpt-5.6-sol", 512_000, ModelTier::FastExecution),
        ("AGENTROUTER/GPT-5.6-SOL", 512_000, ModelTier::FastExecution),
        (
            "agentrouter/gpt-6.1-sol",
            512_000,
            ModelTier::FrontierReasoning,
        ),
        (
            "AGENTROUTER/GPT-6.1-SOL",
            512_000,
            ModelTier::FrontierReasoning,
        ),
        ("gpt-6-astra", 512_000, ModelTier::FrontierReasoning),
        ("GPT-6-ASTRA", 512_000, ModelTier::FrontierReasoning),
    ];

    for (mdl, expected_ctx, expected_tier) in agentrouter_cases {
        let spec = model_spec(mdl);
        assert_eq!(spec.context_window, expected_ctx, "Context mismatch: {mdl}");
        assert_eq!(spec.tier, expected_tier, "Tier mismatch: {mdl}");
        assert_eq!(spec.tool_support, ToolSupport::Native);
        assert!(!spec.supports_vision);
    }

    // 5. Codestral & DeepSeek in uppercase
    let codestral = model_spec("CODESTRAL-2501");
    assert_eq!(codestral.context_window, 256_000);
    assert_eq!(codestral.tier, ModelTier::FastExecution);

    let ds_reasoner = model_spec("DEEPSEEK-REASONER");
    assert_eq!(ds_reasoner.context_window, 128_000);
    assert_eq!(ds_reasoner.tier, ModelTier::FrontierReasoning);

    let exa = model_spec("EXA/EXA-SEARCH");
    assert_eq!(exa.tool_support, ToolSupport::None);
}

#[test]
fn test_empty_tools_vs_array_of_empty_object() {
    // 1. tools: None
    let req_none = ChatCompletionRequest {
        messages: vec![ChatMessage::plain("user", "Hello")],
        tools: None,
        ..Default::default()
    };
    let insp_none = inspect_request(&req_none);
    assert!(
        !insp_none.has_tools,
        "tools: None must result in has_tools = false"
    );

    // 2. tools: Some(json!([])) -> explicitly empty array
    let req_empty_arr = ChatCompletionRequest {
        messages: vec![ChatMessage::plain("user", "Hello")],
        tools: Some(json!([])),
        ..Default::default()
    };
    let insp_empty_arr = inspect_request(&req_empty_arr);
    assert!(
        !insp_empty_arr.has_tools,
        "tools: Some(json!([])) must result in has_tools = false"
    );

    // 3. tools: Some(json!(null))
    let req_null = ChatCompletionRequest {
        messages: vec![ChatMessage::plain("user", "Hello")],
        tools: Some(json!(null)),
        ..Default::default()
    };
    let insp_null = inspect_request(&req_null);
    assert!(
        !insp_null.has_tools,
        "tools: Some(json!(null)) must result in has_tools = false"
    );

    // 4. tools: Some(json!([{}])) -> array with an empty object
    let req_empty_obj_in_arr = ChatCompletionRequest {
        messages: vec![ChatMessage::plain("user", "Hello")],
        tools: Some(json!([{}])),
        ..Default::default()
    };
    let insp_empty_obj_in_arr = inspect_request(&req_empty_obj_in_arr);
    // Observe: array is non-empty (len 1)
    println!(
        "tools: Some(json!([{{}}])) has_tools = {}, estimated_tokens = {}",
        insp_empty_obj_in_arr.has_tools, insp_empty_obj_in_arr.estimated_tokens
    );

    // 5. tools: Some(json!({})) -> raw empty object (malformed, not an array)
    let req_raw_empty_obj = ChatCompletionRequest {
        messages: vec![ChatMessage::plain("user", "Hello")],
        tools: Some(json!({})),
        ..Default::default()
    };
    let insp_raw_empty_obj = inspect_request(&req_raw_empty_obj);
    println!(
        "tools: Some(json!({{}})) has_tools = {}, estimated_tokens = {}",
        insp_raw_empty_obj.has_tools, insp_raw_empty_obj.estimated_tokens
    );

    // 6. Valid tools
    let req_valid = ChatCompletionRequest {
        messages: vec![ChatMessage::plain("user", "Hello")],
        tools: Some(json!([
            {"type": "function", "function": {"name": "get_weather", "description": "Fetches weather"}}
        ])),
        ..Default::default()
    };
    let insp_valid = inspect_request(&req_valid);
    assert!(insp_valid.has_tools);
    assert!(insp_valid.estimated_tokens > insp_empty_arr.estimated_tokens);
}

#[test]
fn test_multimodal_and_malformed_content_structures() {
    // 1. Mixed text and data URLs
    let req_data_url = ChatCompletionRequest {
        messages: vec![ChatMessage {
            role: "user".to_string(),
            content: Some(json!([
                {"type": "text", "text": "Analyze this architectural diagram:"},
                {
                    "type": "image_url",
                    "image_url": {
                        "url": "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAIAAAACCAYAAABytg0kAAAAEklEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg=="
                    }
                }
            ])),
            reasoning_content: None,
            tool_calls: None,
            tool_call_id: None,
            name: None,
        }],
        ..Default::default()
    };
    let insp_data = inspect_request(&req_data_url);
    assert!(insp_data.has_images);
    // Base64 payload should NOT inflate text_bytes:
    // Text: "user" (4) + "Analyze this architectural diagram:" (36) = 40 bytes -> 10 tokens
    // 1 image = 1120 tokens
    // 1 turn = 4 tokens
    // Total should be around 1134 tokens, definitely NOT 1200+
    assert!(
        insp_data.estimated_tokens < 1200,
        "Base64 image should not inflate text token estimation; got {}",
        insp_data.estimated_tokens
    );
    assert!(insp_data.estimated_tokens >= 1124);

    // 2. Direct string image_url (OpenAI variant)
    let req_str_img = ChatCompletionRequest {
        messages: vec![ChatMessage {
            role: "user".to_string(),
            content: Some(json!([
                {"type": "image_url", "image_url": "https://example.com/photo.jpg"}
            ])),
            reasoning_content: None,
            tool_calls: None,
            tool_call_id: None,
            name: None,
        }],
        ..Default::default()
    };
    let insp_str_img = inspect_request(&req_str_img);
    assert!(insp_str_img.has_images);

    // 3. AI-SDK format: {"type": "image", "image": "data:image/png;base64,..."}
    let req_aisdk = ChatCompletionRequest {
        messages: vec![ChatMessage {
            role: "user".to_string(),
            content: Some(json!([
                {"type": "image", "image": "data:image/png;base64,iVBORw0KGgoAAA"}
            ])),
            reasoning_content: None,
            tool_calls: None,
            tool_call_id: None,
            name: None,
        }],
        ..Default::default()
    };
    let insp_aisdk = inspect_request(&req_aisdk);
    assert!(insp_aisdk.has_images);

    // 4. Anthropic base64 format: {"type": "image", "source": {"type": "base64", ...}}
    let req_anthropic = ChatCompletionRequest {
        messages: vec![ChatMessage {
            role: "user".to_string(),
            content: Some(json!([
                {
                    "type": "image",
                    "source": {
                        "type": "base64",
                        "media_type": "image/png",
                        "data": "iVBORw0KGgoAAA"
                    }
                }
            ])),
            reasoning_content: None,
            tool_calls: None,
            tool_call_id: None,
            name: None,
        }],
        ..Default::default()
    };
    let insp_anthropic = inspect_request(&req_anthropic);
    assert!(insp_anthropic.has_images);

    // 5. Bare object without type: {"image_url": {"url": "https://..."}}
    let req_bare_obj = ChatCompletionRequest {
        messages: vec![ChatMessage {
            role: "user".to_string(),
            content: Some(json!([
                {"image_url": {"url": "https://example.com/pic.png"}}
            ])),
            reasoning_content: None,
            tool_calls: None,
            tool_call_id: None,
            name: None,
        }],
        ..Default::default()
    };
    let insp_bare = inspect_request(&req_bare_obj);
    assert!(insp_bare.has_images);

    // 6. Content is a single Object instead of Array
    let req_single_obj = ChatCompletionRequest {
        messages: vec![ChatMessage {
            role: "user".to_string(),
            content: Some(json!({
                "type": "image_url",
                "image_url": {"url": "https://example.com/pic.png"}
            })),
            reasoning_content: None,
            tool_calls: None,
            tool_call_id: None,
            name: None,
        }],
        ..Default::default()
    };
    let insp_single_obj = inspect_request(&req_single_obj);
    assert!(insp_single_obj.has_images);

    // 7. Nested parts:
    // Case A: parts array wrapped inside an object
    let req_nested_parts = ChatCompletionRequest {
        messages: vec![ChatMessage {
            role: "user".to_string(),
            content: Some(json!([
                {
                    "type": "compound",
                    "parts": [
                        {"type": "text", "text": "inner text"},
                        {"type": "image_url", "image_url": {"url": "https://example.com/inner.png"}}
                    ]
                }
            ])),
            reasoning_content: None,
            tool_calls: None,
            tool_call_id: None,
            name: None,
        }],
        ..Default::default()
    };
    let insp_nested = inspect_request(&req_nested_parts);
    println!(
        "Nested parts detection: has_images={}, tokens={}",
        insp_nested.has_images, insp_nested.estimated_tokens
    );

    // Case B: array of arrays (nested array parts)
    let req_nested_arr = ChatCompletionRequest {
        messages: vec![ChatMessage {
            role: "user".to_string(),
            content: Some(json!([
                [
                    {"type": "text", "text": "nested inner text"},
                    {"type": "image_url", "image_url": {"url": "https://example.com/inner.png"}}
                ]
            ])),
            reasoning_content: None,
            tool_calls: None,
            tool_call_id: None,
            name: None,
        }],
        ..Default::default()
    };
    let insp_nested_arr = inspect_request(&req_nested_arr);
    println!(
        "Nested array parts detection: has_images={}, tokens={}",
        insp_nested_arr.has_images, insp_nested_arr.estimated_tokens
    );

    // 8. Malformed content types: numbers, bools, nulls, empty arrays
    let req_malformed = ChatCompletionRequest {
        messages: vec![
            ChatMessage {
                role: "user".to_string(),
                content: Some(json!(123456789)),
                reasoning_content: None,
                tool_calls: None,
                tool_call_id: None,
                name: None,
            },
            ChatMessage {
                role: "user".to_string(),
                content: Some(json!(true)),
                reasoning_content: None,
                tool_calls: None,
                tool_call_id: None,
                name: None,
            },
            ChatMessage {
                role: "user".to_string(),
                content: Some(json!(null)),
                reasoning_content: None,
                tool_calls: None,
                tool_call_id: None,
                name: None,
            },
            ChatMessage {
                role: "user".to_string(),
                content: Some(json!([])),
                reasoning_content: None,
                tool_calls: None,
                tool_call_id: None,
                name: None,
            },
            ChatMessage {
                role: "user".to_string(),
                content: Some(json!([null, 42, false, {}])),
                reasoning_content: None,
                tool_calls: None,
                tool_call_id: None,
                name: None,
            },
        ],
        ..Default::default()
    };
    let insp_malformed = inspect_request(&req_malformed);
    // Must not panic, must produce valid inspection
    assert_eq!(insp_malformed.stage, AgentStage::Planning);
}

#[test]
fn test_stage_detection_structured_json_tool_error() {
    // When tool returns stringified JSON:
    let req_str_json = ChatCompletionRequest {
        messages: vec![ChatMessage {
            role: "tool".to_string(),
            content: Some(json!("{\"error\": \"connection timeout\", \"code\": 504}")),
            reasoning_content: None,
            tool_calls: None,
            tool_call_id: Some("call_1".to_string()),
            name: Some("fetch".to_string()),
        }],
        ..Default::default()
    };
    let insp_str_json = inspect_request(&req_str_json);
    assert_eq!(
        insp_str_json.stage,
        AgentStage::ErrorRecovery,
        "Stringified JSON containing 'error' should trigger ErrorRecovery"
    );

    // When tool returns raw JSON Object with "error" field:
    let req_raw_json_obj = ChatCompletionRequest {
        messages: vec![ChatMessage {
            role: "tool".to_string(),
            content: Some(json!({"error": "connection timeout", "code": 504})),
            reasoning_content: None,
            tool_calls: None,
            tool_call_id: Some("call_1".to_string()),
            name: Some("fetch".to_string()),
        }],
        ..Default::default()
    };
    let insp_raw_json_obj = inspect_request(&req_raw_json_obj);
    println!(
        "Raw JSON object tool result with error field detected stage: {:?}",
        insp_raw_json_obj.stage
    );
}

#[test]
fn test_is_image_part_contract() {
    // Valid image parts
    assert!(is_image_part(&json!({"type": "image_url"})));
    assert!(is_image_part(&json!({"type": "image"})));
    assert!(is_image_part(&json!({"image_url": "https://example.com"})));
    assert!(is_image_part(
        &json!({"image": "data:image/png;base64,..."})
    ));
    assert!(is_image_part(&json!({
        "type": "image_url",
        "image_url": {"url": "https://example.com"}
    })));

    // Non-image parts
    assert!(!is_image_part(&json!({"type": "text", "text": "hello"})));
    assert!(!is_image_part(&json!({"text": "hello"})));
    assert!(!is_image_part(&json!("https://example.com/pic.png")));
    assert!(!is_image_part(&json!(null)));
    assert!(!is_image_part(&json!(123)));
    assert!(!is_image_part(&json!([])));
}

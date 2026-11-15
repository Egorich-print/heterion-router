//! Empirical Challenge Harness for Milestone 3 (Challenger 2)
//!
//! Scope:
//! 1. Empirically challenge `verify_tool_response`:
//!    - Adversarial inputs: empty arguments `""`, empty object `"{}"`, malformed JSON `"{\"a\": broken"`,
//!      whitespace names `"   "`, unicode tag variations, mixed casing `<TOOL_CALL>`, `</Tool>`.
//!    - Verify plain conversational chats (including code snippets discussing tools) are never falsely rejected.
//!    - Verify genuine tool calls with valid JSON arguments always pass.
//! 2. Empirically challenge `ModelQualityTracker`:
//!    - Verify mathematical score transitions: 1.0 -> 0.7 -> 0.4 -> 0.1 (floor).
//!    - Verify 15-minute probation state transition on >=2 consecutive defects.
//!    - Verify priority demotion under formula: `Effective Priority = Base Priority * Quality Score * Availability State`.

use heterion_router_core::{
    ChatChoice, ChatCompletionRequest, ChatCompletionResponse, ChatMessage, Usage,
};
use heterion_router_gateway::resilience::{
    ModelQualityTracker, ProviderCooldownSet, ToolRejection, verify_tool_response,
};
use serde_json::json;
use std::time::{Duration, Instant};

fn req_with_tools() -> ChatCompletionRequest {
    ChatCompletionRequest {
        model: "auto".into(),
        messages: vec![ChatMessage::plain("user", "Please check the status")],
        tools: Some(json!([
            {
                "type": "function",
                "function": {
                    "name": "check_status",
                    "description": "checks status",
                    "parameters": {
                        "type": "object",
                        "properties": {
                            "service": { "type": "string" }
                        }
                    }
                }
            }
        ])),
        ..Default::default()
    }
}

fn req_no_tools() -> ChatCompletionRequest {
    ChatCompletionRequest {
        model: "auto".into(),
        messages: vec![ChatMessage::plain("user", "Hello, how are you?")],
        tools: None,
        ..Default::default()
    }
}

fn res_with_tool_call(name: &str, args: &str, finish_reason: &str) -> ChatCompletionResponse {
    ChatCompletionResponse {
        id: "call_test".into(),
        object: "chat.completion",
        created: 1000,
        model: "test_model".into(),
        usage: Usage::default(),
        choices: vec![ChatChoice {
            index: 0,
            message: ChatMessage {
                role: "assistant".into(),
                content: None,
                reasoning_content: None,
                tool_calls: Some(json!([
                    {
                        "id": "call_1",
                        "type": "function",
                        "function": {
                            "name": name,
                            "arguments": args
                        }
                    }
                ])),
                tool_call_id: None,
                name: None,
            },
            finish_reason: finish_reason.into(),
        }],
        raw: None,
    }
}

fn res_with_text(content: &str, finish_reason: &str) -> ChatCompletionResponse {
    ChatCompletionResponse {
        id: "text_test".into(),
        object: "chat.completion",
        created: 1000,
        model: "test_model".into(),
        usage: Usage::default(),
        choices: vec![ChatChoice {
            index: 0,
            message: ChatMessage {
                role: "assistant".into(),
                content: Some(json!(content)),
                reasoning_content: None,
                tool_calls: None,
                tool_call_id: None,
                name: None,
            },
            finish_reason: finish_reason.into(),
        }],
        raw: None,
    }
}

// =========================================================================
// SECTION 1: verify_tool_response Empirical Challenge
// =========================================================================

#[test]
fn test_adversarial_tool_call_empty_arguments() {
    let req = req_with_tools();
    // Test input with empty arguments ""
    let res_empty_args = res_with_tool_call("check_status", "", "tool_calls");
    let result = verify_tool_response(&req, &res_empty_args);
    // Observe whether empty arguments "" is permitted or rejected
    println!("Observation for empty arguments \"\": {:?}", result);
    // In OpenAI API, empty string arguments for zero-arg tools is standard and permitted by verify_tool_response
    assert!(
        result.is_ok(),
        "Empty arguments string should be permitted for zero-argument tools"
    );
}

#[test]
fn test_adversarial_tool_call_empty_object() {
    let req = req_with_tools();
    // Test input with empty object "{}"
    let res_empty_obj = res_with_tool_call("check_status", "{}", "tool_calls");
    let result = verify_tool_response(&req, &res_empty_obj);
    assert!(result.is_ok(), "Empty object \"{{}}\" arguments must pass");
}

#[test]
fn test_adversarial_tool_call_malformed_json() {
    let req = req_with_tools();
    // Test input with malformed JSON "{\"a\": broken"
    let res_malformed = res_with_tool_call("check_status", "{\"a\": broken", "tool_calls");
    let result = verify_tool_response(&req, &res_malformed);
    assert!(result.is_err(), "Malformed JSON must be rejected");
    assert_eq!(result.unwrap_err(), ToolRejection::EmptyToolCalls);
}

#[test]
fn test_adversarial_tool_call_whitespace_name() {
    let req = req_with_tools();
    // Test input with whitespace name "   "
    let res_ws_name = res_with_tool_call("   ", "{}", "tool_calls");
    let result = verify_tool_response(&req, &res_ws_name);
    assert!(result.is_err(), "Whitespace tool name must be rejected");
    assert_eq!(result.unwrap_err(), ToolRejection::EmptyToolCalls);
}

#[test]
fn test_adversarial_mixed_casing_pseudo_xml() {
    let req = req_with_tools();
    let req_none = req_no_tools();

    // Mixed casing: <TOOL_CALL>
    let res_upper_tool_call = res_with_text(
        "I will call the tool: <TOOL_CALL>{\"name\": \"check_status\"}</TOOL_CALL>",
        "stop",
    );
    let res1 = verify_tool_response(&req, &res_upper_tool_call);
    println!("Result for <TOOL_CALL>: {:?}", res1);
    assert!(
        matches!(res1, Err(ToolRejection::PseudoXmlDetected(_))),
        "<TOOL_CALL> must be detected and rejected when tools were requested"
    );
    assert!(
        verify_tool_response(&req_none, &res_upper_tool_call).is_ok(),
        "Conversational text with <TOOL_CALL> must pass when tools were not requested"
    );

    // Mixed casing: </Tool>
    let res_mixed_tool = res_with_text("Executing command <Tool>check_status</Tool>", "stop");
    let res2 = verify_tool_response(&req, &res_mixed_tool);
    println!("Result for </Tool>: {:?}", res2);
    assert!(
        matches!(res2, Err(ToolRejection::PseudoXmlDetected(_))),
        "</Tool> must be detected and rejected when tools were requested"
    );
    assert!(
        verify_tool_response(&req_none, &res_mixed_tool).is_ok(),
        "Conversational text with </Tool> must pass when tools were not requested"
    );

    // Title casing: <Tool_Call>
    let res_title_call = res_with_text(
        "<Tool_Call>{\"name\": \"check_status\"}</Tool_Call>",
        "stop",
    );
    let res3 = verify_tool_response(&req, &res_title_call);
    println!("Result for <Tool_Call>: {:?}", res3);
    assert!(
        matches!(res3, Err(ToolRejection::PseudoXmlDetected(_))),
        "<Tool_Call> must be detected and rejected when tools were requested"
    );
    assert!(
        verify_tool_response(&req_none, &res_title_call).is_ok(),
        "Conversational text with <Tool_Call> must pass when tools were not requested"
    );

    // Title casing: </tool_call> with uppercase <TOOL_CALL>
    let res_mixed_pair = res_with_text("<TOOL_CALL>{\"name\": \"test\"}</tool_call>", "stop");
    let res4 = verify_tool_response(&req, &res_mixed_pair);
    println!(
        "Result for mixed pair <TOOL_CALL>...</tool_call>: {:?}",
        res4
    );
    assert!(
        matches!(res4, Err(ToolRejection::PseudoXmlDetected(_))),
        "Mixed casing pair must be detected and rejected when tools were requested"
    );
}

#[test]
fn test_adversarial_unicode_tag_variations() {
    let req = req_with_tools();
    let req_none = req_no_tools();

    // Fullwidth angle brackets: ＜tool_call＞ (U+FF1C, U+FF1E)
    let res_fullwidth = res_with_text("＜tool_call＞check_status＜/tool_call＞", "stop");
    let res1 = verify_tool_response(&req, &res_fullwidth);
    println!("Result for fullwidth ＜tool_call＞: {:?}", res1);
    assert!(
        matches!(res1, Err(ToolRejection::PseudoXmlDetected(_))),
        "Fullwidth ＜tool_call＞ must be detected and rejected"
    );
    assert!(
        verify_tool_response(&req_none, &res_fullwidth).is_ok(),
        "Conversational text with fullwidth brackets must pass when no tools requested"
    );

    // Tag with attributes: <tool name="check_status">...
    // Without lowercase closing </tool>
    let res_attr_only = res_with_text("<tool name=\"check_status\">arg</Tool>", "stop");
    let res2 = verify_tool_response(&req, &res_attr_only);
    println!("Result for <tool name=...> with </Tool>: {:?}", res2);
    assert!(
        matches!(res2, Err(ToolRejection::PseudoXmlDetected(_))),
        "<tool name=\"check_status\"> must be detected and rejected"
    );

    // Tag with attributes: <tool_call id="1">...
    let res_attr_call = res_with_text(
        "<tool_call id=\"1\">{\"service\":\"db\"}</Tool_Call>",
        "stop",
    );
    let res3 = verify_tool_response(&req, &res_attr_call);
    println!("Result for <tool_call id=...>: {:?}", res3);
    assert!(
        matches!(res3, Err(ToolRejection::PseudoXmlDetected(_))),
        "<tool_call id=\"1\"> must be detected and rejected"
    );

    // Unclosed attribute tag: <tool name="test">
    let res_attr_test = res_with_text(
        "I need to use <tool name=\"test\"> to inspect files",
        "stop",
    );
    let res4 = verify_tool_response(&req, &res_attr_test);
    println!("Result for <tool name=\"test\">: {:?}", res4);
    assert!(
        matches!(res4, Err(ToolRejection::PseudoXmlDetected(_))),
        "<tool name=\"test\"> must be detected and rejected"
    );
    assert!(
        verify_tool_response(&req_none, &res_attr_test).is_ok(),
        "Conversational text with <tool name=\"test\"> must pass when no tools requested"
    );
}

#[test]
fn test_conversational_chat_without_tools_never_rejected() {
    let req = req_no_tools();

    // Conversational text discussing tools
    let res_discussing = res_with_text(
        "To define a tool in XML, one might write `<tool_call>` or `<tool>`. Here is Python code:\n```python\ncall:check_status()\n```",
        "stop",
    );
    assert!(verify_tool_response(&req, &res_discussing).is_ok());

    // Conversational text with refusal phrasing
    let res_refusal = res_with_text(
        "I do not have access to tools or external APIs to check real-time data.",
        "stop",
    );
    assert!(verify_tool_response(&req, &res_refusal).is_ok());

    // Conversational code snippet
    let res_code = res_with_text(
        "```rust\nfn main() {\n    println!(\"<tool>hello</tool>\");\n}\n```",
        "stop",
    );
    assert!(verify_tool_response(&req, &res_code).is_ok());
}

#[test]
fn test_genuine_tool_calls_always_pass() {
    let req = req_with_tools();

    // Valid single tool call
    let res_valid = res_with_tool_call("check_status", "{\"service\": \"database\"}", "tool_calls");
    assert!(verify_tool_response(&req, &res_valid).is_ok());

    // Valid multiple tool calls
    let res_multi = ChatCompletionResponse {
        id: "multi".into(),
        object: "chat.completion",
        created: 1000,
        model: "test_model".into(),
        usage: Usage::default(),
        choices: vec![ChatChoice {
            index: 0,
            message: ChatMessage {
                role: "assistant".into(),
                content: None,
                reasoning_content: None,
                tool_calls: Some(json!([
                    {
                        "id": "c1",
                        "type": "function",
                        "function": { "name": "check_status", "arguments": "{\"service\": \"db\"}" }
                    },
                    {
                        "id": "c2",
                        "type": "function",
                        "function": { "name": "check_status", "arguments": "{\"service\": \"cache\"}" }
                    }
                ])),
                tool_call_id: None,
                name: None,
            },
            finish_reason: "tool_calls".into(),
        }],
        raw: None,
    };
    assert!(verify_tool_response(&req, &res_multi).is_ok());
}

// =========================================================================
// SECTION 2: ModelQualityTracker Empirical Challenge
// =========================================================================

#[test]
fn test_model_quality_score_transitions() {
    let tracker = ModelQualityTracker::new();
    let m = "test_model_transitions";
    let t0 = Instant::now();

    // Initial score must be 1.0
    assert_eq!(tracker.get_score_at(m, t0), 1.0);

    // 1st defect: 1.0 -> 0.7
    tracker.record_failure_at(m, t0);
    let s1 = tracker.get_score_at(m, t0);
    println!("Score after 1st defect: {}", s1);
    assert!(
        (s1 - 0.7).abs() < 1e-6,
        "Score after 1st defect must be 0.7"
    );
    assert!(
        !tracker.is_in_probation_at(m, t0),
        "Should not be in probation after 1 defect"
    );

    // 2nd defect at t0 + 10s:
    // Notice: Does it transition to 0.4 or is it clamped by probation?
    let t1 = t0 + Duration::from_secs(10);
    tracker.record_failure_at(m, t1);
    let s2 = tracker.get_score_at(m, t1);
    println!("Score after 2nd consecutive defect: {}", s2);
    let in_prob2 = tracker.is_in_probation_at(m, t1);
    println!("Is in probation after 2nd defect: {}", in_prob2);
    assert!(in_prob2, "Must be in probation after 2 consecutive defects");

    // Check what happens if defects are spaced by > 15 minutes (non-consecutive within probation window)
    let tracker2 = ModelQualityTracker::new();
    let m2 = "spaced_model";
    tracker2.record_failure_at(m2, t0);
    assert!((tracker2.get_score_at(m2, t0) - 0.7).abs() < 1e-6);

    // 2nd defect 20 minutes later (past 15-minute probation sliding window)
    let t_spaced = t0 + Duration::from_secs(20 * 60);
    tracker2.record_failure_at(m2, t_spaced);
    let s_spaced = tracker2.get_score_at(m2, t_spaced);
    println!("Score after 2nd defect spaced by 20m: {}", s_spaced);
    let in_prob_spaced = tracker2.is_in_probation_at(m2, t_spaced);
    println!("Is in probation after spaced defect: {}", in_prob_spaced);

    // 3rd defect 20 minutes later
    let t_spaced2 = t_spaced + Duration::from_secs(20 * 60);
    tracker2.record_failure_at(m2, t_spaced2);
    let s_spaced2 = tracker2.get_score_at(m2, t_spaced2);
    println!("Score after 3rd defect spaced by 20m: {}", s_spaced2);

    // Floor clamping to 0.1
    for i in 1..=10 {
        tracker2.record_failure_at(m2, t_spaced2 + Duration::from_secs(i));
    }
    let s_floor = tracker2.get_score_at(m2, t_spaced2 + Duration::from_secs(11));
    println!("Score floor: {}", s_floor);
    assert!((s_floor - 0.1).abs() < 1e-6, "Score floor must be 0.1");
}

#[test]
fn test_probation_15_minute_duration() {
    let tracker = ModelQualityTracker::new();
    let m = "probation_timing_model";
    let t0 = Instant::now();

    // 2 consecutive failures within 15 minutes
    tracker.record_failure_at(m, t0);
    tracker.record_failure_at(m, t0 + Duration::from_secs(10));

    // At t0 + 10s -> in probation
    assert!(tracker.is_in_probation_at(m, t0 + Duration::from_secs(10)));

    // At 14 minutes 59 seconds -> still in probation
    assert!(tracker.is_in_probation_at(m, t0 + Duration::from_secs(10 + 14 * 60 + 59)));

    // At 15 minutes 1 second -> probation expired
    assert!(!tracker.is_in_probation_at(m, t0 + Duration::from_secs(10 + 15 * 60 + 1)));
}

#[test]
fn test_effective_priority_formula_and_demotion() {
    let tracker = ModelQualityTracker::new();
    let cooldowns = ProviderCooldownSet::new();
    let t0 = Instant::now();

    let t1_model = "gemini-3-flash-preview";
    let t1_prov = "gemini";
    let t1_base = 100.0;

    let t2_model = "codestral-latest";
    let t2_prov = "mistral";
    let t2_base = 50.0;

    // 1. Initial healthy state:
    // Effective = Base * Quality * Availability
    // T1: 100.0 * 1.0 * 1.0 = 100.0
    // T2: 50.0 * 1.0 * 1.0 = 50.0
    let eff_t1_init = tracker.effective_priority_at(t1_base, t1_model, t1_prov, &cooldowns, t0);
    let eff_t2_init = tracker.effective_priority_at(t2_base, t2_model, t2_prov, &cooldowns, t0);
    assert_eq!(eff_t1_init, 100.0);
    assert_eq!(eff_t2_init, 50.0);
    assert!(eff_t1_init > eff_t2_init);

    // 2. Single defect on T1:
    // Quality drops to 0.7
    // Effective = 100.0 * 0.7 * 1.0 = 70.0
    tracker.record_failure_at(t1_model, t0);
    let eff_t1_1fail = tracker.effective_priority_at(t1_base, t1_model, t1_prov, &cooldowns, t0);
    assert!((eff_t1_1fail - 70.0).abs() < 1e-6);
    // T1 still above T2 (70.0 > 50.0)
    assert!(eff_t1_1fail > eff_t2_init);

    // 3. 2nd consecutive defect on T1:
    // Enters 15-minute probation, score ceiling 0.2
    // Effective = 100.0 * 0.2 * 1.0 = 20.0
    tracker.record_failure_at(t1_model, t0 + Duration::from_secs(10));
    let eff_t1_prob = tracker.effective_priority_at(
        t1_base,
        t1_model,
        t1_prov,
        &cooldowns,
        t0 + Duration::from_secs(10),
    );
    assert_eq!(eff_t1_prob, 20.0);
    // DEMOTION: T1 is now demoted below T2 (20.0 < 50.0)!
    assert!(eff_t1_prob < eff_t2_init);

    // 4. Provider cooldown on T2 (HTTP 429):
    // Availability drops to 0.0
    // Effective = 50.0 * 1.0 * 0.0 = 0.0
    cooldowns.record_429_at(t2_prov, None, None, t0 + Duration::from_secs(10));
    let eff_t2_cooled = tracker.effective_priority_at(
        t2_base,
        t2_model,
        t2_prov,
        &cooldowns,
        t0 + Duration::from_secs(10),
    );
    assert_eq!(eff_t2_cooled, 0.0);

    // When T2 is in cooldown, T1 (even in probation with 20.0) is preferred over T2 (0.0)
    assert!(eff_t1_prob > eff_t2_cooled);
}

#[test]
fn test_model_quality_tracker_recovery_dynamics() {
    let tracker = ModelQualityTracker::new();
    let m = "recovery_model";
    let t0 = Instant::now();

    // 1. Initial pristine state
    assert_eq!(tracker.get_score_at(m, t0), 1.0);
    assert!(!tracker.is_in_probation_at(m, t0));

    // 2. 1st defect: drops to 0.7, no probation
    tracker.record_failure_at(m, t0);
    let s1 = tracker.get_score_at(m, t0);
    assert!(
        (s1 - 0.7).abs() < 1e-6,
        "1st defect must drop score to exactly 0.7"
    );
    assert!(!tracker.is_in_probation_at(m, t0));

    // 3. 2nd consecutive defect at t0 + 10s:
    // Triggers 15-minute probation.
    // Dynamic probation ceiling is 0.2, but intrinsic underlying score is 0.4.
    let t1 = t0 + Duration::from_secs(10);
    tracker.record_failure_at(m, t1);

    // During probation:
    assert!(
        tracker.is_in_probation_at(m, t1),
        "Must be in probation after 2 consecutive defects"
    );
    let s_prob = tracker.get_score_at(m, t1);
    assert!(
        (s_prob - 0.2).abs() < 1e-6,
        "Effective score during probation must be clamped to ceiling 0.2"
    );

    // Right before 15-minute probation expiration (14m 59s after t1):
    let t_pre_exp = t1 + Duration::from_secs(14 * 60 + 59);
    assert!(tracker.is_in_probation_at(m, t_pre_exp));
    assert!((tracker.get_score_at(m, t_pre_exp) - 0.2).abs() < 1e-6);

    // 4. After probation expiration (15m 1s after t1):
    // Probation is inactive, score un-clamps to intrinsic underlying score 0.4.
    let t_exp = t1 + Duration::from_secs(15 * 60 + 1);
    assert!(
        !tracker.is_in_probation_at(m, t_exp),
        "Probation must be expired after 15 minutes"
    );
    let s_unclamped = tracker.get_score_at(m, t_exp);
    println!(
        "Un-clamped score after probation expiration: {}",
        s_unclamped
    );
    assert!(
        (s_unclamped - 0.4).abs() < 1e-6,
        "Underlying score after probation expiration must be 0.4 (NOT 0.2)"
    );

    // 5. Successful streak after expiration:
    // 1st success recovers score: 0.40 + 0.05 = 0.45
    let t_s1 = t_exp + Duration::from_secs(1);
    tracker.record_success_at(m, t_s1);
    let s_rec1 = tracker.get_score_at(m, t_s1);
    println!("Score after 1st recovery success: {}", s_rec1);
    assert!(
        (s_rec1 - 0.45).abs() < 1e-6,
        "1st success after expiration must recover score to 0.45"
    );

    // 2nd success recovers score: 0.45 + 0.05 = 0.50
    let t_s2 = t_exp + Duration::from_secs(2);
    tracker.record_success_at(m, t_s2);
    let s_rec2 = tracker.get_score_at(m, t_s2);
    println!("Score after 2nd recovery success: {}", s_rec2);
    assert!(
        (s_rec2 - 0.50).abs() < 1e-6,
        "2nd success after expiration must recover score to 0.50"
    );

    // Continuous streak: 10 more successes step-by-step up to 1.0
    for i in 3..=12 {
        let t_si = t_exp + Duration::from_secs(i);
        tracker.record_success_at(m, t_si);
        let s_curr = tracker.get_score_at(m, t_si);
        let expected = 0.40 + (i as f64) * 0.05;
        assert!(
            (s_curr - expected).abs() < 1e-6,
            "Success #{i} must produce score {expected}, got {s_curr}"
        );
    }

    let s_full = tracker.get_score_at(m, t_exp + Duration::from_secs(12));
    assert!(
        (s_full - 1.0).abs() < 1e-6,
        "Score after 12 successes must reach 1.0"
    );

    // Extra success caps at 1.0
    let t_extra = t_exp + Duration::from_secs(13);
    tracker.record_success_at(m, t_extra);
    let s_extra = tracker.get_score_at(m, t_extra);
    assert_eq!(s_extra, 1.0, "Score must not exceed 1.0");

    // 6. Success DURING probation:
    // Verify that success during probation increments underlying score but remains clamped until expiration
    let m_during = "model_success_during_probation";
    tracker.record_failure_at(m_during, t0);
    tracker.record_failure_at(m_during, t1);
    assert_eq!(tracker.get_score_at(m_during, t1), 0.2);

    // Record success 5 minutes into probation
    let t_mid = t1 + Duration::from_secs(5 * 60);
    tracker.record_success_at(m_during, t_mid);
    // Still in probation, so effective score is still clamped at 0.2
    assert!(tracker.is_in_probation_at(m_during, t_mid));
    assert_eq!(tracker.get_score_at(m_during, t_mid), 0.2);

    // When probation expires (15m 1s after t1):
    let t_mid_exp = t1 + Duration::from_secs(15 * 60 + 1);
    assert!(!tracker.is_in_probation_at(m_during, t_mid_exp));
    let s_mid_unclamped = tracker.get_score_at(m_during, t_mid_exp);
    println!(
        "Score after mid-probation success un-clamped: {}",
        s_mid_unclamped
    );
    assert!(
        (s_mid_unclamped - 0.45).abs() < 1e-6,
        "Mid-probation success un-clamps to 0.45 (0.4 + 0.05) after probation expires"
    );
}

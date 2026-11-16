//! Vision relay — the router borrows eyes for a model that has none.
//!
//! A request can carry images and still be served by a model that cannot see
//! them: the router asks a vision-capable model to describe the picture,
//! replaces the image parts with that description and hands the text to the
//! model that would have answered. Without the relay such a request dies on
//! the upstream's `400` ("image input not supported").
//!
//! Two rules keep the relay honest:
//!
//! * the description travels as **text**, behind a marker that names the
//!   model that wrote it — the target model must not believe it saw the
//!   picture itself;
//! * a relay that fails changes nothing: the original request goes out
//!   unchanged, so the worst case is the upstream refusing the image exactly
//!   as it did before the relay existed.
//!
//! The relay is not a second router: it borrows one *backend* directly, so
//! the borrowed call cannot recurse into the ladder.

use heterion_router_core::{ChatCompletionRequest, ChatMessage};
use serde_json::{Value, json};

/// What the describer is asked for.
pub const RELAY_INSTRUCTION: &str = "Describe this image for a model that cannot see it. \
Report what is on it, every readable text verbatim, numbers, tables, interface elements, \
their layout and colours. Answer with the description only, no preamble.";

/// The marker that introduces a borrowed description.
pub fn marker(source: &str) -> String {
    format!("[Image description written by {source}]")
}

/// Every image part of the request, in the order the client sent them.
pub fn image_parts(request: &ChatCompletionRequest) -> Vec<Value> {
    let mut parts = Vec::new();
    for message in &request.messages {
        match message.content.as_ref() {
            Some(Value::Array(items)) => {
                for item in items {
                    if crate::inspector::is_image_part(item) {
                        parts.push(item.clone());
                    }
                }
            }
            Some(content) if crate::inspector::is_image_part(content) => {
                parts.push(content.clone());
            }
            _ => {}
        }
    }
    parts
}

/// The request a describer gets: the instruction and every image, nothing else.
///
/// `None` when the request carries no image — there is nothing to describe.
pub fn describer_request(
    request: &ChatCompletionRequest,
    model: &str,
) -> Option<ChatCompletionRequest> {
    let images = image_parts(request);
    if images.is_empty() {
        return None;
    }
    let mut parts = vec![json!({"type": "text", "text": RELAY_INSTRUCTION})];
    parts.extend(images);

    let mut out = request.clone();
    out.model = model.to_string();
    out.messages = vec![ChatMessage {
        role: "user".to_string(),
        content: Some(Value::Array(parts)),
        reasoning_content: None,
        tool_calls: None,
        tool_call_id: None,
        name: None,
    }];
    // A describer looks, it does not act: no tools, no tool choice, and the
    // answer is needed in one piece.
    out.tools = None;
    out.tool_choice = None;
    out.stream = false;
    Some(out)
}

/// The request the target gets: the images replaced by the description.
///
/// The text of the original message is kept — only the image parts are
/// removed and the description is appended in their place.
pub fn with_description(
    request: &ChatCompletionRequest,
    description: &str,
    source: &str,
) -> ChatCompletionRequest {
    let label = format!("{}\n{}", marker(source), description.trim());
    let mut out = request.clone();
    for message in &mut out.messages {
        let Some(content) = message.content.as_ref() else {
            continue;
        };
        match content {
            Value::Array(items) => {
                if !items.iter().any(crate::inspector::is_image_part) {
                    continue;
                }
                let mut kept: Vec<Value> = items
                    .iter()
                    .filter(|item| !crate::inspector::is_image_part(item))
                    .cloned()
                    .collect();
                kept.push(json!({"type": "text", "text": label}));
                message.content = Some(Value::Array(kept));
            }
            content if crate::inspector::is_image_part(content) => {
                message.content = Some(Value::String(label.clone()));
            }
            _ => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn image_request() -> ChatCompletionRequest {
        ChatCompletionRequest {
            model: "auto".into(),
            messages: vec![ChatMessage {
                role: "user".into(),
                content: Some(json!([
                    {"type": "text", "text": "что на скриншоте?"},
                    {"type": "image_url", "image_url": {"url": "data:image/png;base64,AAA"}},
                ])),
                reasoning_content: None,
                tool_calls: None,
                tool_call_id: None,
                name: None,
            }],
            stream: true,
            ..Default::default()
        }
    }

    #[test]
    fn the_image_parts_are_found_in_their_order() {
        let request = image_request();
        let parts = image_parts(&request);
        assert_eq!(parts.len(), 1);
        assert_eq!(parts[0]["type"], "image_url");
    }

    #[test]
    fn a_request_without_images_has_nothing_to_describe() {
        let request = ChatCompletionRequest {
            model: "auto".into(),
            messages: vec![ChatMessage::plain("user", "привет")],
            ..Default::default()
        };
        assert!(image_parts(&request).is_empty());
        assert!(describer_request(&request, "gemini/gemini-3-flash").is_none());
    }

    #[test]
    fn the_describer_gets_the_instruction_the_images_and_nothing_else() {
        let request = image_request();
        let describer =
            describer_request(&request, "gemini/gemini-3-flash").expect("describer request");
        assert_eq!(describer.model, "gemini/gemini-3-flash");
        assert_eq!(describer.messages.len(), 1);
        assert!(!describer.stream, "the description is needed in one piece");
        let content = describer.messages[0]
            .content
            .as_ref()
            .expect("content")
            .as_array()
            .expect("array");
        assert_eq!(content.len(), 2);
        assert_eq!(content[0]["type"], "text");
        assert_eq!(content[1]["type"], "image_url");
    }

    #[test]
    fn the_description_replaces_the_image_and_keeps_the_text() {
        let request = image_request();
        let rewritten =
            with_description(&request, "красная кнопка \"OK\"", "gemini/gemini-3-flash");
        let content = rewritten.messages[0]
            .content
            .as_ref()
            .expect("content")
            .as_array()
            .expect("array");
        assert_eq!(content.len(), 2);
        assert_eq!(content[0]["text"], "что на скриншоте?");
        let text = content[1]["text"].as_str().expect("text");
        assert!(text.contains("красная кнопка"));
        assert!(text.contains("gemini/gemini-3-flash"));
        assert!(
            !content.iter().any(crate::inspector::is_image_part),
            "the image must not survive the relay"
        );
    }

    #[test]
    fn a_message_that_is_an_image_itself_becomes_text() {
        let request = ChatCompletionRequest {
            model: "auto".into(),
            messages: vec![ChatMessage {
                role: "user".into(),
                content: Some(json!({"image_url": {"url": "data:image/png;base64,AAA"}})),
                reasoning_content: None,
                tool_calls: None,
                tool_call_id: None,
                name: None,
            }],
            ..Default::default()
        };
        let rewritten = with_description(&request, "пустое поле", "gemini/gemini-3-flash");
        let text = rewritten.messages[0]
            .content
            .as_ref()
            .expect("content")
            .as_str()
            .expect("string");
        assert!(text.contains("пустое поле"));
    }
}

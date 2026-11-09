//! Playground — an ad-hoc chat completion against the gateway.
//!
//! The Svelte view (`desktop/ui/src/views/Playground.svelte`) sends a real
//! OpenAI-shaped request to `/v1/chat/completions` with the operator key and
//! shows the answer of the model. The native port keeps that contract: the
//! same request body, the same meta line, the same empty and error states.
//!
//! Two things differ from the web bundle, both on purpose:
//!
//! * the model list is a row of chips instead of a `<datalist>` — gpui has no
//!   combobox, and a chip says what it is;
//! * the prompt gets its own multi-line field (`PromptArea`) because the
//!   shared `TextInput` is single-line.

use std::time::Instant;

use gpui::prelude::*;
use gpui::{
    AnyElement, App, ClipboardItem, Context, CursorStyle, Entity, FocusHandle, Hsla, IntoElement,
    KeyDownEvent, Render, SharedString, Window, div, px, rgba,
};
use serde_json::{Value, json};

use crate::api::{self, ApiError, CombosResponse};
use crate::theme;
use crate::widgets::{TextInput, muted, page_header, primary_button};

/// The prompt the Svelte view starts with.
const DEFAULT_PROMPT: &str = "Reply with exactly: PONG";

/// The answer budget of one playground request, like the Svelte view.
const MAX_TOKENS: u64 = 512;

/// The Playground section.
pub struct PlaygroundView {
    /// The loaded combos (`GET /api/combos`).
    pub data: Option<CombosResponse>,
    /// The last failure, rendered as a banner.
    pub error: Option<String>,
    /// Whether the combo list is still loading.
    pub loading: bool,
    /// The model to talk to: a combo name or `provider/model`.
    pub model: String,
    /// The prompt to send.
    pub prompt: String,
    /// The answer of the last request.
    pub answer: String,
    /// The `model · finish_reason · ms · tokens` line under the answer.
    pub meta: String,
    /// Whether a completion request is in flight.
    pub sending: bool,
    /// The model field.
    model_input: Entity<TextInput>,
    /// The prompt field.
    prompt_input: Entity<PromptArea>,
}

impl PlaygroundView {
    /// Build the view and load the combo list.
    pub fn new(cx: &mut Context<Self>) -> Self {
        let model_input = cx.new(|cx| TextInput::new(cx, ""));
        let prompt_input = cx.new(|cx| PromptArea::new(cx, DEFAULT_PROMPT));

        let mut view = Self {
            data: None,
            error: None,
            loading: true,
            model: String::new(),
            prompt: DEFAULT_PROMPT.to_string(),
            answer: String::new(),
            meta: String::new(),
            sending: false,
            model_input,
            prompt_input,
        };
        view.wire(cx);
        view.refresh(cx);
        view
    }

    /// Connect the two fields to the state of the view.
    fn wire(&mut self, cx: &mut Context<Self>) {
        let view = cx.entity();
        self.model_input.update(cx, |input, _| {
            input.set_placeholder("combo or provider/model");
            let changed = view.clone();
            input.set_on_change(move |value, cx| {
                changed.update(cx, |view, cx| {
                    view.model = value.trim().to_string();
                    cx.notify();
                });
            });
            // Enter in the model field sends, like the submit of the web form.
            let submitted = view.clone();
            input.set_on_submit(move |_value, _window, cx| {
                submitted.update(cx, |view, cx| view.send(cx));
            });
        });

        let view = cx.entity();
        self.prompt_input.update(cx, |input, _| {
            let view = view.clone();
            input.set_on_change(move |value, cx| {
                view.update(cx, |view, cx| {
                    view.prompt = value.to_string();
                    cx.notify();
                });
            });
        });
    }

    /// Reload the combo list from the gateway.
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        let Some(client) = api::session_client(cx) else {
            self.loading = false;
            return;
        };
        self.loading = true;
        api::spawn_request(
            cx,
            move || client.get_json::<CombosResponse>("/api/combos"),
            |result, view: &mut PlaygroundView, cx| {
                match result {
                    Ok(data) => {
                        view.data = Some(data);
                        view.error = None;
                        view.preselect_model(cx);
                    }
                    Err(error) => {
                        view.error = Some(tidy_error(&error));
                        crate::app::report_error(cx, &error);
                    }
                }
                view.loading = false;
                cx.notify();
            },
        );
    }

    /// Pick the first combo when the operator has not named a model yet.
    fn preselect_model(&mut self, cx: &mut Context<Self>) {
        if !self.model.trim().is_empty() {
            return;
        }
        let Some(name) = self
            .data
            .as_ref()
            .and_then(|data| data.combos.first())
            .map(|combo| combo.name.clone())
        else {
            return;
        };
        self.model = name.clone();
        self.model_input
            .update(cx, |input, _| input.set_value(name));
    }

    /// Send the prompt to the gateway.
    pub fn send(&mut self, cx: &mut Context<Self>) {
        if self.sending {
            return;
        }
        let model = self.model.trim().to_string();
        if model.is_empty() {
            return;
        }
        let Some(client) = api::session_client(cx) else {
            return;
        };

        let body = request_body(&model, &self.prompt);
        self.sending = true;
        self.answer.clear();
        self.meta.clear();
        self.error = None;
        let started = Instant::now();

        api::spawn_request(
            cx,
            move || client.post_json::<Value, Value>("/v1/chat/completions", &body),
            move |result, view: &mut PlaygroundView, cx| {
                match result {
                    Ok(payload) => {
                        view.answer = reply_of(&payload);
                        view.meta = meta_of(&payload, &model, started.elapsed().as_millis());
                    }
                    Err(error) => {
                        view.error = Some(tidy_error(&error));
                        crate::app::report_error(cx, &error);
                    }
                }
                view.sending = false;
                cx.notify();
            },
        );
    }

    /// Use a combo as the model of the next request.
    fn select_model(&mut self, name: &str, cx: &mut Context<Self>) {
        self.model = name.to_string();
        self.model_input
            .update(cx, |input, _| input.set_value(name.to_string()));
    }

    /// The failure banner, if the last request failed.
    fn render_error(&self) -> AnyElement {
        match &self.error {
            Some(message) => error_box(message.clone()).into_any_element(),
            None => div().into_any_element(),
        }
    }

    /// The model row, the known models and the prompt.
    fn render_form(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let busy = self.sending;
        let enabled = !busy && !self.model.trim().is_empty();
        let label = if busy { "sending…" } else { "Send" };

        let view = cx.entity();
        let send = move |_: &mut Window, cx: &mut App| {
            view.update(cx, |view, cx| view.send(cx));
        };

        div()
            .flex()
            .flex_col()
            .gap(px(8.0))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .child(
                        div()
                            .text_size(px(13.0))
                            .text_color(theme::text())
                            .child("model"),
                    )
                    .child(div().w(px(260.0)).child(self.model_input.clone()))
                    .child(primary_button("playground-send", label, enabled, send)),
            )
            .child(self.render_models(cx))
            .child(self.prompt_input.clone())
            .into_any_element()
    }

    /// The known model names, as clickable chips.
    fn render_models(&self, cx: &mut Context<Self>) -> AnyElement {
        let names: Vec<String> = self
            .data
            .as_ref()
            .map(|data| data.combos.iter().map(|combo| combo.name.clone()).collect())
            .unwrap_or_default();
        if names.is_empty() {
            return if self.loading {
                muted("loading models…").into_any_element()
            } else {
                div().into_any_element()
            };
        }

        let mut chips: Vec<AnyElement> = Vec::with_capacity(names.len());
        for name in names {
            let active = name == self.model;
            let view = cx.entity();
            let target = name.clone();
            chips.push(
                div()
                    .id(theme::element_id(format!("model-chip:{name}")))
                    .flex()
                    .items_center()
                    .px(px(8.0))
                    .py(px(3.0))
                    .rounded(px(theme::RADIUS_SM))
                    .border_1()
                    .border_color(if active {
                        theme::accent()
                    } else {
                        theme::border()
                    })
                    .text_size(px(12.0))
                    .text_color(if active {
                        theme::accent()
                    } else {
                        theme::muted()
                    })
                    .cursor(CursorStyle::PointingHand)
                    .on_click(move |_event, _window, cx| {
                        view.update(cx, |view, cx| view.select_model(&target, cx));
                    })
                    .child(name)
                    .into_any_element(),
            );
        }

        div()
            .flex()
            .flex_wrap()
            .gap(px(6.0))
            .children(chips)
            .into_any_element()
    }

    /// The meta line and the answer of the last request.
    fn render_answer(&self) -> AnyElement {
        if self.meta.is_empty() {
            return div().into_any_element();
        }

        div()
            .flex()
            .flex_col()
            .gap(px(8.0))
            .child(muted(self.meta.clone()))
            .child(
                div()
                    .bg(theme::panel())
                    .border_1()
                    .border_color(theme::border())
                    .rounded(px(theme::RADIUS))
                    .p(px(12.0))
                    .font(gpui::font(theme::MONO_FAMILY))
                    .text_size(px(12.0))
                    .text_color(theme::text())
                    .whitespace_normal()
                    .child(self.answer.clone()),
            )
            .into_any_element()
    }
}

impl Render for PlaygroundView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .p(px(26.0))
            .gap(px(14.0))
            .child(page_header(
                "Playground",
                "Send a real request through the gateway, exactly as a client would.",
            ))
            .child(self.render_error())
            .child(self.render_form(cx))
            .child(self.render_answer())
    }
}

/// The request body of one playground request — the OpenAI chat-completions
/// shape the Svelte view sends.
fn request_body(model: &str, prompt: &str) -> Value {
    json!({
        "model": model,
        "messages": [{ "role": "user", "content": prompt }],
        "max_tokens": MAX_TOKENS,
    })
}

/// The answer of a completion payload.
///
/// The Svelte view shows `choices[0].message.content` and falls back to a
/// pretty dump of the choice when the model answered with something else —
/// tool calls, for example.
fn reply_of(payload: &Value) -> String {
    let choice = payload
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|choices| choices.first());

    let text = choice
        .and_then(|choice| choice.get("message"))
        .and_then(|message| message.get("content"))
        .and_then(content_text);

    match text {
        Some(text) => text,
        None => pretty(choice.unwrap_or(payload)),
    }
}

/// The text of a message content: a plain string, or the text parts of a
/// multimodal content array.
fn content_text(content: &Value) -> Option<String> {
    match content {
        Value::String(text) => Some(text.clone()),
        Value::Array(parts) => {
            let text: Vec<&str> = parts
                .iter()
                .filter_map(|part| part.get("text").and_then(Value::as_str))
                .collect();
            if text.is_empty() {
                None
            } else {
                Some(text.join(""))
            }
        }
        _ => None,
    }
}

/// A pretty JSON dump, for a reply that is not text.
fn pretty(value: &Value) -> String {
    serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string())
}

/// The `model · finish_reason · ms · tokens` line under the answer.
fn meta_of(payload: &Value, requested_model: &str, elapsed_ms: u128) -> String {
    let model = payload
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or(requested_model);
    let finish = payload
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|choices| choices.first())
        .and_then(|choice| choice.get("finish_reason"))
        .and_then(Value::as_str)
        .unwrap_or("?");
    let tokens = payload
        .get("usage")
        .and_then(|usage| usage.get("total_tokens"))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    format!("{model} · {finish} · {elapsed_ms} ms · {tokens} tokens")
}

/// A readable message for a failed request.
///
/// The `/v1/*` endpoints answer with `{"error": {"message": …}}` while the
/// admin endpoints use a plain string; both are unwrapped here so the banner
/// shows the sentence and not the raw JSON.
fn tidy_error(error: &ApiError) -> String {
    let detail = serde_json::from_str::<Value>(error.message.trim())
        .ok()
        .and_then(|value| {
            value
                .get("error")
                .and_then(|error| {
                    error
                        .get("message")
                        .and_then(Value::as_str)
                        .or_else(|| error.as_str())
                })
                .or_else(|| value.get("message").and_then(Value::as_str))
                .map(str::to_string)
        });

    match detail {
        Some(text) if error.status != 0 => {
            let status = error.status;
            format!("{text} (HTTP {status})")
        }
        Some(text) => text,
        None => error.to_string(),
    }
}

/// The failure banner of the Svelte bundle.
///
/// The shared `error_banner` carries the margins of the content area — it is
/// meant to sit directly in it — so inside this padded column the banner is
/// drawn here, with the same colours and the same radius.
fn error_box(message: impl Into<SharedString>) -> impl IntoElement {
    div()
        .p(px(9.0))
        .px(px(12.0))
        .rounded(px(theme::RADIUS))
        .border_1()
        .border_color(theme::bad())
        .bg(theme::bad_faint())
        .text_size(px(13.0))
        .text_color(Hsla::from(rgba(0xffd7d5ff)))
        .child(message.into())
}

/// The callback a text field reports its changes to.
type ChangeHandler<T> = Box<dyn FnMut(&str, &mut Context<T>)>;

/// A multi-line prompt field.
///
/// The shared `TextInput` is single-line and the Svelte view uses a
/// `<textarea>`, so the prompt gets its own small editor here.
struct PromptArea {
    focus: FocusHandle,
    value: String,
    /// Caret position, as a byte offset into `value`.
    caret: usize,
    /// Selection anchor, as a byte offset into `value`.
    anchor: Option<usize>,
    on_change: Option<ChangeHandler<Self>>,
}

impl PromptArea {
    /// A new field holding `initial`.
    fn new(cx: &mut Context<Self>, initial: impl Into<String>) -> Self {
        let value = initial.into();
        let caret = value.len();
        Self {
            focus: cx.focus_handle(),
            value,
            caret,
            anchor: None,
            on_change: None,
        }
    }

    /// Called after every change to the value.
    fn set_on_change(&mut self, f: impl FnMut(&str, &mut Context<Self>) + 'static) {
        self.on_change = Some(Box::new(f));
    }

    /// The selected range, if any.
    fn selection(&self) -> Option<(usize, usize)> {
        let anchor = self.anchor?;
        if anchor == self.caret {
            return None;
        }
        Some(if anchor < self.caret {
            (anchor, self.caret)
        } else {
            (self.caret, anchor)
        })
    }

    /// Insert text at the caret, replacing the selection when there is one.
    fn insert(&mut self, text: &str, cx: &mut Context<Self>) {
        let (start, end) = self.selection().unwrap_or((self.caret, self.caret));
        let start = char_boundary(&self.value, start);
        let end = char_boundary(&self.value, end.max(start));
        self.value.replace_range(start..end, text);
        self.caret = start + text.len();
        self.anchor = None;
        self.notify_change(cx);
    }

    /// Delete the selection, or the character before the caret.
    fn backspace(&mut self, cx: &mut Context<Self>) {
        let (start, end) = match self.selection() {
            Some(range) => range,
            None => {
                if self.caret == 0 {
                    return;
                }
                let previous = previous_char_boundary(&self.value, self.caret);
                (previous, self.caret)
            }
        };
        self.value.replace_range(start..end, "");
        self.caret = start;
        self.anchor = None;
        self.notify_change(cx);
    }

    /// Delete the selection, or the character after the caret.
    fn delete_forward(&mut self, cx: &mut Context<Self>) {
        let (start, end) = match self.selection() {
            Some(range) => range,
            None => {
                if self.caret >= self.value.len() {
                    return;
                }
                let next = next_char_boundary(&self.value, self.caret);
                (self.caret, next)
            }
        };
        self.value.replace_range(start..end, "");
        self.caret = start;
        self.anchor = None;
        self.notify_change(cx);
    }

    /// Move the caret, optionally extending the selection.
    fn move_cursor(&mut self, offset: isize, extend: bool) {
        let target = if offset < 0 {
            self.value
                .char_indices()
                .rev()
                .map(|(index, _)| index)
                .find(|index| (*index as isize) < self.caret as isize)
                .unwrap_or(0)
        } else {
            self.value
                .char_indices()
                .map(|(index, _)| index)
                .find(|index| *index > self.caret)
                .unwrap_or(self.value.len())
        };
        self.caret = target;
        self.anchor = if extend {
            self.anchor.or(Some(target))
        } else {
            None
        };
    }

    /// Handle one key press.
    fn handle_key(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) {
        let keystroke = &event.keystroke;
        let command = keystroke.modifiers.platform;

        if command {
            match keystroke.key.as_str() {
                "a" => {
                    self.anchor = Some(0);
                    self.caret = self.value.len();
                    return;
                }
                "v" => {
                    let text = crate::api::app_mut(cx)
                        .read_from_clipboard()
                        .and_then(|item| item.text());
                    if let Some(text) = text {
                        self.insert(&text, cx);
                    }
                    return;
                }
                "c" => {
                    if let Some((start, end)) = self.selection()
                        && let Some(text) = self.value.get(start..end).map(str::to_string)
                    {
                        crate::api::app_mut(cx).write_to_clipboard(ClipboardItem::new_string(text));
                    }
                    return;
                }
                "x" => {
                    if let Some((start, end)) = self.selection()
                        && let Some(text) = self.value.get(start..end).map(str::to_string)
                    {
                        crate::api::app_mut(cx).write_to_clipboard(ClipboardItem::new_string(text));
                        self.value.replace_range(start..end, "");
                        self.caret = start;
                        self.anchor = None;
                        self.notify_change(cx);
                    }
                    return;
                }
                _ => {}
            }
        }

        match keystroke.key.as_str() {
            "backspace" => self.backspace(cx),
            "delete" => self.delete_forward(cx),
            "left" => self.move_cursor(-1, keystroke.modifiers.shift),
            "right" => self.move_cursor(1, keystroke.modifiers.shift),
            "home" => {
                self.caret = 0;
                self.anchor = None;
            }
            "end" => {
                self.caret = self.value.len();
                self.anchor = None;
            }
            // A textarea does not submit on Enter: it adds a line.
            "enter" => self.insert("\n", cx),
            _ => {
                if let Some(text) = keystroke.key_char.as_ref()
                    && !text.is_empty()
                    && !text.chars().all(char::is_control)
                {
                    self.insert(text, cx);
                }
            }
        }
    }

    /// Tell the owner that the value changed.
    fn notify_change(&mut self, cx: &mut Context<Self>) {
        let value = self.value.clone();
        if let Some(on_change) = self.on_change.as_mut() {
            on_change(&value, cx);
        }
        cx.notify();
    }
}

impl Render for PromptArea {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let focused = self.focus.is_focused(window);
        let value = self.value.clone();

        div()
            .id(theme::element_id("playground-prompt-area"))
            .w_full()
            .min_h(px(90.0))
            .p(px(9.0))
            .rounded(px(theme::RADIUS_SM))
            .border_1()
            .border_color(if focused {
                theme::accent()
            } else {
                theme::border()
            })
            .bg(theme::panel())
            .text_size(px(13.0))
            .text_color(theme::text())
            .whitespace_normal()
            .track_focus(&self.focus)
            .tab_stop(true)
            .cursor(CursorStyle::IBeam)
            .on_click(cx.listener(|this, _, window, _| {
                window.focus(&this.focus);
            }))
            .on_key_down(cx.listener(|this, event, _, cx| {
                this.handle_key(event, cx);
            }))
            .child(value)
    }
}

/// The byte offset of the character before `cursor`.
fn previous_char_boundary(value: &str, cursor: usize) -> usize {
    value
        .char_indices()
        .rev()
        .find(|(index, _)| *index < cursor)
        .map(|(index, _)| index)
        .unwrap_or(0)
}

/// The byte offset of the character after `cursor`.
fn next_char_boundary(value: &str, cursor: usize) -> usize {
    value
        .char_indices()
        .map(|(index, _)| index)
        .find(|index| *index > cursor)
        .unwrap_or(value.len())
}

/// The largest char boundary at or below `index`.
///
/// Keeps an offset that somehow drifted off a boundary from panicking the
/// editor — the offsets are ours, but a panic in a text field is never worth
/// a byte of state.
fn char_boundary(value: &str, index: usize) -> usize {
    let mut index = index.min(value.len());
    while !value.is_char_boundary(index) {
        index -= 1;
    }
    index
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_request_body_is_the_openai_shape() {
        let body = request_body("combo-a", "hello");
        assert_eq!(body.get("model").and_then(Value::as_str), Some("combo-a"));
        assert_eq!(body.get("max_tokens").and_then(Value::as_u64), Some(512));
        let messages = body
            .get("messages")
            .and_then(Value::as_array)
            .expect("messages");
        assert_eq!(messages.len(), 1);
        assert_eq!(
            messages[0].get("role").and_then(Value::as_str),
            Some("user")
        );
        assert_eq!(
            messages[0].get("content").and_then(Value::as_str),
            Some("hello")
        );
    }

    #[test]
    fn the_reply_is_the_first_choice() {
        let payload = json!({
            "model": "combo-a",
            "choices": [{
                "index": 0,
                "message": {"role": "assistant", "content": "PONG"},
                "finish_reason": "stop"
            }],
            "usage": {"prompt_tokens": 3, "completion_tokens": 1, "total_tokens": 4}
        });
        assert_eq!(reply_of(&payload), "PONG");
        assert_eq!(
            meta_of(&payload, "combo-a", 123),
            "combo-a · stop · 123 ms · 4 tokens"
        );
    }

    #[test]
    fn a_reply_without_text_falls_back_to_the_choice() {
        let payload = json!({
            "choices": [{
                "index": 0,
                "message": {"role": "assistant", "content": null, "tool_calls": []},
                "finish_reason": "tool_calls"
            }]
        });
        let reply = reply_of(&payload);
        assert!(reply.contains("tool_calls"), "{reply}");
        assert_eq!(
            meta_of(&payload, "combo-a", 10),
            "combo-a · tool_calls · 10 ms · 0 tokens"
        );
    }

    #[test]
    fn a_multimodal_answer_is_joined() {
        let payload = json!({
            "choices": [{
                "message": {"content": [
                    {"type": "text", "text": "one "},
                    {"type": "text", "text": "two"}
                ]},
                "finish_reason": "stop"
            }]
        });
        assert_eq!(reply_of(&payload), "one two");
    }

    #[test]
    fn gateway_errors_are_unwrapped() {
        let error = ApiError {
            status: 401,
            message: r#"{"error":{"message":"invalid api key","type":"invalid_request_error"}}"#
                .to_string(),
        };
        assert_eq!(tidy_error(&error), "invalid api key (HTTP 401)");

        let error = ApiError {
            status: 400,
            message: "name must not be empty".to_string(),
        };
        assert_eq!(tidy_error(&error), "HTTP 400: name must not be empty");
    }
}

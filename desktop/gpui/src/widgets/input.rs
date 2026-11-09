//! A single-line text input.
//!
//! gpui has the plumbing for text input (`EntityInputHandler`,
//! `ElementInputHandler`) but no widget. This is the one the dashboard needs:
//! a bordered field that takes focus on click, accepts typing, deletion,
//! caret movement and paste, and reports every change to its owner.
//!
//! The field keeps byte offsets into the value and turns them into pixels with
//! the text system (`window.text_system().shape_line`), so the caret and the
//! selection sit exactly on the glyphs — multi-byte characters included.

use std::cell::Cell;
use std::ops::Range;
use std::rc::Rc;

use gpui::prelude::*;
use gpui::{
    AnyElement, App, Bounds, ClickEvent, ClipboardItem, Context, CursorStyle, Element, ElementId,
    Entity, EntityInputHandler, FocusHandle, Font, FontFeatures, FontStyle, FontWeight,
    GlobalElementId, InspectorElementId, IntoElement, KeyDownEvent, LayoutId, Pixels, Point,
    Render, ShapedLine, SharedString, StyledText, TextRun, UTF16Selection, UnderlineStyle, Window,
    div, point, px, size,
};

use crate::theme;

/// The font size of the text, in pixels.
const FONT_SIZE: f32 = 13.0;
/// The height of one line of text, in pixels.
const LINE_HEIGHT: f32 = 18.0;
/// The system font. The field pins it, so measuring and painting agree.
const FONT_FAMILY: &str = ".SystemUIFont";
/// The width of the caret, in pixels.
const CARET_WIDTH: f32 = 2.0;
/// The horizontal padding of the field, in pixels.
const PADDING_X: f32 = 9.0;
/// The height of the field, in pixels.
const FIELD_HEIGHT: f32 = 34.0;
/// The character a masked field shows instead of the value.
const MASK: char = '\u{2022}';

/// The callback of [`TextInput::set_on_change`].
type OnChange = Box<dyn FnMut(&str, &mut Context<TextInput>)>;

/// The callback of [`TextInput::set_on_submit`].
type OnSubmit = Box<dyn FnMut(&str, &mut Window, &mut App)>;

/// A single-line text field.
pub struct TextInput {
    focus: FocusHandle,
    value: String,
    placeholder: String,
    password: bool,
    /// Whether the field accepts input at all.
    enabled: bool,
    /// Caret position, as a byte offset into `value`.
    cursor: usize,
    /// Selection anchor, as a byte offset into `value`.
    anchor: Option<usize>,
    /// The range of the value the IME is composing, as byte offsets.
    marked: Option<Range<usize>>,
    /// The bounds of the painted text, shared with the element that paints it.
    text_bounds: Rc<Cell<Option<Bounds<Pixels>>>>,
    on_change: Option<OnChange>,
    on_submit: Option<OnSubmit>,
}

impl TextInput {
    /// A new input holding `initial`.
    pub fn new(cx: &mut Context<Self>, initial: impl Into<String>) -> Self {
        let value = initial.into();
        let cursor = value.len();
        Self {
            focus: cx.focus_handle(),
            value,
            placeholder: String::new(),
            password: false,
            enabled: true,
            cursor,
            anchor: None,
            marked: None,
            text_bounds: Rc::new(Cell::new(None)),
            on_change: None,
            on_submit: None,
        }
    }

    /// The greyed-out hint shown while the field is empty.
    pub fn set_placeholder(&mut self, text: impl Into<String>) {
        self.placeholder = text.into();
    }

    /// Mask the value, for secret entry.
    pub fn set_password(&mut self, yes: bool) {
        self.password = yes;
    }

    /// Enable or disable the field. A disabled field cannot be focused and
    /// ignores every key.
    #[allow(dead_code)] // part of the widget API, for the views that show a read-only field
    pub fn set_enabled(&mut self, yes: bool) {
        self.enabled = yes;
    }

    /// Whether the field accepts input.
    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// Called after every change to the value.
    pub fn set_on_change(&mut self, f: impl FnMut(&str, &mut Context<Self>) + 'static) {
        self.on_change = Some(Box::new(f));
    }

    /// Called when the operator presses Enter.
    pub fn set_on_submit(&mut self, f: impl FnMut(&str, &mut Window, &mut App) + 'static) {
        self.on_submit = Some(Box::new(f));
    }

    /// The current value.
    pub fn value(&self) -> &str {
        &self.value
    }

    /// Replace the value and put the caret at its end.
    pub fn set_value(&mut self, value: impl Into<String>) {
        self.value = value.into();
        self.cursor = self.value.len();
        self.anchor = None;
        self.marked = None;
    }

    /// Empty the field.
    pub fn clear(&mut self) {
        self.value.clear();
        self.cursor = 0;
        self.anchor = None;
        self.marked = None;
    }

    /// The focus handle of this field.
    pub fn focus_handle(&self) -> &FocusHandle {
        &self.focus
    }

    /// Whether the field currently has the keyboard focus.
    pub fn is_focused(&self, window: &mut Window) -> bool {
        self.focus.is_focused(window)
    }

    /// The selected range, if any, as byte offsets into the value.
    fn selection(&self) -> Option<(usize, usize)> {
        selection_range(self.anchor, self.cursor)
    }

    /// The byte offset in the displayed text that matches `offset` in the value.
    ///
    /// A masked field shows one bullet per character, so the two differ; an
    /// ordinary field shows the value verbatim and the offsets are the same.
    fn display_offset(&self, offset: usize) -> usize {
        if !self.password {
            return offset;
        }
        let characters = match self.value.get(..offset) {
            Some(prefix) => prefix.chars().count(),
            None => self.value.chars().count(),
        };
        characters * MASK.len_utf8()
    }

    /// The byte offset in the value that matches `offset` in the displayed text.
    fn value_offset(&self, offset: usize) -> usize {
        if !self.password {
            return offset.min(self.value.len());
        }
        let index = offset / MASK.len_utf8();
        self.value
            .char_indices()
            .nth(index)
            .map(|(byte, _)| byte)
            .unwrap_or(self.value.len())
    }

    /// The text as it is displayed — masked for secrets, without the control
    /// characters a single-line field cannot show.
    fn display_text(&self) -> String {
        if self.password {
            return MASK.to_string().repeat(self.value.chars().count());
        }
        let mut text = String::with_capacity(self.value.len());
        for character in self.value.chars() {
            if character.is_control() {
                // Keep the byte length, so that the offsets still line up.
                for _ in 0..character.len_utf8() {
                    text.push(' ');
                }
            } else {
                text.push(character);
            }
        }
        text
    }

    /// Move the caret, optionally extending the selection.
    fn move_cursor(&mut self, target: usize, extend: bool) {
        let previous = self.cursor;
        self.cursor = target.min(self.value.len());
        self.anchor = if extend {
            Some(self.anchor.unwrap_or(previous))
        } else {
            None
        };
    }

    /// Move the caret one character to the left or to the right.
    fn move_by_char(&mut self, direction: isize, extend: bool) {
        let target = if direction < 0 {
            previous_char_boundary(&self.value, self.cursor)
        } else {
            next_char_boundary(&self.value, self.cursor)
        };
        self.move_cursor(target, extend);
    }

    /// Insert text at the caret, replacing the selection when there is one.
    fn insert(&mut self, text: &str, cx: &mut Context<Self>) {
        if insert_text(&mut self.value, &mut self.cursor, &mut self.anchor, text) {
            self.marked = None;
            self.notify_change(cx);
        }
    }

    /// Delete the selection, or the character before the caret.
    fn backspace(&mut self, cx: &mut Context<Self>) {
        if delete_backward(&mut self.value, &mut self.cursor, &mut self.anchor) {
            self.marked = None;
            self.notify_change(cx);
        }
    }

    /// Delete the selection, or the character after the caret.
    fn delete_forward(&mut self, cx: &mut Context<Self>) {
        if delete_forward(&mut self.value, &mut self.cursor, &mut self.anchor) {
            self.marked = None;
            self.notify_change(cx);
        }
    }

    /// Select the whole value.
    fn select_all(&mut self) {
        self.anchor = Some(0);
        self.cursor = self.value.len();
    }

    /// Copy the selection to the clipboard.
    fn copy_selection(&mut self, cx: &mut Context<Self>) {
        if let Some((start, end)) = self.selection()
            && let Some(text) = self.value.get(start..end)
        {
            crate::api::app_mut(cx).write_to_clipboard(ClipboardItem::new_string(text.to_string()));
        }
    }

    /// Copy the selection to the clipboard and delete it.
    fn cut_selection(&mut self, cx: &mut Context<Self>) {
        if let Some((start, end)) = self.selection()
            && let Some(text) = self.value.get(start..end)
        {
            let text = text.to_string();
            crate::api::app_mut(cx).write_to_clipboard(ClipboardItem::new_string(text));
            if !replace_range(&mut self.value, start, end, "") {
                return;
            }
            self.cursor = start;
            self.anchor = None;
            self.marked = None;
            self.notify_change(cx);
        }
    }

    /// Paste the clipboard over the selection.
    fn paste(&mut self, cx: &mut Context<Self>) {
        let text = crate::api::app_mut(cx)
            .read_from_clipboard()
            .and_then(|item| item.text());
        if let Some(text) = text {
            self.insert(&text, cx);
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

    /// Handle one key press.
    fn handle_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if !self.enabled {
            return;
        }

        let keystroke = &event.keystroke;
        let command = keystroke.modifiers.platform;
        let shift = keystroke.modifiers.shift;

        if command {
            match keystroke.key.as_str() {
                "a" => {
                    self.select_all();
                    cx.stop_propagation();
                    return;
                }
                "c" => {
                    self.copy_selection(cx);
                    cx.stop_propagation();
                    return;
                }
                "x" => {
                    self.cut_selection(cx);
                    cx.stop_propagation();
                    return;
                }
                "v" => {
                    self.paste(cx);
                    cx.stop_propagation();
                    return;
                }
                "backspace" => {
                    self.delete_to_start(cx);
                    cx.stop_propagation();
                    return;
                }
                "left" => {
                    self.move_cursor(0, shift);
                    cx.stop_propagation();
                    return;
                }
                "right" => {
                    self.move_cursor(self.value.len(), shift);
                    cx.stop_propagation();
                    return;
                }
                _ => {}
            }
        }

        match keystroke.key.as_str() {
            "backspace" => {
                self.backspace(cx);
                cx.stop_propagation();
            }
            "delete" => {
                self.delete_forward(cx);
                cx.stop_propagation();
            }
            "left" => {
                self.move_by_char(-1, shift);
                cx.stop_propagation();
            }
            "right" => {
                self.move_by_char(1, shift);
                cx.stop_propagation();
            }
            "home" => {
                self.move_cursor(0, shift);
                cx.stop_propagation();
            }
            "end" => {
                self.move_cursor(self.value.len(), shift);
                cx.stop_propagation();
            }
            "enter" => {
                let value = self.value.clone();
                if let Some(on_submit) = self.on_submit.as_mut() {
                    on_submit(&value, window, crate::api::app_mut(cx));
                }
                cx.stop_propagation();
            }
            _ => {
                if let Some(text) = keystroke.key_char.as_ref()
                    && !text.is_empty()
                    && !text.chars().all(char::is_control)
                {
                    self.insert(text, cx);
                    cx.stop_propagation();
                }
            }
        }
    }

    /// Delete everything before the caret.
    fn delete_to_start(&mut self, cx: &mut Context<Self>) {
        let (start, end) = self.selection().unwrap_or((0, self.cursor));
        if start == end {
            return;
        }
        if !replace_range(&mut self.value, start, end, "") {
            return;
        }
        self.cursor = start;
        self.anchor = None;
        self.marked = None;
        self.notify_change(cx);
    }

    /// Focus the field and put the caret where the operator clicked.
    fn handle_click(&mut self, event: &ClickEvent, window: &mut Window, cx: &mut Context<Self>) {
        if !self.enabled {
            return;
        }
        window.focus(&self.focus);

        if let ClickEvent::Mouse(mouse) = event
            && let Some(bounds) = self.text_bounds.get()
        {
            let display = self.display_text();
            let metrics = measure(&display, window);
            let offset = metrics.index_at(mouse.up.position.x - bounds.origin.x);
            let offset = self.value_offset(offset);
            self.move_cursor(offset, false);
        }
        cx.notify();
    }

    /// Replace the byte range `start..end` with `text` and put the caret after it.
    fn replace(&mut self, start: usize, end: usize, text: &str, cx: &mut Context<Self>) {
        let text = sanitize(text);
        if !replace_range(&mut self.value, start, end, &text) {
            return;
        }
        self.cursor = start + text.len();
        self.anchor = None;
        self.marked = None;
        self.notify_change(cx);
    }
}

/// The element that paints the text of the field.
///
/// It exists for two reasons: it hands the field to the platform as the
/// current text input (so that the system clipboard, the IME and the
/// candidate window talk to this widget), and it remembers where the text
/// ended up, so that a click can put the caret where the operator clicked.
struct TextElement {
    focus: FocusHandle,
    entity: Entity<TextInput>,
    content: AnyElement,
    bounds: Rc<Cell<Option<Bounds<Pixels>>>>,
}

impl Element for TextElement {
    type RequestLayoutState = ();
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        (self.content.request_layout(window, cx), ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        self.bounds.set(Some(bounds));
        self.content.prepaint(window, cx);
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        _prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        // The platform only talks to the input handler of the focused element,
        // and only from the paint phase.
        window.handle_input(
            &self.focus,
            gpui::ElementInputHandler::new(bounds, self.entity.clone()),
            cx,
        );
        self.content.paint(window, cx);
    }
}

impl IntoElement for TextElement {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl EntityInputHandler for TextInput {
    fn text_for_range(
        &mut self,
        range: Range<usize>,
        adjusted_range: &mut Option<Range<usize>>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<String> {
        let start = utf16_to_byte(&self.value, range.start);
        let end = utf16_to_byte(&self.value, range.end);
        *adjusted_range = Some(byte_to_utf16(&self.value, start)..byte_to_utf16(&self.value, end));
        self.value.get(start..end).map(str::to_string)
    }

    fn selected_text_range(
        &mut self,
        ignore_disabled_input: bool,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        if !self.enabled && !ignore_disabled_input {
            return None;
        }
        let (start, end) = self.selection()?;
        Some(UTF16Selection {
            range: byte_to_utf16(&self.value, start)..byte_to_utf16(&self.value, end),
            reversed: self.anchor.unwrap_or(self.cursor) > self.cursor,
        })
    }

    fn marked_text_range(
        &self,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Range<usize>> {
        let marked = self.marked.as_ref()?;
        Some(byte_to_utf16(&self.value, marked.start)..byte_to_utf16(&self.value, marked.end))
    }

    fn unmark_text(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        if self.marked.take().is_some() {
            cx.notify();
        }
    }

    fn replace_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        text: &str,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (start, end) = match range {
            Some(range) => (
                utf16_to_byte(&self.value, range.start),
                utf16_to_byte(&self.value, range.end),
            ),
            None => self.selection().unwrap_or((self.cursor, self.cursor)),
        };
        self.replace(start, end, text, cx);
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        new_text: &str,
        new_selected_range: Option<Range<usize>>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (start, end) = match range {
            Some(range) => (
                utf16_to_byte(&self.value, range.start),
                utf16_to_byte(&self.value, range.end),
            ),
            None => self.selection().unwrap_or((self.cursor, self.cursor)),
        };
        let new_text = sanitize(new_text);
        if !replace_range(&mut self.value, start, end, &new_text) {
            return;
        }
        let marked_end = start + new_text.len();
        self.marked = Some(start..marked_end);
        // The caret follows the selection inside the text being composed.
        self.cursor = match new_selected_range {
            Some(selection) => start + utf16_to_byte(&new_text, selection.end),
            None => marked_end,
        };
        self.anchor = None;
        self.notify_change(cx);
    }

    fn bounds_for_range(
        &mut self,
        range_utf16: Range<usize>,
        element_bounds: Bounds<Pixels>,
        window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let start = utf16_to_byte(&self.value, range_utf16.start);
        let display = self.display_text();
        let metrics = measure(&display, window);
        let x = metrics.x_for(self.display_offset(start));
        Some(Bounds::new(
            point(element_bounds.origin.x + x, element_bounds.origin.y),
            size(px(1.0), element_bounds.size.height),
        ))
    }

    fn character_index_for_point(
        &mut self,
        point: Point<Pixels>,
        window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<usize> {
        let bounds = self.text_bounds.get()?;
        let display = self.display_text();
        let metrics = measure(&display, window);
        let offset = metrics.index_at(point.x - bounds.origin.x);
        let offset = self.value_offset(offset);
        Some(byte_to_utf16(&self.value, offset))
    }
}

impl Render for TextInput {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let focused = self.is_focused(window);
        let enabled = self.is_enabled();
        let display = self.display_text();
        let metrics = measure(&display, window);

        let selection = self
            .selection()
            .map(|(start, end)| (self.display_offset(start), self.display_offset(end)));
        let marked = self.marked.as_ref().map(|range| {
            (
                self.display_offset(range.start),
                self.display_offset(range.end),
            )
        });
        let caret = metrics.x_for(self.display_offset(self.cursor));

        let text =
            StyledText::new(display.clone()).with_runs(build_runs(&display, selection, marked));

        let caret = div()
            .absolute()
            .left(px(caret.into()))
            .top_0()
            .w(px(CARET_WIDTH))
            .h(px(LINE_HEIGHT))
            .bg(theme::accent());

        // The placeholder sits where the value would be; the caret only shows
        // while the field has the focus.
        let mut row = div().relative().flex().items_center();
        if display.is_empty() {
            row = row.child(
                div()
                    .text_color(theme::muted())
                    .child(self.placeholder.clone()),
            );
        } else {
            row = row.child(text);
        }
        if focused && enabled {
            row = row.child(caret);
        }
        let content: AnyElement = row.into_any_element();

        let content = TextElement {
            focus: self.focus.clone(),
            entity: cx.entity(),
            content,
            bounds: self.text_bounds.clone(),
        };

        div()
            .id(theme::element_id(format!(
                "text-input:{}",
                cx.entity_id().as_u64()
            )))
            .flex()
            .items_center()
            .h(px(FIELD_HEIGHT))
            .px(px(PADDING_X))
            .rounded(px(theme::RADIUS_SM))
            .border_1()
            .border_color(if enabled && focused {
                theme::accent()
            } else {
                theme::border()
            })
            .bg(if enabled {
                theme::panel()
            } else {
                theme::panel_2()
            })
            .overflow_hidden()
            .text_size(px(FONT_SIZE))
            .line_height(px(LINE_HEIGHT))
            .font(text_font())
            .text_color(if enabled {
                theme::text()
            } else {
                theme::muted()
            })
            .whitespace_nowrap()
            .track_focus(&self.focus)
            .tab_stop(true)
            .cursor(if enabled {
                CursorStyle::IBeam
            } else {
                CursorStyle::Arrow
            })
            .on_click(cx.listener(|this, event, window, cx| {
                this.handle_click(event, window, cx);
            }))
            .on_key_down(cx.listener(|this, event, window, cx| {
                this.handle_key(event, window, cx);
            }))
            .child(content)
    }
}

/// The measured geometry of the displayed text.
struct Metrics {
    /// The shaped line, used to map byte offsets to pixel offsets.
    line: ShapedLine,
}

impl Metrics {
    /// The horizontal offset of the given byte offset in the displayed text.
    fn x_for(&self, byte: usize) -> Pixels {
        self.line.x_for_index(byte)
    }

    /// The byte offset of the character closest to the given horizontal offset.
    fn index_at(&self, x: Pixels) -> usize {
        self.line.closest_index_for_x(x)
    }
}

/// Shape the displayed text and measure it.
fn measure(display: &str, window: &Window) -> Metrics {
    let run = TextRun {
        len: display.len(),
        font: text_font(),
        color: theme::text(),
        background_color: None,
        underline: None,
        strikethrough: None,
    };
    let line =
        window
            .text_system()
            .shape_line(SharedString::new(display), px(FONT_SIZE), &[run], None);
    Metrics { line }
}

/// The font of the field. The element pins the same font, so that the
/// measurement and the painted text agree.
fn text_font() -> Font {
    Font {
        family: FONT_FAMILY.into(),
        features: FontFeatures::default(),
        fallbacks: None,
        weight: FontWeight(400.0),
        style: FontStyle::Normal,
    }
}

/// The text runs of the displayed text: the selection gets a background, the
/// text the IME is composing gets an underline.
fn build_runs(
    display: &str,
    selection: Option<(usize, usize)>,
    marked: Option<(usize, usize)>,
) -> Vec<TextRun> {
    let mut boundaries = vec![0, display.len()];
    for (start, end) in [selection, marked].into_iter().flatten() {
        boundaries.push(start);
        boundaries.push(end);
    }
    boundaries.sort_unstable();
    boundaries.dedup();
    boundaries.retain(|boundary| display.is_char_boundary(*boundary));

    let mut runs = Vec::with_capacity(boundaries.len());
    for pair in boundaries.windows(2) {
        let (start, end) = (pair[0], pair[1]);
        if start == end {
            continue;
        }
        let mut run = TextRun {
            len: end - start,
            font: text_font(),
            color: theme::text(),
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        if let Some((selection_start, selection_end)) = selection
            && start >= selection_start
            && end <= selection_end
        {
            run.background_color = Some(theme::accent_faint());
        }
        if let Some((marked_start, marked_end)) = marked
            && start >= marked_start
            && end <= marked_end
        {
            run.underline = Some(UnderlineStyle {
                thickness: px(1.0),
                color: Some(theme::muted()),
                wavy: false,
            });
        }
        runs.push(run);
    }
    runs
}

/// The ordered byte range of the selection, if there is one.
fn selection_range(anchor: Option<usize>, cursor: usize) -> Option<(usize, usize)> {
    let anchor = anchor?;
    if anchor == cursor {
        return None;
    }
    Some(if anchor < cursor {
        (anchor, cursor)
    } else {
        (cursor, anchor)
    })
}

/// Insert `text` at the caret, replacing the selection. Returns whether the
/// value changed.
fn insert_text(
    value: &mut String,
    cursor: &mut usize,
    anchor: &mut Option<usize>,
    text: &str,
) -> bool {
    let text = sanitize(text);
    if text.is_empty() {
        return false;
    }
    let (start, end) = selection_range(*anchor, *cursor).unwrap_or((*cursor, *cursor));
    if !replace_range(value, start, end, &text) {
        return false;
    }
    *cursor = start + text.len();
    *anchor = None;
    true
}

/// Delete the selection, or the character before the caret. Returns whether the
/// value changed.
fn delete_backward(value: &mut String, cursor: &mut usize, anchor: &mut Option<usize>) -> bool {
    let (start, end) = match selection_range(*anchor, *cursor) {
        Some(range) => range,
        None => {
            if *cursor == 0 {
                return false;
            }
            (previous_char_boundary(value, *cursor), *cursor)
        }
    };
    if !replace_range(value, start, end, "") {
        return false;
    }
    *cursor = start;
    *anchor = None;
    true
}

/// Delete the selection, or the character after the caret. Returns whether the
/// value changed.
fn delete_forward(value: &mut String, cursor: &mut usize, anchor: &mut Option<usize>) -> bool {
    let (start, end) = match selection_range(*anchor, *cursor) {
        Some(range) => range,
        None => {
            if *cursor >= value.len() {
                return false;
            }
            (*cursor, next_char_boundary(value, *cursor))
        }
    };
    if !replace_range(value, start, end, "") {
        return false;
    }
    *cursor = start;
    *anchor = None;
    true
}

/// Replace the byte range `start..end` with `text`. Returns whether the value
/// was changed; an invalid range is ignored rather than panicking.
fn replace_range(value: &mut String, start: usize, end: usize, text: &str) -> bool {
    if start > end
        || end > value.len()
        || !value.is_char_boundary(start)
        || !value.is_char_boundary(end)
    {
        return false;
    }
    value.replace_range(start..end, text);
    true
}

/// A single-line field cannot hold line breaks.
fn sanitize(text: &str) -> String {
    text.chars()
        .map(|character| match character {
            '\n' | '\r' => ' ',
            other => other,
        })
        .collect()
}

/// The byte offset of the character before `cursor`.
fn previous_char_boundary(value: &str, cursor: usize) -> usize {
    value
        .char_indices()
        .rev()
        .map(|(index, _)| index)
        .find(|index| *index < cursor)
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

/// The number of UTF-16 code units of `text`.
fn utf16_len(text: &str) -> usize {
    text.chars().map(char::len_utf16).sum()
}

/// The UTF-16 offset of the byte offset `byte` in `text`.
fn byte_to_utf16(text: &str, byte: usize) -> usize {
    match text.get(..byte) {
        Some(prefix) => utf16_len(prefix),
        None => utf16_len(text),
    }
}

/// The byte offset of the UTF-16 offset `offset` in `text`.
fn utf16_to_byte(text: &str, offset: usize) -> usize {
    let mut utf16 = 0;
    for (byte, character) in text.char_indices() {
        if utf16 >= offset {
            return byte;
        }
        utf16 += character.len_utf16();
    }
    text.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn char_boundaries_survive_multibyte_text() {
        let value = "aäb";
        assert_eq!(previous_char_boundary(value, 3), 1);
        assert_eq!(next_char_boundary(value, 1), 3);
        assert_eq!(next_char_boundary(value, 3), 4);
        assert_eq!(previous_char_boundary(value, 0), 0);
    }

    #[test]
    fn char_boundaries_survive_four_byte_characters() {
        // "a" + U+1D11E (4 bytes) + "b"
        let value = "a\u{1d11e}b";
        assert_eq!(value.len(), 6);
        assert_eq!(previous_char_boundary(value, 5), 1);
        assert_eq!(next_char_boundary(value, 1), 5);
        assert_eq!(previous_char_boundary(value, 1), 0);
        assert_eq!(next_char_boundary(value, 5), 6);
    }

    #[test]
    fn char_boundaries_survive_combining_sequences() {
        // "e" followed by a combining acute accent, then "x"
        let value = "e\u{0301}x";
        assert_eq!(value.len(), 4);
        assert_eq!(previous_char_boundary(value, 3), 1);
        assert_eq!(next_char_boundary(value, 1), 3);
        assert_eq!(next_char_boundary(value, 3), 4);
    }

    #[test]
    fn backspace_removes_a_whole_character() {
        let mut value = String::from("a\u{1d11e}");
        let mut cursor = value.len();
        let mut anchor = None;
        assert!(delete_backward(&mut value, &mut cursor, &mut anchor));
        assert_eq!(value, "a");
        assert_eq!(cursor, 1);
    }

    #[test]
    fn delete_forward_removes_a_whole_character() {
        let mut value = String::from("\u{1d11e}a");
        let mut cursor = 0;
        let mut anchor = None;
        assert!(delete_forward(&mut value, &mut cursor, &mut anchor));
        assert_eq!(value, "a");
        assert_eq!(cursor, 0);
    }

    #[test]
    fn typing_replaces_the_selection() {
        let mut value = String::from("hello");
        let mut cursor = 5;
        let mut anchor = Some(0);
        assert!(insert_text(&mut value, &mut cursor, &mut anchor, "x"));
        assert_eq!(value, "x");
        assert_eq!(cursor, 1);
        assert_eq!(anchor, None);
    }

    #[test]
    fn deleting_removes_the_selection() {
        let mut value = String::from("hello");
        let mut cursor = 2;
        let mut anchor = Some(4);
        assert!(delete_backward(&mut value, &mut cursor, &mut anchor));
        assert_eq!(value, "heo");
        assert_eq!(cursor, 2);
    }

    #[test]
    fn line_breaks_never_enter_the_value() {
        let mut value = String::new();
        let mut cursor = 0;
        let mut anchor = None;
        assert!(insert_text(&mut value, &mut cursor, &mut anchor, "a\nb\rc"));
        assert_eq!(value, "a b c");
        assert_eq!(cursor, 5);
    }

    #[test]
    fn selection_is_ordered_whatever_the_anchor_is() {
        assert_eq!(selection_range(Some(4), 2), Some((2, 4)));
        assert_eq!(selection_range(Some(2), 4), Some((2, 4)));
        assert_eq!(selection_range(Some(2), 2), None);
        assert_eq!(selection_range(None, 2), None);
    }

    #[test]
    fn utf16_offsets_follow_the_code_units() {
        let value = "a\u{1d11e}b";
        // "a" is one code unit, the musical symbol is two, "b" is one.
        assert_eq!(utf16_len(value), 4);
        assert_eq!(byte_to_utf16(value, 5), 3);
        assert_eq!(utf16_to_byte(value, 3), 5);
        assert_eq!(utf16_to_byte(value, 99), value.len());
    }
}

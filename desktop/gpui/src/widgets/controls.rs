//! Buttons, badges, banners and the small layout pieces every view shares.
//!
//! The styles mirror `desktop/ui/src/app.css`: same radii, same border
//! colors, same muted/ok/warn/bad tones.

use gpui::prelude::*;
use gpui::{
    AnyElement, App, CursorStyle, FontWeight, Hsla, IntoElement, SharedString, Window, div, px,
    rgba,
};

use crate::theme;

/// The semantic tone of a badge or status pill.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    /// Neutral — muted text, plain border.
    Neutral,
    /// Healthy.
    Ok,
    /// Degraded but usable.
    Warn,
    /// Broken.
    Bad,
}

impl Tone {
    /// The color of this tone.
    pub fn color(self) -> Hsla {
        match self {
            Tone::Neutral => theme::muted(),
            Tone::Ok => theme::ok(),
            Tone::Warn => theme::warn(),
            Tone::Bad => theme::bad(),
        }
    }
}

/// A sidebar navigation entry.
///
/// The element id is the label: the sidebar shows every section exactly
/// once, so the label is a stable name for the entry.
pub fn nav_item(
    label: impl Into<SharedString>,
    active: bool,
    on_click: impl Fn(&mut Window, &mut App) + 'static,
) -> impl IntoElement {
    let label = label.into();
    let selector = nav_selector(&label);
    let base_bg = if active {
        theme::panel_2()
    } else {
        gpui::transparent_black()
    };
    let hover_bg = theme::panel_2();
    div()
        .id(theme::element_id(format!("nav:{}", label)))
        .flex()
        .items_center()
        .px(px(10.0))
        .py(px(8.0))
        .rounded(px(theme::RADIUS_SM))
        .text_size(px(14.0))
        .text_color(if active {
            theme::text()
        } else {
            theme::muted()
        })
        .font_weight(if active {
            FontWeight::SEMIBOLD
        } else {
            FontWeight::NORMAL
        })
        .bg(base_bg)
        .hover(move |style| style.bg(hover_bg).text_color(theme::text()))
        .cursor(CursorStyle::PointingHand)
        .debug_selector(move || selector.clone())
        .on_click(move |_event, window, cx| on_click(window, cx))
        .child(label)
}

/// A secondary button.
///
/// `key` names the button. It has to be the same on every frame — gpui keeps
/// the click state under the element id — and it has to differ between two
/// buttons that live in the same list.
pub fn button(
    key: impl Into<SharedString>,
    label: impl Into<SharedString>,
    on_click: impl Fn(&mut Window, &mut App) + 'static,
) -> impl IntoElement {
    let label = label.into();
    let key = key.into();
    let selector = button_selector(&key);
    div()
        .id(theme::element_id(key))
        .flex()
        .items_center()
        .px(px(12.0))
        .py(px(6.0))
        .rounded(px(theme::RADIUS_SM))
        .border_1()
        .border_color(theme::border())
        .bg(theme::panel())
        .text_size(px(13.0))
        .text_color(theme::text())
        .hover(|style| style.bg(theme::panel_2()).border_color(theme::muted()))
        .cursor(CursorStyle::PointingHand)
        .debug_selector(move || selector.clone())
        .on_click(move |_event, window, cx| on_click(window, cx))
        .child(label)
}

/// The debug selector of the button with the given key.
///
/// A button is found in a headless test by its key: `debug_bounds("button:Refresh")`
/// returns the button's rectangle, and a click at its centre goes through the
/// real hit-testing path. Outside a test build the selector is a no-op — gpui
/// compiles the recording version only with its `test-support` feature.
pub fn button_selector(key: &str) -> String {
    format!("button:{key}")
}

/// The debug selector of a sidebar entry with the given label.
///
/// The counterpart of [`button_selector`] for the navigation.
pub fn nav_selector(label: &str) -> String {
    format!("nav:{label}")
}

/// The primary action button. A disabled button does not react to clicks.
///
/// `key` names the button — see [`button`].
pub fn primary_button(
    key: impl Into<SharedString>,
    label: impl Into<SharedString>,
    enabled: bool,
    on_click: impl Fn(&mut Window, &mut App) + 'static,
) -> impl IntoElement {
    div()
        .id(theme::element_id(key))
        .flex()
        .items_center()
        .px(px(14.0))
        .py(px(8.0))
        .rounded(px(theme::RADIUS_SM))
        .bg(theme::accent())
        .opacity(if enabled { 1.0 } else { 0.55 })
        .text_size(px(13.0))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(Hsla::from(rgba(0x06101fff)))
        .when(enabled, |this| {
            this.cursor(CursorStyle::PointingHand)
                .hover(|style| style.opacity(0.9))
                .on_click(move |_event, window, cx| on_click(window, cx))
        })
        .child(label.into())
}

/// A small status pill.
pub fn badge(label: impl Into<SharedString>, tone: Tone) -> impl IntoElement {
    div()
        .flex()
        .items_center()
        .px(px(7.0))
        .py(px(1.0))
        .rounded_full()
        .border_1()
        .border_color(tone.color())
        .text_size(px(11.0))
        .text_color(tone.color())
        .child(label.into())
}

/// A status pill that keeps the width of its text.
///
/// [`badge`] is a block-level box, so inside a table cell it stretches to the
/// whole column and the text sits at its left edge. This one is sized by its
/// content — the pill of a table row looks like a pill, not like a field.
pub fn compact_badge(label: impl Into<SharedString>, tone: Tone) -> AnyElement {
    div()
        .flex()
        .items_center()
        .justify_center()
        .flex_none()
        .px(px(8.0))
        .py(px(2.0))
        .rounded(px(theme::RADIUS_SM))
        .border_1()
        .border_color(tone.color())
        .text_size(px(11.0))
        .text_color(tone.color())
        .child(label.into())
        .into_any_element()
}

/// A table button that keeps the width of its text.
///
/// The rows of every table carry their own actions, and they have to look
/// like one family: the same height, the same radius, the same type size. A
/// disabled button stays visible but dead — the row is saving.
pub fn compact_button(
    key: impl Into<SharedString>,
    label: impl Into<SharedString>,
    enabled: bool,
    on_click: impl Fn(&mut Window, &mut App) + 'static,
) -> AnyElement {
    let key = key.into();
    let label = label.into();
    let base = div()
        .id(theme::element_id(key))
        .flex()
        .items_center()
        .justify_center()
        .flex_none()
        .h(px(24.0))
        .px(px(10.0))
        .rounded(px(theme::RADIUS_SM))
        .border_1()
        .border_color(theme::border())
        .bg(theme::panel())
        .text_size(px(11.0))
        .text_color(if enabled {
            theme::text()
        } else {
            theme::muted()
        });

    if enabled {
        base.cursor(CursorStyle::PointingHand)
            .on_click(move |_event, window, cx| on_click(window, cx))
            .child(label)
            .into_any_element()
    } else {
        base.opacity(0.55).child(label).into_any_element()
    }
}

/// A bordered panel.
#[allow(dead_code)] // part of the widget kit — the plain `.card` of the CSS bundle
pub fn card(child: impl IntoElement) -> impl IntoElement {
    div()
        .bg(theme::panel())
        .border_1()
        .border_color(theme::border())
        .rounded(px(theme::RADIUS))
        .p(px(12.0))
        .px(px(14.0))
        .child(child)
}

/// A metric card: a big value with a muted label underneath.
pub fn stat_card(
    label: impl Into<SharedString>,
    value: impl Into<SharedString>,
) -> impl IntoElement {
    div()
        .flex()
        .flex_col()
        .gap_1()
        .bg(theme::panel())
        .border_1()
        .border_color(theme::border())
        .rounded(px(theme::RADIUS))
        .p(px(12.0))
        .px(px(14.0))
        .child(
            div()
                .text_size(px(22.0))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(theme::text())
                .child(value.into()),
        )
        .child(
            div()
                .text_size(px(12.0))
                .text_color(theme::muted())
                .child(label.into()),
        )
}

/// A section heading — the uppercase `h2` of the CSS bundle.
pub fn section_title(title: impl Into<SharedString>) -> impl IntoElement {
    div()
        .mt(px(22.0))
        .mb(px(8.0))
        .text_size(px(14.0))
        .text_color(theme::muted())
        .child(title.into())
}

/// The page heading and its muted subtitle.
pub fn page_header(
    title: impl Into<SharedString>,
    subtitle: impl Into<SharedString>,
) -> impl IntoElement {
    div()
        .flex()
        .flex_col()
        .mb(px(16.0))
        .child(
            div()
                .text_size(px(18.0))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(theme::text())
                .child(title.into()),
        )
        .child(
            div()
                .text_size(px(13.0))
                .text_color(theme::muted())
                .child(subtitle.into()),
        )
}

/// A failure banner.
pub fn error_banner(message: impl Into<SharedString>) -> impl IntoElement {
    div()
        .mx(px(theme::CONTENT_PADDING_X))
        .mt(px(theme::CONTENT_PADDING_Y))
        .mb(px(14.0))
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

/// A success banner.
pub fn notice_banner(message: impl Into<SharedString>) -> impl IntoElement {
    div()
        .mx(px(theme::CONTENT_PADDING_X))
        .mt(px(theme::CONTENT_PADDING_Y))
        .mb(px(14.0))
        .p(px(9.0))
        .px(px(12.0))
        .rounded(px(theme::RADIUS))
        .border_1()
        .border_color(theme::ok())
        .bg(theme::ok_faint())
        .text_size(px(13.0))
        .text_color(Hsla::from(rgba(0xc9f0d4ff)))
        .child(message.into())
}

/// The "nothing here" placeholder.
pub fn empty_state(message: impl Into<SharedString>) -> impl IntoElement {
    div()
        .flex()
        .items_center()
        .justify_center()
        .py(px(24.0))
        .text_size(px(13.0))
        .text_color(theme::muted())
        .child(message.into())
}

/// Monospaced text — ids, key prefixes, code.
pub fn mono(text: impl Into<SharedString>) -> impl IntoElement {
    div()
        .font(gpui::font(theme::MONO_FAMILY))
        .text_size(px(12.0))
        .text_color(theme::text())
        .child(text.into())
}

/// A muted paragraph.
pub fn muted(text: impl Into<SharedString>) -> impl IntoElement {
    div()
        .text_size(px(13.0))
        .text_color(theme::muted())
        .child(text.into())
}

/// A right-aligned number, tabular like the CSS `td.num`.
pub fn number(text: impl Into<SharedString>) -> impl IntoElement {
    div()
        .text_size(px(13.0))
        .text_color(theme::text())
        .text_align(gpui::TextAlign::Right)
        .child(text.into())
}

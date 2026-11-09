//! Charts.
//!
//! The usage view shows how much each provider and each model was used. A
//! horizontal bar chart drawn with plain divs is enough for that, and it keeps
//! the dashboard free of a plotting dependency.

use gpui::prelude::*;
use gpui::{IntoElement, div, px};

use crate::theme;

/// One bar of a chart.
#[derive(Debug, Clone)]
pub struct BarItem {
    /// The label under the bar.
    pub label: String,
    /// The value the bar length is proportional to.
    pub value: f64,
    /// An optional caption next to the value.
    pub caption: Option<String>,
}

impl BarItem {
    /// A bar with a value and a label.
    pub fn new(label: impl Into<String>, value: f64) -> Self {
        Self {
            label: label.into(),
            value,
            caption: None,
        }
    }

    /// Add a caption, e.g. the request count behind the bar.
    pub fn with_caption(mut self, caption: impl Into<String>) -> Self {
        self.caption = Some(caption.into());
        self
    }
}

/// A horizontal bar chart.
pub fn bar_chart(items: Vec<BarItem>) -> impl IntoElement {
    let max = items
        .iter()
        .map(|item| item.value)
        .fold(0.0_f64, |acc, value| acc.max(value));

    div()
        .flex()
        .flex_col()
        .gap_2()
        .children(items.into_iter().map(|item| {
            let fraction = if max > 0.0 { item.value / max } else { 0.0 };
            // `gpui::relative` takes a *fraction* of the parent (1.0 = 100%),
            // not a percentage.
            let width = fraction.clamp(0.0, 1.0) as f32;

            div()
                .flex()
                .flex_col()
                .gap_1()
                .child(
                    div()
                        .flex()
                        .justify_between()
                        .text_size(px(12.0))
                        .text_color(theme::text())
                        .child(item.label.clone())
                        .child(
                            item.caption
                                .clone()
                                .map(|caption| {
                                    div()
                                        .text_size(px(12.0))
                                        .text_color(theme::muted())
                                        .child(caption)
                                })
                                .unwrap_or_else(|| div()),
                        ),
                )
                .child(
                    div()
                        .h(px(8.0))
                        .w_full()
                        .rounded(px(4.0))
                        .bg(theme::panel_2())
                        .child(
                            div()
                                .h(px(8.0))
                                .w(gpui::relative(width))
                                .rounded(px(4.0))
                                .bg(theme::accent()),
                        ),
                )
        }))
}

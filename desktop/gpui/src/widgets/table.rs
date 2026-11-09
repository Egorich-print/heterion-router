//! Table primitives.
//!
//! gpui has no table widget: a table is a column of rows, each row a row of
//! cells. These helpers keep that shape in one place so every view draws
//! tables the same way.

use gpui::prelude::*;
use gpui::{FontWeight, IntoElement, div, px};

use crate::theme;

/// The header row of a table.
pub fn table_header(columns: &[&str]) -> impl IntoElement {
    div()
        .flex()
        .gap_3()
        .px(px(10.0))
        .py(px(7.0))
        .border_b_1()
        .border_color(theme::border())
        .child(
            div()
                .flex_1()
                .text_size(px(11.0))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(theme::muted())
                .child(columns.first().copied().unwrap_or_default().to_string()),
        )
        .child(
            div()
                .flex_1()
                .text_size(px(11.0))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(theme::muted())
                .child(columns.get(1).copied().unwrap_or_default().to_string()),
        )
        .child(
            div()
                .flex_1()
                .text_size(px(11.0))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(theme::muted())
                .child(columns.get(2).copied().unwrap_or_default().to_string()),
        )
}

/// One row of a table. The cells are laid out in equal columns.
pub fn table_row(cells: Vec<gpui::AnyElement>) -> impl IntoElement {
    let mut cells = cells;
    while cells.len() < 3 {
        cells.push(div().into_any_element());
    }
    div()
        .flex()
        .gap_3()
        .px(px(10.0))
        .py(px(7.0))
        .border_b_1()
        .border_color(theme::border())
        .child(cells.remove(0))
        .child(cells.remove(0))
        .child(cells.remove(0))
}

/// The container of a table: a bordered panel around the rows.
pub fn table<I, T>(children: I) -> impl IntoElement
where
    I: IntoIterator<Item = T>,
    T: IntoElement,
{
    div()
        .flex()
        .flex_col()
        .bg(theme::panel())
        .border_1()
        .border_color(theme::border())
        .rounded(px(theme::RADIUS))
        .overflow_hidden()
        .children(children)
}

/// A row of a table with an arbitrary number of cells, each in an equal
/// column — for views that need more or fewer than three columns.
pub fn cells_row(cells: Vec<gpui::AnyElement>) -> impl IntoElement {
    div()
        .flex()
        .gap_3()
        .px(px(10.0))
        .py(px(7.0))
        .border_b_1()
        .border_color(theme::border())
        .children(cells)
}

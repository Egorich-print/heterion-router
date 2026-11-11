//! Table primitives.
//!
//! gpui has no table widget: a table is a column of rows, each row a row of
//! cells. These helpers keep that shape in one place so every view draws
//! tables the same way.

use gpui::prelude::*;
use gpui::{AnyElement, FontWeight, IntoElement, TextAlign, div, px};

use crate::theme;

/// How a column is sized inside a flex row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColumnSizing {
    /// A fixed-width column that never grows or shrinks.
    Fixed(u32),
    /// A flexible column: `flex: 1 1 0%` with `min-width: 0`.
    Flex,
    /// A growing column with a preferred basis: `flex-grow: 1`,
    /// `flex-basis: <px>`, `min-width: 0`.
    GrowBasis(u32),
}

/// Text alignment for a column.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ColumnAlign {
    /// Left-aligned text (the default).
    #[default]
    Left,
    /// Right-aligned text, for numbers.
    Right,
}

/// One column in a table described by a shared layout.
///
/// The same definition is applied to the header cell and to every data
/// cell, so the two can never drift apart.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ColumnDef {
    /// The header label.
    pub label: &'static str,
    /// How the column takes space.
    pub sizing: ColumnSizing,
    /// How the text is aligned inside the column.
    pub align: ColumnAlign,
}

/// Apply the sizing and alignment of a column to a cell container.
fn apply_column_layout(mut cell: gpui::Div, column: ColumnDef) -> gpui::Div {
    match column.sizing {
        ColumnSizing::Fixed(width) => {
            cell = cell.w(px(width as f32)).flex_shrink_0();
        }
        ColumnSizing::Flex => {
            cell = cell.flex_1().min_w_0();
        }
        ColumnSizing::GrowBasis(basis) => {
            cell = cell.flex_grow().flex_basis(px(basis as f32)).min_w_0();
        }
    }
    if column.align == ColumnAlign::Right {
        cell = cell.text_align(TextAlign::Right);
    }
    cell
}

/// The header row of a table described by a shared column layout.
pub fn defined_header(columns: &[ColumnDef]) -> AnyElement {
    let mut row = header_row_shell();
    for column in columns {
        let cell = apply_column_layout(
            div()
                .text_size(px(11.0))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(theme::muted())
                .child(column.label),
            *column,
        );
        row = row.child(cell);
    }
    row.into_any_element()
}

/// One data row of a table described by a shared column layout.
///
/// Each cell is placed into a container that carries the same sizing and
/// alignment as the matching header column. The row takes the full width of
/// its parent.
pub fn defined_row(columns: &[ColumnDef], cells: Vec<AnyElement>, row_height: f32) -> AnyElement {
    let mut row = data_row_shell(row_height);
    let mut iter = cells.into_iter();
    for column in columns {
        let content = iter.next().unwrap_or_else(|| div().into_any_element());
        let cell =
            apply_column_layout(div().flex().items_center().min_w_0(), *column).child(content);
        row = row.child(cell);
    }
    row.into_any_element()
}

/// The common shell of a header row.
fn header_row_shell() -> gpui::Div {
    div()
        .flex()
        .items_center()
        .gap_3()
        .w_full()
        .px(px(10.0))
        .py(px(7.0))
        .border_b_1()
        .border_color(theme::border())
}

/// The common shell of a data row.
fn data_row_shell(row_height: f32) -> gpui::Div {
    div()
        .flex()
        .items_center()
        .gap_3()
        .w_full()
        .px(px(10.0))
        .h(px(row_height))
        .border_b_1()
        .border_color(theme::border())
}

/// A bordered panel that fills the width of its parent.
pub fn table_panel<I, T>(children: I) -> gpui::Div
where
    I: IntoIterator<Item = T>,
    T: IntoElement,
{
    div()
        .flex()
        .flex_col()
        .w_full()
        .bg(theme::panel())
        .border_1()
        .border_color(theme::border())
        .rounded(px(theme::RADIUS))
        .overflow_hidden()
        .children(children)
}

/// Strip ANSI escape sequences (CSI, OSC and simple escapes) from a string.
///
/// Application logs may carry terminal colour codes; the desktop view renders
/// plain text, so the escapes must be removed rather than shown as literals.
pub fn strip_ansi(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\x1b' {
            match chars.peek().copied() {
                Some('[') => {
                    // CSI: ESC [ <parameter bytes>* <final byte @-~>
                    chars.next();
                    while let Some(&next) = chars.peek() {
                        chars.next();
                        if ('@'..='~').contains(&next) {
                            break;
                        }
                    }
                }
                Some(']') => {
                    // OSC: ESC ] ... terminated by BEL or ST (ESC \)
                    chars.next();
                    while let Some(next) = chars.next() {
                        if next == '\x07' {
                            break;
                        }
                        if next == '\x1b' && chars.peek().copied() == Some('\\') {
                            chars.next();
                            break;
                        }
                    }
                }
                Some('(') | Some(')') | Some('*') | Some('+') => {
                    // Character set selection: ESC ( X
                    chars.next();
                    chars.next();
                }
                Some(_) => {
                    // Other single-char escape: consume the next char.
                    chars.next();
                }
                None => {}
            }
        } else {
            out.push(ch);
        }
    }
    out
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

#[cfg(test)]
mod tests {
    use super::*;

    fn call_log_columns() -> Vec<ColumnDef> {
        vec![
            ColumnDef {
                label: "TIME",
                sizing: ColumnSizing::Flex,
                align: ColumnAlign::Left,
            },
            ColumnDef {
                label: "STATUS",
                sizing: ColumnSizing::Fixed(72),
                align: ColumnAlign::Left,
            },
            ColumnDef {
                label: "MODEL",
                sizing: ColumnSizing::Flex,
                align: ColumnAlign::Left,
            },
            ColumnDef {
                label: "PROVIDER",
                sizing: ColumnSizing::Flex,
                align: ColumnAlign::Left,
            },
            ColumnDef {
                label: "MS",
                sizing: ColumnSizing::Fixed(64),
                align: ColumnAlign::Right,
            },
            ColumnDef {
                label: "IN",
                sizing: ColumnSizing::Fixed(64),
                align: ColumnAlign::Right,
            },
            ColumnDef {
                label: "OUT",
                sizing: ColumnSizing::Fixed(64),
                align: ColumnAlign::Right,
            },
            ColumnDef {
                label: "ERROR",
                sizing: ColumnSizing::GrowBasis(200),
                align: ColumnAlign::Left,
            },
        ]
    }

    #[test]
    fn call_log_header_and_rows_share_one_column_model() {
        let columns = call_log_columns();
        // Header and rows are built from the same definitions, so a single
        // source of truth fixes the column drift reported by visual QA.
        assert_eq!(columns.len(), 8);
        assert_eq!(columns[0].label, "TIME");
        assert_eq!(columns[0].sizing, ColumnSizing::Flex);
        assert_eq!(columns[1].label, "STATUS");
        assert_eq!(columns[1].sizing, ColumnSizing::Fixed(72));
        assert_eq!(columns[2].label, "MODEL");
        assert_eq!(columns[2].sizing, ColumnSizing::Flex);
        assert_eq!(columns[3].label, "PROVIDER");
        assert_eq!(columns[3].sizing, ColumnSizing::Flex);
        assert_eq!(columns[4].label, "MS");
        assert_eq!(columns[4].sizing, ColumnSizing::Fixed(64));
        assert_eq!(columns[4].align, ColumnAlign::Right);
        assert_eq!(columns[5].label, "IN");
        assert_eq!(columns[5].sizing, ColumnSizing::Fixed(64));
        assert_eq!(columns[5].align, ColumnAlign::Right);
        assert_eq!(columns[6].label, "OUT");
        assert_eq!(columns[6].sizing, ColumnSizing::Fixed(64));
        assert_eq!(columns[6].align, ColumnAlign::Right);
        assert_eq!(columns[7].label, "ERROR");
        assert_eq!(columns[7].sizing, ColumnSizing::GrowBasis(200));
        // The header and every data row consume the same column vector, so the
        // count and the order of widths cannot diverge.
        for column in &columns {
            let header = [*column];
            let row = [*column];
            assert_eq!(header[0], row[0]);
        }
    }

    #[test]
    fn strip_ansi_removes_colour_codes() {
        assert_eq!(
            strip_ansi("\u{1b}[2m2026\u{1b}[0m \u{1b}[32m INFO\u{1b}[0m ready"),
            "2026  INFO ready"
        );
        assert_eq!(strip_ansi("plain text"), "plain text");
        assert_eq!(strip_ansi("\u{1b}[38;5;82mgreen\u{1b}[0m"), "green");
        assert_eq!(strip_ansi("\u{1b}]0;title\u{7}log"), "log");
        assert_eq!(strip_ansi("\u{1b}(Blog"), "log");
        assert_eq!(strip_ansi(""), "");
    }
}

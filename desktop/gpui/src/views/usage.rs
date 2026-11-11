//! Usage — token usage, latency percentiles and the top providers and models.
//!
//! Ported from `desktop/ui/src/views/Usage.svelte`: the same window selector,
//! the same totals, the same two bucket lists. The latency percentiles are an
//! addition — the gateway has been sending them for a while
//! (`crates/heterion-router-gateway/src/admin.rs`), the web view simply never
//! showed them.

use gpui::prelude::*;
use gpui::{
    AnyElement, ClickEvent, Context, CursorStyle, FontWeight, IntoElement, Render, TextAlign,
    Window, div, px,
};

use crate::api::{self, ApiError, Usage, UsageBucket};
use crate::theme;
use crate::widgets::{self, BarItem};

/// How many rows the gateway returns per bucket list — the Svelte view asks
/// for the same number.
const LIMIT: u32 = 10;

/// The reporting windows the dashboard offers, in days.
const WINDOWS: [(u32, &str); 4] = [
    (1, "24 hours"),
    (7, "7 days"),
    (30, "30 days"),
    (90, "90 days"),
];

/// The number of columns in each metric-card row so every card keeps the same
/// width regardless of how many cards the row shows.
const STAT_COLUMNS: usize = 5;

/// The usage section.
pub struct UsageView {
    /// The loaded payload.
    pub data: Option<Usage>,
    /// The last failure, rendered as a banner.
    pub error: Option<String>,
    /// Whether a request is in flight.
    pub loading: bool,
    /// The selected reporting window, in days. Changing it re-queries the
    /// gateway.
    pub days: u32,
    /// The number of the request that is in flight.
    ///
    /// Every refresh bumps it. A reply that belongs to an older request is
    /// dropped: two clicks in a row must not show the older window's numbers
    /// under the newer window's label.
    request: u64,
}

/// The gateway's answer to `GET /api/usage`.
///
/// The typed [`Usage`] carries the numbers, but the gateway can also answer
/// HTTP 200 with an `{"error": …}` body (a failed query) or with
/// `{"usage": null}` (no database at all). The Svelte view reads that field,
/// so the payload is read through this wrapper.
#[derive(Debug, Clone, serde::Deserialize)]
struct UsagePayload {
    /// The numbers themselves.
    #[serde(flatten)]
    usage: Usage,
    /// The gateway's own error message, when it sent one.
    #[serde(default)]
    error: Option<String>,
}

impl UsageView {
    /// Build the view and load its data.
    pub fn new(cx: &mut Context<Self>) -> Self {
        let mut view = Self {
            data: None,
            error: None,
            loading: true,
            days: 7,
            request: 0,
        };
        view.refresh(cx);
        view
    }

    /// Reload the data from the gateway for the selected window.
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        let Some(client) = api::session_client(cx) else {
            return;
        };
        self.loading = true;
        self.request += 1;
        let request = self.request;
        let path = format!("/api/usage?days={}&limit={LIMIT}", self.days);
        api::spawn_request(
            cx,
            move || client.get_json::<UsagePayload>(&path),
            move |result, view: &mut UsageView, cx| {
                // A reply for a window that has been superseded must not
                // overwrite the newer one: the numbers would belong to
                // another period than the one on screen.
                if view.request != request {
                    return;
                }
                match result {
                    Ok(payload) => {
                        view.data = Some(payload.usage);
                        view.error = payload.error.filter(|message| !message.trim().is_empty());
                        if let Some(message) = view.error.clone() {
                            // The gateway answered 200 with an error body: a
                            // failed query, not a lost session.
                            crate::app::report_error(
                                cx,
                                &ApiError {
                                    status: 200,
                                    message,
                                },
                            );
                        }
                    }
                    Err(error) => {
                        view.error = Some(error.to_string());
                        crate::app::report_error(cx, &error);
                    }
                }
                view.loading = false;
                cx.notify();
            },
        );
    }

    /// The window selector — the same windows the Svelte view offers.
    ///
    /// Picking one re-queries the gateway for that window.
    fn render_window_selector(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let mut pills: Vec<AnyElement> = Vec::new();
        for (days, label) in WINDOWS {
            let active = days == self.days;
            pills.push(
                div()
                    .id(theme::element_id(format!("usage-window:{days}")))
                    .flex()
                    .items_center()
                    .px(px(10.0))
                    .py(px(5.0))
                    .rounded(px(theme::RADIUS_SM))
                    .border_1()
                    .border_color(if active {
                        theme::accent()
                    } else {
                        theme::border()
                    })
                    .bg(if active {
                        theme::accent_faint()
                    } else {
                        theme::panel()
                    })
                    .text_size(px(12.0))
                    .text_color(if active {
                        theme::text()
                    } else {
                        theme::muted()
                    })
                    .cursor(CursorStyle::PointingHand)
                    .on_click(cx.listener(
                        move |view: &mut UsageView,
                              _event: &ClickEvent,
                              _window: &mut Window,
                              cx: &mut Context<UsageView>| {
                            if view.days != days {
                                view.days = days;
                                view.refresh(cx);
                                cx.notify();
                            }
                        },
                    ))
                    .child(label)
                    .into_any_element(),
            );
        }

        div()
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .text_size(px(12.0))
                    .text_color(theme::muted())
                    .child("window"),
            )
            .child(div().flex().gap_2().children(pills))
            .into_any_element()
    }

    /// The totals: requests, success rate, tokens in, tokens out, cache reads.
    fn render_totals(&self, usage: &Usage) -> AnyElement {
        let success = match success_rate(usage) {
            Some(rate) => format!("{rate}%"),
            None => "—".to_string(),
        };

        let cards = vec![
            widgets::stat_card("requests", format_number(usage.requests)),
            widgets::stat_card("success", success),
            widgets::stat_card("tokens in", format_number(usage.tokens_input)),
            widgets::stat_card("tokens out", format_number(usage.tokens_output)),
            widgets::stat_card("cache read", format_number(usage.tokens_cache_read)),
        ];

        card_grid(cards)
    }

    /// The latency percentiles of the window.
    fn render_latency(&self, usage: &Usage) -> Vec<AnyElement> {
        let cards = vec![
            widgets::stat_card("p50", percentile(usage.latency.p50_ms)),
            widgets::stat_card("p95", percentile(usage.latency.p95_ms)),
            widgets::stat_card("p99", percentile(usage.latency.p99_ms)),
        ];

        vec![
            widgets::section_title("Latency").into_any_element(),
            card_grid(cards),
        ]
    }

    /// One bucket list: a bar chart of the requests and the numbers behind it.
    fn render_buckets(
        &self,
        title: &'static str,
        key_title: &'static str,
        buckets: &[UsageBucket],
    ) -> Vec<AnyElement> {
        let mut sections = vec![widgets::section_title(title).into_any_element()];

        if buckets.is_empty() {
            sections
                .push(widgets::empty_state("no usage recorded in this window").into_any_element());
            return sections;
        }

        // One bar per bucket, as long as its share of the requests.
        let items: Vec<BarItem> = buckets
            .iter()
            .map(|bucket| {
                BarItem::new(bucket.key.clone(), bucket.requests as f64)
                    .with_caption(format!("{} req", format_number(bucket.requests)))
            })
            .collect();

        sections.push(
            div()
                .id(theme::element_id(format!("usage-chart:{title}")))
                .p(px(12.0))
                .bg(theme::panel())
                .border_1()
                .border_color(theme::border())
                .rounded(px(theme::RADIUS))
                .max_h(px(360.0))
                .overflow_y_scroll()
                .child(widgets::bar_chart(items))
                .into_any_element(),
        );

        // The same numbers as text, the way the Svelte table shows them.
        let mut rows = vec![bucket_header([
            key_title,
            "requests",
            "tokens in",
            "tokens out",
        ])];
        for bucket in buckets {
            rows.push(bucket_row([
                widgets::mono(bucket.key.clone()).into_any_element(),
                widgets::number(format_number(bucket.requests)).into_any_element(),
                widgets::number(format_number(bucket.tokens_input)).into_any_element(),
                widgets::number(format_number(bucket.tokens_output)).into_any_element(),
            ]));
        }
        sections.push(widgets::table(rows).into_any_element());

        sections
    }
}

impl Render for UsageView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let mut content = div().flex().flex_col().w_full().gap(px(16.0));

        content = content.child(
            widgets::page_header(
                "Usage",
                "Aggregated from usage_history, the table both the JS and Rust gateways write.",
            )
            .into_any_element(),
        );

        if let Some(message) = self.error.clone() {
            // `error_banner` carries the margin of the shell's content column;
            // this view pads itself, so cancel that margin again.
            content = content.child(
                div()
                    .mx(px(-theme::CONTENT_PADDING_X))
                    .child(widgets::error_banner(message))
                    .into_any_element(),
            );
        }

        content = content.child(self.render_window_selector(cx));

        match self.data.as_ref() {
            Some(usage) => {
                content = content.child(self.render_totals(usage));
                for section in self.render_latency(usage) {
                    content = content.child(section);
                }
                for section in self.render_buckets("Top providers", "provider", &usage.by_provider)
                {
                    content = content.child(section);
                }
                for section in self.render_buckets("Top models", "model", &usage.by_model) {
                    content = content.child(section);
                }
            }
            None => {
                if self.loading {
                    content = content.child(widgets::muted("loading…").into_any_element());
                }
            }
        }

        // The scrollable page: the content keeps its own height so everything
        // below the fold remains reachable.
        let page = div()
            .flex()
            .flex_col()
            .flex_1()
            .w_full()
            .id(theme::element_id("usage-page"))
            .overflow_y_scroll()
            .items_start()
            .child(content);

        div()
            .flex()
            .flex_col()
            .flex_1()
            .w_full()
            .p(px(theme::CONTENT_PADDING_X))
            .child(page)
    }
}

/// A grid of metric cards with a fixed number of columns so every row uses the
/// same card width, regardless of how many cards it contains.
fn card_grid(cards: Vec<impl IntoElement + 'static>) -> AnyElement {
    let mut cells: Vec<AnyElement> = Vec::with_capacity(STAT_COLUMNS);
    for card in cards.into_iter().take(STAT_COLUMNS) {
        cells.push(
            div()
                .flex_1()
                .min_w(px(0.0))
                .child(card.into_any_element())
                .into_any_element(),
        );
    }
    while cells.len() < STAT_COLUMNS {
        cells.push(div().flex_1().min_w(px(0.0)).into_any_element());
    }

    div()
        .flex()
        .gap_3()
        .w_full()
        .children(cells)
        .into_any_element()
}

/// Format an integer with a space as the thousands separator.
pub fn format_number(value: i64) -> String {
    let negative = value < 0;
    let digits = value.unsigned_abs().to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3 + 1);
    for (index, ch) in digits.chars().rev().enumerate() {
        if index > 0 && index % 3 == 0 {
            out.push(' ');
        }
        out.push(ch);
    }
    let mut grouped: String = out.chars().rev().collect();
    if negative {
        grouped.insert(0, '-');
    }
    grouped
}

/// The share of successful requests, or `None` when nothing was requested.
fn success_rate(usage: &Usage) -> Option<i64> {
    if usage.requests <= 0 {
        return None;
    }
    Some((usage.successes as f64 / usage.requests as f64 * 100.0).round() as i64)
}

/// A latency percentile in milliseconds, or a dash when the gateway has none.
fn percentile(value: Option<f64>) -> String {
    match value {
        Some(ms) if ms.is_finite() => format!("{} ms", format_number(ms.round() as i64)),
        _ => "—".to_string(),
    }
}

/// The header of a bucket table.
///
/// The bucket lists have four columns, so the header is built here, in the
/// same style as the shared table header.
fn bucket_header(columns: [&'static str; 4]) -> AnyElement {
    div()
        .flex()
        .gap_3()
        .px(px(10.0))
        .py(px(7.0))
        .border_b_1()
        .border_color(theme::border())
        .children(columns.into_iter().enumerate().map(|(index, column)| {
            div()
                .flex_1()
                .text_size(px(11.0))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(theme::muted())
                .text_align(if index == 0 {
                    TextAlign::Left
                } else {
                    TextAlign::Right
                })
                .child(column)
        }))
        .into_any_element()
}

/// One row of a bucket table: four cells in equal columns.
fn bucket_row(cells: [AnyElement; 4]) -> AnyElement {
    div()
        .flex()
        .gap_3()
        .px(px(10.0))
        .py(px(7.0))
        .border_b_1()
        .border_color(theme::border())
        .children(cells.into_iter().map(|cell| div().flex_1().child(cell)))
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_use_thousands_separators() {
        assert_eq!(format_number(0), "0");
        assert_eq!(format_number(999), "999");
        assert_eq!(format_number(1_000), "1 000");
        assert_eq!(format_number(2_854), "2 854");
        assert_eq!(format_number(24_929), "24 929");
        assert_eq!(format_number(61_081), "61 081");
        assert_eq!(format_number(1_234_567), "1 234 567");
        assert_eq!(format_number(-2_854), "-2 854");
    }

    #[test]
    fn a_missing_percentile_is_a_dash() {
        assert_eq!(percentile(None), "—");
        assert_eq!(percentile(Some(1234.0)), "1 234 ms");
        assert_eq!(percentile(Some(12.4)), "12 ms");
        assert_eq!(percentile(Some(24_929.0)), "24 929 ms");
    }

    #[test]
    fn the_success_rate_needs_requests() {
        let mut usage = Usage::default();
        assert_eq!(success_rate(&usage), None);
        usage.requests = 4;
        usage.successes = 3;
        assert_eq!(success_rate(&usage), Some(75));
    }

    #[test]
    fn the_gateway_payload_is_read() {
        let payload: UsagePayload = serde_json::from_str(
            r#"{
                "window_days": 7,
                "requests": 10,
                "successes": 9,
                "tokens_input": 1200,
                "tokens_output": 3400,
                "tokens_cache_read": 500,
                "latency": { "p50_ms": 120.0, "p95_ms": null },
                "by_provider": [
                    { "key": "openai", "requests": 6, "tokens_input": 10, "tokens_output": 20 }
                ],
                "by_model": []
            }"#,
        )
        .expect("the payload parses");

        assert_eq!(payload.usage.requests, 10);
        assert_eq!(payload.usage.tokens_cache_read, 500);
        assert_eq!(payload.usage.latency.p50_ms, Some(120.0));
        assert_eq!(payload.usage.latency.p95_ms, None);
        assert_eq!(payload.usage.by_provider.len(), 1);
        assert_eq!(payload.usage.by_provider[0].key, "openai");
        assert!(payload.usage.by_model.is_empty());
        assert!(payload.error.is_none());
    }

    #[test]
    fn a_gateway_error_body_is_kept() {
        let payload: UsagePayload =
            serde_json::from_str(r#"{"error": "no such table: usage_history"}"#)
                .expect("the payload parses");
        assert_eq!(
            payload.error.as_deref(),
            Some("no such table: usage_history")
        );
        assert_eq!(payload.usage.requests, 0);
    }

    #[test]
    fn a_gateway_without_a_database_is_empty() {
        let payload: UsagePayload =
            serde_json::from_str(r#"{"usage": null}"#).expect("the payload parses");
        assert!(payload.error.is_none());
        assert_eq!(payload.usage.requests, 0);
        assert!(payload.usage.by_provider.is_empty());
    }
}

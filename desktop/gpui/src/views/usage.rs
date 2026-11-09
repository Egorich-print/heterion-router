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
        let path = format!("/api/usage?days={}&limit={LIMIT}", self.days);
        api::spawn_request(
            cx,
            move || client.get_json::<UsagePayload>(&path),
            |result, view: &mut UsageView, cx| {
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
                    .id(theme::element_id("usage-window"))
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

        let cards = [
            widgets::stat_card("requests", compact(usage.requests)),
            widgets::stat_card("success", success),
            widgets::stat_card("tokens in", compact(usage.tokens_input)),
            widgets::stat_card("tokens out", compact(usage.tokens_output)),
            widgets::stat_card("cache read", compact(usage.tokens_cache_read)),
        ];

        div()
            .flex()
            .flex_wrap()
            .gap_3()
            .children(
                cards
                    .into_iter()
                    .map(|card| div().flex_1().min_w(px(140.0)).child(card)),
            )
            .into_any_element()
    }

    /// The latency percentiles of the window.
    fn render_latency(&self, usage: &Usage) -> Vec<AnyElement> {
        let cards = [
            widgets::stat_card("p50", percentile(usage.latency.p50_ms)),
            widgets::stat_card("p95", percentile(usage.latency.p95_ms)),
            widgets::stat_card("p99", percentile(usage.latency.p99_ms)),
        ];

        vec![
            widgets::section_title("Latency").into_any_element(),
            div()
                .flex()
                .flex_wrap()
                .gap_3()
                .children(
                    cards
                        .into_iter()
                        .map(|card| div().flex_1().min_w(px(140.0)).child(card)),
                )
                .into_any_element(),
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
                    .with_caption(format!("{} req", compact(bucket.requests)))
            })
            .collect();

        sections.push(
            div()
                .p(px(12.0))
                .bg(theme::panel())
                .border_1()
                .border_color(theme::border())
                .rounded(px(theme::RADIUS))
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
                widgets::number(compact(bucket.requests)).into_any_element(),
                widgets::number(compact(bucket.tokens_input)).into_any_element(),
                widgets::number(compact(bucket.tokens_output)).into_any_element(),
            ]));
        }
        sections.push(widgets::table(rows).into_any_element());

        sections
    }
}

impl Render for UsageView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let mut sections: Vec<AnyElement> = Vec::new();

        sections.push(
            widgets::page_header(
                "Usage",
                "Aggregated from usage_history, the table both the JS and Rust gateways write.",
            )
            .into_any_element(),
        );

        if let Some(message) = self.error.clone() {
            // `error_banner` carries the margin of the shell's content column;
            // this view pads itself, so cancel that margin again.
            sections.push(
                div()
                    .mx(px(-theme::CONTENT_PADDING_X))
                    .child(widgets::error_banner(message))
                    .into_any_element(),
            );
        }

        sections.push(self.render_window_selector(cx));

        match self.data.as_ref() {
            Some(usage) => {
                sections.push(self.render_totals(usage));
                sections.extend(self.render_latency(usage));
                sections.extend(self.render_buckets(
                    "Top providers",
                    "provider",
                    &usage.by_provider,
                ));
                sections.extend(self.render_buckets("Top models", "model", &usage.by_model));
            }
            None => {
                if self.loading {
                    sections.push(widgets::muted("loading…").into_any_element());
                }
            }
        }

        div()
            .flex()
            .flex_col()
            .p(px(26.0))
            .gap(px(16.0))
            .children(sections)
    }
}

/// A short number, the way the Svelte view prints it: `1.2k`, `3.4M`.
fn compact(value: i64) -> String {
    let value = value as f64;
    if value >= 1_000_000.0 {
        format!("{:.1}M", value / 1_000_000.0)
    } else if value >= 1_000.0 {
        format!("{:.1}k", value / 1_000.0)
    } else {
        format!("{}", value as i64)
    }
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
        Some(ms) if ms.is_finite() => format!("{ms:.0} ms"),
        _ => "—".to_string(),
    }
}

/// The header of a bucket table.
///
/// The shared `table_header` draws three columns; the bucket lists have four,
/// so the header is built here, in the same style.
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
    fn numbers_are_shortened_like_the_web_view() {
        assert_eq!(compact(0), "0");
        assert_eq!(compact(999), "999");
        assert_eq!(compact(1_500), "1.5k");
        assert_eq!(compact(1_000), "1.0k");
        assert_eq!(compact(2_400_000), "2.4M");
    }

    #[test]
    fn a_missing_percentile_is_a_dash() {
        assert_eq!(percentile(None), "—");
        assert_eq!(percentile(Some(1234.0)), "1234 ms");
        assert_eq!(percentile(Some(12.4)), "12 ms");
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

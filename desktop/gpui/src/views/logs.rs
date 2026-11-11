//! Logs — the API call log and the router's application log.
//!
//! Ported from `desktop/ui/src/views/Logs.svelte`: two sources, a row limit
//! and a refresh action. The call log uses a shared column model so the
//! header and every data row stay aligned; both logs render inside bounded,
//! scrollable panels.

use gpui::prelude::*;
use gpui::{
    AnyElement, App, Context, CursorStyle, FontWeight, IntoElement, Render, SharedString, Window,
    div, px,
};

use crate::api::{self, AppLogsResponse, CallLog, LogsResponse};
use crate::theme;
use crate::widgets::{self, ColumnAlign, ColumnDef, ColumnSizing, Tone};

/// Which of the two logs the page shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogSource {
    /// The API call log — one row per call.
    Calls,
    /// The router's own log — one line per entry.
    Application,
}

/// The row counts the limit selector offers.
const LIMITS: [usize; 4] = [25, 50, 100, 250];

/// The row count the view starts with.
const DEFAULT_LIMIT: usize = 50;

/// The height of one row of the call log.
const ROW_HEIGHT: f32 = 30.0;

/// The width of the status column.
const STATUS_WIDTH: u32 = 72;

/// The width of the numeric columns.
const NUMBER_WIDTH: u32 = 64;

/// The shared column model of the call log table.
///
/// Header and data rows are both built from this definition, so their widths
/// and alignment cannot drift apart.
fn call_log_columns() -> [ColumnDef; 8] {
    [
        ColumnDef {
            label: "TIME",
            sizing: ColumnSizing::Flex,
            align: ColumnAlign::Left,
        },
        ColumnDef {
            label: "STATUS",
            sizing: ColumnSizing::Fixed(STATUS_WIDTH),
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
            sizing: ColumnSizing::Fixed(NUMBER_WIDTH),
            align: ColumnAlign::Right,
        },
        ColumnDef {
            label: "IN",
            sizing: ColumnSizing::Fixed(NUMBER_WIDTH),
            align: ColumnAlign::Right,
        },
        ColumnDef {
            label: "OUT",
            sizing: ColumnSizing::Fixed(NUMBER_WIDTH),
            align: ColumnAlign::Right,
        },
        ColumnDef {
            label: "ERROR",
            sizing: ColumnSizing::GrowBasis(200),
            align: ColumnAlign::Left,
        },
    ]
}

/// The Logs section.
pub struct LogsView {
    /// The loaded payload of the call log.
    pub data: Option<LogsResponse>,
    /// The last failure, rendered as a banner.
    pub error: Option<String>,
    /// Whether a request is in flight.
    pub loading: bool,
    /// How many rows to fetch.
    pub limit: usize,
    /// Which of the two logs is on screen.
    pub source: LogSource,
    /// The application log lines.
    pub app_lines: Vec<String>,
    /// Whether the application log request is in flight.
    pub app_loading: bool,
    /// The last failure of the application log.
    pub app_error: Option<String>,
}

impl LogsView {
    /// Build the view and load the call log.
    pub fn new(cx: &mut Context<Self>) -> Self {
        let mut view = Self {
            data: None,
            error: None,
            loading: true,
            limit: DEFAULT_LIMIT,
            source: LogSource::Calls,
            app_lines: Vec::new(),
            app_loading: false,
            app_error: None,
        };
        view.refresh(cx);
        view
    }

    /// Reload the call log from the gateway.
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        let Some(client) = api::session_client(cx) else {
            self.loading = false;
            return;
        };
        self.loading = true;
        let limit = self.limit;
        api::spawn_request(
            cx,
            move || client.get_json::<LogsResponse>(&format!("/api/logs?limit={limit}")),
            |result, view: &mut LogsView, cx| {
                match result {
                    Ok(payload) => {
                        // The gateway reports a failed read in the payload itself.
                        view.error = payload.error.clone();
                        view.data = Some(payload);
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

    /// Reload the application log from the gateway.
    pub fn refresh_app(&mut self, cx: &mut Context<Self>) {
        let Some(client) = api::session_client(cx) else {
            self.app_loading = false;
            return;
        };
        self.app_loading = true;
        let limit = self.limit;
        api::spawn_request(
            cx,
            move || client.get_json::<AppLogsResponse>(&format!("/api/logs/app?limit={limit}")),
            |result, view: &mut LogsView, cx| {
                match result {
                    Ok(payload) => {
                        view.app_lines = payload.lines;
                        view.app_error = None;
                    }
                    Err(error) => {
                        view.app_lines.clear();
                        view.app_error = Some(error.to_string());
                        crate::app::report_error(cx, &error);
                    }
                }
                view.app_loading = false;
                cx.notify();
            },
        );
    }

    /// Reload whichever log is on screen.
    fn reload(&mut self, cx: &mut Context<Self>) {
        match self.source {
            LogSource::Calls => self.refresh(cx),
            LogSource::Application => self.refresh_app(cx),
        }
    }

    /// Show one of the two logs. The application log is fetched on the way in,
    /// the way the web view does it.
    fn select_source(&mut self, source: LogSource, cx: &mut Context<Self>) {
        self.source = source;
        if source == LogSource::Application {
            self.refresh_app(cx);
        }
        cx.notify();
    }

    /// Change the row limit and reload the log on screen.
    fn set_limit(&mut self, limit: usize, cx: &mut Context<Self>) {
        if self.limit == limit {
            return;
        }
        self.limit = limit;
        self.reload(cx);
        cx.notify();
    }

    /// The toolbar: the source tabs, the row limit and the refresh action.
    fn render_toolbar(&self, cx: &mut Context<Self>) -> AnyElement {
        let this = cx.entity();

        let mut tabs = Vec::new();
        for (source, label) in [
            (LogSource::Calls, "API calls"),
            (LogSource::Application, "Application"),
        ] {
            let this = this.clone();
            let active = self.source == source;
            tabs.push(
                tab_button(label, active, move |_window, cx| {
                    this.update(cx, |view, cx| view.select_source(source, cx));
                })
                .into_any_element(),
            );
        }

        let mut limits = Vec::new();
        for limit in LIMITS {
            let this = this.clone();
            let active = self.limit == limit;
            limits.push(
                limit_button(limit, active, move |_window, cx| {
                    this.update(cx, |view, cx| view.set_limit(limit, cx));
                })
                .into_any_element(),
            );
        }

        let this = cx.entity();
        let refresh = widgets::button("logs-refresh", "Refresh", move |_window, cx| {
            this.update(cx, |view, cx| view.reload(cx));
        });

        div()
            .flex()
            .items_center()
            .gap_2()
            .flex_wrap()
            .child(div().flex().items_center().gap_1().children(tabs))
            .when(self.source == LogSource::Calls, |this| {
                this.child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(widgets::muted("rows"))
                        .child(div().flex().items_center().gap_1().children(limits)),
                )
            })
            .child(div().flex_1())
            .child(refresh)
            .into_any_element()
    }

    /// The failure banner of the log on screen.
    fn render_banner(&self) -> AnyElement {
        let message = match self.source {
            LogSource::Calls => self.error.clone(),
            LogSource::Application => self.app_error.clone(),
        };
        match message {
            Some(message) => widgets::error_banner(message).into_any_element(),
            None => div().into_any_element(),
        }
    }

    /// The call log: a table of the most recent calls.
    fn render_calls(&self, _cx: &mut Context<Self>) -> AnyElement {
        let Some(payload) = self.data.as_ref() else {
            // Nothing loaded yet: either the request is in flight or it
            // failed, and the banner says which.
            if self.error.is_some() {
                return div().into_any_element();
            }
            if self.loading {
                return div().child(widgets::muted("loading…")).into_any_element();
            }
            return div()
                .child(widgets::empty_state("No calls recorded yet."))
                .into_any_element();
        };

        let columns = call_log_columns();
        let panel = widgets::table_panel(std::iter::empty::<AnyElement>())
            .flex_1()
            .min_h_0()
            .child(widgets::defined_header(&columns));

        if payload.logs.is_empty() {
            // The table keeps its header, the note sits below it.
            div()
                .flex()
                .flex_col()
                .flex_1()
                .min_h_0()
                .w_full()
                .child(panel)
                .child(widgets::empty_state("No calls recorded yet."))
                .into_any_element()
        } else {
            let rows = div()
                .id(theme::element_id("logs-calls-scroll"))
                .flex()
                .flex_col()
                .flex_1()
                .min_h_0()
                .w_full()
                .overflow_y_scroll()
                .children(payload.logs.iter().map(|row| call_row(&columns, row)));

            div()
                .flex()
                .flex_col()
                .flex_1()
                .min_h_0()
                .w_full()
                .child(panel.child(rows))
                .into_any_element()
        }
    }

    /// The application log: one mono line per entry.
    fn render_application(&self, _cx: &mut Context<Self>) -> AnyElement {
        if self.app_loading {
            return div().child(widgets::muted("loading…")).into_any_element();
        }
        if self.app_lines.is_empty() {
            return div()
                .child(widgets::empty_state("No application logs yet."))
                .into_any_element();
        }

        let lines = div()
            .id(theme::element_id("logs-app-scroll"))
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .w_full()
            .overflow_y_scroll()
            .children(self.app_lines.iter().map(|line| application_line(line)));

        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .w_full()
            .child(
                widgets::table_panel(std::iter::empty::<AnyElement>())
                    .flex_1()
                    .min_h_0()
                    .child(lines),
            )
            .into_any_element()
    }
}

impl Render for LogsView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .p(px(26.0))
            .gap(px(12.0))
            .size_full()
            .min_h_0()
            .overflow_hidden()
            .child(widgets::page_header(
                "Logs",
                "Recent API calls and the router's own log.",
            ))
            .child(self.render_toolbar(cx))
            .child(self.render_banner())
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .child(match self.source {
                        LogSource::Calls => self.render_calls(cx),
                        LogSource::Application => self.render_application(cx),
                    }),
            )
            .child(log_note())
            .into_any_element()
    }
}

/// The tone of a status pill: 5xx is broken, 4xx is degraded, the rest is
/// healthy. A status that is not a number stays neutral — it is not a sign
/// that the call worked.
fn status_tone(status: &str) -> Tone {
    match status.trim().parse::<i64>() {
        Ok(code) if code >= 500 => Tone::Bad,
        Ok(code) if code >= 400 => Tone::Warn,
        Ok(_) => Tone::Ok,
        Err(_) => Tone::Neutral,
    }
}

/// Shorten a timestamp: `2026-11-09T01:44:00.123Z` becomes
/// `2026-11-09 01:44:00`.
fn short_time(value: &str) -> String {
    let mut text = value.replacen('T', " ", 1);
    if let Some(dot) = text.rfind('.') {
        let tail = text[dot + 1..]
            .strip_suffix('Z')
            .unwrap_or(&text[dot + 1..]);
        if !tail.is_empty() && tail.chars().all(|character| character.is_ascii_digit()) {
            text.truncate(dot);
        }
    }
    text
}

/// Build one call-log data row from the shared column model.
fn call_row(columns: &[ColumnDef], row: &CallLog) -> AnyElement {
    let model = row
        .model
        .clone()
        .or_else(|| row.requested_model.clone())
        .unwrap_or_else(|| "—".to_string());
    let time = short_time(row.timestamp.as_deref().unwrap_or_default());
    let provider = row.provider.clone().unwrap_or_else(|| "—".to_string());
    let duration = row
        .duration_ms
        .map(|value| value.to_string())
        .unwrap_or_else(|| "—".to_string());
    let input = row
        .tokens_input
        .map(|value| value.to_string())
        .unwrap_or_else(|| "0".to_string());
    let output = row
        .tokens_output
        .map(|value| value.to_string())
        .unwrap_or_else(|| "0".to_string());
    let error = row.error_summary.clone().unwrap_or_default();

    let cells: Vec<AnyElement> = vec![
        mono_text(time, true).into_any_element(),
        status_pill(row.status.as_deref()).into_any_element(),
        mono_text(model, false).into_any_element(),
        mono_text(provider, true).into_any_element(),
        plain_text(duration).into_any_element(),
        plain_text(input).into_any_element(),
        plain_text(output).into_any_element(),
        muted_text(error).into_any_element(),
    ];
    widgets::defined_row(columns, cells, ROW_HEIGHT)
}

/// Monospaced cell text that truncates instead of forcing the column wider.
fn mono_text(text: impl Into<SharedString>, muted: bool) -> gpui::Div {
    div()
        .w_full()
        .min_w_0()
        .font(gpui::font(theme::MONO_FAMILY))
        .text_size(px(12.0))
        .text_color(if muted { theme::muted() } else { theme::text() })
        .truncate()
        .child(text.into())
}

/// Plain (non-mono) cell text for numeric columns.
fn plain_text(text: impl Into<SharedString>) -> gpui::Div {
    div()
        .w_full()
        .min_w_0()
        .text_size(px(13.0))
        .text_color(theme::text())
        .truncate()
        .child(text.into())
}

/// Muted cell text for the error column.
fn muted_text(text: impl Into<SharedString>) -> gpui::Div {
    div()
        .w_full()
        .min_w_0()
        .text_size(px(13.0))
        .text_color(theme::muted())
        .truncate()
        .child(text.into())
}

/// The status pill of a row — sized to its content, never stretched.
fn status_pill(status: Option<&str>) -> impl IntoElement {
    let (label, tone) = match status {
        Some(status) => (status.to_string(), status_tone(status)),
        None => ("—".to_string(), Tone::Neutral),
    };
    div()
        .flex()
        .items_center()
        .child(widgets::badge(label, tone))
}

/// One line of the application log with ANSI escapes removed.
fn application_line(line: &str) -> AnyElement {
    div()
        .w_full()
        .min_w_0()
        .px(px(10.0))
        .py(px(3.0))
        .font(gpui::font(theme::MONO_FAMILY))
        .text_size(px(12.0))
        .text_color(theme::text())
        .truncate()
        .child(widgets::strip_ansi(line))
        .into_any_element()
}

/// A tab of the source switch.
fn tab_button(
    label: impl Into<SharedString>,
    active: bool,
    on_click: impl Fn(&mut Window, &mut App) + 'static,
) -> impl IntoElement {
    let label = label.into();
    div()
        .id(theme::element_id(format!("log-tab:{label}")))
        .flex()
        .items_center()
        .px(px(12.0))
        .py(px(6.0))
        .rounded(px(theme::RADIUS_SM))
        .border_1()
        .border_color(if active {
            theme::accent()
        } else {
            theme::border()
        })
        .bg(if active {
            theme::panel_2()
        } else {
            gpui::transparent_black()
        })
        .text_size(px(13.0))
        .font_weight(if active {
            FontWeight::SEMIBOLD
        } else {
            FontWeight::NORMAL
        })
        .text_color(if active {
            theme::text()
        } else {
            theme::muted()
        })
        .cursor(CursorStyle::PointingHand)
        .on_click(move |_event, window, cx| on_click(window, cx))
        .child(label)
}

/// One option of the row limit selector.
fn limit_button(
    limit: usize,
    active: bool,
    on_click: impl Fn(&mut Window, &mut App) + 'static,
) -> impl IntoElement {
    div()
        .id(theme::element_id(format!("log-limit:{limit}")))
        .flex()
        .items_center()
        .px(px(9.0))
        .py(px(5.0))
        .rounded(px(theme::RADIUS_SM))
        .border_1()
        .border_color(if active {
            theme::accent()
        } else {
            theme::border()
        })
        .bg(if active {
            theme::panel_2()
        } else {
            theme::panel()
        })
        .text_size(px(12.0))
        .font_weight(if active {
            FontWeight::SEMIBOLD
        } else {
            FontWeight::NORMAL
        })
        .text_color(if active {
            theme::text()
        } else {
            theme::muted()
        })
        .cursor(CursorStyle::PointingHand)
        .on_click(move |_event, window, cx| on_click(window, cx))
        .child(limit.to_string())
}

/// The note under the logs.
fn log_note() -> AnyElement {
    div()
        .flex()
        .items_center()
        .gap_1()
        .child(
            div()
                .text_size(px(13.0))
                .text_color(theme::muted())
                .child("Application logs are written to"),
        )
        .child(
            div()
                .font(gpui::font(theme::MONO_FAMILY))
                .text_size(px(12.0))
                .text_color(theme::muted())
                .child("gateway.log"),
        )
        .child(
            div()
                .text_size(px(13.0))
                .text_color(theme::muted())
                .child("in the data dir (RUST_LOG level)."),
        )
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_timestamp_loses_its_fraction() {
        assert_eq!(
            short_time("2026-11-09T01:44:00.123456Z"),
            "2026-11-09 01:44:00"
        );
        assert_eq!(short_time("2026-11-09T01:44:00Z"), "2026-11-09 01:44:00Z");
        assert_eq!(short_time("2026-11-09 01:44:00"), "2026-11-09 01:44:00");
        assert_eq!(short_time(""), "");
    }

    #[test]
    fn a_status_picks_its_tone() {
        assert_eq!(status_tone("200"), Tone::Ok);
        assert_eq!(status_tone("404"), Tone::Warn);
        assert_eq!(status_tone("503"), Tone::Bad);
        assert_eq!(status_tone("nonsense"), Tone::Neutral);
    }

    #[test]
    fn a_numeric_status_decodes_into_the_contract() {
        // The gateway serializes `call_logs.status` as a number, so the typed
        // contract has to take a number — and to show it as text.
        let payload: LogsResponse = serde_json::from_str(
            r#"{"logs":[{"status":200,"model":"gpt-5.6-sol"},{"status":null}]}"#,
        )
        .expect("the payload decodes");

        assert_eq!(payload.logs.len(), 2);
        assert_eq!(payload.logs[0].status.as_deref(), Some("200"));
        assert_eq!(payload.logs[0].model.as_deref(), Some("gpt-5.6-sol"));
        assert_eq!(payload.logs[1].status, None);
    }

    #[test]
    fn the_typed_contract_also_takes_a_status_written_as_text() {
        let payload = r#"{"logs":[{"status":"200"}]}"#;
        let decoded: LogsResponse = serde_json::from_str(payload).expect("the payload decodes");
        assert_eq!(decoded.logs[0].status.as_deref(), Some("200"));
    }

    #[test]
    fn call_log_columns_are_a_single_shared_definition() {
        let columns = call_log_columns();
        let labels: Vec<&str> = columns.iter().map(|c| c.label).collect();
        assert_eq!(
            labels,
            vec![
                "TIME", "STATUS", "MODEL", "PROVIDER", "MS", "IN", "OUT", "ERROR"
            ]
        );
        assert_eq!(columns[0].sizing, ColumnSizing::Flex);
        assert_eq!(columns[1].sizing, ColumnSizing::Fixed(72));
        assert_eq!(columns[2].sizing, ColumnSizing::Flex);
        assert_eq!(columns[3].sizing, ColumnSizing::Flex);
        assert_eq!(
            columns[4],
            ColumnDef {
                label: "MS",
                sizing: ColumnSizing::Fixed(64),
                align: ColumnAlign::Right
            }
        );
        assert_eq!(
            columns[5],
            ColumnDef {
                label: "IN",
                sizing: ColumnSizing::Fixed(64),
                align: ColumnAlign::Right
            }
        );
        assert_eq!(
            columns[6],
            ColumnDef {
                label: "OUT",
                sizing: ColumnSizing::Fixed(64),
                align: ColumnAlign::Right
            }
        );
        assert_eq!(
            columns[7],
            ColumnDef {
                label: "ERROR",
                sizing: ColumnSizing::GrowBasis(200),
                align: ColumnAlign::Left
            }
        );
    }
}

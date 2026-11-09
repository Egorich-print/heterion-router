//! Logs — the API call log and the router's application log.
//!
//! Ported from `desktop/ui/src/views/Logs.svelte`: two sources, a row limit
//! and a refresh action. Both lists are drawn with `gpui::uniform_list`, so a
//! log with thousands of rows only costs the rows that are on screen.

use gpui::prelude::*;
use gpui::{
    AnyElement, App, Context, CursorStyle, ElementId, FontWeight, IntoElement, Render,
    SharedString, Window, div, px,
};
use serde::Deserialize;

use crate::api::{self, AppLogsResponse, CallLog, LogsResponse};
use crate::theme;
use crate::widgets::{self, Tone};

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

/// The height of one row of the call log. Every row has the same height —
/// the virtualized list measures the first one and reuses it.
const ROW_HEIGHT: f32 = 30.0;

/// The width of the status column.
const STATUS_WIDTH: f32 = 72.0;

/// The width of the numeric columns.
const NUMBER_WIDTH: f32 = 64.0;

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
            move || client.get_json::<WireLogsResponse>(&format!("/api/logs?limit={limit}")),
            |result, view: &mut LogsView, cx| {
                match result {
                    Ok(payload) => {
                        view.data = Some(LogsResponse {
                            logs: payload.logs.into_iter().map(CallLog::from).collect(),
                        });
                        // The gateway reports a failed read in the payload itself.
                        view.error = payload.error;
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
    fn render_calls(&self, cx: &mut Context<Self>) -> AnyElement {
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

        let this = cx.entity();
        let count = payload.logs.len();
        let list = gpui::uniform_list(
            ElementId::named_usize("logs-calls", 0),
            count,
            move |range, _window, cx| -> Vec<AnyElement> {
                let view = this.read(cx);
                let Some(payload) = view.data.as_ref() else {
                    return Vec::new();
                };
                range
                    .filter_map(|index| payload.logs.get(index))
                    .map(call_row)
                    .collect()
            },
        );

        let panel = div()
            .flex()
            .flex_col()
            .bg(theme::panel())
            .border_1()
            .border_color(theme::border())
            .rounded(px(theme::RADIUS))
            .overflow_hidden();

        let body = div().flex().flex_col().flex_1().min_h_0();
        if count == 0 {
            // The table keeps its header, the note sits below it.
            body.child(panel.child(header_row()))
                .child(widgets::empty_state("No calls recorded yet."))
                .into_any_element()
        } else {
            body.child(
                panel
                    .flex_1()
                    .min_h_0()
                    .child(header_row())
                    .child(list.flex_1().min_h_0()),
            )
            .into_any_element()
        }
    }

    /// The application log: one mono line per entry.
    fn render_application(&self, cx: &mut Context<Self>) -> AnyElement {
        if self.app_loading {
            return div().child(widgets::muted("loading…")).into_any_element();
        }
        if self.app_lines.is_empty() {
            return div()
                .child(widgets::empty_state("No application logs yet."))
                .into_any_element();
        }

        let this = cx.entity();
        let count = self.app_lines.len();
        let list = gpui::uniform_list(
            ElementId::named_usize("logs-application", 0),
            count,
            move |range, _window, cx| -> Vec<AnyElement> {
                let view = this.read(cx);
                range
                    .filter_map(|index| view.app_lines.get(index))
                    .map(|line| application_line(line))
                    .collect()
            },
        );

        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .child(list.flex_1().min_h_0())
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
            .h_full()
            .min_h_0()
            .child(widgets::page_header(
                "Logs",
                "Recent API calls and the router's own log.",
            ))
            .child(self.render_toolbar(cx))
            .child(self.render_banner())
            .child(match self.source {
                LogSource::Calls => self.render_calls(cx),
                LogSource::Application => self.render_application(cx),
            })
            .child(log_note())
            .into_any_element()
    }
}

/// The call log payload as the gateway sends it.
///
/// The typed contract keeps the status in a string, but the gateway sends a
/// number, so the rows are decoded here and turned into the contract
/// afterwards.
#[derive(Debug, Deserialize)]
struct WireLogsResponse {
    /// The call log rows.
    #[serde(default)]
    logs: Vec<WireCallLog>,
    /// The failure the gateway reports in the payload itself.
    #[serde(default)]
    error: Option<String>,
}

/// One call log row as the gateway sends it.
#[derive(Debug, Deserialize)]
struct WireCallLog {
    /// Timestamp of the call.
    #[serde(default)]
    timestamp: Option<String>,
    /// Model the gateway served.
    #[serde(default)]
    model: Option<String>,
    /// Model the client asked for.
    #[serde(default)]
    requested_model: Option<String>,
    /// Provider that served the call.
    #[serde(default)]
    provider: Option<String>,
    /// Upstream status — a number on the wire.
    #[serde(default, deserialize_with = "status_text")]
    status: Option<String>,
    /// Duration in milliseconds.
    #[serde(default)]
    duration_ms: Option<i64>,
    /// Input tokens.
    #[serde(default)]
    tokens_input: Option<i64>,
    /// Output tokens.
    #[serde(default)]
    tokens_output: Option<i64>,
    /// Combo the call was routed through.
    #[serde(default)]
    combo_name: Option<String>,
    /// Error summary, when the call failed.
    #[serde(default)]
    error_summary: Option<String>,
}

impl From<WireCallLog> for CallLog {
    fn from(wire: WireCallLog) -> Self {
        CallLog {
            timestamp: wire.timestamp,
            model: wire.model,
            requested_model: wire.requested_model,
            provider: wire.provider,
            status: wire.status,
            duration_ms: wire.duration_ms,
            tokens_input: wire.tokens_input,
            tokens_output: wire.tokens_output,
            combo_name: wire.combo_name,
            error_summary: wire.error_summary,
        }
    }
}

/// Read a status the gateway sends as a number, but the typed contract keeps
/// as a string.
fn status_text<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = serde_json::Value::deserialize(deserializer)?;
    Ok(match value {
        serde_json::Value::Null => None,
        serde_json::Value::String(text) => Some(text),
        other => Some(other.to_string()),
    })
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

/// The header of the call log table.
fn header_row() -> AnyElement {
    div()
        .flex()
        .items_center()
        .gap_3()
        .px(px(10.0))
        .py(px(7.0))
        .border_b_1()
        .border_color(theme::border())
        .child(header_label("TIME").flex_1().min_w_0())
        .child(header_label("STATUS").w(px(STATUS_WIDTH)).flex_shrink_0())
        .child(header_label("MODEL").flex_1().min_w_0())
        .child(header_label("PROVIDER").flex_1().min_w_0())
        .child(
            header_label("MS")
                .w(px(NUMBER_WIDTH))
                .flex_shrink_0()
                .text_align(gpui::TextAlign::Right),
        )
        .child(
            header_label("IN")
                .w(px(NUMBER_WIDTH))
                .flex_shrink_0()
                .text_align(gpui::TextAlign::Right),
        )
        .child(
            header_label("OUT")
                .w(px(NUMBER_WIDTH))
                .flex_shrink_0()
                .text_align(gpui::TextAlign::Right),
        )
        .child(
            header_label("ERROR")
                .flex_grow()
                .flex_basis(px(200.0))
                .min_w_0(),
        )
        .into_any_element()
}

/// One header cell.
fn header_label(text: &str) -> gpui::Div {
    div()
        .text_size(px(11.0))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(theme::muted())
        .child(text.to_string())
}

/// One row of the call log.
fn call_row(row: &CallLog) -> AnyElement {
    let model = row
        .model
        .clone()
        .or_else(|| row.requested_model.clone())
        .unwrap_or_else(|| "—".to_string());

    div()
        .flex()
        .items_center()
        .gap_3()
        .px(px(10.0))
        .h(px(ROW_HEIGHT))
        .border_b_1()
        .border_color(theme::border())
        .child(
            mono_cell(
                short_time(row.timestamp.as_deref().unwrap_or_default()),
                true,
            )
            .flex_1()
            .min_w_0(),
        )
        .child(status_cell(row.status.as_deref()))
        .child(mono_cell(model, false).flex_1().min_w_0())
        .child(
            mono_cell(
                row.provider.clone().unwrap_or_else(|| "—".to_string()),
                true,
            )
            .flex_1()
            .min_w_0(),
        )
        .child(number_cell(
            row.duration_ms
                .map(|value| value.to_string())
                .unwrap_or_else(|| "—".to_string()),
        ))
        .child(number_cell(
            row.tokens_input
                .map(|value| value.to_string())
                .unwrap_or_else(|| "0".to_string()),
        ))
        .child(number_cell(
            row.tokens_output
                .map(|value| value.to_string())
                .unwrap_or_else(|| "0".to_string()),
        ))
        .child(error_cell(row.error_summary.as_deref().unwrap_or_default()))
        .into_any_element()
}

/// A monospaced cell that takes the free space of the row.
fn mono_cell(text: impl Into<SharedString>, muted: bool) -> gpui::Div {
    div()
        .flex_1()
        .min_w_0()
        .font(gpui::font(theme::MONO_FAMILY))
        .text_size(px(12.0))
        .text_color(if muted { theme::muted() } else { theme::text() })
        .truncate()
        .child(text.into())
}

/// A right-aligned number in a fixed column.
fn number_cell(text: impl Into<SharedString>) -> gpui::Div {
    div()
        .w(px(NUMBER_WIDTH))
        .flex_shrink_0()
        .text_size(px(13.0))
        .text_color(theme::text())
        .text_align(gpui::TextAlign::Right)
        .child(text.into())
}

/// The status pill of a row.
fn status_cell(status: Option<&str>) -> gpui::Div {
    let (label, tone) = match status {
        Some(status) => (status.to_string(), status_tone(status)),
        None => ("—".to_string(), Tone::Neutral),
    };
    div()
        .w(px(STATUS_WIDTH))
        .flex_shrink_0()
        .child(widgets::badge(label, tone))
}

/// The error summary of a row.
fn error_cell(text: &str) -> gpui::Div {
    div()
        .flex_grow()
        .flex_basis(px(200.0))
        .min_w_0()
        .text_size(px(13.0))
        .text_color(theme::muted())
        .truncate()
        .child(text.to_string())
}

/// One line of the application log.
fn application_line(line: &str) -> AnyElement {
    div()
        .font(gpui::font(theme::MONO_FAMILY))
        .text_size(px(12.0))
        .text_color(theme::text())
        .truncate()
        .child(line.to_string())
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
        let payload: WireLogsResponse = serde_json::from_str(
            r#"{"logs":[{"status":200,"model":"gpt-5.6-sol"},{"status":null}]}"#,
        )
        .expect("the payload decodes");

        assert_eq!(payload.logs.len(), 2);
        let rows: Vec<CallLog> = payload.logs.into_iter().map(CallLog::from).collect();
        assert_eq!(rows[0].status.as_deref(), Some("200"));
        assert_eq!(rows[0].model.as_deref(), Some("gpt-5.6-sol"));
        assert_eq!(rows[1].status, None);
    }

    #[test]
    fn the_typed_contract_alone_would_reject_a_numeric_status() {
        // The reason the wire type exists: `api::CallLog` wants a string.
        let payload = r#"{"logs":[{"status":200}]}"#;
        assert!(serde_json::from_str::<LogsResponse>(payload).is_err());
    }
}

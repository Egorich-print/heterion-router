//! Auto — the `auto` combo as one live screen.
//!
//! The whole application is built around one combo: `heterion-router/auto`.
//! This screen shows the two things that decide what it serves — the ladder
//! of models the router would try, and how much of each platform's budget is
//! left — and it keeps both live.
//!
//! Both halves are the gateway's own measurements, never a claim:
//!
//! * `GET /api/routing/plan?model=auto` — the candidates in the order the
//!   router would try them, with the score it sorts by (`effective`) and the
//!   reason a model is not selectable (`no_backend`, `cooldown`, …);
//! * `GET /api/providers/quota` — what each platform said about its own
//!   budget: `available`, `exhausted`, `credits_required`, `needs_key` or
//!   `unknown`;
//! * `GET /api/providers/catalog` — the models each platform serves today.
//!
//! The screen refreshes itself: a worker thread asks the gateway every few
//! seconds and hands the answers to the UI thread, so the ladder shows the
//! movement (a model that rose, a model that fell) and the quota shows what
//! the platform said the last time it answered.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use gpui::prelude::*;
use gpui::{App, Context, IntoElement, Render, SharedString, Window, div, px};

use crate::api::{
    self, ApiClient, ApiError, CatalogResponse, PlanCandidate, QuotaReading, QuotaResponse,
    RoutingPlan,
};
use crate::theme;
use crate::widgets::{self, ColumnAlign, ColumnDef, ColumnSizing, Tone};

/// How many candidates the ladder shows before the operator asks for more.
const LADDER_PAGE: usize = 25;

/// The refresh intervals the operator can pick, in seconds.
const INTERVALS: [u64; 3] = [2, 5, 15];

/// The default refresh interval.
const DEFAULT_INTERVAL: u64 = 5;

/// The screen: the `auto` combo, live.
pub struct AutoView {
    /// The ladder of the `auto` combo.
    pub plan: Option<RoutingPlan>,
    /// The quota readings of the platforms.
    pub quota: Option<QuotaResponse>,
    /// The catalogues the platforms published.
    pub catalog: Option<CatalogResponse>,
    /// The last failure, rendered as a banner.
    pub error: Option<String>,
    /// Whether the first answer is still on its way.
    pub loading: bool,
    /// The refresh interval in seconds.
    pub interval_secs: u64,
    /// Whether the plan is built for a request with tools (an agent run).
    pub tools: bool,
    /// Whether the ladder shows every candidate, not only the head.
    pub show_all: bool,
    /// The scores of the previous poll, to show the movement.
    previous: HashMap<String, f64>,
    /// The movement since the previous poll.
    deltas: HashMap<String, f64>,
    /// When the last answer arrived.
    last_update: Option<Instant>,
}

impl AutoView {
    /// Build the view and start the live polling.
    pub fn new(cx: &mut Context<Self>) -> Self {
        let mut view = Self {
            plan: None,
            quota: None,
            catalog: None,
            error: None,
            loading: true,
            interval_secs: DEFAULT_INTERVAL,
            tools: true,
            show_all: false,
            previous: HashMap::new(),
            deltas: HashMap::new(),
            last_update: None,
        };
        view.start_poller(cx);
        view
    }

    /// Ask the gateway once, right now.
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        let Some(client) = api::session_client(cx) else {
            return;
        };
        let tools = self.tools;
        api::spawn_request(
            cx,
            move || Ok::<LivePayload, ApiError>(fetch_live(&client, tools)),
            |result, view: &mut AutoView, cx| {
                match result {
                    Ok(payload) => view.apply(payload, cx),
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

    /// Start the worker thread that keeps the screen live.
    ///
    /// gpui 0.2.2 has no timer, so the thread asks, waits and asks again. It
    /// stops when nobody listens any more: the view is gone, the channel is
    /// dropped, and the send fails.
    fn start_poller(&mut self, cx: &mut Context<Self>) {
        let Some(client) = api::session_client(cx) else {
            return;
        };
        let interval = Duration::from_secs(self.interval_secs.max(1));
        let tools = self.tools;
        let (tx, rx) = async_channel::unbounded::<LivePayload>();

        std::thread::spawn(move || {
            loop {
                let payload = fetch_live(&client, tools);
                if tx.send_blocking(payload).is_err() {
                    break;
                }
                std::thread::sleep(interval);
            }
        });

        cx.spawn(async move |this, cx| {
            while let Ok(payload) = rx.recv().await {
                let _ = this.update(cx, |view, cx| view.apply(payload, cx));
            }
        })
        .detach();
    }

    /// Take an answer of the poller.
    fn apply(&mut self, payload: LivePayload, cx: &mut Context<Self>) {
        let mut failures: Vec<String> = Vec::new();

        match payload.plan {
            Ok(plan) => {
                // The movement is measured against the previous poll: a model
                // that rose or fell in the ladder is what the operator wants
                // to see, not the absolute score alone.
                let mut current: HashMap<String, f64> = HashMap::new();
                let mut deltas: HashMap<String, f64> = HashMap::new();
                for candidate in &plan.candidates {
                    let key = candidate_key(candidate);
                    if let Some(previous) = self.previous.get(&key) {
                        let delta = candidate.effective - previous;
                        if delta.abs() > 0.01 {
                            deltas.insert(key.clone(), delta);
                        }
                    }
                    current.insert(key, candidate.effective);
                }
                self.previous = current;
                self.deltas = deltas;
                self.plan = Some(plan);
            }
            Err(error) => failures.push(format!("план: {error}")),
        }

        match payload.quota {
            Ok(quota) => self.quota = Some(quota),
            Err(error) => failures.push(format!("квота: {error}")),
        }

        match payload.catalog {
            Ok(catalog) => self.catalog = Some(catalog),
            Err(error) => failures.push(format!("каталог: {error}")),
        }

        self.error = if failures.is_empty() {
            None
        } else {
            Some(failures.join(" · "))
        };
        self.last_update = Some(Instant::now());
        cx.notify();
    }

    /// Change the refresh interval and restart the poller with it.
    fn set_interval(&mut self, seconds: u64, cx: &mut Context<Self>) {
        if self.interval_secs == seconds {
            return;
        }
        self.interval_secs = seconds;
        self.start_poller(cx);
        cx.notify();
    }

    /// Toggle the plan between a plain request and one with tools.
    fn set_tools(&mut self, tools: bool, cx: &mut Context<Self>) {
        if self.tools == tools {
            return;
        }
        self.tools = tools;
        self.start_poller(cx);
        cx.notify();
    }

    /// The candidates the ladder shows.
    fn visible_candidates(&self) -> Vec<&PlanCandidate> {
        let Some(plan) = self.plan.as_ref() else {
            return Vec::new();
        };
        if self.show_all {
            plan.candidates.iter().collect()
        } else {
            plan.candidates.iter().take(LADDER_PAGE).collect()
        }
    }

    /// The score the ladder is measured against — the head of the list.
    fn head_score(&self) -> f64 {
        self.plan
            .as_ref()
            .map(|plan| {
                plan.candidates
                    .iter()
                    .map(|c| c.effective)
                    .fold(0.0_f64, f64::max)
            })
            .unwrap_or(0.0)
    }
}

/// The three answers of one poll.
struct LivePayload {
    plan: Result<RoutingPlan, ApiError>,
    quota: Result<QuotaResponse, ApiError>,
    catalog: Result<CatalogResponse, ApiError>,
}

/// Ask the gateway for everything the screen shows.
fn fetch_live(client: &ApiClient, tools: bool) -> LivePayload {
    let plan_path = if tools {
        "/api/routing/plan?model=auto&tools=1"
    } else {
        "/api/routing/plan?model=auto"
    };
    LivePayload {
        plan: client.get_json::<RoutingPlan>(plan_path),
        quota: client.get_json::<QuotaResponse>("/api/providers/quota"),
        catalog: client.get_json::<CatalogResponse>("/api/providers/catalog"),
    }
}

/// The key of a candidate in the movement map.
fn candidate_key(candidate: &PlanCandidate) -> String {
    format!("{}/{}", candidate.provider, candidate.model)
}

/// The state of a quota reading, as the platform reported it.
fn state_of(reading: &QuotaReading) -> &str {
    if reading.state.is_empty() {
        "unknown"
    } else {
        reading.state.as_str()
    }
}

/// The label and the tone of a quota state.
fn state_label(state: &str) -> (&'static str, Tone) {
    match state {
        "available" => ("свободно", Tone::Ok),
        "exhausted" => ("исчерпано", Tone::Bad),
        "credits_required" => ("нужны кредиты", Tone::Warn),
        "needs_key" => ("нет ключа", Tone::Bad),
        "unknown" => ("неизвестно", Tone::Neutral),
        _ => ("неизвестно", Tone::Neutral),
    }
}

/// How bad a state is — the worst state of a platform wins.
fn state_rank(state: &str) -> u8 {
    match state {
        "available" => 0,
        "unknown" => 1,
        "needs_key" => 2,
        "credits_required" => 3,
        "exhausted" => 4,
        _ => 1,
    }
}

/// The worst state of a platform, with the rest of its reading.
struct ProviderQuota {
    provider: String,
    state: String,
    connections: usize,
    remaining: Option<f64>,
    window: String,
    measured_at: Option<String>,
    stale: bool,
    words: String,
}

/// Collect the readings of the platforms — one row per platform.
fn provider_quotas(quota: &QuotaResponse) -> Vec<ProviderQuota> {
    let mut by_provider: HashMap<String, ProviderQuota> = HashMap::new();

    for reading in &quota.providers {
        let state = state_of(reading).to_string();
        let entry = by_provider
            .entry(reading.provider.clone())
            .or_insert_with(|| ProviderQuota {
                provider: reading.provider.clone(),
                state: "available".to_string(),
                connections: 0,
                remaining: None,
                window: reading.window.clone(),
                measured_at: reading.measured_at.clone(),
                stale: reading.stale,
                words: String::new(),
            });
        entry.connections += 1;
        if state_rank(&state) > state_rank(&entry.state) {
            entry.state = state;
        }
        if let Some(remaining) = reading.remaining_percentage {
            entry.remaining = Some(match entry.remaining {
                Some(current) => current.min(remaining),
                None => remaining,
            });
        }
        if reading.measured_at.is_some() {
            entry.measured_at = reading.measured_at.clone();
        }
        entry.stale = entry.stale && reading.stale;
        let words = platform_words(reading.detail.as_deref());
        if !words.is_empty() {
            entry.words = words;
        }
    }

    let mut rows: Vec<ProviderQuota> = by_provider.into_values().collect();
    rows.sort_by(|left, right| {
        state_rank(&right.state)
            .cmp(&state_rank(&left.state))
            .then_with(|| left.provider.cmp(&right.provider))
    });
    rows
}

/// The platform's own words, without the JSON wrapper they arrive in.
///
/// A reading is stored as `{"state":"…","detail":"…"}`, and the detail is the
/// platform's own answer — often JSON of its own, often pretty-printed. Three
/// steps to one line: the wrapper is opened, the sentence inside it is taken
/// (`error.message` or `message`), and the line breaks are folded away — a
/// line break inside a table cell spills over the row below it.
fn platform_words(detail: Option<&str>) -> String {
    let Some(raw) = detail else {
        return String::new();
    };
    let inner = serde_json::from_str::<serde_json::Value>(raw)
        .ok()
        .and_then(|value| {
            value
                .get("detail")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
        })
        .unwrap_or_else(|| raw.to_string());

    let sentence = serde_json::from_str::<serde_json::Value>(&inner)
        .ok()
        .and_then(|value| {
            let from_error = value
                .get("error")
                .and_then(|error| error.get("message"))
                .and_then(serde_json::Value::as_str)
                .map(str::to_string);
            from_error.or_else(|| {
                value
                    .get("message")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string)
            })
        })
        .unwrap_or(inner);

    let flat = sentence.replace(['\n', '\r'], " ");
    let joined = flat.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut words: String = joined.chars().take(110).collect();
    if joined.chars().count() > 110 {
        words.push('…');
    }
    words
}

/// The columns of the ladder table.
fn ladder_columns() -> Vec<ColumnDef> {
    vec![
        ColumnDef {
            label: "#",
            sizing: ColumnSizing::Fixed(38),
            align: ColumnAlign::Right,
        },
        ColumnDef {
            label: "МОДЕЛЬ",
            sizing: ColumnSizing::Flex,
            align: ColumnAlign::Left,
        },
        ColumnDef {
            label: "ПРОВАЙДЕР",
            sizing: ColumnSizing::Fixed(132),
            align: ColumnAlign::Left,
        },
        ColumnDef {
            label: "ПРИОРИТЕТ",
            sizing: ColumnSizing::Fixed(120),
            align: ColumnAlign::Right,
        },
        ColumnDef {
            label: "ДВИЖЕНИЕ",
            sizing: ColumnSizing::Fixed(92),
            align: ColumnAlign::Left,
        },
        ColumnDef {
            label: "КВОТА",
            sizing: ColumnSizing::Fixed(124),
            align: ColumnAlign::Left,
        },
    ]
}

/// The columns of the quota table.
fn quota_columns() -> Vec<ColumnDef> {
    vec![
        ColumnDef {
            label: "ПЛОЩАДКА",
            sizing: ColumnSizing::Fixed(140),
            align: ColumnAlign::Left,
        },
        ColumnDef {
            label: "СОСТОЯНИЕ",
            sizing: ColumnSizing::Fixed(120),
            align: ColumnAlign::Left,
        },
        ColumnDef {
            label: "ОСТАТОК",
            sizing: ColumnSizing::Fixed(84),
            align: ColumnAlign::Right,
        },
        ColumnDef {
            label: "ОКНО",
            sizing: ColumnSizing::Fixed(96),
            align: ColumnAlign::Left,
        },
        ColumnDef {
            label: "СОЕДИНЕНИЙ",
            sizing: ColumnSizing::Fixed(96),
            align: ColumnAlign::Right,
        },
        ColumnDef {
            label: "ЧТО СКАЗАЛА ПЛОЩАДКА",
            sizing: ColumnSizing::Flex,
            align: ColumnAlign::Left,
        },
    ]
}

/// A number with a bar under it — the score in the ladder.
fn score_cell(score: f64, head: f64) -> impl IntoElement {
    let fraction = if head > 0.0 {
        (score / head).clamp(0.0, 1.0)
    } else {
        0.0
    };
    div()
        .flex()
        .flex_col()
        .items_end()
        .gap_1()
        .child(
            div()
                .text_size(px(13.0))
                .text_color(theme::text())
                .child(format!("{score:.1}")),
        )
        .child(
            div()
                .h(px(4.0))
                .w_full()
                .rounded(px(2.0))
                .bg(theme::panel_2())
                .child(
                    div()
                        .h(px(4.0))
                        .w(gpui::relative(fraction as f32))
                        .rounded(px(2.0))
                        .bg(theme::accent()),
                ),
        )
}

/// The movement of a candidate since the previous poll.
fn delta_cell(delta: Option<f64>) -> impl IntoElement {
    let (label, color) = match delta {
        Some(value) if value > 0.0 => ("▲", theme::ok()),
        Some(value) if value < 0.0 => ("▼", theme::warn()),
        _ => ("—", theme::muted()),
    };
    let text = match delta {
        Some(value) => format!("{label} {value:+.1}"),
        None => label.to_string(),
    };
    div().text_size(px(12.0)).text_color(color).child(text)
}

/// The live indicator — how long ago the last answer came.
fn live_label(last_update: Option<Instant>) -> impl IntoElement {
    let (text, color) = match last_update {
        Some(instant) => {
            let seconds = instant.elapsed().as_secs();
            (format!("живое обновление · {seconds} с назад"), theme::ok())
        }
        None => ("ожидание первого ответа…".to_string(), theme::muted()),
    };
    div()
        .flex()
        .items_center()
        .gap_2()
        .text_size(px(12.0))
        .text_color(color)
        .child(div().size(px(8.0)).rounded_full().bg(color))
        .child(text)
}

/// A small labelled button that shows whether it is on.
fn toggle_button(
    key: impl Into<SharedString>,
    label: impl Into<SharedString>,
    on: bool,
    on_click: impl Fn(&mut Window, &mut App) + 'static,
) -> impl IntoElement {
    let key = key.into();
    let label = label.into();
    div()
        .id(theme::element_id(key))
        .flex()
        .items_center()
        .px(px(10.0))
        .py(px(5.0))
        .rounded(px(theme::RADIUS_SM))
        .border_1()
        .border_color(if on { theme::accent() } else { theme::border() })
        .bg(if on {
            theme::accent_faint()
        } else {
            theme::panel()
        })
        .text_size(px(12.0))
        .text_color(if on { theme::accent() } else { theme::muted() })
        .cursor(gpui::CursorStyle::PointingHand)
        .hover(|style| style.border_color(theme::muted()))
        .on_click(move |_event, window, cx| on_click(window, cx))
        .child(label)
}

impl Render for AutoView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let entity = cx.entity();

        let mut root = div()
            .flex()
            .flex_col()
            .gap_4()
            .px(px(theme::CONTENT_PADDING_X))
            .py(px(theme::CONTENT_PADDING_Y));

        // The heading and the live state of the screen.
        root = root.child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .gap_4()
                .child(widgets::page_header(
                    "Auto combo",
                    "heterion-router/auto — лестница моделей и квота площадок, живьём",
                ))
                .child(live_label(self.last_update)),
        );

        // The toolbar: the refresh interval, the plan flavour, the refresh
        // button and the "show everything" switch.
        let refresh = {
            let entity = entity.clone();
            move |_: &mut Window, cx: &mut App| {
                entity.update(cx, |view, cx| {
                    view.refresh(cx);
                    cx.notify();
                });
            }
        };
        let mut toolbar: Vec<gpui::AnyElement> = Vec::new();
        for seconds in INTERVALS {
            let entity = entity.clone();
            let on = self.interval_secs == seconds;
            toolbar.push(
                toggle_button(
                    &format!("auto-interval-{seconds}"),
                    &format!("{seconds} с"),
                    on,
                    move |_: &mut Window, cx: &mut App| {
                        entity.update(cx, |view, cx| {
                            view.set_interval(seconds, cx);
                            cx.notify();
                        });
                    },
                )
                .into_any_element(),
            );
        }
        let tools_entity = entity.clone();
        let tools_on = self.tools;
        toolbar.push(
            toggle_button(
                "auto-tools",
                if tools_on {
                    "запрос с инструментами"
                } else {
                    "запрос без инструментов"
                },
                tools_on,
                move |_: &mut Window, cx: &mut App| {
                    tools_entity.update(cx, |view, cx| {
                        view.set_tools(!tools_on, cx);
                        cx.notify();
                    });
                },
            )
            .into_any_element(),
        );
        let all_entity = entity.clone();
        let all_on = self.show_all;
        toolbar.push(
            toggle_button(
                "auto-show-all",
                if all_on {
                    "показать голову"
                } else {
                    "показать все"
                },
                all_on,
                move |_: &mut Window, cx: &mut App| {
                    all_entity.update(cx, |view, cx| {
                        view.show_all = !all_on;
                        cx.notify();
                    });
                },
            )
            .into_any_element(),
        );
        toolbar
            .push(widgets::button("auto-refresh", "Обновить сейчас", refresh).into_any_element());

        root = root.child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .flex_wrap()
                .child(
                    div()
                        .text_size(px(12.0))
                        .text_color(theme::muted())
                        .child("интервал:"),
                )
                .children(toolbar),
        );

        if let Some(message) = self.error.clone() {
            root = root.child(widgets::error_banner(message));
        }

        // The numbers: what the ladder holds and how much budget is left.
        let quotas = self.quota.as_ref().map(provider_quotas).unwrap_or_default();
        let candidates = self
            .plan
            .as_ref()
            .map(|plan| plan.candidates.len())
            .unwrap_or(0);
        // The head of the ladder is a long name, and the metric card has a
        // fixed width: the name is cut, the card is not.
        let head = self
            .plan
            .as_ref()
            .and_then(|plan| plan.candidates.first())
            .map(|candidate| {
                let full = format!("{}/{}", candidate.provider, candidate.model);
                if full.chars().count() > 26 {
                    let mut cut: String = full.chars().take(25).collect();
                    cut.push('…');
                    cut
                } else {
                    full
                }
            })
            .unwrap_or_else(|| "—".to_string());
        let available = quotas.iter().filter(|row| row.state == "available").count();
        let spent = quotas
            .iter()
            .filter(|row| state_rank(&row.state) >= state_rank("credits_required"))
            .count();
        let models: usize = self
            .catalog
            .as_ref()
            .map(|catalog| catalog.providers.iter().map(|p| p.count).sum())
            .unwrap_or(0);

        root = root.child(
            div()
                .flex()
                .gap_3()
                .child(widgets::stat_card(
                    "кандидатов в лестнице",
                    candidates.to_string(),
                ))
                .child(widgets::stat_card(
                    "моделей в живых каталогах",
                    models.to_string(),
                ))
                .child(widgets::stat_card(
                    "площадок свободно",
                    format!("{available} / {}", quotas.len()),
                ))
                .child(widgets::stat_card("квота исчерпана", spent.to_string()))
                .child(widgets::stat_card("голова лестницы", head)),
        );

        // The two tables, one under the other: side by side the columns of
        // the ladder have no room left for the model id, and the model id is
        // the thing the ladder is about.
        let ladder = ladder_table(self.ladder_rows(), self.head_score(), candidates);
        let quota = quota_table(quotas);

        root = root.child(
            div()
                .flex()
                .flex_col()
                .gap_4()
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_2()
                        .child(widgets::section_title("Лестница приоритетов"))
                        .child(ladder),
                )
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_2()
                        .child(widgets::section_title("Квота площадок"))
                        .child(quota),
                ),
        );

        root
    }
}

/// One row of the ladder table — owned, so the table can be built without
/// borrowing the view twice.
struct LadderRow {
    model: String,
    provider: String,
    effective: f64,
    delta: Option<f64>,
    state: String,
}

impl AutoView {
    /// The rows of the ladder table.
    ///
    /// The quota of a platform is folded into the row of its models: a model
    /// of a spent platform is marked as such, which is what the operator
    /// wants to know — the ladder says *which* model comes next, the quota
    /// says whether that platform can serve it at all.
    fn ladder_rows(&self) -> Vec<LadderRow> {
        let by_provider: HashMap<String, String> = self
            .quota
            .as_ref()
            .map(|quota| {
                provider_quotas(quota)
                    .into_iter()
                    .map(|row| (row.provider, row.state))
                    .collect()
            })
            .unwrap_or_default();

        self.visible_candidates()
            .into_iter()
            .map(|candidate| LadderRow {
                model: candidate.model.clone(),
                provider: candidate.provider.clone(),
                effective: candidate.effective,
                delta: self.deltas.get(&candidate_key(candidate)).copied(),
                state: by_provider
                    .get(&candidate.provider)
                    .cloned()
                    .unwrap_or_else(|| "unknown".to_string()),
            })
            .collect()
    }
}

/// The ladder table: the candidates in the order the router would try them.
fn ladder_table(rows: Vec<LadderRow>, head: f64, total: usize) -> impl IntoElement {
    let columns = ladder_columns();

    let mut cells: Vec<gpui::AnyElement> = Vec::new();
    for (index, row) in rows.iter().enumerate() {
        let (label, tone) = state_label(&row.state);
        let row_cells = vec![
            widgets::number((index + 1).to_string()).into_any_element(),
            div()
                .text_size(px(13.0))
                .text_color(theme::text())
                .truncate()
                .child(row.model.clone())
                .into_any_element(),
            div()
                .text_size(px(13.0))
                .text_color(theme::muted())
                .truncate()
                .child(row.provider.clone())
                .into_any_element(),
            score_cell(row.effective, head).into_any_element(),
            delta_cell(row.delta).into_any_element(),
            widgets::compact_badge(label, tone),
        ];
        cells.push(widgets::defined_row(&columns, row_cells, 32.0));
    }

    let mut panel =
        widgets::table_panel(vec![widgets::defined_header(&columns).into_any_element()]);
    if cells.is_empty() {
        panel = panel.child(
            widgets::empty_state("лестница пуста: у шлюза нет кандидатов для auto")
                .into_any_element(),
        );
    } else {
        panel = panel.children(cells);
    }

    let mut column = div().flex().flex_col().gap_2();
    column = column.child(panel);
    if rows.len() < total {
        column = column.child(
            div()
                .text_size(px(12.0))
                .text_color(theme::muted())
                .child(format!(
                    "показано {} из {total} — переключите «показать все», чтобы увидеть весь хвост",
                    rows.len()
                )),
        );
    }
    column
}

/// The quota table: what every platform said about its own budget.
fn quota_table(rows: Vec<ProviderQuota>) -> impl IntoElement {
    let columns = quota_columns();
    let mut cells: Vec<gpui::AnyElement> = Vec::new();

    for row in &rows {
        let (label, tone) = state_label(&row.state);
        let remaining = match row.remaining {
            Some(value) => format!("{value:.1}%"),
            None => "—".to_string(),
        };
        let row_cells = vec![
            div()
                .text_size(px(13.0))
                .text_color(theme::text())
                .truncate()
                .child(row.provider.clone())
                .into_any_element(),
            widgets::compact_badge(
                if row.stale {
                    format!("{label} · устарело")
                } else {
                    label.to_string()
                },
                tone,
            ),
            widgets::number(remaining).into_any_element(),
            div()
                .text_size(px(12.0))
                .text_color(theme::muted())
                .child(if row.window.is_empty() {
                    "—".to_string()
                } else {
                    row.window.clone()
                })
                .into_any_element(),
            widgets::number(row.connections.to_string()).into_any_element(),
            div()
                .text_size(px(12.0))
                .text_color(theme::muted())
                .truncate()
                .child(row.words.clone())
                .into_any_element(),
        ];
        cells.push(widgets::defined_row(&columns, row_cells, 32.0));
    }

    let mut panel =
        widgets::table_panel(vec![widgets::defined_header(&columns).into_any_element()]);
    if cells.is_empty() {
        panel = panel.child(
            widgets::empty_state("площадки ещё не измерены: нажмите «Обновить сейчас»")
                .into_any_element(),
        );
    } else {
        panel = panel.children(cells);
    }
    panel
}

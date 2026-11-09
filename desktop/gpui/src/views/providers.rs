//! Providers — the provider connections and their credentials.
//!
//! Ported from `desktop/ui/src/views/Providers.svelte`: the same columns, the
//! same "only problems" filter, the same write endpoint
//! (`PATCH /api/connections/{id}`) and the same restart prompt the web view
//! shows when the active flag flipped.
//!
//! The gateway never sends a credential — only whether one is stored — and
//! this view never invents one either.

use std::time::Duration;

use gpui::prelude::*;
use gpui::{
    AnyElement, App, Context, CursorStyle, Entity, FontWeight, Hsla, IntoElement, Render,
    SharedString, Window, div, px, rgba,
};

use crate::api::{self, Connection, ConnectionsResponse, RestartResponse, UpdateConnectionBody};
use crate::theme;
use crate::widgets::{self, TextInput, Tone};

/// The subtitle under the page heading.
const SUBTITLE: &str = "Connections from the shared database. \"servable\" means a backend is built \
for that provider, which requires an active row with a credential and an executor for its \
format. Toggling activity needs a gateway restart; priority and name apply on the next read.";

/// The table columns, in the order of the Svelte view.
const COLUMNS: [&str; 10] = [
    "provider",
    "name",
    "auth",
    "credential",
    "test",
    "active",
    "priority",
    "servable",
    "expires",
    "last error",
];

/// The notice shown when a save flipped the active flag.
const RESTART_NOTICE: &str = "Saved. Connection activity changed — restart the gateway to apply it (backends are built at startup).";

/// The notice shown while the gateway is coming back.
const RESTART_WAITING: &str = "Restart requested — waiting for the gateway to come back…";

/// The response of `PATCH /api/connections/{id}`.
///
/// The gateway answers `{status, id, is_active, priority, restart_required}`;
/// `api.rs` carries no type for it, so the view keeps the one field it needs.
#[derive(Debug, Clone, serde::Deserialize)]
struct ConnectionUpdate {
    /// `true` when the active flag flipped — the gateway builds its backends
    /// at startup, so the change only lands after a restart.
    #[serde(default)]
    restart_required: bool,
}

/// The `/healthz` payload as the restart poller needs it.
///
/// `api::Health` carries the status and the backend only; the poll compares
/// `started_unix` to tell the new process from the one it asked to restart.
#[derive(Debug, Clone, serde::Deserialize)]
struct HealthProbe {
    /// `ok` when the gateway is serving.
    #[serde(default)]
    status: String,
    /// Unix time the serving process started.
    #[serde(default)]
    started_unix: u64,
}

/// The Providers section.
pub struct ProvidersView {
    /// The loaded payload.
    pub data: Option<ConnectionsResponse>,
    /// The last failure, rendered as a banner.
    pub error: Option<String>,
    /// Whether a request is in flight.
    pub loading: bool,
    /// The last successful action, rendered as a notice.
    notice: Option<String>,
    /// Show only the connections that need attention.
    only_problems: bool,
    /// The connection being saved right now.
    saving_id: Option<String>,
    /// The gateway must restart before the last change takes effect.
    restart_pending: bool,
    /// A restart is in flight.
    restarting: bool,
    /// The connection whose name is being edited.
    editing_id: Option<String>,
    /// The edited name.
    name_draft: String,
    /// The text field of the name editor.
    name_input: Entity<TextInput>,
}

impl ProvidersView {
    /// Build the view and load its data.
    pub fn new(cx: &mut Context<Self>) -> Self {
        let name_input = cx.new(|cx| TextInput::new(cx, ""));
        name_input.update(cx, |input, _| {
            input.set_placeholder("connection name");
        });

        let mut view = Self {
            data: None,
            error: None,
            loading: true,
            notice: None,
            only_problems: false,
            saving_id: None,
            restart_pending: false,
            restarting: false,
            editing_id: None,
            name_draft: String::new(),
            name_input,
        };

        // The name editor reports every keystroke back to the view.
        let weak = cx.weak_entity();
        view.name_input.update(cx, |input, _| {
            let changed = weak.clone();
            input.set_on_change(move |value, cx| {
                let _ = changed.update(cx, |view, cx| {
                    view.name_draft = value.to_string();
                    cx.notify();
                });
            });
            let submitted = weak.clone();
            input.set_on_submit(move |value, _window, cx| {
                let _ = submitted.update(cx, |view, cx| {
                    view.commit_name(value.to_string(), cx);
                });
            });
        });

        view.refresh(cx);
        view
    }

    /// Reload the connections from the gateway.
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        let Some(client) = api::session_client(cx) else {
            self.loading = false;
            return;
        };
        self.loading = true;
        api::spawn_request(
            cx,
            move || client.get_json::<ConnectionsResponse>("/api/connections"),
            |result, view: &mut ProvidersView, cx| {
                match result {
                    Ok(data) => {
                        // The gateway reports a broken connection list in the
                        // payload, not as a failed request.
                        view.error = data.connection_error.clone();
                        view.data = Some(data);
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

    /// Patch one connection.
    ///
    /// `restart_required` in the answer means the active flag flipped: the
    /// gateway builds its backends at startup, so the change waits for a
    /// restart.
    fn save(&mut self, id: &str, patch: UpdateConnectionBody, cx: &mut Context<Self>) {
        let Some(client) = api::session_client(cx) else {
            return;
        };
        let path = format!("/api/connections/{id}");
        self.saving_id = Some(id.to_string());
        self.error = None;
        self.notice = None;

        api::spawn_request(
            cx,
            move || client.patch_json::<ConnectionUpdate, _>(&path, &patch),
            |result, view: &mut ProvidersView, cx| {
                match result {
                    Ok(updated) => {
                        view.notice = Some(if updated.restart_required {
                            view.restart_pending = true;
                            RESTART_NOTICE.to_string()
                        } else {
                            "Saved.".to_string()
                        });
                        view.refresh(cx);
                    }
                    Err(error) => {
                        view.error = Some(error.to_string());
                        crate::app::report_error(cx, &error);
                    }
                }
                view.saving_id = None;
                cx.notify();
            },
        );
    }

    /// Flip the active flag of a connection.
    fn set_active(&mut self, id: &str, active: bool, cx: &mut Context<Self>) {
        self.save(
            id,
            UpdateConnectionBody {
                is_active: Some(active),
                ..Default::default()
            },
            cx,
        );
    }

    /// Move a connection one step up or down the dispatch order.
    fn set_priority(&mut self, id: &str, priority: i64, cx: &mut Context<Self>) {
        self.save(
            id,
            UpdateConnectionBody {
                priority: Some(priority),
                ..Default::default()
            },
            cx,
        );
    }

    /// Start editing the display name of a connection.
    fn start_editing(&mut self, id: &str, cx: &mut Context<Self>) {
        let Some(row) = self
            .data
            .as_ref()
            .and_then(|data| data.connections.iter().find(|row| row.id == id))
        else {
            return;
        };
        let name = row.name.clone();
        self.editing_id = Some(id.to_string());
        self.name_draft = name.clone();
        self.name_input.update(cx, |input, _| input.set_value(name));
        cx.notify();
    }

    /// Save the edited display name.
    fn commit_name(&mut self, name: String, cx: &mut Context<Self>) {
        let Some(id) = self.editing_id.clone() else {
            return;
        };
        self.editing_id = None;
        self.name_draft.clear();

        let name = name.trim().to_string();
        if name.is_empty() {
            self.error = Some("the connection name must not be empty".to_string());
            cx.notify();
            return;
        }

        self.save(
            &id,
            UpdateConnectionBody {
                name: Some(name),
                ..Default::default()
            },
            cx,
        );
    }

    /// Ask the gateway to restart and wait until it serves again.
    ///
    /// The gateway refuses the request with 409 when launchd does not
    /// supervise it; its message says so and is worth showing as is.
    fn restart_gateway(&mut self, cx: &mut Context<Self>) {
        let Some(client) = api::session_client(cx) else {
            return;
        };
        self.restarting = true;
        self.error = None;
        self.notice = Some(RESTART_WAITING.to_string());

        api::spawn_request(
            cx,
            move || {
                let before = client
                    .get_json::<HealthProbe>("/healthz")
                    .ok()
                    .map(|probe| probe.started_unix);

                client.post_json::<RestartResponse, _>("/api/restart", &serde_json::json!({}))?;

                // The gateway re-execs about two seconds after the 202, so the
                // poll waits for a process that did not exist before.
                for _ in 0..30 {
                    std::thread::sleep(Duration::from_secs(2));
                    if let Ok(probe) = client.get_json::<HealthProbe>("/healthz")
                        && probe.status == "ok"
                        && Some(probe.started_unix) != before
                    {
                        return Ok(true);
                    }
                }
                Ok(false)
            },
            |result, view: &mut ProvidersView, cx| {
                view.restarting = false;
                match result {
                    Ok(true) => {
                        view.notice = Some("Gateway restarted and serving.".to_string());
                        view.restart_pending = false;
                        view.refresh(cx);
                    }
                    Ok(false) => {
                        view.error = Some(
                            "Gateway did not come back within a minute — check the service."
                                .to_string(),
                        );
                    }
                    Err(error) => {
                        // The gateway's own words: "gateway is not supervised
                        // by launchd; restart it yourself" — show them as they
                        // are, the operator has to act on them.
                        view.notice = None;
                        view.error = Some(error.message.clone());
                        crate::app::report_error(cx, &error);
                    }
                }
                cx.notify();
            },
        );
    }

    /// The name cell: the display name, or the editor when it is open.
    fn render_name_cell(&self, row: &Connection, cx: &Context<Self>) -> AnyElement {
        if self.editing_id.as_deref() == Some(row.id.as_str()) {
            return self.render_name_editor(cx);
        }

        let view = cx.entity();
        let id = row.id.clone();
        let label = if row.name.is_empty() {
            "—".to_string()
        } else {
            row.name.clone()
        };

        div()
            .id(theme::element_id(format!("cell-name:{}", row.id)))
            .text_size(px(13.0))
            .text_color(theme::text())
            .cursor(CursorStyle::PointingHand)
            .on_click(move |_event, window, cx| {
                view.update(cx, |view, cx| view.start_editing(&id, cx));
                // The editor is open — put the caret in the field.
                let handle = {
                    let state = view.read(cx);
                    let input = state.name_input.read(cx);
                    input.focus_handle().clone()
                };
                window.focus(&handle);
            })
            .child(label)
            .into_any_element()
    }

    /// The name editor: the field plus save and cancel.
    fn render_name_editor(&self, cx: &Context<Self>) -> AnyElement {
        let view = cx.entity();

        let saver = view.clone();
        let save = small_button(
            "providers-name-save",
            "Save",
            true,
            move |_window: &mut Window, cx: &mut App| {
                saver.update(cx, |view, cx| {
                    let draft = view.name_draft.clone();
                    view.commit_name(draft, cx);
                });
            },
        );

        let canceller = view.clone();
        let cancel = small_button(
            "providers-name-cancel",
            "Cancel",
            true,
            move |_window: &mut Window, cx: &mut App| {
                canceller.update(cx, |view, cx| {
                    view.editing_id = None;
                    view.name_draft.clear();
                    cx.notify();
                });
            },
        );

        div()
            .flex()
            .items_center()
            .gap_1()
            .child(div().w(px(150.0)).child(self.name_input.clone()))
            .child(save)
            .child(cancel)
            .into_any_element()
    }

    /// One row of the connections table.
    fn render_row(&self, row: &Connection, cx: &Context<Self>) -> AnyElement {
        let saving = self.saving_id.as_deref() == Some(row.id.as_str());
        let view = cx.entity();

        // active — on/off, disabled while this row is being saved
        let active_row = view.clone();
        let active_id = row.id.clone();
        let active_button = small_button(
            format!("providers-active:{}", row.id),
            if row.is_active { "on" } else { "off" },
            !saving,
            move |_window: &mut Window, cx: &mut App| {
                active_row.update(cx, |view, cx| {
                    let Some(row) = view.connection(&active_id) else {
                        return;
                    };
                    let active = !row.is_active;
                    view.set_active(&active_id, active, cx);
                });
            },
        );

        // priority — one step down, the value, one step up
        let lower_row = view.clone();
        let lower_id = row.id.clone();
        let lower = small_button(
            format!("providers-priority-down:{}", row.id),
            "−",
            !saving,
            move |_window: &mut Window, cx: &mut App| {
                lower_row.update(cx, |view, cx| {
                    let Some(row) = view.connection(&lower_id) else {
                        return;
                    };
                    let priority = row.priority - 1;
                    view.set_priority(&lower_id, priority, cx);
                });
            },
        );

        let upper_row = view.clone();
        let upper_id = row.id.clone();
        let upper = small_button(
            format!("providers-priority-up:{}", row.id),
            "+",
            !saving,
            move |_window: &mut Window, cx: &mut App| {
                upper_row.update(cx, |view, cx| {
                    let Some(row) = view.connection(&upper_id) else {
                        return;
                    };
                    let priority = row.priority + 1;
                    view.set_priority(&upper_id, priority, cx);
                });
            },
        );

        let priority = div()
            .flex()
            .items_center()
            .gap_1()
            .child(lower)
            .child(widgets::number(row.priority.to_string()))
            .child(upper);

        let (credential_label, credential_tone) = credential_label(row);
        let test = row.test_status.clone().unwrap_or_default();
        let test_label = if test.is_empty() {
            "—".to_string()
        } else {
            test.clone()
        };

        let cells = vec![
            cell(widgets::mono(row.provider.clone())),
            self.render_name_cell(row, cx),
            cell(widgets::muted(dash(&row.auth_type))),
            cell(widgets::badge(credential_label, credential_tone)),
            cell(widgets::badge(test_label, test_tone(&test))),
            cell(active_button),
            cell(priority),
            cell(widgets::badge(
                if row.servable { "yes" } else { "no" },
                if row.servable { Tone::Ok } else { Tone::Bad },
            )),
            cell(mono_muted(dash(row.expires_at.as_deref().unwrap_or("")))),
            cell(widgets::muted(dash(
                row.last_error.as_deref().unwrap_or(""),
            ))),
        ];

        widgets::cells_row(cells).into_any_element()
    }

    /// The connection with that id, if the gateway still knows it.
    fn connection(&self, id: &str) -> Option<&Connection> {
        self.data
            .as_ref()
            .and_then(|data| data.connections.iter().find(|row| row.id == id))
    }
}

impl Render for ProvidersView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let mut page = div()
            .flex()
            .flex_col()
            .p(px(26.0))
            .gap_2()
            .child(widgets::page_header("Providers", SUBTITLE));

        if let Some(error) = &self.error {
            page = page.child(widgets::error_banner(error.clone()));
        }

        if let Some(notice) = &self.notice {
            page = page.child(widgets::notice_banner(notice.clone()));

            if self.restart_pending {
                let view = cx.entity();
                let restart = move |_window: &mut Window, cx: &mut App| {
                    view.update(cx, |view, cx| view.restart_gateway(cx));
                };
                page = page.child(div().flex().items_center().gap_2().child(
                    widgets::primary_button(
                        "providers-restart",
                        if self.restarting {
                            "restarting…"
                        } else {
                            "Restart now"
                        },
                        !self.restarting,
                        restart,
                    ),
                ));
            }
        }

        // the "only problems" filter
        let view = cx.entity();
        let toggle = move |_window: &mut Window, cx: &mut App| {
            view.update(cx, |view, cx| {
                view.only_problems = !view.only_problems;
                cx.notify();
            });
        };
        page = page.child(div().flex().items_center().gap_2().child(checkbox(
            "providers-only-problems",
            "only problems",
            self.only_problems,
            toggle,
        )));

        // the table — the Svelte view shows it only once the payload arrived
        match &self.data {
            Some(data) => {
                let mut rows: Vec<AnyElement> = vec![header_row(&COLUMNS)];
                let mut total = 0usize;
                let mut shown = 0usize;
                for row in &data.connections {
                    total += 1;
                    if self.only_problems && !needs_attention(row) {
                        continue;
                    }
                    shown += 1;
                    rows.push(self.render_row(row, cx));
                }
                page = page.child(widgets::table(rows));

                if shown == 0 {
                    page = page.child(if total == 0 {
                        widgets::empty_state("No provider connections yet.").into_any_element()
                    } else {
                        widgets::muted("All connections look healthy.").into_any_element()
                    });
                }
            }
            None => {
                if self.error.is_none() {
                    page = page.child(widgets::muted("loading…"));
                }
            }
        }

        page
    }
}

/// One table cell: its content in an equal-width column.
fn cell(content: impl IntoElement) -> AnyElement {
    div().flex_1().min_w_0().child(content).into_any_element()
}

/// The header row of the connections table.
///
/// `widgets::table_header` lays out three columns; this table has ten, so the
/// header is drawn with the style the shared widget uses.
fn header_row(columns: &[&str]) -> AnyElement {
    let cells: Vec<AnyElement> = columns
        .iter()
        .map(|column| {
            div()
                .flex_1()
                .min_w_0()
                .text_size(px(11.0))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(theme::muted())
                .child(column.to_string())
                .into_any_element()
        })
        .collect();
    widgets::cells_row(cells).into_any_element()
}

/// A small table button that goes dead while its row is saving.
fn small_button(
    key: impl Into<SharedString>,
    label: impl Into<SharedString>,
    enabled: bool,
    on_click: impl Fn(&mut Window, &mut App) + 'static,
) -> impl IntoElement {
    div()
        .id(theme::element_id(key))
        .flex()
        .items_center()
        .justify_center()
        .px(px(8.0))
        .py(px(3.0))
        .rounded(px(theme::RADIUS_SM))
        .border_1()
        .border_color(theme::border())
        .bg(theme::panel())
        .text_size(px(12.0))
        .text_color(theme::text())
        .opacity(if enabled { 1.0 } else { 0.55 })
        .when(enabled, |this| {
            this.cursor(CursorStyle::PointingHand)
                .on_click(move |_event, window, cx| on_click(window, cx))
        })
        .child(label.into())
}

/// A checkbox — gpui has none, so the view draws one.
fn checkbox(
    key: impl Into<SharedString>,
    label: impl Into<SharedString>,
    checked: bool,
    on_click: impl Fn(&mut Window, &mut App) + 'static,
) -> impl IntoElement {
    div()
        .id(theme::element_id(key))
        .flex()
        .items_center()
        .gap_2()
        .cursor(CursorStyle::PointingHand)
        .on_click(move |_event, window, cx| on_click(window, cx))
        .child(
            div()
                .flex()
                .items_center()
                .justify_center()
                .w(px(14.0))
                .h(px(14.0))
                .rounded(px(3.0))
                .border_1()
                .border_color(if checked {
                    theme::accent()
                } else {
                    theme::border()
                })
                .bg(if checked {
                    theme::accent()
                } else {
                    gpui::transparent_black()
                })
                .text_size(px(10.0))
                .text_color(Hsla::from(rgba(0x06101fff)))
                .child(if checked { "✓" } else { "" }),
        )
        .child(
            div()
                .text_size(px(13.0))
                .text_color(theme::muted())
                .child(label.into()),
        )
}

/// Monospaced muted text — the `td.mono.muted` of the CSS bundle.
fn mono_muted(text: impl Into<SharedString>) -> impl IntoElement {
    div()
        .font(gpui::font(theme::MONO_FAMILY))
        .text_size(px(12.0))
        .text_color(theme::muted())
        .child(text.into())
}

/// The em dash the Svelte view shows for an empty cell.
fn dash(value: &str) -> String {
    if value.is_empty() {
        "—".to_string()
    } else {
        value.to_string()
    }
}

/// Whether a connection needs the operator's attention.
///
/// The Svelte filter keeps the inactive rows, the rows without a credential,
/// the rows the gateway cannot serve and the rows with a last error.
fn needs_attention(row: &Connection) -> bool {
    !row.is_active
        || !row.has_credential
        || !row.servable
        || row
            .last_error
            .as_deref()
            .is_some_and(|error| !error.is_empty())
}

/// The credential label and its tone — never the credential itself.
fn credential_label(row: &Connection) -> (&'static str, Tone) {
    if row.has_credential {
        if row.auth_type == "oauth" {
            ("token", Tone::Ok)
        } else {
            ("key", Tone::Ok)
        }
    } else {
        ("missing", Tone::Bad)
    }
}

/// The tone of a credential test status.
fn test_tone(status: &str) -> Tone {
    match status.to_ascii_lowercase().as_str() {
        "ok" | "success" | "passed" | "valid" | "active" => Tone::Ok,
        "error" | "failed" | "failure" | "invalid" => Tone::Bad,
        "expired" | "stale" | "warning" => Tone::Warn,
        _ => Tone::Neutral,
    }
}

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
    AnyElement, App, Context, CursorStyle, Entity, Hsla, IntoElement, Render, SharedString, Window,
    div, px, rgba,
};

use crate::api::{
    self, Connection, ConnectionsResponse, CreateConnectionBody, CreateConnectionResponse,
    ProviderCatalog, ProviderInfo, RestartResponse, UpdateConnectionBody,
};
use crate::theme;
use crate::widgets::{self, ColumnAlign, ColumnDef, ColumnSizing, TextInput, Tone};

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

/// The height of one connection row.
const ROW_HEIGHT: f32 = 30.0;

/// The layout of the connections table.
///
/// The header and every row are drawn from this one description, so a column
/// cannot drift from its label. The narrow columns are fixed — a badge, a
/// button and a number need a known width — and the two free-text columns
/// take what is left: the name grows, the error message grows with a basis.
fn connection_columns() -> [ColumnDef; 10] {
    [
        ColumnDef {
            label: COLUMNS[0],
            sizing: ColumnSizing::Fixed(110),
            align: ColumnAlign::Left,
        },
        ColumnDef {
            label: COLUMNS[1],
            sizing: ColumnSizing::GrowBasis(180),
            align: ColumnAlign::Left,
        },
        ColumnDef {
            label: COLUMNS[2],
            sizing: ColumnSizing::Fixed(64),
            align: ColumnAlign::Left,
        },
        ColumnDef {
            label: COLUMNS[3],
            sizing: ColumnSizing::Fixed(72),
            align: ColumnAlign::Left,
        },
        ColumnDef {
            label: COLUMNS[4],
            sizing: ColumnSizing::Fixed(62),
            align: ColumnAlign::Left,
        },
        ColumnDef {
            label: COLUMNS[5],
            sizing: ColumnSizing::Fixed(60),
            align: ColumnAlign::Left,
        },
        ColumnDef {
            label: COLUMNS[6],
            sizing: ColumnSizing::Fixed(92),
            align: ColumnAlign::Left,
        },
        ColumnDef {
            label: COLUMNS[7],
            sizing: ColumnSizing::Fixed(62),
            align: ColumnAlign::Left,
        },
        ColumnDef {
            label: COLUMNS[8],
            sizing: ColumnSizing::Fixed(124),
            align: ColumnAlign::Left,
        },
        ColumnDef {
            label: COLUMNS[9],
            sizing: ColumnSizing::GrowBasis(150),
            align: ColumnAlign::Left,
        },
    ]
}

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

    /// Whether the "add provider" form is open.
    add_form_open: bool,
    /// The provider catalogue, loaded once when the form opens.
    catalog: Option<ProviderCatalog>,
    /// Whether the catalogue is being loaded.
    catalog_loading: bool,
    /// The last catalogue loading failure.
    catalog_error: Option<String>,
    /// The provider selected in the add form.
    selected_provider: Option<String>,
    /// The name of the new connection.
    new_connection_name: String,
    /// The credential of the new connection. Kept only while the form is open.
    new_api_key: String,
    /// The priority of the new connection, as typed.
    new_priority: String,
    /// Whether the new connection is being created.
    creating: bool,
    /// A validation or server error inside the add form.
    add_form_error: Option<String>,
    /// The name field of the add form.
    add_name_input: Entity<TextInput>,
    /// The key field of the add form.
    add_key_input: Entity<TextInput>,
    /// The priority field of the add form.
    add_priority_input: Entity<TextInput>,
    /// The search query over the provider catalogue.
    catalog_filter: String,
    /// The search field of the add form.
    catalog_filter_input: Entity<TextInput>,
}

impl ProvidersView {
    /// Build the view and load its data.
    pub fn new(cx: &mut Context<Self>) -> Self {
        let name_input = cx.new(|cx| TextInput::new(cx, ""));
        name_input.update(cx, |input, _| {
            input.set_placeholder("connection name");
        });

        let add_name_input = cx.new(|cx| {
            let mut input = TextInput::new(cx, "");
            input.set_placeholder("Connection name");
            input
        });
        let add_key_input = cx.new(|cx| {
            let mut input = TextInput::new(cx, "");
            input.set_placeholder("API key");
            input.set_password(true);
            input
        });
        let add_priority_input = cx.new(|cx| {
            let mut input = TextInput::new(cx, "");
            input.set_placeholder("optional");
            input
        });
        let catalog_filter_input = cx.new(|cx| {
            let mut input = TextInput::new(cx, "");
            input.set_placeholder("Search providers");
            input
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
            add_form_open: false,
            catalog: None,
            catalog_loading: false,
            catalog_error: None,
            selected_provider: None,
            new_connection_name: String::new(),
            new_api_key: String::new(),
            new_priority: String::new(),
            creating: false,
            add_form_error: None,
            add_name_input,
            add_key_input,
            add_priority_input,
            catalog_filter: String::new(),
            catalog_filter_input,
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

        // The add form fields report every keystroke back to the view.
        let weak_name = cx.weak_entity();
        view.add_name_input.update(cx, |input, _| {
            let changed = weak_name.clone();
            input.set_on_change(move |value, cx| {
                let _ = changed.update(cx, |view, cx| {
                    view.new_connection_name = value.to_string();
                    view.add_form_error = None;
                    cx.notify();
                });
            });
            let submitted = weak_name.clone();
            input.set_on_submit(move |_value, _window, cx| {
                let _ = submitted.update(cx, |view, cx| {
                    view.submit_add_connection(cx);
                });
            });
        });

        let weak_key = cx.weak_entity();
        view.add_key_input.update(cx, |input, _| {
            let changed = weak_key.clone();
            input.set_on_change(move |value, cx| {
                let _ = changed.update(cx, |view, cx| {
                    view.new_api_key = value.to_string();
                    view.add_form_error = None;
                    cx.notify();
                });
            });
            let submitted = weak_key.clone();
            input.set_on_submit(move |_value, _window, cx| {
                let _ = submitted.update(cx, |view, cx| {
                    view.submit_add_connection(cx);
                });
            });
        });

        let weak_priority = cx.weak_entity();
        view.add_priority_input.update(cx, |input, _| {
            let changed = weak_priority.clone();
            input.set_on_change(move |value, cx| {
                let _ = changed.update(cx, |view, cx| {
                    view.new_priority = value.to_string();
                    view.add_form_error = None;
                    cx.notify();
                });
            });
            let submitted = weak_priority.clone();
            input.set_on_submit(move |_value, _window, cx| {
                let _ = submitted.update(cx, |view, cx| {
                    view.submit_add_connection(cx);
                });
            });
        });

        // The catalogue holds hundreds of providers: the search field is what
        // makes one of them findable.
        let weak_filter = cx.weak_entity();
        view.catalog_filter_input.update(cx, |input, _| {
            let changed = weak_filter.clone();
            input.set_on_change(move |value, cx| {
                let _ = changed.update(cx, |view, cx| {
                    view.catalog_filter = value.to_string();
                    cx.notify();
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
            .w_full()
            .min_w_0()
            .text_size(px(13.0))
            .text_color(theme::text())
            .truncate()
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
        let save = widgets::compact_button(
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
        let cancel = widgets::compact_button(
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
        let active_button = widgets::compact_button(
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
        let lower = widgets::compact_button(
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
        let upper = widgets::compact_button(
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
            mono_truncated(row.provider.clone()).into_any_element(),
            self.render_name_cell(row, cx),
            muted_truncated(dash(&row.auth_type)).into_any_element(),
            widgets::compact_badge(credential_label, credential_tone).into_any_element(),
            widgets::compact_badge(test_label, test_tone(&test)).into_any_element(),
            active_button.into_any_element(),
            priority.into_any_element(),
            widgets::compact_badge(
                if row.servable { "yes" } else { "no" },
                if row.servable { Tone::Ok } else { Tone::Bad },
            )
            .into_any_element(),
            mono_muted(short_time(dash(row.expires_at.as_deref().unwrap_or(""))))
                .into_any_element(),
            muted_truncated(dash(row.last_error.as_deref().unwrap_or(""))).into_any_element(),
        ];

        widgets::defined_row(&connection_columns(), cells, ROW_HEIGHT)
    }

    /// The connection with that id, if the gateway still knows it.
    fn connection(&self, id: &str) -> Option<&Connection> {
        self.data
            .as_ref()
            .and_then(|data| data.connections.iter().find(|row| row.id == id))
    }

    /// Open or close the add-provider form.
    fn toggle_add_form(&mut self, cx: &mut Context<Self>) {
        if self.add_form_open {
            self.close_add_form(cx);
            return;
        }
        self.add_form_open = true;
        self.add_form_error = None;
        self.notice = None;
        self.error = None;
        if self.catalog.is_none() {
            self.load_catalog(cx);
        }
        cx.notify();
    }

    /// Close the form and clear every secret and draft value from memory.
    fn close_add_form(&mut self, cx: &mut Context<Self>) {
        self.add_form_open = false;
        self.add_form_error = None;
        self.creating = false;
        self.selected_provider = None;
        self.new_connection_name.clear();
        self.new_api_key.clear();
        self.new_priority.clear();
        self.catalog_filter.clear();
        self.add_name_input.update(cx, |input, _| input.clear());
        self.add_key_input.update(cx, |input, _| input.clear());
        self.add_priority_input.update(cx, |input, _| input.clear());
        self.catalog_filter_input
            .update(cx, |input, _| input.clear());
        cx.notify();
    }

    /// Load the provider catalogue once and keep it in memory.
    fn load_catalog(&mut self, cx: &mut Context<Self>) {
        let Some(client) = api::session_client(cx) else {
            self.catalog_error = Some("No active session.".to_string());
            return;
        };
        self.catalog_loading = true;
        self.catalog_error = None;
        api::spawn_request(
            cx,
            move || client.get_json::<ProviderCatalog>("/api/providers"),
            |result, view: &mut ProvidersView, cx| {
                match result {
                    Ok(catalog) => {
                        view.catalog = Some(catalog);
                        view.catalog_error = None;
                    }
                    Err(error) => {
                        view.catalog_error = Some(error.to_string());
                        crate::app::report_error(cx, &error);
                    }
                }
                view.catalog_loading = false;
                cx.notify();
            },
        );
    }

    /// Pick a provider for the new connection.
    fn select_provider(&mut self, id: String, cx: &mut Context<Self>) {
        self.selected_provider = Some(id.clone());
        self.add_form_error = None;

        // A keyless or OAuth provider does not take a key: drop anything typed.
        let needs_key = self
            .catalog
            .as_ref()
            .and_then(|catalog| catalog.providers.iter().find(|provider| provider.id == id))
            .map(|provider| provider.needs_key && provider.auth_type != "oauth")
            .unwrap_or(false);
        if !needs_key {
            self.new_api_key.clear();
            self.add_key_input.update(cx, |input, _| input.clear());
        }
        cx.notify();
    }

    /// The provider selected in the add form, if the catalogue still holds it.
    fn selected_provider_info(&self) -> Option<&ProviderInfo> {
        let id = self.selected_provider.as_deref()?;
        self.catalog
            .as_ref()
            .and_then(|catalog| catalog.providers.iter().find(|provider| provider.id == id))
    }

    /// Validate the form and create the connection.
    fn submit_add_connection(&mut self, cx: &mut Context<Self>) {
        if self.creating {
            return;
        }
        self.add_form_error = None;
        self.notice = None;
        self.error = None;

        let Some(provider_id) = self.selected_provider.clone() else {
            self.add_form_error = Some("Select a provider.".to_string());
            cx.notify();
            return;
        };
        let Some(info) = self.selected_provider_info().cloned() else {
            self.add_form_error = Some("The selected provider is unavailable.".to_string());
            cx.notify();
            return;
        };

        let name = self.new_connection_name.trim().to_string();
        if name.is_empty() {
            self.add_form_error = Some("Enter a connection name.".to_string());
            cx.notify();
            return;
        }

        if info.auth_type == "oauth" {
            self.add_form_error = Some(
                "OAuth connections must be configured manually. Adding a key through this form is not available."
                    .to_string(),
            );
            cx.notify();
            return;
        }

        let needs_key = info.needs_key;
        let api_key = self.new_api_key.trim().to_string();
        if needs_key && api_key.is_empty() {
            self.add_form_error = Some("Enter a key.".to_string());
            cx.notify();
            return;
        }

        let priority = if self.new_priority.trim().is_empty() {
            None
        } else {
            match self.new_priority.trim().parse::<i64>() {
                Ok(value) => Some(value),
                Err(_) => {
                    self.add_form_error = Some("Priority must be a whole number.".to_string());
                    cx.notify();
                    return;
                }
            }
        };

        let Some(client) = api::session_client(cx) else {
            self.add_form_error = Some("No active session.".to_string());
            cx.notify();
            return;
        };

        let created_active = true;
        let body = CreateConnectionBody {
            provider: provider_id,
            name,
            api_key: if needs_key { api_key } else { String::new() },
            priority,
            is_active: Some(created_active),
        };

        self.creating = true;
        cx.notify();

        api::spawn_request(
            cx,
            move || client.post_json::<CreateConnectionResponse, _>("/api/connections", &body),
            move |result, view: &mut ProvidersView, cx| {
                // The secret is never kept after the request finishes.
                view.new_api_key.clear();
                view.add_key_input.update(cx, |input, _| input.clear());

                match result {
                    Ok(created) => {
                        view.close_add_form(cx);
                        view.error = None;
                        view.restart_pending = created.restart_required || created_active;
                        view.notice = Some(if view.restart_pending {
                            "Provider added. Restart the gateway for the provider to start serving requests.".to_string()
                        } else {
                            "Provider added.".to_string()
                        });
                        view.refresh(cx);
                    }
                    Err(error) => {
                        // The message comes from the gateway's `error` field; show it as is.
                        view.add_form_error = Some(error.message.clone());
                        crate::app::report_error(cx, &error);
                    }
                }
                view.creating = false;
                cx.notify();
            },
        );
    }

    /// The "add provider" button and the form panel.
    fn render_add_section(&self, cx: &Context<Self>) -> AnyElement {
        let view = cx.entity();

        let toggle = {
            let view = view.clone();
            widgets::compact_button(
                "providers-add-toggle",
                if self.add_form_open {
                    "Hide form"
                } else {
                    "Add provider"
                },
                !self.creating,
                move |_window: &mut Window, cx: &mut App| {
                    view.update(cx, |view, cx| view.toggle_add_form(cx));
                },
            )
        };

        let mut section = div()
            .flex()
            .flex_col()
            .gap_2()
            // The button is content-sized: inside a column it would stretch
            // to the full width of the page.
            .child(div().flex().child(toggle));
        if self.add_form_open {
            section = section.child(self.render_add_form(cx));
        }
        section.into_any_element()
    }

    /// The form for creating a provider connection.
    fn render_add_form(&self, cx: &Context<Self>) -> AnyElement {
        let view = cx.entity();

        let mut form = div()
            .flex()
            .flex_col()
            .gap_2()
            .p(px(12.0))
            .border_1()
            .border_color(theme::border())
            .rounded(px(theme::RADIUS))
            .bg(theme::panel());

        if let Some(error) = &self.catalog_error {
            form = form.child(widgets::error_banner(error.clone()));
            let retry = {
                let view = view.clone();
                widgets::compact_button(
                    "providers-catalog-retry",
                    "Retry",
                    !self.catalog_loading,
                    move |_window: &mut Window, cx: &mut App| {
                        view.update(cx, |view, cx| view.load_catalog(cx));
                    },
                )
            };
            form = form.child(retry);
        } else if self.catalog_loading || self.catalog.is_none() {
            form = form.child(widgets::muted("Loading providers…"));
            return form.into_any_element();
        }

        let all_providers = self
            .catalog
            .as_ref()
            .map(|catalog| catalog.providers.as_slice())
            .unwrap_or(&[]);

        if all_providers.is_empty() {
            form = form.child(widgets::muted("No providers available."));
            return form.into_any_element();
        }

        // The search narrows the catalogue by name or id.
        let query = self.catalog_filter.trim();
        let providers: Vec<&ProviderInfo> = all_providers
            .iter()
            .filter(|provider| matches_query(provider, query))
            .collect();

        form = form.child(
            div()
                .text_size(px(13.0))
                .text_color(theme::text())
                .child("Provider"),
        );
        form = form.child(self.catalog_filter_input.clone());

        if providers.is_empty() {
            form = form.child(widgets::muted(
                "No provider matches the search. Clear it to see the whole catalogue.",
            ));
            return form.into_any_element();
        }

        let mut provider_rows: Vec<AnyElement> = Vec::new();
        for provider in providers {
            let id = provider.id.clone();
            let selected = self.selected_provider.as_deref() == Some(provider.id.as_str());
            let label = format!("{} ({})", provider.name, provider.id);
            let mut details = vec![provider.auth_type.clone(), provider.format.clone()];
            if provider.needs_key {
                details.push("needs key".to_string());
            } else {
                details.push("no key needed".to_string());
            }
            let details = details
                .into_iter()
                .filter(|part| !part.is_empty())
                .collect::<Vec<_>>()
                .join(" · ");
            let row_view = view.clone();
            provider_rows.push(
                div()
                    .id(theme::element_id(format!(
                        "provider-option:{}",
                        provider.id
                    )))
                    .flex()
                    .flex_col()
                    .gap_1()
                    .p(px(8.0))
                    .rounded(px(theme::RADIUS_SM))
                    .border_1()
                    .border_color(if selected {
                        theme::accent()
                    } else {
                        theme::border()
                    })
                    .bg(if selected {
                        theme::panel_2()
                    } else {
                        theme::panel()
                    })
                    .cursor(CursorStyle::PointingHand)
                    .on_click(move |_event, _window, cx| {
                        row_view.update(cx, |view, cx| view.select_provider(id.clone(), cx));
                    })
                    .child(
                        div()
                            .text_size(px(13.0))
                            .text_color(theme::text())
                            .child(label),
                    )
                    .child(widgets::muted(details))
                    .into_any_element(),
            );
        }

        form = form.child(
            div()
                .id(theme::element_id("providers-catalog-list"))
                .flex()
                .flex_col()
                .gap_1()
                .h(px(180.0))
                .overflow_y_scroll()
                .children(provider_rows),
        );

        if let Some(selected) = self.selected_provider_info() {
            form = form.child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .text_size(px(13.0))
                            .text_color(theme::text())
                            .child("Connection name"),
                    )
                    .child(self.add_name_input.clone()),
            );

            if selected.auth_type == "oauth" {
                form = form.child(widgets::notice_banner(
                    "OAuth connections must be configured manually. Adding a key through this form is not available."
                        .to_string(),
                ));
            } else if selected.needs_key {
                form = form.child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(
                            div()
                                .text_size(px(13.0))
                                .text_color(theme::text())
                                .child("API key"),
                        )
                        .child(self.add_key_input.clone()),
                );
            } else {
                form = form.child(widgets::muted("This provider does not need a key."));
            }

            form = form.child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .text_size(px(13.0))
                            .text_color(theme::text())
                            .child("Priority (optional)"),
                    )
                    .child(self.add_priority_input.clone()),
            );

            if let Some(error) = &self.add_form_error {
                form = form.child(widgets::error_banner(error.clone()));
            }

            let can_submit = !self.creating && selected.auth_type != "oauth";
            let save = {
                let view = view.clone();
                widgets::primary_button(
                    "providers-add-save",
                    if self.creating { "Saving…" } else { "Save" },
                    can_submit,
                    move |_window: &mut Window, cx: &mut App| {
                        view.update(cx, |view, cx| view.submit_add_connection(cx));
                    },
                )
            };
            let cancel = {
                let view = view.clone();
                widgets::compact_button(
                    "providers-add-cancel",
                    "Cancel",
                    !self.creating,
                    move |_window: &mut Window, cx: &mut App| {
                        view.update(cx, |view, cx| view.close_add_form(cx));
                    },
                )
            };

            form = form.child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(save)
                    .child(cancel),
            );
        } else {
            form = form.child(widgets::muted(
                "Select a provider from the catalog to fill in the connection.",
            ));
        }

        form.into_any_element()
    }
}

impl Render for ProvidersView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let mut page = div()
            .flex()
            .flex_col()
            .w_full()
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

        // the "add provider" form
        page = page.child(self.render_add_section(cx));

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
                let mut rows: Vec<AnyElement> =
                    vec![widgets::defined_header(&connection_columns())];
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

        // The page scrolls: a long connection list must stay reachable.
        div()
            .id(theme::element_id("providers-page"))
            .flex()
            .flex_col()
            .size_full()
            .min_h(px(0.0))
            .overflow_y_scroll()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .w_full()
                    .p(px(26.0))
                    .pb(px(40.0))
                    .child(page),
            )
    }
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
///
/// A long value (an error message, an expiry stamp) is cut with an ellipsis
/// instead of stretching the column.
fn mono_muted(text: impl Into<SharedString>) -> impl IntoElement {
    div()
        .w_full()
        .min_w_0()
        .font(gpui::font(theme::MONO_FAMILY))
        .text_size(px(12.0))
        .text_color(theme::muted())
        .truncate()
        .child(text.into())
}

/// Monospaced text that is cut with an ellipsis when the column is too narrow.
fn mono_truncated(text: impl Into<SharedString>) -> impl IntoElement {
    div()
        .w_full()
        .min_w_0()
        .font(gpui::font(theme::MONO_FAMILY))
        .text_size(px(12.0))
        .text_color(theme::text())
        .truncate()
        .child(text.into())
}

/// Muted text that is cut with an ellipsis when the column is too narrow.
fn muted_truncated(text: impl Into<SharedString>) -> impl IntoElement {
    div()
        .w_full()
        .min_w_0()
        .text_size(px(13.0))
        .text_color(theme::muted())
        .truncate()
        .child(text.into())
}

/// Whether a catalogue entry matches the search query.
///
/// An empty query matches everything. The query is matched against the id and
/// the display name, case-insensitively — the id is what the operator knows
/// from the registry, the name is what the catalogue shows.
fn matches_query(provider: &ProviderInfo, query: &str) -> bool {
    let query = query.trim().to_lowercase();
    if query.is_empty() {
        return true;
    }
    provider.id.to_lowercase().contains(&query) || provider.name.to_lowercase().contains(&query)
}

/// The em dash the Svelte view shows for an empty cell.
fn dash(value: &str) -> String {
    if value.is_empty() {
        "—".to_string()
    } else {
        value.to_string()
    }
}

/// A timestamp in the column's own format: `2026-09-17 00:30`.
///
/// The gateway sends the full ISO stamp; the seconds and the fraction are
/// noise in a column this narrow, and the date plus the hour is what the
/// operator reads.
fn short_time(value: String) -> String {
    if value == "—" {
        return value;
    }
    let Some((date, time)) = value.split_once('T') else {
        return value;
    };
    if date.len() < 10 || time.len() < 5 {
        return value;
    }
    format!("{} {}", date, &time[..5])
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

#[cfg(test)]
mod tests {
    use super::*;

    fn provider(id: &str, name: &str) -> ProviderInfo {
        ProviderInfo {
            id: id.to_string(),
            name: name.to_string(),
            auth_type: "apikey".to_string(),
            format: "openai".to_string(),
            needs_key: true,
            base_url: "https://example.invalid/v1".to_string(),
        }
    }

    #[test]
    fn an_empty_query_matches_every_provider() {
        let gemini = provider("gemini", "Gemini");
        assert!(matches_query(&gemini, ""));
        assert!(matches_query(&gemini, "   "));
    }

    #[test]
    fn the_query_matches_the_id_and_the_name() {
        let gemini = provider("gemini", "Google Gemini");
        assert!(matches_query(&gemini, "gemini"));
        assert!(matches_query(&gemini, "GEMINI"));
        assert!(matches_query(&gemini, "google"));
        assert!(!matches_query(&gemini, "openai"));
    }
}

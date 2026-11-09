//! Keys — The API keys, masked to their prefix.
//!
//! Ported from `desktop/ui/src/views/Keys.svelte`. The only secret the
//! gateway ever hands out is the full key of a freshly issued API key
//! (`POST /api/keys`). It is shown exactly once, in a panel of its own, and
//! it never reaches a log, the credential file or an error message.

use gpui::prelude::*;
use gpui::{
    AnyElement, App, ClipboardItem, Context, Entity, FontWeight, IntoElement, Render, SharedString,
    Window, div, px,
};

use crate::api::{self, ApiKey, CreateKeyBody, CreatedKey, KeyStateResponse, KeysResponse};
use crate::theme;
use crate::widgets::{self, TextInput};

/// The columns of the key table.
const COLUMNS: [&str; 6] = ["name", "prefix", "status", "created", "last used", ""];

/// The longest name the gateway accepts.
const MAX_NAME_LEN: usize = 64;

/// The Keys section.
pub struct KeysView {
    /// The loaded payload.
    pub data: Option<KeysResponse>,
    /// The last failure, rendered as a banner.
    pub error: Option<String>,
    /// Whether a request is in flight.
    pub loading: bool,
    /// The name of the key being issued.
    pub draft_name: String,
    /// The freshly issued key — the only place the full secret ever exists.
    pub fresh: Option<CreatedKey>,
    /// A confirmation message, rendered as a notice banner.
    pub notice: Option<String>,
    /// The key waiting for a revocation confirmation: `(id, name)`.
    pub pending_revoke: Option<(String, String)>,
    /// Whether a mutating request is in flight.
    pub busy: bool,
    /// The name field of the issue form.
    name_input: Entity<TextInput>,
}

impl KeysView {
    /// Build the view and load its data.
    pub fn new(cx: &mut Context<Self>) -> Self {
        let this = cx.weak_entity();
        let name_input = cx.new(|cx| TextInput::new(cx, ""));

        let on_change = this.clone();
        let on_submit = this.clone();
        name_input.update(cx, |input, _| {
            input.set_placeholder("e.g. laptop");
            input.set_on_change(move |value, cx| {
                let _ = on_change.update(cx, |view, cx| {
                    view.draft_name = value.to_string();
                    cx.notify();
                });
            });
            input.set_on_submit(move |_value, _window, cx| {
                let _ = on_submit.update(cx, |view, cx| view.create(cx));
            });
        });

        let mut view = Self {
            data: None,
            error: None,
            loading: true,
            draft_name: String::new(),
            fresh: None,
            notice: None,
            pending_revoke: None,
            busy: false,
            name_input,
        };
        view.refresh(cx);
        view
    }

    /// Reload the data from the gateway.
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        let Some(client) = api::session_client(cx) else {
            return;
        };
        self.loading = true;
        api::spawn_request(
            cx,
            move || client.get_json::<KeysResponse>("/api/keys"),
            |result, view: &mut KeysView, cx| {
                match result {
                    Ok(data) => {
                        // The gateway answers 200 with an `error` field when
                        // the rows could not be read at all.
                        view.error = data.error.clone();
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

    /// Issue a new key. The secret comes back with this one response.
    fn create(&mut self, cx: &mut Context<Self>) {
        let name = self.draft_name.trim().to_string();
        if name.is_empty() {
            return;
        }
        if name.chars().count() > MAX_NAME_LEN {
            self.error = Some(format!("name must be at most {MAX_NAME_LEN} characters"));
            self.notice = None;
            cx.notify();
            return;
        }
        let Some(client) = api::session_client(cx) else {
            return;
        };
        self.busy = true;
        self.error = None;
        self.notice = None;
        api::spawn_request(
            cx,
            move || {
                let body = CreateKeyBody { name };
                client.post_json::<CreatedKey, CreateKeyBody>("/api/keys", &body)
            },
            |result, view: &mut KeysView, cx| {
                match result {
                    Ok(created) => {
                        // The one copy of the secret: it stays in `fresh`
                        // until the operator dismisses it.
                        view.fresh = Some(created);
                        view.draft_name.clear();
                        view.name_input.update(cx, |input, _| input.clear());
                        view.refresh(cx);
                    }
                    Err(error) => {
                        view.error = Some(error.to_string());
                        crate::app::report_error(cx, &error);
                    }
                }
                view.busy = false;
                cx.notify();
            },
        );
    }

    /// Ask before revoking. gpui has no modal dialog, so the question is
    /// asked in place, right above the table.
    fn request_revoke(&mut self, id: String, name: String, cx: &mut Context<Self>) {
        self.pending_revoke = Some((id, name));
        cx.notify();
    }

    /// Revoke or restore a key.
    fn set_revoked(&mut self, id: String, revoke: bool, cx: &mut Context<Self>) {
        let Some(client) = api::session_client(cx) else {
            return;
        };
        self.busy = true;
        self.error = None;
        self.notice = None;
        // A state change drops the one-time secret, like the Svelte view.
        self.fresh = None;
        self.pending_revoke = None;
        let path = format!(
            "/api/keys/{id}/{}",
            if revoke { "revoke" } else { "restore" }
        );
        api::spawn_request(
            cx,
            move || client.post_empty::<KeyStateResponse>(&path),
            move |result, view: &mut KeysView, cx| {
                match result {
                    Ok(updated) => {
                        view.notice = Some(if revoke {
                            format!(
                                "Key revoked at {}. It already stopped working.",
                                updated.revoked_at.unwrap_or_else(|| "now".to_string())
                            )
                        } else {
                            "Key restored.".to_string()
                        });
                        view.refresh(cx);
                    }
                    Err(error) => {
                        view.error = Some(error.to_string());
                        crate::app::report_error(cx, &error);
                    }
                }
                view.busy = false;
                cx.notify();
            },
        );
    }

    /// Copy the one-time secret to the clipboard.
    fn copy_secret(&mut self, cx: &mut Context<Self>) {
        let Some(created) = self.fresh.as_ref() else {
            return;
        };
        let secret = created.key.clone();
        let app = api::app_mut(cx);
        app.write_to_clipboard(ClipboardItem::new_string(secret.clone()));
        // gpui cannot report a refused clipboard, so read it back.
        let copied = app.read_from_clipboard().and_then(|item| item.text()) == Some(secret);
        if copied {
            self.error = None;
            self.notice = Some("Copied to clipboard.".to_string());
        } else {
            self.notice = None;
            self.error =
                Some("Clipboard blocked — select the key and copy it manually.".to_string());
        }
        cx.notify();
    }

    /// Drop the one-time secret from the screen.
    fn dismiss_secret(&mut self, cx: &mut Context<Self>) {
        self.fresh = None;
        cx.notify();
    }
}

impl Render for KeysView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let this = cx.entity();

        // The issue form.
        let can_issue = !self.busy && !self.draft_name.trim().is_empty();
        let issue = {
            let this = this.clone();
            widgets::primary_button(
                "keys-issue",
                "Issue key",
                can_issue,
                move |_: &mut Window, cx: &mut App| {
                    this.update(cx, |view, cx| view.create(cx));
                },
            )
        };
        let form = div()
            .flex()
            .items_center()
            .gap(px(8.0))
            .mb(px(14.0))
            .child(
                div()
                    .text_size(px(13.0))
                    .text_color(theme::text())
                    .child("new key name"),
            )
            .child(div().w(px(240.0)).child(self.name_input.clone()))
            .child(issue);

        // The table of keys.
        let table = match self.data.as_ref() {
            None => {
                if self.loading && self.error.is_none() {
                    widgets::muted("loading…").into_any_element()
                } else {
                    div().into_any_element()
                }
            }
            Some(data) => {
                let mut rows: Vec<AnyElement> = vec![header_row(&COLUMNS)];
                for key in &data.keys {
                    rows.push(key_row(key, &this));
                }
                let panel = widgets::table(rows);
                if data.keys.is_empty() {
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(8.0))
                        .child(panel)
                        .child(widgets::empty_state(
                            "No keys yet — issue the first one above.",
                        ))
                        .into_any_element()
                } else {
                    panel.into_any_element()
                }
            }
        };

        let mut page = div().flex().flex_col().p(px(26.0)).gap(px(14.0));
        page = page.child(widgets::page_header(
            "Keys",
            "Operator keys for this gateway. Only prefixes are listed — the full secret is shown once, right after creation. Revocation applies immediately, no restart needed.",
        ));
        if let Some(message) = &self.error {
            page = page.child(widgets::error_banner(message.clone()));
        }
        if let Some(message) = &self.notice {
            page = page.child(widgets::notice_banner(message.clone()));
        }
        if let Some(created) = self.fresh.as_ref() {
            page = page.child(secret_panel(created, &this));
        }
        if let Some((id, name)) = self.pending_revoke.as_ref() {
            page = page.child(confirm_panel(id, name, &this, self.busy));
        }
        page.child(form).child(table)
    }
}

/// The one-time panel: the freshly issued secret, shown once.
fn secret_panel(created: &CreatedKey, this: &Entity<KeysView>) -> AnyElement {
    let warning = if created.warning.trim().is_empty() {
        "Copy the key now — the gateway never shows it again.".to_string()
    } else {
        created.warning.clone()
    };
    let title = format!("New key {} ({}…)", created.name, created.prefix);

    let copy = {
        let this = this.clone();
        widgets::primary_button(
            "keys-copy",
            "Copy",
            true,
            move |_: &mut Window, cx: &mut App| {
                this.update(cx, |view, cx| view.copy_secret(cx));
            },
        )
    };
    let dismiss = {
        let this = this.clone();
        widgets::button(
            "keys-stored",
            "I stored it",
            move |_: &mut Window, cx: &mut App| {
                this.update(cx, |view, cx| view.dismiss_secret(cx));
            },
        )
    };

    div()
        .flex()
        .flex_col()
        .gap(px(10.0))
        .p(px(9.0))
        .px(px(12.0))
        .rounded(px(theme::RADIUS))
        .border_1()
        .border_color(theme::ok())
        .bg(theme::ok_faint())
        .child(
            div()
                .flex()
                .flex_col()
                .gap_1()
                .child(
                    div()
                        .text_size(px(13.0))
                        .text_color(theme::text())
                        .child(title),
                )
                .child(
                    div()
                        .text_size(px(12.0))
                        .text_color(theme::muted())
                        .child(warning),
                )
                .child(
                    div()
                        .text_size(px(12.0))
                        .text_color(theme::muted())
                        .child(
                            "Shown once. The shell keeps the secret in memory only — it is never written to the keychain, to a file or to a log.",
                        ),
                ),
        )
        .child(
            div()
                .w_full()
                .p(px(12.0))
                .rounded(px(theme::RADIUS))
                .border_1()
                .border_color(theme::border())
                .bg(theme::panel())
                .child(
                    div()
                        .font(gpui::font(theme::MONO_FAMILY))
                        .text_size(px(13.0))
                        .text_color(theme::text())
                        .whitespace_normal()
                        .child(created.key.clone()),
                ),
        )
        .child(div().flex().gap(px(8.0)).child(copy).child(dismiss))
        .into_any_element()
}

/// The revocation confirmation, asked in place.
fn confirm_panel(id: &str, name: &str, this: &Entity<KeysView>, busy: bool) -> AnyElement {
    let question = format!("Revoke key \"{name}\"? Clients using it get 401 immediately.");

    let revoke = {
        let this = this.clone();
        let id = id.to_string();
        widgets::primary_button(
            "keys-revoke",
            "Revoke",
            !busy,
            move |_: &mut Window, cx: &mut App| {
                this.update(cx, |view, cx| view.set_revoked(id.clone(), true, cx));
            },
        )
    };
    let cancel = {
        let this = this.clone();
        widgets::button(
            "keys-cancel",
            "Cancel",
            move |_: &mut Window, cx: &mut App| {
                this.update(cx, |view, cx| {
                    view.pending_revoke = None;
                    cx.notify();
                });
            },
        )
    };

    div()
        .flex()
        .flex_col()
        .gap(px(8.0))
        .p(px(9.0))
        .px(px(12.0))
        .rounded(px(theme::RADIUS))
        .border_1()
        .border_color(theme::warn())
        .bg(theme::panel())
        .child(
            div()
                .text_size(px(13.0))
                .text_color(theme::text())
                .child(question),
        )
        .child(div().flex().gap(px(8.0)).child(revoke).child(cancel))
        .into_any_element()
}

/// The header row of the key table.
fn header_row(columns: &[&'static str]) -> AnyElement {
    let cells: Vec<AnyElement> = columns
        .iter()
        .map(|label| {
            div()
                .flex_1()
                .text_size(px(11.0))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(theme::muted())
                .child(*label)
                .into_any_element()
        })
        .collect();
    widgets::cells_row(cells).into_any_element()
}

/// One row of the key table.
fn key_row(key: &ApiKey, this: &Entity<KeysView>) -> AnyElement {
    let actions = if key.encrypted {
        div().into_any_element()
    } else if key.revoked {
        let this = this.clone();
        let id = key.id.clone();
        widgets::button(
            "keys-restore",
            "restore",
            move |_: &mut Window, cx: &mut App| {
                this.update(cx, |view, cx| {
                    if view.busy {
                        return;
                    }
                    view.set_revoked(id.clone(), false, cx);
                });
            },
        )
        .into_any_element()
    } else {
        let this = this.clone();
        let id = key.id.clone();
        let name = key.name.clone();
        widgets::button(
            "keys-revoke-row",
            "revoke",
            move |_: &mut Window, cx: &mut App| {
                this.update(cx, |view, cx| {
                    if view.busy {
                        return;
                    }
                    view.request_revoke(id.clone(), name.clone(), cx);
                });
            },
        )
        .into_any_element()
    };

    widgets::cells_row(vec![
        cell(key.name.clone()),
        mono_cell(prefix_label(key)),
        status_cell(key),
        stamp_cell(short_time(key.created_at.as_deref())),
        stamp_cell(short_time(key.last_used_at.as_deref())),
        div().flex_1().child(actions).into_any_element(),
    ])
    .into_any_element()
}

/// The prefix column: the masked prefix, or the at-rest marker.
fn prefix_label(key: &ApiKey) -> String {
    if key.encrypted {
        "enc:v1…".to_string()
    } else if key.prefix.is_empty() {
        "…".to_string()
    } else {
        format!("{}…", key.prefix)
    }
}

/// The status column: the badge of the Svelte view, plus the timestamps it
/// has no column of its own for.
fn status_cell(key: &ApiKey) -> AnyElement {
    let (label, tone) = if key.revoked {
        ("revoked", widgets::Tone::Bad)
    } else if key.encrypted {
        ("JS-only", widgets::Tone::Warn)
    } else if !key.is_active {
        ("off", widgets::Tone::Neutral)
    } else {
        ("live", widgets::Tone::Ok)
    };

    let mut cell = div()
        .flex_1()
        .flex_col()
        .items_start()
        .gap_1()
        .child(widgets::badge(label, tone));
    if let Some(revoked_at) = key.revoked_at.as_deref() {
        cell = cell.child(muted_line(format!(
            "revoked {}",
            short_time(Some(revoked_at))
        )));
    }
    if let Some(expires_at) = key.expires_at.as_deref() {
        cell = cell.child(muted_line(format!(
            "expires {}",
            short_time(Some(expires_at))
        )));
    }
    cell.into_any_element()
}

/// A plain table cell.
fn cell(text: impl Into<SharedString>) -> AnyElement {
    div()
        .flex_1()
        .text_size(px(13.0))
        .text_color(theme::text())
        .child(text.into())
        .into_any_element()
}

/// A monospaced table cell — the masked prefix.
fn mono_cell(text: impl Into<SharedString>) -> AnyElement {
    div()
        .flex_1()
        .font(gpui::font(theme::MONO_FAMILY))
        .text_size(px(12.0))
        .text_color(theme::text())
        .child(text.into())
        .into_any_element()
}

/// A muted monospaced table cell — a timestamp.
fn stamp_cell(text: impl Into<SharedString>) -> AnyElement {
    div()
        .flex_1()
        .font(gpui::font(theme::MONO_FAMILY))
        .text_size(px(12.0))
        .text_color(theme::muted())
        .child(text.into())
        .into_any_element()
}

/// A small muted line under a badge.
fn muted_line(text: impl Into<SharedString>) -> AnyElement {
    div()
        .text_size(px(11.0))
        .text_color(theme::muted())
        .child(text.into())
        .into_any_element()
}

/// The timestamp format of the Svelte view: `2026-10-28 12:00:00`.
fn short_time(value: Option<&str>) -> String {
    let raw = value.unwrap_or_default().replace('T', " ");
    // Drop a fractional-seconds tail (`…:00.123Z`), keep everything else.
    match raw.rfind('.') {
        Some(index) => {
            let tail = &raw[index + 1..];
            let digits = tail.strip_suffix('Z').unwrap_or(tail);
            if !digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit()) {
                raw[..index].to_string()
            } else {
                raw
            }
        }
        None => raw,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timestamps_drop_the_fractional_tail() {
        assert_eq!(
            short_time(Some("2026-10-28T12:00:00.123Z")),
            "2026-10-28 12:00:00"
        );
        assert_eq!(
            short_time(Some("2026-10-28T12:00:00.123")),
            "2026-10-28 12:00:00"
        );
        assert_eq!(
            short_time(Some("2026-10-28T12:00:00Z")),
            "2026-10-28 12:00:00Z"
        );
        assert_eq!(short_time(None), "");
    }

    #[test]
    fn an_encrypted_key_shows_the_at_rest_marker() {
        let key = ApiKey {
            id: "k1".into(),
            name: "legacy".into(),
            prefix: "sk-live".into(),
            encrypted: true,
            created_at: None,
            revoked_at: None,
            expires_at: None,
            last_used_at: None,
            is_active: true,
            revoked: false,
        };
        assert_eq!(prefix_label(&key), "enc:v1…");
    }
}

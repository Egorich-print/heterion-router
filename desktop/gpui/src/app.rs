//! The application shell: sidebar, section routing and the sign-in screen.
//!
//! The shell owns the navigation and the session; every section is its own
//! view entity so it can keep its state (filters, drafts, scroll position)
//! while the operator moves around.

use std::collections::HashMap;

use gpui::prelude::*;
use gpui::{AnyElement, AnyView, App, Context, Entity, FontWeight, Render, Window, div, px, size};

use crate::api::{self, ApiClient, ApiError, Health, Session};
use crate::theme;
use crate::views;

/// A section of the dashboard.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Section {
    /// Gateway identity and row counts.
    Overview,
    /// Routing combos and their entries.
    Combos,
    /// Provider connections.
    Providers,
    /// API keys.
    Keys,
    /// Token and latency usage.
    Usage,
    /// Call log and application log.
    Logs,
    /// Ad-hoc chat completion playground.
    Playground,
}

impl Section {
    /// Every section, in sidebar order.
    pub const ALL: [Section; 7] = [
        Section::Overview,
        Section::Combos,
        Section::Providers,
        Section::Keys,
        Section::Usage,
        Section::Logs,
        Section::Playground,
    ];

    /// The label shown in the sidebar.
    pub fn label(self) -> &'static str {
        match self {
            Section::Overview => "Overview",
            Section::Combos => "Combos",
            Section::Providers => "Providers",
            Section::Keys => "Keys",
            Section::Usage => "Usage",
            Section::Logs => "Logs",
            Section::Playground => "Playground",
        }
    }
}

/// The root view of the desktop app.
pub struct AppShell {
    section: Section,
    views: HashMap<Section, AnyView>,
    key_input: Entity<crate::widgets::TextInput>,
    health: Option<Health>,
    banner: Option<String>,
}

impl AppShell {
    /// Build the shell. A stored operator key signs the operator straight in;
    /// otherwise nothing is fetched until they sign in.
    pub fn new(cx: &mut Context<Self>) -> Self {
        let key_input = cx.new(|cx| crate::widgets::TextInput::new(cx, ""));
        key_input.update(cx, |input, _| {
            input.set_placeholder("sk-…");
            input.set_password(true);
        });

        let mut shell = Self {
            section: Section::Overview,
            views: HashMap::new(),
            key_input,
            health: None,
            banner: None,
        };

        if let Some(key) = crate::secret::load() {
            cx.update_global::<Session, _>(|session, _| session.sign_in(key));
            shell.check_health(cx);
        }

        shell
    }

    /// The view of the active section, created on first use.
    fn section_view(&mut self, section: Section, cx: &mut Context<Self>) -> AnyView {
        if let Some(view) = self.views.get(&section) {
            return view.clone();
        }
        let view: AnyView = match section {
            Section::Overview => cx.new(views::overview::OverviewView::new).into(),
            Section::Combos => cx.new(views::combos::CombosView::new).into(),
            Section::Providers => cx.new(views::providers::ProvidersView::new).into(),
            Section::Keys => cx.new(views::keys::KeysView::new).into(),
            Section::Usage => cx.new(views::usage::UsageView::new).into(),
            Section::Logs => cx.new(views::logs::LogsView::new).into(),
            Section::Playground => cx.new(views::playground::PlaygroundView::new).into(),
        };
        self.views.insert(section, view.clone());
        view
    }

    /// Ask the gateway whether it is alive. Health needs no key.
    fn check_health(&mut self, cx: &mut Context<Self>) {
        let base = api::app_mut(cx).global::<Session>().base().to_string();
        let client = ApiClient::new(base, "");
        api::spawn_request(
            cx,
            move || client.get_json::<Health>("/healthz"),
            |result, shell: &mut AppShell, cx| match result {
                Ok(health) => {
                    shell.health = Some(health);
                    cx.notify();
                }
                Err(error) => {
                    shell.health = None;
                    shell.banner = Some(format!("gateway unreachable: {error}"));
                    cx.notify();
                }
            },
        );
    }

    /// Store the operator key and open the dashboard.
    fn sign_in(&mut self, cx: &mut Context<Self>) {
        let key = cx.read_entity(&self.key_input, |input, _app| input.value().to_string());
        if key.trim().is_empty() {
            return;
        }

        if let Err(error) = crate::secret::store(key.trim()) {
            // A failed credential write is not fatal: the operator can still
            // work in this session, they just have to paste the key again
            // after a restart.
            self.banner = Some(format!("could not store the key: {error}"));
        }

        cx.update_global::<Session, _>(|session, _| session.sign_in(key.trim()));
        self.key_input.update(cx, |input, _| input.clear());
        self.check_health(cx);
        cx.notify();
    }

    /// Forget the operator key and return to the sign-in screen.
    fn sign_out(&mut self, cx: &mut Context<Self>) {
        let _ = crate::secret::clear();
        cx.update_global::<Session, _>(|session, _| session.sign_out());
        self.views.clear();
        self.section = Section::Overview;
        self.banner = None;
        cx.notify();
    }

    /// Render the sign-in screen.
    fn render_login(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let shell = cx.entity();
        let on_submit = move |_: &mut Window, cx: &mut App| {
            shell.update(cx, |shell, cx| shell.sign_in(cx));
        };

        div()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .size_full()
            .bg(theme::bg())
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .w(px(420.0))
                    .p(px(22.0))
                    .bg(theme::panel())
                    .border_1()
                    .border_color(theme::border())
                    .rounded(px(theme::RADIUS))
                    .child(
                        div()
                            .text_size(px(15.0))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme::text())
                            .child("Heterion Router"),
                    )
                    .child(
                        div()
                            .text_size(px(13.0))
                            .text_color(theme::muted())
                            .child(
                                "Paste an API key from api_keys. It is stored on this machine and sent as a bearer token.",
                            ),
                    )
                    .child(self.key_input.clone())
                    .child(crate::widgets::primary_button(
                        "app-open-dashboard",
                        "Open dashboard",
                        true,
                        on_submit,
                    )),
            )
            .into_any_element()
    }

    /// Render the sidebar.
    fn render_sidebar(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let health_label = match &self.health {
            Some(health) => format!("{} · {}", health.backend, health.status),
            None => "checking…".to_string(),
        };

        let mut items: Vec<AnyElement> = Vec::new();
        for section in Section::ALL {
            let active = section == self.section;
            let shell = cx.entity();
            let label = section.label();
            items.push(
                crate::widgets::nav_item(label, active, move |_: &mut Window, cx: &mut App| {
                    shell.update(cx, |shell, cx| {
                        shell.section = section;
                        cx.notify();
                    });
                })
                .into_any_element(),
            );
        }

        let shell = cx.entity();
        let sign_out = move |_: &mut Window, cx: &mut App| {
            shell.update(cx, |shell, cx| shell.sign_out(cx));
        };

        div()
            .flex()
            .flex_col()
            .gap_1()
            .w(px(theme::SIDEBAR_WIDTH))
            .h_full()
            .flex_shrink_0()
            .bg(theme::panel())
            .border_r_1()
            .border_color(theme::border())
            .p(px(16.0))
            .px(px(12.0))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .px(px(10.0))
                    .pt(px(4.0))
                    .pb(px(14.0))
                    .text_color(theme::text())
                    .text_size(px(15.0))
                    .font_weight(FontWeight::SEMIBOLD)
                    .child("Heterion Router")
                    .child(
                        div()
                            .text_size(px(11.0))
                            .font_weight(FontWeight::NORMAL)
                            .text_color(theme::muted())
                            .child(health_label),
                    ),
            )
            .children(items)
            .child(div().flex_1())
            .child(crate::widgets::nav_item("Sign out", false, sign_out))
            .into_any_element()
    }

    /// Render the active section.
    fn render_content(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let view = self.section_view(self.section, cx);
        div()
            .flex_1()
            .h_full()
            .min_w_0()
            .id(theme::element_id("app-content"))
            .overflow_y_scroll()
            .bg(theme::bg())
            .child(view)
            .into_any_element()
    }
}

impl Render for AppShell {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let authenticated = api::app_mut(cx).global::<Session>().is_authenticated();

        if !authenticated {
            return self.render_login(cx);
        }

        let banner = self.banner.clone();
        div()
            .flex()
            .size_full()
            .bg(theme::bg())
            .child(self.render_sidebar(cx))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(match banner {
                        Some(message) => crate::widgets::error_banner(message).into_any_element(),
                        None => div().into_any_element(),
                    })
                    .child(self.render_content(cx)),
            )
            .into_any_element()
    }
}

/// Default window size of the desktop app.
pub const WINDOW_WIDTH: f32 = 1280.0;
/// Default window height of the desktop app.
pub const WINDOW_HEIGHT: f32 = 860.0;

/// Compute the default window bounds, centered on the primary display.
pub fn default_bounds(cx: &mut App) -> gpui::WindowBounds {
    gpui::WindowBounds::Windowed(gpui::Bounds::centered(
        None,
        size(px(WINDOW_WIDTH), px(WINDOW_HEIGHT)),
        cx,
    ))
}

/// Report an error from a view back to the shell: authentication failures
/// drop the session, everything else is logged.
pub fn report_error<T>(cx: &mut Context<'_, T>, error: &ApiError) {
    if theme::is_auth_error(error) {
        cx.update_global::<Session, _>(|session, _| session.sign_out());
        return;
    }
    tracing::warn!(%error, "api request failed");
}

//! Overview — gateway identity, live backends and row counts.
//!
//! Ported from `desktop/ui/src/views/Overview.svelte`: the same heading, the
//! same four metric cards, the same "Live backends" list and the same wording.

use gpui::prelude::*;
use gpui::{App, Context, Entity, IntoElement, Render, SharedString, Window, div, px};

use crate::api::{self, Overview};
use crate::theme;
use crate::widgets::{self, Tone};

/// The overview section.
pub struct OverviewView {
    /// The loaded payload.
    pub data: Option<Overview>,
    /// The last failure, rendered as a banner.
    pub error: Option<String>,
    /// Whether a request is in flight.
    pub loading: bool,
    /// The number of the request that is in flight.
    ///
    /// Every refresh bumps it. A reply that belongs to an older request is
    /// dropped: two clicks in a row must not show the older answer last.
    request: u64,
}

impl OverviewView {
    /// Build the view and load its data.
    pub fn new(cx: &mut Context<Self>) -> Self {
        let mut view = Self {
            data: None,
            error: None,
            loading: true,
            request: 0,
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
        self.request += 1;
        let request = self.request;
        api::spawn_request(
            cx,
            move || client.get_json::<Overview>("/api/overview"),
            move |result, view: &mut OverviewView, cx| {
                // A reply for a request that has been superseded must not
                // overwrite the newer one.
                if view.request != request {
                    return;
                }
                match result {
                    Ok(data) => {
                        view.data = Some(data);
                        view.error = None;
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
}

/// The action of the Refresh button.
///
/// It re-queries `/api/overview` and redraws the view, so the "combos" card
/// shows the current number of active combos. It is a function of its own so
/// that a test can run exactly what the button runs.
pub(crate) fn refresh_action(entity: Entity<OverviewView>) -> impl Fn(&mut App) + 'static {
    move |cx: &mut App| {
        entity.update(cx, |view, cx| {
            view.refresh(cx);
            cx.notify();
        });
    }
}

/// Pick the singular or plural form for `count`.
fn pluralize(count: usize, singular: &str, plural: &str) -> String {
    if count == 1 {
        singular.to_string()
    } else {
        plural.to_string()
    }
}

/// A non-interactive chip sized like a secondary button, so it sits evenly
/// next to action buttons in the same row.
fn info_chip(label: impl Into<SharedString>) -> impl IntoElement {
    div()
        .flex()
        .items_center()
        .px(px(12.0))
        .py(px(6.0))
        .rounded(px(theme::RADIUS_SM))
        .border_1()
        .border_color(theme::border())
        .bg(theme::panel())
        .text_size(px(13.0))
        .text_color(theme::text())
        .child(label.into())
}

impl Render for OverviewView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let entity = cx.entity();
        let refresh = refresh_action(entity);

        // The page, inside the content area: the content padding of the CSS
        // bundle (`.content { padding: 22px 26px }`).
        let mut root = div()
            .flex()
            .flex_col()
            .p(px(theme::CONTENT_PADDING_X))
            .gap(px(16.0));

        if let Some(error) = self.error.clone() {
            // `error_banner` carries the margin of the shell's content column;
            // this view pads itself, so cancel that margin again.
            root = root.child(
                div()
                    .mx(px(-theme::CONTENT_PADDING_X))
                    .mt(px(-theme::CONTENT_PADDING_Y))
                    .child(widgets::error_banner(error)),
            );
        }

        // The scrollable page.
        //
        // `items_start` keeps the content column at its own height: gpui takes
        // the scroll range from the child's box, so a stretched child would
        // hide everything below the fold.
        let page = div()
            .flex()
            .flex_col()
            .flex_1()
            .w_full()
            .id(theme::element_id("overview-page"))
            .overflow_y_scroll()
            .items_start();

        let mut content = div().flex().flex_col().w_full().gap(px(16.0));

        // The heading, the name of the backend the gateway runs and the
        // refresh button. The button re-queries `/api/overview`, so the
        // "combos" card below shows the current number of active combos.
        let mut actions = div().flex().items_center().gap(px(8.0)).flex_none();
        if let Some(data) = self.data.as_ref() {
            actions = actions
                .child(widgets::muted("gateway"))
                .child(info_chip(data.backend.clone()));
        }
        actions = actions.child(widgets::button(
            "overview-refresh",
            "Refresh",
            move |_: &mut Window, cx: &mut App| refresh(cx),
        ));

        content = content.child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .gap(px(16.0))
                .child(div().flex_1().min_w_0().child(widgets::page_header(
                    "Overview",
                    "Which providers are live and what the gateway can serve right now.",
                )))
                .child(actions),
        );

        // A request in flight never hides the data we already have.
        if self.loading {
            content = content.child(widgets::muted("loading…"));
        }

        let Some(data) = self.data.as_ref() else {
            if !self.loading && self.error.is_none() {
                content = content.child(widgets::empty_state("no overview data"));
            }
            return root.child(page.child(content));
        };

        // The metric cards — the same four, in the same order, as the Svelte
        // view.
        let counts = &data.counts;
        let live = data.backends.len();
        let cards = div()
            .flex()
            .flex_wrap()
            .gap(px(12.0))
            .child(stat(
                pluralize(live, "live backend", "live backends"),
                live.to_string(),
            ))
            .child(stat(
                pluralize(counts.combos, "combo", "combos"),
                counts.combos.to_string(),
            ))
            .child(stat(
                "connections active",
                format!("{}/{}", counts.connections_active, counts.connections_total),
            ))
            .child(stat(
                pluralize(counts.api_keys, "API key", "API keys"),
                counts.api_keys.to_string(),
            ));
        content = content.child(cards);

        content = content.child(widgets::section_title("Live backends"));
        content = content.child(widgets::muted(
            "A provider lands here only when it has an active connection with a usable credential (or needs none) and an executor for its wire format.",
        ));

        if data.backends.is_empty() {
            content = content.child(widgets::empty_state("no live backends"));
        } else {
            let mut tags = div().flex().flex_wrap().items_center().gap(px(8.0));
            for backend in &data.backends {
                tags = tags.child(widgets::badge(backend.clone(), Tone::Ok));
            }
            content = content.child(tags);
        }

        root.child(page.child(content))
    }
}

/// One metric card that shares its row evenly, like the `minmax(150px, 1fr)`
/// grid of the CSS bundle.
fn stat(label: impl Into<SharedString>, value: String) -> impl IntoElement {
    div()
        .flex_1()
        .min_w(px(150.0))
        .child(widgets::stat_card(label, value))
}

#[cfg(test)]
mod tests {
    use super::pluralize;

    #[test]
    fn counters_use_the_singular_form_for_one() {
        assert_eq!(pluralize(1, "API key", "API keys"), "API key");
        assert_eq!(pluralize(1, "combo", "combos"), "combo");
        assert_eq!(
            pluralize(1, "live backend", "live backends"),
            "live backend"
        );
    }

    #[test]
    fn counters_use_the_plural_form_for_zero_and_many() {
        assert_eq!(pluralize(0, "API key", "API keys"), "API keys");
        assert_eq!(pluralize(2, "API key", "API keys"), "API keys");
        assert_eq!(pluralize(15, "combo", "combos"), "combos");
    }
}

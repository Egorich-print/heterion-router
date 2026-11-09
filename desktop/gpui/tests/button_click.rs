//! The click path of the shared widgets.
//!
//! The QA harness drives the app from the outside with synthetic mouse
//! events, and those events depend on the app being the frontmost process.
//! That is fragile, so the widgets themselves are checked here instead: a
//! headless gpui window renders the real widget, the element's rectangle is
//! looked up by its debug selector, and a click is simulated at the centre of
//! that rectangle.
//!
//! The click goes through gpui's own hit-testing — the same path a real
//! mouse click takes — so this test fails if `on_click` is wired wrongly, if
//! the element is not interactive, or if something above it swallows the
//! event.

#![allow(dead_code)]
#![allow(unused_imports)]

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

#[path = "../src/api.rs"]
mod api;
#[path = "../src/theme.rs"]
mod theme;
#[path = "../src/widgets/mod.rs"]
mod widgets;

use gpui::prelude::*;
use gpui::{App, Context, IntoElement, Modifiers, Render, TestAppContext, Window, div, px};

/// The debug selector of a button, as a `'static` key.
///
/// gpui's `debug_bounds` takes a `&'static str`; the widget stores a name
/// built from its label, so the test leaks one copy of it. A test leaks a
/// handful of bytes on purpose — that is cheaper than inventing a registry.
fn selector(name: String) -> &'static str {
    Box::leak(name.into_boxed_str())
}

/// A window that shows one button and counts the clicks it gets.
struct ButtonHost {
    clicks: Arc<AtomicUsize>,
}

impl Render for ButtonHost {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let clicks = self.clicks.clone();
        div().flex().p(px(20.0)).child(widgets::button(
            "test-refresh",
            "Refresh",
            move |_window: &mut Window, _cx: &mut App| {
                clicks.fetch_add(1, Ordering::SeqCst);
            },
        ))
    }
}

/// The control case: a plain gpui div with `on_click`, written the way gpui's
/// own examples write it. If this one does not fire either, the problem is
/// the test's use of the framework, not the widget.
struct PlainHost {
    clicks: Arc<AtomicUsize>,
}

impl Render for PlainHost {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let clicks = self.clicks.clone();
        div().flex().p(px(20.0)).child(
            div()
                .id(gpui::ElementId::named_usize("plain-button", 0))
                .bg(gpui::rgb(0x202020))
                .px(px(12.0))
                .py(px(6.0))
                .on_click(move |_event, _window, _cx| {
                    clicks.fetch_add(1, Ordering::SeqCst);
                })
                .child("plain"),
        )
    }
}

/// The control case: a click reaches a plain gpui element.
#[gpui::test]
fn a_plain_gpui_element_answers_a_click(cx: &mut TestAppContext) {
    let clicks = Arc::new(AtomicUsize::new(0));

    let (_view, cx) = cx.add_window_view(|_window, _cx| PlainHost {
        clicks: clicks.clone(),
    });
    cx.run_until_parked();

    // The plain element has no debug selector, so its position is derived
    // from the window: the button sits in the top-left corner.
    cx.simulate_click(gpui::Point::new(px(30.0), px(30.0)), Modifiers::none());

    assert_eq!(
        clicks.load(Ordering::SeqCst),
        1,
        "a plain gpui element must answer a click"
    );
}

/// A click on the button reaches its handler.
#[gpui::test]
fn a_button_answers_a_click(cx: &mut TestAppContext) {
    let clicks = Arc::new(AtomicUsize::new(0));

    let (_view, cx) = cx.add_window_view(|_window, _cx| ButtonHost {
        clicks: clicks.clone(),
    });
    cx.run_until_parked();

    let bounds = cx
        .debug_bounds(selector(widgets::button_selector("test-refresh")))
        .expect("the button must be findable by its selector");
    assert!(
        bounds.size.width > px(0.0) && bounds.size.height > px(0.0),
        "the button must have a real rectangle"
    );

    cx.simulate_click(bounds.center(), Modifiers::none());

    assert_eq!(
        clicks.load(Ordering::SeqCst),
        1,
        "the click must reach the button's handler"
    );
}

/// A click that misses the button must not reach it.
#[gpui::test]
fn a_click_beside_the_button_reaches_nothing(cx: &mut TestAppContext) {
    let clicks = Arc::new(AtomicUsize::new(0));

    let (_view, cx) = cx.add_window_view(|_window, _cx| ButtonHost {
        clicks: clicks.clone(),
    });
    cx.run_until_parked();

    let bounds = cx
        .debug_bounds(selector(widgets::button_selector("test-refresh")))
        .expect("the button must be findable by its selector");

    // Far below the button, inside the same window.
    cx.simulate_click(
        gpui::Point::new(bounds.origin.x, bounds.origin.y + px(200.0)),
        Modifiers::none(),
    );

    assert_eq!(
        clicks.load(Ordering::SeqCst),
        0,
        "a click outside the button must not fire it"
    );
}

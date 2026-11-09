//! Dead-code warnings are expected here: the palette is a shared
//! surface — a view that does not use a colour today still may.
#![allow(dead_code)]

//! Palette and metrics for the native dashboard.
//!
//! Every color mirrors a CSS custom property of the Svelte bundle
//! (`desktop/ui/src/app.css`), so the GPUI shell and the web dashboard stay
//! the same product instead of two look-alikes.

use gpui::{ElementId, Hsla, SharedString, rgb, rgba};

/// Convert a packed `0xRRGGBB` literal into an opaque [`Hsla`].
fn color(hex: u32) -> Hsla {
    rgb(hex).into()
}

/// Page background (`--bg`).
pub fn bg() -> Hsla {
    color(0x0d1117)
}

/// Panel background (`--panel`).
pub fn panel() -> Hsla {
    color(0x161b22)
}

/// Raised panel background (`--panel-2`).
pub fn panel_2() -> Hsla {
    color(0x1c2430)
}

/// Border color (`--border`).
pub fn border() -> Hsla {
    color(0x2a3341)
}

/// Primary text (`--text`).
pub fn text() -> Hsla {
    color(0xe6edf3)
}

/// Muted text (`--muted`).
pub fn muted() -> Hsla {
    color(0x8b98a9)
}

/// Accent (`--accent`).
pub fn accent() -> Hsla {
    color(0x4f8cff)
}

/// Success (`--ok`).
pub fn ok() -> Hsla {
    color(0x3fb950)
}

/// Warning (`--warn`).
pub fn warn() -> Hsla {
    color(0xd29922)
}

/// Failure (`--bad`).
pub fn bad() -> Hsla {
    color(0xf85149)
}

/// The accent at low opacity, for banners and chart tracks.
pub fn accent_faint() -> Hsla {
    rgba(0x4f8cff33).into()
}

/// The failure color at low opacity, for error banners.
pub fn bad_faint() -> Hsla {
    rgba(0xf8514924).into()
}

/// The success color at low opacity, for notice banners.
pub fn ok_faint() -> Hsla {
    rgba(0x3fb9501f).into()
}

/// Sidebar width in pixels (`.sidebar` is `208px` wide in the CSS).
pub const SIDEBAR_WIDTH: f32 = 208.0;
/// Content padding around the active view (`.content` is `22px 26px`).
pub const CONTENT_PADDING_X: f32 = 26.0;
/// Content padding around the active view (`.content` is `22px 26px`).
pub const CONTENT_PADDING_Y: f32 = 22.0;
/// Corner radius of cards and panels.
pub const RADIUS: f32 = 8.0;
/// Corner radius of small controls.
pub const RADIUS_SM: f32 = 6.0;

/// Monospace family for ids, key prefixes and code.
pub const MONO_FAMILY: &str = "SF Mono";
/// Fallback monospace family, used when the first one is unavailable.
pub const MONO_FALLBACK: &str = "Menlo";

/// `true` when the failure was an authentication rejection, i.e. the operator
/// key has to be entered again.
pub fn is_auth_error(error: &crate::api::ApiError) -> bool {
    matches!(error.status, 401 | 403)
}

/// A stable element id.
///
/// gpui keeps the interactive state of an element — the pending mouse-down,
/// the hover state, the focus — **under the element's id**, and it looks that
/// state up again on the next frame. An id that changes with every render
/// therefore loses the state between the mouse-down and the mouse-up, and the
/// click never arrives.
///
/// So every id in this crate is built from a name that means the same thing
/// on every frame. Where several instances of one widget live in a list, the
/// name carries the row's key: `element_id(format!("providers-toggle-{id}"))`.
pub fn element_id(name: impl Into<SharedString>) -> ElementId {
    ElementId::named_usize(name, 0)
}

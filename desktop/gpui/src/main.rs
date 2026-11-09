//! Heterion Router desktop shell — GPUI.
//!
//! The Tauri v2 shell hosted a WebView with the Svelte bundle. This is the
//! native replacement: a GPUI application that speaks the same admin HTTP
//! API as the web dashboard, so both frontends stay pinned to one contract.
//!
//! Production is self-contained: the shell spawns a gateway of its own and
//! talks to it over loopback. In development the shell talks to an already
//! running gateway instead.

mod api;
mod app;
mod secret;
mod sidecar;
mod theme;
mod views;
mod widgets;

use gpui::{App, AppContext, Application, TitlebarOptions, WindowOptions};

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .with_writer(std::io::stderr)
        .init();

    // Only one shell may own the sidecar port; a second launch must not spawn
    // a duplicate gateway that then fails with `AddrInUse`.
    let Some(_instance) = sidecar::acquire_instance_lock()? else {
        tracing::warn!("another Heterion Router shell is already running");
        return Ok(());
    };

    // Release builds carry their own gateway. Debug builds talk to the
    // gateway the operator already runs.
    let _sidecar = match sidecar::spawn_if_owned() {
        Ok(sidecar) => sidecar,
        Err(error) => {
            tracing::warn!(%error, "could not start the bundled gateway");
            None
        }
    };

    let base = sidecar::base_url();

    Application::new().run(move |cx: &mut App| {
        cx.set_global(api::Session::new(base.clone()));

        let bounds = app::default_bounds(cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(bounds),
                titlebar: Some(TitlebarOptions {
                    title: Some("Heterion Router".into()),
                    appears_transparent: false,
                    traffic_light_position: None,
                }),
                app_id: Some("com.heterion.router".into()),
                ..Default::default()
            },
            |_, cx| cx.new(app::AppShell::new),
        )
        .ok();

        cx.activate(true);
    });

    Ok(())
}

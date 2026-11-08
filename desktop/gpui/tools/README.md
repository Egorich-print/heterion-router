# Visual-QA harness — GPUI desktop shell

The desktop shell is a native [GPUI](https://github.com/zed-industries/gpui)
application, so a reviewer cannot tell whether a change rendered: there is no
WebView to inspect and the only feedback is a compiler and a crash log. These
scripts close that loop — they launch the shell, wait for its window, capture
the screen and report what they found.

```
screenshot.sh     launch / shot / stop / status
window_probe.sh   is there a real window? is the gateway alive?
harness.env       shared paths, ports and helpers (sourced, not run)
shots/            captured PNGs (runtime artefacts, not tracked)
```

Everything here is macOS only and needs nothing installed: `bash`,
`osascript`, `screencapture`, `sips` and `curl`, all part of the base system.
No network access, no `sudo`, no credentials on disk.

## Usage

```sh
./desktop/gpui/tools/screenshot.sh launch      # start the app, wait for its window
./desktop/gpui/tools/screenshot.sh shot login  # -> tools/shots/login.png
./desktop/gpui/tools/screenshot.sh status      # app, window, gateway, log tail
./desktop/gpui/tools/screenshot.sh stop        # terminate the app and its sidecar
./desktop/gpui/tools/window_probe.sh           # window + gateway, one screen
```

`desktop/scripts/gpui-screenshot.sh` is a thin wrapper around the same script,
for the people who look in `desktop/scripts/` first.

The app must be built first:

```sh
cargo build --manifest-path desktop/gpui/Cargo.toml
```

`launch` says so and exits `4` when the binary is missing.

A **debug** build talks to the gateway that already runs (port `20128`);
a **release** build owns its sidecar (port `20129`). The harness mirrors
`desktop/gpui/src/sidecar.rs` and honours `HETERION_ROUTER_PORT`,
`OMNIROUTE_RUST_PORT` and `HETERION_ROUTER_SIDECAR_PORT`. A debug launch
without a running gateway still starts — the shell then shows "gateway
unreachable" — so `launch` reports the gateway state but does not fail on it.

### Environment

| Variable | Meaning |
| --- | --- |
| `HETERION_DESKTOP_BIN` | binary under test (default: `target/debug/heterion-router-desktop`) |
| `HETERION_DESKTOP_PROFILE` | `debug` (default) or `release` |
| `HETERION_DESKTOP_LAUNCH_TIMEOUT` | seconds to wait for the window (default `30`) |
| `HETERION_ROUTER_KEY` | operator key — only used to ask the gateway whether it accepts it |

## Exit codes

| Code | Meaning |
| --- | --- |
| `0` | success (also: "already running", "nothing to stop") |
| `1` | usage error |
| `2` | the app is not running |
| `3` | no window appeared, or the capture failed |
| `4` | the app binary is missing (not built) |
| `5` | the gateway does not answer on the configured port |

`status` exits `2` when the app is down and `3` when it runs without a
window, so a reviewer can script on it.

## What the harness does — and what it cannot

* **It proves a window exists, not what is inside it.** The probe asks the
  window server (`System Events`) for the window count, the title and the
  geometry, and reports `gui: yes` only when the process is registered as a
  real GUI application. A process that runs but never opens a window — the
  headless zombie — is reported as `window: NONE`.
* **The capture is a screen crop, not a window capture.** This macOS build
  does not expose `AXWindowNumber`, so `screencapture -l <window id>` cannot
  be used. The harness raises the app, captures the whole screen and cuts the
  window rectangle out of it with `sips`. Anything that overlaps the window
  (a menu, a notification, a Dock tooltip) ends up in the picture. The raise
  can also be refused — the harness checks that the app really is the
  frontmost window and warns on stderr when it is not.
* **Only the main display is captured.** A window on a second display is
  cropped from the main one and comes out wrong.
* **No accessibility-tree dump.** There is no way to read the widgets inside
  the window, so "the login form is drawn" and "the Overview table has rows"
  both come down to: *a window with this title and this size is on screen*.
  For the contents, look at the PNG.
* **The harness cannot sign in.** A fresh launch shows the login screen; the
  operator key in `HETERION_ROUTER_KEY` is only checked against the gateway
  (over `curl -K -`, so the key never reaches a command line and is never
  written to disk). Typing the key into the window would push a credential
  through the accessibility keystroke path, so a human pastes it and the
  harness takes the shot.
* **Permissions decide what works.** The app running the scripts needs Screen
  Recording permission for `screencapture` to see window content, and
  Automation permission for the `System Events` queries. Without them the
  harness says "no window" rather than guessing.
* **Only the app this harness started is ever killed.** `stop` reads the pid
  from `tools/app.pid` and kills exactly that process plus the gateway it
  spawned (a child of it, matched by name). No `pkill`, no pattern over the
  process table — a second copy of the app (for example the bundle in
  `desktop/gpui/dist/`) is left alone.
* **One window at a time.** The shell takes a single-instance lock on port
  `20130`; when another copy of the app already owns the window, the harness
  says so instead of reporting a silent failure.

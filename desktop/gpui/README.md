# Heterion Router desktop shell (GPUI)

The native macOS shell of the router. It replaces the retired Tauri v2 shell:
Tauri v2 hosted a WebView that loaded the Svelte bundle, this crate is a
plain gpui application with no WebView and no HTML.

There are two frontends and one contract:

| Frontend | Where | How it reaches the gateway |
|---|---|---|
| Web dashboard | `desktop/ui/` — a Svelte bundle the **gateway itself serves** (`crates/heterion-router-gateway/src/ui.rs`, routed at `/`) | HTTP to the admin API |
| Native shell | `desktop/gpui/` — this crate | HTTP to the same admin API |

The admin API is defined by `crates/heterion-router-gateway/src/admin.rs` and
mirrored in Rust by `desktop/gpui/src/api.rs`. Both frontends speak it, so a
change to the contract has to land in the gateway, in `src/api.rs` and in both
frontends — the native shell and the web dashboard are not allowed to drift
apart.

The native shell is a **separate application**. It is not served by the
gateway and it does not embed the Svelte bundle; it only borrows the contract.

## Layout of the crate

```
desktop/gpui/
├── Cargo.toml            crate manifest — edition 2024, rust-version 1.99
├── src/
│   ├── main.rs           process start: instance lock, sidecar, gpui window
│   ├── app.rs            the shell: sidebar, section routing, sign-in screen
│   ├── api.rs            the typed admin-API contract + the session global
│   ├── theme.rs          the palette and the metrics
│   ├── secret.rs         the operator-key file
│   ├── sidecar.rs        the gateway the shell talks to — and the one it owns
│   ├── views/            one module per dashboard section
│   └── widgets/          the shared widgets (gpui has none of its own)
├── packaging/            Info.plist and the icon set for the .app bundle
├── tools/                the visual-QA harness (see below)
└── dist/                 the assembled bundle (build output)
```

| Path | What it is |
|---|---|
| `src/api.rs` | The typed mirror of the gateway's admin API: one struct per response, one per request body, plus `ApiClient`, the `Session` global and `api::spawn_request`. **The contract** — a change here is a change to both frontends. |
| `src/theme.rs` | The palette (`bg`, `panel`, `border`, `text`, `muted`, `accent`, `ok`, `warn`, `bad` and the faint variants) and the metrics (`SIDEBAR_WIDTH`, `RADIUS`, …). Every colour mirrors a CSS custom property of `desktop/ui/src/app.css`, so both frontends look the same. Also `theme::unique_id`, the element-id source. |
| `src/widgets/` | `controls.rs` (buttons, badges, tags, cards, banners, empty states), `input.rs` (`TextInput`, an entity), `table.rs` (table panel, header, rows, cells), `chart.rs` (`bar_chart`). gpui ships layout primitives, not widgets — there is no `Button`, `Input` or `Table` in the crate. |
| `src/views/` | One module per section: `overview.rs`, `combos.rs`, `providers.rs`, `keys.rs`, `usage.rs`, `logs.rs`, `playground.rs`. Each is an entity that owns its state and loads its own data. |
| `src/sidecar.rs` | Port resolution, the single-instance lock and the bundled gateway. A release build spawns a gateway of its own; a debug build only talks to one. |
| `src/secret.rs` | Where the operator key lives between runs and how it is protected. |
| `tools/` | The visual-QA harness: `screenshot.sh`, `window_probe.sh`, `harness.env` and the MCP server under `tools/mcp/`. |

## Build

```sh
cargo build --manifest-path desktop/gpui/Cargo.toml
```

* A cold build of the gpui tree takes about **two minutes** on this machine.
* **Xcode is required.** gpui renders through its own Metal renderer and
  compiles its shaders at build time with `xcrun metal`.
* **macOS only.** The crate is excluded from the workspace
  (`Cargo.toml`, `exclude`), so `cargo fmt --all` and
  `cargo clippy --workspace` do not cover it. Check it on its own:

  ```sh
  cargo fmt --manifest-path desktop/gpui/Cargo.toml -- --check
  cargo clippy --manifest-path desktop/gpui/Cargo.toml -- -D warnings
  ```

  The CI job `desktop-fmt` runs the first of the two.

The binary lands in `desktop/gpui/target/debug/heterion-router-desktop`
(release: `.../release/…`).

## Run against a gateway

A **debug** build does not start a gateway — it talks to one that is already
running. The port comes from the gateway's own variable, so the shell and the
gateway agree by construction:

| Build | Port | Environment |
|---|---|---|
| debug | `HETERION_ROUTER_PORT` → `OMNIROUTE_RUST_PORT` → **20129** | the gateway's own default |
| release | `HETERION_ROUTER_SIDECAR_PORT` → `OMNIROUTE_SIDECAR_PORT` → **20129** | the gateway the bundle carries, next to the app executable |

The launchd service on this machine listens on **20128**, so a debug build
reaches it like this:

```sh
HETERION_ROUTER_PORT=20128 cargo run --manifest-path desktop/gpui/Cargo.toml
```

Without the variable the debug build looks for a gateway on 20129 and shows
"gateway unreachable" when nothing answers there.

A release build carries its own gateway: the bundle ships
`heterion-router-gateway` next to the app executable, the shell spawns it on
the sidecar port and the gateway exits by itself once the parent is gone
(`HETERION_ROUTER_PARENT_PID`).

Only one shell may run at a time. The instance lock is a bound TCP listener on
`127.0.0.1:20130`; a second copy logs "another Heterion Router shell is
already running" and exits.

## Package

```sh
desktop/scripts/build-app.sh
```

The script builds both binaries (the shell and the gateway sidecar), assembles
`desktop/gpui/dist/Heterion Router.app`, signs it **ad-hoc** and verifies the
signature and the bundle metadata. The bundle is **not notarized** — it runs on
the machine that built it.

* `--no-build` skips the two cargo builds and reuses the binaries already in
  the target directories. Use it when only the bundle metadata or the icon set
  changed.
* Both binaries go into `Contents/MacOS/` side by side: the shell looks the
  gateway up next to its own executable.
* The icon set and `Info.plist` live in `desktop/gpui/packaging/` — the
  packaging is self-contained, it does not depend on the removed Tauri tree.

## Visual QA

A native window cannot be inspected from the outside: there is no WebView to
poke and no accessibility tree to dump. Two harnesses close that gap.

### `tools/screenshot.sh`

```sh
./desktop/gpui/tools/screenshot.sh launch      # start the app, wait for its window
./desktop/gpui/tools/screenshot.sh shot login  # -> tools/shots/login.png
./desktop/gpui/tools/screenshot.sh status      # app, window, gateway, log tail
./desktop/gpui/tools/screenshot.sh stop        # terminate the app and its sidecar
./desktop/gpui/tools/window_probe.sh           # window + gateway, one screen
```

`desktop/scripts/gpui-screenshot.sh` is a thin wrapper around the same script.
The harness needs the binary to exist (`launch` exits `4` when it does not)
and needs Screen Recording and Automation permissions for the capture and the
window queries.

### `tools/mcp/server.js`

A stdio MCP server (Node standard library only, no dependencies) that lets an
agent inspect the router and drive the shell:

```sh
node desktop/gpui/tools/mcp/server.js
```

| Tool | What it does |
|---|---|
| `router_health` | `GET /healthz` — reachability and the gateway's backend name. No bearer needed. |
| `router_request` | Call any admin endpoint: `method` (`GET`/`POST`/`PATCH`), `path`, optional JSON `body`. |
| `router_endpoints` | The catalogue of admin endpoints with a one-line description each. |
| `app_launch` | Start the native shell; returns the pid and the log path. |
| `app_stop` | Stop the app `app_launch` started, by pid file. |
| `app_status` | Is the app running, does the gateway answer, the last log lines. |
| `app_screenshot` | Capture the screen to `shots/<name>.png` and return the path. |

The bearer is read from `HETERION_ROUTER_KEY` or from the operator-key file
below; it is never printed — it is replaced with `[redacted]` if it ever
appears in a payload. Registration snippet and the details are in
`tools/mcp/README.md`.

## The operator key

The key is a bearer with the `manage` scope — it authorizes every call the
dashboard makes. It is **not** in the gateway's database: `api_keys.key` holds
`hash-only:<id>` markers for the keys the gateway issues, never a usable
bearer.

The shell keeps the operator's own key here:

```
<data dir>/desktop/operator-key.json
```

`<data dir>` is `~/.heterion-router`, or the legacy `~/.omniroute` if that is
the directory that exists — the same precedence the gateway uses, so the shell
and the gateway always look at the same place. `HETERION_ROUTER_DATA_DIR` and
`DATA_DIR` override it.

What the file guarantees:

* **0600** inside a **0700** directory, from the first byte — the temporary
  file is created with those modes before a single byte is written.
* **Atomic** — the key goes into a temporary file in the same directory, is
  flushed, and is renamed over the credential file. A crash leaves either the
  old key or the new one, never a half-written one.
* **Not a link** — `load()` refuses a symlink at the credential path, and it
  compares the inode of the path it stat'ed with the inode of the file it
  opened.
* **Never logged**, not even partially, and never in an error message — only
  paths are reported.

The web dashboard keeps the same key in the browser's `localStorage`
(`desktop/ui/src/api.js`); the native shell deliberately does not.

## Known limitations

* **macOS only.** The build needs Xcode for `xcrun metal`, the bundle is a
  macOS `.app`, and the QA harness uses `osascript` and `screencapture`.
* **The crate is outside the workspace.** The workspace gates
  (`cargo fmt --all`, `cargo clippy --workspace --all-targets -- -D warnings`,
  `cargo test --workspace`) do not cover it; use the per-manifest commands
  above.
* **The bundle is ad-hoc signed and not notarized.** It runs on the machine
  that built it; it is not distributable as-is.
* **No hover tooltips.** The Svelte dashboard uses `title=` attributes on
  several buttons. gpui has no ready-made tooltip widget — the only mechanism
  is `StatefulInteractiveElement::tooltip`/`hoverable_tooltip`, which makes
  the caller build the tooltip body as an `AnyView` — and the port does not
  implement one, so those hints are missing.
* **No "activate combo" control in the Combos view.** The gateway's
  `GET /api/combos` is built from `repos::list_active_combos`, so the list
  contains active combos only and a deactivated combo cannot be switched back
  on from the dashboard. Providers does have the on/off control, because
  `GET /api/connections` returns inactive rows as well.
* **The text input is not a full IME input.** `widgets/input.rs` implements
  `gpui::EntityInputHandler`, so the caret, the selection and the marked text
  are drawn correctly and the clipboard round-trips. Typing still comes from
  key events, though: on macOS the platform only calls `insertText:` for keys
  the widget does not consume, so a *new* IME composition cannot be started
  from the keyboard. Fine for ASCII, not for CJK input.
* **The QA harness proves a window exists, not what is inside it.** There is
  no accessibility-tree dump: "the login form is drawn" and "the Overview
  table has rows" both come down to *a window with this title and this size is
  on screen*. The capture is a screen crop, not a window capture, and only the
  main display is captured.
* **The harness cannot sign in.** The operator key is only checked against the
  gateway; typing it into the window would push a credential through the
  accessibility keystroke path, so a human pastes it and the harness takes the
  shot.
* **A second copy of the shell exits.** The instance lock on port 20130 makes
  the second process log a warning and return instead of opening a window.

## Where to read next

* `docs/port/GPUI-PORT-SPEC.md` — the port contract: the endpoints, the view
  contract, the shared widgets, the rules.
* `docs/port/GPUI-API-NOTES.md` — the verified gpui 0.2.2 API facts, and the
  surprises that cost time during the port.
* `desktop/gpui/tools/README.md` — the visual-QA harness in detail.
* `desktop/gpui/packaging/README.md` — the bundle metadata and the icon set.

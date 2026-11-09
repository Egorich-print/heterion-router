# Heterion Router desktop: Tauri v2 → GPUI port contract

The desktop shell moves from **Tauri v2 + a WebView hosting the Svelte
bundle** to a **native GPUI application**. The web dashboard stays where it is
(`desktop/ui`, served by the gateway); only the native shell changes.

```
desktop/gpui/            the new native app (Rust, gpui 0.2.2)
desktop/ui/              unchanged — the web dashboard the gateway serves
desktop/src-tauri/       removed by the migration
```

## What every agent must read first

1. `docs/port/GPUI-API-NOTES.md` — the verified gpui 0.2.2 API facts. Do not
   guess API names; look them up in the crate source
   (`~/.cargo/registry/src/index.crates.io-*/gpui-0.2.2/src/`).
2. `desktop/gpui/src/api.rs` — the typed API contract. **Do not edit it.**
3. `desktop/gpui/src/theme.rs` — the palette. **Do not edit it.**
4. `desktop/gpui/src/widgets/` — the shared widgets. **Do not edit them**
   unless your task says so.
5. The Svelte view you are porting: `desktop/ui/src/views/<name>.svelte` —
   that file is the functional specification.

## The API contract (already typed for you)

`desktop/gpui/src/api.rs` mirrors the gateway's JSON exactly
(`crates/heterion-router-gateway/src/admin.rs`):

| Endpoint | Type |
|---|---|
| `GET /healthz` | `Health` |
| `GET /api/overview` | `Overview` |
| `GET /api/combos` | `CombosResponse` → `Vec<Combo>` |
| `PATCH /api/combos/{id}` | `UpdateComboBody` |
| `GET /api/connections` | `ConnectionsResponse` → `Vec<Connection>` |
| `PATCH /api/connections/{id}` | `UpdateConnectionBody` |
| `GET /api/keys` | `KeysResponse` → `Vec<ApiKey>` |
| `POST /api/keys` | `CreateKeyBody` → `CreatedKey` (the only secret) |
| `POST /api/keys/{id}/revoke` | `KeyStateResponse` |
| `POST /api/keys/{id}/restore` | `KeyStateResponse` |
| `GET /api/usage?days=&limit=` | `Usage` |
| `GET /api/logs?limit=` | `LogsResponse` → `Vec<CallLog>` |
| `GET /api/logs/app?limit=` | `AppLogsResponse` → `Vec<String>` |
| `POST /api/restart` | `RestartResponse` |

## The view contract

Every view is an entity that owns its data and loads it itself:

```rust
pub struct OverviewView {
    pub data: Option<Overview>,   // the loaded payload
    pub error: Option<String>,    // the last failure
    pub loading: bool,            // a request is in flight
}

impl OverviewView {
    pub fn new(cx: &mut Context<Self>) -> Self { … }   // builds and loads
    pub fn refresh(&mut self, cx: &mut Context<Self>) { … }
}

impl Render for OverviewView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement { … }
}
```

Loading data — always through `api::spawn_request`, never blocking the UI
thread:

```rust
let Some(client) = api::session_client(cx) else { return; };
api::spawn_request(
    cx,
    move || client.get_json::<Overview>("/api/overview"),
    |result, view: &mut OverviewView, cx| {
        match result {
            Ok(data) => { view.data = Some(data); view.error = None; }
            Err(error) => {
                view.error = Some(error.to_string());
                crate::app::report_error(cx, &error);   // drops the session on 401/403
            }
        }
        view.loading = false;
        cx.notify();
    },
);
```

Mutating the gateway — same pattern with `client.post_json`/`client.patch_json`
and the matching body type from `api.rs`.

## The shared widgets

`desktop/gpui/src/widgets/` (already written, styled like the Svelte bundle):

| Widget | Signature |
|---|---|
| `nav_item` | `nav_item(label, active, on_click: impl Fn(&mut Window, &mut App)) -> impl IntoElement` |
| `button` | `button(label, on_click) -> impl IntoElement` |
| `primary_button` | `primary_button(label, enabled, on_click) -> impl IntoElement` |
| `badge` | `badge(label, tone: Tone) -> impl IntoElement` — `Tone::{Neutral, Ok, Warn, Bad}` |
| `tag` | `tag(label) -> impl IntoElement` |
| `card` | `card(child) -> impl IntoElement` |
| `stat_card` | `stat_card(label, value) -> impl IntoElement` |
| `section_title` | `section_title(title) -> impl IntoElement` |
| `page_header` | `page_header(title, subtitle) -> impl IntoElement` |
| `error_banner` | `error_banner(message) -> impl IntoElement` |
| `notice_banner` | `notice_banner(message) -> impl IntoElement` |
| `empty_state` | `empty_state(message) -> impl IntoElement` |
| `mono` | `mono(text) -> impl IntoElement` |
| `muted` | `muted(text) -> impl IntoElement` |
| `number` | `number(text) -> impl IntoElement` |
| `table_header` | `table_header(&[&str]) -> impl IntoElement` |
| `table_row` | `table_row(Vec<AnyElement>) -> impl IntoElement` |
| `cells_row` | `cells_row(Vec<AnyElement>) -> impl IntoElement` — any column count |
| `table` | `table(children) -> impl IntoElement` — the bordered panel |
| `bar_chart` | `bar_chart(Vec<BarItem>) -> impl IntoElement` |
| `TextInput` | a text field — see `widgets/input.rs` |

`TextInput` is an **entity**, not a function:

```rust
let input = cx.new(|cx| TextInput::new(cx, ""));
input.update(cx, |input, _| {
    input.set_placeholder("name");
    input.set_on_change(|value, cx| { … });
    input.set_on_submit(|value, window, cx| { … });
});
div().child(input.clone())     // an Entity is an element
```

## Rules

- **Your file only.** Never edit a file another agent owns. If the build fails
  because of *someone else's* file, ignore that error — only your file must be
  clean.
- **No new dependencies.** `Cargo.toml` is owned by the orchestrator. If you
  genuinely need one, say so in your report instead of adding it.
- Rust **1.99**, edition **2024**. The workspace gates are
  `cargo fmt --all` and `cargo clippy -- -D warnings`.
- No `unwrap()` in a UI path that a user can trigger; no `panic!` in a render
  path.
- Never log a key, a token or any part of a secret. The gateway masks
  everything it returns, and the shell must not undo that.
- Every view must survive an empty payload: no database, no connections, no
  keys, no usage. Render an empty state, not a panic.
- Match the Svelte view's behaviour, not just its look: same endpoints, same
  buttons, same confirmations.

## Done means

- `cargo build` reports **no errors in your file**.
- `cargo clippy --manifest-path desktop/gpui/Cargo.toml -- -D warnings`
  reports nothing for your file.
- The view shows what the Svelte view shows, with the same colours, the same
  empty states and the same error handling.
- Your report names the file you wrote and what a reviewer should look at.

## What the port produced

Seven views, one per sidebar entry. Each is an entity in
`desktop/gpui/src/views/` that owns its state and loads its own data; the
shell (`src/app.rs`) creates them on first use and keeps them alive so a
section keeps its filters and scroll position while the operator moves
around.

| View | Module | Endpoints it uses |
|---|---|---|
| Overview | `src/views/overview.rs` | `GET /api/overview` |
| Combos | `src/views/combos.rs` | `GET /api/combos`, `PATCH /api/combos/{id}` |
| Providers | `src/views/providers.rs` | `GET /api/connections`, `PATCH /api/connections/{id}`, `POST /api/restart` |
| Keys | `src/views/keys.rs` | `GET /api/keys`, `POST /api/keys`, `POST /api/keys/{id}/revoke`, `POST /api/keys/{id}/restore` |
| Usage | `src/views/usage.rs` | `GET /api/usage?days=&limit=` |
| Logs | `src/views/logs.rs` | `GET /api/logs?limit=`, `GET /api/logs/app?limit=` |
| Playground | `src/views/playground.rs` | `GET /api/combos`, `POST /v1/chat/completions` |

The shell itself asks for `GET /healthz` (`src/app.rs`) — no bearer needed —
to show the gateway's state in the sidebar.

The endpoint set is the same one the Svelte dashboard uses
(`desktop/ui/src/views/*.svelte`): the two frontends differ in how they are
drawn, not in what they ask the gateway for.

## Two deliberate deviations

### Combos has no "toggle active" control

The gateway's `GET /api/combos` is built from `repos::list_active_combos`
(`crates/heterion-router-gateway/src/admin.rs`), so the list contains **active
combos only**. `PATCH /api/combos/{id}` does accept an `is_active` field, but a
deactivated combo is in no list a dashboard can read, so its id is out of
reach and nothing can switch it back on. The native Combos view therefore
offers the same controls as the Svelte one — rename, reorder, strategy,
hide/unhide, and the entry list — and no active switch.

Providers is the opposite case: `GET /api/connections` returns inactive rows
as well, so that view does have the on/off control
(`src/views/providers.rs`, `PATCH /api/connections/{id}` with `is_active`).

### No hover tooltips

The Svelte dashboard explains several icon-only buttons with the HTML `title`
attribute — the combo weight buttons (`desktop/ui/src/views/Combos.svelte`,
`title="−5"` / `"+5"` / `"Remove from combo"`) and the connection priority
buttons (`desktop/ui/src/views/Providers.svelte`, `title="Lower priority"` /
`"Raise priority"` / the activate-deactivate hint).

gpui 0.2.2 has no ready-made tooltip widget. The only mechanism is
`StatefulInteractiveElement::tooltip` / `hoverable_tooltip`
(`gpui/src/elements/div.rs`), and both make the caller build the tooltip body
as an `AnyView` — there is nothing to point at a string. The port implements
none, so those buttons show the same glyphs as the web dashboard but none of
their hints. The visible labels are unchanged; what is missing is the
explanation that used to appear on hover.

Both deviations are properties of the target platform and the gateway's data
model, not shortcuts: everything else the Svelte views showed is reproduced.

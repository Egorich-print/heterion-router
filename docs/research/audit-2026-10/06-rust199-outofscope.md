# 06 — Rust 1.99 / edition-2024 readiness + out-of-scope defect hunt

Date: 2026-10-02. Scope: verify `rust-toolchain.toml` (1.98.0) → 1.99 readiness on the
working tree at `51dfe6d` + 35 modified files, edition-2024 hygiene, and defects
unrelated to any feature. Read-only: no source, test, Cargo or DB file was modified;
the live database was only ever opened through `mode=ro` URIs.

`CARGO_TARGET_DIR=/tmp/hra-a6` was used for every cargo invocation. No target
directory was deleted.

## Toolchain matrix

| toolchain | `check --workspace --all-targets` | `clippy --workspace --all-targets -- -D warnings` | `test --workspace` | `fmt --all -- --check` |
|-----------|-----------------------------------|--------------------------------------------------------|--------------------|--------------------------|
| `+1.98.0` (pin) | clean, exit 0 (`Finished dev profile ... in 27.60s`) | **clean, exit 0**, zero `warning:`/`error:` lines | 215 passed, 0 failed | exit 1, 2 diffs (workspace) |
| `+1.99.0` | clean, exit 0 (`Finished dev profile ... in 14.16s`) | **clean, exit 0**, zero `warning:`/`error:` lines | 215 passed, 0 failed | exit 1, 2 diffs (workspace) |
| `+1.99.0` desktop (`desktop/src-tauri`, excluded) | clean, exit 0 (`Finished dev profile ... in 35.07s`) | clean, exit 0 | n/a (no tests in crate) | exit 1, **2 diffs** |
| `+1.98.0` desktop | n/a | n/a | n/a | exit 1, byte-identical output to 1.99 |
| `+stable` (1.98.1, active default) | — | — | — | — |

**1.99 introduces no new lints and no new warnings.** The 1.98 and 1.99 clippy
outputs contain no `warning:` or `error:` line at all, and the two `fmt --check`
diffs are byte-identical between the two toolchains (`diff` of the two captured
outputs is empty). Nothing in the tree needs a code change for the bump.

### Test counts per crate (identical on 1.98 and 1.99)

| target | passed |
|--------|--------|
| heterion-router-config (lib) | 6 |
| heterion-router-core (lib) | 8 |
| heterion-router-crypto (lib) | 13 |
| heterion-router-db (lib) | 17 |
| heterion-router-gateway (lib) | 100 |
| heterion-router-gateway (bin) | 7 |
| upstream-harness | 1 |
| heterion-router-http (lib) | 8 |
| heterion-router-providers (lib) | 14 |
| heterion-router-routing (lib) | 6 |
| heterion-router-translate (lib) | 23 |
| claude-parity | 1 |
| claude-request-parity | 1 |
| gemini-parity | 1 |
| parity | 1 |
| request-parity | 1 |
| tools-parity | 1 |
| heterion-router-usage (lib) | 6 |
| doc-tests ×10 crates | 0 |
| **total** | **215** |

`docs/MISSION-2026-10.md:59` and `STATUS.md:137` still say **213 passed** — the
baseline is stale by 2 (see defect D-13).

### The 2 workspace `fmt` diffs

Both live in uncommitted work-tree edits to `crates/heterion-router-gateway/src/lib.rs`
(`git stash` of that one file makes `cargo fmt --all -- --check` exit 0 at HEAD):

```
Diff in .../crates/heterion-router-gateway/src/lib.rs:964:
-    if let Some(kind) = unsupported_builtin_tool(request.get("tools").and_then(Value::as_array))
-    {
+    if let Some(kind) = unsupported_builtin_tool(request.get("tools").and_then(Value::as_array)) {

Diff in .../crates/heterion-router-gateway/src/lib.rs:1247:
-            Err(error) => (
-                vec![sanitized_error_payload(&error)],
-                false,
-            ),
+            Err(error) => (vec![sanitized_error_payload(&error)], false),
```

### The 2 desktop `fmt` diffs (as AGENTS.md predicted)

```
Diff in .../desktop/src-tauri/build.rs:1:
 fn main() {
-  tauri_build::build()          # 2-space indent
+    tauri_build::build()
 }

Diff in .../desktop/src-tauri/src/lib.rs:81:
-                init_script =
-                    format!("window.__HETERION_ROUTER_GATEWAY_URL__={gateway_url:?};");
+                init_script = format!("window.__HETERION_ROUTER_GATEWAY_URL__={gateway_url:?};");
```

Both are present at HEAD (`git show HEAD:desktop/src-tauri/build.rs` shows the
2-space indent), so they are committed debt, not work-tree drift. AGENTS.md:30-31
is accurate.

## Exact version references to update

`rg -n "1\.98" -g '!target' -g '!Cargo.lock' .` → 19 hits in 11 files.

### Must change (3 statements, all of them the actual contract)

| file:line | statement |
|-----------|-----------|
| `rust-toolchain.toml:2` | `channel = "1.98.0"` → `"1.99.0"` |
| `Cargo.toml:28` | `rust-version = "1.98"` → `"1.99"` |
| `desktop/src-tauri/Cargo.toml:9` | `rust-version = "1.98"` → `"1.99"` |

The 10 member crates all inherit via `edition.workspace = true` /
`rust-version.workspace = true`; nothing else shadows them (verified: no `[lints]`
table anywhere, no per-crate `edition = "20xx"` literal).

### Prose that states the pin (should be updated or explicitly left as history)

| file:line | note |
|-----------|------|
| `STATUS.md:142` | "CI использует `@stable`, тогда как `rust-toolchain.toml` закрепляет `1.98.0`" — the second half needs the new number |
| `docs/MISSION-2026-10.md:61` | `` | `rust-toolchain.toml` | `channel = "1.98.0"` **[V]** | `` |
| `docs/MISSION-2026-10.md:242` | F9 task text |
| `docs/MISSION-2026-10.md:424` | F9 completion line — reads as a historical record, arguably leave |
| `docs/research/mission-2026-10/01-mission-sequencing.md:320,819` | F9 rows |
| `docs/research/mission-2026-10/05-desktop-release.md:70,890` | records an *observed* `rustc 1.98.1` — evidence, leave as is |
| `docs/research/mission-2026-10/06-qa-docs.md:250,279,757` | pin-vs-`@stable` analysis |
| `docs/research/audit-2026-10/02-omniroute-parity.md:222` | CI-vs-pin gap |
| `docs/research/audit-2026-10/03-futureagi-parity.md:363` | CI-vs-pin gap |
| `docs/research/audit-2026-10/05-inventory-reorg.md:76,350,742` | pin is "the version contract" |

### MSRV policy

There is **no MSRV policy document**. `rg -in "msrv|minimum supported"` finds
nothing but the two `rust-version` lines. `cargo msrv` is not installed
(`error: no such command: msrv`) and is not a dependency. The only written
statement of intent is `docs/research/audit-2026-10/05-inventory-reorg.md:742`
("`rust-toolchain.toml` is the version contract"). No README/AGENTS/STATUS
section says "bump N requires updating these three files" — worth adding one
line during the bump, otherwise the pin drifts silently (D-14).

The CI pin mismatch is real and independent of the bump: `.github/workflows/ci.yml:39`
and `.github/workflows/release.yml:30` both use `dtolnay/rust-toolchain@stable`,
so CI compiles whatever stable is, never the pinned 1.99.0.

## Edition-2024 hygiene findings

`edition` declarations found: 3 total (1 workspace default + 1 desktop + 10
inheriting crates). All are `2024`. `resolver = "3"` in `Cargo.toml:8` is valid
and is what edition 2024 implies; cargo accepts it without complaint on both
toolchains.

| check | result |
|-------|--------|
| `static mut` references | none |
| `gen` used as an identifier | none (only `gen-1` inside a JSON string at `crates/heterion-router-gateway/src/openai.rs:644` and a `gen.pollinations.ai` URL) |
| `RefCell`/`Cell` borrowed across `.await` | none; the two mutexes (`gemini.rs:38` `last_message_id`, `gemini.rs:54` signature state) are taken and released inside a single expression |
| `unsafe extern` blocks | none |
| `mem::uninitialized` / `deprecated_safe_2024` | none |
| `unsafe` attributes in macros | none |
| `impl Trait` capture rules | no RPIT lifetime-capture breakage; the tree compiles clean on both toolchains |
| `Box<dyn Error>` never-type fallback | 4 sites (`gateway/src/main.rs:86,359,370,387`), all in `fn ... -> Result<T, Box<dyn std::error::Error>>` — never a bare `Box<dyn Error>` return, so the never→error fallback change cannot bite |
| tail-expression temporary scope (`if let` temporaries) | no observable break; behaviour is covered by `crates/heterion-router-translate/tests/*_parity.rs` (6 tests) and all pass on 1.99 |
| match-ergonomics reservations | no `ref`/`&mut` patterns that edition 2024 would re-bind differently; compile + tests clean |
| `Box<dyn Error>` in the desktop crate | none |

**Verdict: the tree is edition-2024 clean. Nothing blocks the 1.99 bump and
nothing in the edition migration is outstanding.**

## Unsafe audit

`rg -n "\bunsafe\b" crates desktop/src-tauri` → 10 hits, all of them one of:

| site | what | reachable from an HTTP request? | edition-2024 lint status |
|------|------|-------------------------------|-------------------------|
| `crates/heterion-router-crypto/src/lib.rs:253,256,265,268` | `std::env::remove_var`/`set_var` in `#[cfg(test)] mod loader_tests` | no (tests) | correct: edition 2024 makes `set_var`/`remove_var` `unsafe`, and each call is inside an `unsafe {}` block |
| `crates/heterion-router-db/src/db.rs:149,151,156,167` | same, in `#[cfg(test)] mod tests` | no (tests) | correct |
| `crates/heterion-router-gateway/src/main.rs:351` | `libc::kill(pid, 0)` — POSIX liveness probe inside `fn parent_gone` | indirectly: runs in the `watch_parent` watchdog thread, which calls `std::process::exit(0)` | correct: minimal, no `unsafe fn`, no `#[unsafe(...)]` attribute needed |

Findings:

1. **No `unsafe fn` anywhere.** Therefore `unsafe_op_in_unsafe_fn` (deny-by-default
   in edition 2024) has nothing to reject.
2. **No `#[unsafe(no_mangle)]` / `#[unsafe(export_name)]` / `#[unsafe(link_section)]`.**
   `unsafe_attr_outside_unsafe` (also deny-by-default in edition 2024) is satisfied
   vacuously. The one `unsafe extern`-adjacent FFI call is `libc::kill`, reached
   through the safe `libc` crate, so no `unsafe extern "C"` block is written by hand.
3. **Edition-2024 `set_var` unsafety is correctly acknowledged** in all 8 test sites.
   These are the *only* reason the tree needs `unsafe` at all, and they are all in
   `#[cfg(test)]`.
4. **Caveat worth noting (not a defect):** those env-mutating tests mutate
   process-global state while `cargo test` runs the crate's tests on parallel
   threads. `data_dir_prefers_env` (`db.rs:146`) and the two crypto loader tests
   all `remove_var`/`set_var` the same or overlapping keys with no mutex. They pass
   today because they touch disjoint keys per crate, but they are one added test
   away from a flaky failure. This is the classic reason `unsafe { set_var }` is
   now `unsafe`. Severity P3, pre-existing, not introduced by the bump.

## Out-of-scope defects

Findings unrelated to any feature's scope but that break correctness, hygiene or
the build. Ordered by severity.

| id | sev | file:line | one-line |
|----|-----|-----------|----------|
| D-1 | P1 | `crates/heterion-router-db/src/repos.rs:856,884` | `usage_summary` window filter compares TEXT timestamps against `datetime('now', ?1)`; the table holds two spellings, so the window is wrong by up to a day |
| D-2 | P1 | `crates/heterion-router-gateway/src/lib.rs:325,354` | usage/call-log rows record `provider = "routing"` for every request, so per-provider aggregation is useless for all Rust-written rows |
| D-3 | P1 | `crates/heterion-router-gateway/src/main.rs:171` | 5 active `exa-search` connections are silently dropped — no registry entry, no warning |
| D-4 | P2 | `crates/heterion-router-gateway/src/lib.rs:404,415` | a stream that ends without a terminal chunk (client disconnect, upstream cut) records **no** usage and **no** call-log row |
| D-5 | P2 | `crates/heterion-router-db/src/repos.rs:943` | `count_active_api_keys` ignores `is_active`/`is_banned`/`expires_at`, so the dashboard over-reports usable keys |
| D-6 | P2 | `crates/heterion-router-gateway/src/auth.rs:108`, `main.rs:375` | both use `SELECT COUNT(*) FROM api_keys`, so a DB whose only keys are revoked still passes the non-loopback policy check and then 401s every caller |
| D-7 | P2 | `crates/heterion-router-gateway/src/lib.rs:332-334` | cache/reasoning token counters are hard-coded `0` on write and on cost, so cache-heavy providers are billed at full input rate |
| D-8 | P2 | `crates/heterion-router-providers/src/lib.rs:3` | says "277 providers"; `registry.json` has 278 |
| D-9 | P2 | `crates/heterion-router-gateway/src/lib.rs:327,353` | `connection_id` is always NULL and `requested_model` mirrors the resolved model, so per-credential and per-combo attribution is impossible from Rust rows |
| D-10 | P2 | `crates/heterion-router-translate/src/requests.rs:539` | `.expect("schema present")` on a request-derived value; a malformed-but-non-null `json_schema` body reaches it |
| D-11 | P2 | `crates/heterion-router-gateway/src/gemini.rs:338,380,421,498` | `.lock().unwrap()` on a `std::sync::Mutex` — a poisoned mutex turns every later Gemini request into a panic |
| D-12 | P2 | `crates/heterion-router-crypto/src/lib.rs:87,93` | `expect("os random source")` / `expect("gcm encryption")` inside `encrypt()`, which runs on the token-refresh path |
| D-13 | P3 | `STATUS.md:137,165`, `CHANGELOG.md:14`, `docs/MISSION-2026-10.md:59,457` | test-count baseline says 213; the tree has 215 |
| D-14 | P3 | `crates/heterion-router-gateway/src/main.rs:537` | `watch_parent_without_marker_is_a_no_op` asserts nothing |
| D-15 | P3 | `crates/heterion-router-gateway/src/main.rs:245` | 4 more active connections (`agentrouter`, `kimi-coding`, `kiro`, `zai`) dropped without a warning because their registry `format` is `claude`/`kiro` |
| D-16 | P3 | `crates/heterion-router-db/src/db.rs:146-169`, `crates/heterion-router-crypto/src/lib.rs:238-269` | parallel `cargo test` mutates process env without a mutex |
| D-17 | P3 | `crates/heterion-router-gateway/src/lib.rs` (2 fmt diffs) | the pre-commit gate `cargo fmt --all` is currently **red** on the work tree |
| D-18 | P3 | `desktop/src-tauri/build.rs:2`, `desktop/src-tauri/src/lib.rs:81` | 2 committed `fmt` diffs in the excluded crate; no CI job covers them |
| D-19 | P3 | whole tree | no documented MSRV policy; CI uses `@stable`, so the pin is not what CI verifies |
| D-20 | P3 | `crates/heterion-router-gateway/src/routing.rs:233,306` | `.unwrap()` on `last_error` right after assigning `Some(...)` — safe today, but a needless panic surface |

### D-1 — the usage window is computed on mixed-format TEXT (P1)

`repos.rs:856` and `:884` both filter `timestamp >= datetime('now', ?1)`.
`datetime('now', '-30 days')` returns `2026-09-02 17:49:27`; `usage_history`
holds 44 808 rows in ISO-8601 (`2026-07-22T07:52:37.637Z`) and 332 in SQLite
(`2026-10-01 20:02:24`) spelling. Lexicographically `'T' (0x54) > ' ' (0x20)`, so
every ISO row on the cutoff date sorts *above* the cutoff regardless of time:

```
cutoff=2026-09-02 17:49:27
code   =15663      <- what the gateway reports
parsed =15210      <- what it should report
437 rows (2.8%) are counted that are outside the window
```

`GET /api/usage?days=30` therefore inflates `requests`, `successes`,
`tokens_input`, `tokens_output`, `tokens_cache_read` and both `by_*` buckets.
At `days=7` the effect is invisible today (13 rows, all SQLite-spelled) but it
reappears the moment the JS side writes an ISO row on the boundary date.
Minimal fix: normalise in SQL — `WHERE datetime(substr(timestamp,1,19)) >= datetime('now', ?1)`
— or reuse the existing `parse_db_timestamp` logic (already in this file at
`:418`, already documented as handling "both timestamp spellings").

### D-2 — every Rust usage row is attributed to `"routing"` (P1)

`record_completion_usage` writes `provider: Some(state.backend.name().to_string())`.
The backend in `AppState` is the `RoutingBackend`, whose `name()` is the literal
`"routing"` (`routing.rs:196`). Live DB confirms: all 332 Rust-written
`usage_history` rows have `provider = 'routing'`, while JS-written rows carry real
ids (`openrouter` 14 715, `gemini` 7 832, `grok-cli` 3 733…). `by_provider` in
`GET /api/usage` can therefore only ever show one bucket for Rust traffic, and
`call_logs.provider` has the same problem. This is an accounting defect, not a
feature gap: it is invisible until someone tries to answer "which provider did
this gateway serve?".
Minimal fix: thread the resolved backend name out of `RoutingBackend` (it already
knows which `name` it dispatched to) into `record_completion_usage`.

### D-3 — `exa-search` connections silently vanish (P1)

Read-only cross-reference of `provider_connections` against
`crates/heterion-router-providers/registry.json`:

```
ACTIVE providers missing from registry.json:
  exa-search  (5 active conn(s))
```

No entry with `id == "exa-search"`, and no alias maps to it (266 aliases checked,
none exa-like). `select_backend` iterates `active_providers()` and hits
`main.rs:168-171`: `provider_base_url_from` returns `None`, the `else { continue; }`
branch fires, and nothing is logged. The operator sees 5 active credentials in
`GET /api/connections` (`servable: false`) and a gateway that quietly ignores them.
Minimal fix: `tracing::warn!("provider {provider} has no registry entry; skipping")`
before the `continue` at `main.rs:171`. Same for D-15.

### D-4 — a dropped stream records nothing at all (P2)

`stream_response` (`lib.rs:392-452`) only calls `record_stream_usage` on
(a) a chunk with `finish_reason` and (b) an `Err` item. If the client
disconnects mid-stream, axum drops the body, the upstream `drive_stream` sees
`tx.closed()` and returns `Ok(())` (`openai.rs:75`, `gemini.rs:357`), and no
terminal chunk and no error ever reach the accounting closure. `recorded` stays
`false` and both `usage_history` and `call_logs` get no row. The tokens were
spent upstream; the local ledger says the request never happened.
Minimal fix: wrap the stream so the closure runs on drop, or record in a
`Drop` guard that fires when `recorded == false`.

### D-5 — `count_active_api_keys` over-reports (P2)

`repos.rs:943` is `SELECT COUNT(*) FROM api_keys WHERE revoked_at IS NULL`. It
ignores `is_active = 0`, `is_banned = 1` and `expires_at`, all of which
`find_usable_api_key` (`:362-363`) *does* enforce. `GET /api/overview` shows the
result under `counts.api_keys`, so the dashboard can report "3 API keys" while
the auth path would reject all 3. Already logged as a known issue in
`STATUS.md:159-160`; the fix is to reuse the same predicate.

### D-6 — the non-loopback policy check counts dead keys (P2)

`main.rs:375` (`validate_auth_policy`) and `auth.rs:108` both run
`SELECT COUNT(*) FROM api_keys` with no filter at all. Revoke every key and the
gateway still binds `0.0.0.0:20128` — then every request 401s, because
`find_usable_api_key` correctly rejects them. A fail-closed outcome reached
through an open-gate check. Fix: use `count_active_api_keys`, or better, the
same predicate as `find_usable_api_key`.

### D-7 — cache tokens are billed as fresh input (P2)

`UsageRecord` is built with `tokens_cache_read: 0`, `tokens_cache_creation: 0`,
`tokens_reasoning: 0` (`lib.rs:332-334`) and `cost_usd(...)` is called with
literal `0, 0, 0` for cached/reasoning/cache-creation (`lib.rs:266-272`,
`916-922`). `heterion_router_core::Usage` (`core/src/chat.rs:134-138`) only has
`prompt_tokens`/`completion_tokens`/`total_tokens`, so the cache detail that
`openai_chunk_to_stream_chunk` already reads (`backend.rs:130-154`) is discarded
upstream of the writer. Live DB: 18 622 rows carry `tokens_cache_read > 0`
(1 097 822 016 tokens on `xiaomi-mimo-token-plan` alone) and **all 332 Rust rows
have 0**. The `x-heterion-router-cost-usd` header and the dashboard both
over-report spend for cache-heavy providers. Minimal fix: add the three fields to
`core::Usage`, populate them in `openai_chunk_to_stream_chunk`, pass them
through.

### D-8 — the 277/278 drift (P2, pre-existing, already tracked)

`crates/heterion-router-providers/src/lib.rs:3`: `//! The JavaScript `REGISTRY`
(277 providers) is dumped to `registry.json``. The file has **278** entries
(2717 models; 2715 at HEAD — the working tree adds `grok-4.7` to `xai` and
`grok-cli`). No duplicates among the 278 ids, and the `insta` snapshot
`heterion_router_providers__registry__tests__provider_id_list_snapshot.snap`
contains exactly 278 ids, so the test is consistent — only the doc comment lies.
Already tracked as `A3` in `docs/MISSION-2026-10.md:166`; still unfixed.
Note the related numbers *are* correct: `AGENTS.md:63` and `STATUS.md:52-54`
(278 providers; 259 pass the format+base-URL gate; 253 of those have HTTP(S);
6 use `wss://`/stdio/ACP) all reproduce exactly against the working tree.

### D-9 — attribution fields are stubbed out (P2)

Same writer as D-2: `connection_id: None` (`lib.rs:327`) always, and
`requested_model: Some(model.to_string())` (`:353`) where `model` is the
*resolved* model, so the originally-requested combo or alias is never recorded
(`combo_name` is likewise never written — 0 non-null values among the Rust rows).
Three of the JS-era attribution columns are structurally dead for Rust traffic.

### D-10 — request-derived `.expect()` (P2)

`crates/heterion-router-translate/src/requests.rs:533-539`:

```rust
let schema = body.get("response_format").and_then(|format| format.get("json_schema"));
if schema.and_then(|s| s.get("schema")).is_none() { return; }
let schema = schema.expect("schema present");
```

The guard checks `s.get("schema")`, but `Value::get` on a *non-object*
`json_schema` (e.g. `"json_schema": "gpt-4"`) returns `None`, so that path
returns early — fine. The reachable panic needs `json_schema` to be an object
with a `schema` key present but `.get` still `None`, which cannot happen for a
real object. So today the `expect` is unreachable: it is dead defensive code, not
a live panic. Still worth collapsing into `let Some(schema) = ... else { return }`
so the invariant is expressed by the compiler. Same shape in
`claude.rs:313`, `gemini.rs:353`, `responses.rs:400` — those `.expect("chunk is
an object")` calls are on locally-constructed `json!` values and genuinely
cannot fail.

### D-11 — poisonable mutex in the Gemini path (P2)

`gemini.rs:338,380,421,498` all use `.lock().unwrap()` on
`Arc<std::sync::Mutex<Option<String>>>` guarding `last_message_id`. Any panic
while the guard is held poisons the mutex, and every subsequent
`/v1/chat/completions` request routed to Gemini panics inside the handler —
a permanent 500 storm with no recovery short of a restart. Note the codebase
already uses the poison-tolerant form elsewhere:
`gemini.rs:84-88`, `db.rs:130-133`, `routing.rs:119-123` all use
`unwrap_or_else(|poisoned| poisoned.into_inner())`. The four Gemini sites are
the outliers. Minimal fix: same one-liner as the rest of the tree.

### D-12 — `expect` on the token-refresh path (P2)

`crypto/src/lib.rs:87` `getrandom::fill(&mut iv).expect("os random source")` and
`:93` `.expect("gcm encryption")`. `encrypt()` is called from the
`RefreshCallback` closure built in `main.rs:110-130`, i.e. on the live request
path when GrokCli rotates a 401. The doc comment above it already concedes the
panic ("Panics only if the OS random source is unavailable, which would make the
whole process untrustworthy anyway") — so this is a *documented* decision, listed
here only because it is the one `expect` in the tree that a request can reach.
Verdict: acceptable as-is; do not "fix" it silently.

### D-13 — stale test baseline (P3)

`STATUS.md:137` "213 passed", `STATUS.md:165` "213 workspace tests",
`CHANGELOG.md:14` thresholds "213 → 225 → …", `MISSION-2026-10.md:59,457` the
same. Measured on both 1.98 and 1.99: **215**. The mission's first threshold is
already met, so M0's DoD numbers need recomputing.

### D-14 — a test that asserts nothing (P3)

`crates/heterion-router-gateway/src/main.rs:537`
`watch_parent_without_marker_is_a_no_op` calls `watch_parent()` and ends. A
regression that made `watch_parent` spawn a thread, or block, or return early on
the wrong condition, would still pass — the only failure mode it catches is a
panic. It is the sole `#[test]` in the tree with no `assert`/`expect`/`unwrap`/
`panic` in its body (checked exhaustively with a brace-matching scan over all
118 test functions). Minimal fix: assert the observable, e.g. that
`HETERION_ROUTER_PARENT_PID` handling returns before spawning — or delete it and
stop paying for a test that only catches panics.

### D-15 — non-openai formats dropped without a warning (P3)

Same `else { continue; }` shape as D-3, at `main.rs:244-246`
(`if !registry.is_openai_format(&provider) { continue; }`). Active connections
whose registry `format` is not `openai`/`gemini`/`openai-responses`:

```
dropped@main.rs:245 (format=claude) agentrouter x1
dropped@main.rs:245 (format=claude) kimi-coding x1
dropped@main.rs:245 (format=kiro)   kiro       x1
dropped@main.rs:245 (format=claude) zai        x1
```

Together with D-3 that is **9 of the 51 active connections silently unserved**,
out of 278 declared providers of which only 21 get a backend built from this DB.

### D-16 — parallel tests mutate process env (P3)

`db.rs:146-169` and `crypto/src/lib.rs:238-269`: four tests across two crates
call `unsafe { std::env::remove_var / set_var }` with no serialisation, while
`cargo test` runs a crate's tests on parallel threads. They pass because the
keys happen to be disjoint per crate. Any new test touching
`DATA_DIR`/`HETERION_ROUTER_DATA_DIR`/`STORAGE_ENCRYPTION_KEY` in those crates
introduces a nondeterministic failure. This is exactly the unsafety edition 2024
made explicit. Minimal fix: a `static ENV_LOCK: Mutex<()>` in each test module.

### D-17 — the pre-commit `fmt` gate is red (P3)

`AGENTS.md:25` and `scripts/ci/check.sh:20` both run
`cargo fmt --all [-- --check]`. On the current work tree that **exits 1** with
the 2 diffs in `crates/heterion-router-gateway/src/lib.rs` shown above. `HEAD`
is clean (`git stash push` of that one file makes the check exit 0), so this is
uncommitted drift, but it means the documented gate fails right now on both
1.98 and 1.99. `cargo fmt --all` fixes it.

### D-18 — desktop `fmt` debt outside every gate (P3)

`desktop/src-tauri/build.rs:2` (2-space indent) and
`desktop/src-tauri/src/lib.rs:81` (unnecessary line break). Both present at
HEAD. The crate is `exclude`d from the workspace (`Cargo.toml:24`) and CI has no
desktop job, so nothing enforces them. `AGENTS.md:30-31` documents the exclusion
honestly; the debt itself is unfixed.

### D-19 — no MSRV policy, and CI does not test the pin (P3)

No file states an MSRV policy; `cargo msrv` is neither installed nor depended on.
Separately, `.github/workflows/ci.yml:39` and `release.yml:30` use
`dtolnay/rust-toolchain@stable`, so CI compiles a moving target while the
`rust-toolchain.toml` pin exists only locally. Bumping the pin to 1.99 will be
verified by CI on whatever `stable` happens to be, not on 1.99.0.

### D-20 — needless `unwrap()` after assignment (P3)

`routing.rs:233` and `:306`: `last_error = Some(error); if retry_count < … && Self::is_transient(last_error.as_ref().unwrap())`.
`last_error` was assigned `Some` on the immediately preceding line and is not
mutated in between, so it cannot be `None`. Not a live bug — listed because
clippy's `unnecessary_unwrap` does not fire here (it cannot see through the
`if let`/`match` arm) and a future edit that hoists the read above the assignment
would panic in the request path.

### Things checked and found *correct* (so the main agent does not re-audit)

- `record_usage` / `record_call_log` are single unconditional INSERTs with
  `datetime('now')`; there is no retry path that double-writes them. The only
  `recorded` guard that could double-fire is `stream_response`'s
  (`lib.rs:404,415,430`) and `responses_stream`'s (`:1236,1255`), and both set
  the flag immediately after recording. Streaming retries inside
  `RoutingBackend::stream` happen *before* the first chunk is forwarded
  (`routing.rs:287-321`), so a retry cannot duplicate an accounting row.
- `kv_set` is a proper `ON CONFLICT … DO UPDATE` (`repos.rs:760`), and
  `upsert_connection` / `upsert_combo` are proper upserts. `set_api_key_revoked`
  returns `changed > 0`, so a repeated revoke is idempotent from the caller's
  point of view.
- `usage_summary`'s `requests` = `COUNT(*)`, `successes` = `SUM(success)` over
  the same `WHERE` — no rows-vs-distinct confusion, no JOIN to inflate anything.
  `buckets()` uses `COUNT(*)` per group, which is the right aggregate for
  "requests per provider". No `LEFT JOIN`/`DISTINCT` misuse anywhere in the file.
- `list_connections` returns `has_credential` derived from
  `[access_token, api_key].iter().flatten().any(|s| !s.is_empty())` — it never
  selects or serialises a secret value, and `GET /api/connections` only forwards
  the boolean. `SELECT`ing `access_token`/`api_key` to compute a bool is a mild
  over-fetch but not a leak.
- No secret value was printed at any point in this audit. DB access was
  exclusively `sqlite3 "file:/…/storage.sqlite?mode=ro"`; the only secret-adjacent
  facts recorded are lengths and booleans (e.g. `length(key) = 35`,
  `key_hash` length 64, `count(distinct key_hash)` = 1).
- The live DB has exactly 1 `api_keys` row, not revoked, active, unbanned — so
  D-5 and D-6 are latent, not currently manifesting.

## Commands run

All cargo commands used `CARGO_TARGET_DIR=/tmp/hra-a6`. No target directory was
deleted at any point.

```sh
# toolchain inventory
rustup toolchain list
# 1.98.0-aarch64-apple-darwin, 1.99.0-aarch64-apple-darwin, nightly, stable(=1.98.1) present
rustc --version   # rustc 1.98.1 (48a229cea 2026-09-01)  [default = stable]

# component availability (had to install, not present for 1.99.0)
rustup component add --toolchain 1.99.0-aarch64-apple-darwin clippy
rustup component add --toolchain 1.99.0-aarch64-apple-darwin rustfmt

# Part 1
CARGO_TARGET_DIR=/tmp/hra-a6 cargo +1.99.0 check --workspace --all-targets
#   exit 0 — "Finished `dev` profile [unoptimized + debuginfo] target(s) in 14.16s"
CARGO_TARGET_DIR=/tmp/hra-a6 cargo +1.99.0 clippy --workspace --all-targets -- -D warnings
#   exit 0 — no warning:/error: line in the whole output
CARGO_TARGET_DIR=/tmp/hra-a6 cargo +1.99.0 test --workspace
#   exit 0 — 215 passed, 0 failed across 18 test binaries + 10 empty doc-test targets
CARGO_TARGET_DIR=/tmp/hra-a6 cargo +1.99.0 fmt --all -- --check
#   exit 1 — 2 diffs, both crates/heterion-router-gateway/src/lib.rs
CARGO_TARGET_DIR=/tmp/hra-a6 cargo +1.98.0 fmt --all -- --check
#   exit 1 — byte-identical output to 1.99 (`diff` empty)
(cd desktop/src-tauri && CARGO_TARGET_DIR=/tmp/hra-a6 cargo +1.99.0 fmt -- --check)
#   exit 1 — 2 diffs: build.rs:1, src/lib.rs:81
(cd desktop/src-tauri && CARGO_TARGET_DIR=/tmp/hra-a6 cargo +1.98.0 fmt -- --check)
#   exit 1 — byte-identical to 1.99
CARGO_TARGET_DIR=/tmp/hra-a6 cargo +1.98.0 clippy --workspace --all-targets -- -D warnings
#   exit 0 — no warning:/error: line  ⇒ 1.99 adds nothing
CARGO_TARGET_DIR=/tmp/hra-a6 cargo +1.98.0 test --workspace
#   215 passed — identical to 1.99
(cd desktop/src-tauri && CARGO_TARGET_DIR=/tmp/hra-a6 cargo +1.99.0 check --all-targets)
#   exit 0
(cd desktop/src-tauri && CARGO_TARGET_DIR=/tmp/hra-a6 cargo +1.99.0 clippy --all-targets -- -D warnings)
#   exit 0
CARGO_TARGET_DIR=/tmp/hra-a6 cargo +1.99.0 check --workspace --all-targets --locked
#   exit 0 — Cargo.lock needs no change for 1.99

# is the workspace fmt failure work-tree drift or committed?
git stash push -q crates/heterion-router-gateway/src/lib.rs
CARGO_TARGET_DIR=/tmp/hra-a6 cargo +1.99.0 fmt --all -- --check   # exit 0 at HEAD
git stash pop -q                                               # restored, byte-identical
git show HEAD:desktop/src-tauri/build.rs                        # confirms 2-space indent is committed

# Part 2
rg -n "\bunsafe\b" crates desktop/src-tauri                      # 10 hits, tabulated above
rg -n "static mut|\bgen\b|borrowed_ref|unsafe extern|unsafe\(|#\[[a-z_]*unsafe|mem::uninitialized|deprecated_safe_2024" crates desktop/src-tauri
rg -n "deprecated|legacy|mem::uninitialized|deprecated_safe_2024" crates desktop/src-tauri
rg -n "^\s*edition|^\s*rust-version|^\s*resolver" --glob 'Cargo.toml' .   # 3 edition decls, all 2024
rg -n "\[lints|\[workspace.lints" --glob 'Cargo.toml' .                  # none
rg -in "msrv|minimum supported|rust-version" -g '!target' -g '!Cargo.lock' .
cargo msrv --version                                                        # "error: no such command: `msrv`"

# version references
rg -n "1\.98" -g '!target' -g '!Cargo.lock' .    # 19 hits / 11 files

# test census + the one assert-less test (brace-matching python scan over all #[test]/#[tokio::test])
rg -n "#\[test\]" -A2 crates docs
rg -n "\.expect\([0-9]" crates -g '*.rs' | wc -l   # 22
rg -n "\.verify\(\)" crates -g '*.rs' | wc -l      # 0  (Drop impl on wiremock MockServer verifies — checked
                                                     #  wiremock-0.6.5/src/mock_server/exposed_server.rs:489)

# Part 3 — every DB access read-only, no secret values printed
DB="file:/Users/egorich/.heterion-router/storage.sqlite?mode=ro"
sqlite3 "$DB" ".tables"
sqlite3 "$DB" "select count(*) from provider_connections;"            # 64 rows
sqlite3 "$DB" "select provider||'|'||is_active||'|'||count(*) from provider_connections group by provider, is_active order by provider;"
sqlite3 "$DB" ".schema usage_history"                                 # idx_uh_dedup etc.
sqlite3 "$DB" "select case when timestamp like '%T%' then 'ISO-T' else 'space' end fmt, count(*), min(timestamp), max(timestamp) from usage_history group by 1;"
#   ISO-T 44808 (2026-07-22T07:52:37.637Z … 2026-09-15T22:22:27.355Z) | space 332 (2026-09-17 … 2026-10-01)
sqlite3 "$DB" "select (select count(*) from usage_history where timestamp >= datetime('now','-30 days')) as code,
                      (select count(*) from usage_history where datetime(substr(timestamp,1,19)) >= datetime('now','-30 days')) as parsed;"
#   code=15663  parsed=15210   → 437 rows wrongly inside the window  (D-1)
sqlite3 "$DB" "select provider, count(*), sum(tokens_cache_read>0) from usage_history where timestamp not like '%T%' group by 1;"
#   routing|332|0  — every Rust row has zero cache tokens                (D-7)
sqlite3 "$DB" "select provider, count(*) from usage_history group by 1 order by 2 desc limit 8;"
#   openrouter 14715, gemini 7832, grok-cli 3733, … routing is the *only* Rust bucket (D-2)
sqlite3 "$DB" "select count(*), count(distinct key_hash) from api_keys where revoked_at is null;"  # 1|1
sqlite3 "$DB" "select id, length(key), length(key_hash), name from api_keys;"  # lengths only, no value
sqlite3 "$DB" "select count(*) from call_logs where combo_name is not null and timestamp not like '%T%';"  # 0 (D-9)
sqlite3 "$DB" "select sum(tokens_cache_read>0), sum(tokens_reasoning>0), count(*) from usage_history where provider='routing';"

# cross-reference registry.json ↔ live connections (python, read-only sqlite URI)
python3 -c "… registry ids / aliases vs SELECT provider,is_active,count(*) …"
#   → exa-search: 5 active, no registry entry, no alias
#   → agentrouter/kimi-coding/kiro/zai: 1 active each, format claude/kiro
#   → 21 backends buildable vs 278 declared
python3 -c "… format+base_url gate counts …"
#   → 259 pass / 2108 models; 253 have HTTP(S) / 1916 models; 6 use wss://|stdio|ACP
#     (matches AGENTS.md:63, STATUS.md:52-54, README.md:55-56 exactly)

# source reads
sed -n '…' crates/heterion-router-db/src/repos.rs      # 780-960 aggregation, 340-480 auth + timestamp parser
sed -n '…' crates/heterion-router-gateway/src/lib.rs    # 200-450, 875-950, 1225-1290
sed -n '…' crates/heterion-router-gateway/src/main.rs   # 70-300 backend selection
sed -n '…' crates/heterion-router-gateway/src/credentials.rs, admin.rs, auth.rs, routing.rs, gemini.rs
```

## Bottom line for the main agent

1. The bump is safe and needs **three** edits: `rust-toolchain.toml:2`,
   `Cargo.toml:28`, `desktop/src-tauri/Cargo.toml:9`. Nothing else shadows them.
2. `cargo fmt --all` must run as part of the bump — the gate is currently red on
   the work tree (D-17).
3. CI will not verify the pin: `ci.yml:39` / `release.yml:30` use `@stable` (D-19).
4. Three P1 defects are worth fixing in the same pass because they are one-liners
   with outsized blast radius: D-1 (wrong usage window), D-3 (a `warn!` for
   dropped providers), D-8 (277 → 278).
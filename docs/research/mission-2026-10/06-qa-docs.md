# WS-6 — Mission QA & Documentation Workstream

> Architect 6/6. Deliverable only; the canonical repo was **read** and never
> written. Written: 2026-09-29. Target: `~/ai-workstation/Projects/heterion-router`.

---

## 0. Scope, authority, and one honest caveat

**What this document is.** The QA/documentation design for the mission: test
layers, sanitized fixtures, wiremock verification, contract/property tests,
live-canary policy, CI path filters, a coverage-gap register, a documentation
update protocol, independent audit gates, and release evidence — plus a
traceability matrix from mission requirement to test/doc.

**Where the requirements come from.** No mission brief text was supplied to
this workstream. Requirements are therefore **derived from the two canonical
backlogs already committed in the repo**, and given stable IDs so the other
five architects can reconcile against their own numbering:

| ID source | Document | Anchor |
|---|---|---|
| `AUDIT-P{0,1,2}-n` | `docs/research/PROJECT-AUDIT-2026-09.md` | "Prioritized follow-up", lines 179–210 |
| `RM-n` | `docs/ROADMAP-2026-09.md` | "Prioritized backlog", lines 67–102 |
| `MR-xx` | this document | mission requirement, mapped 1:1 to the union of the above (see §10) |

`AUDIT` and `RM` overlap heavily but are not identical (the ROADMAP splits
`Retry-After`/request-id into its own item 8, and adds item 14 on at-rest
Responses encryption). Where they diverge, `MR-xx` cites both.

**Non-claim ledger (read this before quoting anything below).**

1. **No line, branch, or region coverage is measured anywhere in this
   repository.** There is no `cargo-llvm-cov`, `cargo-tarpaulin`, `grcov`,
   `cargo-insta --coverage`, or CI coverage job. `grep -rn 'proptest\|quickcheck\|criterion\|tarpaulin\|llvm-cov' crates .github`
   returns nothing. Every "coverage" number in this document is a **test-count
   or symbol-count**, never a percentage. Any doc that says "X% covered" is
   wrong and must be corrected.
2. Test counts are **measured**, not read from the prior audit — see §1.3.
   The prior audit's "213 workspace tests" claim is independently confirmed.
3. Nothing here was executed against a **live upstream provider**, a live
   SQLite data directory, or the installed launchd service. No credentials
   were used. Live facts quoted from the prior audit are marked `(prior audit,
   unverified here)`.
4. This document proposes tests. It does not implement them. Every test id in
   §9–§11 is a **specification to be written**, not a test that exists.

---

## 1. Measured baseline

### 1.1 What was executed

Read-only, against the canonical tree at commit `51dfe6d` + dirty working tree
(the tree is dirty: 35 modified, 5 untracked paths — measured with
`git status --porcelain | awk '{print substr($0,1,2)}' | sort | uniq -c`).

| Command | Purpose | Result |
|---|---|---|
| `git log --oneline -12` / `git status --porcelain` | tree identity | HEAD `51dfe6d`, branch `main`, dirty |
| `find crates -name '*.rs' \| wc -l` | source inventory | 55 `.rs` files |
| `grep -rc '#\[test\]\|#\[tokio::test\]' crates` | static test-attribute count | **213** |
| `./<test-binary> --list \| grep -c ': test$'` over all 37 prebuilt harnesses in `target/debug/deps` | **measured** test count per target | **213** (table below) |
| `grep -rn '\.verify()' --include='*.rs' crates` | wiremock assertion audit | **0 hits** |
| `grep -rn '\.expect([0-9]\+)' --include='*.rs' crates` | wiremock expectation audit | **22 hits** |
| `grep -rn 'benchmarks' target/debug/deps/<bins>` → then `--list` | distinguish libtest harnesses from the real gateway binary | avoided executing the real binary |
| `git ls-files -s scripts/ desktop/scripts/` | exec-bit audit | both `100755` |
| `git ls-files \| xargs grep -lniE 'sk-[A-Za-z0-9]{20,}\|AKIA[0-9A-Z]{16}\|-----BEGIN [A-Z ]*PRIVATE KEY'` | tracked-file secret scan | **0 hits** |

The prebuilt harnesses were compiled **2026-09-29 11:02–12:14**; every
`crates/**/*.rs` and every `Cargo.toml` has an mtime **earlier than 12:00**
(verified with `find crates -newermt '2026-09-29 12:00'`, empty result). The
binaries therefore correspond to the current source tree. `--list` does not
execute tests, so nothing was mutated.

### 1.2 Measured test matrix

| Crate | lib tests | bin tests | integration tests | total | test module anchor |
|---|---|---|---|---|---|
| `heterion-router-config` | 6 | — | — | 6 | `src/config.rs:89`, `src/settings.rs:49` |
| `heterion-router-core` | 8 | — | — | 8 | `src/chat.rs:188`, `src/format.rs:47` |
| `heterion-router-crypto` | 13 | — | — | 13 | `src/lib.rs:180` |
| `heterion-router-db` | 17 | — | — | 17 | `src/db.rs:143`, `src/migrate.rs:146`, `src/repos.rs:951` |
| `heterion-router-gateway` | 98 | 7 | 1 | **106** | lib target: 17 test modules — `src/lib.rs:1301`, `src/lib.rs:1770` + 15 more; bin target: `src/main.rs:446`; integration: `tests/upstream_harness.rs` |
| `heterion-router-http` | 8 | — | — | 8 | `src/client.rs:107`, `src/sse.rs:92` |
| `heterion-router-providers` | 14 | — | — | 14 | `src/registry.rs:164` |
| `heterion-router-routing` | 6 | — | — | 6 | `src/breaker.rs:121`, `src/router.rs:53` |
| `heterion-router-translate` | 23 | — | 6 | **29** | 6 `mod tests` + `tests/*.rs` |
| `heterion-router-usage` | 6 | — | — | 6 | `src/pricing.rs:130`, `src/records.rs:107` |
| `desktop/src-tauri` (excluded from workspace) | **0** | 0 | 0 | **0** | none — `desktop/src-tauri/src/lib.rs` is 98 LOC |
| **doc-tests** | **0** | — | — | **0** | no ` ``` ` fence in any `///` comment |
| **TOTAL** | **199** | **7** | **7** | **213** | |

Gateway decomposition (lib target, 98 tests) by source file:

| file | tests | file | tests |
|---|---|---|---|
| `src/lib.rs` (router/auth) | 25 | `src/openai.rs` | 9 |
| `src/routing.rs` | 13 | `src/gemini.rs` | 7 |
| `src/grok_cli.rs` | 11 | `src/combos.rs` | 6 |
| `src/backend.rs` | 8 | `src/restart.rs` | 4 |
| `src/credentials.rs` | 3 | `src/auth.rs` | 3 |
| `src/cors.rs` | 2 | `src/ui.rs` | 2 |
| `src/apikeys.rs` | 2 | `src/ids.rs` | 1 |
| `src/admin.rs` | **1** | `src/responses.rs` | **1** |

The last two rows are the headline finding: `src/admin.rs` is **783 LOC with
one test** (`dated_application_log_is_discovered`, `src/admin.rs:774`) and
`src/responses.rs` is **369 LOC with one test** (`completes_via_responses_api`,
`src/responses.rs:335`).

### 1.3 Reconciliation with the prior audit

`docs/research/PROJECT-AUDIT-2026-09.md:27` claims "213 workspace tests
passed". Static attribute count and measured harness count **both equal 213**,
independently. The claim is sound. The audit's phrase "0 failures" was not
re-verified here (running the suite would write to `target/`); the count is
what is confirmed.

---

## 2. Test-layer audit (current state)

Five layers exist informally; only two are enforced in CI.

| Layer | Where | Count | CI-enforced | Notes |
|---|---|---|---|---|
| **L1 unit (pure)** | `crypto`, `core`, `translate`, `routing`, `providers`, `config` | 76 | yes | Best-covered layer. No DB, no network. |
| **L2 unit (I/O-bound)** | `db` (in-memory SQLite via `Db::open_in_memory`), `usage`, `http::sse` | 31 | yes | `tempfile` is the only `db` dev-dep; all current db tests use `:memory:`. |
| **L3 transport (wiremock)** | `gateway::{openai,gemini,grok_cli,responses,backend}`, `http::client`, `tests/upstream_harness.rs` | 33 | yes | 22 `.expect(N)`, **0 `verify()`** — see §3.1 |
| **L4 HTTP-surface (axum `Router` + `tower::ServiceExt::oneshot`)** | `gateway::lib` `mod tests` + `mod responses_tests` | 25 | yes | Uses `EchoBackend` and a local `CaptureBackend`; only `lib.rs:1301`/`1770` construct a full `AppState`. |
| **L5 desktop / packaging / live** | `desktop/src-tauri` (98 LOC), `desktop/ui` (7 Svelte files), launchd, release workflow | **0** | **no** | `ci.yml` has no desktop job; `Cargo.toml:23` excludes `desktop/src-tauri`. |

### 2.1 Layer-4 harness (what already exists and should be reused)

`crates/heterion-router-gateway/src/lib.rs:1301` provides `test_router() ->
Router` over `EchoBackend`, plus a `UsageStreamBackend`. `lib.rs:1770`
provides `responses_router()` and a `CaptureBackend` that records the
`ChatCompletionRequest` that reached the backend. Dev-deps already present:
`tower 0.5` (`features=["util"]`), `http-body-util 0.1`, `wiremock 0.6`
(`crates/heterion-router-gateway/Cargo.toml:40-43`).

**Gap:** these two harness modules are private to their `#[cfg(test)]` module
and cannot be shared with a future `tests/` integration target. §9.1
specifies the extraction.

---

## 3. Wiremock audit (measured, load-bearing)

### 3.1 The `.expect(n)` finding

`wiremock 0.6.5`. Its only public verification entry point is
`MockServer::verify()` — `~/.cargo/registry/src/*/wiremock-0.6.5/src/mock_server/exposed_server.rs:334`
(`pub async fn verify(&self)`, panics on failure). Nothing verifies on drop or
on scope exit. Measured: `grep -rn '\.verify()' --include='*.rs' crates` →
**0 hits** across 22 `.expect(n)` sites:

```
crates/heterion-router-http/src/client.rs:122,149
crates/heterion-router-gateway/tests/upstream_harness.rs:25
crates/heterion-router-gateway/src/gemini.rs:608,637
crates/heterion-router-gateway/src/responses.rs:349
crates/heterion-router-gateway/src/grok_cli.rs:727,764,919,932,978,990,1000,1066
crates/heterion-router-gateway/src/openai.rs:305,346,652,692,698,736,750,830
```

**Precise framing — do not overstate this.** The `.expect(n)` *counters* are
decorative, but most of these tests are not blind: in the common shape
(`src/openai.rs:291-329` `streams_openai_sse_to_finish`) the mock is the sole
responder, so a zero-request regression would surface as a transport error and
fail the assertion. What is genuinely unverified today:

- **Retry multiplicity.** `src/routing.rs:200-260` (`complete`) and `:262-337`
  (`stream`) retry up to `config.max_retries`. No test asserts how many times a
  backend was called for a 429-then-200 sequence. `expect(1)` would catch a
  double-fire *if* it were ever asserted — it is not.
- **Negative send-counts.** `src/openai.rs:698` and `:736` use `.expect(0)` on
  `header_exists("authorization")` / on the chat path. These *are* effectively
  enforced, because a violating request gets a 500 and the body assertion
  fails. They are correct tests whose `.expect` is redundant.
- **Ordering and "never hit" mocks.** Any future mock that must *not* be
  called, where a wrong call would still produce a passing body assertion,
  is silently unverified today.

### 3.2 Sanitized-fixture audit (measured)

Six JSON fixtures, 21 722 bytes total, in `crates/heterion-router-translate/tests/fixtures/`:

| fixture | bytes | shape | consumed by |
|---|---|---|---|
| `responses_to_openai.json` | 3339 | `{state:{chat_id,created,model}, cases:[{input:{type,data}, expected}]}` | `tests/parity.rs:40` |
| `responses_tools.json` | 2924 | same shape, 5 cases (hard-asserted) | `tests/tools_parity.rs:37` |
| `openai_to_responses.json` | 5893 | `[{name, input:{model,body}, expected}]`, 4 cases (hard-asserted) | `tests/request_parity.rs:26` |
| `claude_to_openai.json` | 3345 | `{cases:[…]}` | `tests/claude_parity.rs` |
| `gemini_to_openai.json` | 3156 | `{cases:[…]}` | `tests/gemini_parity.rs` |
| `openai_to_claude.json` | 3065 | top-level array | `tests/claude_request_parity.rs` |

**Sanitization result (measured).** A regex sweep for
`api_key|authorization|bearer|token|secret|password|cookie|session_id` across
the fixture directory returns 47 hits. Every one is a **token-count field**,
not a credential:

```
"prompt_tokens" "completion_tokens" "total_tokens" "max_tokens"
"input_tokens" "output_tokens" "reasoning_tokens"
"max_completion_tokens" "max_output_tokens"
"promptTokenCount" "candidatesTokenCount" "totalTokenCount"
"input_tokens_details"/"output_tokens_details" (one each)
```

The high-entropy credential sweep (§1.1) over **all tracked files including
lockfiles** returns 0. **The JS parity fixtures are clean today.** The policy
in §9.2 exists to keep them that way, not to fix a present leak.

### 3.3 Fixture hardening gap

Two of six parity tests hard-assert their case count
(`request_parity.rs:30` `assert_eq!(cases.len(), 4)`,
`tools_parity.rs:40` `assert_eq!(fixture.cases.len(), 5)`); four do not, so
truncating their fixture to `[]` would make them pass vacuously. That is a
real regression trap for a future contributor who trims a fixture. §9.2 GAP-F2
covers it.

---

## 4. CI audit (measured)

### 4.1 `scripts/ci/check.sh` (32 lines)

Four gates, `set -eu`, accumulator `fail`:

| # | Gate | Command | Notes |
|---|---|---|---|
| 1 | exec bits | `git ls-files -s scripts/ \| grep -v '^100755 '` | Covers `scripts/` **only**. `desktop/scripts/stage-sidecar.sh` (also `100755`, verified) is **not** gated. |
| 2 | fmt | `cargo fmt --all -- --check` | Does **not** cover `desktop/src-tauri` (`Cargo.toml:23` exclude). Prior audit §"Desktop" confirms the Tauri crate has fmt drift. |
| 3 | clippy | `cargo clippy --workspace --all-targets -- -D warnings` | `--all-targets` ⇒ tests are compiled and linted. |
| 4 | test | `cargo test --workspace` | Includes doc-tests (currently 0). |

`ci.yml:41-42` runs exactly this script — the local/CI parity claim in the
header comment is **true and verified by reading both**.

### 4.2 `.github/workflows/ci.yml` — job and path-filter matrix

Two jobs: `rust` (ubuntu-latest, `dtolnay/rust-toolchain@stable` +
`Swatinem/rust-cache@v2` → `scripts/ci/check.sh`) and `ui` (node 24, npm cache
keyed on `desktop/ui/package-lock.json`, `npm ci` + `npm run build`).

Note a latent drift: `ci.yml` uses `dtolnay/rust-toolchain@stable`, but
`rust-toolchain.toml:2` pins `channel = "1.98.0"`. The workflow action
overrides the file, so **CI does not test the pinned toolchain**. Not raised
by the prior audit. → GAP-CI-4.

Path filters, applied identically to `push:main` and `pull_request`
(`ci.yml:10-26`): `Cargo.toml`, `Cargo.lock`, `crates/**`, `desktop/ui/**`,
`rust-toolchain.toml`, `scripts/ci/**`, `.github/workflows/ci.yml`.

### 4.3 `.github/workflows/release.yml`

`workflow_dispatch` only, inputs `ref` + `tag`. Matrix `ubuntu-latest,
macos-14`. Builds `-p heterion-router-gateway`, tars, then
**`sha256sum "$OUT" > "${OUT}.sha256"` (line 44)**. `sha256sum` is a GNU
coreutils binary; it is absent from stock macOS 14 runners, where the
portable spelling is `shasum -a 256`. The prior audit flagged this at line
157-158 as "not executed in this audit and is not the macOS-native spelling" —
that remains true and is **confirmed by inspection**: the step is shared by
both matrix legs, so the `macos-14` leg will fail at line 44 unless coreutils
is preinstalled. Also: `permissions: contents: write` is correct, checkout
uses `persist-credentials` unset (fine, `gh` uses `GH_TOKEN`), and there is no
`paths:` filter because it is manual-only.

### 4.4 Measured CI path-filter gaps

| # | Changed path | Triggers CI today? | Consequence |
|---|---|---|---|
| **CI-1** | `desktop/src-tauri/**` (incl. `tauri.conf.json`, `capabilities/default.json`, `Cargo.toml`) | **no** | Zero automated verification of the desktop crate. Confirmed: 0 tests, no `cargo check`, not in `check.sh`. |
| **CI-2** | `desktop/package.json`, `desktop/scripts/stage-sidecar.sh` | **no** | Sidecar staging and desktop build scripts are ungated. |
| **CI-3** | `docs/**`, `*.md`, `.github/workflows/release.yml` | **no** | No documentation gate; a broken release workflow cannot be caught by CI. |
| **CI-4** | any `crates/**` change | yes, but on **stable**, not the pinned `1.98.0` | Toolchain drift (see §4.2). |
| **CI-5** | `desktop/ui/src/**` | yes (`desktop/ui/**`) but only `vite build` | No UI test runner, no lint. `desktop/ui/package.json:6-10` defines `dev/build/preview` only — no `test` script exists. |

---

## 5. Documentation audit

### 5.1 Structure

```
README.md              102 L  quick start, ports, registry reality, doc index
AGENTS.md               61 L  layout, gates, frozen contracts, provider note
STATUS.md              134 L  identity YAML, "what works", live facts, limits
CHANGELOG.md            90 L  single "Unreleased — working tree snapshot 2026-09-29"
THIRD_PARTY_NOTICES.md  (small)
docs/
  ROADMAP-2026-09.md   169 L  sources + execution-status table + P0/P1/P2 backlog
  adr/README.md         37 L  index table of 10 ADRs + "rejected/deferred"
  adr/ADR-001..010     467 L total; 001-007 historical + factual addenda,
                              009-010 new (untracked)
  research/PROJECT-AUDIT-2026-09.md  213 L  the evidence ledger
  research/CHATHUB-2026-09.md          + chathub-evidence/ (10 files)
  history/README.md + task-c1/c2/c3-report.md   frozen upstream JS, labelled
```

### 5.2 ADR convention (observed, not documented anywhere)

ADR-002..008 use the header shape `# ADR-NNN. Title` followed by
`> Дата: … · Статус: …` then `## Контекст / ## Решение / ## Следствия`.
ADR-009 and ADR-010 (untracked, newest) use `# ADR-NNN: Title (date)` then a
bullet list `- Статус: …` / `- Контекст: …`. **Two different header
conventions coexist** in one directory and neither `docs/adr/README.md` nor
`AGENTS.md` states which is canonical. → GAP-D1.

The "factual addendum" pattern used to reconcile ADR-001..007 with the
working tree is a real, load-bearing convention (see ADR-002's
`## Фактическая граница 2026-09-29`), but it is also **undocumented as a
rule**. → GAP-D1.

### 5.3 STATUS/CHANGELOG relationship

`STATUS.md:19-21` and `docs/ROADMAP-2026-09.md:165-169` both carry the same
disclaimer that the tree is dirty and uncommitted, so neither document is a
reproducible baseline. `CHANGELOG.md:3-6` states the Rust surface has **no
released version section** and that `v*` tags are inherited frozen-OmniRoute
tags that do not point at the Rust gateway. This is deliberate and correct;
the mission must not "fix" it by inventing a version.

`STATUS.md` and the audit **agree** on every substantive finding I spot-checked
(open P0 items, the registry gap numbers 278/2717 → 259/2108 → 253/1916 → 6/192
and 19/609, the `20128`/`20129` distinction, the `20128`/`20129` Vite-proxy
hazard, the adhoc-signing caveat). Documentation quality is genuinely high;
the defect is **absence of a machine-enforced staleness check**, not accuracy.

### 5.4 Documentation that is *not* backed by any gate

No markdown link checker, no `docs:` CI job, no check that the doc-index
tables in `README.md:96-102` / `STATUS.md:126-134` / `docs/adr/README.md:10-21`
point at files that exist. The ADR index table is the highest-risk table (11
rows of hand-maintained links). → GAP-D2.

---

## 6. Prior-audit carry-forward

Verified disposition of each claim in
`docs/research/PROJECT-AUDIT-2026-09.md` that bears on QA:

| Audit claim | Line | This audit's disposition |
|---|---|---|
| "213 workspace tests passed" | 27 | **Confirmed by independent measurement** (§1.2). |
| "213 tests do not cover admin handlers in full, Responses SSE tool loop, alias construction, derived Responses URLs, desktop Rust shell" (`STATUS.md:114-116`) | 114 | **Confirmed.** `admin.rs` = 1 test / 783 LOC; `responses.rs` = 1 / 369; `src-tauri` = 0. |
| "`wiremock.expect()` assertions do not call verify" (`STATUS.md:116`) | 116 | **Confirmed and quantified**: 22 sites, 0 `verify()`. |
| `sha256sum` on the macOS leg not executed | 157 | **Confirmed by inspection**, `release.yml:44`. |
| `cargo fmt --all` does not cover the excluded Tauri crate; the Tauri crate has fmt drift | 154-155 | **Confirmed structurally** (`Cargo.toml:23`). The *drift itself* was not re-measured here. |
| 19 route paths / 21 method-handler pairs | 21 | **Confirmed by reading `lib.rs:145-172`**: 19 `.route()` calls declaring 19 distinct paths and 21 method-handler pairs (`get`×12, `post`×6, `patch`×2, `delete`×1) — `/v1/responses/{id}` carries `get`+`delete`, `/api/keys` carries `get`+`post`. |
| Registry 278 / 2717 | 22 | Not re-measured (prior audit, unverified here). Structurally consistent: the insta snapshot `…provider_id_list_snapshot.snap` is 284 lines for 278 ids + 4 header lines. |
| Live: 19 backends, launchd on `0.0.0.0:20128`, `heterion-local` unconfigured | 25,30 | **Not re-verified** — no live probes were run. Treat as `(prior audit)`. |
| `count_active_api_keys` is wrong (P2 #13, line 208) | 208 | **Confirmed and localized** — see §7.1. |

---

## 7. Coverage-gap register

Each gap: measured evidence → consequence → target layer. This register is the
input to §9 and §10.

### GAP-1 — `count_active_api_keys` under-counts live keys (correctness bug)

`crates/heterion-router-db/src/repos.rs:941-949`:

```rust
pub fn count_active_api_keys(conn: &Connection) -> Result<usize> {
    let count = conn.query_row(
        "SELECT COUNT(*) FROM api_keys WHERE revoked_at IS NULL", [], …)?;
```

It filters **only** `revoked_at IS NULL`. The authentication path,
`find_usable_api_key` (`repos.rs:356-395`), additionally honors `is_banned`,
`is_active`, and `expires_at` (via `timestamp_expired`, `repos.rs:403`), all
of which are exercised by the existing test
`usable_keys_honour_kill_switches` (`repos.rs`). Consequence: a banned, disabled
or expired key still increments `api_keys` in `GET /api/overview`
(`crates/heterion-router-gateway/src/admin.rs:68`), i.e. **the dashboard
over-reports usable keys**. Not covered by any test.

### GAP-2 — wiremock expectations unverified (§3.1)

22 `.expect(n)`, 0 `verify()`. Retry multiplicity, call ordering, and
"must-never-be-called" mocks are unverified.

### GAP-3 — admin surface: 1 test / 783 LOC

Untested handlers in `crates/heterion-router-gateway/src/admin.rs`:
`overview:53`, `combos:92`, `connections:140`, `keys:198`, `create_key:240`,
`revoke_key:318`, `restore_key:327`, `set_key_revoked:335`,
`restart_service:379`, `usage:406`, `logs:449`, `logs_app:511`,
`update_combo:606`, `update_connection:716`, plus helpers
`is_known_strategy:582`, `is_unique_violation:596`, `is_hidden:550`.
Documented contract branches with no test: `manage` scope on each, `404`
unknown id, `409` unique collision, `400` unknown strategy
(`is_known_strategy` returns false for anything but `priority`/`auto`),
`400` empty model list.

### GAP-4 — Responses backend: 1 test / 369 LOC

`crates/heterion-router-gateway/src/responses.rs` — only
`completes_via_responses_api:335`. Untested: `drive_stream:67` (the whole
streaming path), `response_to_completion:216`, `ResponsesBackend::new:38`
validation, `next_token:58`. Directly gates AUDIT-P0-2 ("add a full-stream
contract test with exactly one terminal event").

### GAP-5 — no SSE terminal-event invariant test

The streaming contract asserted in `docs/ROADMAP-2026-09.md:44` ("Нет полного
SSE lifecycle/argument deltas; tool events не SDK-safe") has **no validator**.
`lib.rs` tests assert that chunks arrive and end with `stop`
(`streaming_emits_content_then_done`), but nothing asserts: exactly one
terminal event, monotone `response.output_index`/`item_id` linkage, argument
delta assembly, or `[DONE]` framing. `heterion-router-http/src/sse.rs`
(`push:31`, `push_bytes:36`) is well unit-tested (5 tests: split frames, CRLF,
multiline data, CRLF-across-bytes, UTF-8-across-bytes) but nothing tests the
**gateway's own SSE emission**, only upstream *decoding*.

### GAP-6 — combo retry / breaker half-open untested at the behavioural level

`crates/heterion-router-routing/src/breaker.rs` has 3 unit tests
(`opens_after_threshold_and_recovers`, `failure_while_open_restarts_cooldown`,
`set_tracks_backends_independently`) exercising `allow:52`,
`record_success:60`, `record_failure:67` on `Instant` values — good, but at the
**unit** level only. `src/routing.rs:345` has 13 tests including
`breaker_skips_tripped_primary` and `only_provider_outages_are_breaker_failures`.
Missing: `max_retries`/`retry_delay_ms`/`failover_before_retry`
(`combos.rs:18-38`, parsed at `combos.rs:164-180`) exercised end-to-end; the
half-open *trial* path (`breaker.rs:52 allow` returning `true` after cooldown
and then being re-tripped by a second failure) has no dedicated test; and
`failover_before_retry: true` (the default, `combos.rs:36`) means the
`retry_delay_ms` sleep at `routing.rs:239` and `:312` is **dead code under
default config** and is never executed by any current test.

### GAP-7 — provider-alias construction untested

`crates/heterion-router-providers/src/registry.rs` tests `canonical_id`
behaviour (`aliases_resolve_to_the_provider_entry`,
`a_real_id_wins_over_a_colliding_alias`) and `src/routing.rs:156-171`
canonicalizes the prefix. But the **backend-construction** path that the audit
at `PROJECT-AUDIT-2026-09.md:83-86` calls out — "a connection stored under a
registry alias is inserted into the backend map under the alias, while routing
canonicalizes to the canonical provider id", making the connection
`servable:true` yet unroutable — has **no regression test**. → AUDIT-P1-6 /
RM-6.

### GAP-8 — derived Responses URLs unverified

`resolve_chat_url` / `resolve_responses_url` in `src/openai.rs` are tested for
"both shapes" (`chat_url_resolution_handles_both_shapes`) but the audit
(`PROJECT-AUDIT-2026-09.md:96-97`) notes GitHub Copilot-style derived URLs are
mechanically constructed and unverified against a real upstream. No test pins
the derived-URL table. Cannot be closed by an offline test — needs the live
canary (§9.5) or an explicit "unverified" annotation.

### GAP-9 — zero desktop Rust verification

`desktop/src-tauri/src/lib.rs` (98 LOC) contains logic that **is** unit-testable
off-Tauri: `sidecar_port()` (`:23-32`, env precedence
`HETERION_ROUTER_SIDECAR_PORT` → `OMNIROUTE_SIDECAR_PORT` → `"20129"`) and the
defaulting/filtering rules. It has no test crate, no `[dev-dependencies]`, and
is excluded from `cargo fmt --all`. The injected global is
`window.__HETERION_ROUTER_GATEWAY_URL__` (`lib.rs:85`) — note the Tauri
**fallback in `desktop/ui/src/api.js:44-47` is `http://127.0.0.1:20128`**,
i.e. the service port, while the sidecar is `20129` (`lib.rs:21`). That
mismatch is documented (`README.md:21`, `AGENTS.md:36`) but **not asserted by
any test**, so it can silently regress.

### GAP-10 — no documentation gate

No link check, no ADR-index integrity check, no doc staleness check (§5.4).

### GAP-11 — no property-based or fuzz layer

Zero `proptest`/`quickcheck`/`arbitrary`. The highest-risk code is
byte-oriented and is exactly the code where hand-written cases are weakest:

- `heterion-router-http/src/sse.rs::SseDecoder` — currently 5 hand-picked cases;
  a chunk-boundary property test is the obvious win.
- `heterion-router-translate/src/requests.rs::clamps_call_ids_on_character_boundaries`
  — asserts a Unicode truncation fix (CHANGELOG.md:48 "Unicode `call_id`
  truncation no longer panics") with **one** case.
- `heterion-router-crypto` `round_trips_encrypt_then_decrypt` /
  `encrypt_is_not_idempotent_reuse_safe` — no randomized nonce property.
- `heterion-router-translate/src/responses.rs::convert_event:104` — a pure
  state machine with a JS-parity oracle, i.e. the single best property-test
  target in the repo, currently 4 unit + 2 fixture tests.

### GAP-12 — parity fixtures are one-way (no regeneration path)

`crates/heterion-router-translate/tests/parity.rs:3-7` states the fixture "was
produced by running the JavaScript translator". There is **no script, no
provenance metadata, and no JS reference copy in-tree** to regenerate or
re-verify it. The frozen JS lives outside the repo
(`~/ai-workstation/Archives/OmniRoute/`, `README.md:87-90`). Consequence: a
fixture cannot be re-derived by a third party, so "JS parity" is an
**unfalsifiable claim** inside the repo.

---

## 8. Fixture registry (canonical inventory)

| id | path | kind | producer | consumer | sanitized (measured) |
|---|---|---|---|---|---|
| F-R2O | `crates/heterion-router-translate/tests/fixtures/responses_to_openai.json` | JS parity, stateful | JS `openaiResponsesToOpenAIResponse` | `tests/parity.rs:41` | yes |
| F-RT | `…/responses_tools.json` | JS parity, stateful, 5 cases | JS translator | `tests/tools_parity.rs:38` | yes |
| F-O2R | `…/openai_to_responses.json` | JS parity, request, 4 cases | JS `openaiToOpenAIResponsesRequest` | `tests/request_parity.rs:28` | yes |
| F-C2O | `…/claude_to_openai.json` | JS parity, text | JS translator | `tests/claude_parity.rs` | yes |
| F-G2O | `…/gemini_to_openai.json` | JS parity, text | JS translator | `tests/gemini_parity.rs` | yes |
| F-O2C | `…/openai_to_claude.json` | JS parity, request | JS translator | `tests/claude_request_parity.rs` | yes |
| F-SNAP | `crates/heterion-router-providers/src/snapshots/heterion_router_providers__registry__tests__provider_id_list_snapshot.snap` | insta snapshot, 284 L | `insta` | `registry.rs:344` | yes (public ids only) |
| F-REG | `crates/heterion-router-providers/registry.json` | vendored catalog, 227 599 B | upstream catalog | `include_str!` in `registry.rs` | yes (base URLs only) |
| F-EV | `docs/research/chathub-evidence/*.{json,md}` (10 files) | external research evidence | live probes 2026-09-27/29 | `docs/research/CHATHUB-2026-09.md` | **see note** |
| F-LIVE | live `~/.heterion-router/storage.sqlite` | runtime | — | — | **out of scope for CI** |

**Note on F-EV.** `docs/research/chathub-evidence/README.md:3-4` asserts "No
cookies, session tokens, Supabase keys or provider credentials are included",
and the high-entropy sweep confirms no credential-shaped strings. However the
directory **does** contain live third-party identifiers:
`chat-post.ids.json` holds three real UUIDs
(`conversationId 695e4800-…`, `streamId b972235f-…`, `messageId fe1ecdf8-…`),
and `models.json` is a 16 632-byte live snapshot of 33 third-party models with
`logoUrl` pointing at `https://app.chathub.gg/…`. These are not secrets, but
they are **unsanitized live identifiers from a third-party production system**
committed to a public repository. §9.2 GOV-3 requires an explicit decision
(pseudonymize or accept-with-rationale) rather than silence.

---

## 9. Designed workstream

### 9.1 Layer model

| Layer | Purpose | Runs in | Budget | Gate |
|---|---|---|---|---|
| **L0 static** | fmt, clippy, exec-bits, lockfile hygiene | every push/PR | < 60 s | blocking |
| **L1 pure unit** | functions, no I/O | every push/PR | < 60 s | blocking |
| **L2 in-process I/O** | `:memory:` SQLite, tempfiles | every push/PR | < 60 s | blocking |
| **L3 transport** | wiremock, `verify()`-asserted | every push/PR | < 120 s | blocking |
| **L4 HTTP surface** | axum `Router::oneshot` | every push/PR | < 120 s | blocking |
| **L5 property/fuzz** | proptest, fixed seed in CI | every push/PR, 256 cases | < 180 s | blocking |
| **L6 desktop/package** | `desktop/src-tauri` fmt+check+test, sidecar staging | path-filtered | < 240 s | blocking on `desktop/**` |
| **L7 live canary** | real provider probes | manual/nightly | minutes | **non-blocking, advisory** |

**Extraction spec (WS6-S1).** Move the layer-4 harness out of
`lib.rs:1301`/`1770` into a reusable test-support module so both unit and
integration targets can use it:

```
crates/heterion-router-gateway/src/test_support.rs   // #[cfg(any(test, feature="test-support"))]
  pub fn test_router() -> Router
  pub fn app_with_db(db: Arc<Db>) -> AppState
  pub struct CaptureBackend { pub requests: Arc<Mutex<Vec<ChatCompletionRequest>>> }
  pub async fn get/post/patch(…) -> (StatusCode, Value)
```

Expose via `[features] test-support = []` + `#[cfg(feature = "test-support")]`
so `tests/*.rs` can `use heterion_router_gateway::test_support::*`. Today
`Cargo.toml` has no `[features]` table; adding one is a public API change that
must be listed in `CHANGELOG.md`.

### 9.2 Sanitized-fixture policy

**GOV-1 — allowlist, not denylist.** Fixtures may contain only: model ids,
provider ids/aliases, HTTP paths, header *names*, JSON schema shapes, and
synthetic ids from a reserved namespace. Any value matching
`(sk-|Bearer |AKIA|eyJ[A-Za-z0-9_-]{20,}|-----BEGIN)` is a CI failure.

**GOV-2 — synthetic-id namespace.** All generated ids in fixtures and tests
must be from `00000000-0000-4000-8000-000000000000`. The existing chathub
fixture already uses "the test word `hello`" per its README; formalize.

**GOV-3 — third-party live identifiers need a decision.** F-EV (`chat-post.ids.json`,
`models.json`) must be either (a) pseudonymized to GOV-2 namespace with a
`SANITIZED.md` note recording the mapping method, or (b) explicitly accepted
with a written rationale in `docs/research/chathub-evidence/README.md`. Silent
acceptance is not an option. **Owner decision required; this workstream does
not pick.**

**GOV-4 — provenance header.** Every parity fixture gains a sibling
`fixtures/PROVENANCE.json`:

```json
{ "fixture": "responses_to_openai.json",
  "producer": "openaiResponsesToOpenAIResponse",
  "producer_version": "OmniRoute frozen JS, commit <sha>",
  "regenerated_at": "YYYY-MM-DD",
  "sanitizer": "scripts/qa/sanitize-fixtures.sh@<sha>",
  "sanitizer_version": 1 }
```

This is the fix for GAP-12: it makes "JS parity" falsifiable to a third party
who has the frozen archive, and it makes fixture edits reviewable.

**GOV-5 — fixture-change coupling.** `AGENTS.md:16` already states "Тестовые
снепшоты (`*.snap`) — часть контракта; править только вместе с кодом". Extend
the same rule to `tests/fixtures/**` and `scripts/ci/check.sh`: a diff touching
`fixtures/**` without touching a non-test `.rs` file must fail gate GATE-D2.

**Script spec (WS6-S2).** `scripts/qa/sanitize-fixtures.sh` — POSIX `sh`,
resolves repo root from `$0` like `check.sh:6`, scans `crates/*/tests/fixtures/`
+ `docs/research/**/`, fails non-zero on GOV-1 hits, exits 0 silently otherwise.
Wired into `check.sh` as a fifth gate.

### 9.3 Wiremock verification harness

**WS6-S3 — assert what the mock promised.** In every wiremock-backed test,
`MockServer::verify().await` is awaited **before** the test returns, including
on the error path. Mechanically:

```rust
// crates/heterion-router-gateway/src/openai.rs  — add to every mod test
let result = backend.complete(req).await;
server.verify().await;                 // panics with VerificationReport on mismatch
let response = result.unwrap();
```

Rationale is §3.1: today the counters are decorative; after this change they
are the assertion. This is a **behavioural strengthening of existing tests
only** — no production code changes, so the risk is that a currently-passing
test starts failing. That is the point; each failure is either a real bug or an
over-strict `expect(N)` and must be triaged individually, never blanket-relaxed.

**WS6-S4 — retry-multiplicity harness (closes GAP-2 + AUDIT-P1-7).** In
`crates/heterion-router-gateway/src/routing.rs` `mod tests` (`:345`), add a
`CountingBackend { calls: Arc<AtomicUsize>, fail_first: usize, status: u16 }`
and four tests:

1. `transient_429_retries_up_to_max_retries` — `max_retries: 2`, backend fails
   429 twice then 200 ⇒ `calls == 3`, result `Ok`.
2. `failover_before_retry_skips_same_backend` — default `failover_before_retry:
   true` (`combos.rs:36`) ⇒ `calls == 1` per candidate, 2 candidates tried.
3. `retry_delay_ms_sleeps_only_when_not_failing_over` — exercises
   `routing.rs:239`/`:312`, currently dead under defaults; use
   `retry_delay_ms: 1` and `tokio::time::timeout(200ms, …)`.
4. `non_transient_4xx_does_not_retry` — 400 ⇒ `calls == 1`
   (this pins the `AGENTS.md:38` contract "4xx не роняют breaker").

**WS6-S5 — half-open trial (closes GAP-6).** `breaker.rs` `mod tests` (`:121`):
`half_open_allows_one_trial_then_retrips_on_failure` — drive
`allow(now)` false until cooldown, true after, `record_failure` re-opens,
`allow` false again until a second cooldown.

### 9.4 Contract and property tests

**WS6-S6 — Responses stream validator (closes GAP-5, AUDIT-P0-2).** New file
`crates/heterion-router-gateway/tests/responses_stream_contract.rs`, integration
target (needs `test_support`, §9.1). Properties asserted over the emitted SSE
byte stream of `POST /v1/responses` with `stream: true`:

- P1 exactly one terminal event (`response.completed` **or**
  `response.failed`), and it is last;
- P2 `response.created` is first when present;
- P3 `output_index` is monotone non-decreasing; `item_id` unique per item;
- P4 each `output_item.added` has a matching `output_item.done` with equal
  `output_index`;
- P5 tool-call argument deltas concatenate to a single JSON object per call;
- P6 terminal usage totals ≥ sum of streamed deltas;
- P7 the stream is terminated by `[DONE]` and contains no raw upstream error
  text (closes AUDIT-P0-4 "Sanitize SSE errors" with a wire-level regression
  test, `ROADMAP:77`).

Driven by `wiremock` + `test_support`, no network.

**WS6-S7 — alias-construction regression (closes GAP-7, AUDIT-P1-6).**
`tests/alias_construction.rs`: seed a `provider_connections` row whose
`provider` is a registry *alias* (e.g. `ds`), build backends exactly as
`main.rs` does, then assert:

- the backend map contains the **canonical** id (not the alias) — or, if that is
  the intended fix, that `/v1/models` reports it `servable: false`;
- `GET /v1/models` and routing agree (no `servable:true`-but-unroutable pair).

This test must be written **after** the P1-6 fix lands, and it must be written
**to fail before** it — that is the audit gate (§9.8, GATE-B3).

**WS6-S8 — property tests.** Add `proptest = "1"` to `[dev-dependencies]` of
`heterion-router-http`, `heterion-router-translate`, `heterion-router-crypto`.

| id | target | property |
|---|---|---|
| P-SSE | `sse.rs::SseDecoder::push_bytes` | For any valid SSE byte stream and **any** chunk-splitting into 1..n pieces, the concatenated event list is identical to the unsplit decode. (Directly generalizes the 5 existing hand cases.) |
| P-CALLID | `requests.rs::clamp_call_id` | Output never panics; output is a prefix of input; never splits a UTF-8 char; length ≤ limit. |
| P-CRYPT | `crypto` round-trip | `decrypt(encrypt(x)) == x` for arbitrary `x`; ciphertext never equals plaintext; two encryptions of the same `x` differ (nonce freshness). |
| P-RESP | `translate::responses::convert_event` | The state machine is **total**: no input `event_type`/`data` combination panics; and a replay of a recorded JS fixture yields the identical output sequence (a second, independent parity oracle). |

`proptest` config in CI: `PROPTEST_CASES=256`, fixed `ProptestConfig::seed`
committed so CI failures are reproducible.

**WS6-S9 — admin surface sweep (closes GAP-3, AUDIT-P2-11).** New
`crates/heterion-router-gateway/tests/admin_contract.rs` with a table-driven
`#[tokio::test]` set, one per `admin.rs` handler in §7 GAP-3, each asserting
the four documented outcomes (`200`, `401` no key, `403` non-`manage` key,
`400`/`404`/`409` per the doc comments at `admin.rs:604-605`). Includes
`count_active_api_keys` correctness (GAP-1) asserted through
`GET /api/overview` with a banned/expired/disabled key present.

### 9.5 Live-canary policy

**WS6-S10.** Live probes are **advisory only**, never merge-blocking, never on
PR. Rationale: the repo's own principle (`ROADMAP:104-114` #1 "Fail closed",
and the audit's refusal to add ChatHub as an undocumented upstream,
ADR-010) is that unverifiable external state must not gate correctness.

| Rule | Statement |
|---|---|
| CAN-1 | Canary runs from a **manual** `workflow_dispatch` job (or a scheduled job) and writes an artifact; it never sets a required status check. |
| CAN-2 | Credentials come from repository secrets only, scoped to the canary job. No credential is ever written to a fixture, a log, or an artifact. |
| CAN-3 | Canary requests are **non-mutating**: `GET /healthz`, `GET /v1/models`, one minimal `POST /v1/chat/completions` (`max_tokens: 1`), one `POST /v1/responses` with `store: false`. No combo/connection/key mutation. |
| CAN-4 | Every canary response is **scrubbed** by `scripts/qa/scrub-canary.sh` (strip `authorization`, `set-cookie`, `api_key`, and any value matching GOV-1) **before** it is uploaded as an artifact. A raw response must never leave the runner. |
| CAN-5 | Canary covers exactly what unit tests structurally cannot: derived Responses URLs (GAP-8), real keyless-provider behavior, real SSE framing from a vendor. Its output is recorded **as "unverified upstream"** in `docs/research/`, never as a green gate. |
| CAN-6 | A canary **failure never rewrites a fixture**. Fixtures come from the frozen JS (GOV-4), not from a live vendor, or "parity" silently becomes "whatever the vendor did today". |
| CAN-7 | Rate/ceiling: ≤ 6 requests per run, ≤ 1 run/day, ≤ 20 tokens per request. |

**Canary script spec (WS6-S11).** `scripts/qa/canary.sh`, POSIX `sh`,
`set -eu`, requires `HETERION_ROUTER_CANARY_BASE_URL` + `…_KEY`; exits `0`
always (advisory), writes `artifacts/canary/<UTC>.json` post-scrub.

### 9.6 CI path filters and jobs (closes GAP-CI-1..5)

Replace `ci.yml`'s duplicated filter block with a single shared list. Full
filter set for both `push:main` and `pull_request`:

```yaml
paths: &paths
  - "Cargo.toml"
  - "Cargo.lock"
  - "rust-toolchain.toml"
  - "crates/**"
  - "desktop/**"            # was only desktop/ui/**  (CI-1, CI-2, CI-5)
  - "scripts/**"            # was only scripts/ci/**  (CI-2)
  - ".github/workflows/**"   # now includes release.yml (CI-3)
  - "docs/**"               # doc gate (CI-3)
  - "*.md"
  - "AGENTS.md"
  - "STATUS.md"
  - "README.md"
```

Job graph:

| job | runs-on | when | commands | gating |
|---|---|---|---|---|
| `gates` | ubuntu-latest | always | `scripts/ci/check.sh` | required |
| `ui` | ubuntu-latest | always | `npm ci` + `npm run build` in `desktop/ui` | required |
| `desktop-rust` | macos-14 | path-filtered | `cargo fmt --manifest-path desktop/src-tauri/Cargo.toml -- --check`; `cargo clippy --manifest-path … --all-targets -- -D warnings`; `cargo test --manifest-path …` | required (closes CI-1, and `PROJECT-AUDIT:154-156`) |
| `docs` | ubuntu-latest | path-filtered | `scripts/qa/check-docs.sh` (link + ADR index + stale-claim scan, §9.7) | required |
| `property` | ubuntu-latest | always | `PROPTEST_CASES=256 cargo test --workspace --release --test '*'` | required |
| `canary` | ubuntu-latest | `workflow_dispatch` | `scripts/qa/canary.sh` | **advisory** |

Also: pin CI to the file's toolchain. Replace
`uses: dtolnay/rust-toolchain@stable` with
`uses: dtolnay/rust-toolchain@1.98.0` (or drop the file's `channel` pin) —
currently CI tests a different compiler than a contributor gets locally
(GAP-CI-4).

**`check.sh` amendment.** Gate 1's `git ls-files -s scripts/` becomes
`git ls-files -s scripts/ desktop/scripts/`. Gates 5 and 6 added: fixture
sanitize (§9.2 GOV-1/GOV-5) and docs check (§9.7).

### 9.7 Documentation update protocol

**DOC-1 — every behavior change lands with a doc delta in the same PR.**
Required per change class:

| Change | Doc that must change |
|---|---|
| Route added/removed/renamed | `README.md` architecture block, `STATUS.md` "Что реально работает", new ADR or ADR addendum |
| Env var added/renamed | `AGENTS.md` contracts, `crates/heterion-router-config/src/config.rs` fallback table, ADR addendum |
| Wire-format or stored-data change | `AGENTS.md` "контракты, которые нельзя ломать молча", `CHANGELOG.md` **Compatibility** section |
| Security boundary change | new ADR (next free number is **ADR-011**) + `STATUS.md` limitations table |
| Registry content change | `insta` snapshot + `README.md` registry counts + `STATUS.md` counts — **all four**, together |
| Test-matrix change | `STATUS.md` "Проверки" + the count in `docs/ROADMAP-2026-09.md:32` |
| New known limitation | `STATUS.md` limitations + `PROJECT-AUDIT` follow-up list |

**DOC-2 — the registry counts are a 4-file invariant.** `278 / 2717`,
`259 / 2108`, `253 / 1916`, `6 / 192`, `19 / 609`, `61 / 959` currently appear
in `README.md:55-58`, `STATUS.md:47-52`, `ROADMAP:28`, and
`PROJECT-AUDIT:22-24`. Nothing keeps them in sync.
**WS6-S12:** extend the existing insta snapshot at
`crates/heterion-router-providers/src/registry.rs:344` to emit all six numbers,
and have `scripts/qa/check-docs.sh` fail when any of the six differs from the
snapshot. The snapshot already pins the 278 provider ids, so this extends an
existing mechanism rather than inventing one.

**DOC-3 — ADR header convention.** Normalize on the ADR-009/010 shape
(`# ADR-NNN: Title (YYYY-MM-DD)`, then `- Статус:`, `- Контекст:`), or
explicitly bless the 002-008 shape in `docs/adr/README.md`. Pick one, write it
in `docs/adr/README.md`, and add `scripts/qa/check-docs.sh` rule: every
`docs/adr/ADR-*.md` matches the chosen header regex. → GAP-D1.

**DOC-4 — ADR addendum convention becomes explicit.** The
`## Фактическая граница YYYY-MM-DD` pattern (ADR-002 tail, ADR-006, ADR-007,
ADR-008) is currently an unwritten rule. Write it into `docs/adr/README.md`:
an ADR is never edited to change a decision; drift from the working tree is
recorded as a dated addendum.

**DOC-5 — no invented versions.** `CHANGELOG.md:3-6` correctly states the Rust
surface has no released version and that `v*` tags are frozen-upstream. The
mission must keep that. New entries go under `## Unreleased`; release-tag
naming stays `gateway-YYYYMMDD-<sha>` (`release.yml:47`).

**DOC-6 — no status claim without an artifact.** Every "works / verified"
sentence in `STATUS.md` must name the artifact that proves it (a test name, a
command + its output, or a dated probe). The prior audit already models this
(`PROJECT-AUDIT:16-31` "Verified baseline" table). WS6-S13 adds a lint: flag
`STATUS.md` lines matching `(?i)(покрыт|verified|works|работает)` that do not
cite a backticked test or command.

**DOC-7 — `scripts/qa/check-docs.sh`.** Fails on: (a) broken relative links
in `*.md`; (b) `docs/adr/README.md` index rows whose target file is missing or
whose number is duplicated; (c) DOC-2 registry-count mismatch; (d) DOC-6
uncited-claim heuristic; (e) any `docs/**/*.md` naming a port, path or env var
that no longer exists in the source (a cheap grep-based staleness probe —
`20128`, `20129`, `HETERION_ROUTER_*`, `OMNIROUTE_*`).

### 9.8 Independent audit gates

Quality gates that an implementer cannot satisfy by editing their own diff.

| gate | Who | How | Blocks |
|---|---|---|---|
| **GATE-A1 — test-first** | implementer | the PR's first commit adds a *failing* test (`git show <first> \| grep '^+.*#\[tokio::test\]'` with no production `src/*.rs` change) | merge |
| **GATE-A2 — mutation spot-check** | independent auditor | pick ≥ 3 assertions from the PR's new tests, invert the asserted value, confirm the test fails, then revert | merge |
| **GATE-A3 — fixture provenance** | independent auditor | every touched fixture has a GOV-4 `PROVENANCE.json` entry whose `producer_version` matches the frozen archive | merge |
| **GATE-A4 — wiremock honesty** | independent auditor | every touched wiremock test contains `verify().await`; every `.expect(n)` is either verified or justified in a comment | merge |
| **GATE-B1 — no unmeasured coverage claim** | independent auditor | `git diff` contains no percentage-coverage language unless a `llvm-cov`/`tarpaulin` artifact is attached | merge |
| **GATE-B2 — no live-fixture laundering** | independent auditor | `git diff --name-only` on `tests/fixtures/**` contains no data that appears verbatim in a live response captured that day | merge |
| **GATE-B3 — red-then-green** | independent auditor | for each bug-fix test: `git stash` the src change ⇒ test fails; restore ⇒ passes | merge |
| **GATE-C1 — doc-truth** | independent auditor | every `STATUS.md` / `ROADMAP` sentence changed is traceable to a test id or command output in the same PR | merge |
| **GATE-C2 — count consistency** | independent auditor | registry counts, test count and port numbers are identical across `README.md`, `STATUS.md`, `ROADMAP`, `PROJECT-AUDIT` | merge |
| **GATE-D1 — independent re-audit** | a *different* architect | re-run the §1.1 measurement table on the post-merge tree; diff against the PR's own claims | mission close |

GATE-D1 is the direct successor to this document: it is the mechanism by which
the mission's own coverage claims get falsified rather than trusted.

### 9.9 Release-evidence bundle

`release.yml` currently emits two files (`.tar.gz`, `.sha256`) and nothing
else. Proposed `artifacts/` set uploaded to the same release:

| artifact | content | why |
|---|---|---|
| `heterion-router-gateway-<triple>.tar.gz` | binary | existing |
| `…sha256` | checksum, via **`shasum -a 256`** on macOS / `sha256sum` on Linux, selected by `uname` (closes GAP `release.yml:44`) | existing, fixed |
| `evidence/gates.txt` | full stdout of `scripts/ci/check.sh` | proves the tree that was built was gated |
| `evidence/manifest.txt` | `git rev-parse HEAD`, `rustc -Vv`, `cargo -V`, host triple, `date -u` | reproducibility |
| `evidence/tests.txt` | `cargo test --workspace -- --format terse` listing all N tests | makes the test count auditable without running anything |
| `evidence/docs.txt` | `scripts/qa/check-docs.sh` output | doc gate result |
| `evidence/coverage.txt` | **omitted unless a real coverage tool is added** | honesty: see §0 non-claim 1 |
| `RELEASE-NOTES.md` | generated from `CHANGELOG.md` `## Unreleased`, not hand-written | prevents note/artifact drift |

Release checklist (manual, enforced by reviewer):
`ref` is a tagged commit; `evidence/manifest.txt` HEAD equals the tag; the
SHA-256 recorded in the release body equals `…sha256`; `evidence/gates.txt`
shows `check.sh: all green`; the desktop bundle is **not** published (it is
adhoc-signed and not notarized — `README.md:40-41`, `PROJECT-AUDIT:139-141`).

---

## 10. Traceability matrix

`MR-xx` are the mission requirements (derived, §0). **Status** is measured:
`EXISTS` = a test/doc exists today and I verified it; `GAP-n` = no such
artifact, target specified; `PARTIAL` = something exists but the property is
unverified.

| MR | Requirement | Source | QA/doc artifact (target) | Status now |
|---|---|---|---|---|
| MR-01 | Preserve Responses `reasoning` input items **or** return `400 unsupported_capability`; `store:false` two-turn replay test | `AUDIT-P0-1`, `RM-1` | `tests/responses_stream_contract.rs` WS6-S6 (P3/P4); doc: ADR-011 + `STATUS.md` limits | **GAP-5** — no test; `lib.rs` has `store_false_does_not_create_retrievable_state` but no two-turn replay |
| MR-02 | Typed Responses stream lifecycle + validator asserting ids, indices, exactly one terminal event | `AUDIT-P0-2`, `RM-2` | WS6-S6 P1–P6 (new `tests/responses_stream_contract.rs`); doc: ADR-011 | **GAP-5** |
| MR-03 | Reject unsupported built-in tools instead of dropping them | `AUDIT-P0-3`, `RM-3` | WS6-S6 + a negative `tests/` case; doc: ADR-011, `STATUS.md:101` | **GAP-5** (no test either way) |
| MR-04 | Sanitize SSE errors; wire-level regression test | `AUDIT-P0-4`, `RM-4` | WS6-S6 P7; doc: ADR-011, `STATUS.md:107` | **GAP-5** |
| MR-05 | Bound/encrypt/prune Responses state; gateway-owned storage keys | `AUDIT-P0-5`, `RM-5`, `RM-14` | unit tests on the store in `lib.rs` (`stored_responses_are_scoped_to_the_creating_key` exists, retention does not); doc: ADR-011 + `CHANGELOG` Compatibility | **PARTIAL** |
| MR-06 | Canonicalize provider ids when building backends; report unreachable registry entries | `AUDIT-P1-6`, `RM-6` | WS6-S7 `tests/alias_construction.rs`; GATE-B3; doc: ADR-011, `STATUS.md:104` | **GAP-7** |
| MR-07 | Real provider/connection/combo attribution + real status/error codes in usage & call logs | `AUDIT-P1-7`, `RM-7` | `usage` integration test asserting the written rows; doc: `STATUS.md:61-64` | **GAP-3** (only `usage_row_round_trip` exists) |
| MR-08 | Propagate upstream request-id and `Retry-After`; honour it in retry policy | `AUDIT-P1-7`, `RM-8` | WS6-S4.3 (`retry_delay_ms` path); doc: ADR-011 | **GAP-6** |
| MR-09 | Sidecar readiness/ownership handshake; align `api.js` and Vite ports | `AUDIT-P1-8`, `RM-9` | desktop unit test on `sidecar_port()` (`desktop/src-tauri/src/lib.rs:23`); `desktop/ui` test runner; doc: ADR-011 | **GAP-9** |
| MR-10 | Commit/install a launchd plist or remove service claims from the quick start | `AUDIT-P1-9`, `RM-10` | DOC-7(e) staleness probe; doc: `README.md:23-31`, ADR addendum | **GAP-10** |
| MR-11 | Add admin, alias-construction, combo-retry, breaker half-open, CORS/dashboard, desktop-Rust tests | `AUDIT-P2-11`, `RM-11` | WS6-S4 (retry), WS6-S5 (half-open), WS6-S7 (alias), WS6-S9 (admin, `tests/admin_contract.rs`), GAP-9 (desktop), CI job `desktop-rust` | **GAP-2/3/6/7/9** |
| MR-12 | Implement or remove `maxSetRetries`, `setRetryDelayMs`, `weight`, `strategy:auto` | `AUDIT-P2-12`, `RM-12` | WS6-S4.2 (dead-code path proven); doc: `STATUS.md:43-46`, `README.md:62-64` | **GAP-6** — parsed at `combos.rs:164-180`, never read by routing |
| MR-13 | Fix `count_active_api_keys`; portable release checksum; desktop CI | `AUDIT-P2-13`, `RM-13` | WS6-S9 (asserts through `GET /api/overview`); §9.9 `shasum -a 256`; CI job `desktop-rust` | **GAP-1 / GAP-CI-1 / release.yml:44** |
| MR-14 | Decide at-rest encryption for stored Responses envelopes | `RM-14` | doc-only: ADR-011 | **PARTIAL** — `PROJECT-AUDIT:122` records the risk, no decision |
| MR-15 | Independent audit gate | this workstream §9.8 | GATE-A1..D1; evidence bundle §9.9 | **new** |
| MR-16 | Documentation protocol: ADR convention, provenance, count invariants, no invented versions, no uncited claims | this workstream §9.7 | `scripts/qa/check-docs.sh` + `docs/adr/README.md` updates + `docs/**` in `ci.yml` paths | **GAP-D1 / GAP-D2** |
| MR-17 | Sanitized fixtures + provenance + third-party identifier decision | this workstream §9.2 | `scripts/qa/sanitize-fixtures.sh`, `fixtures/PROVENANCE.json`, GATE-A3, GATE-B2 | **PARTIAL** — measured clean today; governance absent |
| MR-18 | Live-canary policy | `PROJECT-AUDIT:96-98` (derived URLs unverified), §9.5 | `scripts/qa/canary.sh` + advisory CI job + scrub script | **new** |

### 10.1 Docs → requirement index (inverse)

| Document | Requirements it currently carries | Required change under this workstream |
|---|---|---|
| `STATUS.md` | MR-01…MR-14 (all limits listed, lines 99-122) | limits → closed/reduced after each gate; count invariants per DOC-2 |
| `docs/ROADMAP-2026-09.md` | MR-01…MR-14 backlog (lines 67-102) | Execution-status table gains a "gate" column |
| `docs/adr/` | MR-09, MR-10 (002/006/007/008 addenda), MR-14 | header convention (DOC-3); addendum convention (DOC-4); ADR-011 for MR-01…MR-06 |
| `CHANGELOG.md` | all | Unreleased entries per PR (DOC-1); Compatibility section for wire/storage changes; **no invented version** (DOC-5) |
| `AGENTS.md` | frozen contracts (lines 29-38) | extend fixture-coupling rule to `tests/fixtures/**` (GOV-5) |
| `docs/research/PROJECT-AUDIT-2026-09.md` | the evidence ledger | superseded in place by GATE-D1 re-runs; do not delete, append |
| `README.md` | ports, registry counts, quick start | DOC-2 counts; MR-10 plist decision |
| `docs/research/chathub-evidence/` | MR-17 GOV-3 | explicit sanitize/accept decision |

---

## 11. Commands and expected artifacts

Run from the repo root. All read-only except where marked `[write]`.

```sh
# --- baseline re-measurement (read-only) -----------------------------------
cargo fmt --all -- --check                       # L0; PASS (prior audit)
cargo clippy --workspace --all-targets -- -D warnings   # L0; PASS (prior audit)
cargo test --workspace                           # 213 tests expected; PASS (prior audit)
PROPTEST_CASES=256 cargo test --workspace        # + WS6-S8 once added

# --- counts this document asserts ------------------------------------------
grep -rc '#\[test\]\|#\[tokio::test\]' crates | awk -F: '{s+=$2} END {print s}'   # -> 213
grep -rn '\.verify()' --include='*.rs' crates    # -> 0 hits  (must become N>0)
grep -rn '\.expect([0-9]\+)' --include='*.rs' crates | wc -l                      # -> 22
grep -rn --include='*.rs' 'proptest|tarpaulin|llvm-cov|criterion' crates           # -> 0 hits
grep -rn '#\[test\]' desktop/src-tauri/src/     # -> 0 hits

# --- workspace gates -------------------------------------------------------
scripts/ci/check.sh                              # -> "check.sh: all green"

# --- new gates (to be authored) -------------------------------------------
scripts/qa/sanitize-fixtures.sh                   # exit 0 silent, exit 1 on GOV-1
scripts/qa/check-docs.sh                          # exit 1 on DOC-2/DOC-3/DOC-7 violation
scripts/qa/canary.sh                              # always exit 0 (advisory)
scripts/qa/scrub-canary.sh <in.json > out.json    # strips GOV-1 patterns
PROPTEST_CASES=256 cargo test --workspace --test responses_stream_contract
cargo test -p heterion-router-gateway --test admin_contract
cargo test --manifest-path desktop/src-tauri/Cargo.toml   # after MR-11

# --- live (advisory, manual, credentials from secrets only) ----------------
HETERION_ROUTER_CANARY_BASE_URL=… HETERION_ROUTER_CANARY_KEY=… scripts/qa/canary.sh
```

**Expected artifacts after the full workstream:**

| artifact | path |
|---|---|
| stream contract test | `crates/heterion-router-gateway/tests/responses_stream_contract.rs` |
| admin contract test | `crates/heterion-router-gateway/tests/admin_contract.rs` |
| alias construction test | `crates/heterion-router-gateway/tests/alias_construction.rs` |
| shared harness | `crates/heterion-router-gateway/src/test_support.rs` |
| desktop test target | `desktop/src-tauri/tests/sidecar_port.rs` |
| fixture provenance | `crates/heterion-router-translate/tests/fixtures/PROVENANCE.json` |
| QA scripts | `scripts/qa/{sanitize-fixtures,check-docs,canary,scrub-canary}.sh` |
| CI | `.github/workflows/ci.yml` (6 jobs), `release.yml` (evidence upload) |
| docs | `docs/qa/TEST-MATRIX.md` (generated table of §1.2), ADR-011, updated `docs/adr/README.md` |
| evidence | `artifacts/evidence/{gates,manifest,tests,docs}.txt` on each gateway release |

---

## 12. Explicit non-claims

Repeated here so no downstream document over-reads §1–§11.

1. **No coverage percentage is claimed or implied.** There is no coverage tool
   in this repository. §1.2 counts **tests**, not covered lines.
2. **The 213 tests were not re-run.** The count was measured from compiled
   libtest harnesses built from the current sources; "0 failures" is inherited
   from the prior audit, not re-verified here.
3. **Live-service and live-provider facts are inherited, not re-measured**:
   19 backends, launchd on `0.0.0.0:20128`, `heterion-local` unconfigured,
   8 combos with 2 empty, the 278/2717 registry numbers.
4. **The fmt drift in `desktop/src-tauri` is inherited** from
   `PROJECT-AUDIT:154-155`; only the structural exclusion was re-verified.
5. **Every test in §9.4, §9.5, §9.6 is a specification.** None is written.
   Test ids (`WS6-S1`…`WS6-S13`) name work items, not existing files.
6. **`count_active_api_keys` (GAP-1) is a static reading of the SQL**, not an
   executed demonstration. The correct query is defined by
   `find_usable_api_key`'s existing test `usable_keys_honour_kill_switches`.
7. **The wiremock analysis is source-level**, plus a lookup of the vendored
   `wiremock-0.6.5` API surface confirming that `MockServer::verify()` is the
   only public verification entry point.

---

## 13. Open questions for the other architects

| # | Question | Blocks | Owner |
|---|---|---|---|
| Q1 | Are MR-01…MR-14 the canonical mission requirement IDs? If another workstream numbered them differently, this matrix must be re-pointed before anything is implemented. | all of §10 | mission lead |
| Q2 | `MR-11` explicitly requires desktop Rust in CI. Desktop tests on a `macos-14` runner cost ~4 min and need `libwebkit2gtk` system deps for a full `cargo tauri build` — but `cargo test --manifest-path desktop/src-tauri/Cargo.toml` may not need them. Which is it? | WS6 CI job | architect owning L6 |
| Q3 | Is MR-14 (at-rest encryption of Responses envelopes) a code decision or a doc-only decision? If code, it needs a new dev-dependency (`aead` feature coverage) and changes the storage contract. | WS6 traceability | architect owning storage |
| Q4 | `F-EV` (GOV-3): pseudonymize `chat-post.ids.json` + `models.json`, or accept the live third-party identifiers in a public repo with a written rationale? This is a policy call, not a QA call. | WS6-S2 | mission lead / repo owner |
| Q5 | Should `desktop/ui` gain a test runner? There is no `test` script in `desktop/ui/package.json:6-10`, and adding one adds a JS toolchain the project deliberately avoids (`AGENTS.md:4-5`). Alternatives: (a) node `--test` with zero deps, (b) extract `apiBase()` into a pure module and test it from Rust via a fixture, (c) no UI tests, and rely on DOC-7. | MR-09, GAP-9 | architect owning the UI |
| Q6 | Proptest or a hand-rolled deterministic fuzzer for `sse.rs`? `PROPTEST_CASES=256` adds a dev-dep to three crates; a seeded in-crate loop adds none. | WS6-S8 | mission lead |
| Q7 | The dirty tree (35 modified, 5 untracked) means `STATUS.md:19-21` and `ROADMAP:165-169` both disclaim reproducibility. Should the mission's first act be a **commit-only** PR before any of §9 work? I recommend yes — otherwise every gate in §9.8 measures a tree nobody can check out. | all | mission lead |

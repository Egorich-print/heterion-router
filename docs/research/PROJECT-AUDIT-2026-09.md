# Heterion Router — project audit (2026-09-29)

## Snapshot and method

This document consolidates six parallel read-only audits of the working tree at
`~/ai-workstation/Projects/heterion-router`.

- Branch: `main`; last commit at audit start: `51dfe6d`.
- The tree was already dirty before this audit (modified code and untracked
  documents). Findings describe the working tree, not a released commit.
- No credentials, production mutations, or canonical source edits were made by
  the auditors. Runtime probes were read-only.
- Sources: `crates/`, `desktop/`, `.github/`, `scripts/`, SQLite runtime data,
  public upstream documentation, and executed build/test commands.

## Verified baseline

| Area | Result |
|---|---|
| Workspace | 10 Rust crates; `desktop/src-tauri` intentionally excluded |
| Gateway | axum; 19 route paths / 21 method-handler pairs; `/v1/*` requires an API key when keys exist |
| Registry | 278 providers / 2717 model entries embedded at compile time |
| Registry structure | 259 providers / 2108 models pass the builder's format + non-empty base-URL gate; 253 / 1916 have HTTP(S) base URLs |
| Registry transport gaps | 6 / 192 structurally pass but use `wss://`, stdio or CLI/ACP schemes; 19 / 609 fail the format/base-URL gate |
| Live snapshot | 19 backends built; `heterion-local` has no active connection row |
| Combos | 8 rows; `vivanta-auto` and `vivanta-deepseek` have empty model lists |
| Quality gates | `cargo fmt --all -- --check`, workspace clippy with `-D warnings`, and 213 workspace tests passed |
| UI | `npm --prefix desktop/ui run build` passed (Vite 6.4.3) |
| Tauri | `cargo check --manifest-path desktop/src-tauri/Cargo.toml --locked` passed; full local Tauri build produced an app/dmg but is adhoc-signed |
| Live service | launchd `com.heterion.router` listens on `0.0.0.0:20128`; `/api/*` and `/v1/*` return 401 without a key |

## Architecture as built

```text
OpenAI clients / cURL / desktop
              │
              ▼
  axum gateway :20128 (service) / :20129 (sidecar/dev)
  ├─ /healthz, /, /{*path}       public static/health surface
  ├─ /v1/models, /v1/chat/completions, /v1/responses[/{id}]
  └─ /api/*                       manage-scoped admin surface
              │
              ▼
  Authenticated → RoutingBackend
  ├─ provider/model resolution
  ├─ combo expansion from SQLite
  ├─ breaker (4xx does not trip it)
  └─ retry/failover for 429/5xx
              │
  ┌───────────┼────────────┬──────────────┬──────────────┐
  ▼           ▼            ▼              ▼              ▼
OpenAI      Responses     Gemini        Grok CLI       Echo
Chat        backend      backend        backend        (explicit opt-in)
  │
  ├─ reqwest HttpClient + byte-safe SSE decoder
  ├─ SQLite ~/.heterion-router/storage.sqlite (WAL)
  └─ provider_connections / api_keys / combos / key_value / usage tables
```

The `heterion-local` code path is present, but the live database has no active
`heterion-local` connection, so it is not among the 19 running backends.

## Provider reality

The registry is a catalog, not a promise that every entry is executable.

- `executor` is metadata only; runtime construction branches on provider
  `format`. 61 HTTP-capable providers / 959 models use a generic executor
  instead of their declared bespoke executor. A further 6 structurally
  constructible providers / 192 models use non-HTTP schemes and will fail at
  request time.
- 19 providers / 609 models (including all 10 `claude`-format providers) are
  not constructed because their format or base URL fails the builder gate.
  Another 6 / 192 are constructed structurally but use unsupported transport
  schemes. Neither group is a working HTTP provider.
- `claude` translators have JS-parity text tests but no production backend and
  no `/v1/messages` route.
- `zai` is skipped because its provider format is `claude`, although some of its
  models declare an OpenAI target format.
- Responses transport selection does work for models marked
  `target_format: "openai-responses"`; the old roadmap statement that xAI always
  used Chat Completions was false against the current working tree.
- A connection stored under a registry alias (for example `ds`) is inserted
  into the backend map under the alias, while routing canonicalizes to the
  canonical provider id. The connection can therefore report `servable: true`
  yet be unroutable and absent from `/v1/models`.

### Verified transport gaps

1. xAI whole-call streaming snapshots can contain function calls, but the
   translator only implements the incremental `.added`/`.delta`/`.done`
   dialect. A snapshot-only tool call is dropped and reported as `stop`.
2. A Responses request routed to a plain Chat Completions provider forwards
   Responses-only keys (`store`, `include`, `reasoning`, `previous_response_id`,
   `text`, session fields) in the Chat body. Strict gateways may reject them.
3. Some mechanically derived Responses URLs (notably GitHub Copilot entries)
   are not verified against a real upstream.
4. Grok CLI uses a private proxy and a pinned client version. A 426 is not
   classified as an outage, so it does not trip the breaker or fail over.
5. If the special Grok CLI credential pool is empty, the registry row can fall
   through to a generic backend pointed at the wrong private endpoint.
6. Responses streaming emits a small subset of the OpenAI event contract and a
   non-standard `output_item.added` tool shape. It is not yet SDK-safe for
   multi-step tool loops.

## Security and data findings

Implemented protections that are verified by tests:

- API-key hash lookup with legacy plaintext compatibility.
- `manage` scope for admin mutations; model allow-list on Chat/Responses.
- Fail-closed DB/auth errors; non-loopback bind refuses startup without a key.
- AES-GCM field encryption when `STORAGE_ENCRYPTION_KEY` is available.
- CORS allowlist and traversal-safe static file joining.
- Parameterized SQL; no request-driven SSRF path was found.

Open findings:

| Severity | Finding |
|---|---|
| High | SSE error frames expose raw upstream error bodies, while non-stream errors are sanitized. |
| High | Responses storage defaults to on, keeps full request/response envelopes in `key_value` without TTL/size pruning, and uses an upstream-controlled response id as the storage key. |
| High | A missing `STORAGE_ENCRYPTION_KEY` silently makes Grok OAuth refresh write plaintext tokens. |
| Medium | NULL API-key policy columns fall back to `manage` and unrestricted models; the Rust admin API cannot narrow a key. |
| Medium | The Tauri sidecar URL is injected before readiness and has no ownership handshake; a port collision can point the UI at a foreign listener. |
| Medium | `/` and `/{*path}` serve the dashboard without auth; `HETERION_ROUTER_UI_DIR` has no root containment check. |
| Medium | Quota/session/budget columns are migrated but not enforced. |
| Medium | The field-encryption key is stored beside the ciphertext it protects. |
| Low | `no_log` suppresses usage/call logs but not `last_used_at` or Responses transcripts; Gemini signature/log files can be created before permission hardening. |
| Low | Several credential structs derive `Debug`; future debug logging could dump token pools. |

The current data directory is normally `0700`, the SQLite DB and `.env` are
normally `0600`, but log/signature files created after the startup permission
pass can be `0644` until the next boot. The documented “all logs 0600” claim
was therefore too broad.

## Desktop, service and release reality

- The Tauri app builds a real `.app` and `.dmg` locally in about 5m15s cold.
  The bundle is `adhoc`/linker-signed, has no Team ID, and is not notarized;
  it is a developer-machine artifact, not a distributable release.
- The sidecar defaults to `127.0.0.1:20129`; the UI fallback in `api.js` says
  `20128`. If the injected URL is missing, the desktop can talk to the launchd
  service instead of its own sidecar.
- The Vite dev proxy defaults to `20128`, while a plain `cargo run` gateway uses
  `20129`. A contributor following both quick-start paths can edit the service
  database.
- A sidecar bind failure is logged but not retried; the window is created before
  a readiness check.
- The desktop “Restart now” control cannot restart the Tauri sidecar: it either
  receives 409 or restarts the separately installed launchd service on 20128.
- No launchd plist or installation script is committed. The machine-local plist
  points at a development tree and binds `0.0.0.0`.
- `cargo fmt --all` does not cover the excluded Tauri crate; the Tauri crate
  itself currently has formatting differences. CI does not test desktop Rust or
  the UI beyond a bundle build.
- `release.yml` uses `sha256sum` on a macOS matrix leg; the command was not
  executed in this audit and is not the macOS-native spelling.

## Documentation corrections applied

- `README.md` and `AGENTS.md` now distinguish service `20128`, sidecar `20129`,
  and the dev proxy default; they state that the launchd plist is machine-local.
- `STATUS.md` now distinguishes code/test evidence from live usage, reports the
  registry/runtime gap, and lists security/desktop limitations.
- `docs/ROADMAP-2026-09.md` has one current execution-status section, corrected
  gap cells, accurate line/symbol anchors, and a prioritized backlog.
- `CHANGELOG.md` records the audit/documentation milestone without inventing a
  release version; the rename, `heterion-local`, data-dir contract, original
  `/v1/responses`, single-instance, parent-watch and Gemini signature entries
  are restored.
- ADR-002/006/007/008 have factual addenda for current env names, sidecar name,
  injected global, single-instance status, and the actual CI filename.
- `docs/history/` is explicitly labeled as frozen upstream JS OmniRoute
  material, not Rust-project work history.
- `docs/research/CHATHUB-2026-09.md` and ADR-010 record the anonymous-session
  result: no guest cookie, chat `403`, stream `401`, and no adapter.

## Prioritized follow-up

### P0 — correctness and silent data loss

1. Preserve Responses `reasoning` input items or return
   `400 unsupported_capability`; never silently drop encrypted reasoning.
2. Make Responses streaming events and tool arguments SDK-shaped; add a
   full-stream contract test with exactly one terminal event.
3. Reject unsupported built-in tools instead of dropping them.
4. Sanitize streaming errors and add a regression test.
5. Bound/encrypt/prune the Responses state store and mint gateway-owned keys.

### P1 — routing and operations

6. Canonicalize provider ids when building backends; report unreachable registry
   entries in admin/model output.
7. Add real provider/connection/combo attribution and real HTTP status/error
   codes to usage/call logs.
8. Fix `vite.config.js` and the Tauri fallback port; add a sidecar readiness
   handshake and a supervised-restart path for the desktop.
9. Commit/install a launchd plist or remove service claims from the quick start.
10. Add desktop Rust checks/tests to CI and fix the excluded-crate fmt gate.

### P2 — test and maintenance debt

11. Add admin, combo-retry, breaker half-open, CORS/dashboard, and provider
    construction tests.
12. Implement or remove `maxSetRetries`, `setRetryDelayMs`, `weight`, and
    `strategy:auto` fields that the UI exposes but routing ignores.
13. Fix `count_active_api_keys`; make the release checksum command portable.
14. Remove the tracked 0-byte AppleDouble icon and stale generated binaries.

No code fix from this audit is implied by this document: it records verified
facts and the next work items. The working tree still needs an explicit commit
before these documents describe a reproducible `main` checkout.

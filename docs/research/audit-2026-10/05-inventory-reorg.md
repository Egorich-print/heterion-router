# Inventory & reorganisation audit — heterion-router (2026-10-02)

> **Mode:** read-only. No file in the repository was moved, renamed, edited or
> deleted; nothing was committed. The only artefact created is this report.
> No build/test was executed (that would write into `target/`), so all Rust
> numbers are *static* counts from `rg`/`wc`, not `cargo test` output.
> **Baseline:** branch `main`, HEAD `51dfe6d`; working tree `35 M / 7 ??`
> (`git status --porcelain | grep -c '^ M'` / `'^??'`).
> **Standards applied:** `File Organization.md`, `Project Template.md`,
> `Project Registry.md` (row 24), `Hardware Project Template.md` (skim only —
> N/A, software project).
> **Note:** this is file `05` of the `docs/research/audit-2026-10/` series
> (siblings `01-weaknesses.md`, `02-omniroute-parity.md`, `03-futureagi-parity.md`
> were written in parallel by other workstreams while this audit ran).
> `docs/research/mission-2026-10/05-desktop-release.md` is a **different**
> document series and shares only the number.

---

## Repo tree

Top level is 12 entries. Annotations cite the file/line that proves the purpose.

```
heterion-router/                                  # ai-workstation/Projects/heterion-router
├── .github/workflows/                            # CI (2 workflows, no other GitHub config)
│   ├── ci.yml                                    # fmt/clippy/test + UI build; paths-filtered (ci.yml:10-25)
│   └── release.yml                               # workflow_dispatch gateway binaries only (release.yml:1-4)
├── .gitignore                                    # 28 lines; see "Ignore/hygiene findings"
├── AGENTS.md                                     # agent entry point: layout, gates, frozen contracts
├── CHANGELOG.md                                  # Keep-a-changelog; "Unreleased" only, no released version
├── Cargo.lock                                    # 60 KB, workspace lock — tracked by design (.gitignore:1)
├── Cargo.toml                                    # workspace root: 10 members (9-20), exclude (23), resolver 3
├── LICENSE                                       # MIT, upstream copyright preserved (ADR-008 §4)
├── README.md                                     # showcase-grade README + doc index (96-103)
├── STATUS.md                                     # authoritative status snapshot, updated 2026-09-29 (STATUS.md:4)
├── THIRD_PARTY_NOTICES.md                        # Rust-only dependency notice
├── crates/                                       # 10 workspace crates — FROZEN LAYOUT, see "Do NOT reorganize"
│   ├── heterion-router-config/    3 files  252 L  env/port/data-dir resolution (config.rs:51 get2)
│   ├── heterion-router-core/      4 files  379 L  shared types + GatewayError
│   ├── heterion-router-crypto/    1 file   318 L  enc:v1 + STATIC_SALT (lib.rs:18,23)
│   ├── heterion-router-db/        6 files 1862 L  SQLite repos; schema_base.sql embedded (migrate.rs:16)
│   ├── heterion-router-gateway/  18 files 9474 L  axum app: 19 routes (lib.rs:146-168) + binary main.rs
│   ├── heterion-router-http/      3 files  355 L  reqwest client + SSE parser
│   ├── heterion-router-providers/ 3 files  357 L  registry.json embedded (registry.rs:65) + 1 snapshot
│   ├── heterion-router-routing/   3 files  270 L  Router/BreakerSet — decision layer only (lib.rs:1-7)
│   ├── heterion-router-translate/ 7 src + 6 tests + 6 fixtures  3180 L  wire-format translators
│   └── heterion-router-usage/     3 files  361 L  usage rows + pricing
├── desktop/                                      # Tauri shell — FROZEN, see ui.rs:30 path dependency
│   ├── package.json                              # 5 scripts (dev:ui, build:ui, build:sidecar, tauri:dev/build)
│   ├── scripts/stage-sidecar.sh                  # builds+copies sidecar into src-tauri/binaries/ (10-17)
│   ├── src-tauri/                                # EXCLUDED from workspace by design (Cargo.toml:23)
│   │   ├── Cargo.toml / Cargo.lock               # independent lockfile, tracked by design
│   │   ├── .gitignore                            # /target/ /gen/schemas /binaries/
│   │   ├── build.rs (3 L), src/lib.rs (98 L), src/main.rs (6 L)
│   │   ├── capabilities/default.json             # core:default + shell:allow-spawn
│   │   ├── gen/schemas/*.json                    # 4 generated files, gitignored (desktop/src-tauri/.gitignore:4)
│   │   ├── icons/ (15 files)                     # incl. tracked 0-byte AppleDouble (see hygiene)
│   │   ├── tauri.conf.json                       # frontendDist ../ui/dist (7), externalBin (33), csp:null (20)
│   │   └── binaries/                             # gitignored sidecars, present on disk
│   └── ui/                                       # Svelte 5 + Vite dashboard, 14 files 1443 L
│       ├── .gitignore, index.html, package.json, package-lock.json
│       ├── svelte.config.js, vite.config.js      # proxy /api,/v1,/healthz → 20128 (vite.config.js:7,16)
│       └── src/ App.svelte, api.js, app.css, main.js, views/{7}.svelte
├── docs/                                         # 31 .md + 6 .json (8 502 L) — the only reorg candidate
│   ├── MISSION-2026-10.md          611 L  canonical next-mission plan (untracked)
│   ├── ROADMAP-2026-09.md          186 L  API/CLI roadmap + backlog (untracked)
│   ├── adr/            11 ADR + README.md       # index complete (README.md:12-22)
│   ├── history/         3 reports + README.md    # frozen *JS upstream* material, NOT project history
│   └── research/                   13 .md + 6 .json (pre-existing) — entirely UNTRACKED
│       ├── CHATHUB-2026-09.md, PROJECT-AUDIT-2026-09.md
│       ├── chathub-evidence/  4 .md + 6 .json
│       ├── mission-2026-10/  6 reports + README.md
│       └── audit-2026-10/    this file (+ 3 sibling reports 01/02/03, written in
│                              parallel by other audit workstreams during this pass)
├── rust-toolchain.toml                           # pins 1.98.0 (2) — CI uses @stable (ci.yml:39): mismatch
├── scripts/
│   └── ci/check.sh                               # THE gate; path-pinned by CI (ci.yml:42) and self-gating
└── target/                     3.8 G              # gitignored build artifacts
```

Ignored-but-present (not in the tree above): `target/` (3.8 G),
`desktop/src-tauri/target/` (4.0 G), `desktop/src-tauri/gen/` (4 files),
`desktop/src-tauri/binaries/` (2 staged binaries), `desktop/ui/node_modules/` (26 M),
`desktop/ui/dist/` (76 K, 3 entries).

---

## LOC table

`wc -l` over tracked sources; `target/`, `node_modules/`, `dist/` excluded.
Commands: `find crates -path '*/src/*' -name '*.rs' | xargs wc -l` and per-crate loops.

### Rust, per crate

| Crate | src files | src LOC | tests/ files | tests/ LOC | `#[test]` | `#[tokio::test]` | `#[cfg(test)]` mods | snapshots |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| heterion-router-config | 3 | 252 | 0 | 0 | 6 | 0 | 2 | 0 |
| heterion-router-core | 4 | 379 | 0 | 0 | 8 | 0 | 2 | 0 |
| heterion-router-crypto | 1 | 318 | 0 | 0 | 13 | 0 | 3 | 0 |
| heterion-router-db | 5 + `schema_base.sql` | 1862 | 0 | 0 | 17 | 0 | 3 | 0 |
| **heterion-router-gateway** | 17 | **9441** | 1 | 33 | 48 | 60 | 24 | 0 |
| heterion-router-http | 3 | 355 | 0 | 0 | 5 | 3 | 2 | 0 |
| heterion-router-providers | 2 + `registry.json` | 357 | 0 | 0 | 14 | 0 | 1 | **1** |
| heterion-router-routing | 3 | 270 | 0 | 0 | 6 | 0 | 2 | 0 |
| **heterion-router-translate** | 7 | 2908 | 6 | 272 | 29 | 0 | 6 | 0 |
| heterion-router-usage | 3 | 361 | 0 | 0 | 6 | 0 | 2 | 0 |
| **crates total** | **48** | **16 503** | **7** | **305** | **152** | **63** | **47** | **1** |
| desktop/src-tauri (excluded) | 2 + `build.rs` | 107 | 0 | 0 | 0 | 0 | 0 | 0 |
| **Rust total** | **50** | **16 915** | **7** | **305** | **152** | **63** | **47** | **1** |

Largest single files: `gateway/src/lib.rs` 2324 L, `db/src/repos.rs` 1368 L,
`gateway/src/grok_cli.rs` 1166 L, `gateway/src/openai.rs` 881 L,
`gateway/src/gemini.rs` 832 L, `gateway/src/admin.rs` 783 L.

### Non-Rust source

| Area | files | LOC | command |
|---|---:|---:|---|
| `desktop/ui/src/views/*.svelte` (7 files) | 7 | 940 | `wc -l desktop/ui/src/views/*.svelte` |
| `desktop/ui/src/app.css` | 1 | 283 | `wc -l` |
| `desktop/ui/src/{api.js,main.js,App.svelte}` | 3 | 180 | `wc -l` |
| `desktop/ui/{vite,svelte}.config.js`, `index.html` | 3 | 40 | `wc -l` |
| **UI total** | **14** | **1 443** | `find desktop/ui/src desktop/ui/*.js desktop/ui/index.html -type f \| xargs wc -l` |
| Markdown (all, build dirs pruned) | 36 (`docs/` 31 + root 5) | **8 502** | `find . \( -name target -o -name node_modules -o -name .git -o -name dist \) -prune -o -name '*.md' -print \| xargs wc -l` |
| JSON data (`registry.json`, 6 fixtures, 6 chathub captures) | 13 | — | — |

### Source / test / snapshot file census

| Class | Count | Command |
|---|---:|---|
| Rust source files (`crates/*/src/**`) | 48 | `find crates -path '*/src/*' -name '*.rs' \| wc -l` |
| Rust integration-test files (`crates/*/tests/**`) | 7 | `find crates -path '*/tests/*' -name '*.rs' \| wc -l` |
| Insta snapshots | 1 | `find . -name '*.snap' -not -path './target/*'` |
| Translate fixtures (JSON) | 6 | `ls crates/heterion-router-translate/tests/fixtures/` |
| Fixture provenance manifest | **0 (missing)** | mission MR-17 asks for `fixtures/PROVENANCE.json` |
| Rust src files **with** inline tests | 38 of 48 | `grep -rl '#\[cfg(test)\]' crates --include='*.rs' \| wc -l` |
| Rust src files **without** any test | 10 of 48 | the 8 `lib.rs` re-export barrels + `core/src/error.rs` + `db/src/error.rs` |

---

## Doc inventory table

Every file under `docs/`, plus root docs. "Purpose" is taken from the file's own
header; mtime is filesystem `stat -f '%Sm'`.

### Root docs

| File | LOC | mtime | Purpose | In git |
|---|---:|---|---|---|
| `README.md` | 103 | 2026-09-29 | Quick start, architecture sketch (45-53), doc index (96-103) | modified |
| `STATUS.md` | 187 | 2026-09-29 | Authoritative status; live/runtime facts; limits; milestone table | modified |
| `AGENTS.md` | 65 | 2026-09-29 | Layout, pre-commit gates, frozen contracts, service ops | modified |
| `CHANGELOG.md` | ~150 | 2026-09-29 | Unreleased section; no released version exists | modified |
| `THIRD_PARTY_NOTICES.md` | 11 | 2026-09-18 | Rust-only dependency licence notice | clean |

### `docs/` — planning

| File | LOC | mtime | Purpose | In git |
|---|---:|---|---|---|
| `MISSION-2026-10.md` | 611 | 2026-09-29 | Canonical next-mission plan: objectives O-1…O-4, WS-A…WS-H, M0…M3, 9 owner decisions | **untracked** |
| `ROADMAP-2026-09.md` | 186 | 2026-09-29 | Verified snapshot, execution-status table, prioritised backlog P0/P1/P2 | **untracked** |

### `docs/adr/` — decisions

| ADR | Title | Status (as written) | Header format | mtime | In git |
|---|---|---|---|---|---|
| 001 | Граница интеграции JS↔Rust — общая схема SQLite | accepted (2026-09-18) | `> Дата · Статус` | 2026-09-18 | clean |
| 002 | Дашборд раздаёт сам шлюз | accepted (2026-09-18) | `> Дата · Статус` | 2026-09-29 | modified |
| 003 | Dev-режим UI: Vite-proxy, без второго бэкенда | accepted (2026-09-18) | `> Дата · Статус` | 2026-09-18 | clean |
| 004 | Мутации в admin-API — только обслуживаемое | accepted (2026-09-18) | `> Дата · Статус` | 2026-09-18 | clean |
| 005 | API-ключи выпускаются в Rust в JS-форме | accepted (2026-09-18) | `> Дата · Статус` | 2026-09-18 | clean |
| 006 | Рестарт — только под супервизором | accepted (2026-09-18) | `> Дата · Статус` | 2026-09-29 | modified |
| 007 | Десктоп — sidecar + тот же бандл | accepted (2026-09-18) | `> Дата · Статус` | 2026-09-29 | modified |
| 008 | Переименование в Heterion Router, новый репозиторий | accepted (2026-09-19) | `- Статус` bullet | 2026-09-29 | modified |
| 009 | Capability routing, credential rotation, security boundaries | accepted (no date in status line) | `- Статус` bullet | 2026-09-29 | **untracked** |
| 010 | ChatHub не подключается как undocumented upstream | accepted (date in H1: 2026-09-27) | `- Статус` bullet | 2026-09-29 | **untracked** |
| 011 | Границы MISSION-2026-10 и «схема ≠ enforcement» | **принят** (2026-09-29) | `- Статус` bullet | 2026-09-29 | **untracked** |
| `adr/README.md` | 40 | — | Index: all 11 ADRs listed (12-22) + rejected/deferred list (26-40) | modified |

**Index integrity:** 11 ADR files ↔ 11 index rows — complete, no dangling links.
But **3 of 11 ADRs are untracked**, so a fresh clone sees an index pointing at
3 missing files (the exact failure mode mission MR-16 / `06-qa-docs.md:815`
calls *GAP-D1*).

**Format inconsistency:** ADR-001…007 use `> Дата: … · Статус: accepted`;
ADR-008…011 use `- Статус: …`. ADR-009/010 carry no date on the status line.
ADR-011 says `принят`, all others `accepted`.

### `docs/history/` — frozen upstream JS (NOT this project's history)

| File | LOC | mtime | Purpose | In git |
|---|---:|---|---|---|
| `README.md` | 13 | 2026-09-29 | Explains the dir is frozen JS OmniRoute material, not Rust history | **untracked** |
| `task-c1-report.md` | 68 | 2026-09-18 | A2A task memory / Conductor report (JS era) | clean |
| `task-c2-report.md` | 104 | 2026-09-18 | Orchestration drawer experiment (JS era) | clean |
| `task-c3-report.md` | 159 | 2026-09-18 | Orchestration report (JS era) | clean |

### `docs/research/` — evidence (13 .md + 6 .json pre-existing, **all untracked**)

| File | LOC | mtime | Purpose |
|---|---:|---|---|
| `PROJECT-AUDIT-2026-09.md` | 213 | 2026-09-29 | Consolidated evidence ledger of 6 parallel audits; authoritative fact matrix |
| `CHATHUB-2026-09.md` | 108 | 2026-09-29 | ChatHub protocol investigation (input to ADR-010) |
| `chathub-evidence/README.md` | 18 | 2026-09-29 | Index of the sanitised capture |
| `chathub-evidence/INDEPENDENT-RECON.md` | 49 | 2026-09-29 | Commands + endpoint results |
| `chathub-evidence/ANONYMOUS-SESSION.md` | 40 | 2026-09-29 | Fresh guest cookie-jar handshake and retry |
| `chathub-evidence/ANCILLARY.md` | 25 | 2026-09-29 | Secondary observations |
| `chathub-evidence/models.json` | 0 lines / 16 632 B | 2026-09-29 | Public model catalog snapshot (minified, valid JSON) |
| `chathub-evidence/chat-post.request.json` | 0 lines / 384 B | 2026-09-29 | Exact anonymous probe body (synthetic UUIDs only) |
| `chathub-evidence/chat-post.response.json` | 0 lines / 47 B | 2026-09-29 | 403 error body |
| `chathub-evidence/chat-post.ids.json` | 0 lines / 163 B | 2026-09-29 | Captured ids from the corrected probe |
| `chathub-evidence/chat-response.json` | 0 lines / 47 B | 2026-09-29 | 403 error body |
| `chathub-evidence/stream-response.json` | 0 lines / 24 B | 2026-09-29 | 401 error body |
| `mission-2026-10/README.md` | 30 | 2026-09-29 | Index of the 6 mission evidence reports |
| `mission-2026-10/01-mission-sequencing.md` | 889 | 2026-09-29 | Charter, non-goals, workstreams, milestones, risk register |
| `mission-2026-10/02-responses-contract.md` | 733 | 2026-09-29 | Responses silent-loss defects, target design, AC-1…AC-16 |
| `mission-2026-10/03-routing-providers.md` | 778 | 2026-09-29 | Registry/backend construction, canonical ids, attribution |
| `mission-2026-10/04-security-retention.md` | 1320 | 2026-09-29 | Threat model, 12 domains, mitigation waves, acceptance matrix |
| `mission-2026-10/05-desktop-release.md` | 1139 | 2026-09-29 | Desktop/release: 28 executed probes, findings B1–B10, steps S0–S7 |
| `mission-2026-10/06-qa-docs.md` | 994 | 2026-09-29 | Test matrix gaps MR-01…MR-18, doc protocol DOC-1…DOC-7 |
| `audit-2026-10/05-inventory-reorg.md` | this file | 2026-10-02 | Repository inventory + reorg proposal |
| `audit-2026-10/01-weaknesses.md`, `02-omniroute-parity.md`, `03-futureagi-parity.md` | (siblings) | 2026-10-02 | Parallel audit workstreams — **not part of this inventory**; appeared in the folder while this audit was running, so `find docs/research -name '*.md'` now returns 17, not 14. |

All 6 chathub JSON captures parse cleanly (`python3 -c "json.load"` on each) and
contain no cookies/tokens per the README claim — measured clean, but no
automated sanitize gate exists yet (mission MR-17 proposes
`scripts/qa/sanitize-fixtures.sh`).

### Docs referenced but MISSING (dangling forward references)

Enumerated with `grep -rhno 'docs/…\.md' --include='*.md' | sort -u`, then `[ -e ]`:

| Referenced path | Referenced from | Meaning |
|---|---|---|
| `docs/SECURITY.md` | `mission-2026-10/04-security-retention.md:699,725,1193,1254,1277` | proposed, not written |
| `docs/THREAT-MODEL.md` | `04-security-retention.md:699` | proposed, not written |
| `docs/qa/TEST-MATRIX.md` | `mission-2026-10/06-qa-docs.md:954` | proposed by WS-G |
| `docs/adr/ADR-011-responses-state-contract.md` | `04-security-retention.md:1196` | **number collision** — ADR-011 is already `mission-2026-10-scope` |
| `docs/adr/ADR-012-key-and-credential-retirement.md` | `04-security-retention.md:1199` | proposed |

The ADR-011 collision is a live hazard: two different documents claim ADR-011.

---

## Test inventory table

| Metric | Value | Command |
|---|---:|---|
| `#[test]` attributes | **152** | `grep -rho '#\[test\]' crates \| wc -l` |
| `#[tokio::test]` attributes | **63** | `grep -rho '#\[tokio::test\]' crates \| wc -l` |
| Static total test attributes | **215** | sum of the two |
| Documented total | 213 | `STATUS.md:137`, `ROADMAP-2026-09.md:16,34` |
| `#[cfg(test)]` modules | 47 (in 38 files) | `grep -rn '#\[cfg(test)\]' crates \| wc -l` |
| Tests in `desktop/src-tauri` | **0** | `grep -rho '#\[test\]\|#\[tokio::test\]' desktop/src-tauri/src` |
| Integration-test files | 7 | `find crates -path '*/tests/*' -name '*.rs'` |
| Inline-test files | 38 of 48 src files | `grep -rl '#\[cfg(test)\]' crates --include='*.rs' \| wc -l` |
| `Mock::given(...)` sites | **34** | `grep -rn 'Mock::given' crates \| wc -l` |
| Documented `wiremock.expect()` count | 22 | `STATUS.md:167`, `ADR-011 §4` |
| `.verify()` calls | **0** | `grep -rn '\.verify()' crates` → no hits |
| proptest / tarpaulin / criterion | 0 | `grep -rn 'proptest\|tarpaulin\|llvm-cov\|criterion' crates` |

Per-crate distribution is in the LOC table above. Concentration:
gateway = 108/215 (50 %) of all test attributes in 1 of 10 crates;
`config`, `core`, `crypto`, `db`, `routing`, `usage` have integration-test
files = 0 (all tests inline).

`215 static attributes` vs `213 executed tests` is a **2-test delta** that this
audit cannot resolve without running `cargo test --workspace -- --list`
(out of scope: it writes to `target/`). Candidates: attribute matches inside
commented code, or a `#[test]` in a `#[cfg(...)]`-gated block. Either way,
**the documented invariant "213" is now unverifiable statically** and the
count-invariant gap is already logged as DOC-2 in `06-qa-docs.md:895`.

---

## Ignore/hygiene findings

### `.gitignore` effectiveness — proven with `git check-ignore -v`

| Candidate path | Ignored? | Rule |
|---|---|---|
| `target/debug/x` | YES | `.gitignore:3 **/target/` |
| `crates/foo/target/x` | YES | `.gitignore:3 **/target/` |
| `target/release/heterion-router-gateway` | YES | `.gitignore:3 **/target/` |
| `desktop/src-tauri/target/debug/x` | YES | `desktop/src-tauri/.gitignore:3 /target/` |
| `desktop/src-tauri/gen/schemas/desktop-schema.json` | YES | `desktop/src-tauri/.gitignore:4 /gen/schemas` |
| `desktop/src-tauri/binaries/heterion-router-gateway` | YES | `desktop/src-tauri/.gitignore:6 /binaries/` |
| `desktop/ui/node_modules/x` | YES | `desktop/ui/.gitignore:1 node_modules/` |
| `desktop/ui/dist/x` | YES | `desktop/ui/.gitignore:2 dist/` |
| `desktop/ui/.vite/cache.json` | YES | `desktop/ui/.gitignore:3 .vite/` |
| `.env` | YES | `.gitignore:17 .env` |
| `.env.local` | YES | `.gitignore:18 .env.*` |
| `.env.example` | correctly NOT ignored | `.gitignore:19 !.env.example` (negation works) |
| `debug.log` / `foo.log` | YES | `.gitignore:24 *.log` |
| `Foo.app`, `app.dmg` | YES | `.gitignore:8,9` |
| `foo.rs.bk` | YES | `.gitignore:4 **/*.rs.bk` |
| `.DS_Store` | YES | `.gitignore:25` |
| **`storage.sqlite`** | **NOT IGNORED** | no rule |
| **`storage.sqlite-wal` / `-shm`** | **NOT IGNORED** | no rule |
| **`app.db`** | **NOT IGNORED** | no rule |
| **`node_modules/x` (root or any non-`desktop/ui` path)** | **NOT IGNORED** | no rule |
| **`dist/x`**, **`build/x`** (root) | **NOT IGNORED** | no rule |
| **`artifacts/evidence.txt`** | **NOT IGNORED** | no rule |

`git status --porcelain --ignored | grep '^!!'` returns exactly 6 entries —
`target/`, `desktop/src-tauri/target/`, `desktop/src-tauri/gen/`,
`desktop/src-tauri/binaries/`, `desktop/ui/node_modules/`, `desktop/ui/dist/`
— i.e. **no unintended ignored files**; the ignore rules are doing exactly what
they claim.

### Tracked vs ignored

| Finding | Evidence |
|---|---|
| **Tracked build artifact:** `desktop/src-tauri/icons/.!15396!128x128.png` — 0 bytes, an AppleDouble resource-fork stub | `git ls-files` contains it; `git ls-files \| while read f; do [ ! -s "$f" ] && echo EMPTY $f; done` → the only EMPTY hit. Already filed as ROADMAP P2-13 (line 103). |
| **Generated-but-ignored, present:** 4 × `desktop/src-tauri/gen/schemas/*.json` — correct (regenerated by `tauri-build`) | `git ls-files --error-unmatch` → untracked; `git check-ignore -v` → `desktop/src-tauri/.gitignore:4` |
| **Largest tracked files** are all legitimate: `icons/icon.icns` 272 KB, `gateway/src/lib.rs` 84 KB, `icons/icon.png` 52 KB, `icons/icon.ico` 40 KB, `desktop/src-tauri/Cargo.lock` 32 KB | `git ls-files \| xargs -I{} du -k {} \| sort -rn \| head -20` |
| **No tracked `target/`, `dist/`, `node_modules/`** — clean | `git ls-files \| grep -c 'target/\|node_modules/\|/dist/'` → 0 |

### Dirty working tree

| Metric | Today | Documented | Drift |
|---|---:|---|---|
| `M` (modified, tracked) | 35 | 35 (`STATUS.md:19`, `MISSION-2026-10.md:57`) | none |
| `??` (untracked) | **7** | 5 (`STATUS.md:19`) | **+2** — `docs/history/README.md` and `docs/research/` collapsed entries |

The 7 untracked paths: `docs/MISSION-2026-10.md`, `docs/ROADMAP-2026-09.md`,
`docs/adr/ADR-009`, `ADR-010`, `ADR-011`, `docs/history/README.md`, `docs/research/`.
That is **25 files / 7 365 lines of untracked documentation and 3 untracked
ADRs** (`git status --porcelain | grep '^??' | … | xargs wc -l | tail -1` →
`7365 total`, excluding this report and its 3 siblings) — the single largest
hygiene risk, because mission M0 (O-4 "reproducible baseline") is defined as
exactly this cleanup.

### Stale / broken references found

| Reference | Claim | Reality |
|---|---|---|
| `crates/heterion-router-providers/src/lib.rs:3` | "The JavaScript `REGISTRY` (277 providers)" | `registry.json` has **278** providers / **2717** models (measured) |
| `crates/heterion-router-providers/src/lib.rs:4` | "see `rust-rewrite/PLAN.md` §5 Phase 0" | `rust-rewrite/` **does not exist** (`ls -d rust-rewrite` → No such file) |
| `docs/adr/ADR-008:23` | filter-repo kept `.github/workflows/rust.yml` | actual workflow is `ci.yml`; already corrected in ADR-008's own "Фактическое уточнение" (58-59) |
| `STATUS.md:167`, `ADR-011 §4` | "22 `wiremock.expect()`" | **34** `Mock::given` sites today |
| `STATUS.md:19`, `MISSION-2026-10.md:57` | `35 M / 5 ??` | `35 M / 7 ??` |

### Layer/config hygiene (not `.gitignore`, but adjacent)

| Finding | Evidence |
|---|---|
| Toolchain pin mismatch: `rust-toolchain.toml:2` = `1.98.0`; CI installs `@stable` | `ci.yml:39`, `release.yml:30` — CI can compile a different compiler than local. Already logged at `STATUS.md:141-142`. |
| CI `paths` filter omits `docs/**` and `desktop/src-tauri/**` | `ci.yml:10-16` (push), `ci.yml:19-25` (PR) — a docs-only PR runs **no** CI; a src-tauri-only PR runs **no** CI. Logged at `STATUS.md:139-141`. |
| No desktop CI job; `desktop/src-tauri` has 0 tests | `ci.yml` jobs = `rust`, `ui` only; `grep -c '#[test]' desktop/src-tauri/src` → 0. Mission Q8 blocks this. |
| `release.yml:44` uses `sha256sum` but the matrix includes `macos-14` | `release.yml:34-35` matrix `[ubuntu-latest, macos-14]`; `sha256sum` is not a macOS default binary (`shasum -a 256` is). Logged as ROADMAP P2-13. |
| `scripts/ci/check.sh:12` exec-bit gate covers only root `scripts/` | `git ls-files -s scripts/` → 1 file; `desktop/scripts/stage-sidecar.sh` is outside the gate's scope. |
| No `rustfmt.toml`, `clippy.toml`, `deny.toml`, `.cargo/config.toml`, `.editorconfig` | all ABSENT — clippy policy is CLI-only (`-D warnings` in `check.sh:23`) |
| No `.github/dependabot.yml`, `CODEOWNERS`, PR template | `.github/` contains only the 2 workflows |
| 7.8 GB of build artifacts on the SSD | `du -sh target desktop/src-tauri/target` → 3.8 G + 4.0 G |

### Doc/compliance hygiene outside the repo

| Layer | State |
|---|---|
| Obsidian `Projects/heterion-router/{Overview,Decisions,Links}.md` | mtime **2026-09-18** — 14 days stale. `Decisions.md` records only ADR-008 + the 2026-09-19 env/heterion-local decisions; **ADR-009/010/011 are missing**. |
| Obsidian `ADR →` symlink | present and correct (File Organization §2 requirement) |
| Showcase `Documents/Проекты/heterion-router/STATUS.md` | mtime **2026-09-19**, and its table says "Десктоп + sidecar — работает (**известный баг: single-instance guard**)" — contradicted by `desktop/src-tauri/src/lib.rs:41` (`tauri_plugin_single_instance::init`) and by ADR-007's own addendum ("Single-instance guard реализован первым Tauri plugin; прежнее ограничение снято"). |
| `Project Registry.md:24` | `Active`, layer `Intelligence / Execution` — matches `STATUS.md:14`. Registry itself last touched 2026-09-29. In sync on identity; no roadmap/mission pointer column exists to go stale. |

---

## Standards compliance matrix

| # | Requirement | Source doc | Current state | Compliant | Action |
|---|---|---|---|---|---|
| 1 | `README.md` at root | Project Template §4 | present, 103 L | **yes** | — |
| 2 | `STATUS.md` at root | Project Template §4 | present, updated 2026-09-29 | **yes** | — |
| 3 | `CHANGELOG.md` at root | Project Template §4 | present, `Unreleased` only | **yes** | — |
| 4 | `.gitignore` at root | Project Template §4 | present, 28 L, 3 nested variants | **yes** | — |
| 5 | `.gitignore` must cover `target/` | Project Template §4:50 | `.gitignore:2-3` + `desktop/src-tauri/.gitignore:3` | **yes** | — |
| 6 | `.gitignore` must cover `node_modules/` | Project Template §4:51 | only `desktop/ui/node_modules/` | **partial** | add root `node_modules/` |
| 7 | `.gitignore` must cover `dist/` | Project Template §4:52 | only `desktop/ui/dist/` | **partial** | add root `dist/` |
| 8 | `.gitignore` must cover `build/` | Project Template §4:53 | no rule anywhere | **no** | add `build/` |
| 9 | `.gitignore` must cover `*.log` | Project Template §4:55 | `.gitignore:24` | **yes** | — |
| 10 | `.gitignore` must cover `.env` | Project Template §4:56 | `.gitignore:17-18` | **yes** | — |
| 11 | `.gitignore` `.DS_Store` | Project Template §4:54 | `.gitignore:25` | **yes** | — |
| 12 | `docs/architecture/` subdir | Project Template §4:38 (`mkdir -p docs/{architecture,history}`) | **absent**; architecture lives as a 9-line ASCII sketch in `README.md:45-53` | **no** | create `docs/architecture/` (SAFE) |
| 13 | `docs/history/` subdir | Project Template §4:38 | exists, but holds frozen **JS upstream** material, not project history | **path yes / meaning no** | rename to `docs/upstream-omniroute-js/` (mission A6) |
| 14 | ADR lives in `docs/adr/` | Project Template §5, §62 | 11 ADR + complete index | **yes** | **never rename this dir** (Obsidian symlink) |
| 15 | ADR index with title + status | File Organization §2 (`ADR/ → symlink`), §4 | `adr/README.md` lists all 11 (12-22) | **yes (in tree)** / **no (in clone)** | `git add` ADR-009..011 |
| 16 | One source of truth, no duplication | File Organization §4 | `README.md:45-53` architecture sketch duplicates what belongs in `docs/architecture/` | **partial** | move the sketch to `docs/architecture/` and link |
| 17 | `media/{screenshots,recordings,demos}` | Project Template §4:41 | absent; screenshots belong in `Documents/Проекты/` (File Organization §2:68) | **yes (N/A)** | no action |
| 18 | Add to `Project Registry.md` | Project Template §4:44, §99-101 | row 24 present, identity in sync | **yes** | optional: add mission pointer |
| 19 | Obsidian entry `Projects/<name>/` | File Organization §2:76-82 | `Overview.md`/`Decisions.md`/`Links.md` + `ADR →` symlink | **yes (structure)** / **no (freshness)** | update `Decisions.md` with ADR-009..011 |
| 20 | Showcase in `Documents/Проекты/<name>/` with README.md + STATUS.md | File Organization §2:60-68 | both present | **yes (structure)** / **no (accuracy)** | fix the stale single-instance claim |
| 21 | Files divided by purpose, not type | File Organization §1 | `docs/` mixes decisions / planning / evidence / foreign history in one flat dir | **partial** | add `docs/README.md` index; rename `docs/history/` |
| 22 | No `final.zip` / `final2.zip` style versioning | File Organization §6 | no zip archives in tree | **yes** | — |
| 23 | Build artifacts out of `Documents/` | File Organization §2:69 | repo is in `ai-workstation/` (allowed, §2:35) | **yes** | — |
| 24 | Identity block (Name/Type/Layer/Status/Repository) | Project Template §1 | `STATUS.md:10-21` | **yes** | refresh `35 M / 7 ??` |
| 25 | Purpose / Architecture / Current State / Next Steps | Project Template §2,3,6,7 | spread across README/STATUS | **partial** | consolidate into `docs/architecture/` |
| 26 | Frozen contracts documented for agents | (project-local, AGENTS.md:33-43) | present and code-verified (see below) | **yes** | — |

---

## Proposed target tree

Only `docs/`, `scripts/` and `.gitignore` change. `crates/`, `desktop/`, root
manifests and `.github/` stay byte-identical.

```
heterion-router/
├── README.md · STATUS.md · CHANGELOG.md · AGENTS.md · LICENSE · THIRD_PARTY_NOTICES.md
├── Cargo.toml · Cargo.lock · rust-toolchain.toml · .gitignore          # unchanged
├── crates/                                                            # FROZEN
├── desktop/                                                           # FROZEN
├── docs/
│   ├── README.md                                    NEW  index of everything below
│   ├── architecture/                                NEW
│   │   ├── OVERVIEW.md                              extracted from README.md:45-53
│   │   ├── LAYERING.md                              crate ownership rules; why gateway/src/routing.rs
│   │   │                                            stays in gateway (implements ChatBackend)
│   │   └── FROZEN-CONTRACTS.md                      enc:v1 salt, ports 20128/20129, env precedence,
│   │                                                data-dir fallback, launchd label
│   ├── adr/                          UNCHANGED PATH  11 ADR + README.md  (Obsidian symlink target)
│   ├── planning/                     DEFERRED        (MISSION-2026-10.md, ROADMAP-2026-09.md move here
│   │                                                only AFTER MISSION-2026-10 ships — see Risky R-2)
│   ├── research/                      UNCHANGED PATH
│   │   ├── PROJECT-AUDIT-2026-09.md · CHATHUB-2026-09.md
│   │   ├── chathub-evidence/
│   │   ├── mission-2026-10/
│   │   └── audit-2026-10/                           this report + siblings 01/02/03
│   ├── qa/                          BLOCKED         TEST-MATRIX.md — mission WS-G, gated on Q8
│   ├── runbooks/                    NOT PROPOSED    see note below
│   └── upstream-omniroute-js/       RENAME          ← git mv docs/history/ (mission A6)
├── scripts/
│   ├── ci/check.sh                   UNCHANGED       CI path (ci.yml:42) + own exec-bit gate
│   ├── service/                      BLOCKED         plist.in/install.sh/uninstall.sh/README.md
│   │                                                — mission WS-F, gated on Q2
│   └── qa/                           BLOCKED         sanitize-fixtures/check-docs/canary/scrub-canary
│                                                    — mission MR-16…MR-18
├── .github/workflows/                ci.yml · release.yml   UNCHANGED (path-filter change = Risky R-3)
└── media/                            NOT PROPOSED    showcase layer owns screenshots
```

**Why there is no `runbooks/`:** the mission already assigns the operational
runbook to `scripts/service/README.md` (`mission-2026-10/05-desktop-release.md:537`).
A second copy under `docs/runbooks/` would violate File Organization §4
("не дублировать — один источник истины"). If MISSION-2026-10 is descoped,
the runbook belongs in `docs/operations/` instead — but only one of the two,
never both.

### Mis-layered code — findings

| Item | Verdict |
|---|---|
| `crates/heterion-router-gateway/src/routing.rs` (672 L) vs `crates/heterion-router-routing/` (270 L) | **Name collision, NOT a layering error.** `routing/src/lib.rs:4-7` states the crate owns "the *decision* … never the backends themselves, so there is no dependency cycle with the gateway"; `gateway/src/routing.rs` implements `ChatBackend` (`backend.rs`) over that decision. Moving it into the `routing` crate would invert the dependency the ADR-001-era layering deliberately avoids. → **document in `docs/architecture/LAYERING.md`, do not move.** |
| `crates/heterion-router-gateway/src/main.rs` (543 L) with `mod tests` (446) and `mod parent_watch_tests` (524) | Acceptable; `cargo clippy --all-targets` covers bins. No action. |
| Tests inline in 38 of 48 src files (47 `#[cfg(test)]` blocks) vs 7 `tests/` files | Coverage is thinner than structure suggests (`STATUS.md:166-167`). Extracting is **BLOCKED** — mission WS-G already prescribes the exact targets (`src/test_support.rs`, `tests/admin_contract.rs`, `tests/alias_construction.rs`, `tests/responses_stream_contract.rs` — `06-qa-docs.md` §11). Do not pre-empt it. |
| `crates/heterion-router-gateway/src/{gemini,grok_cli,openai,responses}.rs` are provider transports living in the HTTP/app crate | Deliberate: they need `ChatBackend` + `AppState`. Transport-specific logic that does *not* could move to `heterion-router-translate/`, but no concrete offender was found. No action. |
| `desktop/scripts/stage-sidecar.sh` is a *second* script root | Real inconsistency, but it is referenced from `tauri.conf.json:14` (with `cwd: ".."` ⇒ relative to `desktop/`) and `desktop/package.json:9`. Consolidating = **Risky R-4**. |

### ADR staleness — code-verified

Every ADR's *falsifiable* claim was re-checked against the working tree.

| ADR | Claim | Verification | Result |
|---|---|---|---|
| 001 | "Rust reads tables **by name**, without `SELECT *`" | `grep -rn 'SELECT \*' crates` → 0 hits | **holds** |
| 002 | SPA in `desktop/ui/dist`, served by the gateway; `tower-http` not added | `ui.rs:3,30`; `grep -rn 'tower-http' Cargo.toml crates/*/Cargo.toml` → 0 hits | **holds** |
| 002 addendum | primary env `HETERION_ROUTER_UI_DIR`, fallback `OMNIROUTE_UI_DIR` | `ui.rs:23` | **holds** |
| 003 | proxy `/api`, `/v1`, `/healthz` → `20128`, `VITE_GATEWAY_URL` overrides | `vite.config.js:7,16` | **holds** |
| 004 | `PATCH /api/combos/{id}`, `PATCH /api/connections/{id}` | `lib.rs:150,152` | **holds** |
| 005 | `sk-` + 32 hex, 11-char prefix, sha256 lookup | `apikeys.rs:4-5,17,28,44` | **holds** |
| 006 | restart only under launchd, else `409`; `sleep 2` before kickstart | `restart.rs:46,50` | **holds** |
| 007 | sidecar injected as `window.__HETERION_ROUTER_GATEWAY_URL__` | `desktop/src-tauri/src/lib.rs:85`; `desktop/ui/src/api.js:41` | **holds** (addendum) |
| 007 body | "spawns its binary `omniroute-gateway`" | `tauri.conf.json:33` = `heterion-router-gateway` | **superseded by its own addendum** (ADR-007 "Addendum 2026-09-29"), and ADR-008 §"не переписываются" forbids rewriting → **intentional, not stale** |
| 008 | `STATIC_SALT = "omniroute-field-encryption-v1"`, prefix `enc:v1:` | `crypto/src/lib.rs:18,23` | **holds** |
| 008 | CI workflow is `ci.yml`, not `rust.yml` | addendum 58-59; `.github/workflows/` listing | **holds** |
| 009 | `0700/0600` boundaries, `no_log`, hash-only issuance, owner-scoped store | narrowed by its own 2026-09-29 addendum | **holds as narrowed** |
| 010 | no `chathub` registry entry, no ChatHub credentials in repo | `grep -ri chathub crates/` → 0; evidence JSON parses and contains only synthetic ids | **holds** |
| 011 | "`22` calls `wiremock.expect()`, `0` calls `verify()`" | 0 `verify()` holds; **22 → 34 `Mock::given` sites** | **partly stale** |
| — | ADR-001…007 all `accepted`, header `> Дата · Статус` | vs ADR-008…011 `- Статус` | **format drift** |

**Conclusion:** no ADR is *false*. Two are imprecise (ADR-011 test count) and
one is deliberately superseded-in-place (ADR-007). The real ADR problem is
**not content — it is that 3 of 11 ADRs are untracked and the index already
points at them**, plus inconsistent header format and a forward-number
collision (`ADR-011-responses-state-contract.md` proposed in
`04-security-retention.md:1196` vs the real `ADR-011-mission-2026-10-scope.md`).

### Documentation gaps the standards require

| Gap | Required by | Severity |
|---|---|---|
| No `docs/architecture/` — the only architecture description is a 9-line sketch in README | Project Template §4:38 | medium |
| No `docs/README.md` index — 31 files, entry points split across 5 different docs | File Organization §1 (files divided by purpose) | medium |
| No runbook for the launchd service; README:23-31 documents a service the repo cannot reproduce (no plist) | File Organization §2 (operability); already MR-10 | high (operational) |
| No `docs/adr/ADR-011` collision resolution; proposed `SECURITY.md` / `THREAT-MODEL.md` / `qa/TEST-MATRIX.md` are dangling refs | mission MR-16 DOC-2/DOC-7 | medium |
| `crates/heterion-router-providers/src/lib.rs:3-4` stale count + non-existent `rust-rewrite/PLAN.md` | — (code comment) | low |
| No fixture provenance (`tests/fixtures/PROVENANCE.json`) for 6 parity fixtures | mission MR-17 GOV-5 | medium |
| Obsidian `Decisions.md` missing ADR-009/010/011 | File Organization §2:80 | medium |
| Showcase `STATUS.md` contradicts ADR-007 addendum on single-instance | File Organization §2:68 | medium |
| No `rustfmt.toml` / `deny.toml` — formatting & dependency policy is implicit | (hygiene, not a standard rule) | low |

---

## Safe changes to execute now

Docs-only or ignore-only. No build, no CI, no Cargo manifest touched.
Ordered; each is independently verifiable.

**S1 — `git add` the untracked documentation block.**
Scope: `docs/MISSION-2026-10.md`, `docs/ROADMAP-2026-09.md`,
`docs/adr/ADR-009-*`, `ADR-010-*`, `ADR-011-*`, `docs/history/README.md`,
`docs/research/**` (25 files, 7 365 L, excluding the 4 `audit-2026-10/` reports).
Why safe: adds tracked files only; nothing moves; `scripts/ci/check.sh` is
unaffected (its exec-bit gate only inspects `scripts/`).
Verify: `git ls-files docs/ | wc -l` goes **12 → 41** (12 tracked today: 8 ADR +
`adr/README.md` + 3 `history/` reports; +25 pre-existing untracked; +4
`audit-2026-10/` reports); `git check-ignore` output unchanged.
Cross-ref: mission action A2 (`MISSION-2026-10.md:165`), M0/O-4.

**S2 — extend `.gitignore` with the missing template rules.**
Add to `.gitignore` (append under the existing Node block at line 12-14 and a
new data block):
```
node_modules/
dist/
build/
*.sqlite
*.sqlite-wal
*.sqlite-shm
*.db
*.db-wal
*.db-shm
artifacts/
```
Why safe: `git check-ignore -v` shows the only currently-matching paths are
`desktop/ui/{node_modules,dist}` (already covered by `desktop/ui/.gitignore`);
the new rules are strictly additive and match nothing that exists in the tree.
Zero build impact — nothing in `Cargo.toml`, `ci.yml`, `check.sh`, `tauri.conf.json`
consults `.gitignore`.
Verify: re-run the `git check-ignore` table; `git status --porcelain` must stay at
`35 M / 7 ??` → `35 M / 7 ??` (nothing newly hidden).
Satisfies compliance rows 6, 7, 8.

**S3 — remove the tracked 0-byte artifact.**
`git rm --cached desktop/src-tauri/icons/.!15396!128x128.png` (and delete it).
Why safe: it is an AppleDouble stub with 0 bytes; no manifest, workflow, or
`tauri.conf.json` icon list (`tauri.conf.json:26-32`) references it — that list
names only `32x32.png`, `128x128.png`, `128x128@2x.png`, `icon.icns`, `icon.ico`.
Verify: `grep -c '15396' tauri.conf.json crates -r` → 0; `cargo tauri build`
icon resolution unaffected.
Cross-ref: ROADMAP P2-13 (line 103).

**S4 — create `docs/README.md` (new index, ~40 lines).**
Pure addition. Maps: architecture (README sketch + frozen contracts), decisions
(`adr/`), planning (`MISSION-2026-10.md`, `ROADMAP-2026-09.md`), evidence
(`research/`), frozen upstream (`history/`), and states which document is
authoritative for which class of fact.
No existing file changes → no link can break.
Satisfies compliance rows 16, 21.

**S5 — create `docs/architecture/` with three short documents.**
`OVERVIEW.md` (move the `README.md:45-53` sketch text in), `LAYERING.md`
(record the `gateway/src/routing.rs` vs `routing/` split and the "decision vs
backend" rule from `routing/src/lib.rs:4-7`), `FROZEN-CONTRACTS.md` (table of
`STATIC_SALT`/`enc:v1:`, ports, env precedence, data-dir fallback, launchd
label, each with the code line that proves it).
Why safe: new files; then *replace* `README.md:45-53` with a 3-line link —
that edit touches no code, no CI path filter, no build.
Verify: `scripts/ci/check.sh` unaffected; `cargo build -p heterion-router-gateway`
unaffected (no `include_str!` on `docs/`).
Satisfies compliance rows 12, 25.

**S6 — rename `docs/history/` → `docs/upstream-omniroute-js/` (git mv) + fix 20 refs.**
`git mv docs/history docs/upstream-omniroute-js`, then update exactly:
`README.md:102`; `STATUS.md:170,186`; `AGENTS.md:14`; `CHANGELOG.md:83`;
`docs/adr/README.md:5`; `docs/adr/ADR-008:23,56,60,62` (append to the existing
"Фактическое уточнение 2026-09-29" block rather than editing the historical
decision text); `docs/MISSION-2026-10.md:57,165,169,542`;
`docs/research/PROJECT-AUDIT-2026-09.md:174`;
`docs/research/mission-2026-10/01-mission-sequencing.md:41,228,232,650,769`.
Why safe: docs-only; `docs/adr/` untouched (the Obsidian symlink target);
not in any CI `paths` filter (`ci.yml:10-25`); no `include_str!` on `docs/`.
Verify: `grep -rn 'docs/history' --include='*.md' . | grep -v '^\./target'` → 0 hits.
Cross-ref: mission action A6 (`MISSION-2026-10.md:169`) — explicitly a
`git mv`, **never deletion** (risk R-11).

**S7 — fix the two stale code comments (2-line edit, no behaviour change).**
`crates/heterion-router-providers/src/lib.rs:3` `277 providers` → `278`;
line 4 `rust-rewrite/PLAN.md` → `docs/research/PROJECT-AUDIT-2026-09.md`
(or simply delete the dangling parenthetical).
Why safe: doc-comment only; `clippy -D warnings` does not read prose;
`cargo test` unaffected.
Verify: `grep -rn 'rust-rewrite' crates` → 0 hits.

**S8 — reconcile the two count invariants.**
`STATUS.md:19` and `MISSION-2026-10.md:57`: `5 ??` → `7 ??` (or, after S1,
`0 ??`). `STATUS.md:167`: `22` → `34` `Mock::given` sites. `ADR-011 §4`:
same 22 → 34.
Why safe: prose only. Prefer running `cargo test --workspace -- --list` first
to settle 213 vs 215, then record the verified number everywhere it appears
(`STATUS.md:137`, `ROADMAP-2026-09.md:16,34`, `MISSION-2026-10.md`,
`CHANGELOG.md:15`).
Verify: one grep for the number returns the same value in all 5 places.

---

## Risky changes (need care)

**R-1 — ADR header normalisation (ADR-001…007 → `- Статус/Дата` bullets, or the reverse).**
Touches 7 files; the choice of direction is itself a decision because
ADR-008 §"не переписываются" applies to *content*, not formatting.
References to enumerate: `docs/adr/README.md:12-18` (index rows),
`ADR-002/006/007/008` addendum sections (which quote the old header style),
`docs/research/mission-2026-10/06-qa-docs.md` DOC-3 convention.
Risk: rewriting a header changes a historical record. Mitigation: append a
metadata block, never delete the original line.
Class: **RISKY** (touches historical records, not build).

**R-2 — move `MISSION-2026-10.md` + `ROADMAP-2026-09.md` → `docs/planning/`.**
Fan-out: 14 markdown files reference one or both
(`grep -rln 'MISSION-2026-10\|ROADMAP-2026-09' --include='*.md'` →
CHANGELOG, STATUS, AGENTS, README, adr/README, ADR-008, ADR-011,
ROADMAP itself, and all 6 mission evidence reports).
Additional risk: `AGENTS.md:17-18` names `docs/MISSION-2026-10.md` as **the**
mission entry point for every agent; moving it mid-mission is exactly the
"entry point drift" that ADR-011 §7 tries to prevent.
Verdict: docs-only, but high fan-out + active-mission churn.
**Recommendation: defer until MISSION-2026-10 closes.** Apply `docs/planning/`
as a forward convention from ADR-012 onward.
Class: **RISKY**.

**R-3 — extend CI `paths` filters to `docs/**` and `desktop/src-tauri/**`.**
`ci.yml:10-16` and `:19-25`. Effect: docs-only and src-tauri-only PRs start
running CI (more minutes, and the `ui` job would run on changes it doesn't
need). Also fixes the `docs/` gap that lets a broken cross-link merge silently.
Class: **RISKY** (build/CI config). Must be paired with a decision on whether
`desktop/src-tauri` gets a real job (Q8) or only a trigger.

**R-4 — consolidate `desktop/scripts/` into root `scripts/`.**
Must update, in lockstep:
`tauri.conf.json:14` (`beforeBuildCommand.script = "… && sh scripts/stage-sidecar.sh"`,
with `cwd: ".."` → resolves to `desktop/scripts/stage-sidecar.sh`);
`desktop/package.json:9` (`"build:sidecar": "sh scripts/stage-sidecar.sh"`,
cwd = `desktop/`);
`stage-sidecar.sh:10` (`ROOT="$(cd "$(dirname "$0")/../.." && pwd)"` → becomes
`../../..`) and `:15-17` (`$ROOT/desktop/src-tauri/binaries`);
`ci.yml` `paths` (add the new path or keep `scripts/**`);
optionally `check.sh:12` (`git ls-files -s scripts/` would then cover the
sidecar script's exec bit — a **benefit**, but also a new CI failure mode if
the mode is wrong).
Also note `desktop/scripts/` currently escapes the exec-bit gate entirely.
Class: **RISKY** (build config; a wrong `ROOT` silently stages into the wrong
directory, and `externalBin` then fails only at bundle time).

**R-5 — extend `check.sh`'s exec-bit gate to all scripts.**
`check.sh:12` currently reads `git ls-files -s scripts/`. Widening it to
`git ls-files -s '*.sh'` is a one-line change but instantly fails CI if any
script anywhere lacks mode 755. Both current `.sh` files are `100755`
(`git ls-files -s '*.sh'`), so it should pass — verify before committing.
Class: **RISKY** (build/CI gate; cheap to verify, cheap to break).

**R-6 — release checksum portability.**
`release.yml:44` `sha256sum` on a matrix that includes `macos-14`
(`release.yml:34-35`). macOS ships `shasum`, not `sha256sum`.
Class: **RISKY** (release pipeline) — and it is currently a latent break, not a
cosmetic issue, since `macos-14` jobs would fail at the last step after a
successful build.

**R-7 — delete the 3.8 G + 4.0 G of build artifacts.**
Not a repo change, but it is the single biggest disk item and the mission's
M0 "fresh clone" gate depends on being able to rebuild. Sequence it *after*
`scripts/ci/check.sh` has been run once green from the current tree.
Class: **RISKY** (destructive, irreversible time cost).

---

## Blocked / needs decision

**B-1 — Is the ADR numbering scheme frozen or open?**
`mission-2026-10/04-security-retention.md:1196,1199` propose
`ADR-011-responses-state-contract.md` and `ADR-012-key-and-credential-retirement.md`,
but **ADR-011 is already `mission-2026-10-scope`**. Someone must declare the
next free number (012, 013…) and correct the evidence report, or renumber.
Owner decision. Blocks any ADR-driven task in WS-D/WS-C.

**B-2 — Launchd plist in-repo (mission Q2).**
`STATUS.md:161-162`, `README.md:23-31`, `AGENTS.md:46-53` document a service
that a fresh clone cannot reproduce (no plist tracked). Mission WS-F proposes
`scripts/service/{com.heterion.router.plist.in,install.sh,uninstall.sh,README.md}`.
Until the owner answers Q2, either the service docs must be qualified or the
files must be added. **Do not invent the plist.**

**B-3 — Desktop in CI (mission Q8).**
Needed to close the `desktop/src-tauri` fmt/clippy/test gap (`STATUS.md:138-142`,
2 fmt diffs reported in `build.rs:1` and `src/lib.rs:81`, 0 tests).
Owner decision on runner cost + macOS requirement.

**B-4 — Responses store at-rest encryption (mission Q3/Q4) and whether the
store is wanted at all (0 production rows, `STATUS.md:97`).**
Determines whether `docs/SECURITY.md` and `docs/adr/ADR-0xx-responses-state-*`
are written at all. Owner decision; already blocking in ADR-011 §6.

**B-5 — `docs/qa/TEST-MATRIX.md` + `scripts/qa/*.sh`.**
Proposed in `mission-2026-10/06-qa-docs.md:954` and §11. Creating the directory
before WS-G starts would produce a doc that no gate enforces. Defer.

**B-6 — Fail-closed vs JS-parity for legacy NULL policy columns, and the
remaining 7 owner questions.**
`ADR-011 §6` / `MISSION-2026-10.md §13`. Blocking by design; nothing to
reorganise until answered.

**B-7 — Does `docs/` keep `research/` inside the repo at all?**
File Organization §4 allows repo `docs/` for "техдокументация" and Obsidian for
"исследования". The project currently keeps 17 `.md` + 6 `.json` of research and
audit evidence in-repo (13 pre-existing + the 4 `audit-2026-10/` reports), and
symlinks only `docs/adr` into Obsidian. That is compliant (research here is
project evidence, not personal notes), but it is a deliberate choice worth
stating explicitly in `docs/README.md` (S4) rather than changing.

**B-8 — Should `docs/adr/README.md` gain a "superseded / narrowed" column?**
ADR-007 and ADR-009 carry addenda that narrow the original text; ADR-006 and
ADR-002 likewise. A status column (`accepted / accepted+narrowed / superseded`)
would make the ADR index self-describing. Touches the index contract that
mission DOC-3 wants to standardise — sequence after B-1 and R-1.

---

## Do NOT reorganize

| Path | Why not |
|---|---|
| `crates/` layout (10 members) | Pinned in four independent places: `Cargo.toml:9-20` (workspace members), `Cargo.toml:33-41` (path deps), `Cargo.toml:23` (the `exclude`), `ci.yml:13,22` (`paths: crates/**`), `release.yml:33` (`-p heterion-router-gateway`). Renaming any crate also churns `Cargo.lock` and the frozen binary name `heterion-router-gateway`. `AGENTS.md:9-10` documents the layout as the agent contract. |
| `desktop/src-tauri` | Excluded from the workspace **by design** (`Cargo.toml:21-23`: "heavy, standalone binary app: keep it out of the fast gateway workspace/CI loop"). It also has its own `Cargo.lock` and its own `.gitignore`, and `AGENTS.md:30-32` notes workspace fmt does not cover it. |
| `desktop/ui` (path) | `crates/heterion-router-gateway/src/ui.rs:30` resolves the SPA at **compile time** via `env!("CARGO_MANIFEST_DIR")/../../desktop/ui/dist`. Moving `desktop/ui`, or moving the gateway crate, silently changes (or breaks) the default UI directory. Also `tauri.conf.json:7` `frontendDist: "../ui/dist"`. |
| `crates/heterion-router-providers/registry.json` | Embedded with `include_str!("../registry.json")` at `registry.rs:65`. Crate-relative: moving it turns a load-time panic into a compile error, and it is a 278-provider contract guarded by an insta snapshot (`registry.rs:341-344`). |
| `crates/heterion-router-db/schema_base.sql` | Embedded with `include_str!("../schema_base.sql")` at `migrate.rs:16`. Same constraint. |
| `crates/heterion-router-translate/tests/fixtures/` | Six fixtures referenced by `include_str!("fixtures/…")` from `parity.rs:41`, `claude_parity.rs:27`, `claude_request_parity.rs:28`, `gemini_parity.rs:27`, `tools_parity.rs:38`, `request_parity.rs:28`. |
| `crates/heterion-router-providers/src/snapshots/*.snap` | `AGENTS.md:16`: "Тестовые снепшоты (`*.snap`) — часть контракта; править только вместе с кодом." Insta derives the path from the crate name. |
| `docs/adr/` (the path itself) | `Obsidian Vault/Projects/heterion-router/ADR` is a symlink to exactly this directory, and File Organization §2:82 mandates that layout. Renaming it silently breaks the Obsidian graph. |
| `scripts/ci/check.sh` (the path) | Invoked as a literal relative path by CI (`ci.yml:42`), self-referencing `ROOT` at `check.sh:6`, listed in the CI `paths` filter (`ci.yml:16,25`), and it gates the exec bit of everything under `scripts/`. |
| `Cargo.toml`, `Cargo.lock`, `rust-toolchain.toml` | Root-level, listed in the CI `paths` filter, and `rust-toolchain.toml` is the version contract (`1.98.0`, `rust-version = "1.98"` in `Cargo.toml:28`). |
| `crates/heterion-router-gateway/src/routing.rs` → `routing` crate | Not mis-layered: `routing/src/lib.rs:4-7` explicitly states the crate owns the *decision* and "never the backends themselves, so there is no dependency cycle with the gateway". `gateway/src/routing.rs` implements `ChatBackend` from `backend.rs` — moving it inverts the intended dependency direction. Document in `LAYERING.md` instead. |
| The frozen interop constants (`STATIC_SALT`, `enc:v1:`, ports, env precedence, data-dir fallback) | `AGENTS.md:33-43` and ADR-008 §6. Live databases on other machines hold values derived from them. Not a file-layout question at all. |

---

## Commands run

All read-only. Run from `/Volumes/Netac_SSD/ai-workstation/Projects/heterion-router`.

```
git rev-parse --abbrev-ref HEAD; git log --oneline -5
git status --porcelain
git status --porcelain --ignored | grep '^!!'
git ls-files | wc -l                                   # 143
git ls-files --error-unmatch <path>                    # tracked/ignored probes
git ls-files -s '*.sh'                                 # exec-bit modes
git ls-files | while read f; do [ -f "$f" ] && du -k "$f"; done | sort -rn | head -20
git ls-files | while read f; do [ -f "$f" ] && [ ! -s "$f" ] && echo EMPTY "$f"; done
git show HEAD:crates/heterion-router-providers/registry.json | python3 -c "…"
git check-ignore -v <path>                             # ×26 candidate paths
git diff --stat                                        # (not expanded; M-list from git status)

find . -type d \( -name target -o -name node_modules -o -name dist -o -name .git \) -prune -o -type f -print | sort
find docs -type f | while read f; do wc -l; stat -f '%Sm' -t '%Y-%m-%d'; du -k; done
find . -path ./target -prune -o -name '*.rs' -print | xargs wc -l | sort -rn
find crates desktop/src-tauri/src desktop/src-tauri/build.rs -name '*.rs' | xargs wc -l | tail -1   # 16915
for c in crates/*/; do wc -l src/; grep -c '#\[test\]' '#\[tokio::test\]' '#\[cfg(test)\]'; done
find crates -path '*/src/*' -name '*.rs' | wc -l        # 48
find crates -path '*/tests/*' -name '*.rs' | wc -l     # 7
find . -name '*.snap' -not -path './target/*' | wc -l  # 1
find . \( -name target -o -name node_modules -o -name .git -o -name dist \) -prune -o -name '*.md' -print | xargs wc -l | tail -1   # 8502 / 36 files
                                                                                                                          # (naive `-not -path './target/*'` walks into
                                                                                                                          #  desktop/src-tauri/target and reports ~19.4k — wrong)
find desktop/ui/src desktop/ui/index.html desktop/ui/{vite,svelte}.config.js -type f | xargs wc -l
du -sh target desktop/src-tauri/target desktop/ui/node_modules desktop/ui/dist

grep -rn 'SELECT \*' crates
grep -rn 'STATIC_SALT\|omniroute-field-encryption-v1\|enc:v1:' crates
grep -rn 'UI_DIR\|SERVICE_LABEL\|RUST_PORT' crates/*/src/*.rs
grep -rn 'tower-http\|tower_http' Cargo.toml crates/*/Cargo.toml
grep -rn '__HETERION_ROUTER_GATEWAY_URL__\|__OMNIROUTE_GATEWAY_URL__' crates desktop
grep -n '\.route(\|patch(\|Router::new' crates/heterion-router-gateway/src/lib.rs
grep -rn 'include_str!\|include_bytes!' crates desktop/src-tauri/src desktop/src-tauri/build.rs
grep -rn 'registry.json\|schema_base' crates
grep -rn 'docs/' crates/*/src/*.rs crates/*/Cargo.toml desktop/src-tauri/src/*.rs desktop/src-tauri/Cargo.toml
grep -rn 'Mock::given' crates | wc -l                    # 34
grep -rn '\.verify()' crates | wc -l                     # 0
grep -rn 'proptest\|tarpaulin\|llvm-cov\|criterion' crates
grep -rn 'single_instance' desktop/src-tauri/src/lib.rs
grep -rn 'docs/history' --include='*.md' .
grep -rln 'docs/research' --include='*.md' .
grep -rln 'MISSION-2026-10\|ROADMAP-2026-09' --include='*.md' .
grep -rhno 'docs/[A-Za-z0-9_./-]*\.md' --include='*.md' docs *.md | sed 's/^[0-9]*://' | sort -u   # → EXISTS/MISSING
grep -rn 'ADR-011-responses-state-contract\|ADR-012' docs/research/mission-2026-10/04-security-retention.md
grep -n 'paths:\|crates/\*\*\|desktop/ui/\*\*\|scripts/ci/\*\*\|rust-toolchain@stable\|working-directory' .github/workflows/ci.yml
grep -n 'rust-toolchain@stable\|cargo build\|sha256sum\|os: \[' .github/workflows/release.yml
grep -rn 'rust-rewrite' crates
python3 -c "import json; …"                            # registry + 6 chathub captures
ls -d rust-rewrite media docs/architecture scripts/qa scripts/service .env.example docs/README.md …
```

**Not run (deliberately):** `cargo fmt/clippy/test`, `cargo tauri build`,
`npm --prefix desktop/ui build`, any `sqlite3` access against
`~/.heterion-router` or `~/.omniroute`, any `launchctl` call. All would write
outside the read-only scope.

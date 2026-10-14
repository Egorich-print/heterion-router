# 04 — Security & Data-Retention Planning Audit

**Subject:** `heterion-router` (Rust LLM gateway + Tauri desktop shell)
**Canonical repo:** `~/ai-workstation/Projects/heterion-router` @ `51dfe6d` (working tree, dirty — includes `docs/adr/ADR-009`, `ADR-010`, `docs/research/PROJECT-AUDIT-2026-09.md`, `docs/ROADMAP-2026-09.md`)
**Author role:** principal architect 4/6 (read-only audit; no canonical writes, no commits, no live-data mutations)
**Date:** 2026-09-29
**Companion docs:** `docs/adr/ADR-009-runtime-security-boundaries.md` (decision + 2026-09-29 addendum), `docs/research/PROJECT-AUDIT-2026-09.md` §"Security and data findings" (the authoritative current matrix this document refines and extends), `docs/ROADMAP-2026-09.md` §"Security and operations status".

---

## 0. Method and evidence base

Read-only. Every claim below is anchored to `file:line` in the working tree. No
secret values, key material, or live row contents were printed; no production
mutation, no `sqlite3` write, no service restart. Probes were static reads plus
`git log`/`git ls-files`.

Classification vocabulary used throughout:

| Class | Meaning |
|---|---|
| **VERIFIED** | Control exists in code, is on the live path, and its behaviour matches the documented contract. |
| **PARTIAL** | Control exists but is incomplete, order-dependent, fail-open, or narrower than the docs claim. |
| **NEW** | No control exists today. Proposal only. |

`docs/research/PROJECT-AUDIT-2026-09.md` already registers eight open findings.
This audit **confirms all eight** and adds **nine** previously unregistered ones
(§2.4, §2.6, §2.8, §2.9, §2.12 are the net-new material; §3.6 and §3.8 also
surface items the earlier audits ranked lower than this one does).

---

## 1. Threat model and explicit assumptions

### 1.1 What the system is

A single-process axum gateway on one developer machine. It holds every provider
credential for every configured upstream in one SQLite file, terminates an
OpenAI-compatible API, and serves a Svelte dashboard from its own origin. A
Tauri v2 shell may spawn it as a sidecar. It is a **credential broker and
transcript store first**, a router second.

### 1.2 Assets, ranked

1. **Provider credentials** — `provider_connections.access_token`,
   `refresh_token`, `api_key` (schema `crates/heterion-router-db/schema_base.sql:1-47`).
2. **The field-encryption secret** — `STORAGE_ENCRYPTION_KEY`, which decrypts (1).
3. **Conversation transcripts** — full request *and* response envelopes written
   verbatim into `key_value` (`crates/heterion-router-gateway/src/lib.rs:573`).
4. **The operator API key** — grants full `manage` scope including
   `POST /api/restart` and key issuance.
5. **Usage/telemetry** — `usage_history`, `call_logs`, `gateway.log*`,
   `gemini-thought-signatures.jsonl`.
6. **Availability** — upstream balance, connection pool, gateway port.

### 1.3 Adversaries in scope

| # | Adversary | Capability assumed |
|---|---|---|
| A1 | **LAN peer** | Reaches `0.0.0.0:20128` (the *installed* configuration per `AGENTS.md` and the live audit). No key. |
| A2 | **Hostile web origin** | Runs a page at some origin the CORS layer evaluates; controls nothing else. |
| A3 | **Compromised / malicious upstream** | Returns arbitrary SSE frames, error bodies, and an arbitrary `response.id`. |
| A4 | **Local unprivileged process, different user** | Reads what file modes permit. |
| A5 | **XSS or supply-chain compromise in the Tauri webview** | Runs JS inside the trusted origin. |
| A6 | **The operator** | Trusted. Controls env vars, the launchd plist, and the filesystem. |

### 1.4 Assumptions I am *making* (flagged because findings depend on them)

- **A-1 (deployment).** The gateway is **single-operator, single-host**. The
  documented live service binds `0.0.0.0:20128` under a machine-local launchd
  plist, which is not in the repository. This is the weakest assumption in the
  model: if the service is ever LAN-shared between operators, the `manage`-scope
  API key becomes a single-tenant takeover primitive and every Medium below
  becomes High.
- **A-2 (transport).** There is **no TLS anywhere in the process**
  (`crates/heterion-router-gateway/src/main.rs:438-441` binds a plain `TcpListener`
  and `axum::serve`s it). LAN peers therefore read the bearer token off the wire
  in cleartext. ADR-009 lists "TLS termination" as a next milestone; I rate it
  the highest-impact *unaddressed* item for the `0.0.0.0` configuration.
- **A-3 (trust anchor).** The SQLite file at `~/.heterion-router/storage.sqlite`
  is the trust anchor. There is no OS keychain integration; the encryption key
  lives in the same directory (§2.3). "At rest" therefore means "protected by
  file modes", nothing more.
- **A-4 (upstream honesty).** Upstreams are treated as semi-trusted: they see
  prompts, and their *error text* is treated as attacker-influenced (it flows
  into SSE frames and `gateway.log`).
- **A-5 (operator honesty).** Env vars and the launchd plist are trusted. Findings
  that require operator control (§2.8 UI dir, §2.11 restart script interpolation)
  are rated Low accordingly.
- **A-6 (out of scope).** Supply-chain integrity of crates/npm, physical attacks,
  and a malicious operator are out of scope except as noted.

### 1.5 Explicitly out of scope for this document

Registry coverage/alias correctness, Responses protocol fidelity, native
Anthropic Messages, Vite dev-proxy ports. These are in
`docs/ROADMAP-2026-09.md` and are not security findings.

---

## 2. Findings by domain

### 2.1 Auth policy defaults — **PARTIAL**

**VERIFIED (the good part).**

- Extractor-per-handler auth, not middleware, so `/healthz` is public by
  construction and every protected handler opts in explicitly
  (`crates/heterion-router-gateway/src/auth.rs:8-9`, `:95`; route table
  `crates/heterion-router-gateway/src/lib.rs:145-171`).
- `Authenticated` is declared *before* `Json<T>` in every handler
  (`lib.rs:225-229`, `lib.rs:825-829`), so axum rejects an unauthenticated
  request **before** buffering the body. This is correct and worth preserving —
  it is the reason a 2 MiB body cap is not itself a DoS amplifier for anonymous
  callers.
- Fail-closed on infrastructure errors: a DB error yields `503 auth_unavailable`,
  not an open door (`auth.rs:106-111`, `:136`; test `auth_db_failure_does_not_open_the_gateway`,
  `lib.rs:1597`).
- Non-loopback bind refuses startup without a key
  (`main.rs:370-384`; tests `main.rs:476`, `main.rs:502`).
- `manage` scope is a hard gate on every `/api/*` handler
  (`admin.rs:53,92,140,198,240,318,327,379,406,449,511,606,716`).
- Model allow-list is checked **before** dispatch on both surfaces
  (`lib.rs:230`, `lib.rs:850`).

**PARTIAL — the fail-open defaults.**

- `find_usable_api_key` coalesces NULL policy columns to *maximum privilege*:
  `COALESCE(scopes, '["manage"]')`, `COALESCE(allowed_models, '[]')`,
  `COALESCE(allowed_combos, '["combo/*"]')`
  (`crates/heterion-router-db/src/repos.rs:356-363`). A row written by the frozen
  JS server without those columns authenticates as a **full administrator with
  access to every model**. ADR-009's own addendum concedes this
  (`ADR-009:64-65`); it is unremediated.
- `allowed_models = '[]'` means *allow everything*, not *allow nothing*
  (`auth.rs:54`: `if models.is_empty() || ... { return true }`). This is also the
  base-schema default (`schema_base.sql:104`), so it is not only a NULL problem.
  Combined with `auth.rs:57`, `allowed_combos` is only ever a *second* chance —
  a non-empty `allowed_models` that misses can still be rescued by a combo
  pattern, and a key with `allowed_models: '["nope"]'` and
  `allowed_combos: '["*"]'` reaches everything.
- There is **no way to narrow a key after issuance**. The router exposes
  `GET /api/keys`, `POST /api/keys`, `POST /api/keys/{id}/revoke`,
  `POST /api/keys/{id}/restore` and nothing else (`lib.rs:156-158`). The Rust
  admin API cannot set `scopes`, `allowed_models`, `allowed_combos`, `expires_at`
  or `no_log` — the only way is direct SQL against the live file. For a system
  whose whole auth story is "least privilege", this is the structural gap.
- Keys are issued with **no expiry**: `NewApiKey` has no expiry field
  (`admin.rs:276-284`) and `insert_full_api_key` hardcodes `expires_at = NULL`
  (`repos.rs:237`). A leaked operator key is valid forever until someone
  remembers to revoke it.
- `count_active_api_keys` counts only `revoked_at IS NULL` (`repos.rs:941-948`)
  — it ignores `is_active`, `is_banned` and `expires_at`. It feeds
  `/api/overview` (`admin.rs:68`), so the dashboard can report N usable keys
  when zero can authenticate.
- **Lockout trap.** `validate_auth_policy` counts *all* rows
  (`main.rs:375`) and `Authenticated` counts *all* rows (`auth.rs:107-114`).
  Revoke every key and the gateway is permanently unreachable: the table is
  non-empty, so auth is required, and the only route to recovery
  (`POST /api/keys/{id}/restore`) itself needs `manage`. Fail-closed, but
  unrecoverable through the product surface. NEW finding.

**Classification:** **PARTIAL** — the enforcement machinery is verified; the
defaults and the missing narrowing path are the gap.

---

### 2.2 Key hashing and migration — **PARTIAL**

**VERIFIED.**

- New issuance is hash-only: `insert_full_api_key` writes `key_hash` into *both*
  the `key` and `key_hash` columns (`repos.rs:252-253`), so a Rust-issued secret
  is never persisted in recoverable form.
- SHA-256 of a 128-bit CSPRNG secret (`apikeys.rs:25-28`, `:44`) is the correct
  construction here — a slow KDF is unnecessary for a full-entropy secret, and
  the choice is dictated by JS interop (`ADR-005`).
- Kill switches honoured on the hot path: `revoked_at IS NULL`,
  `COALESCE(is_banned,0)=0`, `COALESCE(is_active,1)=1` (`repos.rs:362-363`) plus
  Rust-side expiry parsing that **rejects** malformed timestamps rather than
  extending a key's life (`repos.rs:385-388`, `:403-413`; ADR-005:18-20).
  The direction of that choice is right and rare.
- Secret is returned exactly once at creation; list endpoints select metadata
  only and derive an `is_encrypted` flag from a `LIKE` predicate, never the
  secret (`repos.rs:288-290`, `admin.rs:209-225`).
- Revocation is immediate because the table is read per request (`ADR-005:26`;
  `auth.rs:106`).

**PARTIAL.**

- **No migration exists.** `MIGRATIONS` contains exactly one entry
  (`migrate.rs:32-36`). Legacy plaintext rows are matched by
  `WHERE key = ?1 OR key_hash = ?2` (`repos.rs:362`) and stay readable
  **forever**. `insert_api_key` — the plaintext writer — is still `pub`
  (`repos.rs:195-208`) and still callable by any future code path. ADR-009:24-28
  defers this "until a separate migration"; that migration has not been written.
- The `OR` predicate is not constant-time, but the secret space is
  2^128, so it is not exploitable. Noted, not reported.
- `last_used_at` is written on **every** successful authentication
  (`auth.rs:120-123` → `repos.rs:318-324`) regardless of `no_log`. A `no_log`
  key still mutates the database on every request, and unauthenticated traffic
  causes one `COUNT(*)` per request (`auth.rs:107-111`). Write amplification,
  not a leak, but it is why `no_log` is not actually "no writes".

**Classification:** **PARTIAL** — issuance is correct, retirement is absent.

---

### 2.3 Encryption key lifecycle — **PARTIAL**

**VERIFIED.**

- AES-256-GCM with a frozen `enc:v1:<iv>:<ct>:<tag>` format and a frozen static
  salt `omniroute-field-encryption-v1` (`crypto/src/lib.rs:18-23`), pinned for
  live-DB interop and explicitly documented as non-renamable (`crypto:20-22`).
  Ground-truth JS vectors are asserted in tests (`crypto:190-206`).
- Legacy dynamic-salt derivation is tried as a fallback on decrypt, primary
  first (`crypto:123-127`), so a key rotation on the secret does not orphan
  stored rows.
- Keys are 128-bit CSPRNG, fresh IV per encryption, no IV reuse
  (`crypto:86-87`; test `encrypt_is_not_idempotent_reuse_safe`).
- `Debug` is redacted: `FieldCrypto` prints `(<redacted>)` (`crypto:38-42`).
  `GrokCliBackend` likewise hand-writes a `Debug` that exposes only
  `credential_count` (`grok_cli.rs:146-154`). This is a deliberate, correct
  pattern.
- Undecryptable rows are **skipped**, never served broken
  (`credentials.rs:74-85`), and credential selection keeps an unexpired row with
  a refresh token (`credentials.rs:65-70`).

**PARTIAL — the lifecycle holes.**

- **The key is stored beside the ciphertext it protects.** `load_secret` reads
  `STORAGE_ENCRYPTION_KEY` from the process environment, else from
  `<data_dir>/.env` (`crypto:159-177`) — the same directory as
  `storage.sqlite` (`db.rs:53-63`). File-mode protection and encryption protect
  against the same threat (a reader of the directory), so the encryption buys
  nothing against A4 and only helps against a *stolen DB file in isolation* —
  a narrow win, undercut by both being in the same backup/restore domain.
- **A missing key degrades silently, and the worst path writes plaintext.**
  `FieldCrypto::from_secret` returns `None` for an empty secret (`crypto:46-49`),
  `select_backend` propagates `Option` without complaint (`main.rs:97`), and the
  Grok refresh callback then does
  `.unwrap_or_else(|| token.to_string())` (`main.rs:110-120`) — a freshly minted
  OAuth access + refresh token is written **in cleartext** into
  `provider_connections`. There is no `tracing::warn!` anywhere on the
  missing-key path; I grepped the whole workspace and the only key/secret-adjacent
  log lines are `main.rs:52`, `auth.rs:122` and the `base_url` lines, none of
  which mention encryption. Confirmed High.
- **Rotation is undefined.** Changing the secret leaves every stored
  `enc:v1:` value undecryptable; `credentials.rs:77` then silently drops the
  rows, `select_backend` finds no credentials, and providers simply stop being
  built (`main.rs:203`, `:239`, `:297` log at `warn`/`info` only). The operator
  sees "backend X skipped" and a shrinking backend list, with no statement that
  the key is wrong. NEW finding (operational-security class).
- Only **one** secret is accepted (`from_secret(secret: &str)`,
  `crypto:46`); there is no keyring for in-flight rotation.
- `Credential` derives `Debug` (`credentials.rs:15`) and `grok_cli::Credential`
  derives `Debug` (`grok_cli.rs:157`) — both carry `token` and `refresh_token`.
  Today nothing `{:?}`-formats them (verified: no `derive(Debug)`-consuming
  `?`/`#?` call site on those types in the tree), so this is latent, not live.
  `AppState` derives `Clone` only (`lib.rs:52-53`). ADR-009's addendum rates this
  Low; I agree, and I propose a hand-written redacting `Debug` rather than a
  lint.
- No AAD binds a ciphertext to its column/row, so a write-capable attacker can
  transplant an `access_token` ciphertext into `refresh_token`. Low — such an
  attacker already holds the file.

**Classification:** **PARTIAL** — primitives are correct; the *lifecycle*
(absent key, rotation, key location) is the gap.

---

### 2.4 Responses state: plaintext, unbounded, upstream-keyed — **PARTIAL**, with a **NEW** cross-tenant read

**VERIFIED.**

- Storage is opt-out, not opt-in-by-accident: `should_store` reads `request.store`
  and defaults to `true` (`lib.rs:830-833`) — and `store:false` is honoured
  end-to-end (test `store_false_does_not_create_retrievable_state`, `lib.rs:2006`).
- Ownership is stored and enforced: `response_owner` (`lib.rs:557-562`),
  `response_visible` (`lib.rs:564-571`), re-checked in `load_response_record`
  (`lib.rs:623-625`) **and** in `delete_response` (`lib.rs:793-797`). Covered by
  `stored_responses_are_scoped_to_the_creating_key` (`lib.rs:2115`).
- Id charset is constrained to `[A-Za-z0-9-_.]`, ≤200 chars
  (`valid_response_id`, `lib.rs:549-555`), so the key can't be used to walk out
  of the namespace or to inject a row separator.
- Reads are checked against the key's model allow-list too
  (`lib.rs:626-632`), and replay refuses a cross-model `previous_response_id`
  (`lib.rs:736-740`).
- Read failures are `500` and unparseable records are rejected, not coerced
  (`lib.rs:620-635`, `response_store_error` `:768`).
- Delete actually deletes (`repos.rs:768-774`, test at `lib.rs:1940`).

**PARTIAL — the documented problems.**

- **Default-on and unbounded.** No TTL, no row cap, no byte cap, no pruning
  anywhere in the workspace. `rg 'DELETE FROM|VACUUM|prune|retention|ttl'`
  returns exactly one hit, and it is the user-driven delete
  (`repos.rs:770`). Every `store:true` non-stream response writes the *complete*
  request envelope plus the complete response object
  (`persist_response`, `lib.rs:573-598`) into `key_value` in **plaintext**.
  Growth is unbounded and silent.
- `no_log` does not suppress it. The `no_log` guard lives only in
  `record_completion_usage` (`lib.rs:308-311`); `persist_response` is called
  unconditionally at `lib.rs:880`. A key explicitly marked "do not log" still has
  its full transcript persisted. ADR-009's addendum notes this; it is unfixed.
- No `PRAGMA secure_delete` (`db.rs:33-43` sets busy_timeout/journal/synchronous/
  cache_size/temp_store/mmap_size and nothing else), so a `DELETE` leaves the
  transcript bytes in the file's free pages. The `DELETE /v1/responses/{id}`
  surface therefore promises erasure it does not deliver.
- The stored envelope is exactly the "data retention" artefact an operator would
  be asked to disclose, and it is the one store with **no encryption** even when
  `STORAGE_ENCRYPTION_KEY` is configured.

**NEW — cross-tenant read via storage-id collision (High, not in any prior audit).**

The storage key is the **upstream-controlled** `response.id`, and it is a
**primary key with last-writer-wins** semantics:

```
lib.rs:594   kv_set(&guard, "responses", id, &record)   // id = response["id"]
repos.rs:758 INSERT ... ON CONFLICT(namespace,key) DO UPDATE SET value = excluded.value
```

`chat_to_responses_object` takes the id from the upstream payload verbatim
(`lib.rs:1138-1142`), and `openai_payload_to_completion` falls back to the
**constant** `"chatcmpl-unknown"` when the upstream omits `id`
(`openai.rs:188-192`). Any upstream that omits `id` therefore produces the storage
key `resp_chatcmpl-unknown`, and the *second* such response overwrites the
*first* — including its `api_key_id` owner field.

Consequence: key **K1** creates response `R`; key **K2** creates a response that
collides on `R`. `kv_set` overwrites the record with `api_key_id = K2`. Now:

- `GET /v1/responses/R` as **K2** passes `response_visible` (`lib.rs:567`,
  `stored_owner == key.id`) and returns **K1's request and response envelope** —
  i.e. K1's prompt contents and model output.
- `GET /v1/responses/R` as K1 now 404s (`lib.rs:569-570` `_ => false`).

`stored_responses_are_scoped_to_the_creating_key` (`lib.rs:2115`) does not catch
this, because the test's `CaptureBackend` returns the *distinct* id
`chatcmpl-capture` once per router instance, so the two responses never collide.
The ownership model is sound; the *key derivation* defeats it. Fix in §3.4
(mint the storage id gateway-side).

Secondary, lower: `chat_to_responses_object` returns `response.raw` **verbatim**
when the upstream sent an object (`lib.rs:1134-1136`), so a `raw` response with
`"id": "…"` is stored and echoed wholesale, including any extra upstream fields
the gateway never validated.

**Classification:** ownership **VERIFIED**; storage contract **PARTIAL**;
collision-driven cross-tenant read **NEW**.

---

### 2.5 SSRF and egress control — **VERIFIED absent (request-driven)**, **NEW (egress/redirect)**

**VERIFIED — there is no request-driven SSRF path.** Every outbound URL is
derived from one of: the compile-time registry
(`crates/heterion-router-providers/registry.json`, loaded at `main.rs:151`),
operator env vars (`OPENAI_COMPAT_BASE_URL` `main.rs:138`,
`GROK_CLI_BASE_URL` `main.rs:101`, `HETERION_LOCAL_BASE_URL` `main.rs:165`,
`GROK_OAUTH_TOKEN_URL` `grok_cli.rs:197`), or `provider_connections.provider`
joined against the registry (`main.rs:161-172`). The admin API **cannot** create
or edit a connection's URL: `UpdateConnectionBody` exposes only `name`,
`is_active`, `priority` (`admin.rs:589-593`). There is no proxy feature, no
user-supplied callback URL, and no URL echo. `docs/research/PROJECT-AUDIT-2026-09.md:115`
is correct on this point and I confirm it independently.

**NEW — egress is unconstrained and redirects are un-pinned.**

- `HttpClient::new` sets only a total timeout; no scheme allowlist, no redirect
  policy, no per-host ACL (`crates/heterion-router-http/src/client.rs:34-40`).
  `reqwest` follows redirects by default, so an upstream that answers `302` can
  steer the gateway to an arbitrary address. Whether the `Authorization` header
  survives a cross-host hop is a `reqwest` implementation detail I am **not**
  willing to assert without a test — so this is a *verification task* (§3.6),
  not a claim.
- `OPENAI_COMPAT_BASE_URL` / `HETERION_LOCAL_BASE_URL` are accepted verbatim
  (`main.rs:138-141`). Setting one to a link-local or RFC1918 address turns the
  gateway into a POST-forwarder that carries the user's prompt plus a provider
  bearer. Operator-controlled ⇒ A6 ⇒ Low, but the *missing control* is real and
  costs one `url::Url` parse.
- Response bodies are truncated to 2000 chars on error (`client.rs:60-67`,
  `:96-101`) — a correct bound, and the reason the §2.7 leak is 2000 chars and
  not unbounded.

**Classification:** request-driven SSRF **VERIFIED absent**; egress allowlist,
redirect policy, and cross-host header behaviour **NEW**.

---

### 2.6 CORS — **VERIFIED (adequate), with two notes**

**VERIFIED.** `is_allowed_origin` (`cors.rs:19-40`) accepts exactly the three
Tauri schemes plus loopback hosts on **any port**, with the host parsed
deliberately so that `localhost.evil.com` and `127.0.0.1.evil.com` are rejected
(`cors.rs:30-38`; negative test `cors.rs:104-118` covers `null`, `ftp://`,
`http://[::2]`, `https://tauri.localhost.evil.com`). `Access-Control-Allow-Credentials`
is **not** set, so cookies are not a vector. `Vary: Origin` is set
(`cors.rs:46`). The layer is applied to every route
(`lib.rs:169`), including the unauthenticated static ones.

**Notes (not defects).**

1. "Loopback on any port" means any *other* local service the attacker can get a
   page onto — including a dev server on `http://localhost:5173` — gets CORS
   access to the gateway. The bearer key is the real boundary here
   (`cors.rs:5-7` states this correctly), and `authorization` is an
   `Access-Control-Allow-Headers` value (`cors.rs:52-54`) so preflights succeed.
   Under A-2 the exposure is nil; under A5 (code execution in the webview) the
   CORS rule is not the control that saves you.
2. The middleware is a `.layer()`, so it runs before routing; an `OPTIONS` to any
   path returns `204` even for paths that do not exist (`cors.rs:70-77`). A
   trivially cheap unauthenticated port/path oracle. Noted, not worth a wave.

**Classification: VERIFIED.**

---

### 2.7 Error redaction — **PARTIAL (High, confirmed)**

**VERIFIED — the non-stream path is correct.** `error_response`
(`lib.rs:538-545`) replaces every `GatewayError::Upstream(_)` body with the
constant `"upstream request failed"`, while `error_status`
(`lib.rs:521-536`) still recovers the real status from the message prefix so
clients keep 400/401/403/404/409/422/429/502 semantics
(test `upstream_statuses_are_preserved_for_clients`, `lib.rs:1359`).

**PARTIAL — the streaming paths are not.**

- `error_event` (`lib.rs:493-495`) emits `{"error":{"message": error.to_string()}}`.
  For `GatewayError::Upstream(msg)` the `Display` impl
  (`crates/heterion-router-core/src/error.rs:21`) renders
  `"upstream error: {msg}"`, and `msg` for a client error is
  `"upstream status {code}: {body}"` with `body` = **raw upstream text, up to
  2000 chars** (`client.rs:14-20`). So an A3-chosen error body is relayed
  verbatim to the client, mid-stream.
- `responses_stream` does the same at `lib.rs:1211`:
  `json!({"error": {"message": error.to_string()}})`.
- Neither path is covered by a test. The existing SSE tests
  (`streaming_emits_content_then_done` `lib.rs:1449`,
  `responses_streaming_emits_deltas_then_completed` `lib.rs:1916`) only assert
  the happy path.
- **Logs carry the same raw text.** `tracing::warn!("completion failed model={model}: {error}")`
  (`lib.rs:282`, `lib.rs:899`) writes it to `gateway.log`, which — per §2.9 — is
  `0644` until the next boot, and is served verbatim by `GET /api/logs/app`
  (`admin.rs:511-532`, correctly `manage`-gated). So the raw text is: (a) in a
  world-readable file, (b) in the DB-adjacent log store, (c) in a `manage`-only
  API. (b)/(c) are acceptable by design; (a) is not.
- **Admin handlers echo `DbError` verbatim** to the client
  (`admin.rs:206`, `:303`, `:365`, `:420`, `:460`, `:706`, `:764`).
  `DbError::Sqlite(#[from] rusqlite::Error)` (`db/error.rs:9-10`) renders
  `"sqlite error: {0}"`, and some `rusqlite` variants carry column names, value
  types, or the offending SQL fragment (`ToSqlConversionFailure` includes the
  value's Rust type and Debug). Low-severity schema/typing disclosure, but it is
  a client-visible `Debug` of an internal error and should go through the same
  redactor.
- A second, correctness-adjacent leak: `ProviderOpenAiBackend::complete`
  collapses a provider error *body* into `"{provider}: all credentials failed"`
  (`openai.rs:587-589`) and the body-error branch discards the status prefix
  (`openai.rs:582`). That message then has no parseable status, so
  `is_backend_outage` (`routing.rs:44-55`) and `is_transient`
  (`routing.rs:138-149`) both classify a 4xx returned inside a 200 body as a
  backend outage and trip the breaker. The 4xx-does-not-trip-breaker contract
  that `AGENTS.md` calls out is preserved for transport-level 4xx and **broken**
  for body-level errors.

**Classification: PARTIAL.**

---

### 2.8 Filesystem permissions and the static surface — **PARTIAL**, with two **NEW** items

**VERIFIED.**

- `Db::open` creates the parent `0o700` and the database file `0o600`
  (`db/src/db.rs:53-63`), asserted by a real-filesystem test
  (`db.rs:171-190`).
- `main.rs:390` walks the data dir and forces `0o600` on every regular file
  (`main.rs:359-368`) — that covers a pre-existing `.env` holding
  `STORAGE_ENCRYPTION_KEY` (`crypto:166`).
- `.gitignore` excludes `.env`, `.env.*`, `*.pem`, `*.key`, `*.log`
  (`.gitignore:17-28`); `desktop/src-tauri/binaries/` is ignored and the staged
  gateway binaries are **not** tracked (`git ls-files` returns nothing).
- Static file serving is traversal-safe: `safe_join` (`ui.rs:77-92`) rejects
  `..`, `\`, and empty segments; test `rejects_traversal` (`ui.rs:115`) covers
  `../secret`, `assets/../../secret`, `a/../../b`, `""`, `../`.
- `restart_service` is `manage`-gated (`admin.rs:379`) and only proceeds when
  launchd actually supervises the label (`admin.rs:387-392`, `restart.rs:50-58`),
  answering `409` otherwise.

**PARTIAL — the ordering defect (confirmed in code, mode not runtime-probed).**

`main.rs` performs the permission pass at **line 390** and then creates every
subsequently-created file *after* it:

| Order | Line | Creates | Mode |
|---|---|---|---|
| 1 | 389 | data dir | `0o700` (from `create_dir_all` under umask, then §2.3 `db.rs` only touches it on `Db::open`) |
| 2 | **390** | `restrict_data_files` → `0o600` on **pre-existing** files only | — |
| 3 | 393 | `gateway.log.YYYY-MM-DD` via `tracing_appender::rolling::daily` | `0666 & ~umask` ⇒ **`0644`** |
| 4 | 409 | `storage.sqlite` (+ `-wal`, `-shm`) | `0600` (main file); sidecars per SQLite |
| 5 | 423→`main.rs:154` | `gemini-thought-signatures.jsonl` | `0666 & ~umask` ⇒ **`0644`** |

`ThoughtSignatures::append` opens with
`OpenOptions::new().create(true).append(true)` and **no `.mode()`**
(`gemini.rs:178-181`); `rewrite()` uses bare `std::fs::write`
(`gemini.rs:144`). Neither sets permissions. ADR-009:57-58 and
`PROJECT-AUDIT-2026-09.md:132-135` already flag the log/signature case; I confirm
it from the code ordering and add that the check for `-wal`/`-shm` modes was
**not** runtime-probed here, so it ships as a test task, not a claim.

**NEW-1 — `/` and `/{*path}` are unauthenticated.** `ui::serve` is routed
without an extractor (`lib.rs:160-161`), so the full dashboard bundle is served
to anyone who can reach the port. The bundle itself carries no secrets (the key
lives in `localStorage`, §2.10), so this is information disclosure + an
attack-surface advertisement, not a credential leak. But on the documented
`0.0.0.0` bind it publishes the operator's full UI to the LAN, and
`/api/logs/app` (`admin.rs:511`) — although `manage`-gated — becomes a
discoverable endpoint. `ROADMAP:113-114` treats this as a deliberate boundary
("public static surface"); I agree with the principle but the docs should state
that it is *unauthenticated by design*, not merely "a boundary".

**NEW-2 — `HETERION_ROUTER_UI_DIR` has no root containment.** `ui_dir()`
(`ui.rs:22-32`) takes the env value verbatim (or the compiled-in
`../../desktop/ui/dist`). `safe_join` guarantees containment *within* whatever
root is chosen, but nothing validates the root. Pointing it at `~` or `/` makes
the gateway a read-only file server for anything under it, with
`Content-Type` inferred from the extension (`ui.rs:94-109`). A6-gated ⇒ Low, but
the fix is a `canonicalize()` + `index.html`-present + home/root rejection at
startup, and a per-request containment re-check (cheap) to defeat a symlink
planted inside the bundle.

**NEW-3 (Low) — unquoted interpolation in the restart script.**
`kickstart_script` builds `sh -c "sleep 2; exec launchctl kickstart -k gui/{uid}/{label}"`
(`restart.rs:45-47`) and `request_restart` runs it through `sh -c`
(`restart.rs:62-70`). `label` comes from `HETERION_ROUTER_SERVICE_LABEL`
(`restart.rs:20-26`) — operator-controlled (A6) — and is *gated* by
`is_supervised` succeeding first, so this is hygiene, not a hole. Still: build
the argv as `["launchctl","kickstart","-k", target]` behind a `sleep` instead.

**Classification: PARTIAL.**

---

### 2.9 Retention and log/telemetry growth — **NEW** (as a coherent story)

This is the domain the earlier audits treated as a footnote under "Responses
store unbounded". It is four separate unbounded stores with **no** `DELETE`
anywhere except the user-driven one:

| Store | Written by | Bound today | Contains |
|---|---|---|---|
| `key_value` ns `responses` | `persist_response` (`lib.rs:573-598`) | **none** | full request + response envelopes, plaintext |
| `usage_history` | `record_usage` (`usage/records.rs:75-104`) | **none** | per-request model/provider/key/token counts |
| `call_logs` | `record_call_log` (`usage/records.rs:44-72`) | **none** | path, model, key, status, error code |
| `gateway.log*` | `tracing_appender::rolling::daily` (`main.rs:393`) | **none** — rotates daily, never deletes | model names, error text (§2.7) |
| `gemini-thought-signatures.jsonl` | `append` (`gemini.rs:167-181`) | `SIGNATURE_CAPACITY = 4096` (`gemini.rs:71`), rewritten at load | tool-call ids ↔ signatures |
| `provider_connections` / `api_keys` | — | n/a | **credentials / secrets** — no expiry path for keys (§2.1) |

Two positives worth preserving: the Gemini signature store **is** bounded
(`gemini.rs:71`, `:116-120`, `:157-161`) and is trimmed on load, so it does not
grow across restarts; and the Rust `call_logs` writer never populates
`has_request_body` / `request_summary` / `artifact_relpath`
(`usage/records.rs:46-53`), so **no request bodies are retained in the call
log** — a privacy property of the Rust port that the JS lineage's schema hints
at and that must not be regressed.

**Classification: NEW** (as a retention *policy*); the signature cap is
**VERIFIED**.

---

### 2.10 Tauri sidecar trust — **PARTIAL**, with a **NEW** finding on `csp: null` + `shell:allow-spawn`

**VERIFIED.**

- The single-instance plugin is registered **first**, explicitly to prevent a
  second launch spawning a duplicate sidecar onto an occupied port
  (`desktop/src-tauri/src/lib.rs:38-45`) — correct, and the comment records the
  `AddrInUse` incident that motivated it.
- The gateway is parent-watched: the sidecar exits when the app dies
  (`lib.rs:63`, `main.rs:327-353`), so no orphan holds `20129`. `parent_gone`
  uses `kill(pid,0)` + `ESRCH` and treats `EPERM` as alive (`main.rs:350-353`);
  `#[cfg(unix)]` semantics are correct, and the `HETERION_ROUTER_PARENT_PID`
  marker is unset under launchd (`main.rs:325-333`).
- The sidecar is spawned **from Rust**, and the URL is injected as an
  `initialization_script` that runs before bundle code (`lib.rs:84-92`).
- The loopback fallback ports do not collide by design: service `20128`, sidecar
  `20129` (`AGENTS.md`, `lib.rs:18-32`).

**PARTIAL — the handshake that isn't.**

- The URL is injected and the window is built **before any readiness check**
  (`lib.rs:55-93`); a sidecar bind failure is logged and never retried.
- There is **no ownership handshake**. If something else already holds `20129`,
  the spawned child exits, and the UI happily talks to the foreign listener with
  the operator's `Authorization` header. `api.js:30-48` then falls back through
  `VITE_GATEWAY_URL` → `localStorage` override → injected → Tauri default
  **`http://127.0.0.1:20128`** (`api.js:44-46`) — which is the *launchd service*,
  not the sidecar. So the documented "if the injected URL is missing, the desktop
  can talk to the launchd service instead of its own sidecar" is exactly the
  confused-deputy path, and it is a *silent* credential-forwarding one.
- `csp: null` in `desktop/src-tauri/tauri.conf.json:17-21` disables the webview
  CSP entirely, while `desktop/src-tauri/capabilities/default.json` grants
  `core:default` **and `shell:allow-spawn`** to the `main` window. A `null` CSP
  plus a shell-spawn capability is the classic Tauri RCE-if-XSS pairing: any
  script executing in the trusted origin inherits process-spawn rights. Under
  the current design the webview only ever loads the local bundle
  (`WebviewUrl::App("index.html")`, `lib.rs:88`) and Svelte escapes by default, so
  exploitability needs A5 (XSS or a compromised bundle) — but the *configuration*
  removes the barrier that would make A5 non-fatal, and the spawn capability is
  **not needed**: the sidecar is spawned from Rust with `.shell().sidecar(...)`,
  not from JavaScript. **NEW, and the cheapest high-value fix in this document.**
- The operator key is persisted in `localStorage` under
  `heterion_router_key` (`desktop/ui/src/api.js:1-18`, `App.svelte:22,48-55`) —
  plaintext at rest in the WebView profile, readable by anything with that
  profile. And `apiBase()` honours a `heterion_router_gateway_url` override from
  the same `localStorage` (`api.js:33-38`), so a single script-execution
  primitive in the webview can both read the key and redirect it.
- The bundle is adhoc-signed, not notarized (no Team ID), and
  `externalBin` ships a locally built binary — fine for a developer machine, and
  it is honestly documented as such (`PROJECT-AUDIT-2026-09.md:139-141`).

**Classification: PARTIAL.**

---

### 2.11 Quotas, rate limits and body limits — **NEW**

**VERIFIED (barely).**

- The upstream HTTP client sets a **600 s total timeout**
  (`openai.rs:39`, `responses.rs:44`, `grok_cli.rs:195`), which does bound the
  lifetime — and therefore the cost — of any single stream.
- The admin list endpoints clamp every unbounded query parameter:
  `days` → 1..90, `limit` → 1..500 (`admin.rs:41-50`), `usage` re-clamps to 50
  (`admin.rs:416`), `logs_app` to 1..1000 (`admin.rs:541-545`). `Json` bodies
  are typed and validated (`admin.rs:230-233`, `:560-578`, `:589-593`).

**NEW — everything else is missing.**

- **No rate limiting of any kind.** No per-key token bucket, no global limit, no
  failed-auth throttle. Unauthenticated traffic still costs a `COUNT(*)` and a
  `find_usable_api_key` per request (`auth.rs:106-119`).
- **No concurrency limit.** `axum::serve` (`main.rs:441`) has no
  `ConcurrencyLimitLayer`; N concurrent streams each hold a task, a 64-slot
  channel, and an upstream connection.
- **No explicit body limit.** The only cap is axum's implicit
  `DefaultBodyLimit` (2 MiB) on the `Json` extractor. That is a library default,
  not a stated contract, and it does not apply to the SSE **response** direction
  at all.
- **The schema already contains the enforcement data and nothing reads it.**
  `migrate.rs:73-77` adds `allowed_quotas`, `usage_limit_enabled`,
  `max_sessions`; `schema_base.sql` defines `domain_budgets`
  (`daily_limit_usd`, `weekly_limit_usd`, `monthly_limit_usd`,
  `warning_threshold`, `budget_reset_at`), `domain_budget_reset_logs`,
  `domain_cost_history`, `domain_lockout_state`, `quota_snapshots`. A workspace
  grep for `quota|budget|max_sessions|usage_limit|rate.?limit` across
  `crates/**/*.rs` returns **only** the three migration/insert sites — zero
  enforcement. `ROADMAP:61` and `PROJECT-AUDIT-2026-09.md:127` both rate this
  Medium; I keep Medium, and note that with `no_log` keys unmetered (§2.4) a
  single key can spend an upstream balance invisibly.
- `no_log` is **not** settable through the API (`admin.rs:276-284` omits it, and
  there is no `PATCH /api/keys/{id}`), so today it is only reachable by direct
  SQL. That is a mitigating accident, not a design.

**Classification: NEW.**

---

### 2.12 Threat-model assumptions currently written down — **PARTIAL**

**VERIFIED.** `ADR-009` §"Решение" item 3 states the boundary as
"fail closed" and items 4/7 state hash-only-for-new-issuance and
owner-scoped Responses. `ROADMAP:104-114` lists five binding principles
(fail closed; capability by effective model format; opaque reasoning is data;
state origin must be explicit; public static surface is a boundary). That is a
real, unusually honest set of constraints.

**PARTIAL.** The *written* model and the *implemented* model diverge in four
places, and ADR-009 itself concedes three of them in its 2026-09-29 addendum
(`ADR-009:52-71`):

1. `0700/0600` is applied "at the permission pass", which is *before* the log
   and signature files exist (`ADR-009:57-58` — confirmed in §2.8).
2. Non-stream errors are sanitized but SSE frames are not
   (`ADR-009:59-60` — confirmed in §2.7).
3. Responses state is owner-scoped but "default-on storage is unbounded and the
   storage id is upstream-controlled" (`ADR-009:61-62` — confirmed in §2.4; the
   addendum names the *id* as upstream-controlled but does **not** draw the
   cross-tenant-read conclusion that follows).
4. NULL legacy policy columns are "intentionally fail-open until migrated"
   (`ADR-009:64-65` — confirmed in §2.1, and "intentionally" is doing a lot of
   work in a sentence that describes an unbounded-in-time deferral).

There is **no written threat model anywhere in the repository** — no
`docs/THREAT-MODEL.md`, no asset/adversary list, no explicit statement of the
"single-operator, single-host, LAN-exposed" assumption that §1.4 of this
document makes on the project's behalf. `ADR-009` is a decision record with a
boundary section, not a model. **NEW.**

**Classification: PARTIAL (documents) / NEW (the model itself).**

---

## 3. Mitigation plan, in dependency order

Waves are ordered so that each one's *verification* does not depend on an
unmerged predecessor, and so that the one destructive step (W2 phase 2) is last
among the data-touching steps. Every wave is independently revertible.

### Wave 0 — Baseline capture and policy decision (no code)

**Why first:** W2-phase-2 and W4-retention are the only irreversible steps, and
both need a measured "before".

| Item | Action |
|---|---|
| 0.1 | Decide and record the deployment class (A-1: single-operator local vs LAN-shared). Everything in §2 is rated against it. |
| 0.2 | Read-only inventory of the data dir: `ls -la` (capture modes), `du -sh`, and `SELECT COUNT(*)` per table + `SELECT COUNT(*), SUM(LENGTH(value)) FROM key_value WHERE namespace='responses'`. **Read-only; no writes.** |
| 0.3 | File-level backup of `storage.sqlite` (+ `-wal`, `-shm`) and `.env` to an operator-chosen path, mode `0600`. This is the W2/W4 rollback asset. |
| 0.4 | Enumerate keys whose policy columns are NULL: `SELECT id,name FROM api_keys WHERE scopes IS NULL OR allowed_models IS NULL OR allowed_combos IS NULL`. This is the W1 blast radius. |
| 0.5 | Open `docs/SECURITY.md` (new) with §1 of this document as its skeleton. |

**Rollout:** none. **Rollback:** n/a.

---

### Wave 1 — Fail-closed key policy + the narrowing API

**Depends on:** 0.4. **Blocks:** W2 (an operator must be able to re-scope a key
before its plaintext is removed). **Independent of** every other wave.

**Design.**

1. `crates/heterion-router-db/src/repos.rs::find_usable_api_key` — stop
   `COALESCE`-ing policy columns. Select them as nullable, widen
   `UsableKey` (`repos.rs:339-347`) to `scopes: Option<String>`,
   `allowed_models: Option<String>`, `allowed_combos: Option<String>`, and let
   the caller decide. Keep `expires_at`/`is_banned`/`is_active` behaviour
   byte-identical.
2. `crates/heterion-router-gateway/src/auth.rs` — decision function
   `fn effective_scopes(key: &AuthKey) -> Vec<String>`: `None` ⇒ `[]`
   (deny). `fn allows_model` (`:44-58`): `None` ⇒ `false`; **explicit** `"[]"`
   keeps its current allow-all meaning, so JS-written rows
   (`schema_base.sql:104` default) do not change behaviour. This is the
   surgical distinction that makes the wave non-breaking.
3. `crates/heterion-router-gateway/src/admin.rs` — new
   `PATCH /api/keys/{id}` with `UpdateKeyBody { name?, scopes?, allowed_models?,
   allowed_combos?, expires_at?, no_log? }`, `manage`-gated, mirroring
   `update_combo`'s validation style (`admin.rs:606`) and its
   400/404/409 discipline. Register in `lib.rs:145-171`.
4. `crates/heterion-router-db/src/repos.rs::update_api_key_policy` +
   `count_active_api_keys` corrected to
   `revoked_at IS NULL AND COALESCE(is_active,1)=1 AND COALESCE(is_banned,0)=0`.

**Config/env (new).** `HETERION_ROUTER_LEGACY_KEY_POLICY=1` restores the
`COALESCE` behaviour for one release cycle. Default: off.

**Migration.** None. Deliberately: a data migration that *widens* or *narrows*
authority must be a code decision, not a schema side effect.

**Tests.**
- `db`: `null_scopes_row_is_returned_as_null_not_manage`.
- `auth`: `null_policy_key_is_forbidden_on_admin`,
  `null_policy_key_is_forbidden_on_models`,
  `explicit_empty_allowed_models_still_allows` (compat regression),
  `js_written_row_keeps_its_access`.
- `admin`: `patch_key_narrows_scopes_then_forbidden`,
  `patch_key_sets_expiry_then_key_stops_authenticating`,
  `overview_reports_only_usable_keys`.
- Gate: `cargo test --workspace`, `scripts/ci/check.sh`.

**Rollout.** Ship with `HETERION_ROUTER_LEGACY_KEY_POLICY=1` in the launchd
plist; read the log line `N keys have NULL policy columns; deny-by-default is
active` at boot; flip the flag off after the operator confirms `PATCH`-ed every
row from 0.4.

**Rollback.** Set the env var back to `1` and restart. No data touched.

---

### Wave 2 — Key lifecycle: hash backfill, then plaintext removal, then expiry

**Depends on:** Wave 1 (need `PATCH` to re-scope before a key loses its
plaintext). **Destructive in phase 2.** Rollback asset: 0.3.

**Phase 2a (additive, non-destructive).** Append to `MIGRATIONS` in
`crates/heterion-router-db/src/migrate.rs:32-36` — the list is documented
"Append only" (`:30-31`) and every body must be idempotent (`:26`):

```sql
-- 002_api_key_hash_backfill
UPDATE api_keys
   SET key_hash = lower(hex(sha3(key, 256)))   -- see note
 WHERE (key_hash IS NULL OR key_hash = '')
   AND key NOT LIKE 'enc:v1:%';
```

> **Note — the hashing primitive.** SQLite has no built-in SHA-256. The backfill
> must therefore run in Rust, not in the migration SQL: iterate
> `SELECT id, key FROM api_keys WHERE key NOT LIKE 'enc:v1:%' AND
> (key_hash IS NULL OR key_hash = '')` and call the existing
> `repos::api_key_hash` (`repos.rs:177-179`) — the exact function that
> `find_usable_api_key` looks the token up by (`repos.rs:364`). It is a
> one-time idempotent backfill, so it belongs as a post-migration Rust step
> (a new `pub fn backfill_api_key_hashes(conn) -> Result<usize>` next to
> `ensure_api_keys_columns`, `migrate.rs:89-111`) rather than as a
> `Migration` SQL body. That keeps the ledger semantics honest.

Add a boot log: `backfilled N api key hashes; M plaintext rows remain`.

**Phase 2b (destructive, opt-in).** New `heterion-router-gateway rekey-keys`
subcommand: for each row whose `key` is neither a 64-char hex hash nor
`enc:v1:`-prefixed, verify `key_hash == sha256(key)`, then
`UPDATE api_keys SET key = key_hash WHERE id = ?`. It **refuses** unless
(a) every plaintext row now has a matching `key_hash`, and (b) the operator
passed `--i-have-a-backup`. It prints a count and exits non-zero otherwise.
`repos.rs:362`'s `WHERE (key = ?1 OR key_hash = ?2)` keeps working unchanged
throughout, so a rollback is a restore from 0.3, not a code change.

**Phase 2c.** Close the plaintext writer: `repos::insert_api_key` (`repos.rs:195`)
is `pub` and writes a recoverable secret. Gate it behind a `test-support`
feature (or move it into `repos::tests`) so production code physically cannot
call it. This is the step that makes phase 2b stick.

**Phase 2d.** `POST /api/keys` accepts `expires_in_days: Option<u32>` and a
default `HETERION_ROUTER_KEY_DEFAULT_TTL_DAYS` (unset ⇒ no expiry, preserving
today's behaviour). `CreateKeyBody` at `admin.rs:230-233` and
`NewApiKey` at `repos.rs:213-222` gain the field; `insert_full_api_key`'s
hardcoded `NULL` (`repos.rs:237`) becomes a parameter.

**Config/env (new).** `HETERION_ROUTER_KEY_DEFAULT_TTL_DAYS`,
`HETERION_ROUTER_KEY_MIGRATION` (unused; the subcommand is the gate).

**Tests.** `backfill_is_idempotent`; `backfilled_row_authenticates_by_hash_only`;
`rekey_refuses_when_hashes_are_missing`; `rekey_blanked_row_still_authenticates`;
`rekey_is_a_noop_for_encrypted_and_hashed_rows`; `key_with_ttl_expires`;
`insert_api_key_is_not_reachable_from_production_code` (a `trybuild`/doc-test
guard, or simply "the symbol does not exist without the feature").

**Rollout.** 2a auto-runs (idempotent, no behaviour change) → deploy 2d+2c →
operator runs 2b from a terminal during a quiet window, then re-verifies by
calling `GET /v1/models` with each key. **Rollback:** restore `storage.sqlite`
from 0.3; the `key_hash` backfill alone is harmless and needs no rollback.

---

### Wave 3 — Encryption key lifecycle

**Depends on:** 0.3 (backup). **Independent of** W1/W2. Should land *before*
W4, because W4 reuses `FieldCrypto` for the transcript store.

1. **Fail loudly on a missing key.** `crates/heterion-router-gateway/src/main.rs`
   — after `load_secret` (`:39`, `:97`), count encrypted rows; if the secret is
   absent *and* any `access_token`/`api_key`/`refresh_token` starts with
   `enc:v1:` (`heterion_router_crypto::PREFIX`, `crypto:18`), log `ERROR` and
   **refuse to start** unless `HETERION_ROUTER_ALLOW_PLAINTEXT=1`. This closes
   the silent-degradation path in §2.3.
2. **Refuse the plaintext write-back.** `main.rs:110-120` — replace
   `.unwrap_or_else(|| token.to_string())` with a hard
   `Err("STORAGE_ENCRYPTION_KEY unavailable; refusing to persist a plaintext token")`.
   One line, and it removes the only plaintext *write* in the process.
3. **Keyring for rotation.** `crypto::load_secret` (`crypto:159`) →
   `load_secrets(data_dir) -> Vec<String>` reading
   `HETERION_ROUTER_STORAGE_KEYS` (comma-separated, newest first) with the
   existing single-key path as the one-element fallback. `FieldCrypto`
   (`crypto:32-36`) holds `Vec<[u8;32]>`; `decrypt` (`crypto:107-129`) already
   tries primary-then-legacy — extend to "try every key × every derivation",
   and always encrypt with keys[0] (`crypto:82-103`). `STATIC_SALT` and the
   `enc:v1:` prefix stay frozen (`crypto:20-23`).
4. **`heterion-router-gateway rekey-storage`**: read the data dir backup from
   0.3 as the *source*, derive each provider column under the new secret, write
   with `kv`-style idempotent updates, and report the row count. Run offline.
5. **Warn when the key is co-located.** If `load_secret` satisfied the request
   from `<data_dir>/.env` (`crypto:166`), log
   `WARN storage encryption key is stored beside the ciphertext it protects`.

**Config/env (new).** `HETERION_ROUTER_STORAGE_KEYS` (comma list),
`HETERION_ROUTER_ALLOW_PLAINTEXT=1` (override).

**Tests.** `missing_key_with_encrypted_rows_refuses_startup`;
`missing_key_with_only_plaintext_rows_still_starts`;
`plaintext_refresh_write_back_is_refused`;
`keyring_decrypts_rows_written_under_any_listed_key`;
`rotation_always_encrypts_with_the_newest_key`;
`js_vector_still_decrypts` (retain `crypto:190-206` unchanged);
`rekey_storage_is_idempotent`.

**Rollout.** Land 1+2 behind the override, deploy, watch the boot ERROR, then
3+4 as a separate release. **Rollback:** `HETERION_ROUTER_ALLOW_PLAINTEXT=1`
(1+2) or restore the data-dir backup (3+4).

---

### Wave 4 — Responses state: own the id, bound it, encrypt it, retain it

**Depends on:** Wave 3 step 3 (keyring) so envelopes can be encrypted. Highest
security value of any wave. **Staged internally so nothing is destroyed on the
first release.**

**Stage 1 — gateway-minted storage ids (fixes the cross-tenant read).**

- `lib.rs:1133-1142` `chat_to_responses_object`: always mint
  `id = format!("resp_{}", ids::gateway_id())` (new `ids.rs` helper,
  `chatcmpl-rust-…`-style but namespaced for Responses, 128 bits from
  `getrandom`, matching `apikeys.rs:25-32`). Keep the upstream id inside the
  record as `"upstream_id"` for debugging.
- `persist_response` (`lib.rs:573-598`) then keys on a value the gateway
  generated; collisions become cryptographically infeasible instead of
  *structurally guaranteed* by the `"chatcmpl-unknown"` fallback
  (`openai.rs:188-192`).
- `lib.rs:1134-1136`: stop returning `response.raw` verbatim — copy only the
  fields the gateway models (`id`, `output`, `usage`, `model`), so no
  unvalidated upstream field is echoed or stored.

**Stage 2 — explicit store policy + `no_log` honoured.**

- `should_store` (`lib.rs:830-833`) becomes
  `request.store ?? env HETERION_ROUTER_STORE_RESPONSES (default true)`, logged
  once at boot so the default is never a surprise.
- `persist_response` takes `&Authenticated` and returns early when
  `auth.key.no_log` — closing the `no_log`/transcript gap (§2.4).

**Stage 3 — encryption at rest.** Encrypt the record JSON with the same
`FieldCrypto` (`crypto:82`) before `kv_set`; decrypt in
`load_response_record` (`lib.rs:600-637`). Reuse the frozen `enc:v1:` format so
one `PRAGMA`-free reader path serves both stores. A record that fails to decrypt
returns `500 response_store_error` (`lib.rs:768`) — never plaintext fallback.

**Stage 4 — retention (separate release, after the operator has seen counts).**

- `crates/heterion-router-db/src/repos.rs`: `prune_responses(conn, older_than,
  max_rows, max_bytes) -> Result<usize>` and
  `responses_store_stats(conn) -> (rows, bytes)`. `kv_delete` (`repos.rs:768`)
  already exists; the new query is a `DELETE ... WHERE namespace='responses'`
  ordered by the `stored_at` the record already carries (`lib.rs:588`).
- Call it at boot and on an hourly `tokio::time::interval` in `main.rs` (the
  process already spawns a thread in `watch_parent`, `main.rs:334`, so the
  pattern exists).
- Add `PRAGMA secure_delete = ON` in `Db::apply_pragmas` (`db.rs:33-43`) so
  `DELETE` actually removes transcript bytes from free pages. Document the
  write-amplification cost in the commit message.

**Config/env (new).** `HETERION_ROUTER_STORE_RESPONSES` (bool, default `true`),
`HETERION_ROUTER_RESPONSE_RETENTION_DAYS` (default 7; `0` ⇒ stop writing,
keep existing rows), `HETERION_ROUTER_RESPONSE_MAX_ROWS` (default 1000),
`HETERION_ROUTER_RESPONSE_MAX_BYTES` (default 64 MiB).

**Tests.** `storage_id_is_gateway_minted_and_not_upstream` (asserts the
`chatcmpl-unknown` collision case explicitly);
`colliding_upstream_ids_do_not_overwrite_each_other`;
`cross_owner_read_of_a_collided_id_is_404`;
`record_is_encrypted_at_rest` (assert `value LIKE 'enc:v1:%'`);
`encrypted_record_roundtrips`; `undecryptable_record_is_500_not_plaintext`;
`no_log_key_persists_no_transcript`; `store_false_persists_nothing`;
`retention_zero_disables_new_writes`; `prune_removes_oldest_first`;
`prune_respects_max_bytes`; `upstream_extra_fields_are_not_stored`.

**Rollout.** Stage 1 + 2 land together (no data loss; old rows keep working
because `load_response_record` tolerates a missing `upstream_id`). Stage 3
lands with `HETERION_ROUTER_STORE_RESPONSES` still `true` — both plaintext and
`enc:v1:` records are readable during the transition (`decrypt` passes
non-prefixed values through, `crypto:107-110`). Stage 4 is its own release, after
the operator reviews `responses_store_stats`. **Rollback:** Stage 1–3 = revert
the commit; old plaintext rows remain readable. Stage 4 = **irreversible** —
that is why it is a separate release, and why 0.3 is mandatory.

---

### Wave 5 — Error redaction (smallest, highest ratio)

**Depends on:** nothing. Can land any day; the audit's P0 list already names it.

1. New `crates/heterion-router-gateway/src/redact.rs`:
   `pub fn upstream_message(error: &GatewayError) -> String` returning
   `format!("upstream status {code}")` when `upstream_status` parses
   (`lib.rs:511-519`) and `"upstream request failed"` otherwise; and
   `pub fn db_message(error: &heterion_router_db::DbError) -> String` mapping to
   the variant name only (`db/error.rs:7-23`).
2. `lib.rs:493-495` `error_event` and `lib.rs:1211` use `upstream_message`.
   `lib.rs:538-545` `error_response` delegates to the same helper (behaviour
   for `Upstream` is unchanged — the constant it already returns).
3. `lib.rs:282` and `lib.rs:899` `tracing::warn!` log the redacted message plus
   the status; the raw body only when
   `HETERION_ROUTER_LOG_UPSTREAM_BODIES=1` **and** `RUST_LOG` targets
   `heterion_router_gateway=trace`.
4. `admin.rs` `:206, :303, :365, :420, :460, :706, :764` use `db_message`.
5. The SSE payload keeps `{"error":{"message":…}}` shape (SDK compatibility),
   so `error_status` (`:521`) continues to drive the HTTP status even though
   the body is gone — clients keep 429-vs-5xx distinction, lose only the prose.

**Bonus fix in the same wave.** `openai.rs:582` and `:587-589` should preserve
the `upstream status {code}:` prefix when collapsing a body-level error, so
`routing.rs:44-55` / `:138-149` stop classifying body-level 4xx as an outage.

**Tests.** `sse_error_frame_contains_no_upstream_body` (wire-level: drive a
`wiremock` upstream that 429s with a canary string, assert the canary is
absent from the full SSE byte stream); `responses_stream_error_frame_is_redacted`;
`non_stream_error_is_still_sanitized`; `error_status_still_maps_429`;
`db_error_response_has_no_sql_or_type_names`;
`raw_upstream_body_is_logged_only_with_the_explicit_switch`.

**Rollout.** Code-only, no data impact. **Rollback:** revert the commit.

---

### Wave 6 — Filesystem permissions and log retention

**Depends on:** nothing.

1. **Create sensitive files with the right mode, not a post-hoc pass.**
   - `crates/heterion-router-gateway/src/gemini.rs:178-181` — add
     `#[cfg(unix)] .mode(0o600)` to the `OpenOptions`, and
     `set_permissions(0o600)` after `rewrite()` (`gemini.rs:144`).
   - `crates/heterion-router-db/src/db.rs:53-63` — after the connection is open,
     also `restrict_permissions` the `-wal` and `-shm` siblings if they exist;
     expose `Db::harden(&self) -> Result<()>` and call it at the end of
     `migrate()` (`db.rs:136-139`) so a first-boot write is covered.
2. **Make the boot pass recursive and last.** `main.rs:390` —
   `restrict_data_files` (`main.rs:359-368`) should walk subdirectories
   (`WalkDir`-style recursion, no new dependency: a small explicit stack) and
   should additionally run *after* `select_backend` (`main.rs:423`) so a
   signature file created at boot is hardened. Belt and braces with (1).
3. **Log retention.** `main.rs:392-393` — after configuring the appender, prune
   `gateway.log.*` older than `HETERION_ROUTER_LOG_RETENTION_DAYS` (default 14)
   by filename date suffix, mirroring `application_log_path`'s prefix logic
   (`admin.rs:496-501`) so the two agree on the naming scheme.
4. **Confirm or refute the WAL/SHM claim** with the test below rather than
   assuming it (§2.8).

**Config/env (new).** `HETERION_ROUTER_LOG_RETENTION_DAYS`.

**Tests.** `#[cfg(unix)] data_dir_files_are_private_after_boot` — create a
temp data dir with a pre-existing `.env`, boot the real `main` ordering
(`create_dir_all` → `restrict_data_files` → appender → `Db::open` → migrate),
then assert `0o600` on `.env`, the log, the signature file, `storage.sqlite`,
`-wal` and `-shm`; `log_prune_keeps_n_days`; `prune_is_idempotent`.

**Rollout.** Code-only. **Rollback:** revert; the permission pass is idempotent
and re-running it is always safe.

---

### Wave 7 — Egress, redirects, UI root, transport posture

**Depends on:** nothing, but land after Wave 5 so the log-noise story is settled.

1. `crates/heterion-router-http/src/client.rs:34-40` — add an explicit
   `redirect(reqwest::redirect::Policy::limited(2))` and a connect timeout, and
   keep the 600 s total (`openai.rs:39`). Add a `#[cfg(test)]` wiremock that
   answers `302` to a second mock and assert whether the `Authorization` header
   arrives — **this is the verification task for §2.5**, and the redirect count
   should be chosen from its result.
2. `main.rs:101, :138, :165` and `grok_cli.rs:197` — parse env-provided base
   URLs with `url::Url` (already in the dep graph transitively) and reject
   non-`http(s)` schemes at startup. When `HETERION_ROUTER_ALLOWED_EGRESS_HOSTS`
   is set, enforce it on **every** outbound request in `HttpClient`
   (`client.rs:43-103` is the single choke point — good). Loopback and
   link-local destinations `WARN` unless explicitly listed, so
   `HETERION_LOCAL_BASE_URL` stays usable without being silent.
3. `crates/heterion-router-gateway/src/ui.rs:22-32` — `ui_dir()` canonicalizes,
   requires `index.html`, and refuses `$HOME`, `/`, and any path that is not a
   directory; log the resolved absolute path. Add a per-request
   `starts_with(canonical_root)` re-check in `serve` (`ui.rs:35`) so a symlink
   planted inside the bundle cannot escape.
4. **Transport.** No code change is possible in-process without a TLS stack.
   Instead: (a) log a `WARN` at boot when `config.host` is non-loopback
   (`main.rs:436`), naming the cleartext-bearer consequence; (b) add
   `docs/SECURITY.md` §"Deployment" with a Caddy/nginx TLS-termination example
   matching the homelab-infra convention; (c) make
   `validate_auth_policy` (`main.rs:370-384`) print, on refusal, the exact
   remediation rather than only the error.

**Config/env (new).** `HETERION_ROUTER_ALLOWED_EGRESS_HOSTS` (unset ⇒ no ACL,
today's behaviour), `HETERION_ROUTER_EGRESS_WARN_PRIVATE=1`.

**Tests.** `redirect_does_not_forward_authorization` (the verification);
`base_url_rejects_non_http_scheme`; `egress_acl_blocks_unlisted_host`;
`egress_acl_allows_listed_host`; `ui_dir_rejects_home`;
`ui_dir_requires_index_html`; `serve_refuses_symlink_escape` (unix-only).

**Rollout.** 1+3 are behaviour-preserving for the current install (loopback
`heterion-local` is the only loopback egress today and stays allowed). 2's ACL
is opt-in. **Rollback:** unset `HETERION_ROUTER_ALLOWED_EGRESS_HOSTS`; revert.

---

### Wave 8 — Tauri trust boundary

**Depends on:** nothing. Highest value-per-byte in the document.

1. **`desktop/src-tauri/capabilities/default.json`** — drop `shell:allow-spawn`.
   The sidecar is spawned from Rust (`lib.rs:58-64`), so the webview never needs
   it. `core:default` is sufficient for a window that only calls `fetch`.
2. **`desktop/src-tauri/tauri.conf.json:17-21`** — replace `"csp": null` with
   `default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; connect-src 'self' http://127.0.0.1:* ipc: http://ipc.localhost; object-src 'none'; base-uri 'none'; frame-ancestors 'none'`.
   `connect-src http://127.0.0.1:*` is required because `apiBase()` returns
   `http://127.0.0.1:20129` (`api.js:41-46`) — cross-origin from the Tauri
   origin, which is exactly why the CORS allowlist includes the Tauri schemes
   (`cors.rs:20-24`).
3. **Ownership handshake.** `desktop/src-tauri/src/lib.rs:55-93` — after
   `spawn()`, poll `GET {gateway_url}/healthz` for up to ~5 s (100 ms interval,
   bounded) requiring `status == "ok"` **and** a new `instance_id` field on
   `/healthz` (`lib.rs:173-179`) to equal a value the Rust side generated for
   this launch. Only then build the window. On failure: log, surface a
   "gateway failed to start" state, and do **not** fall back to `20128`.
4. **`AppState::instance_id`** — new `pub(crate) field` set in
   `AppState::new` (`lib.rs:78-90`) / `with_db` (`:93-105`) from
   `getrandom`. `healthz` returns it. This is the server side of the handshake.
5. **`desktop/ui/src/api.js`** — (a) fix the Tauri fallback from `20128` to the
   injected port, and refuse to send `Authorization` when the instance id does
   not match; (b) drop the `heterion_router_gateway_url` `localStorage` override
   in release builds (`:33-38`); (c) keep the key in memory by default with
   `HETERION_ROUTER_UI_REMEMBER_KEY=1` to opt into `localStorage`
   (`:1-18`, `App.svelte:22,48-55`).

**Tests.** `healthz_reports_the_process_instance_id` (gateway, `lib.rs` tests);
`ui_api_refuses_to_send_auth_to_a_foreign_instance` (JS — the UI has no test
runner today; at minimum a `node --test` file under `desktop/ui` and a CI step);
`capabilities_do_not_grant_shell_spawn` (a `#[test]` that parses
`capabilities/default.json` and asserts the permission list — cheap and
prevents silent regression); `tauri_conf_has_a_non_null_csp` (same technique).

**Rollout.** 1+2 are one-line config changes with a `cargo tauri build`
verification; ship first. 3+4+5 need the gateway change too, so they ship as a
coordinated release. **Rollback:** `csp` and capabilities are pure config (revert
the file); the handshake is behind `HETERION_ROUTER_SIDECAR_HANDSHAKE=0`.

---

### Wave 9 — Quotas, rate limits, body limits, telemetry retention

**Depends on:** Wave 1 (the `PATCH /api/keys/{id}` from Wave 1 is how an
operator sets `max_sessions` / `usage_limit_enabled`) and Wave 4-stage-4 (the
prune job is the model for the telemetry prune).

1. **Explicit body limit.** `lib.rs:145-171` — add
   `DefaultBodyLimit::max(cfg.max_body_bytes)` with
   `HETERION_ROUTER_MAX_BODY_BYTES` (default 2 MiB, matching today's implicit
   value so nothing changes) on the `/v1/*` and `/api/*` sub-routers.
2. **Concurrency.** `ConcurrencyLimitLayer` with
   `HETERION_ROUTER_MAX_CONCURRENT` (default 64), `503` on overflow. Note: this
   is a `tower` crate addition — `tower` is already a dev-dependency
   (`lib.rs:1305` uses `tower::ServiceExt`), so promote it to a normal dependency.
3. **Per-key rate limit.** A `Mutex<HashMap<String, (Instant, u32)>>` behind an
   `Arc` in `AppState` with a periodic sweeper; `429 rate_limit_exceeded` with
   `Retry-After`. No new crate needed — this is deliberately hand-rolled because
   the state is one map and one clock.
4. **Enforce the schema that already exists.** New
   `crates/heterion-router-gateway/src/limits.rs`:
   `check_budget(conn, key_id, price_table) -> Option<Response>` reading
   `domain_budgets` (`schema_base.sql` `domain_budgets` block) and summing
   `domain_cost_history`; `check_sessions(key_id) -> Option<Response>` against
   `api_keys.max_sessions`. Called from `Authenticated::from_request_parts`
   (`auth.rs:95-139`) right after the row resolves, so the check shares the
   per-request DB read that already happens. `429` with
   `insufficient_quota` / `session_limit_reached`.
5. **Meter `no_log` keys.** Either write to a separate `quota_cost_history` row
   that `no_log` does not suppress (usage volume is metadata, not content), or
   document the gap explicitly in `docs/SECURITY.md`. I recommend the former;
   `no_log` currently means "no accounting at all" (`lib.rs:308-311`), which
   makes it a spend-hiding primitive.
6. **Telemetry retention.** `prune_telemetry(conn, days)` deleting from
   `usage_history` and `call_logs` older than
   `HETERION_ROUTER_TELEMETRY_RETENTION_DAYS` (default 90), reusing Wave 4's
   job runner. `prune` must never touch the `has_request_body` / `request_summary`
   invariant noted in §2.9.

**Config/env (new).** `HETERION_ROUTER_MAX_BODY_BYTES`,
`HETERION_ROUTER_MAX_CONCURRENT`, `HETERION_ROUTER_RATE_LIMIT_RPM`,
`HETERION_ROUTER_TELEMETRY_RETENTION_DAYS`. All default to "off / generous" in
the first release; flip to enforcing after a week of measurement.

**Tests.** `body_over_limit_is_413`; `concurrency_over_limit_is_503`;
`rate_limit_returns_429_with_retry_after`;
`budget_exceeded_returns_insufficient_quota`; `session_cap_is_enforced`;
`no_log_key_is_still_metered_for_quota`; `telemetry_prune_keeps_recent_rows`;
`telemetry_prune_never_touches_key_value`.

**Rollout.** Land with limits disabled; measure; enable per-key first (the
`PATCH` API from Wave 1 makes it opt-in per key), then globally.
**Rollback:** unset the env vars.

---

### Wave 10 — Documentation, CI, ADR

**Depends on:** all of the above.

1. `docs/SECURITY.md` (new): this document's §1 threat model, the retention
   table from §2.9 with the env var that governs each row, the deployment-class
   statement, and the TLS-termination example from Wave 7.4.
2. `docs/adr/ADR-011-responses-state-contract.md` (new): gateway-minted ids,
   owner scoping, default-on with explicit retention, encryption, and the
   `store:false` path — the decision ADR-009 §7 deferred.
3. `docs/adr/ADR-012-key-and-credential-retirement.md` (new): hash backfill,
   plaintext removal, keyring rotation, and the legacy-policy sunset.
4. CI: add `cargo audit` (or `cargo deny`) as a supply-chain gate; add
   `cargo fmt --check` for the excluded `desktop/src-tauri`; add a
   `capabilities_do_not_grant_shell_spawn` + `tauri_conf_has_a_non_null_csp`
   test job (Wave 8); add the `node --test` UI step.
5. Amend `ADR-009`'s addendum with a third dated entry rather than editing the
   2026-09-29 one — the audit trail is the asset.

**Rollout/rollback:** docs only.

---

## 4. Security acceptance matrix

Legend — **Class:** V = verified control, P = partially implemented, N = new.
**Accept:** the condition under which the row is satisfied.

| # | Control | Class | Current state (evidence) | Mitigation | Accept condition | Wave |
|---|---|---|---|---|---|---|
| S-01 | Auth is per-handler, `/healthz` public by construction | **V** | `auth.rs:8-9,95`; `lib.rs:145-171` | none | route table unchanged and every `/v1`,`/api` handler still names an `Authenticated` extractor | — |
| S-02 | Auth extractor runs before body buffering | **V** | `lib.rs:225-229,825-829` | none | preserved by S-01 ordering | — |
| S-03 | Fail-closed on DB/auth error (503, not open) | **V** | `auth.rs:110,136`; test `lib.rs:1597` | none | test stays green | — |
| S-04 | Non-loopback bind refuses startup without a key | **V** | `main.rs:370-384`; tests `main.rs:476,502` | print remediation (7.4) | + startup message names the fix | 7 |
| S-05 | `manage` scope gates every `/api/*` handler | **V** | 13 handlers in `admin.rs` | none | all 13 keep `ManageAuthenticated` | — |
| S-06 | Model allow-list checked before dispatch | **V** | `lib.rs:230,850` | none | both call sites unchanged | — |
| S-07 | NULL policy columns do **not** grant `manage`/all models | **P** | `repos.rs:356-363` → `auth.rs:54,83-86` | 1.1–1.2 | `null_policy_key_is_forbidden_on_admin` + `…_on_models` green; 0.4 list empty or operator-acknowledged | 1 |
| S-08 | An operator can narrow/re-scope/expiry a key via API | **N** | only issue/revoke/restore exist (`lib.rs:156-158`) | 1.3–1.4 | `PATCH /api/keys/{id}` returns 200 and a narrowed key 403s on `/api` | 1 |
| S-09 | Revoking every key cannot lock the operator out | **N** | `main.rs:375` + `auth.rs:107` both count all rows | 1.4 + 0.4 documented recovery | either a documented CLI recovery path exists, or the count query is narrowed and the mismatch is intentional and tested | 1 |
| S-10 | Issued keys are hash-only in storage | **V** | `repos.rs:252-253` | none | stays | — |
| S-11 | Legacy plaintext keys are migrated, then removed | **P** | `MIGRATIONS` = `["001"]` only (`migrate.rs:32`); plaintext writer still `pub` (`repos.rs:195`) | 2a/2b/2c | `rekey_keys` reports 0 recoverable-plaintext rows; `insert_api_key` unreachable in a production build | 2 |
| S-12 | Kill switches (`revoked`/`banned`/`active`/expiry) enforced | **V** | `repos.rs:362-363,385-388` | none | stays; malformed expiry still rejects | — |
| S-13 | Keys expire by default | **N** | `expires_at` hardcoded `NULL` (`repos.rs:237`, `admin.rs:276-284`) | 2d | `HETERION_ROUTER_KEY_DEFAULT_TTL_DAYS` set ⇒ new key 401s after the TTL | 2 |
| S-14 | `scrypt`/AES-GCM primitives, frozen salt and format | **V** | `crypto:18-29,46-129`; JS vectors `crypto:190-206` | none | vectors still pass; `STATIC_SALT` unchanged | — |
| S-15 | Secret material never reaches a `Debug`/log sink | **P** | `FieldCrypto` redacted (`crypto:38-42`), `GrokCliBackend` redacted (`grok_cli.rs:146-154`); `Credential` derives `Debug` (`credentials.rs:15`, `grok_cli.rs:157`) | hand-written redacting `Debug` | no `{:?}` call site can reach a token; a compile-fail or lint guard exists | 3 |
| S-16 | Missing `STORAGE_ENCRYPTION_KEY` is loud, not silent | **P** | no warning anywhere; degrades to `None` | 3.1 | missing key + any `enc:v1:` row ⇒ process refuses to start | 3 |
| S-17 | No plaintext credential **write** path exists | **P** | `main.rs:110-120` `unwrap_or_else(\|\| token.to_string())` | 3.2 | the `unwrap_or_else` is gone; a refresh without a key returns `Err` | 3 |
| S-18 | Encryption key is not co-located with its ciphertext | **P** | `crypto:166` reads `<data_dir>/.env` beside `storage.sqlite` | 3.5 (warn) + docs | operator runs with the key from env/keychain; boot warns when co-located | 3 |
| S-19 | Key rotation is possible without data loss | **N** | single `from_secret(secret: &str)` (`crypto:46`) | 3.3–3.4 | `HETERION_ROUTER_STORAGE_KEYS` rotation + `rekey-storage` round-trips a fixture DB | 3 |
| S-20 | Responses state ids are gateway-minted (no collision takeover) | **N** | `lib.rs:594` + `repos.rs:758` upsert on an upstream id; constant fallback `chatcmpl-unknown` (`openai.rs:188-192`) | 4.1 | `colliding_upstream_ids_do_not_overwrite_each_other` + `cross_owner_read_of_a_collided_id_is_404` | 4 |
| S-21 | Responses state is owner-scoped | **V** | `lib.rs:557-571,623-625,793-797`; test `lib.rs:2115` | none | test stays green **and** is extended with a deliberately-colliding backend | 4 |
| S-22 | `store` default is explicit and logged | **P** | `lib.rs:830-833` defaults to `true` | 4.2 | boot log states the effective default | 4 |
| S-23 | `no_log` suppresses transcripts, not just telemetry | **P** | guard at `lib.rs:308-311`; `persist_response` unguarded (`lib.rs:880`) | 4.2 | `no_log_key_persists_no_transcript` | 4 |
| S-24 | Responses state is encrypted at rest | **N** | plaintext in `key_value` (`lib.rs:594`) | 4.3 | `record_is_encrypted_at_rest`; no plaintext fallback on decrypt failure | 4 |
| S-25 | Responses state is bounded (TTL / rows / bytes) | **N** | no `DELETE` but the user's (`repos.rs:770`) | 4.4 | `prune_removes_oldest_first`, `prune_respects_max_bytes` | 4 |
| S-26 | Erased transcripts are erased from the file | **N** | no `secure_delete` in `db.rs:33-43` | 4.4 | PRAGMA set; documented write cost | 4 |
| S-27 | Unvalidated upstream fields are not echoed/stored | **P** | `lib.rs:1134-1136` returns `response.raw` verbatim | 4.1 | `upstream_extra_fields_are_not_stored` | 4 |
| S-28 | `usage_history` / `call_logs` are retained/pruned | **N** | unbounded; only insert paths exist | 9.6 | `telemetry_prune_keeps_recent_rows` | 9 |
| S-29 | `gateway.log` retention exists | **N** | `rolling::daily`, never deleted (`main.rs:393`) | 6.3 | `log_prune_keeps_n_days` | 6 |
| S-30 | Gemini signature store is bounded | **V** | `gemini.rs:71,116-120,157-161` | none | stays | — |
| S-31 | No request/response bodies in `call_logs` | **V** | `usage/records.rs:46-53` | none | regress-protected by a test asserting those columns stay NULL | 9 |
| S-32 | Data dir `0700`, DB `.env` `0600` | **V** | `db.rs:53-63` (+test `db.rs:171`); `main.rs:359-368` | none | stays | — |
| S-33 | Files created *after* the permission pass are also `0600` | **P** | log `main.rs:393`, signatures `gemini.rs:144,178-181` — all post-pass | 6.1–6.2 | `data_dir_files_are_private_after_boot` (includes `-wal`/`-shm`) | 6 |
| S-34 | Static serving is traversal-safe | **V** | `ui.rs:77-92`; test `ui.rs:115` | none | stays | — |
| S-35 | `HETERION_ROUTER_UI_DIR` root is validated | **N** | `ui.rs:22-32` takes it verbatim | 7.3 | `ui_dir_rejects_home`, `serve_refuses_symlink_escape` | 7 |
| S-36 | `/` and `/{*path}` unauthenticated **by design and documented** | **P** | `lib.rs:160-161`; `ROADMAP:113-114` | docs (`docs/SECURITY.md`) | the surface is named in the threat model with A1 in scope | 10 |
| S-37 | No request-driven SSRF path | **V** | URLs only from registry/env/DB; `UpdateConnectionBody` has no URL (`admin.rs:589-593`) | none | stays; re-checked if admin gains connection-write | 7 |
| S-38 | Outbound requests are scheme- and host-constrained | **N** | `client.rs:34-40` has neither | 7.2 | `egress_acl_blocks_unlisted_host` when the ACL is set | 7 |
| S-39 | Redirects are pinned and cannot leak the bearer | **N** | reqwest default (follows) | 7.1 | `redirect_does_not_forward_authorization` | 7 |
| S-40 | CORS allowlist rejects lookalike hosts; no credentials | **V** | `cors.rs:19-40,104-118`; no `Allow-Credentials` | none | negative test list unchanged | — |
| S-41 | Non-stream upstream errors are redacted | **V** | `lib.rs:538-545` | none | stays | — |
| S-42 | SSE error frames are redacted (chat **and** responses) | **P** | `lib.rs:493-495`, `lib.rs:1211` emit `error.to_string()` = raw upstream body (`client.rs:14-20`) | 5.1–5.3 | canary upstream body absent from the full SSE byte stream | 5 |
| S-43 | Logs do not contain raw upstream bodies by default | **P** | `lib.rs:282,899` log `{error}` | 5.3 | canary absent from `gateway.log` at default `RUST_LOG` | 5 |
| S-44 | Admin responses do not echo `DbError` internals | **P** | `admin.rs:206,303,365,420,460,706,764` | 5.4 | `db_error_response_has_no_sql_or_type_names` | 5 |
| S-45 | Body-level upstream errors do not trip the breaker | **P** | `openai.rs:582,587-589` drops the status prefix; `routing.rs:44-55` then reads it as an outage | 5 (bonus) | `body_level_4xx_does_not_trip_the_breaker` | 5 |
| S-46 | Quotas / sessions / budgets are enforced | **N** | schema exists (`schema_base.sql`, `migrate.rs:73-77`); zero readers | 9.4 | `budget_exceeded_returns_insufficient_quota` | 9 |
| S-47 | Per-key rate limiting and global concurrency cap | **N** | none (`main.rs:441`) | 9.2–9.3 | `rate_limit_returns_429_with_retry_after`, `concurrency_over_limit_is_503` | 9 |
| S-48 | Request body limit is an explicit contract | **N** | implicit axum 2 MiB only | 9.1 | `body_over_limit_is_413` | 9 |
| S-49 | `no_log` keys cannot hide spend | **N** | `lib.rs:308-311` skips all accounting | 9.5 | `no_log_key_is_still_metered_for_quota` | 9 |
| S-50 | Tauri webview cannot spawn processes | **P** | `capabilities/default.json` grants `shell:allow-spawn`, unused by the app | 8.1 | `capabilities_do_not_grant_shell_spawn` | 8 |
| S-51 | Tauri webview has a non-null CSP | **P** | `tauri.conf.json:19` `"csp": null` | 8.2 | `tauri_conf_has_a_non_null_csp` | 8 |
| S-52 | Sidecar ownership + readiness handshake before the window | **N** | `lib.rs:55-93` injects then builds; no probe | 8.3–8.4 | a foreign listener on `20129` never receives an `Authorization` header | 8 |
| S-53 | Desktop cannot fall back to the service port and forward the key | **N** | `api.js:44-46` defaults to `20128` | 8.5a | no request carries auth to a non-matching `instance_id` | 8 |
| S-54 | Operator key is not persisted to `localStorage` by default | **P** | `api.js:1-18`, `App.svelte:22,48-55` | 8.5c | release build keeps the key in memory unless opted in | 8 |
| S-55 | Sidecar exits when the parent dies | **V** | `main.rs:327-353`; `lib.rs:63` | none | stays; marker unset under launchd | — |
| S-56 | Single-instance guard precedes sidecar spawn | **V** | `lib.rs:38-45` | none | stays | — |
| S-57 | Restart requires real launchd supervision | **V** | `admin.rs:387-392`; `restart.rs:50-58` | argv instead of `sh -c` (hygiene) | 409 preserved; argv built without interpolation | 7 |
| S-58 | Secrets are absent from version control | **V** | `.gitignore:17-28`; `git ls-files desktop/src-tauri/binaries/` empty | none | `scripts/ci/check.sh` gains a secret-pattern check | 10 |
| S-59 | A written threat model exists | **N** | none in the repo | 0.5 / 10.1 | `docs/SECURITY.md` names assets, adversaries, and the deployment class | 10 |
| S-60 | Supply chain is gated | **N** | `ci.yml` has no audit step | 10.4 | `cargo audit` job green | 10 |

**Totals: 24 VERIFIED · 24 PARTIAL · 12 NEW** across 60 rows.

---

## 5. Recommended sequencing (single line)

`0 → 1 → 2a/2d/2c → 3.1/3.2 → 5 → 4.1/4.2 → 6 → 7 → 8.1/8.2 → 4.3 → 3.3/3.4 →
2b → 9 → 4.4 → 8.3–8.5 → 10`

Rationale in one breath: establish the blast radius (0) and the ability to
re-scope keys (1) before touching key material (2); make the process refuse to
degrade silently (3) before reusing its crypto (4); land the two cheapest
security wins (5, 6) early since they carry no data risk; harden egress and the
static root (7) and the Tauri config (8.1/8.2, one-line changes) next; only then
rotate keys (3.3/3.4) and remove plaintext (2b) with a proven rollback; enforce
limits (9) once they can be configured per key; and finally delete data
irreversibly (4.4).

---

## 6. Open questions for the mission

1. **Deployment class (blocks severity ratings).** Is the `0.0.0.0` bind a
   deliberate LAN-sharing decision, or a leftover from the JS server? If the
   former, S-04 alone is insufficient and TLS termination (7.4) moves to P0.
2. **Is `STORAGE_ENCRYPTION_KEY` currently set on the live host?** §2.3's
   plaintext-write path is only *live* if it is not. Answering this is a
   one-line read of a boolean, not of the secret.
3. **Does the JS lineage ever run again?** W1 and W2 both assume the frozen
   OmniRoute DB boundary still matters (`ADR-001`). If it is permanently frozen,
   the compatibility constraints in `repos.rs:362` and `schema_base.sql:104`
   relax and several waves get simpler.
4. **Is the Responses transcript store in scope for the product at all?** If the
   answer is "no", 4.1–4.4 collapse to: reject `store:true`, delete the store,
   ship `store:false` semantics only. That is a *smaller* and better outcome than
   building retention for a feature nobody has used (the live audit reports zero
   production rows).
5. **Cross-host redirect behaviour of the pinned `reqwest`** — deliberately left
   as a test task (7.1) rather than a claim.
6. **SQLite `-wal`/`-shm` file modes** — deliberately left as a test task (6.4)
   rather than a claim.

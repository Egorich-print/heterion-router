# Duplicates & dead code

Audit of `crates/heterion-router-{config,core,crypto,db,gateway,http,providers,routing,translate,usage}`
+ `desktop/src-tauri`. Read-only: no source file was edited, moved or deleted.

Workspace size: **16 912 LOC of Rust** across 57 `.rs` files. Largest files:
`gateway/src/lib.rs` (2324), `db/src/repos.rs` (1368), `gateway/src/grok_cli.rs` (1166),
`gateway/src/openai.rs` (881), `gateway/src/gemini.rs` (832), `gateway/src/admin.rs` (783).

**The compiler is clean.** `cargo check --workspace --all-targets` with
`RUSTFLAGS="-W dead_code -W unused"` emits **zero warnings**, and
`cargo clippy --workspace --all-targets` is clean. So there is *no* mechanically
detectable dead code: everything below is either a `pub` item whose only
callers are its own `#[cfg(test)]` tests, or cross-file duplication the linter
cannot see. Both categories were confirmed by reading, not assumed.

---

## Summary table

| ID | kind | what | files | callers | risk | net LOC saved |
|----|------|------|-------|---------|------|---------------|
| D-01 | dup | `response_to_completion` byte-identical ×2 (97 lines each) | `grok_cli.rs:568-664`, `responses.rs:216-312` | 2 (one per backend) | **S** | **−97** |
| D-02 | dup | SSE→`StreamChunk` pump duplicated ×3 (`/responses` dialects) | `grok_cli.rs:424-506`, `responses.rs:67-147`, `gemini.rs:327-403` | 3 | **M** | **−95** |
| D-03 | dup | `drive_stream` duplicated ×2 inside one file | `openai.rs:53-119` vs `openai.rs:468-532` | 2 | **M** | **−60** |
| D-04 | dup | credential-pool retry loop ×4 | `responses.rs:155-202`, `openai.rs:541-590`, `gemini.rs:411-465`, `grok_cli.rs:514-552` | 4 | **M** | **−60** |
| D-05 | dup | `load_credentials` token-vector block ×3 in one function | `main.rs:180-187`, `main.rs:214-221`, `main.rs:247-254` | 3 | **S** | **−14** |
| D-06 | dup | `upstream_status` parser ×2 + `now_unix` ×4 | `routing.rs:57-65`, `lib.rs:548-556`; `lib.rs:69-75`, `ids.rs:11-16`, `credentials.rs:192-198`, `grok_cli.rs:38-44` | 2 / 4 | **S** | **−20** |
| D-07 | dup | `isHidden` JSON probe ×2 (one named `is_hidden`, one inline) | `admin.rs:550-555`, `lib.rs:190-193` | 2 | **S** | **−6** |
| D-08 | dup | `next_token` round-robin ×3 + `ProviderRegistry::load()` ×2 per startup | `openai.rs:459-466`, `responses.rs:58-65`, `gemini.rs:309-312`; `main.rs:151` + `main.rs:316` | 3 / 2 | **S** | **−25** |
| D-09 | dup | "flatten chat content to text" ×6 variants | `core/chat.rs:53-65`, `openai.rs:168-185`, `gemini_request.rs:393-403`, `lib.rs:1151-1171`, `requests.rs:48-72`, `claude_request.rs:46-63` | 6 | **M** | **−30** |
| D-10 | dup | OpenAI chunk-builder ×3 (`chunk_id`/`chunk_model`/`role_chunk`/`delta_chunk`) | `translate/claude.rs:216-265`, `translate/gemini.rs:246-297`, `translate/responses.rs:326-354` | 3 | **M** | **−45** |
| D-11 | dead | `combo_configs` field written, never read | `routing.rs:34,100,115` | 0 reads | **S** | **−12** |
| D-12 | dead | `_touch()` + its `#[allow(dead_code)]` | `gemini.rs:567-571` | 0 | **S** | **−6** |
| D-13 | dead | `Settings` — entire module unreferenced outside its own tests | `config/src/settings.rs` (86), `config/src/lib.rs:12` | 0 | **S** | **−86** |
| D-14 | dead | test-only `request()` builders ×4 | `openai.rs:276-287`, `grok_cli.rs:691-702`, `gemini.rs:581-592`, `responses.rs:320-331` | tests only | **S** | **−38** |
| D-15 | dead | `pub` API with zero non-test callers (12 items) | see §Dead code | 0 | **S** | **−60** |
| D-16 | dead | unused Cargo features/deps: `http-body-util`, `tempfile` (db) | `gateway/Cargo.toml:41`, `db/Cargo.toml:17` | 0 | **S** | **−2** |

**Total estimated net LOC saved: ~656** (of which ~613 are pure deletions,
~43 are "keep the helper, delete the copies" nets).

Suggested order: **D-01, D-05, D-07, D-12, D-13, D-16** first (all S, all
mechanical), then D-11, D-08, D-06, then the M-risk stream/loop dedups
(D-02, D-03, D-04) which need new tests.

---

### D-01 — `response_to_completion` is byte-identical in two files

**Risk S · −97 LOC · behaviour-preserving**

Both copies are the exact same 97-line function. Verified by
`sed -n '568,664p' grok_cli.rs | diff - <(sed -n '216,312p' responses.rs)`:
the only differences are one comment line and trailing-blank alignment.

- copy 1: `crates/heterion-router-gateway/src/grok_cli.rs:568` (fn at 568, body ends 664)
- copy 2: `crates/heterion-router-gateway/src/responses.rs:216` (fn at 216, body ends 312)

Both build `ChatCompletionResponse` from a Responses-API object: text from
`output[].content[].output_text`, tool calls from `output[].function_call`,
usage from `usage.{input,output,total}_tokens`, and `raw: Some(payload.clone())`.

**Callers (complete):**
- `grok_cli.rs:543` — inside `GrokCliBackend::complete`
- `responses.rs:186` — inside `ResponsesBackend::complete`

Both are `fn` (private) in their own module, so there is **no public API
change** and the Tauri app is unaffected.

**Canonical location:** `crates/heterion-router-gateway/src/responses.rs`.
`ResponsesBackend` is the *generic* `openai-responses` executor (built from
any registry provider); `GrokCliBackend` is the credential-refreshing special
case. The generic module is the right owner.

**Migration:**
1. Change `grok_cli.rs:568` to `use crate::responses::response_to_completion;`
   and bump the fn in `responses.rs:216` from `fn` to `pub(crate) fn`.
2. Delete `grok_cli.rs:567-664` (the doc comment, the fn, and the blank line
   before `#[cfg(test)]`).
3. `grok_cli.rs` loses `ChatChoice` / `Usage` from its `heterion_router_core`
   import at `grok_cli.rs:13-18` if nothing else in the file uses them —
   `cargo check` will tell you.

**Test that proves behaviour is unchanged:**
- `cargo test -p heterion-router-gateway grok_cli::tests::complete_converts_response_object`
  (`grok_cli.rs:752`) — the grok side.
- `cargo test -p heterion-router-gateway responses::tests::completes_via_responses_api`
  (`responses.rs:335`) — asserts `response.raw["metadata"]["opaque"]` survives,
  which is the part most likely to break if the wrong copy wins.
- Before touching anything, confirm the two bodies really are equal:
  `diff <(sed -n '568,664p' crates/heterion-router-gateway/src/grok_cli.rs) <(sed -n '216,312p' crates/heterion-router-gateway/src/responses.rs)`.

---

### D-02 — the SSE→`StreamChunk` pump is written three times

**Risk M · −95 LOC · needs a new test**

All three `drive_stream` bodies contain the same 60-line loop: `SseDecoder`
push → `[DONE]` sentinel → `serde_json::from_str` → per-event translate →
`openai_chunk_to_stream_chunk` → `tx.send`, plus the trailing
`if !finish_seen { "upstream stream ended before terminal event" }` guard.

- `crates/heterion-router-gateway/src/grok_cli.rs:424-506` (83 lines)
- `crates/heterion-router-gateway/src/responses.rs:67-147` (81 lines)
- `crates/heterion-router-gateway/src/gemini.rs:327-403` (77 lines)

Diff of the first two (request-building preamble excluded) is 6 lines of
noise; the loop body is character-for-character the same.

**Divergences you must preserve** (this is why it is M, not S):

| | grok_cli | responses | gemini |
|---|---|---|---|
| translate call | `convert_event(&mut ResponsesState, event_type, &data)` → `Converted` | identical | `convert_event(&mut GeminiState, &data, now)` → `Vec<Value>` |
| post-decode hook | — | — | `record_signatures(&chunk_json, …)` (gemini.rs:381) |
| `[DONE]` w/o finish | error | error | error, but returned from *inside* the loop (gemini.rs:368-374) |

**Canonical location:** a new `pub(crate) async fn pump_sse(...)` in
`crates/heterion-router-gateway/src/backend.rs`, next to
`openai_chunk_to_stream_chunk` (`backend.rs:107`) which is already the shared
stream helper. Signature sketch:

```rust
pub(crate) async fn pump_sse<F>(
    response: reqwest::Response,
    tx: &mpsc::Sender<Result<StreamChunk, GatewayError>>,
    mut on_chunk: F,
) -> Result<(), GatewayError>
where
    F: FnMut(&Value, &mut StreamChunk /* sentinel */) -> ControlFlow<…>,
```

**Migration:** extract, then convert one call site at a time, running
`cargo test -p heterion-router-gateway` after each.

**Tests that prove behaviour is unchanged:**
- `grok_cli::tests::streams_translated_deltas_to_finish` (`grok_cli.rs:706`)
- `grok_cli::tests::streams_incremental_tool_call_arguments` (`grok_cli.rs:834`)
- `gemini::tests::streams_sse_chunks` (`gemini.rs:798`)
- `openai::tests::streams_openai_sse_to_finish` (`openai.rs:291`)
- Add one: assert the terminal-event guard still fires when a stream ends
  without `finish_reason` (today's behaviour is
  `GatewayError::Upstream("upstream stream ended before terminal event")`).
  No test covers this today — **verify manually** that the error is
  load-bearing before extracting.

---

### D-03 — `OpenAiBackend` and `ProviderOpenAiBackend` carry the same `drive_stream`

**Risk M · −60 LOC**

- `crates/heterion-router-gateway/src/openai.rs:53-119` (`OpenAiBackend`)
- `crates/heterion-router-gateway/src/openai.rs:468-532` (`ProviderOpenAiBackend`)

`diff` of the two normalised bodies is **6 lines**, all of it cosmetic:
the `post_json` argument list (`&self.chat_url(), self.bearer()` vs
`&self.chat_url, self.next_token()`) and `if data.get("error").is_some()`
vs `if let Some(error) = data.get("error")`.

`OpenAiBackend::complete` (`openai.rs:127-153`) is likewise a
single-attempt special case of `ProviderOpenAiBackend::complete`
(`openai.rs:541-590`).

**Callers (complete):**
- `OpenAiBackend::new` — `main.rs:144`, only when `OPENAI_COMPAT_BASE_URL` is set
- `ProviderOpenAiBackend::new` — `main.rs:261` (per-provider loop)
- `OpenAiBackend` also referenced in `openai.rs` tests at 309, 350, 370
- `ProviderOpenAiBackend` tests at 656, 702, 754, 834

`OpenAiBackend` **is** reachable production code (env-gated), so it is not a
deletion candidate — it is a "one backend with `tokens: Vec<String>` of size 1"
candidate. `ProviderOpenAiBackend` already has the pool, the
`all credentials failed` fallback and the responses-sub-backend.

**Canonical location:** `ProviderOpenAiBackend`. Make `OpenAiBackend` a thin
constructor:

```rust
// openai.rs, replacing OpenAiBackend's body
pub fn new(base_url: String, api_key: Option<String>) -> Result<Self, HttpError> {
    Self::for_single(provider: "openai-compatible", base_url, api_key.into_iter().collect())
}
```

**Migration:**
1. Add `ProviderOpenAiBackend::for_single(provider, base_url, tokens)` that
   runs `base_url` through `resolve_chat_url` (`openai.rs:382`) and sets
   `responses: None`.
2. Verify the URL is byte-identical: today `OpenAiBackend::chat_url()`
   (`openai.rs:45`) is `format!("{base}/chat/completions")` after
   `trim_end_matches('/')` at `openai.rs:40`; `resolve_chat_url` additionally
   no-ops when the URL *already* ends in `/chat/completions`. For
   `OPENAI_COMPAT_BASE_URL` that is a behaviour change only if the env var
   already carries the full path — **verify manually** against the README /
   `scripts/`.
3. Delete `OpenAiBackend::drive_stream` (`openai.rs:53-119`) and
   `OpenAiBackend::complete` (`openai.rs:127-153`), keeping `name()`.

**Tests:** `openai::tests::streams_openai_sse_to_finish` (`openai.rs:291`),
`openai::tests::complete_returns_typed_completion` (`openai.rs:330`),
`openai::tests::upstream_error_surfaces` (`openai.rs:358`).

---

### D-04 — the credential-pool retry loop is written four times

**Risk M · −60 LOC**

The shape `for _ in 0..pool.len().max(1) { post … ; parse json ; if payload.error { last_error = …; continue } ; return Ok(…) }`
followed by `Err(last_error.unwrap_or_else(|| Upstream(format!("{provider}: all credentials failed"))))`:

- `crates/heterion-router-gateway/src/responses.rs:155-202` (48 lines)
- `crates/heterion-router-gateway/src/openai.rs:541-590` (50 lines)
- `crates/heterion-router-gateway/src/gemini.rs:411-465` (55 lines)
- `crates/heterion-router-gateway/src/grok_cli.rs:514-552` (grok's variant;
  uses `call_with_refresh` instead of a raw post)

Also duplicated inside it: the upstream-error extraction
`payload.get("error").and_then(|e| e.get("message")).and_then(Value::as_str)`
appears at `responses.rs:189`, `openai.rs:578`, `gemini.rs:448`,
`grok_cli.rs:336`.

**Canonical location:** `crates/heterion-router-gateway/src/backend.rs`.

```rust
pub(crate) fn upstream_error_message(payload: &Value) -> Option<&str> {
    payload.get("error")?.get("message")?.as_str()
}
```

plus a `try_pool(attempts, |token| async { … })` helper. The per-backend
differences (responses-sub-backend delegation, gemini header auth, grok 401
refresh) stay in the caller; only the loop skeleton and the error extraction
move.

**Migration:** land `upstream_error_message` first (trivially safe, 6 call
sites), then the loop helper once the three `HttpClient`-based backends are
converted. Leave `grok_cli` alone in step 1 — its refresh-on-401 makes it the
odd one out.

**Tests:** `responses::tests::completes_via_responses_api`,
`openai::provider_tests::keyless_provider_sends_no_bearer` (the AGENTS.md
contract test — keyless pool must send **no** `Authorization`),
`openai::provider_tests::provider_backend_serves_completion`,
`gemini::tests::completes_via_generate_content`.

---

### D-05 — the credential→token-vector block is repeated three times

**Risk S · −14 LOC · behaviour-preserving**

- `crates/heterion-router-gateway/src/main.rs:180-187`
- `crates/heterion-router-gateway/src/main.rs:214-221`
- `crates/heterion-router-gateway/src/main.rs:247-254`

All three are the same 7-line block inside `select_backend`:

```rust
let tokens: Vec<String> = {
    let guard = db.connection();
    load_credentials(&guard, &provider, crypto.as_ref())
        .unwrap_or_default()
        .into_iter()
        .map(|credential| credential.token)
        .collect()
};
```

Each is followed by a slightly different "keyless?" gate
(`main.rs:221-225`, `main.rs:259-261`) — keep those where they are.

**Canonical location:** a free fn in `main.rs`, above `select_backend`
(next to `provider_base_url_from`, `main.rs:74`):

```rust
fn provider_tokens(db: &Db, provider: &str, crypto: Option<&FieldCrypto>) -> Vec<String>
```

**Migration:** add the helper, replace all three blocks. Pure deletion
afterwards; no control flow moves.

**Tests:** `main.rs` has no test that reaches `select_backend` (it needs a DB
and the filesystem), so this is verified by `cargo clippy --workspace
--all-targets -- -D warnings` plus `cargo test -p heterion-router-gateway`
and by reading the three gates again. **Verify manually** that the three call
sites really pass the same `provider` — they do (`main.rs:182`, `216`, `249`
all use `&provider` from the same loop).

---

### D-06 — one `upstream_status` parser written twice; four `now_unix` clones

**Risk S · −20 LOC · behaviour-preserving**

`upstream_status` — parses `"upstream status {code}: {body}"`, identical logic:

- `crates/heterion-router-gateway/src/routing.rs:57-65` (used by
  `is_backend_outage` `routing.rs:46`, `is_transient` `routing.rs:136`,
  tests `routing.rs:390`)
- `crates/heterion-router-gateway/src/lib.rs:548-556` (used by `error_status`
  `lib.rs:558`, test `lib.rs:1489`)

**Canonical:** `gateway/src/lib.rs:548`, made `pub(crate)`; `routing.rs`
imports it. Same crate, so no API change.

`now_unix`-style helpers — four copies of
`SystemTime::now().duration_since(UNIX_EPOCH)…`:

- `crates/heterion-router-gateway/src/ids.rs:11-16` `unix_seconds() -> i64` (canonical, 10 call sites)
- `crates/heterion-router-gateway/src/lib.rs:69-75` `now_unix() -> u64` (callers `lib.rs:88`, `103`, `621`)
- `crates/heterion-router-gateway/src/credentials.rs:192-198` `now_seconds() -> i64` (1 caller, `is_expired` `credentials.rs:126`)
- `crates/heterion-router-gateway/src/grok_cli.rs:38-44` `now_unix_seconds() -> i64` (1 caller, `refresh_expiry` `grok_cli.rs:89`)

The `u64` one (`lib.rs:69`) is the only one not interchangeable — it returns
`u64` for `AppState::started_unix`. Keep it, or cast at the three call sites.

**Migration:** `credentials.rs` and `grok_cli.rs` already have `ids` in scope
in the same crate — call `crate::ids::unix_seconds()` and delete both fns.

**Tests:** `routing::tests::upstream_status_is_parsed_from_the_client_message`
(`routing.rs:390`), `tests::upstream_statuses_are_preserved_for_clients`
(`lib.rs:1489`), `credentials::tests::rfc3339_parser_handles_offsets_and_fractions`
(`credentials.rs:296`), `grok_cli::tests::formats_refresh_expiry`
(`grok_cli.rs:600`).

---

### D-07 — the `isHidden` probe exists as both a named fn and an inline copy

**Risk S · −6 LOC · behaviour-preserving**

- `crates/heterion-router-gateway/src/admin.rs:550-555` — `fn is_hidden(data: &str) -> bool`
  (callers `admin.rs:128`, `admin.rs:693`)
- `crates/heterion-router-gateway/src/lib.rs:190-193` — the same three-line
  expression inlined inside `list_models`

```rust
serde_json::from_str::<Value>(&combo.data)
    .ok()
    .and_then(|value| value.get("isHidden")?.as_bool())
    .unwrap_or(false)
```

Both default a corrupt blob to *visible*, which is the documented intent
(`admin.rs:547-548`).

**Canonical:** `admin.rs:550`. Bump to `pub(crate) fn is_hidden`, import in
`lib.rs`, delete the inline block.

**Test:** `tests::model_list_omits_hidden_combos` (`lib.rs:1511`) — writes both
a visible and a hidden combo and asserts `/v1/models` shows one.

---

### D-08 — round-robin token picker ×3, registry re-parsed twice per startup

**Risk S · −25 LOC**

`next_token` — the identical
`fetch_add(1, Relaxed) % len` round-robin:

- `crates/heterion-router-gateway/src/openai.rs:459-466` (`ProviderOpenAiBackend`)
- `crates/heterion-router-gateway/src/responses.rs:58-65` (`ResponsesBackend`)
- `crates/heterion-router-gateway/src/gemini.rs:309-312` (`GeminiBackend`, returns `&str` not `Option<&str>`)
- (`crates/heterion-router-gateway/src/grok_cli.rs:231` is the 4th copy, over `Vec<Credential>`)

**Canonical:** a `TokenPool { tokens: Vec<String>, next: Arc<AtomicUsize> }`
in `gateway/src/credentials.rs` (already the credential module), with
`next(&self) -> Option<&str>` and `len(&self)`. Each backend holds one field.

This also lets `token_count` be answered uniformly — see D-15, where
`ProviderOpenAiBackend::token_count` (`openai.rs:455`) has a real caller
(`openai.rs:760`) but `ResponsesBackend::token_count` (`responses.rs:53`) and
`GeminiBackend::token_count` (`gemini.rs:305`) have **none**.

`ProviderRegistry::load()` twice — `main.rs:151` and `main.rs:316`, both in
`select_backend`. The first result is used through the provider loop
(`main.rs:176`, `201`, `244`); the second (`main.rs:316`) shadows it and goes
to `with_combo_source`. `ProviderRegistry::load` re-parses the embedded
278-provider `registry.json` each call, so this is a real (if cheap) duplicate
parse. Delete `main.rs:316` and reuse the binding from `main.rs:151` —
`registry.clone()` already works at `main.rs:318`.

**Migration:** land the `main.rs:316` deletion on its own first (one-line,
obviously safe). The `TokenPool` extraction is a separate commit.

**Tests:** `openai::provider_tests::xai_responses_model_uses_responses_endpoint`
(`openai.rs:677`), `openai::provider_tests::keyless_provider_sends_no_bearer`
(`openai.rs:743`), `responses::tests::completes_via_responses_api`.
The `main.rs` change has no direct test — the provider loop only runs with a
populated `provider_connections` table. **Verify manually** by booting against
a fixture DB and comparing the `backend available: …` log lines.

---

### D-09 — "flatten chat content to text" exists in six variants

**Risk M · −30 LOC**

| location | filters on | joins with |
|---|---|---|
| `crates/heterion-router-core/src/chat.rs:53-65` (`ChatMessage::text`) | presence of `text` | `""` |
| `crates/heterion-router-gateway/src/openai.rs:168-185` (`message_text`) | `type == "text"` | `""` |
| `crates/heterion-router-translate/src/gemini_request.rs:393-403` (`content_text`) | presence of `text` | `""` |
| `crates/heterion-router-gateway/src/lib.rs:1151-1171` (`input_item_text`) | `input_text\|output_text\|text\|refusal` | `""` |
| `crates/heterion-router-translate/src/requests.rs:48-72` (`instructions_text`) | type-or-`text` key, drops empties | `"\n\n"` |
| `crates/heterion-router-translate/src/claude_request.rs:46-63` (`normalize_content_to_string`) | `type == "text"`, stringifies numbers/bools | `"\n"` |

The first and third are **byte-identical bodies**; the second is identical
modulo the `type == "text"` filter. The last three differ in separator and
part-type handling and each mirrors a distinct JS function
(`normalizeContentToString`, `toString`, …) with a parity fixture behind it.

**Recommendation — do not merge these eagerly.** Merge only the two identical
bodies (`gemini_request.rs:393-403` → a shared
`pub fn content_text(Option<&Value>) -> String` in `translate/src/lib.rs`) and
make `openai.rs:168` a call to it, adding the `type == "text"` filter at the
call site only if a test demands it. The remaining three are *contract*
code: `claude_request.rs` and `requests.rs` are each pinned by a
`tests/*_parity.rs` fixture (`claude_request_parity.rs`,
`request_parity.rs`) that asserts byte-equivalence with the JS original.
Merging them changes the fixtures, which AGENTS.md calls a contract.

**Tests:** `core::chat::tests::message_text_reads_string_and_parts`
(`core/chat.rs:210`), `translate::tests::claude_request_matches_js_ground_truth`,
`translate::tests::request_matches_js_ground_truth`.

---

### D-10 — OpenAI chunk builders triplicated across the translate crate

**Risk M · −45 LOC**

`chunk_id` / `chunk_model` / `role_chunk` / `delta_chunk` are near-identical in
three modules, all emitting `chat.completion.chunk`:

- `crates/heterion-router-translate/src/claude.rs:216-265`
- `crates/heterion-router-translate/src/gemini.rs:246-297`
- `crates/heterion-router-translate/src/responses.rs:326-354`

Diff of `claude.rs:216-265` vs `gemini.rs:246-297`: **2 lines** of difference,
both the parameter type (`&ClaudeState` / `&GeminiState`) and one extra
`state.role_sent = true;` in gemini's `role_chunk`. `chunk_id` and
`chunk_model` are identical modulo the type name.

`finish_chunk` is genuinely different per protocol (Claude and Gemini attach
different usage shapes) — leave those alone.

**Canonical:** a `chunk.rs` module in `translate/src` holding

```rust
pub struct ChunkIds { pub id: String, pub model: String }
pub fn chunk_ids(message_id: Option<&str>, model: Option<&str>) -> ChunkIds
pub fn role_chunk(ids: &ChunkIds, now: i64) -> Value
pub fn delta_chunk(ids: &ChunkIds, now: i64, content: Option<&str>, reasoning: Option<&str>) -> Value
```

Each state keeps a `ChunkIds` built once on `message_start` / first chunk.

**Migration:** do this last — it touches three fixture-pinned translators at
once. Land `chunk_ids` first (pure extraction, zero output change), then
`role_chunk`/`delta_chunk`.

**Tests:** `translate::tests::claude_matches_js_ground_truth`
(`tests/claude_parity.rs`, 9 cases), `gemini_matches_js_ground_truth`
(`tests/gemini_parity.rs`, 3 cases),
`tests::openai_responses_to_openai_matches_js_ground_truth`
(`tests/parity.rs`), `tests::tools_parity`. **All four must stay byte-green** —
they are the whole reason these functions are safe to touch at all.

---

## Dead code

### D-11 — `combo_configs` is written and never read

`crates/heterion-router-gateway/src/routing.rs`

- declared `routing.rs:34`
- initialised empty `routing.rs:100`
- populated from the DB `routing.rs:107-115` in `with_combo_source`

**Zero reads.** `combo_config()` (`routing.rs:126-135`) ignores the field and
re-runs `load_combos(&guard)` on *every* request. So the field is both dead
*and* the reason a cheap lookup is a full combo-table parse per call.

Two options; pick one, do not do both:
- **(a) delete** the field and the population loop (`routing.rs:34`, the
  `let mut configs = …` block at 109-114, `routing.rs:115`) — pure dead-code
  removal, behaviour identical; or
- **(b) wire it up** — have `combo_config()` read `self.combo_configs` and fall
  back to `ComboConfig::default()`. This is a behaviour change: combos edited
  in the dashboard would stop taking effect until restart. The `plan()` path
  (`routing.rs:175`) already re-reads the DB, so (b) introduces an
  inconsistency between the two unless `plan()` is changed too.

**(a) is the low-risk answer** and is what this audit recommends. Net −12 LOC.
If the per-request `load_combos` cost actually matters, that is a separate
change with its own test (`routing::tests::combo_name_expands_and_rewrites_model`,
`routing.rs:669`, plus a new one asserting a DB edit is picked up).

### D-12 — `_touch()` and its `#[allow(dead_code)]`

`crates/heterion-router-gateway/src/gemini.rs:567-571`

```rust
/// Keep the `json!` import meaningful for tests and future fields.
#[allow(dead_code)]
fn _touch() -> Value {
    json!({})
}
```

This is the **only** `#[allow(dead_code)]` / `#[allow(unused…)]` in the whole
workspace (`rg -n "allow\(dead_code\)|allow\(unused" crates desktop/src-tauri`
returns exactly this one hit). The justification is stale: `json!` *is* used
in production code at `gemini.rs:259`. Delete `gemini.rs:567-571` and the
`#[allow]` disappears with it. Net −6 LOC.

The mid-file `use crate::backend::openai_chunk_to_stream_chunk;` at
`gemini.rs:565` should be folded into the top import block
(`gemini.rs:14-24`) in the same commit — cosmetic, but it is what made the
stale comment look necessary.

### D-13 — the whole `Settings` module is unreferenced

`crates/heterion-router-config/src/settings.rs` (86 lines) +
`crates/heterion-router-config/src/lib.rs:12`

`Settings` is exported but the **only** construction in the workspace is its
own test (`settings.rs:64`). `rg -n "\bSettings\b" crates desktop/src-tauri`
returns five hits, all inside `settings.rs` plus the re-export.

It is also a thin pass-through: `get`/`set`/`get_json`/`set_json`
(`settings.rs:22-46`) do nothing but forward to `repos::kv_get`/`kv_set`, and
the gateway already calls those repos **directly** —
`gateway/src/lib.rs:627` (`kv_set`) and `lib.rs:645` (`kv_get`) for the
Responses store. So the abstraction is already bypassed by its only real
consumer.

Net −86 LOC if removed (module + re-export; keep the test if you want the
`kv_*` round-trip covered — `repos.rs:1354-1366` already covers it).

**Verify manually before deleting:** confirm no out-of-tree consumer. The
workspace has one external binary (`desktop/src-tauri`, excluded from the
workspace) and it depends on **no** workspace crate —
`desktop/src-tauri/Cargo.toml` lists only `serde_json`, `serde`, `log`, and the
three `tauri-*` plugins. So nothing outside the workspace can reach `Settings`
through a path Cargo knows about. Low risk.

### D-14 — four identical `#[cfg(test)]` request builders

**Risk S · −38 LOC**

```rust
fn request() -> ChatCompletionRequest {
    ChatCompletionRequest {
        model: "…".to_string(),
        messages: vec![ChatMessage::plain("user", "…")],
        stream: …,
        max_tokens: None, temperature: None, top_p: None,
        tools: None, tool_choice: None,
        ..Default::default()
    }
}
```

- `crates/heterion-router-gateway/src/openai.rs:276-287`
- `crates/heterion-router-gateway/src/grok_cli.rs:691-702`
- `crates/heterion-router-gateway/src/gemini.rs:581-592`
- `crates/heterion-router-gateway/src/responses.rs:320-331`

Only `model`, the user text and `stream` differ. `backend.rs:319-341` has a
fifth, near-identical pair (`request(text)` + `user_message(text)`).

**Recommendation:** these are 12 lines each and they sit in `#[cfg(test)]`
blocks with different fixture models per backend. Merging them into one
`fn test_request(model: &str, text: &str, stream: bool)` in `backend.rs`
saves ~38 LOC but couples four unrelated test modules to a shared helper.
Worth it only if you are already editing those files. Rank it **below**
everything else.

### D-15 — `pub` items with zero non-test callers

`cargo check -W dead_code` cannot flag these (they are `pub` in a library), so
each was confirmed by reading and by `rg` across `crates/` + `desktop/src-tauri/`.

| item | file:line | only referenced by |
|---|---|---|
| `needs_translation` | `core/src/format.rs:42-44` | its own tests (`format.rs:52,57,65`); re-exported at `core/src/lib.rs:15` |
| `WireFormat::as_str` | `core/src/format.rs:26-34` | its own tests (`format.rs:70-71`) |
| `ProviderRegistry::model_target_format` | `providers/src/registry.rs:133-139` | `model_wire_format` (`registry.rs:144`) + tests |
| `ProviderRegistry::model_wire_format` | `providers/src/registry.rs:143-147` | its own tests (`registry.rs:299,308,312`) |
| `ProviderRegistry::len` | `providers/src/registry.rs:87-89` | its own test (`registry.rs:254`) |
| `ProviderRegistry::is_empty` | `providers/src/registry.rs:92-94` | nothing at all |
| `ProviderRegistry::ids` | `providers/src/registry.rs:99-101` | its own tests (`registry.rs:261,343`) |
| `CircuitBreaker::state` | `routing/src/breaker.rs:41-48` | `BreakerSet::state` + tests |
| `BreakerSet::state` | `routing/src/breaker.rs:115-117` | its own tests (`breaker.rs:167-171`) |
| `PriceTable::set_override` | `usage/src/pricing.rs:97-99` | its own test (`pricing.rs:160`) |
| `PriceTable::price_for` | `usage/src/pricing.rs:102-104` | `cost_usd` (`pricing.rs:118`) + tests |
| `Db::open_default` | `db/src/db.rs:66-70` | **nothing at all** — `main.rs:398` builds the path itself |
| `repos::list_active_connections` | `db/src/repos.rs:64-…` | its own test (`repos.rs:991`); the gateway uses `credentials::active_providers` instead |
| `repos::get_api_key_by_key` | `db/src/repos.rs:182-…` | its own test (`repos.rs:1019-1021`); auth uses `find_usable_api_key` |
| `repos::get_combo_by_name` | `db/src/repos.rs:510-…` | its own tests (`repos.rs:1184,1240`) |
| `ResponsesBackend::token_count` | `gateway/src/responses.rs:53-55` | nothing |
| `GeminiBackend::token_count` | `gateway/src/gemini.rs:305-307` | nothing |
| `ChatMessage::has_tool_calls` | `core/src/chat.rs:67-70` | its own test (`chat.rs:227`) + `openai.rs:811` (a test) |

**Recommendation — split by risk:**

*Delete outright (dead in every sense):*
`ProviderRegistry::is_empty`, `Db::open_default`, `ResponsesBackend::token_count`,
`GeminiBackend::token_count`, and the `#[allow]`-adjacent `_touch` (D-12).
Net −25 LOC. For `Db::open_default`, note it returns `(Db, PathBuf)` and
`main.rs:398-404` reimplements exactly that inline — if you prefer, keep the
function and make `main.rs` call it instead; that is the better direction.

*Keep (public API of a library crate, plausibly next consumer):*
`needs_translation`, `WireFormat::as_str`, `model_wire_format`,
`model_target_format`, `len`, `ids`, `state` ×2, `set_override`, `price_for`,
`has_tool_calls`. These are deliberate vocabulary surfaces — `WireFormat`,
`ProviderRegistry` accessors, breaker introspection — and
`heterion-router-core` / `-providers` / `-routing` are published crates whose
whole point is the vocabulary. Deleting `needs_translation` because the
gateway has not wired the translation matrix yet would be deleting the
roadmap.

*Delete or wire (genuinely ambiguous — **verify manually**):*
`repos::list_active_connections`, `repos::get_api_key_by_key`,
`repos::get_combo_by_name`. These are real repository functions with real
SQL; the question is whether the admin API is *supposed* to expose them.
`get_combo_by_name` in particular is the obvious lookup for combo-name
resolution and is bypassed today by `combos::load_combos`
(`gateway/src/combos.rs:185`), which does a full-table scan and parse. That
looks like an oversight, not an intentional removal — leave them and note
them.

### D-16 — unused Cargo dependencies and features

**Risk S · −2 LOC (plus build time)**

- `crates/heterion-router-gateway/Cargo.toml:41` — `http-body-util = "0.1"`.
  `rg -n "http_body_util" crates desktop/src-tauri` matches only the manifest.
  The gateway tests read bodies through `axum::body::to_bytes`
  (`lib.rs:1341`, `lib.rs:1946`), not `http_body_util`.
- `crates/heterion-router-db/Cargo.toml:17` — `tempfile = { workspace = true }`.
  `rg -n "tempfile" crates` matches only the manifest and an unrelated
  `desktop/src-tauri/Cargo.lock` entry. `db`'s tests use
  `Db::open_in_memory()` throughout.

There are **no `[features]` sections** in any workspace `Cargo.toml`, so the
"unused feature" check comes back empty. Confirmed
(`rg -n "\[features\]" crates/*/Cargo.toml desktop/src-tauri/Cargo.toml` → no hits).

---

## Out of scope / not worth touching

- **`registry.json` (5 400+ lines, 278 providers).** Not code duplication, it
  is data, and `providers/src/registry.rs` contract tests
  (`provider_ids_are_unique`, `every_provider_has_executor_and_format`) plus
  the insta snapshot `providers/src/snapshots/…provider_id_list_snapshot.snap`
  are the contract. Do not touch.

- **`schema_base.sql` + `migrate.rs` `MIGRATIONS`.** One migration entry
  (`migrate.rs:32-35`) including a 17-table base schema verbatim from the JS
  `core.ts`. AGENTS.md is explicit that the runner shares history with the
  Node server and that the ledger has no downgrade path. Out of bounds.

- **`crypto` salt/prefix constants.** `STATIC_SALT`
  (`crypto/src/lib.rs:23`) and `PREFIX` (`:18`) are called out in AGENTS.md as
  a live-database contract (`~/.heterion-router` rows are encrypted with them).
  Never dedup, never rename.

- **`SseDecoder` (`http/src/sse.rs`) vs the per-backend loops.** The decoder
  itself is *already* shared and correct; what is duplicated is the pump
  *around* it (D-02). Do not "fix" `SseDecoder` — its tests
  (`sse.rs:107-153`) cover split-UTF-8 and split-CRLF edge cases that the
  duplication makes irrelevant.

- **`record_completion_usage` / `record_stream_usage` (gateway/src/lib.rs:308,
  :368).** Listed as a suspect in the brief. On reading: **not duplicated.**
  `record_stream_usage` is a five-line wrapper that computes `latency_ms` and
  delegates; there is exactly one `UsageRecord` + one `CallLogRecord`
  construction site (`lib.rs:325-364`). `usage/src/records.rs` is the SQL layer
  and does not overlap with it. No action.

- **`resolve_chat_url` / `resolve_responses_url` (`openai.rs:382, :391`).**
  Also listed as a suspect. On reading: **not duplicated** — they are the only
  two URL builders, `main.rs:263,283` are the only callers, and the
  `trim_end_matches('/')` at `openai.rs:383`/`:392` is their first line, not a
  competing implementation. The bare `trim_end_matches('/')` in the four
  constructors (`openai.rs:40`, `responses.rs:46`, `grok_cli.rs:196`,
  `gemini.rs:296`) is a different job (normalising an input once) and folding
  it in would mean threading a resolved URL through every constructor. Low
  value; D-03 touches part of this area anyway.

- **Stringly-typed provider dispatch in `main.rs`.** The brief flagged repeated
  `match`/comparison on provider ids. The real shape is three sequential
  `if registry.get(&provider).and_then(|e| e.format.as_deref()) == Some("…")`
  comparisons (`main.rs:176-178`, `210-212`, then `is_openai_format` at `:244`).
  They *could* be a `match registry.model_wire_format(...)` — but
  `model_wire_format` takes a *model* id and these branches dispatch on the
  *provider* format, and `parse_wire_format` returns `None` for the ~19
  unmodelled formats (`registry.rs:37-45`) that would then silently fall
  through. Turning this into a typed `WireFormat` match is a real
  improvement and also a behaviour change for every unmodelled provider.
  Medium-to-high risk, ~−20 LOC. Not in this pass.

- **`grok_cli.rs:38-70` (`now_unix_seconds`, `rfc3339_from_epoch`,
  `civil_from_days`).** A hand-rolled date library, ~30 lines, duplicating the
  civil-date arithmetic in `credentials.rs:177-190`
  (`days_from_civil`, the *inverse* function). Both are needed: one formats
  epoch→RFC3339, the other parses RFC3339→epoch. Folding them into one
  `time` helper module would save ~15 lines and is arguably the right long-run
  shape, but it means the credential-expiry comparison and the OAuth refresh
  path share one date implementation — a real correctness surface.
  Recommend as its own task, not bundled here.

- **`claude_request.rs` / `requests.rs` / `claude.rs` translator internals.**
  Deliberately line-by-line ports with byte-equivalence fixtures. AGENTS.md:
  "Тестовые снепшоты (`*.snap`) — часть контракта; править только вместе с
  кодом." Merge only behind green fixtures (see D-09, D-10).

- **`docs/history/` frozen JS material.** Not Rust, not compiled, not in scope.

- **Non-obvious correctness note, not a dedup.** `main.rs:266-280` rebuilds the
  `responses_models` set by hand:
  ```rust
  entry.models.iter()
      .filter(|model| model.target_format.as_deref() == Some("openai-responses"))
  ```
  This duplicates — but does **not** match — `registry.model_target_format`
  (`registry.rs:133-139`), which falls back to the provider's own `format`
  when a model has no explicit `target_format`. So a provider-level
  `format: "openai-responses"` with per-model `target_format: null` models
  would be routed to `/chat/completions` by `main.rs` and to `/responses` by the
  registry helper. Same for the two `Some("gemini")` / `Some("openai-responses")`
  provider-level comparisons at `main.rs:176-212`. **This is a latent bug, not
  duplication** — flagging it, not fixing it. Worth checking against
  `registry.json`: if no provider has `format: "openai-responses"` with null
  per-model targets, it is unreachable today.

---

## Commands run

All read-only. Separate `CARGO_TARGET_DIR=/tmp/hra-a4` to avoid lock
contention with the repo's own `target/`; no target directory was deleted.

```sh
# Size map (57 files, 16 912 LOC)
find crates desktop/src-tauri/src -name '*.rs' | sort | xargs wc -l | sort -k1 -nr

# Public surface of every crate
for f in crates/*/src/lib.rs; do echo "=== $f ==="; cat "$f"; done

# Warning gates — both clean, which is why every item below is manual
CARGO_TARGET_DIR=/tmp/hra-a4 cargo build --workspace
CARGO_TARGET_DIR=/tmp/hra-a4 RUSTFLAGS="-W dead_code -W unused" \
  cargo check --workspace --all-targets
CARGO_TARGET_DIR=/tmp/hra-a4 cargo clippy --workspace --all-targets
CARGO_TARGET_DIR=/tmp/hra-a4 cargo test --workspace     # 215 tests, all green

# Marker sweep
rg -n "allow\(dead_code\)|allow\(unused" crates desktop/src-tauri   # 1 hit (D-12)
rg -n "TODO|FIXME|XXX|HACK" crates desktop/src-tauri                # 0 hits

# Duplicate-pair proofs
diff <(sed -n '568,664p' crates/heterion-router-gateway/src/grok_cli.rs) \
     <(sed -n '216,312p' crates/heterion-router-gateway/src/responses.rs)
diff <(sed -n '53,119p'  crates/heterion-router-gateway/src/openai.rs) \
     <(sed -n '468,532p' crates/heterion-router-gateway/src/openai.rs)
diff <(sed -n '424,506p' crates/heterion-router-gateway/src/grok_cli.rs) \
     <(sed -n '67,147p'  crates/heterion-router-gateway/src/responses.rs)
diff <(sed -n '216,265p' crates/heterion-router-translate/src/claude.rs) \
     <(sed -n '246,297p' crates/heterion-router-translate/src/gemini.rs)

# Per-symbol reachability (every row in the D-15 table)
rg -n "<symbol>" crates desktop/src-tauri

# Dependency reachability
rg -n "http_body_util|tempfile|\[features\]" crates desktop/src-tauri
```
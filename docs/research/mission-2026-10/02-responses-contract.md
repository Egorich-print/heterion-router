# 02 — Responses Contract: implementation plan

**Mission role:** principal architect 2/6
**Scope:** `heterion-router` Responses request/response translation, streaming translator, reasoning & signatures, state store/replay, tools, JS parity fixtures.
**Canonical repo:** `/Volumes/Netac_SSD/ai-workstation/Projects/heterion-router` (read-only for this mission; nothing was modified or committed).
**Baseline verified in-session:** `cargo test --workspace` → **213 passed, 0 failed** (matches `STATUS.md:114`). `git log -1` = `51dfe6d`, working tree already dirty (docs + sources modified) per `docs/research/PROJECT-AUDIT-2026-09.md:9-10`.

Everything labelled **[EXT]** is an external claim from vendor documentation, not from this repository. Everything else is anchored to `file:line` in the working tree.

---

## 1. Executive summary

The gateway has a working Responses *happy path* and a *partially* working Responses streaming path, but three things are true today and all three are silent-data-loss bugs:

1. **The internal pivot cannot carry an Responses-shaped turn.** `StreamChunk` (`crates/heterion-router-core/src/chat.rs:156-167`) is `{content, reasoning, tool_calls: Value, finish_reason, usage}`. It has no response id, no output-item identity, no `sequence_number`, no `usage` details. Everything the OpenAI Responses streaming contract needs is therefore *unrepresentable* and gets dropped rather than mis-shaped.
2. **The `/v1/responses` SSE emitter is a hybrid, non-SDK shape.** `chat_chunk_to_responses_events` (`crates/heterion-router-gateway/src/lib.rs:1254-1298`) emits `response.reasoning_text.delta` (a name that exists in neither the OpenAI event list **[EXT]** nor the xAI list **[EXT]**), a `response.output_item.added` whose `item` carries `call_id: ""` and a nested `tool_calls` array (not an OpenAI `function_call` item), and a `response.completed` with no `id`, no `output`, and no `sequence_number`. The project already records this as non-SDK-safe (`docs/research/PROJECT-AUDIT-2026-09.md:102-104`).
3. **The state store is keyed by an upstream-controlled id, unbounded, plaintext, and never written for streams.** `persist_response` uses `response["id"]` as the SQLite key (`lib.rs:577-581, 594`), defaults `store` to `true` (`lib.rs:830-833`), stores the full request+response envelope in `key_value.value` (`lib.rs:583-591`, `crates/heterion-router-db/schema_base.sql:65-70`), prunes nothing, and is only called on the non-stream branch (`lib.rs:879-881`) — so a `store: true` **streaming** response returns an id that `GET /v1/responses/{id}` cannot resolve.

Additionally, `reasoning` **input** items are dropped outright (`lib.rs:1015-1017`), OpenAI **built-in tools** are dropped by a `filter_map` that requires a top-level `name` (`lib.rs:1068-1069`), reasoning **summary** replay reads `summary` as a string when the contract makes it an array of `{type:"summary_text", text}` (`lib.rs:683-685`, contradicted by **[EXT]** the xAI `GET /v1/responses` example and the OpenAI migrate-to-Responses example), and reasoning/cache token accounting is computed then thrown away twice (`translate/src/responses.rs:479-483` → `backend.rs:130-154` → `lib.rs:1288-1292`).

**Recommendation:** do not redesign the pivot. Add (a) a typed Responses **event emitter** in `heterion-router-translate`, (b) an inbound **request/capability** module extracted out of `lib.rs` into `heterion-router-translate`, (c) an **upstream dialect** layer (OpenAI incremental vs xAI whole-call snapshot), and (d) a real `response_store` table behind a versioned record. Ship every behaviour change behind a flag with a documented rollback.

---

## 2. Current-state evidence ledger

### 2.1 Inbound surface

| Concern | Symbol | Evidence |
|---|---|---|
| Route | `POST /v1/responses`, `GET/DELETE /v1/responses/{id}` | `lib.rs:164-168` |
| Handler | `responses` | `lib.rs:825-915` |
| Store default | `store` defaults to `true` when absent | `lib.rs:830-833` |
| Replay trigger | `previous_response_id` read from `extra` | `lib.rs:839-849` |
| Model policy | `auth.allows_model(&request.model)` | `lib.rs:850-852`; impl `auth.rs:44-58` |
| Request translation | `responses_to_chat` | `lib.rs:922-1107` |
| Input item loop | `message` / `function_call` / `function_call_output`, `_ => {}` | `lib.rs:949-1020`, drop at `:1017` |
| **Reasoning input items dropped** | `_ => {}` with a "ponytail" note | `lib.rs:1015-1017` |
| **Built-in tools dropped** | `filter_map` requires `tool["name"]` | `lib.rs:1065-1083`, esp. `:1068-1069` |
| Responses-only keys → `extra` | `include`, `reasoning`, `previous_response_id`, `text`, `prompt_cache_key`, `session_id`, `conversation_id` | `lib.rs:1033-1045` |
| **Leak into Chat bodies** | `for (key, value) in &request.extra { body[key] = … }` | `backend.rs:98-100` |
| Content flattening | `input_item_text` (drops annotations, logprobs, part structure) | `lib.rs:1111-1130` |

### 2.2 Non-stream output

| Concern | Symbol | Evidence |
|---|---|---|
| Chat → Responses wrapper | `chat_to_responses_object` | `lib.rs:1133-1182` |
| Raw passthrough (accidental correctness) | `if let Some(raw) = response.raw … return raw.clone()` | `lib.rs:1134-1136` |
| Storage key = upstream id | `persist_response` | `lib.rs:573-598`, key at `:594` |
| Record v1 shape | `{version:1, api_key_id, model, backend, stored_at, request, response}` | `lib.rs:583-591` |
| Id validation | `valid_response_id` (alnum + `-_.`, ≤200) | `lib.rs:549-555` |
| Owner scoping | `response_visible` | `lib.rs:564-571` |
| **Reasoning summary replay is wrong-shaped** | `item.get("summary").and_then(Value::as_str)` | `lib.rs:682-686` |
| Upstream `model` leaks into response | `raw` returned verbatim | `lib.rs:1134-1136` vs `lib.rs:877-878` |

### 2.3 Outbound (gateway → upstream Responses)

| Concern | Symbol | Evidence |
|---|---|---|
| Generic Responses executor | `ResponsesBackend` | `gateway/src/responses.rs:27-213` |
| Request build | `openai_to_responses` + `responses_input` override + `stream` | `responses.rs:76-80` |
| Stream loop + dialect gate | `convert_event` per event; `[DONE]` handling | `responses.rs:96-138`, `:106-111` |
| No terminal ⇒ error | `"upstream stream ended before terminal event"` | `responses.rs:140-144` |
| **Whole-call snapshot tool calls dropped** | output loop has `message`/`function_call` only, `_ => {}`; reasoning items dropped; `reasoning_content: None` | `responses.rs:226-264`, `:261`, `:289` |
| Grok CLI executor (same translator) | `GrokCliBackend::drive_stream` | `grok_cli.rs:424-505` |
| Hard-coded shared chat id | `ResponsesState::new("chatcmpl-rust", …)` | `responses.rs:89-93`, `grok_cli.rs:448-452` |
| Duplicate snapshot→chat converter | `grok_cli.rs:568-665` (identical to `responses.rs:216-312`) | — |
| Provider session headers (426 guard) | `session_headers` | `grok_cli.rs:103-125` |
| Per-model Responses dispatch | `responses_models.contains(&request.model)` | `openai.rs:548-552`, `:593-597` |
| Model set built from registry | filter on `target_format == "openai-responses"` **only** (no provider-format fallback) | `main.rs:268-287`, esp. `:275` |
| Registry *does* have the fallback | `model_target_format` → `model.target_format ?? provider.format` | `providers/src/registry.rs:133-146` |
| Responses URL derivation | `resolve_responses_url` | `openai.rs:391-400` |
| Registry reach | 150 models across 14 providers carry `target_format: "openai-responses"` (of 2717 models / 278 providers) | `registry.json` (verified by parse); providers incl. `openai`, `xai`, `xai-oauth`, `grok-cli`, `github`, `ghe-copilot`, `codex`, `cheaperinference`, `opencode*` |

### 2.4 Streaming translator

| Concern | Symbol | Evidence |
|---|---|---|
| Entry point | `convert_event` | `translate/src/responses.rs:104-324` |
| Text delta | `response.output_text.delta` | `:106-112` |
| Reasoning deltas (3 spellings) | `reasoning_summary_text` / `reasoning_content_text` / `reasoning_text` | `:113-121` |
| `output_text.done` ignored | | `:122` |
| Incremental tool lifecycle | `.added` → buffer → `.done` emits buffered args | `:123-307` |
| Terminal | `response.completed` / `.failed` / `error` | `:308-319` |
| **Everything else silently ignored** | `_ => Converted::Ignore` | `:320-322` |
| Finish-reason heuristic | `tool_call_index > 0 \|\| current_tool_call.is_some()` | `:356-362` |
| Usage extraction (with reasoning/cache detail) | `extract_usage` | `:439-499` |
| **Details then dropped twice** | `openai_chunk_to_stream_chunk` → `Usage{prompt,completion,total}`; emitter rebuilds without details | `backend.rs:130-154`; `lib.rs:1288-1292` |

### 2.5 Gateway-side Responses SSE emitter

| Concern | Symbol | Evidence |
|---|---|---|
| Stream handler | `responses_stream` | `lib.rs:1185-1248` |
| SSE event name derived from payload `type` | `Event::default().event(event)` | `lib.rs:1236-1242` |
| Error frame has **no** `type` ⇒ empty `event:` name | `json!({"error": {…}})` | `lib.rs:1210-1213` |
| **Raw upstream text in SSE error** | `error.to_string()` | `lib.rs:1211`; Chat path `lib.rs:493-495` |
| Sanitized non-stream errors | `error_response` → `"upstream request failed"` | `lib.rs:538-545` |
| No `data: [DONE]` on Responses (Chat path has one) | | `lib.rs:1247` vs `lib.rs:447-449` |
| Non-standard reasoning event name | `response.reasoning_text.delta` | `lib.rs:1257-1260` |
| Non-standard tool item | `{"type":"function_call","call_id":"","arguments":"","tool_calls":…}` | `lib.rs:1268-1279` |
| Terminal event lacks `id`/`output`/`sequence_number` | | `lib.rs:1281-1296` |
| Streams are never persisted | `persist_response` only in the non-stream arm | `lib.rs:877-881` |

### 2.6 State store and replay

| Concern | Symbol | Evidence |
|---|---|---|
| Namespace | `"responses"` | `lib.rs:547` |
| Generic KV accessors | `kv_get` / `kv_set` / `kv_delete` | `db/src/repos.rs:746-774` |
| Table | `key_value(namespace, key, value)` — no TTL, no size, no index | `db/schema_base.sql:65-70` |
| Migration ledger | single `"001"`, append-only | `db/src/migrate.rs:30-36` |
| Replay | `replay_previous_response` | `lib.rs:716-758` |
| Model pinning | rejects cross-model | `lib.rs:736-740` |
| **Provider/credential origin not pinned** | nothing compares against stored backend/provider | `lib.rs:716-758` (no such check) |
| Transcript materialisation (not upstream passthrough) | `previous_response_id` removed and inlined | `lib.rs:749-756` |
| Instructions duplicated on replay | new `instructions` are index 0, previous ones are prepended again | `lib.rs:733-745` + `lib.rs:933-940` |

### 2.7 JS parity fixtures

| Fixture | Cases | Test | Consumer |
|---|---|---|---|
| `translate/tests/fixtures/responses_to_openai.json` | 8 | `tests/parity.rs:40` | `tests/parity.rs:41` |
| `translate/tests/fixtures/responses_tools.json` | 5 (asserted exactly, `tools_parity.rs:40`) | `tests/tools_parity.rs:37` | `tests/tools_parity.rs:38` |
| `translate/tests/fixtures/openai_to_responses.json` | 4 (asserted exactly, `request_parity.rs:30`) | `tests/request_parity.rs:26` | `tests/request_parity.rs:28` |
| `claude_to_openai.json`, `gemini_to_openai.json`, `openai_to_claude.json` | — | `claude_parity.rs`, `gemini_parity.rs`, `claude_request_parity.rs` | — |
| Registry id snapshot | — | `insta` snapshot `providers/src/snapshots/…provider_id_list_snapshot.snap` | `registry.rs:344` |

The three Responses fixtures encode the **JS OmniRoute adapter's output contract**, not the OpenAI contract (`translate/src/responses.rs:3-12` names the JS symbols). They are therefore *regression locks for the Chat-shaped translator* and must **not** be rewritten when the Responses *client-facing* emitter changes — they govern `translate::responses`, which this plan does not alter. This is the single most important fixture-hygiene rule in this document (see §7.1).

### 2.8 Precedent to reuse (do not reinvent)

- **Bounded + persistent opaque store:** `ThoughtSignatures` — file-backed, LRU-capped at `SIGNATURE_CAPACITY = 4096`, rewritten on load (`gateway/src/gemini.rs:51-190`, cap at `:71`, evict at `:116-120` and `:157-161`).
- **Opaque-bytes round-trip:** `record_signatures` / `inject_signatures` re-inject the signature when the client dropped it (`gemini.rs:196-264`).
- **Field encryption:** `FieldCrypto::encrypt` → `enc:v1:` prefix, `STATIC_SALT` frozen (`crypto/src/lib.rs:18-24`, `:78-91`); already a gateway dependency (`gateway/Cargo.toml:10`).
- **Env flag convention:** `HETERION_ROUTER_*` primary, `OMNIROUTE_*` one-cycle fallback (`config/src/config.rs:42-49`; contract in `AGENTS.md:33`).
- **Byte-safe SSE decode:** `SseDecoder` (`http/src/sse.rs:19-53`), split-UTF-8/CRLF tests at `:132-153`.
- **Status classification:** `upstream_status` + `error_status` (`lib.rs:511-536`); 4xx must not trip the breaker (`routing.rs:44-55`, `AGENTS.md:38`).

---

## 3. External contract facts used **[EXT]**

All fetched 2026-09-29 from vendor docs; none validated against this gateway.

1. **OpenAI Responses lifecycle events** (developers.openai.com, *Streaming API responses*): the SDK `StreamingEvent` union includes `ResponseCreatedEvent`, `ResponseInProgressEvent`, `ResponseFailedEvent`, `ResponseCompletedEvent`, `ResponseOutputItemAdded/Done`, `ResponseContentPartAdded/Done`, `ResponseOutputTextDelta`, `ResponseOutputTextAnnotationAdded`, `ResponseTextDone`, `ResponseRefusalDelta/Done`, `ResponseFunctionCallArgumentsDelta/Done`, `Error`, plus `file_search`/`code_interpreter` tool events. Documented common set: `response.created`, `response.output_text.delta`, `response.completed`, `error`.
2. **`sequence_number`** is a field on Responses streaming events ("The sequence number of this event, used to order streaming events"), and `openai-node`'s `ResponseStream` exposes `starting_after` keyed on it; `openai-ruby` reconstructs `sequence_number` for delta/done events.
3. **Reasoning items are array-shaped**: `{"type":"reasoning","id":…,"summary":[{"type":"summary_text","text":…}],"content":[]}`; summary is a list, never a bare string.
4. **Encrypted reasoning**: set `store: false` and replay every returned reasoning item; each item carries `encrypted_content`; pass it back unchanged. For ZDR orgs OpenAI enforces `store: false` automatically.
5. **Built-in tools** (`web_search`, `file_search`, `computer_use`, `code_interpreter`, `image_generation`, remote MCP) have **no top-level `name`** — they are discriminated by `type`.
6. **xAI**: `include: ["reasoning.encrypted_content"]` returns encrypted reasoning; `grok-4.7` returns `reasoning.encrypted_content` on *every* Responses response regardless of `include`; reasoning items in `output` carry `encrypted_content` and must be passed back **unchanged** in the next `input`.
7. **xAI tools**: "Currently, only functions and web search are supported as tools. A max of 350 tools."
8. **xAI responses streaming**: for `stream: true`, "Tokens will be sent as data-only server-sent events … with the stream terminated by a `data: [DONE]` message."
9. **xAI `instructions` + `previous_response_id`**: `instructions` "cannot be used alongside `previous_response_id`".
10. **xAI retention**: "New responses will be stored for 30 days and then permanently deleted."
11. **xAI response `status`** ∈ `{completed, in_progress, incomplete}`; `object` is always `"response"`; `usage` carries `input_tokens_details.cached_tokens`, `output_tokens_details.reasoning_tokens`, `total_tokens`, plus xAI-specific `num_server_side_tools_used`, `num_sources_used`, `server_side_tool_usage_details`.
12. **Responses `store` defaults on** upstream ("Responses are stored by default… To disable storage… set `store: false`").
13. **xAI response `id` in the docs' own example is a bare UUID** (`"id": "ad5663da-63e6-86c6-e0be-ff15effa8357"`) while OpenAI uses `resp_…`. A storage key derived from it can contain characters outside `valid_response_id`'s allow-list (`lib.rs:549-555`) — a concrete availability bug, not just a security one.

**Consequence for the design:** the gateway must not assume the upstream id is a safe, stable, gateway-issued handle. It mints its own (§6.4).

---

## 4. Target design

### 4.1 Layering (unchanged pivot, new boundaries)

```text
client  ──HTTP/SSE──▶  /v1/responses                [gateway/src/lib.rs]
                            │
                            ├─ responses::request::decode()   (moved out of lib.rs)
                            │      → ChatCompletionRequest + RequestRequirements + ResponseInput
                            ├─ capability::check()           (new: 400 unsupported_capability)
                            ├─ RoutingBackend::complete/stream
                            │      └─ ResponsesStreamEmitter  (new, typed SSE lifecycle)
                            └─ response_store::{put,get,del,prune}  (new, own table)

upstream ──HTTP/SSE──▶  ResponsesBackend / GrokCliBackend
                            │
                            ├─ requests::openai_to_responses  (chat → Responses request)
                            ├─ upstream::Dialect              (new: openai-incremental | xai-snapshot)
                            └─ responses::convert_event      (unchanged; JS-parity locked)
```

### 4.2 Dialects

`Dialect` is chosen by `DialectDetector` from observed events, with an explicit override flag:

| Dialect | Trigger | Behaviour |
|---|---|---|
| `OpenAiIncremental` (default) | first `response.output_item.added` seen, or any `response.function_call_arguments.delta` | exactly today's `translate::responses::convert_event` path |
| `XaiSnapshot` | terminal event carries `response.output[]` containing items never announced via `.added` (or explicit flag) | `XaiSnapshotFold` synthesises the missing `output_item.added` / `function_call_arguments.done` / `output_item.done` triples, **in `output_index` order**, then defers to the incremental path |

`XaiSnapshotFold` holds `seen_items: HashSet<String>` (item `id`) and `announced_index: BTreeMap<i64, String>`. On `response.completed` / `response.incomplete` it walks `data.response.output`, and for each `type == "function_call"` item whose `id` is unseen it emits the three synthesised events through the **same** `convert_event` entry point (by re-injecting synthetic `response.output_item.added` / `response.output_item.done` payloads). This closes the recorded gap at `docs/research/PROJECT-AUDIT-2026-09.md:90-92` and `STATUS.md:37` ("xAI snapshot-only tool calls теряются") without touching the JS-parity translator's behaviour for incremental input.

**Invariant:** both dialects converge on the same `Converted` stream before it reaches `openai_chunk_to_stream_chunk`, so `ResponsesBackend`, `GrokCliBackend` and the Chat client path see one shape.

### 4.3 SDK-shaped event lifecycle (the core deliverable)

New module `heterion-router-translate/src/responses_events.rs`, pure, `serde`+`serde_json` only (matches `translate/Cargo.toml:6-7`). Input: `StreamChunk`; output: `Vec<ResponsesEvent>` where

```rust
pub struct ResponsesEvent { pub sequence_number: u64, pub kind: ResponsesEventKind }
pub enum ResponsesEventKind { /* one variant per event name below */ }
impl ResponsesEvent { pub fn type_name(&self) -> &'static str; pub fn to_value(&self) -> Value }
```

Emission order per response (validator-enforced):

```text
response.created                    (exactly 1, seq 0)
response.in_progress                (exactly 1)
  per message item:
    response.output_item.added      (item: {id, type:"message", status:"in_progress", role, content:[]})
    response.content_part.added     (part: {type:"output_text", text:"", annotations:[]})
    response.output_text.delta      (× N)
    response.output_text.done       (text: joined)
    response.content_part.done
    response.output_item.done       (item … content:[{…, text: joined}])
  per reasoning item (if chunk.reasoning present):
    response.output_item.added      (item: {id, type:"reasoning", summary:[], content:[], encrypted_content?})
    response.reasoning_summary_part.added
    response.reasoning_summary_text.delta (× N)
    response.reasoning_summary_text.done
    response.reasoning_summary_part.done
    response.output_item.done
  per tool call (index i, sequential by arrival):
    response.output_item.added      (item: {id, type:"function_call", call_id, name, arguments:"", status:"in_progress"})
    response.function_call_arguments.delta (× N, if the backend streams fragments)
    response.function_call_arguments.done  (arguments: joined)
    response.output_item.done       (status:"completed")
exactly one terminal:
  response.completed | response.incomplete | response.failed
```

Every event carries `sequence_number` (monotonic, 0-based, no gaps). Every event is written as `event: <type>\ndata: <json>` — `lib.rs:1236-1242` already does this correctly **when** the payload has a `type`; the emitter guarantees it always does, including for errors (`{"type":"error","code":…,"message":…,"param":null,"sequence_number":N}`).

Terminal `response` object (all fields an SDK deserialises, `null` where not applicable):

```json
{"id":"resp_…","object":"response","created_at":1756315696,"status":"completed","error":null,
 "incomplete_details":null,"instructions":"…","max_output_tokens":null,"model":"…",
 "output":[ …assembled items… ],"parallel_tool_calls":true,"previous_response_id":null,
 "reasoning":{"effort":"high","summary":null},"store":true,"temperature":null,
 "text":{"format":{"type":"text"},"verbosity":null},"tool_choice":"auto","tools":[…],
 "top_p":null,"truncation":"disabled","metadata":{},"user":null,
 "usage":{"input_tokens":N,"input_tokens_details":{"cached_tokens":C},
          "output_tokens":M,"output_tokens_details":{"reasoning_tokens":R},"total_tokens":T}}
```

**Changes deliberately *not* made in phase 1** (recorded so they are not lost): `logprobs` on text parts; `annotations` (URL citations) — OpenAI **[EXT]** emits `response.output_text.annotation.added`; `response.refusal.delta/done`; `web_search_call` / `file_search_call` / `code_interpreter_call` items (they require the built-in-tool capability this plan rejects); `response.reasoning_text.*` (xAI-only spelling **[EXT]**) as an *emitted* event — kept as an *accepted upstream* spelling only, never emitted downstream.

### 4.4 Reasoning items and encrypted content

Three distinct leaks today; three distinct fixes.

| # | Leak | Fix |
|---|---|---|
| R1 | `reasoning` **input** items dropped at `lib.rs:1017` | `responses::request::decode` retains every input item **verbatim** in `ResponseInput`; items whose `type` the internal chat pivot cannot express (everything except `message`, `function_call`, `function_call_output`) are kept in `responses_input` and re-attached verbatim to the upstream `input`. `capability::check` then decides whether the resolved upstream can accept them. |
| R2 | `summary` replayed as a string at `lib.rs:683-685` | `response_output_messages` is rewritten to walk `summary[*].text`, and to emit the whole reasoning item into `responses_input` unchanged rather than synthesising assistant prose. Never decrypt, never log, never render `encrypted_content` as text (ROADMAP principle 3, `docs/ROADMAP-2026-09.md:109-110`). |
| R3 | Upstream reasoning **output** items lost on Chat-shaped upstreams and on streams | `StreamChunk` gains `reasoning_encrypted: Option<String>` (opaque pass-through, `None` when absent). `chat_to_responses_object` and the emitter attach `encrypted_content` to the emitted `reasoning` item. Reasoned summary text continues to flow through `StreamChunk.reasoning` as before. |

The one safe degradation is a **non-Responses upstream receiving a reasoning item**: this returns `400 {"code":"unsupported_capability"}` unless `HETERION_ROUTER_RESPONSES_REASONING=strip`, in which case it drops the item, logs `warn!` with a counter, and continues. Default is fail-closed (`docs/ROADMAP-2026-09.md:106`).

### 4.5 Tools

`capability::classify_tools` returns `(Vec<ToolSpec>, Vec<UnsupportedTool>)`:

- `{"type":"function", "name":…}` → supported (the only universally supported class).
- `xai` + `{"type":"web_search"}` → supported **only** when the resolved provider is `xai`/`xai-oauth` **[EXT]**; forwarded verbatim (already handled by `requests.rs:440-442`).
- Everything else (`file_search`, `computer_use_preview`, `code_interpreter`, `image_generation`, `mcp`, `local_shell`, `custom`, unknown) → `400 unsupported_capability`, naming the exact tool `type`.

No silent drops. This replaces `lib.rs:1068-1069`'s `filter_map` and closes P0 item 3 (`docs/research/PROJECT-AUDIT-2026-09.md:187`).

### 4.6 State store v2

New table (migration `"002"`, appended to `db/src/migrate.rs:32-36`, idempotent DDL):

```sql
CREATE TABLE IF NOT EXISTS response_store (
  id                     TEXT PRIMARY KEY,      -- gateway-minted resp_…
  upstream_response_id   TEXT,                  -- nullable, indexed
  api_key_id             TEXT,                  -- nullable (open-auth installs)
  model                  TEXT NOT NULL,
  origin_provider        TEXT,                  -- canonical provider id
  origin_connection_ids  TEXT NOT NULL DEFAULT '[]',
  origin_format          TEXT NOT NULL,         -- WireFormat::as_str()
  envelope               TEXT NOT NULL,         -- JSON, or enc:v1:… when a key is present
  encrypted              INTEGER NOT NULL DEFAULT 0,
  bytes                  INTEGER NOT NULL,
  stored_at              INTEGER NOT NULL,
  expires_at             INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS response_store_owner   ON response_store(api_key_id, stored_at DESC);
CREATE INDEX IF NOT EXISTS response_store_expiry  ON response_store(expires_at);
CREATE INDEX IF NOT EXISTS response_store_upstream ON response_store(upstream_response_id)
  WHERE upstream_response_id IS NOT NULL;
```

Record v2 (envelope) mirrors §3's response object plus `request`, `id`, `upstream_response_id`, `origin`, `store_mode`, `schema_version: 2`.

Guards: `store` mode flag, TTL (default 2 592 000 s = 30 d, matching xAI **[EXT]**), `MAX_BYTES` per record (default 1 MiB → `400 invalid_request_error`, never truncate), `MAX_RECORDS` LRU cap (default 10 000), opportunistic prune on write + a prune on boot. Encryption via existing `FieldCrypto` (`crypto/src/lib.rs:78-91`); when no key is configured, store plaintext and log `warn! responses_store_unencrypted` — the same policy `main.rs:97,111-119` already applies to OAuth refresh.

Streaming now persists: an `in_progress` placeholder at `response.created`, overwritten with the assembled terminal object **before** `response.completed` is written to the socket, so a client that races a `GET` never 404s.

---

## 5. Module and symbol plan

Legend: **NEW** file · **NEW** symbol · **MOD** existing symbol.

### 5.1 `heterion-router-translate` (deps stay `serde` + `serde_json`; **add** `heterion-router-core` to reuse `WireFormat` — see `core/src/format.rs:32-46`, which exists precisely to stop a second vocabulary from drifting)

**`src/lib.rs` — MOD**: `pub mod capability; pub mod responses_events; pub mod responses_request; pub mod upstream;`

**`src/capability.rs` — NEW**
```rust
pub struct RequestRequirements {
    pub opaque_reasoning_items: Vec<String>,   // item ids
    pub wants_encrypted_reasoning: bool,
    pub unsupported_tools: Vec<UnsupportedTool>,
    pub wants_builtin_web_search: bool,
    pub streams: bool,
    pub store: bool,
}
pub struct UnsupportedTool { pub kind: String, pub reason: &'static str }
pub enum CapabilityError { UnsupportedTools(Vec<UnsupportedTool>), OpaqueReasoning, … }
pub fn classify_tools(tools: Option<&Value>, provider: Option<&str>) -> (Vec<Value>, Vec<UnsupportedTool>);
pub fn check(req: &RequestRequirements, upstream: UpstreamCaps) -> Result<(), CapabilityError>;
pub struct UpstreamCaps { pub wire_format: Option<WireFormat>, pub provider: Option<String> }
```
`CapabilityError` renders to `{"error":{"message":…,"type":"invalid_request_error","code":"unsupported_capability","param":…}}` with HTTP 400, matching the existing `forbidden_response` shape (`lib.rs:497-509`).

**`src/responses_request.rs` — NEW** — hosts `responses_to_chat` (moved verbatim-then-fixed from `lib.rs:922-1107`), `input_item_text`, `append_input_items`, `response_output_messages` (moved from `lib.rs:646-701`), `replay_previous_response`'s pure half.
```rust
pub struct DecodedRequest { pub request: ChatCompletionRequest, pub input: ResponseInput, pub requirements: RequestRequirements }
pub struct ResponseInput { pub raw: Value, pub items: Vec<Value> }   // raw kept verbatim
pub fn decode(body: &Value) -> Result<DecodedRequest, CapabilityError>;
pub fn merge_instructions(previous: Option<&Value>, current: Option<&Value>, mode: InstructionsMode) -> Option<String>;
pub enum InstructionsMode { Dedupe, Concat, Newest }
```
`lib.rs` keeps thin `use` re-exports for one cycle so the 8 in-crate tests at `lib.rs:1824-1885, 2037-2112` keep compiling.

**`src/responses_events.rs` — NEW**
```rust
pub struct ResponsesStreamEmitter {
    pub fn new(response_id: String, created_at: i64, model: String, instructions: Option<String>, store: bool) -> Self;
    pub fn push(&mut self, chunk: &StreamChunk) -> Vec<ResponsesEvent>;
    pub fn finish(&mut self, outcome: Outcome) -> Vec<ResponsesEvent>;   // exactly one terminal event
    pub fn assembled_response(&self) -> Value;                            // full §4.3 object
}
pub enum Outcome { Completed, Incomplete { reason: String }, Failed { code: String, message: String } }
```
Terminal-event enforcement: `finish()` sets `terminated`; a second `finish()` returns `vec![]` and returns `Err` in debug (`assert!`) — the same guard style already used by `finish_sent` (`translate/src/responses.rs:309-312`).

**`src/upstream.rs` — NEW**
```rust
pub enum Dialect { OpenAiIncremental, XaiSnapshot }
pub struct DialectDetector { seen_items: HashSet<String>, forced: Option<Dialect> }
impl DialectDetector {
    pub fn new(forced: Option<Dialect>) -> Self;
    pub fn fold(&mut self, state: &mut ResponsesState, event_type: &str, data: &Value)
        -> Vec<(String, Value)>;   // synthetic events to feed through convert_event first
}
```
`XaiSnapshot` synthesises from `data.response.output` in `output_index` order and only for `function_call` items unseen by id — it never re-emits text/reasoning that the incremental path already delivered, so the two dialects are byte-identical downstream.

**`src/requests.rs` — MOD**: `openai_to_responses` gains `pub fn openai_to_responses_with_input(model, body, input_override: Option<&Value>)`; the current two-line body of `responses.rs:76-79` / `grok_cli.rs:434-436` becomes a call to it, so the override rule lives in exactly one place. Also add reasoning-item emission from `reasoning_content` **only when** the caller supplies no opaque item (`requests.rs:246-294`).

### 5.2 `heterion-router-core`

**`src/chat.rs` — MOD**: `StreamChunk` (`chat.rs:156-167`) gains two `#[derive(Default)]`-compatible fields:
```rust
pub reasoning_encrypted: Option<String>,   // opaque pass-through only
pub usage_details: Option<UsageDetails>,
```
`UsageDetails { cached_tokens: u32, reasoning_tokens: u32, cache_creation_tokens: u32 }` in the same file. `ChatCompletionRequest` (`chat.rs:79-122`) is **unchanged** — no field is added; `extra` (`chat.rs:118-119`) and `responses_input` (`chat.rs:120-121`) already carry what the new code needs. All existing `StreamChunk` literals use `..Default::default()` or the constructors, so this is additive.

**`src/format.rs` — MOD**: add `pub struct RouteOrigin { pub backend: &'static str, pub provider: Option<String>, pub target_model: String, pub wire_format: Option<WireFormat> }`.

### 5.3 `heterion-router-gateway`

**`src/backend.rs` — MOD**
- `pub(crate) fn openai_request_body(request)` (`backend.rs:40-102`) → add `openai_request_body_for(request, policy)` with `ChatWirePolicy { allow_responses_only_keys: bool }`; when `false`, drop `store`, `include`, `reasoning`, `text`, `previous_response_id`, `prompt_cache_key`, `session_id`, `conversation_id` at `backend.rs:98-100`.
- `openai_chunk_to_stream_chunk` (`backend.rs:107-170`) — populate `usage_details` from `usage.output_tokens_details.reasoning_tokens` / `usage.input_tokens_details.cached_tokens` (`backend.rs:130-154`).
- `trait ChatBackend` (`backend.rs:21-34`) — **add with a default impl** so all 6 existing impls stay source-compatible:
  ```rust
  fn resolve_origin(&self, _model: &str) -> RouteOrigin { RouteOrigin::unknown() }
  ```

**`src/openai.rs` — MOD**: `ProviderOpenAiBackend::resolve_origin` returns the provider id and `Some(WireFormat::OpenAiResponses)` when `responses_models.contains(model)`. `resolve_responses_url` (`openai.rs:391-400`) gains a unit test for the four shapes in `openai.rs:628-635` **plus** a base URL that already ends in `/responses` and one ending in a non-`chat/completions` path.

**`src/responses.rs` — MOD**: delete the duplicated `response_to_completion` (`:216-312`) in favour of `translate::responses_request::response_to_completion` (shared with `grok_cli.rs:568-665`) — one implementation, one place that can drop reasoning. `drive_stream` (`:67-146`) gains the `DialectDetector` and uses `openai_to_responses_with_input`. Fix the hard-coded shared chat id at `:89-93`.

**`src/grok_cli.rs` — MOD**: same `DialectDetector` + shared converter; `ResponsesState::new("chatcmpl-rust", …)` at `:448-452` becomes `ids::completion_id()`.

**`src/routing.rs` — MOD**: `RoutingBackend::resolve_origin` (uses `plan()` at `routing.rs:156-191` + `registry.model_wire_format`) so the store can pin provider/connection/model. Not otherwise touched — 4xx breaker semantics (`routing.rs:44-55`) and pre-first-byte failover (`:287-299`) must not change.

**`src/response_store.rs` — NEW**
```rust
pub struct ResponseStore { db: Arc<Db>, crypto: Option<FieldCrypto>, policy: StorePolicy }
pub enum StoreMode { Off, StoreIfRequested, Store }
pub struct StorePolicy { pub mode: StoreMode, pub ttl_secs: i64, pub max_bytes: usize, pub max_records: usize }
impl ResponseStore {
    pub fn put(&self, owner: Option<&str>, req: &Value, resp: &Value, origin: &RouteOrigin) -> Result<String, StoreError>; // → minted id
    pub fn get(&self, owner: Option<&str>, id: &str) -> Result<Option<StoredRecord>, StoreError>;   // v2 id, then v1 upstream-id fallback
    pub fn delete(&self, owner: Option<&str>, id: &str) -> Result<bool, StoreError>;
    pub fn prune(&self) -> Result<usize, StoreError>;
}
```
Moves `RESPONSE_STORE_NAMESPACE` (`lib.rs:547`), `valid_response_id` (`:549-555`), `response_owner`/`response_visible` (`:557-571`), `persist_response` (`:573-598`), `load_response_record` (`:600-637`), `load_response` (`:639-644`) out of `lib.rs`. `lib.rs` keeps the HTTP handlers (`lib.rs:776-819`) thin.

**`src/lib.rs` — MOD**
- `responses` (`:825-915`): run `capability::check` after `RoutingBackend::resolve_origin` and **before** dispatch; on failure return 400 (currently the model allow-list check at `:850-852` runs too late for a capability error to be distinguishable from a policy error).
- `responses_to_chat` / `input_item_text` / `chat_to_responses_object` / `response_output_messages` / `append_input_items` → replaced by the extracted module (re-exported).
- `chat_to_responses_object` (`:1133-1182`): never return `raw` verbatim — merge `raw.output` with a gateway-owned `id`, a requested-`model` `model` field, `store`, and the full §4.3 field set.
- `responses_stream` (`:1185-1248`): replace `chat_chunk_to_responses_events` (`:1254-1298`) with `ResponsesStreamEmitter`; persist before the terminal event; emit `[DONE]` only when the flag is on; sanitise errors via one `sanitize_error(&GatewayError)` shared with `error_response` (`:538-545`) and `error_event` (`:493-495`).
- `persist_response` call site (`:879-881`) → `store.put(...)`, using the returned gateway id for `response_object["id"]`.

**`src/main.rs` — MOD**: `select_backend` (`:83-321`)
- replace the `target_format`-only filter at `:268-280` with `registry.model_wire_format(provider, model) == Some(WireFormat::OpenAiResponses)` so the provider-format fallback in `registry.rs:133-140` is actually used;
- canonicalize backend-map keys with `registry.canonical_id(...)` at `:237` and `:295` (closes the alias-unroutable gap for the Responses path; do **not** sweep the rest of routing in this mission);
- construct `ResponseStore` + `StorePolicy` from config and pass through `AppState` (`lib.rs:53-66`, new field `pub(crate) store: Option<Arc<ResponseStore>>`).

**`src/ids.rs` — MOD**: add
```rust
pub fn response_id() -> String { /* resp_<unix-hex>-<seq-hex>, same shape as completion_id() */ }
pub fn output_item_id(prefix: &str, response_id: &str, index: u32) -> String;
```

### 5.4 `heterion-router-db`

**`src/migrate.rs` — MOD**: append `Migration { version: "002", name: "response_store", sql: … }` to `MIGRATIONS` (`:32-36`). Append-only; the runner skips already-applied versions (`migrate.rs:122-126`).

**`src/repos.rs` — MOD**: `response_store_put`, `response_store_get_by_id`, `response_store_get_by_upstream_id`, `response_store_delete`, `response_store_prune_expired`, `response_store_prune_to_cap`, `response_store_count`. All parameterized (`rusqlite::params!`), matching the existing `kv_*` style (`repos.rs:746-774`).

### 5.5 `heterion-router-config`

**`src/config.rs` — MOD**: add the flag block to `GatewayConfig::from_map` (`:39-85`) using the existing `get2(primary, legacy)` helper (`:42-49`) so every flag also accepts `OMNIROUTE_*` for one cycle.

---

## 6. Data model

### 6.1 `RequestRequirements` (derived, never persisted)

```rust
struct RequestRequirements {
    opaque_reasoning_items: Vec<String>,   // ids of reasoning items we cannot express as chat messages
    wants_encrypted_reasoning: bool,       // client passed include:["reasoning.encrypted_content"] or store:false
    unsupported_tools:      Vec<UnsupportedTool>,
    builtin_web_search:     bool,
    streams: bool,
    store:   bool,                         // effective store decision after StoreMode
    pin_origin: bool,                      // previous_response_id present
}
```

### 6.2 `ResponseInput` (verbatim, lossless)

```rust
struct ResponseInput {
    raw:   Value,          // the client's `input`, untouched
    items: Vec<Value>,     // flattened array form, in order
}
```
Every item the chat pivot cannot express is preserved here and re-attached to the upstream `input` (`responses.rs:77-79` today does this only wholesale; `requests.rs::openai_to_responses_with_input` does it per-item so that an item we *can* express is still normalised and an item we *cannot* is never mangled).

### 6.3 `ResponseStore` record v2

```json
{
  "schema_version": 2,
  "id": "resp_68af4030-1f",
  "upstream_response_id": "ad5663da-63e6-86c6-e0be-ff15effa8357",
  "api_key_id": "k1",
  "model": "xai/grok-4.7",
  "origin": {"backend":"provider","provider":"xai","connection_ids":["c1"],"format":"openai-responses"},
  "store_mode": "store",
  "stored_at": 1756315696,
  "expires_at": 1758907696,
  "encrypted": true,
  "request":  { …client request, verbatim… },
  "response": { …assembled §4.3 response object… }
}
```
Stored columns mirror this; `request`+`response` live in the single `envelope` column so encryption is one `FieldCrypto::encrypt` call.

### 6.4 Identity rules

- **Response id:** gateway-minted `resp_<hex>_<hex>` (`ids::response_id()`). The upstream id is metadata, never a key. Rationale: **[EXT]** xAI's own example uses a bare UUID, which `valid_response_id` (`lib.rs:549-555`) would reject → `GET /v1/responses/{uuid}` currently 404s for every xAI response.
- **Output item ids:** gateway-minted, `msg_…` / `rs_…` / `fc_…`, stable across all events of that item and unique within the response. Upstream item ids are recorded as `upstream_item_id` when available. This is an accepted, documented divergence from byte-parity with a Responses upstream and is the same invariant the store needs.
- **Tool call ids (`call_id`):** passed through **unchanged** from the upstream. It is a client-visible correlation key; never mint it.
- **`tool_call_index`:** OpenAI Chat's per-choice tool index is a gateway-local allocation (`translate/src/responses.rs:169-172`); it must not be reused as the Responses `output_index`.

---

## 7. Wire fixtures (new)

All fixtures land in `crates/heterion-router-translate/tests/fixtures/`. Format follows the existing convention (`state` + ordered `cases` with `input`/`expected`), so the harness code in `tests/parity.rs` is reusable.

### 7.1 Fixture hygiene rules (hard requirements)

1. **Do not edit** `responses_to_openai.json`, `responses_tools.json`, `openai_to_responses.json`. They are the JS-OmniRoute contract (`translate/src/responses.rs:3-12`) and `AGENTS.md:16` calls snapshots "part of the contract; edit only together with the code". This plan does not change `translate::responses` or `requests::openai_to_responses` semantics, so they must keep passing **unchanged** — that is itself an acceptance criterion (§10, AC-7).
2. New fixtures live alongside; the harness `struct`s (`parity.rs:13-37`, `tools_parity.rs:13-30`) are generalised into a shared `tests/support/mod.rs` **only if** that does not require touching the three files above. Otherwise duplicate the small structs in the new test binaries.
3. Every fixture case carries `expected_events: [{sequence_number, type, payload}]` so ordering and `sequence_number` are asserted, not implied.

### 7.2 `responses_events_chat.json` — Chat pivot → Responses SSE

`state`: `{response_id:"resp_test_001", created_at:1756315696, model:"grok-4.7", instructions:"be brief", store:true}`.
`cases`: 7 chunks —
1. `{"content":"Hello"}` → `created`, `in_progress`, `output_item.added(msg)`, `content_part.added`, `output_text.delta("Hello")`
2. `{"content":" world"}` → `output_text.delta(" world")`
3. `{"reasoning":"step 1"}` → reasoning item `added`, `summary_part.added`, `summary_text.delta`
4. `{"reasoning_encrypted":"ENC-BLOB"}` (via the new field) → reasoning item `done` carrying `encrypted_content:"ENC-BLOB"` — **assert the ciphertext is present verbatim and never appears in any text field**
5. `{"tool_calls":[{"index":0,"id":"call_1","type":"function","function":{"name":"get_time","arguments":"{\"ci"}}]}` → function_call `added` + `function_call_arguments.delta`
6. `{"tool_calls":[{"index":1,"id":"call_2","type":"function","function":{"name":"now","arguments":"{}"}}]}` → second call at `output_index: 2` (proves parallel tool indices)
7. `{"finish_reason":"tool_calls","usage":{"prompt_tokens":32,"completion_tokens":9,"total_tokens":151},"usage_details":{"cached_tokens":8,"reasoning_tokens":110,"cache_creation_tokens":0}}` → each open item `.done`, then `response.completed` with the full object incl. `usage.output_tokens_details.reasoning_tokens = 110`.

Assertions: `sequence_number` is `0..N` with no gaps and no repeats; exactly one terminal; `assembled_response()["output"].len() == 3` (message, reasoning, two function calls ⇒ 4 — assert the exact count); every event has a `type`.

### 7.3 `responses_events_upstream_incremental.json` — OpenAI incremental dialect → Chat pivot

A full recorded OpenAI sequence: `response.created`, `response.in_progress`, `output_item.added(message)`, `content_part.added`, `output_text.delta`×2, `output_text.done`, `content_part.done`, `output_item.done`, `output_item.added(reasoning)`, `reasoning_summary_part.added`, `reasoning_summary_text.delta`, `…_done`, `…part.done`, `output_item.added(function_call)`, `function_call_arguments.delta`×2, `function_call_arguments.done`, `output_item.done`, `response.completed`.
Expected: the same assembled response as feeding the equivalent `StreamChunk` list — proving dialect-agnostic convergence. Add a negative case: a foreign event type (`response.audio.delta`) must be ignored, never fabricated.

### 7.4 `responses_events_upstream_snapshot.json` — xAI whole-call dialect

One event:
```json
{"type":"response.completed","response":{
  "id":"ad5663da-63e6-86c6-e0be-ff15effa8357","status":"completed","model":"grok-4.7",
  "output":[
    {"type":"reasoning","id":"","summary":[{"type":"summary_text","text":"…"}],"status":"completed"},
    {"type":"message","id":"msg_…","role":"assistant","status":"completed",
     "content":[{"type":"output_text","text":"…","annotations":[]}]},
    {"type":"function_call","id":"fc_1","call_id":"call_1","name":"get_time","arguments":"{\"city\":\"SF\"}","status":"completed"},
    {"type":"function_call","id":"fc_2","call_id":"call_2","name":"now","arguments":"{}","status":"completed"}],
  "usage":{"input_tokens":32,"input_tokens_details":{"cached_tokens":8},"output_tokens":9,
           "output_tokens_details":{"reasoning_tokens":110},"total_tokens":151}}}
```
Expected: **two** function calls with full arguments, `finish_reason` → `tool_calls` (today: dropped and reported `stop` — `docs/research/PROJECT-AUDIT-2026-09.md:90-92`), reasoning summary present, usage details intact.

### 7.5 `responses_request_reasoning.json` — inbound reasoning items

Input array carrying: `{type:"message",…}`, `{type:"reasoning", id:"rs_1", summary:[…], encrypted_content:"ENC-…"}`, `{type:"function_call"…}`, `{type:"function_call_output"…}`.
Expected: `ChatCompletionRequest.messages` contains the message/function_call/function_call_output roles **in order**; `ResponseInput.items` has all four **verbatim**; `requirements.opaque_reasoning_items == ["rs_1"]`; `requirements.wants_encrypted_reasoning == true`.
Negative cases: input consisting **only** of a reasoning item must not 400 with `"input produced no messages"` (`lib.rs:1024-1026` today); `summary` given as a bare string must be tolerated but flagged, not crash.

### 7.6 `responses_request_builtin_tools.json` — tool classification

Cases → expected `CapabilityError::UnsupportedTools([{kind:"file_search"},{kind:"mcp"}])`; a `web_search` tool with `provider = "xai"` classifies as supported; `web_search` with `provider = None` classifies as unsupported. Error body asserted byte-for-byte including `"code":"unsupported_capability"`.

### 7.7 `gateway/tests/fixtures/responses_store_v1.json` — v1 read compatibility

A literal v1 envelope (shape from `lib.rs:583-591`) with `api_key_id:"k1"`. Asserted: `ResponseStore::get("k1", <upstream-id>)` returns the record; `get` with a different owner returns `None`; `get` with the **gateway** id returns `None` (v2 ids never alias v1 keys); `prune()` never deletes it while in v2 mode; `StoreMode::Off` makes `get` return `None` for both namespaces.

---

## 8. Test plan

### 8.1 Unit (in-module, `#[cfg(test)]`)

| Test | File | Asserts |
|---|---|---|
| `sequence_numbers_are_dense_and_monotonic` | `responses_events.rs` | 0..N, no gaps/dupes |
| `exactly_one_terminal_event` | `responses_events.rs` | second `finish()` is a no-op + debug assert |
| `terminal_response_carries_every_sdk_field` | `responses_events.rs` | §4.3 field set |
| `encrypted_content_is_opaque` | `responses_events.rs` | ciphertext never appears under a text key; never logged (capture via a `tracing` subscriber) |
| `usage_details_survive_to_the_response_object` | `responses_events.rs` + `backend.rs` | reasoning_tokens/cached_tokens land in `usage.*_details` (closes `backend.rs:130-154` → `lib.rs:1288-1292`) |
| `dialect_detector_folds_snapshot_tool_calls` | `upstream.rs` | §7.4 expectations |
| `dialect_detector_does_not_double_emit` | `upstream.rs` | snapshot after incremental `.added` emits nothing extra |
| `reasoning_items_survive_decode` | `responses_request.rs` | §7.5 |
| `summary_array_is_walked` | `responses_request.rs` | `summary[*].text` (regression for `lib.rs:683-685`) |
| `reasoning_only_input_is_accepted` | `responses_request.rs` | no `input produced no messages` |
| `builtin_tools_are_classified_not_dropped` | `capability.rs` | §7.6 |
| `chat_wire_policy_strips_responses_keys` | `backend.rs` | `openai_request_body_for(.., strict)` has no `store`/`include`/`reasoning`/`text`/`previous_response_id` (regression for `backend.rs:98-100`) |
| `resolve_responses_url_shapes` | `openai.rs` | 4 base-URL shapes incl. an already-`/responses` one |
| `migration_002_is_idempotent` | `db/src/migrate.rs` | mirror `fresh_db_applies_001_and_is_idempotent` (`migrate.rs:151`) |
| `prune_removes_expired_then_oldest` | `response_store.rs` | TTL first, then LRU cap |

### 8.2 Integration (wiremock upstream → `ChatBackend`)

- `snapshot_tool_call_is_not_dropped` — `ResponsesBackend` + `wiremock` SSE with one `response.completed` snapshot carrying two `function_call` items ⇒ `finish_reason == "tool_calls"`, both names+arguments intact (`responses.rs`).
- `grok_snapshot_tool_call_is_not_dropped` — same for `GrokCliBackend` (its SSE test harness is `grok_cli.rs:834-896`).
- `stream_ids_are_unique_per_request` — two concurrent streams must not both emit `chatcmpl-rust` (regression for `responses.rs:89-93` and `grok_cli.rs:448-452`).
- `sse_error_frame_is_sanitized` — upstream 500 with body containing `sk-live-SECRET`; assert the SSE frame contains neither the secret nor the raw message, and that the log does contain it at `warn!` (regression for `lib.rs:493-495`, `:1211`).
- `store_false_streaming_is_not_persisted` and `store_true_streaming_is_retrievable` — stream a Responses request, then `GET /v1/responses/{id}` (regression for `lib.rs:877-881`).
- `previous_response_id_replays_reasoning_items` — `store:false` two-turn replay keeps `encrypted_content` verbatim in the upstream body (mirrors ROADMAP P0 item 1, `docs/ROADMAP-2026-09.md:71-73`).

### 8.3 HTTP contract (axum router, `tower::ServiceExt::oneshot`)

- `responses_stream_emits_exactly_one_terminal_event` — assert `sequence_number` density **and** that `response.completed|failed|incomplete` appears exactly once across the whole body.
- `responses_stream_event_lines_match_payload_types` — every `event:` equals `data.type`; no empty `event:` (regression for `lib.rs:1210-1242`).
- `responses_stream_persists_before_terminal` — the id in `response.created` resolves via `GET` even for a client that reads only the first frame.
- `unsupported_builtin_tool_returns_400` — body equals the §4.1 error contract.
- `reasoning_item_to_chat_upstream_returns_400` (default) and `_is_stripped_with_warn` (flag on).
- `store_prunes_on_boot` — 20 expired rows, one live row; after `ResponseStore::prune()` exactly one remains.
- `v1_record_is_readable` — §7.7.
- `cross_origin_replay_is_rejected` — stored origin `xai`, replay routed to `openai` ⇒ 400 `previous_response_origin_mismatch` when `RESPONSES_PIN_ORIGIN=error`.

### 8.4 Contract (offline, vendored wire captures)

`crates/heterion-router-gateway/tests/responses_contract.rs`, fixtures under `tests/fixtures/upstream/`, each a real captured upstream body recorded once and reviewed:
- `openai_responses_text_stream.sse`, `openai_responses_tool_stream.sse`, `openai_responses_reasoning_stream.sse`
- `xai_responses_snapshot.json`, `xai_responses_incremental.sse`

Each is replayed through the full `/v1/responses` path and validated by a `responses_lifecycle_validator` that asserts: first event is `response.created`; `sequence_number` strictly increasing; every `output_index`/`item_id` pair is opened before used and closed exactly once; exactly one terminal; assembled `response.output` equals the concatenation of closed items. This validator is the deliverable the project asked for at `docs/ROADMAP-2026-09.md:74-76`.

**No live conformance run is claimed.** The captures must be produced and reviewed before the fixture is committed; until then the contract tests are marked `#[ignore]` with a comment naming what is missing — the project has an explicit history of not claiming live conformance (`STATUS.md:29-31`, `docs/ROADMAP-2026-09.md:33-35`).

### 8.5 Gates

```sh
cargo fmt --all && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace
npm --prefix desktop/ui ci && npm --prefix desktop/ui run build
cargo check --manifest-path desktop/src-tauri/Cargo.toml --locked
```
(`AGENTS.md:18-27`, `STATUS.md:88-93`.) Tauri is excluded from the workspace (`Cargo.toml:20`) so it needs its own check.

---

## 9. Rollout, flags, rollback

### 9.1 Flag matrix

All read once at boot in `GatewayConfig::from_map` (`config/src/config.rs:39-85`), each with a `OMNIROUTE_*` alias for one cycle (`config.rs:42-49`, `AGENTS.md:33`).

| Env var | Ship default | Promote to | Rollback |
|---|---|---|---|
| `HETERION_ROUTER_RESPONSES_EVENTS` | `legacy` | `sdk` | set back to `legacy` |
| `HETERION_ROUTER_RESPONSES_DONE_SENTINEL` | `0` | `0` (per-dialect later) | n/a (additive) |
| `HETERION_ROUTER_RESPONSES_REASONING` | `error` | `error` | set `strip` |
| `HETERION_ROUTER_RESPONSES_TOOLS` | `strict` | `strict` | set `legacy` |
| `HETERION_ROUTER_RESPONSES_STORE` | `store` | `store-if-requested` | set `store` |
| `HETERION_ROUTER_RESPONSES_TTL_SECS` | `2592000` | same | raise/lower freely |
| `HETERION_ROUTER_RESPONSES_MAX_BYTES` | `1048576` | same | raise freely |
| `HETERION_ROUTER_RESPONSES_MAX_RECORDS` | `10000` | same | raise freely |
| `HETERION_ROUTER_RESPONSES_PIN_ORIGIN` | `warn` | `error` | set `warn` |
| `HETERION_ROUTER_RESPONSES_CHAT_PASSTHROUGH` | `legacy` | `strict` | set `legacy` |
| `HETERION_ROUTER_UPSTREAM_DIALECT` | `auto` | `auto` | force `openai-incremental` |
| `HETERION_ROUTER_SSE_ERROR_SANITIZE` | `1` | `1` | `0` (kill switch only; never ship `0`) |

### 9.2 Sequence

1. **PR 1 — pure, no behaviour change.** Extract `responses_request`, `capability`, `responses_events`, `upstream` into `heterion-router-translate`; move `response_store` helpers; add migration `002` + repos; add the flag block with **every default preserving today's behaviour**; wire `RESPONSES_EVENTS=legacy` as the default code path. All 213 existing tests must pass untouched. *Rollback: revert the commit; no data written yet.*
2. **PR 2 — SSE error sanitisation**, on by default, kill switch kept. Lowest risk, closes a High finding (`PROJECT-AUDIT-2026-09.md:121`). Canary on the launchd service, watch `gateway.log` for the new `responses_stream_error` warn lines.
3. **PR 3 — state store v2** (gateway ids, TTL, caps, encryption, v1 read-fallback). Default `RESPONSES_STORE=store` so live behaviour matches today except that **ids change format**. Existing v1 rows remain readable through the fallback; nothing is deleted. Add `key_value` row count and `response_store` row count to `/api/overview` before enabling. *Rollback: `RESPONSES_STORE=off` stops writes; v1 rows are still served.*
4. **PR 4 — SDK lifecycle** behind `RESPONSES_EVENTS=legacy`. Soak `legacy` for one full agent-loop cycle (Codex-style clients are the consumers). Flip to `sdk` per-key first if a key-scoped override is cheap, otherwise globally. *Rollback: set `legacy` and restart; no stored data is affected because the v2 record is dialect-independent.*
5. **PR 5 — reasoning preservation + strict tools.** Default-fail-closed turns previously-silent 200s into 400s; expect client breakage and announce it in `CHANGELOG.md`. Keep `RESPONSES_REASONING=strip` and `RESPONSES_TOOLS=legacy` available for one cycle.
6. **PR 6 — streaming persistence + origin pinning**, then promote `PIN_ORIGIN` to `error` and `CHAT_PASSTHROUGH` to `strict` after one cycle.

### 9.3 Rollback guarantees

- Every behaviour change is a **read-at-boot flag**; no DB migration is ever required to roll back.
- Migration `002` is additive-only (`CREATE TABLE IF NOT EXISTS`, indexes, new table). The v2 table is never referenced by v1 code paths other than read-fallback. Dropping it is optional and not part of rollback.
- The v1 → v2 record change is a **new namespace/table**, so the worst case is "the client's `previous_response_id` chain restarts once", never "the store is unreadable".
- `RESPONSES_EVENTS=legacy` restores byte-identical SSE output to today (`chat_chunk_to_responses_events`, `lib.rs:1254-1298`) because that function is kept, not deleted, and is reachable for the whole deprecation window.
- 4xx breaker semantics (`routing.rs:44-55`, `AGENTS.md:38`), keyless-no-bearer (`openai.rs:728-777`), fail-closed auth (`auth.rs:109-114`) and non-loopback-without-key startup (`main.rs:370-384`) are **untouched** by this plan and are asserted by their existing tests.

### 9.4 Observability

No metrics crate is present (gateway `Cargo.toml:8-32`), so **do not add one**. Emit structured `tracing` lines and let the existing `/api/logs/app` (`admin.rs:511`, routed `lib.rs:155`) surface them:

| Event | Level | Fields |
|---|---|---|
| `responses_capability_rejected` | `info` | `model`, `reason`, `tool_kinds` |
| `responses_reasoning_stripped` | `warn` | `model`, `item_ids`, `upstream_provider` |
| `responses_stream_terminal` | `info` | `response_id`, `event`, `event_count`, `sequence_last` |
| `responses_stream_error` | `warn` | `response_id`, `sanitized_code`, `raw_len` (**never** `raw`) |
| `responses_store_write` | `info` | `id`, `bytes`, `encrypted`, `expires_at` |
| `responses_store_rejected` | `warn` | `bytes`, `limit` |
| `responses_store_pruned` | `info` | `expired`, `over_cap` |
| `responses_dialect_detected` | `debug` | `backend`, `dialect` |

---

## 10. Risks and mitigations

| Risk | Mitigation |
|---|---|
| Emitting `response.created` breaks a client that today starts reading at the first delta | `RESPONSES_EVENTS=legacy` default; soak before promote. The lifecycle is what SDKs expect **[EXT]**; a client that breaks on it was relying on an undocumented shape. |
| Tightening `/v1/responses/{id}` ownership to `response_id` breaks clients that replay the *upstream* id | `ResponseStore::get` keeps the v1 upstream-id fallback for the whole deprecation window; `previous_response_id` accepts both. |
| `MAX_BYTES` rejection breaks a client sending a huge transcript | Default 1 MiB is far above a normal agent turn; the error is explicit and the flag is adjustable without a rebuild. |
| `responses_to_chat` extraction touches 8 in-crate tests | Re-export shims in `lib.rs`; `rg` for every caller before the move. |
| Two `response_to_completion` copies exist (`responses.rs:216-312`, `grok_cli.rs:568-665`) and may have drifted | They are currently identical; deduplicating is part of PR 1 and the shared version is what adds reasoning extraction, so the dedupe and the feature land together. |
| Capability rejection at the wrong point in the pipeline | `check` runs after `resolve_origin` (needs the resolved provider) and **before** `allows_model`, so an operator reading logs sees capability vs policy separately. |
| `sequence_number` gaps if an event is suppressed mid-emission | `ResponsesEvent` is only constructible through the emitter, which owns the counter; the validator test asserts density. |
| We invent OpenAI behaviour from memory | Every external fact in §3 is quoted from vendor docs with a URL in `docs/ROADMAP-2026-09.md:127-155`. Anything not in §3 is treated as unknown and left unimplemented rather than guessed — same discipline as `docs/ROADMAP-2026-09.md:104-110`. |

---

## 11. Acceptance criteria

**AC-1 — No silent drop of agent-loop-affecting input.** A `/v1/responses` request whose `input` contains `reasoning` items (with or without `encrypted_content`) is either (a) forwarded to a Responses-format upstream with every item byte-identical, or (b) rejected `400 {"code":"unsupported_capability"}`. A test drives a `store:false` two-turn replay and asserts the `encrypted_content` blob appears verbatim in the upstream body and never in any visible-text field, log line, or usage row. *(closes ROADMAP P0 item 1, `docs/ROADMAP-2026-09.md:71-73`)*

**AC-2 — Reasoning summary replay is array-correct.** A stored response whose `output` contains `{"type":"reasoning","summary":[{"type":"summary_text","text":"…"}]}` replays the summary text. The current string-only read (`lib.rs:683-685`) is deleted and a fixture pins the array form. A malformed `summary` (bare string, wrong types) is tolerated without a panic.

**AC-3 — Built-in tools are never silently dropped.** Each of `file_search`, `computer_use_preview`, `code_interpreter`, `image_generation`, `mcp`, `local_shell`, and an unknown `type` returns `400` with `code:"unsupported_capability"` and the offending `type` named, unless `RESPONSES_TOOLS=legacy`. `web_search` is accepted only for `xai`/`xai-oauth`. *(closes ROADMAP P0 item 3)*

**AC-4 — SDK-shaped lifecycle.** A full `/v1/responses` SSE response satisfies all of: first event is `response.created`; every event carries a `type` and a `sequence_number`; `sequence_number` starts at 0 and increases by exactly 1 with no gaps; every `event:` line equals its `data.type`; no event has an empty `event:` name; each `output_index`/`item_id` is opened by `output_item.added` before use and closed exactly once by `output_item.done`; text goes `content_part.added → output_text.delta* → output_text.done → content_part.done`; tool arguments go `output_item.added → function_call_arguments.delta* → function_call_arguments.done → output_item.done`; exactly one terminal event; the terminal `response` object deserialises with every field in §4.3. Enforced by `responses_lifecycle_validator` over the vendored §8.4 captures **and** by a wiremock-backed HTTP test.

**AC-5 — Both upstream dialects produce identical downstream results.** Feeding the §7.3 incremental capture and the §7.4 xAI whole-call snapshot produces byte-identical assembled `response.output` and identical `finish_reason`. A snapshot-only tool call is never reported as `stop`. *(closes `PROJECT-AUDIT-2026-09.md:90-92` and `STATUS.md:37`)*

**AC-6 — No invented behaviour on unknown events.** An unrecognised event type is ignored, never fabricated into a synthetic event, and the count of unrecognised types is logged at `debug`. A fixture includes `response.audio.delta` and asserts it produces no output.

**AC-7 — JS parity fixtures are untouched and still green.** `responses_to_openai.json` (8 cases), `responses_tools.json` (5 cases, `tools_parity.rs:40`) and `openai_to_responses.json` (4 cases, `request_parity.rs:30`) are byte-unchanged, and `tests/parity.rs`, `tests/tools_parity.rs`, `tests/request_parity.rs` pass without modification. The workspace test count is **≥ 213** after the change, with 0 failures.

**AC-8 — Store is bounded, keyed by the gateway, and encrypted when a key exists.** `response_store` has TTL, `MAX_RECORDS` and `MAX_BYTES` enforcement tested at the boundaries; the primary key is a gateway-minted `resp_…`; a v1 record is still readable by its upstream id; `get` with a non-matching owner returns `None`; prune never touches a non-expired row; `encrypted == 1` iff `STORAGE_ENCRYPTION_KEY` produced a key. *(closes `PROJECT-AUDIT-2026-09.md:122` and ROADMAP item 5)*

**AC-9 — Streams are storable and retrievable.** `store:true` + `stream:true` yields an id that `GET /v1/responses/{id}` resolves, and the record exists before the terminal event is written to the socket. `store:false` yields a `404` on `GET`, preserving `store_false_does_not_create_retrievable_state` (`lib.rs:2006-2035`).

**AC-10 — Streaming error frames are sanitized.** The SSE body of a failed `/v1/responses` stream and of a failed `/v1/chat/completions` stream contain neither the upstream message nor any substring of the upstream body; both match the same sanitized shape as `error_response` (`lib.rs:538-545`); the raw text appears only in a `warn!` log line. A fixture contains a canary secret and asserts it is absent from the response bytes. *(closes ROADMAP P0 item 4)*

**AC-11 — Responses-only keys do not reach Chat bodies.** Under `RESPONSES_CHAT_PASSTHROUGH=strict`, a `provider/model` request that routes to a Chat-format upstream contains none of `store`, `include`, `reasoning`, `text`, `previous_response_id`, `prompt_cache_key`, `session_id`, `conversation_id` in the upstream body. *(closes `PROJECT-AUDIT-2026-09.md:93-95`)*

**AC-12 — Usage details survive the whole path.** On a Responses-format upstream that reports `output_tokens_details.reasoning_tokens`, `/v1/responses` streaming delivers `response.completed.response.usage.output_tokens_details.reasoning_tokens` with the same value, and the non-stream path delivers it too. *(closes the double loss at `backend.rs:130-154` + `lib.rs:1288-1292`)*

**AC-13 — Continuation origin is explicit.** Replaying a stored response against a different resolved provider is either rejected `400` or logged at `warn!` with both provider ids — never silently routed to a different provider or credential. Under `PIN_ORIGIN=error` it is rejected.

**AC-14 — Per-model Responses dispatch honours provider-format fallback.** For a registry provider whose `format` is `openai-responses` and whose models declare `target_format: null`, the model resolves to the Responses transport (`main.rs:268-280` replaced by `registry.model_wire_format`). `resolve_responses_url` is unit-tested for base URLs that already end in `/responses` and for base URLs whose path is neither `chat/completions` nor empty.

**AC-15 — Unchanged invariants.** `cargo fmt --all`, `cargo clippy --workspace --all-targets -- -D warnings` and `cargo test --workspace` are green (≥213 tests, 0 failures); `npm --prefix desktop/ui run build` and `cargo check --manifest-path desktop/src-tauri/Cargo.toml --locked` are green. 4xx does not trip the breaker; keyless providers send no `Authorization`; auth is fail-closed; non-loopback bind without a key refuses startup.

**AC-16 — Honest documentation.** `STATUS.md` and `docs/ROADMAP-2026-09.md` are updated in the same PR that flips a default from `legacy` to the new behaviour, stating exactly which wire fixtures are vendored captures and that **no live conformance run against OpenAI or xAI has been performed** (matching the discipline at `STATUS.md:29-31` and `docs/ROADMAP-2026-09.md:122-125`).

---

## 12. Explicitly out of scope

Not implemented by this plan, and not to be smuggled in: WebSocket transport; hosted MCP execution; `file_search` / `code_interpreter` / `computer_use` / `image_generation` execution; the Conversations API; `/v1/responses/compact` **[EXT]**; `logprobs`; URL-citation annotations; `background:true`; a native Anthropic Messages backend (`docs/ROADMAP-2026-09.md:47`, `:116-120`).

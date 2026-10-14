# 03 — Providers, Routing & Observability

**Author:** principal architect 3/6
**Date:** 2026-09-29
**Canonical repo:** `~/ai-workstation/Projects/heterion-router` (read-only; nothing in it was modified, staged or committed)
**Deliverable:** this file only, under `/var/folders/.../T/opencode/heterion-mission/`
**Audit inputs:** working tree at `51dfe6d` + **dirty overlay** (32 modified tracked files, 5 untracked docs). Every finding below describes the *working tree*, matching the caveat in `docs/research/PROJECT-AUDIT-2026-09.md:9-10`.

---

## 0. Method and evidence rules

1. **Registry metrics were recomputed from `crates/heterion-router-providers/registry.json`**, not copied from the audit. Reproduction script in §1.4.
2. **Backend construction was re-implemented in a simulator** and reconciled against the live process' own `gateway.log` "backend available" lines and `/healthz`. §2.4.
3. **Live SQLite was inspected via a read-only copy** including `-wal`/`-shm`. This detail matters: `~/.heterion-router/storage.sqlite` has mtime **Sep 22 20:29** but a **1.6 MB `-wal` with mtime Sep 29 12:59**. Copying only the main file yields a snapshot that is *a week stale* and silently under-reports the active provider set (30 vs 31 distinct providers, 18 vs 19 backends). Any future auditor must `cp storage.sqlite storage.sqlite-wal storage.sqlite-shm` together or use `sqlite3 .backup`. §6.1.
4. Secrets were not read. `.env` was enumerated **by variable name only** (`STORAGE_ENCRYPTION_KEY`, `RATE_LIMIT_MAX_WAIT_MS`).
5. Where this document contradicts the audit, it says so explicitly and gives `file:line`.

---

## 1. Provider registry

### 1.1 Structure

`registry.json` is embedded at compile time (`registry.rs:63-67`) and parsed into `ProviderEntry { id, alias, format, executor, auth_type, base_url, models }` (`registry.rs:19-30`). Lookup is by `HashMap<String, usize>` where aliases are inserted only if the key is free (`registry.rs:78-82`), so a real id always beats a colliding alias — enforced by test `a_real_id_wins_over_a_colliding_alias` (`registry.rs:199-224`).

**Stale doc comment:** `heterion-router-providers/src/lib.rs:3` says "The JavaScript `REGISTRY` (277 providers)". The file has 278. Fix in passing; it is the kind of drift that produced the "277 vs 278" discrepancy in the lib crate header.

### 1.2 Independent metric verification — audit **confirmed exactly**

| Metric | Audit (`PROJECT-AUDIT-2026-09.md:22-25`) | My recomputation | Verdict |
|---|---|---|---|
| Providers | 278 | 278 | ✅ |
| Model entries | 2717 | 2717 | ✅ |
| Pass builder gate (format ∈ {openai, openai-responses, gemini} ∧ non-empty `base_url`) | 259 / 2108 | 259 / 2108 | ✅ |
| …of which HTTP(S) | 253 / 1916 | 253 / 1916 | ✅ |
| …non-HTTP scheme (`wss://`, `://cli/stdio`, `://acp/stdio`, `://app-server/stdio`, `://cli/websocket`) | 6 / 192 | 6 / 192 | ✅ |
| Fail gate | 19 / 609 | 19 / 609 | ✅ |
| HTTP-capable providers declaring a **bespoke** executor (silently served by the generic executor) | 61 / 959 | 61 / 959 | ✅ |
| `claude`-format providers | 10 | 10 | ✅ |

**Addition, not in the audit:** the committed `HEAD` registry is **278 providers / 2715 models**; the working tree adds 2 model entries and 0 providers (`git diff --stat HEAD -- crates/heterion-router-providers/registry.json` → `2 insertions`). Any metric quoted as "the registry" must say *which tree*. The `provider_id_list_snapshot` insta test (`registry.rs:341-345`) snapshots ids only, so the +2 model delta is invisible to it.

### 1.3 The two structural gaps, enumerated

**Fail-gate (19 providers / 609 models) — the exact list:**

| provider | format | base_url | models |
|---|---|---|---|
| `agentrouter` | claude | `https://agentrouter.org/v1/messages` | 3 |
| `anthropic` | claude | `https://api.anthropic.com/v1/messages` | 11 |
| `bailian-coding-plan` | claude | `https://…maas.aliyuncs.com/apps/anthropic/v1` | 6 |
| `claude` | claude | `https://api.anthropic.com/v1/messages` | 11 |
| `devin-cli-agentic` | claude | `devin://acp/stdio` | 110 |
| `kimi-coding` | claude | `https://api.kimi.com/coding/v1/messages?beta=true` | 3 |
| `kimi-coding-apikey` | claude | same | 3 |
| `tabitoken` | claude | `https://tabitoken.com/v1/messages` | 4 |
| `wafer` | claude | `https://pass.wafer.ai/v1/messages` | 4 |
| `zai` | claude | `https://api.z.ai/api/anthropic/v1` | 8 |
| `agy`, `antigravity` | antigravity | `null` | 10 + 10 |
| `bedrock`, `muse-code` | **openai** | `null` | 7 + 8 |
| `cursor`, `cursor-api` | cursor | `https://api2.cursor.sh` | 198 + 198 |
| `clova-studio` | clova | `https://clovastudio.stream…/chat-completions` | 3 |
| `deepai` | custom | `https://api.deepai.org` | 1 |
| `kiro` | kiro | `https://codewhisperer…` | 11 |

Two facts the audit does not call out and that change the shape of the fix:

- **`bedrock` and `muse-code` are `format: "openai"` with `base_url: null`.** They fail *only* on the URL check. They are therefore the cheapest legitimate additions later (one URL each) — but adding them is a provider expansion, which §7.7 puts **out of scope**.
- **`zai` (8 models) has 2 models with `target_format: "openai"`** while the provider format is `claude`. `model_wire_format("zai", <those two>)` returns `Some(WireFormat::OpenAi)` (`registry.rs:133-146`) even though `main.rs:175-179` gates the whole provider on `entry.format == "gemini"` and `main.rs:244` drops anything that is not `openai`. The registry's per-model view and the builder's per-provider view disagree; the audit flags this for `zai` but not as a *general* per-model/per-provider inconsistency.

**Non-HTTP (6 / 192):** `auggie` (`auggie://cli/stdio`, 28), `codex-app-server` (`codex-app-server://cli/websocket`, 33), `devin-cli` (`devin://acp/stdio`, 110), `zcode` (`zcode://app-server/stdio`, 14), `copilot-web` (`wss://copilot.microsoft.com/…`, 3), `copilot-m365-web` (`wss://substrate.office.com/…`, 4). These *pass* the gate and are constructed into a `ProviderOpenAiBackend` whose `chat_url` is `wss://…/chat/completions` — a URL `reqwest` cannot POST to. They fail at request time with a transport error, which `is_backend_outage` (`routing.rs:44-55`) classifies as an outage and therefore **trips the breaker**. The gate is scheme-blind; `main.rs` never inspects the scheme.

**Bespoke-executor gap (61 / 959):** e.g. `glm` (3), `opencode` (3), `vertex` (2), `moonshot` (2), `xai`, `xai-oauth`, `github`, `ghe-copilot`, `grok-web`, `kilocode`, `kilo-gateway`… The generic executor answers them with a plain Bearer + Chat Completions. The registry declares an intent the runtime does not implement, and nothing anywhere reports the divergence.

**Derived-Responses URL exposure:** 14 providers / 150 models carry `target_format: "openai-responses"`, and for all of them `main.rs:281-287` builds a `ResponsesBackend` at `resolve_responses_url(&base_url)` (`openai.rs:391-400`), a pure string transform. The list includes `github`/`ghe-copilot` (`https://api.githubcopilot.com/chat/completions` → `…/responses`), `opencode-go`, `opencode-zen`, `cheaperinference`, `muse-code`. None of these endpoints is verified against an upstream; the audit says this in prose, the code has no allowlist, no probe and no flag.

### 1.4 Reproduction script (read-only)

```sh
cd ~/ai-workstation/Projects/heterion-router
python3 - <<'PY'
import json, collections
d=json.load(open('crates/heterion-router-providers/registry.json'))
M=lambda e: e.get('models') or []
sch=lambda u: u.split(':',1)[0].lower() if ':' in u else None
S={'openai','openai-responses','gemini'}
g1=[e for e in d if e.get('format') in S and (e.get('base_url') or '')]
g2=[e for e in g1 if sch(e['base_url']) in ('http','https')]
g3=[e for e in g1 if e not in g2]
g4=[e for e in d if e not in g1]
for n,g in (('pass gate',g1),('http(s)',g2),('non-http',g3),('fail gate',g4)):
    print(f"{n:12} {len(g):4} providers {sum(len(M(e)) for e in g):5} models")
print("bespoke among http:", sum(1 for e in g2 if e.get('executor')!='default'),
      sum(len(M(e)) for e in g2 if e.get('executor')!='default'))
print("responses models:", sum(1 for e in d for m in M(e) if m.get('target_format')=='openai-responses'))
PY
```

---

## 2. Backend construction (`main.rs::select_backend`)

### 2.1 The current chain

`select_backend` (`main.rs:83-321`) builds a `HashMap<String, Arc<dyn ChatBackend>>` in five stages:

| # | stage | lines | gate |
|---|---|---|---|
| 0 | `echo` (opt-in env) | `main.rs:88-95` | `HETERION_ROUTER_ENABLE_ECHO`/`OMNIROUTE_ENABLE_ECHO` ∈ {1,true,yes} |
| 1 | `grok-cli` (dedicated) | `main.rs:98-135` | `grok_credentials()` non-empty |
| 2 | `openai-compatible` (env) | `main.rs:137-148` | `OPENAI_COMPAT_BASE_URL` set |
| 3 | per-provider from DB | `main.rs:161-299` | see below |
| 4 | route rules + defaults | `main.rs:301-319` | — |

The per-provider gate is a **string-comparison cascade duplicated inline**:

```rust
// main.rs:175-179   gemini?
// main.rs:209-213  openai-responses?
// main.rs:244-246  else must be openai, else `continue`
// main.rs:257-259  no tokens && !allows_keyless -> `continue`
// main.rs:168-172  no base_url -> `continue`
```

Four `continue`s with **no reason recorded anywhere** except two of them (`main.rs:203, 239, 297` log `warn!`, `main.rs:172` and `main.rs:190/223/258` are **silent**). The same predicate is re-implemented a third time in `combos::dispatch_backend` (`combos.rs:234-249`) and a fourth, differently, in `registry::is_openai_format` (`registry.rs:149-153`). Four copies, three different answers for `zai`.

### 2.2 Defects found in construction

**D1 — Backends are keyed by the *stored* connection string, not the canonical id.**
`main.rs:295` `backends.insert(provider, …)` where `provider` comes from `active_providers()` (`credentials.rs:112-119`), i.e. verbatim `provider_connections.provider`. If a row is stored under an alias, the backend key is the alias.

**D2 — The `heterion-local` override is matched on the un-canonicalized string.**
`main.rs:75` `if provider == "heterion-local"` and `main.rs:165` `.then(|| …).flatten()`. A row stored as `heterion` (the alias, `registry.rs` `heterion-local.alias == "heterion"`) gets the registry default `http://127.0.0.1:8080/v1` and ignores `HETERION_LOCAL_BASE_URL` — the exact scenario the override exists for.

**D3 — `grok-cli` can fall through to the generic executor.** Audit gap #5 is real but *narrower* than stated. `grok_credentials` (`main.rs:28-56`) and the loop both read the same rows through the same `load_credentials(conn, "grok-cli", …)`, so a plain "pool empty" state yields `tokens.is_empty() && !allows_keyless` at `main.rs:257-259` and is skipped — the generic backend is *not* built. The reachable path is a row stored under the alias **`gc`**: `grok_credentials` finds nothing (queries `"grok-cli"`), `active_providers` yields `"gc"`, `load_credentials(conn, "gc", …)` returns tokens, `is_openai_format("gc")` is `true`, and the code builds a `ProviderOpenAiBackend` pointing at the private proxy URL `https://cli-chat-proxy.grok.com/v1/chat/completions` **with no Grok session headers** → guaranteed `426`. Fix is a one-line `if canonical == "grok-cli" { continue; }` guard before the generic branch (§7.1).

**D4 — No scheme check.** See §1.3. `wss://` providers are constructed and will trip the breaker on every call.

**D5 — The registry is parsed twice at startup.** `ProviderRegistry::load()` at `main.rs:151` and again at `main.rs:316`; the first `Arc` is dropped and re-parsed from the 1.4 MB embedded JSON. Trivial, but it means `main.rs:319` and `main.rs:429-431` receive an object that is not the one the builder consulted.

**D6 — `contains_key` shadowing is silent.** `main.rs:162` skips a DB provider literally named `echo` or `openai-compatible` with no log line.

**D7 — Construction is untestable.** `select_backend` lives in a `[[bin]]` (`Cargo.toml` `[[bin]] name = "heterion-router-gateway"`). Its 5 unit tests (`main.rs:445-543`) cover `provider_base_url_from`, `validate_auth_policy` and `parent_gone` — **nothing covers the construction loop**. This is why the audit's P2-11 ("provider construction tests") is a real gap and why §7.1 requires moving the loop into the lib crate.

### 2.3 Live verification

`~/.heterion-router/storage.sqlite` (WAL-aware copy): **62 connection rows, 31 distinct providers, 23 active.** Registry coverage of the active set:

- 19 build
- 5 are skipped: `agentrouter` (claude), `kimi-coding` (claude), `kiro` (kiro), `opencode` (active row, no credential), and **`exa-search` — 5 active rows for a provider id that does not exist in `registry.json` at all.**

**`exa-search` is a new finding.** `main.rs:168-172` silently `continue`s on it. `admin::connections` reports `servable: false` (`admin.rs:186`) — correct by accident, with no reason. It is the concrete instance of audit P1-6 and the reason the `reject_reason` enum in §7.2 must include `NotInRegistry`.

### 2.4 Simulator reconciliation — **19 backends confirmed**

Re-implementing the `main.rs:161-299` gate over the live DB predicts 19 built backends, matching (a) the audit's "19 backends built" and (b) the live `gateway.log.2026-09-29` start block at `11:29:23`, which contains exactly 19 `backend available:` lines:

```
grok-cli, blackbox, cline, deepseek, gemini, groq, huggingface, kilo-gateway,
kilocode, llm7, mistral, nvidia, opencode-go, openrouter, pollinations,
sambanova, siliconflow, trae, xai
```

`curl -s :20128/healthz` → `{"backend":"routing","started_unix":1790669776}`, consistent with `RoutingBackend::name()` (`routing.rs:196-198`) being the only backend the handlers ever see.

**No alias drift exists today** (0 of 23 active providers are stored under an alias). D1/D2 are latent correctness defects, not a live outage. That changes their priority honestly: they belong in the same hardening batch as the observability work, not above it.

---

## 3. Aliases and canonical ids

| Consumer | Canonicalizes? | Evidence |
|---|---|---|
| `ProviderRegistry::get` / `base_url` / `is_openai_format` / `allows_keyless` | ✅ yes, via `by_id` | `registry.rs:107-160` |
| `RoutingBackend::plan` (`provider/model`) | ✅ yes | `routing.rs:160-171` |
| `combos::dispatch_backend` | ✅ yes | `combos.rs:239-242` |
| `admin::connections` `servable` set | ❌ **no** — expands *canonical → alias*, the wrong direction | `admin.rs:145-162` |
| `list_models` | ❌ no — matches `entry.id` only | `lib.rs:208-209` |
| `main.rs` backend keys | ❌ no | `main.rs:295` |
| `credentials::load_credentials` / `active_providers` | ❌ no — SQL `WHERE provider = ?1` | `credentials.rs:45-50`, `112-115` |

The `admin.rs:145-162` inversion is the reason the audit's claim is precise: for an alias-keyed backend, the `servable` set contains the *alias* (from `backend_names`) but the alias-expansion loop looks for `entry.id == name` and finds nothing, so the *canonical* id is never added. Result: `servable: true` for a connection that is unroutable and absent from `/v1/models`.

Alias hygiene in the registry itself is good: 266 entries carry an alias, `by_id.entry(...).or_insert(...)` guarantees a real id wins (`registry.rs:80`), and the invariant is tested (`registry.rs:199-224`). The problem is purely on the *write* side (which id is used as a map key) and the *read* side (which direction the admin expands).

---

## 4. Routing, combos, breaker, retry

### 4.1 Resolution order (`routing.rs:156-191`)

1. `provider/model` → canonicalize prefix, look up backend, return single-step plan.
2. Combo name → `load_combos` (full `SELECT name, data FROM combos`) → `plan_steps`. If the plan is empty, **fall through** rather than erroring (`routing.rs:180-184`).
3. Prefix rule / default chain from `Router` (`router.rs:41-49`, case-insensitive prefix, first match wins).

`vivanta-auto` and `vivanta-deepseek` have `models: []` in the live DB, so step 2 always falls through → step 3 → `default` is empty (no `OPENAI_COMPAT_*` in `.env`, no echo) → empty plan → `no_candidate()` (`routing.rs:75-79`) → `GatewayError::UnknownModel`. **This is the exact path of all 11 logged Rust requests.** See §6.3.

### 4.2 Dead and unimplemented config

| Item | Declared | Consumed | Note |
|---|---|---|---|
| `ComboConfig.max_set_retries` | `combos.rs:26`, parsed `combos.rs:173-175` | **never** | dashboard exposes it |
| `ComboConfig.set_retry_delay_ms` | `combos.rs:28`, parsed `combos.rs:177-179` | **never** | |
| `ComboConfig.max_retries` | `combos.rs:19` | `routing.rs:232, 305` | default `0` ⇒ **no retry ever fires by default** |
| `ComboConfig.failover_before_retry` | `combos.rs:24` | `routing.rs:236, 309` | default `true` |
| `RichEntry.weight` | `combos.rs:85` | round-tripped only | audit P2-12 |
| `strategy: "auto"` | `combos.rs:57` | same as `priority` | `plan_steps` ignores `combo.strategy` entirely (`combos.rs:214-226`) |
| `RoutingBackend.combo_configs` | `routing.rs:34`, written `routing.rs:115` | **never read** | dead field |

**Per-request DB cost:** `combo_config()` (`routing.rs:126-135`) and `plan()` (`routing.rs:173-185`) each run a full `load_combos` (`combos.rs:185-206`) on *every* request, and streaming repeats it again. The already-populated `combo_configs` map is the obvious cache and is unused.

`update_combo_meta` (`repos.rs:669-707`) deserialises the current `data` and mutates only the addressed keys, so `config`, `isHidden` and unknown fields **survive a PATCH**. No config loss on dashboard round-trip. (Verified by reading `repos.rs:677-700`; `Combos.svelte:104-110` never sends `config`, and it is not clobbered.)

### 4.3 Breaker

`CircuitBreaker` (`breaker.rs:22-73`) is a consecutive-failure counter with time. `BreakerSet` (`breaker.rs:77-118`) is keyed by backend name and lazily creates breakers via `allow(&mut self, …)`.

**B1 — no half-open single-probe guard.** `allow()` (`breaker.rs:52-57`) returns `true` for *every* concurrent caller once `now - opened_at >= cooldown`. After a 60 s cooldown, N in-flight requests all hit a dead provider simultaneously — exactly the thundering herd the breaker exists to prevent.

**B2 — cooldown restarts on late failures.** `record_failure` (`breaker.rs:67-72`) sets `opened_at = now` unconditionally once `failures >= threshold`. A request admitted *before* the breaker opened that fails afterwards extends the cooldown again. With a pool of concurrent callers this can hold a backend `Open` indefinitely.

**B3 — classification is string sniffing.** `is_backend_outage` (`routing.rs:44-55`) and `is_transient` (`routing.rs:138-149`) both parse the HTTP status back out of a `Display` string via `upstream_status` (`routing.rs:58-65`, duplicated verbatim at `lib.rs:511-519`). The status only survives because `HttpError::Status` formats as `upstream status {status}: {body}` (`http/client.rs:14-20`) and every call site does `error.to_string()` — `openai.rs:67,140,482,496,526`, `responses.rs:85,104,177`, `gemini.rs:348,363,397,436`, `grok_cli.rs:327,334,360,418`.

Consequence, and it is the important one: **errors constructed without a status become indistinguishable from transport failures.** A 2xx SSE frame carrying `{"error": …}` is turned into `Upstream(<bare message>)` at `openai.rs:99` / `responses.rs:134` / `gemini.rs:392`, `upstream_status` returns `None`, and `is_backend_outage` returns `true`. A provider that returned a *content-policy* error inside a 200 stream trips the breaker for every other model on it — the exact failure mode the `routing.rs:38-43` comment says was already fixed once for 4xx.

**B4 — empty stream counts as success.** `routing.rs:322-325`: a `None` from the first `stream.next()` calls `record_success(name)` and returns. A backend that accepts the connection and immediately closes is recorded as healthy.

**B5 — post-first-chunk failures are invisible.** Once `Some(Ok(first))` is seen (`routing.rs:288-298`), the loop drains the stream and `return`s. Errors arriving after the first chunk are forwarded to the client but never reach `record_failure`, and `recorded = true` in `lib.rs:415-426` means they never reach the call log as a failure either.

### 4.4 Retry

`routing.rs:232-247` (non-streaming) and `routing.rs:305-320` (streaming) are byte-identical in shape. `retry_count < config.max_retries` with `max_retries` defaulting to `0` ⇒ the retry arm is dead on a default combo. `failover_before_retry` default `true` breaks to the next candidate. `retry_delay_ms` is a **fixed** sleep; `Retry-After` is never read (`http/client.rs:89-101` discards response headers on the error path). Live evidence that this matters: 878 OpenRouter 429 rows and 208 Gemini 429 rows in `call_logs.error_summary`, all with per-provider text a client could act on.

---

## 5. Transports

| | `OpenAiBackend` / `ProviderOpenAiBackend` | `ResponsesBackend` | `GeminiBackend` | `GrokCliBackend` |
|---|---|---|---|---|
| file | `openai.rs` | `responses.rs` | `gemini.rs` | `grok_cli.rs` |
| `name()` | `"openai-compatible"` (`:124`) / `"provider"` (`:537`) | `"responses"` (`:152`) | `"gemini"` (`:408`) | `"grok-cli"` (`:511`) |
| endpoint | `resolve_chat_url` (`:382`) | caller-supplied; `resolve_responses_url` (`:391`) | `{base}/{model}:generateContent` (`:315-321`) | `{base}/responses` (`:386-388`) |
| auth | `Authorization: Bearer` | Bearer | `x-goog-api-key` header (`:346`) | Bearer + 8 session headers (`:103-125`) |
| pool | `next_token` (`:459-466`), **index discarded** | `next_token` (`:58-65`), **index discarded** | `next_token` (`:309-313`), **index discarded** | `next_credential` (`:225-233`), **index kept for refresh** |
| keyless | `None` bearer (test `keyless_provider_sends_no_bearer`, `:728-777`) | same | **no** — `with_signature_store` errors on empty pool (`:288-293`) | **no** (`:188-192`) |
| per-model transport switch | `responses_models.contains(&request.model)` (`:548, 593`) | — | — | — |

**T1 — Credential identity is thrown away everywhere except Grok.** All three rotating pools drop the index, so no transport can report *which* `provider_connections` row served a request. This is the single biggest attribution lever (§7.3).

**T2 — `ProviderOpenAiBackend::name()` returns the constant `"provider"`** (`openai.rs:536-539`). Even if the router were bypassed, usage rows would say `provider`, not `openrouter`. The trait's `&'static str` signature is the root cause; the fix is a `provider_id()` accessor, not a rename.

**T3 — Grok 426 is unclassified.** `GROK_CLIENT_VERSION` is pinned to `"0.2.106"` (`grok_cli.rs:35`); the proxy answers `426` when it drifts (`grok_cli.rs:100-106` says so). `426 < 500`, so `is_backend_outage` → `false` and `is_transient` → `false`: no breaker, no failover, no retry, and `error_status` (`lib.rs:525-534`) falls through to `502 BAD_GATEWAY` with the generic body `"upstream request failed"` (`lib.rs:541`). The operator sees an opaque 502 and a `tracing::warn!`. Audit gap #4 confirmed; §7.4 gives it a class.

**T4 — Responses keys leak into Chat bodies.** `responses_to_chat` copies `store, include, reasoning, previous_response_id, text, prompt_cache_key, session_id, conversation_id` into `request.extra` (`lib.rs:1028-1045`), and `openai_request_body` splices **every** `extra` key into the Chat body (`backend.rs:98-100`). A `provider/model` request whose model is not in `responses_models` therefore POSTs `store`/`reasoning` to `/chat/completions`. Audit gap #2 confirmed at `backend.rs:98-100`.

**T5 — Responses streaming emits a non-SDK tool shape.** `lib.rs:1268-1279` emits `response.output_item.added` with `item: {type, call_id, arguments, tool_calls}` — no `id`, no `output_index` beyond a hardcoded `0`, no `response.output_item.done`, no argument deltas. Confirms audit gap #6 / ROADMAP P0-2.

**T6 — `persist_response` stores under the upstream-controlled id** (`lib.rs:573-598`, `valid_response_id` at `lib.rs:549-555`) and `store` defaults to **true** (`lib.rs:830-833`). Confirms audit P0-5.

**T7 — Gemini `thought_signature` state is process-global.** `GeminiBackend.last_message_id` is one `Arc<Mutex<Option<String>>>` for the whole backend (`gemini.rs:38`), and the store file `gemini-thought-signatures.jsonl` is now **195 KB** and growing, unbounded across sessions. `SIGNATURE_CAPACITY = 4096` bounds the in-memory map (`gemini.rs:71`) but the *file* is append-only (`append`, `gemini.rs:167-185`) and only trimmed on load (`rewrite`, `gemini.rs:126-145`).

**T8 — `openai_request_body` has no allow-list for the target transport** — see T4. It is the correct place for the capability fix, not the per-provider call sites.

---

## 6. Attribution, status, admin output

### 6.1 What the live tables actually contain

| table | rows | written by | attribution quality |
|---|---|---|---|
| `usage_history` | 45 136 | JS (44 808) + Rust (328) | JS: real `provider`, real `endpoint`, real `combo_strategy`, real `connection_id` (44 259 non-null). Rust: `provider='routing'`, `connection_id=NULL`, `endpoint=NULL`, `combo_strategy='direct'` (the column default) |
| `call_logs` | 11 907 | JS (11 896) + Rust (11) | JS: real `provider`, real `status` ∈ {200,400,401,403,404,429,499,500,502,503,504}, `connection_id` (11 814), `combo_name` (6 306), and `model ≠ requested_model` in **6 304** rows |

### 6.2 The status-vocabulary collision — **finding, not in the audit**

`usage_history.status` is a **TEXT** column holding two disjoint vocabularies:

```
('200',1,26152) ('400',0,880) ('401',0,546) ('402',0,985) ('403',0,164) ('404',0,534)
('410',0,159) ('413',0,913) ('422',0,42) ('429',0,11251) ('451',0,11) ('499',0,146)
('500',0,197) ('502',0,1444) ('503',0,981) ('504',0,394) ('524',0,9)
('failed',0,154) ('succeeded',1,174)
```

The first 16 buckets are the JS server writing the **real upstream HTTP status as a string**; `succeeded`/`failed` are the Rust gateway (`lib.rs:335`). In the JS rows `success == status.is2xx` holds universally, so the Rust vocabulary is not merely different, it is **mutually exclusive** with every existing query: `WHERE status='429'` sees 11 251 rows and 0 of the Rust failures; `WHERE status='succeeded'` sees 174 rows and nothing else. The same applies to `error_code`: JS writes `upstream_429` (11 210), `upstream_402` (979), `upstream_413` (913), `model_unavailable` (741), `upstream_401` (538), `RATE_LIMIT_QUEUE_TIMEOUT` (508); Rust writes only `upstream_error` / `stream_error`.

`admin::usage` groups by `provider` and `model` (`repos.rs:874-875`) and does not touch `status`, so the dashboard is unaffected today — but `error_code` and `status` are exported and any future filter is already wrong. §7.5 fixes the writer and adds a 328-row, information-preserving fixup.

Also missing from the Rust write, though the live JS-owned schema has the columns: `usage_history.combo_strategy`, `.endpoint`, `.account_key`, `.account_label`, `.account_label_priority`; `call_logs.combo_name`, `.combo_step_id`, `.connection_id`, `.source_format`, `.target_format`, `.correlation_id`, `.error_type`. `migrate.rs:62-111` only converges columns for `api_keys` — nothing converges these, and nothing needs to, because they all exist in `schema_base.sql` and in the live DB. **This workstream therefore needs zero schema DDL.**

### 6.3 Every logged Rust request is a mislabelled routing miss

All 11 `call_logs` rows with `request_type='chat'`:

```
2026-09-25 11:56:48  500  vivanta-auto  vivanta-auto  routing  1ms  0/0  stream_error  OpenCode-Main
2026-09-25 13:24:12  500  vivanta-auto  vivanta-auto  routing  1ms  0/0  stream_error  OpenCode-Main
2026-09-26 20:00:01  500  vivanta-auto  vivanta-auto  routing  0ms  0/0  stream_error  OpenCode-Main
2026-09-26 22:45:42  500  vivanta-auto  vivanta-auto  routing  1ms  0/0  stream_error  OpenCode-Main
2026-09-27 15:29:50  500  vivanta-auto  vivanta-auto  routing  0ms  0/0  stream_error  OpenCode-Main
2026-09-27 21:00:27  500  vivanta-auto  vivanta-auto  routing  3ms  0/0  stream_error  OpenCode-Main
2026-09-27 21:30:14  500  vivanta-auto  vivanta-auto  routing  0ms  0/0  stream_error  OpenCode-Main
2026-09-27 22:20:42  500  vivanta-auto  vivanta-auto  routing  1ms  0/0  stream_error  OpenCode-Main
2026-09-28 18:19:35  500  vivanta-auto  vivanta-auto  routing  1ms  0/0  stream_error  OpenCode-Main
2026-09-29 07:52:53  500  vivanta-auto  vivanta-auto  routing  1ms  0/0  stream_error  OpenCode-Main
2026-09-29 09:59:28  500  vivanta-auto  vivanta-auto  routing  0ms  0/0  stream_error  OpenCode-Main
```

Read end to end this is one bug, fully reproduced in production:

1. `vivanta-auto` has `models: []` in the live DB.
2. `plan()` step 2 finds nothing and **falls through** (`routing.rs:180-184`).
3. The default chain is empty ⇒ empty plan ⇒ `no_candidate()` (`routing.rs:75-79`) ⇒ `GatewayError::UnknownModel`.
4. `is_backend_outage(UnknownModel)` → `false` (`routing.rs:53`) — correct, no breaker trip.
5. The SSE path records the failure with a **constant** `error_code: "stream_error"` (`lib.rs:386`), a **hardcoded** `status: 500` (`lib.rs:351`) and `provider: state.backend.name()` = `"routing"` (`lib.rs:354`).
6. `latency_ms` 0–3 because no network call was ever made.

So: an operator reading the dashboard sees *eleven server errors on the provider `routing`*, when the truth is *eleven routing misses on an empty combo*. The real reason (`"vivanta-auto (no backend can serve it; check the combo entries or provider credentials)"`) is on the wire (`lib.rs:493-495`) but never in the log. This is audit P1-7 with a body count.

### 6.4 Other attribution losses

- `call_logs.model` and `.requested_model` are both set to the *requested* string (`lib.rs:353-354`). The resolved model — already in `response.model` and in `tracing::info!` at `lib.rs:245` — is never persisted. The JS side distinguishes them in 6 304 rows.
- `combo_name` is never written (no such field on `CallLogRecord`, `usage/records.rs:26-42`) although `admin::logs` already selects it (`repos.rs:919`) and `Logs.svelte:86` already prefers `row.model`.
- `connection_id` is hardcoded `None` (`lib.rs:327`).
- `UsageRecord.tokens_cache_read / _creation / _reasoning` are hardcoded `0` (`lib.rs:331-333`) even though the SSE decoder captures `input_tokens`/`output_tokens` (`backend.rs:134-141`) and the response object carries `output_tokens_details` in the raw payload.
- **Pricing is keyed on the requested model string.** `PriceTable::price_for` is an exact `HashMap` lookup (`pricing.rs:102-104`), and `cost_usd` is called with `&model` = the requested string (`lib.rs:267`, `lib.rs:883`). For a `provider/model` request the lookup misses and the `x-heterion-router-cost-usd` header is simply absent. Passing the resolved bare model would make it work for free.
- `overview.counts.api_keys` uses `count_active_api_keys` (`admin.rs:68` → `repos.rs:941-948`), which is `WHERE revoked_at IS NULL` and ignores `is_active`, `is_banned` and `expires_at` — all three columns exist and `migrate.rs:62-111` guarantees them. `auth.rs:108` uses a *different* query (`SELECT COUNT(*) FROM api_keys`) for the auth decision. Today there is 1 key so both return 1; they will diverge.

---

## 7. Design

### 7.1 Canonical-id strategy

**Decision: canonicalize on the read path. Do not rewrite `provider_connections`.**

Rationale: the stored string is what the JS dashboard writes; rewriting 62 live rows to satisfy a Rust-side keying bug moves data instead of fixing code, and violates the roadmap's "state origin must be explicit". The defect is the map key, so fix the map key.

**Step 1 — make construction testable.** New module `crates/heterion-router-gateway/src/backends.rs`; move the `main.rs:98-299` body into

```rust
pub struct BuiltBackends {
    pub backends: HashMap<String, Arc<dyn ChatBackend>>,   // canonical keys
    pub alias_index: BTreeMap<String, String>,              // stored -> canonical
    pub report: Vec<BackendReport>,                        // per decision, with reason
}
pub struct BackendReport {
    pub stored: String, pub canonical: Option<String>,
    pub transport: Option<Transport>, pub reason: Option<RejectReason>,
    pub credentials: usize, pub base_url: Option<String>,
}
pub fn build_backends(
    db: &Db, registry: &ProviderRegistry, data_dir: &Path, env: &EnvOverrides,
) -> BuiltBackends
```

`main.rs::select_backend` becomes a call site. This alone unblocks §8 fixture 2 and removes the double `ProviderRegistry::load()` (D5).

**Step 2 — the loop head.** `backends.rs`, replacing `main.rs:161-172`:

```rust
for stored in active_providers(&guard)? {
    let Some(canonical) = registry.canonical_id(&stored) else {
        report.push(BackendReport { stored: stored.clone(), canonical: None,
            reason: Some(RejectReason::NotInRegistry), ..Default::default() });
        tracing::warn!(provider = %stored, "connection provider is not in the registry");
        continue;                                    // <- fixes exa-search reporting
    };
    let canonical = canonical.to_string();
    if stored != canonical { alias_index.insert(stored.clone(), canonical.clone()); }
    if backends.contains_key(&canonical) { continue; }
    let local_override = (canonical == "heterion-local")                 // D2 fixed
        .then(|| env.heterion_local_base_url.clone()).flatten();
    let Some(base_url) = provider_base_url_from(registry, &canonical, local_override.as_deref()) else {
        report.push(reason: NoBaseUrl); continue;
    };
    if canonical == "grok-cli" {                                        // D3 fixed
        tracing::warn!("grok-cli has no usable credential pool; the dedicated executor is mandatory");
        report.push(reason: NoCredential); continue;
    }
    // ... transport branch, keyed by `canonical`:
    backends.insert(canonical.clone(), Arc::new(backend));
    report.push(BackendReport { transport: Some(transport), .. });
}
```

**Step 3 — `list_models` gets a `HashSet`.** `lib.rs:209` is an O(278×19) scan per request. Build `let live: HashSet<&str> = state.backend_names.iter().map(String::as_str).collect();` once at `lib.rs:208` and use `live.contains(entry.id.as_str())`.

**Step 4 — fix the admin inversion.** `admin.rs:145-162` becomes

```rust
let live: HashSet<String> = state.backend_names.iter().cloned().collect();
let servable = |stored: &str| {
    let canonical = registry.canonical_id(stored).unwrap_or(stored);
    live.contains(canonical)
};
```

and the row gains `canonical_provider` and `alias_of` (`admin.rs:172-189`).

**Step 5 — allow-list compatibility.** `auth.allows_model` is evaluated on the **requested** string (`lib.rs:230`, `lib.rs:850`) *before* any canonicalization, and that must not change. But after step 2 a stored allow-list written against an alias (`ds/*`) will stop matching once `/v1/models` advertises `deepseek/*`. Fix inside `auth.rs:44-58`: also test the canonical form, mirroring `matches_pattern` (`auth.rs:61-69`):

```rust
pub fn allows_model(&self, model: &str, registry: Option<&ProviderRegistry>) -> bool {
    let candidates = [Some(model), registry.and_then(|r| r.canonical_provider_of_request(model))];
    // existing empty-list / wildcard / prefix logic, run over both candidates
}
```

Net effect: allow-lists keep working under both spellings, and `/v1/models` stays canonical.

**Step 6 — scheme gate.** In the transport branch, before constructing an HTTP backend:

```rust
if !matches!(scheme(&base_url), "http" | "https") {
    report.push(reason: UnsupportedScheme(scheme));
    continue;                                    // D4 fixed: wss:// never built
}
```

**Do NOT do:** a `provider_connections` alias rewrite migration. Rationale in §7.5.

### 7.2 Capability profile and executor handling

**Principle (ROADMAP §«Принципы» #2):** capability is decided by the *effective model format*; registry `executor` metadata is never proof of an executor.

New in `heterion-router-providers/src/capability.rs`:

```rust
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize)]
pub enum Transport { OpenAiChat, OpenAiResponses, Gemini }

#[derive(Clone, PartialEq, Eq, Debug, Serialize)]
pub enum RejectReason {
    NotInRegistry,
    NoBaseUrl,
    UnsupportedFormat(String),      // "claude", "cursor", "kiro", "clova", "custom", "antigravity"
    UnsupportedScheme(String),      // "wss", "auggie", "devin", "zcode", "codex-app-server"
    NeedsCredential,
    BespokeExecutorUnimplemented,   // executor != "default" and != the transport we built
}

pub struct Serveability {
    pub transport: Option<Transport>,
    pub keyless: bool,
    pub executor: Option<&'static str>,
    pub bespoke_executor: bool,
    pub reject: Option<RejectReason>,
}

impl ProviderRegistry {
    pub fn serveability(&self, id: &str) -> Serveability;   // ONE implementation
    pub fn model_capabilities(&self, provider: &str, model: &str) -> ModelCaps;
}

pub struct ModelCaps {
    pub wire_format: Option<WireFormat>,   // from model_target_format (registry.rs:133-140)
    pub transport: Option<Transport>,
    pub responses_endpoint: bool,          // model.target_format == "openai-responses"
    pub derived_url_unverified: bool,      // true for any non-`openai-responses`-format provider
}
```

**`serveability` becomes the single gate.** `main.rs:175-246`'s three-branch string cascade, `combos::dispatch_backend` (`combos.rs:234-249`) and the `reject_reason` reporting all call it. This is what makes "the router and the admin UI never disagree" (`combos.rs:230-233`) actually true — today the claim is asserted in a comment but three predicates implement it.

**Executor handling — reporting only, never dispatch.** `bespoke_executor = executor != "default"` drives one counter in `/api/overview.registry` and one badge in the Providers table. It must **not** gate construction, because 61 of the 253 HTTP-capable providers run fine under the generic executor (`pollinations`, `kilo-gateway`, `xai`, `trae`, `sambanova` are all live with bespoke executors declared). Gating on it would take 19 backends down to 0. This is the single most dangerous possible misreading of the registry and the design must say so in a comment at the definition site.

**Model-level inconsistency fix.** `zai` has 2 models with `target_format: "openai"` under a `claude`-format provider. `ModelCaps` reports the per-model view; `Serveability` reports the per-provider view; both are surfaced in admin, and the provider-level verdict wins for construction. Do not "fix" the registry data.

### 7.3 Attribution data flow

**Design constraint:** the `ChatBackend` trait (`backend.rs:22-34`) has 8 implementations and a `&'static str` `name()`. Changing its error type would touch all of them plus every `?`. Instead, a request-scoped trace that requires no trait change:

```rust
// crates/heterion-router-gateway/src/trace.rs
#[derive(Clone, Default)]
pub struct RouteTrace {
    pub requested_model: String,
    pub kind: RequestKind,                 // ProviderModel | Combo | PrefixRule
    pub combo_name: Option<String>,
    pub backend: Option<String>,           // canonical provider id
    pub target_model: String,              // bare id sent upstream
    pub connection_id: Option<String>,
    pub wire_format: Option<WireFormat>,
    pub source_format: Option<WireFormat>,
    pub attempts: Vec<Attempt>,
}
pub struct Attempt {
    pub backend: String, pub target_model: String, pub connection_id: Option<String>,
    pub outcome: Outcome, pub status: Option<u16>, pub fault: Option<Fault>,
    pub latency_ms: u32, pub retry_index: u32,
}

tokio::task_local! { pub(crate) TRACE: RefCell<Option<Arc<Mutex<RouteTrace>>>>; }
```

**Task-local vs `tokio::spawn` — the trap that must be designed around.** Every streaming backend spawns (`routing.rs:268`, `openai.rs:158`, `responses.rs:206`, `gemini.rs:470`, `grok_cli.rs:558`), and task-locals do **not** propagate into `tokio::spawn`. So:

1. The handler creates `Arc<Mutex<RouteTrace>>` and installs it in the task-local (`lib.rs`, at the top of `chat_completions` and `responses`, wrapping the whole body — including the `Sse` construction, because `state.backend.stream(request)` is called *before* the response is returned at `lib.rs:401`).
2. `RoutingBackend::stream` reads the task-local **on the calling task** (`routing.rs:263-266`), clones the `Arc`, and `move`s it into the spawned closure.
3. The spawned closure writes attempts into the `Arc`.
4. The handler `await`s stream completion (it already owns the `Sse` body) and reads the `Arc` in `record_stream_usage` (`lib.rs:368-390`).

`RoutingBackend::complete` needs no spawn, so it writes in place. `RoutingBackend::plan` (`routing.rs:156-191`) is called from both paths and is where `requested_model`, `kind`, `combo_name` and `target_model` are decided — it is the correct place to fill the first four fields, and the per-attempt loop in `complete`/`stream` fills `backend`, `connection_id`, `outcome`, `status`, `fault`, `latency_ms`.

**Credential identity (T1).** Change the three pools from `Vec<String>` to `Vec<CredentialSlot>`:

```rust
pub struct CredentialSlot { pub connection_id: Option<String>, pub token: String }
```

and have `next_token()` return `(index, &str)`. `ProviderOpenAiBackend::complete` (`openai.rs:558-586`), `ResponsesBackend::complete` (`responses.rs:169-197`) and `GeminiBackend::complete` (`gemini.rs:427-461`) already loop over pool entries — they write the chosen `connection_id` into the task-local trace. `GrokCliBackend` already has it (`grok_cli.rs:225-233`). This is the smallest change that produces real per-connection attribution and it also removes the `for _ in 0..tokens.len().max(1)` + `continue` pattern that currently retries the *same* key when a pool has one entry.

**Persistence.** `record_completion_usage` (`lib.rs:308-366`) is rewritten to take `&RouteTrace` and fill:

| column | today | after |
|---|---|---|
| `usage_history.provider` | `"routing"` (`lib.rs:325`) | `trace.backend` (canonical) |
| `usage_history.model` | requested | `trace.backend + "/" + trace.target_model` |
| `usage_history.connection_id` | `None` (`lib.rs:327`) | `trace.connection_id` |
| `usage_history.combo_strategy` | column default `'direct'` | `trace.kind` + combo `strategy` |
| `call_logs.provider` | `"routing"` (`lib.rs:354`) | `trace.backend` |
| `call_logs.model` | requested (`lib.rs:353`) | resolved `target_model` |
| `call_logs.requested_model` | requested | unchanged (correct) |
| `call_logs.combo_name` / `.combo_step_id` | never written | `trace.combo_name` / entry `id` (`combos.rs:81`) |
| `call_logs.status` | `200`/`500` (`lib.rs:351`) | `error_status(&error).as_u16()` (`lib.rs:521-536`) |
| `call_logs.error_summary` | `"upstream_error"`/`"stream_error"` | `Fault` discriminant + upstream code, e.g. `upstream_429`, `no_candidate` |
| `usage_history.error_code` | same two constants | `upstream_<status>` / `transport_error` / `no_candidate` |
| `call_logs.connection_id`, `.source_format`, `.target_format` | never written | from the trace |

`CallLogRecord` (`usage/records.rs:26-42`) gains `connection_id`, `combo_name`, `combo_step_id`, `source_format`, `target_format`; `record_call_log` (`usage/records.rs:44-72`) gains the same columns. All exist in `schema_base.sql`. **No migration.**

**Pricing.** `lib.rs:267` and `lib.rs:883` switch from `&model` to `&trace.target_model` when the trace resolved one, so `x-heterion-router-cost-usd` starts appearing for `provider/model` requests. Keep the requested name as a fallback.

**`no_log` behaviour is unchanged** (`lib.rs:309-311`): a `no_log` key writes neither table. The trace is still built in memory and is not persisted — confirmed by test `no_log_key_does_not_write_usage_or_call_logs` (`lib.rs:1686`).

### 7.4 Error, status and retry semantics

**New error shape** in `heterion-router-core/src/error.rs`, keeping the three existing variants and adding one:

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpstreamError {
    pub status: Option<u16>,          // from HttpError::Status, no longer stringly-typed
    pub code: Option<String>,         // upstream `error.code`
    pub message: String,
    pub retry_after: Option<u64>,     // seconds, from the Retry-After header
    pub request_id: Option<String>,   // x-request-id / request-id
}
pub enum GatewayError {
    UnknownModel(String),
    Upstream(String),                 // kept: still constructed for non-HTTP faults
    InvalidRequest(String),
    Detail(UpstreamError),            // new: everything with a status or upstream code
}
impl From<&HttpError> for UpstreamError { … }
```

`http/client.rs:89-101` currently throws away the response headers on the error path. Capture `retry-after`, `x-request-id` and `request-id` there into the `HttpError::Status` payload, then replace every `error.to_string()` at `openai.rs:67,140,482,496,526`, `responses.rs:85,104,177`, `gemini.rs:348,363,397,436`, `grok_cli.rs:327,334,360,418` with `GatewayError::Detail(UpstreamError::from(&error))`.

**Then delete the two string parsers** `routing.rs:58-65` and `lib.rs:511-519`. Both are private, both have exactly two call sites, and both become dead the moment the typed field exists. This is the change that makes B3 impossible to reintroduce.

**One classifier, two questions.** `is_backend_outage` (`routing.rs:44-55`) and `is_transient` (`routing.rs:138-149`) collapse into:

```rust
pub enum Fault {
    Refused,          // 4xx: answered, refused this request
    RateLimited,      // 429          -> retry yes, breaker no
    Misconfigured,    // 425, 426     -> retry no,  breaker no, 502 -> client 503 + diagnostic
    Unavailable,      // 5xx, 426?no -> retry yes, breaker yes
    Unreachable,      // transport/DNS/TLS/timeout/exhausted pool -> retry yes, breaker yes
    Truncated,        // stream ended with no terminal event, or empty stream -> retry yes, breaker yes
    Routed,           // UnknownModel / InvalidRequest -> retry no, breaker no
}
impl Fault {
    pub fn retryable(self) -> bool;
    pub fn trips_breaker(self) -> bool;
    pub fn client_status(self) -> StatusCode;   // 503 for Misconfigured, else 502/504/429
}
```

The `Misconfigured` class is the direct answer to T3 / audit gap #4: a Grok `426` produces **`503` with a message naming the pinned client version** and a `tracing::warn!`, and does **not** retry (an identical retry cannot change the outcome) and does **not** trip the breaker (the backend is reachable). Today it is a bare `502 upstream request failed` and, on the streaming path, an error frame whose text is the raw upstream body (audit high finding #1) — the sanitization fix belongs here: `error_event` (`lib.rs:493-495`) emits the `Fault` discriminant + `UpstreamError::code`, never `UpstreamError::message` when the body was not from a known-safe provider.

`Retry-After` is honored: `retry_delay_ms` becomes `max(retry_delay_ms, retry_after)`, clamped to 60 s, so the default 0 delay does not hammer a rate-limited provider 11 251 times' worth of 429s.

**Breaker fixes (B1, B2, B4, B5).**

```rust
pub struct CircuitBreaker { threshold, cooldown, failures, opened_at, probe_in_flight: bool }
```
- `allow(now)`: `!probe_in_flight` and (`opened_at.is_none()` or cooldown elapsed or `probe_in_flight` is already ours) → at most **one** half-open probe (`breaker.rs:52-57`).
- `record_failure` restarts the cooldown only when the breaker was `Open` **before** this call (`breaker.rs:67-72`) → B2.
- `RoutingBackend::stream`: `None` on the first poll is `Fault::Truncated` and calls `record_failure`, not `record_success` (`routing.rs:322-325`) → B4.
- Post-first-chunk errors call `record_failure` (no replay, no failover) but **do** flip the call-log row to failed if none was written (`routing.rs:288-298`, `lib.rs:415-426`) → B5.

**Retry configuration.** Implement `max_set_retries` as one outer loop over the plan (`routing.rs:210` / `routing.rs:273`) — six lines — rather than deleting the field the dashboard already exposes. Drop `set_retry_delay_ms` from `ComboConfig` and from `parse_combo_config` (`combos.rs:28,177-179`), because `Retry-After` supersedes it; note the removal in `CHANGELOG.md` and in `Combos.svelte` if it renders the field (it currently does not — verified, `Combos.svelte` sends only `strategy` and `models`).

**Combo cache.** Replace the dead `combo_configs` field (`routing.rs:34,115`) and the two per-request `load_combos` calls (`routing.rs:127-133`, `routing.rs:173-185`) with one `Arc<RwLock<Arc<HashMap<String, ComboDef>>>>`, refreshed in `admin::update_combo_meta` (`admin.rs:685`) so the UI's promise "Routing uses the new definition immediately" (`Combos.svelte:41`) stays true.

### 7.5 Migration compatibility

| Concern | Verdict |
|---|---|
| SQLite schema | **No DDL.** `call_logs.{provider,connection_id,account,combo_name,combo_step_id,source_format,target_format,request_type,correlation_id,error_type}` and `usage_history.{provider,connection_id,endpoint,combo_strategy,account_key,account_label,ttft_ms,service_tier,status,success,error_code}` all exist in both `schema_base.sql` and the live JS-owned DB (verified by dumping the live `sqlite_master` DDL). |
| `provider_connections` alias rows | **No rewrite.** Read-path canonicalization (§7.1). 0 alias-drifted rows exist live, so a migration would be a no-op with a write risk. |
| `STATIC_SALT` / `enc:v1:` | Untouched. `load_credentials` (`credentials.rs:74-99`) keeps its "skip what cannot be decrypted" rule. |
| Env names / ports / data dir | Untouched. `HETERION_ROUTER_*` primary, `OMNIROUTE_*` fallback, 20128/20129, `~/.heterion-router` → `~/.omniroute`. |
| `registry.json` | Byte-identical. Only derived accessors added; `provider_id_list_snapshot` (`registry.rs:341-345`) does not change. |
| Migration ledger | `MIGRATIONS` (`migrate.rs:32-36`) stays at `001`. |

**One additive migration, `002`, and it is a data fixup, not DDL.** `usage_history.status` currently holds two disjoint vocabularies (§6.2). Proposed, in the same idempotent style as `ensure_api_keys_columns` (`migrate.rs:89-111`):

```sql
UPDATE usage_history
   SET status = CASE WHEN success = 1 THEN '200' ELSE '500' END
 WHERE status IN ('succeeded', 'failed');
```

328 rows, information-preserving: in every JS row `success == status.is2xx`, so this maps the Rust rows onto the meaning the column already has. It must run **after** the §7.4 writer change ships, or it will re-create the split. If the team prefers zero data migrations, the alternative is to leave the 328 rows and have `admin::usage` (`admin.rs:406-446`) fold `status NOT IN ('succeeded','failed')` into a "legacy" bucket — strictly worse, and it still leaves `error_code` split.

**Behaviour changes that must be recorded in `CHANGELOG.md`:**

1. Connections stored under a registry alias become servable (previously silently dead).
2. Grok `426` returns `503` with a diagnostic instead of an opaque `502`.
3. `usage_history.provider` / `call_logs.provider` change from `"routing"` to the real canonical provider id. **Existing rows keep `"routing"`**, so `Usage.svelte:81-90` ("Top providers") will show a discontinuity. Add a one-line UI note, or a repo query that buckets `provider='routing'` as `legacy: unattributed`.
4. `usage_history.status` becomes the numeric string; `error_code` becomes `upstream_<status>`.
5. `x-heterion-router-cost-usd` starts appearing for `provider/model` requests.
6. An empty-stream upstream response now trips the breaker instead of being recorded as a success.

### 7.6 Admin and UI implications

**`GET /api/overview` (`admin.rs:53-86`)** — add a `registry` block computed from `Serveability`:

```json
"registry": { "providers": 278, "models": 2717, "http_capable": 253, "http_models": 1916,
  "constructed": 19, "rejected": { "not_in_registry": 1, "no_base_url": 2,
    "unsupported_format": 12, "unsupported_scheme": 6, "needs_credential": 1,
    "bespoke_executor_unimplemented": 61 } }
```
Fix `counts.api_keys` to the same predicate `auth.rs:108` uses, and report both (`keys_total`, `keys_usable`).

`Overview.svelte:33-58` gets a "Registry coverage" card; the "live backends" copy at `Overview.svelte:50-53` currently says *"an executor for its wire format"*, which is only true for the 61 bespoke ones by accident — reword to name the actual gate.

**`GET /api/connections` (`admin.rs:140-192`)** — per row add `canonical_provider`, `alias_of` (bool), `transport`, `reject_reason`; split `servable` (backend exists) from `routable` (canonical key matches). `Providers.svelte:121-131` gains two columns; the `onlyProblems` filter at `Providers.svelte:71-79` gains `row.reject_reason`.

**`GET /api/combos` (`admin.rs:92-137`)** — per entry add `canonical_provider` and `reject_reason`, from the same `Serveability` that `dispatch_backend` uses. Today an entry whose provider is `claude`-format or `wss://` is reported identically to one whose provider simply has no connection row.

**`GET /v1/models` (`lib.rs:181-224`)** — add `"transport"` and `"responses_endpoint"` to the `heterion-router` object (`lib.rs:213-219`). Ids stay canonical. A `?provider=` filter is optional and should be skipped.

**`GET /api/usage` (`admin.rs:406-446`)** — no query change needed; `by_provider` becomes real. Do **not** add `by_connection` (YAGNI — the data lands in the table and can be added when someone asks).

**`GET /api/logs` (`admin.rs:449-482`)** — `repos::recent_calls` (`repos.rs:916-938`) already selects `combo_name`; it will start populating. Add `connection_id` to the SELECT and the JSON, and a `combo` column in `Logs.svelte:77-87`. Fix the `row.model ?? row.requested_model` fallback (`Logs.svelte:86`) to show both, since they now differ as they did in the JS era.

**`Playground.svelte:16-26`** — lists combo names only. After Stage 2 it can also list `provider/model` ids from `/v1/models`; optional, ~10 lines.

### 7.7 No-provider-expansion boundary

**This workstream adds zero registry entries and zero executors.** Its entire output is making the existing runtime honest about what it can serve. Concretely **out of scope**, with the counts that make it checkable:

| Not doing | Count | Why |
|---|---|---|
| Per-provider executors for bespoke `executor:` values | 61 providers / 959 models | Different workstream; reporting-only here (§7.2) |
| `claude` transport (`AnthropicBackend`, `/v1/messages`) | 10 providers / 609 models | Explicitly "Not started" in `ROADMAP-2026-09.md:47` |
| WebSocket / stdio / ACP transports | 6 providers / 192 models | `ROADMAP-2026-09.md:118-120` |
| Adding `bedrock` / `muse-code` base URLs | 2 providers / 15 models | Provider expansion, not a routing fix |
| An allowlist of *verified* Responses endpoints | 14 providers / 150 models | Report `derived_url_unverified`, do not curate |
| OpenAI built-in tools, MCP, background jobs, Conversations API | — | `ROADMAP-2026-09.md:116-120` |
| Deleting or "fixing" the 61 bespoke executor rows in the registry | — | Registry is frozen JS-derived data (`registry.rs:1`) |

The one thing that *looks* like expansion but is not: making previously-dead providers routable **by fixing the key**, not by adding a provider. `exa-search` stays unbuilt — it is reported, not repaired.

---

## 8. Test fixtures

Existing conventions to follow: `wiremock` for upstream, `Db::open_in_memory()` + `migrate()` for storage, `tower::ServiceExt::oneshot` for handlers, `insta` only for the registry id list. `heterion-router-providers` has `insta` but no `wiremock`; `heterion-router-gateway` has `tower` and `wiremock` as dev-deps.

| # | File | Asserts |
|---|---|---|
| **F1** | `crates/heterion-router-providers/tests/serveability.rs` | The five registry counts as **data**, not prose: 278/2717, 259/2108, 253/1916, 6/192, 19/609, plus 61/959 bespoke. Any registry regeneration that moves a number fails the build. This is §1.4 as a permanent guard. |
| **F2** | `crates/heterion-router-gateway/tests/alias_construction.rs` | In-memory `Db` with rows stored as `ds`, `gc`, `heterion`, `deepseek`, `kilo-gateway`; call `build_backends()`. Assert: every backend key is canonical; `alias_index == {ds→deepseek, gc→grok-cli, heterion→heterion-local}`; `heterion` **does** pick up `HETERION_LOCAL_BASE_URL`; `gc` produces a `NoCredential` report and **no** backend pointing at `cli-chat-proxy.grok.com`; `exa-search` produces `NotInRegistry`. **Requires §7.1 step 1** — impossible to write today. |
| **F3** | `crates/heterion-router-gateway/tests/attribution.rs` | `RoutingBackend` over two stub backends (`alpha` serving, `beta` 503) + a DB combo + an API key. Assert `usage_history.provider/model/connection_id`, `call_logs.provider/model/requested_model/combo_name/status/error_code`, and the cost header. Streaming variant asserts the same via the spawned-task path — the task-local trap of §7.3. |
| **F4** | `crates/heterion-router-gateway/tests/retry_matrix.rs` | Table-driven over `{200, 400, 401, 403, 404, 409, 413, 422, 425, 426, 429, 500, 502, 503, 504, transport, empty-stream, exhausted-pool}` × `{retryable?, trips_breaker?, client_status?}`. One fixture pins the whole §7.4 table. |
| **F5** | `crates/heterion-router-gateway/tests/breaker_half_open.rs` | 32 concurrent `allow()` calls after cooldown ⇒ exactly `1` `true` (B1). Failure during cooldown does not extend it (B2). |
| **F6** | `grok_cli.rs` `#[cfg(test)]` | wiremock returns `426` ⇒ gateway answers `503`, body names the pinned version, no retry, breaker stays closed (T3). Companion test: an SSE frame `{"error":{…}}` inside a `200` does **not** trip the breaker (B3). |
| **F7** | `crates/heterion-router-gateway/tests/admin_contract.rs` | Synthetic DB; assert the JSON *shape* of `/api/overview.registry`, `/api/connections` (new keys) and `/api/combos` entries. Does not depend on the live 8-combo/19-backend shape, which is machine-local. |
| **F8** | `crates/heterion-router-gateway/tests/vivanta_empty_combo.rs` | The §6.3 repro as a regression: combo with `models: []` ⇒ `404` (from `error_status(UnknownModel)`, `lib.rs:524`), `call_logs.status = 404`, `error_summary = "no_candidate"`, `latency_ms < 50`, `provider = "routing"`… **no**: `provider` must be the resolved value or `NULL`, never the literal `"routing"`. |

---

## 9. Staged acceptance tests

Every stage ends green on the project gate from `AGENTS.md`:

```sh
cargo fmt --all && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace
```

### Stage 0 — Freeze the numbers (no behaviour change)

- **A0.1** F1 lands. `cargo test -p heterion-router-providers` proves 278/2717, 259/2108, 253/1916, 6/192, 19/609, 61/959.
- **A0.2** Script §1.4 is committed as `scripts/registry-coverage.py` (new file, no crate change) and its output pasted into `docs/research/PROJECT-AUDIT-2026-09.md`, annotated **"working tree; HEAD is 278/2715"**.
- **A0.3** Audit corrections filed: the two-vocabulary `usage_history.status` split (§6.2), the 11 mislabelled `500`s (§6.3), `exa-search` (§2.3), and the WAL-staleness trap (§0.3).
- **Gate:** all existing tests pass unchanged. **No production code touched.**

### Stage 1 — Canonical ids (D1, D2, D3, D4 + the §6.3 provider lie)

- **A1.1** §7.1 steps 1–2 land: `backends.rs` exists, `main.rs` calls it, registry loaded once.
- **A1.2** F2 passes — in particular `gc` yields no backend and `heterion` honours the override.
- **A1.3** Scheme gate: a `wss://` provider with an active connection produces **no** backend and a `UnsupportedScheme` report.
- **A1.4** `admin::connections.servable` no longer reports `true` for an alias-stored, unroutable row. Regression assertion inside F7.
- **A1.5** `auth.allows_model` accepts both `ds/x` and `deepseek/x`; `/v1/models` emits **only** canonical ids.
- **A1.6** Live rehearsal: copy the WAL-aware DB to a temp data dir, boot the gateway on a free port, confirm `backend available:` count is **19** and the set is byte-identical to §2.4. *Read-only against the real DB; never against the live one.*
- **Gate:** 19 backends before and after; zero provider gained or lost.

### Stage 2 — Capability profile and reachability reporting

- **A2.1** `serveability()` exists; `combos::dispatch_backend`, the builder and the admin report all consume it. Delete the three-way string cascade at `main.rs:175-246`.
- **A2.2** F7 passes: `/api/overview.registry` numbers match §7.6 exactly.
- **A2.3** `/api/combos` shows a distinct `reject_reason` for a `claude` entry, a `wss://` entry and a no-connection entry.
- **A2.4** `executor` is proven non-gating: a provider with `executor: "glm"` and a live connection **still** builds a backend; a test asserts it, so nobody "fixes" this later.
- **A2.5** `registry.json` byte-identical; `provider_id_list_snapshot` unchanged.
- **Gate:** no change to `/v1/models` ids; no change to which backends exist.

### Stage 3 — Attribution

- **A3.1** §7.3 trace lands; F3 passes for both `complete` and `stream`, including a `beta`-fails-then-`alpha`-succeeds case asserting `attempts.len() == 2`.
- **A3.2** Connection identity: a provider with 3 keys produces 3 distinct `connection_id` values across 3 requests, and the value is a real `provider_connections.id`.
- **A3.3** F8 passes: the empty combo yields `404`, `error_summary="no_candidate"`, and **no** row with `provider='routing'`.
- **A3.4** `no_log` key: zero rows in both tables, trace built but discarded (extend `lib.rs:1686`).
- **A3.5** Pricing: a `provider/model` request for a priced model emits `x-heterion-router-cost-usd`.
- **A3.6** Before/after on a copied DB: `SELECT COUNT(*) FROM usage_history WHERE provider='routing'` stops growing.
- **Gate:** `call_logs` insert still succeeds against the **live** schema — proven by the §0.3 WAL-aware copy, since a fresh Rust DB would not catch a column mismatch.

### Stage 4 — Error, status, retry

- **A4.1** F4 passes for all 18 rows of the matrix.
- **A4.2** `routing.rs:58-65` and `lib.rs:511-519` are **deleted** (`rg 'upstream_status'` returns nothing outside tests). Enforced by F4, not by review.
- **A4.3** Grok `426` → `503` with a version-naming body, no retry, breaker closed (F6).
- **A4.4** A `200` SSE frame carrying `{"error":…}` does not trip the breaker; a `503` does (F6).
- **A4.5** `Retry-After: 2` on a `429` produces a ≥2 s delay on the retry.
- **A4.6** SSE error frames contain no upstream body text (audit high #1) — wire-level assertion on the raw body.
- **A4.7** `max_set_retries: 1` in a combo's `config` produces exactly one extra pass over the plan; `set_retry_delay_ms` is gone from `ComboConfig` and the CHANGELOG says so.
- **Gate:** every existing routing test still passes; the 4xx-does-not-trip-breaker contract (`AGENTS.md`) is unchanged and re-asserted.

### Stage 5 — Breaker and combo cache

- **A5.1** F5 passes: one half-open probe, no cooldown extension from late failures.
- **A5.2** Empty stream ⇒ `record_failure` (`Fault::Truncated`), and a subsequent request is not served by that backend while `Open`.
- **A5.3** Combo definitions are read from the cache: assert `load_combos` is not called per request (count via a `tracing` subscriber or by making `load_combos` `pub(crate)` and wrapping it in a test counter).
- **A5.4** `PATCH /api/combos/:id` invalidates the cache and the very next request uses the new list (the `Combos.svelte:41` promise).
- **A5.5** `maxSetRetries`/`setRetryDelayMs`/`weight`/`strategy:auto` are each either implemented or removed, with `CHANGELOG.md` naming which (closes audit P2-12).

### Stage 6 — Admin and UI, then migration 002

- **A6.1** F7 passes; every new admin key is consumed by a Svelte view (no dead JSON).
- **A6.2** `Usage.svelte` "Top providers" shows real providers and flags the `legacy: unattributed` bucket.
- **A6.3** `npm --prefix desktop/ui ci && npm --prefix desktop/ui run build` green.
- **A6.4** Migration `002` applied to a **copy**: `SELECT DISTINCT status FROM usage_history` returns exactly one vocabulary; `SELECT COUNT(*) FROM usage_history` is unchanged (45 136 on the live snapshot); second `migrate()` is a no-op (`migrate.rs:117-143`).
- **A6.5** `docs/research/PROJECT-AUDIT-2026-09.md`, `docs/ROADMAP-2026-09.md` §P1-7 and `STATUS.md` updated; `CHANGELOG.md` carries the six behaviour changes from §7.5.
- **A6.6** `heterion-router-providers/src/lib.rs:3` "277 providers" corrected to 278.

---

## 10. Summary of corrections to `PROJECT-AUDIT-2026-09.md`

| Audit statement | Status |
|---|---|
| Registry metrics (278/2717, 259/2108, 253/1916, 6/192, 19/609, 61/959, 10 claude) | **all confirmed exactly** (§1.2) |
| 19 route paths / 21 method-handler pairs | **confirmed** — 19 distinct paths, 21 pairs (`/api/keys` and `/v1/responses/{id}` are the two doubles) |
| 19 live backends; `heterion-local` has no active row | **confirmed and reconciled** against `gateway.log` (§2.4) |
| 8 combos; `vivanta-auto`/`vivanta-deepseek` empty | **confirmed**; now shown to be the cause of all 11 logged Rust requests (§6.3) |
| "status in logs synthetic" | **confirmed and materially extended** — `usage_history.status` holds two disjoint vocabularies across 45 136 rows, a compatibility problem the audit does not mention (§6.2) |
| Grok fallthrough (gap #5) | **narrower than stated** — requires an alias-stored row; the plain "empty pool" case is already skipped (§2.2 D3) |
| Alias-keyed connections unroutable | **confirmed as a code path**, with **zero** live rows currently exercising it (§3, §2.4) |
| *Not in the audit* | `exa-search` (5 active rows, not in the registry, §2.3); `usage_history.combo_strategy` mislabelled `direct` for all 328 Rust rows (§6.2); the live DB needs a WAL-aware copy to audit (§0.3); HEAD registry is 278/2715, not 2717 (§1.2); `heterion-router-providers/src/lib.rs:3` says 277 |

# OmniRoute → heterion-router parity

> **Audit date:** 2026-10-02
> **Upstream baseline:** frozen OmniRoute **v3.8.50** — `/Volumes/Netac_SSD/ai-workstation/Projects/OmniRoute-Rust`
> (Next.js 15 / Node 24 app; last commit `c77542e5fc docs: archived 2026-09-19 — continues as heterion-router`).
> Frozen build artifacts: `/Volumes/Netac_SSD/ai-workstation/Archives/OmniRoute/releases/OmniRoute-release-3.8.51-2026-09-09.tgz`.
> Legacy live data (read-only): `/Users/egorich/.omniroute/storage.sqlite` (schema of 132 base tables, 351 indexes, 28 triggers).
> **Target:** `heterion-router` working tree, baseline commit `51dfe6d` + 35 modified / 5 untracked (per `STATUS.md:19`).
> **Method:** read-only. No production code, test, Cargo file or database was modified. No commit.

## Headline numbers

| Metric | Upstream OmniRoute 3.8.50 | heterion-router | Ratio |
|---|---|---|---|
| Providers in catalog | **352** (registry README:590) / 290 in frozen README:586 | 278 entries (`registry.json`) | 0.79× |
| Models in catalog | **2 554** provider-model pairs / 1 283 raw ids | 2 717 model entries | 1.06× |
| Provider executors (transports) | **~60** (141 `.ts` files, `open-sse/executors/`) | **4** (`openai`, `openai-responses`, `gemini`, `grok-cli`) | 0.07× |
| HTTP route handlers | **608** (`src/app/api/**/route.ts`) | **17** (`crates/heterion-router-gateway/src/lib.rs:146-168`) | 0.03× |
| `/v1/*` protocol surfaces | **~35** (chat, responses, messages, count_tokens, embeddings, images×2, audio×3, moderations, rerank, search, ocr, files×3, batches×5, videos, music, quotas×2, web/fetch, ws, v1beta×2, vscode×20, relay×2, …) | **4** (`/v1/models`, `/v1/chat/completions`, `/v1/responses`, `/v1/responses/{id}`) | 0.11× |
| Routing strategies | **19** (README:329-430) | **1 effective** (`priority`); `auto` accepted then ignored | 0.05× |
| Compression engines | **12** (README:778-792) | **0** | 0× |
| Base DB tables | **132** | **17** (of which 11 are dead schema) | 0.13× |
| Dashboard pages | **55** (`src/app/(dashboard)/dashboard/`) | **7 tabs** (`desktop/ui/src/App.svelte:12-20`) | 0.13× |
| CLI commands | **80+** (README:695) | **0** (no arg parser; env vars only) | 0× |
| Test files | **3 723** (`*.test.ts`) | **215** `#[test]`/`#[tokio::test]` | 0.06× |
| MCP tools | 84–104 (`open-sse/mcp-server/tools/*.ts`) | **0** | 0× |
| i18n locales | **43** (README:124-168) | **0** (English only) | 0× |
| Rust LOC (crates + desktop shell + ui src) | — | **17 938** | — |

**Verdict.** heterion-router is a competent, well-tested **narrow gateway**, not a port. It reproduces one
upstream slice — OpenAI-compatible chat + Responses over a handful of HTTP upstreams, with combo
failover — at production quality. Everything orthogonal to that slice (protocol breadth, routing
strategies, compression, MCP/A2A, memory, quota/rate policy, multi-user, dashboard depth, CLI,
backup/migration, i18n) is absent, and the live DB proves the absent parts were heavily used
(3 723 test files, 132 tables, 132 settings keys).

---

## Feature matrix

Status vocabulary: **done** = equivalent capability exists · **partial** = exists but narrower or
less correct · **worse** = exists but a materially different/lesser implementation ·
**missing** = no implementation, zero matches.

### 1. Provider registry & credentials

| Feature | Upstream OmniRoute (evidence) | heterion-router (evidence) | Status | Priority | Effort |
|---|---|---|---|---|---|
| Provider catalog breadth | README:590 "290 providers", web-confirmed 352; `open-sse/executors/` 141 files | `crates/heterion-router-providers/registry.json` — 278 entries, 2 717 models (parsed) | partial | P2 | S |
| Provider *transports* (executors) | `open-sse/executors/{codex,cursor,kiro,antigravity,bedrock,cloudflare-ai,github,gitlab,vertex,qoder,grok-cli,chatgpt-web-codex,polllinations,cliproxyapi,…}.ts` — ~60 | 4 impls: `openai.rs:23`, `responses.rs:26`, `gemini.rs:25`, `grok_cli.rs:1` | worse | P1 | L |
| Executor field honoured | executor per provider drives URL/headers/refresh (`docs/architecture/ARCHITECTURE.md` executor table) | `executor` parsed into `ProviderEntry.executor` (`registry.rs:24`) but **never read to select a transport** — only asserted present in tests (`registry.rs:267-272`) | worse | P1 | L |
| API-key providers | `auth_type: apikey` (49 of 61 rows live in `.omniroute`) | `auth_type: apikey` — 236 of 278 registry entries; `credentials.rs:28-34` picks `access_token` then `api_key` | done | — | — |
| OAuth providers | 13–16 OAuth modules (`src/lib/oauth/providers/`); 12 live `oauth` connections; `/api/oauth/[provider]/[action]` | 21 registry entries declare `oauth`, but **no OAuth flow exists** — read-only credential load (`credentials.rs:40-109`). Refresh only for grok-cli, bespoke form login (`grok_cli.rs`) | worse | P1 | L |
| OAuth login / relogin | `/api/oauth/*` (30+ routes incl. `kiro/social-authorize`, `cursor/import`, `agy-auth/import-bulk`, `codex/import-token`) | none | missing | P2 | L |
| Credential encryption at rest | `src/lib/db/encryption.ts`, AES-256-GCM, `enc:v1:` | `crates/heterion-router-crypto/src/lib.rs` — `FieldCrypto`, `enc:v1:` + salt `omniroute-field-encryption-v1` | done | — | — |
| Provider CRUD (UI + API) | `/api/providers`, `/api/providers/[id]`, `/api/providers/bulk`, `/api/oauth/cliproxy-import`, `/api/providers/[id]/test` | `GET /api/connections` + `PATCH /api/connections/{id}` (only `name`, `is_active`, `priority`) — `lib.rs:151-152`, `admin.rs:586-593` | partial | P2 | M |
| Provider model sync / discovery | `/api/providers/[id]/sync-models`, `/api/models/catalog`, `/api/openrouter-catalog`, `/api/discovery/*`, `/api/free-models`, `model_intelligence`/`model_learning` tables | none — model list comes from the frozen `registry.json` | missing | P2 | L |
| Custom provider nodes | `provider_nodes` table; `/api/provider-nodes`, `/api/provider-nodes/validate` | `provider_nodes` table declared (`schema_base.sql:51`) with **0 Rust references** | missing | P3 | M |
| Provider health / test | `/api/providers/[id]/test`, `/api/models/test-all`, `/api/provider-stats`, `/api/provider-metrics` | none; `/healthz` reports only the static backend name (`lib.rs:173-179`) | missing | P2 | M |

### 2. Protocol endpoints

| Feature | Upstream OmniRoute (evidence) | heterion-router (evidence) | Status | Priority | Effort |
|---|---|---|---|---|---|
| OpenAI Chat Completions | `src/app/api/v1/chat/completions/route.ts` | `lib.rs:163` → `chat_completions`; `openai.rs` 881 lines | done | — | — |
| OpenAI Responses API | `src/app/api/v1/responses/route.ts` + `[...path]` (create/get/delete/cancel) | `lib.rs:164-168` create/get/delete; `responses.rs` 369 lines. No `cancel` | partial | P1 | M |
| Responses streaming fidelity | full lifecycle | text/reasoning deltas + terminal usage only — `STATUS.md:38-40` ("нет полного `response.created`/`output_item.done`/argument-delta lifecycle") | partial | P0 | M |
| Responses state store | `/api/v1/responses` persists envelopes | owner-scoped store, **unbounded, no TTL/size retention, not written for streams** — `STATUS.md:148-150` | partial | P1 | M |
| **Anthropic Messages (`/v1/messages`)** | `src/app/api/v1/messages/route.ts` | **no route.** `grep -rIn "/v1/messages" crates/*/src desktop/*/src` → **0 matches** | **missing** | **P0** | L |
| **`/v1/messages/count_tokens`** | `src/app/api/v1/messages/count_tokens/route.ts` | `grep -rIn "count_tokens"` → **0 matches** | **missing** | P1 | S |
| Gemini native protocol | `gemini` + `gemini-cli` executors; `format: gemini` | `gemini.rs:25` `GeminiBackend`, `generateContent`, thought-signature file (`main.rs:154`) | partial | P1 | M |
| Gemini inbound endpoint | `/v1beta/models`, provider-side Gemini route | `grep -rIn "/v1beta"` → 1 match, not a route; no Gemini-native inbound route | missing | P3 | M |
| Claude wire translation (outbound) | `open-sse/translator/` full registry | `crates/heterion-router-translate/src/claude.rs` — **text path only**, "Explicitly deferred: tool_use/input_json synthesis … signature handling" (`claude.rs:9-11`) | partial | P1 | M |
| **Embeddings** | `src/app/api/v1/embeddings/route.ts` (6 providers) + `/api/v1/providers/[provider]/embeddings` | `grep -rIn "embeddings"` over all Rust `src/` → **1 match, a doc comment** (`config.rs:38`), no route | **missing** | **P1** | M |
| **Images** | `/api/v1/images/generations`, `/api/v1/images/edits`, `/api/v1/chatgpt-web/image/[id]` | `grep -rIn "/v1/images"` → **0 matches** | **missing** | P2 | L |
| **Audio** | `/api/v1/audio/{speech,transcriptions,translations}` | `grep -rIn "/v1/audio"` → **0 matches** | **missing** | P2 | L |
| **Moderations / rerank / search / ocr** | `/api/v1/{moderations,rerank,search,ocr}` | `grep -rIn` for `/v1/moderations`, `/v1/rerank`, `/v1/search`, `/v1/ocr` → **0 matches each** | **missing** | P2 | M |
| **Files / batches** | `/api/v1/files/*`, `/api/v1/batches/*` (5 routes), `batches` + `batch_item_checkpoints` tables | `grep -rIn "/v1/files"`, `"/v1/batches"` → **0 matches each** | **missing** | P2 | L |
| Videos / music | `/api/v1/videos/generations`, `/api/v1/music/generations` | `grep -rIn "/v1/videos"`, `"/v1/music"` → **0 matches each** | **missing** | P3 | L |
| Legacy `/v1/completions` | `src/app/api/v1/completions/route.ts` | `grep -rIn "/v1/completions"` → **0 matches** | missing | P3 | S |
| Quotas | `/api/v1/quotas`, `/api/v1/quotas/check` | none | missing | P2 | M |
| Per-provider pass-through routes | `/api/v1/providers/[provider]/{chat/completions,embeddings,images/generations,limits,models}` | none — only combined `provider/model` in the model id (`routing.rs:160-171`) | partial | P2 | M |
| Provider relay endpoints | `/api/v1/relay/chat/completions`, `…/bifrost` | none | missing | P3 | M |
| VSCode-extension compat | `/api/v1/vscode/**` (20 routes) + `/api/v1/vscode/combos/[token]` | none | missing | P3 | L |
| WebSocket / remote fetch | `/api/v1/ws`, `/api/v1/web/fetch` | `grep -rIn "/v1/ws"` → **0 matches** | missing | P3 | M |
| Routing decision trace header | `X-OmniRoute-Decision: strategy=;provider=;latency_ms=` on every completion (`docs/openapi.yaml`) | none | missing | P2 | S |
| Compression echo header | `X-OmniRoute-Compression: <mode>; source=<source>` (README:833) | none | missing | P3 | S |
| `X-OmniRoute-Compression` request override | per-request `x-omniroute-compression` header (README:833) | none | missing | P3 | S |

### 3. Routing, combos, resilience

| Feature | Upstream OmniRoute (evidence) | heterion-router (evidence) | Status | Priority | Effort |
|---|---|---|---|---|---|
| Combos (model chains) | `/api/combos`, `/api/combos/[id]`, `/api/combos/reorder`, `/api/combos/builder/options`, `/api/combos/test`, `/api/combos/metrics`, `/api/combos/auto` | `combos.rs` parse + `plan_steps`; `GET/PATCH /api/combos` (`lib.rs:149-150`) | partial | P2 | M |
| Structured combo steps (`provider`+`model`+`connection`) | combos `data` carries per-step `providerId`; live DB `combos.data` has `capabilities`, `isActive`, `context_cache_protection`, `context_length`, `sortOrder` | `parse_combo_rich` reads `kind`,`model`,`providerId`,`id`,`weight` only (`combos.rs:92-143`); **connection id ignored** | worse | P2 | M |
| Routing strategies | **19**: priority, fill-first, weighted, round-robin, p2c, least-used, random, strict-random, cost-optimized, headroom, reset-window, reset-aware, context-relay, context-optimized, cache-optimized, lkgp, auto, fusion, pipeline (README:329-430) | `is_known_strategy` accepts exactly `priority` \| `auto` (`admin.rs:582-584`); both follow list order — `combos.rs:7-8` "scored auto-routing is deferred". `grep -rIio` for `weighted`,`round_robin`,`p2c`,`least_used`,`cost_optimized`,`headroom`,`reset_window`,`lkgp`,`auto_combo`,`fusion`,`judge`,`pipeline` → **0 matches each** | **worse** | **P0** | L |
| `auto/*` zero-config routing | `auto`, `auto/coding`, `auto/fast`, `auto/cheap`, `auto/offline`, `auto/smart`, `auto/lkgp`, `auto/chaos`; 16-factor scoring (`docs/routing/AUTO-COMBO.md`) | `grep -rIio "auto_combo"` → **0 matches**; no `auto` model id in `list_models` | **missing** | **P1** | L |
| `/v1/auto-combo/{channel}/candidates` | `src/app/api/v1/auto-combo/[channel]/candidates/route.ts` | none | missing | P2 | M |
| Circuit breaker | 3-layer: provider breaker (per auth-type thresholds 3×/5×/2×, 60/30/15 s HALF-OPEN), connection cooldown (exp ×2, anti-thundering herd, `Retry-After`), model lockout (README:438) | single in-memory `BreakerSet`, one global threshold+cooldown, Closed/Open only (`breaker.rs:13-18`, `routing.rs:84`); **no persistence, no HALF_OPEN, no per-auth-type thresholds, no `Retry-After`** | worse | P1 | M |
| Breaker state persisted | `domain_circuit_breakers` table | `breaker.rs` is `HashMap` in RAM (`breaker.rs:77`); `domain_circuit_breakers` table declared with **0 Rust references** | missing | P1 | S |
| Connection cooldown / lockout | `domain_lockout_state`, `provider_quota_state`, `relay_rate_limits` tables; `modelLockout` setting | `grep -rIio "rate_limit"` → **1 match**, a test fixture string (`grok_cli.rs:785`) | missing | P1 | M |
| Cooldown-aware retry | `src/sse/services/cooldownAwareRetry.ts`, `requestRetry` / `maxRetryIntervalSec` settings | transient retry (429/5xx) with `maxRetries`/`retryDelayMs` (`routing.rs:138-149`, `232-241`); `maxSetRetries`/`setRetryDelayMs` parsed, **unused** (`combos.rs:26-28`) | partial | P1 | M |
| Stream failover | pre-first-byte failover implemented | yes — `routing.rs:274-320` | done | — | — |
| Upstream 4xx does not trip breaker | model-local failure handling | `is_backend_outage` only trips on ≥500 or transport error (`routing.rs:44-55`) | done | — | — |
| Fallback chains (domain) | `domain_fallback_chains`, `domain_fallback_chains` API `/api/fallback/chains` | table declared (`schema_base.sql:196`) with **0 Rust references** | missing | P3 | S |
| Context relay / handoff | `context-relay` strategy; `context_handoffs` table; `/api/context/analytics` | `grep -rIio "context_relay"` → **0 matches** | missing | P3 | L |
| Fusion (panel + judge) | `open-sse/services/fusion.ts` | `grep -rIio` `fusion`,`judge` → **0 matches each** | missing | P3 | L |
| Wildcard / tag routing | `open-sse/services/wildcardRouter.ts`, `src/domain/tagRouter.ts` | prefix matching only (`router.rs:41-49`) | partial | P3 | M |
| Per-model provider pin (`/v1/models/:model`) | `src/app/api/v1/models/[...model]/route.ts`; `modelAliases` key_value namespace (94 live rows) | none — only `PATCH /api/connections/{id}` and model-prefix rules | missing | P2 | M |
| Exclusive session leases | `exclusive_connection_leases`, `session_account_affinity` tables | none | missing | P2 | M |

### 4. Cost, usage, accounting

| Feature | Upstream OmniRoute (evidence) | heterion-router (evidence) | Status | Priority | Effort |
|---|---|---|---|---|---|
| Usage row write | `usage_history` (26 cols incl. `ttft_ms`, `service_tier`, `account_label`) | `usage/src/records.rs:75-104`, 14 of 26 columns; `ttft_ms`/`service_tier` never written | partial | P2 | S |
| Call log write | `call_logs` (37 cols: `detail_state`, `artifact_*`, `has_request_body`, `correlation_id`, `combo_step_id`, `source_format`, `target_format`) | `records.rs:44-72`, 15 cols; `correlation_id`, `artifact_*`, `detail_state`, `combo_step_id` never written | partial | P2 | M |
| **Attribution** | provider/connection/combo/requested-model on every row | **`provider="routing"`, `connection_id=NULL`, synthetic status 200/500** — `routing.rs:6-7`, `STATUS.md:67-70,117` | **worse** | **P0** | M |
| Pricing catalog | `/api/pricing`, `/api/pricing/models`, `/api/pricing/sync`, `pricing_synced` key_value ns (106 live rows) | **5 hard-coded models**: grok-4.6, grok-4.5, gpt-4o-mini, claude-sonnet-4.6, gemini-2.5-flash (`usage/src/pricing.rs:33-89`); `set_override` never called from any handler | worse | P1 | M |
| Cost formula | `costCalculator.ts` | `pricing.rs:109-126` mirrors it (incl. reasoning delta) | done | — | — |
| Cost aggregation views | `/api/usage/*` (23 routes: history, om-usage, utilization, provider-limits, model-latency-stats, quota, request-logs, proxy-logs, route-explain) | 1 endpoint `GET /api/usage?days=&limit=` (`admin.rs:405`) | partial | P2 | M |
| Rollups | `daily_usage_summary`, `hourly_usage_summary`, `token_ledger`, `quota_consumption`, `pin_effectiveness` | `grep -rIio` `token_ledger`,`pin_effectiveness` → **0 matches each** | missing | P3 | M |
| Proxy logs | `proxy_logs` table (17 cols) + `/api/usage/proxy-logs`; 3-level proxy, TLS fingerprint | `proxy_logs` table declared (`schema_base.sql:172`) with **0 Rust references** | missing | P3 | M |
| Request detail capture | `request_detail_logs`, `call_logs.has_request_body`, `log_export_destinations`, `/api/logs/{id}`, `/api/logs/export`, `/api/logs/detail` | `GET /api/logs` returns summary rows only (`admin.rs:449`); no detail/export route | partial | P2 | M |
| Audit log | `audit_log`, `config_audit_log`, `xp_audit_log`, `/api/compliance/audit-log` | `grep -rIio "audit_log"` → **0 matches** | missing | P2 | M |

### 5. Caching & compression

| Feature | Upstream OmniRoute (evidence) | heterion-router (evidence) | Status | Priority | Effort |
|---|---|---|---|---|---|
| Semantic cache | `semantic_cache` table (signature, prompt_hash, hit_count, expires_at) | table declared (`schema_base.sql:250`) with **0 Rust references** | missing | P1 | M |
| Reasoning cache | `reasoning_cache` table; `/api/cache/reasoning` | `grep -rIio "reasoning_cache"` → **0 matches** | missing | P2 | M |
| Cache stats / metrics | `cache_metrics`, `/api/cache/stats`, `/api/cache/entries` | none | missing | P3 | S |
| Signature cache (request dedup) | `open-sse/services/signatureCache.ts` | none | missing | P2 | M |
| **Token compression (12 engines)** | Session-Dedup, CCR, Lite, RTK, Responses-Tool-Output, Headroom, Relevance, Caveman, Aggressive, LLMLingua-2, Ultra, OmniGlyph (README:778-792); `compression_*` (7 tables), `ccr_blocks`, 32 live `compression` settings | `grep -rIio` `compression` → **2 matches, both the inert column `api_keys.compression_enabled`** (`migrate.rs:81`, `repos.rs:235`); `caveman`,`rtk`,`llmlingua` → **0 matches each** | **missing** | **P2** | L |
| Adaptive context budget dial | `reserve-output`/`percentage`/`absolute`, `floor`/`replace-autotrigger`/`off` (README:832) | none | missing | P3 | M |
| Output styles | Terse prose, Less code, Terse CJK (README:828-831) | none | missing | P3 | S |
| Compression eval harness | `npm run eval:compression`; `eval_suites`/`eval_cases`/`eval_runs` tables | `grep -rIio "eval"` → 1 match, the word "evaluated" in a SQL comment (`repos.rs:846`) | missing | P3 | M |
| Upstream Rust `compression-core` | `compression-core/crates/{tokenizer,core-api,ffi,tests,bench}` (upstream already started the port) | **not vendored, not a workspace member** — `Cargo.toml` has 10 crates, none is `compression-core` | missing | P2 | M |

### 6. Prompt / transform pipeline

| Feature | Upstream OmniRoute (evidence) | heterion-router (evidence) | Status | Priority | Effort |
|---|---|---|---|---|---|
| Hub-and-spoke format translation | `open-sse/translator/registry.ts` | `crates/heterion-router-translate/src/` — `claude`, `claude_request`, `gemini`, `gemini_request`, `requests`, `responses`; OpenAI as pivot (`lib.rs:1-3`) | partial | P1 | L |
| Reasoning/signatures replay | `contextHandoff`, `thinkingBudget`, Claude signatures | Gemini thought signatures persist to `gemini-thought-signatures.jsonl` (`main.rs:154`); fallback is **process-global, not per-session** (`STATUS.md:46-47`); Claude signatures not ported (`claude.rs:9-11`) | partial | P1 | M |
| System-prompt injection | `open-sse/services/systemPrompt.ts` | none | missing | P3 | S |
| Thinking budget | `open-sse/services/thinkingBudget.ts` | none | missing | P3 | S |
| PII sanitization of SSE | `open-sse/handlers/chatCore/streamingPipeline.ts:91` | none | missing | P2 | M |
| Idempotency / request hashing | `open-sse/handlers/chatCore.ts:608` | none | missing | P3 | S |
| Guardrails / injection middleware | `/api/guardrails`, `/api/guardrails/test`; `src/middleware/` | `grep -rIio "guardrail"` → **0 matches** | missing | P2 | M |
| Skills injection | `skills` table, `/api/github-skills`, `/api/agent-skills/*`, `skill_executions` | `grep -rIio "skill"` → **0 matches** | missing | P2 | L |
| Memory (extraction/retrieval/RAG) | `memories`, `memory_fts*`, `vec_memories*`, `memory_vec_meta`; `/api/memory/*` (7 routes); qdrant settings | `grep -rIio "qdrant"` → **0 matches**; `memory` → 31 matches, **all** `open_in_memory`/SQLite `temp_store` | missing | P2 | L |
| Tool calling | client function tools, built-in tools | client tools work; **built-in tools silently drop** (`STATUS.md:148-149`) | partial | P1 | M |
| Multimodal (vision/audio in) | v3.8.50 modality bridge (vision + audio) | none | missing | P3 | L |
| Structured output | Gemini `response_format`, Responses `text.format` | implemented (`responses.rs`) | partial | P2 | S |

### 7. Dashboard & admin/CRUD API

| Feature | Upstream OmniRoute (evidence) | heterion-router (evidence) | Status | Priority | Effort |
|---|---|---|---|---|---|
| Admin API surface | **608** route handlers, `docs/openapi.yaml` (233 paths) | **17** routes total (`lib.rs:146-168`) | worse | P1 | L |
| Dashboard pages | **55** (`ls src/app/(dashboard)/dashboard/`: a2a, acp-agents, activity, agent-skills, analytics, api-endpoints, api-manager, audit, auto-combo, batch, cache, changelog, chaos, cli-agents, cli-code, cloud-agents, combos, compression, context, costs, discovery, endpoint, free-provider-rankings, free-tiers, gamification, health, leaderboard, limits, logs, mcp, media-providers, memory, omni-skills, onboarding, playground, plugins, profile, provider-stats, providers, quota, relay, runtime, search-tools, settings, system, tokens, tools, translator, usage, webhooks) | **7 tabs** (`App.svelte:12-20`): overview, combos, providers, keys, usage, logs, playground | worse | P2 | L |
| Key management | `/api/keys`, `[id]`, `[id]/devices`, `[id]/regenerate`, `[id]/reveal`, `[id]/usage-limits`, `/api/keys/groups/**` | `GET/POST /api/keys`, `[id]/revoke`, `[id]/restore` (`lib.rs:156-158`) | partial | P2 | M |
| Key hash issuance | `key_hash` SHA-256 + `key_prefix` | yes, interoperable with JS (`admin.rs:237-239`) | done | — | — |
| Key groups / permissions | `key_groups`, `key_group_members`, `group_model_permissions` tables; `/api/keys/groups/**` | none; `allowed_models`/`allowed_combos` per key only | missing | P2 | M |
| Combo CRUD | 7 combo routes incl. builder, test, reorder, metrics | 2 (`GET`, `PATCH`) | partial | P2 | M |
| Playground | `/api/playground/{presets,improve-prompt,simulate-route}` + `playground_presets` table | `Playground.svelte` view exists, client-side only; no presets API, no `playground_presets` table | partial | P3 | S |
| Settings API | 44 live `settings` keys, `/api/*` setters, `/dashboard/settings` (General/Appearance/Security/Routing/Resilience/Advanced) | env vars only (`config.rs`); no settings read/write endpoint | missing | P2 | M |
| Health / diagnostics | `/api/health/{ping,degradation}`, `/api/db/health`, `/api/monitoring/health`, `/api/network/info` | `/healthz` only (`lib.rs:147`) | partial | P3 | S |
| Plugins | `plugins`, `plugin_metrics`, `plugin_analytics` tables; `/api/plugins/**` | `grep -rIio "plugin"` → 8 matches, **all** `tauri_plugin_*` Rust crate names | missing | P3 | L |

### 8. Auth, multi-user, policy

| Feature | Upstream OmniRoute (evidence) | heterion-router (evidence) | Status | Priority | Effort |
|---|---|---|---|---|---|
| Bearer API-key auth | `/api/keys` | `auth.rs:95-139` extractor; DB failure → 503 fail-closed (`auth.rs:110,136`) | done | — | — |
| Key scopes | 31 MCP scopes; `scopes`, `allowed_combos`, `allowed_quotas` columns | `manage` scope gate (`auth.rs:71-93`); `allowed_quotas`, `disable_non_public_models`, `usage_limit_enabled`, `max_sessions` columns exist but **never enforced** | partial | P1 | M |
| Model allow-list | per-key model policy | `allows_model` glob matcher (`auth.rs:44-58`), applied to Chat/Responses | done | — | — |
| **User login / sessions** | `/api/auth/{login,logout,status,csrf}`, `/api/auth/oidc/{login,callback}`, `forgot-password` page; live setting `requireLogin`, `password` | `grep -rIio "pkce"` → 0; no session/cookie/CSRF/OIDC code — dashboard is a paste-a-key form (`App.svelte:57-64`) | **missing** | P2 | L |
| **OIDC / SSO** | `/api/auth/oidc/login`, `/api/auth/oidc/callback` | none | **missing** | P3 | M |
| Multi-user / org | `user_levels`, `invite_tokens`, `tier_assignments`, `user_badges`, `key_groups`, `leaderboard` tables | none | missing | P3 | L |
| IP filtering | `open-sse/services/ipFilter.ts`, `/dashboard/settings` Security tab | none | missing | P2 | M |
| Provider blocking | `blockedProviders` setting, `/api/policies` | none | missing | P3 | S |
| Non-loopback guard | binding policy | non-loopback bind without a key refuses to start (`STATUS.md:59-60`) | done | — | — |
| CORS | Next.js middleware + `proxy-port-clash-report.md` | explicit origin predicate (`cors.rs:18-40`), loopback + tauri schemes only | done | — | — |
| Field encryption | AES-256-GCM | done (`crates/heterion-router-crypto`) | done | — | — |
| SSRF / safe outbound fetch | `src/shared/network/safeOutboundFetch.ts`, `outboundUrlGuard.ts` (v3.6.6+) | none | missing | P2 | M |

### 9. Rate limiting, quota, resilience policy

| Feature | Upstream OmniRoute (evidence) | heterion-router (evidence) | Status | Priority | Effort |
|---|---|---|---|---|---|
| Per-account rate limiting | `api_key_token_limits`, `provider_key_limits`, `account_key_limits`, `relay_rate_limits`, `learnedRateLimits` setting | none | missing | P1 | M |
| Quota tracking | `provider_quota_state`, `provider_quota_reset_events`, `quota_snapshots`, `quota_pools/groups/allocations/consumption`, `quotaVisibility` setting | `quota_snapshots` table declared (`schema_base.sql:264`) with **0 Rust references** | missing | P1 | M |
| Per-key USD budgets | `domain_budgets`, `domain_budget_reset_logs`, `domain_cost_history` | all three tables declared (`schema_base.sql:201-234`) with **0 Rust references** | missing | P1 | M |
| Policy engine | `src/domain/policyEngine.ts` (lockout → budget → fallback), `costRules`, `lockoutPolicy`, `fallbackPolicy` | none | missing | P1 | L |
| Per-key token limits | `api_key_token_counters`, `api_key_token_limit_reset_logs` | none | missing | P2 | M |
| Connection concurrency | `exclusive_connection_leases`, `/api/admin/concurrency` | `provider_connections.max_concurrent` column exists, never read | missing | P2 | S |
| Proxy pool / subscriptions | `proxy_registry`, `proxy_subscriptions`, `proxy_assignments`, `free_proxies`, `/api/v1/management/proxies*` (7 routes) | none; `proxy_enabled`/`per_key_proxy_enabled` columns never read | missing | P3 | L |

### 10. Observability, agent protocols, CLI, packaging

| Feature | Upstream OmniRoute (evidence) | heterion-router (evidence) | Status | Priority | Effort |
|---|---|---|---|---|---|
| Structured logging | app log + request logs, `/api/logs/console` | `tracing` + `tracing-appender` file sink; `GET /api/logs/app` (`admin.rs:511`) | partial | P2 | S |
| Metrics / telemetry | live analytics: usage, quota, savings, p95 latency (README:210) | `grep -rIio` `telemetry`,`opentelemetry` → **0 matches each**; no Prometheus endpoint | missing | P2 | M |
| **MCP server** | `omniroute --mcp` (stdio), `/api/mcp/stream`, `/api/mcp/sse`, `/api/mcp/tools`, `/api/mcp/audit`; **84–104 tools**; live settings `mcpEnabled`,`mcpTransport` | `grep -rIio "mcp"` → **2 matches, both test data** (`lib.rs:500,1462`) | **missing** | **P2** | L |
| **A2A server** | `/.well-known/agent.json`, `/api/a2a/{status,tasks,tasks/[id],tasks/[id]/cancel}`, `src/lib/a2a/` (JSON-RPC 2.0 + SSE, task lifecycle); live setting `a2aEnabled` | `grep -rIio "a2a"` → **0 matches** | **missing** | P2 | L |
| **Webhooks** | `/api/webhooks`, `[id]`, `[id]/deliveries`, `[id]/test`, `/api/webhooks/validate-url`; `webhooks` + `webhook_deliveries` tables | `grep -rIio "webhook"` → **0 matches** | **missing** | P2 | M |
| **CLI** | **80+ commands**: `chat` (TUI), `setup` (wizard), `doctor`, `connect`, `models`, `providers`, `oauth`, `combo`, `cache`, `compression`, `cost`, `usage`, `quota`, `health`, `telemetry`, `logs`, `audit`, `mcp`, `a2a`, `memory`, `skills`, `eval`, `backup`, `sync`, `webhooks`, `policy`, `pricing`, `translator`, `simulate` (README:695,724) | **no CLI**: no `clap` in `Cargo.toml`; entrypoint is `main()` + `std::env::var` only | **missing** | P2 | L |
| CLI-tool config writer | `/api/cli-tools/*` (33 routes: claude/codex/cursor/cline/crush/qwen/grok/… settings) | none | missing | P3 | L |
| Remote mode / scoped tokens | `omniroute connect <host>`, `/api/cli/{connect,tokens,whoami}`, `cli_access_tokens` table | none | missing | P3 | L |
| Tunnel / remote access | Cloudflare Quick Tunnel, `/api/local/redis/*` | none | missing | P3 | M |
| **DB backup / restore** | `/api/db-backups`, `/export`, `/exportAll`, `/import`, `bin/{snapshot-data,restore-data,rollback}.sh` | `grep -rIio "backup"` → **0 matches** | **missing** | **P2** | M |
| **JS → Rust data migration** | JS owns `storage.sqlite` | Rust creates its own DB at `~/.heterion-router/storage.sqlite`; `MIGRATIONS` has exactly one entry `"001"` (`migrate.rs:32-36`) — no importer for the 132-table legacy DB. Live instance still runs `com.omniroute.rust` on the legacy DB (`Archives/OmniRoute/README.md:36`) | **missing** | **P1** | L |
| Schema-migration runner | 132 tables, `_omniroute_migrations` ledger | ledger exists, 1 migration, plus 19 additive `api_keys` columns via `PRAGMA table_info` (`migrate.rs:62-111`) | partial | P2 | M |
| Desktop packaging | Electron desktop app + PWA + Termux | Tauri v2 sidecar (`desktop/src-tauri/tauri.conf.json`), single-instance, parent-watch; **adhoc-signed, not notarized; fresh clone cannot build** (`STATUS.md:161-164`) | partial | P2 | M |
| Desktop CSP | Next.js headers | `"csp": null` (`tauri.conf.json:20-22`) with an unused `shell:allow-spawn` capability | worse | P1 | S |
| Service supervision | launchd job `com.omniroute.server` | launchd `com.heterion.router` — **plist is not in the repo**, machine-local only (`STATUS.md:161-162`) | partial | P2 | S |
| **Tests** | **3 723** `*.test.ts` files; README:199 "25,000+ tests"; codecov, size-limit, trivy, gitleaks, zizmor, vale, mergify, husky | **215** `#[test]`/`#[tokio::test]`; `scripts/ci/check.sh` = exec-bits + fmt + clippy + test. 22 `wiremock.expect()` without `verify()` (`STATUS.md:165-167`) | partial | P1 | L |
| **CI** | `.github/` full matrix, security scanners, coverage | 2 workflows (`ci.yml`, `release.yml`); **no desktop job**, no coverage, no e2e, no security scan; CI uses `@stable` vs pinned `1.98.0` (`STATUS.md:138-141`) | partial | P1 | M |
| **i18n** | **43 locales**, RTL, translation workflow (`docs/guides/I18N.md`) | `grep -rIio "i18n"` → **0 matches**; UI hardcoded English | missing | P3 | M |
| Chaos engineering | `/api/chaos/{config,run}`, `auto/chaos` variant | `grep -rIio "chaos"` → **2 matches, both the inert column `api_keys.chaos_mode_enabled`** (`migrate.rs:78`, `repos.rs:234`) | missing | P3 | M |
| Gamification | `badge_definitions`, `user_badges`, `xp_action_counts`, `invite_tokens`, `/api/gamification/*` (14 routes) | `grep -rIio` `gamification`,`leaderboard` → **0 matches each**; README:92 lists gamification as a deliberate non-port | missing (accepted) | P3 | L |
| Billing / Telegram pages | `/dashboard/settings` billing, Telegram integration | README:92 lists as deliberate non-port | missing (accepted) | P3 | L |

---

## Missing capabilities

Each item is backed by an exact command whose output is quoted. All greps ran from the
repository root over `crates/*/src` + `desktop/src-tauri/src` + `desktop/ui/src`, excluding
test fixtures where noted. Positive controls are listed first to prove the grep was live.

### Control — the greps are not silently broken

```
$ D=(crates/heterion-router-gateway/src crates/heterion-router-routing/src \
     crates/heterion-router-usage/src crates/heterion-router-db/src \
     crates/heterion-router-providers/src crates/heterion-router-translate/src \
     crates/heterion-router-config/src crates/heterion-router-crypto/src \
     crates/heterion-router-core/src crates/heterion-router-http/src \
     desktop/src-tauri/src desktop/ui/src)
$ for pat in weight breaker combo tracing sqlite sse encrypt strategy; do
    printf "  %-14s hits=%s\n" "$pat" "$(grep -rIin -- "$pat" "${D[@]}" | wc -l | tr -d ' ')"; done
  weight         hits=42
  breaker        hits=83
  combo          hits=260
  tracing        hits=39
  sqlite         hits=38
  sse            hits=604
  encrypt        hits=62
  strategy       hits=40
```

### M1 — No Anthropic Messages endpoint (P0)

The whole gateway router is 22 lines; it has no `/v1/messages`. `translate/claude.rs` is an
*outbound* converter used when an upstream speaks Claude, not an inbound protocol surface.

```
$ grep -rIn "/v1/messages" crates/heterion-router-gateway/src \
    crates/heterion-router-core/src crates/heterion-router-translate/src \
    crates/heterion-router-http/src desktop/ui/src desktop/src-tauri/src | wc -l
0
$ grep -rIn "count_tokens"   <same dirs> | wc -l
0
$ grep -rIn "anthropic-version" <same dirs> | wc -l
0
$ grep -rIn "anthropic_version" <same dirs> | wc -l
0
```

The only Claude-related code is explicitly text-only:

```
$ grep -n "deferred" crates/heterion-router-translate/src/claude.rs
9://! Explicitly deferred: tool_use/input_json synthesis, `<think>` tag parsing,
10://! `pendingThinkClose` markers, and signature handling. Unknown event kinds are ignored, never fabricated.
```

`docs/ROADMAP-2026-09.md` row **P1-5 Native Anthropic Messages → "Not started"** agrees.

### M2 — No embeddings endpoint (P1)

```
$ grep -rIn "embeddings" crates/heterion-router-gateway/src \
    crates/heterion-router-core/src crates/heterion-router-translate/src \
    crates/heterion-router-http/src crates/heterion-router-usage/src \
    desktop/ui/src desktop/src-tauri/src
crates/heterion-router-config/src/config.rs:38:    /// Build configuration from an explicit map (tests, embeddings).
```

One hit, and it is a doc comment on a function signature — not a route. Upstream has
`src/app/api/v1/embeddings/route.ts` plus a per-provider variant, and the legacy data dir
still holds an `embeddings/` cache directory.

### M3 — 14 further `/v1/*` protocol surfaces absent (P2)

```
$ for pat in /v1/images /v1/audio /v1/moderations /v1/rerank /v1/search /v1/ocr \
             /v1/files /v1/batches /v1/videos /v1/music /v1/completions /v1/ws; do
    printf "%-18s %s\n" "$pat" "$(grep -rIn -- "$pat" <all rust src dirs> | wc -l)"; done
  /v1/images          0
  /v1/audio           0
  /v1/moderations     0
  /v1/rerank          0
  /v1/search          0
  /v1/ocr             0
  /v1/files           0
  /v1/batches         0
  /v1/videos          0
  /v1/music           0
  /v1/completions     0
  /v1/ws              0
```

For contrast, the four that do exist:

```
$ grep -n "\.route(" crates/heterion-router-gateway/src/lib.rs | sed -n '3,20p'
163:        .route("/v1/chat/completions", post(chat_completions))
164:        .route("/v1/responses", post(responses))
165:        .route(
166:            "/v1/responses/{id}",
167:            get(get_response).delete(delete_response),
168:        )
```

### M4 — 17 of 19 routing strategies absent; `auto` is a no-op (P0)

`admin.rs` will even reject 17 valid upstream strategy names with a 400, and the two it
accepts both execute the same priority walk.

```
$ grep -n -A3 "fn is_known_strategy" crates/heterion-router-gateway/src/admin.rs
582:fn is_known_strategy(strategy: &str) -> bool {
583-    matches!(strategy, "priority" | "auto")
584-}
```

```
$ sed -n '5,9p' crates/heterion-router-gateway/src/combos.rs
5: //! Ports the first slice of combo routing (`open-sse/services/combo.ts`):
6: //! a combo name expands to its ordered model refs (`"provider/model"`),
7: //! each resolved to a concrete `(backend, bare_model)` the gateway can
8: //! serve. Strategies (`priority` vs `auto`) both follow listed order for
9: //! now; scored auto-routing is deferred. Refs whose provider needs a
```

```
$ for pat in weighted round_robin p2c least_used cost_optimized headroom \
             reset_window lkgp auto_combo fusion judge pipeline; do
    printf "%-16s %s\n" "$pat" "$(grep -rIio -- "$pat" "${D[@]}" | wc -l)"; done
  weighted         0
  round_robin      0
  p2c              0
  least_used       0
  cost_optimized   0
  headroom         0
  reset_window     0
  lkgp             0
  auto_combo       0
  fusion           0
  judge            0
  pipeline         0
```

The live DB proves these were used: `key_value` namespace `lkgp` holds **232** rows, and
`settings.providerStrategies` exists.

### M5 — Zero compression engines (P2)

```
$ grep -rIio -- "compression" "${D[@]}"
crates/heterion-router-db/src/migrate.rs:81:    ("compression_enabled", "INTEGER"),
crates/heterion-router-db/src/repos.rs:235:            compression_enabled
$ grep -rIio -- "caveman"   "${D[@]}" | wc -l   # 0
$ grep -rIio -- "rtk"       "${D[@]}" | wc -l   # 0
$ grep -rIio -- "llmlingua" "${D[@]}" | wc -l   # 0
```

Both `compression` hits are the same inert `api_keys.compression_enabled` column — added by
`API_KEYS_COMPAT_COLUMNS` and written with a constant `1`, never read as behaviour:

```
$ sed -n '237,241p' crates/heterion-router-db/src/repos.rs
            chaos_mode_enabled, cache_default_mode, model_access_mode,
            compression_enabled
        ) VALUES (
...
            1
```

Upstream's own `compression-core` Rust workspace is not vendored here.

### M6 — No MCP, no A2A, no webhooks (P2)

```
$ for pat in mcp a2a webhook; do
    printf "%-10s %s\n" "$pat" "$(grep -rIio -- "$pat" "${D[@]}" | wc -l)"; done
  mcp        2
  a2a        0
  webhook    0
$ grep -rIin -- "mcp" "${D[@]}"
crates/heterion-router-gateway/src/lib.rs:500:    "mcp",
crates/heterion-router-gateway/src/lib.rs:1462:            "mcp",
```

Both `mcp` hits are `"mcp"` strings inside test bodies (a backend name / combo fixture), not a
server. README:92 already records `/api/mcp/*` as a deliberate non-port.

### M7 — No CLI at all (P2)

```
$ grep -rn "clap" crates/*/Cargo.toml Cargo.toml            # 0 matches
$ grep -n "env::var" crates/heterion-router-gateway/src/main.rs | head
29:    if let Ok(token) = std::env::var("GROK_CLI_TOKEN")
91:            std::env::var(key).is_ok_and(|value| matches!(value.as_str(), "1" | "true" | "yes"))
101:        let base_url = std::env::var("GROK_CLI_BASE_URL")
138:    if let Ok(base_url) = std::env::var("OPENAI_COMPAT_BASE_URL")
166:            .then(|| std::env::var("HETERION_LOCAL_BASE_URL").ok())
328:    let Some(pid) = std::env::var("HETERION_ROUTER_PARENT_PID")
```

There is no argument parser anywhere: the binary takes no subcommands. Upstream advertises
80+ commands including `chat` (TUI), `setup` (wizard) and `doctor`.

### M8 — No DB backup / restore and no legacy-data importer (P1/P2)

```
$ grep -rIio -- "backup" "${D[@]}" | wc -l
0
$ sed -n '32,36p' crates/heterion-router-db/src/migrate.rs
pub const MIGRATIONS: &[Migration] = &[Migration {
    version: "001",
    name: "initial_schema",
    sql: BASE_SCHEMA_SQL,
}];
```

One migration. Nothing reads or writes the 132-table legacy DB, so the JS-era data
(providers, quotas, combos, memory, plugins) has no path into the Rust schema. Upstream ships
`/api/db-backups/*` plus `bin/{snapshot-data,restore-data,rollback}.sh`.

### M9 — No auth beyond bearer keys (P2)

```
$ grep -rIio -- "pkce" "${D[@]}" | wc -l
0
$ grep -rIn "cookie\|session\|csrf\|oidc" crates/heterion-router-gateway/src/auth.rs | wc -l
0
```

The dashboard "login" is a paste-a-key form (`desktop/ui/src/App.svelte:57-64`); the key is
kept in the browser and sent as a bearer token. No login, no session cookie, no CSRF, no OIDC.
Upstream has `/api/auth/{login,logout,status,csrf,oidc/*}` and a live `requireLogin` setting.

### M10 — No quota, rate-limit or budget enforcement (P1)

```
$ for pat in quota_snapshots domain_budgets domain_cost_history domain_lockout_state \
             domain_circuit_breakers semantic_cache provider_nodes db_meta proxy_logs \
             domain_fallback_chains domain_budget_reset_logs; do
    printf "%-28s rs_refs=%s\n" "$pat" \
      "$(grep -rw "$pat" crates --include='*.rs' -c 2>/dev/null | awk -F: '{s+=$2} END{print s+0}')"; done
  provider_nodes              rs_refs=0
  db_meta                     rs_refs=0
  proxy_logs                  rs_refs=0
  domain_fallback_chains      rs_refs=0
  domain_budgets              rs_refs=0
  domain_budget_reset_logs    rs_refs=0
  domain_cost_history         rs_refs=0
  domain_lockout_state        rs_refs=0
  domain_circuit_breakers     rs_refs=0
  semantic_cache              rs_refs=0
  quota_snapshots             rs_refs=0
```

**11 of the 17 tables in the Rust schema are dead schema**: declared in
`crates/heterion-router-db/schema_base.sql` and referenced nowhere in Rust code. They exist only
so a JS-owned database can be attached. In particular the resilience/cost/quota/cache subsystem
upstream persists in these tables has no runtime at all — the Rust breaker is a plain in-RAM
`HashMap` (`crates/heterion-router-routing/src/breaker.rs:77`).

### M11 — Usage attribution is wrong, not just narrow (P0)

```
$ sed -n '1,8p' crates/heterion-router-gateway/src/routing.rs
1: //! Routing backend: model-based selection with failover.
...
6: //! fallback on failure for non-streaming requests. Streaming uses the first
7: //! allowed backend (no mid-stream failover yet); per-attempt usage
8: //! attribution is a follow-up, so usage currently records `routing`.
```

`routing.rs:197` returns `fn name(&self) -> &'static str { "routing" }`, and that string is what
lands in `usage_history.provider` / `call_logs.provider` for every request. STATUS.md:117 tracks
this as objective **O-3**, currently `⛔ "routing"`.

### M12 — Cache, memory, plugins, skills, guardrails, evals, i18n, gamification (P2/P3)

```
$ for pat in qdrant guardrail leaderboard gamification i18n skill audit_log \
             telemetry opentelemetry token_ledger request_detail \
             routing_decisions pin_effectiveness; do
    printf "%-20s %s\n" "$pat" "$(grep -rIio -- "$pat" "${D[@]}" | wc -l)"; done
  qdrant              0
  guardrail           0
  leaderboard         0
  gamification        0
  i18n                0
  skill               0
  audit_log           0
  telemetry           0
  opentelemetry       0
  token_ledger        0
  request_detail      0
  routing_decisions   0
  pin_effectiveness   0
```

Two apparent hits are noise: `rate_limit` (1 hit) is a string literal inside a test fixture
(`grok_cli.rs:785`), and `memory` (31 hits) is entirely `Db::open_in_memory` plus SQLite
`temp_store = MEMORY`:

```
$ grep -rIin "memory" "${D[@]}" | grep -v "open_in_memory\|in-memory\|In-memory"
crates/heterion-router-db/src/db.rs:40:        conn.pragma_update(None, "temp_store", "MEMORY")?;
crates/heterion-router-db/src/db.rs:193:    fn in_memory_opens_with_wal_pragmas() {
crates/heterion-router-db/src/db.rs:200:        assert!(journal == "memory" || journal == "wal", "{journal}");
```

### M13 — Provider transports: 4 of ~60 executors

```
$ grep -rIn "executor" crates/heterion-router-gateway/src crates/heterion-router-providers/src
crates/heterion-router-gateway/src/gemini.rs:25:/// Gemini executor over HTTP.
crates/heterion-router-gateway/src/backend.rs:6://! executors (grok-cli, OpenAI, Anthropic, ...) are ported over.
crates/heterion-router-gateway/src/backend.rs:38:/// Shared by executors that speak OpenAI Chat Completions natively or via a
crates/heterion-router-gateway/src/responses.rs:26:/// Generic Responses API executor over HTTP with token rotation.
crates/heterion-router-gateway/src/openai.rs:23:/// OpenAI-compatible executor over HTTP.
crates/heterion-router-gateway/src/grok_cli.rs:1:/// Grok Build (`grok-cli`) backend: real HTTP executor.
crates/heterion-router-providers/src/registry.rs:24:    pub executor: Option<String>,
crates/heterion-router-providers/src/registry.rs:267:    fn every_provider_has_executor_and_format() {
crates/heterion-router-providers/src/registry.rs:270:                entry.executor.as_deref().is_some_and(|s| !s.empty()),
```

The only reads of `executor` outside the struct field are in a **test that asserts the field is
non-empty** (`registry.rs:267-272`). `main.rs:157-299` dispatches on exactly three format strings
(`gemini`, `openai-responses`, then `is_openai_format`) — so the 17 registry entries declaring
`claude` (10), `antigravity` (2), `cursor` (2), `clova` (1), `kiro` (1), `custom` (1) can never
become a backend, and the 61 HTTP-capable providers with a bespoke `executor` silently fall
through to the generic OpenAI path (`STATUS.md:57`).

### M14 — OAuth is read-only

```
$ grep -rIn "oauth" crates/heterion-router-gateway/src | grep -v "^.*test"
crates/heterion-router-gateway/src/grok_cli.rs:...   # only the grok-cli form-login refresh
$ grep -rIn "device_code\|pkce\|authorization_url\|refresh_token" \
    crates/heterion-router-gateway/src/credentials.rs
```

`credentials.rs:40` is `load_credentials` — a pure SELECT. There is no authorization-URL
construction, no callback handler, no token-refresh scheduler; the only refresh that exists is
the grok-cli private 401→form→retry path (`grok_cli.rs`). 21 registry entries declare
`auth_type: oauth` and cannot be onboarded through the product.

---

## Tables present upstream but absent in the Rust schema

Upstream base tables: **132** (excluding FTS/virtual-table shadow tables; `sqlite_master`
reports 145 tables + 351 indexes + 28 triggers). Rust schema: **17**, all of which are a strict
subset of upstream — the Rust port adds no table of its own.

**115 of 132 upstream tables have no counterpart in `crates/heterion-router-db/schema_base.sql`.**

```
$ sqlite3 "file:/Users/egorich/.omniroute/storage.sqlite?mode=ro" \
    "SELECT name FROM sqlite_master WHERE type='table'
       AND name NOT LIKE 'memory_fts%' AND name NOT LIKE 'sqlite_%'
       AND name NOT LIKE 'vec_memories%' AND name NOT LIKE '%_idx'
       AND name NOT LIKE '%_docsize' AND name NOT LIKE '%_data'
       AND name NOT LIKE '%_content' AND name NOT LIKE '%_config'
       AND name NOT LIKE '%_rowids' AND name NOT LIKE '%_vector_chunks%'
     ORDER BY name;" > /tmp/up_tables.txt
$ grep -oE 'CREATE TABLE IF NOT EXISTS [a-z_]+' \
    crates/heterion-router-db/schema_base.sql | awk '{print $6}' | sort > /tmp/rust_tables.txt
$ wc -l /tmp/up_tables.txt /tmp/rust_tables.txt
     132 /tmp/up_tables.txt
      17 /tmp/rust_tables.txt
$ comm -23 /tmp/up_tables.txt /tmp/rust_tables.txt | wc -l
115
$ comm -13 /tmp/up_tables.txt /tmp/rust_tables.txt     # rust-only tables
                                                        # (empty — strict subset)
```

### Present in both (17)

`api_keys`, `call_logs`, `combos`, `db_meta`, `domain_budget_reset_logs`, `domain_budgets`,
`domain_circuit_breakers`, `domain_cost_history`, `domain_fallback_chains`,
`domain_lockout_state`, `key_value`, `provider_connections`, `provider_nodes`, `proxy_logs`,
`quota_snapshots`, `semantic_cache`, `usage_history`

Of these, **11 are dead schema** (0 Rust references — see M10):
`provider_nodes`, `db_meta`, `proxy_logs`, `domain_fallback_chains`, `domain_budgets`,
`domain_budget_reset_logs`, `domain_cost_history`, `domain_lockout_state`,
`domain_circuit_breakers`, `semantic_cache`, `quota_snapshots`.

So only **6 tables are actually live** in the Rust port: `api_keys`, `call_logs`, `combos`,
`key_value`, `provider_connections`, `usage_history`.

### Missing — 115 tables, grouped by subsystem

**Agent protocols (7)** — `a2a_task_events`, `a2a_tasks`, `mcp_tool_audit`,
`agent_bridge_bypass`, `agent_bridge_mappings`, `agent_bridge_state`, `acp_agents` equivalent
(`a2a_*`, `mcp_tool_audit`, `agent_bridge_*`)

**Memory / RAG (5)** — `memories`, `memory_vec_meta`, `vec_memories` (+ `vec_memories_chunks`,
`vec_memories_info`, `vec_memories_rowids`, `vec_memories_vector_chunks00`), `memory_fts_config`

**Compression (9)** — `ccr_blocks`, `compression_analytics`, `compression_cache_stats`,
`compression_combo_assignments`, `compression_combos`, `compression_engine_breakdown`,
`compression_run_telemetry`, `model_learning`, `auto_candidate_overrides`

**Caching (2)** — `reasoning_cache`, `cache_metrics` (plus `semantic_cache`, present but dead)

**Quota / limits (17)** — `account_key_limits`, `api_key_token_counters`,
`api_key_token_limit_reset_logs`, `api_key_token_limits`, `provider_key_limits`,
`provider_quota_reset_events`, `provider_quota_state`, `provider_plans`,
`quota_allocation_model_caps`, `quota_allocations`, `quota_consumption`, `quota_groups`,
`quota_pool_connections`, `quota_pools`, `relay_rate_limits`, `sync_tokens`, `token_ledger`

**Proxy (6)** — `free_proxies`, `free_proxy_sync_errors`, `proxy_assignments`, `proxy_registry`,
`proxy_scope_rotation`, `proxy_subscriptions` (plus `proxy_logs`, present but dead)

**Batches / files (3)** — `batches`, `batch_item_checkpoints`, `files`

**Evals / discovery (5)** — `eval_cases`, `eval_runs`, `eval_suites`, `discovery_results`,
`job_runs`

**Jobs / scheduler (2)** — `jobs`, `version_manager`

**Routing / model metadata (8)** — `combo_adaptation_state`, `model_capabilities`,
`model_capability_overrides`, `model_combo_mappings`, `model_context_overrides`,
`model_intelligence`, `reasoning_routing_rules`, `pin_effectiveness`

**Sessions / context (6)** — `agentic_conversations`, `context_handoffs`,
`conversation_turn_nodes`, `session_account_affinity`, `session_model_history`,
`exclusive_connection_leases`

**Key management (7)** — `api_key_context_sources`, `group_model_permissions`, `key_group_members`,
`key_groups`, `registered_keys`, `tier_assignments`, `user_levels`

**Usage rollups (3)** — `daily_usage_summary`, `hourly_usage_summary`, `quota_snapshots`(dead in Rust)

**Observability / audit (5)** — `audit_log`, `config_audit_log`, `request_detail_logs`,
`routing_decisions`, `log_export_destinations`

**Plugins / skills / webhooks (6)** — `plugin_analytics`, `plugin_metrics`, `plugins`, `skills`,
`skill_executions`, `webhook_deliveries`

**Cloud / remote agents (4)** — `cloud_agent_credentials`, `cloud_agent_tasks`,
`cli_access_tokens`, `command_code_auth_sessions`

**Middleware / plugins runtime (2)** — `middleware_hooks`, `middleware_logs`

**Inspectors / relays (6)** — `inspector_custom_hosts`, `inspector_session_requests`,
`inspector_sessions`, `relay_logs`, `relay_tokens`, `server_tool_executions`

**Gamification / community (8)** — `badge_definitions`, `invite_tokens`, `leaderboard`,
`user_badges`, `xp_action_counts`, `xp_audit_log`, `community_servers`, `radar_settings`

**Radar / referral caches (5)** — `radar_feed_cache`, `radar_intel_cache`,
`radar_local_model_state`, `radar_offers_cache`, `radar_referrals_cache`

**Misc (2)** — `playground_presets`, `db_meta`-adjacent `_omniroute_migrations` (renamed to
`_heterion_router_migrations`, `migrate.rs:40`)

### Settings keys with no Rust counterpart

```
$ sqlite3 "file:/Users/egorich/.omniroute/storage.sqlite?mode=ro" \
    "SELECT key FROM key_value WHERE namespace='settings' ORDER BY key;"
_settingsRevision a2aEnabled autoDisableBannedAccounts autoDisableBannedThreshold
autoRoutingDefaultVariant backgroundDegradation blockedProviders call_log_pipeline_enabled
cliproxyapi_fallback_enabled codexConnectionDefaultsMigrationV1 codex_catalog_revalidated_version
comboDefaults comboStrategy fallbackStrategy learnedRateLimits logToolSources maxRetryIntervalSec
mcpEnabled mcpTransport memoryEmbeddingSource memoryEnabled memoryMaxTokens memoryRerankEnabled
memoryRetentionDays memoryStaticEnabled memoryStrategy memoryTransformersEnabled memoryVectorStore
modelLockout model_sync_last_run password providerStrategies provider_limits_auto_sync_last_run
qdrantCollection qdrantEmbeddingModel qdrantEnabled qdrantHost qdrantPort quotaVisibility
requestRetry requireLogin resilienceSettings setupComplete skillsEnabled
```

44 settings, **0 with a Rust reader**. The Rust gateway reads configuration exclusively from
environment variables (`crates/heterion-router-config/src/config.rs:33-36`).

---

## Commands run

All commands were read-only. `sqlite3` was always invoked with `mode=ro`; no secret values were
printed (only table names, column names, setting **key** names and match counts).

**Upstream discovery**
```sh
ls -la /Volumes/Netac_SSD/ai-workstation/Archives/OmniRoute/
cat /Volumes/Netac_SSD/ai-workstation/Archives/OmniRoute/README.md
ls -R /Volumes/Netac_SSD/ai-workstation/Archives/OmniRoute/js-removal-2026-09-18/
ls /Volumes/Netac_SSD/ai-workstation/Archives/OmniRoute/releases/
tar -tzf releases/OmniRoute-release-3.8.51-2026-09-09.tgz | head -30
tar -tzf releases/OmniRoute-release-3.8.51-2026-09-09.tgz | wc -l          # 7746

ls -la /Volumes/Netac_SSD/ai-workstation/Projects/OmniRoute-Rust/
grep -n '^#\{1,3\} ' README.md
git -C /Volumes/Netac_SSD/ai-workstation/Projects/OmniRoute-Rust log --oneline -15
find src/app/api -name "route.ts" | sed 's|src/app||; s|/route.ts||' | sort | wc -l   # 608
find src/app/api -name "route.ts" | sed 's|src/app||; s|/route.ts||' | sort
find src/app -name "page.tsx" | wc -l                                                # 133
ls "src/app/(dashboard)/dashboard"                                                   # 55 dirs
ls open-sse/executors | wc -l                                                        # 111
find open-sse/executors -name "*.ts" -not -name base.ts -not -name index.ts | wc -l    # 141
ls open-sse/mcp-server/tools                                                          # 13 files
find . -name "*.test.ts" -not -path "*/node_modules/*" | wc -l                         # 3723
ls docs | wc -l ; ls docs ; grep -c "^  /" docs/openapi.yaml                         # 233 paths
head -201 docs/rust-port-research.md
find compression-core -name "*.rs" | xargs wc -l
ls src/lib/a2a/
```

**Legacy DB (read-only)**
```sh
sqlite3 "file:/Users/egorich/.omniroute/storage.sqlite?mode=ro" ".tables"
sqlite3 "file:/Users/egorich/.omniroute/storage.sqlite?mode=ro" \
  "SELECT type, count(*) FROM sqlite_master GROUP BY type;"      # index|351 table|145 trigger|28
sqlite3 "file:/Users/egorich/.omniroute/storage.sqlite?mode=ro" \
  "SELECT name FROM sqlite_master WHERE type='table'
     AND name NOT LIKE 'memory_fts%' AND name NOT LIKE 'sqlite_%'
     AND name NOT LIKE 'vec_memories%' AND name NOT LIKE '%_idx'
     AND name NOT LIKE '%_docsize' AND name NOT LIKE '%_data'
     AND name NOT LIKE '%_content' AND name NOT LIKE '%_config'
     AND name NOT LIKE '%_rowids' AND name NOT LIKE '%_vector_chunks%'
   ORDER BY name;" > /tmp/up_tables.txt
sqlite3 "file:/Users/egorich/.omniroute/storage.sqlite?mode=ro" \
  "SELECT namespace, count(*) FROM key_value GROUP BY namespace ORDER BY 2 DESC;"
sqlite3 "file:/Users/egorich/.omniroute/storage.sqlite?mode=ro" \
  "SELECT key FROM key_value WHERE namespace='settings' ORDER BY key;"      # key names only
sqlite3 "file:/Users/egorich/.omniroute/storage.sqlite?mode=ro" \
  "SELECT DISTINCT auth_type, count(*) FROM provider_connections GROUP BY auth_type;"
sqlite3 "file:/Users/egorich/.omniroute/storage.sqlite?mode=ro" \
  "SELECT DISTINCT j.key FROM combos c, json_each(c.data) j;"
sqlite3 "file:/Users/egorich/.omniroute/storage.sqlite?mode=ro" \
  "SELECT json_extract(data,'\$.strategy') s, count(*) FROM combos GROUP BY s;"   # auto|2 priority|6
comm -23 /tmp/up_tables.txt /tmp/rust_tables.txt        # 115 missing
comm -12 /tmp/up_tables.txt /tmp/rust_tables.txt        # 17 shared
comm -13 /tmp/up_tables.txt /tmp/rust_tables.txt        # empty
```

**Target discovery**
```sh
cd /Volumes/Netac_SSD/ai-workstation/Projects/heterion-router
find crates desktop -type f \( -name "*.rs" -o -name "*.json" -o -name "*.toml" -o -name "*.sql" \) \
  -not -path "*/target/*" | sort
find desktop/ui/src desktop/src-tauri/src -type f | sort
find crates desktop/src-tauri/src desktop/ui/src \( -name "*.rs" -o -name "*.svelte" \) \
  | grep -v target | xargs wc -l | sort -rn | head -40            # 17938 total

python3 - <<'EOF'   # registry.json census
import json,collections
d=json.load(open('crates/heterion-router-providers/registry.json'))
print(len(d))                                                     # 278
for f in ('format','executor','auth_type'):
    print(f, dict(collections.Counter(p.get(f) for p in d)))
print('models', sum(len(p['models']) for p in d))                   # 2717
EOF

grep -n "\.route(" crates/heterion-router-gateway/src/lib.rs
grep -n "CREATE TABLE" crates/heterion-router-db/schema_base.sql
grep -n "CREATE TABLE\|ALTER TABLE" crates/heterion-router-db/src/migrate.rs
grep -n "^pub const MIGRATIONS" -A5 crates/heterion-router-db/src/migrate.rs
grep -n -A3 "fn is_known_strategy" crates/heterion-router-gateway/src/admin.rs
grep -rIn "executor" crates/heterion-router-gateway/src crates/heterion-router-providers/src
grep -n "async fn\|^pub async fn" crates/heterion-router-gateway/src/admin.rs
cat desktop/src-tauri/tauri.conf.json
cat scripts/ci/check.sh ; cat .github/workflows/ci.yml ; cat .github/workflows/release.yml
grep -rc "#\[test\]\|#\[tokio::test\]" crates --include="*.rs" | grep -v ":0"
grep -rho '#\[test\]\|#\[tokio::test\]' crates --include="*.rs" | wc -l     # 215
grep -rn "clap" crates/*/Cargo.toml Cargo.toml                          # 0
grep -n "env::var" crates/heterion-router-gateway/src/main.rs
grep -n -A12 "enum WireFormat" crates/heterion-router-core/src/format.rs
grep -n -A6 "pub fn parse_wire_format" crates/heterion-router-providers/src/registry.rs
grep -n -A6 "pub fn is_openai_format" crates/heterion-router-providers/src/registry.rs
```

**Absence evidence (zsh array — note: zsh does not word-split unquoted vars, so
`$D` must be `"${D[@]}"`)**
```sh
D=(crates/heterion-router-gateway/src crates/heterion-router-routing/src \
   crates/heterion-router-usage/src crates/heterion-router-db/src \
   crates/heterion-router-providers/src crates/heterion-router-translate/src \
   crates/heterion-router-config/src crates/heterion-router-crypto/src \
   crates/heterion-router-core/src crates/heterion-router-http/src \
   desktop/src-tauri/src desktop/ui/src)

# positive controls
for pat in weight breaker combo tracing sqlite sse encrypt strategy; do
  printf "%-14s %s\n" "$pat" "$(grep -rIin -- "$pat" "${D[@]}" | wc -l)"; done

# protocol surfaces
for pat in embeddings /v1/messages count_tokens anthropic-version /v1beta \
           /v1/completions /v1/images /v1/audio /v1/moderations /v1/rerank \
           /v1/search /v1/ocr /v1/files /v1/batches /v1/videos /v1/music \
           /v1/ws web_search prompt_cache_key; do
  printf "%-22s %s\n" "$pat" "$(grep -rIn -- "$pat" "${D[@]}" | wc -l)"; done

# feature surface
for pat in mcp a2a webhook compression caveman rtk llmlingua ccr_block memory \
           rate_limit token_limit quota telemetry opentelemetry metrics \
           oauth pkce device_code authorization_url cli_access_token tunnel \
           audit_log backup restore eval fusion judge pipeline weighted \
           round_robin p2c least_used cost_optimized headroom reset_window \
           lkgp auto_combo guardrail chaos plugin skill leaderboard \
           gamification i18n qdrant token_ledger request_detail \
           routing_decisions pin_effectiveness; do
  printf "%-20s %s\n" "$pat" "$(grep -rIio -- "$pat" "${D[@]}" | wc -l)"; done

# context for every apparent hit
grep -rIin "embeddings"  "${D[@]}"
grep -rIin "rate_limit"  "${D[@]}"
grep -rIin "chaos"       "${D[@]}"
grep -rIin "compression" "${D[@]}"
grep -rIin "mcp"         "${D[@]}"
grep -rIin "memory" "${D[@]}" | grep -v "open_in_memory\|in-memory\|In-memory"

# dead-schema tables: per-table Rust reference count
for t in provider_connections provider_nodes key_value combos api_keys db_meta \
         usage_history call_logs proxy_logs domain_fallback_chains domain_budgets \
         domain_budget_reset_logs domain_cost_history domain_lockout_state \
         domain_circuit_breakers semantic_cache quota_snapshots; do
  printf "%-28s rs_refs=%s\n" "$t" \
    "$(grep -rw "$t" crates --include='*.rs' -c | awk -F: '{s+=$2} END{print s+0}')"; done
```

**Web (optional corroboration of the advertised feature list)**
```sh
websearch: "OmniRoute diegosouzapw open source AI gateway features providers routing strategies"
```

Confirms the frozen snapshot understates upstream's current `main`: 352 providers (vs 290 in the
frozen README), 60 executors, 16-factor Auto-Combo scoring (vs 12), 152 free providers,
`auto/chaos`, Cloudflare Quick Tunnel. The parity gaps below are therefore conservative — the
upstream target keeps moving.
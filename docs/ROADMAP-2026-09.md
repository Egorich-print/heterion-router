# Heterion Router — API/CLI Roadmap, сентябрь 2026

> Срез внешнего исследования и текущего рабочего дерева: **2026-09-29**.
> Область: xAI/Grok Build, OpenAI Responses, Anthropic compatibility и
> эксплуатационная граница Rust-шлюза. Документ фиксирует проверенное
> состояние, а не обещание полного паритета.
> **План следующей миссии:** `docs/MISSION-2026-10.md` (ADR-011). Backlog ниже
> отображается в workstreams WS-A…WS-H и milestones M0…M3 этого плана.

## Короткое резюме

Рабочий код — OpenAI-compatible Rust gateway с combo routing, SQLite,
Responses/Gemini/Grok CLI transports, Svelte dashboard и Tauri sidecar.
Сильные стороны — typed agent controls, per-model Responses dispatch,
single-flight Grok refresh, state retrieve/delete/replay, auth boundaries и
213 workspace tests.

Главные разрывы — не в количестве registry entries, а в silent correctness:
Responses reasoning/tools теряются или получают нестандартные события;
provider registry значительно шире runtime-конструктора; alias-keyed
connections могут быть не routable; streaming errors не sanitized; Responses
state store unbounded; desktop/launchd operational story не воспроизводится
из репозитория.

## Проверенный snapshot

| Область | Факт |
|---|---|
| Workspace | 10 Rust crates; `desktop/src-tauri` исключён |
| Registry | 278 providers / 2717 models в рабочем дереве (в `HEAD` — 278 / 2715); 259 / 2108 pass structural builder gate, 253 / 1916 are HTTP(S)-capable |
| Live runtime | 19 backends при 24 активных провайдерах в БД; `heterion-local` без active connection; `exa-search` — 5 connections при отсутствии в registry |
| Combos | 8 rows; `vivanta-auto` и `vivanta-deepseek` имеют `models: []` |
| Gateway | 19 route paths / 21 method-handler pairs; Chat/Responses/SSE/admin boundary |
| Tests | 213 workspace tests, 0 failures; fmt/clippy green; 0 вызовов `wiremock.verify()` при 22 `expect()` |
| Desktop | UI build и Tauri check green; local app/dmg build green, adhoc-signed; fresh clone desktop не собирает |
| Service | machine-local launchd `com.heterion.router`, live `0.0.0.0:20128`; plist отсутствует в git |
| Responses state | implemented in code, zero observed production rows (namespace `responses` пуст) |

Подробный evidence ledger: `docs/research/PROJECT-AUDIT-2026-09.md`.

## Execution status — 2026-09-29

| Цель | Состояние | Что реально закрыто | Остаточный разрыв |
|---|---|---|---|
| **P0-1 Lossless envelope / capability routing** | Partial | Chat agent controls и unknown `extra` fields форвардятся; per-model Responses transport выбирается по `target_format` | Нет `RequestRequirements`, typed capability rejection и versioned capability profile; Responses-only поля могут утекать в Chat body |
| **P0-2 Responses create/state/stream** | Partial | Create, extras, store, owner-scoped GET/DELETE, replay после restart, basic text/reasoning stream | Нет полного SSE lifecycle/argument deltas; tool events не SDK-safe; reasoning input items и built-in tools могут silently drop; store retention не ограничена |
| **P0-3 Opaque reasoning / signatures** | Partial | Gemini thought signatures сохраняются и переживают restart | Responses encrypted reasoning не replay; `phase` не переносится; Gemini fallback process-global, не изолирован по session |
| **P0-4 xAI / Grok conformance** | Partial | xAI per-model Responses side-backend; dedicated Grok CLI refresh path | Private proxy и pinned client version; нет conformance harness/headless/tool-turn/session-resume canary; 426 не классифицируется как outage |
| **P1-5 Native Anthropic Messages** | Not started | Text-only Claude translators и JS parity fixtures | Нет `AnthropicBackend`, `/v1/messages`, tool/signature/image transport |
| **P1-6 Tools / structured / multimodal** | Partial | Client function tools, Gemini `response_format`, basic Responses `text.format` | Built-in tools drop; нет typed capability rejection; refusal/annotations flatten; schema boundary не валидируется |
| **P1-7 Streaming / usage / observability** | Partial | Byte-safe SSE, cancellation, terminal usage, status mapping, usage/call-log writes | Provider/connection/combo attribution отсутствует; status в logs synthetic; request-id/`Retry-After` нет; error frames не sanitized |

### Security and operations status

- **Verified protections:** auth fail-closed, manage scope, model policy,
  hash-or-legacy key lookup, CORS allowlist, parameterized SQL, private data
  directory, response ownership checks.
- **Open high findings:** raw upstream text in SSE error frames; unbounded
  plaintext Responses envelopes; plaintext OAuth refresh write when
  `STORAGE_ENCRYPTION_KEY` is absent.
- **Open medium findings:** NULL policy columns fail open to manage/all models;
  unauthenticated static routes; sidecar has no ownership/readiness handshake;
  quota/session columns are not enforced; encryption key is beside ciphertext.
- **Operational gaps:** rotated logs can be 0644; launchd plist is not
  reproducible; Vite dev proxy points at service 20128 while plain dev gateway
  is 20129; Tauri fallback port can target the wrong process; desktop restart
  and signing/notarization are not production-ready.

## Prioritized backlog

### P0 — silent agent-loop corruption and data loss

1. Preserve Responses `reasoning` input items (including
   `encrypted_content`) or return `400 unsupported_capability`; add a
   `store:false` two-turn replay test.
2. Replace the hybrid Responses streaming tool event with a typed lifecycle and
   add a validator asserting ids, indices and exactly one terminal event.
3. Reject unsupported built-in tools rather than dropping them.
4. Sanitize SSE errors and add a wire-level regression test.
5. Bound/encrypt/prune Responses state, mint gateway-owned storage keys and
   make retention/default explicit.

### P1 — routing and observability correctness

6. Canonicalize provider ids when building backends; expose unreachable registry
   entries and alias collisions in admin/model output.
7. Thread selected backend, target model, connection and combo into usage and
   call logs; record real HTTP status/error code instead of 200/500.
8. Propagate upstream request-id and `Retry-After`; honor it in retry policy.
9. Add sidecar readiness/ownership handshake; align `api.js` and Vite ports;
   make desktop restart sidecar-aware or honestly disabled.
10. Commit/install a portable launchd plist or remove service instructions
    from a fresh-clone quick start.

### P2 — test and maintenance debt

11. Add admin-handler, alias-construction, derived-Responses-URL, combo-retry,
    breaker half-open, CORS/dashboard and desktop Rust tests.
12. Implement or remove `maxSetRetries`, `setRetryDelayMs`, `weight` and
    `strategy:auto` fields exposed by the dashboard.
13. Fix `count_active_api_keys`, remove the tracked 0-byte AppleDouble icon,
    make release checksums portable and add desktop paths to CI.
14. Decide whether stored Responses envelopes are encrypted at rest or
    explicitly protected only by SQLite file permissions.

## Принципы, которые остаются обязательными

1. **Fail closed:** нельзя silently выбрасывать поля, меняющие agent loop.
2. **Capability by effective model format:** transport выбирается по модели,
   но registry metadata не считается доказательством executor-а.
3. **Opaque reasoning is data:** encrypted content и signatures не превращать
   в видимый текст и не логировать.
4. **State origin must be explicit:** continuation не должен незаметно менять
   provider/credential; это пока не реализовано.
5. **Public static surface is a boundary:** `/healthz` и SPA публичны, API и
   admin routes — нет.
6. **Сначала красный тест, потом фикс** — ни один дефект не закрывается без
   падающего теста, который был красным до исправления.

## Отображение backlog на MISSION-2026-10

| Backlog | Workstream | Milestone |
|---|---|---|
| P0-1…P0-5 (пункты 1–5 ниже) | WS-B (B1–B7), WS-C (C1–C6) | M1, M2 |
| P1-6…P1-10 (пункты 6–10) | WS-D (D1–D10), WS-F (F1–F6) | M2, M3 |
| P2-11…P2-14 (пункты 11–14) | WS-G (G1–G12), WS-H (H1–H5), WS-A | M0, M3 |

Полные формулировки задач, зависимости, роли субагентов, тестовые пороги и
stop/go-критерии: `docs/MISSION-2026-10.md`.

## Границы, которые сознательно не расширяются сейчас

WebSocket transport, hosted MCP execution, все OpenAI built-in tools, фоновые
jobs, Conversations API и эмуляция private Grok Build protocol не входят в
текущий milestone без подтверждённого клиента и измеримой потребности.
Полный список non-goals следующей миссии (включая native Anthropic Messages,
TLS-терминацию, quota enforcement, notarization) — `docs/MISSION-2026-10.md` §3.

## Официальные источники

Внешние страницы, использованные для планирования, проверены 2026-09-29;
это не live conformance-тесты текущего Rust-шлюза.

### xAI API

- [Generate Text / Responses API](https://docs.x.ai/developers/model-capabilities/text/generate-text)
- [Responses REST reference](https://docs.x.ai/developers/rest-api-reference/inference/responses)
- [Streaming](https://docs.x.ai/developers/model-capabilities/text/streaming)
- [Reasoning и encrypted content](https://docs.x.ai/developers/model-capabilities/text/reasoning)
- [Function Calling](https://docs.x.ai/developers/tools/function-calling)
- [Structured Outputs](https://docs.x.ai/developers/model-capabilities/text/structured-outputs)
- [Web Search](https://docs.x.ai/developers/tools/web-search)
- [X Search](https://docs.x.ai/developers/tools/x-search)

### Grok Build / CLI

- [Grok Build overview](https://docs.x.ai/build/overview)
- [Settings](https://docs.x.ai/build/settings)
- [Settings reference](https://docs.x.ai/build/settings/reference)
- [Headless & Scripting](https://docs.x.ai/build/cli/headless-scripting)
- [CLI reference](https://docs.x.ai/build/cli/reference)

### OpenAI

- [Migrate to Responses](https://developers.openai.com/api/docs/guides/migrate-to-responses)
- [Create response reference](https://developers.openai.com/api/reference/resources/responses/methods/create)
- [Streaming Responses](https://developers.openai.com/api/docs/guides/streaming-responses)
- [Function Calling](https://developers.openai.com/api/docs/guides/function-calling)
- [Reasoning models and continuity](https://developers.openai.com/api/docs/guides/reasoning)
- [Conversation state](https://developers.openai.com/api/docs/guides/conversation-state)
- [Structured Outputs](https://developers.openai.com/api/docs/guides/structured-outputs)
- [Tools](https://developers.openai.com/api/docs/guides/tools)

### Anthropic

- [OpenAI SDK compatibility](https://platform.claude.com/docs/en/cli-sdks-libraries/libraries/openai-sdk)
- [Thinking, signatures и tools](https://platform.claude.com/docs/en/build-with-claude/thinking)
- [Extended thinking](https://platform.claude.com/docs/en/build-with-claude/extended-thinking)
- [Handle tool calls](https://platform.claude.com/docs/en/agents-and-tools/tool-use/handle-tool-calls)
- [Streaming Messages](https://platform.claude.com/docs/en/build-with-claude/streaming)

## Статус документа

Документ не описывает релиз и не считается green-планом. Изменения рабочего
дерева должны быть зафиксированы commit'ом перед использованием его как
воспроизводимого baseline.

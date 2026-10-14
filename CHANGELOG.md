# Changelog

> Manifest version: `0.1.0`. The Rust surface has no released version section
> yet; the `v*` tags in this repository are inherited frozen OmniRoute tags and
> do not point at the Rust gateway. Gateway artifacts use the separate
> `gateway-YYYYMMDD-<sha>` release naming.

## Unreleased — working tree snapshot 2026-10-02 (rust-review)

### Fixed

- **Оболочка больше не участвует в рестарте.** `restart.rs` запускал
  `sh -c "sleep 2; exec launchctl kickstart -k gui/{uid}/{label}"`, где `label`
  приходит из окружения. Теперь это argv: сначала проверка цели
  (`[A-Za-z0-9._-]`, ≤128 символов), затем `launchctl` отдельными аргументами
  из отдельного потока с задержкой. Отказ не эхом возвращает label — иначе
  `admin::restart_service` отдал бы его клиенту в теле 500-го ответа.
  ⚠️ **Побочное изменение:** 202 теперь означает «перезапуск запланирован», а
  не «уже выполняется»: сам `spawn` `launchctl` происходит через 2 секунды на
  отдельном потоке, и его ошибка до клиента не доходит. Раньше ошибка
  спавна `sh` давала 500.
- **Блокирующая запись на рантайм-потоке.** `ThoughtSignatures::record` писал
  `open`+`write` на каждый чанк стрима из `async fn drive_stream`. Теперь
  write-behind очередь на 256 записей и один выделенный `std::thread`.
  Добавлен явный `flush()` — блокирующий и намеренный: стриминг не ждёт диск, а
  вызывающий, который сейчас потеряет процесс, требует гарантии сам.
  ⚠️ **Осознанное ослабление:** `record` больше не гарантирует, что пара на
  диске, к моменту возврата; `flush()` её восстанавливает, но **в коде нет
  shutdown-хука, который его вызывает** — `kill -9` в окне сброса теряет хвост.
  Полная очередь роняет запись, и это больше не молчит: счётчик
  `dropped_appends()` плюс `warn!` на первом и каждом 1024-м пропуске.
- **`GatewayError` на `thiserror`** вместо ручных `Display` + `Error` — та же
  конвенция, что и в остальных крейтах воркспейса.
- **`// SAFETY:`** на всех production- и test-`unsafe` блоках
  (`main.rs:460`, `admin.rs` probe-аллокатор).
- **`#[allow(deprecated)]` в `crypto`** сужен с целой функции до двух
  конкретных `from_slice` — следующий устаревший вызов в этой функции больше
  не спрячется за ним.

371 → 556 (M1–M3 дали +184, ревью — +1 net).

### Known, deliberately not fixed

- `lib.rs` — 4981 строка (вся HTTP-поверхность в одном файле), `main.rs` —
  1045, `select_backend` 234 строки, `guardrails.rs` 2204.
  Дробление `lib.rs` по семействам эндпоинтов и `select_backend` по
  провайдерам — работа M6, не ревью: она меняет структуру без изменения
  поведения и без красного теста, который можно предъявить.
- `.context()` в путях старта и `anyhow` для бинарника — требует новой
  зависимости, это решение владельца (§11 плана), а не правка ревью.

## Unreleased — working tree snapshot 2026-10-02 (M1–M3)

### Added — roadmap M1 «честные данные»

- **Колонки схемы пишутся там, где шлюз действительно знает значение**: из 54 полей, добавленных в структуры (57 колонок минус 3, которые даёт БД), заполняются 38; оставшиеся 19 — структурно незнаваемые без фичи, которой нет. `CallLogRecord` +21 (`connection_id`,
  `tokens_cache_*`, `source_format`/`target_format`, `cache_source`,
  `detail_state`, `correlation_id`, `request_summary`, `has_*`, `combo_*`),
  `UsageRecord` +5 (`account_*`, `service_tier`, `ttft_ms`). `ttft_ms` меряется
  от старта запроса до первого токена и равен 0 для non-streaming.
  `combo_name` теперь пишется INSERT'ом — отдельный `UPDATE` удалён.
- **Перцентили**: `UsageSummary.latency` = p50/p95/p99 (метод nearest-rank,
  документирован), и они теперь **доходят до ответа** — `latency.p50_ms` и
  прочее в теле `/api/usage`. Плюс `usage_cost_rows_in_window` — шов для
  расчёта перцентилей стоимости, который не может жить в db-крейте.
- **Заголовки стоимости** на каждом проксированном ответе: `x-router-cost`
  (`{cost:.6}`), `x-router-provider`, `x-router-model-used`.
- **Retention**: `purge_usage_older_than`; `retention_days == 0` означает
  «хранить всё», а не «удалить всё». Обе написанные в кодексе timestamp-формы
  учтены через общий `ts_prefix`.
- **`connection_id` для мультиаккаунтных провайдеров**: падает на голову
  диспетчеризации (`priority DESC, id ASC`) — той же, что строит пул.

### Added — roadmap M2 «протоколы»

- **`POST /v1/messages`** (Anthropic Messages, стриминг и non-streaming) и
  **`POST /v1/messages/count_tokens`**. Переиспользует существующие
  `ChatBackend`, SSE-насос и учёт; собственных HTTP-клиентов не добавляет.
- **`POST /v1/embeddings`** — float и base64, батчи, `dimensions`.

### Added — roadmap M3 «деньги»

- **Бюджеты и лимиты на ключ** (`/api/budgets`): дневной/недельный/месячный
  лимит в USD с порогом предупреждения и RPM-окном. Превышение → 429 с
  причиной в `call_logs`. Нулевой лимит = безлимит; без настройки всё
  работает как раньше.
- **Кэш ответов** (exact + semantic): `x-router-cache: hit|miss|bypass`,
  `x-router-cost: 0` на попадании. Ключ = канонический JSON с
  сортировкой ключей на каждом уровне. Namespace = хэш presented bearer, так
  что один ключ не читает ответы другого. Стриминг обходит кэш полностью.
- **Guardrails** (`/api/guardrails`): `secrets-detector` (префиксы + JWT +
  энтропия Шеннона, порог 4.2 бит/символ), `pii-detector` (email, телефон,
  **Luhn** для карт, AWS id), `keyword-blocklist`, `language-detection`.
  Действия `block|warn|log`; по умолчанию всё в режиме `warn`, то есть без
  настройки новых отказов нет. Совпадения никогда не попадают в тело, заголовки
  и логи — только имя проверки и маска.

### Fixed

- **SSE-кеш ловил себя на пустом месте.** `guardrails` перечитывает тело
  ответа, из-за чего size hint становится «неизвестным», а кэш на
  `is_none_or` молча выключался на всех запросах. Теперь отсекается только
  заведомо oversized тело; реальную границу даёт `to_bytes`.
- **Двойной прогон middleware в тестах** — `budgets`/`cache` накладывались
  поверх роутера, который уже содержит их слои: лимит считался дважды, запрос
  получал 429 вместо 200. Добавлен `build_router_with(state, cache_layer)`, и
  тесты строят роутер через него.
- **Входящие JSON-экстракторы больше не эхом возвращают значения.** `/v1/embeddings`
  и три admin-обработчика ещё использовали `axum::Json`, из-за чего тело с
  неверным типом отдавало клиенту само значение (`dimensions: invalid type:
  string "sk-…"`). Переведены на `JsonBody`, который отвечает статическим
  сообщением.
- **`message_text` оказался мёртвым кодом**: он вызывался только тогда, когда
  `content` отсутствовал, и перечитывал то же поле — всегда пустая строка,
  всегда отфильтровываемая. Доказано подстановкой sentinel'а (ни один тест не
  изменился). Функция и fallback удалены, вместо них 4 теста на verbatim
  сохранность `content`.
- **Exec-bit гейт** покрывает `desktop/scripts/`, а не только `scripts/`.

### Tests

371 → 552. Каждый новый фикс доказан красным прогоном.



### Added — audit 2026-10 (ADR-012)

- **Rust 1.99 / edition 2024**: `rust-toolchain.toml` → `channel = "1.99.0"`,
  `rust-version = "1.99"` в workspace и `desktop/src-tauri`; CI закрепляет тот
  же `1.99.0` вместо `@stable`, в `paths:` добавлены `desktop/src-tauri/**` и
  `docs/**`, добавлен job `tauri-fmt`. Долг `cargo fmt` в исключённом
  desktop-крейте (`build.rs`, `src/lib.rs`) закрыт.
- **Реальная атрибуция usage**: `Attribution::provider` — `Option<String>`, поэтому запрос, не дошедший
  до провайдера, пишет `provider = NULL`, а не имя роутера;: `backend::Attribution` + per-request токен вместо
  константы `provider = "routing"`; `requested_model` — модель клиента,
  `connection_id` и `combo_name` заполняются. Прерванный клиентом поток
  учитывается (`StreamAccounting` в `Drop`, call-log 499).
- **Cache/reasoning token accounting** для non-streaming и streaming:
  `CachedUsage::from_payload` разбирает все известные upstream-спеллинги,
  `pricing.rs` применяет опубликованный discount, непубликованная цена
  сохраняет прежнее поведение.
- **Полупроводящий state store `/v1/responses`**: TTL 7 дней, 512 записей,
  256 KiB на запись, ленивый своп, oversized-ответ деградирует в
  well-formed placeholder.
- **Документация**: `docs/README.md` (индекс), `docs/research/audit-2026-10/`
  (шесть отчётов аудита + индекс), ADR-012.

### Fixed — audit 2026-10

- **Security**: replay SHA-256 дайджеста как bearer (`api_keys.key` для
  выпускаемых ключей хранит маркер `hash-only:<id>`; предикат отвергает маркер,
  `enc:v1:` и строки `key == key_hash`, написанные прежними сборками); plaintext
  запись refresh-токена без `STORAGE_ENCRYPTION_KEY` теперь fail-closed;
  нефильтрованный `COUNT(*) FROM api_keys` в гейте не-loopback bind и
  завышающий `count_active_api_keys` заменены на предикат «действительно
  рабочий ключ»; агрегированное предупреждение о connections без registry
  провайдера (в live-БД молча терялись 9 из 51).
- **Correctness**: окно usage-window сравнивало TEXT в двух написаниях
  timestamp (30 дней давали 15663 вместо 15210 строк); ошибка combo-store
  превращалась в `404 unknown model` вместо 502; upstream-статус из тела,
  пришедшего с HTTP 200, восстанавливается, поэтому 429 больше не становится
  502 и не роняет breaker (контракт «4xx не роняет breaker» соблюдён и на этом
  пути).
- **Resources**: SSE-декодер ограничен (`MAX_SSE_EVENT_BYTES = 1 MiB`, fail-closed
  без эха upstream-текста) и больше не работает квадратично; `/api/logs/app`
  читает хвост файла (`APP_LOG_TAIL_BYTES = 512 KiB`) вместо всего лога.
- **Reliability**: breaker отдаёт ровно один half-open probe и не воскрешает
  уже открытый breaker; клиент разделяет connect/response/idle таймауты, поэтому
  длинный здоровый поток больше не обрывается общим 600-секундным дедлайном.
- **Hygiene**: `.gitignore` закрывает `*.sqlite*`, `*.db*`, `node_modules/`,
  `dist/`, `build/`, `artifacts/`, `._*`; из индекса убран 0-байтный
  AppleDouble-иконка; `release.yml` не зовёт `sha256sum` на macOS-раннере.

### Changed

- **Дубликаты удалены**: `content_text`/`content_text_with` (core),
  `translate::chunk::*`, `pump_sse_stream` (responses), `drive_openai_sse`,
  `try_each_credential`, `backend::next_token`, `upstream_status_or_recovered`.
  Удалён мёртвый `config::settings` (−86 LOC) и мёртвое поле `combo_configs`.
  `registry.json` парсится один раз за запуск вместо двух.
- **Tests**: 215 → 371 (коммит `51dfe6d` даёт 186 — прирост аудита считается
  от рабочего дерева, в котором уже лежал P0-срез предыдущей сессии). Все 30 `wiremock.expect()` проверяются
  `MockServer::verify()`; каждая проверка доказана load-bearing. Утверждения
  добавлены в тест, который их не содержал.
- **Docs**: `providers/src/lib.rs` утверждает 278 providers (фактическое число),
  а не 277; baseline тестов в `STATUS.md`/`CHANGELOG.md` приведён к факту.

## Unreleased — working tree snapshot 2026-09-29

### Planning

- **MISSION-2026-10 «Fail-closed & reproducible»** — утверждённый план следующей
  миссии (`docs/MISSION-2026-10.md`): objective O-1…O-4, workstreams WS-A…WS-H,
  milestones M0…M3 с измеримым DoD и тестовыми порогами (215 baseline → 225 → 245 → 290),
  dependency graph, exclusivity-скоупы шести субагентов, rollback-границы,
  stop/go-критерии и девять открытых решений владельца.
- **ADR-011** — границы миссии, принцип «схема ≠ enforcement», правило
  «красный тест перед фиксом» и запрет верифицировать mission-правки на живом
  launchd-инстансе.
- **Evidence миссии** — шесть параллельных архитектурных отчётов в
  `docs/research/mission-2026-10/` (sequencing, Responses contract, routing и
  providers, security и retention, desktop и release, QA и docs).

### Added

- **Heterion Router identity**: OmniRoute-Rust → Heterion Router rename,
  `heterion-router-*` crates, `heterion-router-gateway` binary and
  `com.heterion.router` desktop/service label (ADR-008).
- **`heterion-local` provider path**: keyless OpenAI-compatible backend for
  `http://127.0.0.1:8080/v1`, overridable with `HETERION_LOCAL_BASE_URL`; the
  backend is created only when an active `provider_connections` row exists.
- **Data-directory contract**: `~/.heterion-router` is primary, with
  `~/.omniroute` retained as a legacy fallback.
- **OpenAI-compatible gateway**: `/v1/chat/completions`, `/v1/models`, SSE,
  combo routing, breaker classification, retry/failover and typed upstream
  status mapping.
- **Responses surface**: `/v1/responses`, Responses extras, per-model
  `target_format: openai-responses` dispatch, owner-scoped state retrieve/delete
  and `previous_response_id` transcript replay.
- **Grok CLI transport**: session headers, 401 → form-urlencoded OAuth refresh
  → original request retry, single-flight rotation and optional encrypted DB
  persistence callback.
- **Gemini transport**: `generateContent`, tools and persistent thought
  signature storage.
- **Security boundaries**: `manage` scope, model allow-list, non-loopback
  fail-closed startup, hash-only new API-key issuance, legacy plaintext lookup,
  `no_log`, `last_used_at`, private data directory and CORS allowlist.
- **Observability**: non-stream and terminal-stream usage/call-log writes,
  dated application-log discovery and upstream status preservation.
- **Desktop integration**: Svelte 5/Vite dashboard, Tauri v2 sidecar,
  single-instance guard and parent-watch.
- **ChatHub research**: public protocol inventory and anonymous-session
  evidence; deliberately no ChatHub provider (ADR-010).

### Fixed

- UTF-8 and CRLF boundaries no longer corrupt SSE chunks.
- Client disconnect cancels upstream streaming tasks.
- EOF without a terminal event is an explicit error/incomplete path.
- Gemini thought-signature fallback is not overwritten by empty stream chunks.
- Malformed API-key and provider expiry values fail closed.
- Unicode `call_id` truncation no longer panics.
- Upstream 400/401/403/404/409/422/429 statuses are preserved for non-stream
  clients; streaming error sanitization remains an open finding.

### Changed

- The project remains an OpenAI-compatible Rust gateway with SQLite combo
  routing, provider backends and a Tauri sidecar; the architectural direction
  did not change.
- New API keys are not issued in plaintext; legacy plaintext rows remain
  readable until a separate migration is performed.
- The registry is explicitly documented as a catalog, not a guarantee that all
  entries are executable.

### Audit and documentation — 2026-09-29

- Added `docs/research/PROJECT-AUDIT-2026-09.md`, consolidating six parallel
  architecture, provider, security, desktop/build, documentation and quality
  audits.
- Corrected the roadmap, status, README, AGENTS and ADR addenda against the
  actual working tree; stale claims are no longer presented as completed work.
- Labeled `docs/history/` as frozen upstream JS OmniRoute material rather than
  Rust-project history.

### Known open work

- Responses reasoning input/tool preservation and SDK-shaped streaming events.
- Bounded/encrypted Responses state retention and gateway-owned storage keys.
- Provider/connection/combo attribution, real HTTP status/error codes and
  `Retry-After`/request-id telemetry.
- Canonical provider construction, unreachable registry reporting and dead combo
  configuration fields.
- Sidecar readiness/restart, launchd reproducibility, desktop CI and signed
  desktop distribution.
- Full Responses/Anthropic capability contracts and native Anthropic Messages.

### Compatibility (one cycle)

- `OMNIROUTE_*` remains a fallback for `HETERION_ROUTER_*`.
- `~/.omniroute` remains a legacy data-directory fallback.
- Ports `20128` and `20129` remain unchanged.
- `enc:v1:` and `omniroute-field-encryption-v1` remain frozen for existing
  databases.

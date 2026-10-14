# MISSION-2026-10 — «Fail-closed & reproducible»

> Канонический план следующей миссии Heterion Router.
> Составлен: 2026-09-29 · Baseline: `main` @ `51dfe6d` + рабочее дерево
> (на момент старта планирования: 35 M / 5 ??; этот план добавляет ещё 2 untracked-файла)
> Статус: **утверждён к реализации** · Реализация не начата
> Evidence: `docs/research/mission-2026-10/` (6 параллельных архитектурных отчётов)

Миссия закрывает **не новые фичи, а расхождение между тем, что шлюз обещает
клиенту, и тем, что он реально делает**: silent loss на Responses-пути, утечку
upstream-текста в SSE, ложную атрибуцию в usage/call-logs и невоспроизводимый
baseline. Направление проекта (Rust OpenAI-compatible gateway + SQLite/combo
routing + Tauri sidecar) не меняется.

---

## 0. Как читать этот документ

Метки достоверности обязательны и не смешиваются:

| Метка | Значение |
|---|---|
| **[V]** | Проверено в текущем дереве: команда/чтение кода, воспроизводимо |
| **[A]** | Проверено агентом миссии, приложено в `docs/research/mission-2026-10/` |
| **[P]** | Проектная оценка/предложение, не измерено |
| **[?]** | Открытый вопрос, требует решения владельца (см. §13) |

Ссылки вида `file.rs:123` указывают на **текущее рабочее дерево**. Ссылки на
замороженный `HEAD` помечены явно.

---

## 1. Objective

> Сделать шлюз честным под агентным циклом и воспроизводимым как артефакт:
> ни одно поле, меняющее agent loop, не теряется молча; ни одна ошибка
> провайдера не утекает клиенту; каждый запрос, попавший в usage/call-logs,
> атрибутируется реальному провайдеру, модели и статусу; всё это
> воспроизводится из зафиксированного `HEAD`.

| # | Результат | Метрика «готово» |
|---|---|---|
| **O-1** | Нет silent data loss на Responses-пути | `reasoning` input items (вкл. `encrypted_content`) доходят до upstream байт-в-байт **или** приходят `400 unsupported_capability`; built-in tools отклоняются, а не выбрасываются; tool-loop стрим проходит lifecycle-валидатор |
| **O-2** | Нет утечки upstream-текста в SSE | Wiremock-маркер в теле ответа отсутствует в SSE-фрейме, присутствует в `gateway.log`; Responses-only поля не попадают в Chat body |
| **O-3** | Attribution и статус достоверны | `provider` ≠ `"routing"`, `connection_id` заполнен, `combo_name` записан, `call_logs.status` = реальный HTTP-код, `requested_model` ≠ `model` при combo, `Retry-After` учитывается |
| **O-4** | Baseline воспроизводим | `git clone` в чистый каталог → `scripts/ci/check.sh` = `all green`; все doc-ссылки резолвятся; `git status --porcelain` пуст на границе milestone |

---

> **Снимок 2026-09-29.** Таблица ниже фиксирует состояние на момент написания
> плана и с тех пор не пересчитывалась — часть строк уже неверна (тестов было
> 213, toolchain стоял на `1.98.0`, диффы fmt в Tauri-крейте были). Актуальные
> числа — в `STATUS.md`; что из этого раздела уже закрыто — в ADR-012.

## 2. Проверенный baseline

### 2.1 Git и гейты **[V]**

| Факт | Значение |
|---|---|
| `HEAD` | `51dfe6d` на `main` |
| Рабочее дерево | 35 modified + 5 untracked (`docs/ROADMAP-2026-09.md`, `ADR-009`, `ADR-010`, `docs/history/README.md`, `docs/research/`) |
| `scripts/ci/check.sh` | `check.sh: all green` (прогнан 2026-09-29) |
| `cargo test --workspace` | **213 passed, 0 failed** (пересчитано мной, не унаследовано) |
| `cargo fmt --manifest-path desktop/src-tauri` | **2 диффа** в `build.rs:1` и `src/lib.rs:81` — Tauri-крейт вне workspace, гейтом не ловится **[V]** |
| `rust-toolchain.toml` | `channel = "1.99.0"` — было `1.98.0` на момент плана (ADR-012) |
| `.github/workflows/ci.yml:39` | `dtolnay/rust-toolchain@stable` — CI не проверяет закреплённый компилятор **[V]** |
| `.github/workflows/release.yml:24,44` | `macos-14` + `sha256sum` (не переносимо) **[V]** |

### 2.2 Registry **[V]** (пересчитано мной через `python3`)

| Метрика | Рабочее дерево | `HEAD` |
|---|---|---|
| providers / models | 278 / 2717 | 278 / 2715 |
| проходят structural builder gate | 259 / 2108 | — |
| HTTP(S)-capable | 253 / 1916 | — |
| неподдерживаемые схемы | 6 / 192 | — |
| не проходят format/base-URL gate | 19 / 609 | — |
| bespoke `executor` обслуживается generic path | 61 / 959 | — |

`crates/heterion-router-providers/src/lib.rs:3` всё ещё утверждает «277 providers»
— устаревший комментарий, не совпадает ни с деревом, ни с `HEAD` **[V]**.

### 2.3 Live runtime **[V]** (read-only запросы к живой БД, PID 68027, старт 2026-09-29 11:16)

| Факт | Значение |
|---|---|
| Живой listener | `0.0.0.0:20128`, launchd `com.heterion.router`, `ProgramArguments` указывает в dev-дерево `target/release/` |
| Активных провайдеров в БД | **24** (из них бэкендов в логе — **19**) |
| Провайдеры с пустыми credentials | `kimi-coding`, `kiro`, `opencode` (по 1 строке, `api_key` пуст) |
| `exa-search` | **5** активных connection-строк, но провайдера нет в `registry.json` → молча теряются при `select_backend` |
| Combos | 8 строк; `vivanta-auto` и `vivanta-deepseek` имеют `models: []` (0 элементов, `strategy: "auto"`) |
| `usage_history.status` | **два непересекающихся словаря**: цифровые коды (`200`/`429`/`502`/…) — JS-сторона; `succeeded` (174) / `failed` (154) — Rust. `WHERE status='429'` и `WHERE status='succeeded'` не пересекаются |
| `call_logs` от Rust | 11 строк `status=500, provider='routing', error_summary='stream_error'` на combo `vivanta-auto` — 11 routing-промахов записаны как серверные ошибки |
| Responses store | namespace `responses` в `key_value` — **0 строк**; production-трафика по store не было |
| `STORAGE_ENCRYPTION_KEY` | присутствует в `~/.heterion-router/.env` (проверено имя переменной, не значение); в `EnvironmentVariables` plist его нет, но `crypto::load_secret` (`crypto/src/lib.rs:159-177`) читает `<data_dir>/.env` ⇒ **на этом хосте plaintext-путь не активен**, код-путь остаётся |
| `provider_connections` | колонки `is_banned` **не существует** (есть `is_active`, `access_token`, `refresh_token`, `api_key`, …) — часть запросов в отчётах к этой таблице некорректна |

### 2.4 Подтверждённые дефекты (выборочно, с evidence)

| ID | Дефект | Evidence |
|---|---|---|
| C-1 | Streaming error frames не sanitized: raw upstream-текст попадает в SSE | **[V]** `lib.rs:538-545` sanitized только для non-stream |
| C-2 | `reasoning` input items выбрасываются (`_ => {}` с комментарием «ponytail») | **[V]** `lib.rs:1015-1017` |
| C-3 | Built-in tools выбрасываются `filter_map`, требующим top-level `name` | **[V]** `lib.rs:1068-1069` |
| C-4 | Responses SSE — гибридная не-SDK форма: `response.reasoning_text.delta` (нет ни в OpenAI, ни в xAI), `call_id: ""`, `completed` без `id`/`output`/`sequence_number` | **[V]** `lib.rs:1254-1298` **[A]** `02` |
| C-5 | Storage key берётся из **upstream** `response["id"]`; `store` дефолт `true`; поток не пишется в store вовсе; prune/TTL отсутствуют | **[V]** `lib.rs:573-594`, `lib.rs:830-833`, `lib.rs:877-881`; схема `schema_base.sql:65-70` без `stored_at` |
| C-6 | Alias-keyed connection возможен как `servable:true`, но не routable; `/v1/models` не канонизирован | **[V]** `main.rs:168-172`, `routing.rs` |
| C-7 | Attribution фиктивна: `provider = state.backend.name()` → `"routing"` для `RoutingBackend`; `connection_id = None`; `status = 200/500`; `requested_model == model` | **[V]** `lib.rs:308-367`, `routing.rs:197` |
| C-8 | NULL `scopes` → `COALESCE(scopes,'["manage"]')`, NULL `allowed_models` → `[]` ⇒ legacy-ключ получает `manage` и все модели | **[V]** `repos.rs:356-363` |
| C-9 | Колонка `key` хранит **хеш** (`insert_full_api_key` пишет `row.key_hash` и в `key`, и в `key_hash`) ⇒ прочитанный из БД хеш является валидным bearer; индекса на `key_hash` нет (есть только `idx_ak_key` на `key`) | **[V]** `repos.rs:226-252`, `repos.rs:362`, `schema_base.sql:90` |
| C-10 | `count_active_api_keys` фильтрует только `revoked_at IS NULL`, игнорируя `is_banned`/`is_active`/`expires_at`, которые учитывает `find_usable_api_key` ⇒ `/api/overview` завышает число рабочих ключей | **[V]** `repos.rs:941-948` vs `repos.rs:353-364` |
| C-12 | `restrict_data_files` вызывается **до** создания текущего log-файла ⇒ логи текущего дня могут оставаться `0644` | **[V]** `main.rs:390-394` |
| C-17 | Dev-fallback `api.js:45` и `vite.config.js:7` указывают на `20128`, тогда как sidecar и dev-gateway — `20129`; `tauri dev` не поднимает sidecar (`!cfg!(debug_assertions)`) ⇒ dev-окно пишет в БД живого сервиса | **[V]** `desktop/ui/src/api.js:45`, `desktop/ui/vite.config.js:7`, `desktop/src-tauri/src/lib.rs:21,55` |
| C-18 | В CI нет desktop-job; Tauri-крейт вне workspace, 2 диффа `fmt` не ловятся | **[V]** выше |
| C-19 | `tauri.conf.json:20` `csp: null` + `capabilities/default.json:6` `shell:allow-spawn` (спавн идёт из Rust, capability не используется) | **[V]** |
| C-21 | `release.yml` шлёт только gateway-бинарь, `macos-14` + `sha256sum` | **[V]** |
| C-24 | Responses-only поля (`store`, `include`, `reasoning`, `previous_response_id`, `text`, `session_id`, `conversation_id`) не фильтруются при маршрутизации на Chat-only provider | **[A]** `02` §5.3 |
| **NEW-1** | **Cross-tenant read через коллизию storage id:** `kv_set` upsert по upstream-controlled id ⇒ провайдер, вернувший чужой id (или `chatcmpl-unknown` при отсутствии id), перезаписывает запись вместе с `api_key_id` | **[A]** `04` §2.4; `[V]` `lib.rs:594` |
| **NEW-2** | **Fresh clone не собирает desktop:** `externalBin` (`tauri.conf.json:33`) указывает на `binaries/…`, а каталог в `.gitignore:7`; `stage-sidecar.sh` вызывается только из `beforeBuildCommand` | **[A]** `05` §3 (воспроизведено на изолированной копии, exit 101) |
| **NEW-3** | **Bundle не проходит `codesign --verify --deep --strict`** даже у свежей сборки 2026-09-29 (ad-hoc/linker, resources seal отсутствует) | **[A]** `05` §3 |

Полные списки: `docs/research/mission-2026-10/01` §1.5 (24 дефекта C-1…C-24),
`04` §2 (12 доменов безопасности), `05` §3 (10 операционных находок),
`06` §7 (12 coverage-gap'ов).

---

## 3. Non-goals

| # | Не делаем | Почему |
|---|---|---|
| NG-1 | Native Anthropic Messages backend / `/v1/messages` | Трансляторы `claude.rs`/`claude_request.rs` существуют, но `AnthropicBackend` нет — это фича-поверхность, а не дефект **[A]** `02` §7, `06` §6 |
| NG-2 | ChatHub как upstream | ADR-010; anonymous session не выдаётся (403/401) **[V]** |
| NG-3 | WebSocket, hosted MCP, Conversations API, background jobs, все OpenAI built-in tools | Уже вне milestone; нет подтверждённого клиента **[V]** `ROADMAP` §«Границы» |
| NG-4 | Полный Grok Build conformance harness + живой canary | Требует реального `grok` CLI. Инфраструктуру canary (opt-in, dry-run) закладываем в M3 **[A]** `06` |
| NG-5 | Notarization подписанного `.dmg` | Требует Apple Developer Team ID, которого нет **[A]** `05` §6.4 |
| NG-6 | TLS-терминация и LAN-hardening | Задача reverse-proxy/VPN вне шлюза; миссия лишь документирует факт `0.0.0.0` bind **[V]** §2.3 |
| NG-7 | Enforcement квот/лимитов по существующим колонкам `api_keys` | Продуктовое решение (кто платит, чем ограничиваем). Миссия фиксирует «схема ≠ enforcement» в ADR **[A]** `04` §2.11 |
| NG-8 | Реализация `weight` / `strategy:"auto"` | Миссия **удаляет** из UI то, что не работает (H1/H2), а не изобретает алгоритм **[V]** `combos.rs` |
| NG-9 | Несовместимое изменение `enc:v1:`/`STATIC_SALT` или схемы `api_keys` | Frozen interop; живые БД держат эти значения **[V]** `crypto/src/lib.rs:18-23` |

---

## 4. Принципы миссии

1. **Fail closed.** Нельзя молча выбрасывать поля, меняющие agent loop.
2. **Capability по effective model format**; registry metadata ≠ доказательство executor'а.
3. **Opaque reasoning — это данные.** Не превращать в видимый текст, не логировать.
4. **State origin must be explicit.** Продолжение не меняет незаметно provider/credential.
5. **Public static surface — это граница.** `/healthz` и SPA публичны; API и admin — нет.
6. **Сначала красный тест, потом фикс.** 22 вызова `wiremock.expect()` и **0** вызовов
   `verify()` **[V]** ⇒ существующие ассерты декоративны; fix-first без красного теста
   воспроизводит ту же проблему.
7. **Никакой проверки mission-правок на живом сервисе** (`0.0.0.0:20128`, бинарь из
   dev-дерева, стартовал 11:16). Только чистый clone + `check.sh` + wiremock.

---

## 5. Workstreams

Effort — проектные полосы в человеко-днях для одного инженера **[P]**.
Владелец — роль субагента из §6.

### WS-A — Baseline Freeze & Doc Truth → O-4 · Agent **A6** · 1.5–2.5 дня

| # | Задача |
|---|---|
| A1 | Разбить `+2997/−364` на осмысленные коммиты (код / тесты / docs) — не один `wip` **[P]** |
| A2 | `git add` untracked: `docs/ROADMAP-2026-09.md`, `docs/MISSION-2026-10.md`, `ADR-009..011`, `docs/history/README.md`, `docs/research/` **[V]** |
| A3 | Поправить `providers/src/lib.rs:3` («277» → фактическое число) и зафиксировать в ROADMAP разницу «дерево 278/2717 vs HEAD 278/2715» **[V]** |
| A4 | Удалить tracked 0-byte `desktop/src-tauri/icons/.!15396!128x128.png`, добавить `._*`/`.!*` в `.gitignore` **[V]** |
| A5 | ADR-addenda (не переписывая ADR-001..007): ADR-007 (sidecar, injected global, single-instance), ADR-002/006 (`HETERION_ROUTER_*` первичен), ADR-008 (`rust.yml` → `ci.yml`) **[A]** `01` §5 |
| A6 | `docs/history/` — `git mv` в `upstream-omniroute-js-history/` с README; поправить 5 ссылок **[A]** `01` §5 |
| A7 | Переименовать `file.rs:N` → `file.rs:symbol` в roadmap-цитатах, где строки устарели **[A]** `01` §5 |
| A8 | `STATUS.md` «Проверки» → «локально; шире, чем CI» + перечень расхождений **[V]** §2.1 |
| A9 | `README.md`: plist машинно-локальный, `0.0.0.0` bind, Vite-proxy → 20128 **[V]** |
| A10 | ADR-011: non-goals миссии, «схема ≠ enforcement», решение по Responses store **[?]** §13 |

### WS-B — Fail-Closed Correctness → O-1, O-2 · Agent **A2** · 5–8 дней

| # | Задача | Дефект |
|---|---|---|
| B1 | Sanitize SSE error frames той же веткой, что `error_response`; wire-регресс с маркером | C-1 |
| B2 | Сохранять `reasoning` input items (вкл. `encrypted_content`) байт-в-байт **или** `400 unsupported_capability`; двухходовый `store:false` replay-тест | C-2 |
| B3 | Reject unsupported built-in tools (`web_search`/`x_search`/неизвестный `type`) → `400`, если target provider их не объявляет | C-3 |
| B4 | Типизированный Responses SSE lifecycle + **валидатор** (ids, индексы, ровно один terminal event) | C-4 |
| B5 | Синтез tool-call из `response.completed` snapshot (xAI whole-call dialect); `finish_reason` ≠ `stop` при наличии tool call | C-4 |
| B6 | Фильтрация Responses-only полей при маршрутизации на Chat-only provider + wire-тест | C-24 |
| B7 | Reasoning/cache token usage доходит до клиента (сейчас теряется дважды) | **[A]** `02` §1 |

B4 — самый дорогой пункт: переписывает публичную форму SSE и требует отдельного PR.

### WS-C — State Store: Ownership, Retention, Confidentiality → O-1/O-2 · Agent **A4** · 2–3 дня

| # | Задача | Дефект |
|---|---|---|
| C1 | Gateway-owned storage key (`resp_<uuid>`), upstream id — поле внутри записи; read-fallback по старому id | C-5, NEW-1 |
| C2 | Retention: индекс по `stored_at`, prune при старте (default 30 дней, конфигурируемо) + byte-cap | C-5 |
| C3 | Решение в ADR: шифровать envelopes через `FieldCrypto` или явно объявить границей права файла БД | **[?]** §13 Q3 |
| C4 | `no_log` подавляет и `persist_response`, а не только usage/call-log | **[A]** `02` |
| C5 | Зафиксировать дефолт `store` (сейчас `true`) | **[?]** §13 Q4 |
| C6 | Миграция `002` (append-only, ledger без downgrade; `migrate.rs:31-36`) | **[V]** |

### WS-D — Routing Identity & Observability → O-3 · Agent **A3** · 2.5–4 дня

| # | Задача | Дефект |
|---|---|---|
| D1 | Канонизировать `provider` **до** `backends.insert`; регресс-тест на alias-строку | C-6 |
| D2 | Прокинуть `(backend, model, connection, combo)` в `record_completion_usage`/`record_stream_usage` | C-7 |
| D3 | `call_logs.status` = реальный код; `requested_model` ≠ `model`; писать `combo_name` | C-7 |
| D4 | Upstream request-id + `Retry-After` извлекать и **учитывать** в backoff | **[A]** `03` §7.4 |
| D5 | `count_active_api_keys` учитывает `is_active`/banned/expired (совпадает с `find_usable_api_key`) | C-10 |
| D6 | Возврат `x-request-id` клиенту для трассировки через `/api/logs` | **[P]** |
| D7 | Отчёт о недостижимых registry-записях (25/801 не строятся, 61/959 игнорируют executor) в `/api/overview` | **[A]** `03` |
| D8 | Единый классификатор `Fault` вместо разбора строк; Grok `426` → `503` без retry | **[A]** `03` §7.4 |
| D9 | Единый гейт `Serveability` вместо трёхкаскадного сравнения строк | **[A]** `03` §7.2 |
| D10 | Unify vocabulary `usage_history.status` (328 строк миграции данных) | **[V]** §2.3 |

### WS-E — Auth & Data-Boundary Hardening → O-2 · Agent **A4** · 3–4.5 дня

| # | Задача | Дефект |
|---|---|---|
| E1 | Fail-closed policy для NULL `scopes`/`allowed_models` **или** ADR-фиксация parity; обновить `usable_keys_honour_kill_switches` | C-8 **[?]** §13 Q1 |
| E2 | `PATCH /api/keys/{id}` для сужения существующего ключа | **[A]** `04` |
| E3 | Отказ от plaintext-записи refresh-токена без `STORAGE_ENCRYPTION_KEY` + `warn!` на старте | **[A]** `04` §2.3 |
| E4 | Колонка `key` перестаёт быть валидным bearer'ом; индекс на `key_hash`; legacy-plaintext предикат | C-9 |
| E5 | Создание log/signature файлов с явным `0600`, включая текущий день | C-12 |
| E6 | `HETERION_ROUTER_UI_DIR` containment check; `REQUIRE_AUTH_UI` для non-loopback | **[A]** `04` |
| E7 | `launchctl` через `argv`, не `sh -c` | **[A]** `04` |
| E8 | Redacting `Debug` на credential-структурах (прецедент `FieldCrypto`) | **[A]** `04` |
| E9 | `logs_app` — tail вместо slurp | **[A]** `04` |
| E10 | ADR: ключ шифрования лежит рядом с шифротекстом ⇒ AES-GCM защищает от экспорта файла, но не от чтения каталога | **[A]** `04` §2.3 |

### WS-F — Operational Boundary (desktop, service, release) → O-4 · Agent **A5** · 3–4.5 дня

| # | Задача | Находка |
|---|---|---|
| F1 | `api.js:45` fallback и `vite.config.js:7` → `20129` (или громкая ошибка вместо fallback) | C-17 |
| F2 | Dev-изоляция: `tauri dev` поднимает sidecar либо data-dir `~/.heterion-router-dev`; документировать override | NEW (B2 в `05`) |
| F3 | Readiness/ownership handshake: эфемерный порт либо one-time nonce; чужая слушающая сторона отвергается явно | **[A]** `05` §6.2 |
| F4 | Sidecar death: bounded respawn + видимое состояние в окне | **[A]** `05` |
| F5 | «Restart now»: sidecar-aware либо честно disabled с объяснением | **[A]** `05` |
| F6 | Коммит `scripts/launchd/com.heterion.router.plist` с шаблонизированными путями + install/uninstall | **[A]** `05` |
| F7 | Включить CSP в `tauri.conf.json`; сузить capability `main` (убрать неиспользуемый `shell:allow-spawn`) | C-19 |
| F8 | `cargo fmt --manifest-path desktop/src-tauri` в `check.sh`; починить 2 диффа | C-18 **[V]** |
| F9 | CI: `desktop/src-tauri/**` и `docs/**` в `paths:`; Tauri job; закрепить toolchain `1.99.0` вместо `@stable` | **[V]** §2.1 (ADR-012) |
| F10 | `release.yml`: `shasum -a 256`, `macos-15`, релиз только при зелёном CI того же SHA | C-21 |
| F11 | `LSMinimumSystemVersion`, `engines` для Node, пин `cargo tauri` | **[A]** `05` |
| F12 | `stage-sidecar.sh` принимает готовый бинарь из `release.yml`; починить fresh-clone build | NEW-2 **[A]** `05` |
| F13 | Подпись: проверить `codesign --verify --deep --strict`; зафиксировать статус «developer-machine artifact» | NEW-3 **[A]** `05` |

F3 — самая рискованная часть (0 тестов в Tauri-крейте). Имеет запасной вариант.

### WS-G — Test Foundation · Agent **A1** · 3–4.5 дня

| # | Задача |
|---|---|
| G1 | `MockServer::verify()` во всех 22 местах с `.expect()` (сейчас 0 вызовов) **[V]** |
| G2 | Вынести `test_support` из `lib.rs` в переиспользуемый модуль (`test-support` feature) **[A]** `06` §9.1 |
| G3 | Handler-тесты admin API (13 handlers / 1 тест) **[A]** `06` |
| G4 | CORS + dashboard через `Router::oneshot`: preflight, `GET /`, `GET /{spa-route}` |
| G5 | Combo retry/failover: transient→success, счётчик попыток, delay-путь, breaker-взаимодействие |
| G6 | Breaker half-open: N конкурентных ⇒ ровно один probe |
| G7 | Desktop Rust: `sidecar_port()` precedence + контракт single-instance |
| G8 | Интеграционный тест построения backend из **реальной строки** `provider_connections` (иначе C-6 невидим) |
| G9 | Расширить JS-parity фикстуры (~32 кейса; `gemini_to_openai` = 3) |
| G10 | `cargo audit` / `cargo deny` в `check.sh` |
| G11 | `scripts/qa/sanitize-fixtures.sh` (allowlist, не denylist) + `check-docs.sh` (резолв doc-ссылок) **[A]** `06` §9.2 |
| G12 | `PROVENANCE.json` для parity-фикстур (GOV-4) |

### WS-H — Dead Config & Semantics Closure · Agent **A3** · 0.5–1 день

| # | Задача | Дефект |
|---|---|---|
| H1 | `maxSetRetries` / `setRetryDelayMs`: реализовать **или** удалить из `ComboConfig`, парсера и UI | **[V]** `combos.rs` |
| H2 | `weight` / `strategy:"auto"`: убрать из UI-редактора, оставив round-trip | **[V]** |
| H3 | Удалить мёртвое `RoutingBackend::combo_configs`; убрать двойной `load_combos` на запрос | **[A]** `01` §5 |
| H4 | Affordance «этот combo пуст» в дашборде (2 из 8 живых комбо не обслуживаются) | **[V]** §2.3 |
| H5 | Combo с `models: []` → `404 no_candidate` вместо `500` в логах | **[V]** §2.3 |

### Сводка effort **[P]**

| WS | Полоса | Параллелизуемо? |
|---|---|---|
| A — Baseline | 1.5–2.5 | нет, блокирует всё |
| B — Fail-closed | 5–8 | частично (B1/B2/B3 независимы от B4) |
| C — State store | 2–3 | после B2 |
| D — Routing/observability | 2.5–4 | да, независим от B |
| E — Auth/data | 3–4.5 | да |
| F — Ops/desktop | 3–4.5 | да |
| G — Test foundation | 3–4.5 | да, сквозной |
| H — Dead config | 0.5–1 | да, в конце |
| **Итого** | **20.5–32** | ядро (A+B+D) ≈ **9–14.5** |

---

## 6. Роли субагентов и границы write-скоупа

Параллельность разрешена **только** при непересекающихся write-скоупах.
Каждая роль получает полный read на репозиторий и пишет **только** в свои пути.

| Роль | Workstream | Write-скоуп (эксклюзивно) | Запрещено трогать |
|---|---|---|---|
| **A1** Test Foundation | WS-G | `crates/*/tests/**`, `crates/heterion-router-gateway/src/test_support.rs`, `scripts/qa/**` | production-код вне `test_support` |
| **A2** Responses | WS-B | `crates/heterion-router-translate/src/**`, `crates/heterion-router-gateway/src/responses.rs`, `chat.rs`, `backend.rs` | `repos.rs`, `auth.rs`, `routing.rs`, desktop, CI |
| **A3** Routing & Observability | WS-D, WS-H | `crates/heterion-router-gateway/src/{routing,combos}.rs`, `main.rs`, `crates/heterion-router-usage/src/records.rs`, `desktop/ui/src/**` | `lib.rs` без согласования, `repos.rs`, ADR |
| **A4** Security & State | WS-C, WS-E | `crates/heterion-router-db/src/{repos,migrate}.rs`, `schema_base.sql`, `gateway/src/{auth,apikeys,credentials}.rs`, `crypto` | `routing.rs`, desktop, CI, translate |
| **A5** Desktop & Release | WS-F | `desktop/**`, `scripts/launchd/**`, `.github/workflows/**`, `scripts/ci/check.sh` | `crates/**` (кроме `test_support` через A1) |
| **A6** Baseline & Docs | WS-A | `docs/**`, `README.md`, `STATUS.md`, `CHANGELOG.md`, `AGENTS.md`, `.gitignore`, `crates/heterion-router-providers/src/lib.rs` | любой production-код |

**Правила разрешения конфликтов:**

1. `crates/heterion-router-gateway/src/lib.rs` — **общий файл**. Правки сериализуются:
   A2 владеет Responses-участком, A3 — usage/call-участком, A4 — store/auth-участком.
   Перед началом каждого дня A2/A3/A4 публикуют список hunks; merge-очередь —
   A2 → A3 → A4 на каждом milestone.
2. Никогда не смешивать WS-A с WS-B/C/D в одном коммите: baseline-коммиты идут первыми.
3. Изменения схемы — отдельный коммит (WS-C) до остального WS-C.
4. Каждый агент завершает работу прогоном `scripts/ci/check.sh` на своём дереве и
   предъявляет список затронутых файлов.

**Интеграция:** после каждого milestone один агент-интегратор (роль A6) делает
`git rebase`/`squash` в `main` только при зелёном CI, иначе milestone откатывается,
а не смягчается.

---

## 7. Зависимости

```text
                ┌──────────────────────┐
                │  WS-A  Baseline      │  ← M0, жёсткий блокер
                │  freeze + doc truth  │
                └──────────┬───────────┘
        ┌──────────────────┼──────────────────────────┐
        ▼                  ▼                          ▼
┌───────────────┐  ┌──────────────────┐      ┌────────────────┐
│ WS-G Tests    │  │ WS-D Routing &   │      │ WS-F Ops /     │
│ G1 → G2..G12  │  │ observability    │      │ desktop        │
└───────┬───────┘  └────────┬─────────┘      └───────┬────────┘
        │ даёт красные      │                       │ F3 требует
        │ тесты для B       │                       │ тестов G7
        ▼                   ▼                       │
┌───────────────────────────────────────┐            │
│ WS-B  Fail-closed P0                  │            │
│ B1 │ B2 ──┐ │ B3 │ B6 │ B7  (B4,B5)   │            │
│ B4 ◄── G4, G8 ─┤                      │            │
│ B5 ◄── B4 ────┘                      │            │
└───────────────┬───────────────────────┘            │
                │ B2 определяет, что хранить           │
                ▼                                      │
┌───────────────────────────┐  ┌──────────────┐        │
│ WS-C  State store C1..C6  │  │ WS-E Auth    │        │
└───────────────────────────┘  │ E1..E10      │        │
                               └──────────────┘        │
┌────────────────────────────────────────────────────┐
│ WS-H Dead config H1..H5  (после B, D, E)            │
└────────────────────────────────────────────────────┘
```

Критические рёбра:

- **WS-A → всё.** Без зафиксированного `HEAD` миссия не откатывается.
- **G1/G2 → WS-B.** Без harness'а «красный тест перед фиксом» неосуществим.
- **B2 → C1..C6.** Решение «хранить reasoning или отклонять» определяет содержимое store.
- **G7 → F3.** Handshake нельзя покрыть тестом без обвязки Tauri-shell.
- **E1 → обновление существующих тестов** (`usable_keys_honour_kill_switches`): это
  намеренное изменение test-locked контракта, не регресс.
- **B4 → H1.** Валидатор SSE определяет, может ли combo-конфиг влиять на выдачу.

---

## 8. Milestones

### M0 — Reproducible Baseline → O-4 · **1.5–2.5 дня** · WS-A

**DoD:**
- `git status --porcelain` пуст; `HEAD` содержит весь код, описанный в `STATUS.md`.
- `git clone` в чистый каталог → `scripts/ci/check.sh` = `all green`.
- Ни одной doc-ссылки на отсутствующий в `HEAD` файл.
- 0-byte `._15396!128x128.png` удалён; `.gitignore` прикрывает `._*`/`.!*`.
- `STATUS.md` «Проверки» явно называет расхождения с CI.
- ADR-011 принят (non-goals, «схема ≠ enforcement», решения §13).

**Стоп-критерий:** фиксация baseline требует >3 дней ⇒ миссия переходит в режим
диагностики (R-1), WS-B не начинается.

### M1 — Fail Closed → O-1, O-2 · **4–6 дней** · G1, G2(min), B1, B2, B3, B6, B7, D1

**DoD:**
- B1: wiremock-тест, тело ответа содержит маркер → маркера **нет** в SSE-фрейме, он есть в `gateway.log`.
- B2: `reasoning` item с `encrypted_content` доходит до тела upstream **побайтово** **или** приходит `400 unsupported_capability`; двухходовый `store:false` replay проходит.
- B3: `tools:[{"type":"web_search"}]` + chat-only provider ⇒ `400`, поле не теряется молча.
- B6: перехваченное тело `/chat/completions` не содержит `store`/`include`/`reasoning`/`previous_response_id`/`text`/`session_id`/`conversation_id`.
- D1: backend, построенный из строки с алиасом, находится `routing.plan()`, рекламируется в `/v1/models`, не теряется в combo-plan.
- Все существующие `.expect()` вызывают `verify()`; harness переиспользован минимум в 5 новых тестах.
- `cargo test --workspace` ≥ **225** passed, 0 failed.

**Стоп-критерий:** B4 оценивается в >6 дней ⇒ переносится первым пунктом в M2.

### M2 — Trustworthy State & Attribution → O-3 · **5–7 дней** · B4, B5, C1..C6, D2..D10, E1, E3, E5, H1..H5

**DoD:**
- B4: tool-loop стрим проходит валидатор: `response.created` есть, `output_item.added` несёт настоящий `call_id`, `output_item.done` есть, **ровно один** terminal event, индексы монотонны.
- B5: snapshot-only tool call даёт `finish_reason: "tool_calls"`, не `"stop"`.
- C1: storage key — gateway-minted; тест «upstream вернул id, совпадающий с чужим» не разрушает чужую запись; **старый** id резолвится после миграции.
- C2: prune по retention срабатывает; запись уважает byte-cap.
- C6: миграция `002` в `MIGRATIONS`, append-only, ledger-семантика сохранена.
- D2/D3: для combo-запроса `usage_history` содержит имя **конечного** provider'а, `combo_name`, реальный HTTP-код в `call_logs`, `requested_model != model`.
- D4: `Retry-After: 3` ⇒ пауза ≥3 s; upstream request-id сохраняется и возвращается клиенту.
- D5: `count_active_api_keys` совпадает с `find_usable_api_key` на наборе из revoked/banned/inactive/expired.
- D10: после миграции `WHERE status='429'` и `WHERE status='succeeded'` описывают один словарь.
- E1: решение (fail-closed или ADR-фиксация parity) **записано**; при fail-closed `usable_keys_honour_kill_switches` обновлён, а не удалён.
- E3: без `STORAGE_ENCRYPTION_KEY` refresh-запись **отказывает**; `warn!` на старте.
- E5: `gemini-thought-signatures.jsonl` и текущий `gateway.log` создаются `0600`.
- E4: `key`-колонка больше не валидный bearer; `key_hash` проиндексирован; legacy-plaintext строки аутентифицируются.
- H5: combo с `models: []` даёт `404` + `error_summary="no_candidate"`, ноль строк `provider='routing'`.
- `cargo test --workspace` ≥ **245** passed, 0 failed.

### M3 — Operational Boundary & CI → O-4 (операционная часть) · **5–7 дней** · WS-F, E2/E6/E7/E9/E10, G3..G12

**DoD:**
- F1/F2: `api.js` и `vite.config.js` указывают на `20129`; dev-окно не пишет в БД живого сервиса; `AGENTS.md`/README отражают это.
- F3: sidecar поднимается на эфемерном порту **или** требует одноразовый nonce; коллизия порта даёт явную ошибку, а не логин в пустоту.
- F5: restart либо работает, либо не показывается с объяснением; тест на `/api/restart` (409 vs 202).
- F6: `scripts/launchd/*.plist` в репозитории + install/uninstall; README перестаёт быть неисполнимым.
- F8: `cargo fmt --manifest-path desktop/src-tauri` в `check.sh`/CI; 2 диффа починены.
- F9: `desktop/src-tauri/**` и `docs/**` в `paths:` — сделано; Tauri job в CI —
  сделано частично (`tauri-fmt`, только парсинг, без системных библиотек;
  `cargo check`/`tauri build` требуют webkit2gtk/GTK и в CI не гоняются);
  toolchain закреплён — сделано, но на `1.99.0`, а не на `1.98.0` (ADR-012).
- F10: `shasum -a 256`; `macos-15`; релиз требует зелёного CI того же SHA.
- F12: **fresh clone собирает desktop** (бывший blocker B1).
- G3: **все 13** admin handlers имеют handler-тесты.
- G4: preflight и `GET /` / `GET /{spa-route}` проходят через `build_router_with_state`.
- G6: N конкурентных запросов ⇒ ровно один half-open probe.
- G11/G12: `scripts/qa/sanitize-fixtures.sh` и `check-docs.sh` зелёные; `PROVENANCE.json` присутствует.
- `cargo test --workspace` ≥ **290** passed, 0 failed; `scripts/ci/check.sh` зелёный на **чистом clone**.

**Стоп-критерий:** F3 требует >2 дней или ломает single-instance ⇒ заменить на честное
удаление Tauri-fallback'а (F1) + явную ошибку в окне; записать «desktop restricted»
в `STATUS.md`.

---

## 9. Протокол верификации

### 9.1 Обязательный гейт каждого агента перед сдачей

```sh
scripts/ci/check.sh                                    # = fmt + clippy + test
# плюс, если затронут crate:
cargo test -p <crate>                                   # адресный прогон
# плюс, если затронут desktop:
npm --prefix desktop/ui run build
cargo fmt --manifest-path desktop/src-tauri/Cargo.toml -- --check
cargo check --manifest-path desktop/src-tauri/Cargo.toml --locked
```

### 9.2 Пороги тестового счётчика

| Milestone | Минимум passed | Основание |
|---|---|---|
| baseline | 213 | измерено мной 2026-09-29 |
| M1 | 225 | +≥12 новых |
| M2 | 245 | +≥20 новых |
| M3 | 290 | +≥45 новых |

Тестовые счётчики — атрибуты milestone, не жёсткий контракт: порог повышается,
если задача, удаляющая код, честно уменьшает число тестов (например, H1 при
выборе «удалить»), но общее число тестов на защищённой поверхности
(admin/SSE/alias) не должно падать (STOP-8).

### 9.3 Инварианты, которые нельзя нарушить **[V]**

- `cargo fmt --all`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace` — green.
- Upstream 4xx **не** роняет breaker (`routing.rs:44-55`).
- Keyless provider **не** отправляет `Authorization` (`client.rs:80-92`, тест `keyless_provider_sends_no_bearer`).
- Non-loopback bind без ключа **не** стартует (`main.rs:370-384`, тест `non_loopback_requires_a_key_before_startup`).
- JS-parity фикстуры `responses_to_openai.json`, `responses_tools.json`,
  `openai_to_responses.json` — **побайтово неизменны** (`AGENTS.md`).
- `STATIC_SALT`/`enc:v1:` и data-dir `~/.heterion-router` → legacy `~/.omniroute` — frozen.
- Миграции append-only; ни одна не удаляет/переименовывает колонку.

### 9.4 Что считается breaking для клиентов

| Изменение | Кто сломается | Как объявлять |
|---|---|---|
| B4: настоящий Responses SSE lifecycle вместо `output_item.tool_calls` | Клиенты на нестандартной форме | `CHANGELOG ### Breaking` + feature-flag на 1 релиз |
| B3: built-in tools ⇒ `400` вместо дропа | Агентные клиенты, полагавшиеся на дроп | `CHANGELOG`; это и есть O-1 |
| C1: смена формы storage key | Клиенты, сохранившие `previous_response_id` до обновления | read-fallback; тест «старый id резолвится» |
| E4: `key`-колонка перестаёт хранить валидный bearer | Скрипты, читающие `api_keys.key` | README + обратимая миграция |
| D3: `call_logs.status` становится реальным кодом | Дашборд, показывавший «500» | Ожидаемое изменение, отметить в CHANGELOG |

### 9.5 Rollback

1. Каждый workstream — отдельная ветка от M0; squash в `main` только на зелёном CI.
2. Изменения схемы — отдельный ревьюируемый коммит.
3. Milestone не прошёл DoD ⇒ **откат milestone, а не смягчение DoD**.
4. Живой runtime перезапускается только через launchd и только после зелёного CI;
   прямой `cargo run` на `20128` запрещён (иначе C-18 в проде).

---

## 10. Go / Stop критерии

### Go

| # | Критерий |
|---|---|
| GO-1 | `scripts/ci/check.sh` зелёный на чистом clone `HEAD` |
| GO-2 | Тестовые пороги §9.2 достигнуты, 0 failures |
| GO-3 | Ни один дефект C-1…C-24 не закрыт без падающего теста, который был красным до фикса |
| GO-4 | Ни одна миграция не ломает legacy plaintext-ключи в живой БД |
| GO-5 | `git status --porcelain` чист на границе каждого milestone |
| GO-6 | Для E1/E4 (test-locked поведение) решение записано в ADR, а не молчаливо |
| GO-7 | Ни один пункт не расширяет non-goals §3 без явного пересмотра charter |

### Stop

| # | Триггер | Действие |
|---|---|---|
| STOP-1 | Фиксация baseline >3 дней или дерево не собирается изолированно | MISSION → диагностика (R-1) |
| STOP-2 | Миграция ломает legacy plaintext-ключи в живой БД | Откат; E1 → «ADR-фиксация parity» вместо fail-closed |
| STOP-3 | B4 ломает replay `previous_response_id` или owner-scoped GET/DELETE | Выпустить B1/B2/B3/B6; B4 — отдельной миссией |
| STOP-4 | F3 handshake ломает single-instance или >2 дней | Заменить на F1 + явную ошибку; «desktop restricted» в STATUS |
| STOP-5 | C1 теряет состояние у клиентов с continuation | Откатить C1; оставить только prune |
| STOP-6 | Канонизация ломает `/v1/models` для живых комбо | Проверить 8 комбо против 24 провайдеров **до** мержа |
| STOP-7 | Пункт требует обращения к undocumented upstream (Grok proxy, ChatHub) | Пометить `[?]`; не строить workaround на непроверяемой гипотезе |
| STOP-8 | Число тестов на защищённой поверхности падает после рефакторинга | Откат рефакторинга; тесты — контракт |
| STOP-9 | `release.yml` падает на macOS-ноге после F10 | Не публиковать релиз; зафиксировать внешний блокер с датой |

---

## 11. Risk register

| ID | Риск | L | I | Триггер | Митигация |
|---|---|---|---|---|---|
| R-1 | Baseline не фиксируется (40 записей, ветвление) | Высокая | Высокая | STOP-1 | Разбивать по `git diff`-кластерам; иначе один честный коммит |
| R-2 | Fail-closed по policy (E1) **логинит живого оператора** | Средняя | Критическая | STOP-2 | Инвентаризация всех `api_keys` до правки; backfill-миграция; отказ от E1 при неполноте |
| R-3 | B4 ломает внешних SDK-клиентов на нестандартной форме | Средняя | Средняя | STOP-3 | Feature-flag на 1 релиз; `### Breaking` в CHANGELOG |
| R-4 | C1 ломает `previous_response_id` у всех, кто получил id до обновления | Средняя | Критическая | STOP-5 | Read-fallback по `upstream_response_id`; тест |
| R-5 | Retention-prune удаляет «долгоживущие» данные | Низкая | Высокая | — | Default 30 дней, конфигурируемо, prune только по `stored_at` |
| R-6 | F3 ломает единственную рабочую desktop-фичу | Средняя | Средняя | STOP-4 | Запасной вариант объявлен заранее |
| R-7 | Live-сервис на `20128` расходится с mission-правками | Высокая | Средняя | Любая «живая» верификация | Не проверять mission-правки на живом сервисе (принцип 7) |
| R-8 | Регресс в `select_backend` при канонизации (24→19 инвариант) | Средняя | Высокая | STOP-6 | Инвариантный тест `|backends| == 19` до и после; golden-тест на 8 комбо |
| R-9 | G1 (`verify()`) сразу вскрывает latent-баги в 22 ассертах | Средняя-высокая | Низкая | Сразу после G1 | Ожидать; считать **инцидентом-уведомлением**, а не поломкой миссии |
| R-10 | Mиссия разрастается («раз уж мы тут» — quota, Anthropic, notary) | Высокая | Средняя | Любой PR за пределами §5 | Non-goals §3 прибиты; нарушение = эскалация |
| R-11 | `docs/history/` переименование путается с удалением | Низкая | Низкая | — | По умолчанию `git mv` + README, не удаление |
| R-12 | Внешние док-источники дают 200, но это не conformance-тест шлюза | Средняя | Средняя | Правка «по документации» | Каждая ссылка помечена как ориентир; conformance — только wiremock-фикстуры |
| R-13 | `macos-14` удаляется с runner-образов 2026-11-02 | Низкая | Низкая | Дата | F10 в M3, до дедлайна |

---

## 12. Статус миссии (шаблон)

```markdown
# STATUS

> Mission: MISSION-2026-10 «Fail-closed & reproducible»
> Baseline: <sha> · Обновлено: <YYYY-MM-DD>

| MS | Название | Цель | Статус | DoD | Блокер |
|---|---|---|---|---|---|
| M0 | Reproducible Baseline | O-4 | ⬜ | _/6_ | — |
| M1 | Fail Closed | O-1, O-2 | ⬜ | _/7_ | — |
| M2 | State & Attribution | O-3 | ⬜ | _/15_ | — |
| M3 | Ops & CI | O-4b | ⬜ | _/13_ | — |

| # | Objective | Метрика | Сейчас | Цель |
|---|---|---|---|---|
| O-1 | Нет silent loss на Responses | reasoning wire-тест | ⛔ нет | ✅ есть |
| O-2 | Нет утечки upstream в SSE | маркер в SSE-фрейме | ⛔ есть | ✅ нет |
| O-3 | Достоверная атрибуция | provider ≠ "routing" | ⛔ "routing" | ✅ реальный |
| O-4 | Воспроизводимый baseline | clone → check.sh | ⛔ грязное дерево | ✅ зелёный |
```

---

## 13. Открытые решения владельца **[?]**

Эти пункты блокируют конкретные задачи и **не** могут быть решены агентом
самостоятельно — они меняют продуктовый контракт.

| # | Вопрос | Блокирует | Рекомендация | Действие по умолчанию, если ответа нет |
|---|---|---|---|---|
| Q1 | Fail-closed (E1) или фиксация COALESCE-parity? | E1, миграция scopes | Fail-closed **после** полной инвентаризации `api_keys`; иначе риск R-2 | Оставить parity, записать в ADR как осознанный долг |
| Q2 | `0.0.0.0` bind — осознанное LAN-sharing или наследие? | Severity TLS (NG-6) | Считать осознанным; TLS вне гейта | Считать осознанным, TLS-terminate вне шлюза |
| Q3 | Шифровать ли Responses envelopes at-rest? | C3 | Да, через существующий `FieldCrypto`; key = `STORAGE_ENCRYPTION_KEY` | Только права файла БД + retention, записать в ADR |
| Q4 | Нужен ли Responses store продукту вообще? (0 production-строк **[V]**) | C1..C6 | Оставить, но с retention; альтернатива — `store:false`-only | Оставить store с retention |
| Q5 | Требуется ли commit-only PR первым? | Q7 из `06` | **Да** — иначе каждый гейт измеряет дерево, которое нельзя checkout'нуть | Выполнить в рамках A1/A2 |
| Q6 | UI test runner? | G3/G9 (частично) | Без нового JS-тулчейна: `node --test` с нулём зависимостей, остальное — doc-gate | Без UI-тестов, полагаемся на `check-docs.sh` |
| Q7 | Proptest или детерминированный сид-фаззер для `sse.rs`? | L5-слой | Сид-фаззер без dev-dep | Сид-фазкер |
| Q8 | Desktop-тесты в CI: полный `tauri build` или только `cargo test`? | F9 | Только `cargo test` + `check` (экономит ~4 мин, не требует webkit deps) | `cargo test` |
| Q9 | Третий-сторонние идентификаторы в `chathub-evidence/` — псевдонимизировать или принять с обоснованием? | G12 (GOV-3) | Псевдонимизировать в `00000000-0000-4000-8000-…` + `SANITIZED.md` | Псевдонимизировать |

---

## 14. Evidence index

| Файл | Что внутри |
|---|---|
| [`docs/research/mission-2026-10/01-mission-sequencing.md`](research/mission-2026-10/01-mission-sequencing.md) | Charter, workstreams, dependency graph, milestones, effort, stop/go, risk register |
| [`02-responses-contract.md`](research/mission-2026-10/02-responses-contract.md) | Responses contract: pivot analysis, 4 silent-loss бага, target design, fixtures, AC-1…AC-16 |
| [`03-routing-providers.md`](research/mission-2026-10/03-routing-providers.md) | Registry metrics, backend construction, canonical-id стратегия, attribution, staged acceptance A0…A6 |
| [`04-security-retention.md`](research/mission-2026-10/04-security-retention.md) | Threat model, 12 доменов, mitigation waves 0–10, acceptance matrix |
| [`05-desktop-release.md`](research/mission-2026-10/05-desktop-release.md) | 28 исполненных проверок, B1–B10, S0–S7, AT-01…AT-42, version matrix |
| [`06-qa-docs.md`](research/mission-2026-10/06-qa-docs.md) | Test layers L0–L7, GAP-1…GAP-12, fixture governance GOV-1…GOV-5, traceability |

Предыдущая миссия: [`docs/research/PROJECT-AUDIT-2026-09.md`](research/PROJECT-AUDIT-2026-09.md).

---

## 15. Статус документа

План утверждён к реализации; реализация не начата. Все числа baseline помечены и
проверены на текущем рабочем дереве 2026-09-29. Оценки effort — проектные.
Пункты §13 требуют решения владельца до старта соответствующих задач.

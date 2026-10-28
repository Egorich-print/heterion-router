# 01 — Mission Charter: Heterion Router (следующая миссия)

**Роль:** principal architect 1/6 · **Дата:** 2026-09-29
**Репозиторий (read-only):** `/Volumes/Netac_SSD/ai-workstation/Projects/heterion-router`
**Канонический репозиторий не модифицирован и не закоммичен.** Единственный
записанный файл — этот отчёт.

**Evidence base:** рабочее дерево поверх `HEAD=51dfe6d`; собственный
статический read + локальный прогон гейтов; шесть параллельных отчётов в
`/var/folders/…/T/opencode/heterion-audit/`
(`01-architecture`, `02-providers`, `03-security`, `04-desktop-build`,
`05-docs-factcheck`, `06-quality-roadmap`); `docs/ROADMAP-2026-09.md`,
`docs/research/PROJECT-AUDIT-2026-09.md`, `docs/adr/*`, живой runtime
(launchd-сервис + read-only SQLite).

---

## 0. Легенда достоверности

Каждое утверждение помечено:

| Метка | Значение |
|---|---|
| **[V]** | **Проверено мной** в этом сеансе: source read + локальный прогон команды или чтение live-данных. Приведена ссылка `file:line` или команда. |
| **[A]** | **Проверено предыдущим аудитом** (один из шести отчётов), мной не перепроверялось. |
| **[P]** | **Предложение** — проектное/секвенционое суждение, не факт о коде. |
| **[?]** | Известная неопределённость. |

Числа в §1 пересчитаны мной напрямую, а не взяты из отчётов, где это возможно.

---

## 1. Verified baseline — что mission стартует с этого состояния

### 1.1 Git и воспроизводимость

| Факт | Метка | Evidence |
|---|---|---|
| `HEAD = 51dfe6d` («feat: grok-cli with refresh_token support…»), 2026-09-22 | **[V]** | `git log --oneline -5` |
| Рабочее дерево грязное: **35 modified + 5 untracked = 40** записей `git status --porcelain`; диф `+2997 / −364` | **[V]** | `git status --porcelain`, `git diff --stat \| tail -1` |
| Untracked: `docs/ROADMAP-2026-09.md`, `docs/adr/ADR-009-*`, `docs/adr/ADR-010-*`, `docs/history/README.md`, `docs/research/` | **[V]** | `git status` |
| Следствие: в свежем `git clone` ломаются как минимум ссылки `docs/adr/README.md` на ADR-009/010 и `STATUS.md` → `docs/ROADMAP-2026-09.md` | **[A]** | 05-docs-factcheck F-04 |
| `docs/ROADMAP-2026-09.md` содержит минимум 5 фактически ложных ячеек, противоречащих собственным разделам того же файла | **[A]** | 05-docs-factcheck F-02; 02-providers §3 |
| Remote `origin` существует, ветка только `main` | **[V]** | `git remote -v`, `git branch -a` |

**Следствие для миссии:** весь код, который описывают `STATUS.md`/`ROADMAP`/
`ADR-009`, существует **только в рабочем дереве**. Любая работа, начатая до
фиксации baseline, не воспроизводима и не откатываема через git.

### 1.2 Гейты качества (прогнаны мной)

| Команда | Результат | Метка |
|---|---|---|
| `cargo test --workspace` | **213 passed, 0 failed** (28 блоков `test result: ok`) | **[V]** |
| `cargo fmt --all -- --check` | clean | **[A]** (gate в `check.sh`) |
| `cargo clippy --workspace --all-targets -- -D warnings` | clean | **[A]** |
| `scripts/ci/check.sh` | «all green» | **[A]** |
| `npm --prefix desktop/ui run build` | pass (Vite 6.4.3, 117 modules) | **[A]** |
| `cargo check --manifest-path desktop/src-tauri/Cargo.toml --locked` | pass | **[A]** |
| **`(cd desktop/src-tauri && cargo fmt -- --check)`** | **2 диффа**: `build.rs:1-3` (2-space indent), `src/lib.rs:81` (line-wrap) | **[V]** |
| `cargo test --manifest-path desktop/src-tauri/Cargo.toml` | **0 тестов** | **[A]** |

**Распределение тестов (пересчитано мной, `#[test]`/`#[tokio::test]` по крейтам):**

```
gateway 106 · translate 29 · db 17 · providers 14 · crypto 13
core 8 · http 8 · routing 6 · usage 6 · config 6          = 213
```

| Факт | Метка | Evidence |
|---|---|---|
| `admin.rs` (783 LOC) содержит **1 тест** (`dated_application_log_is_discovered`), при 13 handlers | **[V]** | `rg '#\[test\]' crates/heterion-router-gateway/src/admin.rs` → только `admin.rs:773` |
| `wiremock.expect()` в `openai.rs` — 8 использований, `.verify()` — **0** во всём `crates/` | **[V]** | `rg -c '\.expect\(' …/openai.rs` = 8; `rg '\.verify\(\)' crates/` пусто |
| Единственный integration-тест gateway — `tests/upstream_harness.rs` (28 строк), не использует `HttpClient`/роутер | **[A]** | 06 §2 |
| Test distribution: 51 % тестов в одном крейте; `admin`, Responses SSE tool loop, alias-construction, derived Responses URL, desktop Rust — не покрыты | **[A]** | 06 §2–3 |

**Размер кодовой базы [V]:** `crates/**/*.rs` = **16 678** строк;
`desktop/src-tauri/src` + `desktop/ui/src` = **1 224**.
Крупнейшие модули: `lib.rs` 2194, `admin.rs` 783, `routing.rs` 672,
`translate/responses.rs` 584, `main.rs` 543.

### 1.3 Live runtime

| Факт | Метка | Evidence |
|---|---|---|
| Launchd job `com.heterion.router`, PID **68027**, старт `Tue Sep 29 11:16:15 2026`, слушает `0.0.0.0:20128` | **[V]** | `ps -o pid,lstart`, `lsof -iTCP:20128 -sTCP:LISTEN` |
| `GET /healthz` → `{"backend":"routing","status":"ok","started_unix":1790669776}` | **[V]** | `curl :20128/healthz` |
| `GET /api/overview` без ключа → **401** | **[V]** | `curl -o /dev/null -w %{http_code}` |
| В последнем старте логируется **19 backends** (`grok-cli, blackbox, cline, deepseek, gemini, groq, huggingface, kilo-gateway, kilocode, llm7, mistral, nvidia, opencode-go, openrouter, pollinations, sambanova, siliconflow, trae, xai`) | **[V]** | `rg 'backend available' gateway.log.2026-09-29 \| tail -19` |
| В БД **24** различных активных провайдера → 5 не построены: `agentrouter` (format `claude`), `kimi-coding` (`claude`), `kiro` (`kiro`), `exa-search` (**отсутствует в registry**), `opencode` (**0 credentials** — `access_token`/`api_key` пусты) | **[V]** | `SELECT DISTINCT provider … WHERE is_active=1` (24) + сверка с логом (19) + `registry.json` + `SELECT … WHERE provider='opencode'` → пустые длины |
| `combos`: 8 строк; **`vivanta-auto`** и **`vivanta-deepseek`** имеют `"models": []` | **[V]** | `SELECT name, substr(data,…) FROM combos` |
| `key_value` namespace `responses` → **0 строк** | **[V]** | `SELECT COUNT(*) FROM key_value WHERE namespace='responses'` |
| `api_keys`: 1 строка `OpenCode-Main`; `key` = 35 симв. (plaintext `sk-`+32hex), `key_hash` = 64 hex; `scopes=["manage","self:usage","self:account-quota"]`, `allowed_models=[]` | **[V]** | `SELECT name, length(key), length(key_hash), scopes, allowed_models FROM api_keys` |
| `provider_connections LIKE 'heterion%'` → **0 строк** ⇒ `heterion-local` backend не поднимается | **[V]** | SQL + `main.rs:161` |
| Все файлы в `~/.heterion-router` — `0600`, каталог `0700`; **включая `gateway.log.2026-09-29`** | **[V]** | `stat -f '%Sp %N' ~/.heterion-router/*` — **F6 из 01-architecture сегодня НЕ воспроизводится** (лог был пересоздан/вычищен последующим стартом) |
| Live plist вне репозитория, указывает на **dev-дерево** (`…/target/release/heterion-router-gateway`, `…/desktop/ui/dist`) и `HETERION_ROUTER_HOST=0.0.0.0` | **[V]** | `cat ~/Library/LaunchAgents/com.heterion.router.plist` |
| В репозитории **нет ни одного `*.plist`** | **[V]** | `find . -name '*.plist' -not -path './.git/*'` → только `Info.plist` внутри собранного `.app` |

**Поправка к 01-architecture F6:** утверждение «rotated logs `0644`»
**сейчас не подтверждается** — все девять `gateway.log.*` имеют `0600`.
Механизм (`restrict_data_files()` в `main.rs:390` до создания appender'ом
файла в `main.rs:392-394`) в коде остаётся **[A] 01-architecture F6 /
03-security L3**, но наблюдаемое состояние изменилось. Формулировка
в `STATUS.md` может оставаться, а вот «live-доказательство 0644» — нет.
Это пример того, почему mission должен начинаться с re-verification, а не
с переноса audit-findings.

### 1.4 Registry (пересчитано мной, `python3` по `registry.json`)

| Метрика | Значение | Метка |
|---|---|---|
| Providers / model entries | **278 / 2717** | **[V]** |
| Проходят structural builder gate (format ∈ {openai, gemini, openai-responses} + непустой base_url) | **259 / 2108** | **[V]** |
| Из них HTTP(S)-совместимы | **253 / 1916** | **[V]** |
| Структурно проходят, но не-HTTP схема | **6** providers: `auggie, codex-app-server, copilot-m365-web, copilot-web, devin-cli, zcode` | **[V]** |
| Format distribution | `openai` 253 · `claude` 10 · `openai-responses` 5 · `gemini` 3 · `antigravity` 2 · `cursor` 2 · `kiro` 1 · `clova` 1 · `custom` 1 | **[V]** |
| Моделей с `target_format=openai-responses` | **150** у 15 providers | **[V]** |
| Моделей на `openai`-provider'ах с `target_format`, **противоречащим** provider format | **35** (все `claude`: `github` 11, `opencode-go` 13, …) | **[V]** |
| `executor` — мёртвые метаданные; runtime ветвится по `format` | **[A]** | 02-providers §1, `registry.rs:267-280` vs `main.rs:177,211,244` |
| `github` / `ghe-copilot`: base `https://api.githubcopilot.com/chat/completions`, 25 моделей помечены `openai-responses` ⇒ derived URL `…/responses` — не существующий endpoint Copilot | **[V]** (registry) + **[A]** (последствие) | `resolve_responses_url` `openai.rs:391-400` |

### 1.5 Подтверждённые дефекты, которые я лично перечитал в коде

| ID | Дефект | Метка | Evidence |
|---|---|---|---|
| **C-1** | **SSE error frame не sanitized.** Не-stream sanitizes (`lib.rs:538-545`: `Upstream(_) => "upstream request failed"`), stream отдаёт `error.to_string()` — до **2000 символов сырого тела провайдера** | **[V]** | `lib.rs:493-495` vs `lib.rs:538-545`; `http/src/client.rs:14-21, 90-101`; повтор в Responses-потоке `lib.rs:1211-1213` (`json!({"error": {"message": error.to_string()}})`) |
| **C-2** | **`reasoning` input items молча выбрасываются** в `responses_to_chat` | **[V]** | `lib.rs:1015-1017` — `// ponytail: reasoning/other item kinds are skipped` + `_ => {}` |
| **C-3** | **Built-in tools (`web_search`, `x_search`, …) молча выбрасываются** | **[V]** | `lib.rs:1069` — `tool.get("name").and_then(Value::as_str)?` внутри `filter_map`; у built-in tools поля `name` нет |
| **C-4** | **Responses SSE — 4 типа событий из ~10**, `response.output_item.added` несёт нестандартную форму: `call_id: ""`, `arguments: ""`, вложенный массив `tool_calls` | **[V]** | `lib.rs:1254-1298` (`chat_chunk_to_responses_events`) |
| **C-5** | **`store` по умолчанию `true`**, полный request+response envelope пишется в `key_value`; ключ хранилища — **upstream-controlled** id; TTL/размер/шифрование отсутствуют | **[V]** | `lib.rs:830-833` (`.unwrap_or(true)`), `lib.rs:573-598` (`kv_set(…, id, …)` где `id = response["id"]`) |
| **C-6** | **Backend map ключуется сырой строкой `provider`, роутер канонизирует.** Строка-алиас в `provider_connections` ⇒ backend под алиасом, `servable: true`, но не routable и не рекламируется | **[V]** | `main.rs:201, 237, 295` (`backends.insert(provider, …)`) vs `routing.rs:166` (`canonical_id`) и `combos.rs:240`; `/v1/models` фильтр `lib.rs:208-211`; `servable` из `backend_names` — `admin.rs:186` |
| **C-7** | **Атрибуция в usage/call-logs синтетическая**: `provider = state.backend.name()` (в проде литерал `"routing"`), `connection_id: None`, `call_logs.status ∈ {200, 500}`, `requested_model == model`, `combo_name` не пишется | **[V]** | `lib.rs:325` (`provider`), `lib.rs:327` (`connection_id: None`), `lib.rs:348-353` (`status: if success {200} else {500}`, `requested_model`) |
| **C-8** | **Fail-open по умолчанию для API-key policy**: `COALESCE(scopes,'["manage"]')`, `COALESCE(allowed_models,'[]')`; пустой allow-list = «без ограничений» | **[V]** | `repos.rs:353-364`, `auth.rs:54-56` |
| **C-9** | **Колонка `key` хранит hash ⇒ hash сам является валидным bearer-токеном**; индекса на `key_hash` нет ⇒ полный table scan на каждый аутентифицированный запрос | **[V]** | `repos.rs:226-256` (пишет `row.key_hash` и в `key`, и в `key_hash`); предикат `key = ?1 OR key_hash = ?2` — `repos.rs:362`; `schema_base.sql:90` — `CREATE INDEX … ON api_keys(key)` только |
| **C-10** | **`count_active_api_keys` игнорирует `is_active`/`is_banned`** | **[V]** | `repos.rs:941-948` — `WHERE revoked_at IS NULL` |
| **C-11** | **Refresh-callback пишет plaintext**, если `STORAGE_ENCRYPTION_KEY` не разрешён | **[V]** | `main.rs:110-113` (`.unwrap_or_else(\|\| token.to_string())`) и `main.rs:115-120` для refresh token; `crypto/src/lib.rs:159-177` возвращает `None` без секрета |
| **C-12** | **Gemini signature store создаётся без mode** → umask (обычно 0644) | **[V]** | `gemini.rs:178-182` — `OpenOptions::new().create(true).append(true).open(path)` без `.mode()` |
| **C-13** | **Мёртвый конфиг комбо**: `max_set_retries`, `set_retry_delay_ms` парсятся и нигде не читаются; `weight` и `strategy:"auto"` не влияют на порядок | **[V]** | `combos.rs:26,28,173,177` — единственные вхождения в `crates/`; `combos.rs:76-77` (комментарий признаёт) |
| **C-14** | **Мёртвое поле `RoutingBackend::combo_configs`** заполняется (`routing.rs:115`) и не читается; `load_combos` выполняется дважды на запрос | **[V]** | `routing.rs:34,100,115` — три вхождения, чтений нет |
| **C-15** | **Ни `Retry-After`, ни upstream request-id** не извлекаются, не хранятся и не учитываются в retry | **[V]** | `rg -i 'retry.after\|request_id' crates/` → **пусто** |
| **C-16** | **xAI whole-call streaming dialect теряет tool calls**: `response.completed` обрабатывается только как `finish_chunk`, синтез tool-call из snapshot не реализован | **[V]** | `translate/responses.rs:308-315`; `responses.rs:9-12` — «Explicitly deferred» |
| **C-17** | **Tauri fallback-порт `20128` при sidecar-порте `20129`** ⇒ при отсутствии инъекции UI (вкл. `Authorization`) уходит на launchd-сервис | **[V]** | `desktop/ui/src/api.js:45` vs `desktop/src-tauri/src/lib.rs:21` (`SIDECAR_PORT = "20129"`) |
| **C-18** | **Vite dev-proxy указывает на `20128`**, а `cargo run` gateway — на `20129` ⇒ dev-контур по README правит живую БД | **[V]** | `desktop/ui/vite.config.js:7` vs `config.rs:51-53` (default `20129`) |
| **C-19** | **CSP выключен** (`"csp": null`), а окно `main` имеет `shell:allow-spawn`; gateway URL переопределяется из `localStorage` | **[V]** | `desktop/src-tauri/tauri.conf.json:20`; `capabilities/default.json` (`core:default`, `shell:allow-spawn`); `desktop/ui/src/api.js:34-35` |
| **C-20** | **Restart через `sh -c`** с интерполяцией env-лейбла | **[V]** | `restart.rs:45` (`kickstart_script`) + `restart.rs:66-70` (`Command::new("sh").args(["-c", …])`; защита — `is_supervised` через `argv`) |
| **C-21** | **Release использует `sha256sum` на macOS-ноге**, где его нет; `macos-14` на грани удаления | **[V]** (файл) / **[A]** (runner manifest) | `.github/workflows/release.yml:24` (`os: [ubuntu-latest, macos-14]`), `:44` (`sha256sum "$OUT"`) |
| **C-22** | **CI `paths:` не покрывает `desktop/src-tauri/**` и `docs/**`**; Tauri-крейт исключён из workspace ⇒ его `Cargo.lock` (500 пакетов) не верифицируется | **[V]** (файл) | `.github/workflows/ci.yml` (`paths:` список), `Cargo.toml` `exclude = ["desktop/src-tauri"]` |
| **C-23** | **UI-dir без containment check**: `HETERION_ROUTER_UI_DIR` указывает произвольный каталог, отдаётся без auth | **[V]** | `ui.rs:22-32`; маршруты `/` и `/{*path}` без extractor — `lib.rs:160-161` |
| **C-24** | **xAI whole-call `store`/Responses-поля утекают в Chat body**: `extra` сплайсается целиком | **[V]** | `backend.rs:97-99` (`for (key, value) in &request.extra { body[key] = … }`) + `lib.rs:1028-1045` |

### 1.6 Что работает и не должно быть сломано mission'ей

Verified protections **[V]** (перепроверено в коде): per-handler extractor вместо
middleware (`/healthz` публичен by construction, `lib.rs:145-171`); fail-closed
на DB-ошибке в auth; `revoked_at IS NULL AND is_banned=0 AND is_active=1`
(`repos.rs:362-363`); hash-only issuance (`repos.rs:226-256`); CORS exact-host
allowlist; traversal-safe static join; параметризованный SQL; AES-256-GCM
`enc:v1:` с рандомным 128-битным IV; keyless-бэкенды не шлют `Authorization`;
4xx не роняет breaker; `single_instance` регистрируется **первым** плагином;
`watch_parent` корректно трактует `EPERM` как «жив» и не логирует из
watchdog-потока перед `exit(0)`.

Frozen contracts **[V]** (`AGENTS.md` + `AGENTS.md` §«Контракты»):
`STATIC_SALT = "omniroute-field-encryption-v1"` + префикс `enc:v1:`;
`HETERION_ROUTER_*` первичен / `OMNIROUTE_*` — fallback на один цикл;
`~/.heterion-router` → legacy `~/.omniroute`; порты `20128` (service) /
`20129` (sidecar+dev); keyless ⇒ без bearer; upstream 4xx ⇒ breaker closed.

---

## 2. Objective

> **Сделать шлюз честным под агентным циклом и воспроизводимым как артефакт:
> ни одно поле, меняющее agent loop, не теряется молча; ни одна ошибка
> провайдера не утекает клиенту; каждый запрос, попавший в usage/call-logs,
> атрибутируется реальному провайдеру, модели и статусу; а всё это
> воспроизводится из зафиксированного `HEAD`.**

Четыре измеримых результата миссии:

| # | Результат | Метрика «готово» |
|---|---|---|
| **O-1** | **Нет silent data loss на Responses-пути.** `reasoning` input items (в т.ч. `encrypted_content`) либо доходят до upstream байт-в-байт, либо приходят как `400 unsupported_capability`. Built-in tools отклоняются, а не выбрасываются. | Wiremock-тест: `reasoning` item с `encrypted_content` присутствует в теле upstream **идентично**; тест: built-in tool + chat-only provider → 400 |
| **O-2** | **Нет утечки upstream-текста в SSE и в логи «как будто всё хорошо».** Error frames sanitized наравне с non-stream; стриминговые ошибки попадают в `tracing` с provider attribution. | Wiremock с маркером в теле ответа; маркер отсутствует в SSE-фрейме; в `gateway.log` присутствует строка терминального отказа |
| **O-3** | **Attribution и статус в usage/call-logs достоверны.** `provider` ≠ `"routing"`, `connection_id` заполняется, `combo_name` пишется, `call_logs.status` = реальный HTTP-код, `requested_model` ≠ `model`, когда combo разрешился. `Retry-After` пробрасывается и учитывается. | Тест на combo-маршрут: строка в `usage_history` содержит имя конечного provider'а и реальный код; `Retry-After: 3` ⇒ ≥3 s до retry |
| **O-4** | **Baseline воспроизводим.** Рабочее дерево зафиксировано одним или несколькими коммитами; ни один документ не ссылается на несуществующий в `HEAD` файл; `docs/ROADMAP-2026-09.md` не содержит ложных ячеек; `STATUS.md` «Проверки» соответствует тому, что реально гоняет CI. | `git clone` в чистый каталог → `scripts/ci/check.sh` зелёный; все ссылки в `STATUS.md`/`docs/adr/README.md` резолвятся; `git status --porcelain` пуст |

---

## 3. Non-goals (осознанно вне миссии)

| # | Не делаем | Почему |
|---|---|---|
| NG-1 | **Native Anthropic Messages backend / `/v1/messages`.** | Трансляторы `claude.rs`/`claude_request.rs` существуют и покрыты text-only parity-фикстурами, но `AnthropicBackend` нет. Это фича-поверхность на 120+ моделей и 10 providers, а не исправление дефекта. **[A] 02 §7, 06 §6** |
| NG-2 | **ChatHub как upstream.** | ADR-010; evidence: anonymous session не выдаётся, chat 403, stream 401. **[A] 01 §13 claim 25, 03 I6** |
| NG-3 | **WebSocket transport, hosted MCP, Conversations API, background jobs, все OpenAI built-in tools.** | Уже вне текущего milestone в `ROADMAP`; нет подтверждённого клиента. **[V]** `ROADMAP:117-120` |
| NG-4 | **Полный Grok Build conformance harness + opt-in live canary.** | Требует реального `grok` CLI и санитизированного корпуса событий. **Но:** инфраструктуру canary (opt-in env, dry-run) закладываем в M3 без запуска живого canary. **[A] 06 §6** |
| NG-5 | **Notarization / signing distributable `.dmg`.** | Требует Apple Developer Team ID, которого нет. Миссия фиксирует честный статус «developer-machine artifact» в `STATUS.md`, но не покупает сертификат. **[A] 04 §6.4 G19** |
| NG-6 | **TLS-терминация и LAN-exposure hardening.** | Reverse-proxy/VPN задача; вне шлюза. Миссия лишь документирует факт `0.0.0.0` bind. **[A] 01 §13 claim 15** |
| NG-7 | **Реализация quota/rate-limit enforcement** по 10 неиспользуемым колонкам `api_keys`. | Реальная фича с продуктовым решением (кто платит, чем ограничиваем). Миссия делает **явным** (ADR/STATUS: «схема ≠ enforcement»), но не реализует. **[A] 03 M4** |
| NG-8 | **Реализация `weight` / `strategy:"auto"`.** | Миссия выбирает дешёвый честный путь: **удалить** из UI то, что не работает (см. WS-I3), а не изобретать алгоритм авто-роутинга. **[P]** |
| NG-9 | **Изменение шифро-контракта** (`enc:v1:`, `STATIC_SALT`) или схемы `api_keys` в несовместимой форме. | Frozen interop; живые БД держат эти значения. **[A] 03 I5** |

---

## 4. Принципы миссии (наследуются + один новый)

1. **Fail closed.** Нельзя молча выбрасывать поля, меняющие agent loop. **[V]** `ROADMAP:106`
2. **Capability по effective model format**, registry metadata ≠ доказательство executor'а. **[V]** `ROADMAP:107-108`
3. **Opaque reasoning — это данные.** Не превращать в видимый текст, не логировать. **[V]** `ROADMAP:109`
4. **State origin must be explicit.** Продолжение не должно незаметно менять provider/credential. **[V]** `ROADMAP:110`
5. **Public static surface — это граница.** **[V]** `ROADMAP:111`
6. **NEW: каждый дефект сначала получает падающий тест, потом фикс.** Обоснование: 8 из 8 `wiremock.expect()` в `openai.rs` декоративны (`.verify()` нет нигде **[V]**), а C-1…C-24 все были невидимы для CI именно потому, что соответствующие пути не покрыты. Fix-first без красного теста воспроизводит ту же проблему.

---

## 5. Workstreams

Effort-полосы **[P]**, в человеко-днях, для одного инженера, знакомого с кодовой базой.

### WS-A — Baseline Freeze & Doc Truth

**Цель:** O-4. Сделать `HEAD` воспроизводимым источником истины.

| # | Задача | Метка |
|---|---|---|
| A1 | Разбить `+2997/−364` на осмысленные коммиты (код / тесты / docs) — не один «wip» | **[P]** |
| A2 | `git add` untracked: `docs/ROADMAP-2026-09.md`, `ADR-009`, `ADR-010`, `docs/history/README.md`, `docs/research/` | **[V]** (факт: untracked) |
| A3 | Переписать «Текущее состояние» таблицу `ROADMAP-2026-09.md` — либо удалить 5 ложных ячеек, либо переименовать заголовок в «Разрывы на срез 2026-09-25» и добавить колонку «Закрыто с тех пор» | **[A]** 05 F-02 |
| A4 | Вернуть 9 удалённых из `CHANGELOG.md` записей (rename, `heterion-local`, `POST /v1/responses`, single-instance, parent-watch, Gemini signature) | **[A]** 05 F-03 |
| A5 | ADR addenda (не переписывая ADR-001..007): ADR-007 (sidecar name, injected global, single-instance), ADR-002/006 (`HETERION_ROUTER_*` первичен), ADR-008 (`rust.yml` → `ci.yml`) | **[A]** 05 F-05/06/15 |
| A6 | `docs/history/` — либо `git mv` в `upstream-omniroute-js-history/`, либо удаление; поправить 5 ссылок (README:75, STATUS:87, AGENTS:13-14, `docs/adr/README.md:4-5`, ADR-008:23,56) | **[A]** 05 F-01 |
| A7 | Переименование `file.rs:N` → `file.rs:symbol` во всех roadmap-цитатах (12 цитат указывают на неверные строки) | **[A]** 05 F-11 |
| A8 | Удалить tracked 0-byte `desktop/src-tauri/icons/.!15396!128x128.png` + `._*`/`.!*` в `.gitignore` | **[A]** 05 F-16, 04 G20 |
| A9 | `STATUS.md` «Проверки» → «Проверки (локально; шире, чем CI)», перечислить расхождения (`cargo check` для Tauri нигде в CI; exec-bit gate в `check.sh`) | **[A]** 05 F-14 |
| A10 | `README.md`: документировать, что plist машинно-локальный, сервис слушает `0.0.0.0`, Vite-прокси указывает на 20128 | **[A]** 05 F-07, 04 C4/C5 |
| A11 | Перенести ChatHub evidence из `/var/folders/…` в `docs/research/chathub-evidence/` (уже частично сделано: каталог существует **[V]**) | **[A]** 05 F-10a |

**Effort: 1.5–2.5 дня.** DoD: `git clone` → `scripts/ci/check.sh` зелёный; все внутренние doc-ссылки резолвятся; `git status --porcelain` пуст.

### WS-B — Fail-Closed Correctness (P0)

**Цель:** O-1, O-2. Шесть пунктов, каждый — красный тест + фикс.

| # | Задача | Дефект | Метка |
|---|---|---|---|
| B1 | Sanitize SSE error frames через ту же ветку, что `error_response`; добавить wire-level регрессионный тест с маркером в теле мока | C-1 | **[V]** |
| B2 | Сохранять `reasoning` input items (вкл. `encrypted_content`) в `responses_input` **байт-в-байт**, либо `400 unsupported_capability`; тест `store:false` двухходовый replay | C-2 | **[V]** |
| B3 | Reject unsupported built-in tools: `web_search`/`x_search`/неизвестный `type` ⇒ `400 unsupported_capability`, если target provider их не объявляет | C-3 | **[V]** |
| B4 | Типизированный Responses SSE lifecycle: `response.created` → `output_item.added` (настоящий `function_call` с реальным `call_id`) → `function_call_arguments.delta|done` → `output_item.done` → ровно один terminal event. **+ валидатор** (test-only), ассертящий ids, индексы, единственность терминала | C-4 | **[V]** |
| B5 | Синтез tool-call из `response.completed` snapshot (xAI whole-call dialect) — иначе `finish_reason` не должен быть `stop` при наличии tool call | C-16 | **[V]** |
| B6 | Фильтрация Responses-only ключей (`store`, `include`, `reasoning`, `previous_response_id`, `text`, `session_id`, `conversation_id`) при маршрутизации на Chat-only provider; wire-тест на теле `/chat/completions` | C-24 | **[V]** |

**Effort: 5–8 дней.** B4 — самый дорогой (переписывает `chat_chunk_to_responses_events`, `lib.rs:1254-1298`, и его потребителей).

### WS-C — State Store: Retention, Key Ownership, Confidentiality

**Цель:** O-1 (частично) + граница доверия к диску.

| # | Задача | Дефект | Метка |
|---|---|---|---|
| C1 | Gateway-owned storage key (`resp_<uuid>`), upstream id — поле внутри записи; устраняет cross-tenant overwrite через `kv_set`-upsert | C-5 | **[V]** |
| C2 | Retention: `stored_at`-индексированный prune (default 30 дней, значение в конфиге), прогон при старте; плюс byte-cap на запись | C-5 | **[P]** для порога, **[V]** для отсутствия |
| C3 | Решить и зафиксировать в ADR: шифровать ли envelopes через `FieldCrypto` (уже в зависимостях gateway **[V]**) или явно объявить границей режим файла БД | — | **[A]** 06 item 20 |
| C4 | `no_log` должен подавлять и `persist_response`, а не только usage/call-log | — | **[A]** 02 D8 |
| C5 | Задокументировать/сменить дефолт `store` (сейчас `true` **[V]**) | C-5 | **[P]** |
| C6 | Индекс по retention-полю требует **новой миграции** `002` — `key_value` сейчас `(namespace, key) PRIMARY KEY` без `stored_at` **[V]** `schema_base.sql:65-69`; миграции append-only, ledger без downgrade **[V]** `migrate.rs:31-36` | — | **[V]** |

**Effort: 2–3 дня.** Зависит от B2 (что именно хранить — после решения по reasoning).

### WS-D — Routing Identity & Observability

**Цель:** O-3.

| # | Задача | Дефект | Метка |
|---|---|---|---|
| D1 | Канонизировать `provider` в `select_backend` **до** `backends.insert` | C-6 | **[V]** |
| D2 | Прокинуть выбранный `(backend, model, connection, combo)` из `RoutingBackend` в `record_completion_usage` / `record_stream_usage` | C-7 | **[V]** |
| D3 | `call_logs.status` = реальный код; `requested_model` ≠ `model` при combo; писать `combo_name` | C-7 | **[V]** |
| D4 | Извлекать и хранить upstream request-id; `Retry-After` — извлекать и **учитывать** в backoff | C-15 | **[V]** |
| D5 | `count_active_api_keys` — учесть `is_active`/`is_banned`; один SQL + один тест | C-10 | **[V]** |
| D6 | Повторный `x-request-id` в ответе клиенту (прослеживаемость через `/api/logs`) | — | **[P]** |
| D7 | Отчёт о недостижимых registry-записях: 25 providers / 801 models не строятся, 61/959 игнорируют свой executor — показать в `/api/overview` и в `/v1/models` | — | **[A]** 02 §1 |

**Effort: 2.5–4 дня.** D1 — 1 строка + регрессионный тест, который надо сначала написать (существующий `alias_prefixed_model_reaches_the_canonical_backend` тест строит map вручную каноническим ключом и поэтому дефект не видит **[A]** 02 D1).

### WS-E — Auth & Data-Boundary Hardening

**Цель:** O-2 (частично) + fail-closed по policy.

| # | Задача | Дефект | Метка |
|---|---|---|---|
| E1 | Fail-closed policy для строк с NULL `scopes`/`allowed_models`; обновить `usable_keys_honour_kill_switches`; **или** явная ADR-фиксация COALESCE как JS-parity | C-8 | **[V]** |
| E2 | `PATCH /api/keys/{id}`, позволяющий сузить существующий ключ (сейчас `CreateKeyBody` имеет одно поле `name` **[V]** `admin.rs:231-233`) | C-8 | **[V]** |
| E3 | Отказ от plaintext-записи обновлённого refresh-токена без `STORAGE_ENCRYPTION_KEY` + `warn!` на старте | C-11 | **[V]** |
| E4 | `key` = hash ⇒ hash валиден как bearer; убрать дубль, индексировать `key_hash`, различить legacy-plaintext предикатом | C-9 | **[V]** |
| E5 | Создание log/signature файлов с явным `0o600`; `restrict_data_files` — переименовать/перенести так, чтобы покрывал и текущий день | C-12 | **[V]** |
| E6 | `HETERION_ROUTER_UI_DIR` — containment check; опция `HETERION_ROUTER_REQUIRE_AUTH_UI` для не-loopback bind | C-23 | **[V]** |
| E7 | `launchctl` через `argv`, не `sh -c` | C-20 | **[V]** |
| E8 | `#[derive(Debug)]` на 4 credential-структурах → redacting impl (прецедент `FieldCrypto` **[A]** 03 L6) | — | **[A]** |
| E9 | `logs_app` — tail вместо slurp (OOM на аутентифицированном запросе) | — | **[A]** 03 L8 |
| E10 | Зафиксировать в ADR, что ключ шифрования лежит рядом с шифротекстом ⇒ AES-GCM защищает от экспорта файла БД, но не от чтения каталога | — | **[A]** 03 M5 |

**Effort: 3–4.5 дня.** E1 и E4 меняют **закоммиченное тестом** поведение ⇒ потребуют явного решения «parity vs fail-closed» и обновления теста.

### WS-F — Operational Boundary (desktop, service, release)

**Цель:** O-4 + снятие риска «клиент говорит не тому процессу».

| # | Задача | Дефект | Метка |
|---|---|---|---|
| F1 | `api.js:45` fallback `20128` → `20129` (или удалить fallback и падать громко) | C-17 | **[V]** |
| F2 | `vite.config.js:7` default → `20129`; документировать override | C-18 | **[V]** |
| F3 | Sidecar readiness/ownership handshake: привязка к порту 0 + чтение фактического порта, либо one-time nonce в init script | — | **[A]** 03 M2, 04 G2 |
| F4 | Sidecar death: bounded respawn с backoff + видимое состояние в окне вместо молчаливого логина | — | **[A]** 04 G1 |
| F5 | «Restart now»: гейтить на стороне клиента по факту supervised, либо сделать sidecar-aware; иначе — честно «disabled» с объяснением | — | **[A]** 04 §5.2/5.3 |
| F6 | Коммит `scripts/launchd/com.heterion.router.plist` с шаблонизированными путями + install/uninstall; либо убрать service-инструкции из quick start | — | **[A]** 04 G17 |
| F7 | Включить CSP в `tauri.conf.json`; сузить capability `main` | C-19 | **[V]** |
| F8 | `cargo fmt --manifest-path desktop/src-tauri/Cargo.toml -- --check` в `check.sh`/CI + починить 2 диффа | — | **[V]** |
| F9 | CI: добавить `desktop/src-tauri/**`, `docs/**` в `paths:`; job `cargo check --locked` для Tauri; закрепить toolchain (`1.98.0` вместо `@stable`) | C-22 | **[V]** |
| F10 | `release.yml`: `shasum -a 256` вместо `sha256sum`; `macos-14` → `macos-15`; гейт релиза на зелёный CI для того же SHA | C-21 | **[V]** |
| F11 | `LSMinimumSystemVersion`, запечённый `CARGO_MANIFEST_DIR` в UI-dir sidecar-бинаря, `cargo tauri` без пинна, `engines` для Node | — | **[A]** 04 G6/G5/G15 |
| F12 | `stage-sidecar.sh`: уметь принимать готовый бинарь из `release.yml` вместо принудительной пересборки | — | **[A]** 04 G11 |

**Effort: 3–4.5 дня.** F3 — самая рискованная часть (требует тестируемости Tauri-shell, у которой сейчас **0 тестов** **[A]**).

### WS-G — Test Foundation

**Цель:** сделать принцип №6 исполнимым и закрыть невидимые зоны.

| # | Задача | Метка |
|---|---|---|
| G1 | Вызвать `MockServer::verify()` во всех 8 местах с `.expect()` | **[V]** (сейчас 0 вызовов) |
| G2 | Обобщить `tests/upstream_harness.rs` в переиспользуемый harness с per-provider стабами | **[A]** 06 §2 |
| G3 | Handler-тесты admin API (13 handlers / 1 тест): `update_combo` (4 ветки 400), `create_key` 201+коллизия, revoke/restore идемпотентность, `update_connection` → `restart_required`, `restart_service` 409 | **[V]** |
| G4 | CORS + dashboard **через роутер**: один preflight (allowed + disallowed origin), `GET /` и `GET /{spa-route}` с временным `ui_dir` | **[A]** 06 §3.5–3.6 |
| G5 | Combo retry/failover: counting `FailBackend` (transient → success), ассерт attempt count, delay-путь, breaker-взаимодействие | **[A]** 06 §3.7 |
| G6 | Breaker half-open: in-flight trial cap (классический half-open) + тест N конкурентных ⇒ ровно один probe | **[A]** 06 §3.8 |
| G7 | Desktop Rust: `sidecar_port()` precedence + контракт порядка single-instance (защищён только комментарием) | **[A]** 06 §3.14 |
| G8 | Интеграционный тест построения backend'а **из реальной строки `provider_connections`** (иначе C-6 невидим) | **[A]** 02 §8 |
| G9 | Расширить JS-parity фикстуры (~32 кейса на 2908 строк translate-кода; `gemini_to_openai` = 3) | **[A]** 06 §3 |
| G10 | `cargo audit` / `cargo deny` в `check.sh` | **[A]** 03 I2 |

**Effort: 3–4.5 дня.** G1 — 30 минут и мгновенно повышает доверие к существующим 8 ассертам. G3 — самая высокая value-per-line: handlers уже доступны любому с `manage`-ключом.

### WS-H — Dead Config & Semantics Closure

**Цель:** убрать из UI то, что шлюз не делает.

| # | Задача | Дефект | Метка |
|---|---|---|---|
| H1 | `maxSetRetries` / `setRetryDelayMs`: **реализовать или удалить** из `ComboConfig`, парсера и UI | C-13 | **[V]** |
| H2 | `weight` / `strategy:"auto"`: убрать из UI-редактора, оставив round-trip, **или** реализовать | C-13 | **[V]** |
| H3 | Удалить мёртвое поле `RoutingBackend::combo_configs`; убрать двойной `load_combos` на запрос (существующий pre-loaded cache — правильное место) | C-14 | **[V]** |
| H4 | Affordance «этот combo пуст» в дашборде: 2 из 8 живых комбо не обслуживаются, а UI показывает `servable` без причины | — | **[A]** 01 F2 |

**Effort: 0.5–1 день.** H3 — почти чистый выигрыш: убирает два полных `SELECT … FROM combos` + JSON-парс + мьютексный захват одиночного writer'а на каждый запрос **[A]** 02 D11.

### Сводка effort

| WS | Полоса (чел.-дней) | Параллелизуемо? |
|---|---|---|
| A — Baseline | 1.5–2.5 | нет, блокирует всё |
| B — Fail-closed | 5–8 | частично (B1/B2/B3 независимы от B4) |
| C — State store | 2–3 | после B2 |
| D — Routing/observability | 2.5–4 | да, независим от B |
| E — Auth/data | 3–4.5 | да |
| F — Ops/desktop | 3–4.5 | да |
| G — Test foundation | 3–4.5 | да, сквозной |
| H — Dead config | 0.5–1 | да, в конце |
| **Итого** | **20.5–32** | **A + B subset ≈ 7–11** (ядро миссии) |

**Полосы [P]:** XS = ≤0.5 дня · S = 1 день · M = 2–3 дня · L = 4–6 дней.
Полный объём — **M-калибр** для одного инженера; **ядро (A + B + D)** — **S/M**.
При 2 инженерах с параллельной работой над D/E/F/G — реалистично 3–4 недели **[P]**.

---

## 6. Dependency graph

```text
                ┌──────────────────────┐
                │  WS-A  Baseline      │  ← M0, жёсткий блокер
                │  freeze + doc truth  │
                └──────────┬───────────┘
                           │ (всё остальное стартует на зафиксированном HEAD)
        ┌──────────────────┼──────────────────────────────────┐
        │                  │                                  │
        ▼                  ▼                                  ▼
┌───────────────┐  ┌──────────────────┐              ┌────────────────┐
│ WS-G Test     │  │ WS-D Routing &   │              │ WS-F Ops /     │
│ foundation    │  │ observability    │              │ desktop        │
│ G1 → G3..G9  │  │ D1..D7           │              │ F1..F12        │
└───────┬───────┘  └────────┬─────────┘              └───────┬────────┘
        │                   │                                │
        │  G1..G5 дают      │                                │ F3 требует
        │  красные тесты    │                                │ тестируемости
        │  для B            │                                │ из G7
        ▼                   │                                │
┌────────────────────────────────────────┐                   │
│ WS-B  Fail-closed P0                  │                   │
│ B1 (indep)  B2 ──┐  B3 (indep)        │                   │
│ B4 ◄── G4, G8 ──┤  B6 (indep)         │                   │
│ B5 ◄── B4 ──────┘                     │                   │
└───────────────┬────────────────────────┘                   │
                │ B2 решает, что хранить                   │
                ▼                                            │
        ┌───────────────────────┐                            │
        │ WS-C  State store     │                            │
        │ C1..C6 (+ миграция 002)│                           │
        └───────────┬───────────┘                            │
                    │                                         │
┌──────────────────────────────────────────────────────────────┘
▼
┌────────────────────────────────┐   ┌──────────────────┐   ┌──────────────┐
│ WS-E  Auth & data boundary    │   │ WS-H Dead config │   │ M3 Canaries  │
│ E1..E10                        │   │ H1..H4          │   │ (инфра-      │
│ (E1/E4 ломают тест-контракты)  │   │ (после B, D, E) │   │  структура    │
└────────────────────────────────┘   └──────────────────┘   │  без прогона)│
                                                          └──────────────┘
```

Критические рёбра:
- **WS-A → всё.** Без зафиксированного HEAD mission не откатывается.
- **WS-G1/G2 → WS-B.** Без harness'а «красный тест перед фиксом» неосуществим.
- **WS-B2 → WS-C.** Решение «хранить reasoning или отклонять» определяет, что попадает в store и какой нужен byte-cap.
- **WS-G7 → WS-F3.** Handshake нельзя покрыть тестом без тестовой обвязки Tauri-shell.
- **WS-E1/E4 → обновление существующих тестов** (`usable_keys_honour_kill_switches`, `repos.rs:1125-1130`): это **намеренное** изменение test-locked контракта, не регресс.
- **WS-B4 → WS-H1**: валидатор SSE определяет, что комбо-конфиг вообще может влиять на выдачу.

---

## 7. Milestones

### M0 — Reproducible Baseline
**Цель:** O-4. **Содержит:** WS-A (всё). **Полоса: S (1.5–2.5 дня).**

DoD:
- `git status --porcelain` пуст; `HEAD` содержит весь описанный в `STATUS.md` код.
- `git clone` в чистый каталог → `scripts/ci/check.sh` = «all green».
- Ни одной ссылки в `STATUS.md` / `docs/adr/README.md` / `README.md` на файл, отсутствующий в `HEAD`.
- В `docs/ROADMAP-2026-09.md` нет ни одной ячейки, опровергнутой собственным кодом (проверено по списку из 05 F-02 + 02 §3 + 01 §13 claims 26–31).
- `CHANGELOG.md` содержит 9 восстановленных записей; ни одного выдуманного номера версии.
- 0-byte `._*` артефакт удалён из индекса.

**Стоп-критерий:** если фиксация baseline требует >3 дней или выявляет, что часть дерева не компилируется отдельно от остального — mission переводится в режим «диагностика», а не «реализация»; §12 risk R-1 срабатывает.

### M1 — Fail Closed
**Цель:** O-1 + O-2. **Содержит:** WS-G1–G2 (минимально), WS-B (B1, B2, B3, B6), WS-D1. **Полоса: M (4–6 дней).**

DoD:
- B1: wiremock-тест, где тело ответа провайдера содержит уникальный маркер → маркер **отсутствует** в SSE-фрейме; присутствует в `gateway.log`.
- B2: `reasoning` item с `encrypted_content` доходит до тела upstream **идентично** (побайтовое сравнение) **или** приходит `400 unsupported_capability`. Двухходовый `store:false` replay проходит.
- B3: `tools:[{"type":"web_search"}]` + chat-only provider ⇒ `400`, поле не теряется молча.
- B6: перехваченное тело `/chat/completions` **не** содержит `store`/`include`/`reasoning`/`previous_response_id`/`text`/`session_id`/`conversation_id` для Responses-запроса.
- D1: регрессионный тест «backend, построенный из строки с алиасом `provider_connections`» ⇒ `routing.plan()` находит его, `/v1/models` его рекламирует, combo-plan его не теряет.
- Все 8 существующих `.expect()` вызывают `verify()`; приборная база harness'а переиспользована минимум в 5 новых тестах.
- `cargo test --workspace` ≥ **225** passed, 0 failed (213 + ≥12 новых).

**Стоп-критерий:** если B4 оценивается в >6 дней **после** M1 — он выносится в M2 первым пунктом, а не выполняется «по пути».

### M2 — Trustworthy State & Attribution
**Цель:** O-3 + доверие к диску. **Содержит:** WS-B4/B5 (если не в M1), WS-C, WS-D2–D7, WS-H3, WS-E1/E3/E5.

DoD:
- B4: полный tool-loop стрим проходит валидатор: `response.created` присутствует, `output_item.added` несёт настоящий `call_id`, `output_item.done` присутствует, **ровно один** terminal event, индексы монотонны.
- B5: snapshot-only tool call даёт `finish_reason: "tool_calls"`, а не `"stop"`.
- C1: storage key — gateway-minted; тест «upstream вернул id, совпадающий с чужим» не разрушает чужую запись.
- C2: prune по retention срабатывает; тест на устаревшую запись.
- C6: миграция `002` добавлена в `MIGRATIONS` (append-only), ledger-семантика сохранена.
- D2/D3: для combo-запроса строка `usage_history` содержит имя **конечного** provider'а, `combo_name`, реальный HTTP-код в `call_logs`, и `requested_model != model`.
- D4: `Retry-After: 3` ⇒ фактическая пауза ≥3 s (assert с допуском на таймер); upstream request-id сохраняется и возвращается клиенту.
- E1: решение (fail-closed или ADR-фиксация parity) принято **и записано**; если fail-closed — `usable_keys_honour_kill_switches` обновлён, а не удалён.
- E3: без `STORAGE_ENCRYPTION_KEY` refresh-запись **отказывает**, а не пишет plaintext; `warn!` на старте.
- E5: `gemini-thought-signatures.jsonl` и текущий `gateway.log` создаются `0600` (тест на свежем data-dir).
- `cargo test --workspace` ≥ **245** passed, 0 failed.

### M3 — Operational Boundary & CI
**Цель:** O-4 (операционная часть) + снятие «не тому процессу». **Содержит:** WS-F, WS-E2/E4/E6/E7/E9/E10, WS-H1/H2/H4, WS-G3–G10.

DoD:
- F1/F2: `api.js` fallback и `vite.config.js` указывают на `20129`; `AGENTS.md` и README это отражают.
- F3: sidecar поднимается на эфемерном порту **или** требует одноразовый nonce; коллизия порта приводит к явной ошибке, а не к логину в пустоту.
- F5: кнопка restart либо работает, либо не показывается с объяснением. Покрыто тестом на маршрут `/api/restart` (409 vs 202).
- F6: `scripts/launchd/*.plist` в репозитории + install/uninstall; README перестаёт быть неисполнимым.
- F8: `cargo fmt --manifest-path desktop/src-tauri/Cargo.toml -- --check` в `check.sh`; 2 диффа починены.
- F9: `desktop/src-tauri/**` и `docs/**` в `paths:`; Tauri `cargo check --locked` — в CI; toolchain закреплён.
- F10: `shasum -a 256`; `macos-15`; release требует зелёного CI для того же SHA.
- G3: **все 13** admin handlers имеют handler-тесты.
- G4: preflight и `GET /` / `GET /{spa-route}` проходят через `build_router_with_state`.
- G6: N конкурентных ⇒ ровно один half-open probe.
- H1/H2: `maxSetRetries`/`setRetryDelayMs`/`weight`/`strategy:auto` либо реализованы с тестом, либо отсутствуют в `ComboConfig` **и** в UI.
- E4: `key`-колонка больше не хранит рабочий bearer; `key_hash` проиндексирован; legacy-plaintext строки продолжают аутентифицироваться (тест).
- `cargo test --workspace` ≥ **290** passed, 0 failed; `scripts/ci/check.sh` зелёный на чистом clone.

**Стоп-критерий:** если F3 (handshake) требует больше 2 дней или ломает single-instance порядок — заменить на **честное удаление Tauri-fallback'а** (F1) + явную ошибку в окне, и записать в `STATUS.md`, что desktop-restricted. Умолчание: не рисковать десктопом ради полировки.

---

## 8. Sequencing

```text
Неделя/фаза   M0 ──────────► M1 ─────────────► M2 ─────────────► M3
             Baseline     Fail Closed      State+Attribution   Ops+CI

D0  A1-A3   ── freeze кода, ROADMAP-таблица
D1  A4-A11  ── changelog, ADR-addenda, history, ссылки, plist-документация
             (точка контроля: `git status` чист, clone зелёный)

D2  G1,G2   ── verify() + harness         ┐
D3  B1      ── SSE sanitize + регресс-тест│ ядро; B1/B2/B3 независимы
D4  B2,B3   ── reasoning + built-in tools │
D5  B6,D1   ── фильтрация extra + alias  ┘
             (точка контроля M1: ≥225 тестов)

D6  D2-D5   ── attribution, статус, Retry-After, count_active_api_keys
D7  B4,B5   ── SSE lifecycle + валидатор + snapshot-синтез
D8  C1-C6   ── state store (+ миграция 002)
D9  E1,E3,E5 ── fail-closed policy, plaintext-отказ, 0600
             (точка контроля M2: ≥245 тестов)

D10 G3-G6   ── admin, CORS/dashboard, combo retry, breaker half-open
D11 F1,F2,F5,F6,F8,F9,F10 ── порты, restart, plist, fmt-gate, CI paths, release
D12 F3,F4,F7 ── handshake, respawn, CSP     (рискованный блок; запасной вариант)
D13 E2,E4,E6,E7,E9,E10, H1-H4, G7-G10
             (точка контроля M3: ≥290 тестов, clone зелёный)
```

**Правила параллелизма [P]:**
- D6–D9, D11, D13 независимы по файлам и могут идти параллельно двумя потоками.
- **Никогда** не смешивать WS-A с WS-B/C/D в одном коммите: baseline-коммиты должны быть частями, а «сверху» — только mission-изменения.
- B4 — единственный пункт, который переписывает публичную форму SSE. Он требует отдельного PR с пометкой **breaking** для клиентов, которые уже завязались на `output_item.tool_calls` (нестандартная форма — **[V]** C-4). Таких клиентов в репозитории нет; внешние — неизвестно (**[?]**).

---

## 9. Rollback и границы совместимости

### 9.1 Frozen contracts — mission не имеет права их ломать без ADR

| Контракт | Где | Проверка |
|---|---|---|
| `STATIC_SALT = "omniroute-field-encryption-v1"`, префикс `enc:v1:` | `crypto/src/lib.rs:18-23` | тест-векторы, `AGENTS.md` |
| `HETERION_ROUTER_*` первичен, `OMNIROUTE_*` — fallback на один цикл | `config.rs:42-49` | `config.rs:99-153` |
| Data dir `~/.heterion-router` → legacy `~/.omniroute` | `db/src/db.rs:85-126` | `db.rs:146-169` |
| Порты `20128` (service) / `20129` (sidecar+dev) | `config.rs:51-53`, `src-tauri/src/lib.rs:21` | — |
| Keyless ⇒ **без** `Authorization` | `openai.rs:460-467` | `keyless_provider_sends_no_bearer` |
| Upstream 4xx ⇒ breaker **не** роняется | `routing.rs:44-55` | `only_provider_outages_are_breaker_failures` |
| `launchd` label `com.heterion.router` | `restart.rs:19-28` | `restart.rs:92-114` |
| `LEGACY` hash-only issuance shape `sk-`+32hex | `apikeys.rs:25-52` | `apikeys.rs:59-75` |

### 9.2 Обратная совместимость, которую mission сохраняет

- **Плоскийtext API-key строки продолжают аутентифицироваться.** E1 может изменить *policy* (scopes/models по умолчанию), но не должен ломать lookup. Если выбран fail-closed путь — нужна **одноразовая** миграция `002`/`003`, backfilling `scopes`/`allowed_models` для существующих строк из фактического поведения, иначе живая БД потеряет доступ. **Это главный rollback-риск миссии (R-2).**
- **`store: true` у клиентов продолжает работать.** C1 меняет форму storage-key ⇒ `previous_response_id`, выданный **до** обновления, перестанет резолвиться. Нужен read-fallback: если `resp_<uuid>` не найден, искать по сохранённому `upstream_response_id`. Без него — тихая потеря состояния у всех, кто пользуется continuation.
- **Живая БД.** Миграции строго append-only, ledger без downgrade **[V]** `migrate.rs:31-36`. Все DDL — `IF NOT EXISTS` / `INSERT OR IGNORE`. Никаких `DROP`/`ALTER` разрушающих.
- **Обе реализации (JS-frozen + Rust) делят одну БД** (ADR-001). Любая новая колонка обязана быть nullable или с константным default, иначе JS-сторона начнёт падать.
- **`/api/*` — только `manage`.** WS-F не может добавить публичный admin-путь «для удобства».

### 9.3 Что считается breaking для клиентов

| Изменение | Кто сломается | Как объявить |
|---|---|---|
| B4: настоящий Responses SSE lifecycle вместо `output_item.tool_calls` | Клиенты, завязавшиеся на нестандартную форму **[V]** C-4 | CHANGELOG `### Breaking`; feature-flag на 1 релиз |
| B3: built-in tools ⇒ `400` вместо молчаливого дропа | Агентные клиенты, полагавшиеся на дроп | CHANGELOG; это **и есть** O-1 |
| C5: смена дефолта `store` с `true` | Все, кто не задаёт `store` явно | Оставить `true` + retention; менять дефолт — отдельным решением **[P]** |
| E4: `key`-колонка перестаёт хранить hash | Скрипты/бэкапы, читающие `api_keys.key` | Документировать в `README`; live-колонка одна, миграция обратима |
| D3: `call_logs.status` становится реальным кодом | Дашборд, показывающий «500» | Это исправление; дашборд начнёт показывать 404/429 — ожидаемо |

### 9.4 Rollback-процедура

1. Каждый workstream — **отдельная ветка** от M0, squash в `main` только на зелёном CI.
2. Изменения схемы — отдельный коммит, ревьюится как таковой: без него остальное WS-C нельзя мержить.
3. Если milestone не проходит DoD — **откат milestone, а не смягчение DoD**. DoD-критерии, ослабленные однажды, не восстанавливаются.
4. Живой runtime перезапускается **только** через launchd и только после зелёного CI. Прямой `cargo run` на `20128` запрещён (иначе получим C-18 в проде).

---

## 10. Resource & Effort Bands

**[P]** — все оценки проектные, не измерены.

| Категория | Полоса | Обоснование |
|---|---|---|
| Baseline (WS-A) | **S**, 1.5–2.5 дня | 11 задач, в основном редактура; самая дорогая — A1 (разбиение дифа на коммиты) |
| Fail-closed ядро (WS-B) | **L**, 5–8 дней | B4 переписывает публичную SSE-форму; 4 независимых фикса с тестами |
| State store (WS-C) | **M**, 2–3 дня | новая миграция + read-fallback для старых id |
| Routing/observability (WS-D) | **M**, 2.5–4 дня | D1 тривиален, D2 — сквозная правка сигнатур по всему hot path |
| Auth/data (WS-E) | **M**, 3–4.5 дня | E1/E4 меняют test-locked поведение ⇒ нужны миграция и обновление тестов |
| Ops/desktop (WS-F) | **M**, 3–4.5 дня | F3 (handshake) — самая рискованная часть; имеет запасной вариант |
| Test foundation (WS-G) | **M**, 3–4.5 дня | G3 (13 handlers) + G4–G9; G1 тривиален и обязателен первым |
| Dead config (WS-H) | **XS**, 0.5–1 дня | удаление кода вместо реализации |
| **Всего** | **20.5–32 человеко-дня** | ≈ 4–6.5 недель одного инженера |
| **Ядро (A + B + D)** | **9–14.5 человеко-дня** | ≈ 2–3 недели — это mission, если ресурс ограничен |
| **Ядро + M2 (A+B+C+D)** | **11–17.5 человеко-дня** | ≈ 2.5–3.5 недели |

**Инфраструктурные затраты [V]:**
- Workspace tests: **~26 s** wall (213 тестов).
- `cargo tauri build` cold: **~5m15s** (1m55s gateway + 3m18s tauri) **[A] 04 §6.6**.
- `desktop/src-tauri` имеет **0 тестов**; добавление тестовой обвязки — отдельная работа (G7).

**Человеческие роли [P]:** 1 инженер (миссия целиком), либо 2 (поток «корректность B+D» и поток «ops F+G») с D10 как точкой синхронизации.

---

## 11. Stop / Go критерии

### 11.1 Go (продолжать)

| # | Критерий |
|---|---|
| GO-1 | `scripts/ci/check.sh` зелёный на **чистом clone** `HEAD`, а не только в грязном дереве |
| GO-2 | `cargo test --workspace` ≥ **225** после M1, ≥ **245** после M2, ≥ **290** после M3, 0 failures |
| GO-3 | Ни один новый дефект уровня C-1…C-24 не закрыт без падающего теста, который был красным до фикса |
| GO-4 | Ни одна миграция не удаляет/переименовывает существующую колонку; ledger остаётся append-only |
| GO-5 | Все существующие 8 `wiremock.expect()` вызывают `verify()` и проходят |
| GO-6 | `git status --porcelain` чист в конце каждого milestone |
| GO-7 | Для каждого пункта, меняющего test-locked поведение (E1, E4), есть **записанное решение** в ADR, а не молчаливая правка теста |
| GO-8 | Ни один пункт не расширяет non-goals (§3) без явного пересмотра mission charter |

### 11.2 Stop (остановиться и эскалировать)

| # | Триггер | Действие |
|---|---|---|
| STOP-1 | Фиксация baseline требует >3 дней, либо обнаруживается, что часть дерева не собирается изолированно | Mission → режим диагностики; R-1. Не начинать WS-B до разбора |
| STOP-2 | Любая миграция, которая ломает **плоскийtext API-key строки** в живой БД | Откат миграции; E1 переводится в «ADR-фиксация parity» вместо fail-closed |
| STOP-3 | B4 ломает `previous_response_id` replay или owner-scoped `GET/DELETE /v1/responses/{id}` (тесты `stored_responses_can_be_retrieved_and_deleted`, `stored_responses_are_scoped_to_the_creating_key`) | Остановить B4, выпустить B1/B2/B3/B6, переоткрыть B4 отдельной миссией |
| STOP-4 | F3 handshake ломает single-instance порядок или упирается в >2 дня | Заменить на F1 (удаление fallback) + явную ошибку; записать «desktop restricted» в `STATUS.md` |
| STOP-5 | Fiкс C-5 (store) приводит к потере состояния у клиентов, использующих continuation | Откатить C1; оставить только retention-prune (не меняющий ключ) |
| STOP-6 | Fiкс C-6 (alias) меняет `/v1/models` так, что **живые** комбо перестают разрешаться | Проверить 8 живых комбо против 24 активных провайдеров **до** мержа; при расхождении — канонизировать и в combo-plan, и в builder одновременно |
| STOP-7 | Любой пункт требует обращения к vendored/undocumented upstream (Grok private proxy, ChatHub, и т.п.) для подтверждения поведения | Пометить как `[?]`; не строить workaround на непроверяемой гипотезе |
| STOP-8 | Число тестов на защищённой поверхности (admin / SSE / alias) падает после рефакторинга | Откат рефакторинга; тесты — это контракт, а не артефакт |
| STOP-9 | `release.yml` на macOS-ноге падает **после** F10, несмотря на `shasum` | Не публиковать релиз; зафиксировать как внешний блокер с датой проверки |

---

## 12. Risk register

| ID | Риск | L | I | Триггер | Митигация | Владелец |
|---|---|---|---|---|---|---|
| **R-1** | Baseline не фиксируется (ветвление, конфликты при разбиении `+2997/−364` на коммиты) | Высокая | Высокая | STOP-1 | Разбивать по существующим `git diff`-кластерам; если не выходит — один коммит с честным сообщением, а не 11 фиктивных | architect |
| **R-2** | Fail-closed по policy (E1) **логинит живого оператора** с live-БД: `OpenCode-Main` имеет `scopes=["manage",…]`, `allowed_models=[]` **[V]** — но любая другая legacy-строка может иметь NULL | Средняя | **Критическая** | STOP-2 | До правки — инвентаризация всех `api_keys` строк; `INSERT OR REPLACE` backfill в миграции; отказ от E1 при невозможности доказать полноту | security |
| **R-3** | B4 (SSE lifecycle) ломает внешних SDK-клиентов, уже завязавшихся на нестандартный `output_item.tool_calls` | Средняя | Средняя | STOP-3 | Feature-flag на 1 релиз; CHANGELOG `### Breaking`; alias-событие на 1 релиз | gateway |
| **R-4** | C1 (gateway-owned key) ломает `previous_response_id` у всех, кто получил id до обновления | Средняя | **Критическая** | STOP-5 | Read-fallback по `upstream_response_id`; тест «старый id резолвится после миграции» | gateway |
| **R-5** | Retain-prune (C2) удаляет данные, которые клиент считает долгоживущими | Низкая | Высокая | — | Retention **по умолчанию** длинный (30 дней, как у OpenAI **[A]**), значение конфигурируемое, prune только по явному `stored_at` | gateway |
| **R-6** | F3 (sidecar handshake) ломает единственную работающую desktop-фичу | Средняя | Средняя | STOP-4 | Запасной вариант (F1 + явная ошибка) объявлен заранее | desktop |
| **R-7** | Live-сервис на `20128` и mission-правки расходятся: проверка «вживую» идёт против 4-дневного билда **[A] 01 F11** — сегодня PID 68027 от 2026-09-29 11:16, но `ProgramArguments` указывает в dev-дерево **[V]** | Высокая | Средняя | Любая «живая» верификация | **Не проверять mission-правки на живом сервисе.** Только чистый clone + `check.sh` + wiremock. Live-верификация — отдельным шагом после `launchctl` с пересобранным бинарём | architect |
| **R-8** | Regression в `select_backend` при канонизации (C-6): 24 активных провайдера, 8 комбо, 19 бэкендов — инварианты «сломаются молча» | Средняя | Высокая | STOP-6 | Инвариантный тест: `|backends| == 19` для текущего набора БД **до и после**; golden-тест на 8 комбо | routing |
| **R-9** | Рост test-suite делает гейт медленным и люди начинают его пропускать | Низкая | Средняя | Время гейта >60 s | Инкрементальные wiremock-тесты вместо end-to-end; `cargo test` по крейтам в CI при большом диффе | architect |
| **R-10** | `wiremock.verify()` (G1) немедленно вскрывает latent-баги в 8 существующих ассертах | **Средняя-высокая** | Низкая | Мгновенно после G1 | Ожидать этого. Считать G1 **отдельным инцидентом-уведомлением**, не «поломкой mission» | gateway |
| **R-11** | `docs/history/` решение (A6) перепутано: файлы удалены вместо переименования — теряется frozen upstream reference | Низкая | Низкая | — | По умолчанию `git mv` + README, **не** удаление | architect |
| **R-12** | ADR-001..007 формально «не переписывать» (`AGENTS.md:13-14`), но A5 требует addenda — конфликт инструкций | Низкая | Низкая | — | Только addenda, никогда не правка decision-текста; это уже зафиксировано в текущем дереве как pattern | architect |
| **R-13** | Внешние doc-источники (xAI / OpenAI / Anthropic) вернули 200 на 2026-09-29 **[A] 05 §F-09**, но это **не** conformance-тест шлюза | Средняя | Средняя | Любая правка «по документации» | Каждая ссылка на upstream-док помечается как ориентир, а не как доказательство соответствия; conformance проверяется wiremock-фикстурами | architect |
| **R-14** | `macos-14` удаляется с runner-образов 2026-11-02 **[A] 06 §4** — F10/F9 откладываются и ломаются позже | Низкая | Низкая | Дата | F10 в M3, до дедлайна | CI |
| **R-15** | Mission разрастается: «раз уж мы тут» — quota enforcement (NG-7), Anthropic backend (NG-1), notary (NG-5) | **Высокая** | Средняя | Любой PR за пределами §5 | Non-goals §3 — прибиты гвоздями; нарушение = STOP-8-подобная эскалация | architect |

---

## 13. Proposed mission status template

Предлагается как `STATUS.md`-шаблон на время миссии: заменяет «снимок фактов» на трекер обязательств, сохраняя раздел verified-facts.

```markdown
# STATUS — Heterion Router

> Mission: MISSION-2026-10 «Fail-closed & reproducible»
> Обновлено: <YYYY-MM-DD> · Ветка: <branch> · Baseline: <sha>

## Identity

```yaml
Name: heterion-router
Type: Platform / Service (AI gateway)
Layer: Intelligence / Execution
Status: Active — mission MISSION-2026-10
Manifest version: 0.1.0
Baseline commit: <sha>            # ← NEW: фиксированный, воспроизводимый
Mission started: <YYYY-MM-DD>     # ← NEW
```

## Milestone tracker

| MS | Название | Цель | Статус | DoD выполнено | Блокер |
|---|---|---|---|---|---|
| M0 | Reproducible Baseline | O-4 | ✅ done / 🟡 in progress / ⛔ blocked | _/6_ | — |
| M1 | Fail Closed | O-1, O-2 | … | _/7_ | — |
| M2 | State & Attribution | O-3 | … | _/10_ | — |
| M3 | Ops & CI | O-4b | … | _/12_ | — |

Статусы: ⬜ not started · 🟡 in progress · ✅ done · ⛔ blocked · ⏸ deferred (в non-goals)

## Objectives

| # | Objective | Метрика | Текущее значение | Целевое |
|---|---|---|---|---|
| O-1 | Нет silent loss на Responses | reasoning-item wire-тест | ⛔ нет теста | ✅ есть |
| O-2 | Нет утечки upstream в SSE | маркер в SSE-фрейме | ⛔ есть | ✅ нет |
| O-3 | Достоверная атрибуция | provider ≠ "routing" | ⛔ "routing" | ✅ реальный |
| O-4 | Воспроизводимый baseline | clone → check.sh | ⛔ грязное дерево | ✅ зелёный |

## Workstream status

| WS | Название | Полоса | Статус | PR | Замечания |
|---|---|---|---|---|---|
| A | Baseline Freeze & Doc Truth | S | ✅ | #… | … |
| B | Fail-Closed Correctness | L | 🟡 | #… | B4 в M2 |
| C | State Store | M | ⬜ | | ждёт B2 |
| D | Routing & Observability | M | 🟡 | | |
| E | Auth & Data Boundary | M | ⬜ | | E1/E4 ломают тесты |
| F | Operational Boundary | M | ⬜ | | F3 рискован |
| G | Test Foundation | M | 🟡 | | сквозной |
| H | Dead Config Closure | XS | ⬜ | | в конце |

## Quality gates (факт, не намерение)

| Гейт | Команда | Результат | Когда |
|---|---|---|---|
| Workspace tests | `cargo test --workspace` | **N passed / 0 failed** | <date> |
| fmt | `cargo fmt --all -- --check` | pass/fail | <date> |
| clippy | `cargo clippy --workspace --all-targets -- -D warnings` | pass/fail | <date> |
| Desktop fmt | `cargo fmt --manifest-path desktop/src-tauri/Cargo.toml -- --check` | pass/fail | <date> |
| Desktop check | `cargo check --manifest-path desktop/src-tauri/Cargo.toml --locked` | pass/fail | <date> |
| UI build | `npm --prefix desktop/ui run build` | pass/fail | <date> |
| CI | `scripts/ci/check.sh` на чистом clone | pass/fail | <date> |

> «Локально; шире, чем CI» — расхождения перечислены ниже и **являются** mission-пунктами.

## Verified facts (только что подтверждено в этом сеансе)

- <факт> — `<file:line>` или <команда>
- …

> Всё, чего здесь нет, — **не** проверено. Не наследовать утверждения из
> предыдущих отчётов без повторной проверки.

## Known open (не в mission scope)

- <пункт> — ссылка на §Non-goals или risk ID

## Mission decisions (ADR-ссылки)

| Решение | ADR | Дата | Следствия |
|---|---|---|---|
| … | ADR-011 | … | … |
```

**Правила шаблона [P]:**
1. Числа в Quality gates заполняются **прогоном**, а не переносом. Раздел «Verified facts» допускает только то, что перепроверено в текущем сеансе.
2. Objective-метрики выражены **проверяемыми** утверждениями (есть тест / нет маркера / значение поля), а не «улучшено».
3. Workstream-таблица — источник истины для sequencing; roadmap получает из неё финальную таблицу.
4. Секция «Known open» обязана ссылаться на §Non-goals или Risk ID — иначе она становится новым неучтённым бэклогом.

---

## 14. Prioritized table для копирования в canonical roadmap

> Копировать в `docs/ROADMAP-2026-09.md`, заменив текущий раздел
> «Prioritized backlog» (строки 67–102). Столбец **WS** → **MS** — целевые
> милестоуны из §7. Столбец **Факт** — проверенное сегодня состояние;
> столбец **Acceptance** — измеримый критерий DoD.

### M0 — Reproducible Baseline (WS-A)

| # | Пункт | Факт (проверено 2026-09-29) | Acceptance | Sev | MS |
|---|---|---|---|---|---|
| 1 | Зафиксировать рабочее дерево коммитами | `HEAD=51dfe6d`; 35 modified + 5 untracked; `+2997/−364` **[V]** | `git status --porcelain` пуст; clone → `scripts/ci/check.sh` зелёный | High | M0 |
| 2 | Починить «Текущее состояние» таблицу ROADMAP | ≥5 ячеек опровергнуты кодом **[A 05 F-02]** | ни одной опровергнутой ячейки, либо таблица помечена как «срез 2026-09-25» | High | M0 |
| 3 | Вернуть 9 записей в `CHANGELOG.md` | удалены при uncommitted-rewrite **[A 05 F-03]** | все 9 на месте; ни одного выдуманного номера версии | High | M0 |
| 4 | ADR addenda (007 / 002 / 006 / 008) | 3 места в ADR-007 устарели **[A 05 F-05]** | addenda есть; decision-текст ADR-001..007 не тронут | Med | M0 |
| 5 | Разобрать `docs/history/` | описывает чужой (JS) проект **[A 05 F-01]** | `git mv` + README, 5 ссылок поправлены | Med | M0 |
| 6 | Цитаты `file.rs:N` → `file.rs:symbol` | 12 цитат указывают на неверные строки **[A 05 F-11]** | 0 «голых» `:N` в ROADMAP | Low | M0 |
| 7 | Убрать tracked 0-byte `._*` иконку | в индексе **[V]** | нет в `git ls-files`; `._*`/`.!*` в `.gitignore` | Low | M0 |
| 8 | Документация портов и plist | plist вне репозитория, bind `0.0.0.0` **[V]** | README/AGENTS явно называют 20128/20129 и LAN-bind | Med | M0 |
| 9 | Перенести ChatHub evidence из `/var/folders` | в `docs/research/chathub-evidence/` **[V]** | ссылки не указывают на temp-каталог | Low | M0 |

### M1 — Fail Closed (WS-B + WS-D1 + WS-G1/2)

| # | Пункт | Факт (проверено 2026-09-29) | Acceptance | Sev | MS |
|---|---|---|---|---|---|
| 10 | Sanitize SSE error frames | `lib.rs:493-495` отдаёт сырое тело до 2000 симв.; non-stream sanitizes (`lib.rs:538-545`) **[V]** | маркер из тела мока отсутствует в SSE; присутствует в `gateway.log` | **High** | M1 |
| 11 | Сохранять `reasoning` input items | `lib.rs:1015-1017` `_ => {}` **[V]** | байт-в-байт до upstream **или** `400 unsupported_capability`; `store:false` 2 хода | **High** | M1 |
| 12 | Отклонять built-in tools | `lib.rs:1069` `filter_map` по `name` **[V]** | `web_search` + chat-only ⇒ 400, не тихий дроп | **High** | M1 |
| 13 | Фильтровать Responses-only ключи в Chat body | `backend.rs:97-99` сплайсит `extra` целиком **[V]** | тело `/chat/completions` без `store`/`include`/`reasoning`/`previous_response_id`/`text` | High | M1 |
| 14 | Канонизировать `provider` при сборке backend | `main.rs:201,237,295` vs `routing.rs:166` **[V]** | backend из alias-строки routable, рекламируется в `/v1/models`, не теряется в combo | High | M1 |
| 15 | Вызывать `MockServer::verify()` | 8 × `.expect()`, 0 × `.verify()` **[V]** | все 8 верифицируются и проходят | High | M1 |
| 16 | Обобщить `upstream_harness.rs` | 1 тест, 28 строк, без `HttpClient` **[A 06 §2]** | переиспользован в ≥5 новых тестах | Med | M1 |

### M2 — Trustworthy State & Attribution (WS-B4/5 + WS-C + WS-D2–7 + WS-E1/3/5)

| # | Пункт | Факт (проверено 2026-09-29) | Acceptance | Sev | MS |
|---|---|---|---|---|---|
| 17 | Типизированный Responses SSE lifecycle | 4 события из ~10; `call_id:""`, вложенный `tool_calls` **[V]** | `response.created` + настоящий `call_id` + `output_item.done` + **ровно один** terminal; валидатор | **High** | M2 |
| 18 | Синтез tool-call из `response.completed` | `translate/responses.rs:308-315` только `finish_chunk` **[V]** | snapshot-only tool call ⇒ `finish_reason: "tool_calls"`, не `"stop"` | **High** | M2 |
| 19 | Gateway-owned storage key | `kv_set(…, upstream_id, …)` — upsert без ownership **[V]** | чужой id от upstream не перезаписывает чужую запись | **High** | M2 |
| 20 | Retention + byte-cap для Responses store | `store` default `true` (`lib.rs:830-833`), TTL/размер отсутствуют **[V]** | prune по `stored_at` при старте; тест на устаревшую запись | **High** | M2 |
| 21 | Миграция `002` под retention | `key_value` = `(namespace,key) PK`, нет `stored_at` **[V]**; ledger append-only **[V]** | миграция добавлена, `001` не тронут | High | M2 |
| 22 | Read-fallback для старых `previous_response_id` | сейчас 0 строк в проде **[V]**, но клиенты будут | id, выданный до обновления, резолвится | **High** | M2 |
| 23 | Provider/connection/combo attribution | `provider="routing"`, `connection_id: None`, `combo_name` не пишется **[V]** | для combo-запроса в `usage_history` — конечный provider + `combo_name` | High | M2 |
| 24 | Реальный HTTP-статус в `call_logs` | `status: if success {200} else {500}` (`lib.rs:348`) **[V]** | 404/429/5xx пишутся как есть; `requested_model != model` при combo | High | M2 |
| 25 | `Retry-After` + request-id | `rg -i 'retry.after' crates/` пусто **[V]** | `Retry-After: 3` ⇒ пауза ≥3 s; id сохраняется и отдаётся клиенту | Med | M2 |
| 26 | Fail-closed или ADR для NULL policy | `COALESCE(scopes,'["manage"]')` (`repos.rs:353-364`) **[V]** | решение записано; живая БД не теряет доступ | **High** | M2 |
| 27 | Отказ от plaintext-записи refresh-токена | `main.rs:110-113` `unwrap_or_else(plaintext)` **[V]** | без ключа запись отказывает + `warn!` на старте | Med | M2 |
| 28 | Логи и signature-файлы `0600` | `gemini.rs:178-182` без `.mode()`; `main.rs:390` до `392` **[V]** | свежий data-dir ⇒ `gateway.log` и `…signatures.jsonl` = 0600 | Med | M2 |
| 29 | Убрать двойной `load_combos` + мёртвый `combo_configs` | 3 вхождения, чтений нет **[V]** | 1 загрузка на запрос; поле удалено | Low | M2 |
| 30 | Отчёт о недостижимых registry-записях | 25 providers / 801 models не строятся; 61/959 игнорируют executor **[A 02 §1]** | `/api/overview` показывает недостижимые записи | Med | M2 |
| 31 | `no_log` гасит и state store | `persist_response` не проверяет флаг **[A 02 D8]** | `no_log`-ключ не пишет transcript | Med | M2 |

### M3 — Operational Boundary & CI (WS-F + WS-E + WS-G + WS-H)

| # | Пункт | Факт (проверено 2026-09-29) | Acceptance | Sev | MS |
|---|---|---|---|---|---|
| 32 | Tauri fallback-порт `20128` → `20129` | `api.js:45` vs `src-tauri/src/lib.rs:21` **[V]** | fallback указывает на sidecar; тест на разрешение | **High** | M3 |
| 33 | Vite dev-proxy `20128` → `20129` | `vite.config.js:7` vs `config.rs:51-53` **[V]** | dev-контур не попадает в живую БД | **High** | M3 |
| 34 | Sidecar readiness/ownership handshake | URL инжектится до bind **[A 04 G2]** | коллизия порта ⇒ явная ошибка, не «логин в пустоту» | High | M3 |
| 35 | Sidecar respawn с backoff | `CommandEvent::Terminated` только логируется **[A 04 G1]** | death виден в окне, bounded respawn | High | M3 |
| 36 | Кнопка «Restart now» на десктопе | 409 **или** рестарт чужого процесса на 20128 **[A 04 §5.2]** | работает, или не показывается с объяснением; покрыто тестом | High | M3 |
| 37 | Коммит launchd plist + install | `find . -name '*.plist'` пусто **[V]** | plist в репозитории, install/uninstall задокументированы | High | M3 |
| 38 | Handler-тесты admin API | 13 handlers / 1 тест **[V]** | 13/13 покрыты | High | M3 |
| 39 | `cargo fmt` для `desktop/src-tauri` в гейте | 2 диффа, гейт зелёный **[V]** | step в `check.sh`, диффы починены | Med | M3 |
| 40 | CI `paths:` + Tauri job + toolchain | `paths:` без `desktop/src-tauri/**`, `docs/**`; `@stable` vs pin 1.98.0 **[V]** | Tauri `cargo check --locked` в CI; toolchain закреплён | Med | M3 |
| 41 | `release.yml`: `shasum`, `macos-15`, гейт на CI | `sha256sum` на macos-ноге **[V]**; `macos-14` уходит 2026-11-02 **[A 06 §4]** | workflow_dispatch проходит; релиз требует зелёного CI | Med | M3 |
| 42 | CSP в Tauri | `"csp": null` + `shell:allow-spawn` **[V]** | CSP задан; capability сужена | Med | M3 |
| 43 | `key`-колонка = hash ⇒ hash валидный bearer | `repos.rs:226-256`, индекс только на `key` **[V]** | дубль убран, `key_hash` проиндексирован, legacy работает | Med | M3 |
| 44 | `PATCH /api/keys/{id}` для сужения ключа | `CreateKeyBody` = одно поле `name` **[V]** | существующий ключ можно сузить | Med | M3 |
| 45 | `HETERION_ROUTER_UI_DIR` containment | без проверки корня **[V]** | корень ограничен; опция auth для UI на non-loopback | Med | M3 |
| 46 | `launchctl` без `sh -c` | `restart.rs:45,66-70` **[V]** | метка передаётся через `argv` | Med | M3 |
| 47 | Мёртвый combo-конфиг | `maxSetRetries`/`setRetryDelayMs`/`weight`/`strategy:auto` не влияют **[V]** | реализовано с тестом **или** отсутствует в коде и UI | Med | M3 |
| 48 | CORS + dashboard через роутер | не тестируются **[A 06 §3.5–3.6]** | preflight (allowed/disallowed) + `GET /` + `GET /{spa}` через `build_router_with_state` | Med | M3 |
| 49 | Combo retry/failover тесты | ни один тест не задаёт retry-конфиг **[A 06 §3.7]** | counting backend: попытки, задержка, breaker | Med | M3 |
| 50 | Breaker half-open: cap + тест | `allow()` возвращает true всем после cooldown **[A 06 §3.8]** | N конкурентных ⇒ ровно один probe | Med | M3 |
| 51 | `count_active_api_keys` | игнорирует `is_active`/`is_banned` **[V]** | SQL + тест | Low | M3 |
| 52 | `logs_app`: tail, не slurp | читает файл целиком **[A 03 L8]** | bounded read | Low | M3 |
| 53 | Redacting `Debug` на 4 credential-структурах | `Credential`, `ResponsesBackend`, `ProviderOpenAiBackend`, `OpenAiBackend` **[A 03 L6]** | прецедент `FieldCrypto` применён | Low | M3 |
| 54 | `stage-sidecar.sh` принимает готовый бинарь | всегда пересобирает **[A 04 G11]** | `release.yml`-артефакт можно переиспользовать | Low | M3 |
| 55 | `LSMinimumSystemVersion`, baked `CARGO_MANIFEST_DIR`, пинн Node/`cargo tauri` | arm64-бандл с `10.13`; Node 24 vs 26 **[A 04 G5/G6/G15]** | манифесты закреплены | Low | M3 |
| 56 | Affordance «пустой combo» | 2 из 8 комбо не обслуживаются **[V]** | UI показывает, что combo пуст | Low | M3 |
| 57 | Расширить JS-parity фикстуры | ~32 кейса на 2908 строк; `gemini_to_openai` = 3 **[A 06 §3]** | +10 кейсов (parallel tools, multi-part, images, `strict`, reasoning) | Med | M3 |
| 58 | `cargo audit` / `deny` в `check.sh` | нет в CI **[A 03 I2]** | добавлено | Low | M3 |
| 59 | Desktop Rust-тесты | 0 тестов **[A 06 §3.14]** | `sidecar_port()` precedence + контракт порядка single-instance | Med | M3 |
| 60 | Тест «backend из реальной строки `provider_connections`» | без него C-6 невидим **[A 02 §8]** | тест есть; иначе фикс 14 снова откатится | Med | M1 |

---

## 15. Appendix — команды, выполненные для этого отчёта

```sh
# структура и git
git log --oneline -5 ; git status --porcelain ; git diff --stat | tail -3
git remote -v ; git branch -a

# гейты
cargo test --workspace                      # 213 passed, 0 failed
(cd desktop/src-tauri && cargo fmt -- --check)   # 2 diffs

# source
rg -n "fn error_event|fn error_response" crates/heterion-router-gateway/src/lib.rs
sed -n '1005,1025p;1060,1080p;1250,1300p;300,370p;570,600p;826,840p' \
    crates/heterion-router-gateway/src/lib.rs
rg -n "backends.insert" crates/heterion-router-gateway/src/main.rs
rg -n "fn count_active_api_keys" -A10 crates/heterion-router-db/src/repos.rs
rg -n "max_set_retries|set_retry_delay_ms|combo_configs" crates/
rg -c "\.verify\(\)" crates/ ; rg -c "\.expect\(" crates/heterion-router-gateway/src/openai.rs
rg -n "csp" desktop/src-tauri/tauri.conf.json
rg -n "20128|20129" desktop/ui/src/api.js desktop/ui/vite.config.js desktop/src-tauri/src/lib.rs
rg -n "sha256sum|macos-14" .github/workflows/release.yml
rg -n "paths:" -A9 .github/workflows/ci.yml
find . -name "*.plist" -not -path "./.git/*"
rg -n "key_value|CREATE INDEX" crates/heterion-router-db/schema_base.sql

# registry (пересчёт)
python3 -c "…" crates/heterion-router-providers/registry.json

# live runtime (только чтение)
curl -s http://127.0.0.1:20128/healthz
curl -s -o /dev/null -w '%{http_code}\n' http://127.0.0.1:20128/api/overview
ps -o pid,lstart -p 68027 ; lsof -nP -iTCP:20128 -sTCP:LISTEN
cat ~/Library/LaunchAgents/com.heterion.router.plist
stat -f '%Sp %N' ~/.heterion-router/*
rg 'backend available' ~/.heterion-router/gateway.log.2026-09-29 | tail -19
sqlite3 "file:$HOME/.heterion-router/storage.sqlite?mode=ro" \
  "SELECT COUNT(*) FROM key_value WHERE namespace='responses';" \
  "SELECT DISTINCT provider FROM provider_connections WHERE is_active=1;" \
  "SELECT name, length(key), length(key_hash), scopes FROM api_keys;" \
  "SELECT provider, length(access_token), length(api_key) FROM provider_connections
    WHERE provider IN ('opencode','xai','deepseek');"
```

**Ничего в каноническом репозитории не изменено и не закоммичено.**
`git status --porcelain` остаётся тем же, что до этого отчёта (40 записей),
за вычетом возможной правки `target/` при `cargo test`.

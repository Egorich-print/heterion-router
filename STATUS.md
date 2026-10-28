# STATUS

> Heterion Router — Rust-шлюз, дашборд и десктоп-оболочка.
> Обновлено: 2026-10-02
> Миссия: **MISSION-2026-10 «Fail-closed & reproducible»** — в работе; P0- и P1-срезы
> закрыты вместе с hardening-пакетом из аудита 2026-10 (ADR-012).

## Identity

```yaml
Name: heterion-router
Type: Platform / Service (AI gateway)
Layer: Intelligence / Execution
Status: Active — planning MISSION-2026-10
Repository: ai-workstation/Projects/heterion-router
GitHub: https://github.com/Egorich-print/heterion-router
License: MIT
Manifest version: 0.1.0
Baseline commit: 51dfe6d          # + рабочее дерево аудита поверх него
Mission started: 2026-09-29
```

> Snapshot: этот документ описывает текущее рабочее дерево. На момент аудита
> оно содержало незакоммиченные изменения поверх `main` (`51dfe6d`); перед
> использованием как release-документации изменения нужно зафиксировать в git.
> Это первый пункт milestone M0.

## Что реально работает

- **Gateway**: axum-сервис с `/v1/chat/completions`, `/v1/models`,
  `/v1/responses` и `GET/DELETE /v1/responses/{id}`, SSE, combo routing,
  retry/failover и breaker. Upstream 4xx не роняет breaker.
- **Responses happy path**: text/tools input, Responses extras, native
  per-model Responses transport для моделей с `target_format=openai-responses`,
  owner-scoped non-stream state store и replay `previous_response_id` после
  restart. Код покрыт unit-тестами; live traffic в production не наблюдался
  (`responses` namespace в live DB был пуст).
- **Responses streaming — частичный**: есть text/reasoning deltas и terminal
  usage, но нет полного `response.created`/`output_item.done`/argument-delta
  lifecycle; tool event shape не является проверенным OpenAI SDK contract.
- **xAI/Grok**: per-model Responses dispatch для xAI; отдельный Grok CLI
  backend с 401→form refresh→retry, single-flight и optional DB write-back.
  Это private/undocumented transport; live refresh требует runtime OAuth
  client configuration.
- **Gemini**: `generateContent`, tools и persistent thought-signature file.
  Fallback последней подписи остаётся process-global и не изолирован по
  параллельным сессиям.
- **Routing/combos**: canonical `provider/model`, combo plan, priority order,
  transient retry и pre-first-byte stream failover работают. `weight`,
  `strategy:auto`, `maxSetRetries` и `setRetryDelayMs` парсятся, но не
  влияют на routing.
- **Registry — каталог, не гарантия доступности**: 278 providers / 2717
  models в рабочем дереве (в `HEAD` — 278 / 2715) в `registry.json`; structurally
  проходят builder 259 / 2108, из них 253 / 1916 имеют HTTP(S) base URL, а
  6 / 192 используют неподдерживаемые схемы. Ещё 19 providers / 609 models не
  проходят format/base-URL gate; 61 HTTP-capable provider / 959 models игнорируют
  bespoke `executor` и обслуживаются generic path.
- **Auth**: DB failures fail closed; admin mutations требуют `manage`;
  model allow-list применяется к Chat/Responses; non-loopback bind без ключа
  не стартует. Новые Rust-issued keys хранятся по SHA-256, legacy plaintext
  lookup совместим; `no_log` отключает usage/call-log, но не state store и
  не `last_used_at`. NULL policy columns дают legacy-строкам `manage` и
  неограниченный доступ к моделям.
- **Storage**: shared SQLite boundary, WAL, data dir обычно `0700`, DB/`.env`
  обычно `0600`. Log и signature files, созданные после startup permission
  pass, могут оставаться `0644` до следующего boot.
- **Observability**: non-stream и terminal-stream usage/call-log writes
  существуют, но Rust rows используют `provider="routing"`, `connection_id=NULL`,
  requested model и synthetic status 200/500; provider/connection/combo
  attribution и request-id/`Retry-After` не реализованы.
- **Dashboard/desktop**: Svelte 5/Vite UI и Tauri v2 sidecar, single-instance
  и parent-watch компилируются и проходят локальную проверку. Это не
  notarized distributable: local build даёт adhoc `.app`/`.dmg`; sidecar
  readiness, restart и release service описаны в `docs/research/PROJECT-AUDIT-2026-09.md`.
- **ChatHub**: внутренний protocol и fresh anonymous-session probes (0 guest
  cookies, chat 403, stream 401) задокументированы; ChatHub намеренно не
  подключён как undocumented upstream (ADR-010).

## Live/runtime facts

- Launchd label: `com.heterion.router`; установленный job слушает
  `0.0.0.0:20128`. Plist машинно-локальный и не входит в репозиторий; его
  `ProgramArguments` указывает на `target/release/` dev-дерева, а сам запущенный
  бинарь новее `HEAD` — живой инстанс **не** пригоден для проверки mission-правок.
- Tauri sidecar и чистый dev gateway используют `20129`; Vite dev-proxy по
  умолчанию использует `20128`, а `tauri dev` не поднимает sidecar ⇒ dev-окно
  пишет в БД живого launchd-сервиса. Требует исправления (MISSION-2026-10, F1/F2).
- В БД 24 активных провайдера, в логе — 19 backends. `heterion-local` не
  настроен. `exa-search` имеет 5 активных connection-строк, но отсутствует в
  `registry.json` и молча теряется при построении backends. У `kimi-coding`,
  `kiro` и `opencode` credentials пустые.
- Восемь combo; `vivanta-auto` и `vivanta-deepseek` имеют `models: []` и сейчас
  логируются как `500 / provider='routing' / stream_error` (11 строк в живой БД).
- `usage_history.status` содержит два непересекающихся словаря: цифровые HTTP-коды
  (JS-сторона) и `succeeded`/`failed` (Rust, 328 строк). Запросы вида
  `status='429'` и `status='succeeded'` не пересекаются.
- Responses store: namespace `responses` пуст — production-трафика по store не было.
- `STORAGE_ENCRYPTION_KEY` присутствует в `~/.heterion-router/.env`, поэтому
  plaintext-путь записи OAuth-токена на этом хосте не активирован; сам код-путь
  (запись без ключа) остаётся и будет закрыт в MISSION-2026-10 (E3).
- `/healthz` и статика SPA публичны; `/v1/*` и `/api/*` закрыты auth/key
  boundary при наличии ключей.

## Milestone tracker — MISSION-2026-10

| MS | Название | Цель | Статус | DoD |
|---|---|---|---|---|
| M0 | Reproducible Baseline | O-4 | ⬜ not started | _/6_ |
| M1 | Fail Closed | O-1, O-2 | ⬜ not started | _/7_ |
| M2 | Trustworthy State & Attribution | O-3 | ⬜ not started | _/15_ |
| M3 | Operational Boundary & CI | O-4b | ⬜ not started | _/13_ |

| # | Objective | Метрика | Сейчас | Цель |
|---|---|---|---|---|
| O-1 | Нет silent loss на Responses | reasoning-item wire-тест | ⛔ нет теста | ✅ есть |
| O-2 | Нет утечки upstream в SSE | маркер в SSE-фрейме | ⛔ есть | ✅ нет |
| O-3 | Достоверная атрибуция | `provider ≠ "routing"` | ⛔ `"routing"` | ✅ реальный |
| O-4 | Воспроизводимый baseline | clone → `check.sh` | ⛔ грязное дерево | ✅ зелёный |

Детали, зависимости, роли субагентов и stop/go-критерии:
`docs/MISSION-2026-10.md`. Девять открытых решений владельца — §13 там же.

## Проверки

```sh
scripts/ci/check.sh    # fmt + clippy + test, ровно то, что гоняет CI
```

Локально дополнительно (шире, чем CI):

```sh
npm --prefix desktop/ui run build
cargo check --manifest-path desktop/src-tauri/Cargo.toml --locked
cargo fmt --manifest-path desktop/src-tauri/Cargo.toml -- --check
```

На 2026-10-02: `scripts/ci/check.sh` = `all green` на **Rust 1.99.0**
(552 passed, 0 failed), `cargo fmt --all -- --check` и
`cargo fmt --check` в `desktop/src-tauri` — чисто. Два baseline нужно
различать: коммит `51dfe6d` даёт 186 тестов, а рабочее дерево на момент
начала аудита — 215 (в нём уже лежал P0-срез предыдущей сессии). Прирост
аудита считается от 215. Расхождения с CI закрыты:
`rust-toolchain.toml` и CI закрепляют один и тот же `1.99.0` (было `@stable`
против пина `1.98.0`), в `paths:` добавлены `desktop/src-tauri/**` и `docs/**`,
появился job `tauri-fmt`, а `release.yml` больше не зовёт `sha256sum` на
macOS-раннере. Остаётся: `cargo check`/`cargo tauri build` для Tauri в CI не
гоняются (нужны webkit2gtk/GTK). Полный `cargo tauri build` проходит локально,
но не воспроизводится из fresh clone: `externalBin` указывает на gitignored
`desktop/src-tauri/binaries/`, а `stage-sidecar.sh` вызывается только из
`beforeBuildCommand`.

## Известные ограничения и открытые риски

- **Cost & limits**: бюджеты по ключу, RPM и кэш ответов закрывают
  «сколько я потратил», но `domain_cost_history` пополняется только из
  `/v1/*`; admin-поверхности, которые тратят деньги, не обходятся через него.
  Перцентили стоимости не считаются — есть только шов
  `usage_cost_rows_in_window` и p50/p95/p99 по задержке.
- **Guardrails**: эвристики, не классификатор. Порог энтропии 4.2 бит/символ
  отделяет прозу от base64-блобов, но на коротких токенах (<32 символов) не
  срабатывает; `language-detection` воздерживается при <3 латинских словах и при
  равенстве стоп-слов; CJK различается только по наличию kana; `ko` не
  определяется никогда.
- **Cache**: тела ответов лежат в `storage.sqlite` открытым текстом и наследуют
  только права 0600/0700 — файл чувствителен ровно настолько, насколько
  чувствительны промпты, которые он повторяет. TTL — единственный способ
  стирания. Индекс по `expires_at` отсутствует, своп идёт полным сканом.
- **Responses**: state store теперь ограничен (TTL 7 дней, 512 записей,
  256 KiB на запись, своп ленивый) и не пишется для стримов; built-in tools
  отвергаются fail-closed; streaming tool contract неполон, reasoning input
  items могут silently drop.
- **Provider routing**: alias-keyed connections могут быть `servable:true`, но
  не routable; часть registry entries недостижима; xAI snapshot-only tool
  calls теряются; Responses-only поля могут утекать в Chat body. Стратегии
  кроме ordered-priority (`weight`, `least-latency`, `cost-optimized`) парсятся
  и отбрасываются.
- **Security**: закрыто в 2026-10 — sanitization streaming error frames,
  fail-closed на plaintext-запись refresh-токена без `STORAGE_ENCRYPTION_KEY`,
  не-servable провайдеры логируются агрегированно, replay дайджеста
  `api_keys.key`, завышение `count_active_api_keys`, нефильтрованный
  `COUNT(*)` в гейте не-loopback bind, неограниченный буфер SSE и чтение
  всего лога. Остаётся — NULL policy columns дают legacy keys `manage` и
  неограниченный доступ к моделям; dashboard static routes и Tauri sidecar
  port требуют hardening; `tauri.conf.json` имеет `csp: null` вместе с
  неиспользуемой capability `shell:allow-spawn`.
- **Operations**: рестарт больше не идёт через `sh -c` (argv + валидация
  launchd-цели), но сама метка по-прежнему приходит из окружения, так что
  неверно настроенный label тихо ничего не перезапустит — ошибка видна только
  в логе; launchd install не воспроизводим из репозитория; текущий
  live plist указывает на dev tree; rotated logs могут быть world-readable;
  desktop restart может перезапустить не тот процесс; fresh clone не собирает
  desktop; bundle не проходит `codesign --verify --deep --strict` и не notarized.
- **Testing**: 556 workspace tests (215 в дереве до аудита, 186 на `51dfe6d`). Не покрыты в полном объёме
  Responses SSE tool loop, alias construction, derived Responses URLs и
  desktop Rust shell (в `desktop/src-tauri` тестов нет вовсе). Все 30
  `wiremock.expect()` теперь проверяются `MockServer::verify()`, и каждая
  проверка доказана load-bearing (28 прогонов, где тест обязан упасть).
- **TLS/LAN**: gateway сам не является TLS-сервером; LAN bind требует внешнего
  TLS/VPN/reverse-proxy и API key.
- **Legacy**: `docs/history/` — frozen JS OmniRoute material, не история
  Rust-порта; plaintext API-key rows требуют отдельной миграции.
- **ChatHub**: live inference и tools не проверены; anonymous session не
  выдаётся, chat/stream возвращают 403/401. Нужен vendor-authorized API.

## Документы

- `docs/ROADMAP-2026-10.md` — план развития по итогам аудита 2026-10
  (M0…M7, DoD на фазу, stop/go). M1, M2 и M3 выполнены и отмечены в нём.

| Что | Где |
| --- | --- |
| План следующей миссии | `docs/MISSION-2026-10.md` |
| Полный project audit | `docs/research/PROJECT-AUDIT-2026-09.md` |
| Evidence миссии (6 отчётов) | `docs/research/mission-2026-10/` |
| Roadmap и фактический backlog | `docs/ROADMAP-2026-09.md` |
| ADR | `docs/adr/` |
| Changelog | `CHANGELOG.md` |
| ChatHub evidence | `docs/research/CHATHUB-2026-09.md` |
| Frozen upstream history | `docs/history/` |
| Референс OmniRoute | `/Volumes/Netac_SSD/ai-workstation/Archives/OmniRoute/` |

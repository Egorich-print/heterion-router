# AGENTS.md — Heterion Router

> Rust-шлюз к LLM-провайдерам. Sibling of Heterion, successor of OmniRoute-Rust.
> JS здесь нет (только `desktop/ui` на Svelte) — JS-хуков, eslint и prettier
> в репозитории нет осознанно (ADR-008 §8).

## Layout

- `crates/heterion-router-{config,core,crypto,db,gateway,http,providers,routing,translate,usage}` —
  workspace; бинарь `heterion-router-gateway` (`crates/heterion-router-gateway/src/main.rs`).
- `desktop/` — Tauri v2 оболочка (`src-tauri`, исключён из workspace) + Svelte-дашборд (`ui`).
  Тот же бандл, что отдаёт шлюз сам.
- `docs/adr/` — решения (ADR-008 — переименование); ADR-001..007 —
  исторические решения с addenda; `docs/history/` — frozen upstream JS
  OmniRoute material, не история Rust-порта.
- Тестовые снепшоты (`*.snap`) — часть контракта; править только вместе с кодом.
- План следующей миссии — `docs/MISSION-2026-10.md` (ADR-011): objective,
  workstreams, milestones, роли субагентов, тестовые пороги и stop/go.
  Принцип миссии: **сначала красный тест, потом фикс**; mission-правки **не**
  проверяются на живом launchd-инстансе `20128`.

## Gates (перед каждым коммитом)

```sh
cargo fmt --all && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace
```

То же, что гоняет CI, одной командой: `scripts/ci/check.sh`.

Тяжёлое отдельно: `npm --prefix desktop/ui ci/build` и
`cargo tauri build` в `desktop/` (локальный cold build ≈5m15s; bundle
adhoc-signed, не notarized). Workspace fmt не покрывает исключённый
`desktop/src-tauri`; для него нужен отдельный check
(`cargo fmt --manifest-path desktop/src-tauri/Cargo.toml -- --check`; в CI это
job `tauri-fmt`).

Toolchain закреплён `1.99.0` в `rust-toolchain.toml`, `rust-version = "1.99"` в
обоих манифестах, CI использует тот же пин — не `@stable`. Новый крейт обязан
собираться на 1.99 и на `edition = "2024"`.

## Контракты, которые нельзя ломать молча

- `STATIC_SALT = "omniroute-field-encryption-v1"` + префикс `enc:v1:`
  (`heterion-router-crypto`) — живые БД держат значения с этой солью.
- Env: `HETERION_ROUTER_*` первичен, `OMNIROUTE_*` — fallback на один цикл.
  Data dir: `~/.heterion-router` → legacy `~/.omniroute`.
- Порты: сервис `20128`, Tauri sidecar `20129`; Vite dev-proxy по умолчанию
  `20128`, поэтому для dev gateway нужен `VITE_GATEWAY_URL=...:20129`.
- Keyless-бэкенды не шлют `Authorization` (тест `keyless_provider_sends_no_bearer`).
- 4xx вышестоящего провайдера не роняют breaker (не «чинить» это поведение).

## Сервис (macOS)

```sh
launchctl kickstart -k gui/$(id -u)/com.heterion.router   # если job установлен
curl -s http://127.0.0.1:20128/healthz                    # проверка
```

Plist — машинно-локальный (`~/Library/LaunchAgents`), не хранится в репозитории.
Текущая установленная конфигурация слушает `0.0.0.0:20128`; не-loopback bind
без ключа блокирует старт.

Перезапуск из UI — только под launchd (`restart::is_supervised`), иначе 409.

- Атрибуция usage/ call-log реальная: `backend::Attribution` + per-request
  токен (`ATTRIBUTION_TOKEN`). Константа `provider = "routing"` больше не
  пишется — на этом строится per-provider аналитика.
- Ничего, что лежит в БД, не является credential: `api_keys.key` для ключей,
  выпускаемых портом, хранит маркер `hash-only:<id>`, а не дайджест; дайджест в
  `key_hash` нельзя предъявить как bearer. Legacy JS-строки с plaintext в `key`
  продолжают работать.
- Breaker не роняется от 4xx провайдера ни по префиксу `upstream status {code}:`
  (`crates/heterion-router-http/src/client.rs`), ни по статусу, восстановленному
  из тела, пришедшего с HTTP 200 (`lib.rs::upstream_status_or_recovered`).

## Провайдер Heterion

`heterion-local` в `registry.json` (keyless, openai-формат) — это только
code path: в текущей рабочей БД нет активной строки `provider_connections`, и
backend не поднимается. URL по умолчанию `http://127.0.0.1:8080/v1`, override —
`HETERION_LOCAL_BASE_URL`. Model id — транзит на Heterion (там один загруженный
вес). Registry содержит 278 providers; 259 проходят structural builder gate, но
только 253 имеют HTTP(S) base URL, а 6 используют неподдерживаемые схемы.
Детали в `docs/research/PROJECT-AUDIT-2026-09.md`.

## Деньги и безопасность (M3, действует с 2026-10-02)

Порядок слоёв важен и задаётся в `build_router_with_state`: axum выполняет
последний применённый слой первым, поэтому порядок в коде —
cache → budgets → guardrails → cors, а выполнение — cors → guardrails →
budgets → cache. Слой, который блокирует запрос, обязан быть **вне** кэша
(иначе блок можно обойти попаданием) и **вне** бюджета (иначе попадание в кэш
не будет учтено).

- **Бюджеты по умолчанию не заданы** — `domain_budgets` пуст, значит всё
  безлимитно. `daily_limit_usd = 0` означает «безлимит», а не «заблокировать».
- **Кэш разделён по presented bearer** (`sha256(bearer)[:16]`), поэтому один
  ключ не читает ответы, закэшированные другим. Стриминг обходит кэш и на
  чтении, и на записи.
- **Guardrails по умолчанию в режиме `warn`**: без настройки новых отказов нет.
  Совпадение никогда не попадает в тело, заголовок или лог — только имя
  проверки и маска.

## Аудит 2026-10

Шесть read-only-отчётов в `docs/research/audit-2026-10/` (слабые места, паритет
с OmniRoute, паритет с Future-AGI, дубликаты, инвентаризация, Rust 1.99) и
принятые по ним решения — ADR-012. Перед тем как чинить что-то из отчёта,
проверь `file:line`: числа в отчётах — снимок на 2026-10-02.

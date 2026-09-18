# AGENTS.md — Heterion Router

> Rust-шлюз к LLM-провайдерам. Sibling of Heterion, successor of OmniRoute-Rust.
> JS здесь нет (только `desktop/ui` на Svelte) — JS-хуков, eslint и prettier
> в репозитории нет осознанно (ADR-008 §8).

## Layout

- `crates/heterion-router-{config,core,crypto,db,gateway,http,providers,routing,translate,usage}` —
  workspace; бинарь `heterion-router-gateway` (`crates/heterion-router-gateway/src/main.rs`).
- `desktop/` — Tauri v2 оболочка (`src-tauri`, исключён из workspace) + Svelte-дашборд (`ui`).
  Тот же бандл, что отдаёт шлюз сам.
- `docs/adr/` — решения (ADR-008 — переименование); старые ADR-001..007 и
  `docs/history/` — фиксация фактов, не переписывать.
- Тестовые снепшоты (`*.snap`) — часть контракта; править только вместе с кодом.

## Gates (перед каждым коммитом)

```sh
cargo fmt --all && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace
```

Тяжёлое отдельно: `cargo tauri build` в `desktop/` (~3.5 мин),
`npm --prefix desktop/ui install/build` для дашборда.

## Контракты, которые нельзя ломать молча

- `STATIC_SALT = "omniroute-field-encryption-v1"` + префикс `enc:v1:`
  (`heterion-router-crypto`) — живые БД держат значения с этой солью.
- Env: `HETERION_ROUTER_*` первичен, `OMNIROUTE_*` — fallback на один цикл.
  Data dir: `~/.heterion-router` → legacy `~/.omniroute`.
- Порты: сервис `20128`, sidecar/dev `20129`. Label: `com.heterion.router`.
- Keyless-бэкенды не шлют `Authorization` (тест `keyless_provider_sends_no_bearer`).
- 4xx вышестоящего провайдера не роняют breaker (не «чинить» это поведение).

## Сервис (macOS)

```sh
launchctl kickstart -k gui/$(id -u)/com.heterion.router   # рестарт
curl -s http://127.0.0.1:20128/healthz                    # проверка
```

Перезапуск из UI — только под launchd (`restart::is_supervised`), иначе 409.

## Провайдер Heterion

`heterion-local` в `registry.json` (keyless, openai-формат). URL по умолчанию
`http://127.0.0.1:8080/v1`, override — `HETERION_LOCAL_BASE_URL`. Бэкенд
появляется только при активной строке в `provider_connections`. Model id —
транзит на Heterion (там один загруженный вес).

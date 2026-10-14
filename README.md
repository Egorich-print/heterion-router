# Heterion Router

> OpenAI-совместимый шлюз к LLM-провайдерам на Rust: один `baseURL`, один ключ,
> роутинг по комбо, failover, встроенный дашборд, Tauri-десктоп.
> Sibling of [Heterion](https://github.com/Egorich-print/heterion) (там —
> инференс; здесь — маршрутизация). Successor of OmniRoute-Rust.

**Статус:** Active · **Лицензия:** MIT (см. `LICENSE`: сохранён копирайт
исходного upstream `diegosouzapw`).

## Быстрый старт

```sh
cargo run -p heterion-router-gateway        # :20129 по умолчанию
# после npm --prefix desktop/ui run build:
open http://127.0.0.1:20129/                # dev gateway dashboard
```

`cargo run` поднимает dev gateway на `127.0.0.1:20129`. Vite dev-proxy в
`desktop/ui/vite.config.js` по умолчанию указывает на `20128`, поэтому для
одновременного запуска UI и dev gateway нужен явный `VITE_GATEWAY_URL=http://127.0.0.1:20129`.

Установленный launchd-сервис `com.heterion.router` — отдельная машинно-локальная
конфигурация на `20128`; plist не входит в репозиторий. На текущей машине он
слушает `0.0.0.0:20128` (LAN bind), поэтому для публичного доступа нужен
внешний TLS/VPN/reverse-proxy и API key.

```sh
launchctl kickstart -k gui/$(id -u)/com.heterion.router   # если job уже установлен
curl -s http://127.0.0.1:20128/healthz
```

Сборка дашборда и десктопа:

```sh
npm --prefix desktop/ui ci && npm --prefix desktop/ui run build
(cd desktop && cargo tauri build)
```

Локальный Tauri build проверен, но текущий `.app`/`.dmg` adhoc-signed и не
notarized; это developer-machine artifact, не готовый дистрибутив.

## Как это устроено

```
[ клиент ] ──► heterion-router-gateway (axum, :20128 сервис / :20129 dev)
                   ├─ /v1/*      OpenAI-совместимый API (стриминг SSE)
                   ├─ /api/*     admin-API + Svelte-дашборд
                   └─ router ──► transports (openai chat / openai-responses /
                                  gemini / grok-cli / keyless local)
                 │
              SQLite (~/.heterion-router/storage.sqlite)
```

Registry содержит 278 provider entries и 2717 model entries, но это каталог:
в текущем коде структурно проходят builder 259 providers / 2108 models;
из них 253 / 1916 имеют HTTP(S) base URL, а 6 / 192 используют неподдерживаемые
схемы (`wss://`, `stdio`, CLI/ACP). Ещё 19 providers / 609 models не проходят
format/base-URL gate. Список проверенных runtime-транспортов и ограничений — в
`docs/research/PROJECT-AUDIT-2026-09.md`.

- **Комбо** вместо выбора модели: список кандидатов из БД с приоритетами;
  `weight` сохраняется для dashboard, но Rust router использует порядок
  списка (см. `STATUS.md` и `crates/heterion-router-gateway/src/combos.rs`).
- **Heterion (нативно):** провайдер `heterion-local` — keyless
  OpenAI-совместимый бэкенд на Heterion-сервер
  (`http://127.0.0.1:8080/v1`, переопределяется `HETERION_LOCAL_BASE_URL`).
  Подключение:
   ```sql
   INSERT INTO provider_connections (id, provider, is_active, created_at, updated_at)
   VALUES (lower(hex(randomblob(16))), 'heterion-local', 1, datetime('now'), datetime('now'));
   ```
   и комбо с моделью `heterion-local/default` (model id проходит транзитом —
   Heterion отдаёт загруженные веса под любым именем). В текущей рабочей
   БД активного `provider_connections` для этого провайдера нет, поэтому
   backend сейчас не поднимается автоматически.
- **Ключи:** `HETERION_ROUTER_*` первичны, `OMNIROUTE_*` — fallback на один
  цикл (см. `crates/heterion-router-config/src/config.rs`).
- **Заморожено навсегда:** `enc:v1:` + соль `omniroute-field-encryption-v1`
  (в живых БД лежат значения с ней), порты `20128/20129`.
- **Границы доступа:** `/healthz` и статика SPA публичны; `/v1/*` требует
  bearer key при наличии ключей, `/api/*` — `manage` scope. Не выставляйте
  gateway на `0.0.0.0` без TLS/VPN и ключа.

## OmniRoute — референс

JS-реализация заморожена и сюда не входит: архивный репозиторий
[OmniRoute-Rust](https://github.com/Egorich-print/OmniRoute-Rust),
`~/ai-workstation/Archives/OmniRoute/`, upstream
[diegosouzapw/OmniRoute](https://github.com/diegosouzapw/OmniRoute).
Осознанно не портировано: `/api/mcp/*`, OAuth-перелогин провайдеров,
служебные страницы (биллинг, телеграм, геймификация).

## Документы

| Что | Где |
| --- | --- |
| Статус | `STATUS.md` |
| План следующей миссии | `docs/MISSION-2026-10.md` |
| Решения (ADR) | `docs/adr/` |
| Аудит проекта | `docs/research/PROJECT-AUDIT-2026-09.md` |
| Frozen upstream JS history | `docs/history/` (не история Rust-порта) |
| Обзор и связи | `Obsidian Vault/Projects/heterion-router/` |

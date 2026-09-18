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
open http://127.0.0.1:20129/                # дашборд (после сборки UI)
```

Сервис (launchd `com.heterion.router`, порт `20128`):

```sh
launchctl kickstart -k gui/$(id -u)/com.heterion.router
```

Сборка дашборда и десктопа:

```sh
npm --prefix desktop/ui install && npm --prefix desktop/ui run build
(cd desktop && cargo tauri build)
```

## Как это устроено

```
[ клиент ] ──► heterion-router-gateway (axum, :20128 сервис / :20129 dev)
                   ├─ /v1/*      OpenAI-совместимый API (стриминг SSE)
                   ├─ /api/*     admin-API + Svelte-дашборд
                   └─ router ──► провайдеры (openai / gemini / responses /
                                  keyless / grok-cli / heterion-local)
                 │
              SQLite (~/.heterion-router/storage.sqlite)
```

- **Комбо** вместо выбора модели: список кандидатов из БД с приоритетами
  и весами, роутер перебирает их при сбое (см. `docs/adr/ADR-001-*`).
- **Heterion (нативно):** провайдер `heterion-local` — keyless
  OpenAI-совместимый бэкенд на Heterion-сервер
  (`http://127.0.0.1:8080/v1`, переопределяется `HETERION_LOCAL_BASE_URL`).
  Подключение:
  ```sql
  INSERT INTO provider_connections (id, provider, is_active, created_at, updated_at)
  VALUES (lower(hex(randomblob(16))), 'heterion-local', 1, datetime('now'), datetime('now'));
  ```
  и комбо с моделью `heterion-local/default` (model id проходит транзитом —
  Heterion отдаёт загруженные веса под любым именем).
- **Ключи:** `HETERION_ROUTER_*` первичны, `OMNIROUTE_*` — fallback на один
  цикл (см. `crates/heterion-router-config/src/config.rs`).
- **Заморожено навсегда:** `enc:v1:` + соль `omniroute-field-encryption-v1`
  (в живых БД лежат значения с ней), порты `20128/20129`.

## OmniRoute — референс

JS-реализация заморожена и сюда не входит: архивный репозиторий
[OmniRoute-Rust](https://github.com/Egorich-print/OmniRoute-Rust),
`~/ai-workstation/Archives/OmniRoute/`, upstream
[diegosouzapw/OmniRoute](https://github.com/diegosouzapw/OmniRoute).
Осознанно не портировано: `/api/mcp/*`, OAuth-перелогин провайдеров,
служебные страницы (биллинг, телеграм, геймификация).

## Документы

| Что            | Где                                    |
| -------------- | -------------------------------------- |
| Статус         | `STATUS.md`                            |
| Решения (ADR)  | `docs/adr/` (переименование — ADR-008) |
| История работ  | `docs/history/`                        |
| Обзор и связи  | `Obsidian Vault/Projects/heterion-router/` |

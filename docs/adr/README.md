# ADR — Heterion Router (ранее OmniRoute-Rust, см. 008)

Решения, определившие архитектуру Rust-версии. Формат: контекст → решение →
следствия. Один файл — одно решение. ADR-001..007 — исторические решения с
фактическими addenda; `docs/history/` — frozen upstream JS material, не
история Rust-порта.

Дата старта: 2026-09-18.

| #                                               | Решение                                                       |
| ----------------------------------------------- | ------------------------------------------------------------- |
| [001](ADR-001-shared-sqlite-boundary.md)        | Граница интеграции JS↔Rust — общая схема SQLite               |
| [002](ADR-002-gateway-serves-dashboard.md)      | Дашборд раздаёт сам шлюз                                      |
| [003](ADR-003-vite-proxy-over-dev-server.md)    | Dev-режим UI: Vite-proxy, без второго бэкенда                 |
| [004](ADR-004-admin-api-mutations.md)           | Мутации в admin-API — только то, что шлюз реально обслуживает |
| [005](ADR-005-api-keys-rust-issued.md)          | API-ключи выпускаются в Rust в JS-форме                       |
| [006](ADR-006-supervised-restart-only.md)       | Рестарт — только под супервизором                             |
| [007](ADR-007-desktop-sidecar-shared-bundle.md) | Десктоп — sidecar + тот же бандл                              |
| [008](ADR-008-rename-to-heterion-router.md)     | Переименование в Heterion Router, новый репозиторий           |
| [009](ADR-009-runtime-security-boundaries.md)   | Capability routing, credential rotation и security boundaries |
| [010](ADR-010-chathub-upstream-boundary.md)     | ChatHub не подключается как undocumented upstream            |
| [011](ADR-011-mission-2026-10-scope.md)         | Границы MISSION-2026-10 и принцип «схема ≠ enforcement»      |
| [012](ADR-012-rust-199-and-audit-hardening.md)| Rust 1.99 / edition 2024 и hardening по аудиту 2026-10     |

## Отклонённые / отложенные

- **MCP-сервер** (`/api/mcp/*`) — не портирован. На дату миграции единственный
  потребитель в `~/.config/opencode/opencode.json` был заглушкой с
  `REPLACE_WITH_YOUR_TOKEN`, т.е. мёртвым. При необходимости поднимается
  переустановкой pinned JS-версии (см. `Archives/OmniRoute/`).
- **OAuth-перелогин провайдеров** — автоматический relogin UI ещё не портирован;
  Grok refresh-token rotation работает через runtime OAuth client configuration,
  а expired/legacy credentials требуют явного обновления.
- **Полный Responses/SDK contract** — state store и replay реализованы, но
  streaming event/tool dialect, reasoning retention и provider pinning ещё
  частичные.
- **Launchd/desktop distribution** — plist, sidecar readiness, restart и
  notarized bundle не являются воспроизводимым release-потоком. Исправляется в
  MISSION-2026-10 (WS-F); notarization остаётся за пределами (ADR-011 §2).
- **Next mission plan** — `docs/MISSION-2026-10.md`.
- **Project audit** — `docs/research/PROJECT-AUDIT-2026-09.md`.
- **Audit 2026-10** (слабые места, паритет с OmniRoute и Future-AGI,
  дубликаты, инвентаризация, Rust 1.99) — `docs/research/audit-2026-10/`.

# ADR-007. Десктоп — sidecar + тот же бандл

> Дата: 2026-09-18 · Статус: accepted

## Контекст

Tauri-оболочка создавалась как замена Electron. Варианты: держать отдельный UI
для десктопа; требовать внешний шлюз; встроить шлюз в само приложение.

## Решение

Приложение **самодостаточно**: Tauri v2 спавнит свой бинарник
`omniroute-gateway` как sidecar (externalBin) на порту шлюза по умолчанию
(`:20129`, в launchd-сервисе переопределён на `:20128`, так что коллизий нет).
Перед исполнением бандла в окно инжектируется `window.__OMNIROUTE_GATEWAY_URL__`,
и `apiBase()` (см. ADR-002) использует этот адрес. UI — **тот же самый бандл**,
что раздаёт шлюз, поэтому интерфейсы веба и десктопа идентичны.

Sidecar живёт и умирает вместе с приложением; его вывод идёт в лог приложения.
В dev sidecar не поднимается — Vite-proxy уже указывает на шлюз.

## Следствия

- `tauri build` самодостаточен: `beforeBuildCommand` собирает UI и
  стейджит sidecar (`desktop/scripts/stage-sidecar.sh`).
- Один UI, одна кодовая база, два способа доставки.
- Второй инстанс приложения упирается в занятый порт — известное ограничение
  (кандидат на single-instance guard).

## Addendum 2026-09-29

- Sidecar binary: `heterion-router-gateway` (не `omniroute-gateway`).
- Injected global: `window.__HETERION_ROUTER_GATEWAY_URL__`;
  `__OMNIROUTE_GATEWAY_URL__` читается только как legacy fallback.
- Single-instance guard реализован первым Tauri plugin; прежнее ограничение
  снято.
- Sidecar по-прежнему не имеет readiness handshake/respawn; bundle
  (`cargo tauri build`) проверен локально, но adhoc-signed и не notarized.
  Детали и port-mismatch с Vite/API fallback: `docs/research/PROJECT-AUDIT-2026-09.md`.

# ADR-008: Переименование в Heterion Router, новый репозиторий (2026-09-19)

- Статус: accepted (2026-09-19)
- Контекст: Rust-порт (`crates/omniroute-*`, дашборд, Tauri) перерос исходный
  проект: OmniRoute JS остаётся замороженным референсом, а Rust-шлюз живёт
  своей жизнью рядом с Heterion. Имя OmniRoute больше не описывает проект.

## Проверка имени (2026-09-19)

| Проверка | Результат |
|---|---|
| GitHub `Egorich-print/heterion-router` | свободен, похожих репозиториев нет |
| crates.io `heterion-router`, `-gateway`, `-core`, `-config` | 404 (свободны) |
| npm `heterion-router-desktop`, `heterion-router-desktop-ui` | E404 (свободны) |
| Товарный знак | **gate публичного запуска**: отдельный TM-поиск; при живых правах имя пересматривается |

## Решение

1. **Имя: Heterion Router.** Репозиторий `Egorich-print/heterion-router`
   (public, как предшественник), каталог `~/ai-workstation/Projects/heterion-router`.
2. **Новый репозиторий, не fork.** История Rust-поверхности перенесена
   `git filter-repo` (keep: `crates/`, `desktop/`, `Cargo.*`,
   `rust-toolchain.toml`, `docs/adr/`, `docs/history/`, `STATUS.md`,
   `LICENSE`, `THIRD_PARTY_NOTICES.md`, `.github/workflows/rust.yml`);
   JS-дерево не переносилось. Связи с `diegosouzapw/OmniRoute` нет.
3. **Отдельная развёртка, рядом с Heterion.** «Нативная поддержка» v1 —
   провайдер `heterion-local`: keyless OpenAI-совместимый бэкенд на
   `http://127.0.0.1:8080/v1` (переопределяется `HETERION_LOCAL_BASE_URL`),
   model id проходит транзитом. Общие DB-схемы — follow-up, не здесь.
4. **Лицензия MIT**, с сохранением `Copyright (c) 2026 diegosouzapw` +
   `Copyright (c) 2026 Egorich-print` (требование MIT при копировании).
5. **OmniRoute — замороженный референс**, не вендорится: репозиторий
   OmniRoute-Rust (архив), `~/ai-workstation/Archives/OmniRoute/`,
   upstream `diegosouzapw/OmniRoute`.
6. **Замороженные interop-константы** (переименование их ломает молча):
   - `STATIC_SALT = "omniroute-field-encryption-v1"` + префикс `enc:v1:`
     (в живых БД лежат значения, выведенные с этой солью);
   - `OMNIROUTE_*` env — fallback на один цикл (`HETERION_ROUTER_*`
     первичен);
   - `~/.omniroute` — legacy-ветка резолва data dir;
   - порты `20128/20129` не меняются.
7. **Миграция по схеме nyrix ADR-008**: Stage 1 идентичность (этот ADR,
   Cargo, десктоп) → Stage 2 слои Knowledge → Stage 3 cutover рантайма
   (копия данных, label `com.heterion.router`, те же порты) → Stage 4
   GitHub-финал и CI (gateway-артефакты; десктоп-бандл остаётся локальным).
8. **JS-гейты (husky/prettier/eslint) не переносятся.** Причина: каждый
   коммит требовал бы 3.8 ГБ `node_modules`; в новом репо JS — только
   `desktop/ui` (Svelte), его собирает CI точечно.

## Последствия

- Крейты `heterion-router-*`; бинарь `heterion-router-gateway`; Tauri
  `com.heterion.router`; data dir `~/.heterion-router`.
- Переименование крейтов — отдельным механическим коммитом; env-совместимость
  и `heterion-local` — отдельными содержательными.
- Старые ADR-001..007 и `docs/history/` не переписываются (фиксация фактов).

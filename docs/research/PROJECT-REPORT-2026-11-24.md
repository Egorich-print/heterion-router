# Heterion Router — состояние проекта (2026-11-24)

Срез: 2026-11-24, коммит `2dfc79d`. Живая БД, launchd-инстанс и внешние
площадки не опрашивались (раздел 6). В таблицах `lib.rs` =
`crates/heterion-router-gateway/src/lib.rs`.

## Оглавление

1. [Контекст сессии](#1-контекст-сессии)
2. [Сводка состояния](#2-сводка-состояния)
3. [Найденные факты](#3-найденные-факты)
4. [Проблемы](#4-проблемы)
5. [Варианты решения](#5-варианты-решения)
6. [Приложение](#6-приложение)

## 1. Контекст сессии

Локальная `main` опережала `origin/main` на 59 коммитов. В `origin/main` лежали
два CI-коммита владельца — `d7846ce`, `5fd786a` (автор 3g0r1ch, 16.11.2026),
сделанные на старом дереве `13c4104` и никогда не смерженные. Слияние `2dfc79d`
(родители `68c4c93` + `5fd786a`) прошло без конфликтов: `.github/workflows/release.yml`
удалён (131 строка), `Jenkinsfile` переписан — 65 вставок / 167 удалений.

**Push выполнен**: `git push origin main` → `5fd786a..2dfc79d main -> main`,
ветки синхронизированы (`main...origin/main` — расхождений нет).

Гейт прогнан локально перед пушем — **зелёный**, код выхода 0:
`cargo fmt --all -- --check` (ок), `cargo clippy --workspace --all-targets -- -D warnings`
(1 мин 33 с, без предупреждений), `cargo test --workspace` (~23 мин, doctests —
все `ok`). Тяжёлое отдельно (`npm --prefix desktop/ui build`, сборка `desktop/gpui`)
в этой сессии не гонялось — см. раздел 6.

Релиз ведёт Jenkins (LXC 120 на pve-xeon). GitHub Actions отключены — биллинг не
активен, последний прогон 11.11.2026. Гейт репозитория — `scripts/ci/check.sh`.

## 2. Сводка состояния

| Зона | Вердикт | Цифры |
|---|---|---|
| Реестр и провайдеры | работает; враньё в документации | 275 провайдеров / 2373 модели; 46 served, 182 needs_key, 14 dead_endpoint |
| Маршрутизация и деградация | пороги зафиксированы; 429-баланс трактуется как квота | кулдаун 30→60→120 с; breaker 3 подряд / 60 с; фриз баланса 1 ч только на 402/403 |
| Responses-контракт | частичный, fail-closed не доведён | reasoning-items теряются; в мосте chat→Responses нет `output_item.done`/`response.created`; storage key = upstream id |
| GPUI-десктоп | впереди веб-дашборда, но вне CI | 8 экранов против 7; 82 теста не идут ни в одном контуре |
| CI и релиз | единственный живой контур — Jenkins; macOS-артефакт не собирает никто | `REPO_HAS_RELEASE='no'`; `REPO_RELEASE_HOOK` пуст; тулчейн 1.99.0 против пина 1.99.1 |
| Документация | врёт в числах, ссылках и статусах | 11 пунктов в §3.5; индекс ADR обрывается на ADR-024 |
| Тестовое покрытие | 1049 атрибутов в workspace, 82 — вне гейта | 781 unit + 268 интеграционных; `provider_sync.rs` (451 строка) — без тестов |

## 3. Найденные факты

### 3.1 CI и релиз

| Факт | Доказательство |
|---|---|
| GH Actions не запускаются с 11.11.2026 ⇒ 59+ коммитов без автопрогона; последний прогон: `ci` — success, `release` — падение за 0 с | журнал Actions (данные владельца) |
| Единственный живой CI — Jenkins: находит родной гейт и зовёт его под `flock --close` | `Jenkinsfile:135-141`, `:188` |
| Контейнер Jenkins — Linux; `codesign`/`plutil` есть только в `desktop/scripts/build-app.sh`, `xcrun metal` нужен gpui ⇒ **macOS `.app` не собирает никто**; `scripts/release.sh` нет ⇒ `REPO_RELEASE_HOOK` пуст | `Jenkinsfile:158`, `desktop/scripts/build-app.sh:139-159` |
| `REPO_HAS_RELEASE` = `'no'` после удаления `release.yml` ⇒ релиз по тегу `v*` публикует Jenkins | `Jenkinsfile:151-153` |
| **Баг:** `sha256sum * > SHA256SUMS` хеширует сам себя, если манифест уже лежит в `dist` (glob раскрывается до создания файла, `dist` наполняется из постоянного архива) ⇒ `sha256sum -c` даёт `SHA256SUMS: FAILED` | `Jenkinsfile:342`, `:340` |
| **Баг:** `gh release create` не идемпотентен — повторный прогон тега падает «release already exists» | `Jenkinsfile:356` |
| В контейнере объявлены тулчейны «1.99.0 / 1.98.0 / stable», а `rust-toolchain.toml` требует **1.99.1** ⇒ rustup доустанавливает тулчейн на лету (нужна сеть) либо сборка падает | `Jenkinsfile:83` |
| `GH_REPO` ломается на SSH-URL (сплит по `/` → `owner:repo`); имя аккаунта вшито | `Jenkinsfile:131`, `:128` |

### 3.2 Тесты и покрытие

| Факт | Доказательство |
|---|---|
| 1049 тест-атрибутов: 781 unit + 268 интеграционных; в `gateway/tests/` — 33 файла | подсчёт по `crates/*/src` и `crates/*/tests` |
| **82 теста `desktop/gpui` (76 `#[test]` + 6 `#[gpui::test]`) не выполняются ни в одном CI**: крейт вне workspace, гейт делает для него только fmt | `Cargo.toml:23-24`, `scripts/ci/check.sh:27`, `.github/workflows/ci.yml:49-63` |
| Файл `provider_sync.rs` — 451 строка, функция `sync_all` — `:386-451` (66 строк), **ни одного теста**; `repos::list_connections(&guard).unwrap_or_default()` превращает ошибку БД в «нет соединений» — sweep тихо ничего не измеряет | `provider_sync.rs:386`, `:394` |
| Конкурентная запись в БД (`buffer_unordered(8)`) при съедаемой ошибке ⇒ каталог молча устаревает | `provider_sync.rs:443` |

### 3.3 Код: риски

| Факт | Доказательство |
|---|---|
| `getrandom::fill(&mut iv).expect("os random source")` на пути записи секрета в БД: отказ ГСЧ = паника вместо 500 | `crypto/src/lib.rs:86` |
| `.env` усекается до одной строки при **ошибке чтения** (не только отсутствии файла), затирая чужие настройки, хотя док-комментарий обещает «extended, never rewritten» | `main.rs:902`, `:908`, `:890-891` |
| `Command::new("security").output()` — блокирующий подпроцесс без таймаута на старте шлюза, может висеть на GUI-диалоге разблокировки связки ключей; `read_client_secret` синхронно сканирует бинарь IDE | `antigravity.rs:229`, `main.rs:253`, `antigravity.rs:283` |
| `from_utf8_unchecked` без комментария `// SAFETY:` — единственный такой в проде (в строках 233 и 238 комментарий есть) | `context_pruner.rs:135` |
| `include_str!("../registry.json")` + `expect`: битый registry.json = шлюз не стартует вовсе | `registry.rs:74` |
| `#[derive(Debug)]` на структуре с plaintext-токеном. Уточнение: у `FieldCrypto` Debug ручной и редактирующий — правило «redacting Debug» не применено именно к `Credential` | `credentials.rs:14`, `:21`, `crypto/src/lib.rs:38-42` |
| `RwLock`/`Mutex` в async-контексте; `.await` под guard'ом сейчас нигде нет — риск отложенный | `main.rs:335`, `routing.rs:310`, `resilience.rs:62`, `gemini.rs:69` |
| `todo!`/`unimplemented!` в проде — **0**; продуктовых `.unwrap()`/`.expect()` — 15, все под гарантированным инвариантом, в тестовых модулях — 1386. Паник в проде GPUI нет: все 37 `.expect()` лежат в `#[cfg(test)]` | замер по `crates/*/src` + `desktop/gpui/src` |

### 3.4 Реестр и провайдеры

| Факт | Доказательство |
|---|---|
| **275 провайдеров / 2373 модели**. Форматы: openai 252, claude 9, openai-responses 6, gemini 3, antigravity 2, clova 1, custom 1, kiro 1. Auth: apikey 225, oauth 20, none 19, optional 10, cookie 1. `free_only: true` — ровно один (`opencode-free`) | `crates/heterion-router-providers/registry.json` |
| Живая классификация площадок: 46 served, 182 needs_key, 14 dead_endpoint, 14 not_a_chat_api, 5 credits_required, 6 not_served | `docs/research/provider-matrix-2026-11-16.md:5-15` |
| Выключены: `agentrouter` — 403 insufficient_user_quota («user quota: $0.004456, need quota: $0.032528», 17.11); `sambanova` (5 ключей) — 401; `blackbox`, `command-code` — 404; `kimi-coding` — 401, OAuth-токен истёк 17.09; `grok-cli` — 400 client_id is required; `opencode-go` — «Invalid credential»; `minimax` — 402; `ollama-cloud-main` — 402 | `AGENTS.md:539-545`, `:681`, `:699`, `:738`, `:752` |
| **Пробел в правилах:** `zai` отдаёт 429 «Insufficient balance or no resource package» (баланс, не квота), а любой 429 трактуется как модельный кулдаун ⇒ площадка остаётся в ротации и платит отказом каждый запрос. Правила «баланс → провайдерский фриз» нет; есть только 402/403 → фриз на час | `docs/research/COMBO-REBUILD-2026-11-05.md:88`, `routing.rs:2301`, `:2592`, `resilience.rs:188` |
| `sealion` — измерен 400 ContextWindowExceededError при `max_completion_tokens=32000` и промпте 110 761 символ. Потолок **вывода** есть (32 000), потолок **входа** (`max_payload_chars`) — только у oneminai | `AGENTS.md:867-868`, `provider_caps.rs:112`, `:149-153` |
| `max_output_tokens` сравнивает id модели **регистрозависимо** (`"SEA-LION-v4-32B"`), а `allowed_efforts` — нормализованно ⇒ клиент с другим регистром молча теряет потолок 32 000 и получает 400 площадки | `provider_caps.rs:102`, `:112`, `:34` |
| Пороги деградации: 429-модельный кулдаун 30→60→120 с; breaker — ключ **провайдер**, 3 подряд / 60 с, half-open — один пробник; пробация качества — дефект −0.3, успех +0.05, 2 дефекта за 15 мин; мягкий cooldown ×0.1 на 120 с; 402/403 → фриз баланса 1 ч | `resilience.rs:54-58,127`, `breaker.rs:121-164`, `routing.rs:933-935`, `resilience.rs:660-665`, `routing.rs:1748,180`, `resilience.rs:188` |
| Breaker по провайдеру — отдельная проблема: у площадки с одним соединением (например keyless-пул `pollinations`, или анонимный `opencode-free` с `auth_type: optional`) три 5xx глушат весь пул | `breaker.rs:121-164` |
| `DEFAULT_AUTO_CANDIDATES` ставит в Tier 1 `agentrouter/gpt-5.6-sol` — площадку с нулевым балансом и выключенным соединением | `routing.rs:105`, `:112-115` |

### 3.5 Документация врёт

| Факт | Доказательство |
|---|---|
| `docs/PROVIDERS.md`: «280 записей / 2741 модель» (`:12,139,143,144`) против 275/2373; «221 / 1993» (`:149`) против 220/2042 по gate `admin::is_key_provider`; keyless 20 (`:150`) против 29; из 9 не прошедших gate `cursor`, `cursor-api`, `deepai` в реестре отсутствуют (`:170-174`); `:219` упоминает вычеркнутый `ollama-cloud`. Верны «7 / 302» (`:147`) и «9 не проходят» (`:148`) | `docs/PROVIDERS.md`, `admin.rs:791-807` |
| Док-комментарий «JavaScript REGISTRY (280 providers)» | `crates/heterion-router-providers/src/lib.rs:3` |
| `STATUS.md` — снимок 2026-11-09: 280/2741 (`:53`), «221 запись» (`:56`), Tauri v2 sidecar (`:76,78,91,189`), toolchain 1.99.0 (`:143,149`), `release.yml` (`:151`), `stage-sidecar.sh` (`:155`); трекер миссии M0–M3 весь «⬜ not started» (`:110-117`) | `STATUS.md` |
| Секция статуса миссии — пустой шаблон со всеми ⬜ при 110+ коммитах после baseline | `docs/MISSION-2026-10.md:564-577` |
| Индекс ADR обрывается на ADR-024, хотя ADR-025…032 лежат на диске и на них ссылается AGENTS.md | `docs/adr/README.md:35` |
| Описывает macOS-джобы Actions (файла нет); «окно 128 слов» у repetition guard (в коде `WINDOW_WORDS = 256`); «274 providers / 2363 models» | `AGENTS.md:51-59`, `:90`, `repetition_guard.rs:30`, `AGENTS.md:474` |
| Toolchain 1.99.0 (`README.md:81` уже верен); индекс исследований знает три отчёта, ноябрьские не внесены | `docs/INSTALL.md:21`, `docs/README.md:21-25` |
| Комментарий ссылается на несуществующий `release.yml`; подсказка «start scripts/webdriver/server.py» — каталога `scripts/webdriver/` в репозитории нет | `.github/workflows/ci.yml:5`, `main.rs:702` |
| Правило `*.sql`: новая SQL-схема молча не добавится в индекс | `.gitignore:34` |
| Примеры `ollama-cloud/minimax-m3`, `zai/glm-5.3`: `ollama-cloud` отсутствует, у `zai` только `glm-4.7-flash` | `docs/AGENT-ROUTER-GUIDE.md:45-47` |
| Research-отчёты 05–08.11 оперируют провайдерами, которых в реестре уже нет (`aihubmix`, `crouton-alpha-free`, `ollama-cloud/*`) — устарели по существу, а не только по числам | `docs/research/COMBO-REBUILD-2026-11-05.md` и соседние |

### 3.6 GPUI-десктоп

| Факт | Доказательство |
|---|---|
| Не отстаёт от Svelte, а **впереди**: 8 экранов против 7. Экран Auto: `GET /api/routing/plan?model=auto`, квота `/api/providers/quota`, каталог `/api/providers/catalog`, живой опрос 2/5/15 с; в Providers — форма `POST /api/connections`; в Usage — p50/p95/p99; экран входа создаёт первый ключ | `desktop/gpui/src/views/` (8) против `desktop/ui/src/views/` (7), `api.rs:646`, `views/auto.rs:10-16,40`, `views/providers.rs:1136`, `views/usage.rs:215-217`, `app.rs:215,248` |
| Эндпоинты шлюза, не выведенные ни в один фронтенд: `/api/providers/sync`, `/api/providers/{id}/sync`, `/api/budgets`, `/api/budgets/{api_key_id}`, `/api/guardrails` | `lib.rs:424-441` |
| Не покрыты тестами: `views/auto.rs` (936 стр.), `views/combos.rs` (1073), `widgets/controls.rs`, `widgets/chart.rs`, `app.rs` | `wc -l` |
| Дрейф контракта: локальный `struct ConnectionUpdate` вместо типа в `api.rs` (в комментарии признано: «`api.rs` carries no type for it») и локальный `HealthProbe` ради поля `started_unix`, которого нет в `api::Health` | `views/providers.rs:182-193`, `:194-206` |
| `#![allow(dead_code)]` на уровне модуля; документация ссылается на `theme::unique_id`, которого в коде нет (есть `theme::element_id`) ⇒ агент по документации напишет некомпилируемый код | `api.rs:3`, `theme.rs:3`, `theme.rs:116`, `docs/port/GPUI-API-NOTES.md:286,300,389`, `desktop/gpui/README.md:45` |

### 3.7 Миссия и roadmap

| Факт | Доказательство |
|---|---|
| Objective: «честный шлюз под агентным циклом и воспроизводимый как артефакт». Закрыто: B1, B3, B7, C6, D2, D3, D4, D5, E3, E4, E6, E7, E9, F8, F10, H3, H4. Не закрыто: B2, B4, C1, C4, C5, D6–D10, E1, E2, E5, E8, F1–F3, F6, G2–G12, H1; G1 — частично (42 вызова `.verify()` против 71 wiremock-`.expect(...)`: ассерты не все верифицируются) | `docs/MISSION-2026-10.md:180-281` (план и DoD) |
| Дыры: **B2** — `reasoning`-items выбрасываются молча, кода `unsupported_capability` нет; **B4** — `output_item.added` с `call_id:""`, в мосте chat→Responses нет `output_item.done`/`response.created` (в отдельном Responses-потоке `grok_cli.rs:710` событие `response.output_item.done` есть); **C1** — ключ storage = upstream-id ⇒ провайдер, вернувший чужой id, перезаписывает чужую запись; **D10** — в `usage_history.status` живут два словаря; **E2** — `PATCH /api/keys/{id}` отсутствует; **E5** — 0600 не гарантирован для `gateway.log` и signature-файла | `lib.rs:2367-2369`, `:2644-2651`, `:2490-2493`, `:946`, `docs/MISSION-2026-10.md:93`, `lib.rs:430-432`, `main.rs:941`, `gemini.rs:117`, `grok_cli.rs:710` |
| Статусы пунктов ниже — сверка с кодом, а не выписка из документа: в самом `MISSION-2026-10.md` отметок выполнения нет, статус делегирован ADR-012 | `docs/MISSION-2026-10.md:180-281` (план и DoD), `:53` (отсылка к ADR-012) |
| Остаток ≈ **15–22 человеко-дня** против заявленной полосы миссии 20.5–32 дня; план писался до перевода десктопа на GPUI: **F8** и **F9** ссылаются на `desktop/src-tauri`, F2/F3/F7 — на `tauri dev` и `tauri.conf.json`, F12 — на `stage-sidecar.sh` (всё это удалено) | `docs/MISSION-2026-10.md:294`, `:239-251` |

## 4. Проблемы

| № | Проблема | Влияние | Где доказано | Серьёзность |
|---|---|---|---|---|
| 1 | macOS-артефакт не собирает никто: контейнер Jenkins — Linux, `scripts/release.sh` отсутствует | релиз по тегу публикует только Linux-бинари | `Jenkinsfile:158`, `desktop/scripts/build-app.sh:139-159` | высокая |
| 2 | Релиз не идемпотентен, `SHA256SUMS` может хешировать сам себя | повторный прогон тега падает; манифест не проходит `sha256sum -c` | `Jenkinsfile:342,356` | высокая |
| 3 | Тулчейн 1.99.1 не заявлен в контейнере (там 1.99.0/1.98.0/stable) | rustup доустанавливает тулчейн на лету (нужна сеть) либо сборка падает | `Jenkinsfile:83` | высокая |
| 4 | 82 теста `desktop/gpui` не идут ни в одном CI | нативный слой (8 экранов) не защищён гейтом | `Cargo.toml:23-24`, `scripts/ci/check.sh:27` | высокая |
| 5 | 429 «Insufficient balance» трактуется как квота модели | площадка без денег остаётся в ротации и платит отказом каждый запрос | `routing.rs:2301,2592` | высокая |
| 6 | Responses: reasoning-items теряются, SSE-lifecycle неполон, storage key = upstream id | тихая потеря ввода; чужой id перезаписывает чужую запись | `lib.rs:2367-2369,2490-2493,2644-2651` | высокая |
| 7 | `sync_all()` без тестов, ошибка БД съедается | каталог молча устаревает, sweep ничего не измеряет | `provider_sync.rs:386,394,443` | средняя |
| 8 | `.env` усекается при ошибке чтения; `getrandom` через `expect` | затираются чужие настройки; отказ ГСЧ = паника | `main.rs:902,908`, `crypto/src/lib.rs:86` | средняя |
| 9 | `Credential` печатается через derived `Debug` | plaintext-токен утекает в лог | `credentials.rs:14,21` | средняя |
| 10 | `security` вызывается без таймаута на старте | старт шлюза может висеть на GUI-диалоге | `antigravity.rs:229`, `main.rs:253` | средняя |
| 11 | Документация врёт в числах, ссылках, статусах | агент по документации пишет неверный код (несуществующий `theme::unique_id`) | §3.5 | средняя |
| 12 | Breaker по провайдеру; `DEFAULT_AUTO_CANDIDATES` с мёртвой площадкой | три 5xx глушат keyless-пул; Tier 1 тратит запрос на заведомый отказ | `breaker.rs:121-164`, `routing.rs:105` | средняя |

### Сквозные проблемы

1. **Проверяемость.** Единственный живой контур — Jenkins; он зовёт родной гейт, но
   гейт не покрывает `desktop/gpui`, а release-шаг не воспроизводим на macOS.
   Actions отключены, `release.yml` и `stage-sidecar.sh` удалены.
2. **Правда в числах.** Реестр, снимки (`STATUS.md`, `PROVIDERS.md`), ADR-индекс и
   трекеры миссии разошлись с кодом; документу нельзя верить без сверки с
   `registry.json` и исходниками.
3. **Тихая потеря данных.** `reasoning`-items, ошибки БД в `sync_all()`, два словаря
   `usage_history.status`, `call_id:""` в SSE — не падают, а портят результат
   незаметно.
4. **Границы ответственности не заданы правилом.** Breaker по провайдеру,
   429-как-квота, ключ storage от upstream — три места, где поведение выбрано по
   умолчанию, а не по контракту площадки.

## 5. Варианты решения

### Горизонт A — «за час» (без новых зависимостей, без риска)

| Что | Эффект | Трудоёмкость | Риск |
|---|---|---|---|
| Вернуть `desktop/gpui` в живой контур: `cargo test --manifest-path desktop/gpui/Cargo.toml` в `scripts/ci/check.sh` либо джоб в Jenkinsfile | 82 теста защищают нативный слой | 15–30 мин | низкий; проверить: gpui на Linux требует Metal-шейдеров при сборке — нужен `cargo test --no-run` или тесты без окна |
| Починить `SHA256SUMS` (исключить манифест из glob) и `gh release create` (`gh release view` → `gh release upload --clobber`) | релиз не падает при повторе; манифест проходит проверку | 10–20 мин | низкий |
| `// SAFETY:` в `context_pruner.rs:135`; `getrandom` без `expect` (ошибка → 500) | два паник-риска сняты | 20–30 мин | низкий |
| `.env` без усечения: различать «файла нет» и «ошибка чтения» | чужие настройки не затираются | 10 мин | низкий |
| Скрипт сверки чисел реестра (`jq` по `registry.json` → таблица в `docs/PROVIDERS.md`) + проверка в CI | числа перестают расходиться | 30–40 мин | низкий |
| Дописать ADR-025…032 в `docs/adr/README.md`; починить `docs/README.md`, `docs/INSTALL.md:21` | индекс и ссылки перестают врать | 20 мин | нет |
| Поправить `AGENTS.md` §Gates (`:51-59`): macOS-джобы Actions и «ловушка YAML с `env:`-комментарием» описывают **удалённый** `release.yml`; живой контур — Jenkins, релиз по тегу `v*` | правило перестаёт уводить агента в несуществующий workflow | 15 мин | нет |

### Горизонт B — «за день-два»

| Что | Эффект | Трудоёмкость | Риск |
|---|---|---|---|
| `scripts/release.sh`: сборка `.app`, проверка arm64, `ditto`, `shasum -a 256`, `gh release upload --clobber` | Jenkins публикует macOS-артефакт; `REPO_RELEASE_HOOK` не пуст | 1 день | средний: нужен macOS-раннер; `build-app.sh` требует Xcode и `xcrun metal` |
| Поставить тулчейн 1.99.1 в контейнер Jenkins (или убрать ложное утверждение из `Jenkinsfile:83`) | сборка не зависит от догрузки rustup | 30 мин | низкий |
| Тест на `sync_all()`: пустая БД / ошибка соединения → явный признак | каталог не устаревает молча | 2–3 ч | низкий |
| HTTP-тест «oversize → 400 `request too large for provider/model`, а не 404» | контракт зафиксирован | 2 ч | низкий |
| Обновить `STATUS.md` и трекеры миссии (`docs/MISSION-2026-10.md:564-577`) | трекеры перестают врать | 2–3 ч | нет |

### Горизонт C — «на неделю и дальше»

| Что | Эффект | Трудоёмкость | Риск |
|---|---|---|---|
| B2/B4 — fail-closed Responses: сохранять `reasoning` байт-в-байт либо `400 unsupported_capability`; типизированный SSE-lifecycle + валидатор | тихая потеря ввода становится явной ошибкой | 3–5 дней | средний: B4 переписывает публичную форму SSE (отдельный PR) |
| C1 — storage key, minted шлюзом (`resp_<uuid>`), upstream-id — поле внутри записи | чужой id не перезаписывает чужую запись | 1–2 дня | средний: миграция записей + read-fallback |
| D10 — единый словарь `usage_history.status` (миграция 328 строк) | запросы по журналу перестают быть слепыми | 1–2 дня | средний: затрагивает дашборд и рассылки |
| E1/E2/E5 — fail-closed по `NULL scopes`, `PATCH /api/keys/{id}`, явный `0600` для log/signature | граница данных описана и enforce'ится | 2–3 дня | низкий-средний: E1 требует решения владельца (ADR) |
| Правило «баланс → провайдерский фриз» (по тексту отказа, а не по коду 429) | площадка без денег уходит из ротации | 1 день | низкий: нужно отличать баланс от квоты по тексту |
| Инвалидация capability-таблицы по дате замера; регенерация счётчиков реестра в CI | «измеренные» правила не живут вечно | 1–2 дня | низкий |
| Breaker: ключ по (провайдер, соединение) либо отдельный лимит для keyless-пулов | три 5xx у одного пула не глушат весь пул | 1 день | средний: меняет семантику `breaker.rs` |

### Рекомендация

Первым — **горизонт A целиком**: он не меняет контракты, не тянет зависимости и
закрывает два риска, которые сегодня тихо ломают релиз (неидемпотентный
`gh release create`, самоподсчёт `SHA256SUMS`), и один, который тихо ломает
проверяемость (82 теста GPUI вне гейта). Внутри него приоритет у пункта про
десктоп: пока `desktop/gpui` не в гейте, правки в нём не защищены.

Вторым — B-часть горизонта B (тест на `sync_all()`, HTTP-тест «oversize → 400»,
сверка чисел реестра, актуализация `STATUS.md` и трекеров): дёшево и убирает ложь
из документации, на которую дальше ссылаются агенты.

Горизонт C — отдельно и по пунктам: B2/B4 и C1 трогают публичный контракт, их
делают после того, как гейт реально зелёный, иначе неисправленный контракт
Responses нечем будет ловить. Оценка остатка — 15–22 человеко-дня при заявленной
полосе миссии 20.5–32 дня, то есть это не «хвост», а основной объём работы.

## 6. Приложение

### Команда → что проверяет

| Команда | Что проверяет |
|---|---|
| `git ls-remote origin refs/heads/main` | удалённая ветка (после push: `2dfc79d`) |
| `git rev-list --left-right --count origin/main...main` | расхождение веток (после push: `0 0`) |
| `git show --stat 2dfc79d` | состав слияния: `release.yml` удалён, `Jenkinsfile` переписан |
| `jq 'length' crates/heterion-router-providers/registry.json` | число провайдеров (275) |
| `jq '[.[].models\|length]\|add' …/registry.json` | число моделей (2373) |
| `jq` по `registry.json` с фильтром `is_key_provider` (`admin.rs:791-807`) | 220 провайдеров / 2042 модели в каталоге |
| подсчёт `#\[test\]\|#\[tokio::test\]` по `crates/*/src` и `crates/*/tests` | 781 unit + 268 интеграционных |
| тот же подсчёт по `desktop/gpui` | 82 (76 `#[test]` + 6 `#[gpui::test]`) |
| `wc -l crates/heterion-router-gateway/src/provider_sync.rs` | 451 строка, `sync_all` — `:386-451`; тестов нет |
| `scripts/ci/check.sh` | родной гейт: fmt (включая `desktop/gpui`), clippy `-D warnings`, `cargo test --workspace` |
| `cargo fmt --manifest-path desktop/gpui/Cargo.toml -- --check` | единственное, что CI делает для десктопа |
| `npm --prefix desktop/ui ci && npm --prefix desktop/ui run build` | сборка веб-дашборда |
| `gh run list` | прогоны GitHub Actions (последний — 11.11.2026) |
| `sha256sum -c SHA256SUMS` | проверка опубликованных артефактов релиза |

### Не проверено

- Живая БД на `20128`: активные соединения, реальные счётчики `usage_history`,
  состав комбо — запросы к живой БД не выполнялись.
- Launchd-инстанс на `20128` и его соответствие рабочему дереву.
- Визуальное качество рендера GPUI — скриншоты экранов не снимались.
- Фактический прогон `cargo test`/clippy **выполнен** в этой сессии: гейт зелёный
  (код выхода 0). Числа покрытия получены статическим подсчётом атрибутов, а не
  разбором вывода `cargo test`; тяжёлое (`npm --prefix desktop/ui build`, сборка
  `desktop/gpui`, `desktop/scripts/build-app.sh`) не гонялось.
- Поведение Jenkins-контейнера (что он Linux, что `codesign`/`plutil`/`xcrun metal`
  недоступны, последние прогоны Actions) — данные владельца, локально не
  воспроизведены.
- Живые ответы площадок (`zai` 429 «Insufficient balance», `agentrouter` 403) —
  взяты из замеров владельца и research-отчётов, заново не мерялись.

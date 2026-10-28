# 05 — Desktop, service и release: операционный план

> **Роль:** principal architect 5/6 · **Дата аудита:** 2026-09-29
> **Объект:** `/Volumes/Netac_SSD/ai-workstation/Projects/heterion-router` @ `51dfe6d` + рабочее дерево
> **Канонический репозиторий не модифицирован.** Все проверки — read-only либо
> изолированные (см. §1.2). Единственный запись — этот файл.

---

## 0. Резюме для решения

Проект **не имеет воспроизводимого desktop/release-потока**. При этом
исправления — в основном конфигурация и ~150 строк Rust; блокеры внешние
(Apple Developer Program, macOS runner) и не блокируют шаги S0–S5.

| # | Блокер | Severity | Исправляется в |
|---|--------|----------|----------------|
| B1 | **Fresh clone не собирает desktop вообще.** `externalBin` не найден → build script падает. Проверено исполнением. | **Blocker** | S1 |
| B2 | **Dev-окно ходит в launchd-сервис на 20128 и правит его БД.** `tauri dev` не поднимает sidecar (debug), Vite-proxy по умолчанию `20128`. | **Blocker** (data loss) | S2 |
| B3 | **Нет readiness/ownership handshake.** URL инжектится до bind; чужой слушатель на 20129 принимает трафик UI; bind-failure только логируется. | High | S3 |
| B4 | **«Restart now» в десктопе рестартует не тот процесс** — kickstart'ает label `com.heterion.router` (20128), а sidecar на 20129 не трогает. | High | S4 |
| B5 | **Нет plist, нет install/uninstall.** Конфигурация не воспроизводима; живой plist указывает на `target/release/` dev-дерева и крутит `cargo build` в работающий бинарь. | High | S5 |
| B6 | **Bundle не проходит `codesign --verify --deep --strict` и `spctl`.** Подпись ad-hoc/linker, `Info.plist=not bound`, resources seal отсутствует. | High | S6 |
| B7 | **Нет desktop-job в CI.** Исключённый из workspace crate не проверяется fmt/clippy/test — и **уже падает** `fmt --check`. | High | S7 |
| B8 | **`release.yml` шлёт только gateway-бинарь.** Desktop-артефактов, подписи, notarization нет. macOS-leg использует `sha256sum`. | Medium | S7 |
| B9 | **Нет тега/коммита, описывающего собранное.** 35 modified + 5 untracked; откатываться не на что. | High | S0 |
| B10 | **`VITE_GATEWAY_URL` запекается в бандл и бьёт по injected URL.** Переменная нужна и proxy, и `apiBase()`. Ошибка в release-сборке = жёстко зашитый чужой хост. | Medium | S2 |

**Рекомендуемая последовательность: S0 → S1 → S2 → S3 → S4 → S5 → S7 → S6.**
S2 и S3 — единственные, что меняют поведение для уже установленных
пользователей; они разделены и обратимы независимо.

---

## 1. Метод и дисциплина доказательств

### 1.1 Что читалось

Полностью прочитаны: `desktop/` (все 10 исходников Svelte, `vite.config.js`,
`api.js`, `App.svelte`, `Providers.svelte`, все 6 конфигов `src-tauri`,
`stage-sidecar.sh`, оба `package.json`), `.github/workflows/{ci,release}.yml`,
`scripts/ci/check.sh`, `crates/heterion-router-gateway/src/{lib,main,admin,cors,restart,ui}.rs`,
`crates/heterion-router-config/src/config.rs`,
`crates/heterion-router-db/src/{db,migrate}.rs`,
`README.md`, `STATUS.md`, `AGENTS.md`, `docs/ROADMAP-2026-09.md`,
`docs/research/PROJECT-AUDIT-2026-09.md`, `docs/adr/{003,006,007,009}`.
Частично: `docs/adr/README.md`, `desktop/src-tauri/gen/schemas/*` (только
конфигурация), `Providers.svelte`/`Keys.svelte` (фрагментарно).

### 1.2 Что исполнено на этой машине (2026-09-29)

| # | Команда | Результат |
|---|---------|-----------|
| E1 | `cargo fmt --manifest-path desktop/src-tauri/Cargo.toml -- --check` | **exit 1** — расхождения в `build.rs:2` и `src/lib.rs:84-85` |
| E2 | `cargo fmt --all -- --check` | exit 0 |
| E3 | `sh scripts/ci/check.sh` (первые секции) | exec-bits ok, fmt ok; остальное не ждал |
| E4 | `git ls-files -s scripts/ desktop/scripts/` | оба скрипта `100755` — exec-bit gate проходит |
| E5 | `launchctl print gui/501/com.heterion.router` | job **running**, pid 68027, `runs = 17`, порт `*:20128` |
| E6 | `plutil -p ~/Library/LaunchAgents/com.heterion.router.plist` | plist существует, `ProgramArguments[0]` = `<repo>/target/release/heterion-router-gateway`, `KeepAlive=true`, `ThrottleInterval=10` |
| E7 | `curl http://127.0.0.1:20128/healthz` | `{"backend":"routing","started_unix":1790669776,"status":"ok"}`, HTTP 200 |
| E8 | `curl http://127.0.0.1:20129/healthz` | HTTP 000 — порт свободен |
| E9 | `lsof -nP -iTCP -sTCP:LISTEN` | единственный слушатель: `heterion- 68027 … *:20128` |
| E10 | `codesign -dv --verbose=4 "/Applications/Heterion Router.app"` | `Signature=adhoc`, `flags=0x20002(adhoc,linker-signed)`, `Identifier=heterion_router_desktop-f4d18cc6d4112e57`, `Info.plist=not bound` |
| E11 | `spctl -a -vvv "/Applications/Heterion Router.app"` | **отказ**: `code has no resources but signature indicates they must be present` |
| E12 | `codesign --verify --deep --strict` на свежем бандле из `target/release/bundle/macos/` | **тот же отказ** (свежая сборка 2026-09-29 11:08) |
| E13 | `plutil -extract CFBundleIdentifier raw` свежего бандла | `com.heterion.router`, version `0.1.0` |
| E14 | `security find-identity -v -p codesigning` | **`0 valid identities found`** |
| E15 | `sw_vers` | macOS 26.7 (25G229); `xcrun --sdk macosx --show-sdk-version` = 27.0 |
| E16 | `command -v sha256sum` | **`/sbin/sha256sum` существует** на этой macOS 26.7 |
| E17 | `rustc -vV` / `cargo --version` / `cargo tauri --version` | 1.98.1, host `aarch64-apple-darwin`, tauri-cli **2.11.4** |
| E18 | `node --version` / `npm --version` | v26.9.0 / 11.19.1 |
| E19 | `node -e` по `desktop/ui/package-lock.json` | `lockfileVersion 3`, 90 packages; vite 6.4.3, svelte 5.57.0, plugin 5.1.1 |
| E20 | `rg` по `desktop/src-tauri/Cargo.lock` | `tauri 2.11.5` (в `Cargo.toml` `2.11.3` — caret), `tauri-build 2.6.3` |
| E21 | `ls desktop/src-tauri/binaries/` | `heterion-router-gateway` (**stale, 21.09, без triple**) + `heterion-router-gateway-aarch64-apple-darwin` (29.09) |
| E22 | `git status --porcelain` | **35 modified + 5 untracked**, HEAD = `51dfe6d` |
| E23 | `rg` по репозиторию: `notariz|codesign|APPLE_ID|TEAM_ID|entitlements|hardened-runtime|export-options` | ноль конфигурационных совпадений (только проза в docs) |
| E24 | `rg` по репозиторию: `launchctl|LaunchAgents|plist` | ноль шаблонов/скриптов установки |
| E25 | **`cargo check` на изолированной копии `src-tauri` без `binaries/`** | **exit 101**: `resource path 'binaries/heterion-router-gateway-aarch64-apple-darwin' doesn't exist` → B1 доказан исполнением |
| E26 | `rg` по `tauri-utils-2.10.0/src/resources.rs:46-60, 205-208` | подтверждает схему имени `{externalBin}-{triple}` и `Error::ResourcePathNotFound` |
| E27 | `sed` по `crates/heterion-router-db/src/db.rs:33-41, 104-120` | `busy_timeout=2000`, `journal_mode=WAL`, data-dir `~/.heterion-router` → legacy `~/.omniroute` → XDG |
| E28 | Проверка чистоты после E25 | `git status` = те же 40 записей; `git diff --name-only` не содержит `desktop/src-tauri` |

### 1.3 Что **НЕ** исполнялось (каждая метка ниже — явное «not executed»)

- `npm --prefix desktop/ui run build` — пишет в `desktop/ui/dist/` (рабочее дерево).
- `cargo tauri build` / `cargo tauri dev` — 5+ мин, перезаписывает бандл и sidecar.
- `cargo test --workspace`, `cargo clippy` целиком — долго, не относится к десктопу.
- `cargo tauri dev` end-to-end — требует GUI-сессии.
- `release.yml` на GitHub-раннере — особенно поведение `sha256sum` на образе
  `macos-14`. **E16 доказывает наличие `/sbin/sha256sum` только на macOS 26.7.**
  Доступность на `macos-14` (Sonoma) из этой среды не проверялась; вывод в §7.6
  построен на переносимости, а не на наблюдении.
- `xcrun notarytool`, `stapler`, `spctl` на notarized-артефакте — сертификата нет
  (E14), выполнить невозможно.
- Live-проверка sidecar (spawn, bind, parent-watch) — не запускалась: 20129
  свободен (E8), но запуск изменил бы состояние машины.

---

## 2. Проверенные факты (ledger)

### 2.1 Sidecar: жизненный цикл

| Факт | Якорь | Статус |
|------|-------|--------|
| `externalBin: ["binaries/heterion-router-gateway"]` | `desktop/src-tauri/tauri.conf.json:33` | прочитано |
| Tauri ищет `{externalBin}-{triple}` | `tauri-utils-2.10.0/src/resources.rs:58` | прочитано в зависимости |
| Отсутствующий файл → `ResourcePathNotFound` → build script exit 1 | там же `:205-208`; **E25** | **исполнено** |
| Sidecar спавнится **только в release**: `if !cfg!(debug_assertions)` | `desktop/src-tauri/src/lib.rs:55` | прочитано |
| Порог: `HETERION_ROUTER_SIDECAR_PORT` → `OMNIROUTE_SIDECAR_PORT` → `"20129"` | `src/lib.rs:21-32` | прочитано |
| В sidecar ставятся `HETERION_ROUTER_PORT` + `OMNIROUTE_RUST_PORT` + `HETERION_ROUTER_PARENT_PID` | `src/lib.rs:61-63` | прочитано |
| `HETERION_ROUTER_HOST` sidecar'у **не передаётся** → дефолт `127.0.0.1` | `config.rs:54-61` | прочитано |
| Окно создаётся **сразу** после `spawn()`, без ожидания bind | `src/lib.rs:88-93` | прочитано |
| URL инжектится как `window.__HETERION_ROUTER_GATEWAY_URL__` | `src/lib.rs:84-85` | прочитано |
| `Terminated` только логируется, респавна нет | `src/lib.rs:77-79` | прочитано |
| Parent-watch: poll 2 s, `kill(pid,0)`/ESRCH, `exit(0)` | `main.rs:327-353` | прочитано |
| Env sidecar'а наследуется от GUI-процесса (`.env()` только добавляет) | API `tauri-plugin-shell`; `.env()` не очищает | вывод из кода |
| `stage-sidecar.sh` навешан на `beforeBuildCommand`, **не** на `beforeDevCommand` | `tauri.conf.json:9-16` | прочитано |
| Stale `binaries/heterion-router-gateway` без triple лежит в каталоге | **E21** | исполнено |

### 2.2 Порты и bind

| Consumer | Порт | Bind | Источник |
|----------|------|------|----------|
| `cargo run -p heterion-router-gateway` | `20129` | `127.0.0.1` | `config.rs:51-61` |
| Tauri sidecar (release) | `20129` | `127.0.0.1` | `src/lib.rs:21, 57` |
| Vite dev-proxy → gateway | **`20128`** | — | `vite.config.js:7` |
| Tauri fallback в `apiBase()` | **`20128`** | — | `api.js:44-46` |
| launchd-сервис (эта машина) | `20128` | **`0.0.0.0`** | **E5, E6, E9** |

Три из пяти строк сходятся на `20128`, две — на `20129`. Это корень B2, B3, B10.

### 2.3 Readiness и health

- `/healthz` — единственный публичный probe, `lib.rs:147`; тело
  `{status, backend, started_unix}`, `lib.rs:173-179`.
- Маршрут существует **только после** `axum::serve`, т.е. 200 ⇒ listener bound
  и accept-loop запущен. Но **до** этого main.rs:388-419 уже прошёл
  `create_dir_all` → `restrict_data_files` → `Db::open` → `migrate` →
  `validate_auth_policy` → `select_backend`. То есть cold-start до первого 200
  включает миграцию схемы.
- `started_unix` — единственный идентификатор инстанса (`lib.rs:65, 86`); он и
  должен стать основой ownership-токена.
- **Идентификации процесса в ответе нет**: любой процесс, занявший порт, отвечает
  `200 {"status":"ok"}` и будет принят UI за «свой» шлюз.
- UI делает **ровно один** `fetch(/healthz)` в `$effect` без retry и без
  `AbortController` — `App.svelte:29-42`. Единственный `setTimeout` в UI — 2000 мс
  после restart в `Providers.svelte:57`.

### 2.4 Restart

- `POST /api/restart` → `admin::restart_service` (`admin.rs:379-403`).
- Проверка: `launchctl print gui/<uid>/<label>` (`restart.rs:50-58`).
- Действие: `sh -c 'sleep 2; exec launchctl kickstart -k gui/<uid>/<label>'` (`restart.rs:45-47, 62-70`).
- Label: `HETERION_ROUTER_SERVICE_LABEL` → `OMNIROUTE_SERVICE_LABEL` → `com.heterion.router` (`restart.rs:14-28`).
- **Десктопный sidecar не является этим job'ом** ⇒ 409, либо — если job установлен —
  рестартуется сервис на 20128. Это уже зафиксировано в ADR-006 addendum
  (`docs/adr/ADR-006-supervised-restart-only.md:35-38`).
- UI переживает рестарт фиксированным `sleep(2000)` без проверки
  `started_unix` (`Providers.svelte:57`).

### 2.5 launchd

- Живой plist: `~/Library/LaunchAgents/com.heterion.router.plist`, 1385 B, mtime 2026-09-18 (**E6**).
- `ProgramArguments[0]` = `<repo>/target/release/heterion-router-gateway`.
  Следствие: `cargo build --release` и `sh scripts/stage-sidecar.sh` (который
  делает именно `cargo build --release`, `stage-sidecar.sh:14`) **перезаписывают
  работающий исполняемый файл**; при `KeepAlive=true` launchd может стартовать
  на полузаписанном бинаре.
- `HETERION_ROUTER_UI_DIR` в plist указывает на `<repo>/desktop/ui/dist` — сервис
  раздаёт статику из рабочего дерева.
- Ни plist, ни install/uninstall-скрипта в репозитории нет (**E24**).
- Рядом лежит legacy `com.omniroute.rust.plist` (17.09) — не управляется кодом.

### 2.6 Сборка, подпись, release

- `bundle.targets: "all"` ⇒ `.app` + `.dmg` на macOS (**E12/E13** подтверждают наличие).
- `bundle.macOS.*` отсутствует: нет `entitlements`, нет `signingIdentity`, нет
  `minimumSystemVersion`.
- `app.security.csp: null` (`tauri.conf.json:19-21`) — CSP выключен.
- `capabilities/default.json:6` — `["core:default", "shell:allow-spawn"]`.
  `shell:allow-execute` отсутствует (и не нужен).
- Подпись: ad-hoc/linker-signed, `TeamIdentifier=not set` (**E10, E12**).
- `codesign --verify --deep --strict` **падает** на свежей сборке 2026-09-29 (**E12**).
- `spctl` **отказывает** (**E11**).
- 5 мест с версией, ничем не связанных: `Cargo.toml:26`, `desktop/src-tauri/Cargo.toml:3`,
  `desktop/package.json:3`, `desktop/ui/package.json:4`, `tauri.conf.json:4`.
- `ci.yml` триггерится на `desktop/ui/**`, но **не** на `desktop/src-tauri/**`,
  `desktop/package.json`, `desktop/scripts/**`, `desktop/**` в release-триггере.
- `ci.yml:1-4` и `release.yml:1-3` явно признают: desktop не в CI.
- `release.yml:24` matrix `[ubuntu-latest, macos-14]`; `release.yml:44` — `sha256sum`.
- `release.yml` не имеет `persist-credentials: false` (в отличие от `ci.yml:38, 48`).

---

## 3. Находки с обоснованием

### B1 — fresh clone не собирает desktop (Blocker, доказано E25)

`binaries/` в `.gitignore` (`.gitignore:7`, `desktop/src-tauri/.gitignore:6`).
`stage-sidecar.sh` вызывается только из `beforeBuildCommand`
(`tauri.conf.json:14`), а `beforeDevCommand` (`tauri.conf.json:9-12`) — нет.
`tauri-build` разрешает `externalBin` на этапе build script и падает при
отсутствии файла.

Воспроизведено: копия `src-tauri` без `binaries/` → `cargo check` → exit 101,
`resource path 'binaries/heterion-router-gateway-aarch64-apple-darwin' doesn't exist`.

Следствие: у CONTRIBUTER'а `cargo tauri dev` не работает. Это не «документированная
особенность», это блокер онбординга, скрытый тем, что у автора каталог заполнен.

### B2 — dev-окно правит БД живого сервиса (Blocker)

Цепочка: `tauri dev` ⇒ `debug_assertions == true` ⇒ sidecar **не** спавнится
(`src/lib.rs:55`) ⇒ `apiBase()` возвращает `""` (`api.js:47`, same-origin) ⇒ Vite
проксирует `/api` на `20128` (`vite.config.js:7`) ⇒ `PATCH /api/connections/:id`
(`lib.rs:152`) меняет строки в `~/.heterion-router/storage.sqlite` — это БД
поднятого LAN-сервиса.

`Providers.svelte:34-37` прямо показывает, что UI ожидает
`restart_required` — то есть рассчитывает на живой restart. Restart из dev-окна
попадёт в `launchctl kickstart -k gui/501/com.heterion.router` (**E5** подтверждает,
что job существует) и перезапустит production-сервис.

Это самая дорогая находка: она не проявляется ошибкой, она тихо портит данные.

### B3 — нет ownership/readiness handshake (High)

`spawn()` возвращается до bind. Окно создаётся в том же тике (`src/lib.rs:88`).
`apiBase()` немедленно возвращает инжектированный URL. Один `fetch` в
`App.svelte:31` без retry.

Три независимых отказа:
1. **Гонка старта.** Gateway до первого 200 делает `create_dir_all`,
   `restrict_data_files`, `Db::open`, `migrate` (`main.rs:389-419`) — на большой БД
   это десятки миллисекунд. Первый `fetch` отвалится, `health` навсегда
   `{status:"unreachable"}` до перемонтирования окна. Единственный
   `setTimeout` в UI — restart-путь, не health.
2. **Чужой слушатель.** Если 20129 занят (забытый `cargo run`), spawn умирает с
   `AddrInUse`, лог получает одну строку `sidecar exited` (`src/lib.rs:78`),
   окно всё равно создаётся, и UI годами читает чужой `/healthz`. Именно это
   описано в `PROJECT-AUDIT-2026-09.md:125` и `ADR-007` addendum:38.
3. **Нет респавна.** Единственный `Terminated` — `log::warn`.

### B4 — restart целится не туда (High)

Уже зафиксировано в ADR-006 addendum:35-38, но не исправлено. Дополнительно:
`Providers.svelte:57` ждёт ровно 2000 мс — ровно столько же, сколько задержка в
`restart.rs:46`. То есть UI просыпается в ту же миллисекунду, когда процесс ещё
убивается. Гонка не «иногда медленно», а структурная.

### B5 — сервис не воспроизводим (High)

Нет plist в репозитории (**E24**). Живой plist указывает на dev-дерево (**E6**).
`cargo build --release` перезаписывает исполняемый файл работающего job'а при
`KeepAlive=true`. Также в `~/Library/LaunchAgents` лежит осиротевший
`com.omniroute.rust.plist`.

Отдельно: `restrict_data_files` (`main.rs:359-368`) возвращает `Err` на первой
неудаче и стоит **до** инициализации трейсинга. Любая непроходимая
`set_permissions` в data-dir → `main` → exit → `KeepAlive` рецикл с
`ThrottleInterval=10`. Прямо не наблюдалось, но путь обработки ошибок в коде
именно такой.

### B6 — bundle не проходит верификацию подписи (High)

E10/E11/E12. Конкретно: `flags=0x20002(adhoc,linker-signed)` и
`Info.plist=not bound` ⇒ resources seal не соответствует содержимому ⇒
`codesign --verify --deep --strict` и `spctl` отказывают. Причина — линкерная
ad-hoc подпись, а не подпись бандла: Tauri не смог выбрать identity (E14 = 0
identities) и откатился на linker-signed.

Отдельно: **установленный в `/Applications` бандл старше текущего конфига.**
`CFBundleIdentifier` в его Info.plist — `com.heterion.router` (совпадает), но
codesign-identifier — `heterion_router_desktop-f4d18cc6d4112e57`, а sidecar внутри
имеет mtime **24.09**, а desktop-бинарь — **20.09**. То есть в bundle
подложили sidecar вручную. Это не сборка `cargo tauri build` (такая датирует
оба файла 29.09 11:05/11:08 — **E12/E13**).

### B7 — desktop вне CI, и он уже падает (High)

`Cargo.toml:23` `exclude = ["desktop/src-tauri"]` ⇒ `cargo fmt --all` его не
видит. **E1: `cargo fmt --check` на нём → exit 1** (`build.rs` 2-space indent,
`src/lib.rs:84-85` лишний перенос). Ни clippy, ни тестов у крейта нет
вообще — в `src/` только `main.rs` (6 строк) и `lib.rs` (98 строк), ни одного
`#[test]`.

### B8 — release не выпускает desktop (Medium)

`release.yml` собирает только `heterion-router-gateway`. Ни `.app`, ни `.dmg`,
ни подписи, ни notarization. `sha256sum` на macos-14 (**:44**) — не
macOS-родная форма; на **этой** машине `/sbin/sha256sum` есть (**E16**), но
доступность на образе `macos-14` не проверялась (not executed). Переносимое
решение — `shasum -a 256`, присутствует на всех macOS и на всех GitHub Linux
образах; либо явный fallback.

### B9 — нет точки отката (High)

**E22**: HEAD = `51dfe6d`, 35 modified + 5 untracked, ни одного тега. Бандл от
20.09 собран из другого состояния дерева. `STATUS.md:19-21` и
`PROJECT-AUDIT-2026-09.md:213` это уже признают, но отката по-прежнему нет.

### B10 — `VITE_GATEWAY_URL` запекается в бандл (Medium)

`api.js:31` читает `import.meta.env.VITE_GATEWAY_URL` **раньше** injected URL
(`api.js:41`). Та же переменная управляет proxy (`vite.config.js:7`). Один
экспорт в `.zshrc` или в CI-среде → `vite build` запекает литерал в
`dist/assets/*.js`, и в release-приложении он перебьёт `http://127.0.0.1:20129`.
Проверяемо grep'ом по `dist/`.

Сопутствующее: `api.js:34` — override из `localStorage` имеет приоритет над
injected URL и переживает обновление приложения. Для sidecar-приложения это
некорректный канал (нет UI для его установки/сброса).

---

## 4. Матрица «developer vs distributable»

| Аспект | Developer (`tauri dev`) | Developer (`tauri build`, локально) | Distributable (цель) |
|---|---|---|---|
| Sidecar | **не спавнится** (`lib.rs:55`) | спавнится | спавнится |
| Источник URL | same-origin `""` (`api.js:47`) | `window.__HETERION_ROUTER_GATEWAY_URL__` | injected, только после handshake |
| Fallback URL | — | `20128` — **чужой процесс** (`api.js:44-46`) | **удалить** |
| Порт шлюза | `5173` (Vite, `strictPort`) | `20129` | `20129` (или динамический) |
| Порт API | `20128` через proxy (**сервис!**) | `20129` | `20129` loopback |
| Data dir | `~/.heterion-router` (**прода!**) | `~/.heterion-router` | `~/.heterion-router` (осознанно общий с сервисом) |
| Restart | `409` или **рестартует сервис** | то же | sidecar-aware |
| Health | 1 попытка, без retry | 1 попытка, без retry | poll до `ready` с дедлайном |
| Auth | из `localStorage` | из `localStorage` | из `localStorage` (вне зоны этого плана) |
| Подпись | n/a (debug) | ad-hoc/linker, **verify падает** | Developer ID + hardened runtime + notarized + stapled |
| Gatekeeper | n/a | `spctl` **отказ** (E11) | `spctl` accept |
| Entitlements | нет | нет | `entitlements.macos.plist` |
| CSP | `null` | `null` | ограниченный |
| Bundle id | `com.heterion.router` | `com.heterion.router` | `com.heterion.router`, заморожен |
| Arch | host | `aarch64` (E13/E21) | `aarch64` + `x86_64` (matrix) |
| Версия | из 5 мест | из 5 мест | одна, проверяется в CI |
| Update | ручной | ручной | ручной (`stapler`+DMG) — updater вне scope |
| Cold build | ~5 мин (STATUS) | ~5 мин 15 с (PROJECT-AUDIT:139) | CI, воспроизводимо |
| Артефакт в репозитории | нет | `target/`, `binaries/` (gitignored) | `release-manifest.json` + `SHA256SUMS` |

---

## 5. Контракт окружения

Единая таблица. `HETERION_ROUTER_*` первичен, `OMNIROUTE_*` — fallback на один
цикл (проверено: `config.rs:42-49`, `restart.rs:20`, `ui.rs:23`, `db.rs:86`).

### 5.1 Gateway (`heterion-router-gateway`)

| Переменная | Дефолт | Владелец | Якорь | Замечание |
|---|---|---|---|---|
| `HETERION_ROUTER_PORT` | `20129` | whoever starts | `config.rs:51-53` | sidecar переопределяет |
| `HETERION_ROUTER_HOST` | `127.0.0.1` | whoever starts | `config.rs:54-61` | `0.0.0.0` ⇒ требует ≥1 api_key (`main.rs:370-384`) |
| `HETERION_ROUTER_DATA_DIR` | `~/.heterion-router` | оператор | `config.rs:62-69`, `db.rs:104-120` | legacy `~/.omniroute` |
| `RUST_LOG` / `HETERION_ROUTER_LOG` | `info` | оператор | `config.rs:70-79` | |
| `HETERION_ROUTER_REQUIRE_AUTH` | off | оператор | `config.rs:80-83` | |
| `HETERION_ROUTER_UI_DIR` | `<repo>/desktop/ui/dist` | оператор | `ui.rs:22-32` | **в sidecar это путь в dev-дерево** |
| `HETERION_ROUTER_PARENT_PID` | unset | Tauri shell | `main.rs:328` | присутствие ⇒ sidecar-режим |
| `HETERION_ROUTER_SERVICE_LABEL` | `com.heterion.router` | оператор | `restart.rs:14-28` | |
| `HETERION_LOCAL_BASE_URL` | из registry | оператор | `main.rs:70-81` | только для `heterion-local` |
| `OPENAI_COMPAT_BASE_URL` / `_API_KEY` | unset | оператор | `main.rs:138-147` | |
| `GROK_CLI_TOKEN` / `GROK_CLI_BASE_URL` | unset / `cli-chat-proxy.grok.com` | оператор | `main.rs:29, 101` | |
| `HETERION_ROUTER_ENABLE_ECHO` | off | тесты | `main.rs:88-95` | не для прода |
| `STORAGE_ENCRYPTION_KEY` | unset | оператор | ADR-009 | отсутствие → plaintext write-back |
| `HETERION_ROUTER_INSTANCE_TOKEN` | **нет** | **вводится в S3** | — | ownership |

### 5.2 Desktop shell (`heterion-router-desktop`)

| Переменная | Владелец | Направление | Замечание |
|---|---|---|---|
| `HETERION_ROUTER_SIDECAR_PORT` | разработчик | shell → порт sidecar | `src/lib.rs:24` |
| `HETERION_ROUTER_DATA_DIR` | **наследуется** | GUI-env → sidecar | **опасность:** экспорт в `.zshrc` меняет БД приложения |
| `HETERION_ROUTER_HOST` | **наследуется** | GUI-env → sidecar | **опасность:** экспорт `0.0.0.0` откроет sidecar в LAN |
| `RUST_LOG` | наследуется | | |

**Проблема наследования (новая, из §2.1).** `Command::env()` в
`tauri-plugin-shell` дописывает переменные к окружению родителя, а не заменяет
его. Приложение, запущенное из Finder/LaunchServices, получает минимальное
окружение; запущенное из терминала — наследует всё. Значит поведение sidecar
зависит от способа запуска .app. Минимальное лечение: перед `spawn()` явно
передать `HETERION_ROUTER_HOST=127.0.0.1` и `HETERION_ROUTER_UI_DIR` (пустой),
плюс — при желании — `env_remove` для `HETERION_ROUTER_REQUIRE_AUTH` и
`OMNIROUTE_*`.

### 5.3 UI (`desktop/ui`)

| Переменная | Дефолт | Назначение | Якорь |
|---|---|---|---|
| `VITE_GATEWAY_URL` | `http://127.0.0.1:20128` | **двойное назначение**: proxy + запекается в бандл | `vite.config.js:7`, `api.js:31` |
| — | `http://127.0.0.1:20128` | Tauri-fallback | `api.js:45` |
| — | injected | sidecar URL | `api.js:41` |
| — | `localStorage.heterion_router_gateway_url` | ручной override | `api.js:34` |

Развязка (S2): **proxy** берёт `VITE_PROXY_TARGET` (dev-only, читается
`vite.config.js` в рантайме Node, в бандл не попадает), **бандл** берёт
`VITE_API_BASE`, который в release-сборке обязан быть пустым. Тогда
`import.meta.env.VITE_GATEWAY_URL` из `api.js` исчезает совсем.

---

## 6. Целевой дизайн

### 6.1 Владение портами

```
                    127.0.0.1:5173   vite (dev only, strictPort)
                          │
   tauri dev ─────────────┤  НЕ поднимает sidecar
   (window → 5173)       └── proxy /api /v1 /healthz → 127.0.0.1:20129  ← S2: было 20128

   Heterion Router.app
     └─ sidecar 127.0.0.1:<P>   P выбирает shell, проверяет токеном
     └─ window  → http://127.0.0.1:<P>  (инжект после handshake)

   launchd com.heterion.router
     └─ 127.0.0.1:20128  (LAN 0.0.0.0 — только осознанный opt-in)
```

Правила владения:

1. **20128** принадлежит label `com.heterion.router`. Ни один dev-процесс не
   должен на него указывать по умолчанию.
2. **20129** принадлежит dev-gateway (`cargo run`) **или** sidecar'у, но не обоим
   одновременно. Они взаимно исключаются по времени, а не по конфигурации.
3. **Sidecar-порт выбирает shell**, а не константа. Реализация: `TcpListener::bind("127.0.0.1:0")` → прочитать порт → дропнуть listener → передать в env. TOCTOU-окно на loopback ничтожно, но главное — владение портом становится доказуемым, а не предполагаемым.
4. Никогда не фолбэчить на 20128 из приложения.

### 6.2 Handshake готовности и владения

Минимальный контракт, не требующий новых зависимостей и не требующий миграции БД.

**Изменение A — `crates/heterion-router-config/src/config.rs`:**
```rust
/// Ownership token echoed by `/healthz` so a supervisor can prove the
/// listener on the port is the child it started.
pub instance_token: Option<String>,
```
читается из `HETERION_ROUTER_INSTANCE_TOKEN` (legacy: `OMNIROUTE_INSTANCE_TOKEN`),
в `GatewayConfig`, дальше в `AppState` (`lib.rs:53-66`) рядом с `started_unix`.

**Изменение B — `crates/heterion-router-gateway/src/lib.rs:173-179`:**
```rust
async fn healthz(State(state): State<AppState>) -> impl IntoResponse {
    Json(json!({
        "status": "ok",
        "backend": state.backend.name(),
        "started_unix": state.started_unix,
        "instance_token": state.instance_token,   // Option<String>
        "ready": true,
    }))
}
```
`ready: true` — константа, существующая только для прямой читаемости контракта;
роут и так монтируется до первого запроса.

**Изменение C — `crates/heterion-router-gateway/src/main.rs:438`:**
bind делать **до** `select_backend`/`migrate`? Нет — это сломает «не подниматься
без ключа» (`main.rs:370-384`). Оставляем порядок, handshake покрывает
ожидание.

**Изменение D — `desktop/src-tauri/src/lib.rs`, замена `:55-86`:**
```
1. port = pick_free_loopback_port()            // TcpListener::bind("127.0.0.1:0")
2. token = hex(16 random bytes from /dev/urandom)
3. spawn sidecar с env:
     HETERION_ROUTER_PORT=port
     OMNIROUTE_RUST_PORT=port
     HETERION_ROUTER_HOST=127.0.0.1            // ← закрыть наследование
     HETERION_ROUTER_INSTANCE_TOKEN=token
     HETERION_ROUTER_PARENT_PID=<pid>
4. deadline = now + 20s
   loop {
     match GET http://127.0.0.1:<port>/healthz {
       200 + json.instance_token == token  => ready, break
       Err(_) | non-200 | token mismatch  => sleep 100ms; continue
     }
     if now > deadline => fail(...)
     if child terminated => respawn (шаг 3), budget: 3 попытки, backoff 1s/2s/4s
   }
5. только теперь WebviewWindowBuilder + initialization_script с URL
```

Почему это правильно: токен доказывает, что слушающий процесс — именно наш
ребёнок, а не «кто-то занял порт». `/healthz` публичен (`lib.rs:147`) и не
требует ключа, поэтому handshake работает на first-run без `api_keys`.

**Изменение E — `desktop/ui/src/api.js:44-46`:** удалить ветку
`window.__TAURI_INTERNALS__ → 20128`. В Tauri без инжекта `apiBase()` должен
вернуть `""` и показать честную ошибку, а не подключиться к чужому процессу.
Опционально: убрать и localStorage-override (`api.js:33-38`) — для sidecar-приложения
некем им управлять.

**Изменение F — `desktop/ui/src/App.svelte:29-42`:** poll вместо одного вызова.
```js
$effect(() => {
  let cancelled = false;
  (async () => {
    for (let attempt = 0; attempt < 60 && !cancelled; attempt++) {
      try {
        const res = await fetch(`${apiBase()}/healthz`, { signal: AbortSignal.timeout(2000) });
        if (res.ok) { const d = await res.json(); if (!cancelled) health = d; return; }
      } catch { /* sidecar ещё поднимается */ }
      await new Promise((r) => setTimeout(r, 250));
    }
    if (!cancelled) health = { status: "unreachable" };
  })();
  return () => { cancelled = true; };
});
```
15 с суммарного ожидания при 2-секундном таймауте на запрос.

**Изменение G — restart (S4).** Три уровня, по возрастанию честности:

1. **Сервер (обязательно).** `admin.rs:379` — если `HETERION_ROUTER_PARENT_PID`
   задан, процесс sidecar ⇒ `409` с текстом «sidecar is restarted by its host
   application». Это одна проверка, защищает всех клиентов, и убирает
   «рестартнул не тот процесс» полностью. ADR-006 уже требует supervised-restart;
   sidecar не supervised.
2. **UI (обязательно).** `Providers.svelte:106-111` — если 409 с этой причиной,
   кнопка даёт понятный текст, а не «Gateway restarted and serving».
3. **Shell (желательно).** Tauri command `restart_sidecar`: убить child,
   переспавнить с тем же портом/токеном, `window.reload()`. Тогда кнопка
   «Restart now» работает и в десктопе.

**Изменение H — `Providers.svelte:57`:** заменить `setTimeout(2000)` на poll
`/healthz` до смены `started_unix` (таймаут 30 с). Ровно то, для чего ADR-006:20
и вводил `started_unix`, но UI его не использует.

### 6.3 launchd: точные файлы

Новые файлы в репозитории (сейчас их нет — **E24**):

```
scripts/service/com.heterion.router.plist.in     # шаблон с @PLACEHOLDERS@
scripts/service/install.sh                      # bootout + write + bootstrap
scripts/service/uninstall.sh                    # bootout + rm
scripts/service/README.md                       # runbook
```

`com.heterion.router.plist.in` (ключи — из **E6**, нормализованные):
```xml
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN"
  "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>            <string>com.heterion.router</string>
  <key>ProgramArguments</key>
  <array><string>@BINARY@</string></array>
  <key>EnvironmentVariables</key>
  <dict>
    <key>HETERION_ROUTER_PORT</key>     <string>20128</string>
    <key>HETERION_ROUTER_HOST</key>     <string>127.0.0.1</string>
    <key>HETERION_ROUTER_DATA_DIR</key> <string>@DATA_DIR@</string>
    <key>HETERION_ROUTER_UI_DIR</key>   <string>@UI_DIR@</string>
    <key>RUST_LOG</key>                 <string>info</string>
    <key>NO_PROXY</key>                 <string>127.0.0.1,localhost,::1</string>
    <key>no_proxy</key>                 <string>127.0.0.1,localhost,::1</string>
    <key>PATH</key><string>/usr/bin:/bin:/usr/sbin:/sbin</string>
  </dict>
  <key>RunAtLoad</key>        <true/>
  <key>KeepAlive</key>        <dict><key>SuccessfulExit</key><false/></dict>
  <key>ThrottleInterval</key> <integer>10</integer>
  <key>StandardOutPath</key>  <string>@LOG_DIR@/heterion-router.out.log</string>
  <key>StandardErrorPath</key><string>@LOG_DIR@/heterion-router.err.log</string>
  <key>ProcessType</key>      <string>Background</string>
</dict>
</plist>
```

Два осознанных отличия от живого plist:
- `HETERION_ROUTER_HOST` = `127.0.0.1`, а не `0.0.0.0` (**E6**). LAN-режим —
  отдельный opt-in через `@HOST@`, документированный как требующий TLS/VPN и
  ключа (README:81-83).
- `KeepAlive` = `SuccessfulExit:false` вместо `true`. Причина: если бинарь в
  data-dir упрётся в `restrict_data_files` (`main.rs:359-368`) и выйдет, текущий
  plist даст бесконечный рецикл; этот вариант поднимает процесс только при
  нештатном завершении.

`install.sh` (POSIX sh, идемпотентный):
```sh
#!/bin/sh
set -eu
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
LABEL="${HETERION_ROUTER_SERVICE_LABEL:-com.heterion.router}"
PLIST="$HOME/Library/LaunchAgents/$LABEL.plist"
UIDN="$(id -u)"
BIN="${HETERION_ROUTER_BIN:-$ROOT/target/release/heterion-router-gateway}"
[ -x "$BIN" ] || { echo "missing gateway binary: $BIN" >&2; exit 1; }
launchctl bootout "gui/$UIDN/$LABEL" 2>/dev/null || true
sed -e "s|@BINARY@|$BIN|g" \
    -e "s|@DATA_DIR@|${HETERION_ROUTER_DATA_DIR:-$HOME/.heterion-router}|g" \
    -e "s|@UI_DIR@|${HETERION_ROUTER_UI_DIR:-$ROOT/desktop/ui/dist}|g" \
    -e "s|@HOST@|${HETERION_ROUTER_HOST:-127.0.0.1}|g" \
    -e "s|@LOG_DIR@|$HOME/Library/Logs|g" \
    "$(dirname "$0")/$LABEL.plist.in" > "$PLIST"
plutil -lint "$PLIST"
launchctl bootstrap "gui/$UIDN" "$PLIST"
# readiness contract
i=0; while [ $i -lt 60 ]; do
  curl -fsS -m 2 "http://127.0.0.1:20128/healthz" >/dev/null 2>&1 && { echo ready; exit 0; }
  i=$((i+1)); sleep 0.5
done
echo "service did not become ready in 30s" >&2; exit 1
```

`uninstall.sh`:
```sh
#!/bin/sh
set -eu
LABEL="${HETERION_ROUTER_SERVICE_LABEL:-com.heterion.router}"
launchctl bootout "gui/$(id -u)/$LABEL" 2>/dev/null || true
rm -f "$HOME/Library/LaunchAgents/$LABEL.plist"
# data dir НЕ трогаем: это живая БД с ключами и комбо.
```

Обязательная защита от **B5**: `install.sh` обязан **не** указывать на
`$ROOT/target/release/`, если `$ROOT` — рабочее дерево. Минимальное правило —
скрипт отказывается ставить, если `$ROOT` содержит `.git` и
`HETERION_ROUTER_ALLOW_DEV_TREE` не выставлен; тогда оператор обязан указать
`HETERION_ROUTER_BIN=/Applications/Heterion Router.app/Contents/MacOS/...`
(нет — это desktop) либо путь к установленному gateway. Практичный вариант:
поддержать `$HETERION_ROUTER_PREFIX` (например `/usr/local/opt/heterion-router`)
и в README сделать его единственным поддерживаемым путём для install.

### 6.4 Data dir и миграция

**Схема БД не меняется.** `MIGRATIONS` содержит одну запись `001`
(`migrate.rs:34-38`), тело — `schema_base.sql` с `IF NOT EXISTS`, runner
выполняет каждую миграцию в своей транзакции. Instance-token — процессное
состояние, не колонка. Следовательно, **data migration для этого плана
отсутствует**; это осознанное проектное решение, а не пропуск.

Sidecar и launchd-сервис **намеренно делят** `~/.heterion-router/storage.sqlite`
(ADR-001 shared SQLite boundary). Это правильно: иначе десктоп не увидит
комбо/ключи пользователя. Проверено, что concurrency выдерживается: `WAL` +
`busy_timeout=2000` (`db.rs:36-37`).

Два остаточных риска, которые план закрывает явно:
- `restrict_data_files` (`main.rs:359-368`) возвращает `Err` на первой неудаче и
  стоит **до** инициализации логирования. Лечение: сделать best-effort
  (`let _ = std::fs::set_permissions(...)`) и перенести вызов **после**
  `tracing_subscriber::init()`. Тогда ошибка прав будет в логе, а не в crash-loop.
- `busy_timeout=2000` короток, если два процесса одновременно мигрируют на
  большой БД. Практика: runbook говорит «запускайте десктоп при остановленном
  сервисе», либо в плане S5 поднять таймаут до 10 000 (одна строка, но требует
  миграционного решения, т.к. значение зашито в `apply_pragmas`).

---

## 7. CI и release

### 7.1 Текущее состояние (проверено)

- `ci.yml` — 2 job'а: `rust` (ubuntu, `scripts/ci/check.sh`) и `ui` (ubuntu,
  node 24, `npm ci && npm run build`).
- Триггеры покрывают `desktop/ui/**` (`ci.yml:14, 24`), но **не**
  `desktop/src-tauri/**`, `desktop/package.json`, `desktop/scripts/**`.
- `release.yml` — 1 job, matrix ubuntu+macos-14, только `heterion-router-gateway`.

### 7.2 Целевые job'ы

| Job | Runner | Триггер | Время | Что делает |
|---|---|---|---|---|
| `rust` | ubuntu-latest | push/PR | ~3 мин | без изменений |
| `ui` | ubuntu-latest | push/PR | ~40 с | `npm ci && npm run build` + **новое: greps** |
| `desktop-rust` | ubuntu-latest | push/PR | ~4 мин | fmt/clippy/test на `desktop/src-tauri` |
| `desktop-bundle` | `macos-14` | push/PR по `desktop/**` | ~8 мин | `cargo tauri build --bundles app`, ad-hoc, проверки структуры |
| `version-consistency` | ubuntu-latest | push/PR | ~5 с | 5 мест с версией равны |
| `service-lint` | ubuntu-latest | push/PR | ~5 с | `plutil`-эквивалент для plist (валидация через `xmllint` + grep ключей) |
| `release-gateway` | matrix | `workflow_dispatch` | ~6 мин | существующий, с фиксом checksum |
| `release-desktop` | `macos-14` | tag `desktop-v*` | ~12 мин | sign + notarize + staple + attach |

`desktop-rust` (исправляет B7):
```yaml
  desktop-rust:
    name: desktop fmt / clippy / test
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
        with: { persist-credentials: false }
      - uses: dtolnay/rust-toolchain@stable
        with: { targets: aarch64-apple-darwin, x86_64-apple-darwin }
      - uses: Swatinem/rust-cache@v2
        with:
          workspaces: |
            desktop/src-tauri -> target
      - uses: actions/setup-node@v4
        with: { node-version: "24" }
      - run: npm ci --no-audit --no-fund
        working-directory: desktop/ui
      - run: npm run build
        working-directory: desktop/ui
      - name: stage sidecar (host target is fine for a check)
        run: sh desktop/scripts/stage-sidecar.sh
      - name: fmt
        run: cargo fmt --manifest-path desktop/src-tauri/Cargo.toml -- --check
      - name: clippy
        run: cargo clippy --manifest-path desktop/src-tauri/Cargo.toml --all-targets --locked -- -D warnings
      - name: test
        run: cargo test --manifest-path desktop/src-tauri/Cargo.toml --locked
```
Замечание: ubuntu-runner — это `x86_64-linux`, и `stage-sidecar.sh` возьмёт
`x86_64-unknown-linux-gnu`; Tauri соберёт desktop-крейт под Linux и
`externalBin` разрешится. Если нужен именно macOS-крейт, добавить
`targets: aarch64-apple-darwin` и `--target aarch64-apple-darwin` — тогда job
уезжает на macOS. **Рекомендация: `desktop-rust` — на ubuntu (дёшево, ловит
fmt/clippy), `desktop-bundle` — на macOS (единственный, кто реально собирает
.bundle).**

`ui` — дополнительные greps (ловит B10):
```yaml
      - name: no baked gateway URL in the distributable bundle
        run: |
          set -eu
          # build повторно БЕЗ VITE_GATEWAY_URL и убедиться, что в ассетах нет литерала
          ( cd desktop/ui && VITE_GATEWAY_URL= npm run build )
          ! grep -rq "127.0.0.1:20128" desktop/ui/dist/assets || {
            echo "VITE_GATEWAY_URL leaked into the bundle"; exit 1; }
          ! grep -rq "__HETERION_ROUTER_GATEWAY_URL__" desktop/ui/dist/assets || {
            echo "injected global must not be baked"; exit 1; }
      - name: dev proxy target is the dev gateway, not the service
        run: grep -q 'VITE_PROXY_TARGET ?? "http://127.0.0.1:20129"' desktop/ui/vite.config.js
```

`desktop-bundle` (ловит B6 на этапе PR, без секретов):
```yaml
      - name: bundle structure
        run: |
          set -eu
          APP="desktop/src-tauri/target/release/bundle/macos/Heterion Router.app"
          test -d "$APP"
          test -x "$APP/Contents/MacOS/heterion-router-desktop"
          test -x "$APP/Contents/MacOS/heterion-router-gateway"        # sidecar внутри
          plutil -extract CFBundleIdentifier raw "$APP/Contents/Info.plist" | grep -qx com.heterion.router
          # sidecar и shell — один коммит: сверяем mtime/размер хешем из манифеста
      - name: signature is at least internally consistent
        run: codesign --verify --deep --strict "desktop/src-tauri/target/release/bundle/macos/Heterion Router.app"
```
Последний шаг **упадёт** на текущем коде (E12) — это ровно тот сигнал, который
нужен: он делает S6+S1 наблюдаемыми до покупки сертификата.

### 7.3 `release-gateway` — фикс checksum (B8)

Заменить `release.yml:44`:
```sh
          # portable across ubuntu and macOS images
          if command -v sha256sum >/dev/null 2>&1; then
            sha256sum "$OUT" > "${OUT}.sha256"
          else
            shasum -a 256 "$OUT" > "${OUT}.sha256"
          fi
```
Добавить `persist-credentials: false` в `actions/checkout@v4` (`release.yml:28-30`).

### 7.4 `release-desktop` — signing/notarization

```yaml
  release-desktop:
    needs: [version-consistency]
    if: startsWith(github.ref, 'refs/tags/desktop-v')
    runs-on: macos-14
    permissions: { contents: write }
    env:
      APPLE_CERTIFICATE: ${{ secrets.APPLE_CERTIFICATE }}
      APPLE_CERTIFICATE_PASSWORD: ${{ secrets.APPLE_CERTIFICATE_PASSWORD }
      APPLE_SIGNING_IDENTITY: ${{ secrets.APPLE_SIGNING_IDENTITY }
      APPLE_ID: ${{ secrets.APPLE_ID }
      APPLE_PASSWORD: ${{ secrets.APPLE_PASSWORD }
      APPLE_TEAM_ID: ${{ secrets.APPLE_TEAM_ID }
    steps:
      - uses: actions/checkout@v4
        with: { ref: ${{ github.ref }}, persist-credentials: false }
      - uses: dtolnay/rust-toolchain@stable
        with: { targets: aarch64-apple-darwin }
      - uses: Swatinem/rust-cache@v2
      - uses: actions/setup-node@v4
        with: { node-version: "24" }
      - name: import Developer ID
        run: |
          set -eu
          KEYCHAIN="$RUNNER_TEMP/build.keychain-db"
          security create-keychain -p actions "$KEYCHAIN"
          security set-keychain-settings -lut 21600 "$KEYCHAIN"
          security unlock-keychain -p actions "$KEYCHAIN"
          echo "$APPLE_CERTIFICATE" | base64 --decode > "$RUNNER_TEMP/cert.p12"
          security import "$RUNNER_TEMP/cert.p12" -k "$KEYCHAIN" \
            -P "$APPLE_CERTIFICATE_PASSWORD" -T /usr/bin/codesign -T /usr/bin/security
          security set-key-partition-list -S apple-tool:,apple: -s -k actions "$KEYCHAIN"
          security list-keychain -d user -s "$KEYCHAIN"
          rm -f "$RUNNER_TEMP/cert.p12"
      - run: npm ci --no-audit --no-fund
        working-directory: desktop/ui
      - name: build + sign
        run: |
          set -eu
          cd desktop/src-tauri
          cargo tauri build --bundles app,dmg \
            --config '{"bundle":{"macOS":{"signingIdentity":"'"$APPLE_SIGNING_IDENTITY"'"}}}'
      - name: hardened runtime (explicit; do not rely on the bundler)
        run: |
          set -eu
          codesign --options runtime --timestamp --force --deep --sign "$APPLE_SIGNING_IDENTITY" \
            "target/release/bundle/macos/Heterion Router.app"
      - name: verify before notarizing
        run: |
          set -eu
          APP="target/release/bundle/macos/Heterion Router.app"
          codesign --verify --deep --strict --verbose=2 "$APP"
          codesign -dv --verbose=4 "$APP" 2>&1 | tee sig.txt
          grep -q "flags=.*runtime" sig.txt
          codesign -dv --entitlements - "$APP" 2>&1 | tee ent.txt
          diff -u ../src-tauri/entitlements.macos.plist ent.txt || true
      - name: notarize + staple
        run: |
          set -eu
          xcrun notarytool submit "target/release/bundle/dmg/Heterion Router_0.1.0_aarch64.dmg" \
            --apple-id "$APPLE_ID" --password "$APPLE_PASSWORD" --team-id "$APPLE_TEAM_ID" --wait
          xcrun stapler staple "target/release/bundle/dmg/Heterion Router_0.1.0_aarch64.dmg"
          xcrun stapler staple "target/release/bundle/macos/Heterion Router.app"
      - name: gatekeeper acceptance
        run: |
          set -eu
          spctl --assess --type install --verbose=4 "target/release/bundle/dmg/Heterion Router_0.1.0_aarch64.dmg"
          spctl --assess --type exec --verbose=4 "target/release/bundle/macos/Heterion Router.app"
```

> `stapler staple` на `.app` внутри `bundle/macos/` имеет смысл только для
> публикации `.app.zip`; для DMG достаточно stapler на DMG. Оба оставлены,
> потому что оба артефакта публикуются (§7.6).

### 7.5 Пререквизиты подписи — чеклист блокеров

| # | Пререквизит | Текущее состояние | Как проверить |
|---|-------------|-------------------|---------------|
| P1 | Apple Developer Program (платная подписка) | **нет данных** | — |
| P2 | Сертификат `Developer ID Application: … (TEAMID)` в keychain | **E14: 0 identities** | `security find-identity -v -p codesigning` |
| P3 | `entitlements.macos.plist` | **отсутствует** (E23) | `test -f desktop/src-tauri/entitlements.macos.plist` |
| P4 | App Store Connect API key для `notarytool` | нет | `xcrun notarytool store-credentials` |
| P5 | Hardened Runtime | не включён (E12: ad-hoc) | `codesign -dv` → `flags=.*runtime` |
| P6 | Resources seal корректен | **падает** (E12) | `codesign --verify --deep --strict` |
| P7 | Минимальная macOS-версия определена | нет (`LSMinimumSystemVersion 10.13` из дефолта) | `bundle.macOS.minimumSystemVersion` |
| P8 | x86_64 slice, если нужен Intel | нет, только `aarch64` (E13/E21) | `lipo -archs` |
| P9 | Воспроизводимая точка сборки | **нет** (E22) | `git describe --tags --exact-match` |

Минимальный `desktop/src-tauri/entitlements.macos.plist` (создать):
```xml
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN"
  "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <!-- WKWebView needs JIT under the hardened runtime. -->
  <key>com.apple.security.cs.allow-jit</key><true/>
  <!-- The shell execs a nested binary; without this the hardened runtime
       refuses to load it even when signed by the same identity. -->
  <key>com.apple.security.cs.disable-library-validation</key><true/>
  <!-- Outbound to LLM providers. No app-sandbox: the sidecar must bind a
       loopback listener and share the user's SQLite, which a sandbox forbids. -->
  <key>com.apple.security.network.client</key><true/>
</dict>
</plist>
```
Осознанно **не** включаем `com.apple.security.app-sandbox` — под песочницей
sidecar не сможет писать в `~/.heterion-router` (ADR-001 shared boundary) и
не сможет слушать loopback без `network.server`. Это осознанный размен:
приложение остаётся outside-sandbox, а защита Gatekeeper обеспечивается
подписью + notarization.

### 7.6 Артефакты релиза

| Артефакт | Источник job'а | Подпись | Checksum |
|----------|----------------|---------|----------|
| `heterion-router-gateway-<triple>.tar.gz` | `release-gateway` | — | `.sha256` (после §7.3) |
| `heterion-router-gateway-<triple>.tar.gz.sha256` | `release-gateway` | — | — |
| `Heterion-Router_<ver>_aarch64.dmg` | `release-desktop` | notarized + stapled | в `SHA256SUMS` |
| `Heterion-Router_<ver>_x86_64.dmg` | `release-desktop` (matrix) | notarized + stapled | в `SHA256SUMS` |
| `Heterion-Router_<ver>_aarch64.app.zip` | `release-desktop` | notarized + stapled | в `SHA256SUMS` |
| `SHA256SUMS` | `release-desktop` | — | подписывается PGP/GPG, если есть ключ |
| `release-manifest.json` | `release-desktop` | — | — |

`release-manifest.json` — якорь воспроизводимости (закрывает P9):
```json
{
  "version": "0.1.0",
  "git_sha": "<full sha>",
  "git_describe": "desktop-v0.1.0",
  "built_at_utc": "2026-09-29T12:00:00Z",
  "runner": "macos-14",
  "toolchain": { "rustc": "1.98.1", "node": "24.x", "cargo_tauri": "2.11.4" },
  "bundle": {
    "identifier": "com.heterion.router",
    "minimum_system_version": "11.0",
    "architectures": ["aarch64"],
    "sidecar_sha256": "…",
    "app_sha256": "…"
  },
  "signing": { "identity": "Developer ID Application: … (TEAMID)", "team_id": "…" },
  "notarization": { "submission_id": "…", "stapled": true }
}
```

**Версия в 5 местах обязана совпадать** — иначе bundle id, `CFBundleVersion` и
`Cargo` metadata разъедутся. `version-consistency` job'а:
```sh
set -eu
V="$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)"
for f in desktop/src-tauri/Cargo.toml desktop/package.json desktop/ui/package.json; do
  case "$f" in
    */Cargo.toml)  G="$(sed -n 's/^version = "\(.*\)"/\1/p' "$f" | head -1)" ;;
    */package.json) G="$(node -p "require('./$f').version")" ;;
  esac
  [ "$G" = "$V" ] || { echo "version mismatch: $f has $G, workspace has $V"; exit 1; }
done
T="$(node -p "require('./desktop/src-tauri/tauri.conf.json').version")"
[ "$T" = "$V" ] || { echo "tauri.conf.json has $T, workspace has $V"; exit 1; }
```

---

## 8. Миграция и откат

### 8.0 Текущая точка отката: её нет

**E22**: HEAD `51dfe6d`, 35 modified + 5 untracked, ни одного тега. Бандл в
`/Applications` собран 2026-09-20 из другого состояния и дополнительно
пропатчен вручную (sidecar от 24.09 при бинаре от 20.09 — §B6).
**Откатываться не на что.** Это и есть первая задача.

### 8.1 Схема БД-миграций

**Отсутствует — намеренно.** Instance-token и readiness — процессное состояние
в ответе `/healthz`, не колонка. `MIGRATIONS` не меняется
(`migrate.rs:34-38`). Ни один шаг плана не требует бэкапа или дампа SQLite.

### 8.2 Ступени (каждая — отдельный коммит, обратима независимо)

| Шаг | Содержание | Влияние на пользователя | Откат |
|-----|-----------|------------------------|-------|
| **S0** | Коммит рабочего дерева; тег `pre-ops-2026-09-29`; зафиксировать `Cargo.lock`, `Cargo.lock` в `src-tauri` | нет | тег уже есть |
| **S1** | `stage-sidecar.sh` в `beforeDevCommand`; удалить stale `binaries/heterion-router-gateway`; `cargo fmt` на `desktop/src-tauri` | нет (только чинит fresh clone) | `git revert` |
| **S2** | `vite.config.js`: `VITE_PROXY_TARGET ?? 20129`; dev-скрипт с изолированным `HETERION_ROUTER_DATA_DIR=~/.heterion-router-dev`; `api.js`: убрать `VITE_GATEWAY_URL` и ветку 20128 | **dev-flow**: окно перестаёт бить в сервисную БД | revert; если БД уже испорчена — из `db_backups/` |
| **S3** | instance-token + handshake + retry в `App.svelte` + respawn | desktop стартует надёжнее; при неудаче — диалог вместо тихого обрыва | revert; приложение возвращается к текущему поведению |
| **S4** | server-side 409 для sidecar; UI различает 409; `started_unix`-poll вместо `sleep(2000)`; опционально Tauri command `restart_sidecar` | кнопка restart в десктопе перестаёт рестартовать сервис на 20128 | revert |
| **S5** | `scripts/service/*` (plist.in, install, uninstall, README); `restrict_data_files` → best-effort + перенос после `tracing init` | сервис ставится воспроизводимо; `0.0.0.0` → `127.0.0.1` по умолчанию | `uninstall.sh` + revert; живой plist **не трогать до явного согласия** |
| **S7** | CI: `desktop-rust`, `desktop-bundle`, `version-consistency`, `service-lint`; greps в `ui`; фикс checksum в `release.yml` | нет; `ci.yml` может стать красным на S1–S4, пока те не смержены | revert workflow |
| **S6** | `entitlements.macos.plist`, `bundle.macOS` в `tauri.conf.json`, `release-desktop` job, артефакты | появляется подписанный/notarized релиз | revert workflow + `tauri.conf.json`; уже выпущенные теги не отозвать |

**Порядок жёсткий: S0 → S1 → S2 → S3 → S4 → S5 → S7 → S6.**
Причина: S7 включает проверки, которые падают на не-merged S1–S4, а S6 требует
P1–P7 из §7.5 (внешние блокеры, могут занять недели).

### 8.3 Rollback-процедуры (конкретно)

**Откат desktop-сборки:**
```sh
git checkout <S_prev>
(cd desktop && npm --prefix ui ci && cargo tauri build)
rm -rf "/Applications/Heterion Router.app"
ditto "desktop/src-tauri/target/release/bundle/macos/Heterion Router.app" "/Applications/"
open "/Applications/Heterion Router.app"
```

**Откат сервиса (обязательно ДО любых правок живого plist):**
```sh
cp ~/Library/LaunchAgents/com.heterion.router.plist /tmp/com.heterion.router.plist.bak
# ... менять ...
launchctl bootout gui/$(id -u)/com.heterion.router
cp /tmp/com.heterion.router.plist.bak ~/Library/LaunchAgents/com.heterion.router.plist
launchctl bootstrap gui/$(id -u) ~/Library/LaunchAgents/com.heterion.router.plist
curl -fsS http://127.0.0.1:20128/healthz
```

**Откат release-артефакта:** теги `desktop-v*` иммутабельны; отзыв = `gh release
delete <tag> --yes` + публикация `<tag+1>`. Notarization необратима, поэтому
версии не переиспользуются никогда.

**Откат не-релизного CI:** revert workflow-файла; runner state не затрагивается.

### 8.4 Что может сломаться при миграции и как страхуемся

| Риск | Страховка |
|------|----------|
| S2 меняет порт dev-прокси — у контрибьютора не поднимется gateway | README/AGENTS-правка в том же коммите; `VITE_PROXY_TARGET` оставляет override |
| S2 меняет data-dir дев-окна — потеряет dev-данные | дев-дир изолирован; прод-дир не трогается |
| S3 handshake не срабатывает — приложение не открывается | дедлайн 20 с → явная ошибка вместо тихого; тест AT-05 |
| S3 респавн вводит цикл kill/spawn | бюджет 3 попытки, backoff, лог каждой |
| S4 409 ломает привычный restart из UI | 409 уже документирован в ADR-006; текст ошибки различает причины |
| S5 `install.sh` указывает на dev-дерево | скрипт отказывает без явного `HETERION_ROUTER_BIN` |
| S5 смена `0.0.0.0` → `127.0.0.1` рвёт LAN-доступ | LAN — явный opt-in `@HOST@`; README фиксирует требование TLS/VPN/ключа |
| S6 notarytool отвергает (unsigned nested sidecar) | sidecar подписывается тем же identity (Tauri делает это по умолчанию); проверка `codesign -dv` обоих бинарей в шаге verify |

---

## 9. Измеримые acceptance-тесты

Каждый тест — команда с порогом. `✔` = воспроизводим сейчас или после
соответствующего шага; `⚠` = требует окружения, которого сейчас нет.

### 9.1 S1 — сборка

| ID | Тест | Команда | Порог | Статус |
|----|------|---------|-------|--------|
| AT-01 | Fresh clone собирает desktop | `git clone <repo> /tmp/fr && cd /tmp/fr && (cd desktop && cargo tauri build --bundles app 2>&1 | tail -5)` | exit 0, нет `doesn't exist` | ⚠ сейчас **падает** (E25) |
| AT-02 | Tauri-крейт отформатирован | `cargo fmt --manifest-path desktop/src-tauri/Cargo.toml -- --check` | exit 0 | ⚠ сейчас **exit 1** (E1) |
| AT-03 | Sidecar в bundle совпадает с workspace | `shasum -a 256 target/release/heterion-router-gateway` vs `…/bundle/macos/Heterion Router.app/Contents/MacOS/heterion-router-gateway` | хеши равны | ✔ (после сборки) |
| AT-04 | Нет stale-файлов в `binaries/` | `ls desktop/src-tauri/binaries/ \| wc -l` | `1` | ⚠ сейчас `2` (E21) |

### 9.2 S2 — dev-изоляция (защита от B2)

| ID | Тест | Команда | Порог | Статус |
|----|------|---------|-------|--------|
| AT-05 | Dev-прокси не указывает на сервис | `grep -c '20128' desktop/ui/vite.config.js` | `0` | ⚠ сейчас `1` |
| AT-06 | Dev-окно не трогает прод-БД | запустить `tauri dev`, `PATCH /api/connections/:id` через прокси, затем `sqlite3 ~/.heterion-router/storage.sqlite "select updated_at from provider_connections order by updated_at desc limit 1"` | `updated_at` **не** меняется | ⚠ требует GUI |
| AT-07 | Dev изолирован по data-dir | `ls -d ~/.heterion-router-dev` после `tauri dev` | существует, `0700` | ⚠ требует GUI |
| AT-08 | В бандл не запечён gateway URL | `grep -rc '127.0.0.1:2012' desktop/ui/dist/assets/*.js` | `0` | ⚠ требует build |
| AT-09 | `VITE_GATEWAY_URL` в окружении не ломает релиз | `cd desktop/ui && VITE_GATEWAY_URL=http://1.2.3.4:9 npm run build && grep -rc '1.2.3.4' dist/assets` | `0` | ⚠ |

### 9.3 S3 — handshake и владение

| ID | Тест | Команда | Порог | Статус |
|----|------|---------|-------|--------|
| AT-10 | Health отдаёт токен | `curl -s http://127.0.0.1:20129/healthz \| jq -e '.instance_token and .ready==true and .started_unix'` | truthy | ⚠ требует S3 |
| AT-11 | Токен уникален на инстанс | два запуска подряд, `jq -r .instance_token` | значения различаются | ⚠ |
| AT-12 | Приложение стартует при холодной БД | удалить `storage.sqlite-wal`, запустить .app, замерить до появления `status: ok` в UI | `< 20 с` (дедлайн handshake) | ⚠ требует GUI |
| AT-13 | **Чужой слушатель отвергается** | `nc -l 127.0.0.1 20129` (подделать ответ без токена), запустить .app | приложение показывает ошибку, **не** подключается | ⚠ |
| AT-14 | Sidecar умирает вместе с родителем | запустить .app, `pkill -9 -f "Heterion Router.app/Contents/MacOS/heterion-router-desktop"`, через 5 с `lsof -nP -iTCP:20129 -sTCP:LISTEN` | пусто | ⚠ требует GUI |
| AT-15 | Стейл-файл не остаётся | то же + `ls desktop/src-tauri/binaries/` | один файл | ✔ |
| AT-16 | UI переживает медленный старт | подделать задержку `migrate()` (например, big DB), запустить .app | sidebar показывает `routing · ok`, не `unreachable` | ⚠ |

### 9.4 S4 — restart

| ID | Тест | Команда | Порог | Статус |
|----|------|---------|-------|--------|
| AT-17 | Sidecar честно отказывает | `curl -s -o /dev/null -w '%{http_code}' -XPOST http://127.0.0.1:20129/api/restart -H "authorization: Bearer $KEY"` | `409` | ⚠ сейчас `409` только если job'а нет; на этой машине job есть ⇒ **не 409** |
| AT-18 | Restart из UI не трогает 20128 | `PID_BEFORE=$(lsof -t -iTCP:20128 -sTCP:LISTEN)`; нажать «Restart now» в десктопе; `PID_AFTER=$(…)` | равны | ⚠ |
| AT-19 | Restart сервиса из UI меняет `started_unix` | `A=$(curl -s …/healthz\|jq .started_unix)`; restart; poll до смены | `B != A` за `< 30 с` | ✔ для сервиса на 20128 |
| AT-20 | Нет гонки 2000 мс | 50 итераций restart, ни одной с `unreachable` в UI | `0` ошибок | ⚠ |

### 9.5 S5 — сервис

| ID | Тест | Команда | Порог | Статус |
|----|------|---------|-------|--------|
| AT-21 | Plist проходит `plutil -lint` | `plutil -lint ~/Library/LaunchAgents/com.heterion.router.plist` | `OK` | ✔ (E6: plist существует) |
| AT-22 | Install идемпотентен | `sh scripts/service/install.sh && sh scripts/service/install.sh` | оба exit 0 | ⚠ |
| AT-23 | Uninstall чист | `sh scripts/service/uninstall.sh; launchctl print gui/$(id -u)/com.heterion.router` | job отсутствует, plist удалён, `~/.heterion-router` **цел** | ⚠ |
| AT-24 | Plist не указывает на dev-дерево | `plutil -p …plist \| grep ProgramArguments` | путь не содержит `/ai-workstation/Projects/` | ⚠ сейчас **указывает** (E6) |
| AT-25 | Bind по умолчанию — loopback | `lsof -nP -iTCP:20128 -sTCP:LISTEN` | `127.0.0.1:20128` | ⚠ сейчас `*:20128` (E9) |
| AT-26 | Non-loopback без ключа не стартует | `HETERION_ROUTER_HOST=0.0.0.0 … binary` на пустой БД | exit != 0, `non-loopback gateway requires at least one API key` | ✔ логика в `main.rs:370-384` + тест `non_loopback_requires_a_key_before_startup` |
| AT-27 | `cargo build --release` не ломает живой job | бинарь = `$ROOT/target/release/…`? | **нет** | ⚠ сейчас **да** (E6) |
| AT-28 | Плохая правка в data-dir не даёт crash-loop | `chmod 0400 ~/.heterion-router/storage.sqlite`, `kickstart -k` | процесс жив, ошибка в `gateway.log`, не exit | ⚠ |

### 9.6 S6 — подпись и notarization

| ID | Тест | Команда | Порог | Статус |
|----|------|---------|-------|--------|
| AT-29 | Внутренняя консистентность подписи | `codesign --verify --deep --strict --verbose=2 "…/Heterion Router.app"` | exit 0 | ⚠ сейчас **падает** (E12) |
| AT-30 | Gatekeeper принимает | `spctl -a -vvv -t exec "…/Heterion Router.app"` | `accepted` | ⚠ сейчас **отказ** (E11) |
| AT-31 | Hardened runtime включён | `codesign -dv --verbose=4 … \| grep flags` | содержит `runtime` | ⚠ |
| AT-32 | Оба бинаря подписаны одним identity | `codesign -dv` на `.app` и на `Contents/MacOS/heterion-router-gateway` | `Authority=Developer ID Application` у обоих | ⚠ |
| AT-33 | Entitlements на месте | `codesign -dv --entitlements - …` | `allow-jit`, `disable-library-validation` | ⚠ |
| AT-34 | Notarized | `spctl -a -vvv -t install …dmg` + `xcrun stapler validate …dmg` | `accepted` + `valid` | ⚠ требует P1–P4 |
| AT-35 | Свежая установка на чистой macOS | удалить .app, смонтировать .dmg, перетащить, запустить | стартует без `нельзя открыть, т.к. разработчик не проверен` | ⚠ |

### 9.7 S7 — CI и release

| ID | Тест | Команда | Порог | Статус |
|----|------|---------|-------|--------|
| AT-36 | `scripts/ci/check.sh` зелёный | `sh scripts/ci/check.sh` | exit 0 | ⚠ полный прогон не ждал (E3) |
| AT-37 | Tauri-крейт в CI | job `desktop-rust` | зелёный на PR | ⚠ |
| AT-38 | Версии согласованы | `sh scripts/ci/check-version.sh` | exit 0 | ⚠ |
| AT-39 | Checksum переносим | `sh -c 'command -v sha256sum || echo shasum'` на ubuntu и macos-14 | один из двух доступен | ⚠ macos-14 **не проверен** |
| AT-40 | Plist валиден в CI | `xmllint --noout` + проверка обязательных ключей | exit 0 | ⚠ |
| AT-41 | Артефакты релиза полны | `gh release view <tag> --json assets -q '.assets[].name'` | есть dmg ×2, app.zip, SHA256SUMS, release-manifest.json, gateway tar.gz ×2 | ⚠ |

### 9.8 Регрессии, которые нельзя допустить

| ID | Проверка | Порог |
|----|----------|-------|
| AT-42 | `cargo test --workspace` | 213 тестов, 0 падений (по ROADMAP:32) |
| AT-43 | `cargo clippy --workspace --all-targets -- -D warnings` | exit 0 |
| AT-44 | Контракт `STATIC_SALT`/`enc:v1:` не тронут | `rg 'omniroute-field-encryption-v1' crates/` — строка в `crypto` |
| AT-45 | Legacy `OMNIROUTE_*` fallback работает | тесты `legacy_omniroute_spellings_still_work`, `label_prefers_new_name_then_legacy_then_default` |
| AT-46 | Keyless-провайдер не шлёт `Authorization` | тест `keyless_provider_sends_no_bearer` |
| AT-47 | 4xx провайдера не роняет breaker | существующие breaker-тесты |

---

## 10. Открытые вопросы (требуют решения владельца, не инженера)

1. **Apple Developer Program.** Без P1/P2 S6 не начинается в принципе. Если
   подписка не нужна — цель S6 сворачивается, и S1–S5+S7 становятся полным
   релизом «developer-machine distribution» (локальный `cargo tauri build`,
   раздача `.app` по AirDrop). Это осознанный и приемлемый режим; S6 тогда
   переводится в бэклог.
2. **Разделение dev/прод data-dir (S2).** Sidecar намеренно делит БД с
   сервисом. Но dev-окно — нет. Нужен ли третий, «stage»-data-dir? Моё
   мнение: нет, YAGNI. Один dev-дир достаточно.
3. **Динамический порт sidecar vs фиксированный 20129.** Динамический убирает
   целый класс конфликтов, но ломает привычку «sidecar всегда на 20129»
   (документирована в AGENTS.md:35-36, README:46). Рекомендация: динамический,
   с `20129` как fallback, если `bind(":0")` не удался; обновить AGENTS/README.
4. **Архитектуры.** Сейчас только `aarch64` (E13/E21). Нужен ли `x86_64` —
   вопрос к владельцу; без ответа релизим `aarch64` и честно это пишем.
5. **Auto-updater.** Вне scope. Плана нет ни в `tauri.conf.json`, ни в
   зависимостях. Notarized DMG + ручное обновление — допустимый v0.1.
6. **CSP.** `app.security.csp: null` (`tauri.conf.json:19-21`). Для notarized
   релиза разумно включить минимальный CSP. Не включён в S1–S7, т.к. требует
   аккуратного подбора под `tauri://localhost` и inline-скрипт инжекта
   (`src/lib.rs:85`). Предлагаю отдельной задачей перед S6.

---

## 11. Сводка «что исполнено / что не исполнено»

**Исполнено на машине 2026-09-29 (28 проверок, §1.2):** E1–E28. Из них
принципиально новые по сравнению с существующим
`docs/research/PROJECT-AUDIT-2026-09.md`:

- **E25** — воспроизведённый отказ сборки на fresh clone (B1). В существующем
  аудите этого нет.
- **E12** — `codesign --verify --deep --strict` падает на **свежей** сборке
  2026-09-29 (аудит проверял установленный бандр от 20.09).
- **E13 + §B6** — установленный `/Applications` бандл содержит sidecar от
  24.09 при бинаре от 20.09, то есть подложен вручную; это не выход
  `cargo tauri build`.
- **E16** — `/sbin/sha256sum` **есть** на macOS 26.7; формулировка аудита
  («не macOS-родное написание») верна по происхождению, но практический отказ
  на образе `macos-14` **не подтверждён** и **не опровергнут** (not executed).
- **E14 + E23** — 0 signing identities и ноль конфигурации notarization.
- **E1** — точный состав расхождений форматирования в исключённом крейте.
- **E27** — `busy_timeout=2000` / WAL как основание для допустимости общего
  data-dir.
- **E28** — подтверждено, что репозиторий не изменён.

**Не исполнено (перечень §1.3).** Все шаги S0–S7 и все acceptance-тесты со
статусом `⚠` — это проектные предложения, а не наблюдения. Ни одно утверждение
о notarized-артефакте, `desktop-bundle` job'е или `release-desktop` job'е не
может считаться проверенным до выполнения шагов S1–S7.

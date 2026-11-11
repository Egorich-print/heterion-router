# Heterion Router

Heterion Router — OpenAI-совместимый шлюз к LLM-провайдерам на Rust: один `baseURL` и один свой API-ключ вместо ключа каждого провайдера, роутинг по комбо с failover, кэш, бюджеты, guardrails, аудит и встроенный админ-интерфейс — веб-дашборд на Svelte и нативное приложение на GPUI. Шлюз держит ключи провайдеров в своей зашифрованной БД, а клиентам (OpenAI SDK, Claude Code, Cursor, агенты) выдаёт уже свои ключи.

![License](https://img.shields.io/badge/license-GPL--3.0--or--later-blue.svg)
![Platform](https://img.shields.io/badge/gateway-Linux%20%7C%20macOS%20%C2%B7%20desktop-macOS-lightgrey.svg)
![Version](https://img.shields.io/badge/version-0.1.0-informational.svg)

![Heterion Router](desktop/gpui/packaging/icons/icon.png)

## Быстрый старт для нового пользователя

Терминал и `curl` для настройки **не нужны**: всё делается в приложении.

1. **Установка.** Соберите или возьмите бандл `Heterion Router.app` и перетащите его в `/Applications`.
   Подпись приложения — ad-hoc, поэтому для первого запуска на чужом Mac нужен один из двух шагов:
   правой кнопкой по приложению → «Открыть», либо снять карантин:
   ```sh
   xattr -cr "/Applications/Heterion Router.app"
   ```
   Подробности — в [`docs/INSTALL.md`](docs/INSTALL.md).

2. **Первый запуск.** Приложение само поднимает свой sidecar-шлюз на порту `20129`
   и работает с каталогом данных `~/.heterion-router` (legacy-каталог `~/.omniroute`
   читается, но новый используется по умолчанию).

3. **Первый ключ.** На экране входа есть кнопка **Create first key**: на свежей БД
   аутентификация открыта, поэтому ключ выпускается прямо из приложения. Он сохраняется
   в `<data dir>/desktop/operator-key.json` (файл `0600`, каталог `0700`) и используется
   и для входа, и для запросов к `/v1/*`.

4. **Ключ провайдера.** Раздел **Providers** → **Add provider** → выберите провайдера
   из каталога → вставьте свой API-ключ провайдера → **Save**. Ключ шифруется и ложится
   в БД шлюза (префикс `enc:v1:`), в открытом виде он больше не показывается. Чтобы
   провайдер начал обслуживаться, перезапустите шлюз (**Restart now**).

5. **Проверка.** Вызов OpenAI-совместимого эндпоинта (порт sidecar-шлюза — `20129`,
   у установленного сервиса — `20128`):
   ```sh
   curl http://127.0.0.1:20129/v1/chat/completions \
     -H "Authorization: Bearer sk-ваш-ключ" \
     -H "Content-Type: application/json" \
     -d '{
       "model": "имя-комбо-или-модели",
       "messages": [{"role": "user", "content": "Привет"}]
     }'
   ```
   `sk-ваш-ключ` — плейсхолдер: подставьте ключ, выпущенный в приложении.

## Возможности

- **Роутинг по комбо.** Вместо выбора одной модели — список кандидатов с приоритетами;
  шлюз сам выбирает живой backend и переключается на следующего.
- **Failover и breaker.** Транзиентные ошибки и обрывы до первого байта приводят к
  переходу на следующего кандидата; breaker отключает заведомо мёртвый backend.
  4xx от провайдера breaker не роняют.
- **Кэш с разделением по bearer.** Ответы кэшируются (TTL — час) и разнесены по
  предъявленному ключу: один ключ не читает ответы, закешированные другим.
  Стриминг кэш обходит и на чтении, и на записи.
- **Бюджеты.** Дневные лимиты и лимиты запросов на ключ; по умолчанию всё безлимитно.
- **Guardrails.** Проверки входящих запросов (по умолчанию — режим `warn`): в тело,
  заголовок и лог попадает только имя проверки, совпадение — никогда.
- **Аудит и атрибуция.** `call_logs` и `usage_history` с реальной атрибуцией к
  провайдеру, подключению и комбо, а не заглушкой `provider = "routing"`.
- **Стриминг.** SSE для всех поддерживаемых форматов, включая Responses.
- **Форматы API.** OpenAI Chat (`/v1/chat/completions`), OpenAI Responses
  (`/v1/responses`), Anthropic Messages (`/v1/messages`), Embeddings
  (`/v1/embeddings`), список моделей (`/v1/models`).
- **Поиск Exa как модель.** `exa/exa-search` — обычная модель в чат-поверхности:
  запрос — последнее сообщение пользователя, ответ — цитированный блок.
- **Нативный десктоп.** Приложение на GPUI (не Electron, не Tauri): свой sidecar-шлюз,
  свой операторский ключ в файле, разделы **Overview**, **Combos**, **Providers**, **Keys**,
  **Usage**, **Logs**, **Playground**.
- **Веб-дашборд.** Тот же admin-API, что и у десктопа, на Svelte 5 + Vite; шлюз
  раздаёт его сам.

## Сборка из исходников

Требования:

- **Rust 1.99.1** — пин в `rust-toolchain.toml`; edition `2024`, MSRV манифестов `1.99`.
- **Node.js 24** — только для веб-дашборда.
- **macOS + Xcode** — для десктопа: gpui компилирует Metal-шейдеры через `xcrun metal`.

```sh
# шлюз
cargo build -p heterion-router-gateway

# веб-дашборд (собирается в desktop/ui/dist и раздаётся шлюзом)
npm --prefix desktop/ui ci && npm --prefix desktop/ui run build

# нативное приложение macOS → desktop/gpui/dist/Heterion Router.app
desktop/scripts/build-app.sh
```

Проверка качества — ровно то, что гоняет CI:

```sh
scripts/ci/check.sh    # cargo fmt --all && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace
```

Запуск сервиса на macOS (plist машинно-локальный, в репозитории не хранится):

```sh
scripts/deploy-service.sh
launchctl kickstart -k gui/$(id -u)/com.heterion.router   # если job уже установлен
curl -s http://127.0.0.1:20128/healthz
```

## Карта репозитория

| Путь | Что внутри |
| --- | --- |
| `crates/heterion-router-{config,core,crypto,db,gateway,http,providers,routing,translate,usage}/` | Workspace: конфиг, ядро, крипто (`enc:v1:`), БД (SQLite), axum-шлюз, HTTP-клиент, реестр провайдеров, роутинг, перевод форматов, учёт usage. |
| `crates/heterion-router-gateway/` | Бинарь `heterion-router-gateway` — `/v1/*`, `/api/*`, раздача дашборда. |
| `crates/heterion-router-providers/registry.json` | Каталог провайдеров и моделей, из которого берётся UI-список. |
| `desktop/gpui/` | Нативная оболочка на GPUI (вне workspace, собирается отдельно). |
| `desktop/ui/` | Веб-дашборд на Svelte 5 + Vite. |
| `desktop/scripts/build-app.sh` | Сборка `.app`: оболочка + sidecar-шлюз, иконка, `Info.plist`, ad-hoc подпись. |
| `docs/` | Документация: решения (ADR), отчёты, шпаргалка агента. |
| `scripts/` | `deploy-service.sh` (launchd-сервис), `ci/check.sh` (гейт CI). |
| `.github/workflows/`, `Jenkinsfile` | CI: GitHub Actions и локальный Jenkins. |

## Провайдеры

Каталог провайдеров лежит в `crates/heterion-router-providers/registry.json`; это
сотни записей, но список в интерфейсе показывает только те, что проходят
структурную проверку. Полный разбор — каталог, авторизация, форматы, как добавить
ключ — в [`docs/PROVIDERS.md`](docs/PROVIDERS.md).

Ключевой провайдер `heterion-local` работает без ключа (keyless, openai-формат) и
указывает на локальный инференс-сервер Heterion; адрес переопределяется переменной
`HETERION_LOCAL_BASE_URL`.

## Установка на macOS

Подробная инструкция — в [`docs/INSTALL.md`](docs/INSTALL.md): бандл `.app`, первый
запуск, выпуск ключей, диагностика. Если коротко: приложение подписывается ad-hoc и
не нотарифицируется, поэтому для первого запуска на чужом Mac нужен один из двух
шагов (см. раздел «Быстрый старт»).

## API

Публичный API (OpenAI-совместимый):

| Метод и путь | Что это |
| --- | --- |
| `POST /v1/chat/completions` | Чаты, стриминг SSE. |
| `POST /v1/responses` | Responses-протокол OpenAI. |
| `POST /v1/messages` | Anthropic-формат сообщений. |
| `POST /v1/embeddings` | Эмбеддинги. |
| `GET /v1/models` | Список доступных моделей и комбо. |
| `GET /healthz` | Проверка живости, публичен. |

Admin-API (`/api/*`) — обзор, комбо, подключения, ключи, usage, журнал, бюджеты,
guardrails; требует ключ оператора со scope `manage`. Полный контракт — в
[`docs/AGENT-ROUTER-GUIDE.md`](docs/AGENT-ROUTER-GUIDE.md).

## Происхождение проекта

Heterion Router вырос из **OmniRoute** — шлюза к LLM-провайдерам, который
начинался как JavaScript-проект. Это самостоятельный проект: он **не аффилирован**
с OmniRoute и его авторами, не поддерживается ими и не одобрен ими.

Почти весь код написан заново. Ядро шлюза — новая реализация на Rust
(axum, Tokio, rusqlite): роутинг, failover, кэш, бюджеты, guardrails, шифрование
полей и учёт usage. Нативное приложение macOS написано с нуля на GPUI. На
JavaScript остался только веб-дашборд `desktop/ui` — небольшой, и он тоже написан
в ходе порта. Общими с OmniRoute остаются сама идея, часть форматов данных и
несколько констант совместимости (например, соль шифрования
`omniroute-field-encryption-v1` и поддержка legacy-строк в таблице ключей).

Авторские права на выведенный материал сохранены: уведомление об исходном
проекте и его авторе — в [`LICENSE`](LICENSE), подробности — в
[`THIRD_PARTY_NOTICES.md`](THIRD_PARTY_NOTICES.md). Исходники OmniRoute:
<https://github.com/diegosouzapw/OmniRoute>.

## Лицензия

**GPL-3.0-or-later.** Полный текст — в [`LICENSE`](LICENSE).

## Документы

| Что | Где |
| --- | --- |
| Текущий статус проекта | [`STATUS.md`](STATUS.md) |
| Решения (ADR) | [`docs/adr/`](docs/adr/) |
| Шпаргалка для агента: модели, комбо, exa-search | [`docs/AGENT-ROUTER-GUIDE.md`](docs/AGENT-ROUTER-GUIDE.md) |
| Поддерживаемые провайдеры | [`docs/PROVIDERS.md`](docs/PROVIDERS.md) |
| Установка на macOS | [`docs/INSTALL.md`](docs/INSTALL.md) |
| Changelog | [`CHANGELOG.md`](CHANGELOG.md) |

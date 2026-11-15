# Camoufox-контур: отчёт по состоянию на 2026-11-15

Документ для владельца: что установлено, как настроено, что собрано, что
требует ручного шага. Технические решения — в
[`docs/adr/ADR-025-web-executors.md`](../adr/ADR-025-web-executors.md).

## 1. Что установлено

| часть | версия | где |
|---|---|---|
| Camoufox (Python) | 0.5.9 | `~/.local/share/camoufox/venv` (uv, Python 3.13) |
| Camoufox browser | 158.0.1-beta.41-9038f86d | `~/Library/Caches/camoufox/browsers/official/` |
| Playwright | 1.62.0 | venv |
| browser-cookie3 | 0.20.1 | venv (перенос сессии Vivaldi) |
| драйвер веб-исполнителей | — | `~/.local/share/camoufox/scripts/webdriver/server.py` (порт **20131**) |
| профиль оператора | 160 МБ | `~/.local/share/camoufox/profile` (постоянный, `cookies.sqlite` 512 КБ) |

Контур **вне репозитория**: он работает с профилем оператора, а не с кодом
шлюза, и в git не попадает.

Настройки запуска (`scripts/cf.py`): `os=macos` (отпечаток совпадает с
машиной), `geoip`, `humanize` (человеческие паузы), `block_webrtc`,
`persistent_context=True` — сессия переживает перезапуск, **`locale=de-DE`**
(см. ниже).

**Локаль закреплена намеренно.** С одним только `geoip` Camoufox выбирал язык
интерфейса заново при каждом запуске (de/fr/en/ru/zh/zh-TW), и подпись кнопки
в дашборде становилась непредсказуемой: «Neuer Schlüssel» в одном прогоне и
«Nouvelle clé» в следующем. Это ломало автоматизацию сильнее любой другой
мелочи. С закреплённой локалью подписи стабильны.

## 2. Сессия

Перенесено **1882 cookie** из Vivaldi по **617 доменам**. Инвентаризация:
**66 провайдеров реестра** с живой сессией — 27 с активным подключением в
роутере (категория A) и 36 только с сессией (категория B).

Входы, сделанные автоматически через Google-аккаунт оператора (собственная
учётная запись, без обхода 2FA): cline, kilo-gateway, xai, glm/glmt, novita,
cerebras, maritalk, kimi, bazaarlink (egorichkrypto78@gmail.com), deepseek
(не завершён), huggingchat (требует пароль), gitlab-duo (OAuth, не API-ключ).

У bazaarlink при первом выпуске ключа показывается согласие с условиями
(18+, контент-безопасность) — оно принято один раз в аккаунте оператора,
это обязательный шаг онбординга перед выпуском ключа.

## 3. Ключи в роутере

Записаны через admin-API (`POST /api/provider-connections` + `PATCH
is_active:false` для старых подключений):

| провайдер | ключ | статус |
|---|---|---|
| upstage | `up_cCz…ni9XF` | 403 — нет кредитов (нужна оплата) |
| cline | `sk_9b8d…712bb6` | 402 — нулевой баланс |
| xai | `xai-eKJo…zUj` | 403 — у команды нет кредитов/лицензий |
| kilo-gateway | **keyless** (`api_key: ""`) | 200 — провайдер отвергает любой bearer |
| bazaarlink | `sk-bl-…` (54 симв.) | **200** на `auto:free`; платные модели → 402 (нет баланса) |
| ai21 | UUID | endpoint провайдера выведен из обслуживания (410) |
| minimax | `sk-api-…` | ключ жив; конкретная модель в каталоге недоступна (404) |
| novita | `sk_…` | 403 NOT_ENOUGH_BALANCE |
| glm, glmt | `32hex.16chars` (z.ai) | записаны |
| dahl | 64 hex | **невалиден** — 401 invalid_api_key |

Перечень моделей bazaarlink в реестре заменён на живой каталог площадки
(126 id из `GET /v1/models`): 17 прежних id площадка уже не отдаёт, 93 живых
id в реестре не было.

Ключи пишутся в роутер сразу по мере сбора; секреты в отчёты и логи не
попадают.

## 4. Веб-исполнители (ADR-025)

Девять веб-провайдеров реестра (`executor: "web"`, `auth_type: "none"`):
подписка ChatGPT/Claude/Gemini/Grok/Kimi/DeepSeek/Z.ai не даёт API-ключа, и
выдумывать его нельзя. Ответ приходит из сессии оператора в браузере: шлюз
(`crates/heterion-router-gateway/src/web.rs`) говорит с Python-драйвером
Camoufox по локальному HTTP.

### Правила честности

- **usage = 0** — веб-чат не отдаёт счётчики токенов; выдуманный ноль — это
  фальшивое число.
- **Стриминг = один дельта-чанк и `stop`** — страница отдаёт готовый текст.
- **Четыре различимые ошибки драйвера:** `not_logged_in` (вход),
  `needs_operator` (2FA/капча — оператор), `selector_miss` (верстка
  изменилась — код адаптера), `upstream_error`.
- **Бэкенд поднимается только для провайдеров, у которых есть адаптер.**
  Драйвер отвечает `GET /providers`, шлюз регистрирует только их: провайдер
  без адаптера в каталоге не появляется и не падает на каждом запросе.

### Состояние (живые проверки 2026-11-15)

| провайдер | адаптер | ответ через роутер |
|---|---|---|
| `chatgpt-web` | есть | **200**, живой текст |
| `claude-web` | есть | **200**, живой текст |
| `gemini-web` | есть | **200**, живой текст |
| `grok-web` | есть | **200**, живой текст |
| `kimi-web` | есть (после входа) | **200**, живой текст |
| `zai-web` | есть | **200**, живой текст |
| `deepseek-web` | нет | вход через Google не завершается |
| `blackbox-web` | нет | сайт сменился на корпоративный, чата нет |
| `chatgpt-web-codex` | нет | раздел Codex для аккаунта Free недоступен |

Каталог после деплоя: **565 моделей**, из них 126 моделей bazaarlink и 29
веб-моделей шести провайдеров.

## 5. Что требует ручного шага

1. **Вход в DeepSeek — нужен один hCaptcha.** Google-OAuth больше не проходит
   без проверки: по клику на «Mit Google anmelden» площадка показывает
   drag-головоломку hCaptcha («перетащите букву»). Это шаг оператора — обход
   CAPTCHA в контуре не заводится. В headless-браузере OAuth не запускается
   вовсе, поэтому шаг — в видимом окне Camoufox (`scripts/login_deepseek.py
   --headed`). После капчи OAuth доходит до экрана согласия Google
   («Продолжить»), и сессия сохраняется в профиле.
2. **Вход в Hugging Face** — требует «Confirm your identity» (пароль).
3. **GitLab Duo** — регистрация через OAuth, API-ключа не существует.
4. **Кредиты** — решение владельца, покупок не делалось. Техническая часть
   закрыта: провайдеры без баланса отключены (см. ADR-026), бесплатные пути
   оставлены в каталоге.
5. **Mistral / codestral** — план Free запрещает выпуск API-ключей вовсе
   («Upgrade, um deine API Keys zu aktivieren»), апгрейд невозможен. Рабочий
   бесплатный путь найден: `mistral/codestral-latest` отвечает 200 через
   уже имеющиеся ключи провайдера `mistral`.
6. **Ключи без страницы выпуска**: routeway, nara, qoder, opencode-zen
   (оплата WeChat Pay), cohere и cheaperinference (застряли на онбординге),
   routeway/nara — страница ключа не найдена.

## 6. Как запускать

```sh
# драйвер веб-исполнителей — под launchd, поднимается сам
launchctl list | grep com.heterion.webdriver
curl -s http://127.0.0.1:20131/health
curl -s http://127.0.0.1:20131/providers

# сервис роутера
cd /Volumes/Netac_SSD/ai-workstation/Projects/heterion-router
./scripts/deploy-service.sh
launchctl kickstart -k gui/$(id -u)/com.heterion.router
```

Диагностика селекторов адаптера — через `GET /probe?provider=<id>` драйвера,
а не на глаз: селекторы меняются вместе с вёрсткой провайдера.

---

## Раздел 2. Бесплатные замены платных сервисов Camoufox (2026-11-15)

Промо-список Camoufox предлагает десять сервисов: три поисковых/скрапинговых
API (SerpApi, Scrappey, Cloro) и один набор «веб-данных» (Thordata), пять
провайдеров прокси (NodeMaven, OkkProxy, NiuProxy, Node Proxy, Byteful) и
сервис репутации IP (Layer3 Intel). Что из этого можно поставить бесплатно,
а что остаётся платным по существу.

### Бесплатно и уже работает

| вместо | чем | команда |
|---|---|---|
| Scrappey (scraping API) | `scripts/scrape.py` — рендер Camoufox, извлечение текста/markdown/ссылок/скриншота | `./venv/bin/python scripts/scrape.py URL --format markdown` |
| SerpApi (SERP) | `scripts/search.py` — движки `ddg` (DuckDuckGo через Camoufox), `bing`, `searxng` (публичные инстансы, тоже через браузер: на обычный запрос отдают JS-челлендж), `exa` | `./venv/bin/python scripts/search.py "запрос"` |
| Cloro (SERP + поиск по ИИ) | `scripts/search.py --engine exa` — ключ оператора уже в роутере (`exa/exa-search`); плюс веб-исполнители `chatgpt-web`/`gemini-web`/`grok-web` отвечают из той же сессии | `./venv/bin/python scripts/search.py "запрос" --engine exa` |
| Thordata (SERP API, Web Scraper API) | те же `search.py` и `scrape.py` | то же |
| Layer3 Intel (репутация IP) | `scripts/proxycheck.py` — ip-api.com (флаги `proxy`/`hosting`), ipwho.is (страна, ASN, провайдер), ipinfo.io (сеть) | `./venv/bin/python scripts/proxycheck.py --me` |

Проверено живьём 2026-11-15: выдача DuckDuckGo, Bing, SearXNG и Exa приходит,
`scrape.py` рендерит страницу, `proxycheck.py` верно определяет адрес машины
(AS50436 Tele Columbus AG, флагов прокси/хостинга нет — «чисто»).

SearXNG публичные инстансы отвечают только через браузер: на обычный запрос
`searx.be` отдаёт страницу «Verifying your browser…». Свой SearXNG в podman не
поднимался — на машине podman без Linux-VM, а разворачивать VM без решения
владельца не следует.

Чего бесплатные источники **не** дают: базы «кто и где продаёт этот IP»
(Layer3 Intel) и residential-пула. Списки бесплатных прокси ненадёжны и часто
вредоносны — подменять ими платный пул нельзя.

### Платное, но установленное как проводка

Прокси-профили ведутся в `~/.local/share/camoufox/state/proxy.json` (0600):

```sh
./venv/bin/python scripts/proxy_config.py set --name nodemaven \
    --server http://host:port --user U --pass P
./venv/bin/python scripts/proxy_config.py test --name nodemaven   # адрес, который видит сайт
./venv/bin/python scripts/proxy_config.py show                      # пароли скрыты
```

Профиль подхватывают `scrape.py --proxy <имя>`, `search.py --proxy <имя>`,
`proxycheck.py --proxy <имя>` и веб-драйвер (`HETERION_WEB_DRIVER_PROXY=<имя>`
при запуске `scripts/webdriver/server.py`). Проверено на локальном
CONNECT-прокси: браузер действительно идёт через него (`CONNECT
example.com:443` в логе прокси).

Прокси **не применяется по умолчанию**: смена адреса меняет геолокацию,
которую видит сайт, и ломает сессию оператора. Включается явно.

Промо-коды для платных провайдеров (CAMOUFOX35, CAMOUFOX40, PAY2,
camoufox80, CAMOUFOX10, CAMOUFOX) лежат в `state/services.json` — там же
реестр: что бесплатно, чем заменено, что платное и куда вписать учётные
данные.

---

## Раздел 3. Режим «только бесплатно» и Cloudflare Workers AI (2026-11-15, вечер)

Решение владельца: платить нельзя, денег нет. Техническая часть закрыта
полностью; решения, которые остаются за владельцем, — только пополнение
счетов, если он когда-нибудь захочет.

### Что отключено и почему

| провайдер | ответ площадки | действие |
|---|---|---|
| `upstage` | 403 `API key suspended due to insufficient credit` | подключение выключено |
| `cline` | 402 `Insufficient balance … $0.00` | подключение выключено |
| `xai` | 403 `team doesn't have any credits or licenses` | подключение выключено |
| `novita` | 403 `NOT_ENOUGH_BALANCE` | подключение выключено |
| `dahl` | 401 `invalid_api_key` | подключение выключено |
| `ai21` | 410 — endpoint выведен из обслуживания | подключение выключено |

Ключи сохранены: вернуть провайдера — одна правка
(`PATCH /api/connections/{id} {"is_active": true}` + рестарт шлюза). После
рестарта каталог сжался с 565 до 519 моделей.

Из реестра убраны модели, за которые аккаунт заплатить не может: у
`bazaarlink` 125 id из 126 (баланс 0 → `402 … you can only afford 0`, остался
`auto:free`), у `mistral` — `mistral-large-latest` (403
`tier_not_allowed`, модель не в тарифе Free). `agentrouter/gpt-6-astra`
оставлен: модель есть на площадке, исчерпан её бюджетный пул — это не наш
счёт, и контрактный тест требует список площадки.

Бесплатные пути, которые остались рабочими: `mistral/codestral-latest`,
`bazaarlink/auto:free`, `agentrouter/gpt-5.6-sol` и `gpt-6.1-sol`, шесть
веб-исполнителей, свободные тиры gemini/groq/typhoon/sealion/kilocode и
теперь **Cloudflare Workers AI**.

### Cloudflare AI: в чём была проблема и как исправлено

1. **Адрес.** В реестре стоял `https://api.cloudflare.com/client/v4/accounts` —
   это листинг аккаунтов (GET), а не инференс. Инференс Workers AI лежит по
   адресу `…/accounts/{account_id}/ai/v1/chat/completions`.
2. **`account_id` принадлежит аккаунту оператора**, а не провайдеру, поэтому в
   реестре он не зашит: там плейсхолдер `{env:HETERION_ROUTER_CLOUDFLARE_ACCOUNT_ID}`,
   значение — в окружении сервиса. Незакрытый плейсхолдер означает, что
   провайдер не зарегистрирован вовсе (а не сломанный адрес).
3. **Токен.** Нужен **аккаунтный** (`cfat_…`, разрешение `Account.Workers AI`).
   User-токен (`cfut_…`), выпущенный в профиле, площадка на account-эндпоинтах
   не принимает — `Authentication error`. У оператора два аккаунта Cloudflare;
   `account_id` из истории URL дашборда принадлежал второму, рабочий —
   `7cee0416ccd6a0257ceb58e72b22cb06`.
4. **Модели — из площадки и проверены живым вызовом.** В выдаче
   `GET /accounts/{id}/ai/models/search` 75 моделей, из них 40 текстовые; на
   Free-плане (10 000 нейронов в сутки) отвечают **24**, остальные требуют
   платёжный метод (`not available on the Workers Free plan`). В реестр записаны
   24 проверенные.

Живой ответ Workers AI (напрямую): `{"choices":[{"message":{"content":"OK"}}],
"usage":{"neurons":1.396…}}` — нейроны видно в usage, то есть бесплатный
тариф активен.

### Автозапуск драйвера

`scripts/webdriver/server.py` завёрнут в launchd-агент
`com.heterion.webdriver` (порт 20131, `KeepAlive`, `RunAtLoad`), логи —
`~/Library/Logs/heterion-webdriver.{out,err}.log`. Драйвер работает headless
(фоновая служба); разовые шаги входа — в видимом браузере. Проверено:
после `launchctl bootstrap` агент поднялся, `chatgpt-web/gpt-5` ответил через
роутер «Работает.».

### DeepSeek: что выяснилось

Кнопка «Mit Google anmelden» — это `div[role=button].ds-sign-in-form__social-link`,
и её перекрывает «измеритель» текста (`ds-sign-in-form__social-links-measurer`).
Клик по ней **в headless не запускает OAuth вовсе**, а в видимом браузере
запускает — но площадка показывает hCaptcha-перетаскивание. Это шаг оператора;
после него остаётся экран согласия Google с кнопкой «Продолжить» (в
`login_web.py` клик по плитке аккаунта сбрасывал поток, потому что флаг
«аккаунт выбран» был привязан к `id()` страницы — Python переиспользует эти
адреса; теперь флаг один на весь поток).

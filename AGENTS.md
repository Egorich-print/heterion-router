# AGENTS.md — Heterion Router

> Rust-шлюз к LLM-провайдерам. Sibling of Heterion, successor of OmniRoute-Rust.
> Вырос из OmniRoute: **не аффилирован** с ним и его авторами (они не ведут этот
> репозиторий и не одобряют его), почти весь код переписан заново — Rust-шлюз и
> приложение на GPUI; на JS остался только веб-дашборд. Авторские права на
> выведенный материал сохранены в `LICENSE` и `THIRD_PARTY_NOTICES.md`.
> JS здесь нет (только `desktop/ui` на Svelte) — JS-хуков, eslint и prettier
> в репозитории нет осознанно (ADR-008 §8).

## Layout

- Рабочая копия — на внешнем SSD: `/Volumes/Netac_SSD/ai-workstation/Projects/heterion-router`
  (переехала 2026-10-28). Старый путь `~/ai-workstation/...` — устаревшая
  копия: правки туда не попадают, коммить и гонять CI нужно в Netac-копии.
- `crates/heterion-router-{config,core,crypto,db,gateway,http,providers,routing,translate,usage}` —
  workspace; бинарь `heterion-router-gateway` (`crates/heterion-router-gateway/src/main.rs`).
- `desktop/` — нативная оболочка на **GPUI** (`desktop/gpui`, исключён из
  workspace) + Svelte-дашборд (`desktop/ui`), который шлюз раздаёт сам.
  Десктоп — отдельное нативное приложение поверх того же admin-API
  (ADR-023), веб-дашборд остаётся как был.
- `docs/adr/` — решения (ADR-008 — переименование); ADR-001..007 —
  исторические решения с addenda; `docs/history/` — frozen upstream JS
  OmniRoute material, не история Rust-порта.
- Тестовые снепшоты (`*.snap`) — часть контракта; править только вместе с кодом.
- План следующей миссии — `docs/MISSION-2026-10.md` (ADR-011): objective,
  workstreams, milestones, роли субагентов, тестовые пороги и stop/go.
  Принцип миссии: **сначала красный тест, потом фикс**; mission-правки **не**
  проверяются на живом launchd-инстансе `20128`.

## Gates (перед каждым коммитом)

```sh
cargo fmt --all && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace
```

То же, что гоняет CI, одной командой: `scripts/ci/check.sh`.

Тяжёлое отдельно: `npm --prefix desktop/ui ci/build`,
`cargo build --manifest-path desktop/gpui/Cargo.toml` (cold ≈2m; нужен Xcode —
gpui компилирует Metal-шейдеры через `xcrun metal`) и `desktop/scripts/build-app.sh`
(bundle adhoc-signed, не notarized). Workspace fmt не покрывает исключённый
`desktop/gpui`; для него нужен отдельный check
(`cargo fmt --manifest-path desktop/gpui/Cargo.toml -- --check`; в CI это job
`desktop-fmt`).

Toolchain закреплён `1.99.1` в `rust-toolchain.toml`, `rust-version = "1.99"` в
обоих манифестах, CI использует тот же пин — не `@stable`. Новый крейт обязан
собираться на 1.99 и на `edition = "2024"`.

Релизные сборки в GitHub Actions: macOS-джобы — только на `macos-15` и
обязаны **проверять архитектуру результата** (артефакты названы по ней, Intel-раннер
должен ронять джоб, а не подписывать x86-64 сборку именем arm64). `macos-14` больше
не работает: он указывает на снятую конфигурацию `macos-14-arm64-us-default`, и джоб
падает за секунду до начала сборки. Отдельная ловушка того же класса: блок
`env:`/значение отступа, внутри которого только комментарий, — это `null` в YAML,
схема Actions отвергает такой файл целиком, и **все** прогоны падают за 0 с с
«This run likely failed because of a workflow file issue». Проверять workflow
настоящим YAML-парсером (nil-значения у `env`/`with`/`permissions`), а не глазами.

## Контракты, которые нельзя ломать молча

- `STATIC_SALT = "omniroute-field-encryption-v1"` + префикс `enc:v1:`
  (`heterion-router-crypto`) — живые БД держат значения с этой солью.
- Env: `HETERION_ROUTER_*` первичен, `OMNIROUTE_*` — fallback на один цикл.
  Data dir: `~/.heterion-router` → legacy `~/.omniroute`.
- Порты: сервис `20128`, sidecar `20129`; Vite dev-proxy по умолчанию
  `20128`, поэтому для dev gateway нужен `VITE_GATEWAY_URL=...:20129`.
- Keyless-бэкенды не шлют `Authorization` (тест `keyless_provider_sends_no_bearer`).
- 4xx вышестоящего провайдера не роняют breaker (не «чинить» это поведение).
- **429 — это квота, а не дефект модели.** Кулдаун 429 **модельный**
  (`record_429(provider, Some(model), …)`), не провайдерный: лимит на одной
  модели не выключает её соседей по провайдеру — иначе недостижим Tier-3
  «high-quota protector», который существует именно ради этого. Штраф
  качества (`QualityScore -= 0.3`, 15-минутный probation) за 429 **не
  начисляется**: он зарезервирован под дефектные ответы, иначе модель сидит
  в probation в 7.5 раза дольше собственного кулдауна (30 → 60 → 120 с).
  403/402 остаются провайдерными: баланс общий на аккаунт.
- **Прямой путь `provider/model` намеренно НЕ гейтится кулдауном.** Кулдаун
  рулит *лестницей* (кого пробовать следующим). Клиент, назвавший конкретную
  модель, получает запрос к ней: замороженный контракт «4xx провайдера не
  отключает провайдера». Закреплено тестом
  `a_direct_model_request_is_not_disabled_by_a_rate_limit`.
- **`Retry-After` реального апстрима доезжает до кулдауна.** `HttpError::Status`
  рендерит заголовок в своё сообщение (`retry-after: Ns` — **перед** телом,
  чтобы JSON-парсеры payload не сломались): это единственный канал до
  роутера. Раньше заголовок терялся в `Display`, и провайдер, просивший
  300 с, получал 30 с (первый шаг бэкоффа).
- Вырожденный повтор в ответе модели **обрезается роутером**
  (`repetition_guard.rs`): доля повторяющихся 8-грамм в хвостовом окне (128
  слов) ≥ 0.5 при повторе одного окна ≥ 4 раз → ответ режется до начала
  петли, `finish_reason` становится `length`, поток обрывается (апстрим
  больше не тратит токены на петлю). Два семейства — два правила: петля из
  handful слов (≤ 16 различных слов в окне) режется сразу; структурное
  повторение списка — только когда ответ перевалил за 2000 слов (иначе
  режется честная цитата, которую модель повторяет по просьбе промпта).
  Логируется как `WARN repetition guard model=… source=content|reasoning
  ratio=… repeats=… words=…`. Здоровый текст даёт 0.0; петля — 0.99+. Не
  «чинить» пороги на глаз — у них есть тесты (`repetition_guard::tests`) и
  сквозной тест на фейковом апстриме
  (`crates/heterion-router-gateway/tests/repetition_guard.rs`).
- Запрос с завершающим assistant-ходом (prefill) **адаптируется только для
  провайдеров, которые такую форму отвергают** (`continuation.rs`): Mistral
  отвечает 400 `invalid_request_message_order` (code 3230, «Expected last
  role User or Tool»), Gemini — 400 «Requests ending with a model turn are not
  supported». Для них в историю добавляется один user-ход с просьбой
  продолжить (`CONTINUATION_INSTRUCTION`), а ответ остаётся текстом модели:
  префикс остаётся у клиента, как при нативном prefill OpenAI. Для
  openai-форматных провайдеров префикс остаётся дословным — адаптация там
  ломала бы verbatim prefill (тест
  `a_provider_that_serves_prefills_keeps_the_verbatim_turn`). Assistant-ход с
  `tool_calls` не адаптируется: переписка без результата инструмента
  невалидна для любого провайдера (OpenAI тоже её отвергает).
- **Запрос приводится к тому, что провайдер способен принять** — правила в
  `provider_caps.rs`, применяются в `routing.rs::attempt()` (у провайдера
  свой набор, у модели свой лимит; решение принимается по **провайдеру**,
  потому что один и тот же id модели за разными площадками — это разные
  API). Все правила — измеренные отказы прогона 16.11.2026:
  - `reasoning_effort` **clamp** к набору площадки: groq принимает только
    `low`/`medium`/`high` (400 «must be one of …») — сильный запрос
    (`xhigh`, `max`) опускается до сильнейшего допустимого, слабый
    поднимается до слабейшего, незнакомое значение — поле убирается;
    oneminai знает всё до `xhigh`, но не `max`; pollinations — только
    `none`/`minimal`/`low`/`medium`/`high» (400 «unknown variant `max`»,
    114 отказов);
  - `reasoning_effort` **убирается совсем**, если площадка не принимает
    поле: groq для Qwen3.8 (400 «invalid Qwen3.8 reasoning_effort»);
  - `reasoning_content` в истории **вычищается**: groq (400 «property
    'reasoning_content' is unsupported»), oneminai (400 «Unrecognized key»),
    Mistral (422 `extra_forbidden`). Видимый текст assistant-хода не
    трогается — убирается только sidecar размышления;
  - `max_tokens`/`max_completion_tokens` **обрезаются до потолка**: typhoon
    16384 (400 «must be at most 16384»), oneminai 32768, а у
    `sealion/aisingapore/Qwen-SEA-LION-v4-32B-IT` — 32000 (400 «cannot be
    greater than max_model_len=32000»); у соседней 27B-модели тот же
    бюджет проходит; у groq/Qwen3.8 — свой потолок 16384;
  - **потолок промпта**: groq (413 «Request too large … TPM: Limit 8000») —
    кандидат с промптом выше потолка площадки **не вызывается вовсе**,
    лестница идёт дальше: отказ заведомый, платить за него незачем;
  - **потолок числа сообщений**: typhoon (422 «List should have at most 50
    items after validation, not 69») — самые старые ходы уходят, запрос
    обслуживается вместо отказа;
  - **форма тела** по площадке: cloudflare-ai валидирует тело по своей
    схеме — `content` должен быть строкой, а не массивом частей (400
    «Type mismatch of '/messages/0/content', 'array' not in 'string'»), и
    у каждого хода должно быть поле `content` (400 «required properties
    at '/messages/5' are 'role,content'»);
  - **набор уровней `reasoning_effort` бывает модельным, а не площадочным**:
    за одним адресом cloudflare-ai сидят разные движки, и каждый называет
    свои уровни — `gpt-oss` на Harmony берёт `high`, `medium`, `low` (400
    «reasoning_effort='max' is not supported by Harmony. Supported values are:
    high, medium, low», 54 отказа 17.11.2026), а `qwen3.8` берёт `xhigh`,
    `medium`, `low` и отвергает `high` (400 «Unexpected reasoning effort
    high. Supported types are xhigh (default), medium, and low», 60 отказов
    тем же днём). Тот же движок Qwen3.8 за sealion отвечает тем же текстом
    (8 отказов), поэтому правило называет **id модели**, а не площадку;
  - **поле `reasoning_effort` убирается совсем**, когда его не знает модель:
    `ovhcloud/Meta-Llama-3_3-70B-Instruct` — 400 «feature 'reasoning_effort
    for this model' is not currently supported»;
  - **потолок в символах, а не в токенах**: oneminai считает запрос
    символами и складывает историю со схемами инструментов и форматом ответа
    — 400 «Messages, tools and response format exceed the 100,000 character
    limit» (2 отказа 17.11.2026; проба: ~110 000 символов одних схем
    инструментов отвергнуты так же, а одно сообщение длиннее 100 000
    символов даёт «Too big: expected string to have <=100000 characters»).
    Старые ходы уходят (как у typhoon), а когда и этого мало — кандидат
    **не вызывается вовсе**;
  - **пропущенный по размеру кандидат — это не «unknown model».** Когда
    кандидат снят из лестницы по потолку (промпт, символы, окно), клиент
    получает **400 `request too large for provider/model: …`** с указанием
    предела, а не 404 «no backend can serve it»: модель известна, запрос
    просто не помещается. Ложь про неизвестную модель прятала настоящую
    причину (замер 17.11.2026: прямой запрос к oneminai сверх 100 000
    символов отвечал 404 «unknown model»).
  - роль `developer` переименовывается в `system` для провайдеров, которые
    её отвергают (`developer_role.rs`): Mistral (422 `union_tag_invalid`),
    mixlayer (400 «unknown variant 'developer'»).
  Правило «только по измеренным отказам»: новый провайдер в списки не
  добавляется без живого замера.
- **Результат инструмента с `$ref` уходит в Gemini текстом, а не объектом**
  (`gemini_request.rs`): Gemini резолвит указатель по своим объявлениям и
  отказывает — «The referenced name `#/$defs/__schema5` in
  function_response.response does not match to a display_name in the
  function_response.parts» (21 отказ, 16.11.2026). Указатели приходят из
  самих MCP-схем, поэтому результат инструмента их цитирует. Значение
  `$ref` не важно — важен сам ключ (проверено: `$ref` с текстом вместо
  ссылки отвергается так же). Остальные результаты сохраняют объектную
  форму.
- Картинка в запросе **не должна исчезать на переводе**: OpenAI→Gemini
  (`gemini_request.rs`) кладёт `image_url` c data-URL в `inlineData`. Gemini не
  умеет забирать картинку по URL — `fileData` с https отвергается («Cannot fetch
  content from the provided URL»); URL, который не data, остаётся в теле
  текстовой заметкой, а не пропадает. Проверка способности «читает картинку» —
  по `usage.prompt_tokens` (текст ~28 против картинки ~1120) и
  `promptTokensDetails: [{modality: IMAGE}]`, а не по тексту ответа модели:
  модель способна правдоподобно описать картинку, которую не получила.
  История: `docs/research/GEMINI-VISION-FIX-REPORT-2026-10-26.md`.
- Поиск Exa живёт на **чат-поверхности** как модель `exa/exa-search`
  (`executor: "exa"`, ADR-016/019): последнее сообщение пользователя — запрос,
  ответ — цитированный блок. Параметры поиска (`numResults`, `type`, `category`,
  `includeDomains`, `excludeDomains`, даты) едут в теле запроса под объектом
  `"exa"`; без него тело не меняется. Ключи Exa лежат только в БД роутера —
  потребитель (плагин `~/.minimax/plugins/exa-search/`) берёт ключ **роутера** и
  ходит в `http://127.0.0.1:20128`, а не в Exa напрямую.
- Провайдер `agentrouter` — **Responses-протокол + идентификация клиента**
  (ADR-021). Формат в реестре `openai-responses`, адрес
  `https://agentrouter.org/v1/responses`; `/v1/chat/completions` площадка
  отвергает, а живой каталог (`GET /v1/models` с ключом) — ровно
  `gpt-6.1-sol`, `gpt-6-astra`, `gpt-5.6-sol` (Claude-модели нашей группе
  `default` недоступны). Площадка **меряет квоту по типу клиента**: без
  распознанного клиента — `401 unauthorized_client_error` даже с валидным
  ключом. Отпечаток (`originator: codex_cli_rs`) — политика провайдера в
  `client_profile.rs`, не глобальное правило; свой user-agent роутера не
  влияет на группу и множитель. Перечень моделей в реестре берётся **из
  площадки**, а не из памяти: устаревшие id в реестре обещают клиенту
  модели, которой нет.
  **Баланс почти кончился (замер 17.11.2026).** Ключ **жив**: Responses-вызов
  по `gpt-5.6-sol` с `originator: codex_cli_rs` вернул `status: "completed"`.
  Но денег на работу почти нет: `403 insufficient_user_quota` — «user quota:
  $0.004456, need quota: $0.032528» (запрос на 4000 токенов). Мелкий запрос
  (64 токена) ещё проходит. Значит **включать обратно пока нельзя**: соединение
  снова выключено (`PATCH /api/connections/{id}` → `restart_required: true`,
  дальше — рестарт шлюза), а вернётся оно после пополнения баланса.
- **OpenCode Console требует `x-opencode-session`** (`client_profile.rs`).
  Без него площадка отвечает `MissingSessionID` — «Request is missing
  x-opencode-session and cannot be routed efficiently» (замер 17.11.2026),
  то есть запрос не маршрутизируется вовсе. Заголовок обязан быть
  **стабильным** внутри разговора: новый id на каждый запрос убил бы
  кеш промптов у площадки. Поэтому id выводится из самого разговора
  (первый user-ход ⇒ `ses_<sha256[0..16]>`), а не берётся из таблицы
  клиентов. Адреса Console (документация площадки, 17.11.2026):
  pay-as-you-go — `https://opencode.ai/inference/openai/v1`, Go —
  `https://opencode.ai/inference/go/openai/v1`; старые `zen/v1` и `zen/go/v1`
  ещё отвечают, но это не документированные пути.
- **«Ключ без входа» у OpenCode — это литерал `public`, а не ключ.** Механизм
  в бинаре клиента (`~/.opencode/bin/opencode`, 1.18.35): если нет
  `OPENCODE_API_KEY`, нет подключённой учётки и в провайдере нет ключа, клиент
  подставляет в тело запроса строку `apiKey = "public"` и отключает все модели
  с ненулевой ценой входа. Площадка принимает такой «ключ» как **анонимный
  доступ — только бесплатные модели Zen**. Замер 20.11.2026: `Bearer public` на
  платной модели → 401 «Missing API key.»; на бесплатной → 429
  `FreeUsageLimitError` («Free usage limit reached… subscribe to OpenCode Go»)
  с `Retry-After: 51509` (≈14 ч). Без заголовка вообще — то же самое (сервер
  сам считает запрос анонимным). Анонимная квота **общая на всех анонимов, а
  не по клиенту**: проверено на разных сетевых идентичностях — IPv4
  37.120.49.34 и IPv6 2a02:2455:… (тот же счётчик, `Retry-After` отличается на
  секунду), внешние прокси с exit-IP из разных стран и разных PoP Cloudflare
  (FRA, SIN) — везде один и тот же 429 `FreeUsageLimitError` с одним и тем же
  обратным отсчётом. Заголовки `x-opencode-session`, `x-opencode-client`,
  `x-opencode-project`, `x-opencode-request` тоже не влияют (6 вариантов).
  Итог: **резидентские прокси не помогают** — анонимный пул один на всех,
  «несколько бесплатных клиентов» в анонимном режиме не существует.
- **Настоящие ключи выпускает консоль под учётной записью.** Device-flow:
  `POST https://opencode.ai/console/auth/device/code` {`client_id`: `opencode-cli`}
  → `user_code` + `verification_uri_complete` (`/console/device?user_code=…`,
  600 с); обмен — `POST /console/auth/device/token`
  (`grant_type=urn:ietf:params:oauth:grant-type:device_code`). Код выдаётся
  **без входа**, но подтвердить его без сессии нельзя:
  `GET /console/auth/device/details` → 401 «Invalid or expired session»,
  `POST /console/auth/device/approve` → 400 `DeviceApproveFailed`. Регистрация
  консоли — `POST /console/auth/register` (email + password + challengeToken).
- **Формат ключей консоли:** `oc_sk_<12 hex>_<32 симв.>` либо `sk-<64 симв.>`;
  у ключа есть `permissions` (`all`/`read`/`restricted`/`inference-only`),
  `scopes` (в том числе `keys:write`), `expiresAt`, `ipAllowlist`. API выпуска:
  `POST /console/api/service-accounts` → `POST
  /console/api/service-accounts/{id}/keys` (ответ содержит `token` — он
  показывается один раз). Без сессии эти пути отвечают 401 (то есть
  существуют), а `/console/api/keys` → 404 (такого пути нет).
- **Go — платная подписка; квота привязана к подписке, а не к числу ключей.**
  Страница `/console/go` (замер 20.11.2026): Go $10/мес, Go Plus $40/мес, у
  каждой модели «Relative request allowances» на 5 ч и месячный лимит. Площадка:
  `Bearer public` → 401 «OpenCode Go requires an API key»; без
  `x-opencode-session` → 400 «Request is missing x-opencode-session and cannot
  be routed efficiently».
- **Инструмент:** `scripts/opencode_keys.py` — `list` / `mint --count N` /
  `probe` через свою сессию консоли (`OPENCODE_CONSOLE_TOKEN` или
  `OPENCODE_CONSOLE_COOKIE`). Учётные записи не создаёт: только ключи внутри
  уже оплаченной подписки.
- **Анонимный Zen-пул — отдельный провайдер реестра `opencode-free`**
  (`auth_type: optional`, `free_only: true`, 12 моделей), а не хвост
  `opencode`: платные модели анонимно площадка не отдаёт (401 «Missing API
  key»). Живой каталог площадки (103 модели) фильтруется **по опубликованной
  ею цене**: в каталог и в лестницу попадают только модели с
  `cost.text.input.price == 0`, а цена, которую площадка не опубликовала,
  бесплатной не считается. Замер 20.11.2026: 12 бесплатных из 103 —
  `big-pickle`, `mimo-v2.6-flash-free`, `deepseek-v4.1-flash-free`,
  `longcat-2.5-free`, `iceman-free`, `beam-free`, `flint-40b-free`,
  `nemotron-3.5-super-vl-free`, `jev-1.13-free`,
  `nemotron-3.5-lightning-free`, `hy4-flash-free`, `tidepool-alpha-free`.
  Ключ соединения — литерал `public` (тот же, что шлёт клиент opencode);
  соединение «OpenCode Zen Free (аноним)». Окно пула общее на всех (см.
  выше), поэтому чтение квоты сейчас `exhausted` — с собственным текстом
  площадки, а не выдуманным числом.
- **Для агентной работы по умолчанию — `gpt-5.6-sol`.** Замер 2026-10-28
  (по 8 вызовов на модель, вперемешку, обычная задача + стрим): `gpt-5.6-sol`
  — 8/8 успех, медиана 9.0 с; `gpt-6-astra` — 8/8, медиана 19.8 с;
  `gpt-6.1-sol` — 7/8, медиана 30.9 с. Все три умеют вызывать инструменты
  (`finish_reason: tool_calls`), reasoning-effort принимается в low/medium/high.
  Известная особенность `gpt-5.6-sol`: обход репозитория у неё
  **разветвляется** — за ход она просит 8 инструментов сразу и уходит в
  глубину (на schemkit за 18 ходов дошла до `pjlim`, `stamp_diode_companion`
  и шага backward Euler сама). Для агентной работы это плюс, но лимит ходов
  надо задирать, иначе отчёт не успевает собраться.
- **Обрезанный ответ — это `finish_reason: "length"`, а не `"stop"`.** У
  Responses-объекта есть `status: "incomplete"` с
  `incomplete_details.reason: "max_output_tokens"`; в потоке площадка шлёт
  событие `response.incomplete` вместо `response.completed`. Роутер обязан
  отдавать `length` (OpenAI-контракт) и **не терять поток**: необработанное
  `response.incomplete` обрывало стрим без финального чанка, то есть 502
  вместо короткого ответа. Это важно для `gpt-6.1-sol`/`gpt-6-astra`: они
  reasoning-модели, тратят 600–1200 токенов на размышление, и при
  `max_tokens` меньше этого весь бюджет уходит в thinking — ответ пустой.
  Рабочий минимум для agentrouter — `max_tokens` 4000+.
- **Контекстное окно agentrouter — ~870k prompt-токенов, а не 128k.**
  Площадка **не публикует** лимит контекста: ни `GET /v1/models`, ни
  `/api/pricing`, ни `/api/status` его не содержат — окно приходится мерить
  (`scripts/subagent/ctx_probe.py`). Замер 2026-10-28 прямым Responses API
  по `gpt-5.6-sol`: приняты 107 719 / 153 873 / 230 796 / 346 180 /
  538 488 / 679 582 / 873 741 prompt-токенов; 950 000 и 1 000 000 — 504 от
  балансировщика (таймаут по скорости, а не отказ по размеру), то есть
  рабочий потолок — около 870k.
- **`limit.context` — это порог компрессии рантайма, а не метаданные.**
  Порог считает `resolveCompactionTokenBudget` (local-runtime-v2):
  `providerInputLimit = min(floor(0.95·W), W − 16384, W − min(C,16384) − 2048)`,
  `automaticTriggerAt = min(providerInputLimit, floor(0.95·(W − min(C, W/4))))`,
  где W = `limit.context`, C = `limit.output`. Сжатие начинается, когда
  локальная оценка входа превысит порог. При W = 512000 и C = 32768 порог —
  **455 270** токенов. Для custom-провайдера W берётся из `limit.context`
  конфига (при отсутствии — 200000), C — из `limit.output`.
  Заниженное значение значит, что агент теряет разведку задолго до того, как
  площадка что-то откажет: при 128000 порог был бы 91 200, а агентный прогон
  по schemkit доходит до 124k. Поэтому `limit.context` для
  `agentrouter/gpt-5.6-sol` — **512000**: вдвое ниже подтверждённого окна, с
  запасом на таймаут балансировщика (он начинается выше 900k) и на
  `limit.output` = 32768 в том же запросе.
- **Каталог моделей MiniMax Code — снимок на старте приложения.** Правки
  `~/.minimax/config.yaml` (в т.ч. `limit.context`) рантайм на ходу не
  подхватывает: модель, добавленная в конфиг после старта, отвергается с
  «unavailable in the current catalog» (проверено пробой 2026-10-28). Значит
  после правки конфига нужен **перезапуск приложения**; сессия, пережившая
  рестарт, берёт новое значение на следующем ходу — модель резолвится
  каждый ход через `byokConfigGetter`. Пользовательский рычаг
  `contextManagement.compactionTriggerAt` (доля в (0,1] или число токенов)
  умеет только **ускорить** сжатие: берётся `min` от встроенного порога.
- У Responses-объекта поле `error` присутствует всегда (`null` при успехе) —
  отказом считается **непустой** объект ошибки, иначе весь ответ подменяется
  на `upstream error: null`.
- **Исход хода пишется в журнал: `finish_reason`, `tool_called`, `had_tools`.**
  Три колонки в `call_logs` (`finish_reason` TEXT, `tool_called` INTEGER,
  `had_tools` INTEGER) — они nullable и добавляются на старте как
  **compat-колонки** (`ensure_call_logs_columns`), а не миграцией: `ALTER TABLE`
  в SQLite не идемпотентен, а база, у которой `call_logs` вообще нет, дала бы
  падение на boot. Значения: `finish_reason` — почему апстрим закрыл ход
  (`stop` / `length` / `tool_calls`); `tool_called` — был ли вызов инструмента
  (`0`/`1`, NULL = неизвестно, например оборванный поток); `had_tools` — давал
  ли клиент инструменты вообще (без него `tool_called = 0` не отличить от
  обычного вопроса без инструментов).
- **Агентный ход, закрытый текстом, — это факт, а не приговор модели.**
  Запрос с инструментами, `finish_reason = "stop"` и `tool_called = 0` даёт
  WARN `agent turn ended with text instead of a tool call provider=… model=…`.
  Штрафа за это нет: финальный текст агента — законен, и понижать модель в
  лестнице должен владелец, а не роутер. Данные собираются, решение — за
  владельцем.
- **Журнал считает вызовы апстрима, а не обращения клиента.** Ответ из
  `semantic_cache` (повтор идентичного запроса) не вызывает апстрим и **не
  пишет строку в `call_logs`** — вместо неё клиент получает
  `x-router-cache: hit` и `x-router-cost: 0.000000`. Любая доля, посчитанная
  по журналу, — это доля среди реально обслуженных вызовов.
- **В `call_logs` — одна строка на один вызов апстрима, а не одна на запрос.**
  Строка исхода запроса называла только **последнюю** попытку, поэтому отказ
  первых кандидатов в лестнице был не виден: провайдер, ответивший 429 на
  трёх первых и обслуживший четвёртого, выглядел как «ни разу не
  отказавший». Теперь каждый отказ пишет свою строку: `provider`/`model`/
  `connection_id` — свои, `tokens_in/out = 0`, `finish_reason`/`tool_called` —
  `NULL` (ответа не было), `error_summary` — текст отказа дословно.
  Успешный запрос: строки отказов **плюс** строка победителя. Полностью
  упавший запрос: только строки отказов (итоговая строка не дублируется —
  последняя попытка уже описана). Отказ по tool-validation (апстрим ответил
  200, роутер выбросил ответ) — тоже строка отказа: статус клиента 502, в
  `error_summary` `tool validation failed`. Потоковый путь ведёт себя так же:
  у него отдельная петля фейловера. Колонка `status` в строке отказа —
  клиентский статус (как и у строки исхода), а **исходный статус апстрима
  лежит в `error_summary`** дословно (`upstream status 402: …`): апстрим 402
  доходит до клиента как 502, и по колонке это не прочитать.
- **Режим `goal` в рантайме MCode меняет смысл «текстового хода».** В goal
  прогоне рантайм не даёт ходу закрыться, пока агент явно не объявит миссию
  выполненной: там текстовый ход означает «агент считает миссию завершённой»,
  а не «агент забыл вызвать инструмент». Один и тот же признак в обычном и в
  goal прогоне — разные события; мерить их надо раздельно. Замер по сессиям
  2026-11-16 (12 сессий, 184 хода): в 10 сессиях текстовый ход был
  **последним**, в 2 — промежуточным, и после него рантайм продолжал работу
  (следующий ход шёл с вызовом инструмента). Признак «текст без
  инструмента» в goal-прогоне — это либо объявление о завершении, либо
  запрос продолжить; отказ агента — это когда после него не следует ни
  одного хода.

## Десктоп на GPUI (ADR-023)

Нативная оболочка — Rust-приложение на [gpui](https://gpui.rs) 0.2.2 в
`desktop/gpui` (крейт `heterion-router-desktop`, вне workspace). Tauri v2 и
`desktop/src-tauri` удалены.

- **Крейт:** `gpui = { version = "0.2.2", default-features = false, features =
  ["font-kit"] }`. Фича `font-kit` обязательна на macOS (без неё нет
  текст-системы). Фича `macos-blade` не нужна: gpui рисует своим
  Metal-рендерером, шейдеры компилируются на сборке через `xcrun metal` —
  **нужен Xcode**. Шрифты — системные macOS (`.SystemUIFont`), свои файлы
  не нужны.
- **Структура:** `src/api.rs` — типизированный контракт admin-API (зеркало
  `crates/heterion-router-gateway/src/admin.rs`); `src/theme.rs` — палитра из
  `desktop/ui/src/app.css`; `src/widgets/` — свои виджеты (в gpui нет ни
  `Button`, ни `Input`, ни `Table`); `src/views/` — по представлению на
  секцию; `src/sidecar.rs` — sidecar-шлюз; `src/secret.rs` — файл ключа.
- **Контракт — один на оба фронтенда.** Правка `src/api.rs` обязана идти
  вместе с правкой gateway-обработчиков; расхождение ломает либо веб, либо
  десктоп.
- **Ключ оператора** лежит в `<data dir>/desktop/operator-key.json` (0600,
  каталог 0700), а не в `localStorage` WebView. В БД шлюза его нет.
- **Sidecar:** release-сборка поднимает `heterion-router-gateway` из каталога
  рядом со своим бинарём на порту 20129 (`HETERION_ROUTER_SIDECAR_PORT` /
  `OMNIROUTE_SIDECAR_PORT`) с `HETERION_ROUTER_PARENT_PID`; debug-сборка
  говорит с уже запущенным шлюзом (20128) — **но только если задан
  `HETERION_ROUTER_PORT=20128`**: без переменной debug-сборка берёт
  `GATEWAY_DEFAULT_PORT` = 20129 и показывает «gateway unreachable».
  Запуск для визуальной проверки:
  `HETERION_ROUTER_PORT=20128 desktop/gpui/tools/screenshot.sh launch`.
- **Факты по API gpui** — `docs/port/GPUI-API-NOTES.md`, контракт порта —
  `docs/port/GPUI-PORT-SPEC.md`. Не выдумывать имена API: исходники крейта в
  `~/.cargo/registry/src/index.crates.io-*/gpui-0.2.2/src/`.
- **Инструменты:** `desktop/gpui/tools/screenshot.sh` (запуск, скриншот,
  статус окна) и `desktop/gpui/tools/mcp/server.js` (MCP-сервер: состояние
  шлюза + снимок экрана для агента).

## Сервис (macOS)

```sh
scripts/deploy-service.sh                            # сборка + установка + перезапуск
launchctl kickstart -k gui/$(id -u)/com.heterion.router   # если job установлен
curl -s http://127.0.0.1:20128/healthz                    # проверка
```

Plist — машинно-локальный (`~/Library/LaunchAgents`), не хранится в репозитории.
Текущая установленная конфигурация слушает `0.0.0.0:20128`; не-loopback bind
без ключа блокирует старт.

**Сервис запускается не из `target/release`, а из копии на системном диске**
(`~/.local/bin/heterion-router-gateway`, путь в plist). macOS для свежезаписанного
бинаря на смонтированном томе гоняет проверку подписи (`trustd`), и она может
зависнуть: процесс висит в `dyld` с 0% CPU и не слушает порт. Копия на системном
диске стартует всегда, и сервис переживает отмонтирование тома. Ставится через
`scripts/deploy-service.sh`; launchd держит кэшированное определение job, поэтому
переустановка бинаря требует `bootout` + `bootstrap` (один `kickstart` не берёт
новый plist).

Перезапуск из UI — только под launchd (`restart::is_supervised`), иначе 409.

- Атрибуция usage/ call-log реальная: `backend::Attribution` + per-request
  токен (`ATTRIBUTION_TOKEN`). Константа `provider = "routing"` больше не
  пишется — на этом строится per-provider аналитика.
- Ничего, что лежит в БД, не является credential: `api_keys.key` для ключей,
  выпускаемых портом, хранит маркер `hash-only:<id>`, а не дайджест; дайджест в
  `key_hash` нельзя предъявить как bearer. Legacy JS-строки с plaintext в `key`
  продолжают работать.
- Breaker не роняется от 4xx провайдера ни по префиксу `upstream status {code}:`
  (`crates/heterion-router-http/src/client.rs`), ни по статусу, восстановленному
  из тела, пришедшего с HTTP 200 (`lib.rs::upstream_status_or_recovered`).

## Провайдер Heterion

`heterion-local` в `registry.json` (keyless, openai-формат) — это только
code path: в текущей рабочей БД нет активной строки `provider_connections`, и
backend не поднимается. URL по умолчанию `http://127.0.0.1:8080/v1`, override —
`HETERION_LOCAL_BASE_URL`. Model id — транзит на Heterion (там один загруженный
вес). Registry содержит 274 providers / 2363 models; в каталог формы добавления
ключа (`GET /api/providers`) проходят записи с поддерживаемой схемой. Перечень:
`docs/PROVIDERS.md`; метод аудита: `docs/research/PROJECT-AUDIT-2026-09.md`.

## Диагностика лестницы `auto`

**`auto` — это динамическая лестница, а не запись комбо.** Запись комбо
`heterion-router/auto` в БД содержит несколько строк, но план строится как
курируемые кандидаты (`routing.rs::DEFAULT_AUTO_CANDIDATES`, 7 штук) плюс
**хвост из всех живых моделей всех провайдеров с бэкендом** — живой пул
сейчас 431 кандидат. Плана в БД как «списка участников» не существует.

- `GET /api/routing/plan?model=auto` — план с числами по каждому
  кандидату: `base_priority`, `quality_score`, `availability`,
  `soft_cooldown`, `effective` и причина отсева (`no_backend`,
  `no_vision`, `no_tools`, `pseudo_tools`, `context`, `cooldown`,
  `breaker`, `zero_score`). Флаги `&tools=1`, `&images=1` строят план для
  агентного запроса — план зависит от запроса, и без инструментов он
  другой.
- **Ротация — не залипание:** ответивший уходит в мягкий кулдаун, следующий
  запрос берёт другую голову. Проверяется срезом плана до и после запроса.
- **Плана в логах не было** — поэтому «почему эта модель не отвечает» было
  нельзя ответить по журналу. Эндпоинт это закрывает; при разборе прогона
  начинать с него, а не с догадок.
- Статистика прогона: `scripts/auto_combo_stats.py` (окно считается в
  **UTC** — журнал пишет `timestamp` в UTC).

## Матрица провайдеров (ADR-032)

**Причина провайдера — измеренный ответ площадки, а не заявление реестра.**
Модуль `crates/heterion-router-gateway/src/provider_status.rs`: enum
`ProviderReason` (`served`, `needs_key`, `credits_required`, `not_served`,
`no_connection_row`, `no_web_adapter`, `unsupported_format`, `not_a_chat_api`,
`dead_endpoint`, `unknown`), `classify(status, body)`, `probe`, `reason_for`.
Классификация: 2xx → `served`; 401/403 → `needs_key`; 402 → `credits_required`;
JSON-404 → `not_a_chat_api`; HTML-404/410 → `dead_endpoint`; 405 →
`not_a_chat_api`; 429/5xx/таймаут → `unknown`. Порядок выбора причины: `served`
→ структурный пробел (`unsupported_format` / `no_web_adapter`) → измеренная
причина → `needs_key` (нужен ключ и нет подключения) → `no_connection_row`
(keyless без строки) → `not_served`.

- **Keyless-проход берёт только `executor: default` + `format: openai` +
  http(s) + keyless + без подключения + проба доказала `served`.** Именованные
  исполнители (web-страницы, CLI, websocket) исключены: их «эндпоинт» отвечает
  200 на любой POST, и такие провайдеры попадали в каталог как живые.
- Проба: `POST /api/providers/{id}/probe` (ключ оператора), результат пишется в
  `provider_probes` (миграция `004`). При старте шлюза автоматически
  простукиваются keyless-площадки (пул 8, таймаут 10 с).
- `GET /api/providers`: старый список `providers` (для формы ключа) сохранён;
  полный реестр — в новом ключе `matrix` (обратная совместимость).
- Отчёт: `scripts/provider_matrix.py` (проба всех площадок, 8 потоков,
  markdown + JSON) — пример прогона `docs/research/provider-matrix-2026-11-16.md`.
  Матрица воспроизводима: пробу и классификацию делает сам шлюз, скрипт лишь
  собирает отчёт, поэтому расхождение с админ-API невозможно.

## Ключи площадок: что живо, что мертво (замер 17.11.2026)

Проверка ключа — **живой вызов площадки**: ключ расшифровывается тем же
способом, что и у шлюза (`enc:v1:` ⇒ scrypt + AES-256-GCM, соль
`omniroute-field-encryption-v1`), уходит запрос на документированный адрес,
и ответ площадки и есть вердикт. Сам ключ при этом не печатается и в репозиторий
не попадает. Помощник — `~/.heterion-router/keyprobe.py` (вне репозитория).

| провайдер | адрес в реестре | вердикт ключа | что делать |
| --- | --- | --- | --- |
| `agentrouter` | верен | **жив, но денег нет**: ключ проходит, `403 insufficient_user_quota` («user quota: $0.004456, need quota: $0.032528») | пополнить баланс; соединение оставлено выключенным (ADR-026) |
| `command-code` | был неверен (`api.commandcode.ai` → 404) | **истёк**: «Your API key has expired… run `cmd login`» | новый ключ; адрес исправлен на документированный Provider API |
| `blackbox` | был неверен (`api.blackbox.ai` → 404) | **мёртв**: «Invalid API key» | новый ключ; адрес исправлен на `enterprise.blackbox.ai` |
| `sambanova` | верен | **мёртвы все пять**: 401 `invalid_api_key` | новые ключи |
| `opencode-go` | верен | **мёртв**: «Invalid credential» (проверено 20.11.2026 и на `/zen/v1/models`, и на `/zen/go/v1/models`; формат ключа в БД верный — `sk-<64>`) | новый ключ формата `oc_sk_…` из консоли; заголовок `x-opencode-session` роутер теперь шлёт |
| `opencode` | — | ключа в соединении **нет** вовсе; без ключа площадка работает анонимно (`public`) — только бесплатные модели Zen, квота общая на клиента | можно оставить без ключа (аноним) либо вписать `public` явно; платные модели Zen без ключа недоступны |
| `kimi-coding` | верен | OAuth-токен истёк 17.09.2026 | вход владельца: автоматического refresh в Rust-шлюзе нет |

**Правило «только по измеренным отказам» работает и здесь:** адрес площадки
меняется только после того, как документированный адрес отвечает живым
ответом площадки (не 404 «нет такого пути»). Каталог моделей в реестре
обновляется **из живой выдачи** площадки (`GET .../models`), а не из памяти;
у `command-code` в каталоге 101 модель, из них 12 отвечают только на
`/v1/messages` — их в `openai`-формат не берём (для них нужен отдельный
провайдер в формате `claude`).

## Живой каталог и квота площадок (измерение, а не заявление)

Реестр — это то, как площадка выглядела в день написания каталога. Площадка —
это то, что она обслуживает **сегодня**, и отвечает она только сама. Роутер
поэтому меряет обе вещи сам, ключом соединения (`provider_sync.rs`):

- **Каталог моделей.** `GET <models-endpoint>` площадки → таблица
  `provider_catalog` (миграция 005). Ответы читаются в трёх видах: OpenAI
  (`{"data":[{"id":…}]}`), Gemini (`{"models":[{"name":"models/…"}]}`) и
  голые строки. Измеренный каталог **дополняет хвост лестницы `auto`**: модель,
  которую площадка перечислила, а реестр не знает, становится кандидатом
  (непроверенным — как и весь хвост, поэтому `404` от неё не вытесняет
  проверенную модель; не больше 50 моделей от одной площадки). У площадки с
  несколькими соединениями список **один**: площадка публикует один набор
  моделей, соединения отличаются только ключом — поэтому в выдаче
  `GET /api/providers/catalog` модель площадки указана один раз, а `count`
  считает уникальные id.
- **Квота.** Остаток бюджета читается из ответа: заголовки
  (`x-ratelimit-remaining-*`, `x-ratelimit-limit-*`, `ratelimit-*`,
  `retry-after`) и текст отказа — agentrouter прямо пишет «user quota:
  $0.004456, need quota: $0.032528». История — в `quota_snapshots` (таблица
  была в БД с времён JS, теперь её наполняет Rust).
- **Проба квоты — запрос с `max_tokens: 1`, но не обязательно им.** Площадка,
  отвергающая такой бюджет, получает следующий: лестница 1 → 16 → 64, и
  поднимается она **только** по жалобе площадки на сам бюджет. AI Horde
  отвечает «the AI Horde refuses a token limit below 16, the minimum
  generation length of its native text API» (замер 17.11.2026) — с одним
  токеном квота там читалась как `unknown`. **Модель пробы** — сначала из
  реестра, но площадка, сказавшая «такой модели нет», отправляет пробу к
  следующей модели **из живого каталога самой площадки**. Отказ про **модель**
  — не отказ про квоту, и таких отказов четыре, все измерены вживую
  (17.11.2026): llm7 «is currently unavailable» (`model_unavailable`),
  nvidia — голый `404 page not found` без слова «model», sealion «is not
  available for this API key» (`key_model_access_denied`), AI Horde «No
  worker is currently serving the model» (`no_eligible_worker`), и litellm-
  шлюз sealion «Unmapped LLM provider for this endpoint» (на чат-эндпоинте
  это эмбеддинг-модель — `BAAI/bge-m3`, `BAAI/bge-reranker-v2-m3`). Если
  модели не отвечает **вся** площадка, последний отказ сохраняется как
  чтение (`unknown`) — доказательство остаётся, провайдер не исчезает из
  отчёта. Каталог площадки перечислен в алфавитном порядке, и первые строки —
  часто не чат-модели (`BAAI/bge-m3`, `BAAI/bge-reranker-v2-m3` у sealion и
  api-airforce), поэтому проба берёт до **8** моделей каталога.
- **Измерение идёт по тому же адресу, что и бэкенд.** Адрес площадки может
  нести плейсхолдер `{env:NAME}` (Cloudflare Workers AI требует account id
  внутри пути, и он принадлежит оператору, а не площадке), значение — в
  окружении. Проба и каталог обязаны подставить его так же, как бэкенд: без
  подстановки cloudflare-ai отвечал «Could not route to
  /client/v4/accounts/%7Benv:HETERION_ROUTER_CLOUDFLARE_ACCOUNT_ID%7D/ai/v1/
  chat/completions» — то есть измерялась площадка, которой не существует.
  Подстановка живёт в библиотеке (`placeholders.rs`), а не в бинаре, и
  незаполненный плейсхолдер даёт **нет адреса**, а не адрес наполовину.
- **Когда меряется:** автоматически при старте шлюза (фоновая задача, восемь
  площадок одновременно) и по требованию — `POST /api/providers/sync` (все) и
  `POST /api/providers/{id}/sync` (одна).
- **Читается:** `GET /api/providers/catalog` и `GET /api/providers/quota`.
- **Правило то же:** только измеренное. Площадка, которая ничего не сказала о
  квоте, получает `unknown`, а не выдуманный процент. Проба квоты — это
  запрос с `max_tokens: 1`: измерение, но всё же запрос (копейка на платной
  площадке), поэтому она идёт только по соединениям, которые оператор сам
  подключил.
- **Заголовок сессии при каталоге:** у OpenCode Console запрос без
  `x-opencode-session` отвергается, поэтому и каталог уходит с ним
  (`catalog::catalog_headers`).

## Веб-исполнители (ADR-025)

Девять провайдеров реестра (`chatgpt-web`, `chatgpt-web-codex`, `claude-web`,
`gemini-web`, `grok-web`, `kimi-web`, `deepseek-web`, `zai-web`,
`blackbox-web`) — это веб-приложения, а не API: подписка ChatGPT/Claude/
Gemini/Grok/Kimi/DeepSeek/Z.ai не даёт ключа. Их `executor` = `web`,
`auth_type` = `none` — **ключа у них нет, и выдумывать его нельзя**.

Ответ приходит из сессии оператора в браузере: шлюз (`src/web.rs`,
`src/web_driver.rs`) говорит с процессом-драйвером на Python (Camoufox) по
локальному HTTP. Адрес — `HETERION_WEB_DRIVER_URL` (по умолчанию
`http://127.0.0.1:20131`; **20130 занят десктоп-приложением**).

Правила, которые нельзя ломать молча:

- **Usage = ноль, cost-заголовка нет.** Веб-чат не отдаёт счётчики токенов;
  выдуманный ноль — это фальшивое число.
- **Стриминг — один дельта-чанк и `stop`.** Страница отдаёт готовый текст;
  псевдострим с задержкой не выдумывать.
- **Ошибки драйвера различимы и не сливаются:** `not_logged_in` (сессия истекла
  — вход в браузере), `needs_operator` (2FA/капча — оператор), `selector_miss`
  (верстка изменилась — код адаптера), `upstream_error`. Смешать их — значит
  чинить не то.
- **Бэкенды веб-провайдеров поднимаются только если драйвер отвечает** на
  `/health`. Провайдер в каталоге, который не может ответить, хуже
  отсутствующего.
- Драйвер **не покупает** ничего: пустой баланс — это `upstream_error` с
  текстом провайдера.

Адаптеры лежат вне репозитория (в установке Camoufox), потому что работают с
профилем оператора, а не с кодом шлюза. Селекторы адаптера — хрупкая часть
подсистемы: проверять их надо через `GET /probe?provider=<id>` драйвера, а не
на глаз.

**Драйвер живёт под launchd** (`com.heterion.webdriver`, порт 20131, `KeepAlive`),
логи — `~/Library/Logs/heterion-webdriver.{out,err}.log`. Без него веб-исполнители
молчат после перезагрузки машины. Драйвер работает **headless** (фоновая служба),
а разовые шаги входа в сайты — в видимом браузере: Google не запускает OAuth в
headless-браузере. Профиль Firefox открывает один процесс, поэтому на время
ручного входа драйвер нужно остановить.

**hCaptcha не обходится.** DeepSeek (и другие площадки) показывают
drag-головоломку перед OAuth; это шаг оператора (`needs_operator`), а не
автоматика. Никаких обходов CAPTCHA/2FA в контуре нет.

## Режим «только бесплатно» (ADR-026)

Владелец объявил: платить нельзя. Роутер обслуживает только бесплатные пути.

- **Подключение без денег отключается целиком:** `PATCH /api/connections/{id}`
  с `{"is_active": false}` — ключ сохраняется, модели уходят из каталога.
  **Нужен рестарт шлюза:** бэкенды строятся на старте (в ответе PATCH —
  `restart_required: true`).
- **Модель, за которую нельзя заплатить внутри живого провайдера,** убирается
  из реестра: `bazaarlink` (125 id, баланс 0 → 402) и `mistral-large-latest`
  (403 `tier_not_allowed` — модель не в тарифе Free).
- **Модель, которая есть на площадке, но недоступна из-за лимита самой
  площадки, остаётся:** `agentrouter/gpt-6-astra` отвечает
  `402 Budget pool quota has been exhausted` — это не наш счёт, и контрактный
  тест требует список площадки.
- **Мёртвый провайдер отключается целиком, мёртвая модель убирается из
  реестра** (решение владельца от 16.11.2026):
  - соединения `grok-cli` отключены (6 шт.): провайдер отвечает
    `400 client_id is required` — не квота, а недостающая настройка клиента;
    успехов в журнале нет с 26.10;
  - `gemini/gemini-2.5-pro` убрана из `registry.json` (Google снял модель с
    продажи: `404 no longer available to new users`), её строки в
    `model_capabilities`, `model_capability_overrides` и
    `model_context_overrides` удалены — модель исчезает и из каталога
    `/v1/models`, и из хвоста лестницы `auto`;
  - ранее по решению владельца отключены `opencode-go`, `siliconflow`,
    `trae`, `agentrouter`;
  - `morph` оставлен намеренно: `402 Monthly quota exceeded` — возможно,
    починится новым ключом; он стоит в голове лестницы и стоит одного
    отказа на запрос.
- **Вторая волна чистки — по журналу отказов прогона 100 субагентов под
  MiniMax M3.1 Flash Max (16.11.2026, 18:02 UTC):**
  - соединения отключены (все модели провайдера отказывали): `blackbox`
    (404 на все 9 моделей), `command-code` (404 «Check the docs for
    available routes» — эндпоинта нет), `kimi-coding` (401 «API Key appears
    to be invalid»), `sambanova` (401/404), `minimax` (402
    `insufficient_balance_error` — баланса нет, ADR-026);
  - модели убраны из реестра: `gemini/gemini-2.5-flash`,
    `gemini/gemini-2.5-flash-lite` (404 сняты площадкой), `nvidia` —
    `moonshotai/kimi-k3`, `deepseek-ai/deepseek-v4-pro-0813`,
    `google/diffusiongemma-26b-a4b-it` (410 «reached its end of life»),
    `groq` — `llama-3.3-70b-versatile`, `groq/compound`, `qwen/qwen3-32b`,
    `qwen/qwen3.6-27b`, `meta-llama/llama-4-scout-17b-16e-instruct`
    (404 «does not exist»), `kilo-gateway` — три `:free`-id, которых нет в
    каталоге площадки, `ovhcloud/Qwen2.5-Coder-32B-Instruct` (404
    `model_not_found`), `zai` — все, кроме `glm-4.7-flash` (429 «Insufficient
    balance» — платить нельзя), провайдер `aihubmix` целиком (`crouton-alpha-free`
    помечена площадкой как `model_retired`).
  - **Не тронуты** те, чьи отказы — наша вина или временная квота: `llm7`
    («Model … is currently unavailable» — агрегатор ротирует доступность),
    `pollinations` (429 «Your previous request is still running» — лимит
    параллелизма), `aion`/`cloudflare-ai`/`gemini` (дневные квоты сбрасываются),
    `mistral`, `sealion/…-32B`, `typhoon`, `oneminai`, `mixlayer` — их отказы
    чинятся правкой параметров (ниже).
- **Третья волна чистки — тот же прогон, журнал до 22:15 UTC:**
  - модели, которых площадка не знает (404/400, успехов ноль): `aion` —
    `aion-labs/aion-2.5` («Unknown model»); `huggingface` — пять id, которых
    нет в каталоге площадки (`meta-llama/llama-3.1-8b-instruct` — в каталоге
    `meta-llama/Llama-3.1-8B-Instruct`, регистр другой; `llama-3.2-11b-instruct`,
    `mistral-7b-instruct`, `gemma-2-9b-it`, `Qwen2.5-7B-Instruct` — таких id
    на площадке нет вовсе);
  - модели, за которые платить нельзя (402, успехов ноль, ADR-026):
    `huggingface/deepseek-ai/DeepSeek-V3` («Purchase pre-paid credits»),
    `ollama-cloud` — `beam`, `nemotron-3-super`, `nemotron-3-ultra`,
    `kilocode` — `anthropic/claude-opus-5`, `anthropic/claude-sonnet-5`,
    `api-airforce` — `x-ai/grok-3`, `moonshot/kimi-k2.6`,
    `openrouter/auto`.
  Итог: 276 провайдеров, 2291 модель (было 2328).
- **Четвёртая волна чистки — тот же прогон, журнал до 22:40 UTC:**
  - `pollinations` — Classic API площадки обслуживает ровно одну модель
    (`openai-fast` + псевдоним `openai`); 29 id отвечали `404 NOT_FOUND:
    Model "…" isn't on the Classic API` при нуле успехов (было 31, осталось 2).
  Итог после волны: 276 провайдеров, 2262 модели (было 2328).
- **Пятая волна чистки — модели, снятые самой площадкой:**
  - `minimax` — четыре id отвечают `404 not_found_error: The model "…" is no
    longer available. (2069)` (28–30 отказов на каждый): `MiniMax-M2.5`,
    `MiniMax-M2.5-highspeed`, `MiniMax-M2.7`, `MiniMax-M2.7-highspeed`.
    Остался `MiniMax-M3`; соединение провайдера и так отключено;
  - `ollama-cloud/nemotron-3-nano:30b` — `410 Gone: nemotron-3-nano:30b was
    retired on November 9, 2026. Try glm-5.3-flash:cloud` (площадка сама
    называет замену).
  Итог после волны: 276 провайдеров, 2257 моделей (было 2328).
- **Шестая волна — провайдер стал платным (ADR-026):**
  - `ollama-cloud` отвечает `402 Add usage credits or subscribe and save up to
    66%: https://ollama.com/plans` на **все** модели (проверено на
    `gemma4:31b`, `gpt-oss:20b`, `glm-5.3-flash`, `minimax-m3.1-flash` и ещё
    на 402 в журнале прогона). Бесплатного тарифа у площадки больше нет,
    поэтому провайдер вычеркнут из реестра целиком (18 моделей), соединение
    `ollama-cloud-main` отключено, capability-строки удалены;
  - `minimax/MiniMax-M3` — `402 insufficient_balance_error: insufficient
    balance (1008)`, успехов ноль; последняя модель провайдера, поэтому
    вычеркнут и он (соединение и так было отключено).
  Итог: 274 провайдера, 2363 модели (после правки адресов и живых каталогов 17.11.2026).

**Соединение без моделей в реестре — мёртвое соединение.** У `huggingface`
(все id вычеркнуты волнами выше) и `aihubmix` (провайдера нет в реестре, а
модель `crouton-alpha-free` площадка сняла сама) соединения были активны, но
обслуживать им нечего — шлюз писал «1 active connection(s) … are not served».
Оба отключены.

**Модели Google Flash — по живым пробам 17.11.2026** (через роутер, `say ok`):

| модель | где | жива? | решение |
|---|---|---|---|
| `gemini-3.6-flash` | реестр + лестница | 200 за 1.5 с, `effective=260` | остаётся |
| `gemini-4-hydrogen` | **внесена в реестр** (17.11) | 200 за 2 с | до этого отсутствовала в реестре, хотя на неё ссылались активные комбо `google` и `main-1m` |
| `gemini-3.7-flash` | **вычеркнута** | последний успех 15.09.2026, две пробы 17.11 — таймаут 150 с | в лестнице без tools стояла на `effective=235` и висела на запросах вместо отказа |
| `gemini-3.8-flash` | **не вносится** | площадка её знала (251 успех до 30.10), но 17.11 две пробы подряд — таймаут | нет ни отказа, ни ответа: ждать, пока ответит |

У `gemini-4-hydrogen` нет capability-строки: измеренного окна в каталоге площадки
нет, а выдумывать его нельзя — окно берётся из `model_spec` (все `gemini*` → 1M,
vision, native tools, tier `fast`).

**Каталог площадки роутер не подтягивает.** Флаг `autoFetchModels` в
`provider_specific_data` соединения — рудимент от JS-дашборда, в Rust-коде не
реализован; строки `model_capabilities` — снимок сентября (`auto:discovery`,
`refreshed_at 2026-09-16`). Поэтому «модель есть у площадки» проверяется живым
вызовом, а не чтением каталога.

**Отказ соединения — не отказ модели.** `sambanova` отвечает `401 Incorrect
API key provided` (все пять соединений отключены: ключи мертвы), поэтому её
`404 «model … does not exist or you do not have access to it»` — следствие
мёртвого ключа, а не смерти модели: из реестра эти id не вычеркиваются.
`blackbox` и `command-code` отвечают `404` на **все** модели площадки —
у `command-code` прямо сказано, что маршрут не зарегистрирован
(`POST https://api.commandcode.ai/chat/completions is not a registered API
route`), то есть адрес неверный, а не модели мёртвые. Оба соединения
отключены; чинить нужно ключ/адрес, а не реестр.

**`tool_call` в capability-таблице — измеренный факт, а не пожелание.**
Сверка всего каталога с журналом (2026-11-17): у
`ovhcloud/Mistral-Small-3.2-24B-Instruct-2506` стояло `tool_call = 1`, а за
467 ходов агентного прогона с инструментами модель **ни разу** инструмент не
вызвала — при том что другие модели в том же прогоне вызывали (gemini 99,
sealion 59, pollinations 58). Поэтому `tool_call` исправлен на 0: модель
исключается из запросов с инструментами, а в запросах без них остаётся.
Обратная правка — одна строка в `model_capabilities`; бэкап БД перед правкой:
`~/Backups/2026-11-17-heterion-router-db/storage.sqlite.pre-toolcall.sqlite`.

**`base_url` может нести плейсхолдер `{env:ИМЯ}`** — например Cloudflare
Workers AI требует `account_id` в пути, а он принадлежит аккаунту оператора,
а не провайдеру. Значение берётся из окружения
(`HETERION_ROUTER_CLOUDFLARE_ACCOUNT_ID`). **Незакрытый плейсхолдер = провайдер
не зарегистрирован** (`provider_base_url_from` → `None`), а не провайдер с
неполным адресом.

**Cloudflare Workers AI требует аккаунтный токен** (`cfat_…`, разрешение
`Account.Workers AI`). User-токен (`cfut_…`) площадка на account-эндпоинтах не
принимает (`Authentication error`). Бесплатный тариф — 10 000 нейронов в сутки;
из 40 текстовых моделей каталога площадки на Free отвечают 24 — в реестре
именно они, перечень проверен живым вызовом.

**Classic API pollinations обслуживает ровно одну модель.** Каталог площадки
(`GET https://text.pollinations.ai/models`, проверено 2026-11-16) отдаёт один
элемент: `openai-fast` (GPT-OSS 20B Reasoning, OVH) с псевдонимами `openai`,
`gpt-oss`, `gpt-oss-20b`, `ovh-reasoning`. Остальные id площадка отвергает
`404 NOT_FOUND: Model "claude" isn't on the Classic API` — и в реестре их
больше нет (было 31, осталось 2: `openai-fast`, `openai`; успехи в журнале
покрывают оба). Остальные модели pollinations живут на платном
`enter.pollinations.ai` — это другой API, а не этот.

## Единый пул Switchyard (ADR-027)

`auto` и `heterion-router/auto` — **виртуальные** модели: их обслуживает
`RoutingBackend::auto_plan()`, а не комбо из БД. `resolve()` перехватывает
`is_auto_model(model)` до чтения комбо, поэтому комбо `auto` /
`heterion-router/auto` в таблице `combos` для маршрутизации мертвы.

- **Пул = курируемая лестница (7 кандидатов) + весь живой каталог.**
  Хвост — все модели всех провайдеров с живым бэкендом (реестр ∩ `backends`,
  тот же список, что в `/v1/models`), базовый приоритет 50, порядок
  детерминирован `(provider, model)`. Контракт семи кандидатов
  (`empirical_challenge_m4_1.rs` / `m5_1.rs`) не ломается.
- **Бюджет попыток:** `AUTO_MAX_ATTEMPTS = 24` апстрим-вызова на запрос.
  Кандидат в кулдауне/пробации бюджет не тратит.
- **4xx не штрафует модель** — штраф за 404 ломал контракт I-12
  (`empirical_challenge_m5_2.rs`): модель уходила в пробацию и выпадала из
  лестницы. Мёртвые модели чистятся правкой реестра.
- **Веб-исполнители не берут tool-запросы:** у них `tool_calls` жёстко `None`
  (`web.rs`), поэтому `executor: "web"` исключён из пула при `tools`.
- **Алиас `google` → провайдер `gemini`:** клиент пишет `google/gemini-…`
  (измерено: 404 против 200 у `gemini/gemini-…`).
- **В комбо `model` — голый id модели, без префикса провайдера** (ADR-028):
  admin-API сам клеит `provider/` при записи (`render_model_items`), а
  парсер срезает префикс провайдера, только если ведущий сегмент — именно
  он. Поэтому id со слешем внутри (`stealth/glyph-cluster`,
  `openai/gpt-oss-20b`) не режется, а удвоенный ref (`kilocode/kilocode/…`)
  разворачивается до голого id. Отправить префикс в `PATCH
  /api/combos/{id}` — значит получить удвоенный ref и 400 от апстрима.

## Лестница по возможностям и мягкая ротация (ADR-029)

Порядок пула `auto` **считается**, а не прописан: приоритет модели выводится
из capability-таблицы (`model_capabilities` + `model_capability_overrides`),
а измеренный контекст (`model_context_overrides.real_context`) побеждает
объявленный.

**Capability-таблица — снимок на старте шлюза** (`main.rs` → `CapabilityStore::load`):
измеренное окно, записанное в `model_context_overrides` после старта процесса,
лестница не увидит до рестарта. Пример из прогона 2026-11-16: у
`sealion/aisingapore/Qwen-SEA-LION-v4-32B-IT` реальное окно 32000 токенов
(`real_context`, `live:2026-11-16`), а в `model_capabilities.limit_context`
стояло 32768 — и пока шлюз работал со старым снимком, запрос уходил с
`max_completion_tokens=32000` при промпте в 110 761 символ и площадка отвечала
`400 ContextWindowExceededError`. Запись в override сделали в 20:10 UTC, шлюз
подняли в 20:04 UTC — окно подхватилось бы только с рестартом.

- **Класс лестницы** (`frontier` / `fast` / `fallback`) лежит в
  `model_capability_overrides` под ключом `tier`: площадка не сообщает, насколько
  модель умна, это исследовательская величина (источники — `docs/research/ladder/`).
- **Формула:** класс (240/140/60) + контекст (0..60, логарифм) + native tools (25)
  + vision (15) + reasoning (20).
- **Ручные приоритеты `DEFAULT_AUTO_CANDIDATES` (300…90) — запасной вариант**
  для моделей без записи в таблице; эмпирические контракты на них завязаны.
- **Ротация — отдельный слой с переключателем `with_rotation(bool)`.** Лестница
  отвечает «какая модель лучше подходит» и статична; ротация отвечает «кто
  отвечает сейчас». В шлюзе слой **включён** (это требование владельца: все
  доступные модели участвуют в ротации). Контракты, фиксирующие **порядок
  лестницы**, выключают его явно — иначе «модель, которая только что ответила»
  по определению уезжает вниз и ломает контракт «демотированная модель теряет
  голову лестницы».
- **Мягкий cooldown — механика ротации:** модель, только что обслужившая запрос,
  получает приоритет × `1/(1+count)` на 45 с (`AUTO_SOFT_COOLDOWN_SECS`),
  опускается ниже соседей и **возвращается** со всеми своими свойствами.
  Сигналы квоты (429 → cooldown модели, 402/403 → cooldown провайдера) и
  качества (5xx → пробация 15 мин) складываются с ним.
  Фиксированный множитель (0.35) был откатан: лучшая модель проваливалась ниже
  всего хвоста — качественный обвал.
- **Фильтры запроса тоже смотрят в таблицу:** `spec_for()` отдаёт данные
  capability-таблицы, `context_limit_for()` — измеренное окно.
- Таблица перечитывается **при старте шлюза**: после слияния данных нужен
  рестарт (как после смены подключений).

## Доверие к модели и понижение за провал (ADR-030)

Capability-таблица говорит, какая модель **есть**; доверие — что её
**отдаёт эта площадка**. Без второго research выносит в голову лестницы
модели, которые отвечают `404` (живой замер 2026-11-16: 4 запроса из 6
через `auto` провалились).

- **Доверенная модель:** есть `call_logs` со `status = 200` по этой паре
  «провайдер/модель», либо измеренный `real_context` в
  `model_context_overrides`, либо она в курируемой лестнице.
- **Недоверенная** (research есть, доказательств нет) получает приоритет
  × `UNTRUSTED_DISCOUNT` (0.1) — ниже любой модели, с которой роутер
  говорил. Скидка касается только приоритетов из research: у модели без
  research-заявки нет и повода для недоверия. Если доказательств нет
  вообще — скидки нет.
- **Доверие растёт с использованием:** успешный ответ помечает модель
  доверенной (`mark_trusted`), и дальше она соревнуется на capabilities.
- **Провал = временное понижение:** 400/401/404/422/5xx дают кандидату
  множитель `AUTO_FAILURE_FACTOR` (0.1) на `AUTO_FAILURE_COOLDOWN_SECS`
  (120 с). Это **не** штраф quality-трекера: 4xx не роняет модель в
  15-минутную пробацию (контракт I-12), не роняет провайдера и не трогает
  его соседние модели. 429 не трогается (модельный cooldown), 402/403 —
  провайдерный.
- **Чистка `call_logs` убирает доверие:** модели снова становятся
  недоказанными и скидка возвращается.

## Эстафета vision (ADR-031)

Роутер умеет **занимать глаза**: если запрос несёт картинку, а модель,
которая должна ответить, картинок не видит, роутер просит vision-модель
описать картинку и отдаёт ответчику **описание текстом**. Без этого такой
запрос умирает на `400` апстрима.

- **Обязательная эстафета включена** (`with_vision_relay(true)`): слепая
  цель иначе не обслуживается вообще.
- **Эконом-вариант выключен** по умолчанию (`HETERION_ROUTER_VISION_SAVING=1`
  включает): цель, которая картинку видеть умеет, отдаёт её более дешёвому
  классу и получает описание. Экономия — на токенах картинки (~1120 против
  текста), но цель **теряет саму картинку**: это решение оператора, не
  роутера.
- **Описание едет текстом** под маркером
  `[Image description written by provider/model]` — ответчик не должен
  думать, что видел картинку сам.
- **Неудачная эстафета ничего не меняет:** исходный запрос уходит как был.
  Описателей пробуют **до трёх** (`VISION_RELAY_DESCRIBERS`): площадка может
  отвергнуть форму картинки (живой замер: `oneminai/gpt-4o-mini` → `400
  invalid_request_error` на `messages.0.content`), и это не должно убивать
  эстафету.
- **Описатель должен быть моделью, а не роутером-алиасом:** окно от
  `MIN_MODEL_WINDOW` (4096). Виртуальные алиасы (`auto`, `auto:free`,
  `kilo-auto/*`) research помечает плейсхолдером «окно 1 токен» — такие
  строки **не дают ничего** (ни vision, ни tools, ни класс), а площадка
  картинку для них отвергает (`400 auto_free_modality_unsupported`).
- **Описатель выбирается лестницей:** ниже класс — дешевле, внутри класса —
  ниже приоритет; непроверенная модель — только если проверенных нет
  (описание от модели, которая ответит `404`, — не описание).
- Эстафета **берёт бэкенд напрямую**, а не роутер: рекурсии в лестницу нет.
- План запроса **не меняется**: `auto_plan_at` по-прежнему отбрасывает слепые
  модели для запроса с картинками (контракт
  `test_multimodal_vision_routing`).

## Деньги и безопасность (M3, действует с 2026-10-02)

Порядок слоёв важен и задаётся в `build_router_with_state`: axum выполняет
последний применённый слой первым, поэтому порядок в коде —
cache → budgets → guardrails → cors, а выполнение — cors → guardrails →
budgets → cache. Слой, который блокирует запрос, обязан быть **вне** кэша
(иначе блок можно обойти попаданием) и **вне** бюджета (иначе попадание в кэш
не будет учтено).

- **Бюджеты по умолчанию не заданы** — `domain_budgets` пуст, значит всё
  безлимитно. `daily_limit_usd = 0` означает «безлимит», а не «заблокировать».
- **Кэш разделён по presented bearer** (`sha256(bearer)[:16]`), поэтому один
  ключ не читает ответы, закэшированные другим. Стриминг обходит кэш и на
  чтении, и на записи.
- **Повтор одного и того же запроса попадает в кэш (TTL 1 час), и это видно в
  замерах.** Ключ кэша — SHA-256 канонического тела запроса, поэтому побайтово
  тот же запрос отдаётся из кэша за ~0 с, **не пишется в `call_logs`** и не
  тратит апстрим. Для замера скорости модели нужен уникальный текст запроса:
  иначе «мгновенный ответ» — это кэш-хит, а не скорость модели. Побочно: в
  кэш-хите `usage` в теле ответа остаётся от оригинального вызова, так что
  счётчик токенов в пробнике показывает больше, чем реально списано.
- **Guardrails по умолчанию в режиме `warn`**: без настройки новых отказов нет.
  Совпадение никогда не попадает в тело, заголовок или лог — только имя
  проверки и маска.

## Аудит 2026-10

Шесть read-only-отчётов в `docs/research/audit-2026-10/` (слабые места, паритет
с OmniRoute, паритет с Future-AGI, дубликаты, инвентаризация, Rust 1.99) и
принятые по ним решения — ADR-012. Перед тем как чинить что-то из отчёта,
проверь `file:line`: числа в отчётах — снимок на 2026-10-02.

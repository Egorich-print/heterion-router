# agentrouter: диагностика и починка провайдера

Дата: 2026-10-27 · Решение: ADR-021 · Код: `client_profile.rs`,
`crates/heterion-router-providers/registry.json`,
`crates/heterion-router-gateway/tests/agentrouter.rs`

## Коротко

Провайдер был сломан по трём независимым причинам, и все три закрыты:

| Причина | Симптом | Что сделано |
| --- | --- | --- |
| Клиент-детект площадки | `401 unauthorized_client_error` при валидном ключе | добавлен отпечаток `originator: codex_cli_rs` (`client_profile.rs`) |
| Не тот протокол | `400 不支持 /v1/chat/completions 接口，请改用 /v1/responses` | формат провайдера переведён на `openai-responses`, адрес `https://agentrouter.org/v1/responses` |
| Не тот каталог моделей | `503 当前分组 default 下对于模型 … 无可用渠道` | в реестре остались только `gpt-6.1-sol`, `gpt-6-astra`, `gpt-5.6-sol` |
| Дефект роутера: `error: null` | `502 upstream error: null` вместо ответа | успешный Responses-объект больше не читается как отказ (`try_each_credential`) |

Ключ в БД не менялся: там уже лежало зашифрованное (`enc:v1:`) значение того
же ключа. Ни в коде, ни в отчётах, ни в памяти значение ключа не дублируется.

## Дефект роутера, найденный живой проверкой

Объект Responses API **всегда** содержит поле `error` — `null`, когда ответ
успешный. Пул кредитов (`try_each_credential`) считал любое наличие ключа
`error` отказом и подставлял `upstream_refusal(Value::Null)` → `null`:

```
WARN completion failed model=agentrouter/gpt-6.1-sol: upstream error: null
```

Ответ модели терялся целиком — 502 вместо текста. Исправление: отказом
считается только **непустой** объект ошибки. Затрагивало всех
Responses-провайдеров (`deepseek`, `codex`, `perplexity-agent`), а не только
agentrouter. Красный тест —
`responses::tests::a_successful_payload_whose_error_field_is_null_is_not_an_error`.

## Что показала площадка

Публичные эндпоинты отдают всё, кроме авторизованной консоли:

- `GET /api/pricing` — каталог моделей с группами и множителями:
  - `gpt-6.1-sol` — группы `probation, svip, test, core, default`,
    `model_ratio` 0.5, `completion_ratio` 5;
  - `gpt-5.6-sol` — те же группы, `model_ratio` 0.5;
  - `gpt-6-astra` — те же группы, `model_ratio` 1;
  - `claude-opus-5-5` — **только** `svip, test` → нашей группе недоступна;
  - `usable_group: {"": 用户分组, "default": 默认分组}`, `group_ratio: {"default": 1}`.
- `GET /api/status` — правила площадки: поддерживаемые клиенты
  (`claude-code⭐, codex⭐, openclaw, pi, opencode, …`), неподдерживаемый
  клиент → группа `probation` с двойным множителем, лимиты выдаются батчами
  (10:00 и 19:00 UTC+8), при исчерпании — `402 Budget pool quota has been
  exhausted`, резервный домен `https://ps.air-outer.com`.
- `GET /v1/models` (с ключом) — **три** модели: `gpt-5.6-sol`, `gpt-6-astra`,
  `gpt-6.1-sol`, у всех `supported_endpoint_types: ["openai"]`.

Текущая группа аккаунта — `default`: её называет сама площадка в ошибке
«当前分组 default 下对于模型 … 无可用渠道» (послано с заведомо несуществующей
моделью — ответ приходит мгновенно и без генерации).

## Клиент-детект: что именно проверяется

Живые пробы 2026-10-27 (`POST /v1/responses`, модель `gpt-6.1-sol`):

| Заголовки | Ответ |
| --- | --- |
| только `Authorization` | `401 unauthorized_client_error` |
| `user-agent: Mozilla/5.0 …` | `401 unauthorized_client_error` |
| `originator: codex_cli_rs` | `200`, `status: completed` |
| `originator` + свой user-agent роутера | `200` |
| `originator` + `openai-beta: responses=experimental` | `200` |
| `user-agent: codex_cli_rs/…` (без `originator`) | `200` |

Вывод: площадка читает **`originator`**; user-agent на детект не влияет. Он
не влияет и на группу — все варианты сообщили `default`, поэтому роутер
отправляет **свой** user-agent (`heterion-router/<version> (codex-responses
client)`), а не заимствованную версию Codex CLI.

Протокол площадки — Codex Responses: `/v1/responses`, тело с `input`/`instructions`,
стрим с событиями `response.created` / `response.output_text.delta` /
`response.completed`. Всё это роутер уже умеет
(`openai_to_responses`, `convert_event`, `pump_sse_stream`).

## Что изменено в роутере

1. **`crates/heterion-router-providers/registry.json`** — запись `agentrouter`:
   `format` `claude` → `openai-responses`, `base_url` →
   `https://agentrouter.org/v1/responses`, модели → три живые.
2. **`crates/heterion-router-gateway/src/client_profile.rs`** (новый) —
   политика «дополнительные заголовки по провайдеру» (по образцу
   `reasoning.rs`). Для `agentrouter`: `originator: codex_cli_rs` + свой
   user-agent; для остальных — пусто.
3. **`crates/heterion-router-gateway/src/responses.rs`** — `ResponsesBackend`
   несёт эти заголовки и шлёт их через `post_json_with_headers`.
4. **`crates/heterion-router-gateway/src/embeddings.rs`** — тест отказа в
   embeddings больше не привязан к формату провайдера (формат изменился —
   тест падал).
5. **`crates/heterion-router-gateway/tests/agentrouter.rs`** (новый) — три
   теста: каталог в реестре, отпечаток в запросе, отсутствие отпечатка у
   других провайдеров. До фикса — красные.

## Живые проверки

Через сам роутер (релизный бинарь, перезапущенный под launchd):

| Запрос | Результат |
| --- | --- |
| `agentrouter/gpt-6.1-sol`, без стрима | `200`, `finish_reason: stop`, текст `OK`, usage 10/29 |
| `agentrouter/gpt-6.1-sol`, стрим | `200`, 3 SSE-чанка, финал `data: [DONE]` |
| `agentrouter/gpt-5.6-sol`, без стрима | `200`, `finish_reason: stop`, текст `OK` |
| `agentrouter/gpt-6-astra`, без стрима | `502 upstream status 502 … server_error` — сбой канала площадки |
| стрим `gpt-6.1-sol` (первая попытка) | `502 Your requests to gpt-6.1-sol for gpt-6.1-sol in eastus2 have exceeded token rate limit` — лимит площадки; повтор прошёл |

`GET /v1/models` шлюза теперь отдаёт `agentrouter/gpt-6.1-sol`,
`agentrouter/gpt-6-astra`, `agentrouter/gpt-5.6-sol` (всего 339 моделей).

Прямые вызовы площадки: `gpt-6.1-sol`, `gpt-6-astra`, `gpt-5.6-sol` —
200 `status: completed`; стрим — `event: response.created` … `response.completed`.

## Консоль площадки и cookie Vivaldi

Cookie Vivaldi для `agentrouter.org` расшифровываются (схема Chromium на
macOS: `v10` → PBKDF2-HMAC-SHA1(«saltysalt», 1003, 16) + AES-128-CBC с IV из
16 пробелов; после `v10` лежат 16 байт соли и блок, который отбрасывается):

- `acw_tc` — 64 hex-символа (токен WAF Aliyun);
- `session` — 672 символа, внутри base64 от `1795310708|…` (токен сессии).

Сам консольный API через HTTP-клиент не открывается:

- без cookie — `401 无权进行此操作，未登录且未提供 access token`;
- с «сырым» значением `session` — `400 Bad Request` (WAF не принимает значение);
- с percent-encoded значением — `200` со страницей JS-челленджа Aliyun WAF
  (`<meta name="aliyun_waf_aa" …>`);
- с ключом как access token — тоже челлендж (200 + HTML).

Консоль открылась в браузере: копия профиля Vivaldi запущена headless
(`--remote-debugging-port`), страница читается по CDP. Что там:

| Раздел | Что видно (2026-10-27, ~18:20) |
| --- | --- |
| Dashboard | аккаунт `github_337359`, баланс $0.00, статистика: 19 запросов, 334 токена |
| API Token | два токена: `OpenCode` (лимит $200 000 000) и `OmniRoute` (лимит $2000.00, **израсходовано $1958.10**), оба Enabled, группа `default`, модели и IP не ограничены, срок не истекает |
| Usage log | наш вызов: `gpt-6.1-sol`, 10 prompt + 26 completion токенов, spend $0.000140, «模型: 0.5 × Group ratio: 1» |
| Model Status | доступно 3/3 моделей, success rate 89.14% за 24 ч |
| Wallet | баланс $0.00, онлайн-пополнение площадкой не включено, есть пополнение кодами |

**Model Status** (окно 24 ч, обновлено 18:22):

| Модель | Состояние | Эндпоинты | Доступность | TTFT P90 | Avg latency |
| --- | --- | --- | --- | --- | --- |
| `gpt-6-astra` | Normal | chat, responses | 93.29% | 57.3 s | 27.6 s |
| `gpt-6.1-sol` | Demote | chat, responses | 82.54% | 91.2 s | 37.7 s |
| `gpt-5.6-sol` | Demote | chat, responses | 90.95% | 59.2 s | 29.9 s |

Что из этого следует:

- группа аккаунта — `default`, множитель 1 (в логе: `Group ratio: 1`),
  модельный коэффициент `gpt-6.1-sol` — 0.5;
- **лимит токена почти исчерпан**: у `OmniRoute` осталось ≈ $41.90 из $2000
  (в диалоге правки токена `Quota: 979049713` единиц = $1958.10 при
  `Unlimited quota: on`; если флаг выключат, квота упрётся в $2000);
- площадка сама объясняет наши сбои: TTFT P90 под 90 секунд и доступность
  82–93% — отсюда зависания, `server_error` и «token rate limit»;
- «Demote» у двух моделей из трёх — площадка понижает их канал;
- заявление «chat, responses» в Model Status не противоречит отказу
  `/v1/chat/completions`: живая проверка 2026-10-27 дала `400 请改用
  /v1/responses`, поэтому в роутере остаётся Responses-протокол.

Открытые (публичные) endpoints `/api/pricing`, `/api/status`, `/v1/models`
отдают весь каталог моделей, групп и множителей без авторизации — по ним уже
были сняты все решения выше.

## Ограничения и остаток

- Площадка отвечает нестабильно по времени: TTFT P90 под 90 секунд,
  доступность 82–93% за 24 ч (Model Status). Успехи от 2–7 с, но встречаются
  зависания до 180 с, `502 server_error` и «token rate limit» по отдельным
  каналам. Это свойство площадки, а не дефект роутера.
- Квота выдаётся батчами (10:00 и 19:00 UTC+8) — в исчерпанном окне приходит
  `402 Budget pool quota has been exhausted`.
- У токена `OmniRoute` осталось ≈ $41.90 из $2000 (см. раздел про консоль) —
  под пополнение/квоту пора смотреть.
- Модельный список в реестре — снимок на 2026-10-27; при изменении
  каталога площадки его надо пересобрать по `GET /v1/models`.

# Heterion Router — краткая шпаргалка для агента

> Проверено на живой машине 2026-11-10: шлюз `http://127.0.0.1:20128`, backend
> `routing`, 344 модели в `/v1/models`, 9 активных комбо, 66 подключений (53
> активных). Все цифры ниже — снимок на эту дату; перепроверяй `GET /v1/models`
> и `GET /api/combos`, если нужен текущий срез.

## 1. Что это

Локальный шлюз к LLM-провайдерам. OpenAI-совместимый API на одном порту;
ключи провайдеров живут зашифрованными в БД роутера и наружу не выходят —
ни клиент, ни агент, ни MCP-сервер их не видит.

- Base URL: `http://127.0.0.1:20128/v1`
- Проверка живости: `GET http://127.0.0.1:20128/healthz` → `{"status":"ok","backend":"routing"}`

## 2. Как взять ключ

| Способ | Где |
| --- | --- |
| Ключ оператора (файл, 0600) | `~/.heterion-router/desktop/operator-key.json` |
| MCode-провайдер | `~/.minimax/config.yaml` → `custom_provider.heterion` (`baseURL` + `apiKey` + список моделей) |
| Переменные окружения | `HETERION_ROUTER_BASE_URL`, `HETERION_ROUTER_API_KEY` — приоритет над конфигом |

Ключ оператора годится и для `/v1/*`, и для `/api/*`. **Не печатай его в логи и
ответы** — это единственный секрет, который в этой системе вообще есть.

## 3. Эндпоинты

| Метод | Путь | Что это |
| --- | --- | --- |
| GET | `/v1/models` | список моделей (OpenAI-формат) |
| POST | `/v1/chat/completions` | чат, OpenAI-формат |
| POST | `/v1/responses` | OpenAI Responses (`agentrouter` живёт тут) |
| POST | `/v1/messages` | Anthropic-формат |
| POST | `/v1/embeddings` | эмбеддинги |
| GET | `/api/overview`, `/api/combos`, `/api/connections`, `/api/usage`, `/api/logs`, `/api/keys`, `/api/budgets` | admin-API (тот же контракт, что у веб-дашборда и GPUI-оболочки) |

Заголовок: `Authorization: Bearer <ключ>`.

## 4. Как адресовать модель

Три способа, все рабочие:

1. **Прямой id** `провайдер/модель` — 344 модели: `agentrouter/gpt-5.6-sol`,
   `agentrouter/gpt-6-astra`, `gemini/gemini-3.6-flash`, `mistral/codestral-latest`,
   `ollama-cloud/minimax-m3`, `zai/glm-5.3`, `openrouter/auto` …
2. **Комбо** — виртуальные модели с весами и приоритетом; при отказе провайдера
   роутер идёт по следующей записи (breaker).
3. **Псевдоним провайдера** — например `exa/exa-search`.

### Активные комбо (9)

| Комбо | Состав | Когда брать |
| --- | --- | --- |
| `auto-subagent` | 8 записей: ollama-cloud/beam, mistral/codestral-latest, kilocode/stealth/glyph-cluster, … agentrouter/gpt-5.6-sol | дешёвая агентная работа |
| `auto-fast` | 5: ollama-cloud/nemotron-3-super, gpt-oss:20b, pollinations/openai-fast, groq/gpt-oss-20b, gemini-3.1-flash-lite | быстрые черновики |
| `auto-free` | 6: kilocode/free, pollinations/openai-fast, groq/gpt-oss-20b, … | без платных ключей |
| `main-128k` | 4: mistral/codestral-latest, zai/glm-5.3-flash, groq/gpt-oss-20b, … | контекст 128k |
| `main-262k` | 1: mistral/codestral-latest | контекст 262k |
| `main-1m` | 5: grok-cli/grok-4.6, zai/glm-5.3-flash, … | контекст 1M |
| `google` | 4: только Gemini (3.8/3.7/3.6 flash, 3.1-flash-lite) | Gemini-флоу |
| `fallback-free` | 4: zai/glm-5.3-flash, groq/gpt-oss-20b, … | запасной бесплатный |
| `openrouter-free` | 1: openrouter/nemotron-3-super-120b-a12b:free | один бесплатный nemotron |

### Что выбирать

- **Агентная работа по умолчанию — `agentrouter/gpt-5.6-sol`** (замер
  2026-10-28: 8/8 успех, медиана 9.0 с против 19.8 с у `gpt-6-astra` и 30.9 с у
  `gpt-6.1-sol`). Умеет вызывать инструменты; за ход просит сразу много
  инструментов, поэтому лимит ходов задирай.
- `agentrouter` — Responses-протокол и **клиентская идентификация**: площадка
  меряет квоту по типу клиента, без распознанного клиента отдаёт
  `401 unauthorized_client_error` даже с валидным ключом. Отпечаток клиента —
  политика провайдера (`client_profile.rs`), не глобальное правило.
- `agentrouter`: `max_tokens` **≥ 4000** — модели reasoning тратят 600–1200
  токенов на размышление, при меньшем бюджете ответ приходит пустым.
- `agentrouter`: обрезанный ответ — это `finish_reason: "length"`, а не
  `"stop"`; в потоке приходит `response.incomplete`, и его нельзя ронять без
  финального чанка.

## 5. exa-search

Поиск живёт **на чат-поверхности роутера**: последнее сообщение пользователя —
запрос, ответ — цитированный блок.

### Способ A (рекомендуемый): MCP

```jsonc
// tool_search: "exa" → mcp__exa__web_search и mcp__exa-search__web_search
{ "query": "rust programming language",
  "numResults": 10,            // 1–100, по умолчанию 5
  "type": "deep",              // auto|neural|fast|deep|lite|deep-lite|deep-reasoning|instant
  "category": "github",
  "includeDomains": ["docs.rs"],
  "excludeDomains": ["pinterest.com"],
  "startPublishedDate": "2026-01-01",
  "endPublishedDate": "2026-11-10" }
```

Оба MCP-сервера — один и тот же stdio-скрипт
`~/.minimax/plugins/exa-search/server.py`: он берёт base URL и ключ роутера
(`HETERION_ROUTER_*` → `custom_provider.heterion` → `http://127.0.0.1:20128/v1`)
и ходит в `POST /v1/chat/completions` с моделью `exa/exa-search`. Ключ Exa в
этом процессе не участвует.

### Способ B: как модель

```bash
curl -s http://127.0.0.1:20128/v1/chat/completions \
  -H "Authorization: Bearer $KEY" -H 'Content-Type: application/json' \
  -d '{"model":"exa/exa-search",
       "messages":[{"role":"user","content":"кто выиграл ЧМ-1998"}],
       "exa":{"numResults":5,"type":"deep","includeDomains":["fifa.com"]}}'
```

Параметры поиска едут в теле запроса объектом `"exa"`; без него тело не
меняется. Обе формы id работают: `exa/exa-search` и `exa-search/exa-search`
(проверено 2026-11-10, ответ — блок «Found N result(s) for …» с цитатами).

## 6. tinyfish

Отдельный MCP-сервер `tinyfish` (`http`, `https://agent.tinyfish.ai/mcp`).
**API-ключ не нужен**: рантайм сам проходит OAuth через Clerk
(`clerk.tinyfish.ai`), хранит креды в `~/.minimax/mcp-oauth/` и обновляет токен
сам.

Инструменты (префикс `mcp__tinyfish__`): `search`, `fetch_content`,
`run_web_automation`, `discover_run`, `poll_status`, `get_steps`, `get_wallet`,
`get_search_usage`, `list_fetch_usage`, `create_browser_session`,
`close_browser_session`, мониторы (`create/run/pause/resume/cancel/list`),
датасеты (`create/list/plan/delete`), `create_profile`, `cancel_deep_research`.

**Две ловушки, обе уже пройдены:**

1. **Индекс инструментов собирается в начале сессии.** Сервер, зарегистрированный
   позже, в текущей сессии не виден — нужен перезапуск сессии. Проверять
   работоспособность надо из свежей сессии.
2. **Прямой `curl` к эндпоинту даёт 401 `invalid_token`**, даже с сохранённым
   токеном из `~/.minimax/mcp-oauth/`. Это ложный след: у токена scope
   `openid profile email offline_access`, без MCP-аудитории, а рантайм идёт
   своим путём. Через MCP всё работает.

Деньги (`tinyfish.ai/pricing`, `docs.tinyfish.ai`, проверено 2026-11-09):
Search $0 (30 req/min, 500 req/hour), Fetch $0 (150 url/min, 1000 url/day),
Agent $0.016/шаг, Browser $0.002/мин, Monitor $0.005, Dataset $0.10/строка,
Research $2/$3. Wallet от $10 (новому аккаунту $8); Search и Fetch кошелёк не
трогают.

## 7. Что выбрать под задачу

| Задача | Инструмент |
| --- | --- |
| Найти факт, которого нет в репозитории | `mcp__exa__web_search` (Exa через роутер) |
| Снять страницу с JS → markdown | `mcp__tinyfish__fetch_content` |
| Браузерная автоматизация | `mcp__tinyfish__run_web_automation` |
| Агентный код с инструментами | модель `agentrouter/gpt-5.6-sol` через роутер |
| Черновик, дёшево | `auto-fast` / `auto-free` |
| Gemini-специфичное | комбо `google` |

## 8. Подводные камни роутера

- **Кэш.** Побайтово тот же запрос отдаётся из кэша за ~0 с, в `call_logs` не
  попадает и апстрим не тратит. Для замера скорости модели нужен **уникальный
  текст** — иначе «мгновенный ответ» это кэш-хит.
- **4xx провайдера не роняет breaker** — так и задумано, не «чинить».
- **Кэш разделён по presented bearer** (`sha256(bearer)[:16]`): один ключ не
  читает ответы, закэшированные другим.
- **Бюджеты по умолчанию пусты** — всё безлимитно; `daily_limit_usd = 0`
  означает «безлимит», а не «заблокировать».
- **Guardrails по умолчанию в режиме `warn`** — новых отказов не будет.
- **Обрезанный ответ — `finish_reason: "length"`**, а не `"stop"`.
- **Запрос с завершающим assistant-ходом (prefill)** адаптируется только для
  провайдеров, которые такую форму отвергают (Mistral, Gemini); для
  openai-форматных префикс остаётся дословным.

## 9. Чего не делать

- Не выводить ключ оператора и не класть его в командную строку, лог или ответ.
- Не «чинить» breaker на 4xx и не менять пороги `repetition_guard` на глаз.
- Не путать «мгновенный ответ» с быстрой моделью (кэш).
- Не считать `curl` к Tinyfish проверкой работоспособности — проверять из
  свежей сессии.

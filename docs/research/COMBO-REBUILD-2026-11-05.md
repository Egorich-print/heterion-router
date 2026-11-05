# Пересборка комбо auto-subagent: доступность × бенчмарки

Дата: 2026-11-05 · Проект: `heterion-router` · Комбо: `auto-subagent`
(priority, `context_length` 1048576) · Соединения: 53 активных

## Коротко

- Пересобрано комбо `auto-subagent`: **13 записей → 7**, порядок обхода
  перестроен по фактической доступности *сегодня*, а не по бенчмаркам вообще.
  Модели, которые отдают 403 по гео или «balance insufficient», из комбо
  убраны — они не давали ничего, кроме траты времени на обход.
- Из доступных моделей **первыми идут бесплатные**: `ollama-cloud/beam`
  (Beam 1.0 501B-A23B), `mistral/codestral-latest`, `mistral/open-mistral-nemo`,
  `pollinations/openai-fast`. Три модели agentrouter оставлены **последними**:
  по бенчмаркам они лучшие (DeepSWE 75.2 / 72.7 / 74.1), но баланс аккаунта
  почти исчерпан, и они съели 120M входных токенов за два дня.
- Попутно найден и починен баг в роутере: `open-mistral-nemo` получал
  `reasoning_effort` и отвечал **400** «reasoning_effort is not enabled for
  this model» — семейство `mistral*` в фильтре не покрывало префикс
  `open-mistral`. Теперь покрывает.
- Failover проверен живьём: с выключенным `ollama-cloud` комбо уходит на
  `codestral-latest`, с выключенными `ollama-cloud` и `mistral` — на
  `openai-fast`.

## Почему так

Старое комбо выглядело так (порядок массива = порядок обхода, `weight` —
метаданные для дашборда, Rust-роутер вес не читает):

| # | Модель | Что происходит сегодня |
| --- | --- | --- |
| 1 | `ollama-cloud/beam` | работает |
| 2 | `ollama-cloud/nemotron-3-ultra` | 429 — месячный лимит аккаунта исчерпан |
| 3 | `ollama-cloud/gemma4:31b` | 429 — тот же лимит |
| 4 | `ollama-cloud/nemotron-3-super` | 429 — тот же лимит |
| 5 | `ollama-cloud/gpt-oss:120b` | 429 — тот же лимит |
| 6 | `gemini/gemini-3-flash-preview` | 400 «User location is not supported for the API use» |
| 7 | `mistral/codestral-latest` | работает |
| 8 | `openrouter/grok-4.6` | 403 Cloudflare по гео |
| 9 | `pollinations/openai-fast` | работает |
| 10 | `kilocode/openrouter/free` | 403 |
| 11 | `gemini/gemini-3.6-flash` | 400 гео |
| 12 | `gemini/gemini-3.1-flash-lite` | 400 гео |
| 13 | `gemini/gemma-4-26b-a4b-it` | 400 гео |

То есть 10 из 13 записей были мёртвыми: четыре Gemini-дубликата давали
одинаковый 400 по гео, три OpenRouter-производных — одинаковый 403, четыре
ollama-cloud — одинаковый 429. В списке также не было ни одной платной
модели, кроме agentrouter, — а это самые сильные модели в каталоге.

## Новое комбо

Порядок обхода (первый = самый предпочтительный):

| # | Модель | Вес | Бенчмарки | Почему здесь |
| --- | --- | --- | --- | --- |
| 1 | `ollama-cloud/beam` | 90 | SWE-bench Verified 81.6, TB2.1 80.1, SWE-Pro v2-Hard 76.5, DeepSWE 43.3 | бесплатно, отвечает за ~1 с, tool-calling работает |
| 2 | `mistral/codestral-latest` | 80 | SWE-bench Verified ~43.5–60.1 | FIM-обученная, минимальная задержка, tool-calling работает |
| 3 | `mistral/open-mistral-nemo` | 45 | старая 12B-модель | единственная ещё живая Mistral-модель; дешёвый запасной вариант |
| 4 | `pollinations/openai-fast` | 35 | SWE-bench Verified 60.7 (high effort) — GPT-OSS 20B | без ключа, но 1 ответ / 45 с на IP |
| 5 | `agentrouter/gpt-6.1-sol` | 30 | DeepSWE 75.2, TB4.0 58.2, AA Index 50 | лучший DeepSWE, ~$0.65/задача — но платный |
| 6 | `agentrouter/gpt-5.6-sol` | 25 | DeepSWE 72.7, TB2.1 88.8, SWE-Pro 64.6, AA Coding Index 80 | AA Coding Agent Index 80 |
| 7 | `agentrouter/gpt-6-astra` | 20 | DeepSWE 74.1, AA Index 55 | сильнейший, но ~5× дороже Sol 6.1 за задачу |

Правило отбора: (1) только то, что отвечает сегодня; (2) среди отвечающих —
бесплатное вперёд, потому что баланс agentrouter на исходе; (3) внутри
уровня — по опубликованным агентным бенчмаркам.

Все четыре бесплатные модели проверены на tool-calling — `finish_reason:
tool_calls` с корректными аргументами.

## Измерения

### Что доступно из РФ (живая проба через роутер, 2026-11-05)

Прогнаны все 26 живых бэкендов и все модели, которые провайдер отдаёт в
своём `/models`. Результат:

| Провайдер | Отвечает | Комментарий |
| --- | --- | --- |
| `agentrouter` | **да** — `gpt-5.6-sol`, `gpt-6-astra`, `gpt-6.1-sol` | платно, баланс почти исчерпан |
| `ollama-cloud` | **да** — только `beam` | остальные 25 моделей: 429 (месячный лимит) / 402 / 451 |
| `mistral` | **да** — `codestral-latest`, `open-mistral-nemo` | остальные: 429 rate-limit |
| `pollinations` | **да** — `openai-fast` | 1 ответ / 45 с на IP (FREE_LIMIT, 402) |
| `aihubmix` | нет | 42 free-модели в каталоге, но trial-квота 10 запросов исчерпана |
| `gemini` | нет | 400 «User location is not supported» |
| `openrouter`, `kilocode`, `kilo-gateway`, `groq`, `xai`, `grok-cli` | нет | 403 по гео (весь домен OpenRouter) |
| `zai` | нет | 429 «Insufficient balance or no resource package» |
| `huggingface`, `deepseek`, `sambanova`, `ollama-cloud` (платные) | нет | 402 / 502 |
| `cline`, `kimi-coding`, `siliconflow`, `trae`, `command-code`, `blackbox` | нет | 401 / 404, токены истекли |
| `opencode`, `opencode-go`, `qoder`, `llm7`, `nvidia` | нет | 404 / 400 / 451 |

Отдельно проверено, что `openrouter` заблокирован **по IP**, а не по ключу:
403 отдают все пять ключей OpenRouter из БД, на любую модель, включая
несуществующую, и даже на `GET /` (весь домен).

### Починенный баг: `reasoning_effort` у `open-mistral-nemo`

Mistral валидирует тело строго и отвечает
`400 invalid_request_invalid_args` «reasoning_effort is not enabled for this
model» (code 3051). Роутер это уже знает для семейства `mistral*`
(`rejects_reasoning_effort` в `crates/heterion-router-gateway/src/reasoning.rs`),
но `open-mistral-nemo` начинается с `open-`, а не с `mistral-`, и в фильтр
не попадал. В комбо такая модель отдавала бы 400 на каждом запросе с
`reasoning_effort` — то есть выпадала бы из ротации целиком.

Проверено напрямую по `https://api.mistral.ai/v1/chat/completions`:

| Модель | без `reasoning_effort` | с `reasoning_effort` |
| --- | --- | --- |
| `open-mistral-nemo` | 200 | 400 (code 3051) |
| `codestral-latest` | 200 | 400 (code 3051) |

Правка: в `rejects_reasoning_effort` добавлена ветка `open-mistral`, плюс
тест `mistral_families_reject_reasoning_effort` (было пусто — функция вообще
не была покрыта). После деплоя `open-mistral-nemo` с `reasoning_effort: high`
отвечает 200.

### Failover (живая проверка)

| Состояние | Кто ответил | Время |
| --- | --- | --- |
| всё включено | `beam` | 1.0 с |
| `ollama-cloud` выключен | `codestral-latest` | 0.5 с |
| `ollama-cloud` + `mistral` выключены | `openai-fast` | 4.2 с |

Все подключения после проверки возвращены в исходное состояние: набор
неактивных соединений совпадает с бэкапом до правки (cerebras 1, devin-cli
1, mlx-gemma 1, mlx-qwen 1, nvidia 2, qoder 5, xiaomi-mimo 1,
xiaomi-mimo-token-plan 1).

### Стоимость agentrouter

По `call_logs`: 938 вызовов `gpt-5.6-sol` израсходовали **120 528 653**
входных токенов, 1 060 426 выходных. Всего за 27–29 октября — 122.9M входных
токенов. Это и есть причина, по которой agentrouter ушёл в конец комбо,
несмотря на лучшие бенчмарки.

## Что не трогали

- **Модели, у которых лимит восстановится.** `ollama-cloud` (месячный лимит),
  `mistral` (rate-limit), `zai` (нет баланса), `aihubmix` (trial-квота) вернутся
  в работу сами. Они не в комбо не потому, что плохи, а потому, что сегодня
  не отвечают; после восстановления лимита их стоит вернуть — кандидаты
  `ollama-cloud/nemotron-3-super` (SWE-bench 60.5) и
  `aihubmix/nemotron-3.5-super-vl-free` (SWE-bench Verified 71.1) по
  бенчмаркам сильнее `beam` на DeepSWE.
- **Geo-блоки** (OpenRouter, Gemini, Groq, x.ai, Kilo) — из РФ они не
  доступны в принципе, обход через exit-node Tailscale запрещён владельцем
  (трафик машины при этом падает).
- **`auto-free`, `auto-fast`, `main-1m`, `fallback-free`, `google`** — другие
  активные комбо не пересобирались; они содержат те же мёртвые записи и
  требуют такого же прохода.
- **MCode-конфиг** (`~/.minimax/config.yaml`) — записи `auto-subagent`,
  `auto-fast`, `auto-free` указывают на комбо по имени, состав комбо в
  конфиге менять не нужно.

## Как пересобрать

Через admin API (комбо переписывается целиком, порядок массива = порядок
обхода):

```sh
curl -X PATCH http://127.0.0.1:20128/api/combos/<combo-id> \
  -H "Authorization: Bearer $ROUTER_KEY" \
  -H "Content-Type: application/json" \
  -d '{"strategy":"priority","models":[
        {"provider":"ollama-cloud","model":"beam","weight":90},
        {"provider":"mistral","model":"codestral-latest","weight":80}]}'
```

Бэкапы: `~/backups/2026-11-05-crouton-alpha/storage-before-combo-rebuild.sqlite`
и `auto-subagent-before.json` (тот же каталог, `SHA256SUMS.txt`).

## Ограничения

- Порядок обхода задан списком, `weight` — метаданные для дашборда
  (Rust-роутер вес не читает, см. `combos.rs`).
- `pollinations/openai-fast` отдаёт 402 «1 reply every 45 seconds per IP» —
  в комбо из 12 параллельных субагентов этот слот будет занят почти всегда,
  так что фактически он работает как последний резерв.
- `agentrouter` при исчерпании баланса начнёт отдавать 401/402; в момент
  проверки он отвечал, но счёт идёт к нулю.

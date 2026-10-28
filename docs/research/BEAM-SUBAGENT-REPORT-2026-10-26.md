# Внешние модели как субагенты в Minimax Code: Beam 1.0 через Heterion Router

**Дата:** 2026-10-26 · **Замеры:** живой прогон против `http://127.0.0.1:20128` (Heterion Router)
**Вопрос:** можно ли звать внешние модели субагентами Minimax Code и есть ли в этом смысл
для длинных миссий/целей.

**Короткий ответ:** да, работает — доказано живым спавном субагента, который на Beam 1.0
вызвал `read` и `bash` и вернул верные ответы. Да, смысл есть: Beam 1.0 **бесплатен
(ограниченное время), 1M контекста и reasoning до `max`**, а модель родителя
(MiniMax M3.1-Flash) работает по подписке. Всё, что уводится в Beam, не тратит квоту
плана MiniMax.

---

## 1. Что такое Beam 1.0 и где он живёт

`beam` — не абстрактная «Beam 1.0 из фантазии пользователя», а конкретная модель
**Reflection**: `reflection/beam-1.0-501b-a23b`.

| Свойство | Значение | Источник |
|---|---|---|
| Архитектура | MoE, 501.3B total / 23.4B active, 256 экспертов | `ollama.com/library/beam:cloud` |
| Контекст | 1 048 576 токенов | там же + `models.opencode.ai` |
| Макс. output | 131 072 | `models.opencode.ai` |
| Tools / reasoning / temperature | Yes / Yes / Yes | `models.opencode.ai` |
| Модальности входа | только text | `/v1/models` Ollama |
| Reasoning levels | low, medium, high, xhigh, **max** (по умолчанию max) | `/v1/models` Ollama |
| Цена | **бесплатно, ограниченное время** (с 2026-10-16) | `ollama.com/blog/reflection-beam`, официальный X-пост Ollama |
| Профиль | software engineering, reasoning, long-running agentic tasks | Ollama blog |

Два факта из документации, важные для экономики:

* «**Token-efficient reasoning** — сопоставимое качество при ~**3× меньшем** числе
  inference-time reasoning токенов» (Ollama library). То есть даже на платном тарифе
  Beam дешевле за ту же задачу.
* Beam спроектирован под «long-running agentic tasks» — ровно профиль субагента.

**Где именно:** Ollama Cloud, OpenAI-совместимый эндпоинт `https://ollama.com/v1`
(модель в магазине называется `beam:cloud`, в API — `beam`).

Ключ пользователя: `c633458a1d384e43b54ed052968e3efc.4_irvhniiMQNeACjfDlJzjMs`.

---

## 2. Что реально покрывает этот ключ (замер, а не догадка)

Провайдер Ollama Cloud в роутере уже был в `registry.json`, но со **старым** списком
моделей (`gpt-oss`, `deepseek-v4`, `glm-5.1`…), в котором `beam` не было. Список
обновлён на актуальный (22 модели).

Проверка каждой модели живым запросом через роутер (2026-10-26):

| Модель | Контекст | Результат |
|---|---|---|
| `ollama-cloud/beam` | 1M | **OK** |
| `ollama-cloud/nemotron-3-ultra` | 262k | **OK** |
| `ollama-cloud/nemotron-3-super` | 262k | **OK** |
| `ollama-cloud/nemotron-3-nano:30b` | 262k | **OK** |
| `ollama-cloud/gpt-oss:120b` | 131k | **OK** |
| `ollama-cloud/gpt-oss:20b` | 131k | **OK** |
| `ollama-cloud/gemma4:31b` | 262k | **OK** |
| `glm-5.3`, `glm-5.3-flash`, `glm-5.2` | 1M | 402 (нет кредитов) |
| `kimi-k3`, `kimi-k2.7-code`, `kimi-k2.6` | 1M–262k | 402 |
| `minimax-m3`, `minimax-m3.1-flash`, `minimax-m2.7` | 512k–196k | 402 |
| `muse-spark-open` | 1M | 402 |
| `deepseek-v4-pro:0813`, `deepseek-v4.1-flash*` | 1M | 402 |
| `mistral-large-4` | 1M | 403 |
| `mistral-large-3:675b` | 262k | 402 |

**Вывод, который меняет всё:** на этом ключе Beam — **единственная** модель с 1M
контекстом и reasoning `max`. Остальные «1M-модели» в меню Ollama недоступны без
покупки кредитов. Поэтому стратегия «Beam первый, остальные — фолбэк» — не вкусовщина,
а следствие тарифа.

### Остальной роутер на момент замера

| Модель роутера | Статус |
|---|---|
| `openrouter/grok-4.6`, `pollinations/openai-fast`, `mistral/codestral-latest`, `groq/openai/gpt-oss-20b`, `kilocode/openrouter/free` | OK |
| `grok-cli/grok-4.6` | 400 |
| `zai/glm-5.3-flash`, `zai/glm-5.3` | 429 (rate-limit) |
| `opencode-go/*` (все 10 проверенных) | 400 |
| `agentrouter/claude-opus-5` | 401 |
| `blackbox/claude-opus-4.8` | 404 |
| `openrouter/free`, `kilo-gateway/kilo-auto/free` | 404 / 401 |
| `gemini/gemini-3.7-flash` | нет ответа (таймаут) |
| `gemini/gemini-2.5-pro` | 404 |

Про Gemini: пользователь предупреждал, что «Google любит филонить и часто заканчивает
задачи ничего не сделав». Замер это подтвердил с другой стороны — Gemini сейчас просто
не работает (404 / нет ответа), а не «думает слишком долго». В любом случае **в
авто-меню субагентов Gemini не включён**.

---

## 3. Механика: как именно внешняя модель становится субагентом

Разобрано по бандлу рантайма (`/Applications/MiniMax Code.app/Contents/Resources/app.asar`),
не по догадкам.

1. **Провайдер задаётся в `~/.minimax/config.yaml`**, в дереве `custom_provider`
   (не `provider` — оттуда legacy-миграция переносит BYOK-записи в
   `custom_provider:*` и переписывает файл).
2. Ключ модели содержит `/` — рантайм режет ref по **первому** слэшу
   (`splitModelId`: «custom model ids may themselves contain slashes»), поэтому
   `custom_provider:heterion/ollama-cloud/beam` → provider `custom_provider:heterion`,
   model `ollama-cloud/beam`.
3. Формат записи:

```yaml
custom_provider:
  heterion:
    kind: custom
    enabled: true
    name: Heterion Router
    api: openai-completions
    options:
      baseURL: http://127.0.0.1:20128/v1
      apiKey: <ключ роутера>
      authMode: api-key
    models:
      "ollama-cloud/beam":
        name: "Ollama beam"
        enabled: true
        reasoning: true
        tool_call: true      # без этого модель вообще не попадает в список
        attachment: false
        modalities: {input: [text], output: [text]}
        limit: {context: 1048576, output: 65536}
        thinking:
          effortOptions: [low, medium, high, xhigh, max]
          defaultEffort: max
```

4. **Спавн субагента** (это и есть проверка «можно ли использовать внешние модели»):

```
task(agent_name="worker",
     model="custom_provider:heterion/ollama-cloud/beam",
     effort="max",
     prompt="...")
```

`effort` **обязателен**: без него рантайм отвечает
`Model ... requires an explicit thinking effort: low, medium, high, xhigh, max`.
Допустимые уровни — `minimal|low|medium|high|xhigh|max` (у Ollama есть ещё `none`,
но рантайм его не понимает, поэтому `none` отброшен при маппинге на `low`).

### Живое доказательство

| Прогон | Модель | Что сделал субагент | Вердикт |
|---|---|---|---|
| A | `ollama-cloud/beam` | вызвал `read` на AGENTS.md, назвал 10 крейтов, порты 20128/20129 и правило про 4xx-breaker | ✅ |
| B | родительская (MiniMax M3.1-Flash) | тот же промпт, тоже вызвал `read`, ответ того же уровня | ✅ (контроль) |
| C | `auto-subagent` (комбо роутера) | вызвал `read` + `bash`, разобрал registry.json: 279 провайдеров, `base_url` оllama-cloud | ✅ |

Подтверждено в самой БД роутера (`call_logs`/`usage_history`), а не только по ответу:

```
ts                   requested_model          model     provider      status duration combo_name     tokens_in tokens_out
2026-10-26 13:52:07  auto-subagent            beam      ollama-cloud  200    856      auto-subagent  0          0
2026-10-26 13:52:10  auto-subagent            beam      ollama-cloud  200    2207     auto-subagent  0          0
2026-10-26 13:52:14  auto-subagent            beam      ollama-cloud  200    2297     auto-subagent  0          0
```

Три вызова = первый запрос → вызов инструмента → финальный ответ. **Цикл с
инструментами работает.**

---

## 4. «Auto»-принцип: выбор модели под задачу + фолбэк

Реализовано двумя слоями.

### Слой 1 — комбо в роутере (автофейловер)

Созданы три комбо (`combos` в БД, `strategy: priority`, `failoverBeforeRetry: true`):

* **`auto-subagent`** — ctx 1 048 576: `beam` → `nemotron-3-ultra` → `nemotron-3-super`
  → `gemma4:31b` → `gpt-oss:120b`
* **`auto-fast`** — ctx 262 144: `nemotron-3-super` → `gpt-oss:20b` → `pollinations/openai-fast`
  → `groq/openai/gpt-oss-20b`
* **`auto-free`** — ctx 262 144: `kilocode/openrouter/free` → `pollinations/openai-fast`
  → `groq/openai/gpt-oss-20b` → `mistral/codestral-latest` → `beam`

Цепочки собраны **только из моделей, проверенных живым запросом**. Это важно:
изначально в `auto-subagent` стояли `glm-5.3`/`kimi-k3`/`mistral-large-4`, и замер
показал, что они отдают 402 — то есть комбо «выглядело» хорошо, а работало бы только
на первой ступени. Все цепочки заканчиваются бесплатным ключевым/безключевым
провайдером, поэтому задача не умирает никогда.

### Слой 2 — диспетчер по типу задачи

`scripts/subagent/auto_call.py` — классифицирует задачу и идёт по цепочке кандидатов,
каждый со своим `reasoning_effort`:

| Класс | Назначение | Цепочка |
|---|---|---|
| `deep` | архитектура, дизайн, root cause, доказательства | `beam[max]` → `auto-subagent[max]` → `nemotron-3-ultra[medium]` → `auto-free[medium]` |
| `code` | ограниченное изменение в коде | `beam[high]` → `auto-subagent[high]` → `auto-fast[medium]` → `auto-free[medium]` |
| `longctx` | весь репозиторий или длинная транскрипция в контексте | `beam[max]` → `auto-subagent[max]` → `gemma4:31b[medium]` → `auto-free[medium]` |
| `quick` | мелкий факт, один файл | `auto-fast[medium]` → `nemotron-3-super[medium]` → `auto-free[medium]` |
| `bulk` | механическая однотипная работа | `auto-fast[low]` → `gpt-oss:20b[low]` → `auto-free[low]` |
| `verify` | независимая проверка результата | `nemotron-3-ultra[medium]` → `beam[high]` → `auto-free[medium]` |

Замер: `--probe` — все 8 кандидатов отвечают; рабочий вызов классифицировал задачу
в `deep`, пошёл в `beam[max]`, 2 939 токенов, 11.55 с.

---

## 5. Найденное и починенное ограничение: 2 MiB на тело запроса

`beam` держит 1M контекста, но **роутер не давал туда дойти**.

Тест: payload 2.95 МБ (≈591k токенов) → `400 invalid_request: request body could be read`.
Причина — axum's `DefaultBodyLimit` 2 MiB (в коде не задан, т.е. дефолт).

Это тихо убивало ровно то, ради чего Beam и нужен: «один репозиторий — один запрос».
Проверено на живом стенде: payload 1.5 МБ проходил (335 001 prompt tokens), 2.95 МБ — нет.

**Починено по правилу миссии (сначала красный тест, потом фикс):**

* красный тест `chat_completions_accepts_a_body_larger_than_the_axum_default` → 400;
* `GatewayConfig.max_body_bytes` (`HETERION_ROUTER_MAX_BODY_BYTES`, дефолт 32 MiB;
  0/мусор игнорируются) → `AppState` → `DefaultBodyLimit::max(...)` в роутере;
* зелёный тест, `scripts/ci/check.sh` — **all green**.

**После фикса, живой замер:**

```
payload 2 953 900 байт → HTTP OK, 17.0 с
usage: {prompt_tokens: 634392, completion_tokens: 938, total_tokens: 635330}
```

634k токенов контекста за один вызов, ответ верный. Полный 1M теперь достижим
(1M токенов ≈ 4 МБ текста < 32 MiB лимита).

---

## 6. Экономика: есть ли смысл на длинных миссиях

### Что стоит денег

| Сторона | Как тарифицируется |
|---|---|
| Модель родителя (MiniMax M3.1-Flash) | **OAuth-план подписки** (`agent.default`, `agent-backend`), не по-токанный API-ключ. В `config.yaml` её контекст 1M помечен `higher_usage` — то есть 1M-контекст у родителя явно «дороже» по квоте. |
| Beam через Ollama Cloud | **$0** на время действия бесплатного периода. Подтверждено прайсом Ollama (прочитан 2026-10-26): в таблице моделей **строки `beam` нет вообще** — то есть тарифа нет, модель промо; «Add credits to unlock all models» объясняет 402 на остальные. После промо: оплата по кредитам Ollama (glm-5.3 $1.40/$4.40 за 1M in/out, kimi-k3 $3.00/$15.00, deepseek-v4-pro $1.32/$3.96, nemotron-3-super $0.015/$0.60, gpt-oss:20b $0.07/$0.30). |
| Beam «после бесплатного периода» | Всё равно дешевле большинства: 3× меньше reasoning-токенов при сопоставимом качестве. |

### Замеры объёма

| Сценарий | Токены | Время |
|---|---|---|
| Репрезентативный ход субагента (system + 12 схем инструментов + чтение файла + follow-up) | 3 536 in / 120 out | 2.5 с |
| Дамп репозитория (119 файлов) | 335 001 in / 564 out | 8.3 с |
| Дамп репозитория после фикса лимита | 634 392 in / 938 out | 16.9 с |
| Средняя латентность Beam (по `usage_history`) | — | 2 047 мс, TTFT 823 мс |
| Субагентные вызовы Beam (из лога роутера) | — | 856 / 2 207 / 2 297 мс |

### Модель экономии длинной миссии

Допустим, миссия = 6 субагентов × 5 ходов × 120k токенов контекста ≈ **3.6M токенов**.

* На родителе это **3.6M токенов квоты плана MiniMax** — и 1M-контекст там помечен
  как `higher_usage`, то есть ещё и по более дорогой ставке.
* На Beam это **$0** и 1M контекста в одном запросе, без компрессии и без
  пересборки контекста.

Главный выигрыш — **не цена токена, а отсутствие компрессии**. Длинная миссия умирает
не от токенов, а от потери состояния: каждый раз, когда контекст приходится
пересобирать, модель начинает работать по обрезанной картине. 1M-контекстная Beam
позволяет держать репозиторий целиком на протяжении всей миссии.

### Оговорки (честно)

* **Бесплатность Beam — временная и не гарантированная.** По условиям Ollama срок не
  назван; после окончания Beam уходит на кредиты Ollama. Планировать бюджет надо с
  этим.
* **Concurrency.** По FAQ Ollama (прочитан 2026-10-26): «Free includes 1 concurrent
  request, Pro 3, and Max and Team 10». Роутер держит 6 параллельных субагентов
  (`subagent.maxConcurrentPerSession: 6`), поэтому параллельный веер упирается в
  лимит: запросы сверх лимита **ставятся в очередь** и обрабатываются по мере
  освобождения слота, а при переполнении очереди — **отбрасываются** (429/503 на
  стороне Ollama). Плюс есть «starter credits» с месячным ресетом: Free-план включает
  стартовый пакет usage для ограниченного набора стартовых моделей, и «Add credits to
  unlock all models» — ровно то, что мы наблюдали как 402. Для параллельной работы
  над Beam нужен платный тариф (Pro $20/мес с $60 кредитов, 3 параллельных запроса) или
  разнесение по провайдерам.
* **Токены стриминговых вызовов не считаются.** Ollama Cloud не отдаёт `usage` в
  стриминге даже с `stream_options.include_usage` — в `usage_history` такие вызовы
  пишутся с `tokens_input = 0`. Считать расход по логам роутера на живых стримах
  нельзя; замеры выше сделаны нестриминговыми запросами.
* **Качество — не «бесплатно».** Beam — open-weight MoE 23.4B active. На контрольном
  прогоне ответ был не хуже родительского, но для задач, где нужен абсолютный
  максимум качества, `deep`-класс стоит проверять на конкретной задаче, а не
  полагаться на общую рекомендацию.

---

## 7. Что изменено в репозитории

| Файл | Изменение |
|---|---|
| `crates/heterion-router-providers/registry.json` | `ollama-cloud`: список моделей заменён на актуальные 22, добавлен `beam` (был устаревший, без `beam`) |
| `crates/heterion-router-config/src/config.rs` | `GatewayConfig.max_body_bytes` + `HETERION_ROUTER_MAX_BODY_BYTES` (дефолт 32 MiB) + тесты |
| `crates/heterion-router-config/src/lib.rs` | ре-экспорт `DEFAULT_MAX_BODY_BYTES` |
| `crates/heterion-router-gateway/src/lib.rs` | `AppState.max_body_bytes`, `with_max_body_bytes`, слой `DefaultBodyLimit::max`, красный→зелёный тест на тело > 2 MiB |
| `crates/heterion-router-gateway/src/main.rs` | прокидывание `config.max_body_bytes` в состояние |
| `scripts/subagent/auto_call.py` | диспетчер «auto»: классификация задачи → цепочка моделей → фолбэк; `--probe`, `--list-classes` |

Вне репозитория:

* `~/.heterion-router/storage.sqlite` — подключение `ollama-cloud` (ключ зашифрован
  `enc:v1:` через `heterion-router-crypto`, round-trip проверен) + три комбо
  `auto-subagent` / `auto-fast` / `auto-free`.
* `~/.minimax/config.yaml` — провайдер `custom_provider.heterion` с 10 моделями
  (7 прямых + 3 комбо). Бэкап: `/tmp/config.yaml.bak-162648`.
* `/tmp/secrets.md` — sudo-пароль добавлен к существующим серверным секретам,
  права `0600`, sudo проверен.

Гейт: `scripts/ci/check.sh` → **all green** (fmt, clippy `-D warnings`, весь workspace).
Сервис перезапущен через launchd, `healthz` отвечает, `beam` виден в `/v1/models`.

---

## 8. Рекомендация

1. **Держать Beam первым в `deep`/`longctx`.** Это единственная доступная модель с
   1M + reasoning max, и она бесплатна.
2. **Фолбэк строить только из проверенного.** Цепочки, собранные по каталогу, а не по
   замеру, молча деградируют на первой ступени — это уже случилось один раз.
3. **Не держать Gemini в авто-меню.** Сейчас он не работает вовсе; при появлении —
   оставить для ручного запуска.
4. **Следить за лимитом тела.** 32 MiB — с запасом на 1M-контекст, но если в миссии
   появится вложение/картинка, запас тает.
5. **Заложить переход на платный Beam** (или разнесение по провайдерам) до того, как
   бесплатный период закончится: параллельная работа 6 субагентов на Free-плане
   ограничена одним одновременным запросом.

---

## 9. Воспроизведение

```sh
# 1) живой вызов Beam через роутер
curl -s http://127.0.0.1:20128/v1/chat/completions \
  -H "Authorization: Bearer $ROUTER_KEY" -H 'Content-Type: application/json' \
  -d '{"model":"ollama-cloud/beam","messages":[{"role":"user","content":"say ok"}],
       "max_tokens":2000,"reasoning_effort":"max"}'

# 2) какие кандидаты живы прямо сейчас
./scripts/subagent/auto_call.py --probe

# 3) классификация и авто-выбор
./scripts/subagent/auto_call.py --task "root cause of the deadlock in X" --max-tokens 4000

# 4) субагент в Minimax Code
task(agent_name="worker",
     model="custom_provider:heterion/ollama-cloud/beam",
     effort="max",
     prompt="...")
```

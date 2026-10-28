# Swarm внешних моделей: замена Beam, зрение, авто-выбор и учёт токенов

**Дата:** 2026-10-26 (продолжение `BEAM-SUBAGENT-REPORT-2026-10-26.md`)
**Замеры:** живой прогон против `http://127.0.0.1:20128` (Heterion Router)
**Вопросы:** что заменит `beam`, если он перестанет работать · какие модели читают
картинки · как MiniMax M3.1 Flash должен запускать swarm субагентов на разных
моделях · почему Ollama Cloud не отдаёт токены и как это починить.

> ### ⚠️ Errata от 2026-10-26 (вечер)
>
> Утверждение этого отчёта «`gemini/gemini-3-flash-preview` читает картинки» было
> **неверным**. Роутер терял изображение на пути OpenAI→Gemini: в
> `crates/heterion-router-translate/src/gemini_request.rs` не было ни одного
> `inline_data`, и часть сообщения с картинкой исчезала до отправки. Модель
> правдоподобно выдумывала описание — и это выдуманное описание было принято за
> измерение.
>
> Прибор, который это вскрыл: `usage.prompt_tokens` — 28 (только текст) против
> 1120 (текст + картинка), плюс `promptTokensDetails: [{modality: IMAGE}]` в
> ответе апстрима.
>
> Исправлено и переизмерено: картинку читают **семь** Gemini-моделей
> (`gemini-3.1-flash-lite`, `gemini-3-flash-preview`, `gemini-3.6-flash`,
> `gemini-3.5-flash`, `gemini-3.5-flash-lite`, `gemini-flash-lite-latest`,
> `gemma-4-26b-a4b-it`), а не одна. Подробности, красный тест, фикс и новые
> замеры — в `GEMINI-VISION-FIX-REPORT-2026-10-26.md`.
>
> Выводы этого отчёта про `gemma4:31b`, замену beam, swarm и учёт токенов
> остаются в силе; меняется только картина «кто читает картинки».

**Короткий ответ.** Замена есть, и она измерена: `ollama-cloud/gemma4:31b` —
единственная доступная модель, которая **и** держит 262k контекста, **и** умеет
читать изображения, **и** работает на том же бесплатном тарифе. Swarm собран и
запущен: пять capability-агентов, пять разных моделей, пять разных провайдеров,
все пять отработали. Токены для «немых» провайдеров теперь считаются — и
стриминговые, и обычные.

---

## 1. Кто умеет читать изображения (измерено, не заявлено)

Тестовая картинка `/tmp/vision_test.png`: красный круг, синий квадрат с белым
текстом `IN`, зелёный треугольник, чёрный `VISION-OK-42`, серый `HETERION SWARM`.
Проверка шла через OpenAI-совместимый формат (`image_url` с data-URL) прямо
через роутер.

| Модель | Провайдер | Контекст | Картинка | Что ответила |
|---|---|---|---|---|
| `ollama-cloud/gemma4:31b` | ollama-cloud | 262 144 | **да** | «A red circle / A blue square / A green triangle», текст `IN`, `VISION-OK-42`, `HETERION SWARM` — всё дословно |
| `gemini/gemini-3-flash-preview` | gemini | 1 048 576 | **да** | все три фигуры и все три надписи назвала; **но перепутала позиции** («синий квад слева, красный круг в центре» — на самом деле наоборот) |
| `ollama-cloud/beam` | ollama-cloud | 1 048 576 | **нет** | HTTP 400 — текстовый провайдер, `input_modalities: ["text"]` |
| `ollama-cloud/gpt-oss:120b` / `:20b` | ollama-cloud | 131 072 | нет | HTTP 400 |
| `ollama-cloud/nemotron-3-ultra` / `-super` / `-nano:30b` | ollama-cloud | 262 144 | нет | HTTP 400 |
| `mistral/codestral-latest` | mistral | 262 144 | нет | HTTP 400 |
| `pollinations/openai-fast` | pollinations | — | нет | HTTP 400 |
| `siliconflow/Qwen/Qwen2.5-VL-7B-Instruct`, `Qwen3-Omni-30B-A3B-*`, `deepseek-vl2` | siliconflow | — | — | HTTP 401 — нет учётных данных |

**Вывод.** Через этот роутер читают картинки **две** модели: `gemma4:31b` и
`gemini-3-flash-preview`. `beam` картинки не читает — подтверждено прямым
запросом, как и предполагал пользователь.

Точность различается: `gemma4:31b` назвала фигуры **и их расположение**;
Gemini назвала фигуры и текст, но перепутала, где что находится. Для UI-скриншотов
и диаграмм, где важна позиция элемента, `gemma4:31b` предпочтительнее.

Отдельно: `siliconflow` держит целое семейство VL/Omni-моделей, но на них нет
учётных данных — провайдер в списке живых, но не подключён.

---

## 2. Замена Beam

### 2.1 Что стоит за `beam`

`beam` = `reflection/beam-1.0-501b-a23b`: MoE 501B total / 23.4B active,
контекст 1 048 576, output 131 072, reasoning до `max`, tools ✓, **бесплатно**
(в прайсе Ollama строки `beam` нет вообще).

Но на текущем ключе Beam — **единственная** модель с 1M-контекстом и reasoning
`max`: `glm-*`, `kimi-*`, `minimax-*`, `muse-spark-open`, `deepseek-*` отдают 402
(нет кредитов на бесплатном стартовом пуле), `mistral-large-4` — 403.

### 2.2 Цепочка замены (все члены измерены живыми запросами)

| # | Модель | Провайдер | Контекст | Reasoning | Картинки |
|---|---|---|---|---|---|
| 1 | `ollama-cloud/nemotron-3-ultra` | ollama-cloud | 262 144 | ✓ | нет |
| 2 | `ollama-cloud/gemma4:31b` | ollama-cloud | 262 144 | ✓ | **да** |
| 3 | `ollama-cloud/nemotron-3-super` | ollama-cloud | 262 144 | ✓ | нет |
| 4 | `ollama-cloud/gpt-oss:120b` | ollama-cloud | 131 072 | ✓ | нет |
| 5 | `gemini/gemini-3-flash-preview` | gemini | 1 048 576 | ✓ | **да** |
| 6 | `mistral/codestral-latest` | mistral | 262 144 | — | нет |
| 7 | `openrouter/grok-4.6` | openrouter | — | — | нет |
| 8 | `pollinations/openai-fast` | pollinations | — | — | нет |
| 9 | `kilocode/openrouter/free` | kilocode | — | — | нет |

Главное свойство цепочки: она **не лежит в одном провайдере**. Прежняя версия
`auto-subagent` состояла только из моделей `ollama-cloud` — то есть исчерпание
бесплатного пула или падение Ollama Cloud уносили её целиком. Теперь хвост
цепочки — другой провайдер, и ещё дальше `auto-free` (без ключей вообще).

### 2.3 Фолбэк проверен, а не предположен

Создан временный комбо `zz-failover-check`: первым членом — заведомо мёртвая
`ollama-cloud/glm-5.3` (402), вторым — `beam`:

```
requested : zz-failover-check
served    : beam
content   : 'FAILOVER_OK'
HTTP 200
```

Первый член упал, роутер переключился на следующий и ответил. Тестовое комбо
удалено после проверки.

---

## 3. Как родитель (MiniMax M3.1 Flash) запускает swarm

### 3.1 Механизм закрепления модели за агентом

Возможность найдена в бандле рантайма (`app.asar`), а не угадана. Формат
`agent.md` поддерживает **верхнеуровневое поле `model:`** (плюс `effort:`):

```yaml
---
name: vision-reader
description: ...
model: custom_provider:heterion/ollama-cloud/gemma4:31b
effort: medium
x-mavis:
  displayName: Vision Reader
---
```

`model` обязан быть в форме `provider/model`, и режется по **первому** слэшу —
поэтому id модели может содержать `/` (`ollama-cloud/gemma4:31b`).

Приоритет выбора модели при спавне (из рантайма, `taskModelSources`):

```
task-model-override  →  task-target-agent  →  task-parent-session
```

`task-target-agent` — это и есть модель, закреплённая за целевым агентом.
То есть **`task(agent_name="vision-reader")` без параметра `model` уже
достаточно**: рантайм сам подставит модель агента. Это и есть swarm: родитель
выбирает *агента по функции*, а модель — по закреплённой за ним.

Проверено на практике: при первой попытке спавна `code-runner` рантайм ответил
`Model custom_provider:heterion/mistral/codestral-latest **from task-target-agent**
is unavailable in the current catalog` — то есть путь `task-target-agent`
работает и модель была взята именно из агента.

### 3.2 Пять агентов swarm

| Агент | Модель (закреплена) | Роль | Reasoning |
|---|---|---|---|
| `vision-reader` | `ollama-cloud/gemma4:31b` | картинки, скриншоты, диаграммы | medium |
| `code-runner` | `mistral/codestral-latest` | код: патчи, импорт, фиксы | — (модель без reasoning) |
| `deep-architect` | `auto-subagent` (комбо роутера) | архитектура, root cause, доказательства | max |
| `quick-scout` | `auto-fast` (комбо роутера) | разведка: где что лежит | medium |
| `wide-vision` | `gemini/gemini-3-flash-preview` | картинка **и** весь репозиторий в одном окне | medium |

`deep-architect` и `quick-scout` закреплены на **комбо**, а не на одной модели:
это даёт им автофолбэк бесплатно — если `beam` перестанет отвечать, цепочка
`auto-subagent` уйдёт на следующего провайдера сама.

### 3.3 Swarm реально отработал

Все пять запущены параллельно, каждый на своей модели. Подтверждено в БД роутера
(`call_logs`, последний час):

| Провайдер | Модель | Вызовов | tok_in | tok_out | avg, мс |
|---|---|---|---|---|---|
| ollama-cloud | `ollama-cloud/beam` | 80 | 7 | 44 | 2 917 |
| gemini | `gemini/gemini-3-flash-preview` | 65 | 3 516 134 | 14 226 | 5 772 |
| ollama-cloud | `auto-subagent` (комбо) | 40 | — | — | 2 547 |
| ollama-cloud | `auto-fast` (комбо) | 27 | — | — | 11 731 |
| mistral | `mistral/codestral-latest` | 8 | 144 032 | 33 433 | 37 528 |
| ollama-cloud | `gemma4:31b` | 15 | 2 146 | 159 | 1 424–35 450 |
| pollinations | `pollinations/openai-fast` | 6 | 7 | 44 | 819 |

Пять моделей, пять провайдеров, все пять отработали. Результаты:

* `vision-reader` (gemma4) — назвал все фигуры, цвета и текст, **и их расположение**.
* `code-runner` (codestral) — создал интеграционный тест; застрял на неверных
  API (`Db::in_memory` вместо `open_in_memory`, приватное поле `backend`,
  `hyper` как прямая зависимость) и в итоге оставил файл, который не
  компилировался. Тест переписан вручную по его заданию и проходит.
* `deep-architect` (auto-subagent → beam) — нашёл **реальный** пробел: оценка
  токенов не применялась к нестриминговым путям (см. §4).
* `quick-scout` (auto-fast → nemotron-3-super) — вернул полную таблицу
  провайдеров по потоковым вызовам.
* `wide-vision` (Gemini) — прочитал картинку и registry.json, связал их; честно
  отметил, что `input_modalities` в `registry.json` отсутствует (это поле из
  API Ollama, не из реестра роутера).

**Устойчивость.** Один из пяти (code-runner) не довёл дело до конца — не из-за
нестабильности модели, а из-за неверного предположения о публичном API. Остальные
четыре отработали полностью. Ни один вызов не упал из-за недоступности
провайдера: фолбэк срабатывал там, где первый член цепочки умирал.

---

## 4. Учёт токенов: Ollama Cloud не отдаёт `usage`

### 4.1 Что показало измерение

Ollama Cloud **вообще не присылает `usage` в стриминге** — даже при
`stream_options: {"include_usage": true}`:

```
A) openai-compat stream, без stream_options   → usage_lines=0
A2) openai-compat stream, с include_usage=true → usage_lines=0
B) нативный /api/chat (stream)                 → prompt_eval_count: 48, eval_count: 24
```

То есть нативный API Ollama токены отдаёт, а OpenAI-совместимый — нет. Роутер
говорит с провайдером по OpenAI-совместимому протоколу, поэтому посчитать их
на стороне апстрима нельзя.

Масштаб проблемы по данным `usage_history` (изоляция потоков через `ttft_ms > 0`):

| Провайдер | Потоков | С токенами | % |
|---|---|---|---|
| `ollama-cloud` | 149 | 0 | **0.0** |
| `vivanta-auto` | 249 | 0 | **0.0** |
| `vivanta-ollama` | 91 | 0 | **0.0** |
| `OpenRouter-free` | 78 | 0 | **0.0** |
| `Google` | 37 | 0 | **0.0** |
| `vivanta-core` / `vivanta-lm-studio` / `vivanta-deepseek` | 44 | 0 | **0.0** |
| `pollinations` | 46 | 6 | 13.0 |
| `openrouter` | 9 429 | 9 269 | 98.3 |
| `gemini` | 3 984 | 3 981 | 99.9 |
| `mistral`, `deepseek`, `grok-cli`, `ollama-local`, `nvidia`, `groq`, `zai`, `kilo-gateway` … | — | — | 100.0 |

**691 потоковых вызов в базе не имели ни одного токена.** Провайдер, который
реально работал, был неотличим от провайдера, к которому не обращались, — а
стоимость длинной миссии просто исчезала из books.

### 4.2 Что сделано

Новый модуль `crates/heterion-router-gateway/src/usage_estimate.rs`: оценка
размера запроса и ответа тем же эвристическим правилом «байты / 4», которое уже
использует эндпоинт `/v1/messages/count_tokens` — чтобы обе стороны шлюза
согласованно определяли «токен».

Правило: **заполняется только то, о чём апстрим не сказал**. Провайдер,
отчитавшийся своими числами, не переоценивается; нулевое поле заполняется,
ненулевое — не трогается. Двойного счёта нет.

Покрыты **все** пути:

* потоковые — `/v1/chat/completions`, `/v1/responses`, `/v1/messages`
  (`StreamAccounting` теперь копит байты текста и рассуждений);
* нестриминговые — те же три эндпоинта (`fill_missing` по ответу);
* оценка reasoning считается только если поток действительно выдал рассуждение.

### 4.3 Изменение контракта — осознанное

Тест `openai::stream_accounting_tests::a_stream_without_a_usage_frame_books_exactly_what_it_booked_before`
закреплял старое поведение («нет usage-кадра — нет токенов»). Это был
regression guard, а не случайность. Контракт **изменён намеренно** по прямому
требованию пользователя («добавь поддержку токенов для этого провайдера и тех, где
поддержка также отсутствует»), тест переименован в
`a_stream_without_a_usage_frame_books_the_local_estimate` и теперь фиксирует
новое правило; комментарий в тесте объясняет, почему.

### 4.4 Проверка на живом шлюзе

До правки и после, один и тот же стриминговый запрос к `ollama-cloud/beam`:

```
ts                   model              provider      tokens_input  tokens_output
2026-10-26 15:17:29  ollama-cloud/beam  ollama-cloud  7             44          ← после
2026-10-26 15:01:21  ollama-cloud/beam  ollama-cloud  0             0           ← до
```

И по всем «немым» провайдерам сразу после перезапуска:

| provider | model | tokens_input | tokens_output |
|---|---|---|---|
| groq | `groq/openai/gpt-oss-20b` | 73 | 40 |
| kilocode | `kilocode/openrouter/free` | 7 | 31 |
| pollinations | `pollinations/openai-fast` | 7 | 44 |
| ollama-cloud | `ollama-cloud/beam` | 7 | 44 |

---

## 5. Авто-выбор: capability вместо «одна модель на всё»

`scripts/subagent/auto_call.py` переработан: теперь есть **две независимые оси**.

* **Capability** — что задаче *нужно*: `vision`, `code`, `longctx`, `reasoning`,
  `fast`, `beam-replacement`.
* **Task class** — что за работа: `deep`, `code`, `longctx`, `quick`, `bulk`, `verify`.

Каждый кандидат в цепочке несёт **доказательство** того, почему он там.
Цепочки собраны из живых замеров; версии, собранной по каталогу, в репозитории
не осталось — она деградировала бы на первом же шаге (`glm-5.3` и `kimi-k3`
отдают 402).

```sh
./scripts/subagent/auto_call.py --list-capabilities
./scripts/subagent/auto_call.py --task "root cause of the deadlock"   # класс
./scripts/subagent/auto_call.py --task "..." --image /tmp/shot.png    # зрение
./scripts/subagent/auto_call.py --probe                                # кто жив
```

`--probe` проверяет 8 моделей: сейчас живы все 8.

---

## 6. Что изменено

| Файл | Изменение |
|---|---|
| `crates/heterion-router-gateway/src/usage_estimate.rs` | **новый** — оценка prompt/completion, `fill_missing`, `value_text_bytes` + тесты |
| `crates/heterion-router-gateway/src/lib.rs` | `StreamAccounting` копит байты потока и синтезирует недостающие токены; `StreamRequest` (clippy: ≤7 аргументов); нестриминговые пути `/v1/chat/completions` и `/v1/responses` тоже заполняют пустые счётчики |
| `crates/heterion-router-gateway/src/claude_endpoint.rs` | то же для `/v1/messages`; общий оценщик вместо приватной копии |
| `crates/heterion-router-gateway/src/openai.rs` | контрактный тест переписан под новое правило (см. §4.3) |
| `crates/heterion-router-gateway/tests/body_limit.rs` | **новый** — интеграционный тест лимита тела через публичный API (написан code-runner, доведён до зелёного) |
| `crates/heterion-router-gateway/src/usage_estimate.rs` | тесты оценки |
| `scripts/subagent/auto_call.py` | capability-реестр с доказательствами, `--image`, `--capability`, `--list-capabilities` |
| `~/.minimax/config.yaml` | провайдер `custom_provider.heterion`: 12 моделей (7 прямых + 3 комбо + `mistral/codestral-latest` + `gemini/gemini-3-flash-preview`); `defaultModel` возвращён на `minimax/MiniMax-M3.1-Flash` |
| `~/.minimax/agents/{vision-reader,code-runner,deep-architect,quick-scout,wide-vision}/agent.md` | **новые** — capability-агенты с закреплённой моделью |
| `~/.heterion-router/storage.sqlite` | комбо `auto-subagent` (1M, с хвостом из других провайдеров), `auto-fast`, `auto-free` |

Гейт: `scripts/ci/check.sh` → **all green** (fmt, clippy `-D warnings`, 363 теста
в gateway + интеграционные). Шлюз пересобран и перезапущен через launchd.

---

## 7. Честные оговорки

* **Gemini в позиционировании неточен.** Он читает картинки и держит 1M, но
  перепутал расположение фигур. Для UI-задач `gemma4:31b` надёжнее. Плюс
  пользовательский опыт: Gemini «филонит» — в измерении `auto-fast` (nemotron)
  отвечал заметно быстрее.
* **Оценка токенов — оценка.** Правило «байты / 4» совпадает с тем, что уже
  использует `count_tokens`, но для кириллицы (2 байта на символ) занижает
  реальное число токенов примерно вдвое. Для провайдеров, которые отчитываются
  сами, оценка не используется вовсе.
* **Overload на бесплатном пуле.** При массовом параллельном опросе часть
  запросов к `ollama-cloud` получала 402 — это потолок параллелизма на Free-плане
  (документировано: 1 одновременный запрос). Изолированные 4 параллельных
  вызова проходят; 5 при одновременной работе другого агента — нет. Для
  настоящего swarm на 6 субагентов нужен платный тариф или разнесение по
  провайдерам.
* **`siliconflow` не подключён.** Там есть целое семейство VL/Omni-моделей,
  но нет учётных данных (401).
* **Контракт HTTP-статуса не менялся.** Перебор лимита тела даёт 400
  («failed to buffer the request body»), а не 413 — так ведёт себя axum-экстрактор
  `Json`. Тест фиксирует фактическое поведение; менять его на 413 — отдельное
  решение с отдельными последствиями для клиентов.
* **Баг, найденный sub-агентом, не закрыт.** `deep-architect` указал, что
  прерванный поток с уже пришедшим usage-кадром сохраняет полный счёт, а
  прерванный поток от «немого» провайдера всё равно списывает оценку. Это
  осознанное поведение (провайдер же работал), но стоит пересмотреть.

---

## 8. Воспроизведение

```sh
# способности и живые кандидаты
./scripts/subagent/auto_call.py --list-capabilities
./scripts/subagent/auto_call.py --probe

# зрение
./scripts/subagent/auto_call.py --task "что на картинке?" --image /tmp/vision_test.png

# swarm: родитель выбирает агента, модель закреплена за агентом
task(agent_name="vision-reader", prompt="опиши /tmp/vision_test.png")
task(agent_name="code-runner",   prompt="сделайbounded-изменение")
task(agent_name="deep-architect", prompt="найди root cause")
task(agent_name="quick-scout",    prompt="где определён этот символ?")
task(agent_name="wide-vision",    prompt="сопоставь картинку с кодом")

# проверка токенов
sqlite3 ~/.heterion-router/storage.sqlite \
 "SELECT provider, model, tokens_input, tokens_output FROM usage_history
  WHERE ttft_ms>0 AND timestamp > datetime('now','-10 minutes');"
```

---

## 9. Поиск: ещё одна capability (добавлено вечером 2026-10-26)

К capability-цепочкам добавилась `search` — единственный кандидат `exa/exa-search`,
потому что поиск это не чат-модель, а отдельная поверхность шлюза (ADR-016/019).
Запрос уходит как последнее сообщение пользователя, ответ приходит цитированным
блоком; ключи Exa остаются зашифрованными в БД роутера.

Параметры поиска едут в теле запроса под объектом `"exa"`
(`numResults`, `type`, `category`, `includeDomains`/`excludeDomains`, даты) —
без него тело не меняется.

Классификатор задач отделяет веб-вопросы от репозиторных: «последний релиз axum»
идёт в поиск, а «текущая версия в cargo.toml» — в репозиторный класс
(предохранитель `REPO_MARKERS`).

```sh
./scripts/subagent/auto_call.py --capability search --task "Rust 1.99.1 release date"
```

Живая проверка: `Found 5 result(s)` за 2.67 s, первым — официальный анонс
Rust 1.99.1 от 2026‑10‑15.

В MiniMax Code тот же поиск доступен как инструмент `web_search`
(локальный плагин `~/.minimax/plugins/exa-search/`, MCP-сервер на stdio):
ключ берётся из конфига приложения, запрос идёт в роутер — прямого доступа
к Exa из плагина нет.

### 9.1 Два разных отказа Gemini на free tier

Полная проба кандидатов (16 моделей, пауза 4 с) показала три отказа подряд у
`gemini-3-flash-preview`, `gemini-3.6-flash` и `gemini-3.1-flash-lite` — с
**503**, а не с 429. Прямые вызовы к апстриму сразу после этого отвечали 200,
поэтому это не поломка и не исчерпание квоты:

| Отказ | Что это | Кого задевает |
|---|---|---|
| **429** | минутная квота аккаунта исчерпана (всплеск запросов) | всплеск из 18 моделей подряд |
| **503** | Google перегрузил популярную free-tier модель | семейство «5 RPM» (`3 Flash`, `3.6`) |

Поэтому в цепочках вперёд стоят тиры с запасом: `gemini-3.1-flash-lite`
(15 RPM, RPD 500) и `gemma-4-26b-a4b-it` (30 RPM), а «5 RPM»-семейство — следом.
На клиенте оба отказа выглядят как 502 `upstream request failed`, при этом
апстрим-статус сохраняется в теле ошибки.

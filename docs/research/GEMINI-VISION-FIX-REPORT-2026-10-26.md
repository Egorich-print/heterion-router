# Gemini на free tier: полный каталог, починенное зрение, реальные квоты

**Дата:** 2026-10-26
**Повод:** пользователь показал дашборд «Gemini API Rate Limit» (free tier, проект
«Default Gemini Project») и указал, что моделей Gemini на самом деле много больше,
чем одна в цепочке.

**Главный вывод:** моделей действительно много (60 в апстриме, 46 с
`generateContent`), но настоящая находка оказалась не в списке, а в роутере:
**путь OpenAI→Gemini молча терял картинки**. Все прежние утверждения отчёта
«Gemini читает изображения» были галлюцинациями моделей, а не измерением.
После фикса картинку читают **семь** Gemini-моделей, а не одна.

---

## 1. Что на самом деле доступно

Каталог взят не из реестра роутера, а из апстрима: `GET
https://generativelanguage.googleapis.com/v1beta/models` с ключом одного из
подключений (ключ расшифрован тем же AES-256-GCM/scrypt, что и в
`heterion-router-crypto`; сам ключ нигде не печатался и не сохранялся).

**60 моделей, из них 46 с `generateContent`.** Реестр роутера до правки знал
8; в нём были и заведомо мёртвые для этого ключа id.

### 1.1 Живая текстовая проба

18 кандидатов, последовательно, через роутер (`POST /v1/chat/completions`),
2026-10-26. Замеры — не каталог, а ответы апстрима.

| Модель | HTTP | Время | Что ответил апстрим |
|---|---|---|---|
| `gemini/gemma-4-26b-a4b-it` | **200** | 2.4 s | ok, usage 6/61 |
| `gemini/gemini-3.5-flash-lite` | **200** | 28.2 s | ok, usage 6/1 |
| `gemini/gemini-3.1-flash-lite` | **200** | 1.5 s | ok, usage 6/1 |
| `gemini/gemini-3.1-flash-lite-preview` | **200** | 1.9 s | ok, usage 6/1 |
| `gemini/gemini-flash-lite-latest` | **200** | 24.9 s | ok, usage 6/1 |
| `gemini/gemini-3.5-flash` | **200** | 2.6 s | ok, usage 6/60 |
| `gemini/gemini-3.6-flash` | **200** | 2.2 s | ok, usage 6/58 |
| `gemini/gemini-3-flash-preview` | **200** | 1.4 s | ok, usage 6/51 |
| `gemini/gemma-4-31b-it` | 502 | 49 s | апстрим `500 INTERNAL` (прямой вызов: `Internal error encountered` за 29.7 s) |
| `gemini/antigravity-preview-latest` | 400 | 2.5 s | `This model only supports Interactions API` |
| `gemini/gemini-2.5-flash` | 404 | 1.2 s | `no longer available to new users … use models/gemini-3.8-flash` |
| `gemini/gemini-2.5-flash-lite` | 404 | 1.1 s | `no longer available to new users … use models/gemini-3.5-flash-lite` |
| `gemini/gemini-flash-latest` | 502 | 63.9 s | зависает |
| `gemini/gemini-3.7-flash` | 502 | 102.9 s | зависает |
| `gemini/gemini-3.8-flash` | таймаут | >120 s | зависает |
| `gemini/gemini-3.1-pro-preview` | 429 | 2.2 s | нет free tier (в дашборде 0/0) |
| `gemini/gemini-4-argon` | 429 | 1.8 s | нет free tier (0/0) |
| `gemini/gemini-omni-1.1-flash` | 429 | 1.4 s | нет free tier (0/0) |

Три «429» ровно совпали со строками `0 / 0` в дашборде пользователя: free tier
эти модели не покрывает. Два «404» — это «модель недоступна новым
пользователям», то есть на ключе с биллингом они бы работали; на free tier — нет.

### 1.2 Сопоставление с дашбордом квот

| Модель в дашборде | RPM | RPD | Состояние на ключе |
|---|---|---|---|
| Gemini 3 Flash | 5 | 20 | живая (пик 22/20 — это максимум за 90 дней, а не текущий остаток) |
| Gemini 3.6 Flash | 5 | 20 | живая |
| Gemini 3.8 Flash | 5 | 20 | зависает |
| Gemini 3.5 Flash Lite | 15 | **500** | живая |
| Gemini 3.1 Flash Lite | 15 | **500** | живая |
| Gemma 4 26B / 31B | 30 | **14 400** | 26B живая, 31B ловит апстрим-500 |
| Antigravity | 60 | 100 | не поддержан (Interactions API) |
| Gemini 3.1 Pro / 4 Argon / Omni 1.1 Flash | 0 | 0 | 429 |

Вывод по квотам: **надо опираться не на «3 Flash / 3.6 / 3.8», а на семейство
Flash Lite (RPD 500) и Gemma 4 (RPD 14 400)**. Это и есть тот запас, который
не исчерпывается.

---

## 2. Находка: роутер выбрасывал картинки на пути к Gemini

Пока искали «какая модель читает картинки», обнаружилось расхождение: все
Gemini-модели отвечали подозрительно одинаковым бойлерплейтом («Based on the
image provided, here is a description…») и при этом **никогда не называли
текст с картинки**. Один ответ был в чистую воду галлюцинацией: «This image is
a graphic design piece featuring a variety of geometric…».

### 2.1 Доказательства

1. **В коде нет `inline_data` нигде.** Поиск по `crates/` даёт ноль совпадений
   и по `inline_data`, и по `inlineData`.
2. Док-комментарий модуля прямо признавал дефект:
   `crates/heterion-router-translate/src/gemini_request.rs:9` —
   *«Explicitly deferred: … multimodal inline data beyond text»*.
3. Ветка пользовательского сообщения брала только текст:
   `content_text(message.get("content"))` → массив частей с `image_url`
   превращался в пустую строку, и **часть с картинкой исчезала до отправки**.
4. **Usage это подтверждал.** До фикса запрос с картинкой давал
   `prompt_tokens = 28` (только текст вопроса). После фикса — `1120`.
5. Прямая проверка формы, которую принимает API (вне роутера):
   * `inlineData` (camelCase) → **HTTP 200**, и в ответе
     `usageMetadata.promptTokensDetails: [{modality: IMAGE, tokenCount: 1092}]` —
     картинка действительно дошла;
   * `inline_data` (snake_case) → тоже 200 (API принимает оба);
   * `fileData.fileUri` с обычным https-URL → **HTTP 400**
     `Cannot fetch content from the provided URL`.

Пункт 5 — важный: Gemini **не умеет забирать** картинку по URL. Единственная
рабочая форма — байты внутри запроса.

### 2.2 Что это значит для прежнего отчёта

Утверждение «`gemini-3-flash-preview` читает картинки, но путает позиции» было
**неверным**. Роутер не мог передать изображение в Gemini вообще, а модель
правдоподобно выдумывала описание. Прежний вывод «картинки читают две модели»
правильным быть не мог: читала одна — `ollama-cloud/gemma4:31b`, у которой
OpenAI-совместимый путь не транслируется вовсе.

### 2.3 Фикс

Красный тест → красный → фикс → зелёный, как требует правило миссии.

`crates/heterion-router-translate/src/gemini_request.rs`:

* `content_parts()` — контент сообщения → части Gemini: текст остаётся
  текстом, картинка становится `inlineData`-частью, порядок сохраняется;
* `content_part()` — одна часть: `text`, `image_url` (и строковая, и объектная
  формы) и AI-SDK-форма `{"type":"image","image":"data:…"}`;
* `picture_part()` — картинка → `{"inlineData":{"mimeType":…,"data":…}}`;
* `data_url()` — разбор `data:<mime>;base64,<payload>` (та же форма, что
  матчит JS-адаптер `^data:([^;]+);base64,(.+)$`);
* удалённый URL, который Gemini забрать не может, **больше не исчезает
  молча**: вместо картинки уходит текстовая заметка с этим URL, чтобы модель не
  описывала картинку, которую не видела.

Тесты (три новых, сначала падали):

| Тест | До фикса | После |
|---|---|---|
| `maps_an_image_data_url_to_a_gemini_inline_data_part` | FAILED | ok |
| `maps_the_ai_sdk_image_form_to_a_gemini_inline_data_part` | FAILED | ok |
| `a_remote_image_url_is_reported_instead_of_being_dropped_silently` | FAILED | ok |

Весь крейт: 39 тестов, зелёные. `scripts/ci/check.sh` — `all green`.

---

## 3. Зрение после фикса

Та же тестовая картинка (`/tmp/vision_test.png`: красный круг, синий квадрат с
белым `IN`, зелёный треугольник, `VISION-OK-42`, `HETERION SWARM`), тот же
вопрос, что и раньше.

| Модель | prompt-токены | Время | Что ответила |
|---|---|---|---|
| `ollama-cloud/gemma4:31b` | 320 | 2.1 s | **эталон**: «Red circle: Left / Blue square: Center / Green triangle: Right», текст дословно |
| `gemini/gemini-3.1-flash-lite` | 1120 | **2.6 s** | три фигуры, позиции верны, все три надписи процитированы |
| `gemini/gemini-3-flash-preview` | 1120 | 4.9 s | то же |
| `gemini/gemini-3.6-flash` | 1120 | 4.8 s | то же |
| `gemini/gemini-3.5-flash` | 1120 | 4.9 s | то же |
| `gemini/gemini-3.5-flash-lite` | 1120 | 23.5 s | то же |
| `gemini/gemini-flash-lite-latest` | 1120 | 24.8 s | то же |
| `gemini/gemma-4-26b-a4b-it` | 286 | 101 s | видит (три фигуры в ряд), но очень медленно |
| `gemini/gemma-4-31b-it` | — | 82 s | 502, апстрим `INTERNAL` |

Разница `prompt_tokens` — это и есть прибор: 28 (текст) против 1120 (текст +
картинка) и `IMAGE`-модальность в `usageMetadata` апстрима.

**Практический выбор по квотам:** `gemini-3.1-flash-lite` — 2.6 s, 1M контекста,
RPD 500. Это лучший «читающий картинки» кандидат на бесплатном тарифе;
`gemma4:31b` остаётся самым точным и быстрым, но живёт на пуле Ollama Cloud
(1 одновременный запрос).

---

## 4. Квоты: почему «всё сломалось» на минуту

В пуле роутера **пять разных Google-аккаунтов**:

```
nikulinatatiana420 · egork2503 · korosteleved2503 · egorichkrypto78 · printedbyek
```

У каждого свой проект и своя free tier-квота. Дашборд, который прислал
пользователь, — один из них; у остальных лимиты такие же.

Во время первой пробы 18 моделей подряд **все** Gemini-модели ответили 502
`upstream request failed`, хотя за минуту до этого отвечали. Разбор:

* каждый аккаунт даёт 5 RPM (Flash) или 15 RPM (Flash Lite);
* всплеск из ~20 запросов выбрал минутный бюджет всех пяти аккаунтов;
* апстрим отвечает 429, роутер отдаёт клиенту 502;
* отдельный признак: тривиальный `say OK` от `gemini-3.1-flash-lite` занял
  **31.4 s** — это внутренние ретраи по ключам, а не медленная модель.

Проверка: те же пять ключей по отдельности — 200 OK у всех пяти. То есть
модели живы, сломался лимит.

**Вывод:** при free tier ограничение не «в день», а «в минуту», причём
поквартирно по аккаунту. Пробник теперь делает паузу 4 с между кандидатами —
после этого **15/15 кандидатов живы** (до паузы: 5 Gemini-подряд → 502).

---

## 5. Проверка через настоящий рантайм

Замеры выше — это запросы к роутеру руками. Настоящая проверка: субагенты
Minimax Code, привязанные к Gemini-моделям, должны прочитать картинку так, как
это делает приложение.

| Агент | Модель | Ответ | prompt-токенов в журнале роутера |
|---|---|---|---|
| `fast-vision` | `gemini/gemini-3.1-flash-lite` | «Red circle: left, top / Blue square: center, top / Green triangle: right, top», текст `IN`, `VISION-OK-42`, `HETERION SWARM` — дословно | 17 759 / 17 618, HTTP 200 |
| `wide-vision` | `gemini/gemini-3-flash-preview` | то же, «top left / top centre / top right», `IN` — белым внутри синего квадрата | 17 892 / 16 573, HTTP 200 |

Оба субагента отдельно оговорили, что картинку **получили**, а не угадали: в
ответе есть уникальные строки `VISION-OK-42` и `HETERION SWARM`, которых не
было в задании.

Это и есть сквозной путь: рантайм Minimax (результат инструмента `read` с
картинкой) → `POST /v1/chat/completions` → трансляция OpenAI→Gemini
(`inlineData`) → Gemini API. До фикса этот путь рвался на трансляции.

---

## 6. Что изменено

### 6.1 В репозитории

| Файл | Изменение |
|---|---|
| `crates/heterion-router-translate/src/gemini_request.rs` | картинки → `inlineData`; удалённый URL → заметка вместо молчания; 3 теста; обновлён док-комментарий модуля |
| `crates/heterion-router-providers/registry.json` | провайдер `gemini`: +6 моделей — `gemini-3.5-flash`, `gemini-3.6-flash`, `gemini-3.5-flash-lite`, `gemini-3.1-flash-lite-preview`, `gemini-flash-lite-latest`, `gemma-4-26b-a4b-it` |
| `scripts/subagent/auto_call.py` | в цепочки добавлены измеренные Gemini-кандидаты; в `--probe` добавлена пауза 4 с (квота/мин) |
| `docs/research/GEMINI-VISION-FIX-REPORT-2026-10-26.md` | этот отчёт |

Мёртвые id (`gemini-2.5-*`, `gemini-3.7-flash`, `gemini-3.1-pro-preview`) из
реестра **не удалены**: реестр — каталог провайдера, а не список того, что
работает на конкретном ключе (на ключе с биллингом `2.5-pro` вполне жив). В
цепочки они не попали.

### 6.2 Вне репозитория

* `~/.minimax/config.yaml` — в каталог добавлены `gemini/gemini-3.1-flash-lite`,
  `gemini/gemini-3.5-flash-lite`, `gemini/gemini-3.6-flash`,
  `gemini/gemma-4-26b-a4b-it` (у всех `attachment: true`).
* `~/.minimax/agents/fast-vision/agent.md` — новый агент на
  `custom_provider:heterion/gemini/gemini-3.1-flash-lite`: чтение картинок с
  большим запасом квоты (500 RPD вместо 20).
* `~/.heterion-router/storage.sqlite` — комбо `auto-subagent` и `auto-fast`
  расширены хвостом из Gemini-моделей (бэкап до правки:
  `/tmp/storage.sqlite.bak-*`).

Цепочка `auto-subagent` после правки:

```
100 beam · 95 nemotron-3-ultra · 90 gemma4:31b · 85 nemotron-3-super
75 gpt-oss:120b · 62 gemini-3.6-flash · 60 gemini-3-flash-preview
58 gemini-3.1-flash-lite · 54 gemma-4-26b-a4b-it · 50 codestral-latest
40 grok-4.6 · 30 openai-fast · 20 openrouter/free
```

Раньше кросс-провайдерный хвост был один (`gemini-3-flash-preview`); теперь
Gemini-сегмент расширен и перекрывает квоту самой семьи 3 Flash.

---

## 7. Ограничения (честно)

* **Удалённые URL картинок не инлайнятся.** Gemini не умеет fetch, а HTTP-клиент
  роутера умеет только POST. Сейчас на месте картинки остаётся текстовая
  заметка с URL — модель не будет выдумывать содержимое, но и не увидит картинку.
  Поддержка потребовала бы `GET` в `heterion-router-http` и выборки байтов в
  `gemini.rs`; это отдельная задача.
* `gemma-4-31b-it` — нестабилен на стороне Google (`500 INTERNAL`), в цепочки не
  включён.
* `antigravity-preview-latest` — только Interactions API, роутер его не
  реализует.
* `gemini-3.7-flash`, `gemini-3.8-flash`, `gemini-flash-latest` — висят
  (100+ s), в цепочки не включены.
* Квота — **в минуту**, а не в день: пачка параллельных Gemini-вызовов
  исчерпывает минутный бюджет аккаунтов. Для Free-плана это ограничивает
  параллельный swarm сильнее, чем дневной лимит.

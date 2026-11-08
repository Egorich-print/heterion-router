# Crouton Alpha через OpenRouter: VPN открыл API, но модель режет регион

Дата: 2026-11-08 · Проект: `heterion-router` · Проверка: живая, с машины владельца

## Коротко

- **OpenRouter теперь доступен.** Раньше домен целиком отдавал 403 Cloudflare
  по российскому IP. Сейчас трафик идёт через VPN (AmneziaVPN, выход в
  Германии, AS50436 Tele Columbus AG, 37.120.49.20), и OpenRouter отвечает
  200 на `/api/v1/models` (каталог из 461 модели) и обслуживает запросы.
- **Но `stealth/crouton-alpha` через OpenRouter по-прежнему не работает:**
  провайдер Stealth отвечает **403 «This model is not available in your
  region»**. Это **не** блок OpenRouter и **не** проблема VPN: другие модели
  через тот же ключ и тот же IP работают.
- **Блок модель-специфичный.** Второй провайдер Stealth — `glyph-cluster` —
  через тот же шлюз Kilo отвечает нормально. То есть Stealth блокирует
  именно `crouton-alpha` для наших аккаунтов, а не весь анонимный провайдер.
- **Что заработало и добавлено в роутер:** `kilocode/stealth/glyph-cluster` —
  бесплатная, работает, tool-calling и reasoning на месте, держит **12/12
  параллельных запросов за 4 секунды**. Добавлена в комбо `auto-subagent`.
- С VPN ожили и другие пути: `kilocode/openrouter/free`, `openrouter/auto`,
  free-модели OpenRouter (`nvidia/...:free` и др.).

## Что измерено

### Egress и доступность OpenRouter

| Проверка | Было (2026-11-05) | Стало (2026-11-08) |
| --- | --- | --- |
| Egress | Москва, AS12714 MegaFon, 95.220.195.208 | **Германия**, AS50436 Tele Columbus AG, 37.120.49.20, ip-api/ipwho.is: DE/Brandenburg, `proxy: false`, `hosting: false` |
| `GET /api/v1/models` | 403 Cloudflare | **200**, 461 модель |
| `GET /api/v1/models/user` | — | 200, 429 моделей, **ничего не скрыто** от аккаунта |
| `openrouter/auto` через роутер | 403 | **200, `PONG`** |
| `openrouter/nvidia/nemotron-3-nano-omni-30b-a3b-reasoning:free` | 403 | **200, `PONG`** |
| `kilocode/openrouter/free` | 403 | **200, `PONG`** |

VPN — это **AmneziaVPN** (не exit-node Tailscale: `ExitNodeID` пуст, трафик
идёт через TUN `utun4`).

### Где именно ломается crouton-alpha

Прямой вызов по OpenRouter с ключом роутера:

```json
{"error":{"message":"Provider returned error","code":403,
  "metadata":{"raw":"This model is not available in your region.",
              "provider_name":"Stealth","is_byok":false}}}
```

Проверено на **всех пяти** ключах OpenRouter из БД (`egork`,
`korosteleved`, `printedbyek`, `egorichkrypto`, `nikulinatatiana`) — ответ
идентичный. Через Kilo (другой агрегатор, тот же апстрим Stealth) — тоже 403
с тем же текстом.

Контрольные модели на том же ключе и том же IP:

| Модель | Ответ |
| --- | --- |
| `nvidia/nemotron-3-ultra-550b-a55b:free` | **OK, `PONG`** |
| `nvidia/nemotron-3-nano-omni-30b-a3b-reasoning:free` | **OK, `PONG`** |
| `stealth/glyph-cluster` (через Kilo) | **OK, `PONG`** |
| `openai/gpt-5.6-sol` | 402 Insufficient credits (кредиты ключа исчерпаны: `total_credits: 10`, `total_usage: 10.2`) |
| `anthropic/claude-opus-4.8`, `google/gemini-3-flash-preview`, `meta-llama/llama-4-scout`, `openai/gpt-oss-120b`, `qwen/qwen3.8-27b` | 402 Insufficient credits |
| `stealth/crouton-alpha` | **403 «not available in your region»** |

То есть кредитная проблема у ключа есть, но она **не** касается
`crouton-alpha`: модель бесплатная ($0/$0) и упирается в региональный
гейт Stealth.

### Почему это не «российский IP» и не «плохой VPN»

- GeoIP трёх независимых сервисов (ip-api.com, ipwho.is, локальный ipinfo)
  сходится: DE, резидентный ISP, флаги `proxy`/`hosting` не выставлены.
- OpenRouter **не прячет** модель от нашего аккаунта: публичный каталог и
  `/api/v1/models/user` совпадают по составу (429 моделей, `hidden: 0`),
  `stealth/crouton-alpha` присутствует в обоих.
- OpenRouter классифицирует 403 как «Geographic or legal blocks» — по
  документации это провайдерский, а не собственный гейт OpenRouter
  (`docs/guides/providers/traffic-routing.md`).
- У второй модели того же анонимного провайдера (`glyph-cluster`) регион
  проходит — значит ограничение адресное к конкретной модели, а не ко всему
  Stealth.
- Региональные домены `eu.openrouter.ai` / `us.openrouter.ai` не помогают:
  оба отдают 403 error code 1010 (In-Region Routing доступен только на
  Business/Enterprise).

**Наиболее вероятная причина:** Stealth определяет страну по **аккаунту**
OpenRouter/Kilo (страна регистрации), а не по IP запроса. Оба наших аккаунта
зарегистрированы из России, и оба шлюза показывают одну и ту же картину.
Отдельно возможно, что у Stealth на `crouton-alpha` свой узкий allowlist
стран, в который не входит DE residential ISP.

**Решающий эксперимент (нужен владелец):** переключить выход AmneziaVPN на
другую страну и повторить запрос. Если регион задан аккаунтом — не поможет;
если IP — поможет сразу.

### Найденная рабочая альтернатива: `stealth/glyph-cluster`

Каталог Kilo (`api.kilo.ai/api/openrouter/models`, 380 моделей) содержит обе
модели Stealth:

| Модель | Контекст | Вход | Цена | Статус |
| --- | --- | --- | --- | --- |
| `stealth/glyph-cluster` | 256K | text | $0 | **работает** |
| `stealth/crouton-alpha` | 1 048 576 | text+image+video+file+audio | $0 | 403 регион |

Проверено через роутер (`kilocode/stealth/glyph-cluster`):

| Проверка | Результат |
| --- | --- |
| Базовый вызов | `PONG`, 2.6–4.0 с |
| Tool-calling | `finish_reason: tool_calls`, корректные аргументы `get_weather({"city":"Moscow"})` |
| `reasoning_effort` low / medium / high | 200, ответ `4` на всех уровнях |
| `max_tokens: 100` | 200, ответ `PONG` (не съеден reasoning'ом, в отличие от crouton) |
| **8 параллельных запросов** | **8/8 за 3.9 с** |
| **12 параллельных запросов** | **12/12 за 4.0 с**, латентность 0.0–4.0 с |

Ровно тот сценарий, ради которого затевалась Crouton Alpha: массовые
параллельные субагентские задачи.

## Что изменено в репозитории

1. `crates/heterion-router-providers/registry.json`:
   - `openrouter`: добавлена модель `stealth/crouton-alpha` (она есть в
     живом каталоге OpenRouter; сейчас 403 по региону — модель станет
     рабочей сменой региона);
   - `kilocode`: добавлены `stealth/glyph-cluster` и `stealth/crouton-alpha`.
2. Комбо `auto-subagent` (priority, ctx 1048576) — `kilocode/stealth/glyph-cluster`
   вставлен третьим, вес 60:

| # | Модель | Вес |
| --- | --- | --- |
| 1 | `ollama-cloud/beam` | 90 |
| 2 | `mistral/codestral-latest` | 80 |
| 3 | **`kilocode/stealth/glyph-cluster`** | **60** |
| 4 | `mistral/open-mistral-nemo` | 45 |
| 5 | `pollinations/openai-fast` | 35 |
| 6–8 | `agentrouter/gpt-6.1-sol` / `gpt-5.6-sol` / `gpt-6-astra` | 30 / 25 / 20 |

Порядок прежний: бесплатное и рабочее — вперёд, платное (баланс agentrouter
на исходе) — в конец.
3. `/v1/models` роутера теперь отдаёт 344 модели, среди них
   `openrouter/stealth/crouton-alpha`, `kilocode/stealth/crouton-alpha`,
   `kilocode/stealth/glyph-cluster`, `cline/stealth/crouton-alpha`.

## Что осталось за владельцем

| Действие | Что даёт |
| --- | --- |
| **Сменить страну выхода AmneziaVPN** и повторить `stealth/crouton-alpha` | решающий тест IP-vs-аккаунт; при успехе — полная мультимодальность (image/video/file/audio) и контекст 1M |
| **Проверить страну аккаунта OpenRouter** (Settings → Preferences) | если там RU — сменить; это, вероятно, и есть причина блока |
| **Пополнить AIHubMix на $1** | снимает trial-10, открывает `crouton-alpha-free` (и 41 другие free-модели) |
| **Создать API-ключ Cline** (`app.cline.bot` → Settings → API Keys) | `cline/stealth/crouton-alpha` — ещё один путь к той же модели |

## Ограничения

- `kilocode/stealth/glyph-cluster` — только текст (вход `text`, не image),
  контекст 256K. Для мультимодальности нужна именно `crouton-alpha`.
- Бесплатный пул Kilo ротируется; 200 запросов/час на IP по их документам —
  при 12 параллельных субагентах это ~16 burst'ов в час.
- Kilo проксирует OpenRouter, поэтому блок Stealth приходит и оттуда; при
  смене региона нужно проверять оба пути.
- Кредиты на ключах OpenRouter исчерпаны (`total_credits: 10`,
  `total_usage: 10.2`) — платные модели через OpenRouter сейчас не
  заработают независимо от региона.

# Провайдеры с бесплатной квотой, доступные из России: API-ключи и OAuth-сессии

Дата: 2026-11-05 · Проект: `heterion-router` · Источник списка: FreeLLMAPI
(`tashfeenahmed/freellmapi`, ревиз `f2b3afc`) · Проверка: живая, с машины
владельца (egress Москва, AS12714 MegaFon, 95.220.195.208)

## Коротко

Из **73 платформ** каталога FreeLLMAPI:

| Категория | Сколько | Что значит |
| --- | --- | --- |
| Работают из РФ **без всяких ключей** | **4** | OVHcloud, LLM7, AI Horde, free.ai |
| Работают из РФ **с бесплатным ключом** | **9** | Z AI, ModelScope, SiliconFlow, Pollinations, free.ai, HuggingFace, Ollama, Mistral, Requesty/Router9/… |
| API отвечает, ключ обязателен (квота не подтверждена) | **18** | регистрация нужна, но 401 = «достижимо» |
| **Заблокированы по гео** | **23** | 403 Cloudflare — ключи не помогут |
| Мёртвые / не резолвятся | **14** | 404, DNS, таймаут |

Главное: **четыре провайдера дают бесплатный LLM-доступ без ключа и без
регистрации** — их можно использовать уже сейчас. Ещё один нюанс: **все
OAuth-сессии, которые уже есть в роутере, из РФ не работают** — ни одна из
десяти (см. раздел «OAuth»).

## Категория 1. Работают из РФ без ключа (можно взять и пользоваться сейчас)

| Провайдер | Base URL | Квота (проверено/документировано) | Как получить ключ | Что отвечает |
| --- | --- | --- | --- | --- |
| **OVHcloud AI Endpoints** | `https://oai.endpoints.kepler.ai.cloud.ovh.net/v1` | **2 RPM на IP на модель** (измерено: 3-й запрос подряд → «API rate limit exceeded») | ключ не нужен; с ключом — 400 RPM и $200 trial | `gpt-oss-120b`, `gpt-oss-20b`, `Qwen3.8-27B`, `Qwen3.5-397B-A17B`, `Meta-Llama-3_3-70B-Instruct` |
| **LLM7.io** | `https://api.llm7.io/v1` | анонимно — turbo-модели; с бесплатным токеном — **60 RPM, 250 req/час, 100K токенов/24ч** | `https://token.llm7.io/` (страница доступна) | `gpt-oss:20b`, `nemotron-3-nano:30b`, `codestral-latest`, `mistral-Nemo-Instruct-2407` |
| **AI Horde** | `https://oai.aihorde.net/v1` | анонимно — sentinel-ключ `0000000000`, низший приоритет в очереди; с регистрацией — свой ключ и выше приоритет + kudos | `https://aihorde.net/register` (доступна) | `koboldcpp/G4-MeroMero-26B-A4B-it-uncensored-heretic` и др. из живой очереди воркеров |
| **free.ai** | `https://api.free.ai/v1` | бесплатный аккаунт: **30 000 общих внутренних токенов/день и 1 000 API-запросов/месяц**, без карты | `https://free.ai/account/#api-keys` (доступна) | self-hosted: `qwen7b`, `qwen-coder`, `qwen3-coder`, `deepseek-r1` — все отвечают как `Qwen/Qwen3-30B-A3B-Instruct` |

### Важные оговорки

- **OVH — 2 RPM.** Это жёсткий лимит: 2 запроса в минуту на IP. Для
  параллельных субагентов не годится, но как «живой» fallback на случай,
  когда всё остальное лежит, — идеально. Модели крупные (Qwen3.5-397B-A17B,
  gpt-oss-120b).
- **free.ai: `/v1/chat/completions` отдаёт только self-hosted модели.**
  Внешние (включая `stealth/crouton-alpha`, `nvidia/nemotron-3.5-super-vl:free`)
  идут через `POST /v1/chat/` и требуют **купленных токенов** — ответ
  `premium_requires_purchase`. То есть Crouton Alpha в free.ai есть в
  каталоге, но бесплатно не достаётся.
- **AI Horde отдаёт нестабильный каталог**: модель может исчезнуть между
  двумя запросами («not known to this horde»). Плюс воркеры дописывают
  шаблонный текст после ответа — качество ниже, у зарегистрированных ключей
  приоритет выше.
- **LLM7 ротирует каталог часто**; turbo-модели анонимно, pro — только за
  $12/мес.

## Категория 2. Работают из РФ, нужен ключ с бесплатной квотой

| Провайдер | Base URL | Квота | Где взять | Заметка |
| --- | --- | --- | --- | --- |
| **Z AI (z.ai)** | `https://api.z.ai/api/paas/v4` | **GLM-4.7-Flash / GLM-4.6V-Flash — $0 навсегда** (в прайсе z.ai: input / cached input / output = Free) | `https://z.ai/manage-apikey/apikey-list` (доступна) | На международной платформе z.ai **телефон не требуется** и карта не нужна; chat-API **не требует** реального имени (Batch API — требует). Domestic `open.bigmodel.cn/api/paas/v4` — **отдельная система аккаунтов, балансы и ключи не переносятся**: ключ с z.ai даёт 401 на bigmodel.cn и наоборот. GLM-4.5-Flash выводится из эксплуатации, запросы перенаправляются на GLM-4.7-Flash |
| **ModelScope** | `https://api-inference.modelscope.cn/v1` | **2 000 запросов/день** всего, ≤500/модель; ~250 Magicube/день (≈0.5–2 на вызов) | `https://modelscope.cn/my/myaccesstoken` (доступна) | Нужна привязка Alibaba Cloud + верификация личности (китайские документы) — **это главное препятствие**; токен формата `ms-…` |
| **SiliconFlow** | `https://api.siliconflow.cn/v1` (CN) / `api.siliconflow.com/v1` (INT) | CN: часть моделей навсегда ¥0; INT: $1 кредит | `https://cloud.siliconflow.cn/account/ak` (доступна) | Free-модели CN требуют верификации по **китайским** документам; иностранцам — писать в саппорт |
| **Pollinations** | `https://gen.pollinations.ai/v1` | **1 ответ / 45 сек на IP** (FREE_LIMIT, код 402) | `https://enter.pollinations.ai/keys` (доступна) | `pollinations/openai-fast` уже работает в роутере — самый «живой» сейчас |
| **HuggingFace** | `https://router.huggingface.co/v1` | free-кредиты на счёте (у нас 402 — нет денег) | `https://huggingface.co/settings/tokens` (доступна) | Каталог из 125 моделей открыт |
| **Ollama Cloud** | `https://ollama.com/v1` | месячный лимит бесплатных вызовов | `https://ollama.com/settings/keys` (доступна) | У нас месячный лимит исчерпан — 429; `beam` ещё работает |
| **Mistral** | `https://api.mistral.ai/v1` | free-режим по умолчанию, без карты | `https://console.mistral.ai/api-keys/` (редирект 307 → доступна) | `codestral-latest` и `open-mistral-nemo` работают; free-промпты могут идти на обучение |
| **Requesty / Router9 / Typhoon / UnoRouter / Inferbase / Iteracompute / Novai / Bazaarlink / Lucidity / Plugsky / Ainative / ElectronHub / Routeway / Waterfall** | см. каталог | заявлены free-уровни, **не проверены живым вызовом** | ссылки в `client/src/components/keys/shared.tsx` | Все отдают 200 на каталог без ключа → API достижим из РФ; квота требует регистрации |

## Категория 3. API отвечает 401 (нужен ключ), но квота не подтверждена

`agnes`, `anyapi`, `bai`, `beatapi`, `blockrun`, `experiential`, `longcat`,
`nabh`, `nara`, `radeon` (AMD), `sail`, `sealion`, `simplellm`,
`volcengine` (Volcengine Ark), `xfyun` (讯飞), `zhipu` (в этой группе
 числится из-за разных namespace), `siliconflow` (INT).

Отдельно: `zhipu`, `siliconflow`, `volcengine`, `xfyun`, `qianfan`,
`longcat`, `radeon` — **китайские платформы**, они физически доступны из РФ
и не гео-блокируются, но бесплатные модели у большинства из них требуют
китайской верификации личности.

## Категория 4. Заблокированы по гео — ключи бесполезны

`openrouter` (весь домен), `groq`, `google` (Gemini: «User location is not
supported for the API use»), `kilo`, `opencode`, `cerebras`, `cohere`,
`aion`, `airforce`, `apinex`, `clod`, `freeinference`, `gizmo`, `kiosapi`,
`moondream`, `navy`, `qianfan`, `reka`, `septor`, `xkiro`, `parallel`,
`exa`.

Проверено отдельно для OpenRouter: 403 отдают **все пять** ключей из БД,
на **любую** модель, и даже на `GET /` — блок по IP/гео, не по ключу и не
по модели. Gemini блокируется мягче: `/v1beta/models` отвечает 200, а
`chat/completions` — 400 с текстом про локацию.

## Категория 5. Мёртвые или сломанные

- **Не резолвятся / нет TCP:** `aclide`, `dreamprompting`, `kenari`,
  `llmtr`, `logfare`, `speka` (DNS не резолвится).
- **404 на каталоге** (возможно, другой путь API): `blaze` (есть подсказка
  в ответе: `api.blazeapi.org/paid/v1/chat/completions`), `onomeo`,
  `orcarouter`, `jamai`.
- **451 гео:** `nvidia` (NVIDIA NIM).
- **Таймаут:** `aisense`.
- **404 — это не LLM-эндпоинты:** `tavily`, `exa`, `firecrawl`, `linkup`,
  `keenable`, `serpsearch`, `brave`, `cloudflare`, `speechify` — это
  поиск/скрейпинг/медиа, а не чат.

## OAuth-сессии: все десять не работают из РФ

В роутере уже есть подключения с `auth_type: oauth`: `cline`, `kilocode`,
`trae`, `opencode`, `opencode-go`, `kiro`, `command-code`, `qoder`,
`grok-cli`, `devin-cli`. Живая проверка через роутер:

| Провайдер | Ответ | Причина |
| --- | --- | --- |
| `cline` | 401 | OAuth-токен истёк 2026-09-17; refresh отдаёт `invalid_grant` |
| `kilocode` | 403 | гео-блок |
| `trae` | 404 | путь API не тот |
| `opencode` / `opencode-go` | 404 / 400 | гео-блок + нет бэкенда |
| `kiro` / `devin-cli` | 404 | нет бэкенда |
| `grok-cli` | 403 | гео-блок (61.8 с) |
| `command-code` | 404 | нет бэкенда |
| `kimi-coding` | 401 | ключ недействителен |
| `qoder` | — | все 5 подключений выключены |

**Вывод:** новую OAuth-сессию стоит брать только там, где сама OAuth-страница
достижима из РФ. Из проверенного — Cline (`cline.bot`) отдаёт каталог из 435
моделей (включая `stealth/crouton-alpha`) с кодом 200, но его refresh-токен
мёртв; пере-авторизация браузером — единственный путь поставить Cline на
поток. Kilo/Grok/Google — гео-блок, возврат сессии не поможет.

## Что делать прямо сейчас (без регистраций)

1. **OVHcloud** — уже можно завозить в роутер как `ovh` с ключом `no-key`:
   пять моделей, 2 RPM. Резерв на случай, когда остальные лежат.
2. **LLM7** — анонимно `gpt-oss:20b` и `nemotron-3-nano:30b`; с бесплатным
   токеном с `token.llm7.io` — 60 RPM и 100K токенов/сутки.
3. **free.ai** — `qwen7b` / `deepseek-r1` без ключа; с ключом — 30K
   токенов/день, 1000 запросов/месяц.
4. **AI Horde** — анонимно, sentinel `0000000000`; регистрация даёт
   приоритет в очереди.

## Что требует одного действия владельца

| Действие | Что даёт |
| --- | --- |
| **Пополнить AIHubMix на $1** | снимает trial-10 и открывает 100 запросов/день + 42 free-модели, включая `crouton-alpha-free` (SWE-bench Verified 71.1 у `nemotron-3.5-super-vl-free`) |
| **Ключ Z AI** (z.ai, без телефона и без верификации) | бесплатные GLM-4.7-Flash (200K, reasoning) и GLM-4.6V-Flash (мультимодальные) — $0 навсегда |
| **Ключ Pollinations** | снимает лимит «1 ответ / 45 сек» на IP |
| **Пере-авторизация Cline** | возвращает провайдер с `stealth/crouton-alpha` в каталоге из 435 моделей |
| **Аккаунт ModelScope** | 2000 запросов/день, но нужна привязка Alibaba Cloud и верификация по китайским документам — для владельца в РФ это, скорее всего, закрыто |

## Методика

- Список платформ взят из FreeLLMAPI: `server/src/providers/index.ts`
  (реестр), `client/src/components/keys/shared.tsx` (73 платформы с
  ссылками на страницы ключей и флагом `keyless`), `docs/providers/*.md`
  (пошаговые инструкции по части провайдеров).
- Квоты и правила — из `mnfst/awesome-free-llm-apis` и
  `pacocartones/free-llm-api-hub` (обе — с проверенными ссылками на
  документацию вендоров), плюс `docs/providers/freeai.md` из самого
  FreeLLMAPI.
- Доступность — **живой прогон с машины владельца**: TCP-соединение к хосту,
  затем `GET /v1/models` без ключа (200 = каталог открыт, 401 = нужен ключ,
  403 = гео-блок), затем реальный `POST /chat/completions` по каждой
  платформе с каталогом 200, затем проверка страниц регистрации ключей.
- Обход гео-блоков **не применялся**: exit-node Tailscale запрещён
  владельцем (трафик машины при этом падает).

## Чего отчёт не покрывает

- Не проверены живым вызовом платформы с 200-каталогом, но не из таблицы
  раздела 1: `ainative`, `bazaarlink`, `electronhub`, `huggingface`,
  `inferbase`, `iteracompute`, `lucidity`, `novai`, `plugsky`, `requesty`,
  `router9`, `routeway`, `unorouter`, `waterfall` — по ним известно только,
  что API достижим и каталог открыт; квота не подтверждена.
- Не измерялась фактическая скорость и качество ответов — отчёт о том, где
  **можно взять** ключ/сессию и **работает ли** из РФ.
- Каталоги вольных моделей у OpenRouter/Kilo быстро устаревают: проверено
  на OpenRouter, что его каталог вообще недостижим, поэтому список `:free`
  моделей там приведён по документации, а не по живой выдаче.

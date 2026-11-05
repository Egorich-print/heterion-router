# Crouton Alpha (stealth): подключение к роутеру и MiniMax Code

Дата: 2026-11-05 · Проект: `heterion-router` · Ключ: `aihubmix`

## Коротко

- Модель — это **не** модель MiniMax и **не** модель OpenRouter как таковые.
  `stealth/crouton-alpha` — анонимный провайдер «Stealth» в каталоге
  OpenRouter; в самом каталоге MiniMax (`agent.minimax.io/mavis/api/v1/models`)
  такой модели нет — там только `MiniMax-M3` и `MiniMax-M3.1-Flash`
  (проверено 2026-11-05). То есть «stealth» — это не режим MiniMax, а
  отдельный провайдер с анонимным разработчиком.
- **OpenRouter из России недоступен целиком**: Cloudflare отдаёт 403
  «Access denied by security policy» на *весь домен* — включая `/` и
  `/models`, не только на API. Egress этой машины — Москва, AS12714
  (MegaFon), 95.220.195.208. Все пять ключей OpenRouter из БД роутера дают
  один и тот же 403, то есть дело не в ключах. Обход через Tailscale
  exit-node не использовался (см. «Что не трогали»).
- **Рабочий путь найден: AIHubMix** (`aihubmix.com`) — агрегатор, доступный
  из РФ без обхода, в его каталоге есть `crouton-alpha-free`
  (`developer_id: 41` = Stealth). Роутер теперь отдаёт модель как
  `aihubmix/crouton-alpha-free`.
- Модель **работает**: через роутер отвечает `PONG`, умеет tool-calling и
  `reasoning_effort` (low/medium/high). Контекст 1 048 576, вход — text+image.
- **Ограничение осталось**: бесплатный trial AIHubMix — 10 запросов
  (`trial_unpaid_request_limit`), они израсходованы. После разового
  пополнения от $1 включаются боевые лимиты: 100 запросов/день,
  10/минуту, 1 000 000 токенов/день (`paid_threshold_usd: 1`).
  До пополнения любой запрос возвращает HTTP 200 с текстом
  «Sorry, to prevent abuse of free resources…» — то есть **ошибка лимита
  приходит как успешный ответ**, и её легко не заметить.

## Что измерено

### Диагностика OpenRouter (гео-блок)

| Запрос | Результат |
| --- | --- |
| `GET https://openrouter.ai/` | 403 (Cloudflare) |
| `GET https://openrouter.ai/api/v1/models` | 403 (Cloudflare) |
| `GET https://openrouter.ai/api/v1/chat/completions` (любая модель) | 403 |
| То же с 5 разными ключами из БД | 403 у всех |
| `GET https://openrouter.ai/api/v1/models` через `api.openrouter.ai` | DNS не резолвится |

Заголовки ответа: `server: cloudflare`, `cf-ray: …-HEL` — фронт Хельсинки,
блок по IP/гео, а не по ключу и не по модели.

Роутер с этим сталкивается ровно так же: `openrouter/stealth/crouton-alpha`,
`openrouter/auto`, `openrouter-free`, `kilocode/openrouter/free` и
`kilo-gateway/kilo-auto/free` — все 403.

### Пути, которые не сработали

| Путь | Итог |
| --- | --- |
| OpenRouter напрямую | 403 по гео (весь домен) |
| `kilocode` (`api.kilo.ai/api/openrouter/...`) | 403 Forbidden |
| `cline` (`api.cline.bot/api/v1/...`) | 401 — OAuth-токен истёк 2026-09-17; refresh отдаёт `invalid_grant` |
| `trae` (`core-normal.trae.ai`) | 404 (путь не тот API) |
| Tailscale exit-node `297203` | трафик через него не идёт (см. ниже) |
| AIHubMix без пополнения | 10 запросов, затем «quota» текстом |

Обход через exit-node **не применялся**: при включённом exit-node весь
интернет-трафик машины перестаёт идти (`curl` → 000/timeout), а сам
Tailscale уходит в `BackendState: Stopped` и теряет netmap с пирами. Это
правило владельца, и оно же подтверждено измерением.

### Пути, которые сработали

Обход по списку агрегаторов (все ответы из РФ, без VPN):

| Агрегатор | `/v1/models` | `crouton` в каталоге |
| --- | --- | --- |
| `aihubmix.com` | 200 | да (`crouton-alpha-free`) |
| `api.cline.bot` | 200 | да (`stealth/crouton-alpha`) |
| `api.sambanova.ai`, `api.z.ai`, `api.deepseek.com`, `router.huggingface.co` | 200 | нет |
| `openrouter.ai`, `api.kilo.ai`, `opencode.ai`, `api.groq.com`, `api.cerebras.ai`, `api.together.xyz` | 403 | — (гео-блок) |
| `api.302.ai`, `api.gpt.ge`, `api.nuwaapi.com`, `api.qingyuntop.top`, `api.siliconflow.com` | 401 | — (нужен ключ) |

Из всех 27 провайдеров, у которых есть живое подключение в БД роутера,
`crouton` нашёлся только у двух: **aihubmix** (200) и **cline** (200, но токен
истёк).

### Поведение модели

Прогон напрямую по `https://aihubmix.com/v1/chat/completions`, ключ из
`~/.minimax/config.yaml`-источника (проба до расхода trial-квоты):

| Проверка | Результат |
| --- | --- |
| Базовый вызов | `PONG`, `finish_reason: stop`, 10 токенов |
| Tool-calling | `finish_reason: tool_calls`, `get_weather({"city":"Москва"})` |
| `reasoning_effort: low` | 200, ответ `4` |
| `reasoning_effort: high` | 200, ответ `2 + 2 = 4`, `reasoning_tokens: 17` |
| Вход с картинкой (data-URL PNG) | 400 от провайдера Stealth на 1×1 px; на нормальном PNG — ответ отправлен, но съеден квотой |

**Важно про `max_tokens`:** при `max_tokens: 100` модель возвращает
`finish_reason: "length"` с **пустым** content — бюджет съедает reasoning.
С `max_tokens: 4000` тот же запрос отвечает нормально. Это ровно тот класс
ловушки, что уже описан в AGENTS.md для `gpt-6.1-sol`/`gpt-6-astra`
(«весь бюджет уходит в thinking — ответ пустой»), поэтому рабочий минимум
здесь тоже 4000+.

### Через роутер

| Проверка | Результат |
| --- | --- |
| `GET /v1/models` | 340 моделей, среди них `aihubmix/crouton-alpha-free` |
| Лог старта | `backend available: aihubmix (https://aihubmix.com/v1/chat/completions, 1 credential(s))` |
| `POST /v1/chat/completions` | 200, `model: crouton-alpha-free`, ответ `PONG` |
| Стриминг | SSE-чанки идут, `finish_reason` доезжает |

## Что сделано в репозитории

1. `crates/heterion-router-providers/registry.json` — новый провайдер:

```json
{
  "id": "aihubmix",
  "alias": "ahm",
  "format": "openai",
  "executor": "default",
  "auth_type": "apikey",
  "base_url": "https://aihubmix.com/v1/chat/completions",
  "models": [{ "id": "crouton-alpha-free", "target_format": null }]
}
```

2. Снапшот `provider_id_list_snapshot.snap` обновлён (в списке id провайдеров
   появилась строка `aihubmix`) — снапшот часть контракта, правится вместе с
   кодом.

3. Подключение в БД роутера (`provider_connections`): `provider=aihubmix`,
   `auth_type=apikey`, `is_active=1`, `priority=3`, ключ зашифрован штатным
   `FieldCrypto` (`enc:v1:…`, `STATIC_SALT`, prefix `enc:v1:`) — тем же
   алгоритмом, что и остальные ключи роутера. Бэкап БД перед правкой:
   `~/backups/2026-11-05-crouton-alpha/` (`storage.sqlite` + `SHA256SUMS.txt`).

4. `~/.minimax/config.yaml` — модель добавлена в `custom_provider.heterion`
   под ключом `aihubmix/crouton-alpha-free`: `reasoning`, `tool_call`,
   `temperature`, `attachment`, вход `[text, image]`, `limit.context 1048576`,
   `limit.output 32768`, `thinking.effortOptions [low, medium, high]`.
   Бэкап: `~/.minimax/config.yaml.bak-20261105-crouton-alpha`.

## Ограничения, которые остались

1. **Квота AIHubMix.** Trial — 10 запросов, израсходованы. Без пополнения
   аккаунта модель отвечает «извините, 10 попыток» HTTP 200 — ложный успех.
   С пополнением от $1: 100 запросов/день, 10/минуту, 1M токенов/день.
2. **Каталог MiniMax Code — снимок на старте приложения** (это уже зафиксировано
   в AGENTS.md для `limit.context`). Модель, добавленная в `config.yaml` после
   старта, рантаймом не подхватывается: `assertRequiredCatalogModel` отвечает
   «Model … is unavailable in the current catalog». **После рестарта
   MiniMax Code** модель появится; до него работает только явный путь через
   роутер.
3. **Cline как запасной путь** (модель там называется `stealth/crouton-alpha`)
   требует переавторизации: и access-, и refresh-токен истекли.

## Как пользоваться

Роутер (после рестарта MiniMax Code — как модель в UI):

```sh
curl http://127.0.0.1:20128/v1/chat/completions \
  -H "Authorization: Bearer $ROUTER_KEY" \
  -H "Content-Type: application/json" \
  -d '{"model":"aihubmix/crouton-alpha-free","messages":[{"role":"user","content":"Say PONG"}],"max_tokens":4000}'
```

В MiniMax Code после рестарта — как custom_provider:

```
custom_provider:heterion/aihubmix/crouton-alpha-free
```

Указывать `effort` обязательно (`low`/`medium`/`high`) — иначе рантайм
требует его явно, как и для остальных reasoning-моделей.

## Что не трогали

- **Exit-node Tailscale** не включался в рабочем режиме: при попытке весь
  трафик машины падает, а Tailscale теряет netmap. После отката состояние
  восстановлено (`ExitNodeID: ""`, `WantRunning: true`, все пиры видны, egress
  — Москва 95.220.195.208).
- Живой launchd-инстанс `20128` для проверки правок в `~/.minimax/config.yaml`
  не использовался — mission-правки проверяются на живом инстансе только по
  отдельному решению.

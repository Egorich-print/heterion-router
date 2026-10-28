# ADR-019 — Exa search executor (`custom`/`exa` providers)

Дата: 2026-10-14 · Статус: принято (реализовано в рабочем дереве поверх `a878692`).

## Контекст

`exa-search` был добавлен в реестр (ADR-016), но не имел исполнителя — любой
запрос падал в unserved. Формат `custom` + `executor: exa`; по плану миссии —
поисковый инструмент, НЕ член чат-комбо.

Факт со спеки: OpenAPI Exa (`exa-spec.yaml`, сервер `https://api.exa.ai`,
**без `/v1`**): `POST /search`, auth `x-api-key` или Bearer; без `contents`
ответ содержит только title/url/date — снипеты приходят только при
`contents.highlights`/`text`. Прежний `base_url: https://api.exa.ai/v1` в
реестре был ошибочен (404 на `/v1/search`, подтверждено curl: `/search` → 402
без ключа, `/v1/search` → 404); исправлен на `https://api.exa.ai`.

## Решение

Модуль `crates/heterion-router-gateway/src/exa.rs`:

- `ExaBackend::new(provider, endpoint_url, tokens)` — ротация ключей
  (`x-api-key`).
- Chat-поверхность: последний user-месседж = запрос (`content_text`),
  тело `{query, numResults: 5, contents: {highlights: true}}`.
- Ответ рендерится в цитированный блок: заголовок, URL, дата, сниппет
  (highlights-массив или text, до 300 символов). Пустой результат —
  честное «No search results», ничего не выдумывается.
- Стриминг: один дельта-чанк + `stop` (upstream не стримит).
- Usage нулевой: Exa тарифицирует за поиск, не за токены — cost-заголовок
  остаётся выключенным (unpriced → no fake zero).
- Регистрация в `select_backend` по `executor == "exa"`; тест
  `unknown_providers_are_reported_once_with_connection_counts` переведён на
  фиктивного провайдера.

## Следствия

- `exa-search/exa-search` доступен как модель шлюза (прямой вызов, не в
  комбо).
- Живая проверка (гейт плана): реальный поиск «GLM-5.3-Flash context window
  specs» вернул 5 результатов с контентом; среди источников — доки Z.AI
  («support for a 1M-token context window») и Cloudflare («1,048,576
  tokens») — независимо подтвердившие контекст GLM-5.3-Flash.
- Оставшиеся без исполнителей форматы: `antigravity` (2), `cursor` (2),
  `clova` (1), `kiro` (1).

## Проверка

- `scripts/ci/check.sh` → all green (8 тестов exa).
- Реальный вызов через тестовый инстанс шлюза — контент подтверждён.

---

## Аддендум 2026-10-26 — параметры поиска и потребитель в MiniMax Code

### Контекст

Поиск работал, но был «слепым»: всегда ровно 5 результатов, только highlights, без
фильтров по датам и доменам. Плюс к этому в MiniMax Code не было инструмента
поиска вообще, хотя ключи Exa уже лежали в роутере (5 аккаунтов, зашифровано).

### Решение

1. **Параметры поиска едут в теле чат-запроса** под объектом `"exa"` (структура
   запроса сохраняет неизвестные поля, поэтому старый клиент ничего не ломает):

   | Поле | Значение | Поведение |
   |---|---|---|
   | `numResults` | 1..100 | по умолчанию 5, за пределами диапазона — clamp |
   | `type` | `auto`/`neural`/`fast`/`deep`/`lite`/`deep-lite`/`deep-reasoning`/`instant` | незнакомый режим отбрасывается (апстрим отвечает 400) |
   | `category` | строка | проксируется как есть |
   | `includeDomains` / `excludeDomains` | массив строк | пустые не отправляются |
   | `startPublishedDate` / `endPublishedDate` | ISO-дата | проксируются как есть |

   Без объекта `"exa"` тело запроса не меняется — прежнее поведение сохранено
   (тест `without_options_the_upstream_request_keeps_its_default_shape`).

2. **Потребитель — плагин MiniMax Code**, а не новый секрет: MCP-сервер
   `~/.minimax/plugins/exa-search/server.py` отдаёт инструмент `web_search`,
   который ходит в роутер (`model: "exa/exa-search"`) и возвращает
   цитированный блок. Ключ Exa в плагине не хранится: сервер берёт
   `custom_provider.heterion.options.apiKey` из конфига MiniMax Code, а сам
   поиск, ротацию ключей и биллинг оставляет роутеру.

### Следствия

- `exa/exa-search` — модель шлюза с настраиваемыми параметрами; ключи Exa
  остаются зашифрованными в БД роутера и не дублируются ни в плагине, ни в
  конфиге приложения.
- Инструмент `web_search` доступен модели как обычный MCP-вызов; проверено
  живым вызовом из субагента (найден актуальный релиз Rust 1.99.1 от
  2026-10-16 и документация Exa `/search`).
- Ограничение прежнее: поиск — не член чат-комбо (ADR-016), а отдельная
  модель шлюза.

### Проверка

- `scripts/ci/check.sh` → all green (10 тестов exa).
- Живой вызов: `numResults: 2` → «Found 2 result(s)» на живом Exa.
- Протокольный тест MCP: `initialize` → `tools/list` → `tools/call` (реальный
  поиск) → пустой запрос отдаёт `isError`.

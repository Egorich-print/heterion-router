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

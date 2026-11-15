# ADR-028 — Ссылка на модель в комбо: голый id или полный ref

- **Статус:** принято 2026-11-16
- **Уточнило:** ADR-013 (рефакторинг мёртвых полей комбо) — правило разбора
  модели в комбо теперь однозначно.

## Контекст

В комбо модель хранится в поле `model` рядом с `providerId`. Что именно
лежит в этом поле, не было зафиксировано, и три источника писали его по-разному:

1. **JS-дашборд** (наследие OmniRoute) писал **полный ref** —
   `"model": "kilocode/stealth/glyph-cluster"` вместе с
   `"providerId": "kilocode"`.
2. **Rust admin-API** (`PATCH /api/combos/{id}`) принимает **голый id** и
   **сам приклеивает префикс провайдера**:
   `render_model_items` пишет `format!("{}/{}", spec.provider, bare)`
   (`crates/heterion-router-db/src/repos.rs`). Клиенту отправлять префикс
   нельзя — получится удвоенный ref.
3. **Rust-парсер** (`parse_combo_rich`) срезал первый сегмент **всегда**,
   независимо от провайдера.

Из этого выросли две живые поломки.

- **`main-262k` был сломан целиком.** В его данных лежали удвоенные ref
  (`mistral/mistral/codestral-latest`, `kilocode/kilocode/stealth/glyph-cluster`).
  Парсер срезал один сегмент → апстрим получал `mistral/codestral-latest`
  и отвечал 400; клиент видел `502 upstream request failed` на все восемь
  моделей комбо. Найдено прогоном моделей MCode 2026-11-16: `main-262k` —
  единственная модель из 37, которая падала и напрямую, и через `auto`.
- **Модели, чей id сам содержит слеш** (`groq/openai/gpt-oss-20b`,
  `kilocode/stealth/glyph-cluster`, `sealion/aisingapore/…`) ломались при
  правке комбо через веб-дашборд: дашборд шлёт **голый** id, сервер клеит
  префикс провайдера, а парсер срезал первый сегмент голого id — то есть
  у `stealth/glyph-cluster` исчезала первая часть пути.

## Решение

Парсер снимает префикс провайдера **только если ведущий сегмент именно
этот провайдер**, и снимает столько сегментов, сколько их повторяется:

| в `model` | `providerId` | уходит на апстрим |
|---|---|---|
| `codestral-latest` | `mistral` | `codestral-latest` |
| `mistral/codestral-latest` | `mistral` | `codestral-latest` |
| `kilocode/kilocode/stealth/glyph-cluster` | `kilocode` | `stealth/glyph-cluster` |
| `openai/gpt-oss-20b` | `groq` | `openai/gpt-oss-20b` |
| `groq/openai/gpt-oss-20b` | `groq` | `openai/gpt-oss-20b` |

То есть формат хранения («`provider/bare`») и формат API («голый id»)
остаются как были, а разбор становится терпимым к обоим и к удвоенному
префиксу.

**Что делать оператору:** в `PATCH /api/combos/{id}` поле `model` — это
**голый id** модели (без префикса провайдера). Префикс сервер приклеит сам.
Дашборд на Svelte уже шлёт голый id — это правильно.

## Данные

`main-262k` перезаписан через admin-API в канонической форме
(`mistral/codestral-latest`, `kilocode/stealth/glyph-cluster`, …). Живая
проверка после правки: `main-262k` → 200, обслужил `mistral/codestral-latest`.

## Контрактные тесты

`crates/heterion-router-gateway/src/combos.rs`:
`a_bare_model_id_keeps_every_segment` и `a_repeated_provider_prefix_is_stripped`.

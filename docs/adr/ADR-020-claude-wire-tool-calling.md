# ADR-020 — Claude-wire tool calling (request and streaming response)

Дата: 2026-10-14 · Статус: принято (реализовано поверх `6e2963c`).

## Контекст

Агентские сессии на main-262k (голова `zai/glm-5.3-flash`, claude-wire)
«успешно» заканчивали ход, но миссия не продолжалась — пользователю
приходилось писать «продолжай». Причина — двойной дроп инструментов:

1. **Запрос**: `openai_to_claude` не переносил `tools`/`tool_choice`
   («tool-carrying requests are out of scope») — модель не видела
   инструменты вообще и отвечала текстом.
2. **Ответ (стрим)**: `convert_event` игнорировал `content_block_start`
   с `tool_use` и `input_json_delta` («explicitly deferred») — клиент
   получал `finish_reason: tool_calls` с пустым списком вызовов.

Следствие: агент без инструментов не может выполнить работу → ход
завершается текстом → миссия стоит. Сюда же относится вызов скиллов
OpenCode: для модели это обычный tool-вызов.

## Решение

**Запрос** (`claude_request.rs`):
- `tools` → Claude-определения (`input_schema`), не-function типы
  пропускаются; `tool_choice`: `auto→auto`, `required→any`,
  `none→none`, named→`{"type":"tool"}`.
- assistant `tool_calls` → `tool_use` блоки (arguments-строка парсится
  в объект; битый JSON деградирует в `{}`).
- role `tool` → `tool_result` в user-ходе, непосредственно следующим за
  assistant `tool_use` (не склеивается с пользовательским текстом);
  consecutive user-ходы мержатся, tool_result идёт первым (валидный
  Claude, соответствует JS role-merging).

**Ответ (стрим)** (`claude.rs`):
- `ClaudeState` накапливает `tool_use` блоки (id/name из
  `content_block_start`, аргументы из `input_json_delta`);
- на `content_block_stop` блок эмитится как OpenAI `tool_calls`-дельта
  с последовательными индексами;
- фрагмент без начатого блока дропается, не выдумывается.

## Следствия

- Агентские циклы через claude-wire провайдеров (zai, agentrouter,
  kimi-coding, wafer, …) работают насквозь: мультиходовой
  tool→result→tool переигрывается корректно.
- Скиллы OpenCode вызываются агентами на main-262k/main-128k.
- Проверено живьём: стрим main-262k с `get_weather` →
  `finish_reason: tool_calls`, вызов с аргументами `{"city":"Paris"}`.

## Проверка

- `scripts/ci/check.sh` → all green (+6 тестов translate, +2 на
  round-trip и деградацию битого JSON).
- Живой стрим через боевой шлюз :20128 после обновления /Applications.

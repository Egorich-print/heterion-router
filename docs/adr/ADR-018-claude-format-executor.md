# ADR-018 — Claude-format executor for upstream `/v1/messages` providers

Дата: 2026-10-14 · Статус: принято (реализовано в рабочем дереве поверх `3fa7dd1`).

## Контекст

`select_backend` умел строить бэкенды только для форматов `openai`
(253 провайдера), `gemini` (3) и `openai-responses` (5). Всё прочее падало в
unserved — в том числе **10 провайдеров формата `claude`**: `zai`,
`agentrouter`, `anthropic`, `claude`, `kimi-coding`, `kimi-coding-apikey`,
`bailian-coding-plan`, `tabitoken`, `wafer`, `devin-cli-agentic`. У `zai` и
`kimi-coding` были активные креды, но ни один запрос к ним не мог быть
обслужен.

## Решение

Новый модуль `crates/heterion-router-gateway/src/claude.rs`:

- `ClaudeBackend::new(provider, endpoint_url, tokens)` — ротация кредов,
  как у `ResponsesBackend`.
- Запрос: `openai_request_body` → `openai_to_claude` (перевод уже был,
  паритет-тесты на JS ground truth).
- Заголовки: `anthropic-version: 2023-06-01` + кред в `x-api-key` и
  `Authorization: Bearer` одновременно (разные провайдеры ждут разное).
- Non-stream: новый конвертер `claude_message_to_completion` — блоки
  `text` → текст, `thinking` → `reasoning_content`, `tool_use` →
  `tool_calls`, `stop_reason` → finish (`end_turn→stop`,
  `max_tokens→length`, `tool_use→tool_calls`), usage input/output.
- Stream: общий `pump_sse_stream` + `convert_event` из translate;
  `MissingTerminal::Fail` — у claude нет `[DONE]`, терминал присылает
  `message_stop`.
- Регистрация в `select_backend` отдельной веткой `format == "claude"`;
  тест `unsupported_formats_are_reported_with_connection_counts` переведён
  на `kiro`/`cursor`.

## Следствия

- Осветлены 3 бэкенда с живыми кредами: `zai`, `kimi-coding`,
  `agentrouter` (последний отвечает 401 — ключ отклонён провайдером,
  не проблема исполнителя).
- Покрытие исполнителями: ~271 из 279 провайдеров реестра. Остались
  экзотические форматы: `antigravity` (2), `cursor` (2), `custom`/exa (2),
  `clova` (1), `kiro` (1).
- Проверено живьём: `zai/glm-5.3-flash` — текст, reasoning_content,
  usage корректны.

## Проверка

- `scripts/ci/check.sh` → all green (fmt, clippy -D warnings, 558 тестов,
  из них 11 новых в `claude.rs`).
- Живые вызовы через тестовый инстанс шлюза.

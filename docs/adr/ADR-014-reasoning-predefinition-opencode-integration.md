# ADR-014 — Reasoning mode predefinition and OpenCode integration

Дата: 2026-10-14 · Статус: принято (реализовано в рабочем дереве поверх `51dfe6d`).

## Контекст

В текущей реализации:
- Модели в `opencode.json` имеют явное управление параметром `reasoning: true`
- Heterion-router не предопределяет оптимальные режимы reasoning для моделей
- Пользователь должен вручную настраивать параметры reasoning в OpenCode
- Это приводит к неоптимальному использованию токенов: либо избыточному потреблению при незачем включенном reasoning, либо недостаточному качеству при выключенном reasoning когда оно было бы полезно

Audit показал что разные модели имеют разные оптимальные режимы reasoning:
- Grok models: эффективны на уровне reasoning: medium/low
- Claude models: часто требуют reasoning: high для сложных задач  
- Gemini models: имеют специфичные ограничения из-за rate limits
- DeepSeek models: варьируются в эффективности reasoning

## Решение

1. **Преdefine оптимальные режимы reasoning в heterion-router**:
   - Каждой модели присваивается рекомендуемый уровень reasoning на основе:
     * Документации провайдера
     * Тестирования на фиксированном наборе задач
     * Анализов от источников типа Artificial Analysis
   - Режимы варьируются от `none` (no reasoning) до `low`, `medium`, `high`
   - Режимы хранятся в конфигурации heterion-router и применяются автоматически

2. **Убрать явное управление reasoning из opencode.json для heterion-router моделей**:
   - Удалить все `"reasoning": true` поля из секции `"heterion-router"` → `"models"`
   - Параметры reasoning теперь управляются исключительно heterion-router
   - Другие провайдеры в opencode.json (commandcode, intel-b580 и т.д.) оставляют свои настройки reasoning

3. **Документировать какие модели имеют какое предопределенное reasoning**:
   - В документации проекта объяснить логику выбора режимов
   - Предоставить таблицу соответствия модели → reasoning level

## Следствия

- **Оптимизация использования токенов**: уменьшение расхода на избыточный reasoning там где он не нужен
- **Улучшение качества ответов**: увеличение reasoning там где он дает существенное улучшение качества
- **Простота использования**: пользователь не должен вручную настраивать reasoning для heterion-router
- **Совместимость**: изменение не ломает существующую функциональность, только улучшает её
- **Прозрачность**: пользователь все еще может видеть какой уровень reasoning используется через логи

## Реализация (2026-10-14)

Модуль `crates/heterion-router-gateway/src/reasoning.rs`: `default_effort(model) -> Option<&'static str>`.
Хук в `backend.rs::openai_request_body`: применяется только когда запрос НЕ содержит явный
`reasoning_effort` / `reasoning.effort` — клиентский override всегда выигрывает. Покрывает
исполнители openai, responses и grok-cli (все идут через `openai_request_body`;
`openai_to_responses` уже маппит `reasoning_effort` → `reasoning.effort`). Gemini-исполнитель
строит своё тело — предопределение на него не влияет (Gemini thinking управляется иначе).

Таблица предопределений (bare model part, регистронезависимо; `provider/model` сводится к модели):

| Семейство | Effort | Обоснование |
|-----------|--------|-------------|
| `claude-opus-*` | `high` | Сложные агентные задачи (docs провайдера) |
| `grok-*` | `medium` | Эффективен на medium/low (Artificial Analysis) |
| `deepseek-*` | `medium` | Сбалансированный дефолт по задачам |
| `gemini-*` | `low` | Rate-limited семейство — дешёвое мышление |
| остальное | — | Нет подтверждённых данных → провайдерный дефолт |

Тесты: таблица (`reasoning.rs`), применение в теле запроса и приоритет override
(`backend.rs::request_body_predefines_reasoning_for_evidenced_families`,
`explicit_reasoning_effort_wins_over_the_predefined_default`).

## Проверка

- `scripts/ci/check.sh` → `check.sh: all green`
- `cargo fmt --all -- --check` → exit 0
- `cargo fmt --manifest-path desktop/src-tauri/Cargo.toml -- --check` → exit 0
- `cargo clippy --workspace --all-targets -- -D warnings` → 0 warnings
- `cargo test --workspace` → 556 passed / 0 failed
- Специальные тесты проверяют правильное применение предопределенных reasoning уровней
- Ручное тестирование подтверждает улучшение качества/уменьшение расхода токенов
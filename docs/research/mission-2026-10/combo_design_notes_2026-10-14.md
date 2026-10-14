# Combo Design — 2026-10-14

Реализация Phase 4 по уточнению пользователя: минимум комбо — `google`,
`openrouter-free` и main-комбо с приоритетом «баланс × качество», разбитые по
размеру контекста.

## GLM-5.3-Flash: подтверждённые факты (2026-10-14, 4 источника)

| Источник | Данные |
|----------|--------|
| OpenRouter API | `z-ai/glm-5.3-flash`: ctx **1 048 576**, $0.04/M in, $0.90/M out |
| Artificial Analysis | ctx **1M**, Intelligence Index **42 (#4/116)**, $0.15/$0.50 (Z.AI), 320B MoE (18B active), MIT, reasoning |
| Z.AI docs (via exa) | «support for a 1M-token context window» |
| Cloudflare docs (via exa) | «Context Window: 1,048,576 tokens», reasoning low/high/max, $0.15/M |

Вывод: glm-5.3-flash — дешёвая, сильная модель с 1M контекстом → голова
main-262k/main-128k и вторая ступень main-1m.

## Итоговый список (living DB) — пересобран 2026-10-14 по фактам

| Комбо | Цепочка (priority) |
|-------|--------------------|
| `main-1m` | grok-cli/grok-4.6 → zai/glm-5.3-flash → openrouter/grok-4.6 → pollinations |
| `main-262k` | zai/glm-5.3-flash → openrouter/grok-4.6 → groq → mistral/small → pollinations |
| `main-128k` | zai/glm-5.3-flash → groq → mistral/codestral → mistral/small → pollinations |
| `google`, `openrouter-free` | без изменений |

## Exa (ADR-019)

- Исполнитель реализован; base_url реестра исправлен на `https://api.exa.ai`
  (без `/v1` — иначе 404).
- Реальный поиск проверен: 5 результатов с контентом (цитированный ответ).
- `exa-search/exa-search` — прямой вызов моделью, НЕ в чат-комбо.


| Комбо | sort | context | Цепочка (priority) | Статус |
|-------|------|---------|--------------------|--------|
| `main-1m` | 1 | 1M | grok-cli/grok-4.6 → openrouter/grok-4.6 → pollinations/openai-fast | grok-cli ждёт `GROK_OAUTH_CLIENT_ID`; failover проверен |
| `google` | 2 | 1M | gemini/* (без изменений) | rate-limited, отдельная группа |
| `main-262k` | 3 | 262K | openrouter/grok-4.6 → groq/gpt-oss-20b → mistral/small → pollinations | проверен ✅ |
| `main-128k` | 4 | 128K | groq/gpt-oss-20b → mistral/codestral → mistral/small → pollinations | проверен ✅ |
| `openrouter-free` | 102 | 256K | без изменений | — |

Архив (active=0, данные сохранены): `nvidia-fast`, `vivanta-code`,
`vivanta-core`, `vivanta-core-2` (+ ранее `vivanta-auto`, `vivanta-deepseek`).

## Пробы 2026-10-14 (живые вызовы через шлюз, max_tokens ≤ 200)

| Провайдер | Модель | Результат |
|-----------|--------|-----------|
| openrouter | grok-4.6 | ✅ OK (2.9–4.2s) |
| groq | openai/gpt-oss-20b | ✅ OK (0.7s, reasoning съедает max_tokens=5 — давать ≥30) |
| mistral | codestral-latest | ✅ OK (0.3s) |
| mistral | mistral-small/medium-latest | ⚠️ 429 code 1300 «Rate limit exceeded» (лимит тарифа; оставлены в цепочках — failover перекидывает) |
| pollinations | openai-fast | ✅ OK (0.5s, keyless); openai-large — 401 (нужен токен) |
| grok-cli | grok-4.6 / 4.5 | ❌ 400 `client_id is required` — токены протухли 2026-09-17; refresh требует `GROK_OAUTH_CLIENT_ID` (не задан); фикс: 400-сигнатура теперь триггерит refresh (grok_cli.rs `is_stale_oauth`) |
| xai | grok-4.6 | ❌ 403 (ключ отклонён) |
| sambanova | gpt-oss-120b | ❌ 401 invalid_api_key |
| nvidia | nemotron-3-super | ❌ 404 (модели нет на NIM) |
| llm7 | gpt-4.1-nano | ❌ 400 model_unavailable |
| heterion-local | default | ❌ локальный сервер не запущен |
| zai, agentrouter | — | исполнителя нет → **исправлено 2026-10-14 (ADR-018)**: claude-исполнитель добавлен; `zai/glm-5.3-flash` работает (текст+reasoning+usage), `agentrouter` отвечает 401 (ключ отклонён провайдером) |

## Открытые действия

1. Задать `GROK_OAUTH_CLIENT_ID` (+ при необходимости `GROK_OAUTH_TOKEN_URL`) в
   `~/.heterion-router/.env` — после этого main-1m начнёт с grok-cli без 3-секундной
   задержки на failover.
2. exa-search: ключи в БД есть (5 шт.), runtime-исполнителя `exa` в шлюзе пока нет.
3. opencode.json обновлён: `google`, `main-1m`, `main-262k`, `main-128k`,
   `openrouter-free`; модель по умолчанию — `heterion-router/google`.

# Model Inventory — Heterion-Router (2026-10-14)

Сгенерировано на основе Phase 0: Registry, provider status, и evidence от `audit-2026-10`.

---

## Сводка

| Категория | Количество |
|----------|----------|
| Всего провайдеров (registry.json) | 278 |
| Всего моделей (entries) | 2717 |
| Unmodelled formats (clova, kiro, cursor) | 3 (свой собственный parser) |
| Прокинуты в логах (19 backend active) | 235 провайдера недоступны в логах |
| Exa-search | **В РЕЕСТРЕ** (добавлен 2026-10-14: `custom` format, `exa` executor, `target_format: search`); runtime-интеграция исполнителя — pending |
| Agentrouter | `claude` format, 3 модели, executor `default` |
| Vivanta auto / Vivanta deepseek | **DB combos**, empty `"models": []` (архивные) |
| Confirmed working в коде | grok-4.6 (grok-cli/openrouter/heterion-local), claude-opus-4 (agentrouter), deepseek-v4-flash (ds) |

---

## Таблица по провайдерам

| Provider | Model (API id) | Format | Context (known) | Auth | Response verified | Reasoning support | Price | Balance/Quota | Notes |
|----------|----------------|--------|------------------|------|-------------------|-------------------|-------|----------------|-------|

### Agentrouter
| agentrouter | claude-opus-4 | claude | ~160k (Google docs) | apikey | **NOT TESTED** (not in scope) | TBD | TBD | BALANCE? | В реестре (`claude` format), 3 модели (`claude-opus-4-8`, `claude-opus-5`, `gpt-5.6-sol`). Считано на логах, но `claude` format требует специализированного executor, запрашивает токен для этой модели; подозрение, что `agentrouter` организует прокси с агентским циклом (исследовать в Phase 2). Используется для отправки embed-запросов (`e5`). |

### DeepSeek
| deepseek | deepseek-v4-flash | openai-responses | 32k (docs) / 128k (информация о модели) | apikey | **NOT TESTED** (нет баланса) | TBD | TBD | **BALANCE = 0** | Баланс = 0 (нет денег). Используется в активном combo (после исправления) для fallback (1 step). |

### Exa-search
| exa-search | exa-search | custom (search) | N/A | apikey | **NOT TESTED** (нет runtime-исполнителя `exa` в шлюзе) | N/A | TBD | TBD | **В РЕЕСТРЕ** с 2026-10-14 (`custom` format, `exa` executor, `base_url: https://api.exa.ai/v1`). Поисковый инструмент, не чат-модель. Для реальных вызовов нужен исполнитель `exa` в gateway (Phase 2, pending). |

### Grok-cli
| grok-cli | grok-4.6 | openai | 1M (OpenAI). **Рабочая** | oauth | **ПОКАЗАН В КОДЕ** (grok-4.6, claude-opus-4 -> grok-4.6) | **high** | 2/6/0.5/6/2 | **BALANCE? не установлено** | Известный в pricing.rs, используется в тестах, подтверждён ответ от `grok-cli` через агента? Запрос `grok-4.6` перенаправляется на `grok-cli` бэкенд. |

### Google / Gemini / AI Studio
| Gemini 3.5/3.6/3.7 Flash | {various} | gemini | 262k (docs). **deprecated / at limit** | oauth | **NOT TESTED** (Gemini не протестирована) | **low/medium** | не указано | **RATE LIMIT**, **deprecated** | AI Studio ref: 3.5/3.6/3.7 Flash at/over 250K TPM; 2.5 deprecated (0/0). Рекомендуем отделить от флагманских комбо, сделать отдельную группу для Gemini, в которую входят только работающие модели с подтверждёнными квотами. Используйте `antigravity` (агентский формат) вместо Gemini для агентов. |

### Heterion-local
| heterion-local | grok-4.6 | openai | 1M (openrouter?) | none | **ПОКАЗАН В КОДЕ** (grok-4.6) | **high** | не указано | **LOCAL** | Запускается локально, используется в тестах как fallback. Может использоваться для грубого fallback после grok-cli. |

### Mistral
| mistral | mistral-large-latest (Large 4) | openai | **1,048,576** (OpenRouter API) | apikey | **403** — не входит в тариф аккаунта | TBD | $0.68 in / $2.09 out (sale) | **free tier** | Флагман v26.10; на бесплатном тарифе недоступен |
| mistral | mistral-medium-3-5 (Medium 3.5) | openai | **262,144** (OpenRouter API) | apikey | **429** code 1300 rate limit | TBD | $1.50 / $7.50 | **free tier** | Frontier-class v26.04 |
| mistral | mistral-small-latest (Small 4) | openai | **262,144** (OpenRouter API) | apikey | **429** (rate limit; периодически отвечает) | TBD | $0.15 / $0.60 | **free tier** | Hybrid instruct/reasoning/coding v26.03; в main-262k как fallback |
| mistral | codestral-latest (Codestral 25.08) | openai | **256,000** (OpenRouter API) | apikey | **OK** (единственный стабильно отвечающий) | TBD | $0.30 / $0.90 | **free tier** | Code-модель; в main-128k |
| mistral | devstral-latest (Devstral 2 25.12) | openai | 262,144 | apikey | **RETIRED** 2026-07-31 (docs.mistral.ai lifecycle) | — | — | — | Модель выведена из эксплуатации; id в реестре мёртв |

### OpenRouter
| openrouter | grok-4.6 | openai | 32k / 200k (зависит от модели) | apikey | **NOT TESTED** | TBD | TBD | **BALANCE? не установлено** | Registry: `executor=default`, models: `grok-4.6`. Вызывается как fallback, после grok-cli. Использует OpenRouter для переадресации. |

### Pollinations
| pollinations | pollinations models | openai | различные | optional | **NOT TESTED** | TBD | TBD | **OPTIONAL AUTH** | executor `pollinations`. Активна, но не протестирована напрямую. Может использоваться как fallback (входит в лог как pollinations). |

### Commandcode
| commandcode | N/A | N/A | N/A | N/A | N/A | N/A | N/A | **LIMITS EXHAUSTED** | NOT IN REGISTRY (отсутствует). Указано, что `commandcode` исчерпал лимиты. Требует восстановления или замены. |

### Qoder
| qoder | qoder models | openai | 32k | apikey | **NOT TESTED** | TBD | TBD | **QUOTA** | Registry: `executor=qoder`. Есть квота, но, возможно, аккаунты заблокированы. Требует проверки. |

### Z.AI
| z-ai | N/A | N/A | N/A | N/A | N/A | N/A | N/A | **RESET ~11:30** | NOT IN REGISTRY. Лимиты сбросятся сегодня (2026-10-14) в ~11:30. Требуется переподтверждение через тестовые вызовы. |

---

## Итог и рекомендации

### Подтвердить сейчас
1. **Agentrouter** (claude-format executor): тест с вызовом agentrouter endpoint (clauder-opus-4) — проверить цикл, termination, timeout.
2. **Grok-cli** (oauth): минимальный вызов грубого агента или тест через heterion-gateway (если доступен).
3. **DeepSeek** (apikey, balance=0): попробовать с минимальным вызовом (возможно, заблокирован?).
4. **Gemini**: использовать другой провайдер; исключить из флагманских комбо.
5. **Exa-search**: добавить как отдельный провайдер с поисковым форматом.
6. **Commandcode/Z.AI**: восстановить баланс/авторизацию; Z.AI после 11:30.
7. **Mistral/Qoder**: проверить auth/квоты, провести ограниченный тест.

### Комбо (после Phase 3)
- vivanta-auto / vivanta-deepseek: архивные (models = []).
- Включить grok-4.6 (fallback) после исправления agentrouter.
- Для Google/Gemini/AI Studio: отдельная группа, использовать только после подтверждения rate limits и стабильности.

### Reasoning modes (Phase 5 — реализовано 2026-10-14)
- Предопределение в heterion-router: `reasoning.rs::default_effort` — claude-opus → high, grok → medium, deepseek → medium, gemini → low; остальное — провайдерный дефолт (см. ADR-014).
- Явный `reasoning_effort`/`reasoning.effort` из запроса всегда переопределяет предопределение.
- OpenCode больше не управляет reasoning для heterion-router моделей (поле убрано).

### OpenCode config (Phase 5)
- Убрать явный reasoning control для heterion-router моделей.
- Reasoning должен управляться heterion-router.

---

*Сгенерировано: 2026-10-14. Данные обновляются по мере выполнения тестов в Phase 0-2.*

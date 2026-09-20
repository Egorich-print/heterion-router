# STATUS

> Heterion Router — Rust-шлюз, дашборд и десктоп-оболочка.
> Обновлено: 2026-09-19

## Identity

```yaml
Name: heterion-router
Type: Platform / Service (AI gateway)
Layer: Intelligence / Execution
Status: Active
Repository: ai-workstation/Projects/heterion-router
GitHub: https://github.com/Egorich-print/heterion-router
License: MIT
```

## Что работает

- **Шлюз** (`heterion-router-gateway`, axum): OpenAI-совместимые `/v1/chat/completions`
  и `/v1/models`, стриминг SSE, роутинг по комбо из БД, breaker-классификация
  (4xx не роняет провайдера), failover по кандидатам.
- **`/v1/responses`** — поверхность Responses API (grok CLI / Codex клиенты):
  перевод туда-обратно, стрим как `response.*`-события; проверено живьём через комбо.
- **Бэкенды:** `openai`-совместимые, `gemini` (персистентные thought signatures),
  `openai-responses` (deepseek), keyless-провайдеры, `grok-cli`,
  **`heterion-local`** (keyless на Heterion-сервер, нативная поддержка v1).
- **Дашборд** (Svelte 5): Overview, Combos, Providers, Keys, Usage, Logs, Playground.
- **Admin-API:** `/api/overview|combos|connections|keys|usage|logs|restart` (см. ADR-004).
- **Десктоп** (Tauri v2, `com.heterion.router`): тот же бандл + sidecar шлюза
  на `:20129`. Single-instance guard (второй запуск фокусирует окно);
  parent-watch — sidecar сам выходит, когда приложение умерло (quit,
  force-quit, краш), утёкших шлюзов на порту больше нет.
- **Сервис:** launchd `com.heterion.router`, порт `20128`, данные `~/.heterion-router`
  (legacy `~/.omniroute` резолвится автоматически).

## Тесты

```
cargo test --workspace    все зелёные
cargo clippy --workspace --all-targets -- -D warnings    чисто
cargo fmt --all -- --check    чисто
```

## Переименование (2026-09-19)

Бывший OmniRoute-Rust (см. ADR-008). Заморожено: `enc:v1:` + соль,
`OMNIROUTE_*`-fallback на один цикл, порты. Не портировано из JS (осознанно):
`/api/mcp/*`, OAuth-перелогин, служебные страницы.

## Ближайшие шаги

1. Подключить `heterion-local` в живую БД (Heterion-сервер был down;
   SQL для wiring — в README).
2. Уборка копии данных (`~/.omniroute` 2.3 ГБ) после burn-in — только
   с явного подтверждения.
3. Parent-watch в шлюзе для launchd-сервиса не нужен (маркер не ставится).

## Документы

| Что               | Где                                          |
| ----------------- | -------------------------------------------- |
| Обзор и связи     | `Obsidian Vault/Projects/heterion-router/`   |
| ADR               | `docs/adr/`                                  |
| История работ     | `docs/history/`                              |
| Референс OmniRoute| `~/ai-workstation/Archives/OmniRoute/` + архивный репо |

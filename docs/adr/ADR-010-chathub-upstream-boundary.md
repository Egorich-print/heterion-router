# ADR-010: ChatHub as an upstream provider (2026-09-27)

- Статус: accepted
- Контекст: запрос на поиск endpoint'ов `app.chathub.gg` и их подключение к
  heterion-router.

## Контекст

Публичный frontend ChatHub и его bundle подтверждают внутренний протокол:

- `POST /api/v5/chat/completions` и `/stream` с cookie-based same-origin chat client;
- `x-app-id`, `x-device-id`, `x-model`, `x-client-time`;
- SSE с Vercel AI UI message stream и server-side tools
  `web_search`/`image_generation`; отдельный image-gen service имеет свой
  bearer path.

Это не опубликованный API contract. Anonymous probes в изолированном sandbox
дали `403 {"error":"Please sign-in to access this model"}` на chat POST и `401`
на stream GET. Официальные `llms.txt`/`agents.md` прямо не рекомендуют
programmatic access, а Terms of Service запрещают automated use, reverse
engineering и конкурирующие продукты.

## Решение

1. **Не добавлять ChatHub provider в heterion-router.** Не превращать
   undocumented endpoint в поддерживаемый runtime/registry contract, не принимать
   session cookies и не обходить 401/403.
2. **Сохранить найденный протокол как research evidence**, а не как runtime
   dependency: детали и live-probe результаты находятся в
   [`docs/research/CHATHUB-2026-09.md`](../research/CHATHUB-2026-09.md).
3. **Поддержать обратную интеграцию.** Captured ChatHub documentation
   (2026-09-27, re-verified 2026-09-29, browser-extension settings only)
   разрешает подключение OpenAI-compatible Custom Chatbot, поэтому
   существующий heterion-router endpoint может быть подключён к ChatHub из
   extension settings без добавления ChatHub как upstream.
4. **Не заявлять live tool support.** Статический `tool-*` contract найден, но
   реальный tool call нельзя проверить без авторизованной сессии; live
   verification остаётся заблокированной.

## Следствия

- В репозитории нет ChatHub credentials, cookies, token storage или adapter.
- Provider registry не получает `chathub` entry.
- Для ChatHub-as-provider нужен отдельный vendor-authorized API/partner access;
  только после него можно проектировать transport и fixtures.
- Публичный каталог `/api/models` нельзя считать доказательством доступности
  инференса: он возвращает 200 анонимно, а chat endpoint — 401/403.

## Проверка

Research и live evidence: `docs/research/CHATHUB-2026-09.md`.
Изменения не содержат секретов и не требуют provider-specific тестов в
`heterion-router`.

## Anonymous-session addendum 2026-09-29

Повторная попытка с fresh public cookie jar не изменила решение: `/`,
`/api/user` и `/api/app-config` не выдали `Set-Cookie`, jar остался пустым,
`/api/user` вернул `{"user":null}`. Корректный chat POST с
`tools:["web_search"]` снова дал `403`, stream GET — `401`; AI SSE и tool
events не получены. Нужен vendor-authorized API, а не guest session.

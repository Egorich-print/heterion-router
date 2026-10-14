# ChatHub integration investigation (2026-09-27)

## Scope

Проверено в изолированном sandbox и сохранено в репозитории:

`docs/research/chathub-evidence/`

Sandbox capture path (ephemeral): `/var/folders/wx/lw5nmhfn60zbfb8lc9fj7nnr0000gn/T/opencode/chathub-sandbox/independent`

Использованы только публичные `curl`/`wget`/`webfetch`/`websearch` и чтение
анонимных frontend bundles. Cookies, session tokens, Supabase keys,
login и любые способы обхода 401/403 не использовались.

## Acceptance criteria

| Критерий | Результат |
|---|---|
| Найти frontend-visible chat endpoint | ✅ `POST /api/v5/chat/completions` подтверждён bundle-константой; anonymous live probe закрыт `403` |
| Определить auth/transport | ✅ chat client: same-origin cookie-based fetch, `x-app-id`, `x-device-id`, `x-model`, `x-client-time`; `Authorization` в chat fetcher отсутствует (отдельный image-gen path использует bearer) |
| Получить реальный AI response | ⛔ anonymous `POST` → `403 {"error":"Please sign-in to access this model"}` |
| Получить live stream | ⛔ anonymous `GET .../stream` → `401 {"error":"Unauthorized"}` |
| Проверить guest/anonymous session | ⛔ fresh public cookie jar: 0 cookies и 0 `Set-Cookie`; `/api/user` → `{"user":null}` |
| Проверить tool invocation | ⛔ только статический client contract (`web_search`, `image_generation`, `tool-*` events); live вызов требует authenticated session |
| Добавить adapter в heterion-router | ⛔ отменено: нет публичного API/authorisation, vendor прямо не рекомендует programmatic access |

## Публично наблюдаемые endpoints

Клиентский bundle `use-chat-fFi4HfWS.js` содержит:

- `POST /api/v5/chat/completions` (также legacy `v3`/`v4` routes в manifest);
- `GET/POST/DELETE /api/v5/chat/completions/stream` для resume/stop/ack;
- body: `model`, `botId`, `conversationId`, `streamId`, `messages`, `fileIds`,
  `tools`, `threadId`, `chatMode`;
- stream headers: `x-chathub-stream-id`, `x-chathub-stream-state`,
  `x-vercel-ai-ui-message-stream: v1`.

Публичный `GET /api/models` вернул 33 модели: у всех есть `tools`, а у 24
дополнительно `vision` (у 9 только `tools`). Это каталог, не доказательство
доступности инференса без подписки/авторизации.

Публичные ancillary probes (не upstream candidates): `/api/app-config`,
`/api/models`, `/api/community-prompts`, `/api/ext-config`, `/api/image-models`,
`/api/video-models`, `/api/notice`, `/api/user` и `/health`. `/api/user`
возвращает `{"user":null}` анонимно; auth-gated routes отдельно возвращают
`401/403`.

## Fresh anonymous session attempt

Создан новый curl cookie jar и последовательно запрошены только публичные
`GET /`, `GET /api/user` и `GET /api/app-config`. Ответы не содержали
`Set-Cookie`, jar содержал 0 cookie rows, а `/api/user` вернул
`{"user":null}`. Затем тот же jar использован для корректного chat body
(`threadId == conversationId`, `tools:["web_search"]`); точный request сохранён
в `chathub-evidence/chat-post.request.json`. Chat POST вернул `403`, stream
GET — `401`; SSE и tool events не получены.

Это подтверждает отсутствие guest session, но не утверждает, что licensed
session не может работать.

## Anonymous live probes

| Request | Status | Наблюдение |
|---|---:|---|
| `GET https://app.chathub.gg/` | 200 | SSR app; `curl` и `wget` bodies совпали по SHA-256 |
| `GET /api/models` | 200 | 33 public model records |
| `POST /api/v5/chat/completions` с `tools:["web_search"]` | 403 | `Please sign-in to access this model` |
| `GET /api/v5/chat/completions/stream?...` | 401 | `Unauthorized` |
| `GET /api/debug-stream` | 200 | только публичный SSE heartbeat, не AI stream |

Полная команда и сохранённые ответы: `chathub-evidence/INDEPENDENT-RECON.md`,
`chathub-evidence/ANONYMOUS-SESSION.md`, `chathub-evidence/chat-response.json`,
`chathub-evidence/stream-response.json` и
`chathub-evidence/chat-post.request.json` и
`chathub-evidence/ANCILLARY.md`; полный список ancillary endpoints и статусов
сохранён в evidence directory.

## Tools

Frontend отправляет строковый массив `["web_search", "image_generation"]`.
Обрабатываются события `tool-input-*`, `tool-output-*`, `tool-approval-request`
и `tool-output-denied`. JSON Schema не приходит из браузера: названия mapped
на server-side providers. Поэтому adapter нельзя проектировать как обычный
OpenAI function-calling backend без авторизованного контракта и live trace.

## Vendor position

Официальные страницы:

- <https://chathub.gg/llms.txt> — ChatHub не является public model hosting API;
- <https://chathub.gg/agents.md> — не предлагать direct programmatic access;
- <https://chathub.gg/tos> — automated use, reverse engineering и competing
  products запрещены;
- <https://doc.chathub.gg/custom-chatbots> — captured via `webfetch` on
  2026-09-27 and re-verified 2026-09-29: ChatHub может подключить
  OpenAI-compatible custom endpoint (только browser-extension settings).

## Decision

ChatHub не добавляется в `heterion-router` как provider. Не существует
безопасного/разрешённого способа получить живую модель или tool call с
анонимной сессией, а credentials/обход авторизации не приемлемы.

Поддерживаемая интеграция — настроить существующий heterion-router
OpenAI-compatible endpoint как ChatHub Custom Chatbot. Для ChatHub-as-provider
нужен отдельный письменный vendor API/partner access; без него критерий
«реальный ответ и tool invocation» остаётся заблокированным доказательствами
`403/401` выше.

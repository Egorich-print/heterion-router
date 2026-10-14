# ChatHub independent sandbox verification

Date: 2026-09-27
Scope: public, anonymous requests only. No login, cookies, bearer tokens, publishable keys, or access-control bypass.

## Reproducible commands

```sh
curl -sS -L --max-time 20 -D headers -o landing.html https://app.chathub.gg/
wget -q -T 20 -O landing.wget.html https://app.chathub.gg/
curl -sS --max-time 20 -D headers -o models.json https://app.chathub.gg/api/models
curl -sS --max-time 20 -D headers -o chat.json -X POST https://app.chathub.gg/api/v5/chat/completions \
  -H 'content-type: application/json' -H 'x-app-id: web' \
  -H 'x-device-id: 00000000-0000-4000-8000-000000000000' \
  -H 'x-model: openai/gpt-6-astra' -H 'x-client-time: 2026-09-27T00:00:00.000Z' \
  --data '{"model":"openai/gpt-6-astra","botId":"cloud-gpt-6-astra","conversationId":"00000000-0000-4000-8000-000000000001","streamId":"00000000-0000-4000-8000-000000000002","messages":[{"id":"00000000-0000-4000-8000-000000000003","role":"user","parts":[{"type":"text","text":"hello"}]}],"fileIds":[],"tools":["web_search"],"threadId":"00000000-0000-4000-8000-000000000001","chatMode":"single"}'
curl -sS --max-time 20 -D headers -o stream.json 'https://app.chathub.gg/api/v5/chat/completions/stream?botId=cloud-gpt-6-astra&conversationId=00000000-0000-4000-8000-000000000001&streamId=00000000-0000-4000-8000-000000000002'
```

## Observed

| Request | Status | Body/evidence |
|---|---:|---|
| `GET /` | 200 | Public SSR page; curl and wget bodies have identical SHA-256 |
| `GET /api/models` | 200 | 33 models; all expose `tools`, 24 also expose `vision` |
| `POST /api/v5/chat/completions` with `tools:["web_search"]` | 403 | `{"error":"Please sign-in to access this model"}` |
| `GET /api/v5/chat/completions/stream?...` | 401 | `{"error":"Unauthorized"}` |
| `GET /api/debug-stream` | 200 | Public SSE heartbeat, `event: message` / numeric `data:` frames |

A fresh anonymous cookie-jar handshake is recorded in
`ANONYMOUS-SESSION.md`: 0 cookies, no `Set-Cookie`, and
`/api/user` → `{"user":null}` before the same 403/401 chat results.

## Static protocol evidence

Fetched public bundles `use-chat-fFi4HfWS.js`, `server-api-D2Lj5daZ.js`, and `manifest-75119565.js` from `https://app.chathub.gg/assets/`.

- Shared client: `baseURL: "/api"`, headers `x-app-id: web`, persisted `x-device-id`.
- Chat route: `POST /api/v5/chat/completions`; resume/control route: `/api/v5/chat/completions/stream`.
- Body keys: `model`, `botId`, `conversationId`, `streamId`, `messages`, `fileIds`, `tools`, `threadId`, `chatMode`.
- Chat headers include `x-model` and `x-client-time`; no `Authorization` header is present in the chat fetcher. The separate image-generation service has a bearer-token path.
- Tools are server-side names `web_search` and `image_generation`; client parses `tool-input-*`, `tool-output-*`, and approval events.
- This is a private/undocumented client protocol, not a published API contract.

## Decision

No real AI response or tool invocation can be verified without an authenticated ChatHub session. Authenticating, sharing a session, or bypassing the 401/403 boundary is out of scope and would conflict with the vendor's public agent guidance and Terms. No ChatHub provider adapter should be added to heterion-router.

The supported integration direction is the reverse: configure heterion-router's OpenAI-compatible endpoint as a ChatHub **Custom Chatbot** (documented for browser-extension configuration).

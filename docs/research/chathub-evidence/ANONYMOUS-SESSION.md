# Fresh anonymous session verification

Date: 2026-09-29 (fresh guest-session rerun; initial public capture 2026-09-27)
No login, signup, CAPTCHA solving, token forging, or access-control bypass.

A fresh curl cookie jar was used for public requests only:

```sh
curl -sS -L -c guest.jar -b guest.jar -D root.headers -o root.html https://app.chathub.gg/
curl -sS -c guest.jar -b guest.jar -D user.headers -o user.body https://app.chathub.gg/api/user
curl -sS -c guest.jar -b guest.jar -D config.headers -o config.body https://app.chathub.gg/api/app-config
```

Results:

- `Set-Cookie`: absent from all public responses.
- Cookie jar rows: `0`.
- `GET /api/user`: `{"user":null}`.
- `GET /api/app-config`: `200`, normal public config.

The fresh jar was then used for the documented chat request with fresh UUIDs,
`threadId == conversationId`, and `tools:["web_search"]`. The exact request is
saved as `chat-post.request.json` (with `chat-post.ids.json`); its response is
`chat-post.response.json`:

```sh
curl -sS -b guest.jar -c guest.jar --data-binary @chat-post.request.json \
  -H 'content-type: application/json' -H 'x-app-id: web' \
  -H 'x-device-id: 00000000-0000-4000-8000-000000000010' \
  -H 'x-model: openai/gpt-6-astra' -H 'x-client-time: 2026-09-29T00:00:00.000Z' \
  -X POST https://app.chathub.gg/api/v5/chat/completions
```

- `POST /api/v5/chat/completions` → `403 {"error":"Please sign-in to access this model"}`.
- `GET /api/v5/chat/completions/stream?...` → `401 {"error":"Unauthorized"}`.
- No SSE, `text-*` events, or `tool-*` events were returned.

Conclusion: the public site has no guest session or anonymous model quota. A
licensed/authenticated vendor session would be required for live inference;
this attempt did not create or reuse one.

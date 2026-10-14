# ChatHub capture evidence

Sanitized public/anonymous capture from 2026-09-27, with a fresh guest-session
rerun on 2026-09-29. No cookies, session tokens, Supabase keys or provider
credentials are included.

- `INDEPENDENT-RECON.md` — commands and endpoint results.
- `ANONYMOUS-SESSION.md` — fresh guest cookie-jar handshake and retry.
- `models.json` — public model catalog snapshot.
- `chat-response.json` / `stream-response.json` — anonymous error bodies.
- `chat-post.request.json` — the exact anonymous request body used for the
  corrected `threadId == conversationId` probe; it contains only synthetic
  UUIDs and the test word `hello`.
- `chat-post.response.json` / `chat-post.ids.json` — the corresponding 403
  response and synthetic identifiers.

These files document why ChatHub is not wired as an undocumented upstream;
see `docs/adr/ADR-010-chathub-upstream-boundary.md`.

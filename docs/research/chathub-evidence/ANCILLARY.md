# ChatHub ancillary capture summary

Public/anonymous metadata probes captured during the 2026-09-27 sandbox run.
No credentials or cookies were used. These endpoints are catalog/config only,
not inference providers.

| Endpoint | Status | Result |
|---|---:|---|
| `GET /api/app-config` | 200 | version/IAP config |
| `GET /api/models` | 200 | 33 public model records |
| `GET /api/user` | 200 | `{"user":null}` |
| `GET /api/user/usage` | 200 | `{"usage":null}` |
| `GET /api/user/prompts` | 200 | `{"prompts":[]}` |
| `GET /api/user-flags` | 200 | `{}` |
| `GET /api/community-prompts` | 200 | public prompt catalog |
| `GET /api/ext-config` | 200 | extension config |
| `GET /api/image-models` | 200 | public image catalog |
| `GET /api/video-models` | 200 | public video catalog |
| `GET /api/notice` | 200 | `null` |
| `GET /health` | 200 | `{"ok":true}` |
| `GET /api/user/videos` | 403 | sign-in required |
| `GET /api/notebook/entry` | 401 | unauthorized |

The full chat/stream results are in `chat-response.json`,
`stream-response.json` and `ANONYMOUS-SESSION.md`.

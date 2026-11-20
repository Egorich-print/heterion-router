# Antigravity / Google AI Code (Cloud Code) — protocol notes

Measured 2026-11-21 against the Antigravity IDE 2.32.1 installed at
`/Users/egorich/Applications/Antigravity.app`.

Everything below was observed on the wire. Items that could not be confirmed
are listed explicitly under **Not determined** — nothing here is guessed.

## 1. What the Antigravity language server talks to

The IDE ships a Go binary, the *language server*, and launches it as:

```
language_server --standalone --override_ide_name antigravity --subclient_type hub \
  --override_ide_version 2.32.1 --override_user_agent_name antigravity \
  --http_server_port 0 --https_server_port 0 --csrf_token <uuid> \
  --app_data_dir antigravity \
  --api_server_url https://generativelanguage.googleapis.com \
  --cloud_code_endpoint https://daily-cloudcode-pa.googleapis.com
```

* `--cloud_code_endpoint` is the **Cloud Code "PA"** host. It can also be set
  through the environment (`CLOUD_CODE_URL`, `BAICODE_ENDPOINT_URL`).
* The IDE itself is started with `--api_server_url`, the *AI Code* host.
* The language server also exposes a local Connect-RPC service
  `exa.language_server_pb.LanguageServerService` on the ports it writes to
  `~/.gemini/antigravity/daemon/hub_<pid>.json` (HTTP and HTTPS, plus an
  `x-codeium-csrf-token` header). `GetAvailableModels`, `GetAuthStatus`,
  `GetLoadCodeAssist` and `SetCloudCodeURL` live there.

Hosts seen in the binary: `daily-cloudcode-pa.googleapis.com` (the IDE's
endpoint), `cloudcode-pa.googleapis.com` (production),
`generativelanguage.googleapis.com` (the API server), `aicode.googleapis.com`,
`oauth2.googleapis.com`, `sts.googleapis.com`.

## 2. Credential

**Location.** macOS keychain, generic password:

```
security find-generic-password -s gemini -a antigravity
  "svce" = "gemini"
  "acct" = "antigravity"
```

The value is `<17-char prefix>:<base64 JSON>`. The JSON document is:

```json
{
  "token": {"access_token": "…", "refresh_token": "…", "token_type": "Bearer",
            "expiry": "…", "expires_in": 3599},
  "auth_method": "consumer",
  "id_token_email": "…",
  "writer_id": "…"
}
```

The stored access token **expires** (the one on this machine expired on
2026-11-16) — the IDE renews it in memory and does **not** write the renewed
token back to the keychain, so the keychain copy is never fresh by itself.

**Renewal.** The IDE renews the bearer at the standard Google token endpoint:

```
POST https://oauth2.googleapis.com/token
Content-Type: application/x-www-form-urlencoded

grant_type=refresh_token&refresh_token=…&client_id=…&client_secret=…
```

The OAuth client is the Antigravity one:

* client id — `1071006060591-tmhssin2h21lcre235vtolojh4g403ep.apps.googleusercontent.com`
* client secret — a 35-character `GOCSPX-…` value that the IDE embeds in
  plaintext in `language_server`, immediately after the string
  `/v1beta/models/%s:generateContent`.

The router therefore reads the client secret from the installed IDE at
runtime (`crates/heterion-router-gateway/src/antigravity.rs`,
`read_client_secret`) instead of committing it. A refresh with the measured
pair returned **200** and an access token valid for 3599 s.

The keychain entry is also the *only* credential on this machine: nothing
under `~/.gemini` holds a usable token.

## 3. Headers

Every Cloud Code call carries:

```
Authorization: Bearer <google oauth access token>
Content-Type: application/json
User-Agent: antigravity/hub/2.32.1 (aidev_client; os_type=darwin; arch=arm64;
             cl=1021296516; datacollection_opt_in=false)
Accept-Encoding: gzip
```

* There is **no** `X-Goog-Api-Client` and **no** `Client-Metadata` header.
  The client metadata travels in the JSON body (`metadata.ideType`).
* The platform answers with `x-cloudaicompanion-trace-id`.
* A request without a bearer is answered `401 UNAUTHENTICATED`
  (`CREDENTIALS_MISSING`); a request with an expired one is answered
  `401 … invalid authentication credentials`.

## 4. Endpoints (all confirmed live, `POST`, JSON)

| Endpoint | Body | Answer |
|---|---|---|
| `/v1internal:loadCodeAssist` | `{"metadata":{"ideType":"ANTIGRAVITY"}}` | tier + project |
| `/v1internal:fetchAvailableModels` | `{"project":"aicode-consumers"}` | model catalogue |
| `/v1internal:listExperiments` | `{"project":"aicode-consumers","metadata":{"updateChannel":"stable"}}` | experiment ids |
| `/v1internal:fetchUserInfo` | `{"project":"aicode-consumers"}` | `{"userSettings":{},"regionCode":"ES"}` |
| `/v1internal:tabChat?alt=sse` | `{"project":…,"request":{…}}` | SSE chat answer |
| `/v1internal:generateContent` | `{"project":…,"requestId":…,"request":{…}}` | one answer |
| `/v1internal:streamGenerateContent` | same request | SSE answer |

`loadCodeAssist` answers with the tier the account is on:

```json
{"currentTier":{"id":"standard-tier","name":"Antigravity","usesGcpTos":true},
 "allowedTiers":[{"id":"standard-tier","isDefault":true}],
 "cloudaicompanionProject":"aicode-consumers",
 "paidTier":{"id":"g1-pro-tier","name":"Google AI Pro"}}
```

`ineligibleTiers` names `free-tier` with reason `UNSUPPORTED_LOCATION` — the
free tier is not available here, the account is on the Pro tier.

## 5. Request body

The Cloud Code envelope wraps the actual model request:

```json
{
  "project": "aicode-consumers",
  "requestId": "<uuid>",
  "request": { "model": "…", "contents": [ … ], "tools": [ … ],
               "systemInstruction": { … }, "generationConfig": { … } }
}
```

The inner `request` is an AI Platform *master* `GenerateContentRequest`
(`google.cloud.aiplatform.master.GenerateContentRequest`), i.e. the Gemini
request body with `model` as a field instead of a URL segment. The reply is
the mirror image:

```json
{"response": {"candidates": [ … ], "usageMetadata": { … }},
 "traceId": "…", "consumedCredits": …, "remainingCredits": …}
```

The `response` wrapper is why the shared translator can be reused as-is: the
Gemini→OpenAI converter unwraps a `response` field when it sees one.

The `tabChat` variant carries `exa.api_server_pb.GetChatMessageRequest`
instead, whose fields are `metadata`, `prompt`, `chatMessagePrompts`,
`useInternalChatModel`, `internalChatModel`, `customModelInfoOverride`,
`requestType`, `configuration`, `experimentConfig`, `tools`, `toolChoice`,
`systemPromptCacheOptions`, `chatModelName`, `trajectoryReference`,
`promptId`, `providerSource`.

## 6. Catalogue (measured 2026-11-21 for this account)

`POST /v1internal:fetchAvailableModels` answers with 22 model ids. The ones
worth publishing (the agent group plus the tier anchors) with their display
names and the internal model enum the platform uses:

| id | display name | internal enum |
|---|---|---|
| `gemini-3.8-flash-high` | Gemini 3.8 Flash (High) | `MODEL_PLACEHOLDER_M318` |
| `gemini-3.8-flash-medium` | Gemini 3.8 Flash (Medium) | `MODEL_PLACEHOLDER_M319` |
| `gemini-3.8-flash-low` | Gemini 3.8 Flash (Low) | `MODEL_PLACEHOLDER_M320` |
| `gemini-3.8-flash-tiered` | — | `MODEL_PLACEHOLDER_M322` |
| `gemini-3.5-flash-lite` | Gemini 3.5 Flash Lite | — |
| `gemini-3.1-flash-lite` | Gemini 3.1 Flash Lite | — |
| `claude-sonnet-4-6` | Claude Sonnet 4.6 | — |
| `claude-opus-4-6-thinking` | Claude Opus 4.6 (Thinking) | — |
| `gpt-oss-120b-medium` | GPT-OSS 120B (Medium) | — |

Others the account also sees: `gemini-3-flash`, `gemini-2.5-pro`,
`gemini-2.5-flash`, `gemini-2.5-flash-lite`, `gemini-2.5-flash-thinking`,
`gemini-3.1-flash-image`, `gemini-4-carbon-disabled`,
`gemini-4-argon-disabled`, `gemini-4-hydrogen-disabled`,
`gemini-4-neon-disabled`, `gemini-4-hydrogen-sonar`,
`tab_flash_lite_preview`, `tab_jump_flash_lite_preview`.

Answer metadata worth knowing:

```json
{"defaultAgentModelId": "gemini-3.8-flash-high",
 "agentModelSorts": [{"displayName":"Recommended","groups":[{"modelIds":[…]}]}],
 "commandModelIds": ["gemini-3-flash"],
 "webSearchModelIds": ["gemini-3.5-flash-lite"],
 "tieredModelIds": {"flashLite": ["gemini-3.5-flash-lite"],
                    "flash":    ["gemini-3.8-flash-tiered"],
                    "pro":      ["gemini-3.8-flash-tiered"]},
 "groupedModels": {"gemini-3.8-flash": {"displayName":"Gemini 3.8 Flash",
                     "supportedThinkingLevels":[{"thinkingSuffix":"low"},…]}}}
```

Per model the catalogue also carries `maxTokens` (1 048 576),
`maxOutputTokens` (65 536), `supportsImages`, `supportsThinking`,
`minThinkingBudget`, `supportedMimeTypes` and `quotaInfo`
(`remainingFraction`, `resetTime`).

**Tier naming.** The IDE derives the tier from the model id string: an id
containing `flash-lite` becomes the `flash_lite` tier, an id containing
`flash` becomes `flash`, everything else becomes `pro`. The published ids
follow that rule on purpose (`gemini-3.5-flash-lite` → flash_lite,
`gemini-3.8-flash-tiered` → flash, `claude-sonnet-4-6` → pro).

## 7. Model discovery in the IDE

`model_discovery.go` resolves the catalogue in four tiers and logs which one
answered:

1. **Tier 1** — inline `models.list` (admin settings),
2. **Tier 2** — remote catalogue (`fetchAvailableModels`),
3. **Tier 3** — gateway discovery at the configured gateway URL,
4. **Tier 4** — built-in fallback catalogue.

With `AGY_LLM_GATEWAY_URL` set the IDE logs
`[ModelConfig] Resolving models from Tier 3 (Gateway discovery at …,
wireProtocol: …)` and **replaces** its catalogue with the gateway's — which
is why the native models disappear from the picker. The router therefore has
to serve them itself.

## 8. How the protocol was measured

* The language server binary was pointed at a local logging reverse proxy
  (`--cloud_code_endpoint http://127.0.0.1:<port>`), which forwarded to
  `daily-cloudcode-pa.googleapis.com` and logged method, path, headers
  (authorization redacted) and body.
* The OAuth refresh was captured with a MITM proxy in front of
  `oauth2.googleapis.com`; the captured secret was written to a file outside
  the repository and never printed.
* Model discovery, `loadCodeAssist`, `fetchAvailableModels`,
  `listExperiments` and `fetchUserInfo` were replayed directly with the
  user's own credential and answered 200.

## Not determined

* **The exact body the IDE sends for a chat turn.** The IDE sends the chat
  request with `Transfer-Encoding: chunked`; the capture that would have
  shown the body was taken before the proxy learned chunked bodies, and the
  RPC that triggered it (`tabChat`) could not be reproduced on demand.
* **Which model reference the inference endpoint accepts.** A hand-built
  request with `model` set to the catalogue id (`gemini-3.8-flash-high`) or
  to the internal enum (`MODEL_PLACEHOLDER_M318`) answers
  `404 "Requested entity was not found"` on the daily host, while the
  production host answers `429 RESOURCE_EXHAUSTED` for the same body — i.e.
  the entity is found there and the account's production quota is empty.
  The daily host rejects `userAgent` / `requestType` as unknown fields, so
  its inner request type is an older revision than the production one.
* **The streaming event shape** of `streamGenerateContent`. Only the
  endpoint, the request envelope and the `response` wrapper are confirmed;
  the SSE frames themselves were not captured.
* **The `aicode.googleapis.com` inference path.** The host accepts the
  token and answers `400 "Request contains an invalid argument"` for a
  Gemini-shaped body, and it rejects a `model` field in the body
  ("oneof field 'inference' is already set"), so the request there differs
  from the Cloud Code one. Not pursued further.

## Consequence for the router

The router's `antigravity` executor
(`crates/heterion-router-gateway/src/antigravity.rs`) implements the Cloud
Code envelope, the shared Gemini request/response translation, streaming over
SSE, and the router's error contract (429 → model-scoped cooldown with
`Retry-After`, 402/403 → provider quota event, 4xx → no breaker trip). The
credential is read at runtime from the keychain item above; the OAuth client
secret is read from the installed IDE. The inference endpoint is called with
the catalogue id as `model`; if the platform answers `404`, that is reported
as an upstream refusal rather than silently retried.

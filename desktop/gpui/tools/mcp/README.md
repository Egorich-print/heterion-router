# Heterion Router — MCP server

A stdio [MCP](https://modelcontextprotocol.io) server that lets an AI agent
inspect and drive the router's native GPUI shell.

The native dashboard is a GPUI window: an agent cannot see whether it rendered,
what it shows, or which gateway it talks to. This server closes that gap —
it speaks the router's admin HTTP API and it can start, stop, inspect and
screenshot the app.

* **Node standard library only.** No dependencies, no `npm install`, no build
  step, no network access at build time.
* **macOS only** for the screenshot tool (`screencapture`); everything else
  runs anywhere Node 18+ runs.

```
desktop/gpui/tools/mcp/
├── server.js      # the server
├── package.json   # name, version, type: module, start script — no dependencies
├── README.md
└── .gitignore     # runtime/ and shots/ are generated
```

## Registering it with a client

The server speaks MCP over stdio: one JSON-RPC message per line on stdout,
nothing else. Point the client at the file — there is nothing to build first.

```jsonc
{
  "mcpServers": {
    "heterion-router": {
      "command": "node",
      "args": ["/Volumes/Netac_SSD/ai-workstation/Projects/heterion-router/desktop/gpui/tools/mcp/server.js"]
    }
  }
}
```

With environment variables (see below):

```jsonc
{
  "mcpServers": {
    "heterion-router": {
      "command": "node",
      "args": ["<repo>/desktop/gpui/tools/mcp/server.js"],
      "env": {
        "HETERION_ROUTER_BASE_URL": "http://127.0.0.1:20128",
        "HETERION_ROUTER_KEY": "sk-…"
      }
    }
  }
}
```

Run it by hand to check that it answers:

```sh
node desktop/gpui/tools/mcp/server.js
```

It waits for JSON-RPC on stdin. A quick smoke test:

```sh
printf '%s\n%s\n' \
  '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05"}}' \
  '{"jsonrpc":"2.0","id":2,"method":"tools/list"}' \
  | node desktop/gpui/tools/mcp/server.js
```

## Environment variables

| Variable | Meaning | Default |
|---|---|---|
| `HETERION_ROUTER_BASE_URL` | Gateway base URL. | `http://127.0.0.1:20128` |
| `HETERION_ROUTER_PORT` | Used to build the base URL when the URL is not set. | — |
| `HETERION_ROUTER_KEY` | The operator bearer. **Takes precedence** over the key file. | — |
| `HETERION_ROUTER_DATA_DIR` | The router's data directory, where the key file lives. | `~/.heterion-router`, else the legacy `~/.omniroute` |

The bearer is a key with the `manage` scope — the same one the dashboard signs
in with. It is read from `<data dir>/desktop/operator-key.json` (the file the
shell writes) when `HETERION_ROUTER_KEY` is not set.

**The key is never printed** — not in a result, not in an error message. If it
ever shows up in a payload, it is replaced with `[redacted]`.

Port note: the launchd service listens on `20128`, a bundled sidecar on `20129`.
The default is `20128`; point `HETERION_ROUTER_BASE_URL` at the other one when
you want the sidecar.

## Tools

| Tool | What it does |
|---|---|
| `router_health` | `GET /healthz` — reachability and the gateway's backend name. No bearer needed. When the configured base does not answer, it also probes `20128`/`20129` and reports which port answers. |
| `router_request` | Call any admin endpoint. Parameters: `method` (`GET`/`POST`/`PATCH`), `path`, optional `body` (a JSON object). Sends the bearer, returns the status and the parsed JSON, truncated to 200 KB. |
| `router_endpoints` | The static catalogue of admin endpoints with a one-line description and the shape of each body. |
| `app_launch` | Start the native shell (`desktop/gpui/target/debug/heterion-router-desktop`) with its output in `runtime/app.log`; returns the pid and the log path. Refuses to start a second copy. |
| `app_stop` | Stop the app `app_launch` started — by pid file, and only if the pid really is the router shell. |
| `app_status` | Is the app running, does the gateway answer, and the last 30 lines of the app log. |
| `app_screenshot` | Capture the screen to `shots/<name>.png` and return the absolute path, so the agent can read the image. Default name `shot`. |

### The admin API

`router_request` speaks the same contract as `desktop/gpui/src/api.rs`:

```
GET   /healthz
GET   /api/overview
GET   /api/combos
PATCH /api/combos/{id}
GET   /api/connections
PATCH /api/connections/{id}
GET   /api/keys
POST  /api/keys
POST  /api/keys/{id}/revoke
POST  /api/keys/{id}/restore
GET   /api/usage?days=&limit=
GET   /api/logs?limit=
GET   /api/logs/app?limit=
POST  /api/restart
```

`router_endpoints` carries the same list with descriptions; call it when you
are not sure which endpoint to use.

## Limitations

* **Full-screen capture only.** `app_screenshot` grabs the whole display with
  `screencapture -x -o` — it cannot target the app's window. macOS asks the
  terminal that runs this server for Screen Recording permission
  (System Settings → Privacy & Security → Screen Recording); without it the
  capture fails and the error says so.
* **macOS only** for `app_screenshot` and for the app binary path
  (`target/debug/heterion-router-desktop`).
* **The app must be built first.** `app_launch` does not build anything:

  ```sh
  cargo build --manifest-path desktop/gpui/Cargo.toml
  ```

  Without the binary, `app_launch` returns an `app_not_built` error.
* **The screen is not the window.** A screenshot shows whatever is on the
  display, including other applications and any notification.
* **`app_stop` kills by pid file.** It only ever signals the pid this server
  recorded, and it refuses when the pid is not the router shell. It does not
  manage a launchd-installed or manually started copy.
* **The gateway may refuse the bearer.** `POST /api/restart` answers `409`
  when the gateway is not supervised by launchd, and the admin endpoints need a
  key with the `manage` scope — a plain API key gets `403`.
* **Screenshots and the app log are written under this directory**
  (`shots/`, `runtime/`) and are gitignored.

#!/usr/bin/env node
/**
 * Heterion Router — MCP stdio server.
 *
 * A native GPUI window is invisible to an agent: nothing tells it whether the
 * shell rendered, what the dashboard shows, or which gateway the shell talks
 * to. This server closes that gap. It speaks MCP over stdio (JSON-RPC 2.0,
 * one message per line) and exposes:
 *
 *   - the router's admin HTTP API (the same contract `desktop/gpui/src/api.rs`
 *     speaks), and
 *   - the native app itself: launch it, stop it, ask whether it is alive, and
 *     screenshot the screen so the agent can look at it.
 *
 * Rules this file never breaks:
 *   - stdout carries JSON-RPC messages and nothing else. Diagnostics go to
 *     stderr — an agent dies if stdout carries noise.
 *   - the operator key is never printed, not even in an error message.
 *   - nothing is written outside this directory.
 *   - untrusted input is never interpolated into a shell command: every
 *     child process gets an argument array.
 *
 * Node standard library only — no dependencies, no build step.
 */

import { execFileSync, spawn, spawnSync } from "node:child_process";
import fs from "node:fs";
import http from "node:http";
import https from "node:https";
import net from "node:net";
import os from "node:os";
import path from "node:path";
import readline from "node:readline";
import { fileURLToPath } from "node:url";

// --- paths -----------------------------------------------------------------

const HERE = path.dirname(fileURLToPath(import.meta.url));
/**
 * `desktop/gpui` — the crate root that owns the app binary.
 *
 * The default is the parent of this file's directory (the server lives in
 * `desktop/gpui/tools/mcp/`). `HETERION_ROUTER_DESKTOP_DIR` overrides it, so
 * a copy of this server that ships outside the repository — a bundled
 * plugin, for instance — still finds the application.
 */
const GPUI_ROOT = (() => {
  const override = (process.env.HETERION_ROUTER_DESKTOP_DIR || "").trim();
  return override ? path.resolve(override) : path.resolve(HERE, "..", "..");
})();
/** Where screenshots land. */
const SHOTS_DIR = path.join(HERE, "shots");
/** Where this server keeps the pid file and the app log. */
const RUNTIME_DIR = path.join(HERE, "runtime");
const PID_FILE = path.join(RUNTIME_DIR, "app.pid");
const LOG_FILE = path.join(RUNTIME_DIR, "app.log");
/** The debug build of the native shell. */
const APP_BINARY = path.join(GPUI_ROOT, "target", "debug", "heterion-router-desktop");

/** The app claims this port to keep a second copy from starting. */
const INSTANCE_LOCK_PORT = 20130;

/** Largest payload this server hands back to an agent. */
const MAX_OUTPUT_BYTES = 200 * 1024;
/** How long a single admin request may take. */
const HTTP_TIMEOUT_MS = 20_000;

const SERVER_NAME = "heterion-router-mcp";
const SERVER_VERSION = "0.1.0";

/** Protocol revisions this server can speak. */
const SUPPORTED_PROTOCOL_VERSIONS = ["2024-11-05", "2025-03-26", "2025-06-18"];
const DEFAULT_PROTOCOL_VERSION = "2025-06-18";

// --- stdout hygiene --------------------------------------------------------
// Anything that ends up on stdout must be a JSON-RPC message. Console output
// is redirected to stderr so a stray log line can never corrupt the stream.

for (const level of ["log", "info", "warn", "error", "debug"]) {
  console[level] = (...args) => {
    process.stderr.write(`${args.map((arg) => String(arg)).join(" ")}\n`);
  };
}

function log(message) {
  process.stderr.write(`[${SERVER_NAME}] ${message}\n`);
}

// --- configuration ---------------------------------------------------------

/** The gateway base URL, without a trailing slash. */
function resolveBaseUrl() {
  const explicit = (process.env.HETERION_ROUTER_BASE_URL || "").trim();
  if (explicit) return explicit.replace(/\/+$/, "");
  // `HETERION_ROUTER_*` first, the legacy `OMNIROUTE_*` name second — the
  // same precedence the gateway itself uses.
  for (const name of ["HETERION_ROUTER_PORT", "OMNIROUTE_RUST_PORT"]) {
    const port = (process.env[name] || "").trim();
    if (/^\d{1,5}$/.test(port)) return `http://127.0.0.1:${port}`;
  }
  // The launchd service listens here; a debug shell talks to the same one.
  return "http://127.0.0.1:20128";
}

/**
 * The router's data directory, mirroring `desktop/gpui/src/secret.rs`:
 * the explicit environment wins, then `~/.heterion-router`, then the legacy
 * `~/.omniroute`.
 */
function resolveDataDir() {
  for (const key of ["HETERION_ROUTER_DATA_DIR", "DATA_DIR"]) {
    const value = (process.env[key] || "").trim();
    if (value) return path.resolve(value);
  }
  const home = os.homedir();
  const modern = path.join(home, ".heterion-router");
  if (fs.existsSync(modern)) return modern;
  const legacy = path.join(home, ".omniroute");
  if (fs.existsSync(legacy)) return legacy;
  return modern;
}

/** The file the native shell writes the operator key to. */
function operatorKeyPath() {
  return path.join(resolveDataDir(), "desktop", "operator-key.json");
}

/**
 * The operator key: `HETERION_ROUTER_KEY` first, then the key file.
 *
 * The file is named `operator-key.json` but the shell writes the bare key
 * into it, so a JSON object is only accepted when it really parses.
 */
function loadOperatorKey() {
  const fromEnv = (process.env.HETERION_ROUTER_KEY || "").trim();
  if (fromEnv) return { key: fromEnv, source: "HETERION_ROUTER_KEY" };

  const file = operatorKeyPath();
  let raw;
  try {
    raw = fs.readFileSync(file, "utf8").trim();
  } catch {
    return { key: null, source: null };
  }
  if (!raw) return { key: null, source: null };

  if (raw.startsWith("{")) {
    try {
      const parsed = JSON.parse(raw);
      const candidate = parsed.key ?? parsed.token ?? parsed.secret;
      if (typeof candidate === "string" && candidate.trim()) {
        return { key: candidate.trim(), source: file };
      }
    } catch {
      // Not JSON after all — fall through to the bare key.
    }
  }
  return { key: raw, source: file };
}

/** Replace every occurrence of the operator key with a placeholder. */
function redact(text, key) {
  let out = typeof text === "string" ? text : String(text ?? "");
  if (key) out = out.split(key).join("[redacted]");
  return out;
}

// --- HTTP ------------------------------------------------------------------

/**
 * One HTTP request. Resolves with `{ok:true, status, text}` or
 * `{ok:false, error:{kind, message, code}}` — it never rejects and never
 * throws, so a dead gateway cannot take the server down.
 */
function httpRequest(method, urlString, { headers = {}, body = null, timeoutMs = HTTP_TIMEOUT_MS } = {}) {
  return new Promise((resolve) => {
    let url;
    try {
      url = new URL(urlString);
    } catch (error) {
      resolve({ ok: false, error: { kind: "invalid_url", message: error.message } });
      return;
    }

    const payload = body === null ? null : Buffer.from(JSON.stringify(body));
    const requestHeaders = { accept: "application/json", ...headers };
    if (payload) {
      requestHeaders["content-type"] = "application/json";
      requestHeaders["content-length"] = String(payload.length);
    }

    const transport = url.protocol === "https:" ? https : http;
    const request = transport.request(
      url,
      {
        method,
        headers: requestHeaders,
        // No connection pooling: a pooled socket would keep the event loop
        // alive after the client closed the pipe.
        agent: false,
      },
      (response) => {
        const chunks = [];
        let size = 0;
        response.on("data", (chunk) => {
          size += chunk.length;
          // A runaway body must not exhaust memory; 8 MB is far above any
          // admin endpoint.
          if (size <= 8 * 1024 * 1024) chunks.push(chunk);
        });
        response.on("end", () => {
          resolve({
            ok: true,
            status: response.statusCode ?? 0,
            text: Buffer.concat(chunks).toString("utf8"),
          });
        });
        response.on("error", (error) => {
          resolve({ ok: false, error: { kind: "response", message: error.message } });
        });
      },
    );

    request.setTimeout(timeoutMs, () => {
      request.destroy(new Error(`timed out after ${timeoutMs} ms`));
    });
    request.on("error", (error) => {
      resolve({
        ok: false,
        error: { kind: "transport", message: error.message, code: error.code ?? null },
      });
    });

    if (payload) request.write(payload);
    request.end();
  });
}

/** Parse a response body as JSON, falling back to the raw text. */
function parseBody(text) {
  const trimmed = text.trim();
  if (!trimmed) return { parsed: null, isJson: false };
  try {
    return { parsed: JSON.parse(trimmed), isJson: true };
  } catch {
    return { parsed: trimmed, isJson: false };
  }
}

/**
 * Shrink a payload until it fits into `maxBytes`.
 *
 * Structure is preserved where it matters: arrays lose their tail, objects
 * lose their last keys, and the caller learns what was dropped. The size is
 * accumulated per element, so a large list costs one pass, not one per item.
 */
function fitPayload(value, maxBytes) {
  const encoded = JSON.stringify(value);
  if (encoded === undefined) return { value, truncated: false, dropped: null };
  if (Buffer.byteLength(encoded) <= maxBytes) return { value, truncated: false, dropped: null };

  if (Array.isArray(value)) {
    const kept = [];
    let size = 2;
    for (const item of value) {
      const itemSize = Buffer.byteLength(JSON.stringify(item) ?? "null") + 1;
      if (size + itemSize > maxBytes) break;
      size += itemSize;
      kept.push(item);
    }
    return { value: kept, truncated: true, dropped: { items: value.length - kept.length } };
  }

  if (value && typeof value === "object") {
    const kept = {};
    const droppedFields = [];
    let size = 2;
    for (const [key, item] of Object.entries(value)) {
      const fieldSize =
        Buffer.byteLength(JSON.stringify(key)) + Buffer.byteLength(JSON.stringify(item) ?? "null") + 4;
      if (size + fieldSize > maxBytes) {
        droppedFields.push(key);
        continue;
      }
      size += fieldSize;
      kept[key] = item;
    }
    return { value: kept, truncated: true, dropped: { fields: droppedFields } };
  }

  if (typeof value === "string") {
    return { value: value.slice(0, maxBytes), truncated: true, dropped: { characters: true } };
  }

  return { value, truncated: false, dropped: null };
}

// --- tool results ----------------------------------------------------------

/** A successful tool result: a JSON payload in a text content block. */
function okResult(payload) {
  return {
    content: [{ type: "text", text: JSON.stringify(payload, null, 2) }],
    isError: false,
  };
}

/**
 * A failed tool result. MCP models a tool failure as a normal result with
 * `isError: true` — the agent can read the message and carry on.
 */
function errorResult(code, message, details = {}) {
  return {
    content: [
      {
        type: "text",
        text: JSON.stringify({ ok: false, error: { code, message, ...details } }, null, 2),
      },
    ],
    isError: true,
  };
}

// --- the admin API catalogue -----------------------------------------------

/**
 * The admin endpoints, transcribed from `desktop/gpui/src/api.rs` and
 * `crates/heterion-router-gateway/src/admin.rs`.
 */
const ENDPOINTS = [
  {
    method: "GET",
    path: "/healthz",
    auth: false,
    description: "Gateway identity: {status, backend, started_unix}. Public — no bearer needed.",
  },
  {
    method: "GET",
    path: "/api/overview",
    auth: true,
    description:
      "Gateway identity, live backends and row counts: {status, backend, backends[], counts:{combos, connections_active, connections_total, api_keys}}.",
  },
  {
    method: "GET",
    path: "/api/combos",
    auth: true,
    description:
      "Every combo with its entries and each entry's dispatch verdict: {combos:[{id, name, strategy, sort_order, is_hidden, active, servable_entries, total_entries, entries:[{id, provider, model, weight, backend, servable}]}]}.",
  },
  {
    method: "PATCH",
    path: "/api/combos/{id}",
    auth: true,
    description:
      "Update a combo. Body (every field optional): name, sort_order, is_hidden, is_active, strategy (priority|auto), models[] {id?, provider, model, weight?}.",
  },
  {
    method: "GET",
    path: "/api/connections",
    auth: true,
    description:
      "Provider connections with credentials masked: {connections:[{id, provider, name, auth_type, is_active, priority, expires_at, test_status, last_error, has_credential, servable}]}.",
  },
  {
    method: "PATCH",
    path: "/api/connections/{id}",
    auth: true,
    description: "Update a connection. Body (every field optional): name, is_active, priority.",
  },
  {
    method: "GET",
    path: "/api/keys",
    auth: true,
    description:
      "Every API key with its secret masked to a prefix: {keys:[{id, name, prefix, encrypted, created_at, revoked_at, expires_at, last_used_at, is_active, revoked}]}.",
  },
  {
    method: "POST",
    path: "/api/keys",
    auth: true,
    description:
      "Create an API key. Body: {name}. The response is the only one that carries the secret — it is shown once and never stored in readable form.",
  },
  {
    method: "POST",
    path: "/api/keys/{id}/revoke",
    auth: true,
    description: "Revoke an API key. Returns {status, id, revoked, revoked_at}.",
  },
  {
    method: "POST",
    path: "/api/keys/{id}/restore",
    auth: true,
    description: "Restore a revoked API key. Same response shape as revoke.",
  },
  {
    method: "GET",
    path: "/api/usage?days=&limit=",
    auth: true,
    description:
      "Totals, latency percentiles and the top buckets: {window_days, requests, successes, tokens_input, tokens_output, tokens_cache_read, latency:{p50_ms,p95_ms,p99_ms}, by_provider[], by_model[]}. days is clamped to 1..90 (default 7), limit to 1..50 (default 10).",
  },
  {
    method: "GET",
    path: "/api/logs?limit=",
    auth: true,
    description:
      "Recent calls, newest first: {logs:[{timestamp, model, requested_model, provider, status, duration_ms, tokens_input, tokens_output, combo_name, error_summary}]}. limit is clamped to 1..500 (default 100).",
  },
  {
    method: "GET",
    path: "/api/logs/app?limit=",
    auth: true,
    description:
      "Recent application/tracing log lines, oldest first: {lines:[...]}. limit is clamped to 1..1000 (default 50).",
  },
  {
    method: "POST",
    path: "/api/restart",
    auth: true,
    description:
      "Restart the launchd-supervised gateway. Answers 409 when the gateway is not supervised, 503 when the user id cannot be determined. The gateway re-execs ~2s later, so in-flight requests are dropped.",
  },
];

// --- tools -----------------------------------------------------------------

/** GET /healthz — reachability and backend name. Needs no bearer. */
async function toolRouterHealth() {
  const base = resolveBaseUrl();
  const attempt = await httpRequest("GET", `${base}/healthz`);
  const { key } = loadOperatorKey();

  if (attempt.ok) {
    const raw = key && attempt.text.includes(key) ? redact(attempt.text, key) : attempt.text;
    const { parsed, isJson } = parseBody(raw);
    return okResult({
      reachable: true,
      base_url: base,
      status: attempt.status,
      backend: isJson && parsed ? parsed.backend ?? null : null,
      body: parsed,
    });
  }

  // The configured base may be the wrong port: the service lives on 20128 and
  // a sidecar on 20129. Probe both so the agent learns where the gateway is.
  const alternatives = [];
  for (const port of [20128, 20129]) {
    const url = `http://127.0.0.1:${port}/healthz`;
    if (url === `${base}/healthz`) continue;
    const probe = await httpRequest("GET", url, { timeoutMs: 2000 });
    if (probe.ok) {
      const { parsed, isJson } = parseBody(probe.text);
      alternatives.push({
        base_url: `http://127.0.0.1:${port}`,
        status: probe.status,
        backend: isJson && parsed ? parsed.backend ?? null : null,
      });
    }
  }

  return errorResult("gateway_unreachable", `the gateway at ${base} did not answer`, {
    base_url: base,
    detail: attempt.error,
    alternatives_that_answer: alternatives,
  });
}

/**
 * Call any admin endpoint.
 *
 * `method` is GET/POST/PATCH, `path` is an absolute path on the gateway
 * (`/api/overview`), `body` an optional JSON object.
 */
async function toolRouterRequest(args) {
  const method = String(args.method ?? "GET").toUpperCase();
  if (!["GET", "POST", "PATCH"].includes(method)) {
    return errorResult("bad_method", `unsupported method: ${method} (use GET, POST or PATCH)`);
  }

  const path = String(args.path ?? "").trim();
  if (!path) return errorResult("bad_path", "path is required");
  if (!path.startsWith("/")) {
    return errorResult("bad_path", "path must be an absolute path on the gateway, e.g. /api/overview");
  }
  if (path.startsWith("//") || path.includes("://")) {
    return errorResult("bad_path", "path must not point at another host");
  }
  // eslint-disable-next-line no-control-regex
  if (/[\u0000-\u001f]/.test(path)) {
    return errorResult("bad_path", "path must not contain control characters");
  }
  if (path.split("/").includes("..")) {
    return errorResult("bad_path", "path must not contain '..' segments");
  }

  let body = null;
  if (args.body !== undefined && args.body !== null) {
    if (typeof args.body !== "object" || Array.isArray(args.body)) {
      return errorResult("bad_body", "body must be a JSON object");
    }
    body = args.body;
  }

  const base = resolveBaseUrl();
  const url = `${base}${path}`;
  const { key, source } = loadOperatorKey();
  const headers = key ? { authorization: `Bearer ${key}` } : {};

  const attempt = await httpRequest(method, url, { headers, body });
  if (!attempt.ok) {
    return errorResult("gateway_unreachable", `the gateway at ${base} did not answer`, {
      method,
      path,
      url,
      detail: attempt.error,
    });
  }

  // The key must never reach the agent, not even inside a response body.
  const rawText = key && attempt.text.includes(key) ? redact(attempt.text, key) : attempt.text;
  const { parsed, isJson } = parseBody(rawText);
  const fitted = fitPayload(parsed, MAX_OUTPUT_BYTES);

  const payload = {
    method,
    path,
    url,
    status: attempt.status,
    http_ok: attempt.status >= 200 && attempt.status < 300,
    authenticated: Boolean(key),
    key_source: source,
    is_json: isJson,
    body: fitted.value,
  };
  if (fitted.truncated) {
    payload.truncated = true;
    payload.dropped = fitted.dropped;
    payload.note = `the response was larger than ${MAX_OUTPUT_BYTES} bytes and was shortened`;
  }
  if (isJson && parsed && typeof parsed === "object" && parsed.error) {
    payload.gateway_error = parsed.error;
  }

  return okResult(payload);
}

/** The static endpoint catalogue. */
async function toolRouterEndpoints() {
  return okResult({
    base_url: resolveBaseUrl(),
    note:
      "Every endpoint except /healthz needs a bearer with the 'manage' scope. " +
      "The server sends it for you; set HETERION_ROUTER_KEY or sign in once in the app.",
    endpoints: ENDPOINTS,
  });
}

// --- the native app --------------------------------------------------------

/** Read the pid file, or null when there is none / it is broken. */
function readPidFile() {
  try {
    const parsed = JSON.parse(fs.readFileSync(PID_FILE, "utf8"));
    if (!parsed || typeof parsed.pid !== "number") return null;
    return parsed;
  } catch {
    return null;
  }
}

function writePidFile(record) {
  fs.mkdirSync(RUNTIME_DIR, { recursive: true });
  fs.writeFileSync(PID_FILE, `${JSON.stringify(record, null, 2)}\n`, { mode: 0o600 });
}

function removePidFile() {
  try {
    fs.rmSync(PID_FILE, { force: true });
  } catch {
    // Nothing to remove.
  }
}

/** Is the process alive? EPERM means it exists but belongs to someone else. */
function pidAlive(pid) {
  try {
    process.kill(pid, 0);
    return true;
  } catch (error) {
    return error.code === "EPERM";
  }
}

/** The command line of a process, or "" when it cannot be read. */
function processCommand(pid) {
  try {
    return execFileSync("/bin/ps", ["-p", String(pid), "-o", "command="], {
      encoding: "utf8",
      timeout: 5000,
    }).trim();
  } catch {
    return "";
  }
}

/** Does this pid belong to the router desktop shell? */
function isAppProcess(pid) {
  const command = processCommand(pid);
  return command.includes("heterion-router-desktop");
}

/** Is something listening on a loopback port? */
function portInUse(port) {
  return new Promise((resolve) => {
    const socket = net.connect({ host: "127.0.0.1", port });
    let settled = false;
    const finish = (value) => {
      if (settled) return;
      settled = true;
      socket.destroy();
      resolve(value);
    };
    socket.setTimeout(700);
    socket.on("connect", () => finish(true));
    socket.on("timeout", () => finish(false));
    socket.on("error", () => finish(false));
  });
}

/**
 * Who holds the single-instance lock: the pid and command of the process
 * listening on the lock port, so the agent can tell the operator who to stop.
 */
function lockHolder() {
  try {
    const output = execFileSync("/usr/sbin/lsof", ["-nP", "-iTCP:20130", "-t"], {
      encoding: "utf8",
      timeout: 5000,
    });
    const pid = Number(output.trim().split("\n")[0]);
    if (!Number.isInteger(pid) || pid <= 0) return null;
    return { pid, command: processCommand(pid) || "unknown" };
  } catch {
    return null;
  }
}

/** The last `count` lines of the app log. */
function logTail(count) {
  try {
    const text = fs.readFileSync(LOG_FILE, "utf8");
    const lines = text.split("\n").filter((line) => line.length > 0);
    return lines.slice(Math.max(0, lines.length - count));
  } catch {
    return [];
  }
}

/** Start the native shell. Refuses to start a second copy. */
async function toolAppLaunch() {
  if (!fs.existsSync(APP_BINARY)) {
    return errorResult(
      "app_not_built",
      `the native shell is not built: ${APP_BINARY} is missing — build it with ` +
        "`cargo build --manifest-path desktop/gpui/Cargo.toml`",
    );
  }

  const record = readPidFile();
  if (record && pidAlive(record.pid)) {
    if (isAppProcess(record.pid)) {
      return errorResult(
        "already_running",
        `the app is already running (pid ${record.pid}) — stop it with app_stop first`,
        { pid: record.pid, log_path: record.log_path ?? LOG_FILE },
      );
    }
    // The pid was recycled by an unrelated process: do not touch it.
    removePidFile();
  }

  if (await portInUse(INSTANCE_LOCK_PORT)) {
    const holder = lockHolder();
    return errorResult(
      "already_running",
      `another instance already holds the single-instance lock (port ${INSTANCE_LOCK_PORT}) — ` +
        "refusing to start a second copy",
      { port: INSTANCE_LOCK_PORT, holder },
    );
  }

  fs.mkdirSync(RUNTIME_DIR, { recursive: true });
  const fd = fs.openSync(LOG_FILE, "a");
  // The child gets the operator key neither in its environment nor in its
  // arguments — it reads the key file itself, if it reads it at all.
  const childEnv = { ...process.env };
  delete childEnv.HETERION_ROUTER_KEY;
  let child;
  try {
    child = spawn(APP_BINARY, [], {
      detached: true,
      stdio: ["ignore", fd, fd],
      env: childEnv,
    });
  } catch (error) {
    fs.closeSync(fd);
    return errorResult("spawn_failed", `could not start the app: ${error.message}`);
  }
  fs.closeSync(fd);

  if (!child.pid) {
    return errorResult("spawn_failed", "could not start the app: the spawn produced no pid");
  }
  // A failure that only surfaces after the spawn (a bad dynamic library, a
  // refused sandbox) must not leave a pid file behind.
  child.on("error", (error) => {
    log(`the app failed to start: ${error.message}`);
    removePidFile();
  });
  child.unref();

  const started = {
    pid: child.pid,
    binary: APP_BINARY,
    log_path: LOG_FILE,
    started_at: new Date().toISOString(),
  };
  writePidFile(started);

  return okResult({
    started: true,
    pid: child.pid,
    binary: APP_BINARY,
    log_path: LOG_FILE,
    started_at: started.started_at,
    note: "the app writes its tracing output to the log file; use app_status for the tail",
  });
}

/** Stop the app this server started. Never touches anything else. */
async function toolAppStop() {
  const record = readPidFile();
  if (!record) {
    return okResult({
      stopped: false,
      reason: "no pid file — this server did not start the app",
    });
  }

  const pid = record.pid;
  if (!pidAlive(pid)) {
    removePidFile();
    return okResult({ stopped: false, reason: `pid ${pid} is gone`, pid });
  }

  const command = processCommand(pid);
  if (!command.includes("heterion-router-desktop")) {
    return errorResult(
      "not_our_process",
      `pid ${pid} does not look like the router desktop shell — refusing to kill it`,
      { pid, command: command || "unknown" },
    );
  }

  let signal = "SIGTERM";
  try {
    process.kill(pid, "SIGTERM");
  } catch (error) {
    return errorResult("kill_failed", `could not signal pid ${pid}: ${error.message}`);
  }

  // Give the app a moment to close its window, then insist.
  const deadline = Date.now() + 5000;
  while (Date.now() < deadline && pidAlive(pid)) {
    await new Promise((resolve) => setTimeout(resolve, 100));
  }
  if (pidAlive(pid)) {
    signal = "SIGKILL";
    try {
      process.kill(pid, "SIGKILL");
    } catch {
      // It died between the check and the signal.
    }
  }

  removePidFile();
  return okResult({ stopped: true, pid, signal, log_path: LOG_FILE });
}

/** Is the app alive, does the gateway answer, what did the app log say. */
async function toolAppStatus(args) {
  const lines = Number.isInteger(args.lines) && args.lines > 0 ? Math.min(args.lines, 500) : 30;

  const record = readPidFile();
  let app = { running: false, pid: null, binary: APP_BINARY, log_path: LOG_FILE };
  if (record) {
    const alive = pidAlive(record.pid);
    app = {
      ...app,
      running: alive && isAppProcess(record.pid),
      pid: record.pid,
      started_at: record.started_at ?? null,
      uptime_seconds: alive && record.started_at
        ? Math.max(0, Math.round((Date.now() - Date.parse(record.started_at)) / 1000))
        : null,
    };
    if (!alive) app.note = "the pid file is stale — the app is not running";
  } else {
    app.note = "this server has not started the app";
  }

  const base = resolveBaseUrl();
  const probe = await httpRequest("GET", `${base}/healthz`, { timeoutMs: 5000 });
  let gateway = { reachable: false, base_url: base };
  if (probe.ok) {
    const { parsed, isJson } = parseBody(probe.text);
    gateway = {
      reachable: true,
      base_url: base,
      status: probe.status,
      backend: isJson && parsed ? parsed.backend ?? null : null,
    };
  } else {
    gateway.error = probe.error;
  }

  // The log tail is text from a process: redact the key out of it as well.
  const { key } = loadOperatorKey();
  const tail = logTail(lines).map((line) => redact(line, key));

  return okResult({ app, gateway, log_tail: tail });
}

/** Screenshot the screen into this directory's `shots/` folder. */
async function toolAppScreenshot(args) {
  const raw = typeof args.name === "string" ? args.name.trim() : "";
  const name = raw || "shot";

  // A name is a name, never a path: no separators, no traversal, no
  // absolute paths.
  if (!/^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$/.test(name) || name.includes("..")) {
    return errorResult(
      "bad_name",
      "name must be a plain file name (letters, digits, '.', '_', '-'), for example 'shot' or 'overview-1'",
    );
  }

  if (process.platform !== "darwin") {
    return errorResult("unsupported_platform", "screenshots need macOS (screencapture)");
  }

  const target = path.join(SHOTS_DIR, `${name}.png`);
  // Belt and braces: the resolved parent must be the shots directory.
  if (path.dirname(path.resolve(target)) !== path.resolve(SHOTS_DIR)) {
    return errorResult("bad_name", "the screenshot would be written outside the shots directory");
  }

  fs.mkdirSync(SHOTS_DIR, { recursive: true });

  const result = spawnSync("/usr/sbin/screencapture", ["-x", "-o", target], {
    encoding: "utf8",
    timeout: 30_000,
  });
  if (result.error) {
    return errorResult("capture_failed", `screencapture failed: ${result.error.message}`);
  }
  if (result.status !== 0) {
    const detail = redact(result.stderr || "", loadOperatorKey().key).trim();
    return errorResult(
      "capture_failed",
      `screencapture exited with ${result.status}${detail ? `: ${detail}` : ""}`,
      {
        hint:
          "macOS needs Screen Recording permission for the terminal that runs this server " +
          "(System Settings → Privacy & Security → Screen Recording)",
      },
    );
  }

  let bytes = 0;
  try {
    bytes = fs.statSync(target).size;
  } catch {
    // The file vanished between the capture and the stat.
  }

  return okResult({
    path: path.resolve(target),
    bytes,
    note: "read this file with your image tool to look at the screen",
  });
}

// --- tool catalogue --------------------------------------------------------

const TOOLS = [
  {
    name: "router_health",
    description:
      "Check that the router gateway is reachable. GET /healthz — reports the gateway's backend name. No bearer needed. " +
      "Use this first: it is the cheapest way to learn whether the gateway is up and which port answers.",
    inputSchema: { type: "object", properties: {}, additionalProperties: false },
    handler: toolRouterHealth,
  },
  {
    name: "router_request",
    description:
      "Call any admin endpoint of the router gateway. Parameters: method (GET, POST or PATCH), path (e.g. /api/overview), " +
      "optional body (a JSON object). The operator bearer is attached automatically. The response is returned as JSON, " +
      "truncated to 200 KB so a large list cannot flood you. Use router_endpoints to see what exists.",
    inputSchema: {
      type: "object",
      properties: {
        method: {
          type: "string",
          enum: ["GET", "POST", "PATCH"],
          description: "HTTP method. Defaults to GET.",
        },
        path: {
          type: "string",
          description: "Absolute path on the gateway, e.g. /api/usage?days=1&limit=10.",
        },
        body: {
          type: "object",
          description: "JSON body for POST and PATCH, e.g. {\"name\": \"new combo\"}.",
        },
      },
      required: ["path"],
      additionalProperties: false,
    },
    handler: toolRouterRequest,
  },
  {
    name: "router_endpoints",
    description:
      "The catalogue of admin endpoints the router exposes, with a one-line description and the shape of each body. " +
      "Call this before router_request when you are not sure which endpoint to use.",
    inputSchema: { type: "object", properties: {}, additionalProperties: false },
    handler: toolRouterEndpoints,
  },
  {
    name: "app_launch",
    description:
      "Start the native GPUI desktop shell (the debug build) and return its pid and log path. Refuses to start a second copy. " +
      "The app's tracing output goes to the log file.",
    inputSchema: { type: "object", properties: {}, additionalProperties: false },
    handler: toolAppLaunch,
  },
  {
    name: "app_stop",
    description:
      "Stop the desktop shell that app_launch started. It only ever kills the process recorded in the pid file, " +
      "and refuses to kill a pid that is not the router shell.",
    inputSchema: { type: "object", properties: {}, additionalProperties: false },
    handler: toolAppStop,
  },
  {
    name: "app_status",
    description:
      "One look at the whole picture: is the desktop app running, does the gateway answer, and what do the last 30 lines " +
      "of the app log say.",
    inputSchema: {
      type: "object",
      properties: {
        lines: {
          type: "integer",
          description: "How many log lines to return. Defaults to 30.",
        },
      },
      additionalProperties: false,
    },
    handler: toolAppStatus,
  },
  {
    name: "app_screenshot",
    description:
      "Capture the whole screen into this server's shots/ directory and return the absolute path, so you can read the image " +
      "and see what the native app renders. Full-screen capture only.",
    inputSchema: {
      type: "object",
      properties: {
        name: {
          type: "string",
          description:
            "File name without an extension, e.g. 'shot' or 'overview-1'. Defaults to 'shot'. " +
            "Must be a plain name — no path separators.",
        },
      },
      additionalProperties: false,
    },
    handler: toolAppScreenshot,
  },
];

const TOOLS_BY_NAME = new Map(TOOLS.map((tool) => [tool.name, tool]));

// --- JSON-RPC --------------------------------------------------------------

function send(message) {
  process.stdout.write(`${JSON.stringify(message)}\n`);
}

function reply(id, result) {
  send({ jsonrpc: "2.0", id, result });
}

function replyError(id, code, message, data) {
  const error = { code, message };
  if (data !== undefined) error.data = data;
  send({ jsonrpc: "2.0", id, error });
}

function handleInitialize(params) {
  const requested = typeof params?.protocolVersion === "string" ? params.protocolVersion : "";
  const protocolVersion = SUPPORTED_PROTOCOL_VERSIONS.includes(requested)
    ? requested
    : DEFAULT_PROTOCOL_VERSION;

  return {
    protocolVersion,
    capabilities: { tools: { listChanged: false } },
    serverInfo: { name: SERVER_NAME, version: SERVER_VERSION },
    instructions:
      "Inspect the Heterion Router. Start with router_health to see whether the gateway answers, " +
      "router_endpoints for the admin API, and router_request to call it. " +
      "For the native desktop app: app_launch, app_status, app_screenshot (the returned path can be read " +
      "with your image tool), app_stop.",
  };
}

async function handleToolsCall(params) {
  const name = params?.name;
  if (typeof name !== "string" || !TOOLS_BY_NAME.has(name)) {
    return { error: { code: -32602, message: `unknown tool: ${String(name)}` } };
  }

  const args = params?.arguments;
  if (args !== undefined && (typeof args !== "object" || args === null || Array.isArray(args))) {
    return { error: { code: -32602, message: "arguments must be an object" } };
  }

  const tool = TOOLS_BY_NAME.get(name);
  try {
    return { result: await tool.handler(args ?? {}) };
  } catch (error) {
    // An unexpected exception must never reach stdout as a stack trace.
    log(`tool ${name} failed: ${error?.stack ?? error}`);
    return {
      error: {
        code: -32603,
        message: `internal error in ${name}: ${redact(error?.message ?? String(error), loadOperatorKey().key)}`,
      },
    };
  }
}

async function dispatch(message) {
  const { id, method, params } = message ?? {};

  // Notifications carry no id and must not be answered.
  if (id === undefined || id === null) return;

  switch (method) {
    case "initialize":
      reply(id, handleInitialize(params));
      return;
    case "tools/list":
      reply(id, { tools: TOOLS.map(({ handler, ...tool }) => tool) });
      return;
    case "tools/call": {
      const outcome = await handleToolsCall(params);
      if (outcome.error) replyError(id, outcome.error.code, outcome.error.message, outcome.error.data);
      else reply(id, outcome.result);
      return;
    }
    case "ping":
      reply(id, {});
      return;
    case "resources/list":
      reply(id, { resources: [] });
      return;
    case "prompts/list":
      reply(id, { prompts: [] });
      return;
    default:
      replyError(id, -32601, `method not found: ${String(method)}`);
  }
}

// --- main loop -------------------------------------------------------------

/** How many requests are still in flight. */
let pending = 0;
/** Set when the client closed the pipe. */
let closing = false;

/**
 * Leave once the pipe is closed and every request has answered.
 *
 * The client closes stdin as soon as it is done; a request that is still in
 * flight (an HTTP call, a screenshot) must still get its reply.
 */
function maybeExit() {
  if (!closing || pending > 0) return;
  process.exitCode = 0;
  // If something keeps the event loop alive, do not hang forever.
  const timer = setTimeout(() => process.exit(0), 500);
  timer.unref();
}

/** Run a request and keep the server alive until it has answered. */
function track(promise) {
  pending += 1;
  return promise.finally(() => {
    pending -= 1;
    maybeExit();
  });
}

function main() {
  const input = readline.createInterface({ input: process.stdin, crlfDelay: Infinity });

  input.on("line", (line) => {
    const trimmed = line.trim();
    if (!trimmed) return;

    let message;
    try {
      message = JSON.parse(trimmed);
    } catch (error) {
      // Not JSON: the client broke the framing. Say so, do not crash.
      replyError(null, -32700, `parse error: ${error.message}`);
      return;
    }

    if (Array.isArray(message)) {
      for (const entry of message) {
        track(dispatch(entry).catch((error) => log(`dispatch failed: ${error?.stack ?? error}`)));
      }
      return;
    }
    track(dispatch(message).catch((error) => log(`dispatch failed: ${error?.stack ?? error}`)));
  });

  input.on("close", () => {
    // The client closed the pipe: answer what is still running, then leave.
    closing = true;
    maybeExit();
  });

  log(`ready — gateway ${resolveBaseUrl()}, app ${APP_BINARY}`);
}

process.on("uncaughtException", (error) => {
  log(`uncaught exception: ${error?.stack ?? error}`);
});
process.on("unhandledRejection", (reason) => {
  log(`unhandled rejection: ${reason?.stack ?? reason}`);
});
process.stdout.on("error", (error) => {
  // The client went away mid-write.
  if (error?.code === "EPIPE") process.exit(0);
  log(`stdout error: ${error?.message ?? error}`);
});

main();

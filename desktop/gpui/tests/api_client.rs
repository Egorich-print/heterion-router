//! The typed admin client against a fake gateway.
//!
//! `desktop/gpui` is a binary crate, so an integration test cannot import its
//! modules. The module files are included by path instead: the tests exercise
//! the very code the shell runs — the client, the response types and the
//! auth-error predicate.
//!
//! The fake gateway is a hand-written HTTP/1.1 server on loopback (no extra
//! dependency). It answers with the JSON shapes the real gateway produces in
//! `crates/heterion-router-gateway/src/admin.rs`, so a drift between the
//! client and the gateway shows up as a decoding failure here.

// The included module files are larger than these tests use: their
// unused items are the shell's business, not the test's.
#![allow(dead_code)]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

#[path = "../src/api.rs"]
mod api;

#[path = "../src/theme.rs"]
mod theme;

/// The key the fake gateway expects. A fixture, not a credential.
const OPERATOR_KEY: &str = "test-operator-key";

/// `GET /api/overview` — the shape of `admin::overview`.
const OVERVIEW: &str = r#"{
    "status": "ok",
    "backend": "sqlite",
    "backends": ["sqlite"],
    "counts": {
        "combos": 2,
        "connections_active": 1,
        "connections_total": 3,
        "api_keys": 4
    }
}"#;

/// `GET /api/combos` — the shape of `admin::combos`.
const COMBOS: &str = r#"{
    "combos": [
        {
            "id": "combo-default",
            "name": "default",
            "strategy": "priority",
            "sort_order": 10,
            "is_hidden": false,
            "active": true,
            "servable_entries": 1,
            "total_entries": 2,
            "entries": [
                {
                    "id": "entry-1",
                    "provider": "openai",
                    "model": "gpt-5.6-sol",
                    "weight": 50,
                    "backend": "openai",
                    "servable": true
                },
                {
                    "id": "entry-2",
                    "provider": "agentrouter",
                    "model": "gpt-6-astra",
                    "weight": 30,
                    "backend": null,
                    "servable": false
                }
            ]
        }
    ]
}"#;

/// `GET /api/connections` — the shape of `admin::connections`.
const CONNECTIONS: &str = r#"{
    "connections": [
        {
            "id": "conn-1",
            "provider": "openai",
            "name": "OpenAI",
            "auth_type": "api_key",
            "is_active": true,
            "priority": 10,
            "expires_at": null,
            "test_status": "ok",
            "last_error": null,
            "has_credential": true,
            "servable": true
        },
        {
            "id": "conn-2",
            "provider": "groq",
            "name": null,
            "auth_type": null,
            "is_active": true,
            "priority": 0,
            "expires_at": null,
            "test_status": null,
            "last_error": null,
            "has_credential": false,
            "servable": false
        }
    ]
}"#;

/// `GET /api/keys` — the shape of `admin::keys`.
const KEYS: &str = r#"{
    "keys": [
        {
            "id": "key-1",
            "name": "operator",
            "prefix": "sk-test",
            "encrypted": false,
            "created_at": "2026-10-28 12:00:00",
            "revoked_at": null,
            "expires_at": null,
            "last_used_at": null,
            "is_active": true,
            "revoked": false
        }
    ]
}"#;

/// `GET /api/usage` — the shape of `admin::usage`.
const USAGE: &str = r#"{
    "window_days": 7,
    "requests": 10,
    "successes": 9,
    "tokens_input": 100,
    "tokens_output": 200,
    "tokens_cache_read": 5,
    "latency": {
        "p50_ms": 12.5,
        "p95_ms": 40.0,
        "p99_ms": null
    },
    "by_provider": [
        {
            "key": "openai",
            "requests": 10,
            "tokens_input": 100,
            "tokens_output": 200
        }
    ],
    "by_model": [
        {
            "key": "gpt-5.6-sol",
            "requests": 4,
            "tokens_input": 40,
            "tokens_output": 80
        }
    ]
}"#;

/// `GET /api/logs` — the shape of `admin::logs`.
const LOGS: &str = r#"{
    "logs": [
        {
            "timestamp": "2026-10-28 12:00:00",
            "model": "gpt-5.6-sol",
            "requested_model": "gpt-5.6-sol",
            "provider": "openai",
            "status": "200",
            "duration_ms": 123,
            "tokens_input": 10,
            "tokens_output": 20,
            "combo_name": "default",
            "error_summary": null
        }
    ]
}"#;

/// `GET /api/logs/app` — the shape of `admin::logs_app`.
const APP_LOGS: &str = r#"{
    "lines": [
        "2026-10-28 12:00:00 INFO gateway started",
        "2026-10-28 12:00:01 WARN upstream retry"
    ]
}"#;

/// What the fake gateway answers with.
struct Reply {
    status: u16,
    body: String,
}

impl Reply {
    /// A successful answer.
    fn ok(body: &str) -> Self {
        Self {
            status: 200,
            body: body.to_string(),
        }
    }

    /// A failing answer.
    fn error(status: u16, body: &str) -> Self {
        Self {
            status,
            body: body.to_string(),
        }
    }
}

/// One request the fake gateway received.
#[derive(Clone, Debug)]
struct Seen {
    path: String,
    authorization: Option<String>,
}

/// Every listener the fake gateway ever bound.
///
/// The ports stay reserved for the lifetime of the test binary. A port that
/// is given back to the system could be handed to the next test — and the
/// client keeps its connections in a pool, so it would then talk to a
/// server that is not the one it asked for.
static PORTS: Mutex<Vec<TcpListener>> = Mutex::new(Vec::new());

/// A loopback HTTP/1.1 server that answers from a closure.
struct FakeGateway {
    base: String,
    seen: Arc<Mutex<Vec<Seen>>>,
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl FakeGateway {
    /// Start a server that answers every request with `handler`.
    fn start<F>(handler: F) -> Self
    where
        F: Fn(&str) -> Reply + Send + Sync + 'static,
    {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("the fake gateway must bind");
        let port = listener
            .local_addr()
            .expect("the fake gateway must have an address")
            .port();
        listener
            .set_nonblocking(true)
            .expect("the fake gateway must go non-blocking");

        // The worker gets its own handle; the original stays bound so the
        // port is never handed to another server.
        let worker_listener = listener
            .try_clone()
            .expect("the listener must be cloneable");
        PORTS
            .lock()
            .expect("the port registry must not be poisoned")
            .push(listener);

        let seen: Arc<Mutex<Vec<Seen>>> = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));

        let worker = {
            let seen = Arc::clone(&seen);
            let stop = Arc::clone(&stop);
            let handler = Arc::new(handler);
            std::thread::spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    match worker_listener.accept() {
                        Ok((stream, _)) => serve(stream, handler.as_ref(), seen.as_ref()),
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            std::thread::sleep(Duration::from_millis(2));
                        }
                        Err(_) => break,
                    }
                }
            })
        };

        Self {
            base: format!("http://127.0.0.1:{port}"),
            seen,
            stop,
            worker: Some(worker),
        }
    }

    /// The base URL of the fake gateway.
    fn base(&self) -> &str {
        &self.base
    }

    /// Every request the server has seen so far.
    fn requests(&self) -> Vec<Seen> {
        self.seen
            .lock()
            .expect("the request log must not be poisoned")
            .clone()
    }
}

impl Drop for FakeGateway {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

/// Answer one request and close the connection.
fn serve(stream: TcpStream, handler: &dyn Fn(&str) -> Reply, seen: &Mutex<Vec<Seen>>) {
    let Ok(read_half) = stream.try_clone() else {
        return;
    };
    let mut reader = BufReader::new(read_half);

    let mut request_line = String::new();
    if reader.read_line(&mut request_line).is_err() || request_line.trim().is_empty() {
        return;
    }

    let mut content_length = 0usize;
    let mut authorization = None;
    loop {
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(0) => break,
            Ok(_) => {
                let line = line.trim_end();
                if line.is_empty() {
                    break;
                }
                if let Some((name, value)) = line.split_once(':') {
                    let name = name.trim().to_ascii_lowercase();
                    let value = value.trim().to_string();
                    if name == "content-length" {
                        content_length = value.parse().unwrap_or(0);
                    } else if name == "authorization" {
                        authorization = Some(value);
                    }
                }
            }
            Err(_) => break,
        }
    }

    if content_length > 0 {
        let mut body = vec![0u8; content_length];
        let _ = reader.read_exact(&mut body);
    }

    let path = request_line.split_whitespace().nth(1).unwrap_or("/");
    let path = path.split('?').next().unwrap_or("/").to_string();

    seen.lock()
        .expect("the request log must not be poisoned")
        .push(Seen {
            path: path.clone(),
            authorization,
        });

    let reply = handler(&path);
    let head = format!(
        "HTTP/1.1 {} {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        reply.status,
        reason(reply.status),
        reply.body.len()
    );

    let mut stream = stream;
    let _ = stream.write_all(head.as_bytes());
    let _ = stream.write_all(reply.body.as_bytes());
    let _ = stream.flush();
    let _ = stream.shutdown(Shutdown::Write);
}

/// The reason phrase of a status the fake gateway can answer with.
fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        500 => "Internal Server Error",
        _ => "Status",
    }
}

/// The fake gateway that serves every endpoint the shell reads.
fn serving_gateway() -> FakeGateway {
    FakeGateway::start(|path| match path {
        "/api/overview" => Reply::ok(OVERVIEW),
        "/api/combos" => Reply::ok(COMBOS),
        "/api/connections" => Reply::ok(CONNECTIONS),
        "/api/keys" => Reply::ok(KEYS),
        "/api/usage" => Reply::ok(USAGE),
        "/api/logs" => Reply::ok(LOGS),
        "/api/logs/app" => Reply::ok(APP_LOGS),
        other => Reply::error(404, &format!(r#"{{"error":"no route {other}"}}"#)),
    })
}

#[test]
fn the_client_decodes_every_endpoint_the_shell_reads() {
    let gateway = serving_gateway();
    let client = api::ApiClient::new(gateway.base(), OPERATOR_KEY);

    let overview: api::Overview = client.get_json("/api/overview").expect("overview");
    assert_eq!(overview.status, "ok");
    assert_eq!(overview.backend, "sqlite");
    assert_eq!(overview.backends, vec!["sqlite".to_string()]);
    assert_eq!(overview.counts.combos, 2);
    assert_eq!(overview.counts.connections_active, 1);
    assert_eq!(overview.counts.connections_total, 3);
    assert_eq!(overview.counts.api_keys, 4);

    let combos: api::CombosResponse = client.get_json("/api/combos").expect("combos");
    assert_eq!(combos.combos.len(), 1);
    let combo = &combos.combos[0];
    assert_eq!(combo.id, "combo-default");
    assert_eq!(combo.name, "default");
    assert_eq!(combo.strategy, "priority");
    assert_eq!(combo.sort_order, 10);
    assert!(!combo.is_hidden);
    assert!(combo.active);
    assert_eq!(combo.servable_entries, 1);
    assert_eq!(combo.total_entries, 2);
    assert_eq!(combo.entries.len(), 2);
    assert_eq!(combo.entries[0].provider, "openai");
    assert_eq!(combo.entries[0].model, "gpt-5.6-sol");
    assert_eq!(combo.entries[0].weight, 50);
    assert_eq!(combo.entries[0].backend.as_deref(), Some("openai"));
    assert!(combo.entries[0].servable);
    assert_eq!(combo.entries[1].backend, None);
    assert!(!combo.entries[1].servable);

    let connections: api::ConnectionsResponse =
        client.get_json("/api/connections").expect("connections");
    assert_eq!(connections.connections.len(), 2);
    let connection = &connections.connections[0];
    assert_eq!(connection.id, "conn-1");
    assert_eq!(connection.provider, "openai");
    assert_eq!(connection.name, "OpenAI");
    assert_eq!(connection.auth_type, "api_key");
    assert!(connection.is_active);
    assert_eq!(connection.priority, 10);
    assert_eq!(connection.expires_at, None);
    assert_eq!(connection.test_status.as_deref(), Some("ok"));
    assert_eq!(connection.last_error, None);
    assert!(connection.has_credential);
    assert!(connection.servable);
    assert_eq!(connections.connection_error, None);

    let keys: api::KeysResponse = client.get_json("/api/keys").expect("keys");
    assert_eq!(keys.keys.len(), 1);
    let key = &keys.keys[0];
    assert_eq!(key.id, "key-1");
    assert_eq!(key.name, "operator");
    assert_eq!(key.prefix, "sk-test");
    assert!(!key.encrypted);
    assert_eq!(key.created_at.as_deref(), Some("2026-10-28 12:00:00"));
    assert_eq!(key.revoked_at, None);
    assert_eq!(key.expires_at, None);
    assert_eq!(key.last_used_at, None);
    assert!(key.is_active);
    assert!(!key.revoked);
    assert_eq!(keys.error, None);

    let usage: api::Usage = client.get_json("/api/usage").expect("usage");
    assert_eq!(usage.window_days, 7);
    assert_eq!(usage.requests, 10);
    assert_eq!(usage.successes, 9);
    assert_eq!(usage.tokens_input, 100);
    assert_eq!(usage.tokens_output, 200);
    assert_eq!(usage.tokens_cache_read, 5);
    assert_eq!(usage.latency.p50_ms, Some(12.5));
    assert_eq!(usage.latency.p95_ms, Some(40.0));
    assert_eq!(usage.latency.p99_ms, None);
    assert_eq!(usage.by_provider.len(), 1);
    assert_eq!(usage.by_provider[0].key, "openai");
    assert_eq!(usage.by_provider[0].requests, 10);
    assert_eq!(usage.by_provider[0].tokens_input, 100);
    assert_eq!(usage.by_provider[0].tokens_output, 200);
    assert_eq!(usage.by_model.len(), 1);
    assert_eq!(usage.by_model[0].key, "gpt-5.6-sol");
    assert_eq!(usage.by_model[0].tokens_output, 80);

    let logs: api::LogsResponse = client.get_json("/api/logs").expect("logs");
    assert_eq!(logs.logs.len(), 1);
    let log = &logs.logs[0];
    assert_eq!(log.timestamp.as_deref(), Some("2026-10-28 12:00:00"));
    assert_eq!(log.model.as_deref(), Some("gpt-5.6-sol"));
    assert_eq!(log.requested_model.as_deref(), Some("gpt-5.6-sol"));
    assert_eq!(log.provider.as_deref(), Some("openai"));
    assert_eq!(log.status.as_deref(), Some("200"));
    assert_eq!(log.duration_ms, Some(123));
    assert_eq!(log.tokens_input, Some(10));
    assert_eq!(log.tokens_output, Some(20));
    assert_eq!(log.combo_name.as_deref(), Some("default"));
    assert_eq!(log.error_summary, None);

    let app_logs: api::AppLogsResponse = client.get_json("/api/logs/app").expect("app logs");
    assert_eq!(app_logs.lines.len(), 2);
    assert!(app_logs.lines[0].contains("gateway started"));
    assert!(app_logs.lines[1].contains("upstream retry"));
}

/// A `NULL` column must not take the whole response down.
///
/// The gateway serializes nullable database columns as they are: a `NULL`
/// arrives as `null`, not as an empty string. `provider_connections.name` and
/// `.auth_type` are nullable, and two of the 66 connections in the live
/// database have a null name — which used to fail the whole
/// `/api/connections` response and left the Providers screen empty.
#[test]
fn a_null_column_decodes_as_the_default_instead_of_failing_the_response() {
    let gateway = serving_gateway();
    let client = api::ApiClient::new(gateway.base(), OPERATOR_KEY);

    let connections: api::ConnectionsResponse =
        client.get_json("/api/connections").expect("connections");
    let null_row = connections
        .connections
        .iter()
        .find(|row| row.id == "conn-2")
        .expect("the row with the null columns");
    assert_eq!(null_row.name, "");
    assert_eq!(null_row.auth_type, "");
    assert_eq!(null_row.test_status, None);

    // The rows next to it are untouched.
    let named = connections
        .connections
        .iter()
        .find(|row| row.id == "conn-1")
        .expect("the named row");
    assert_eq!(named.name, "OpenAI");
    assert_eq!(named.auth_type, "api_key");
}

/// The same rule for the other nullable columns of the contract.
#[test]
fn every_nullable_column_of_the_contract_decodes_as_the_default() {
    // `api_keys.key_prefix` and `usage_history.provider`/`model` are nullable
    // as well; a null there must not fail the response either.
    let key: api::ApiKey = serde_json::from_str(
        r#"{
            "id": "key-1",
            "name": "operator",
            "prefix": null,
            "encrypted": false,
            "created_at": "2026-10-28 12:00:00",
            "revoked_at": null,
            "expires_at": null,
            "last_used_at": null,
            "is_active": true,
            "revoked": false
        }"#,
    )
    .expect("a key with a null prefix");
    assert_eq!(key.prefix, "");

    let bucket: api::UsageBucket = serde_json::from_str(
        r#"{"key": null, "requests": 3, "tokens_input": 0, "tokens_output": 0}"#,
    )
    .expect("a bucket with a null key");
    assert_eq!(bucket.key, "");
    assert_eq!(bucket.requests, 3);
}

#[test]
fn the_client_presents_the_operator_key_as_a_bearer() {
    let gateway = serving_gateway();
    let client = api::ApiClient::new(gateway.base(), OPERATOR_KEY);

    let _: api::ApiError = client
        .get_json::<api::Health>("/healthz")
        .expect_err("no health route");
    let _: api::Overview = client.get_json("/api/overview").expect("overview");

    let requests = gateway.requests();
    assert!(!requests.is_empty(), "the gateway must have seen a request");
    let expected = format!("Bearer {OPERATOR_KEY}");
    for request in &requests {
        assert_eq!(
            request.authorization.as_deref(),
            Some(expected.as_str()),
            "every request must present the operator key"
        );
    }
    assert_eq!(
        requests.last().expect("a request").path,
        "/api/overview",
        "the last request must be the overview"
    );
}

#[test]
fn a_trailing_slash_in_the_base_url_is_dropped() {
    let gateway = serving_gateway();
    let client = api::ApiClient::new(format!("{}/", gateway.base()), OPERATOR_KEY);
    assert_eq!(client.base(), gateway.base());

    let _: api::Overview = client.get_json("/api/overview").expect("overview");
    let requests = gateway.requests();
    assert!(!requests.is_empty(), "the gateway must have seen a request");
    for request in &requests {
        assert_eq!(
            request.path, "/api/overview",
            "the trailing slash must not change the path"
        );
    }
}

#[test]
fn a_gateway_error_is_surfaced_with_its_status_and_message() {
    let gateway =
        FakeGateway::start(|_| Reply::error(400, r#"{"error":"name must not be empty"}"#));
    let client = api::ApiClient::new(gateway.base(), OPERATOR_KEY);

    let error = client
        .get_json::<api::Overview>("/api/keys")
        .expect_err("a 400 must not decode");
    assert_eq!(error.status, 400);
    assert_eq!(error.message, "name must not be empty");
    assert_eq!(error.to_string(), "HTTP 400: name must not be empty");
    assert!(
        !theme::is_auth_error(&error),
        "a 400 is a bad request, not an authentication failure"
    );
}

#[test]
fn an_unauthorized_answer_is_recognised_as_an_auth_failure() {
    let gateway = FakeGateway::start(|path| match path {
        "/api/keys" => Reply::error(401, r#"{"error":"unauthorized"}"#),
        _ => Reply::error(403, r#"{"error":"forbidden"}"#),
    });
    let client = api::ApiClient::new(gateway.base(), OPERATOR_KEY);

    let unauthorized = client
        .get_json::<api::KeysResponse>("/api/keys")
        .expect_err("a 401 must not decode");
    assert_eq!(unauthorized.status, 401);
    assert_eq!(unauthorized.message, "unauthorized");
    assert!(
        theme::is_auth_error(&unauthorized),
        "a 401 must ask the operator for the key again"
    );

    let forbidden = client
        .get_json::<api::KeysResponse>("/api/overview")
        .expect_err("a 403 must not decode");
    assert_eq!(forbidden.status, 403);
    assert!(
        theme::is_auth_error(&forbidden),
        "a 403 must ask the operator for the key again"
    );
}

#[test]
fn a_closed_port_is_a_transport_error_and_not_a_panic() {
    // A port nobody serves: bound, then given up again. Another test may
    // grab the port in the tiny window between the two, so the check is
    // repeated with a fresh port.
    for _ in 0..5 {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind");
        let port = listener.local_addr().expect("address").port();
        drop(listener);

        let client = api::ApiClient::new(format!("http://127.0.0.1:{port}"), OPERATOR_KEY);
        let error = match client.get_json::<api::Overview>("/api/overview") {
            // Somebody serves this port now — try another one.
            Ok(_) => continue,
            Err(error) => error,
        };

        if error.status != 0 {
            // The port answered — try another one.
            continue;
        }

        assert_eq!(
            error.status, 0,
            "a transport failure never reached the gateway"
        );
        assert!(!error.message.is_empty(), "the failure must explain itself");
        assert!(!theme::is_auth_error(&error));
        return;
    }

    panic!("no closed port could be found");
}

#[test]
fn an_unreadable_body_is_reported_as_a_decode_failure() {
    let gateway = FakeGateway::start(|_| Reply::ok("{\"combos\": \"not a list\"}"));
    let client = api::ApiClient::new(gateway.base(), OPERATOR_KEY);

    let error = client
        .get_json::<api::CombosResponse>("/api/combos")
        .expect_err("a broken body must not decode");
    assert_eq!(
        error.status, 200,
        "the gateway answered, its body was wrong"
    );
    assert!(
        error.message.starts_with("unexpected response:"),
        "the failure must name the decode problem: {}",
        error.message
    );
}

#[test]
fn an_error_body_without_json_is_kept_verbatim() {
    let gateway = FakeGateway::start(|_| Reply::error(500, "the gateway is on fire"));
    let client = api::ApiClient::new(gateway.base(), OPERATOR_KEY);

    let error = client
        .get_json::<api::Overview>("/api/overview")
        .expect_err("a 500 must not decode");
    assert_eq!(error.status, 500);
    assert_eq!(error.message, "the gateway is on fire");
}

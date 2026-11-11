//! Contract tests for adding a provider connection from the desktop shell.
//!
//! The view posts `POST /api/connections` with the body the gateway expects,
//! refreshes the connection list after success, and must understand that an
//! active connection needs a gateway restart before it can serve traffic.
//!
//! `desktop/gpui` is a binary crate, so the client module is included by path
//! — the same way `tests/api_client.rs` exercises the shell's code.

#![allow(dead_code)]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

#[path = "../src/api.rs"]
mod api;

/// A fixture, not a credential.
const OPERATOR_KEY: &str = "test-operator-key";

#[derive(Clone, Debug)]
struct Seen {
    method: String,
    path: String,
    body: String,
}

struct Reply {
    status: u16,
    body: String,
}

impl Reply {
    fn json(status: u16, body: &str) -> Self {
        Self {
            status,
            body: body.to_string(),
        }
    }
}

static PORTS: Mutex<Vec<TcpListener>> = Mutex::new(Vec::new());

struct FakeGateway {
    base: String,
    seen: Arc<Mutex<Vec<Seen>>>,
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl FakeGateway {
    fn start<F>(handler: F) -> Self
    where
        F: Fn(&str, &str, &str) -> Reply + Send + Sync + 'static,
    {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind");
        let port = listener.local_addr().expect("addr").port();
        listener.set_nonblocking(true).expect("nonblocking");

        let worker_listener = listener.try_clone().expect("clone");
        PORTS.lock().expect("ports").push(listener);

        let seen: Arc<Mutex<Vec<Seen>>> = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let worker = {
            let seen = Arc::clone(&seen);
            let stop = Arc::clone(&stop);
            let handler = Arc::new(handler);
            std::thread::spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    match worker_listener.accept() {
                        Ok((stream, _)) => {
                            let handler = Arc::clone(&handler);
                            let seen = Arc::clone(&seen);
                            std::thread::spawn(move || serve(stream, handler.as_ref(), &seen));
                        }
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

    fn base(&self) -> &str {
        &self.base
    }

    fn requests(&self) -> Vec<Seen> {
        self.seen.lock().expect("seen").clone()
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

fn serve(
    mut stream: TcpStream,
    handler: &dyn Fn(&str, &str, &str) -> Reply,
    seen: &Mutex<Vec<Seen>>,
) {
    let _ = stream.set_nonblocking(false);
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));

    let Ok(read_half) = stream.try_clone() else {
        return;
    };
    let mut reader = BufReader::new(read_half);

    let mut request_line = String::new();
    if reader.read_line(&mut request_line).is_err() || request_line.trim().is_empty() {
        return;
    }

    let mut content_length = 0usize;
    loop {
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(0) => break,
            Ok(_) => {
                let line = line.trim_end();
                if line.is_empty() {
                    break;
                }
                if let Some((name, value)) = line.split_once(':')
                    && name.trim().eq_ignore_ascii_case("content-length")
                {
                    content_length = value.trim().parse().unwrap_or(0);
                }
            }
            Err(_) => break,
        }
    }

    let mut body = vec![0u8; content_length];
    if content_length > 0 {
        let _ = reader.read_exact(&mut body);
    }
    let body = String::from_utf8_lossy(&body).to_string();

    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("GET").to_string();
    let path = parts
        .next()
        .unwrap_or("/")
        .split('?')
        .next()
        .unwrap_or("/")
        .to_string();

    seen.lock().expect("seen").push(Seen {
        method: method.clone(),
        path: path.clone(),
        body: body.clone(),
    });

    let reply = handler(&method, &path, &body);
    let head = format!(
        "HTTP/1.1 {} {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        reply.status,
        reason(reply.status),
        reply.body.len()
    );
    let _ = stream.write_all(head.as_bytes());
    let _ = stream.write_all(reply.body.as_bytes());
    let _ = stream.flush();
    let _ = stream.shutdown(Shutdown::Write);
}

fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        201 => "Created",
        400 => "Bad Request",
        401 => "Unauthorized",
        409 => "Conflict",
        500 => "Internal Server Error",
        _ => "Status",
    }
}

/// The provider catalogue decodes the shape the gateway returns.
#[test]
fn provider_catalog_decodes() {
    let payload = r#"{
        "providers": [
            {
                "id": "openai",
                "name": "OpenAI",
                "auth_type": "api_key",
                "format": "openai",
                "needs_key": true,
                "base_url": "https://api.openai.com/v1"
            },
            {
                "id": "local",
                "name": "Local",
                "auth_type": "none",
                "format": "openai",
                "needs_key": false,
                "base_url": "http://127.0.0.1:8080/v1"
            },
            {
                "id": "oauthp",
                "name": "OAuth Provider",
                "auth_type": "oauth",
                "format": "openai",
                "needs_key": true,
                "base_url": "https://example.com"
            }
        ]
    }"#;

    let catalog: api::ProviderCatalog = serde_json::from_str(payload).expect("catalog");
    assert_eq!(catalog.providers.len(), 3);
    assert_eq!(catalog.providers[0].id, "openai");
    assert!(catalog.providers[0].needs_key);
    assert!(!catalog.providers[1].needs_key);
    assert_eq!(catalog.providers[2].auth_type, "oauth");
}

/// The create-connection body omits the key when the provider needs none and
/// carries it when it does.
#[test]
fn create_connection_body_serializes_key_only_when_needed() {
    let with_key = api::CreateConnectionBody {
        provider: "openai".to_string(),
        name: "My OpenAI".to_string(),
        api_key: "secret-value".to_string(),
        priority: Some(5),
        is_active: Some(true),
    };
    let json = serde_json::to_value(&with_key).expect("json");
    assert_eq!(json["provider"], "openai");
    assert_eq!(json["name"], "My OpenAI");
    assert_eq!(json["api_key"], "secret-value");
    assert_eq!(json["priority"], 5);
    assert_eq!(json["is_active"], true);

    let without_key = api::CreateConnectionBody {
        provider: "local".to_string(),
        name: "Local".to_string(),
        api_key: String::new(),
        priority: None,
        is_active: Some(true),
    };
    let json = serde_json::to_value(&without_key).expect("json");
    assert_eq!(json["provider"], "local");
    assert_eq!(json["name"], "Local");
    assert!(json.get("api_key").is_none(), "empty key must be omitted");
    assert!(
        json.get("priority").is_none(),
        "absent priority must be omitted"
    );
    assert_eq!(json["is_active"], true);
}

/// Creating an active connection posts the right body, the gateway may report
/// that a restart is required, and the refreshed list contains the new row.
#[test]
fn add_connection_posts_body_and_refresh_sees_new_connection() {
    let state = Arc::new(Mutex::new(Vec::<serde_json::Value>::new()));
    let gateway = FakeGateway::start({
        let state = Arc::clone(&state);
        move |method, path, body| match (method, path) {
            ("GET", "/api/providers") => Reply::json(
                200,
                r#"{"providers":[{"id":"openai","name":"OpenAI","auth_type":"api_key","format":"openai","needs_key":true,"base_url":"https://api.openai.com/v1"}]}"#,
            ),
            ("POST", "/api/connections") => {
                let value: serde_json::Value = serde_json::from_str(body).expect("body json");
                state.lock().expect("state").push(value);
                Reply::json(
                    201,
                    r#"{"status":"ok","id":"conn-new","provider":"openai","restart_required":true}"#,
                )
            }
            ("GET", "/api/connections") => Reply::json(
                200,
                r#"{"connections":[{"id":"conn-new","provider":"openai","name":"My OpenAI","auth_type":"api_key","is_active":true,"priority":5,"has_credential":true,"servable":false}]}"#,
            ),
            _ => Reply::json(404, r#"{"error":"not found"}"#),
        }
    });

    let client = api::ApiClient::new(gateway.base(), OPERATOR_KEY);

    let catalog: api::ProviderCatalog = client.get_json("/api/providers").expect("catalog");
    assert_eq!(catalog.providers.len(), 1);
    assert_eq!(catalog.providers[0].id, "openai");

    let body = api::CreateConnectionBody {
        provider: "openai".to_string(),
        name: "My OpenAI".to_string(),
        api_key: "sk-test".to_string(),
        priority: Some(5),
        is_active: Some(true),
    };
    let created: serde_json::Value = client.post_json("/api/connections", &body).expect("create");
    assert_eq!(created["status"], "ok");
    assert_eq!(created["id"], "conn-new");
    assert_eq!(created["provider"], "openai");
    assert_eq!(
        created["restart_required"], true,
        "an active connection must require a restart before it serves traffic"
    );

    let refreshed: api::ConnectionsResponse =
        client.get_json("/api/connections").expect("connections");
    assert_eq!(refreshed.connections.len(), 1);
    assert_eq!(refreshed.connections[0].id, "conn-new");
    assert_eq!(refreshed.connections[0].name, "My OpenAI");
    assert!(refreshed.connections[0].has_credential);

    let seen = gateway.requests();
    assert!(
        seen.iter()
            .any(|r| r.method == "POST" && r.path == "/api/connections")
    );
    assert!(
        seen.iter()
            .any(|r| r.method == "GET" && r.path == "/api/connections")
    );

    let posted = state.lock().expect("state");
    assert_eq!(posted.len(), 1);
    assert_eq!(posted[0]["provider"], "openai");
    assert_eq!(posted[0]["name"], "My OpenAI");
    assert_eq!(posted[0]["api_key"], "sk-test");
    assert_eq!(posted[0]["priority"], 5);
    assert_eq!(posted[0]["is_active"], true);
}

/// A server-side validation error is surfaced through the client's message.
#[test]
fn create_connection_surfaces_server_error_message() {
    let gateway = FakeGateway::start(|method, path, _body| match (method, path) {
        ("POST", "/api/connections") => Reply::json(400, r#"{"error":"provider not found"}"#),
        _ => Reply::json(404, r#"{"error":"not found"}"#),
    });

    let client = api::ApiClient::new(gateway.base(), OPERATOR_KEY);
    let body = api::CreateConnectionBody {
        provider: "missing".to_string(),
        name: "Broken".to_string(),
        api_key: String::new(),
        priority: None,
        is_active: Some(true),
    };
    let error = client
        .post_json::<serde_json::Value, _>("/api/connections", &body)
        .expect_err("must fail");
    assert_eq!(error.message, "provider not found");
}

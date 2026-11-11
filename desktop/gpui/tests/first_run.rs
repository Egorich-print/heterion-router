//! Первый запуск: выдача первого ключа с экрана входа.
//!
//! На свежей базе (нет ключей) шлюз открыт — `GET /api/keys` без bearer
//! отдаёт 200 и пустой список, а `POST /api/keys` создаёт ключ и возвращает
//! полный секрет один раз. Экран входа должен это распознать, предложить
//! создать ключ и сохранить его автоматически.

#![allow(dead_code)]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

#[path = "../src/api.rs"]
mod api;
#[path = "../src/app.rs"]
mod app;
#[path = "../src/secret.rs"]
mod secret;
#[path = "../src/theme.rs"]
mod theme;
#[path = "../src/views/mod.rs"]
mod views;
#[path = "../src/widgets/mod.rs"]
mod widgets;

/// Секрет, который фейковый шлюз «выпускает». Это фикстура теста, не ключ.
const ISSUED_SECRET: &str = "sk-first-run-test-secret";

/// Ответ шлюза.
struct Reply {
    status: u16,
    body: String,
}

impl Reply {
    fn json(status: u16, body: impl Into<String>) -> Self {
        Self {
            status,
            body: body.into(),
        }
    }
}

/// Один принятый запрос.
#[derive(Clone, Debug)]
struct Seen {
    method: String,
    path: String,
    authorization: Option<String>,
    body: String,
}

/// Фейковый шлюз: 200 + пустой список ключей, 201 с секретом на создание.
struct FakeGateway {
    base: String,
    seen: Arc<Mutex<Vec<Seen>>>,
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl FakeGateway {
    fn start() -> Self {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind");
        let port = listener.local_addr().expect("addr").port();
        listener.set_nonblocking(true).expect("nonblocking");

        let seen: Arc<Mutex<Vec<Seen>>> = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));

        let worker = {
            let seen = Arc::clone(&seen);
            let stop = Arc::clone(&stop);
            std::thread::spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    match listener.accept() {
                        Ok((stream, _)) => {
                            let seen = Arc::clone(&seen);
                            std::thread::spawn(move || serve(stream, &seen));
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
        self.seen.lock().expect("poisoned").clone()
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

fn serve(mut stream: TcpStream, seen: &Mutex<Vec<Seen>>) {
    // Принятый сокет наследует O_NONBLOCK от слушателя на macOS — вернуть
    // в блокирующий режим, иначе чтение может дать WouldBlock и соединение
    // закроется без ответа.
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

    let mut body = vec![0u8; content_length];
    if content_length > 0 {
        let _ = reader.read_exact(&mut body);
    }
    let body_text = String::from_utf8_lossy(&body).to_string();

    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("GET").to_string();
    let path = parts
        .next()
        .unwrap_or("/")
        .split('?')
        .next()
        .unwrap_or("/")
        .to_string();

    seen.lock().expect("poisoned").push(Seen {
        method: method.clone(),
        path: path.clone(),
        authorization,
        body: body_text,
    });

    let reply = match (method.as_str(), path.as_str()) {
        // Свежая база: ключей нет, аутентификация открыта.
        ("GET", "/api/keys") => Reply::json(200, r#"{"keys":[]}"#),
        // Первый ключ: шлюз возвращает секрет один раз.
        ("POST", "/api/keys") => Reply::json(
            201,
            format!(
                r#"{{"status":"ok","id":"key-1","key":"{ISSUED_SECRET}","prefix":"sk-first","warning":"copy now"}}"#
            ),
        ),
        ("GET", "/healthz") => Reply::json(200, r#"{"status":"ok","backend":"routing"}"#),
        _ => Reply::json(404, r#"{"error":"no route"}"#),
    };

    let response = format!(
        "HTTP/1.1 {} OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        reply.status,
        reply.body.len(),
        reply.body
    );
    let _ = stream.write_all(response.as_bytes());
    let _ = stream.flush();
    let _ = stream.shutdown(Shutdown::Write);
}

/// Временная data dir: файл ключа пишется сюда, а не в реальную директорию.
struct DataDir {
    _lock: std::sync::MutexGuard<'static, ()>,
    previous: Option<String>,
    _dir: tempfile::TempDir,
}

impl DataDir {
    fn new() -> Self {
        let lock = secret::ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let dir = tempfile::tempdir().expect("temp dir");
        let previous = std::env::var("HETERION_ROUTER_DATA_DIR").ok();
        // SAFETY: доступ к переменной сериализован ENV_LOCK.
        unsafe { std::env::set_var("HETERION_ROUTER_DATA_DIR", dir.path()) };
        Self {
            _lock: lock,
            previous,
            _dir: dir,
        }
    }
}

impl Drop for DataDir {
    fn drop(&mut self) {
        match self.previous.take() {
            // SAFETY: тот же замок, что и в new.
            Some(value) => unsafe { std::env::set_var("HETERION_ROUTER_DATA_DIR", value) },
            None => unsafe { std::env::remove_var("HETERION_ROUTER_DATA_DIR") },
        }
    }
}

#[test]
fn fresh_install_is_detected_from_an_open_empty_keys_response() {
    let gateway = FakeGateway::start();
    let client = api::ApiClient::new(gateway.base(), "");

    let result = client.get_json::<api::KeysResponse>("/api/keys");
    assert_eq!(app::classify_first_run(result), app::FirstRunState::Fresh);
}

#[test]
fn existing_keys_mean_normal_login() {
    let gateway = FakeGateway::start_with_keys();
    let client = api::ApiClient::new(gateway.base(), "");

    let result = client.get_json::<api::KeysResponse>("/api/keys");
    assert_eq!(app::classify_first_run(result), app::FirstRunState::Normal);
}

#[test]
fn unauthorized_means_normal_login() {
    let gateway = FakeGateway::start_unauthorized();
    let client = api::ApiClient::new(gateway.base(), "");

    let result = client.get_json::<api::KeysResponse>("/api/keys");
    assert_eq!(app::classify_first_run(result), app::FirstRunState::Normal);
}

#[test]
fn creating_the_first_key_saves_it_to_the_credential_file() {
    let _data = DataDir::new();
    let gateway = FakeGateway::start();
    let client = api::ApiClient::new(gateway.base(), "");

    let created: api::CreatedKey = client
        .post_json(
            "/api/keys",
            &api::CreateKeyBody {
                name: "operator".to_string(),
            },
        )
        .expect("create key");

    // Секрет сохраняется тем же путём, что и обычный вход.
    secret::store(&created.key).expect("store");
    assert_eq!(secret::load().as_deref(), Some(ISSUED_SECRET));

    // В запросе было осмысленное непустое имя.
    let post = gateway
        .requests()
        .into_iter()
        .find(|req| req.method == "POST" && req.path == "/api/keys")
        .expect("post /api/keys");
    let body: serde_json::Value = serde_json::from_str(&post.body).expect("json body");
    let name = body
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    assert!(!name.trim().is_empty(), "имя ключа не должно быть пустым");
}

impl FakeGateway {
    /// Шлюз, на котором ключи уже есть (GET /api/keys отдаёт непустой список).
    fn start_with_keys() -> Self {
        Self::start_with(|method, path| match (method, path) {
            ("GET", "/api/keys") => Reply::json(
                200,
                r#"{"keys":[{"id":"k1","name":"operator","prefix":"sk-x","is_active":true}]}"#,
            ),
            _ => Reply::json(404, r#"{"error":"no route"}"#),
        })
    }

    /// Шлюз, на котором аутентификация закрыта (401).
    fn start_unauthorized() -> Self {
        Self::start_with(|_, path| match path {
            "/api/keys" => Reply::json(401, r#"{"error":"unauthorized"}"#),
            _ => Reply::json(404, r#"{"error":"no route"}"#),
        })
    }

    fn start_with<F>(handler: F) -> Self
    where
        F: Fn(&str, &str) -> Reply + Send + Sync + 'static,
    {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind");
        let port = listener.local_addr().expect("addr").port();
        listener.set_nonblocking(true).expect("nonblocking");

        let seen: Arc<Mutex<Vec<Seen>>> = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));

        let worker = {
            let seen = Arc::clone(&seen);
            let stop = Arc::clone(&stop);
            let handler = Arc::new(handler);
            std::thread::spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    match listener.accept() {
                        Ok((stream, _)) => {
                            let seen = Arc::clone(&seen);
                            let handler = Arc::clone(&handler);
                            std::thread::spawn(move || serve_with(stream, &seen, handler.as_ref()));
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
}

fn serve_with(
    mut stream: TcpStream,
    seen: &Mutex<Vec<Seen>>,
    handler: &dyn Fn(&str, &str) -> Reply,
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

    let mut body = vec![0u8; content_length];
    if content_length > 0 {
        let _ = reader.read_exact(&mut body);
    }
    let body_text = String::from_utf8_lossy(&body).to_string();

    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("GET").to_string();
    let path = parts
        .next()
        .unwrap_or("/")
        .split('?')
        .next()
        .unwrap_or("/")
        .to_string();

    seen.lock().expect("poisoned").push(Seen {
        method: method.clone(),
        path: path.clone(),
        authorization,
        body: body_text,
    });

    let reply = handler(&method, &path);
    let response = format!(
        "HTTP/1.1 {} OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        reply.status,
        reply.body.len(),
        reply.body
    );
    let _ = stream.write_all(response.as_bytes());
    let _ = stream.flush();
    let _ = stream.shutdown(Shutdown::Write);
}

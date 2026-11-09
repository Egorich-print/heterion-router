//! The real shell, a real click, a real request.
//!
//! [`button_click`] proves that the shared widget answers a click. This test
//! proves the whole chain: the shell renders the Overview, the Refresh button
//! is found by its debug selector, a click at its centre goes through gpui's
//! hit-testing, and the gateway is asked a second time.
//!
//! The shell reads the stored operator key from the data directory, so the
//! test points `HETERION_ROUTER_DATA_DIR` at a temporary directory that holds
//! the test's own fixture key: the shell signs in with it, and the key stored
//! on the machine is never read and never sent.

#![allow(dead_code)]
#![allow(unused_imports)]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
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

use gpui::prelude::*;
use gpui::{AppContext, Modifiers, TestAppContext};

use app::AppShell;
use widgets::button_selector;

/// The key the fake gateway expects. A fixture, not a credential.
const OPERATOR_KEY: &str = "test-operator-key";

/// A loopback HTTP/1.1 server that answers `/api/overview` and counts the
/// requests it got.
struct FakeGateway {
    base: String,
    overview_requests: Arc<AtomicUsize>,
}

impl FakeGateway {
    /// Start the server.
    fn start() -> Self {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("the fake gateway must bind");
        let port = listener
            .local_addr()
            .expect("the fake gateway must have an address")
            .port();
        let overview_requests = Arc::new(AtomicUsize::new(0));

        let counter = Arc::clone(&overview_requests);
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { break };
                let counter = Arc::clone(&counter);
                std::thread::spawn(move || serve(stream, counter));
            }
        });

        Self {
            base: format!("http://127.0.0.1:{port}"),
            overview_requests,
        }
    }

    /// The base URL of the fake gateway.
    fn base(&self) -> &str {
        &self.base
    }

    /// The number of `/api/overview` requests so far.
    fn overview_requests(&self) -> usize {
        self.overview_requests.load(Ordering::SeqCst)
    }
}

/// Answer one request.
fn serve(mut stream: TcpStream, counter: Arc<AtomicUsize>) {
    let mut reader = BufReader::new(stream.try_clone().expect("the stream must be cloneable"));
    let mut request_line = String::new();
    if reader.read_line(&mut request_line).is_err() {
        return;
    }

    // Drain the headers, otherwise the client waits for the answer.
    let mut line = String::new();
    loop {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) => break,
            Ok(_) => {
                if line.trim().is_empty() {
                    break;
                }
            }
            Err(_) => break,
        }
    }

    let path = request_line.split_whitespace().nth(1).unwrap_or("/");
    let path = path.split('?').next().unwrap_or("/");

    let body = if path == "/api/overview" {
        counter.fetch_add(1, Ordering::SeqCst);
        r#"{"status":"ok","backend":"routing","backends":["routing"],"counts":{"combos":3,"connections_active":1,"connections_total":1,"api_keys":1}}"#
    } else {
        r#"{"status":"ok"}"#
    };

    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    );
    let _ = stream.write_all(response.as_bytes());
    let _ = stream.flush();
}

/// The data directory the shell reads during the test.
///
/// It holds the test's own fixture key, so the shell signs in with it: the
/// key stored on the machine is never read and never sent anywhere. The
/// previous value of the variable is restored when the guard is dropped.
struct DataDir {
    _lock: std::sync::MutexGuard<'static, ()>,
    previous: Option<String>,
    _dir: tempfile::TempDir,
}

impl DataDir {
    /// Point the shell at a fresh, empty data directory.
    fn set() -> Self {
        // The lock is the one the module's own tests hold: the variable is
        // process-wide, and a test that reads it while another one writes it
        // would see the other one's directory.
        let lock = secret::ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let previous = std::env::var("HETERION_ROUTER_DATA_DIR").ok();
        let dir = tempfile::tempdir().expect("temp dir");
        // SAFETY: the lock serializes every access to this variable.
        unsafe { std::env::set_var("HETERION_ROUTER_DATA_DIR", dir.path()) };
        secret::store(OPERATOR_KEY).expect("the fixture key must be storable");
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
            // SAFETY: same lock as in `set`.
            Some(value) => unsafe { std::env::set_var("HETERION_ROUTER_DATA_DIR", value) },
            // SAFETY: same lock as in `set`.
            None => unsafe { std::env::remove_var("HETERION_ROUTER_DATA_DIR") },
        }
    }
}

/// The Refresh button of the Overview screen re-queries the gateway.
#[gpui::test]
fn clicking_refresh_asks_the_gateway_again(cx: &mut TestAppContext) {
    let gateway = FakeGateway::start();
    let _data_dir = DataDir::set();
    cx.set_global(api::Session::new(gateway.base()));

    let (_shell, cx) = cx.add_window_view(|_window, cx| AppShell::new(cx));
    cx.run_until_parked();

    // The first load already asked — but the request runs on a worker
    // thread, so the counter is not filled in the same tick. Wait for it
    // instead of asserting right away.
    // The budget is generous on purpose: the first run of a freshly built
    // binary on macOS goes through the signature check (`trustd`), which
    // can hold the process up for a long while.
    let deadline = std::time::Instant::now() + Duration::from_secs(90);
    while gateway.overview_requests() == 0 {
        assert!(
            std::time::Instant::now() < deadline,
            "the shell must load the overview on its own"
        );
        cx.run_until_parked();
        std::thread::sleep(Duration::from_millis(5));
    }
    let before = gateway.overview_requests();

    let bounds = cx
        .debug_bounds(Box::leak(
            button_selector("overview-refresh").into_boxed_str(),
        ))
        .expect("the Refresh button must be findable by its selector");
    assert!(
        bounds.size.width > gpui::px(0.0) && bounds.size.height > gpui::px(0.0),
        "the Refresh button must have a real rectangle"
    );

    cx.simulate_click(bounds.center(), Modifiers::none());

    // The click must reach the gateway.
    // The budget is generous on purpose: the first run of a freshly built
    // binary on macOS goes through the signature check (`trustd`), which
    // can hold the process up for a long while.
    let deadline = std::time::Instant::now() + Duration::from_secs(90);
    while gateway.overview_requests() <= before {
        assert!(
            std::time::Instant::now() < deadline,
            "the click must make the shell ask the gateway again"
        );
        cx.run_until_parked();
        std::thread::sleep(Duration::from_millis(5));
    }
}

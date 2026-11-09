//! The Overview screen's Refresh button against a fake gateway.
//!
//! `desktop/gpui` is a binary crate, so an integration test cannot import its
//! modules. The module files are included by path instead — the same trick
//! `tests/api_client.rs` uses — so the test drives the very code the shell
//! runs: the Overview view, the typed client and the request plumbing.
//!
//! The fake gateway answers `GET /api/overview` with the shape of
//! `admin::overview` and a number of active combos the test controls. The
//! test runs the action of the Refresh button and checks that the view
//! shows the new number of active combos.

// The crate's modules are included whole, but this test uses only a part of
// them: the unused widgets are not dead code here.
#![allow(dead_code)]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

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

use gpui::{AppContext, Entity, TestAppContext};

use views::overview::OverviewView;

/// The key the fake gateway expects. A fixture, not a credential.
const OPERATOR_KEY: &str = "test-operator-key";

/// A loopback HTTP/1.1 server that answers `/api/overview` with the number
/// of active combos the test sets, and can hold an answer back so that two
/// requests come back in the wrong order.
struct FakeGateway {
    base: String,
    combos: Arc<AtomicUsize>,
    hold: Arc<Mutex<Option<Arc<AtomicBool>>>>,
    overview_requests: Arc<AtomicUsize>,
    answers: Arc<AtomicUsize>,
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl FakeGateway {
    /// Start a server that reports `combos` active combos.
    fn start(combos: usize) -> Self {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("the fake gateway must bind");
        let port = listener
            .local_addr()
            .expect("the fake gateway must have an address")
            .port();
        listener
            .set_nonblocking(true)
            .expect("the fake gateway must go non-blocking");

        let combos = Arc::new(AtomicUsize::new(combos));
        let hold = Arc::new(Mutex::new(None));
        let overview_requests = Arc::new(AtomicUsize::new(0));
        let answers = Arc::new(AtomicUsize::new(0));
        let stop = Arc::new(AtomicBool::new(false));

        let worker = {
            let combos = Arc::clone(&combos);
            let hold = Arc::clone(&hold);
            let overview_requests = Arc::clone(&overview_requests);
            let answers = Arc::clone(&answers);
            let stop = Arc::clone(&stop);
            std::thread::spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    match listener.accept() {
                        Ok((stream, _)) => {
                            // One thread per connection: a held-back answer
                            // must not hold back the next request as well.
                            let combos = Arc::clone(&combos);
                            let hold = Arc::clone(&hold);
                            let overview_requests = Arc::clone(&overview_requests);
                            let answers = Arc::clone(&answers);
                            std::thread::spawn(move || {
                                serve(stream, &combos, &hold, &overview_requests, &answers)
                            });
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
            combos,
            hold,
            overview_requests,
            answers,
            stop,
            worker: Some(worker),
        }
    }

    /// The base URL of the fake gateway.
    fn base(&self) -> &str {
        &self.base
    }

    /// The number of active combos the gateway reports from now on.
    fn set_combos(&self, combos: usize) {
        self.combos.store(combos, Ordering::SeqCst);
    }

    /// Hold the next answer back until the returned flag is set.
    ///
    /// The test decides when the answer is released, so the order of the two
    /// answers does not depend on how long a sleep happens to take — a busy
    /// machine cannot make this test fail.
    fn hold_next_answer(&self) -> Arc<AtomicBool> {
        let release = Arc::new(AtomicBool::new(false));
        *self
            .hold
            .lock()
            .expect("the hold slot must not be poisoned") = Some(Arc::clone(&release));
        release
    }

    /// How many `/api/overview` requests the gateway has answered.
    fn overview_requests(&self) -> usize {
        self.overview_requests.load(Ordering::SeqCst)
    }

    /// How many answers the gateway has actually sent.
    fn answers(&self) -> usize {
        self.answers.load(Ordering::SeqCst)
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
fn serve(
    mut stream: TcpStream,
    combos: &AtomicUsize,
    hold: &Mutex<Option<Arc<AtomicBool>>>,
    overview_requests: &AtomicUsize,
    answers: &AtomicUsize,
) {
    // The listener is non-blocking so that the accept loop can poll it, and
    // on macOS the accepted socket inherits that flag. A read that finds no
    // data yet would then fail with `WouldBlock` instead of waiting for it,
    // and the gateway would close the connection without an answer — the
    // client sees an incomplete message and reports a transport error.
    // Put the connection back into blocking mode; the read timeout keeps a
    // silent client from wedging the worker forever.
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

    if content_length > 0 {
        let mut body = vec![0u8; content_length];
        let _ = reader.read_exact(&mut body);
    }

    let path = request_line.split_whitespace().nth(1).unwrap_or("/");
    let path = path.split('?').next().unwrap_or("/").to_string();

    let authorized = authorization.as_deref() == Some(&format!("Bearer {OPERATOR_KEY}"));
    let (status, body) = if path == "/api/overview" && authorized {
        // The number is taken when the request arrives, not when the answer
        // goes out: a held-back answer must carry the number of its own
        // request.
        let count = combos.load(Ordering::SeqCst);
        overview_requests.fetch_add(1, Ordering::SeqCst);
        // A held-back answer waits for the test to release it: the order of
        // the two answers must not depend on how long a sleep happens to
        // take on a busy machine. The lock is taken and released before the
        // wait, so the next request is never blocked by the held one.
        let release = {
            let mut slot = hold.lock().expect("the hold slot must not be poisoned");
            slot.take()
        };
        if let Some(release) = release {
            let deadline = Instant::now() + Duration::from_secs(120);
            while !release.load(Ordering::SeqCst) && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(1));
            }
        }
        (
            "200 OK",
            format!(
                r#"{{"status":"ok","backend":"routing","backends":["routing"],"counts":{{"combos":{count},"connections_active":1,"connections_total":2,"api_keys":3}}}}"#
            ),
        )
    } else {
        (
            "401 Unauthorized",
            r#"{"error":"unauthorized"}"#.to_string(),
        )
    };

    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    answers.fetch_add(1, Ordering::SeqCst);
    let _ = stream.write_all(response.as_bytes());
    let _ = stream.flush();
    // Only the writing half is shut down: a full shutdown may answer with a
    // reset, and the client would lose the answer it just received.
    let _ = stream.shutdown(Shutdown::Write);
}

/// Point the session at the fake gateway and sign in.
fn sign_in(cx: &mut TestAppContext, gateway: &FakeGateway) {
    cx.set_global(api::Session::new(gateway.base()));
    cx.update_global::<api::Session, _>(|session, _| session.sign_in(OPERATOR_KEY));
}

/// The number of active combos the view shows right now.
fn combos(cx: &TestAppContext, view: &Entity<OverviewView>) -> Option<usize> {
    cx.read(|app| view.read(app).data.as_ref().map(|data| data.counts.combos))
}

/// Run the app until `condition` holds.
fn wait_for(cx: &mut TestAppContext, condition: impl Fn(&TestAppContext) -> bool) {
    // The budget is generous on purpose. The wait itself is milliseconds,
    // but the first run of a freshly built binary on macOS goes through the
    // signature check (`trustd`), which can hold the process up for a long
    // while — a loaded machine must not turn a passing test into a failing
    // one.
    let deadline = Instant::now() + Duration::from_secs(90);
    loop {
        if condition(cx) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for the overview to load"
        );
        cx.run_until_parked();
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// The Refresh button re-queries `/api/overview`, and the card shows the
/// number of active combos the gateway reports at that moment.
#[gpui::test]
fn the_refresh_button_shows_the_new_number_of_active_combos(cx: &mut TestAppContext) {
    let gateway = FakeGateway::start(3);
    sign_in(cx, &gateway);

    let view = cx.new(OverviewView::new);

    // The first load fills the card with what the gateway reported.
    wait_for(cx, |cx| combos(cx, &view) == Some(3));

    // The gateway learns about one more active combo.
    gateway.set_combos(4);

    // The Refresh button: the very action the button runs.
    let refresh = views::overview::refresh_action(view.clone());
    cx.update(|app| refresh(app));

    // The card shows the new number.
    wait_for(cx, |cx| combos(cx, &view) == Some(4));
    assert_eq!(gateway.overview_requests(), 2, "the button must ask again");
}

/// Two clicks in a row must not show the older answer last: the reply of the
/// superseded request is dropped.
#[gpui::test]
fn a_late_reply_does_not_overwrite_the_newer_number(cx: &mut TestAppContext) {
    let gateway = FakeGateway::start(3);
    sign_in(cx, &gateway);

    let view = cx.new(OverviewView::new);
    wait_for(cx, |cx| combos(cx, &view) == Some(3));

    // The first refresh is held back, the second one is quick: the older
    // answer lands after the newer one.
    let release = gateway.hold_next_answer();
    gateway.set_combos(4);
    let refresh = views::overview::refresh_action(view.clone());
    cx.update(|app| refresh(app));
    wait_for(cx, |_| gateway.overview_requests() == 2);

    gateway.set_combos(5);
    cx.update(|app| refresh(app));

    // The newer answer wins ...
    wait_for(cx, |cx| combos(cx, &view) == Some(5));

    // ... and the older one, which lands later, is dropped.
    release.store(true, Ordering::SeqCst);
    wait_for(cx, |_| gateway.answers() == 3);
    // The late answer has been sent; give the shell the moment it needs to
    // take it in — a test that fails here would be a test that is too eager.
    std::thread::sleep(Duration::from_millis(100));
    cx.run_until_parked();
    assert_eq!(combos(cx, &view), Some(5));
}

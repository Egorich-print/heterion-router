//! The gateway the desktop shell talks to — and the one it owns.
//!
//! Two modes:
//!
//! * **release** — the bundle carries `heterion-router-gateway` next to the
//!   app executable. The shell spawns it on the sidecar port and the gateway
//!   exits on its own once the parent is gone (`HETERION_ROUTER_PARENT_PID`).
//! * **debug** — the operator runs the gateway themselves; the shell only
//!   talks to it.
//!
//! # Ports
//!
//! | Consumer | Port | Env |
//! |---|---|---|
//! | the gateway's own default | 20129 | `HETERION_ROUTER_PORT` / `OMNIROUTE_RUST_PORT` |
//! | the launchd service (this machine) | 20128 | — the shell never starts it |
//! | the sidecar the shell owns | 20129 | `HETERION_ROUTER_SIDECAR_PORT` / `OMNIROUTE_SIDECAR_PORT` |
//! | the instance lock | 20130 | `HETERION_ROUTER_INSTANCE_LOCK_PORT` |
//!
//! The gateway binds `127.0.0.1` unless `HETERION_ROUTER_HOST` says
//! otherwise, so every URL here is loopback-only.
//!
//! Two rules keep the ports apart:
//!
//! * The sidecar always gets an **explicit** port. A globally configured
//!   `HETERION_ROUTER_PORT=20128` (the installed service) would otherwise be
//!   inherited by the sidecar, and the sidecar would die with `AddrInUse`
//!   against the service. The shell therefore sets `HETERION_ROUTER_PORT` and
//!   `OMNIROUTE_RUST_PORT` on the child itself.
//! * The gateway's own `HETERION_ROUTER_PORT` is honoured only when the
//!   shell does *not* own the gateway (debug). That is exactly the variable
//!   the gateway itself reads, so shell and gateway agree by construction.
//!
//! # Lifetime
//!
//! The gateway watches its parent pid (`HETERION_ROUTER_PARENT_PID`) and
//! exits on its own once that process is gone — that is the contract in
//! `crates/heterion-router-gateway/src/main.rs`. It is the safety net for a
//! shell that dies without unwinding (`SIGKILL`, a panic in a foreign
//! thread): the gateway notices within its 2 s poll and exits, so no orphan
//! keeps holding the sidecar port. On a normal quit the shell kills the
//! gateway itself, immediately — see [`Sidecar::shutdown`].

use std::io::{BufRead, BufReader, Read};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::Context;

/// Port the launchd service listens on.
///
/// The shell never starts the service; the constant is here so the port
/// table in the module docs has a name.
#[allow(dead_code)]
pub const SERVICE_PORT: u16 = 20128;

/// Port a sidecar gateway listens on when nothing overrides it.
///
/// The same number as the gateway's own default ([`GATEWAY_DEFAULT_PORT`]):
/// the service owns 20128, so the shell's own gateway uses 20129 and the two
/// never collide.
pub const SIDECAR_PORT: u16 = 20129;

/// The gateway's own default port (`HETERION_ROUTER_PORT`, `20129` in
/// `crates/heterion-router-config/src/config.rs`).
pub const GATEWAY_DEFAULT_PORT: u16 = SIDECAR_PORT;

/// Port used to detect that another shell is already running.
pub const INSTANCE_LOCK_PORT: u16 = 20130;

/// Name of the gateway executable, next to the app executable.
pub const GATEWAY_EXECUTABLE: &str = "heterion-router-gateway";

/// How long the shell waits for a gateway to answer `GET /healthz`.
///
/// The bound is generous because the first start migrates the database; the
/// probe gives up rather than hanging the launch.
pub const READINESS_TIMEOUT: Duration = Duration::from_secs(10);

/// How often the readiness probe retries.
const READINESS_INTERVAL: Duration = Duration::from_millis(100);

/// How long a single readiness request may take.
const READINESS_REQUEST_TIMEOUT: Duration = Duration::from_secs(2);

/// The port a gateway the shell owns listens on.
///
/// `HETERION_ROUTER_SIDECAR_PORT` → `OMNIROUTE_SIDECAR_PORT` → 20129. Same
/// spelling and order as the Tauri shell this one replaces.
pub fn sidecar_port() -> u16 {
    env_port(&["HETERION_ROUTER_SIDECAR_PORT", "OMNIROUTE_SIDECAR_PORT"]).unwrap_or(SIDECAR_PORT)
}

/// The port the shell talks to.
///
/// * **release** — the sidecar the shell owns, so [`sidecar_port`].
/// * **debug** — a gateway the operator runs: the gateway's own contract
///   (`HETERION_ROUTER_PORT` → `OMNIROUTE_RUST_PORT`) wins, because that is
///   the very variable the gateway binds. Without it the gateway listens on
///   its own default (20129); the installed launchd service is on 20128 and
///   is selected with `HETERION_ROUTER_PORT=20128`.
pub fn gateway_port() -> u16 {
    if cfg!(debug_assertions) {
        env_port(&["HETERION_ROUTER_PORT", "OMNIROUTE_RUST_PORT"]).unwrap_or(GATEWAY_DEFAULT_PORT)
    } else {
        sidecar_port()
    }
}

/// The base URL of the gateway the shell talks to.
pub fn base_url() -> String {
    format!("http://127.0.0.1:{}", gateway_port())
}

/// The first non-empty value of `keys`, parsed as a port.
///
/// Mirrors the gateway's own resolution (`crates/heterion-router-config`):
/// the primary spelling wins, the legacy one fills the gap, and a value that
/// does not parse falls back to the caller's default. Port `0` is rejected —
/// it would make the gateway bind an ephemeral port nobody can reach.
fn env_port(keys: &[&str]) -> Option<u16> {
    let value = keys
        .iter()
        .map(|key| std::env::var(key).unwrap_or_default())
        .find(|value| !value.trim().is_empty())?;

    value.trim().parse::<u16>().ok().filter(|port| *port != 0)
}

/// Proof that this process is the only shell.
///
/// The lock is a bound TCP listener on the loopback: whoever holds it owns
/// the shell. Dropping it releases the lock, so the guard has to be kept
/// alive for as long as the shell runs.
pub struct InstanceLock {
    listener: TcpListener,
}

impl InstanceLock {
    /// Take the instance lock.
    ///
    /// `Ok(None)` — another shell already holds the lock; the caller must
    /// not start a second gateway. `Err` — the lock could not be taken for
    /// a reason other than "somebody else has it".
    pub fn acquire() -> anyhow::Result<Option<Self>> {
        match TcpListener::bind(("127.0.0.1", instance_lock_port())) {
            Ok(listener) => Ok(Some(Self { listener })),
            Err(error) if error.kind() == std::io::ErrorKind::AddrInUse => {
                tracing::warn!(
                    target: "shell",
                    port = instance_lock_port(),
                    "another Heterion Router shell is already running"
                );
                Ok(None)
            }
            Err(error) => Err(error).with_context(|| {
                format!(
                    "binding the instance lock on 127.0.0.1:{}",
                    instance_lock_port()
                )
            }),
        }
    }
}

/// Take the instance lock: `Ok(None)` means another shell already runs.
///
/// The caller must treat `None` as "do not start a second gateway":
///
/// ```no_run
/// let Some(_lock) = sidecar::acquire_instance_lock()? else {
///     tracing::warn!("another Heterion Router shell is already running — not starting a second gateway");
///     return Ok(());
/// };
/// ```
///
/// The returned guard releases the lock when it is dropped.
pub fn acquire_instance_lock() -> anyhow::Result<Option<InstanceLock>> {
    InstanceLock::acquire()
}

/// Claims the instance lock. `Ok(None)` means another shell already runs.
///
/// Kept for the crate's existing callers; new code should use
/// [`acquire_instance_lock`], whose `None` carries the typed guard instead of
/// a bare listener.
#[allow(dead_code)]
pub fn ensure_single_instance() -> anyhow::Result<Option<TcpListener>> {
    Ok(acquire_instance_lock()?.map(|lock| lock.listener))
}

/// The port the instance lock is held on.
fn instance_lock_port() -> u16 {
    env_port(&["HETERION_ROUTER_INSTANCE_LOCK_PORT"]).unwrap_or(INSTANCE_LOCK_PORT)
}

/// A running gateway owned by this process.
pub struct Sidecar {
    child: Child,
    stopped: bool,
}

impl Sidecar {
    /// The gateway's process id.
    pub fn pid(&self) -> u32 {
        self.child.id()
    }

    /// Wait for the gateway to exit.
    #[allow(dead_code)]
    pub fn wait(&mut self) -> std::io::Result<std::process::ExitStatus> {
        self.child.wait()
    }

    /// Stop the gateway and reap it. Idempotent.
    ///
    /// This is the normal-quit path: the gateway is killed at once instead of
    /// waiting for its parent-watch to notice. A shell that dies without
    /// unwinding is still covered — the gateway exits by itself once the
    /// parent pid is gone.
    pub fn shutdown(&mut self) -> anyhow::Result<()> {
        if self.stopped {
            return Ok(());
        }
        self.stopped = true;

        // The gateway watches its parent pid and exits on its own; killing it
        // here is the belt to those braces, so a quit never leaves an orphan
        // holding the sidecar port.
        match self.child.kill() {
            Ok(()) => {}
            // Already exited and reaped: nothing left to kill.
            Err(error) if error.kind() == std::io::ErrorKind::InvalidInput => {}
            Err(error) => {
                return Err(error).context("stopping the bundled gateway");
            }
        }
        let _ = self.child.wait();
        Ok(())
    }
}

impl Drop for Sidecar {
    fn drop(&mut self) {
        let _ = self.shutdown();
    }
}

/// Where the bundled gateway executable lives: next to the app executable.
pub fn gateway_executable_path() -> Option<PathBuf> {
    let candidate = app_executable_dir()?.join(GATEWAY_EXECUTABLE);
    candidate.is_file().then_some(candidate)
}

/// The directory the app executable lives in.
pub fn app_executable_dir() -> Option<PathBuf> {
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(Path::to_path_buf))
}

/// Start the bundled gateway. Only release builds carry one.
///
/// The gateway is started on the sidecar port with an explicit
/// `HETERION_ROUTER_PORT`/`OMNIROUTE_RUST_PORT`, so a globally configured
/// `HETERION_ROUTER_PORT=20128` (the installed service) cannot make the
/// sidecar fight the service for the port.
///
/// The call returns only once the gateway answers `GET /healthz`, so the UI
/// never talks to a port that is not serving yet.
pub fn spawn_if_owned() -> anyhow::Result<Option<Sidecar>> {
    if cfg!(debug_assertions) {
        return Ok(None);
    }

    let Some(executable) = gateway_executable_path() else {
        anyhow::bail!("{}", missing_gateway_message());
    };

    let port = sidecar_port();
    let mut command = Command::new(&executable);
    command
        .env("HETERION_ROUTER_PORT", port.to_string())
        .env("OMNIROUTE_RUST_PORT", port.to_string())
        .env("HETERION_ROUTER_PARENT_PID", std::process::id().to_string())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut child = command
        .spawn()
        .with_context(|| format!("starting {}", executable.display()))?;

    // The gateway's output is its only diagnostics: without this a failing
    // gateway is invisible. Both streams are forwarded to the shell's own
    // tracing output at debug level.
    forward_output(child.stdout.take(), "stdout");
    forward_output(child.stderr.take(), "stderr");

    let mut sidecar = Sidecar {
        child,
        stopped: false,
    };

    if let Err(error) = wait_until_ready(port, READINESS_TIMEOUT) {
        let _ = sidecar.shutdown();
        return Err(error);
    }

    tracing::debug!(target: "gateway", pid = sidecar.pid(), port, "gateway sidecar is up");
    Ok(Some(sidecar))
}

/// Wait until the gateway answers `GET /healthz`.
///
/// Polls every 100 ms until the gateway replies or the timeout expires, then
/// fails with the reason it never came up. Any HTTP answer counts as ready:
/// the question is whether the port is served, not whether the gateway is
/// happy.
pub fn wait_until_ready(port: u16, timeout: Duration) -> anyhow::Result<()> {
    let url = format!("http://127.0.0.1:{port}/healthz");
    let client = reqwest::blocking::Client::builder()
        .timeout(READINESS_REQUEST_TIMEOUT)
        .build()
        .context("building the readiness probe")?;

    let deadline = Instant::now() + timeout;

    loop {
        let failure = match client.get(&url).send() {
            Ok(response) => {
                tracing::debug!(
                    target: "gateway",
                    status = response.status().as_u16(),
                    "the gateway answered the readiness probe"
                );
                return Ok(());
            }
            Err(error) => error.to_string(),
        };

        if Instant::now() >= deadline {
            anyhow::bail!(
                "the gateway did not answer on {url} within {}s ({failure})",
                timeout.as_secs()
            );
        }
        std::thread::sleep(READINESS_INTERVAL);
    }
}

/// Forward a gateway output stream into the shell's own tracing output.
///
/// Every line is scrubbed of anything that looks like a credential before it
/// reaches the log: the gateway is not expected to print the operator key,
/// but the shell must not be the place where one leaks.
fn forward_output(stream: Option<impl Read + Send + 'static>, name: &'static str) {
    let Some(stream) = stream else {
        return;
    };

    std::thread::spawn(move || {
        let reader = BufReader::new(stream);
        for line in reader.lines() {
            let Ok(line) = line else { break };
            tracing::debug!(target: "gateway", stream = name, "{}", redact(&line));
        }
    });
}

/// Mask credentials in a line of gateway output.
///
/// A bearer token and an `sk-`-style key are replaced before the line is
/// logged; everything else is left alone.
fn redact(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut after_bearer = false;

    for chunk in line.split_inclusive(char::is_whitespace) {
        let start = chunk.len() - chunk.trim_start().len();
        let end = chunk.trim_end().len();
        let word = &chunk[start..end];

        let masked = if after_bearer && !word.is_empty() {
            "***"
        } else if is_key_like(word) {
            "sk-***"
        } else {
            word
        };
        after_bearer = word.eq_ignore_ascii_case("bearer");

        out.push_str(&chunk[..start]);
        out.push_str(masked);
        out.push_str(&chunk[end..]);
    }

    out
}

/// `true` when a word looks like an `sk-`-style API key.
fn is_key_like(word: &str) -> bool {
    word.len() > 12
        && word.starts_with("sk-")
        && word[3..].chars().all(|character| {
            character.is_ascii_alphanumeric() || character == '-' || character == '_'
        })
}

/// The error shown when the bundled gateway is not next to the app.
fn missing_gateway_message() -> String {
    match app_executable_dir() {
        Some(directory) => format!(
            "the bundled gateway `{GATEWAY_EXECUTABLE}` is missing: looked in {} (next to the app executable)",
            directory.display()
        ),
        None => format!(
            "the bundled gateway `{GATEWAY_EXECUTABLE}` is missing and the path of the app executable is unknown"
        ),
    }
}

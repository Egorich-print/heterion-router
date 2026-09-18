//! Process environment for the Rust gateway.
//!
//! Deliberately narrow: the JS server reads ~700 variables, but the gateway
//! only needs a handful. Everything else stays in `key_value` settings.

use std::collections::HashMap;
use std::path::PathBuf;

/// Gateway configuration sourced from the environment.
///
/// `HETERION_ROUTER_*` wins; the `OMNIROUTE_*` spellings are honored as a
/// one-cycle fallback for operators migrating from OmniRoute (the frozen
/// reference implementation).
#[derive(Debug, Clone)]
pub struct GatewayConfig {
    /// TCP port to listen on (`HETERION_ROUTER_PORT`, default 20129).
    pub port: u16,
    /// Interface to bind (`HETERION_ROUTER_HOST`, default `127.0.0.1`;
    /// `0.0.0.0` to match the JS server's all-interfaces default).
    pub host: String,
    /// Data directory (`HETERION_ROUTER_DATA_DIR` / `DATA_DIR`, else default).
    pub data_dir: PathBuf,
    /// Log filter (`RUST_LOG` / `HETERION_ROUTER_LOG`, default `"info"`).
    pub log_filter: String,
    /// Force API-key auth even when the `api_keys` table is empty
    /// (`HETERION_ROUTER_REQUIRE_AUTH=1`). When false, auth is still enforced as
    /// soon as at least one key exists.
    pub require_auth: bool,
}

impl GatewayConfig {
    /// Read configuration from the process environment.
    pub fn from_env() -> Self {
        let vars: HashMap<String, String> = std::env::vars().collect();
        Self::from_map(&vars)
    }

    /// Build configuration from an explicit map (tests, embeddings).
    pub fn from_map(vars: &HashMap<String, String>) -> Self {
        let get = |key: &str| vars.get(key).map(String::as_str).unwrap_or("");
        // New name wins, legacy OmniRoute spelling fills the gap.
        let get2 = |primary: &str, legacy: &str| {
            let value = get(primary);
            if value.trim().is_empty() {
                get(legacy)
            } else {
                value
            }
        };
        Self {
            port: get2("HETERION_ROUTER_PORT", "OMNIROUTE_RUST_PORT")
                .parse()
                .unwrap_or(20_129),
            host: {
                let host = get2("HETERION_ROUTER_HOST", "OMNIROUTE_RUST_HOST");
                if host.trim().is_empty() {
                    "127.0.0.1".to_string()
                } else {
                    host.to_string()
                }
            },
            data_dir: {
                let dir = get2("HETERION_ROUTER_DATA_DIR", "DATA_DIR");
                if dir.trim().is_empty() {
                    heterion_router_db::Db::data_dir()
                } else {
                    PathBuf::from(dir)
                }
            },
            log_filter: if get("RUST_LOG").is_empty() {
                let log = get2("HETERION_ROUTER_LOG", "OMNIROUTE_LOG");
                if log.is_empty() {
                    "info".to_string()
                } else {
                    log.to_string()
                }
            } else {
                get("RUST_LOG").to_string()
            },
            require_auth: matches!(
                get2("HETERION_ROUTER_REQUIRE_AUTH", "OMNIROUTE_REQUIRE_AUTH"),
                "1" | "true" | "yes"
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vars(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect()
    }

    #[test]
    fn defaults_are_sane() {
        let config = GatewayConfig::from_map(&vars(&[]));
        assert_eq!(config.port, 20_129);
        assert_eq!(config.host, "127.0.0.1");
        assert_eq!(config.log_filter, "info");
        assert!(!config.require_auth);
    }

    #[test]
    fn port_flag_and_filter_parse() {
        let config = GatewayConfig::from_map(&vars(&[
            ("HETERION_ROUTER_PORT", "22000"),
            ("HETERION_ROUTER_REQUIRE_AUTH", "1"),
            ("RUST_LOG", "debug"),
        ]));
        assert_eq!(config.port, 22000);
        assert!(config.require_auth);
        assert_eq!(config.log_filter, "debug");

        let bound = GatewayConfig::from_map(&vars(&[("HETERION_ROUTER_HOST", "0.0.0.0")]));
        assert_eq!(bound.host, "0.0.0.0");
    }

    #[test]
    fn bad_port_falls_back_to_default() {
        let config = GatewayConfig::from_map(&vars(&[("HETERION_ROUTER_PORT", "nope")]));
        assert_eq!(config.port, 20_129);
    }

    #[test]
    fn legacy_omniroute_spellings_still_work() {
        let config = GatewayConfig::from_map(&vars(&[
            ("OMNIROUTE_RUST_PORT", "22001"),
            ("OMNIROUTE_RUST_HOST", "0.0.0.0"),
            ("OMNIROUTE_LOG", "warn"),
            ("OMNIROUTE_REQUIRE_AUTH", "yes"),
        ]));
        assert_eq!(config.port, 22001);
        assert_eq!(config.host, "0.0.0.0");
        assert_eq!(config.log_filter, "warn");
        assert!(config.require_auth);
    }

    #[test]
    fn new_names_win_over_legacy_ones() {
        let config = GatewayConfig::from_map(&vars(&[
            ("HETERION_ROUTER_PORT", "22002"),
            ("OMNIROUTE_RUST_PORT", "22001"),
            ("HETERION_ROUTER_LOG", "error"),
            ("OMNIROUTE_LOG", "warn"),
        ]));
        assert_eq!(config.port, 22002);
        assert_eq!(config.log_filter, "error");
    }
}

//! Configuration for the Rust gateway.
//!
//! Ports the curated subset of `src/lib/env/` the gateway needs: a handful of
//! process environment variables, not the 819-var JS surface. The `key_value`
//! settings namespaces are read and written directly through
//! `heterion_router_db::repos::{kv_get,kv_set}` (the only live consumer).

pub mod config;

pub use config::GatewayConfig;

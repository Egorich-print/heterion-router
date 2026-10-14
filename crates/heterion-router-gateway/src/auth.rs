//! Bearer API-key authentication.
//!
//! Mirrors the JS `validateApiKey` contract: when the `api_keys` table holds
//! at least one key (or `require_auth` is set), `/v1/*` requires
//! `Authorization: Bearer <key>`. A fresh database with no keys stays open so
//! first-run provisioning never locks the operator out.
//!
//! Implemented as an extractor (not middleware) so each handler opts in
//! explicitly and `/healthz` stays public by construction.

use axum::{
    Json,
    extract::FromRequestParts,
    http::{StatusCode, header, request::Parts},
    response::{IntoResponse, Response},
};
use serde_json::json;

use crate::AppState;

/// The API key a request authenticated with.
#[derive(Debug, Clone)]
pub struct AuthKey {
    /// Row id in `api_keys`.
    pub id: String,
    /// Display name of the key.
    pub name: String,
    pub scopes: String,
    pub allowed_models: String,
    pub allowed_combos: String,
    pub no_log: bool,
}

/// Extractor yielding the authenticated key, or `None` when auth is open.
///
/// Rejects with `401` JSON when keys exist but the request carries no (or a
/// wrong) bearer token.
pub struct Authenticated {
    /// Present when the request authenticated against a non-empty key table.
    pub key: Option<AuthKey>,
}

impl Authenticated {
    pub fn allows_model(&self, model: &str) -> bool {
        let Some(key) = self.key.as_ref() else {
            return true;
        };
        let Ok(models) = serde_json::from_str::<Vec<String>>(&key.allowed_models) else {
            return false;
        };
        let Ok(combos) = serde_json::from_str::<Vec<String>>(&key.allowed_combos) else {
            return false;
        };
        if models.is_empty() || models.iter().any(|pattern| matches_pattern(pattern, model)) {
            return true;
        }
        combos.iter().any(|pattern| matches_pattern(pattern, model))
    }
}

fn matches_pattern(pattern: &str, value: &str) -> bool {
    if pattern == "*" {
        return true;
    }
    if let Some(prefix) = pattern.strip_suffix("/*") {
        return value == prefix || value.starts_with(&format!("{prefix}/"));
    }
    pattern == value
}

pub struct ManageAuthenticated;

/// SQL predicate for a key that can still authenticate.
///
/// Mirrors [`heterion_router_db::repos::find_usable_api_key`]: a key that is
/// revoked, banned, deactivated, blank, or past `expires_at` cannot pass a
/// bearer check, so it must not count towards the "a key exists" gate
/// either. `datetime()` normalizes both timestamp spellings the column holds
/// (SQLite `YYYY-MM-DD HH:MM:SS` and ISO-8601 `...T...Z`/offsets) and
/// returns NULL for junk, which keeps this in step with the Rust parser in
/// `find_usable_api_key`: absent or empty means no expiry, unparseable means
/// expired rather than indefinitely valid.
const USABLE_KEY_PREDICATE: &str = "revoked_at IS NULL AND COALESCE(is_banned, 0) = 0 AND COALESCE(is_active, 1) = 1 \
     AND key IS NOT NULL AND TRIM(key) != '' \
     AND (expires_at IS NULL OR TRIM(expires_at) = '' \
          OR (datetime(expires_at) IS NOT NULL AND datetime(expires_at) > datetime('now')))";

/// Whether the database still holds at least one genuinely usable key.
///
/// `COUNT(*)` is not enough: a database whose every key has been revoked,
/// banned, deactivated or expired must behave exactly like an empty one,
/// otherwise a non-loopback bind is allowed to start and then 401s every
/// request, or auth silently opens when it was meant to be closed.
pub fn has_usable_api_key(conn: &rusqlite::Connection) -> rusqlite::Result<bool> {
    let count: i64 = conn.query_row(
        &format!("SELECT COUNT(*) FROM api_keys WHERE {USABLE_KEY_PREDICATE}"),
        [],
        |row| row.get(0),
    )?;
    Ok(count > 0)
}

impl FromRequestParts<AppState> for ManageAuthenticated {
    type Rejection = Response;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let auth = Authenticated::from_request_parts(parts, state).await?;
        let allowed = match auth.key.as_ref() {
            None => true,
            Some(key) => serde_json::from_str::<Vec<String>>(&key.scopes)
                .map(|scopes| scopes.iter().any(|scope| scope == "manage"))
                .unwrap_or(false),
        };
        if allowed {
            Ok(Self)
        } else {
            Err(forbidden("manage scope required"))
        }
    }
}

impl FromRequestParts<AppState> for Authenticated {
    type Rejection = Response;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let Some(db) = state.db.as_ref() else {
            return Ok(Self { key: None });
        };

        let guard = db.connection();
        let key_exists = match has_usable_api_key(&guard) {
            Ok(exists) => exists,
            Err(_) => return Err(auth_unavailable()),
        };
        if !key_exists && !state.require_auth {
            return Ok(Self { key: None });
        }

        let Some(token) = bearer_token(parts) else {
            return Err(unauthorized("missing bearer token"));
        };
        match heterion_router_db::repos::find_usable_api_key(&guard, &token) {
            Ok(Some(row)) => {
                if let Err(error) = heterion_router_db::repos::touch_api_key(&guard, &row.id) {
                    tracing::warn!("api key last_used_at update failed: {error}");
                }
                Ok(Self {
                    key: Some(AuthKey {
                        id: row.id,
                        name: row.name,
                        scopes: row.scopes,
                        allowed_models: row.allowed_models,
                        allowed_combos: row.allowed_combos,
                        no_log: row.no_log,
                    }),
                })
            }
            Ok(None) => Err(unauthorized("invalid api key")),
            Err(_) => Err(auth_unavailable()),
        }
    }
}

fn bearer_token(parts: &Parts) -> Option<String> {
    let value = parts.headers.get(header::AUTHORIZATION)?;
    let text = value.to_str().ok()?;
    let (scheme, token) = text.split_once(' ')?;
    if !scheme.eq_ignore_ascii_case("bearer") {
        return None;
    }
    let token = token.trim();
    (!token.is_empty()).then(|| token.to_string())
}

fn unauthorized(message: &str) -> Response {
    (
        StatusCode::UNAUTHORIZED,
        Json(json!({
            "error": {
                "message": message,
                "type": "invalid_request_error",
                "code": "invalid_api_key",
            }
        })),
    )
        .into_response()
}

fn auth_unavailable() -> Response {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        Json(json!({
            "error": {
                "message": "authentication is temporarily unavailable",
                "type": "server_error",
                "code": "auth_unavailable",
            }
        })),
    )
        .into_response()
}

fn forbidden(message: &str) -> Response {
    (
        StatusCode::FORBIDDEN,
        Json(json!({
            "error": {
                "message": message,
                "type": "permission_error",
                "code": "forbidden",
            }
        })),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use heterion_router_db::{Db, repos::ApiKeyRow};

    fn keyed_db() -> Db {
        let db = Db::open_in_memory().unwrap();
        db.migrate().unwrap();
        let guard = db.connection();
        heterion_router_db::repos::insert_api_key(
            &guard,
            &ApiKeyRow {
                id: "k1".to_string(),
                name: "test".to_string(),
                key: "sk-test-123".to_string(),
                allowed_models: "[\"*\"]".to_string(),
                no_log: false,
            },
        )
        .unwrap();
        drop(guard);
        db
    }

    fn parts_with_auth(value: Option<&str>) -> Parts {
        let mut builder = Request::builder();
        if let Some(value) = value {
            builder = builder.header(header::AUTHORIZATION, value);
        }
        let (parts, _) = builder.body(Body::empty()).unwrap().into_parts();
        parts
    }

    #[test]
    fn bearer_parsing_accepts_case_insensitive_scheme() {
        let parts = parts_with_auth(Some("Bearer sk-test-123"));
        assert_eq!(bearer_token(&parts).as_deref(), Some("sk-test-123"));

        let lower = parts_with_auth(Some("bearer sk-test-123"));
        assert_eq!(bearer_token(&lower).as_deref(), Some("sk-test-123"));

        let basic = parts_with_auth(Some("Basic abc"));
        assert_eq!(bearer_token(&basic), None);

        let missing = parts_with_auth(None);
        assert_eq!(bearer_token(&missing), None);
    }

    #[test]
    fn keyed_db_lookup_finds_inserted_key() {
        let db = keyed_db();
        let guard = db.connection();
        let row = heterion_router_db::repos::find_usable_api_key(&guard, "sk-test-123")
            .unwrap()
            .unwrap();
        assert_eq!(row.id, "k1");
    }

    #[test]
    fn revoked_key_stops_authenticating() {
        let db = keyed_db();
        db.migrate().unwrap();
        let guard = db.connection();
        assert!(
            heterion_router_db::repos::find_usable_api_key(&guard, "sk-test-123")
                .unwrap()
                .is_some()
        );
        heterion_router_db::repos::set_api_key_revoked(&guard, "k1", true).unwrap();
        assert!(
            heterion_router_db::repos::find_usable_api_key(&guard, "sk-test-123")
                .unwrap()
                .is_none()
        );
    }

    // --- FIX 2: the "is there a usable key?" gate must not count dead rows.

    fn db_with_key(id: &str, key: &str) -> Db {
        let db = Db::open_in_memory().unwrap();
        db.migrate().unwrap();
        let guard = db.connection();
        heterion_router_db::repos::insert_api_key(
            &guard,
            &ApiKeyRow {
                id: id.to_string(),
                name: id.to_string(),
                key: key.to_string(),
                allowed_models: "[\"*\"]".to_string(),
                no_log: false,
            },
        )
        .unwrap();
        drop(guard);
        db
    }

    #[test]
    fn valid_key_passes_the_gate() {
        let db = db_with_key("k1", "sk-live");
        let guard = db.connection();
        assert!(has_usable_api_key(&guard).unwrap());
    }

    #[test]
    fn only_revoked_keys_do_not_pass_the_gate() {
        let db = db_with_key("k1", "sk-revoked");
        let guard = db.connection();
        heterion_router_db::repos::set_api_key_revoked(&guard, "k1", true).unwrap();
        assert!(!has_usable_api_key(&guard).unwrap());
    }

    #[test]
    fn only_banned_keys_do_not_pass_the_gate() {
        let db = db_with_key("k1", "sk-banned");
        let guard = db.connection();
        guard
            .execute("UPDATE api_keys SET is_banned = 1 WHERE id = 'k1'", [])
            .unwrap();
        assert!(!has_usable_api_key(&guard).unwrap());
    }

    #[test]
    fn only_inactive_keys_do_not_pass_the_gate() {
        let db = db_with_key("k1", "sk-off");
        let guard = db.connection();
        guard
            .execute("UPDATE api_keys SET is_active = 0 WHERE id = 'k1'", [])
            .unwrap();
        assert!(!has_usable_api_key(&guard).unwrap());
    }

    #[test]
    fn only_expired_keys_do_not_pass_the_gate() {
        let db = db_with_key("k1", "sk-expired");
        let guard = db.connection();
        guard
            .execute(
                "UPDATE api_keys SET expires_at = '2001-01-01 00:00:00' WHERE id = 'k1'",
                [],
            )
            .unwrap();
        assert!(!has_usable_api_key(&guard).unwrap());
    }

    /// Both spellings of `expires_at` must agree with
    /// `find_usable_api_key`, which parses them rather than comparing text.
    #[test]
    fn iso_and_sqlite_expiry_spellings_agree_with_the_request_path() {
        let db = db_with_key("k1", "sk-dated");
        let guard = db.connection();
        for (label, past, future) in [
            ("sqlite", "2001-01-01 00:00:00", "2999-01-01 00:00:00"),
            (
                "iso",
                "2001-01-01T00:00:00.000Z",
                "2999-01-01T00:00:00.000Z",
            ),
        ] {
            guard
                .execute(
                    "UPDATE api_keys SET expires_at = ?1 WHERE id = 'k1'",
                    rusqlite::params![past],
                )
                .unwrap();
            assert!(
                !has_usable_api_key(&guard).unwrap(),
                "{label}: past must not count"
            );
            assert!(
                heterion_router_db::repos::find_usable_api_key(&guard, "sk-dated")
                    .unwrap()
                    .is_none(),
                "{label}: gate and request path must agree on a past expiry"
            );

            guard
                .execute(
                    "UPDATE api_keys SET expires_at = ?1 WHERE id = 'k1'",
                    rusqlite::params![future],
                )
                .unwrap();
            assert!(
                has_usable_api_key(&guard).unwrap(),
                "{label}: future must count"
            );
            assert!(
                heterion_router_db::repos::find_usable_api_key(&guard, "sk-dated")
                    .unwrap()
                    .is_some(),
                "{label}: gate and request path must agree on a future expiry"
            );
        }
    }

    /// An unparseable expiry is expired, never "valid forever" — the
    /// request path rejects it, so the gate must not count it.
    #[test]
    fn an_unparseable_expiry_does_not_pass_the_gate() {
        let db = db_with_key("k1", "sk-junk-date");
        let guard = db.connection();
        guard
            .execute(
                "UPDATE api_keys SET expires_at = 'whenever' WHERE id = 'k1'",
                [],
            )
            .unwrap();
        assert!(!has_usable_api_key(&guard).unwrap());
        assert!(
            heterion_router_db::repos::find_usable_api_key(&guard, "sk-junk-date")
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn only_blank_keys_do_not_pass_the_gate() {
        let db = db_with_key("k1", "sk-blank");
        let guard = db.connection();
        guard
            .execute("UPDATE api_keys SET key = '   ' WHERE id = 'k1'", [])
            .unwrap();
        assert!(!has_usable_api_key(&guard).unwrap());
    }

    /// The Rust schema declares `key TEXT NOT NULL`, but the JS reference
    /// server (whose databases are still migrated in place) allows NULL, and
    /// a NULL secret can never satisfy `find_usable_api_key`.
    #[test]
    fn a_nullable_key_column_with_only_nulls_does_not_pass_the_gate() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE api_keys (
                 id TEXT PRIMARY KEY,
                 key TEXT,
                 revoked_at TEXT,
                 expires_at TEXT,
                 is_active INTEGER NOT NULL DEFAULT 1,
                 is_banned INTEGER NOT NULL DEFAULT 0
             );
             INSERT INTO api_keys (id, key) VALUES ('k1', NULL);",
        )
        .unwrap();
        assert!(!has_usable_api_key(&conn).unwrap());
        conn.execute("UPDATE api_keys SET key = 'sk-live' WHERE id = 'k1'", [])
            .unwrap();
        assert!(has_usable_api_key(&conn).unwrap());
    }

    /// One live key among dead ones still passes — the gate is an OR, not
    /// an AND.
    #[test]
    fn a_single_live_key_keeps_the_gate_closed() {
        let db = db_with_key("k1", "sk-dead");
        let guard = db.connection();
        heterion_router_db::repos::set_api_key_revoked(&guard, "k1", true).unwrap();
        drop(guard);
        let guard = db.connection();
        heterion_router_db::repos::insert_api_key(
            &guard,
            &ApiKeyRow {
                id: "k2".to_string(),
                name: "k2".to_string(),
                key: "sk-live".to_string(),
                allowed_models: "[\"*\"]".to_string(),
                no_log: false,
            },
        )
        .unwrap();
        assert!(has_usable_api_key(&guard).unwrap());
    }

    /// A revoked-only database must leave `/v1/*` open exactly like an empty
    /// one, or the gateway refuses to serve the operator it just locked out.
    #[tokio::test]
    async fn revoked_only_database_leaves_the_gate_open() {
        let db = db_with_key("k1", "sk-revoked");
        {
            let guard = db.connection();
            heterion_router_db::repos::set_api_key_revoked(&guard, "k1", true).unwrap();
        }

        let state = crate::AppState::with_db(
            std::sync::Arc::new(crate::backend::EchoBackend),
            std::sync::Arc::new(db),
        );
        let mut parts = parts_with_auth(None);
        let authenticated =
            <Authenticated as FromRequestParts<AppState>>::from_request_parts(&mut parts, &state)
                .await
                .unwrap();
        assert!(authenticated.key.is_none(), "auth must stay open");
    }
}

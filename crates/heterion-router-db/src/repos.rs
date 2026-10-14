//! First-contour repositories.
//!
//! Only the tables the gateway needs first: `key_value` (settings),
//! `provider_connections` (credential selection), `api_keys` (auth) and
//! `combos` (routing). Every query names explicit columns so a database that
//! the Node server extended via later migrations keeps working.

use rusqlite::{Connection, OptionalExtension, params};
use sha2::{Digest, Sha256};
use std::collections::HashSet;

use crate::error::Result;

/// A row in `provider_connections` (auth/selection-relevant columns only).
#[derive(Debug, Clone, PartialEq)]
pub struct ConnectionRow {
    pub id: String,
    pub provider: String,
    pub auth_type: Option<String>,
    pub name: Option<String>,
    pub email: Option<String>,
    pub priority: i64,
    pub is_active: bool,
    pub access_token: Option<String>,
    pub refresh_token: Option<String>,
    pub expires_at: Option<String>,
    pub api_key: Option<String>,
    pub provider_specific_data: Option<String>,
}

fn map_connection(row: &rusqlite::Row<'_>) -> rusqlite::Result<ConnectionRow> {
    Ok(ConnectionRow {
        id: row.get("id")?,
        provider: row.get("provider")?,
        auth_type: row.get("auth_type")?,
        name: row.get("name")?,
        email: row.get("email")?,
        priority: row.get::<_, Option<i64>>("priority")?.unwrap_or(0),
        is_active: row.get::<_, Option<i64>>("is_active")?.unwrap_or(0) != 0,
        access_token: row.get("access_token")?,
        refresh_token: row.get("refresh_token")?,
        expires_at: row.get("expires_at")?,
        api_key: row.get("api_key")?,
        provider_specific_data: row.get("provider_specific_data")?,
    })
}

const CONNECTION_COLUMNS: &str = "id, provider, auth_type, name, email, priority, \
    is_active, access_token, refresh_token, expires_at, api_key, provider_specific_data";

/// Fetch a connection by id.
pub fn get_connection(conn: &Connection, id: &str) -> Result<Option<ConnectionRow>> {
    let row = conn
        .query_row(
            &format!("SELECT {CONNECTION_COLUMNS} FROM provider_connections WHERE id = ?1"),
            params![id],
            map_connection,
        )
        .optional()?;
    Ok(row)
}

/// List active connections, highest priority first.
///
/// Deliberate repository API. The gateway reaches active connections via
/// the gateway's startup scan, but a db crate exposing its own
/// priority-ordered query is normal shape, not an internal leftover.
#[allow(dead_code)]
pub fn list_active_connections(
    conn: &Connection,
    provider: Option<&str>,
) -> Result<Vec<ConnectionRow>> {
    let (sql, bind_provider) = match provider {
        Some(_) => (
            format!(
                "SELECT {CONNECTION_COLUMNS} FROM provider_connections \
                 WHERE is_active = 1 AND provider = ?1 \
                 ORDER BY priority DESC, id ASC"
            ),
            true,
        ),
        None => (
            format!(
                "SELECT {CONNECTION_COLUMNS} FROM provider_connections \
                 WHERE is_active = 1 ORDER BY priority DESC, id ASC"
            ),
            false,
        ),
    };

    let mut statement = conn.prepare(&sql)?;
    let rows = if bind_provider {
        statement.query_map(params![provider], map_connection)?
    } else {
        statement.query_map([], map_connection)?
    };
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(crate::error::DbError::from)
}

/// Insert or replace a connection row.
pub fn upsert_connection(conn: &Connection, row: &ConnectionRow) -> Result<()> {
    conn.execute(
        "INSERT INTO provider_connections (
            id, provider, auth_type, name, email, priority, is_active,
            access_token, refresh_token, expires_at, api_key,
            provider_specific_data, created_at, updated_at
        ) VALUES (
            ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12,
            datetime('now'), datetime('now')
        ) ON CONFLICT(id) DO UPDATE SET
            provider = excluded.provider,
            auth_type = excluded.auth_type,
            name = excluded.name,
            email = excluded.email,
            priority = excluded.priority,
            is_active = excluded.is_active,
            access_token = excluded.access_token,
            refresh_token = excluded.refresh_token,
            expires_at = excluded.expires_at,
            api_key = excluded.api_key,
            provider_specific_data = excluded.provider_specific_data,
            updated_at = datetime('now')",
        params![
            row.id,
            row.provider,
            row.auth_type,
            row.name,
            row.email,
            row.priority,
            i64::from(row.is_active),
            row.access_token,
            row.refresh_token,
            row.expires_at,
            row.api_key,
            row.provider_specific_data,
        ],
    )?;
    Ok(())
}

pub fn update_connection_tokens(
    conn: &Connection,
    id: &str,
    access_token: &str,
    refresh_token: Option<&str>,
    expires_at: Option<&str>,
) -> Result<()> {
    conn.execute(
        "UPDATE provider_connections
         SET access_token = ?1, refresh_token = ?2,
             expires_at = COALESCE(?3, expires_at),
             updated_at = datetime('now')
         WHERE id = ?4",
        params![access_token, refresh_token, expires_at, id],
    )?;
    Ok(())
}

/// A row in `api_keys`.
#[derive(Debug, Clone, PartialEq)]
pub struct ApiKeyRow {
    pub id: String,
    pub name: String,
    pub key: String,
    pub allowed_models: String,
    pub no_log: bool,
}

fn map_api_key(row: &rusqlite::Row<'_>) -> rusqlite::Result<ApiKeyRow> {
    Ok(ApiKeyRow {
        id: row.get("id")?,
        name: row.get("name")?,
        key: row.get("key")?,
        allowed_models: row
            .get::<_, Option<String>>("allowed_models")?
            .unwrap_or_else(|| "[]".to_string()),
        no_log: row.get::<_, Option<i64>>("no_log")?.unwrap_or(0) != 0,
    })
}

pub fn api_key_hash(secret: &str) -> String {
    hex::encode(Sha256::digest(secret.as_bytes()))
}

/// Marker prefix for a `key` column that carries no credential material.
///
/// A hash-issued row cannot leave `key` blank: the column is `NOT NULL` and
/// `auth.rs`'s "is there a usable key?" gate requires `TRIM(key) <> ''`, or a
/// freshly issued row would read as "no keys exist" and open the gateway. It
/// must not be the secret, the digest or the display prefix either — anything
/// stored in the column is readable by whoever holds a copy of the file, and
/// `API_KEY_MATCH` would happily compare a stored value back verbatim. So the
/// column holds this marker plus the row id (which `api_keys.key` indexes
/// uniquely, since `id` is the primary key) and the predicate refuses the whole
/// shape.
const HASH_ONLY_MARKER: &str = "hash-only:";

/// The credential predicate every `api_keys` lookup shares.
///
/// `?1` is the presented bearer token, `?2` its SHA-256 ([`api_key_hash`]),
/// `?3` [`HASH_ONLY_MARKER`]. A stored value is only ever accepted as a
/// credential through the `key_hash` branch, which needs the secret itself;
/// the `key = ?1` branch exists for rows the frozen JS server wrote with a
/// plaintext secret and excludes everything derived or opaque:
///
/// * `key <> ?3 || id` — the marker a hash-issued row carries.
/// * `key IS NOT key_hash` — a digest mirrored into `key` (which is what a
///   pre-fix build wrote, and what the rekey plan in `docs/research/mission-
///   2026-10/04-security-retention.md` §2b would write). `IS NOT` rather than
///   `<>` so a legacy row with no `key_hash` at all is not silently dropped.
/// * `key NOT LIKE 'enc:v1:%'` — a field-encrypted `key` is ciphertext, which
///   is not a credential in any language, and `ApiKeySummary::is_encrypted`
///   already reports such rows as unusable by this gateway.
///
/// One query, one round trip: the hash branch is indexed by `key_hash` in
/// practice and the plaintext branch by `key`, so neither degrades to a scan
/// of the whole table for the common case.
const API_KEY_MATCH: &str = "key_hash = ?2 \
    OR (key = ?1 AND key <> ?3 || id AND key IS NOT key_hash \
        AND key NOT LIKE 'enc:v1:%')";

/// Fetch an API key by its secret value or SHA-256 hash (the auth hot path).
///
/// Deliberate repository API: auth goes through `find_usable_api_key`
/// (revocation-aware), while this stays the plain lookup.
#[allow(dead_code)]
pub fn get_api_key_by_key(conn: &Connection, key: &str) -> Result<Option<ApiKeyRow>> {
    let row = conn
        .query_row(
            &format!(
                "SELECT id, name, key, allowed_models, no_log FROM api_keys WHERE {API_KEY_MATCH}"
            ),
            params![key, api_key_hash(key), HASH_ONLY_MARKER],
            map_api_key,
        )
        .optional()?;
    Ok(row)
}

/// Insert an API key.
pub fn insert_api_key(conn: &Connection, row: &ApiKeyRow) -> Result<()> {
    conn.execute(
        "INSERT INTO api_keys (id, name, key, allowed_models, no_log, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, datetime('now'))",
        params![
            row.id,
            row.name,
            row.key,
            row.allowed_models,
            i64::from(row.no_log)
        ],
    )?;
    Ok(())
}

/// A dashboard-created API key, mirroring what the JS server stores for an
/// operator key: the full secret is returned only at issuance, while the row
/// stores its SHA-256 hash for authentication and a prefix for display.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewApiKey {
    pub id: String,
    pub name: String,
    /// The plaintext secret. Returned once at issuance and **never written**:
    /// [`insert_full_api_key`] persists only the hash (see
    /// [`HASH_ONLY_MARKER`]).
    pub key: String,
    pub key_prefix: String,
    pub key_hash: String,
    pub allowed_models: String,
    pub scopes: String,
}

/// Insert a full operator key. Unspecified columns take the same defaults the
/// JS dashboard writes, so the key works on both sides.
///
/// The secret is written **once**, as `key_hash`. The `key` column gets the
/// [`HASH_ONLY_MARKER`] placeholder instead of a second copy of the digest:
/// mirroring the digest into `key` used to make it a working bearer token,
/// because `API_KEY_MATCH`'s plaintext branch compared a stored column value
/// back verbatim — a backup, a copied `storage.sqlite`, a log or a support
/// export then carried a live credential.
pub fn insert_full_api_key(conn: &Connection, row: &NewApiKey) -> Result<()> {
    conn.execute(
        "INSERT INTO api_keys (
            id, name, key, machine_id, allowed_models, no_log, created_at,
            revoked_at, expires_at, last_used_at, key_prefix, scopes,
            allowed_combos, stream_default_mode, allowed_quotas,
            disable_non_public_models, usage_limit_enabled, is_active,
            max_sessions, is_banned, key_hash, allow_usage_command,
            chaos_mode_enabled, cache_default_mode, model_access_mode,
            compression_enabled
        ) VALUES (
            ?1, ?2, ?3, NULL, ?4, 0, datetime('now'),
            NULL, NULL, NULL, ?5, ?6,
            '[\"combo/*\"]', 'legacy', '[]',
            0, 0, 1,
            0, 0, ?7, 1,
            0, 'legacy', 'all',
            1
        )",
        params![
            row.id,
            row.name,
            format!("{HASH_ONLY_MARKER}{}", row.id),
            row.allowed_models,
            row.key_prefix,
            row.scopes,
            row.key_hash,
        ],
    )?;
    Ok(())
}

/// An API key as the dashboard lists it: metadata only, never the secret.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiKeySummary {
    pub id: String,
    pub name: String,
    pub key_prefix: Option<String>,
    /// Whether the stored secret is `enc:v1:`-encrypted (the plaintext `key`
    /// branch of the credential predicate skips such rows, so they only
    /// authenticate through their own `key_hash`).
    pub is_encrypted: bool,
    pub created_at: Option<String>,
    pub revoked_at: Option<String>,
    pub expires_at: Option<String>,
    pub last_used_at: Option<String>,
    pub is_active: bool,
}

fn map_key_summary(row: &rusqlite::Row<'_>) -> rusqlite::Result<ApiKeySummary> {
    Ok(ApiKeySummary {
        id: row.get("id")?,
        name: row.get("name")?,
        key_prefix: row.get("key_prefix")?,
        is_encrypted: row.get::<_, i64>("is_encrypted")? != 0,
        created_at: row.get("created_at")?,
        revoked_at: row.get("revoked_at")?,
        expires_at: row.get("expires_at")?,
        last_used_at: row.get("last_used_at")?,
        is_active: row.get::<_, Option<i64>>("is_active")?.unwrap_or(1) != 0,
    })
}

const KEY_SUMMARY_COLUMNS: &str = "id, name, key_prefix, \
    CASE WHEN key LIKE 'enc:v1:%' THEN 1 ELSE 0 END AS is_encrypted, \
    created_at, revoked_at, expires_at, last_used_at, is_active";

/// List every API key, newest first. Secrets are never selected — only a
/// derived encrypted flag.
pub fn list_api_keys(conn: &Connection) -> Result<Vec<ApiKeySummary>> {
    let mut statement = conn.prepare(&format!(
        "SELECT {KEY_SUMMARY_COLUMNS} FROM api_keys ORDER BY created_at DESC, id ASC"
    ))?;
    let rows = statement.query_map([], map_key_summary)?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(crate::error::DbError::from)
}

/// Fetch one key's summary by id. Secrets are never selected.
pub fn get_api_key_summary(conn: &Connection, id: &str) -> Result<Option<ApiKeySummary>> {
    let row = conn
        .query_row(
            &format!("SELECT {KEY_SUMMARY_COLUMNS} FROM api_keys WHERE id = ?1"),
            params![id],
            map_key_summary,
        )
        .optional()?;
    Ok(row)
}

/// Revoke (`true`) or restore (`false`) an API key. Returns whether the row
/// exists. Revocation takes effect immediately: authentication reads the
/// table on every request.
pub fn touch_api_key(conn: &Connection, id: &str) -> Result<()> {
    conn.execute(
        "UPDATE api_keys SET last_used_at = datetime('now') WHERE id = ?1",
        params![id],
    )?;
    Ok(())
}

pub fn set_api_key_revoked(conn: &Connection, id: &str, revoked: bool) -> Result<bool> {
    let changed = conn.execute(
        "UPDATE api_keys SET revoked_at = CASE WHEN ?1 THEN datetime('now') ELSE NULL END \
         WHERE id = ?2",
        params![revoked, id],
    )?;
    Ok(changed > 0)
}

type UsableKeyRow = (String, String, Option<String>, String, String, String, i64);

/// An API key that passed every usability gate (the auth hot path).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsableKey {
    pub id: String,
    pub name: String,
    pub scopes: String,
    pub allowed_models: String,
    pub allowed_combos: String,
    pub no_log: bool,
}

/// Fetch an API key by its secret value, honouring the kill switches.
///
/// Unlike [`get_api_key_by_key`], a row only matches when it is not revoked,
/// not banned, active, and unexpired. Expiry is checked in Rust because the
/// table mixes SQLite (`"2026-09-17 13:00:00"`) and ISO-8601 spellings.
///
/// The credential half of the predicate is [`API_KEY_MATCH`]: the secret is
/// matched by its SHA-256, and the plaintext `key` branch only serves legacy
/// JS rows — never a digest, a marker or a ciphertext lifted out of the file.
pub fn find_usable_api_key(conn: &Connection, key: &str) -> Result<Option<UsableKey>> {
    let row: Option<UsableKeyRow> = conn
        .query_row(
            &format!(
                "SELECT id, name, expires_at, \
                        COALESCE(scopes, '[\"manage\"]'), \
                        COALESCE(allowed_models, '[]'), \
                        COALESCE(allowed_combos, '[\"combo/*\"]'), \
                        COALESCE(no_log, 0) \
                 FROM api_keys \
                 WHERE ({API_KEY_MATCH}) AND revoked_at IS NULL \
                   AND COALESCE(is_banned, 0) = 0 AND COALESCE(is_active, 1) = 1"
            ),
            params![key, api_key_hash(key), HASH_ONLY_MARKER],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                ))
            },
        )
        .optional()?;
    let Some((id, name, expires_at, scopes, allowed_models, allowed_combos, no_log)) = row else {
        return Ok(None);
    };
    let now = unix_now();
    match timestamp_expired(expires_at.as_deref(), now) {
        Ok(false) => {}
        Ok(true) | Err(()) => return Ok(None),
    }
    Ok(Some(UsableKey {
        id,
        name,
        scopes,
        allowed_models,
        allowed_combos,
        no_log: no_log != 0,
    }))
}

/// Seconds since the unix epoch, or `0` for a clock that predates it.
fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0)
}

/// Whether an `expires_at` value is already past.
///
/// `None` (no expiry) and an empty value count as valid; malformed non-empty
/// values are rejected instead of silently extending a key's lifetime.
fn timestamp_expired(expires_at: Option<&str>, now_unix: u64) -> std::result::Result<bool, ()> {
    let Some(text) = expires_at else {
        return Ok(false);
    };
    if text.trim().is_empty() {
        return Ok(false);
    }
    parse_db_timestamp(text)
        .map(|expiry| expiry <= now_unix)
        .ok_or(())
}

/// Parse the two timestamp spellings this database holds — SQLite
/// `"YYYY-MM-DD HH:MM:SS"` and ISO-8601 (`"...)T...Z"`, offsets, fractions)
/// — into unix seconds. Returns `None` when the text does not match either.
fn parse_db_timestamp(text: &str) -> Option<u64> {
    let text = text.trim();
    let (date, time) = text.split_once(['T', ' '])?;
    let mut date_parts = date.split('-');
    let year: i64 = date_parts.next()?.parse().ok()?;
    let month: i64 = date_parts.next()?.parse().ok()?;
    let day: i64 = date_parts.next()?.parse().ok()?;
    if date_parts.next().is_some() || !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    // Offset suffix: `Z`, `+hh:mm`, `+hhmm`, `+hh`, or nothing (assume UTC —
    // both writers store UTC).
    let (clock, offset_secs) = if let Some(stripped) = time.strip_suffix(['Z', 'z']) {
        (stripped, 0)
    } else if let Some(at) = time.rfind(['+', '-']) {
        let (clock, zone) = time.split_at(at);
        (clock, parse_zone_offset(zone)?)
    } else {
        (time, 0)
    };
    let mut clock_parts = clock.split(':');
    let hour: i64 = clock_parts.next()?.parse().ok()?;
    let minute: i64 = clock_parts.next()?.parse().ok()?;
    let second: i64 = clock_parts.next()?.split('.').next()?.parse().ok()?;
    if clock_parts.next().is_some() || hour > 23 || minute > 59 || second > 60 {
        return None;
    }
    // Days since 1970-01-01 (Howard Hinnant's days_from_civil).
    let adjusted = if month <= 2 { year - 1 } else { year };
    let era = adjusted.div_euclid(400);
    let year_of_era = adjusted.rem_euclid(400);
    let month_prime = (month + 9).rem_euclid(12);
    let day_of_year = (153 * month_prime + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    let days = era * 146097 + day_of_era - 719468;
    let stamp = days * 86_400 + hour * 3600 + minute * 60 + second - offset_secs;
    u64::try_from(stamp).ok()
}

/// Parse a `Z`/`±hh[:mm]` zone suffix into seconds east of UTC.
fn parse_zone_offset(zone: &str) -> Option<i64> {
    let sign = match zone.as_bytes().first()? {
        b'+' => 1,
        b'-' => -1,
        _ => return None,
    };
    let digits: String = zone[1..].chars().filter(|ch| *ch != ':').collect();
    let (hours, minutes) = match digits.len() {
        2 => (digits.parse::<i64>().ok()?, 0),
        4 => (
            digits[..2].parse::<i64>().ok()?,
            digits[2..].parse::<i64>().ok()?,
        ),
        _ => return None,
    };
    if hours > 23 || minutes > 59 {
        return None;
    }
    Some(sign * (hours * 3600 + minutes * 60))
}

/// A row in `combos`.
#[derive(Debug, Clone, PartialEq)]
pub struct ComboRow {
    pub id: String,
    pub name: String,
    pub data: String,
    pub sort_order: i64,
    pub active: bool,
}

fn map_combo(row: &rusqlite::Row<'_>) -> rusqlite::Result<ComboRow> {
    Ok(ComboRow {
        id: row.get("id")?,
        name: row.get("name")?,
        data: row.get("data")?,
        sort_order: row.get::<_, Option<i64>>("sort_order")?.unwrap_or(0),
        active: row.get::<_, Option<i64>>("active")?.unwrap_or(1) != 0,
    })
}

/// Fetch a combo by id.
pub fn get_combo(conn: &Connection, id: &str) -> Result<Option<ComboRow>> {
    let row = conn
        .query_row(
            "SELECT id, name, data, sort_order, active FROM combos WHERE id = ?1",
            params![id],
            map_combo,
        )
        .optional()?;
    Ok(row)
}

/// Fetch a combo by name.
///
/// Deliberate repository API: name is the operator-facing combo handle, so
/// this is the lookup any name-addressed caller (admin, CLI, desktop) will
/// reach for.
#[allow(dead_code)]
pub fn get_combo_by_name(conn: &Connection, name: &str) -> Result<Option<ComboRow>> {
    let row = conn
        .query_row(
            "SELECT id, name, data, sort_order, active FROM combos WHERE name = ?1",
            params![name],
            map_combo,
        )
        .optional()?;
    Ok(row)
}

/// List combos in display order.
pub fn list_combos(conn: &Connection) -> Result<Vec<ComboRow>> {
    let mut statement = conn.prepare(
        "SELECT id, name, data, sort_order, active FROM combos ORDER BY sort_order ASC, name ASC",
    )?;
    let rows = statement.query_map([], map_combo)?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(crate::error::DbError::from)
}

/// List only active combos in display order.
///
/// Combos with `active = 0` are archived but still present in the database;
/// this is the query the router and the admin UI use to populate the
/// selectable list.
pub fn list_active_combos(conn: &Connection) -> Result<Vec<ComboRow>> {
    let mut statement = conn.prepare(
        "SELECT id, name, data, sort_order, active FROM combos \
         WHERE active = 1 ORDER BY sort_order ASC, name ASC",
    )?;
    let rows = statement.query_map([], map_combo)?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(crate::error::DbError::from)
}

/// Insert or replace a combo.
pub fn upsert_combo(conn: &Connection, row: &ComboRow) -> Result<()> {
    conn.execute(
        "INSERT INTO combos (id, name, data, sort_order, active, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, datetime('now'), datetime('now'))
         ON CONFLICT(id) DO UPDATE SET
            name = excluded.name,
            data = excluded.data,
            sort_order = excluded.sort_order,
            active = excluded.active,
            updated_at = datetime('now')",
        params![
            row.id,
            row.name,
            row.data,
            row.sort_order,
            i64::from(row.active)
        ],
    )?;
    Ok(())
}

/// Fields that `PATCH /api/combos/:id` may change.
///
/// `name` and `sort_order` live both as columns and inside the `data` JSON;
/// this patch keeps the two copies in sync. `models` replaces the `models`
/// array wholesale (see [`render_model_items`]); callers must validate first
/// (non-empty, known providers are the admin layer's job).
#[derive(Debug, Clone, Default)]
pub struct ComboMetaPatch {
    pub name: Option<String>,
    pub sort_order: Option<i64>,
    pub is_hidden: Option<bool>,
    pub is_active: Option<bool>,
    pub strategy: Option<String>,
    pub models: Option<Vec<ComboModelSpec>>,
}

/// One model entry for a combo rewrite: provider plus bare model id.
///
/// `model` is the bare id exactly as the router derives it (the stored ref
/// minus its first `provider/` segment), and may itself contain slashes
/// (`"openai/gpt-oss-20b"`, `"nvidia/nemotron-3-super-120b-a12b"`). It is
/// joined verbatim: the server never strips prefixes, so a GET → PATCH
/// round-trip is byte-stable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComboModelSpec {
    /// Keep this entry id when set and non-blank; otherwise one is derived.
    pub id: Option<String>,
    pub provider: String,
    pub model: String,
    pub weight: i64,
}

/// Build the `models` JSON array for a combo rewrite.
///
/// New model items are emitted in order with generated ids where missing.
/// When a spec id matches a previous model item, unknown extra keys (e.g. a
/// JS-side `label`) are carried over so a Rust-side edit never drops data it
/// does not model. Array items of any other `kind` are preserved verbatim at
/// the end so a future JS-side kind survives as well.
fn render_model_items(
    combo_name: &str,
    specs: &[ComboModelSpec],
    previous: &[serde_json::Value],
) -> Vec<serde_json::Value> {
    fn slug(text: &str) -> String {
        let mut out = String::new();
        let mut last_dash = true;
        for ch in text.to_lowercase().chars() {
            if ch.is_ascii_alphanumeric() {
                out.push(ch);
                last_dash = false;
            } else if !last_dash {
                out.push('-');
                last_dash = true;
            }
            if out.len() >= 40 {
                break;
            }
        }
        out.trim_matches('-').to_string()
    }

    let mut taken: HashSet<String> = HashSet::new();
    let mut consumed: HashSet<String> = HashSet::new();
    let mut out = Vec::with_capacity(specs.len());
    for (index, spec) in specs.iter().enumerate() {
        // Verbatim join: `model` is already the bare id, slashes included.
        let bare = spec.model.clone();
        let base = match spec.id.clone().filter(|id| !id.trim().is_empty()) {
            Some(id) => id.trim().to_string(),
            None => format!(
                "{}-model-{}-{}-{}",
                slug(combo_name),
                index + 1,
                slug(&spec.provider),
                slug(&bare)
            ),
        };
        let mut id = base.clone();
        let mut counter = 2;
        while !taken.insert(id.clone()) {
            id = format!("{base}-{counter}");
            counter += 1;
        }
        // Carry over unknown keys from the previous item with the same id.
        let mut item = previous
            .iter()
            .find(|item| item.get("id").and_then(|id| id.as_str()) == Some(id.as_str()))
            .filter(|item| {
                item.get("kind")
                    .and_then(|kind| kind.as_str())
                    .is_none_or(|kind| kind == "model")
            })
            .cloned()
            .unwrap_or_else(|| serde_json::json!({}));
        consumed.insert(id.clone());
        item["id"] = serde_json::Value::String(id);
        item["kind"] = serde_json::Value::String("model".to_string());
        item["model"] = serde_json::Value::String(format!("{}/{}", spec.provider, bare));
        item["providerId"] = serde_json::Value::String(spec.provider.clone());
        item["weight"] = serde_json::Value::from(spec.weight);
        out.push(item);
    }
    for item in previous {
        let id = item.get("id").and_then(|id| id.as_str()).unwrap_or("");
        if consumed.contains(id) {
            continue;
        }
        let is_model = item
            .get("kind")
            .and_then(|kind| kind.as_str())
            .is_none_or(|kind| kind == "model");
        if !is_model {
            out.push(item.clone());
        }
    }
    out
}

/// Update combo metadata, keeping the `data` JSON (`name`, `sortOrder`,
/// `isHidden`, `isActive`) in sync with the columns.
///
/// The archive flag lives in the `active` column (ADR-015) —
/// `list_active_combos` filters on it — and is mirrored into the JSON for
/// UI consumers.
///
/// Returns the updated row, or `None` when `id` does not exist. A duplicate
/// `name` surfaces as a SQLite unique-constraint error for the caller to map.
pub fn update_combo_meta(
    conn: &Connection,
    id: &str,
    patch: &ComboMetaPatch,
) -> Result<Option<ComboRow>> {
    let Some(current) = get_combo(conn, id)? else {
        return Ok(None);
    };
    let mut data: serde_json::Value =
        serde_json::from_str(&current.data).unwrap_or(serde_json::json!({}));
    if !data.is_object() {
        data = serde_json::json!({});
    }
    let name = patch.name.clone().unwrap_or(current.name);
    let sort_order = patch.sort_order.unwrap_or(current.sort_order);
    let active = patch.is_active.unwrap_or(current.active);
    if let Some(hidden) = patch.is_hidden {
        data["isHidden"] = serde_json::Value::Bool(hidden);
    }
    if let Some(active) = patch.is_active {
        data["isActive"] = serde_json::Value::Bool(active);
    }
    if let Some(strategy) = patch.strategy.clone() {
        data["strategy"] = serde_json::Value::String(strategy);
    }
    if let Some(specs) = patch.models.as_deref() {
        let previous: Vec<serde_json::Value> = data
            .get("models")
            .and_then(|models| models.as_array())
            .cloned()
            .unwrap_or_default();
        data["models"] = serde_json::Value::Array(render_model_items(&name, specs, &previous));
    }
    data["name"] = serde_json::Value::String(name.clone());
    data["sortOrder"] = serde_json::Value::from(sort_order);
    let data_text = serde_json::to_string(&data)?;
    conn.execute(
        "UPDATE combos SET name = ?1, data = ?2, sort_order = ?3, active = ?4, \
         updated_at = datetime('now') WHERE id = ?5",
        params![name, data_text, sort_order, i64::from(active), id],
    )?;
    get_combo(conn, id)
}

/// Fields that `PATCH /api/connections/:id` may change.
///
/// Secrets are deliberately not patchable here; credential rotation stays a
/// JS-dashboard/CLI operation until the Rust side owns decryption.
#[derive(Debug, Clone, Default)]
pub struct ConnectionMetaPatch {
    pub name: Option<String>,
    pub is_active: Option<bool>,
    pub priority: Option<i64>,
}

/// Update connection metadata. Returns the updated row, or `None` when `id`
/// does not exist.
///
/// Note: flipping `is_active` only takes effect on the live gateway after a
/// restart, because backends are built once at startup. Callers should tell
/// the user that.
pub fn update_connection_meta(
    conn: &Connection,
    id: &str,
    patch: &ConnectionMetaPatch,
) -> Result<Option<ConnectionRow>> {
    let Some(current) = get_connection(conn, id)? else {
        return Ok(None);
    };
    let name = patch.name.clone().or(current.name);
    let is_active = patch.is_active.unwrap_or(current.is_active);
    let priority = patch.priority.unwrap_or(current.priority);
    conn.execute(
        "UPDATE provider_connections SET name = ?1, is_active = ?2, priority = ?3, \
         updated_at = datetime('now') WHERE id = ?4",
        params![name, i64::from(is_active), priority, id],
    )?;
    get_connection(conn, id)
}

/// Read a `key_value` setting.
pub fn kv_get(conn: &Connection, namespace: &str, key: &str) -> Result<Option<String>> {
    let value: Option<String> = conn
        .query_row(
            "SELECT value FROM key_value WHERE namespace = ?1 AND key = ?2",
            params![namespace, key],
            |row| row.get(0),
        )
        .optional()?;
    Ok(value)
}

/// Write a `key_value` setting.
pub fn kv_set(conn: &Connection, namespace: &str, key: &str, value: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO key_value (namespace, key, value) VALUES (?1, ?2, ?3)
         ON CONFLICT(namespace, key) DO UPDATE SET value = excluded.value",
        params![namespace, key, value],
    )?;
    Ok(())
}

/// Delete a `key_value` setting.
pub fn kv_delete(conn: &Connection, namespace: &str, key: &str) -> Result<()> {
    conn.execute(
        "DELETE FROM key_value WHERE namespace = ?1 AND key = ?2",
        params![namespace, key],
    )?;
    Ok(())
}

/// A `provider_connections` row shaped for the admin UI (no secrets).
#[derive(Debug, Clone, PartialEq)]
pub struct ConnectionSummary {
    pub id: String,
    pub provider: String,
    pub name: Option<String>,
    pub auth_type: Option<String>,
    pub is_active: bool,
    pub priority: i64,
    pub expires_at: Option<String>,
    pub test_status: Option<String>,
    pub last_error: Option<String>,
    /// Whether the row carries a usable secret (never the secret itself).
    pub has_credential: bool,
}

/// List every connection, active first, for the admin UI.
pub fn list_connections(conn: &Connection) -> Result<Vec<ConnectionSummary>> {
    let mut statement = conn.prepare(
        "SELECT id, provider, name, auth_type, is_active, priority, expires_at, \
                test_status, last_error, access_token, api_key \
         FROM provider_connections ORDER BY is_active DESC, provider ASC, priority DESC",
    )?;
    let rows = statement.query_map([], |row| {
        let access_token: Option<String> = row.get("access_token")?;
        let api_key: Option<String> = row.get("api_key")?;
        let has_credential = [access_token, api_key]
            .iter()
            .flatten()
            .any(|secret| !secret.is_empty());
        Ok(ConnectionSummary {
            id: row.get("id")?,
            provider: row.get("provider")?,
            name: row.get("name")?,
            auth_type: row.get("auth_type")?,
            is_active: row.get::<_, Option<i64>>("is_active")?.unwrap_or(0) != 0,
            priority: row.get::<_, Option<i64>>("priority")?.unwrap_or(0),
            expires_at: row.get("expires_at")?,
            test_status: row.get("test_status")?,
            last_error: row.get("last_error")?,
            has_credential,
        })
    })?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(crate::error::DbError::from)
}

/// One `(key, requests, tokens_in, tokens_out)` aggregate row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsageBucket {
    pub key: String,
    pub requests: i64,
    pub tokens_input: i64,
    pub tokens_output: i64,
}

/// Aggregate usage since `since` (a SQLite datetime modifier, e.g. `-7 days`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct UsageSummary {
    pub requests: i64,
    pub successes: i64,
    pub tokens_input: i64,
    pub tokens_output: i64,
    pub tokens_cache_read: i64,
    /// `latency_ms` percentiles over the same window as the sums above.
    pub latency: LatencyPercentiles,
    pub by_provider: Vec<UsageBucket>,
    pub by_model: Vec<UsageBucket>,
}

/// `latency_ms` percentiles over a usage window. Each figure is `None` when
/// the window holds no rows at all — never a fabricated `Some(0)`, which a
/// dashboard would render as a real measurement.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct LatencyPercentiles {
    pub p50_ms: Option<i64>,
    pub p95_ms: Option<i64>,
    pub p99_ms: Option<i64>,
}

/// Re-spell a timestamp expression as the canonical `YYYY-MM-DD HH:MM:SS`
/// prefix, so TEXT comparison against a cutoff is order-preserving.
///
/// `usage_history.timestamp` holds two spellings: SQLite's own
/// (`2026-09-22 20:25:19`, written by `datetime('now')`) and the ISO-8601 one
/// (`2026-09-22T20:25:19.000Z`, written by the Node server). Compared raw, `'T'`
/// sorts after `' '`, so a boundary-day ISO row looked hours later than a
/// cutoff written in the SQLite spelling and landed in a window it predates.
/// Both writers store UTC, so both sides of the comparison are normalised and
/// only the 19 shared leading characters are compared.
fn ts_prefix(expression: &str) -> String {
    format!("substr(replace(replace({expression}, 'T', ' '), 'Z', ''), 1, 19)")
}

/// The `usage_history.timestamp` window predicate shared by every read here.
///
/// Same `ts_prefix` normalisation on both sides as [`usage_summary`], so a new
/// reader cannot accidentally add a second (or no) normalisation.
fn usage_window(since_param: usize) -> String {
    format!(
        "{} >= {}",
        ts_prefix("timestamp"),
        ts_prefix(&format!("datetime('now', ?{since_param})"))
    )
}

/// Summarise `usage_history` for the dashboard.
pub fn usage_summary(conn: &Connection, since: &str, top: usize) -> Result<UsageSummary> {
    let mut summary = UsageSummary::default();

    let window = usage_window(1);
    let totals = conn.query_row(
        &format!(
            "SELECT COUNT(*), COALESCE(SUM(success), 0), \
             COALESCE(SUM(tokens_input), 0), COALESCE(SUM(tokens_output), 0), \
             COALESCE(SUM(tokens_cache_read), 0) \
             FROM usage_history WHERE {window}"
        ),
        params![since],
        |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, i64>(4)?,
            ))
        },
    )?;
    summary.requests = totals.0;
    summary.successes = totals.1;
    summary.tokens_input = totals.2;
    summary.tokens_output = totals.3;
    summary.tokens_cache_read = totals.4;
    summary.latency = latency_percentiles(conn, since)?;

    summary.by_provider = buckets(conn, "provider", since, top)?;
    summary.by_model = buckets(conn, "model", since, top)?;
    Ok(summary)
}

/// The nearest-rank percentile of an **ascending** `values` slice: rank
/// `ceil(quantile * n)`, 1-based and clamped to the last element, so every
/// `quantile` in `(0, 1]` returns an observed sample rather than an
/// interpolation between two of them.
///
/// Nearest-rank, not linear interpolation: it is exact for a single-request
/// window (p50 of `[7]` is `7`) and needs no fractional output type, which
/// matters here because `latency_ms` is a millisecond integer. An empty slice
/// yields `None` — an empty window has no percentile, and must not panic.
fn nearest_rank(values: &[i64], quantile: f64) -> Option<i64> {
    if values.is_empty() {
        return None;
    }
    let rank = (quantile * values.len() as f64).ceil().max(1.0) as usize;
    values.get(rank.min(values.len()) - 1).copied()
}

/// p50/p95/p99 of `usage_history.latency_ms` over the `since` window.
///
/// SQLite has no percentile aggregate, so the sample is read in full and
/// reduced in Rust. Every row in the window contributes its recorded value —
/// no success filter and no dropping of the schema's `latency_ms` default of
/// `0` — so the figures describe the window the dashboard asks for, not a
/// filtered one.
fn latency_percentiles(conn: &Connection, since: &str) -> Result<LatencyPercentiles> {
    let window = usage_window(1);
    let mut statement = conn.prepare(&format!(
        "SELECT COALESCE(latency_ms, 0) FROM usage_history WHERE {window}"
    ))?;
    let mut samples: Vec<i64> = statement
        .query_map(params![since], |row| row.get(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    drop(statement);
    samples.sort_unstable();
    Ok(LatencyPercentiles {
        p50_ms: nearest_rank(&samples, 0.50),
        p95_ms: nearest_rank(&samples, 0.95),
        p99_ms: nearest_rank(&samples, 0.99),
    })
}

/// One `usage_history` row, reduced to exactly what pricing needs: the model
/// to look up plus the five token counts and the latency.
///
/// USD cannot be computed here — the price table lives in the `usage` crate —
/// so this is the seam: a caller prices each row and reduces the resulting
/// `Vec<f64>` with [`nearest_rank`] itself. `None` on `model` is what makes an
/// unpriced row reportable as unknown rather than silently free.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsageCostRow {
    pub model: Option<String>,
    pub tokens_input: i64,
    pub tokens_output: i64,
    pub tokens_cache_read: i64,
    pub tokens_cache_creation: i64,
    pub tokens_reasoning: i64,
    pub latency_ms: i64,
}

/// Every `usage_history` row in the `since` window, unaggregated, in one
/// query — the input a cost percentile has to be computed from.
///
/// A percentile cannot be derived from the sums in [`UsageSummary`] (the
/// distribution is exactly what is lost), and no SQL-level histogram exists
/// to approximate it, so the raw rows go across. `since` is a SQLite datetime
/// modifier (`-7 days`), the same parameter [`usage_summary`] takes, and the
/// window is normalised through the same [`ts_prefix`] helper.
pub fn usage_cost_rows_in_window(conn: &Connection, since: &str) -> Result<Vec<UsageCostRow>> {
    let window = usage_window(1);
    let mut statement = conn.prepare(&format!(
        "SELECT model, COALESCE(tokens_input, 0), COALESCE(tokens_output, 0), \
                COALESCE(tokens_cache_read, 0), COALESCE(tokens_cache_creation, 0), \
                COALESCE(tokens_reasoning, 0), COALESCE(latency_ms, 0) \
         FROM usage_history WHERE {window}"
    ))?;
    let rows = statement.query_map(params![since], |row| {
        Ok(UsageCostRow {
            model: row.get(0)?,
            tokens_input: row.get(1)?,
            tokens_output: row.get(2)?,
            tokens_cache_read: row.get(3)?,
            tokens_cache_creation: row.get(4)?,
            tokens_reasoning: row.get(5)?,
            latency_ms: row.get(6)?,
        })
    })?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(crate::error::DbError::from)
}

/// How many rows a retention sweep removed from each table.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PurgeOutcome {
    pub usage_history: usize,
    pub call_logs: usize,
}

/// Delete `usage_history` and `call_logs` rows older than
/// `retention_days` days, and report how many went from each table.
///
/// `retention_days == 0` means **keep everything** and deletes nothing. It is
/// the documented "retention off" setting, and the arithmetic that would make
/// it "delete all history" is the classic way a sweep eats a database, so it
/// short-circuits before any `DELETE` is prepared.
///
/// The cutoff is strict (`<`): a row stamped exactly at it is kept, so
/// `retention_days = 7` keeps the last seven whole days rather than the last
/// seven days minus one second. Both timestamp spellings are compared through
/// [`ts_prefix`], which — like every reader here — means the sweep forfeits
/// `idx_uh_timestamp`/`idx_cl_timestamp` for the re-spelled expression and
/// scans. Correctness over speed: a sweep runs on a schedule, not per request.
pub fn purge_usage_older_than(conn: &Connection, retention_days: u32) -> Result<PurgeOutcome> {
    if retention_days == 0 {
        return Ok(PurgeOutcome::default());
    }
    let since = format!("-{retention_days} days");
    let older_than = format!(
        "{} < {}",
        ts_prefix("timestamp"),
        ts_prefix("datetime('now', ?1)")
    );
    let mut outcome = PurgeOutcome::default();
    for (table, deleted) in [
        ("usage_history", &mut outcome.usage_history),
        ("call_logs", &mut outcome.call_logs),
    ] {
        // `table` is a fixed literal chosen here, never user input.
        *deleted = conn.execute(
            &format!("DELETE FROM {table} WHERE {older_than}"),
            params![since],
        )?;
    }
    Ok(outcome)
}

/// Ids of the active `provider_connections` rows for `provider`, in selection
/// order (`priority DESC, id ASC`).
///
/// When a provider has several active rows the gateway needs the *ids* to
/// record which one served a request, without the caller writing its own SQL
/// against a schema it does not own.
///
/// Index: `idx_pc_priority ON provider_connections(provider, priority)`
/// (schema_base.sql:49) already covers this — `EXPLAIN QUERY PLAN` reports
/// `SEARCH provider_connections USING INDEX idx_pc_priority (provider=?)`,
/// with the index's own `priority DESC` ordering satisfying the first sort key
/// and a temp b-tree only for the `id ASC` tiebreak. `idx_pc_active`
/// (`is_active`, schema_base.sql:48) is not chosen: it cannot carry the
/// `provider` equality, so it would only add a filter pass.
pub fn active_connection_ids(conn: &Connection, provider: &str) -> Result<Vec<String>> {
    let mut statement = conn.prepare(
        "SELECT id FROM provider_connections \
         WHERE is_active = 1 AND provider = ?1 \
         ORDER BY priority DESC, id ASC",
    )?;
    let rows = statement.query_map(params![provider], |row| row.get(0))?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(crate::error::DbError::from)
}

fn buckets(conn: &Connection, column: &str, since: &str, top: usize) -> Result<Vec<UsageBucket>> {
    // `column` is a fixed identifier chosen by the caller, never user input.
    let window = usage_window(1);
    let sql = format!(
        "SELECT COALESCE({column}, 'unknown') AS key, COUNT(*), \
                COALESCE(SUM(tokens_input), 0), COALESCE(SUM(tokens_output), 0) \
         FROM usage_history WHERE {window} \
         GROUP BY key ORDER BY COUNT(*) DESC LIMIT ?2"
    );
    let mut statement = conn.prepare(&sql)?;
    let rows = statement.query_map(params![since, top as i64], |row| {
        Ok(UsageBucket {
            key: row.get(0)?,
            requests: row.get(1)?,
            tokens_input: row.get(2)?,
            tokens_output: row.get(3)?,
        })
    })?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(crate::error::DbError::from)
}

/// A recent request as the dashboard shows it (no bodies, no secrets).
#[derive(Debug, Clone, PartialEq)]
pub struct CallLogSummary {
    pub timestamp: String,
    pub model: Option<String>,
    pub requested_model: Option<String>,
    pub provider: Option<String>,
    pub status: Option<i64>,
    pub duration_ms: Option<i64>,
    pub tokens_input: Option<i64>,
    pub tokens_output: Option<i64>,
    pub combo_name: Option<String>,
    pub error_summary: Option<String>,
}

/// Most recent `call_logs` rows, newest first.
pub fn recent_calls(conn: &Connection, limit: usize) -> Result<Vec<CallLogSummary>> {
    let mut statement = conn.prepare(
        "SELECT timestamp, model, requested_model, provider, status, duration, \
                tokens_in, tokens_out, combo_name, error_summary \
         FROM call_logs ORDER BY timestamp DESC LIMIT ?1",
    )?;
    let rows = statement.query_map(params![limit as i64], |row| {
        Ok(CallLogSummary {
            timestamp: row.get(0)?,
            model: row.get(1)?,
            requested_model: row.get(2)?,
            provider: row.get(3)?,
            status: row.get(4)?,
            duration_ms: row.get(5)?,
            tokens_input: row.get(6)?,
            tokens_output: row.get(7)?,
            combo_name: row.get(8)?,
            error_summary: row.get(9)?,
        })
    })?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(crate::error::DbError::from)
}

/// Number of API keys that can still authenticate.
///
/// Mirrors [`find_usable_api_key`] — callers treat this count as a security
/// signal, so revoked, banned, inactive, expired and secret-less rows must not
/// be counted. Expiry goes through the same [`timestamp_expired`] helper as the
/// auth hot path (rather than a SQL comparison) because `expires_at` mixes both
/// timestamp spellings; a malformed value is rejected by both.
pub fn count_active_api_keys(conn: &Connection) -> Result<usize> {
    let mut statement = conn.prepare(
        "SELECT expires_at FROM api_keys \
         WHERE revoked_at IS NULL AND COALESCE(is_banned, 0) = 0 \
           AND COALESCE(is_active, 1) = 1 AND COALESCE(TRIM(key), '') <> ''",
    )?;
    let rows = statement.query_map([], |row| row.get::<_, Option<String>>(0))?;
    let now = unix_now();
    let mut count = 0usize;
    for expires_at in rows {
        if let Ok(false) = timestamp_expired(expires_at?.as_deref(), now) {
            count += 1;
        }
    }
    Ok(count)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Db;

    fn migrated() -> Db {
        let db = Db::open_in_memory().unwrap();
        db.migrate().unwrap();
        db
    }

    fn sample_connection(id: &str) -> ConnectionRow {
        ConnectionRow {
            id: id.to_string(),
            provider: "grok-cli".to_string(),
            auth_type: Some("oauth".to_string()),
            name: None,
            email: Some("user@example.com".to_string()),
            priority: 10,
            is_active: true,
            access_token: Some("tok".to_string()),
            refresh_token: Some("ref".to_string()),
            expires_at: None,
            api_key: None,
            provider_specific_data: None,
        }
    }

    #[test]
    fn connections_round_trip_and_active_order() {
        let db = migrated();
        let guard = db.connection();

        upsert_connection(&guard, &sample_connection("c-low")).unwrap();
        let mut high = sample_connection("c-high");
        high.priority = 99;
        upsert_connection(&guard, &high).unwrap();
        let mut off = sample_connection("c-off");
        off.is_active = false;
        upsert_connection(&guard, &off).unwrap();

        let active = list_active_connections(&guard, Some("grok-cli")).unwrap();
        assert_eq!(active.len(), 2);
        assert_eq!(active[0].id, "c-high");
        assert_eq!(active[1].id, "c-low");

        let fetched = get_connection(&guard, "c-low").unwrap().unwrap();
        assert_eq!(fetched.email.as_deref(), Some("user@example.com"));

        assert!(get_connection(&guard, "missing").unwrap().is_none());
    }

    #[test]
    fn api_keys_round_trip_by_secret() {
        let db = migrated();
        let guard = db.connection();

        insert_api_key(
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

        let fetched = get_api_key_by_key(&guard, "sk-test-123").unwrap().unwrap();
        assert_eq!(fetched.id, "k1");
        assert!(get_api_key_by_key(&guard, "sk-nope").unwrap().is_none());
    }

    #[test]
    fn timestamps_parse_both_spellings() {
        assert_eq!(parse_db_timestamp("1970-01-01 00:00:00"), Some(0));
        assert_eq!(parse_db_timestamp("2026-09-17 13:00:00"), Some(1789650000));
        assert_eq!(
            parse_db_timestamp("2000-02-29T12:30:15.123Z"),
            Some(951827415)
        );
        assert_eq!(
            parse_db_timestamp("2000-02-29T14:30:15+02:00"),
            Some(951827415)
        );
        assert_eq!(parse_db_timestamp("not a date"), None);
        assert_eq!(parse_db_timestamp("2026-13-01 00:00:00"), None);
        assert_eq!(timestamp_expired(None, 1789650000), Ok(false));
        assert_eq!(timestamp_expired(Some("garbage"), 1789650000), Err(()));
        assert_eq!(
            timestamp_expired(Some("2026-09-17 13:00:01"), 1789650000),
            Ok(false)
        );
        assert_eq!(
            timestamp_expired(Some("2026-09-17 13:00:00"), 1789650000),
            Ok(true)
        );
    }

    #[test]
    fn usable_keys_honour_kill_switches() {
        let db = migrated();
        let guard = db.connection();

        for (id, key) in [
            ("good", "sk-good"),
            ("revoked", "sk-revoked"),
            ("banned", "sk-banned"),
            ("inactive", "sk-inactive"),
            ("expired", "sk-expired"),
        ] {
            guard
                .execute(
                    "INSERT INTO api_keys (id, name, key, created_at) VALUES (?1, ?2, ?3, datetime('now'))",
                    rusqlite::params![id, id, key],
                )
                .unwrap();
        }
        guard
            .execute(
                "UPDATE api_keys SET revoked_at = datetime('now') WHERE id = 'revoked'",
                [],
            )
            .unwrap();
        guard
            .execute("UPDATE api_keys SET is_banned = 1 WHERE id = 'banned'", [])
            .unwrap();
        guard
            .execute(
                "UPDATE api_keys SET is_active = 0 WHERE id = 'inactive'",
                [],
            )
            .unwrap();
        guard
            .execute(
                "UPDATE api_keys SET expires_at = '2001-01-01 00:00:00' WHERE id = 'expired'",
                [],
            )
            .unwrap();

        let usable = find_usable_api_key(&guard, "sk-good").unwrap().unwrap();
        assert_eq!(usable.id, "good");
        for dead in [
            "sk-revoked",
            "sk-banned",
            "sk-inactive",
            "sk-expired",
            "sk-nope",
        ] {
            assert!(
                find_usable_api_key(&guard, dead).unwrap().is_none(),
                "{dead} must not authenticate"
            );
        }
    }

    #[test]
    fn full_keys_insert_list_revoke_restore() {
        let db = migrated();
        let guard = db.connection();

        insert_full_api_key(
            &guard,
            &NewApiKey {
                id: "full-1".to_string(),
                name: "ops".to_string(),
                key: "sk-abc123".to_string(),
                key_prefix: "sk-abc123".to_string(),
                key_hash: api_key_hash("sk-abc123"),
                allowed_models: "[]".to_string(),
                scopes: "[\"manage\"]".to_string(),
            },
        )
        .unwrap();
        let stored_key: String = guard
            .query_row("SELECT key FROM api_keys WHERE id = 'full-1'", [], |row| {
                row.get(0)
            })
            .unwrap();
        // The column holds the marker, not a second copy of the digest and
        // not the secret: neither of those may authenticate.
        assert_ne!(stored_key, api_key_hash("sk-abc123"));
        assert!(!stored_key.contains("sk-abc123"));
        assert!(
            find_usable_api_key(&guard, &stored_key).unwrap().is_none(),
            "whatever sits in api_keys.key must not authenticate"
        );

        let listed = list_api_keys(&guard).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].name, "ops");
        assert!(!listed[0].is_encrypted);
        assert!(find_usable_api_key(&guard, "sk-abc123").unwrap().is_some());

        assert!(set_api_key_revoked(&guard, "full-1", true).unwrap());
        assert!(find_usable_api_key(&guard, "sk-abc123").unwrap().is_none());
        assert!(
            get_api_key_summary(&guard, "full-1")
                .unwrap()
                .unwrap()
                .revoked_at
                .is_some()
        );

        assert!(set_api_key_revoked(&guard, "full-1", false).unwrap());
        assert!(find_usable_api_key(&guard, "sk-abc123").unwrap().is_some());
        assert!(!set_api_key_revoked(&guard, "missing", true).unwrap());
    }

    // --- The `api_keys.key` digest-is-a-credential exploit (W-13 / C-9 / E4).
    //
    // A backup, a copied `storage.sqlite`, a log line or a support export
    // carries the `key` and `key_hash` columns verbatim. Whatever is stored
    // there must not work as a bearer token; only the operator's real secret
    // may authenticate.

    const SECRET: &str = "sk-exploit-synthetic-000000000000";

    fn issue(conn: &Connection, id: &str, secret: &str) {
        insert_full_api_key(
            conn,
            &NewApiKey {
                id: id.to_string(),
                name: id.to_string(),
                key: secret.to_string(),
                key_prefix: secret[..11].to_string(),
                key_hash: api_key_hash(secret),
                allowed_models: "[]".to_string(),
                scopes: "[\"manage\"]".to_string(),
            },
        )
        .unwrap();
    }

    /// The raw `key` / `key_hash` values of one row, straight out of SQLite —
    /// what anyone with a copy of the file is holding.
    fn raw_row(conn: &Connection, id: &str) -> (String, String) {
        conn.query_row(
            "SELECT key, key_hash FROM api_keys WHERE id = ?1",
            params![id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap()
    }

    #[test]
    fn stored_key_column_value_is_not_a_credential() {
        let db = migrated();
        let guard = db.connection();
        issue(&guard, "issued", SECRET);

        let (stored_key, _stored_hash) = raw_row(&guard, "issued");

        assert!(
            find_usable_api_key(&guard, SECRET).unwrap().is_some(),
            "the operator's real secret must authenticate"
        );
        assert!(
            find_usable_api_key(&guard, &stored_key).unwrap().is_none(),
            "the value stored in api_keys.key authenticates as a bearer token"
        );
        assert!(
            get_api_key_by_key(&guard, &stored_key).unwrap().is_none(),
            "the plain lookup accepts the value stored in api_keys.key"
        );
    }

    #[test]
    fn stored_digest_is_not_a_credential() {
        let db = migrated();
        let guard = db.connection();
        issue(&guard, "issued", SECRET);

        let (stored_key, stored_hash) = raw_row(&guard, "issued");
        assert_eq!(stored_hash, api_key_hash(SECRET));

        assert!(
            find_usable_api_key(&guard, &stored_hash).unwrap().is_none(),
            "the SHA-256 digest in api_keys.key_hash authenticates as a bearer token"
        );
        assert!(
            get_api_key_by_key(&guard, &stored_hash).unwrap().is_none(),
            "the plain lookup accepts the SHA-256 digest"
        );
        assert_ne!(
            stored_key, stored_hash,
            "api_keys.key must not duplicate the digest"
        );
        assert!(
            !stored_key.contains(SECRET),
            "api_keys.key must not hold a recoverable secret"
        );
    }

    /// Rows the pre-fix build already wrote: the digest sits in *both* columns.
    /// Live databases carry them, so the predicate — not just the writer — has
    /// to refuse the digest.
    #[test]
    fn a_pre_fix_hash_in_key_column_is_rejected() {
        let db = migrated();
        let guard = db.connection();
        let digest = api_key_hash(SECRET);
        guard
            .execute(
                "INSERT INTO api_keys (id, name, key, key_hash, key_prefix, created_at)
                 VALUES ('legacy-rust', 'legacy-rust', ?1, ?1, 'sk-expl', datetime('now'))",
                params![digest],
            )
            .unwrap();

        assert!(find_usable_api_key(&guard, SECRET).unwrap().is_some());
        assert!(
            find_usable_api_key(&guard, &digest).unwrap().is_none(),
            "a digest duplicated into api_keys.key authenticates"
        );
        assert!(get_api_key_by_key(&guard, &digest).unwrap().is_none());
    }

    /// A `enc:v1:` ciphertext left by the JS server in `key` (with the
    /// plaintext digest in `key_hash`) must be just as unusable as a digest.
    #[test]
    fn an_encrypted_key_column_value_is_not_a_credential() {
        let db = migrated();
        let guard = db.connection();
        let ciphertext = "enc:v1:00112233445566778899aabbccddeeff:aabb:ccdd";
        guard
            .execute(
                "INSERT INTO api_keys (id, name, key, key_hash, created_at)
                 VALUES ('js-enc', 'js-enc', ?1, ?2, datetime('now'))",
                params![ciphertext, api_key_hash(SECRET)],
            )
            .unwrap();

        assert!(find_usable_api_key(&guard, SECRET).unwrap().is_some());
        assert!(
            find_usable_api_key(&guard, ciphertext).unwrap().is_none(),
            "the ciphertext in api_keys.key authenticates as a bearer token"
        );
    }

    /// Legacy rows hold the plaintext secret in `key`, and the frozen JS server
    /// matches them verbatim. They must keep working — with and without a
    /// backfilled `key_hash`. (`api_keys.key` is UNIQUE, so each row needs its
    /// own secret.)
    #[test]
    fn legacy_plaintext_rows_still_authenticate() {
        let db = migrated();
        let guard = db.connection();
        let second = "sk-exploit-synthetic-legacy-222";
        for (id, secret, backfill) in [
            ("legacy-null", SECRET, false),
            ("legacy-hashed", second, true),
        ] {
            guard
                .execute(
                    "INSERT INTO api_keys (id, name, key, key_hash, created_at)
                     VALUES (?1, ?1, ?2, ?3, datetime('now'))",
                    params![id, secret, backfill.then(|| api_key_hash(secret))],
                )
                .unwrap();
            assert_eq!(
                find_usable_api_key(&guard, secret)
                    .unwrap()
                    .map(|row| row.id),
                Some(id.to_string()),
                "{id}: a legacy plaintext row must still authenticate"
            );
            assert!(
                get_api_key_by_key(&guard, secret).unwrap().is_some(),
                "{id}: the plain lookup must still find it"
            );
        }
        // The backfilled digest is not a credential either.
        assert!(
            find_usable_api_key(&guard, &api_key_hash(second))
                .unwrap()
                .is_none(),
            "a backfilled digest must not authenticate"
        );
    }

    /// A dead row stays dead whichever value is presented.
    #[test]
    fn revoked_banned_and_expired_rows_reject_secret_and_digest() {
        let db = migrated();
        let guard = db.connection();
        for (id, flag) in [
            ("revoked", "revoked_at = datetime('now')"),
            ("banned", "is_banned = 1"),
            ("inactive", "is_active = 0"),
            ("expired", "expires_at = '2001-01-01 00:00:00'"),
        ] {
            issue(&guard, id, &format!("{SECRET}-{id}"));
            guard
                .execute(&format!("UPDATE api_keys SET {flag} WHERE id = '{id}'"), [])
                .unwrap();
        }
        for id in ["revoked", "banned", "inactive", "expired"] {
            let secret = format!("{SECRET}-{id}");
            let (stored_key, stored_hash) = raw_row(&guard, id);
            for presented in [secret.as_str(), stored_key.as_str(), stored_hash.as_str()] {
                assert!(
                    find_usable_api_key(&guard, presented).unwrap().is_none(),
                    "{id}: a dead row must reject {presented}"
                );
            }
        }
    }

    #[test]
    fn issued_keys_do_not_collide() {
        let db = migrated();
        let guard = db.connection();
        let second = "sk-exploit-synthetic-111111111111";
        issue(&guard, "one", SECRET);
        issue(&guard, "two", second);

        assert_eq!(
            find_usable_api_key(&guard, SECRET)
                .unwrap()
                .map(|row| row.id),
            Some("one".to_string())
        );
        assert_eq!(
            find_usable_api_key(&guard, second)
                .unwrap()
                .map(|row| row.id),
            Some("two".to_string())
        );
        for id in ["one", "two"] {
            let (stored_key, stored_hash) = raw_row(&guard, id);
            assert_ne!(stored_key, stored_hash);
            for presented in [stored_key.as_str(), stored_hash.as_str()] {
                assert!(find_usable_api_key(&guard, presented).unwrap().is_none());
                assert!(get_api_key_by_key(&guard, presented).unwrap().is_none());
            }
        }
        assert_eq!(list_api_keys(&guard).unwrap().len(), 2);
        assert_eq!(count_active_api_keys(&guard).unwrap(), 2);
    }

    /// The summary the dashboard lists carries neither the secret nor the
    /// digest, in any field.
    #[test]
    fn listed_summaries_carry_no_secret_or_digest() {
        let db = migrated();
        let guard = db.connection();
        issue(&guard, "issued", SECRET);

        let listed = format!("{:?}", list_api_keys(&guard).unwrap());
        let one = format!("{:?}", get_api_key_summary(&guard, "issued").unwrap());
        let digest = api_key_hash(SECRET);
        for rendered in [&listed, &one] {
            assert!(!rendered.contains(SECRET), "summary leaks the secret");
            assert!(!rendered.contains(&digest), "summary leaks the digest");
        }
    }

    /// `POST /api/keys` needs a non-blank `key` for the *gate* in `auth.rs`
    /// (`TRIM(key) != ''`), or the freshly issued row would read as "no keys
    /// exist" and open the gateway. The placeholder is not blank, and is not
    /// credential-shaped either.
    #[test]
    fn a_hash_only_row_keeps_the_auth_gate_closed() {
        let db = migrated();
        let guard = db.connection();
        issue(&guard, "issued", SECRET);

        let stored_key = raw_row(&guard, "issued").0;
        assert!(!stored_key.trim().is_empty());
        assert_eq!(count_active_api_keys(&guard).unwrap(), 1);
        let summary = get_api_key_summary(&guard, "issued").unwrap().unwrap();
        assert!(!summary.is_encrypted);
    }

    #[test]
    fn combos_round_trip_and_order() {
        let db = migrated();
        let guard = db.connection();

        upsert_combo(
            &guard,
            &ComboRow {
                id: "b".to_string(),
                name: "b-combo".to_string(),
                data: "{}".to_string(),
                sort_order: 2,
                active: false,
            },
        )
        .unwrap();
        upsert_combo(
            &guard,
            &ComboRow {
                id: "a".to_string(),
                name: "a-combo".to_string(),
                data: "{}".to_string(),
                sort_order: 1,
                active: false,
            },
        )
        .unwrap();

        let list = list_combos(&guard).unwrap();
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].id, "a");

        assert_eq!(
            get_combo_by_name(&guard, "b-combo").unwrap().unwrap().id,
            "b"
        );
        assert_eq!(get_combo(&guard, "a").unwrap().unwrap().name, "a-combo");
    }

    #[test]
    fn combo_meta_patch_keeps_json_in_sync() {
        let db = migrated();
        let guard = db.connection();

        upsert_combo(
            &guard,
            &ComboRow {
                id: "c1".to_string(),
                name: "alpha".to_string(),
                data: r#"{"name":"alpha","sortOrder":1,"isHidden":false,"models":[]}"#.to_string(),
                sort_order: 1,
                active: false,
            },
        )
        .unwrap();

        // Regression (ADR-015): patching is_active must flip the `active`
        // column — the flag `list_active_combos` filters on — not just the
        // JSON mirror.
        upsert_combo(
            &guard,
            &ComboRow {
                id: "c2".to_string(),
                name: "gamma".to_string(),
                data: r#"{"name":"gamma","models":[{"id":"m1"}]}"#.to_string(),
                sort_order: 2,
                active: true,
            },
        )
        .unwrap();
        let archived = update_combo_meta(
            &guard,
            "c2",
            &ComboMetaPatch {
                is_active: Some(false),
                ..Default::default()
            },
        )
        .unwrap()
        .expect("row exists");
        assert!(!archived.active);
        assert!(
            list_active_combos(&guard)
                .unwrap()
                .iter()
                .all(|row| row.id != "c2")
        );
        let restored = update_combo_meta(
            &guard,
            "c2",
            &ComboMetaPatch {
                is_active: Some(true),
                ..Default::default()
            },
        )
        .unwrap()
        .expect("row exists");
        assert!(restored.active);
        assert!(
            list_active_combos(&guard)
                .unwrap()
                .iter()
                .any(|row| row.id == "c2")
        );

        let updated = update_combo_meta(
            &guard,
            "c1",
            &ComboMetaPatch {
                sort_order: Some(9),
                is_hidden: Some(true),
                ..Default::default()
            },
        )
        .unwrap()
        .expect("row exists");
        assert_eq!(updated.sort_order, 9);
        assert_eq!(updated.name, "alpha");
        let data: serde_json::Value = serde_json::from_str(&updated.data).unwrap();
        assert_eq!(data["sortOrder"], serde_json::json!(9));
        assert_eq!(data["isHidden"], serde_json::json!(true));
        assert_eq!(data["name"], serde_json::json!("alpha"));

        let renamed = update_combo_meta(
            &guard,
            "c1",
            &ComboMetaPatch {
                name: Some("beta".to_string()),
                ..Default::default()
            },
        )
        .unwrap()
        .expect("row exists");
        assert_eq!(renamed.name, "beta");
        let data: serde_json::Value = serde_json::from_str(&renamed.data).unwrap();
        assert_eq!(data["name"], serde_json::json!("beta"));
        // Untouched fields survive.
        assert_eq!(data["sortOrder"], serde_json::json!(9));

        assert!(get_combo_by_name(&guard, "beta").unwrap().is_some());
        assert!(
            update_combo_meta(&guard, "missing", &ComboMetaPatch::default())
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn combo_models_rewrite_round_trips() {
        let db = migrated();
        let guard = db.connection();

        upsert_combo(
            &guard,
            &ComboRow {
                id: "c1".to_string(),
                name: "mix".to_string(),
                data: r#"{"name":"mix","strategy":"priority","sortOrder":0,
                    "models":[
                        {"id":"keep-me","kind":"model","model":"groq/openai/gpt-oss-20b","providerId":"groq","weight":95,"label":"Fast"},
                        {"id":"legacy","kind":"widget","model":"x/y"}
                    ]}"#
                    .to_string(),
                sort_order: 0,
                active: false,
            },
        )
        .unwrap();

        let updated = update_combo_meta(
            &guard,
            "c1",
            &ComboMetaPatch {
                strategy: Some("auto".to_string()),
                models: Some(vec![
                    ComboModelSpec {
                        id: Some("keep-me".to_string()),
                        provider: "groq".to_string(),
                        // Joined verbatim: provider + bare id, slashes kept.
                        model: "openai/gpt-oss-20b".to_string(),
                        weight: 80,
                    },
                    ComboModelSpec {
                        id: None,
                        provider: "gemini".to_string(),
                        model: "gemini-3-flash-preview".to_string(),
                        weight: 70,
                    },
                ]),
                ..Default::default()
            },
        )
        .unwrap()
        .expect("row exists");
        let data: serde_json::Value = serde_json::from_str(&updated.data).unwrap();
        assert_eq!(data["strategy"], serde_json::json!("auto"));
        let models = data["models"].as_array().unwrap();
        // Two rewritten models plus the preserved non-model item.
        assert_eq!(models.len(), 3);
        assert_eq!(models[0]["id"], serde_json::json!("keep-me"));
        assert_eq!(
            models[0]["model"],
            serde_json::json!("groq/openai/gpt-oss-20b")
        );
        assert_eq!(models[0]["weight"], serde_json::json!(80));
        // Unknown extra keys survive the rewrite via the matched id.
        assert_eq!(models[0]["label"], serde_json::json!("Fast"));
        assert_eq!(
            models[1]["model"],
            serde_json::json!("gemini/gemini-3-flash-preview")
        );
        assert!(
            models[1]["id"]
                .as_str()
                .is_some_and(|id| id.starts_with("mix-model-2-gemini-"))
        );
        assert_eq!(models[2]["id"], serde_json::json!("legacy"));
    }

    #[test]
    fn connection_meta_patch_toggles_active() {
        let db = migrated();
        let guard = db.connection();

        upsert_connection(&guard, &sample_connection("c1")).unwrap();

        let updated = update_connection_meta(
            &guard,
            "c1",
            &ConnectionMetaPatch {
                is_active: Some(false),
                priority: Some(3),
                ..Default::default()
            },
        )
        .unwrap()
        .expect("row exists");
        assert!(!updated.is_active);
        assert_eq!(updated.priority, 3);
        // Untouched columns survive.
        assert_eq!(updated.provider, "grok-cli");

        assert!(
            update_connection_meta(&guard, "missing", &ConnectionMetaPatch::default())
                .unwrap()
                .is_none()
        );
    }

    /// Re-spell a `YYYY-MM-DD HH:MM:SS` stamp as ISO-8601, the way the Node
    /// server wrote `usage_history.timestamp`.
    fn iso(stamp: &str) -> String {
        format!("{}.000Z", stamp.replacen(' ', "T", 1))
    }

    #[test]
    fn usage_window_counts_both_timestamp_spellings() {
        let db = migrated();
        let guard = db.connection();

        // Anchor the rows to the exact cutoff `usage_summary` will build, so
        // the boundary cases never depend on the wall clock's time of day.
        let cutoff: String = guard
            .query_row("SELECT datetime('now', '-7 days')", [], |row| row.get(0))
            .unwrap();
        let (before, after, stale): (String, String, String) = guard
            .query_row(
                "SELECT datetime(?1, '-1 hour'), datetime(?1, '+1 hour'), datetime(?1, '-1 day')",
                params![cutoff],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();

        for (model, stamp) in [
            ("sqlite-after", &after),
            ("iso-after", &iso(&after)),
            ("sqlite-before", &before),
            ("iso-before", &iso(&before)),
            ("sqlite-stale", &stale),
            ("iso-stale", &iso(&stale)),
        ] {
            guard
                .execute(
                    "INSERT INTO usage_history (provider, model, success, timestamp)
                     VALUES ('test', ?1, 1, ?2)",
                    params![model, stamp],
                )
                .unwrap();
        }

        let summary = usage_summary(&guard, "-7 days", 10).unwrap();
        assert_eq!(summary.requests, 2, "only the two post-cutoff rows");
        assert_eq!(summary.successes, 2);
        let mut models: Vec<&str> = summary
            .by_model
            .iter()
            .map(|bucket| bucket.key.as_str())
            .collect();
        models.sort_unstable();
        assert_eq!(models, vec!["iso-after", "sqlite-after"]);
        let providers = &summary.by_provider;
        assert_eq!(providers.len(), 1);
        assert_eq!(providers[0].key, "test");
        assert_eq!(providers[0].requests, 2);
    }

    #[test]
    fn active_api_key_count_matches_the_auth_gate() {
        let db = migrated();
        let guard = db.connection();

        for (id, key) in [
            ("good", "sk-good"),
            ("inactive", "sk-inactive"),
            ("banned", "sk-banned"),
            ("revoked", "sk-revoked"),
            ("expired-sqlite", "sk-expired-sqlite"),
            ("expired-iso", "sk-expired-iso"),
            ("future-sqlite", "sk-future-sqlite"),
            ("future-iso", "sk-future-iso"),
            ("no-expiry", "sk-no-expiry"),
            ("blank-expiry", "sk-blank-expiry"),
            ("garbage-expiry", "sk-garbage-expiry"),
            ("blank-key", ""),
        ] {
            guard
                .execute(
                    "INSERT INTO api_keys (id, name, key, created_at)
                     VALUES (?1, ?1, ?2, datetime('now'))",
                    rusqlite::params![id, key],
                )
                .unwrap();
        }
        for (id, flag) in [
            ("inactive", "is_active = 0"),
            ("banned", "is_banned = 1"),
            ("revoked", "revoked_at = datetime('now')"),
            ("expired-sqlite", "expires_at = datetime('now', '-1 day')"),
            (
                "expired-iso",
                "expires_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now', '-1 day')",
            ),
            ("future-sqlite", "expires_at = datetime('now', '+1 day')"),
            (
                "future-iso",
                "expires_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now', '+1 day')",
            ),
            ("blank-expiry", "expires_at = ''"),
            ("garbage-expiry", "expires_at = 'not-a-date'"),
        ] {
            guard
                .execute(&format!("UPDATE api_keys SET {flag} WHERE id = '{id}'"), [])
                .unwrap();
        }

        let usable = [
            "sk-good",
            "sk-future-sqlite",
            "sk-future-iso",
            "sk-no-expiry",
            "sk-blank-expiry",
        ];
        for key in usable {
            assert!(
                find_usable_api_key(&guard, key).unwrap().is_some(),
                "{key} must authenticate"
            );
        }
        for dead in [
            "sk-inactive",
            "sk-banned",
            "sk-revoked",
            "sk-expired-sqlite",
            "sk-expired-iso",
            "sk-garbage-expiry",
        ] {
            assert!(
                find_usable_api_key(&guard, dead).unwrap().is_none(),
                "{dead} must not authenticate"
            );
        }

        assert_eq!(count_active_api_keys(&guard).unwrap(), usable.len());

        let empty = migrated();
        let empty_guard = empty.connection();
        assert_eq!(count_active_api_keys(&empty_guard).unwrap(), 0);
    }

    #[test]
    fn key_value_set_get_delete() {
        let db = migrated();
        let guard = db.connection();

        assert_eq!(kv_get(&guard, "settings", "theme").unwrap(), None);
        kv_set(&guard, "settings", "theme", "dark").unwrap();
        assert_eq!(
            kv_get(&guard, "settings", "theme").unwrap().as_deref(),
            Some("dark")
        );
        kv_set(&guard, "settings", "theme", "light").unwrap();
        assert_eq!(
            kv_get(&guard, "settings", "theme").unwrap().as_deref(),
            Some("light")
        );
        kv_delete(&guard, "settings", "theme").unwrap();
        assert_eq!(kv_get(&guard, "settings", "theme").unwrap(), None);
    }

    // --- M1.3: latency percentiles + the raw rows a cost percentile needs.

    /// One `usage_history` row an hour old, so every test below sits inside a
    /// `-7 days` window whatever the wall clock says.
    fn seed_latency(conn: &Connection, model: &str, latency_ms: i64) {
        conn.execute(
            "INSERT INTO usage_history (provider, model, success, latency_ms, timestamp)
             VALUES ('test', ?1, 1, ?2, datetime('now', '-1 hour'))",
            params![model, latency_ms],
        )
        .unwrap();
    }

    #[test]
    fn usage_summary_reports_nearest_rank_latency_percentiles() {
        let db = migrated();
        let guard = db.connection();

        // 1..=20 → nearest-rank p50 = rank 10 = 10, p95 = rank 19 = 19,
        // p99 = rank 20 = 20: three distinct values, so a constant or an
        // average cannot pass.
        for latency_ms in 1..=20 {
            seed_latency(&guard, "grok-4.6", latency_ms);
        }
        // Outside the window: must not move a single percentile.
        guard
            .execute(
                "INSERT INTO usage_history (provider, model, success, latency_ms, timestamp)
                 VALUES ('test', 'grok-4.6', 1, 9999, datetime('now', '-40 days'))",
                [],
            )
            .unwrap();

        let summary = usage_summary(&guard, "-7 days", 10).unwrap();
        assert_eq!(summary.latency.p50_ms, Some(10));
        assert_eq!(summary.latency.p95_ms, Some(19));
        assert_eq!(summary.latency.p99_ms, Some(20));
    }

    #[test]
    fn usage_summary_latency_percentiles_are_none_without_rows() {
        let db = migrated();
        let guard = db.connection();

        // Empty window: None, not a panic and not a fabricated zero.
        let summary = usage_summary(&guard, "-7 days", 10).unwrap();
        assert_eq!(summary.latency, LatencyPercentiles::default());
        assert_eq!(summary.latency.p50_ms, None);
        assert_eq!(summary.latency.p95_ms, None);
        assert_eq!(summary.latency.p99_ms, None);

        // A window whose only rows are stale stays empty.
        seed_latency(&guard, "grok-4.6", 42);
        guard.execute("DELETE FROM usage_history", []).unwrap();
        guard
            .execute(
                "INSERT INTO usage_history (provider, model, success, latency_ms, timestamp)
                 VALUES ('test', 'grok-4.6', 1, 42, datetime('now', '-40 days'))",
                [],
            )
            .unwrap();
        assert_eq!(
            usage_summary(&guard, "-7 days", 10).unwrap().latency,
            LatencyPercentiles::default()
        );

        // A single sample is its own p50/p95/p99.
        guard
            .execute(
                "INSERT INTO usage_history (provider, model, success, latency_ms, timestamp)
                 VALUES ('test', 'grok-4.6', 1, 77, datetime('now', '-1 hour'))",
                [],
            )
            .unwrap();
        let single = usage_summary(&guard, "-7 days", 10).unwrap().latency;
        assert_eq!(single.p50_ms, Some(77));
        assert_eq!(single.p95_ms, Some(77));
        assert_eq!(single.p99_ms, Some(77));
    }

    /// The percentile window must be the same window as the totals — both
    /// timestamp spellings, normalised once by [`ts_prefix`].
    #[test]
    fn usage_summary_latency_percentiles_span_both_timestamp_spellings() {
        let db = migrated();
        let guard = db.connection();

        let cutoff: String = guard
            .query_row("SELECT datetime('now', '-7 days')", [], |row| row.get(0))
            .unwrap();
        let (before, after): (String, String) = guard
            .query_row(
                "SELECT datetime(?1, '-1 hour'), datetime(?1, '+1 hour')",
                params![cutoff],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        for (model, stamp, latency_ms) in [
            ("sqlite-before", before.as_str(), 9999),
            ("iso-before", iso(&before).as_str(), 9999),
            ("sqlite-after", after.as_str(), 10),
            ("iso-after", iso(&after).as_str(), 30),
        ] {
            guard
                .execute(
                    "INSERT INTO usage_history (provider, model, success, latency_ms, timestamp)
                     VALUES ('test', ?1, 1, ?2, ?3)",
                    params![model, latency_ms, stamp],
                )
                .unwrap();
        }

        let latency = usage_summary(&guard, "-7 days", 10).unwrap().latency;
        // Sorted [10, 30]: p50 = rank 1 = 10, p95/p99 = rank 2 = 30.
        assert_eq!(latency.p50_ms, Some(10));
        assert_eq!(latency.p95_ms, Some(30));
        assert_eq!(latency.p99_ms, Some(30));
    }

    /// The documented nearest-rank definition, checked against its own
    /// boundary cases rather than through the summary.
    #[test]
    fn nearest_rank_percentile_definition() {
        assert_eq!(nearest_rank(&[], 0.5), None);
        assert_eq!(nearest_rank(&[], 0.99), None);
        assert_eq!(nearest_rank(&[7], 0.5), Some(7));
        assert_eq!(nearest_rank(&[7], 0.99), Some(7));
        assert_eq!(nearest_rank(&[1, 2], 0.5), Some(1));
        assert_eq!(nearest_rank(&[1, 2], 0.95), Some(2));
        assert_eq!(nearest_rank(&[1, 2, 3, 4], 0.5), Some(2));
        assert_eq!(nearest_rank(&[1, 2, 3, 4], 0.95), Some(4));
        // Unsorted input is the caller's business: the slice must be sorted.
        assert_eq!(nearest_rank(&[1, 2, 3, 4], 0.0), Some(1));
        assert_eq!(nearest_rank(&[1, 2, 3, 4], 1.0), Some(4));
    }

    /// Cost percentiles live in the `usage` crate (it owns the price table);
    /// this returns exactly what `PriceTable::cost_usd` is called with.
    #[test]
    fn usage_cost_rows_carry_the_inputs_a_cost_percentile_needs() {
        let db = migrated();
        let guard = db.connection();

        guard
            .execute(
                "INSERT INTO usage_history (provider, model, success, latency_ms,
                        tokens_input, tokens_output, tokens_cache_read,
                        tokens_cache_creation, tokens_reasoning, timestamp)
                 VALUES ('test', 'grok-4.6', 1, 120, 1000, 500, 200, 50, 30,
                         datetime('now', '-1 hour'))",
                [],
            )
            .unwrap();
        // Same numbers, ISO spelling, a NULL model and no cache detail: a
        // cost percentile has to be able to price both (or report the row
        // unpriced) without a second query.
        guard
            .execute(
                "INSERT INTO usage_history (provider, model, success, latency_ms,
                        tokens_input, tokens_output, timestamp)
                 VALUES ('test', NULL, 1, 340, 10, 20, ?1)",
                params![iso(&after_hour(&guard))],
            )
            .unwrap();
        // Outside the window.
        guard
            .execute(
                "INSERT INTO usage_history (provider, model, success, latency_ms, timestamp)
                 VALUES ('test', 'grok-4.6', 1, 9999, datetime('now', '-40 days'))",
                [],
            )
            .unwrap();

        let rows = usage_cost_rows_in_window(&guard, "-7 days").unwrap();
        assert_eq!(rows.len(), 2, "only the two in-window rows");
        let priced = rows
            .iter()
            .find(|row| row.model.as_deref() == Some("grok-4.6"))
            .expect("priced row");
        assert_eq!(priced.tokens_input, 1000);
        assert_eq!(priced.tokens_output, 500);
        assert_eq!(priced.tokens_cache_read, 200);
        assert_eq!(priced.tokens_cache_creation, 50);
        assert_eq!(priced.tokens_reasoning, 30);
        assert_eq!(priced.latency_ms, 120);
        let unpriced = rows
            .iter()
            .find(|row| row.model.as_deref().is_none_or(|model| model.is_empty()))
            .expect("row without a model");
        assert_eq!(unpriced.tokens_input, 10);
        assert_eq!(unpriced.tokens_cache_read, 0);
        assert_eq!(unpriced.latency_ms, 340);

        assert!(
            usage_cost_rows_in_window(&migrated().connection(), "-7 days")
                .unwrap()
                .is_empty()
        );
    }

    /// `datetime('now', '-1 hour')` read back through the connection, so an
    /// ISO stamp can be built from the exact same instant as a SQLite one.
    fn after_hour(conn: &Connection) -> String {
        conn.query_row("SELECT datetime('now', '-1 hour')", [], |row| row.get(0))
            .unwrap()
    }

    // --- M1.6: the retention sweep.

    /// Stamps the fixture rows are written from, so the sweep's own cutoff and
    /// the boundary row can be made to name the same second.
    fn sweep_now(conn: &Connection) -> String {
        conn.query_row("SELECT datetime('now')", [], |row| row.get(0))
            .unwrap()
    }

    /// Three generations per spelling: older than the cutoff, exactly at it,
    /// and inside the window.
    fn seed_sweep_fixture(conn: &Connection, now: &str) {
        let edge = format!("-{SWEEP_DAYS} days");
        for (model, modifier) in [
            ("old", "-8 days".to_string()),
            ("edge", edge),
            ("fresh", "-1 day".to_string()),
        ] {
            let sqlite: String = conn
                .query_row("SELECT datetime(?1, ?2)", params![now, modifier], |row| {
                    row.get(0)
                })
                .unwrap();
            for (stamp, spelling) in [(sqlite.clone(), "sqlite"), (iso(&sqlite), "iso")] {
                guard_insert_usage(conn, model, spelling, &stamp);
                guard_insert_call_log(conn, model, spelling, &stamp);
            }
        }
    }

    fn guard_insert_usage(conn: &Connection, model: &str, spelling: &str, stamp: &str) {
        conn.execute(
            "INSERT INTO usage_history (provider, model, success, timestamp)
             VALUES ('test', ?1 || '-' || ?2, 1, ?3)",
            params![model, spelling, stamp],
        )
        .unwrap();
    }

    fn guard_insert_call_log(conn: &Connection, model: &str, spelling: &str, stamp: &str) {
        conn.execute(
            "INSERT INTO call_logs (id, timestamp, status) VALUES (?1 || '-' || ?2, ?3, 200)",
            params![model, spelling, stamp],
        )
        .unwrap();
    }

    fn models_left(conn: &Connection) -> Vec<String> {
        let mut statement = conn
            .prepare("SELECT model FROM usage_history ORDER BY model ASC")
            .unwrap();
        let mut models: Vec<String> = statement
            .query_map([], |row| row.get(0))
            .unwrap()
            .map(|row| row.unwrap())
            .collect();
        models.sort();
        models
    }

    fn call_logs_left(conn: &Connection) -> Vec<String> {
        let mut statement = conn
            .prepare("SELECT id FROM call_logs ORDER BY id ASC")
            .unwrap();
        let mut ids: Vec<String> = statement
            .query_map([], |row| row.get(0))
            .unwrap()
            .map(|row| row.unwrap())
            .collect();
        ids.sort();
        ids
    }

    const SWEEP_DAYS: u32 = 7;

    /// `retention_days == 0` means "keep everything". The off-by-one that
    /// makes it "delete everything" is the whole point of this sweep, so it
    /// gets its own test: every fixture row is older than a `now` cutoff.
    #[test]
    fn purge_with_zero_retention_keeps_everything() {
        let db = migrated();
        let guard = db.connection();
        let now = sweep_now(&guard);
        seed_sweep_fixture(&guard, &now);

        let outcome = purge_usage_older_than(&guard, 0).unwrap();
        assert_eq!(outcome.usage_history, 0);
        assert_eq!(outcome.call_logs, 0);
        assert_eq!(models_left(&guard).len(), 6, "no usage row may go");
        assert_eq!(call_logs_left(&guard).len(), 6, "no call log may go");
    }

    /// Both spellings, and a row stamped exactly at the sweep's own cutoff:
    /// strictly older goes, the boundary row stays.
    #[test]
    fn purge_removes_both_spellings_older_than_the_cutoff() {
        let db = migrated();
        let guard = db.connection();

        // The cutoff is `datetime('now', '-7 days')` evaluated inside the
        // purge, so a row stamped at the cutoff only survives if no whole
        // second passed between the two evaluations. Re-stamp while the clock
        // ticks instead of asserting a coin flip.
        for _ in 0..50 {
            guard.execute("DELETE FROM usage_history", []).unwrap();
            guard.execute("DELETE FROM call_logs", []).unwrap();
            let now = sweep_now(&guard);
            seed_sweep_fixture(&guard, &now);

            let outcome = purge_usage_older_than(&guard, SWEEP_DAYS).unwrap();
            if sweep_now(&guard) != now {
                continue;
            }

            // Two spellings of the two stale generations.
            assert_eq!(outcome.usage_history, 2, "only the stale rows go");
            assert_eq!(outcome.call_logs, 2);
            assert_eq!(
                models_left(&guard),
                vec!["edge-iso", "edge-sqlite", "fresh-iso", "fresh-sqlite",],
                "the row exactly at the cutoff survives"
            );
            assert_eq!(
                call_logs_left(&guard),
                vec!["edge-iso", "edge-sqlite", "fresh-iso", "fresh-sqlite"]
            );
            return;
        }
        panic!("the wall clock kept ticking between two queries");
    }

    // --- M1.4: naming the connection that served a request.

    #[test]
    fn active_connection_ids_are_ordered_and_provider_scoped() {
        let db = migrated();
        let guard = db.connection();

        assert!(
            active_connection_ids(&guard, "grok-cli")
                .unwrap()
                .is_empty(),
            "a provider with no row at all yields nothing"
        );

        upsert_connection(&guard, &sample_connection("c-only")).unwrap();
        assert_eq!(
            active_connection_ids(&guard, "grok-cli").unwrap(),
            vec!["c-only".to_string()],
            "one active row is named"
        );

        let mut high = sample_connection("c-high");
        high.priority = 99;
        upsert_connection(&guard, &high).unwrap();
        let mut tie_low = sample_connection("c-tie-low");
        tie_low.priority = 10;
        upsert_connection(&guard, &tie_low).unwrap();
        let mut tie_high = sample_connection("c-tie-high");
        tie_high.priority = 10;
        upsert_connection(&guard, &tie_high).unwrap();
        // Demote the first row so the order has to span three priorities.
        let mut only = sample_connection("c-only");
        only.priority = 5;
        upsert_connection(&guard, &only).unwrap();
        // Inactive rows are not candidates at all.
        let mut off = sample_connection("c-off");
        off.is_active = false;
        off.priority = 1000;
        upsert_connection(&guard, &off).unwrap();
        // Another provider entirely.
        let mut other = sample_connection("c-other");
        other.provider = "gemini".to_string();
        upsert_connection(&guard, &other).unwrap();

        assert_eq!(
            active_connection_ids(&guard, "grok-cli").unwrap(),
            vec![
                "c-high".to_string(),
                "c-tie-high".to_string(),
                "c-tie-low".to_string(),
                "c-only".to_string(),
            ],
            "priority DESC, then id ASC — deterministic, and not the inactive row"
        );

        // Every id resolves back to a row, so a caller can record it verbatim.
        for id in active_connection_ids(&guard, "grok-cli").unwrap() {
            let row = get_connection(&guard, &id).unwrap().unwrap();
            assert!(row.is_active);
            assert_eq!(row.provider, "grok-cli");
        }
    }

    /// An alias-keyed row: same account, reachable under the alias the router
    /// resolves. The id is handed back exactly as stored, and a query for one
    /// alias never leaks the other provider's row.
    #[test]
    fn active_connection_ids_handle_alias_keyed_rows() {
        let db = migrated();
        let guard = db.connection();

        let mut alias = sample_connection("grok-cli::acct-9f2a");
        alias.provider = "grok-cli".to_string();
        alias.name = Some("work (alias)".to_string());
        upsert_connection(&guard, &alias).unwrap();
        let mut canonical = sample_connection("grok-cli-acct-9f2a");
        canonical.provider = "grok".to_string();
        upsert_connection(&guard, &canonical).unwrap();

        assert_eq!(
            active_connection_ids(&guard, "grok-cli").unwrap(),
            vec!["grok-cli::acct-9f2a".to_string()],
            "the alias id comes back verbatim, scoped to its own provider"
        );
        assert_eq!(
            active_connection_ids(&guard, "grok").unwrap(),
            vec!["grok-cli-acct-9f2a".to_string()]
        );
        assert!(
            active_connection_ids(&guard, "grok-cl").unwrap().is_empty(),
            "a provider name is matched whole, never as a prefix"
        );
    }
}

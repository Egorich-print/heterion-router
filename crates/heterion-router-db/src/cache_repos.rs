//! Response-cache storage on `semantic_cache` (roadmap M3.2).
//!
//! The table is in the shipped 001 base schema and was dead code until now.
//! What it actually declares (`schema_base.sql:250`) — worth stating because
//! the audit note ("no primary key, no index") is wrong:
//!
//! * `id TEXT PRIMARY KEY` and `signature TEXT NOT NULL UNIQUE`. The UNIQUE
//!   index is the only lookup path the cache needs, and it is automatic.
//! * `idx_sc_sig` / `idx_sc_model` are redundant with it (a UNIQUE column is
//!   already indexed); harmless, left alone.
//! * **No vector column and no `expires_at` index.** Two consequences, both
//!   handled here rather than with a migration:
//!
//!   1. A semantic entry is stored as **two rows**: the answer under
//!      `signature = "x:<ns>:<path>:<prompt hash>"` and its vector under
//!      `signature = "v:<ns>:<path>:<prompt hash>"`, the vector itself in the
//!      `response` column as a JSON array. `signature` prefixes are ASCII
//!      and `:` (0x3a) sorts before `;` (0x3b), so [`vectors_in`] can scan
//!      one namespace, path and model as a **range** over the UNIQUE index
//!      rather than a table scan.
//!   2. [`sweep_expired`] is a full scan. At laptop scale (thousands of rows,
//!      one row per cacheable completion) that is microseconds, and the sweep
//!      only runs on a write. The index worth having is
//!      `CREATE INDEX idx_sc_expires ON semantic_cache(expires_at)`; it is
//!      **not** created from Rust here on purpose — this crate's DDL belongs
//!      in `MIGRATIONS` (`migrate.rs`), which owns the ledger.
//!
//! Timestamps are produced by SQLite itself (`datetime('now')`,
//! `datetime('now', '+N seconds')`) so they compare with the same
//! `datetime(expires_at) > datetime('now')` predicate `auth.rs` already uses
//! for API-key expiry. No date library, no second spelling of "now".

use rusqlite::{Connection, OptionalExtension, params};
use sha2::{Digest, Sha256};

use crate::error::Result;

/// One cached response, as stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CacheRow {
    pub signature: String,
    pub model: String,
    pub prompt_hash: String,
    /// The stored response body, verbatim.
    pub response: String,
    /// Tokens the stored answer saved, accumulated over its hits.
    pub tokens_saved: i64,
    pub hit_count: i64,
    pub created_at: String,
    pub expires_at: String,
}

/// A cacheable answer to store.
#[derive(Debug, Clone)]
pub struct CacheWrite {
    /// Namespaced key: `x:<namespace>:<path tag>:<prompt hash>`.
    pub signature: String,
    /// Model that answered, so a semantic scan can stay inside one model.
    pub model: String,
    pub prompt_hash: String,
    /// Response body, verbatim.
    pub response: String,
    /// Tokens this answer saves each time it is served.
    pub tokens_saved: i64,
    /// Lifetime in seconds; negative values store an already-expired row
    /// (that is how the expiry path is tested without waiting).
    pub ttl_seconds: i64,
}

const COLUMNS: &str = "signature, model, prompt_hash, response, tokens_saved, hit_count, \
     created_at, expires_at";

/// Live rows only. An expired row reads as absent, which is the whole TTL
/// contract: [`put`] never has to be right about expiry for a read to be
/// correct, and a row that expired while the process was down is skipped
/// rather than resurrected.
const LIVE: &str = "datetime(expires_at) > datetime('now')";

fn ttl_modifier(seconds: i64) -> String {
    format!("{seconds:+} seconds")
}

/// The `id` a signature stores under.
///
/// A pure function of the signature, so the UNIQUE signature already implies
/// a UNIQUE id and no random generator is needed: the primary key can never
/// collide with a row another key wrote.
pub fn row_id(signature: &str) -> String {
    hex::encode(Sha256::digest(signature.as_bytes()))
}

/// Store (or replace) one answer.
///
/// `ON CONFLICT(signature) DO UPDATE` rather than delete-then-insert: a
/// repeated request must not lose the hit count of the entry it replaces,
/// and a crash between the two statements must not leave a hole.
pub fn put(conn: &Connection, write: &CacheWrite) -> Result<()> {
    conn.execute(
        "INSERT INTO semantic_cache (
            id, signature, model, prompt_hash, response, tokens_saved, hit_count,
            created_at, expires_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, 0, datetime('now'), datetime('now', ?7))
         ON CONFLICT(signature) DO UPDATE SET
            model = excluded.model,
            prompt_hash = excluded.prompt_hash,
            response = excluded.response,
            tokens_saved = excluded.tokens_saved,
            created_at = excluded.created_at,
            expires_at = excluded.expires_at",
        params![
            row_id(&write.signature),
            &write.signature,
            &write.model,
            &write.prompt_hash,
            &write.response,
            write.tokens_saved,
            ttl_modifier(write.ttl_seconds),
        ],
    )?;
    Ok(())
}

/// Fetch a live answer by its exact key. `None` for a miss *and* for an
/// expired row.
pub fn get(conn: &Connection, signature: &str) -> Result<Option<CacheRow>> {
    Ok(conn
        .query_row(
            &format!("SELECT {COLUMNS} FROM semantic_cache WHERE signature = ?1 AND {LIVE}"),
            params![signature],
            |row| {
                Ok(CacheRow {
                    signature: row.get("signature")?,
                    model: row.get("model")?,
                    prompt_hash: row.get("prompt_hash")?,
                    response: row.get("response")?,
                    tokens_saved: row.get("tokens_saved")?,
                    hit_count: row.get("hit_count")?,
                    created_at: row.get("created_at")?,
                    expires_at: row.get("expires_at")?,
                })
            },
        )
        .optional()?)
}

/// Book one served-from-cache call against the row it was served from.
pub fn record_hit(conn: &Connection, signature: &str, tokens_saved: i64) -> Result<()> {
    conn.execute(
        "UPDATE semantic_cache SET hit_count = hit_count + 1, tokens_saved = tokens_saved + ?2 \
         WHERE signature = ?1",
        params![signature, tokens_saved],
    )?;
    Ok(())
}

/// Delete every row past its TTL. Returns how many went.
///
/// Lazy by construction: the gateway calls this on a write, so an entry that
/// is never written again is never swept — which is exactly the row nobody
/// reads either.
pub fn sweep_expired(conn: &Connection) -> Result<usize> {
    Ok(conn.execute(
        "DELETE FROM semantic_cache WHERE datetime(expires_at) <= datetime('now')",
        [],
    )?)
}

/// Rows in the table, expired ones included. Diagnostics and tests only.
pub fn count(conn: &Connection) -> Result<i64> {
    Ok(conn.query_row("SELECT COUNT(*) FROM semantic_cache", [], |row| row.get(0))?)
}

/// A stored query vector.
#[derive(Debug, Clone, PartialEq)]
pub struct VectorRow {
    /// `v:<namespace>:<path tag>:<prompt hash>`.
    pub signature: String,
    pub vector: Vec<f32>,
}

/// Which stored vectors a semantic lookup may consider.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VectorScope<'a> {
    /// Inclusive lower bound: `v:<namespace>:<path tag>:`.
    pub prefix: &'a str,
    /// Only vectors stored for this model — a cheap model must never answer
    /// for an expensive one.
    pub model: &'a str,
}

/// Exclusive upper bound of a `prefix` range.
///
/// `prefix` always ends in `':'` (0x3a); `';'` (0x3b) is the next string in
/// binary order, so every signature that starts with the prefix and nothing
/// else falls inside `[prefix, upper)`.
fn upper_bound(prefix: &str) -> String {
    let mut upper = prefix.to_string();
    if let Some(last) = upper.pop() {
        upper.push(char::from(last as u8 + 1));
    }
    upper
}

/// Store a query vector beside the answer it belongs to.
///
/// `response` holds the JSON array — the schema has no vector column and no
/// migration may be added, so the vector rides in the one column that is free
/// on a `v:` row.
pub fn put_vector(
    conn: &Connection,
    signature: &str,
    model: &str,
    prompt_hash: &str,
    vector: &[f32],
    ttl_seconds: i64,
) -> Result<()> {
    let encoded = serde_json::to_string(vector).unwrap_or_else(|_| "[]".to_string());
    put(
        conn,
        &CacheWrite {
            signature: signature.to_string(),
            model: model.to_string(),
            prompt_hash: prompt_hash.to_string(),
            response: encoded,
            tokens_saved: 0,
            ttl_seconds,
        },
    )
}

/// Every live vector in one namespace, path and model.
///
/// The range over `signature` uses the UNIQUE index, so this is not the table
/// scan a `LIKE 'v:%'` would be. Expired rows and rows whose `response` is not
/// a vector (a hand-edited or legacy row) are skipped rather than failing the
/// lookup: one bad row must not take the cache down.
pub fn vectors_in(conn: &Connection, scope: &VectorScope<'_>) -> Result<Vec<VectorRow>> {
    let mut statement = conn.prepare(&format!(
        "SELECT signature, response FROM semantic_cache \
         WHERE signature >= ?1 AND signature < ?2 AND model = ?3 AND {LIVE}"
    ))?;
    let rows = statement.query_map(
        params![scope.prefix, upper_bound(scope.prefix), scope.model],
        |row| {
            Ok((
                row.get::<_, String>("signature")?,
                row.get::<_, String>("response")?,
            ))
        },
    )?;
    let mut vectors = Vec::new();
    for row in rows {
        let (signature, encoded) = row?;
        if let Ok(vector) = serde_json::from_str::<Vec<f32>>(&encoded)
            && !vector.is_empty()
        {
            vectors.push(VectorRow { signature, vector });
        }
    }
    Ok(vectors)
}

/// The closest stored vector at or above `threshold`.
///
/// Plain scan: SQLite has no cosine, and the row set is one namespace's
/// entries. Returns the cosine alongside via [`cosine`] if the caller wants
/// to report it.
pub fn nearest(rows: &[VectorRow], query: &[f32], threshold: f32) -> Option<VectorRow> {
    rows.iter()
        .filter_map(|row| {
            let similarity = cosine(&row.vector, query);
            (similarity >= threshold).then(|| (similarity, row.clone()))
        })
        .max_by(|left, right| left.0.total_cmp(&right.0))
        .map(|(_, row)| row)
}

/// Cosine similarity, `0.0` for anything that cannot be compared.
///
/// Widths must agree: a stored vector from a different embedding model is not
/// a near neighbour, it is a dimension error, and returning `0.0` keeps it
/// below every threshold.
pub fn cosine(left: &[f32], right: &[f32]) -> f32 {
    if left.is_empty() || left.len() != right.len() {
        return 0.0;
    }
    let (mut dot, mut left_norm, mut right_norm) = (0f64, 0f64, 0f64);
    for (left, right) in left.iter().zip(right) {
        let (left, right) = (f64::from(*left), f64::from(*right));
        dot += left * right;
        left_norm += left * left;
        right_norm += right * right;
    }
    if left_norm == 0.0 || right_norm == 0.0 {
        return 0.0;
    }
    (dot / (left_norm.sqrt() * right_norm.sqrt())) as f32
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Db;

    fn db() -> Db {
        let db = Db::open_in_memory().unwrap();
        db.migrate().unwrap();
        db
    }

    fn write(signature: &str, response: &str) -> CacheWrite {
        CacheWrite {
            signature: signature.to_string(),
            model: "gpt-test".to_string(),
            prompt_hash: "abc123".to_string(),
            response: response.to_string(),
            tokens_saved: 7,
            ttl_seconds: 3600,
        }
    }

    #[test]
    fn exact_roundtrip_returns_the_stored_body() {
        let db = db();
        let guard = db.connection();
        put(&guard, &write("x:ns:a", "answer-a")).unwrap();
        let hit = get(&guard, "x:ns:a").unwrap().expect("row");
        assert_eq!(hit.response, "answer-a");
        assert_eq!(hit.model, "gpt-test");
        assert_eq!(hit.tokens_saved, 7);
        assert_eq!(hit.hit_count, 0);
        assert_eq!(count(&guard).unwrap(), 1);
    }

    #[test]
    fn an_expired_row_is_never_returned() {
        let db = db();
        let guard = db.connection();
        let mut expired = write("x:ns:a", "answer-a");
        expired.ttl_seconds = -60;
        put(&guard, &expired).unwrap();
        assert!(get(&guard, "x:ns:a").unwrap().is_none());
    }

    #[test]
    fn storing_the_same_signature_twice_replaces_the_body() {
        let db = db();
        let guard = db.connection();
        put(&guard, &write("x:ns:a", "first")).unwrap();
        put(&guard, &write("x:ns:a", "second")).unwrap();
        assert_eq!(get(&guard, "x:ns:a").unwrap().unwrap().response, "second");
        assert_eq!(count(&guard).unwrap(), 1);
    }

    #[test]
    fn hits_accumulate_count_and_tokens_saved() {
        let db = db();
        let guard = db.connection();
        put(&guard, &write("x:ns:a", "answer-a")).unwrap();
        record_hit(&guard, "x:ns:a", 12).unwrap();
        record_hit(&guard, "x:ns:a", 12).unwrap();
        let row = get(&guard, "x:ns:a").unwrap().unwrap();
        assert_eq!(row.hit_count, 2);
        assert_eq!(row.tokens_saved, 31);
    }

    #[test]
    fn sweep_removes_expired_rows_and_keeps_live_ones() {
        let db = db();
        let guard = db.connection();
        let mut dead = write("x:ns:dead", "gone");
        dead.ttl_seconds = -1;
        put(&guard, &dead).unwrap();
        put(&guard, &write("x:ns:live", "kept")).unwrap();
        assert_eq!(sweep_expired(&guard).unwrap(), 1);
        assert_eq!(count(&guard).unwrap(), 1);
        assert!(get(&guard, "x:ns:live").unwrap().is_some());
    }

    #[test]
    fn vectors_roundtrip_and_are_scoped_to_one_namespace() {
        let db = db();
        let guard = db.connection();
        put_vector(
            &guard,
            "v:ns1:v1:chat:1",
            "gpt-test",
            "1",
            &[1.0, 0.0],
            3600,
        )
        .unwrap();
        put_vector(
            &guard,
            "v:ns1:v1:chat:2",
            "gpt-test",
            "2",
            &[0.0, 1.0],
            3600,
        )
        .unwrap();
        put_vector(
            &guard,
            "v:ns2:v1:chat:3",
            "gpt-test",
            "3",
            &[1.0, 0.0],
            3600,
        )
        .unwrap();
        put_vector(
            &guard,
            "v:ns1:v1:claude:4",
            "gpt-test",
            "4",
            &[1.0, 0.0],
            3600,
        )
        .unwrap();
        put_vector(
            &guard,
            "v:ns1:v1:chat:5",
            "other-model",
            "5",
            &[1.0, 0.0],
            3600,
        )
        .unwrap();

        let scope = VectorScope {
            prefix: "v:ns1:v1:chat:",
            model: "gpt-test",
        };
        let rows = vectors_in(&guard, &scope).unwrap();
        assert_eq!(rows.len(), 2, "one namespace, one path, one model");
        let best = nearest(&rows, &[0.9, 0.1], 0.5).unwrap();
        assert_eq!(best.signature, "v:ns1:v1:chat:1");
        assert!(nearest(&rows, &[0.0, 1.0], 0.99).is_some());
        assert!(
            nearest(&rows, &[-1.0, 0.0], 0.5).is_none(),
            "below threshold"
        );
    }

    #[test]
    fn an_expired_vector_is_not_a_candidate() {
        let db = db();
        let guard = db.connection();
        put_vector(&guard, "v:ns1:v1:chat:1", "gpt-test", "1", &[1.0, 0.0], -5).unwrap();
        let rows = vectors_in(
            &guard,
            &VectorScope {
                prefix: "v:ns1:v1:chat:",
                model: "gpt-test",
            },
        )
        .unwrap();
        assert!(rows.is_empty());
    }

    #[test]
    fn cosine_peaks_on_the_same_direction() {
        assert!((cosine(&[1.0, 0.0], &[1.0, 0.0]) - 1.0).abs() < 1e-6);
        assert!(cosine(&[1.0, 0.0], &[0.0, 1.0]).abs() < 1e-6);
        assert!((cosine(&[1.0, 0.0], &[-1.0, 0.0]) + 1.0).abs() < 1e-6);
        assert_eq!(cosine(&[1.0, 0.0], &[1.0]), 0.0, "width mismatch");
        assert_eq!(cosine(&[], &[]), 0.0, "no vector");
    }

    #[test]
    fn row_id_is_derived_from_the_signature() {
        let db = db();
        let guard = db.connection();
        put(&guard, &write("x:ns:a", "answer-a")).unwrap();
        let id: String = guard
            .query_row(
                "SELECT id FROM semantic_cache WHERE signature = 'x:ns:a'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(id, row_id("x:ns:a"));
    }
}

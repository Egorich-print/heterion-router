//! Migration runner keyed on `_heterion_router_migrations`.
//!
//! Mirrors `src/lib/db/migrationRunner.ts` and the `core.ts` boot order: the
//! ledger is created, each pending migration runs inside its own transaction,
//! and applied versions are recorded. Later migrations are added by appending
//! to [`MIGRATIONS`]; the runner only ever runs what the ledger lacks.

use std::collections::HashSet;

use rusqlite::{Connection, OptionalExtension};

use crate::error::{DbError, Result};

/// Base schema, extracted verbatim from `src/lib/db/core.ts` `SCHEMA_SQL`
/// (17 tables + indexes).
const BASE_SCHEMA_SQL: &str = include_str!("../schema_base.sql");

/// A single versioned migration.
pub struct Migration {
    /// Ledger version, e.g. `"001"`. The JS server seeds `"001"` for the
    /// inline schema, so this runner and the JS runner agree on history.
    pub version: &'static str,
    /// Human-readable name recorded in the ledger.
    pub name: &'static str,
    /// SQL body. Must be idempotent (`IF NOT EXISTS` / `OR IGNORE`) so a
    /// database owned by the Node server can be attached safely.
    pub sql: &'static str,
}

/// Ordered migration history. Append only — never reorder or edit applied
/// entries, the ledger has no downgrade path.
///
/// `002` archives empty-model combos (`"models": []`) by flipping the
/// `active` column to 0 — the data stays, only the archive flag moves. It
/// relies on the `active` column existing before it runs, which
/// [`ensure_combos_columns`] guarantees (fresh databases get the column from
/// the base schema; databases predating ADR-015 get it added on boot).
pub const MIGRATIONS: &[Migration] = &[
    Migration {
        version: "001",
        name: "initial_schema",
        sql: BASE_SCHEMA_SQL,
    },
    Migration {
        version: "002",
        name: "archive_empty_combos",
        sql: "UPDATE combos \
              SET active = 0, updated_at = datetime('now') \
              WHERE active = 1 \
                AND json_valid(data) \
                AND COALESCE(json_array_length(data, '$.models'), 0) = 0;",
    },
];

/// Columns the combo archive feature (ADR-015) needs on `combos` that
/// postdate the 001 base schema. A fresh Rust database gets `active` from
/// the base schema itself; a database created before ADR-015 (the ledger
/// already records 001) never re-runs it, so the column is converged here.
const COMBOS_COMPAT_COLUMNS: &[(&str, &str)] = &[("active", "INTEGER NOT NULL DEFAULT 1")];

/// Add any [`COMBOS_COMPAT_COLUMNS`] the database lacks.
///
/// Skips silently when the `combos` table does not exist yet (a fresh
/// database creates it, with the column, via migration 001). Runs before the
/// pending migrations so `002`'s backfill can reference the column on
/// databases that predate ADR-015.
fn ensure_combos_columns(conn: &Connection) -> Result<()> {
    let table: Option<String> = conn
        .query_row(
            "SELECT name FROM sqlite_master WHERE type = 'table' AND name = 'combos'",
            [],
            |row| row.get(0),
        )
        .optional()
        .map_err(DbError::from)?;
    if table.is_none() {
        return Ok(());
    }
    let mut statement = conn.prepare("PRAGMA table_info(combos)")?;
    let present: HashSet<String> = statement
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<std::result::Result<_, _>>()?;
    for (column, ddl) in COMBOS_COMPAT_COLUMNS {
        if !present.contains(*column) {
            conn.execute_batch(&format!("ALTER TABLE combos ADD COLUMN {column} {ddl}"))?;
        }
    }
    Ok(())
}

fn ensure_ledger(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS _heterion_router_migrations (
            version TEXT PRIMARY KEY,
            name TEXT NOT NULL,
            applied_at TEXT NOT NULL DEFAULT (datetime('now'))
        );",
    )?;
    Ok(())
}

fn applied_versions(conn: &Connection) -> Result<HashSet<String>> {
    let mut statement = conn.prepare("SELECT version FROM _heterion_router_migrations")?;
    let versions = statement
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<std::result::Result<HashSet<_>, _>>()?;
    Ok(versions)
}

/// Columns the admin API needs on `api_keys` that postdate the 001 base
/// schema. The JS server adds them through its own migrations; a fresh Rust
/// database needs them too. All nullable or constant-defaulted, so the
/// backfill never touches existing rows and never fails where the JS server
/// already added them.
const API_KEYS_COMPAT_COLUMNS: &[(&str, &str)] = &[
    ("key_prefix", "TEXT"),
    ("revoked_at", "TEXT"),
    ("expires_at", "TEXT"),
    ("last_used_at", "TEXT"),
    ("is_active", "INTEGER NOT NULL DEFAULT 1"),
    ("is_banned", "INTEGER NOT NULL DEFAULT 0"),
    ("key_hash", "TEXT"),
    ("scopes", "TEXT"),
    ("allowed_combos", "TEXT"),
    ("stream_default_mode", "TEXT"),
    ("allowed_quotas", "TEXT"),
    ("disable_non_public_models", "INTEGER"),
    ("usage_limit_enabled", "INTEGER"),
    ("max_sessions", "INTEGER"),
    ("allow_usage_command", "INTEGER"),
    ("chaos_mode_enabled", "INTEGER"),
    ("cache_default_mode", "TEXT"),
    ("model_access_mode", "TEXT"),
    ("compression_enabled", "INTEGER"),
];

/// Add any [`API_KEYS_COMPAT_COLUMNS`] the database lacks.
///
/// Runs on every boot (not via the ledger): the JS-owned database already
/// has these columns, so there is nothing versioned to record — the PRAGMA
/// check makes the step a no-op there.
fn ensure_api_keys_columns(conn: &Connection) -> Result<()> {
    let table: Option<String> = conn
        .query_row(
            "SELECT name FROM sqlite_master WHERE type = 'table' AND name = 'api_keys'",
            [],
            |row| row.get(0),
        )
        .optional()
        .map_err(DbError::from)?;
    if table.is_none() {
        return Ok(());
    }
    let mut statement = conn.prepare("PRAGMA table_info(api_keys)")?;
    let present: HashSet<String> = statement
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<std::result::Result<_, _>>()?;
    for (column, ddl) in API_KEYS_COMPAT_COLUMNS {
        if !present.contains(*column) {
            conn.execute_batch(&format!("ALTER TABLE api_keys ADD COLUMN {column} {ddl}"))?;
        }
    }
    Ok(())
}

/// Apply every migration the ledger lacks, oldest first, then converge
/// additive columns the admin API needs.
///
/// Returns the versions applied by this call (empty when up to date).
pub fn migrate(conn: &mut Connection) -> Result<Vec<String>> {
    ensure_ledger(conn)?;
    let applied = applied_versions(conn)?;

    ensure_combos_columns(conn)?;

    let mut newly_applied = Vec::new();
    for migration in MIGRATIONS {
        if applied.contains(migration.version) {
            continue;
        }

        let transaction = conn.transaction()?;
        if let Err(source) = transaction.execute_batch(migration.sql) {
            return Err(DbError::Migration(
                migration.version.to_string(),
                source.to_string(),
            ));
        }
        transaction.execute(
            "INSERT OR IGNORE INTO _heterion_router_migrations (version, name) VALUES (?1, ?2)",
            rusqlite::params![migration.version, migration.name],
        )?;
        transaction.commit()?;
        newly_applied.push(migration.version.to_string());
    }
    ensure_api_keys_columns(conn)?;
    Ok(newly_applied)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Db;

    #[test]
    fn fresh_db_applies_001_002_and_is_idempotent() {
        let db = Db::open_in_memory().unwrap();
        let mut guard = db.connection();

        let first = migrate(&mut guard).unwrap();
        assert_eq!(first, vec!["001".to_string(), "002".to_string()]);

        let second = migrate(&mut guard).unwrap();
        assert!(second.is_empty());

        let tables: i64 = guard
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        // 17 base tables + the ledger.
        assert_eq!(tables, 18);
    }

    /// The exact schema a database created before ADR-015 has: ledger says
    /// 001 is applied and `combos` has no `active` column.
    fn open_pre_adr015_db() -> Db {
        let db = Db::open_in_memory().unwrap();
        {
            let guard = db.connection();
            guard
                .execute_batch(
                    "CREATE TABLE combos (
                     id TEXT PRIMARY KEY,
                     name TEXT NOT NULL UNIQUE,
                     data TEXT NOT NULL,
                     sort_order INTEGER NOT NULL DEFAULT 0,
                     created_at TEXT NOT NULL,
                     updated_at TEXT NOT NULL
                 );
                 CREATE TABLE _heterion_router_migrations (
                     version TEXT PRIMARY KEY,
                     name TEXT NOT NULL,
                     applied_at TEXT NOT NULL DEFAULT (datetime('now'))
                 );
                 INSERT INTO _heterion_router_migrations (version, name)
                     VALUES ('001', 'initial_schema');",
                )
                .unwrap();
        }
        db
    }

    #[test]
    fn pre_adr015_db_gains_active_column_without_losing_rows() {
        let db = open_pre_adr015_db();
        {
            let guard = db.connection();
            guard
                .execute(
                    "INSERT INTO combos (id, name, data, sort_order, created_at, updated_at)
                     VALUES ('keep', 'vivanta-code', '{\"name\":\"vivanta-code\",\"models\":[{\"id\":\"m1\"}]}', 0, datetime('now'), datetime('now'))",
                    [],
                )
                .unwrap();
        }

        let mut guard = db.connection();
        let applied = migrate(&mut guard).unwrap();
        assert_eq!(applied, vec!["002".to_string()]);

        let columns: HashSet<String> = {
            let mut statement = guard.prepare("PRAGMA table_info(combos)").unwrap();
            statement
                .query_map([], |row| row.get::<_, String>(1))
                .unwrap()
                .collect::<std::result::Result<_, _>>()
                .unwrap()
        };
        assert!(columns.contains("active"), "active column missing");

        // Existing rows survive the ALTER, defaulted to active.
        let (name, active): (String, i64) = guard
            .query_row(
                "SELECT name, active FROM combos WHERE id = 'keep'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(name, "vivanta-code");
        assert_eq!(active, 1);
    }

    #[test]
    fn pre_adr015_empty_combos_are_archived_not_deleted() {
        let db = open_pre_adr015_db();
        {
            let guard = db.connection();
            guard
                .execute_batch(
                    "INSERT INTO combos (id, name, data, sort_order, created_at, updated_at) VALUES
                     ('dead', 'vivanta-auto', '{\"name\":\"vivanta-auto\",\"models\":[]}', 0, datetime('now'), datetime('now')),
                     ('live', 'vivanta-code', '{\"name\":\"vivanta-code\",\"models\":[{\"id\":\"m1\"}]}', 1, datetime('now'), datetime('now')),
                     ('broken', 'broken-json', 'not json at all', 2, datetime('now'), datetime('now'));",
                )
                .unwrap();
        }

        let mut guard = db.connection();
        migrate(&mut guard).unwrap();

        let rows: Vec<(String, i64)> = {
            let mut statement = guard
                .prepare("SELECT id, active FROM combos ORDER BY id")
                .unwrap();
            statement
                .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
                .unwrap()
                .collect::<std::result::Result<_, _>>()
                .unwrap()
        };
        // Empty-model combo archived (active = 0); everything else untouched.
        assert_eq!(
            rows,
            vec![
                ("broken".to_string(), 1),
                ("dead".to_string(), 0),
                ("live".to_string(), 1),
            ]
        );
        // No deletion: all three rows still exist (checked by the count above).
    }

    #[test]
    fn preseeded_ledger_skips_base_schema() {
        let db = open_pre_adr015_db();

        // A database the Node server already migrated must not have its base
        // schema re-run — only the new 002 backfill applies on top.
        let mut guard = db.connection();
        let applied = migrate(&mut guard).unwrap();
        assert_eq!(applied, vec!["002".to_string()]);
    }

    fn api_keys_columns(conn: &rusqlite::Connection) -> HashSet<String> {
        let mut statement = conn.prepare("PRAGMA table_info(api_keys)").unwrap();
        statement
            .query_map([], |row| row.get::<_, String>(1))
            .unwrap()
            .collect::<std::result::Result<_, _>>()
            .unwrap()
    }

    #[test]
    fn fresh_db_gains_api_keys_compat_columns() {
        let db = Db::open_in_memory().unwrap();
        db.migrate().unwrap();
        let guard = db.connection();

        let columns = api_keys_columns(&guard);
        for column in [
            "key_prefix",
            "revoked_at",
            "expires_at",
            "last_used_at",
            "is_active",
            "is_banned",
            "key_hash",
            "scopes",
        ] {
            assert!(columns.contains(column), "{column} missing");
        }
        // Second boot is a no-op.
        drop(guard);
        db.migrate().unwrap();
    }

    #[test]
    fn already_extended_db_is_left_alone() {
        let db = Db::open_in_memory().unwrap();
        let mut guard = db.connection();
        migrate(&mut guard).unwrap();
        guard
            .execute(
                "INSERT INTO api_keys (id, name, key, revoked_at, created_at)
                 VALUES ('k', 'n', 'sk-x', '2026-01-01 00:00:00', datetime('now'))",
                [],
            )
            .unwrap();
        drop(guard);

        db.migrate().unwrap();
        let guard = db.connection();
        let revoked: String = guard
            .query_row(
                "SELECT revoked_at FROM api_keys WHERE id = 'k'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(revoked, "2026-01-01 00:00:00");
    }
}

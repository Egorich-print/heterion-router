//! Per-key spend budgets (roadmap M3.1).
//!
//! Storage for the inherited `domain_budgets` and `domain_cost_history`
//! tables. Those tables have been in `schema_base.sql` since the Node port
//! with no Rust reader or writer; this module is the first.
//!
//! ## One row per key
//!
//! `domain_budgets` declares `api_key_id TEXT PRIMARY KEY`
//! (`schema_base.sql:201`), so "one budget row per key" is enforced by the
//! schema, not by this module: every write is an `INSERT ... ON CONFLICT
//! (api_key_id) DO UPDATE`. There is no migration to add, and no code path
//! that can leave two rows for one key.
//!
//! (The roadmap briefing said the table had no primary key. It does. Nothing
//! here depends on that being true either way: `ON CONFLICT` is keyed on the
//! column, which is exactly the constraint the table already carries.)

use rusqlite::{Connection, OptionalExtension, params};

use crate::Result;

/// `key_value` namespace the RPM ceiling is stored under.
///
/// The RPM ceiling has no column anywhere in the schema. Rather than add one
/// (a migration) it lives in `key_value`, the project's existing generic
/// settings store — the same table the response store and the dashboard
/// settings already use. `DELETE /api/budgets/{id}` clears the key with it.
pub const RPM_NAMESPACE: &str = "budgets";

/// `key_value` key holding the per-key requests-per-minute ceiling.
pub const RPM_KEY: &str = "rpm";

/// Default warning fraction, matching the column default in the schema.
pub const DEFAULT_WARNING_THRESHOLD: f64 = 0.8;

/// One key's configured budget.
///
/// Every limit is normalised on write and on read, so a caller never has to
/// ask whether a `0` means "blocked" or "unset" — see [`normalize_limit`].
#[derive(Debug, Clone, Default, PartialEq)]
pub struct BudgetRow {
    pub api_key_id: String,
    pub daily_limit_usd: f64,
    pub weekly_limit_usd: f64,
    pub monthly_limit_usd: f64,
    pub warning_threshold: f64,
    pub reset_interval: String,
    pub reset_time: String,
    pub budget_reset_at: Option<i64>,
    pub last_budget_reset_at: Option<i64>,
    pub warning_emitted_at: Option<i64>,
    pub warning_period_start: Option<i64>,
}

impl BudgetRow {
    /// A budget with the schema defaults and no limits at all: the state a key
    /// is in until someone configures it, which means unlimited.
    pub fn for_key(api_key_id: impl Into<String>) -> Self {
        Self {
            api_key_id: api_key_id.into(),
            warning_threshold: DEFAULT_WARNING_THRESHOLD,
            reset_interval: "daily".to_string(),
            reset_time: "00:00".to_string(),
            ..Default::default()
        }
    }

    /// The limit for `period`, where the period is `daily`, `weekly` or
    /// `monthly`. `0.0` means unlimited for that period.
    pub fn limit_for(&self, period: Period) -> f64 {
        normalize_limit(match period {
            Period::Daily => self.daily_limit_usd,
            Period::Weekly => self.weekly_limit_usd,
            Period::Monthly => self.monthly_limit_usd,
        })
    }

    /// The normalised warning fraction, always in `(0, 1]`.
    pub fn warning_fraction(&self) -> f64 {
        normalize_threshold(self.warning_threshold)
    }
}

/// The three spend windows a budget is expressed over.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Period {
    Daily,
    Weekly,
    Monthly,
}

impl Period {
    /// Stable name used in log lines and `code` fields.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Daily => "daily",
            Self::Weekly => "weekly",
            Self::Monthly => "monthly",
        }
    }
}

/// What a limit of `0` (or anything unusable) means, in one place.
///
/// **A limit that is zero, negative, NaN or infinite means "no limit for that
/// period"** — not "blocked", and not "you may spend nothing".
///
/// The reasoning, because "0 means blocked" is the tempting reading and it is
/// the wrong one:
/// * `weekly_limit_usd` and `monthly_limit_usd` both carry
///   `DEFAULT 0` in the schema. A row that configures only a daily cap
///   therefore *reads back* a `0` for weekly and monthly, and a `0` that
///   blocked would make "set a daily cap" mean "block every request".
/// * SQLite has no way to spell "no row", so `0` is the only available
///   encoding of "this period is not limited" that survives a round trip.
/// * A real zero-dollar cap is not a useful feature: any spend at all exceeds
///   it, which is a kill switch, and a kill switch deserves to be spelled
///   `no_log`-style or as a revoked key, not as an accidental arithmetic
///   result of a partially filled row.
///
/// The cost of the choice: an operator who genuinely wants "this key may
/// never spend again" cannot express it with `0`; they revoke the key. That
/// is one line in the admin API and already exists.
pub fn normalize_limit(value: f64) -> f64 {
    if value.is_finite() && value > 0.0 {
        value
    } else {
        0.0
    }
}

/// The warning fraction, clamped into `(0, 1]`.
///
/// A `NaN`, an infinity or a value outside the range collapses to the schema
/// default rather than to `0.0`, because a `0.0` threshold would warn on the
/// very first request of every period.
pub fn normalize_threshold(value: f64) -> f64 {
    if !value.is_finite() {
        return DEFAULT_WARNING_THRESHOLD;
    }
    value.clamp(f64::MIN_POSITIVE, 1.0)
}

/// Read one key's budget, or `None` when the key has never been configured.
pub fn get_budget(conn: &Connection, api_key_id: &str) -> Result<Option<BudgetRow>> {
    conn.query_row(
        "SELECT api_key_id, daily_limit_usd, weekly_limit_usd, monthly_limit_usd,
                warning_threshold, reset_interval, reset_time, budget_reset_at,
                last_budget_reset_at, warning_emitted_at, warning_period_start
         FROM domain_budgets WHERE api_key_id = ?1",
        params![api_key_id],
        map_budget,
    )
    .optional()
    .map_err(Into::into)
}

fn map_budget(row: &rusqlite::Row<'_>) -> rusqlite::Result<BudgetRow> {
    Ok(BudgetRow {
        api_key_id: row.get("api_key_id")?,
        daily_limit_usd: normalize_limit(row.get("daily_limit_usd")?),
        weekly_limit_usd: normalize_limit(row.get("weekly_limit_usd")?),
        monthly_limit_usd: normalize_limit(row.get("monthly_limit_usd")?),
        warning_threshold: normalize_threshold(row.get("warning_threshold")?),
        reset_interval: row.get("reset_interval")?,
        reset_time: row.get("reset_time")?,
        budget_reset_at: row.get("budget_reset_at")?,
        last_budget_reset_at: row.get("last_budget_reset_at")?,
        warning_emitted_at: row.get("warning_emitted_at")?,
        warning_period_start: row.get("warning_period_start")?,
    })
}

/// Every configured budget, in key order so a listing is stable.
pub fn list_budgets(conn: &Connection) -> Result<Vec<BudgetRow>> {
    let mut statement = conn.prepare(
        "SELECT api_key_id, daily_limit_usd, weekly_limit_usd, monthly_limit_usd,
                warning_threshold, reset_interval, reset_time, budget_reset_at,
                last_budget_reset_at, warning_emitted_at, warning_period_start
         FROM domain_budgets ORDER BY api_key_id ASC",
    )?;
    let rows = statement.query_map([], map_budget)?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(Into::into)
}

/// Insert or replace one key's budget.
///
/// Idempotent on `api_key_id`, so this is also the "configure" operation:
/// calling it twice leaves exactly one row, not two.
pub fn upsert_budget(conn: &Connection, row: &BudgetRow) -> Result<()> {
    conn.execute(
        "INSERT INTO domain_budgets (
             api_key_id, daily_limit_usd, weekly_limit_usd, monthly_limit_usd,
             warning_threshold, reset_interval, reset_time, budget_reset_at,
             last_budget_reset_at, warning_emitted_at, warning_period_start
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
         ON CONFLICT(api_key_id) DO UPDATE SET
             daily_limit_usd = excluded.daily_limit_usd,
             weekly_limit_usd = excluded.weekly_limit_usd,
             monthly_limit_usd = excluded.monthly_limit_usd,
             warning_threshold = excluded.warning_threshold,
             reset_interval = excluded.reset_interval,
             reset_time = excluded.reset_time,
             budget_reset_at = excluded.budget_reset_at,
             last_budget_reset_at = excluded.last_budget_reset_at,
             warning_emitted_at = excluded.warning_emitted_at,
             warning_period_start = excluded.warning_period_start",
        params![
            row.api_key_id,
            normalize_limit(row.daily_limit_usd),
            normalize_limit(row.weekly_limit_usd),
            normalize_limit(row.monthly_limit_usd),
            normalize_threshold(row.warning_threshold),
            row.reset_interval,
            row.reset_time,
            row.budget_reset_at,
            row.last_budget_reset_at,
            row.warning_emitted_at,
            row.warning_period_start,
        ],
    )?;
    Ok(())
}

/// Delete one key's budget. Returns whether a row was there to delete.
///
/// Absence is the "unlimited" state, so this is the whole of
/// `DELETE /api/budgets/{id}`.
pub fn delete_budget(conn: &Connection, api_key_id: &str) -> Result<bool> {
    let removed = conn.execute(
        "DELETE FROM domain_budgets WHERE api_key_id = ?1",
        params![api_key_id],
    )?;
    Ok(removed > 0)
}

/// Append one cost to a key's spend ledger. Returns the new row id.
///
/// This is an *append-only ledger*, not a counter: the period sums read it
/// back with [`spend_since`]. Costs are never updated in place, so a
/// correction is a new row and the history stays auditable.
pub fn record_cost(conn: &Connection, api_key_id: &str, cost: f64, timestamp: i64) -> Result<i64> {
    // A non-finite cost would poison every later SUM for this key (NaN
    // propagates through SQLite's `SUM`), so it is stored as 0 rather than
    // trusted. A negative cost is a real refund-shaped correction and is
    // kept, because a budget that ignores refunds over-counts.
    let cost = if cost.is_finite() { cost } else { 0.0 };
    conn.execute(
        "INSERT INTO domain_cost_history (api_key_id, cost, timestamp) VALUES (?1, ?2, ?3)",
        params![api_key_id, cost, timestamp],
    )?;
    Ok(conn.last_insert_rowid())
}

/// What a key has spent since `since` (unix seconds, inclusive).
///
/// Sums the ledger rather than keeping a running total, so a crash, a
/// concurrent writer or a hand-edited row cannot drift the number a budget
/// is judged against.
pub fn spend_since(conn: &Connection, api_key_id: &str, since: i64) -> Result<f64> {
    let total: f64 = conn.query_row(
        "SELECT COALESCE(SUM(cost), 0.0) FROM domain_cost_history
         WHERE api_key_id = ?1 AND timestamp >= ?2",
        params![api_key_id, since],
        |row| row.get(0),
    )?;
    Ok(if total.is_finite() { total } else { 0.0 })
}

/// How many cost rows a key has, and what they added up to. Test-facing.
pub fn cost_history_summary(conn: &Connection, api_key_id: &str) -> Result<(i64, f64)> {
    let row: (i64, f64) = conn.query_row(
        "SELECT COUNT(*), COALESCE(SUM(cost), 0.0) FROM domain_cost_history WHERE api_key_id = ?1",
        params![api_key_id],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    Ok(row)
}

/// Stamp the warning as emitted for `period_start`, at `now`.
///
/// Written as a plain UPDATE rather than a conditional one so the gateway
/// layer can own the "once per period" rule (and its test) instead of
/// spreading it across two crates.
pub fn mark_warning_emitted(
    conn: &Connection,
    api_key_id: &str,
    now: i64,
    period_start: i64,
) -> Result<()> {
    conn.execute(
        "UPDATE domain_budgets SET warning_emitted_at = ?2, warning_period_start = ?3
         WHERE api_key_id = ?1",
        params![api_key_id, now, period_start],
    )?;
    Ok(())
}

/// Read a key's RPM ceiling. Absent, unparseable or non-positive means off.
pub fn rpm_limit(conn: &Connection, api_key_id: &str) -> Result<Option<u32>> {
    let raw = crate::repos::kv_get(conn, RPM_NAMESPACE, api_key_id).unwrap_or(None);
    Ok(raw.and_then(|text| {
        let value: f64 = text.trim().parse().ok()?;
        (value.is_finite() && value >= 1.0).then_some(value as u32)
    }))
}

/// Set a key's RPM ceiling; `None` clears it.
pub fn set_rpm_limit(conn: &Connection, api_key_id: &str, rpm: Option<u32>) -> Result<()> {
    match rpm {
        Some(rpm) => crate::repos::kv_set(conn, RPM_NAMESPACE, api_key_id, &rpm.to_string()),
        None => crate::repos::kv_delete(conn, RPM_NAMESPACE, api_key_id),
    }
}

#[cfg(test)]
mod budget_tests {
    use super::*;
    use crate::Db;

    fn migrated() -> Db {
        let db = Db::open_in_memory().unwrap();
        db.migrate().unwrap();
        db
    }

    fn daily(row: &BudgetRow, usd: f64) -> BudgetRow {
        BudgetRow {
            daily_limit_usd: usd,
            ..row.clone()
        }
    }

    #[test]
    fn an_unconfigured_key_has_no_budget_row() {
        let db = migrated();
        let guard = db.connection();
        assert!(
            get_budget(&guard, "k1").unwrap().is_none(),
            "absence is how 'unlimited' is spelled"
        );
        assert!(list_budgets(&guard).unwrap().is_empty());
    }

    /// The "one row per key" guarantee, exercised the only way it can break:
    /// writing the same key twice.
    #[test]
    fn upsert_is_idempotent_on_api_key_id() {
        let db = migrated();
        let guard = db.connection();
        let base = BudgetRow::for_key("k1");

        upsert_budget(&guard, &daily(&base, 5.0)).unwrap();
        upsert_budget(&guard, &daily(&base, 9.0)).unwrap();

        let rows = list_budgets(&guard).unwrap();
        assert_eq!(rows.len(), 1, "two writes, one row: {rows:?}");
        assert_eq!(rows[0].daily_limit_usd, 9.0, "the last write wins");
        assert_eq!(
            get_budget(&guard, "k1").unwrap().unwrap().daily_limit_usd,
            9.0
        );
    }

    #[test]
    fn delete_budget_returns_to_unlimited() {
        let db = migrated();
        let guard = db.connection();
        upsert_budget(&guard, &daily(&BudgetRow::for_key("k1"), 5.0)).unwrap();
        assert!(delete_budget(&guard, "k1").unwrap());
        assert!(!delete_budget(&guard, "k1").unwrap(), "already gone");
        assert!(get_budget(&guard, "k1").unwrap().is_none());
    }

    /// The pinned meaning of `0`. Every unusable spelling lands on the same
    /// answer, and none of them is "blocked".
    #[test]
    fn zero_negative_and_non_finite_limits_all_mean_unlimited() {
        for value in [0.0, -1.0, -0.5, f64::NAN, f64::INFINITY] {
            assert_eq!(
                normalize_limit(value),
                0.0,
                "{value} must normalise to the unlimited marker"
            );
        }
        assert_eq!(normalize_limit(4.25), 4.25, "a real cap is untouched");
    }

    #[test]
    fn an_unusable_limit_survives_a_round_trip_as_unlimited() {
        let db = migrated();
        let guard = db.connection();
        upsert_budget(
            &guard,
            &BudgetRow {
                // NaN is the hostile case: rusqlite would store it as NULL,
                // and the column is NOT NULL.
                daily_limit_usd: f64::NAN,
                monthly_limit_usd: -3.0,
                ..BudgetRow::for_key("k1")
            },
        )
        .unwrap();
        let row = get_budget(&guard, "k1").unwrap().unwrap();
        assert_eq!(row.limit_for(Period::Daily), 0.0);
        assert_eq!(row.limit_for(Period::Monthly), 0.0);
        assert_eq!(row.limit_for(Period::Weekly), 0.0);
    }

    #[test]
    fn a_warning_threshold_outside_the_range_collapses_to_the_default() {
        assert_eq!(normalize_threshold(0.0), f64::MIN_POSITIVE);
        assert_eq!(normalize_threshold(5.0), 1.0);
        assert_eq!(normalize_threshold(f64::NAN), DEFAULT_WARNING_THRESHOLD);
        assert_eq!(normalize_threshold(0.8), 0.8);
    }

    #[test]
    fn record_cost_appends_one_row_per_call_and_sums_the_period() {
        let db = migrated();
        let guard = db.connection();
        record_cost(&guard, "k1", 0.25, 1_000).unwrap();
        record_cost(&guard, "k1", 0.75, 2_000).unwrap();
        record_cost(&guard, "k2", 9.0, 2_000).unwrap();

        let (count, total) = cost_history_summary(&guard, "k1").unwrap();
        assert_eq!((count, total), (2, 1.0));
        assert_eq!(spend_since(&guard, "k1", 0).unwrap(), 1.0);
        assert_eq!(
            spend_since(&guard, "k1", 1_500).unwrap(),
            0.75,
            "rows before the window must not count"
        );
        assert_eq!(
            spend_since(&guard, "k2", 0).unwrap(),
            9.0,
            "spend is per key"
        );
        assert_eq!(spend_since(&guard, "k3", 0).unwrap(), 0.0);
    }

    #[test]
    fn a_non_finite_cost_is_stored_as_zero_not_as_nan() {
        let db = migrated();
        let guard = db.connection();
        record_cost(&guard, "k1", f64::NAN, 1_000).unwrap();
        record_cost(&guard, "k1", f64::INFINITY, 1_000).unwrap();
        let (count, total) = cost_history_summary(&guard, "k1").unwrap();
        assert_eq!(count, 2);
        assert!(
            total.is_finite(),
            "a NaN would poison every later SUM for this key: {total}"
        );
    }

    #[test]
    fn the_warning_stamp_round_trips() {
        let db = migrated();
        let guard = db.connection();
        upsert_budget(&guard, &BudgetRow::for_key("k1")).unwrap();
        mark_warning_emitted(&guard, "k1", 1_700_000_000, 1_699_000_000).unwrap();
        let row = get_budget(&guard, "k1").unwrap().unwrap();
        assert_eq!(row.warning_emitted_at, Some(1_700_000_000));
        assert_eq!(row.warning_period_start, Some(1_699_000_000));
    }

    /// The RPM ceiling has no column, so it rides in `key_value` and costs no
    /// migration.
    #[test]
    fn the_rpm_ceiling_lives_in_key_value() {
        let db = migrated();
        let guard = db.connection();
        assert_eq!(rpm_limit(&guard, "k1").unwrap(), None);
        set_rpm_limit(&guard, "k1", Some(60)).unwrap();
        assert_eq!(rpm_limit(&guard, "k1").unwrap(), Some(60));
        set_rpm_limit(&guard, "k1", None).unwrap();
        assert_eq!(rpm_limit(&guard, "k1").unwrap(), None);
    }

    #[test]
    fn an_unusable_rpm_ceiling_reads_as_off() {
        let db = migrated();
        let guard = db.connection();
        crate::repos::kv_set(&guard, RPM_NAMESPACE, "k1", "0").unwrap();
        assert_eq!(rpm_limit(&guard, "k1").unwrap(), None);
        crate::repos::kv_set(&guard, RPM_NAMESPACE, "k1", "nonsense").unwrap();
        assert_eq!(rpm_limit(&guard, "k1").unwrap(), None);
    }
}

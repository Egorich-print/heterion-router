//! `usage_history` writes.

use rusqlite::{Connection, params};

use heterion_router_db::Result;

/// One accounted request. Mirrors the columns `saveRequestUsage` writes.
///
/// Field order follows the table: every column `usage_history` declares is
/// bindable here, so a caller that knows a value has somewhere to put it.
#[derive(Debug, Clone, Default)]
pub struct UsageRecord {
    pub provider: Option<String>,
    pub model: Option<String>,
    pub connection_id: Option<String>,
    /// Upstream account the credential belongs to, when the provider has one.
    pub account_key: Option<String>,
    /// Human label shown for `account_key` in the admin UI.
    pub account_label: Option<String>,
    /// Sort rank of `account_label` among the labels of one account.
    pub account_label_priority: u32,
    pub api_key_id: Option<String>,
    pub api_key_name: Option<String>,
    pub tokens_input: u32,
    pub tokens_output: u32,
    pub tokens_cache_read: u32,
    pub tokens_cache_creation: u32,
    pub tokens_reasoning: u32,
    /// Billing tier the upstream charged for; `None` leaves the column at the
    /// schema default `'standard'` rather than NULL, so readers that filter on
    /// it keep seeing the rows.
    pub service_tier: Option<String>,
    pub status: Option<String>,
    pub success: bool,
    pub latency_ms: u32,
    /// Time to first streamed token. Zero on non-streaming calls.
    pub ttft_ms: u32,
    pub error_code: Option<String>,
}

/// One request line in `call_logs`.
///
/// Bodies are never stored here. A captured request/response lives in the
/// artifact store and is referenced by `artifact_relpath` /
/// `artifact_size_bytes` / `artifact_sha256`; the `has_*` flags only say
/// whether that store holds one. `request_summary` and `error_summary` are
/// short human-readable strings, not payloads.
#[derive(Debug, Clone, Default)]
pub struct CallLogRecord {
    pub id: String,
    pub method: String,
    pub path: String,
    pub status: i64,
    pub model: Option<String>,
    pub requested_model: Option<String>,
    pub provider: Option<String>,
    /// Upstream account that served the call, mirroring `usage_history`.
    pub account: Option<String>,
    pub connection_id: Option<String>,
    pub duration: u32,
    pub tokens_in: u32,
    pub tokens_out: u32,
    /// Optional here (schema `DEFAULT NULL`) where `UsageRecord` counts are
    /// not: a call that never reported a cache breakdown is NULL, not 0.
    pub tokens_cache_read: Option<u32>,
    pub tokens_cache_creation: Option<u32>,
    pub tokens_reasoning: Option<u32>,
    /// Tokens saved by prompt compression, when compression ran.
    pub tokens_compressed: Option<u32>,
    /// Where the cache hit came from; `None` keeps the schema default
    /// `'upstream'`.
    pub cache_source: Option<String>,
    /// Router verb the request hit, e.g. `chat` or `responses`.
    pub request_type: Option<String>,
    /// Wire format the client spoke, e.g. `openai`.
    pub source_format: Option<String>,
    /// Wire format the backend spoke, e.g. `anthropic`.
    pub target_format: Option<String>,
    pub api_key_id: Option<String>,
    pub api_key_name: Option<String>,
    /// Combo that expanded this request.
    ///
    /// The combo that served the request, when it was served through one.
    ///
    /// The INSERT is the only writer: the gateway builds its `CallLogRecord`
    /// with this field populated and no follow-up `UPDATE` exists.
    pub combo_name: Option<String>,
    /// Step within the combo that served this request.
    pub combo_step_id: Option<String>,
    /// Ties the step rows of one combo run together.
    pub combo_execution_key: Option<String>,
    pub error_summary: Option<String>,
    /// Whether the detail payload is available for this row; `None` keeps the
    /// schema default `'none'`.
    pub detail_state: Option<String>,
    /// Relative path of the detail artifact in the artifact store. Bodies are
    /// never inlined into this row.
    pub artifact_relpath: Option<String>,
    pub artifact_size_bytes: Option<i64>,
    pub artifact_sha256: Option<String>,
    /// True when the artifact store holds the request body. The body itself
    /// must be written through the artifact mechanism, never into this table.
    pub has_request_body: bool,
    /// True when the artifact store holds the response body, same rule.
    pub has_response_body: bool,
    /// True when the artifact holds the routing/translation pipeline detail.
    pub has_pipeline_details: bool,
    /// Short human-readable line describing what was requested.
    pub request_summary: Option<String>,
    /// Ties this row to the log line or trace it came from.
    pub correlation_id: Option<String>,
}

pub fn record_call_log(conn: &Connection, record: &CallLogRecord) -> Result<()> {
    conn.execute(
        "INSERT INTO call_logs (
            id, timestamp, method, path, status, model, requested_model,
            provider, account, connection_id, duration, tokens_in, tokens_out,
            tokens_cache_read, tokens_cache_creation, tokens_reasoning,
            tokens_compressed, cache_source, request_type, source_format,
            target_format, api_key_id, api_key_name, combo_name, combo_step_id,
            combo_execution_key, error_summary, detail_state, artifact_relpath,
            artifact_size_bytes, artifact_sha256, has_request_body,
            has_response_body, has_pipeline_details, request_summary,
            correlation_id
        ) VALUES (
            ?1, datetime('now'), ?2, ?3, ?4, ?5, ?6,
            ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15,
            ?16, COALESCE(?17, 'upstream'), ?18, ?19, ?20, ?21, ?22,
            ?23, ?24, ?25, ?26, COALESCE(?27, 'none'), ?28, ?29,
            ?30, ?31, ?32, ?33, ?34, ?35
        )",
        params![
            record.id,
            record.method,
            record.path,
            record.status,
            record.model,
            record.requested_model,
            record.provider,
            record.account,
            record.connection_id,
            record.duration,
            record.tokens_in,
            record.tokens_out,
            record.tokens_cache_read,
            record.tokens_cache_creation,
            record.tokens_reasoning,
            record.tokens_compressed,
            record.cache_source,
            record.request_type,
            record.source_format,
            record.target_format,
            record.api_key_id,
            record.api_key_name,
            record.combo_name,
            record.combo_step_id,
            record.combo_execution_key,
            record.error_summary,
            record.detail_state,
            record.artifact_relpath,
            record.artifact_size_bytes,
            record.artifact_sha256,
            i64::from(record.has_request_body),
            i64::from(record.has_response_body),
            i64::from(record.has_pipeline_details),
            record.request_summary,
            record.correlation_id,
        ],
    )?;
    Ok(())
}

/// Append a usage row.
pub fn record_usage(conn: &Connection, record: &UsageRecord) -> Result<()> {
    conn.execute(
        "INSERT INTO usage_history (
            provider, model, connection_id, account_key, account_label,
            account_label_priority, api_key_id, api_key_name,
            tokens_input, tokens_output, tokens_cache_read, tokens_cache_creation,
            tokens_reasoning, service_tier, status, success, latency_ms, ttft_ms,
            error_code, timestamp
        ) VALUES (
            ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13,
            COALESCE(?14, 'standard'), ?15, ?16, ?17, ?18, ?19,
            datetime('now')
        )",
        params![
            record.provider,
            record.model,
            record.connection_id,
            record.account_key,
            record.account_label,
            record.account_label_priority,
            record.api_key_id,
            record.api_key_name,
            record.tokens_input,
            record.tokens_output,
            record.tokens_cache_read,
            record.tokens_cache_creation,
            record.tokens_reasoning,
            record.service_tier,
            record.status,
            i64::from(record.success),
            record.latency_ms,
            record.ttft_ms,
            record.error_code,
        ],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use heterion_router_db::Db;

    /// Well-known digest of the empty byte string, stored verbatim to prove
    /// `artifact_sha256` round-trips as an opaque string.
    const SHA256_OF_EMPTY: &str =
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

    #[test]
    fn usage_row_round_trip() {
        let db = Db::open_in_memory().unwrap();
        db.migrate().unwrap();
        let guard = db.connection();

        record_usage(
            &guard,
            &UsageRecord {
                provider: Some("grok-cli".to_string()),
                model: Some("grok-4.6".to_string()),
                api_key_id: Some("k1".to_string()),
                tokens_input: 100,
                tokens_output: 50,
                success: true,
                latency_ms: 1200,
                ..Default::default()
            },
        )
        .unwrap();

        let (count, tokens_in): (i64, i64) = guard
            .query_row(
                "SELECT COUNT(*), SUM(tokens_input) FROM usage_history WHERE model = 'grok-4.6'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(count, 1);
        assert_eq!(tokens_in, 100);
    }

    #[test]
    fn call_log_round_trip() {
        let db = Db::open_in_memory().unwrap();
        db.migrate().unwrap();
        let guard = db.connection();
        record_call_log(
            &guard,
            &CallLogRecord {
                id: "call-1".to_string(),
                method: "POST".to_string(),
                path: "/v1/chat/completions".to_string(),
                status: 200,
                model: Some("grok-4.6".to_string()),
                requested_model: Some("combo".to_string()),
                provider: Some("routing".to_string()),
                duration: 12,
                tokens_in: 3,
                tokens_out: 4,
                api_key_id: Some("key-1".to_string()),
                request_type: Some("chat".to_string()),
                ..Default::default()
            },
        )
        .unwrap();
        let count: i64 = guard
            .query_row(
                "SELECT COUNT(*) FROM call_logs WHERE id = 'call-1'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 1);
    }

    /// Every column the `usage_history` schema has and the writer used to skip.
    #[test]
    fn usage_row_carries_every_new_column() {
        let db = Db::open_in_memory().unwrap();
        db.migrate().unwrap();
        let guard = db.connection();

        record_usage(
            &guard,
            &UsageRecord {
                account_key: Some("acct-primary".to_string()),
                account_label: Some("Work account".to_string()),
                account_label_priority: 7,
                service_tier: Some("flex".to_string()),
                ttft_ms: 412,
                ..Default::default()
            },
        )
        .unwrap();

        guard
            .query_row(
                "SELECT account_key, account_label, account_label_priority, service_tier, ttft_ms
                 FROM usage_history",
                [],
                |row| {
                    assert_eq!(
                        row.get::<_, Option<String>>("account_key")?.as_deref(),
                        Some("acct-primary"),
                        "account_key"
                    );
                    assert_eq!(
                        row.get::<_, Option<String>>("account_label")?.as_deref(),
                        Some("Work account"),
                        "account_label"
                    );
                    assert_eq!(
                        row.get::<_, Option<i64>>("account_label_priority")?,
                        Some(7),
                        "account_label_priority"
                    );
                    assert_eq!(
                        row.get::<_, Option<String>>("service_tier")?.as_deref(),
                        Some("flex"),
                        "service_tier"
                    );
                    assert_eq!(row.get::<_, Option<i64>>("ttft_ms")?, Some(412), "ttft_ms");
                    Ok(())
                },
            )
            .unwrap();
    }

    /// Every column the `call_logs` schema has and the writer used to skip.
    #[test]
    fn call_log_row_carries_every_new_column() {
        let db = Db::open_in_memory().unwrap();
        db.migrate().unwrap();
        let guard = db.connection();

        record_call_log(
            &guard,
            &CallLogRecord {
                id: "call-new".to_string(),
                account: Some("acct-primary".to_string()),
                connection_id: Some("conn-9".to_string()),
                tokens_cache_read: Some(11),
                tokens_cache_creation: Some(22),
                tokens_reasoning: Some(33),
                tokens_compressed: Some(44),
                cache_source: Some("local".to_string()),
                source_format: Some("openai".to_string()),
                target_format: Some("anthropic".to_string()),
                combo_name: Some("fast-then-cheap".to_string()),
                combo_step_id: Some("step-2".to_string()),
                combo_execution_key: Some("exec-77".to_string()),
                detail_state: Some("stored".to_string()),
                artifact_relpath: Some("artifacts/2026/10/call-new.json".to_string()),
                artifact_size_bytes: Some(2048),
                artifact_sha256: Some(SHA256_OF_EMPTY.to_string()),
                has_request_body: true,
                has_response_body: true,
                has_pipeline_details: true,
                request_summary: Some("claude-opus-4 -> grok-4.6, 2 steps".to_string()),
                correlation_id: Some("corr-1".to_string()),
                ..Default::default()
            },
        )
        .unwrap();

        guard
            .query_row(
                "SELECT account, connection_id, tokens_cache_read, tokens_cache_creation,
                        tokens_reasoning, tokens_compressed, cache_source, source_format,
                        target_format, combo_name, combo_step_id, combo_execution_key,
                        detail_state, artifact_relpath, artifact_size_bytes, artifact_sha256,
                        has_request_body, has_response_body, has_pipeline_details,
                        request_summary, correlation_id
                 FROM call_logs WHERE id = 'call-new'",
                [],
                |row| {
                    assert_eq!(
                        row.get::<_, Option<String>>("account")?.as_deref(),
                        Some("acct-primary"),
                        "account"
                    );
                    assert_eq!(
                        row.get::<_, Option<String>>("connection_id")?.as_deref(),
                        Some("conn-9"),
                        "connection_id"
                    );
                    assert_eq!(
                        row.get::<_, Option<i64>>("tokens_cache_read")?,
                        Some(11),
                        "tokens_cache_read"
                    );
                    assert_eq!(
                        row.get::<_, Option<i64>>("tokens_cache_creation")?,
                        Some(22),
                        "tokens_cache_creation"
                    );
                    assert_eq!(
                        row.get::<_, Option<i64>>("tokens_reasoning")?,
                        Some(33),
                        "tokens_reasoning"
                    );
                    assert_eq!(
                        row.get::<_, Option<i64>>("tokens_compressed")?,
                        Some(44),
                        "tokens_compressed"
                    );
                    assert_eq!(
                        row.get::<_, Option<String>>("cache_source")?.as_deref(),
                        Some("local"),
                        "cache_source"
                    );
                    assert_eq!(
                        row.get::<_, Option<String>>("source_format")?.as_deref(),
                        Some("openai"),
                        "source_format"
                    );
                    assert_eq!(
                        row.get::<_, Option<String>>("target_format")?.as_deref(),
                        Some("anthropic"),
                        "target_format"
                    );
                    assert_eq!(
                        row.get::<_, Option<String>>("combo_name")?.as_deref(),
                        Some("fast-then-cheap"),
                        "combo_name"
                    );
                    assert_eq!(
                        row.get::<_, Option<String>>("combo_step_id")?.as_deref(),
                        Some("step-2"),
                        "combo_step_id"
                    );
                    assert_eq!(
                        row.get::<_, Option<String>>("combo_execution_key")?
                            .as_deref(),
                        Some("exec-77"),
                        "combo_execution_key"
                    );
                    assert_eq!(
                        row.get::<_, Option<String>>("detail_state")?.as_deref(),
                        Some("stored"),
                        "detail_state"
                    );
                    assert_eq!(
                        row.get::<_, Option<String>>("artifact_relpath")?.as_deref(),
                        Some("artifacts/2026/10/call-new.json"),
                        "artifact_relpath"
                    );
                    assert_eq!(
                        row.get::<_, Option<i64>>("artifact_size_bytes")?,
                        Some(2048),
                        "artifact_size_bytes"
                    );
                    assert_eq!(
                        row.get::<_, Option<String>>("artifact_sha256")?.as_deref(),
                        Some(SHA256_OF_EMPTY),
                        "artifact_sha256"
                    );
                    assert_eq!(
                        row.get::<_, i64>("has_request_body")?,
                        1,
                        "has_request_body"
                    );
                    assert_eq!(
                        row.get::<_, i64>("has_response_body")?,
                        1,
                        "has_response_body"
                    );
                    assert_eq!(
                        row.get::<_, i64>("has_pipeline_details")?,
                        1,
                        "has_pipeline_details"
                    );
                    assert_eq!(
                        row.get::<_, Option<String>>("request_summary")?.as_deref(),
                        Some("claude-opus-4 -> grok-4.6, 2 steps"),
                        "request_summary"
                    );
                    assert_eq!(
                        row.get::<_, Option<String>>("correlation_id")?.as_deref(),
                        Some("corr-1"),
                        "correlation_id"
                    );
                    Ok(())
                },
            )
            .unwrap();
    }

    /// A zeroed record must still be insertable, and must not paper over the
    /// schema defaults the other readers (`WHERE service_tier = 'standard'`)
    /// rely on.
    #[test]
    fn default_records_still_write_a_valid_row() {
        let db = Db::open_in_memory().unwrap();
        db.migrate().unwrap();
        let guard = db.connection();

        record_usage(&guard, &UsageRecord::default()).unwrap();
        record_call_log(&guard, &CallLogRecord::default()).unwrap();

        guard
            .query_row(
                "SELECT account_key, account_label, account_label_priority, service_tier, ttft_ms
                 FROM usage_history",
                [],
                |row| {
                    assert_eq!(
                        row.get::<_, Option<String>>("account_key")?,
                        None,
                        "unset account_key stays NULL"
                    );
                    assert_eq!(
                        row.get::<_, Option<String>>("account_label")?,
                        None,
                        "unset account_label stays NULL"
                    );
                    assert_eq!(
                        row.get::<_, i64>("account_label_priority")?,
                        0,
                        "unset account_label_priority is zero"
                    );
                    assert_eq!(
                        row.get::<_, String>("service_tier")?,
                        "standard",
                        "unset service_tier falls back to the schema default"
                    );
                    assert_eq!(row.get::<_, i64>("ttft_ms")?, 0, "unset ttft_ms is zero");
                    Ok(())
                },
            )
            .unwrap();

        guard
            .query_row(
                "SELECT account, connection_id, tokens_cache_read, artifact_size_bytes,
                        has_request_body, cache_source, detail_state
                 FROM call_logs",
                [],
                |row| {
                    assert_eq!(
                        row.get::<_, Option<String>>("account")?,
                        None,
                        "unset account stays NULL"
                    );
                    assert_eq!(
                        row.get::<_, Option<String>>("connection_id")?,
                        None,
                        "unset connection_id stays NULL"
                    );
                    assert_eq!(
                        row.get::<_, Option<i64>>("tokens_cache_read")?,
                        None,
                        "unset tokens_cache_read stays NULL"
                    );
                    assert_eq!(
                        row.get::<_, Option<i64>>("artifact_size_bytes")?,
                        None,
                        "unset artifact_size_bytes stays NULL"
                    );
                    assert_eq!(
                        row.get::<_, i64>("has_request_body")?,
                        0,
                        "has_request_body defaults to 0"
                    );
                    assert_eq!(
                        row.get::<_, String>("cache_source")?,
                        "upstream",
                        "unset cache_source falls back to the schema default"
                    );
                    assert_eq!(
                        row.get::<_, String>("detail_state")?,
                        "none",
                        "unset detail_state falls back to the schema default"
                    );
                    Ok(())
                },
            )
            .unwrap();
    }

    /// The five per-call token counters are five independent columns in each
    /// table: writing one must never bleed into another.
    #[test]
    fn token_counts_round_trip_in_both_tables() {
        let db = Db::open_in_memory().unwrap();
        db.migrate().unwrap();
        let guard = db.connection();

        record_usage(
            &guard,
            &UsageRecord {
                tokens_input: 1,
                tokens_output: 2,
                tokens_cache_read: 3,
                tokens_cache_creation: 4,
                tokens_reasoning: 5,
                ..Default::default()
            },
        )
        .unwrap();
        record_call_log(
            &guard,
            &CallLogRecord {
                id: "call-tokens".to_string(),
                tokens_in: 10,
                tokens_out: 20,
                tokens_cache_read: Some(30),
                tokens_cache_creation: Some(40),
                tokens_reasoning: Some(50),
                tokens_compressed: Some(60),
                ..Default::default()
            },
        )
        .unwrap();

        let usage: (i64, i64, i64, i64, i64) = guard
            .query_row(
                "SELECT tokens_input, tokens_output, tokens_cache_read,
                        tokens_cache_creation, tokens_reasoning
                 FROM usage_history",
                [],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                },
            )
            .unwrap();
        assert_eq!(usage, (1, 2, 3, 4, 5));

        let call: (i64, i64, Option<i64>, Option<i64>, Option<i64>, Option<i64>) = guard
            .query_row(
                "SELECT tokens_in, tokens_out, tokens_cache_read, tokens_cache_creation,
                        tokens_reasoning, tokens_compressed
                 FROM call_logs WHERE id = 'call-tokens'",
                [],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                    ))
                },
            )
            .unwrap();
        assert_eq!(call, (10, 20, Some(30), Some(40), Some(50), Some(60)));
    }
}

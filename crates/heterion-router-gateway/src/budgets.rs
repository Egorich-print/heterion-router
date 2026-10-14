//! Per-key spend budgets and RPM ceilings (roadmap M3.1).
//!
//! One middleware in front of the `/v1/*` endpoints that can bill. For a
//! request that resolves to an API key it:
//!
//! 1. rejects with **429** when the key is over a daily/weekly/monthly USD cap
//!    or over its requests-per-minute ceiling;
//! 2. lets the request through but marks it once per period when spend has
//!    reached `warning_threshold` of a cap;
//! 3. appends the call's cost to `domain_cost_history` after a successful
//!    completion, so the next request is judged against what has actually
//!    been spent.
//!
//! ## The zero-configuration contract
//!
//! A key with no row in `domain_budgets` and no RPM setting is unlimited, and
//! an installation with no database at all, or no API keys at all, is
//! unlimited. Nothing here needs to be switched on: the middleware is a pass
//! through until a budget row exists.
//!
//! ## Where it sits, and why a rejected request is still authenticated
//!
//! `Authenticated` is an *extractor*, and extractors run after every
//! `.layer()`. So this middleware authenticates for itself — by calling the
//! very same [`Authenticated`] extractor — rather than reading the bearer
//! header a second way. A request with a bad or missing bearer therefore
//! gets the extractor's own `401`, not a `429`: the budget check never runs
//! for a key that failed to authenticate, and a caller cannot learn a key's
//! spend without holding that key.
//!
//! The consequence is one extra authenticated lookup per request (the handler
//! then runs the extractor again). It is a primary-key point query plus a
//! `last_used_at` touch, and it buys a single source of truth for "who is
//! this request" — worth it over a second parser for `Authorization`.
//!
//! ## Accounting is not billing
//!
//! `domain_cost_history` is an append-only ledger of what a key spent, read
//! back only to judge budgets. The money tables the dashboards use are
//! `usage_history` and `call_logs`, which the chat path already writes exactly
//! once per completion. This module writes one ledger row per *successful*
//! completion and never touches `usage_history`, so a call is counted once in
//! each ledger and twice in neither. `no_second_billing_row_per_completion`
//! pins that.
//!
//! The recorded cost is the `x-router-cost` value the response already carries,
//! so what the budget is judged against is byte-for-byte what the operator
//! reads in the header. An unpriced model has no header and is therefore not
//! recorded — an unknown cost stays unknown rather than becoming a zero.

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex, MutexGuard};

use axum::{
    Json,
    extract::{FromRequestParts, Path, Request, State},
    http::{HeaderName, HeaderValue, Method, StatusCode, header},
    middleware::Next,
    response::{IntoResponse, Response},
};
use heterion_router_core::GatewayError;
use heterion_router_db::Db;
use heterion_router_db::budget_repos::{self, BudgetRow, Period};
use heterion_router_usage::{CallLogRecord, record_call_log};
use rusqlite::Connection;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::AppState;
use crate::JsonBody;
use crate::auth::{AuthKey, Authenticated, ManageAuthenticated};
use crate::ids;
use crate::sanitized_error_payload;

/// The cost header the completion path already sets. Re-declared rather than
/// imported because `lib.rs`'s `HEADER_COST` is private to that module.
const COST_HEADER: &str = "x-router-cost";

/// Marker header on a request that crossed a warning threshold this period.
const WARNING_HEADER: &str = "x-router-budget-warning";

/// Paths that can bill, and therefore the only ones budgeted or rate limited.
///
/// `GET /v1/models` and `POST /v1/messages/count_tokens` are free, and
/// `GET/DELETE /v1/responses/{id}` is a store lookup. Charging for any of them
/// would make an idle dashboard trip a budget.
const BILLABLE_PATHS: [&str; 4] = [
    "/v1/chat/completions",
    "/v1/responses",
    "/v1/messages",
    "/v1/embeddings",
];

/// Whether a request is one that can be charged, and therefore the only kind
/// this middleware has anything to say about.
fn is_billable(method: &Method, path: &str) -> bool {
    method == Method::POST && BILLABLE_PATHS.contains(&path)
}

/// Seconds in a fixed RPM window.
const RPM_WINDOW_SECS: i64 = 60;

/// In-memory per-key request counters, keyed by `api_key_id`.
///
/// A fixed window, not a token bucket: no dependency, and the answer to
/// "retry_after" is exact rather than an estimate. State is per process and
/// per boot — a restart forgives the window, and a second gateway process
/// would count its own. That is an acceptable trade here because the RPM
/// ceiling is a guard against a runaway *loop*, not a quota to bill against,
/// and the USD budgets (which do decide whether a call is allowed at all) are
/// in SQLite and survive a restart.
static RPM_WINDOWS: LazyLock<Mutex<HashMap<String, Window>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Windows are dropped once this many keys are tracked, so a key created,
/// used once and never seen again cannot grow the map without bound.
const RPM_TRACKED_KEYS: usize = 1024;

struct Window {
    /// Unix seconds the current window opened.
    started: i64,
    used: u32,
}

fn windows() -> MutexGuard<'static, HashMap<String, Window>> {
    // A panic while holding the lock must not take the limiter down with it:
    // the map is plain counters and is still consistent.
    RPM_WINDOWS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// What the budget check concluded.
#[derive(Debug, Clone, PartialEq)]
enum Verdict {
    /// No cap reached: serve it.
    Allow,
    /// Over a warning fraction but under the cap: serve it, warn once.
    Warn {
        period: Period,
        spent: f64,
        limit: f64,
    },
    /// Over a cap: 429.
    Blocked {
        period: Period,
        spent: f64,
        limit: f64,
    },
}

impl Verdict {
    fn header_value(&self) -> Option<String> {
        match self {
            Self::Allow => None,
            Self::Warn {
                period,
                spent,
                limit,
            }
            | Self::Blocked {
                period,
                spent,
                limit,
            } => Some(format!("{} {spent:.4}/{limit:.4} usd", period.as_str())),
        }
    }
}

/// Budget and rate-limit enforcement for the `/v1/*` endpoints.
///
/// Register with
/// `axum::middleware::from_fn_with_state(state.clone(), budgets::enforce)`
/// after the `/v1/*` routes and before `.with_state(state)`.
pub async fn enforce(State(state): State<AppState>, request: Request, next: Next) -> Response {
    let (mut parts, body) = request.into_parts();
    let method = parts.method.clone();
    let path = parts.uri.path().to_string();

    if !is_billable(&method, &path) {
        return next.run(Request::from_parts(parts, body)).await;
    }

    // Without a database there is nowhere for a budget to live. Serve it.
    let Some(db) = state.db.clone() else {
        return next.run(Request::from_parts(parts, body)).await;
    };

    // Authenticate for ourselves, through the extractor's own logic. A key
    // that fails to authenticate must see its own 401 and not a budget
    // verdict, so the rejection is returned verbatim.
    let auth = match Authenticated::from_request_parts(&mut parts, &state).await {
        Ok(auth) => auth,
        Err(rejection) => return rejection,
    };
    // No key means auth is open (an empty `api_keys` table). There is no
    // per-key identity to budget or meter, which is the common case on a
    // fresh install: nothing to configure, nothing enforced.
    let Some(key) = auth.key else {
        return next.run(Request::from_parts(parts, body)).await;
    };
    let request = Request::from_parts(parts, body);

    let now = ids::unix_seconds();

    let rpm_retry_after = rpm_retry_after(&db, &key.id, now);
    let verdict = {
        let guard = db.connection();
        evaluate(&guard, &key.id, now)
    };

    // The guard is dropped before `next` runs: holding a `MutexGuard` across
    // an await would block every other writer on the database for the whole
    // upstream call.
    if let Some(retry_after) = rpm_retry_after {
        return refuse(
            &db,
            &key,
            &request,
            "rate_limit_exceeded",
            "request rate limit exceeded for this api key",
            Some(retry_after),
            &request_summary(&key, &path),
        );
    }
    let mut warning_marker = None;
    match verdict {
        Verdict::Blocked {
            period,
            spent,
            limit,
        } => {
            let message = format!(
                "{} spend budget exhausted ({spent:.6} of {limit:.6} usd)",
                period.as_str()
            );
            return refuse(
                &db,
                &key,
                &request,
                "budget_exceeded",
                &message,
                None,
                &request_summary(&key, &path),
            );
        }
        Verdict::Warn {
            period,
            spent,
            limit,
        } => {
            // Only the request that actually emits the warning carries the
            // marker; a later one in the same period is an ordinary call.
            if warn_once(&db, &key.id, period, spent, limit, now) {
                warning_marker = verdict.header_value();
            }
        }
        Verdict::Allow => {}
    }

    let mut response = next.run(request).await;

    // The warning rides back on the response so the caller and an operator
    // reading headers can see that this call was the one that crossed.
    if let Some(marker) = warning_marker
        && let Ok(value) = HeaderValue::from_str(&marker)
    {
        response
            .headers_mut()
            .insert(HeaderName::from_static(WARNING_HEADER), value);
    }

    // Only a served completion is money: a 4xx/5xx carries no cost header and
    // nothing upstream was bought.
    if response.status().is_success()
        && let Some(cost) = response
            .headers()
            .get(COST_HEADER)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.parse::<f64>().ok())
    {
        let guard = db.connection();
        if let Err(error) = budget_repos::record_cost(&guard, &key.id, cost, now) {
            tracing::warn!("cost history append failed: {error}");
        }
    }
    response
}

/// Reject with the project's error envelope and record why.
///
/// The envelope skeleton comes from [`crate::sanitized_error_payload`], the
/// one function every client-facing error goes through, so nothing from an
/// upstream body can reach the wire here. The message is then replaced with a
/// static string this module owns: the sanitizer rewrites every error into a
/// `Display` prefix (`"upstream error: …"`, `"invalid request: …"`), which is
/// not what a budget refusal should read like.
fn refuse(
    db: &Db,
    key: &AuthKey,
    request: &Request,
    code: &str,
    message: &str,
    retry_after: Option<i64>,
    summary: &str,
) -> Response {
    record_rejection(db, key, request, code, summary);

    let mut payload = sanitized_error_payload(&GatewayError::Upstream("budget".to_string()));
    if let Some(detail) = payload.get_mut("error").and_then(Value::as_object_mut) {
        detail.insert("message".into(), json!(message));
        detail.insert("type".into(), json!("rate_limit_error"));
        detail.insert("code".into(), json!(code));
        if let Some(seconds) = retry_after {
            detail.insert("retry_after".into(), json!(seconds));
        }
    }

    let mut response = (StatusCode::TOO_MANY_REQUESTS, Json(payload)).into_response();
    if let Some(seconds) = retry_after
        && let Ok(value) = HeaderValue::from_str(&seconds.to_string())
    {
        response.headers_mut().insert(header::RETRY_AFTER, value);
    }
    response
}

/// Write the rejection to `call_logs` so an operator can see it.
///
/// The roadmap's DoD is "the reason lands in `call_logs`", and no helper was
/// missing to do it: [`record_call_log`] lives in `heterion_router_usage`, not
/// in the gateway's `lib.rs`, so this writes a real row rather than a
/// side-channel field. Everything this row asserts is what the middleware
/// already knows — no token counts, no upstream, no model — because none of it
/// ran. Skipped for a `no_log` key, like every other call log.
fn record_rejection(db: &Db, key: &AuthKey, request: &Request, code: &str, summary: &str) {
    if key.no_log {
        return;
    }
    let guard = db.connection();
    let id = ids::call_id();
    let record = CallLogRecord {
        id: id.clone(),
        method: request.method().as_str().to_string(),
        path: request.uri().path().to_string(),
        status: i64::from(StatusCode::TOO_MANY_REQUESTS.as_u16()),
        api_key_id: Some(key.id.clone()),
        api_key_name: Some(key.name.clone()),
        request_type: Some(request_type(request.uri().path()).to_string()),
        source_format: Some("openai".to_string()),
        error_summary: Some(code.to_string()),
        request_summary: Some(summary.to_string()),
        correlation_id: Some(id),
        ..Default::default()
    };
    if let Err(error) = record_call_log(&guard, &record) {
        tracing::warn!("rejection call log failed: {error}");
    }
}

fn request_summary(key: &AuthKey, path: &str) -> String {
    format!("{} rejected by spend budget at {}", key.name, path)
}

/// The router verb a path reports in `call_logs`, matching what the handlers
/// write for the same path.
fn request_type(path: &str) -> &'static str {
    match path {
        "/v1/responses" => "responses",
        "/v1/messages" => "messages",
        "/v1/embeddings" => "embeddings",
        _ => "chat",
    }
}

/// The RPM verdict for one key: `Some(retry_after)` when the window is full.
///
/// A window boundary rolls the counter rather than letting a key that used its
/// whole allowance stay blocked: an idle 60 seconds is a fresh window.
fn rpm_retry_after(db: &Db, api_key_id: &str, now: i64) -> Option<i64> {
    let limit = budget_repos::rpm_limit(&db.connection(), api_key_id).ok()??;
    let mut map = windows();
    if map.len() > RPM_TRACKED_KEYS {
        map.retain(|_, window| now - window.started < RPM_WINDOW_SECS);
    }
    let window = map.entry(api_key_id.to_string()).or_insert(Window {
        started: now,
        used: 0,
    });
    if now - window.started >= RPM_WINDOW_SECS {
        window.started = now;
        window.used = 0;
    }
    if window.used >= limit {
        return Some((window.started + RPM_WINDOW_SECS - now).max(1));
    }
    window.used += 1;
    None
}

/// Compare the key's spend against every configured cap.
///
/// Caps are checked daily, then weekly, then monthly, and the *first* one
/// reached decides — so a key over both its daily and its monthly cap is told
/// about the daily one, the tighter and sooner-expiring of the two.
///
/// A read failure returns [`Verdict::Allow`]. This is the one place the
/// middleware fails open, deliberately: a budget check that cannot run must
/// not take the gateway down for every request, and if SQLite is unusable the
/// completion path's own `usage_history` write is failing too. The error is
/// logged at `error` so the outage is still visible.
fn evaluate(conn: &Connection, api_key_id: &str, now: i64) -> Verdict {
    let budget = match budget_repos::get_budget(conn, api_key_id) {
        Ok(Some(budget)) => budget,
        // No row at all is the unlimited state, not an error.
        Ok(None) => return Verdict::Allow,
        Err(error) => {
            tracing::error!("budget read failed, serving unbudgeted: {error}");
            return Verdict::Allow;
        }
    };

    let mut warning = None;
    for period in [Period::Daily, Period::Weekly, Period::Monthly] {
        let limit = budget.limit_for(period);
        // Zero is how "no cap for this period" is spelled.
        if limit <= 0.0 {
            continue;
        }
        let spent = match budget_repos::spend_since(conn, api_key_id, period_start(period, now)) {
            Ok(spent) => spent,
            Err(error) => {
                tracing::error!("budget spend sum failed, serving unbudgeted: {error}");
                return Verdict::Allow;
            }
        };
        if spent >= limit {
            return Verdict::Blocked {
                period,
                spent,
                limit,
            };
        }
        if spent >= limit * budget.warning_fraction() && warning.is_none() {
            warning = Some(Verdict::Warn {
                period,
                spent,
                limit,
            });
        }
    }
    warning.unwrap_or(Verdict::Allow)
}

/// Stamp and log the warning, at most once per period. Returns whether this
/// call was the one that emitted it.
///
/// `warning_emitted_at` alone cannot say "once per period": it is just a
/// timestamp and the period rolls under it. `warning_period_start` is what
/// makes it a decision — the stamp only counts while it still names the period
/// the request fell in, so crossing midnight (or the 1st) re-arms it.
fn warn_once(db: &Db, api_key_id: &str, period: Period, spent: f64, limit: f64, now: i64) -> bool {
    let start = period_start(period, now);
    let guard = db.connection();
    let Ok(Some(budget)) = budget_repos::get_budget(&guard, api_key_id) else {
        return false;
    };
    let already_warned =
        budget.warning_emitted_at.is_some() && budget.warning_period_start == Some(start);
    if already_warned {
        return false;
    }
    if let Err(error) = budget_repos::mark_warning_emitted(&guard, api_key_id, now, start) {
        tracing::warn!("budget warning stamp failed: {error}");
        return false;
    }
    tracing::warn!(
        "api key {api_key_id} at {spent:.6} of its {limit:.6} usd {} budget ({:.0}%)",
        period.as_str(),
        budget.warning_fraction() * 100.0
    );
    true
}

/// Unix seconds at the start of `period`'s window containing `now`, in UTC.
///
/// UTC only. The operator's own timezone is not knowable here, and a budget
/// that silently rolls at a different hour than the operator expects is worse
/// than one that rolls at midnight UTC.
fn period_start(period: Period, now: i64) -> i64 {
    match period {
        // A UTC day is exactly 86_400 seconds: no leap seconds exist, so
        // integer division is the day boundary.
        Period::Daily => now.div_euclid(86_400) * 86_400,
        Period::Weekly => {
            let day = now.div_euclid(86_400);
            // Day 0 (1970-01-01) was a Thursday, so a Monday-based weekday is
            // `(day + 3).rem_euclid(7)`.
            (day - (day + 3).rem_euclid(7)) * 86_400
        }
        Period::Monthly => {
            let (year, month, _) = civil_from_days(now.div_euclid(86_400));
            days_from_civil(year, month, 1) * 86_400
        }
    }
}

/// Days since 1970-01-01 for a civil date (Howard Hinnant's algorithm).
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let year_of_era = year - era * 400;
    let day_of_year = (153 * (month + if month > 2 { -3 } else { 9 }) + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

/// Civil `(year, month, day)` for a count of days since 1970-01-01.
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let shifted = days + 719_468;
    let era = if shifted >= 0 {
        shifted
    } else {
        shifted - 146_096
    } / 146_097;
    let day_of_era = shifted - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = month_prime + if month_prime < 10 { 3 } else { -9 };
    (year + i64::from(month <= 2), month, day)
}

// ---------------------------------------------------------------------------
// Admin surface: `/api/budgets`
// ---------------------------------------------------------------------------

/// Body of `PUT /api/budgets/{api_key_id}`.
///
/// Every field is optional and absent means *leave unchanged*, so
/// `{"daily_limit_usd": 5}` sets the daily cap and does not silently clear the
/// monthly one. `DELETE` is how a budget is removed; `0` is not.
#[derive(Debug, Default, Deserialize)]
pub struct BudgetBody {
    pub daily_limit_usd: Option<f64>,
    pub weekly_limit_usd: Option<f64>,
    pub monthly_limit_usd: Option<f64>,
    pub warning_threshold: Option<f64>,
    pub rpm: Option<u32>,
}

/// `GET /api/budgets` — every configured budget with its current spend.
pub async fn list(State(state): State<AppState>, _auth: ManageAuthenticated) -> Json<Value> {
    let Some(db) = state.db.as_ref() else {
        return Json(json!({ "budgets": [] }));
    };
    let guard = db.connection();
    let Ok(rows) = budget_repos::list_budgets(&guard) else {
        return Json(json!({ "budgets": [] }));
    };
    let now = ids::unix_seconds();
    let out: Vec<Value> = rows.iter().map(|row| describe(&guard, row, now)).collect();
    Json(json!({ "budgets": out }))
}

fn describe(conn: &Connection, row: &BudgetRow, now: i64) -> Value {
    let spend = |period: Period| {
        budget_repos::spend_since(conn, &row.api_key_id, period_start(period, now)).unwrap_or(0.0)
    };
    json!({
        "api_key_id": row.api_key_id,
        "daily_limit_usd": row.daily_limit_usd,
        "weekly_limit_usd": row.weekly_limit_usd,
        "monthly_limit_usd": row.monthly_limit_usd,
        "warning_threshold": row.warning_fraction(),
        "rpm": budget_repos::rpm_limit(conn, &row.api_key_id).ok().flatten(),
        "spend": {
            "daily": spend(Period::Daily),
            "weekly": spend(Period::Weekly),
            "monthly": spend(Period::Monthly),
        },
        "warning_emitted_at": row.warning_emitted_at,
        "warning_period_start": row.warning_period_start,
    })
}

/// `PUT /api/budgets/{api_key_id}` — configure or update one key's budget.
pub async fn upsert(
    State(state): State<AppState>,
    _auth: ManageAuthenticated,
    Path(api_key_id): Path<String>,
    JsonBody(body): crate::JsonBody<BudgetBody>,
) -> Response {
    let Some(db) = state.db.as_ref() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({ "error": "database not configured" })),
        )
            .into_response();
    };
    let guard = db.connection();
    let current = budget_repos::get_budget(&guard, &api_key_id)
        .ok()
        .flatten()
        .unwrap_or_else(|| BudgetRow::for_key(api_key_id.clone()));

    let updated = BudgetRow {
        daily_limit_usd: body.daily_limit_usd.unwrap_or(current.daily_limit_usd),
        weekly_limit_usd: body.weekly_limit_usd.unwrap_or(current.weekly_limit_usd),
        monthly_limit_usd: body.monthly_limit_usd.unwrap_or(current.monthly_limit_usd),
        warning_threshold: body.warning_threshold.unwrap_or(current.warning_threshold),
        // A changed cap re-arms the warning: the old stamp described a budget
        // that no longer exists.
        warning_emitted_at: None,
        warning_period_start: None,
        ..current
    };
    if let Err(error) = budget_repos::upsert_budget(&guard, &updated) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": error.to_string() })),
        )
            .into_response();
    }
    if let Err(error) = budget_repos::set_rpm_limit(&guard, &api_key_id, body.rpm) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": error.to_string() })),
        )
            .into_response();
    }
    Json(describe(&guard, &updated, ids::unix_seconds())).into_response()
}

/// `DELETE /api/budgets/{api_key_id}` — back to unlimited.
///
/// Always succeeds: a key with no budget is already in the state this asks
/// for, so an unknown id is not a 404.
pub async fn remove(
    State(state): State<AppState>,
    _auth: ManageAuthenticated,
    Path(api_key_id): Path<String>,
) -> Response {
    let Some(db) = state.db.as_ref() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({ "error": "database not configured" })),
        )
            .into_response();
    };
    let guard = db.connection();
    let existed = budget_repos::delete_budget(&guard, &api_key_id).unwrap_or(false);
    let _ = budget_repos::set_rpm_limit(&guard, &api_key_id, None);
    Json(json!({ "api_key_id": api_key_id, "removed": existed })).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::Arc;

    use axum::Router;
    use axum::body::Body;
    use axum::body::to_bytes;
    use axum::routing::get;
    use tower::ServiceExt;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use heterion_router_db::repos::{self, ApiKeyRow};

    use crate::backend::{self, ChatBackend};
    use crate::openai::{ProviderOpenAiBackend, resolve_chat_url};

    /// Secret for every test key. Asserted-absent from 429 bodies.
    const SECRET: &str = "sk-budget-secret-CANARY-9f3a";

    fn db_with_key(id: &str) -> Arc<Db> {
        let db = Arc::new(Db::open_in_memory().unwrap());
        db.migrate().unwrap();
        let guard = db.connection();
        repos::insert_api_key(
            &guard,
            &ApiKeyRow {
                id: id.to_string(),
                name: format!("{id}-name"),
                key: SECRET.to_string(),
                allowed_models: "[\"*\"]".to_string(),
                no_log: false,
            },
        )
        .unwrap();
        drop(guard);
        db
    }

    /// The router exactly as `lib.rs` builds it. `enforce` is already part of
    /// `build_router_with_state`, so layering it a second time here would run
    /// the limiter twice per request — every request would consume two RPM
    /// slots and every completion would be billed twice.
    fn guarded(db: Arc<Db>, backend: Arc<dyn ChatBackend>) -> Router {
        let state = AppState::with_db(backend, db);
        crate::build_router_with_state(state)
    }

    fn completion(model: &str) -> String {
        json!({
            "model": model,
            "messages": [{"role": "user", "content": "hi"}],
        })
        .to_string()
    }

    async fn post(router: Router, key: &str) -> Response {
        router
            .oneshot(
                axum::http::Request::builder()
                    .method("POST")
                    .uri("/v1/chat/completions")
                    .header("content-type", "application/json")
                    .header("authorization", format!("Bearer {key}"))
                    .body(Body::from(completion("grok-4.6")))
                    .unwrap(),
            )
            .await
            .unwrap()
    }

    async fn post_path(router: Router, key: &str, uri: &str) -> Response {
        router
            .oneshot(
                axum::http::Request::builder()
                    .method("POST")
                    .uri(uri)
                    .header("content-type", "application/json")
                    .header("authorization", format!("Bearer {key}"))
                    .body(Body::from(completion("grok-4.6")))
                    .unwrap(),
            )
            .await
            .unwrap()
    }

    async fn text(response: Response) -> String {
        let collected = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        String::from_utf8_lossy(&collected).into_owned()
    }

    fn set_budget(db: &Db, id: &str, daily: f64, threshold: f64) {
        let guard = db.connection();
        budget_repos::upsert_budget(
            &guard,
            &BudgetRow {
                daily_limit_usd: daily,
                warning_threshold: threshold,
                ..BudgetRow::for_key(id)
            },
        )
        .unwrap();
    }

    fn spend(db: &Db, id: &str, cost: f64, at: i64) {
        let guard = db.connection();
        budget_repos::record_cost(&guard, id, cost, at).unwrap();
    }

    fn call_log_rows(db: &Db) -> Vec<(String, i64, Option<String>)> {
        let guard = db.connection();
        let mut statement = guard
            .prepare("SELECT path, status, error_summary FROM call_logs ORDER BY rowid")
            .unwrap();
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, Option<String>>(2)?,
                ))
            })
            .unwrap();
        rows.map(|row| row.unwrap()).collect()
    }

    fn admin_app(state: AppState) -> Router {
        Router::new()
            .route("/api/budgets", get(list))
            .route(
                "/api/budgets/{api_key_id}",
                axum::routing::put(upsert).delete(remove),
            )
            .with_state(state)
    }

    /// A key allowed to call `/api/*`, which needs both a bearer and the
    /// `manage` scope.
    async fn admin_call(app: Router, method: &str, uri: &str, body: String) -> Response {
        let mut builder = axum::http::Request::builder()
            .method(method)
            .uri(uri)
            .header("authorization", format!("Bearer {SECRET}"));
        if !body.is_empty() {
            builder = builder.header("content-type", "application/json");
        }
        app.oneshot(builder.body(Body::from(body)).unwrap())
            .await
            .unwrap()
    }

    // ---- the zero-configuration contract ---------------------------------

    /// The common case: an installation that configured nothing must be
    /// completely unaffected. No budget row, no RPM, no database at all.
    #[tokio::test]
    async fn a_key_with_no_budget_row_is_unlimited() {
        let db = db_with_key("k-unlimited");
        let router = guarded(db.clone(), Arc::new(backend::EchoBackend));
        for _ in 0..5 {
            let response = post(router.clone(), SECRET).await;
            assert_eq!(response.status(), StatusCode::OK);
        }
        // Configured nothing, so nothing is judged and nothing is invented.
        assert_eq!(
            budget_repos::list_budgets(&db.connection()).unwrap().len(),
            0
        );
    }

    /// With no `api_keys` row at all auth is open, so there is no key to
    /// budget: the request is served and no ledger row appears.
    #[tokio::test]
    async fn a_keyless_installation_is_untouched() {
        let db = Arc::new(Db::open_in_memory().unwrap());
        db.migrate().unwrap();
        let router = guarded(db.clone(), Arc::new(backend::EchoBackend));
        let response = post(router, "").await;
        assert_eq!(response.status(), StatusCode::OK);
        let (count, _) = budget_repos::cost_history_summary(&db.connection(), "").unwrap();
        assert_eq!(count, 0);
    }

    // ---- rejection, and the upstream must never see it --------------------

    /// Over the daily cap: 429, and the provider is never contacted. The
    /// `.expect(0)` is the point — a budget that still called the upstream
    /// would have spent the money it exists to prevent.
    #[tokio::test]
    async fn a_key_over_its_daily_limit_is_429_and_never_calls_the_upstream() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": "1",
                "model": "grok-4.6",
                "choices": [{"index": 0,
                             "message": {"role": "assistant", "content": "hi"},
                             "finish_reason": "stop"}],
                "usage": {"prompt_tokens": 10, "completion_tokens": 10},
            })))
            .expect(0)
            .mount(&server)
            .await;

        let db = db_with_key("k-over");
        set_budget(&db, "k-over", 1.0, 0.8);
        spend(&db, "k-over", 1.5, ids::unix_seconds());
        let before = budget_repos::cost_history_summary(&db.connection(), "k-over").unwrap();

        let backend = Arc::new(
            ProviderOpenAiBackend::new(
                "pollinations".to_string(),
                resolve_chat_url(&server.uri()),
                Vec::new(),
            )
            .unwrap(),
        );
        let router = guarded(db.clone(), backend);
        let response = post(router, SECRET).await;
        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
        server.verify().await;
        assert!(
            server.received_requests().await.unwrap().is_empty(),
            "a rejected request must never reach a provider"
        );

        let payload: Value = serde_json::from_str(&text(response).await).unwrap();
        assert_eq!(payload["error"]["code"], "budget_exceeded");
        assert!(
            payload["error"]["message"]
                .as_str()
                .unwrap()
                .contains("daily"),
            "{payload}"
        );
        assert_eq!(
            budget_repos::cost_history_summary(&db.connection(), "k-over").unwrap(),
            before,
            "a rejected request must add nothing to the ledger"
        );
    }

    /// Weekly and monthly caps are enforced on their own windows.
    #[tokio::test]
    async fn weekly_and_monthly_caps_are_enforced_too() {
        for (id, weekly, monthly) in [
            ("k-week", Some(0.000_001), None),
            ("k-month", None, Some(0.000_001)),
        ] {
            let db = db_with_key(id);
            {
                let guard = db.connection();
                budget_repos::upsert_budget(
                    &guard,
                    &BudgetRow {
                        weekly_limit_usd: weekly.unwrap_or(0.0),
                        monthly_limit_usd: monthly.unwrap_or(0.0),
                        ..BudgetRow::for_key(id)
                    },
                )
                .unwrap();
            }

            let router = guarded(db.clone(), Arc::new(backend::EchoBackend));
            assert_eq!(
                post(router.clone(), SECRET).await.status(),
                StatusCode::OK,
                "{id}: under the cap the call is served"
            );
            assert_eq!(
                post(router, SECRET).await.status(),
                StatusCode::TOO_MANY_REQUESTS,
                "{id}: the first call crossed the cap, so the second is refused"
            );
            assert_eq!(
                budget_repos::cost_history_summary(&db.connection(), id)
                    .unwrap()
                    .0,
                1,
                "{id}: only the served call was billed"
            );
        }
    }

    /// The refusal names the window that was actually exceeded: with
    /// `daily = 0` (unset) only the monthly window can be the reason.
    #[tokio::test]
    async fn the_refusal_names_the_window_that_was_actually_exceeded() {
        let db = db_with_key("k-which-period");
        {
            let guard = db.connection();
            budget_repos::upsert_budget(
                &guard,
                &BudgetRow {
                    monthly_limit_usd: 0.000_001,
                    ..BudgetRow::for_key("k-which-period")
                },
            )
            .unwrap();
        }
        spend(&db, "k-which-period", 0.5, ids::unix_seconds());
        let response = post(guarded(db, Arc::new(backend::EchoBackend)), SECRET).await;
        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
        let payload: Value = serde_json::from_str(&text(response).await).unwrap();
        assert!(
            payload["error"]["message"]
                .as_str()
                .unwrap()
                .contains("monthly"),
            "{payload}: the daily window was unset and must not be blamed"
        );
    }

    /// A key with spend well under every cap is served, and the response is
    /// byte-for-byte what it would have been without the middleware.
    #[tokio::test]
    async fn a_key_under_its_limit_passes_through_untouched() {
        let db = db_with_key("k-under");
        set_budget(&db, "k-under", 100.0, 0.8);
        spend(&db, "k-under", 0.10, ids::unix_seconds());

        let router = guarded(db.clone(), Arc::new(backend::EchoBackend));
        let response = post(router.clone(), SECRET).await;
        assert_eq!(response.status(), StatusCode::OK);
        assert!(
            response.headers().get(WARNING_HEADER).is_none(),
            "far below the warning fraction"
        );
        assert_eq!(
            response
                .headers()
                .get(COST_HEADER)
                .and_then(|value| value.to_str().ok()),
            Some("0.000052"),
            "the middleware must not touch the served response"
        );
        assert_eq!(post(router, SECRET).await.status(), StatusCode::OK);
    }

    // ---- warnings ---------------------------------------------------------

    /// The warning is informational: the call goes through, and the operator
    /// hears about it exactly once per period — then again after the period
    /// rolls over.
    #[tokio::test]
    async fn the_warning_fires_once_per_period_and_again_after_the_rollover() {
        let db = db_with_key("k-warn");
        set_budget(&db, "k-warn", 1.0, 0.8);
        // 85% of the cap: over the threshold, under the limit.
        spend(&db, "k-warn", 0.85, ids::unix_seconds());

        let router = guarded(db.clone(), Arc::new(backend::EchoBackend));

        let first = post(router.clone(), SECRET).await;
        assert_eq!(
            first.status(),
            StatusCode::OK,
            "a warning is not a rejection"
        );
        let marker = first
            .headers()
            .get(WARNING_HEADER)
            .and_then(|value| value.to_str().ok())
            .map(str::to_string)
            .expect("first crossing warns");
        assert!(marker.starts_with("daily "), "{marker}");

        for _ in 0..3 {
            let repeat = post(router.clone(), SECRET).await;
            assert_eq!(repeat.status(), StatusCode::OK);
            assert!(
                repeat.headers().get(WARNING_HEADER).is_none(),
                "the warning must not repeat inside one period"
            );
        }

        let stamped = {
            let guard = db.connection();
            let row = budget_repos::get_budget(&guard, "k-warn").unwrap().unwrap();
            row.warning_period_start
        };
        assert_eq!(
            stamped,
            Some(period_start(Period::Daily, ids::unix_seconds()))
        );

        // The period rolls: yesterday's stamp no longer describes today, so
        // the warning is armed again. This is exactly the midnight case the
        // `warning_period_start` column exists for.
        {
            let guard = db.connection();
            budget_repos::mark_warning_emitted(
                &guard,
                "k-warn",
                period_start(Period::Daily, ids::unix_seconds()) - 86_400,
                period_start(Period::Daily, ids::unix_seconds()) - 86_400,
            )
            .unwrap();
        }

        let after_rollover = post(router, SECRET).await;
        assert_eq!(after_rollover.status(), StatusCode::OK);
        assert!(
            after_rollover.headers().get(WARNING_HEADER).is_some(),
            "a new period warns again"
        );
    }

    /// A threshold of `1.0` sits exactly on the limit, so it can never warn —
    /// the request would be refused first. Pinned because it is the boundary
    /// of the warning rule, and because it must still *serve* below the cap
    /// rather than start refusing early.
    #[tokio::test]
    async fn a_threshold_of_one_can_never_warn_but_still_serves() {
        let db = db_with_key("k-warn-one");
        set_budget(&db, "k-warn-one", 1.0, 1.0);
        spend(&db, "k-warn-one", 0.99, ids::unix_seconds());
        let router = guarded(db.clone(), Arc::new(backend::EchoBackend));
        let response = post(router, SECRET).await;
        assert_eq!(response.status(), StatusCode::OK);
        assert!(response.headers().get(WARNING_HEADER).is_none());
    }

    // ---- what "0" means ---------------------------------------------------

    /// `daily_limit_usd = 0` means *unlimited*, pinned. Not "blocked", and not
    /// an accidental read of "you may spend nothing".
    #[tokio::test]
    async fn a_zero_daily_limit_means_unlimited_not_blocked() {
        let db = db_with_key("k-zero");
        set_budget(&db, "k-zero", 0.0, 0.8);
        spend(&db, "k-zero", 25.0, ids::unix_seconds());

        let router = guarded(db.clone(), Arc::new(backend::EchoBackend));
        for _ in 0..3 {
            let response = post(router.clone(), SECRET).await;
            assert_eq!(
                response.status(),
                StatusCode::OK,
                "a configured row of 0 must not block: the schema defaults \
                 weekly/monthly to 0 and a partially filled row would wedge the key"
            );
            assert!(response.headers().get(WARNING_HEADER).is_none());
        }
    }

    /// A negative cap is the same unlimited marker, not an always-blocked key.
    #[tokio::test]
    async fn a_negative_daily_limit_means_unlimited() {
        let db = db_with_key("k-negative");
        set_budget(&db, "k-negative", -10.0, 0.8);
        spend(&db, "k-negative", 25.0, ids::unix_seconds());
        let response = post(guarded(db, Arc::new(backend::EchoBackend)), SECRET).await;
        assert_eq!(response.status(), StatusCode::OK);
    }

    /// `NaN` and `±inf` must not become a silent block or a silent unlimited.
    /// SQLite stores a Rust `NaN` as NULL against a `NOT NULL` column, so the
    /// write is what has to normalize it.
    #[tokio::test]
    async fn a_non_finite_limit_lands_as_unlimited_instead_of_failing_the_write() {
        for (id, value) in [
            ("k-nan", f64::NAN),
            ("k-inf", f64::INFINITY),
            ("k-neg-inf", f64::NEG_INFINITY),
        ] {
            let db = db_with_key(id);
            set_budget(&db, id, value, 0.8);
            spend(&db, id, 25.0, ids::unix_seconds());
            let response = post(guarded(db.clone(), Arc::new(backend::EchoBackend)), SECRET).await;
            assert_eq!(
                response.status(),
                StatusCode::OK,
                "{id}: {value} must normalise, not wedge the key"
            );
        }
    }

    // ---- accounting -------------------------------------------------------

    /// One completion, one ledger row — and one `usage_history` row, so the
    /// two ledgers each count the call once and the call is billed once.
    #[tokio::test]
    async fn one_completion_books_one_cost_row_and_no_second_billing_row() {
        let db = db_with_key("k-accounting");
        set_budget(&db, "k-accounting", 100.0, 0.8);
        let router = guarded(db.clone(), Arc::new(backend::EchoBackend));

        let response = post(router.clone(), SECRET).await;
        assert_eq!(response.status(), StatusCode::OK);
        let cost: f64 = response
            .headers()
            .get(COST_HEADER)
            .and_then(|value| value.to_str().ok())
            .unwrap()
            .parse()
            .unwrap();

        let (cost_rows, total, usage, logged) = {
            let guard = db.connection();
            let (rows, total) = budget_repos::cost_history_summary(&guard, "k-accounting").unwrap();
            // The chat path's own accounting is untouched: one usage row for
            // one completion, not two, which is what "no double count" means.
            let usage: i64 = guard
                .query_row("SELECT COUNT(*) FROM usage_history", [], |row| row.get(0))
                .unwrap();
            let logged: i64 = guard
                .query_row(
                    "SELECT COUNT(*) FROM call_logs WHERE status = 200",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            (rows, total, usage, logged)
        };
        assert_eq!(cost_rows, 1, "exactly one domain_cost_history row");
        assert!(
            (total - cost).abs() < 1e-9,
            "the ledger must hold what the header reported: {total} vs {cost}"
        );
        assert_eq!(
            usage, 1,
            "the usage ledger still books the call exactly once"
        );
        assert_eq!(
            logged, 1,
            "one call log, not a second record from the middleware"
        );

        let second = post(router, SECRET).await;
        assert_eq!(second.status(), StatusCode::OK);
        assert_eq!(
            budget_repos::cost_history_summary(&db.connection(), "k-accounting")
                .unwrap()
                .0,
            2,
            "the second completion books its own row"
        );
    }

    /// An unpriced model produces no cost header, so nothing is recorded:
    /// an unknown cost stays unknown rather than becoming a zero.
    #[tokio::test]
    async fn an_unpriced_model_records_no_cost_row() {
        let db = db_with_key("k-unpriced");
        set_budget(&db, "k-unpriced", 100.0, 0.8);
        let router = guarded(db.clone(), Arc::new(backend::EchoBackend));
        let response = router
            .oneshot(
                axum::http::Request::builder()
                    .method("POST")
                    .uri("/v1/chat/completions")
                    .header("content-type", "application/json")
                    .header("authorization", format!("Bearer {SECRET}"))
                    .body(Body::from(completion("no-such-model")))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert!(response.headers().get(COST_HEADER).is_none());
        assert_eq!(
            budget_repos::cost_history_summary(&db.connection(), "k-unpriced")
                .unwrap()
                .0,
            0
        );
    }

    /// A served request leaves the warning marker off and books the row only
    /// on success — a 5xx must not be billed.
    #[tokio::test]
    async fn a_failed_completion_books_no_cost_row() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;
        let db = db_with_key("k-failed");
        set_budget(&db, "k-failed", 100.0, 0.8);
        let backend = Arc::new(
            ProviderOpenAiBackend::new(
                "pollinations".to_string(),
                resolve_chat_url(&server.uri()),
                Vec::new(),
            )
            .unwrap(),
        );
        let response = post(guarded(db.clone(), backend), SECRET).await;
        assert!(
            !response.status().is_success(),
            "a 500 upstream must not be served as a success: {:?}",
            response.status()
        );
        assert_eq!(
            budget_repos::cost_history_summary(&db.connection(), "k-failed")
                .unwrap()
                .0,
            0,
            "no money was spent, so nothing is booked"
        );
    }

    // ---- rejection reason, envelope and secrecy ---------------------------

    /// The roadmap's DoD: the reason a request was refused is visible to an
    /// operator in `call_logs`.
    #[tokio::test]
    async fn a_rejection_reason_is_recorded_in_call_logs() {
        let db = db_with_key("k-logged");
        set_budget(&db, "k-logged", 1.0, 0.8);
        spend(&db, "k-logged", 9.0, ids::unix_seconds());

        let response = post(guarded(db.clone(), Arc::new(backend::EchoBackend)), SECRET).await;
        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);

        let rows = call_log_rows(&db);
        assert_eq!(rows.len(), 1, "one row for the refused call: {rows:?}");
        assert_eq!(rows[0].0, "/v1/chat/completions");
        assert_eq!(rows[0].1, 429);
        assert_eq!(rows[0].2.as_deref(), Some("budget_exceeded"));
    }

    /// A `no_log` key writes no call log — the same rule every other log obeys.
    #[tokio::test]
    async fn a_no_log_key_records_no_rejection_row() {
        let db = Db::open_in_memory().unwrap();
        db.migrate().unwrap();
        {
            let guard = db.connection();
            repos::insert_api_key(
                &guard,
                &ApiKeyRow {
                    id: "k-quiet".to_string(),
                    name: "quiet".to_string(),
                    key: SECRET.to_string(),
                    allowed_models: "[\"*\"]".to_string(),
                    no_log: true,
                },
            )
            .unwrap();
            budget_repos::upsert_budget(
                &guard,
                &BudgetRow {
                    daily_limit_usd: 1.0,
                    ..BudgetRow::for_key("k-quiet")
                },
            )
            .unwrap();
            budget_repos::record_cost(&guard, "k-quiet", 9.0, ids::unix_seconds()).unwrap();
        }
        let db = Arc::new(db);
        let response = post(guarded(db.clone(), Arc::new(backend::EchoBackend)), SECRET).await;
        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
        assert!(call_log_rows(&db).is_empty());
    }

    /// The 429 is the project envelope, exactly, and carries neither a secret
    /// nor anything an upstream said.
    #[tokio::test]
    async fn the_429_body_is_the_project_envelope_and_leaks_nothing() {
        let db = db_with_key("k-envelope");
        set_budget(&db, "k-envelope", 1.0, 0.8);
        spend(&db, "k-envelope", 9.0, ids::unix_seconds());
        let response = post(guarded(db.clone(), Arc::new(backend::EchoBackend)), SECRET).await;
        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
        assert!(
            response.headers().get(COST_HEADER).is_none(),
            "a refused call did not cost anything"
        );

        let raw = text(response).await;
        assert!(!raw.contains(SECRET), "the bearer must never be echoed");
        assert!(!raw.contains("CANARY"), "{raw}");

        let payload: Value = serde_json::from_str(&raw).unwrap();
        let detail = payload["error"].as_object().expect("the error envelope");
        assert_eq!(detail["type"], "rate_limit_error");
        assert_eq!(detail["code"], "budget_exceeded");
        assert!(detail["message"].is_string(), "{payload}");
        assert!(
            payload.get("error").is_some() && payload.as_object().unwrap().len() == 1,
            "one top-level key, the envelope: {payload}"
        );
    }

    /// A rejected request carries no cost or provider headers.
    #[tokio::test]
    async fn a_rejection_carries_no_router_headers() {
        let db = db_with_key("k-noheaders");
        set_budget(&db, "k-noheaders", 1.0, 0.8);
        spend(&db, "k-noheaders", 9.0, ids::unix_seconds());
        let response = post(guarded(db, Arc::new(backend::EchoBackend)), SECRET).await;
        let all = format!("{:?}", response.headers());
        for header in [COST_HEADER, "x-router-provider", "x-router-model-used"] {
            assert!(
                !all.contains(header),
                "{header} must not describe a call that never ran"
            );
        }
    }

    // ---- the rate limiter -------------------------------------------------

    /// Over the RPM window: 429 with an actionable `retry_after`, in both the
    /// body and the header.
    #[tokio::test]
    async fn rpm_over_the_window_is_429_with_a_retry_after_hint() {
        let db = db_with_key("k-rpm");
        {
            let guard = db.connection();
            budget_repos::set_rpm_limit(&guard, "k-rpm", Some(2)).unwrap();
        }
        let router = guarded(db.clone(), Arc::new(backend::EchoBackend));
        for _ in 0..2 {
            assert_eq!(post(router.clone(), SECRET).await.status(), StatusCode::OK);
        }
        let refused = post(router, SECRET).await;
        assert_eq!(refused.status(), StatusCode::TOO_MANY_REQUESTS);
        let retry = refused
            .headers()
            .get(header::RETRY_AFTER)
            .and_then(|value| value.to_str().ok())
            .expect("retry-after header")
            .parse::<i64>()
            .unwrap();
        assert!(
            (1..=RPM_WINDOW_SECS).contains(&retry),
            "retry_after must say when to come back, got {retry}"
        );
        let payload: Value = serde_json::from_str(&text(refused).await).unwrap();
        assert_eq!(payload["error"]["code"], "rate_limit_exceeded");
        assert_eq!(payload["error"]["retry_after"], retry);
    }

    /// A refusal must cost nothing: no ledger row, and a logged reason.
    #[tokio::test]
    async fn an_rpm_refusal_is_logged_and_not_billed() {
        let db = db_with_key("k-rpm-logged");
        {
            let guard = db.connection();
            budget_repos::set_rpm_limit(&guard, "k-rpm-logged", Some(1)).unwrap();
        }
        let router = guarded(db.clone(), Arc::new(backend::EchoBackend));
        assert_eq!(post(router.clone(), SECRET).await.status(), StatusCode::OK);
        assert_eq!(
            post(router.clone(), SECRET).await.status(),
            StatusCode::TOO_MANY_REQUESTS
        );
        assert_eq!(
            budget_repos::cost_history_summary(&db.connection(), "k-rpm-logged")
                .unwrap()
                .0,
            1,
            "only the served call"
        );
        let rows = call_log_rows(&db);
        assert_eq!(rows.len(), 2, "the served call and the refusal: {rows:?}");
        assert_eq!(rows[1].2.as_deref(), Some("rate_limit_exceeded"));
    }

    /// The window is per key, so one noisy key cannot exhaust another's.
    #[tokio::test]
    async fn the_rpm_window_is_per_key() {
        let db = db_with_key("k-rpm-a");
        let other = {
            let db2 = Arc::new(Db::open_in_memory().unwrap());
            db2.migrate().unwrap();
            {
                let guard = db2.connection();
                repos::insert_api_key(
                    &guard,
                    &ApiKeyRow {
                        id: "k-rpm-b".to_string(),
                        name: "b".to_string(),
                        key: SECRET.to_string(),
                        allowed_models: "[\"*\"]".to_string(),
                        no_log: false,
                    },
                )
                .unwrap();
                budget_repos::set_rpm_limit(&guard, "k-rpm-b", Some(5)).unwrap();
            }
            db2
        };
        {
            let guard = db.connection();
            budget_repos::set_rpm_limit(&guard, "k-rpm-a", Some(1)).unwrap();
        }
        let a = guarded(db.clone(), Arc::new(backend::EchoBackend));
        let b = guarded(other, Arc::new(backend::EchoBackend));
        assert_eq!(post(a.clone(), SECRET).await.status(), StatusCode::OK);
        assert_eq!(
            post(a, SECRET).await.status(),
            StatusCode::TOO_MANY_REQUESTS
        );
        for _ in 0..4 {
            assert_eq!(
                post(b.clone(), SECRET).await.status(),
                StatusCode::OK,
                "the exhausted key must not spend another's window"
            );
        }
    }

    // ---- which requests are billable at all -------------------------------

    /// The billable set, pinned. A free endpoint that started being billed would
    /// quietly drain a budget on a dashboard poll.
    #[test]
    fn only_the_charging_endpoints_are_billable() {
        for path in BILLABLE_PATHS {
            assert!(
                is_billable(&Method::POST, path),
                "{path} bills and must be checked"
            );
        }
        for path in [
            "/v1/models",
            "/v1/messages/count_tokens",
            "/v1/responses/resp_123",
            "/healthz",
            "/api/usage",
        ] {
            assert!(!is_billable(&Method::POST, path), "{path} is free");
            assert!(!is_billable(&Method::GET, path), "{path} is free");
        }
        assert!(
            !is_billable(&Method::GET, "/v1/chat/completions"),
            "reading a completion path bills nothing"
        );
    }

    /// A key that is over budget must still be able to read the free
    /// endpoints, or a refusal would lock the operator out of the dashboard
    /// and out of the API surface they need to fix it.
    #[tokio::test]
    async fn free_endpoints_still_answer_a_key_that_is_over_budget() {
        let db = db_with_key("k-free");
        set_budget(&db, "k-free", 0.000_001, 0.8);
        spend(&db, "k-free", 99.0, ids::unix_seconds());
        let router = guarded(db.clone(), Arc::new(backend::EchoBackend));

        let counted = post_path(router.clone(), SECRET, "/v1/messages/count_tokens").await;
        assert_eq!(
            counted.status(),
            StatusCode::OK,
            "counting tokens is free and must stay free"
        );
        let models = router
            .clone()
            .oneshot(
                axum::http::Request::builder()
                    .method("GET")
                    .uri("/v1/models")
                    .header("authorization", format!("Bearer {SECRET}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(models.status(), StatusCode::OK);
        // And the billing path is still refused, so the exemption is narrow.
        assert_eq!(
            post(router, SECRET).await.status(),
            StatusCode::TOO_MANY_REQUESTS
        );
    }

    // ---- authentication ordering ------------------------------------------

    /// The layer runs *before* the extractor, so it authenticates for itself.
    /// A wrong key must still get the extractor's 401 and never learn anything
    /// about the budget.
    #[tokio::test]
    async fn an_unauthenticated_request_gets_401_not_429() {
        let db = db_with_key("k-auth");
        set_budget(&db, "k-auth", 1.0, 0.8);
        spend(&db, "k-auth", 9.0, ids::unix_seconds());
        let router = guarded(db.clone(), Arc::new(backend::EchoBackend));

        let wrong = post(router.clone(), "sk-not-the-key").await;
        assert_eq!(
            wrong.status(),
            StatusCode::UNAUTHORIZED,
            "a budget must not be disclosed to a caller that cannot authenticate"
        );
        let missing = router
            .clone()
            .oneshot(
                axum::http::Request::builder()
                    .method("POST")
                    .uri("/v1/chat/completions")
                    .header("content-type", "application/json")
                    .body(Body::from(completion("grok-4.6")))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(missing.status(), StatusCode::UNAUTHORIZED);
        assert!(
            call_log_rows(&db).is_empty(),
            "a 401 is the extractor's business"
        );
    }

    // ---- the admin surface ------------------------------------------------

    #[tokio::test]
    async fn the_admin_surface_lists_configures_and_clears() {
        let db = db_with_key("k-admin");
        {
            let guard = db.connection();
            guard
                .execute(
                    "UPDATE api_keys SET scopes = '[\"manage\"]' WHERE id = 'k-admin'",
                    [],
                )
                .unwrap();
        }
        let app = admin_app(AppState::with_db(
            Arc::new(backend::EchoBackend),
            db.clone(),
        ));

        let put = admin_call(
            app.clone(),
            "PUT",
            "/api/budgets/k-admin",
            json!({"daily_limit_usd": 5.0, "warning_threshold": 0.5, "rpm": 30}).to_string(),
        )
        .await;
        assert_eq!(put.status(), StatusCode::OK);

        let (row, rpm) = {
            let guard = db.connection();
            (
                budget_repos::get_budget(&guard, "k-admin")
                    .unwrap()
                    .unwrap(),
                budget_repos::rpm_limit(&guard, "k-admin").unwrap(),
            )
        };
        assert_eq!(row.daily_limit_usd, 5.0);
        assert_eq!(
            row.weekly_limit_usd, 0.0,
            "an absent field leaves the cap alone"
        );
        assert_eq!(row.warning_fraction(), 0.5);
        assert_eq!(rpm, Some(30));

        let listed = admin_call(app.clone(), "GET", "/api/budgets", String::new()).await;
        assert_eq!(listed.status(), StatusCode::OK);
        let payload: Value = serde_json::from_str(&text(listed).await).unwrap();
        assert_eq!(payload["budgets"][0]["api_key_id"], "k-admin");
        assert_eq!(payload["budgets"][0]["daily_limit_usd"], 5.0);
        assert_eq!(payload["budgets"][0]["rpm"], 30);

        let deleted = admin_call(app, "DELETE", "/api/budgets/k-admin", String::new()).await;
        assert_eq!(deleted.status(), StatusCode::OK);
        let guard = db.connection();
        assert!(
            budget_repos::get_budget(&guard, "k-admin")
                .unwrap()
                .is_none()
        );
        assert_eq!(budget_repos::rpm_limit(&guard, "k-admin").unwrap(), None);
    }

    /// Configuring a key twice leaves one row, which is what makes the
    /// documented "one budget row per key" true end to end.
    #[tokio::test]
    async fn configuring_a_key_twice_leaves_one_row() {
        let db = db_with_key("k-twice");
        {
            let guard = db.connection();
            guard
                .execute(
                    "UPDATE api_keys SET scopes = '[\"manage\"]' WHERE id = 'k-twice'",
                    [],
                )
                .unwrap();
        }
        let app = admin_app(AppState::with_db(
            Arc::new(backend::EchoBackend),
            db.clone(),
        ));
        for daily in [1.0, 2.0] {
            let response = admin_call(
                app.clone(),
                "PUT",
                "/api/budgets/k-twice",
                json!({"daily_limit_usd": daily}).to_string(),
            )
            .await;
            assert_eq!(response.status(), StatusCode::OK);
        }
        let rows = budget_repos::list_budgets(&db.connection()).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].daily_limit_usd, 2.0);
    }

    /// The admin surface is behind `manage`, like every other `/api/*` route.
    /// A key with a narrower scope is refused.
    #[tokio::test]
    async fn the_admin_surface_needs_the_manage_scope() {
        let db = db_with_key("k-noscope");
        {
            let guard = db.connection();
            // A NULL `scopes` column reads back as `["manage"]`, so the
            // narrower scope has to be written down explicitly.
            guard
                .execute(
                    "UPDATE api_keys SET scopes = '[\"chat\"]' WHERE id = 'k-noscope'",
                    [],
                )
                .unwrap();
        }
        let app = admin_app(AppState::with_db(Arc::new(backend::EchoBackend), db));
        let response = admin_call(app, "GET", "/api/budgets", String::new()).await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    // ---- period arithmetic ------------------------------------------------

    /// The warning once-per-period rule leans on this, so the boundaries are
    /// pinned rather than assumed.
    #[test]
    fn periods_start_on_real_boundaries() {
        assert_eq!(period_start(Period::Daily, 0), 0);
        assert_eq!(period_start(Period::Daily, 86_399), 0);
        assert_eq!(period_start(Period::Daily, 86_400), 86_400);
        assert_eq!(
            period_start(Period::Weekly, 0),
            -3 * 86_400,
            "day 0 was a Thursday, so its week started on the Monday before"
        );
        // Weeks are Monday-anchored, checked from the arithmetic rather than
        // from a hand-picked date, so the assertion cannot rot.
        let monday = period_start(Period::Weekly, 0);
        let monday_day = monday / 86_400;
        assert_eq!(
            (monday_day + 3).rem_euclid(7),
            0,
            "a week must start on a Monday, got day index {monday_day} ({})",
            {
                let (y, m, d) = civil_from_days(monday_day);
                format!("{y}-{m}-{d}")
            }
        );
        let some_day = days_from_civil(2026, 10, 3) * 86_400;
        let week = period_start(Period::Weekly, some_day);
        assert_eq!(
            period_start(Period::Weekly, week + 6 * 86_400),
            week,
            "the sixth day into a week is still that week"
        );
        assert_eq!(
            period_start(Period::Weekly, week + 7 * 86_400),
            week + 7 * 86_400,
            "the seventh day opens a new one"
        );
        let october = period_start(Period::Monthly, some_day);
        assert_eq!(october, days_from_civil(2026, 10, 1) * 86_400);
        assert!(october <= some_day);
        assert_eq!(
            period_start(Period::Monthly, days_from_civil(2026, 11, 15) * 86_400),
            days_from_civil(2026, 11, 1) * 86_400,
            "November is its own window"
        );
        // A short month must not borrow the next one's length.
        assert_eq!(
            period_start(Period::Monthly, days_from_civil(2026, 2, 10) * 86_400),
            days_from_civil(2026, 2, 1) * 86_400
        );
    }

    #[test]
    fn a_period_start_never_exceeds_the_moment_it_describes() {
        let now = 1_800_000_000;
        for period in [Period::Daily, Period::Weekly, Period::Monthly] {
            let start = period_start(period, now);
            assert!(start <= now, "{period:?} start {start} is after {now}");
            assert!(
                now - start < 32 * 86_400,
                "{period:?} window {start} is not a month wide"
            );
        }
    }

    #[test]
    fn the_civil_calendar_round_trips() {
        for days in [0, 1, 58, 365, 11_574, 19_723, 20_000, 30_000, 51_000] {
            let (year, month, day) = civil_from_days(days);
            assert_eq!(days_from_civil(year, month, day), days);
        }
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        // February in a leap year and a common one.
        assert_eq!(civil_from_days(days_from_civil(2024, 2, 29)), (2024, 2, 29));
        assert_eq!(civil_from_days(days_from_civil(2026, 2, 28)), (2026, 2, 28));
    }

    /// The window is a fixed 60 seconds that actually rolls, which is what makes
    /// `retry_after` an exact answer rather than a guess.
    #[tokio::test]
    async fn the_fixed_window_rolls_after_sixty_seconds() {
        let db = db_with_key("k-roll");
        {
            let guard = db.connection();
            budget_repos::set_rpm_limit(&guard, "k-roll", Some(1)).unwrap();
        }
        let now = ids::unix_seconds();
        assert_eq!(rpm_retry_after(&db, "k-roll", now), None);
        assert_eq!(rpm_retry_after(&db, "k-roll", now), Some(RPM_WINDOW_SECS));
        assert_eq!(
            rpm_retry_after(&db, "k-roll", now + RPM_WINDOW_SECS / 2),
            Some(RPM_WINDOW_SECS / 2),
            "the answer shrinks as the window drains"
        );
        assert_eq!(
            rpm_retry_after(&db, "k-roll", now + RPM_WINDOW_SECS),
            None,
            "a fresh window starts with the allowance intact"
        );
    }

    /// A key with no RPM setting is never rate limited, whatever it does.
    #[tokio::test]
    async fn no_rpm_setting_means_no_rate_limit() {
        let db = db_with_key("k-no-rpm");
        let now = ids::unix_seconds();
        for _ in 0..500 {
            assert_eq!(rpm_retry_after(&db, "k-no-rpm", now), None);
        }
    }
}

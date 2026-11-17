//! The platforms' own catalogue and quota — measured, stored, reported.
//!
//! Contracts:
//! 1. `a_measured_catalogue_is_stored_as_it_arrived` — the models a platform
//!    lists land in `provider_catalog`, under the connection they were
//!    fetched with.
//! 2. `a_quota_reading_is_read_from_the_answer` — the rate-limit headers and
//!    the text of a refusal say how much of the budget is left.
//! 3. `the_admin_api_reports_both` — `GET /api/providers/catalog` and
//!    `GET /api/providers/quota` show what was measured, and a fresh
//!    measurement replaces the old catalogue instead of adding to it.

use std::sync::Arc;

use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use heterion_router_db::{Db, repos};
use heterion_router_gateway::{
    AppState, backend::EchoBackend, build_router_with_state, provider_sync,
};
use http_body_util::BodyExt;
use tower::ServiceExt;
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{method, path},
};

/// A platform that lists two models and reports its rate-limit headers.
async fn platform() -> MockServer {
    let upstream = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            r#"{"data":[{"id":"model-b"},{"id":"model-a","context_length":131072}]}"#,
        ))
        .mount(&upstream)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("x-ratelimit-remaining-requests", "3")
                .insert_header("x-ratelimit-limit-requests", "12")
                .set_body_string(r#"{"id":"x","choices":[]}"#),
        )
        .mount(&upstream)
        .await;
    upstream
}

#[tokio::test]
async fn a_measured_catalogue_is_stored_as_it_arrived() {
    let upstream = platform().await;
    let db = Db::open_in_memory().unwrap();
    db.migrate().unwrap();
    let http = reqwest::Client::new();

    let outcome = provider_sync::sync_connection(
        &http,
        "sambanova",
        Some("openai"),
        &format!("{}/v1", upstream.uri()),
        "connection-1",
        "sk-test",
        Some("model-a"),
    )
    .await;

    assert_eq!(outcome.catalog.len(), 2, "the platform listed two models");
    assert!(
        outcome.catalog.iter().any(|model| model.id == "model-a"),
        "the catalogue must carry the platform's own ids: {:?}",
        outcome.catalog
    );

    provider_sync::store_outcome(&db, &outcome);

    let guard = db.connection();
    let rows = repos::list_provider_catalog(&guard);
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].provider, "sambanova");
    assert_eq!(rows[0].connection_id, "connection-1");
}

#[tokio::test]
async fn a_quota_reading_is_read_from_the_answer() {
    let upstream = platform().await;
    let http = reqwest::Client::new();

    let outcome = provider_sync::sync_connection(
        &http,
        "sambanova",
        Some("openai"),
        &format!("{}/v1", upstream.uri()),
        "connection-1",
        "sk-test",
        Some("model-a"),
    )
    .await;

    let reading = outcome.quota.expect("the platform answered");
    assert_eq!(reading.state.as_str(), "available");
    assert_eq!(
        reading.remaining_percentage,
        Some(25.0),
        "3 of 12 requests left"
    );
}

#[tokio::test]
async fn a_refusal_names_the_remaining_budget() {
    let upstream = MockServer::start().await;
    // Both routes answer the same way: the probe of an `openai-responses`
    // provider goes to `/responses`, of an `openai` one to `/chat/completions`.
    for route in ["/v1/chat/completions", "/v1/responses"] {
        Mock::given(method("POST"))
            .and(path(route))
            .respond_with(ResponseTemplate::new(403).set_body_string(
                r#"{"error":{"message":"pre-consume quota failed, user quota: $0.004456, need quota: $0.032528"}}"#,
            ))
            .mount(&upstream)
            .await;
    }
    let http = reqwest::Client::new();

    let outcome = provider_sync::sync_connection(
        &http,
        "agentrouter",
        Some("openai-responses"),
        &format!("{}/v1", upstream.uri()),
        "connection-2",
        "sk-test",
        Some("gpt-5.6-sol"),
    )
    .await;

    let reading = outcome.quota.expect("the platform answered");
    assert_eq!(reading.state.as_str(), "exhausted");
    let percentage = reading.remaining_percentage.expect("a percentage");
    assert!(
        (13.0..14.0).contains(&percentage),
        "expected ~13.7% left, got {percentage}"
    );
}

#[tokio::test]
async fn a_fresh_measurement_replaces_the_old_catalogue() {
    let db = Db::open_in_memory().unwrap();
    db.migrate().unwrap();
    let guard = db.connection();
    repos::replace_provider_catalog(
        &guard,
        "sambanova",
        "connection-1",
        &[("model-a".to_string(), None), ("model-b".to_string(), None)],
    )
    .unwrap();
    drop(guard);
    let guard = db.connection();
    repos::replace_provider_catalog(
        &guard,
        "sambanova",
        "connection-1",
        &[("model-c".to_string(), None)],
    )
    .unwrap();
    let rows = repos::list_provider_catalog(&guard);
    assert_eq!(
        rows.len(),
        1,
        "the old models must be gone, not kept alongside the new ones"
    );
    assert_eq!(rows[0].model_id, "model-c");
}

#[tokio::test]
async fn the_admin_api_reports_both() {
    let db = Db::open_in_memory().unwrap();
    db.migrate().unwrap();
    {
        let guard = db.connection();
        repos::replace_provider_catalog(
            &guard,
            "sambanova",
            "connection-1",
            &[("model-a".to_string(), Some(131072))],
        )
        .unwrap();
        repos::insert_quota_snapshot(
            &guard,
            &repos::QuotaSnapshotRow {
                provider: "sambanova".to_string(),
                connection_id: "connection-1".to_string(),
                window_key: "credits".to_string(),
                remaining_percentage: Some(13.7),
                is_exhausted: true,
                next_reset_at: Some("2026-11-18T00:00:00Z".to_string()),
                window_duration_ms: None,
                raw_data: Some("{\"state\":\"exhausted\"}".to_string()),
                created_at: String::new(),
            },
        )
        .unwrap();
    }

    let router = build_router_with_state(AppState::with_db(Arc::new(EchoBackend), Arc::new(db)));

    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/providers/catalog")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let payload: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(payload["providers"][0]["provider"], "sambanova");
    assert_eq!(payload["providers"][0]["count"], 1);
    assert_eq!(payload["providers"][0]["models"][0]["id"], "model-a");

    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/providers/quota")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let payload: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(payload["providers"][0]["provider"], "sambanova");
    assert_eq!(payload["providers"][0]["exhausted"], true);
    assert_eq!(payload["providers"][0]["remaining_percentage"], 13.7);
    assert_eq!(
        payload["providers"][0]["stale"], false,
        "a reading taken now is not stale"
    );
}

#[tokio::test]
async fn a_reading_from_the_js_era_is_marked_stale() {
    let db = Db::open_in_memory().unwrap();
    db.migrate().unwrap();
    {
        let guard = db.connection();
        // The JS server wrote ISO timestamps with a `T` and a `Z`.
        guard
            .execute(
                "INSERT INTO quota_snapshots (provider, connection_id, window_key, is_exhausted, created_at)
                 VALUES ('deepseek', 'old-1', 'credits_usd', 0, '2026-09-16T23:13:55.320Z')",
                [],
            )
            .unwrap();
    }

    let router = build_router_with_state(AppState::with_db(Arc::new(EchoBackend), Arc::new(db)));
    let response = router
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/providers/quota")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let payload: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(
        payload["providers"][0]["stale"], true,
        "a reading from another era must not pass as a measurement: {payload}"
    );
}

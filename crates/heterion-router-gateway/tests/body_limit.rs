//! Integration test for the gateway's request body limit.
//!
//! The unit tests inside the crate cover the same behaviour; this file proves
//! it through the **public** API only, the way an embedder would use it.

use std::sync::Arc;

use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use heterion_router_db::Db;
use heterion_router_gateway::{AppState, backend::EchoBackend, build_router_with_state};
use http_body_util::BodyExt;
use tower::ServiceExt;

/// A router over the public API with the body limit set to `limit` bytes.
fn test_router(limit: usize) -> axum::Router {
    let db = Arc::new(Db::open_in_memory().unwrap());
    db.migrate().unwrap();
    let state = AppState::with_db(Arc::new(EchoBackend), db).with_max_body_bytes(limit);
    build_router_with_state(state)
}

/// A chat-completions request whose body is roughly `size` bytes.
fn request_of_size(size: usize) -> Request<Body> {
    let mut body = String::from(r#"{"model":"grok-4.6","messages":[{"role":"user","content":""#);
    // Fill the message with a run of `x` so the body grows to `size`.
    let filler = size.saturating_sub(body.len() + 32);
    body.push_str(&"x".repeat(filler));
    body.push_str(r#""}]}"#);
    Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header("content-type", "application/json")
        .body(Body::from(body))
        .unwrap()
}

#[tokio::test]
async fn a_body_above_the_axum_default_is_accepted() {
    // axum's own default is 2 MiB, which silently capped every request at
    // roughly 500k tokens and made the 1M-context models unreachable.
    let router = test_router(32 * 1024 * 1024);
    let response = router
        .oneshot(request_of_size(3 * 1024 * 1024))
        .await
        .unwrap();
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "a 3 MiB body must be accepted when the limit is 32 MiB"
    );
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    assert!(!bytes.is_empty());
}

#[tokio::test]
async fn a_body_above_the_configured_limit_is_rejected() {
    let router = test_router(64 * 1024);
    let response = router.oneshot(request_of_size(256 * 1024)).await.unwrap();
    // axum's `Json` extractor turns an over-limit body into a 400 ("failed to
    // buffer the request body"), not a 413. Asserted here as it actually
    // behaves, so the test documents the contract instead of an aspiration.
    assert_eq!(
        response.status(),
        StatusCode::BAD_REQUEST,
        "a body over the configured limit must be rejected"
    );
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let text = String::from_utf8_lossy(&bytes);
    assert!(
        text.contains("buffer") || text.contains("body"),
        "the rejection should name the body, got: {text}"
    );
}

//! Integration tests for API hardening, structured errors, and request correlation IDs.

use axum::{
    body::{to_bytes, Body},
    http::{Request, StatusCode},
};
use openflows_manager::server::{create_router, AppState};
use serde_json::Value;
use tower::ServiceExt;

#[tokio::test]
async fn custom_request_id_is_propagated_in_headers_and_error_payload() {
    let state = AppState::for_tests();
    let app = create_router(state);

    let custom_id = "test-corr-id-12345";
    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/tenants/missing-tenant")
                .header("x-request-id", custom_id)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    assert_eq!(
        response
            .headers()
            .get("x-request-id")
            .unwrap()
            .to_str()
            .unwrap(),
        custom_id
    );

    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let json: Value = serde_json::from_slice(&bytes).unwrap();

    assert_eq!(json["error"]["code"], "tenant_not_found");
    assert_eq!(json["error"]["request_id"], custom_id);
    assert!(json["error"]["message"]
        .as_str()
        .unwrap()
        .contains("missing-tenant"));
}

#[tokio::test]
async fn generated_request_id_present_when_none_supplied() {
    let state = AppState::for_tests();
    let app = create_router(state);

    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/fleet/missing-tenant")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let header_id = response
        .headers()
        .get("x-request-id")
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();

    assert!(header_id.starts_with("req_"));

    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let json: Value = serde_json::from_slice(&bytes).unwrap();

    assert_eq!(json["error"]["request_id"], header_id);
}

#[tokio::test]
async fn success_responses_include_request_id_header() {
    let state = AppState::for_tests();
    let app = create_router(state);

    let response = app
        .oneshot(
            Request::builder()
                .uri("/health")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert!(response.headers().contains_key("x-request-id"));
}

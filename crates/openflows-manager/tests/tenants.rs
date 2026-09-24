//! Integration tests for Tenant lifecycle APIs (/api/v1/tenants).

use axum::{
    body::{to_bytes, Body},
    http::{Request, StatusCode},
};
use openflows_manager::server::{create_router, AppState};
use pocketflow_core::SharedStore;
use serde_json::Value;
use tower::ServiceExt;

#[tokio::test]
async fn tenant_crud_lifecycle() {
    let store = SharedStore::new_in_memory();
    let state = AppState::for_tests(); // uses MockTenantProvisioner
    let app = create_router(state);

    // 1. Initially empty
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/v1/tenants")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let bytes = to_bytes(res.into_body(), usize::MAX).await.unwrap();
    let tenants: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(tenants.as_array().unwrap().len(), 0);

    // 2. Create tenant
    let create_payload = serde_json::json!({
        "repo": "acme/backend",
        "name": "acme",
        "fleet": 2
    });
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/tenants")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&create_payload).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(res.status(), StatusCode::CREATED);
    let bytes = to_bytes(res.into_body(), usize::MAX).await.unwrap();
    let json: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(json["tenant"], "acme");
    assert_eq!(json["repository"], "acme/backend");
    assert_eq!(json["fleet"], 2);
    assert_eq!(json["workspace_id"], "ws-mock-nexus-acme");

    // 3. Duplicate creation returns 409 Conflict
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/tenants")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&create_payload).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::CONFLICT);
    let bytes = to_bytes(res.into_body(), usize::MAX).await.unwrap();
    let json: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(json["error"]["code"], "tenant_already_exists");

    // 4. Detail endpoint returns state
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/v1/tenants/acme")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let bytes = to_bytes(res.into_body(), usize::MAX).await.unwrap();
    let json: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(json["name"], "acme");
    assert_eq!(json["repository"], "acme/backend");
    assert!(json["key_count"].as_u64().unwrap() >= 2);

    // 5. Clean tenant resets stale tickets and worker slots
    let t_store = store.for_tenant("acme");
    t_store
        .set(
            "tickets",
            serde_json::json!([
                {
                    "id": "T-1",
                    "title": "Stale ticket",
                    "status": { "type": "failed", "worker_id": "f1", "attempts": 2 },
                    "attempts": 2
                },
                {
                    "id": "T-2",
                    "title": "Open ticket",
                    "status": { "type": "open" },
                    "attempts": 0
                }
            ]),
        )
        .await;
    t_store
        .set(
            "worker_slots",
            serde_json::json!({
                "f1": { "id": "f1", "status": { "type": "working" } }
            }),
        )
        .await;

    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/tenants/acme/clean")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_vec(&serde_json::json!({})).unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let bytes = to_bytes(res.into_body(), usize::MAX).await.unwrap();
    let json: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(json["tenant"], "acme");

    // 6. Delete tenant
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri("/api/v1/tenants/acme?purge=true")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let bytes = to_bytes(res.into_body(), usize::MAX).await.unwrap();
    let json: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(json["tenant"], "acme");

    // 7. After deletion, get returns 404
    let res = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/tenants/acme")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn create_tenant_input_validations() {
    let state = AppState::for_tests();
    let app = create_router(state);

    // Invalid repository format (missing slash)
    let bad_repo = serde_json::json!({
        "repo": "noslash",
        "name": "test"
    });
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/tenants")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&bad_repo).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    let bytes = to_bytes(res.into_body(), usize::MAX).await.unwrap();
    let json: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(json["error"]["code"], "invalid_repository");

    // Invalid tenant name characters
    let bad_name = serde_json::json!({
        "repo": "owner/repo",
        "name": "bad@name!"
    });
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/tenants")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&bad_name).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    let bytes = to_bytes(res.into_body(), usize::MAX).await.unwrap();
    let json: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(json["error"]["code"], "invalid_tenant_name");

    // Fleet < 1
    let bad_fleet = serde_json::json!({
        "repo": "owner/repo",
        "name": "valid",
        "fleet": 0
    });
    let res = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/tenants")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&bad_fleet).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    let bytes = to_bytes(res.into_body(), usize::MAX).await.unwrap();
    let json: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(json["error"]["code"], "invalid_request");
}

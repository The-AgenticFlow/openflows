//! Integration tests for the Fleet API (/api/v1/fleet).

use axum::{
    body::{to_bytes, Body},
    http::{Request, StatusCode},
};
use openflows_manager::server::{create_router, AppState};
use pocketflow_core::SharedStore;
use serde_json::Value;
use tower::ServiceExt;

#[tokio::test]
async fn fleet_endpoint_returns_empty_when_no_tenants() {
    let state = AppState::for_tests();
    let app = create_router(state);

    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/fleet")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let json: Value = serde_json::from_slice(&bytes).unwrap();

    assert_eq!(json["total_tenants"], 0);
    assert_eq!(json["tenants"].as_array().unwrap().len(), 0);
    assert_eq!(json["summary"]["total_tickets"], 0);
}

#[tokio::test]
async fn tenant_fleet_endpoint_returns_runtime_state_and_counts() {
    let store = SharedStore::new_in_memory_with_tenant("acme");
    let state = AppState::new(store.clone());
    let app = create_router(state);

    // Setup tenant state
    let t_store = store.for_tenant("acme");
    t_store
        .set("repository", serde_json::json!("acme/web-app"))
        .await;

    let tickets = serde_json::json!([
        {
            "id": "T-001",
            "title": "First ticket",
            "priority": 1,
            "status": { "type": "open" },
            "attempts": 0
        },
        {
            "id": "T-002",
            "title": "Failing ticket",
            "priority": 2,
            "status": {
                "type": "failed",
                "worker_id": "forge-1",
                "reason": "Compilation error in tests",
                "attempts": 3
            },
            "attempts": 3
        },
        {
            "id": "T-003",
            "title": "Escalated ticket",
            "priority": 3,
            "status": {
                "type": "awaiting_human",
                "worker_id": "sentinel-1",
                "reason": "Security review needed",
                "attempts": 1
            },
            "attempts": 1
        }
    ]);
    t_store.set("tickets", tickets).await;

    let worker_slots = serde_json::json!({
        "forge-1": {
            "id": "forge-1",
            "status": {
                "type": "working",
                "ticket_id": "T-001"
            },
            "workspace_id": "ws-123"
        }
    });
    t_store.set("worker_slots", worker_slots).await;

    let pending_prs = serde_json::json!([
        {
            "number": 42,
            "ticket_id": "T-001",
            "title": "Add feature"
        }
    ]);
    t_store.set("pending_prs", pending_prs).await;

    t_store
        .set("ci_readiness", serde_json::json!({ "type": "ready" }))
        .await;

    t_store
        .set(
            "ticket:T-001:status",
            serde_json::json!({
                "phase": "building",
                "role": "forge",
                "ts": 1234567890
            }),
        )
        .await;

    t_store
        .set(
            "heartbeat:forge-T-001",
            serde_json::json!({
                "ts": 1234567890,
                "ws_id": "ws-123",
                "status": "running"
            }),
        )
        .await;

    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/fleet/acme")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let json: Value = serde_json::from_slice(&bytes).unwrap();

    assert_eq!(json["tenant"], "acme");
    assert_eq!(json["repository"], "acme/web-app");
    assert_eq!(json["ticket_counts"]["total"], 3);
    assert_eq!(json["ticket_counts"]["open"], 1);
    assert_eq!(json["ticket_counts"]["failed"], 1);
    assert_eq!(json["ticket_counts"]["awaiting_human"], 1);
    assert_eq!(json["escalations"]["awaiting_human_count"], 1);
    assert_eq!(json["escalations"]["failed_count"], 1);
    assert_eq!(json["escalations"]["tickets"].as_array().unwrap().len(), 2);
    assert_eq!(json["ci_readiness"], "ready");
    assert_eq!(json["phases"]["T-001"]["phase"], "building");
    assert!(json["heartbeats"].get("forge-T-001").is_some());
    assert_eq!(json["pending_prs"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn multi_tenant_fleet_summary_aggregates_and_preserves_isolation() {
    let store = SharedStore::new_in_memory();
    let state = AppState::new(store.clone());

    // Setup tenant alpha
    let store_alpha = store.for_tenant("alpha");
    store_alpha
        .set("repository", serde_json::json!("org/alpha"))
        .await;
    store_alpha
        .set(
            "tickets",
            serde_json::json!([
                { "id": "T-A1", "title": "Alpha 1", "status": { "type": "open" } }
            ]),
        )
        .await;

    // Setup tenant beta
    let store_beta = store.for_tenant("beta");
    store_beta
        .set("repository", serde_json::json!("org/beta"))
        .await;
    store_beta
        .set(
            "tickets",
            serde_json::json!([
                { "id": "T-B1", "title": "Beta 1", "status": { "type": "open" } },
                { "id": "T-B2", "title": "Beta 2", "status": { "type": "open" } }
            ]),
        )
        .await;

    let app = create_router(state);

    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/fleet")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let json: Value = serde_json::from_slice(&bytes).unwrap();

    assert_eq!(json["total_tenants"], 2);
    assert_eq!(json["summary"]["total_tickets"], 3);

    let tenants = json["tenants"].as_array().unwrap();
    let alpha = tenants.iter().find(|t| t["tenant"] == "alpha").unwrap();
    let beta = tenants.iter().find(|t| t["tenant"] == "beta").unwrap();

    assert_eq!(alpha["tickets"].as_array().unwrap().len(), 1);
    assert_eq!(beta["tickets"].as_array().unwrap().len(), 2);
    // Verify no cross-tenant bleeding
    assert!(alpha["tickets"][0]["id"] == "T-A1");
    assert!(beta["tickets"][0]["id"] == "T-B1");
}

#[tokio::test]
async fn tenant_fleet_not_found_returns_404_with_structured_error() {
    let state = AppState::for_tests();
    let app = create_router(state);

    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/fleet/nonexistent")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let json: Value = serde_json::from_slice(&bytes).unwrap();

    assert_eq!(json["error"]["code"], "tenant_not_found");
    assert!(json["error"]["message"]
        .as_str()
        .unwrap()
        .contains("nonexistent"));
}

#[tokio::test]
async fn tenant_fleet_invalid_tenant_name_returns_400() {
    let state = AppState::for_tests();
    let app = create_router(state);

    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/fleet/bad%40name") // decoded as bad@name
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let json: Value = serde_json::from_slice(&bytes).unwrap();

    assert_eq!(json["error"]["code"], "invalid_tenant_name");
}

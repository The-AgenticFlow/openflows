//! Integration tests for normalized Kanban and Ticket APIs (/api/v1/tenants/{tenant}/tickets).

use axum::{
    body::{to_bytes, Body},
    http::{Request, StatusCode},
};
use openflows_manager::server::{create_router, AppState};
use pocketflow_core::SharedStore;
use serde_json::Value;
use tower::ServiceExt;

#[tokio::test]
async fn normalized_tickets_stage_resolution_and_detail() {
    let store = SharedStore::new_in_memory();
    let state = AppState::new(store.clone());
    let app = create_router(state);

    let t_store = store.for_tenant("acme");
    t_store
        .set("repository", serde_json::json!("acme/repo"))
        .await;

    // Create tickets representing different stages
    let tickets = serde_json::json!([
        {
            "id": "T-1",
            "title": "Open ticket without phase",
            "status": { "type": "open" },
            "priority": 1,
            "attempts": 0
        },
        {
            "id": "T-2",
            "title": "Planning ticket",
            "status": { "type": "assigned", "worker_id": "forge-1" },
            "priority": 2,
            "attempts": 0
        },
        {
            "id": "T-3",
            "title": "Building ticket",
            "status": { "type": "in_progress", "worker_id": "forge-1" },
            "priority": 3,
            "attempts": 0
        },
        {
            "id": "T-4",
            "title": "Review ticket",
            "status": { "type": "assigned", "worker_id": "sentinel-1" },
            "priority": 4,
            "attempts": 0
        },
        {
            "id": "T-5",
            "title": "Awaiting human ticket",
            "status": {
                "type": "awaiting_human",
                "worker_id": "sentinel-1",
                "reason": "Approval needed",
                "attempts": 1
            },
            "priority": 5,
            "attempts": 1
        },
        {
            "id": "T-6",
            "title": "Merged ticket",
            "status": {
                "type": "merged",
                "worker_id": "vessel-1",
                "pr_number": 99
            },
            "priority": 6,
            "attempts": 1
        }
    ]);
    t_store.set("tickets", tickets).await;

    // Phases
    t_store
        .set(
            "ticket:T-2:status",
            serde_json::json!({ "phase": "planning", "role": "forge" }),
        )
        .await;
    t_store
        .set(
            "ticket:T-3:status",
            serde_json::json!({ "phase": "building", "role": "forge" }),
        )
        .await;
    t_store
        .set(
            "ticket:T-4:status",
            serde_json::json!({ "phase": "review_ready", "role": "sentinel" }),
        )
        .await;

    // Artifacts for T-4
    t_store
        .set(
            "ticket:T-4:pr",
            serde_json::json!({ "number": 101, "title": "Feat: core" }),
        )
        .await;
    t_store
        .set(
            "ticket:T-4:review:sentinel",
            serde_json::json!({ "verdict": "approve", "report": "All tests pass" }),
        )
        .await;
    t_store
        .set(
            "ticket:T-4:gate:planning",
            serde_json::json!({ "approved_by": "sentinel", "ts": 123456 }),
        )
        .await;

    // 1. List tickets endpoint
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/v1/tenants/acme/tickets")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(res.status(), StatusCode::OK);
    let bytes = to_bytes(res.into_body(), usize::MAX).await.unwrap();
    let list: Value = serde_json::from_slice(&bytes).unwrap();
    let array = list.as_array().unwrap();
    assert_eq!(array.len(), 6);

    let t1 = array.iter().find(|t| t["id"] == "T-1").unwrap();
    assert_eq!(t1["stage"], "open");
    assert_eq!(t1["stage_label"], "Open");

    let t2 = array.iter().find(|t| t["id"] == "T-2").unwrap();
    assert_eq!(t2["stage"], "planning");
    assert_eq!(t2["stage_label"], "Planning");

    let t3 = array.iter().find(|t| t["id"] == "T-3").unwrap();
    assert_eq!(t3["stage"], "building");
    assert_eq!(t3["stage_label"], "Building");

    let t4 = array.iter().find(|t| t["id"] == "T-4").unwrap();
    assert_eq!(t4["stage"], "review");
    assert_eq!(t4["stage_label"], "Review");

    let t5 = array.iter().find(|t| t["id"] == "T-5").unwrap();
    assert_eq!(t5["stage"], "awaiting_human");
    assert_eq!(t5["stage_label"], "Awaiting Human");

    let t6 = array.iter().find(|t| t["id"] == "T-6").unwrap();
    assert_eq!(t6["stage"], "done");
    assert_eq!(t6["stage_label"], "Done");

    // 2. Ticket detail endpoint with rich artifacts
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/v1/tenants/acme/tickets/T-4")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(res.status(), StatusCode::OK);
    let bytes = to_bytes(res.into_body(), usize::MAX).await.unwrap();
    let detail: Value = serde_json::from_slice(&bytes).unwrap();

    assert_eq!(detail["id"], "T-4");
    assert_eq!(detail["tenant"], "acme");
    assert_eq!(detail["stage"], "review");
    assert_eq!(detail["pr"]["number"], 101);
    assert_eq!(detail["review"]["verdict"], "approve");
    assert_eq!(detail["reviews"].as_array().unwrap().len(), 1);
    assert_eq!(detail["gates"].as_array().unwrap().len(), 1);

    // 3. Ticket detail endpoint with clean degradation on missing artifacts
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/v1/tenants/acme/tickets/T-1")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(res.status(), StatusCode::OK);
    let bytes = to_bytes(res.into_body(), usize::MAX).await.unwrap();
    let detail: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(detail["id"], "T-1");
    assert!(detail["pr"].is_null());
    assert!(detail["review"].is_null());
    assert_eq!(detail["reviews"].as_array().unwrap().len(), 0);

    // 4. Ticket not found
    let res = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/tenants/acme/tickets/T-NONEXISTENT")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(res.status(), StatusCode::NOT_FOUND);
    let bytes = to_bytes(res.into_body(), usize::MAX).await.unwrap();
    let json: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(json["error"]["code"], "ticket_not_found");
}

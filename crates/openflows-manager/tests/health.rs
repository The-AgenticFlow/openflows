//! Smoke tests for Manager liveness, readiness, routing, and shutdown behavior.

use axum::{
    body::{to_bytes, Body},
    http::{Request, StatusCode},
};
use serde_json::Value;
use std::{future::Future, pin::Pin, sync::Arc};
use tokio::{net::TcpListener, sync::oneshot};
use tower::ServiceExt;

#[derive(Clone)]
struct UnreadyProbe;

impl openflows_manager::server::ReadinessCheck for UnreadyProbe {
    fn check(
        &self,
    ) -> Pin<Box<dyn Future<Output = Result<(), openflows_manager::error::ManagerError>> + Send + '_>>
    {
        Box::pin(async {
            Err(openflows_manager::error::ManagerError::Service(
                anyhow::anyhow!("store unavailable"),
            ))
        })
    }
}

#[tokio::test]
async fn health_endpoint_returns_ready_without_secrets() {
    let app =
        openflows_manager::server::create_router(openflows_manager::server::AppState::for_tests());

    let response = app
        .oneshot(
            Request::builder()
                .uri("/health")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(StatusCode::OK, response.status());

    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let value: Value = serde_json::from_slice(&body).unwrap();

    assert_eq!(value["status"], "ready");
    assert_eq!(value["service"], "openflows-manager");
    assert_eq!(value["api_version"], "v1");

    let serialized = value.to_string();
    assert!(!serialized.contains("redis://"));
    assert!(!serialized.contains("CODER_SESSION_TOKEN"));
}

#[tokio::test]
async fn health_remains_live_when_readiness_dependency_fails() {
    let app = openflows_manager::server::create_router(
        openflows_manager::server::AppState::with_readiness_check(
            pocketflow_core::SharedStore::new_in_memory_with_tenant("test"),
            Arc::new(UnreadyProbe),
        ),
    );

    let response = app
        .oneshot(
            Request::builder()
                .uri("/health")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(StatusCode::OK, response.status());
}

#[tokio::test]
async fn readiness_returns_unavailable_when_dependency_check_fails() {
    let app = openflows_manager::server::create_router(
        openflows_manager::server::AppState::with_readiness_check(
            pocketflow_core::SharedStore::new_in_memory_with_tenant("test"),
            Arc::new(UnreadyProbe),
        ),
    );

    let response = app
        .oneshot(
            Request::builder()
                .uri("/ready")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(StatusCode::SERVICE_UNAVAILABLE, response.status());

    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let value: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(value["status"], "unavailable");
    assert_eq!(value["service"], "openflows-manager");

    let serialized = value.to_string();
    assert!(!serialized.contains("redis://"));
    assert!(!serialized.contains("store unavailable"));
}

#[tokio::test]
async fn api_v1_router_is_mounted_for_future_routes() {
    let app =
        openflows_manager::server::create_router(openflows_manager::server::AppState::for_tests());

    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/v1")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(StatusCode::OK, response.status());

    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let value: Value = serde_json::from_slice(&body).unwrap();

    assert_eq!(value["version"], "v1");
}

#[tokio::test]
async fn server_starts_serves_readiness_and_shuts_down() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (shutdown_tx, shutdown_rx) = oneshot::channel();

    let server = tokio::spawn(openflows_manager::server::serve(
        listener,
        openflows_manager::server::AppState::for_tests(),
        async {
            let _ = shutdown_rx.await;
        },
    ));

    let response = reqwest::get(format!("http://{addr}/ready")).await.unwrap();
    assert_eq!(StatusCode::OK, response.status());

    let value: Value = response.json().await.unwrap();
    assert_eq!(value["status"], "ready");

    shutdown_tx.send(()).unwrap();
    server.await.unwrap().unwrap();
}

//! Readiness and liveness behavior for the Manager, including the WP-01
//! requirement that database unavailability causes a bounded readiness failure
//! while process liveness remains healthy, and that existing local-mode health
//! behavior is preserved.

use axum::{
    body::{to_bytes, Body},
    http::{Request, StatusCode},
};
use openflows_manager::db::Db;
use openflows_manager::server::{AppState, ManagerServices, ReadinessCheck};
use serde_json::Value;
use sqlx::postgres::PgPoolOptions;
use std::{future::Future, pin::Pin, sync::Arc, time::Duration};
use tower::ServiceExt;

/// A readiness probe that always succeeds, so a test can isolate the database
/// dependency's effect on readiness.
#[derive(Clone)]
struct OkProbe;

impl ReadinessCheck for OkProbe {
    fn check(
        &self,
    ) -> Pin<Box<dyn Future<Output = Result<(), openflows_manager::error::ManagerError>> + Send + '_>>
    {
        Box::pin(async { Ok(()) })
    }
}

/// Build an AppState whose database points at an unreachable address (port 1 is
/// never a listening Postgres), simulating runtime database unavailability.
fn state_with_unreachable_db() -> AppState {
    let url = "postgres://openflows:openflows@127.0.0.1:1/openflows_control_plane";
    let pool = PgPoolOptions::new()
        .max_connections(1)
        .acquire_timeout(Duration::from_millis(300))
        .connect_lazy(url)
        .expect("lazy pool (no connect yet)");
    let services = ManagerServices::from_db(Db::from_pool(pool));
    let config = openflows_manager::config::ManagerConfig {
        mode: openflows_manager::config::Mode::Hosted,
        database_url: Some(url.to_string()),
        secret_provider: "in-memory".to_string(),
        http_addr: "127.0.0.1:3002".to_string(),
    };
    AppState::with_services(
        pocketflow_core::SharedStore::new_in_memory_with_tenant("test"),
        Arc::new(OkProbe),
        services,
        config,
    )
}

#[tokio::test]
async fn health_remains_live_when_database_is_unavailable() {
    // Criterion 8: liveness must stay 200 even when the database is down.
    let app = openflows_manager::server::create_router(state_with_unreachable_db());

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
async fn readiness_is_bounded_and_unavailable_when_database_is_unavailable() {
    // Criterion 8: readiness must fail (503) and must be bounded (no hang) when
    // the database is unreachable, while liveness stays healthy (covered above).
    let app = openflows_manager::server::create_router(state_with_unreachable_db());

    let start = std::time::Instant::now();
    let response = tokio::time::timeout(
        Duration::from_secs(5),
        app.oneshot(
            Request::builder()
                .uri("/ready")
                .body(Body::empty())
                .unwrap(),
        ),
    )
    .await
    .expect("readiness must be bounded and not hang")
    .unwrap();

    assert!(
        start.elapsed() < Duration::from_secs(5),
        "readiness must fail within a bounded window"
    );
    assert_eq!(StatusCode::SERVICE_UNAVAILABLE, response.status());

    // The readiness body must identify the failing dependency separately and
    // must not leak internal error text.
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let value: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(value["status"], "unavailable");
    assert_eq!(value["service"], "openflows-manager");

    let deps = value["dependencies"]
        .as_array()
        .expect("dependencies array");
    let database = deps
        .iter()
        .find(|d| d["name"] == "database")
        .expect("database dependency present");
    assert_eq!(
        database["ok"], false,
        "database dependency must be reported down"
    );

    let serialized = value.to_string();
    assert!(
        !serialized.contains("postgres://"),
        "no connection URL leaked"
    );
    assert!(!serialized.contains("openflows_control_plane"));
}

#[tokio::test]
async fn local_mode_without_database_stays_healthy() {
    // Criterion 9: the existing local-mode path (no control-plane database)
    // must remain healthy — health and readiness both succeed via the in-memory
    // store, exactly as before WP-01.
    let app = openflows_manager::server::create_router(AppState::for_tests());

    let health = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/health")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(StatusCode::OK, health.status());

    let ready = app
        .oneshot(
            Request::builder()
                .uri("/ready")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(StatusCode::OK, ready.status());
}

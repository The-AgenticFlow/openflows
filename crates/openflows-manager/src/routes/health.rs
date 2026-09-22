//! Liveness and readiness endpoints for the Manager process.

use crate::server::AppState;
use axum::{extract::State, http::StatusCode, response::IntoResponse, Json};
use serde::Serialize;

#[derive(Serialize)]
pub struct HealthResponse {
    // Keep field names stable and generic so both liveness and readiness can
    // share the same response type.
    status: &'static str,
    service: &'static str,
    api_version: &'static str,
}

pub async fn health() -> Json<HealthResponse> {
    // Liveness answers whether the process can serve HTTP, not whether every
    // dependency is healthy. This endpoint intentionally does not touch Redis.
    Json(HealthResponse {
        status: "ready",
        service: "openflows-manager",
        api_version: "v1",
    })
}

pub async fn ready(State(state): State<AppState>) -> impl IntoResponse {
    // Readiness is stricter than liveness: callers should route traffic here
    // only after the manager can reach the tenant-scoped backing store.
    match state.check_readiness().await {
        Ok(()) => (
            StatusCode::OK,
            Json(HealthResponse {
                status: "ready",
                service: "openflows-manager",
                api_version: "v1",
            }),
        ),
        Err(error) => {
            // Log the specific failure for operators, but return a sanitized
            // body so dependency URLs, tokens, or internal errors are not
            // exposed through a public probe.
            tracing::warn!(%error, "OpenFlows Manager readiness check failed");
            (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(HealthResponse {
                    status: "unavailable",
                    service: "openflows-manager",
                    api_version: "v1",
                }),
            )
        }
    }
}

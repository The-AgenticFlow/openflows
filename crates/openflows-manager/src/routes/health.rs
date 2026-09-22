//! Liveness and readiness endpoints for the Manager process.

use crate::server::AppState;
use axum::{extract::State, http::StatusCode, response::IntoResponse, Json};
use serde::Serialize;

#[derive(Serialize)]
pub struct HealthResponse {
    status: &'static str,
    service: &'static str,
    api_version: &'static str,
}

pub async fn health() -> Json<HealthResponse> {
    Json(HealthResponse {
        status: "ready",
        service: "openflows-manager",
        api_version: "v1",
    })
}

pub async fn ready(State(state): State<AppState>) -> impl IntoResponse {
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

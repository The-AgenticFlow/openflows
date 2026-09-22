use crate::server::AppState;
use axum::{extract::State, Json};
use serde::Serialize;

#[derive(Serialize)]
pub struct HealthResponse {
    status: &'static str,
    service: &'static str,
    api_version: &'static str,
}

pub async fn health(State(state): State<AppState>) -> Json<HealthResponse> {
    let _ = state.store();

    Json(HealthResponse {
        status: "ready",
        service: "openflows-manager",
        api_version: "v1",
    })
}

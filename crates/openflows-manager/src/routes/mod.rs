//! Versioned HTTP route composition for the OpenFlows Manager API.

use crate::server::AppState;
use axum::{routing::get, Json, Router};
use serde::Serialize;

pub mod health;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/health", get(health::health))
        .route("/ready", get(health::ready))
        .nest("/api/v1", api_v1_router())
}

fn api_v1_router() -> Router<AppState> {
    Router::new().route("/", get(api_index))
}

#[derive(Serialize)]
struct ApiIndexResponse {
    version: &'static str,
}

async fn api_index() -> Json<ApiIndexResponse> {
    Json(ApiIndexResponse { version: "v1" })
}

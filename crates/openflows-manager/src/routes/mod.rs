//! Versioned HTTP route composition for the OpenFlows Manager API.

use crate::server::AppState;
use axum::{routing::get, Json, Router};
use serde::Serialize;

pub mod health;

pub fn router() -> Router<AppState> {
    // Keep platform-oriented probes at the root because orchestrators and load
    // balancers generally expect stable, version-independent health URLs.
    Router::new()
        .route("/health", get(health::health))
        .route("/ready", get(health::ready))
        // Product/API routes are versioned from the start so future public
        // endpoints can evolve without moving operational probes.
        .nest("/api/v1", api_v1_router())
}

fn api_v1_router() -> Router<AppState> {
    // The index route gives clients a cheap way to confirm the v1 mount exists
    // while the manager API surface is still growing.
    Router::new().route("/", get(api_index))
}

#[derive(Serialize)]
struct ApiIndexResponse {
    version: &'static str,
}

async fn api_index() -> Json<ApiIndexResponse> {
    // Return a deliberately small payload. The index is a capability marker,
    // not a service-discovery document.
    Json(ApiIndexResponse { version: "v1" })
}

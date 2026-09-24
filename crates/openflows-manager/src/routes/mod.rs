//! Versioned HTTP route composition for the OpenFlows Manager API.

use crate::server::AppState;
use axum::{
    routing::{get, post},
    Json, Router,
};
use serde::Serialize;

pub mod fleet;
pub mod health;
pub mod tenants;
pub mod tickets;

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
    Router::new()
        .route("/", get(api_index))
        // Fleet routes
        .route("/fleet", get(fleet::get_fleet))
        .route("/fleet/{tenant}", get(fleet::get_tenant_fleet))
        // Tenant routes
        .route(
            "/tenants",
            get(tenants::list_tenants).post(tenants::create_tenant),
        )
        .route(
            "/tenants/{tenant}",
            get(tenants::get_tenant).delete(tenants::remove_tenant),
        )
        .route("/tenants/{tenant}/clean", post(tenants::clean_tenant))
        // Ticket / Kanban routes
        .route("/tenants/{tenant}/tickets", get(tickets::list_tickets))
        .route(
            "/tenants/{tenant}/tickets/{ticket}",
            get(tickets::get_ticket),
        )
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

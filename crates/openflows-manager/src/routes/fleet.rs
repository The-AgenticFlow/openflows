//! Fleet HTTP routes.

use crate::{
    error::ApiErrorEnvelope,
    middleware::RequestId,
    models::fleet::{FleetSummaryResponse, TenantFleetResponse},
    server::AppState,
};
use axum::{
    extract::{Extension, Path, State},
    http::StatusCode,
    Json,
};

/// Retrieve multi-tenant fleet overview.
pub async fn get_fleet(
    State(state): State<AppState>,
    request_id: Option<Extension<RequestId>>,
) -> Result<Json<FleetSummaryResponse>, (StatusCode, Json<ApiErrorEnvelope>)> {
    let req_id = request_id.as_ref().map(|Extension(r)| r);
    state
        .fleet_service()
        .get_fleet_summary()
        .await
        .map(Json)
        .map_err(|err| err.to_api_response(req_id))
}

/// Retrieve single-tenant fleet runtime state.
pub async fn get_tenant_fleet(
    State(state): State<AppState>,
    Path(tenant): Path<String>,
    request_id: Option<Extension<RequestId>>,
) -> Result<Json<TenantFleetResponse>, (StatusCode, Json<ApiErrorEnvelope>)> {
    let req_id = request_id.as_ref().map(|Extension(r)| r);
    state
        .fleet_service()
        .get_tenant_fleet(&tenant)
        .await
        .map(Json)
        .map_err(|err| err.to_api_response(req_id))
}

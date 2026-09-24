//! Tenant lifecycle HTTP routes.

use crate::{
    error::ApiErrorEnvelope,
    middleware::RequestId,
    models::tenant::{
        TenantCleanRequest, TenantCleanResponse, TenantCreateRequest, TenantCreateResponse,
        TenantDetail, TenantRemoveQuery, TenantRemoveResponse, TenantSummary,
    },
    server::AppState,
};
use axum::{
    extract::{Extension, Path, Query, State},
    http::StatusCode,
    Json,
};

/// List all registered tenants.
pub async fn list_tenants(
    State(state): State<AppState>,
    request_id: Option<Extension<RequestId>>,
) -> Result<Json<Vec<TenantSummary>>, (StatusCode, Json<ApiErrorEnvelope>)> {
    let req_id = request_id.as_ref().map(|Extension(r)| r);
    state
        .tenant_service()
        .list_tenants()
        .await
        .map(Json)
        .map_err(|err| err.to_api_response(req_id))
}

/// Create/register a new tenant environment.
pub async fn create_tenant(
    State(state): State<AppState>,
    request_id: Option<Extension<RequestId>>,
    Json(payload): Json<TenantCreateRequest>,
) -> Result<(StatusCode, Json<TenantCreateResponse>), (StatusCode, Json<ApiErrorEnvelope>)> {
    let req_id = request_id.as_ref().map(|Extension(r)| r);
    state
        .tenant_service()
        .create_tenant(payload)
        .await
        .map(|res| (StatusCode::CREATED, Json(res)))
        .map_err(|err| err.to_api_response(req_id))
}

/// Retrieve detailed state for a tenant.
pub async fn get_tenant(
    State(state): State<AppState>,
    Path(tenant): Path<String>,
    request_id: Option<Extension<RequestId>>,
) -> Result<Json<TenantDetail>, (StatusCode, Json<ApiErrorEnvelope>)> {
    let req_id = request_id.as_ref().map(|Extension(r)| r);
    state
        .tenant_service()
        .get_tenant(&tenant)
        .await
        .map(Json)
        .map_err(|err| err.to_api_response(req_id))
}

/// Reset/clean runtime state for a tenant.
pub async fn clean_tenant(
    State(state): State<AppState>,
    Path(tenant): Path<String>,
    request_id: Option<Extension<RequestId>>,
    payload: Option<Json<TenantCleanRequest>>,
) -> Result<Json<TenantCleanResponse>, (StatusCode, Json<ApiErrorEnvelope>)> {
    let req_id = request_id.as_ref().map(|Extension(r)| r);
    let reset_all = payload.map(|Json(p)| p.reset_all).unwrap_or(false);
    state
        .tenant_service()
        .clean_tenant(&tenant, reset_all)
        .await
        .map(Json)
        .map_err(|err| err.to_api_response(req_id))
}

/// Remove a tenant and optionally purge Redis keys.
pub async fn remove_tenant(
    State(state): State<AppState>,
    Path(tenant): Path<String>,
    Query(query): Query<TenantRemoveQuery>,
    request_id: Option<Extension<RequestId>>,
) -> Result<Json<TenantRemoveResponse>, (StatusCode, Json<ApiErrorEnvelope>)> {
    let req_id = request_id.as_ref().map(|Extension(r)| r);
    state
        .tenant_service()
        .remove_tenant(&tenant, query.purge)
        .await
        .map(Json)
        .map_err(|err| err.to_api_response(req_id))
}

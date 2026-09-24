//! Kanban and normalized ticket HTTP routes.

use crate::{
    error::ApiErrorEnvelope,
    middleware::RequestId,
    models::kanban::{NormalizedTicket, NormalizedTicketDetail},
    server::AppState,
};
use axum::{
    extract::{Extension, Path, State},
    http::StatusCode,
    Json,
};

/// List all normalized tickets and their derived Kanban stages for a tenant.
pub async fn list_tickets(
    State(state): State<AppState>,
    Path(tenant): Path<String>,
    request_id: Option<Extension<RequestId>>,
) -> Result<Json<Vec<NormalizedTicket>>, (StatusCode, Json<ApiErrorEnvelope>)> {
    let req_id = request_id.as_ref().map(|Extension(r)| r);
    state
        .kanban_service()
        .list_normalized_tickets(&tenant)
        .await
        .map(Json)
        .map_err(|err| err.to_api_response(req_id))
}

/// Retrieve detailed normalized ticket with all review, PR, gate, and deployment records.
pub async fn get_ticket(
    State(state): State<AppState>,
    Path((tenant, ticket)): Path<(String, String)>,
    request_id: Option<Extension<RequestId>>,
) -> Result<Json<NormalizedTicketDetail>, (StatusCode, Json<ApiErrorEnvelope>)> {
    let req_id = request_id.as_ref().map(|Extension(r)| r);
    state
        .kanban_service()
        .get_normalized_ticket_detail(&tenant, &ticket)
        .await
        .map(Json)
        .map_err(|err| err.to_api_response(req_id))
}

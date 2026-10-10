//! HTTP routes for the GitHub connection lifecycle (WP-03).
//!
//! Only active Openflows organization admins may initiate, complete, reconcile,
//! or disconnect a connection. Members may view sanitized connection status.
//! Mutations require an `Idempotency-Key`. The OAuth/setup callbacks are browser
//! redirects that validate flow-specific state and the authenticated session.

use crate::connections::service::{CallbackOutcome, ConnectionDto};
use crate::connections::FlowType;
use crate::error::ManagerError;
use crate::id::ConnectionId;
use crate::routes::organizations::{authenticated_user, idempotency_key, parse_org, request_id};
use axum::{
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Redirect, Response},
    Json,
};
use serde::{Deserialize, Serialize};

/// The body for POST /organizations/{org}/github/connect.
#[derive(Deserialize)]
pub struct ConnectRequest {
    #[serde(default)]
    pub flow_type: Option<String>,
}

/// The response for starting a connection flow. Raw URLs are disclosed once.
#[derive(Serialize)]
pub struct ConnectResponse {
    pub attempt_id: String,
    pub authorization_url: Option<String>,
    pub setup_url: Option<String>,
}

#[derive(Serialize)]
pub struct ConnectionsResponse {
    pub items: Vec<ConnectionDto>,
}

#[derive(Deserialize)]
pub struct OauthCallbackQuery {
    pub code: Option<String>,
    pub state: Option<String>,
}

#[derive(Deserialize)]
pub struct SetupCallbackQuery {
    pub state: Option<String>,
    pub installation_id: Option<i64>,
    pub setup_action: Option<String>,
}

/// POST /api/v1/organizations/{org}/github/connect (admin).
pub async fn connect(
    State(state): State<crate::server::AppState>,
    headers: HeaderMap,
    Path(org): Path<String>,
    Json(body): Json<ConnectRequest>,
) -> Result<Response, ManagerError> {
    let services = state
        .services()
        .ok_or_else(|| ManagerError::api("SERVICE_UNAVAILABLE", "service unavailable"))?;
    let principal = authenticated_user(&headers, &services.session_manager).await?;
    crate::routes::organizations::require_csrf_for_browser(&principal, &headers).await?;
    let org_id = parse_org(&org)?;
    let key = idempotency_key(&headers)?;
    let flow_type = parse_flow_type(body.flow_type.as_deref())?;

    let result = services
        .connections_service
        .start_connect(
            principal.user_id,
            org_id,
            flow_type,
            &key,
            &request_id(&headers),
        )
        .await?;
    Ok(Json(ConnectResponse {
        attempt_id: result.attempt_id.to_string(),
        authorization_url: result.authorization_url,
        setup_url: result.setup_url,
    })
    .into_response())
}

/// GET /api/v1/github/oauth/callback — connection-flow OAuth leg.
pub async fn oauth_callback(
    State(state): State<crate::server::AppState>,
    headers: HeaderMap,
    Query(q): Query<OauthCallbackQuery>,
) -> Result<Response, ManagerError> {
    let services = state
        .services()
        .ok_or_else(|| ManagerError::api("SERVICE_UNAVAILABLE", "service unavailable"))?;
    let principal = authenticated_user(&headers, &services.session_manager).await?;
    let (Some(code), Some(state_param)) = (q.code, q.state) else {
        return Err(ManagerError::api(
            "AUTH_FAILED",
            "missing OAuth callback parameters",
        ));
    };
    let request_id = request_id(&headers);
    let outcome = services
        .connections_service
        .oauth_callback(principal.user_id, &state_param, &code, &request_id)
        .await?;
    Ok(callback_redirect(outcome))
}

/// GET /api/v1/github/setup — connection-flow installation-setup leg.
pub async fn setup_callback(
    State(state): State<crate::server::AppState>,
    headers: HeaderMap,
    Query(q): Query<SetupCallbackQuery>,
) -> Result<Response, ManagerError> {
    let services = state
        .services()
        .ok_or_else(|| ManagerError::api("SERVICE_UNAVAILABLE", "service unavailable"))?;
    let principal = authenticated_user(&headers, &services.session_manager).await?;
    let (Some(state_param), Some(installation_id)) = (q.state, q.installation_id) else {
        return Err(ManagerError::api(
            "AUTH_FAILED",
            "missing installation setup parameters",
        ));
    };
    // `setup_action` is never trusted as proof of authority; full owner
    // verification runs at binding time.
    let request_id = request_id(&headers);
    let outcome = services
        .connections_service
        .setup_callback(principal.user_id, &state_param, installation_id, &request_id)
        .await?;
    Ok(callback_redirect(outcome))
}

/// GET /api/v1/organizations/{org}/github/connections (member view).
pub async fn list_connections(
    State(state): State<crate::server::AppState>,
    headers: HeaderMap,
    Path(org): Path<String>,
) -> Result<Json<ConnectionsResponse>, ManagerError> {
    let services = state
        .services()
        .ok_or_else(|| ManagerError::api("SERVICE_UNAVAILABLE", "service unavailable"))?;
    let principal = authenticated_user(&headers, &services.session_manager).await?;
    let org_id = parse_org(&org)?;
    let items = services
        .connections_service
        .list_connections(principal.user_id, org_id, crate::pagination::PageLimit::new(None))
        .await?;
    Ok(Json(ConnectionsResponse { items }))
}

/// POST /api/v1/organizations/{org}/github/connections/{id}/reconcile (admin).
pub async fn reconcile_connection(
    State(state): State<crate::server::AppState>,
    headers: HeaderMap,
    Path((org, id)): Path<(String, String)>,
) -> Result<Response, ManagerError> {
    let services = state
        .services()
        .ok_or_else(|| ManagerError::api("SERVICE_UNAVAILABLE", "service unavailable"))?;
    let principal = authenticated_user(&headers, &services.session_manager).await?;
    crate::routes::organizations::require_csrf_for_browser(&principal, &headers).await?;
    let org_id = parse_org(&org)?;
    let connection_id = parse_connection_id(&id)?;
    let key = idempotency_key(&headers)?;
    let op = services
        .connections_service
        .reconcile_connection(
            principal.user_id,
            org_id,
            connection_id,
            &key,
            &request_id(&headers),
        )
        .await?;
    Ok((
        StatusCode::ACCEPTED,
        Json(crate::routes::organizations::OperationResponse {
            operation_id: op.to_string(),
            resource_id: connection_id.to_string(),
            status: "queued".to_string(),
        }),
    )
        .into_response())
}

/// DELETE /api/v1/organizations/{org}/github/connections/{id} (admin).
pub async fn disconnect(
    State(state): State<crate::server::AppState>,
    headers: HeaderMap,
    Path((org, id)): Path<(String, String)>,
) -> Result<Response, ManagerError> {
    let services = state
        .services()
        .ok_or_else(|| ManagerError::api("SERVICE_UNAVAILABLE", "service unavailable"))?;
    let principal = authenticated_user(&headers, &services.session_manager).await?;
    crate::routes::organizations::require_csrf_for_browser(&principal, &headers).await?;
    let org_id = parse_org(&org)?;
    let connection_id = parse_connection_id(&id)?;
    let key = idempotency_key(&headers)?;
    let op = services
        .connections_service
        .disconnect(
            principal.user_id,
            org_id,
            connection_id,
            &key,
            &request_id(&headers),
        )
        .await?;
    Ok((
        StatusCode::ACCEPTED,
        Json(crate::routes::organizations::OperationResponse {
            operation_id: op.to_string(),
            resource_id: connection_id.to_string(),
            status: "queued".to_string(),
        }),
    )
        .into_response())
}

fn parse_flow_type(raw: Option<&str>) -> Result<FlowType, ManagerError> {
    match raw {
        None | Some("both") => Ok(FlowType::Both),
        Some("user_oauth") => Ok(FlowType::UserOauth),
        Some("installation_setup") => Ok(FlowType::InstallationSetup),
        Some(other) => Err(ManagerError::InvalidInput(format!(
            "invalid flow_type '{other}'"
        ))),
    }
}

fn parse_connection_id(s: &str) -> Result<ConnectionId, ManagerError> {
    s.parse::<ConnectionId>()
        .map_err(|_| ManagerError::not_found("connection"))
}

/// Redirect to a simple completion page depending on the flow outcome. No
/// tokens or sensitive state are placed in the redirect target.
fn callback_redirect(outcome: CallbackOutcome) -> Response {
    let path = match outcome {
        CallbackOutcome::Connected { .. } => "/api/v1/me?connection=connected",
        CallbackOutcome::PendingAnotherLeg => "/api/v1/me?connection=pending",
    };
    Redirect::temporary(path).into_response()
}

//! Organization, membership, and invitation routes.
//!
//! All org-scoped lookups go through the central policy layer after resolving
//! the caller's *current* membership from the database. Browser mutations carry
//! a CSRF token; CLI calls use a Bearer access token.

use crate::auth::csrf;
use crate::auth::sessions::SessionManager;
use crate::error::ManagerError;
use crate::id::{InvitationId, OrganizationId, UserId};
use crate::organizations::policy::Policy;
use crate::routes::auth::{bearer_token, parse_cookies, SESSION_COOKIE};
use crate::server::AppState;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::{Deserialize, Serialize};

/// The resolved authenticated principal for an org request.
pub struct AuthPrincipal {
    pub user_id: UserId,
    /// The session id (for recent-auth checks). Browser sessions always carry
    /// one; CLI sessions carry one too.
    pub session_id: crate::id::SessionId,
    /// True when the request came from a browser cookie (requires CSRF on
    /// mutations).
    pub is_browser: bool,
}

/// Resolve the authenticated user from a browser cookie or CLI bearer.
pub async fn authenticated_user(
    headers: &HeaderMap,
    session_manager: &SessionManager,
) -> Result<AuthPrincipal, ManagerError> {
    if let Some(cookie) = parse_cookies(headers).get(SESSION_COOKIE) {
        let p = session_manager
            .validate_browser(cookie)
            .await?
            .ok_or_else(|| ManagerError::api("UNAUTHORIZED", "session is invalid or expired"))?;
        return Ok(AuthPrincipal {
            user_id: p.user_id,
            session_id: p.session_id,
            is_browser: true,
        });
    }
    if let Some(bearer) = bearer_token(headers) {
        let p = session_manager
            .validate_cli(&bearer)
            .await?
            .ok_or_else(|| {
                ManagerError::api("UNAUTHORIZED", "access token is invalid or expired")
            })?;
        return Ok(AuthPrincipal {
            user_id: p.user_id,
            session_id: p.session_id,
            is_browser: false,
        });
    }
    Err(ManagerError::api("UNAUTHORIZED", "authentication required"))
}

/// Validate CSRF for browser mutations. CLI (bearer) requests are exempt.
pub async fn require_csrf_for_browser(
    principal: &AuthPrincipal,
    headers: &HeaderMap,
) -> Result<(), ManagerError> {
    if !principal.is_browser {
        return Ok(());
    }
    let cookie = parse_cookies(headers)
        .get(csrf::CSRF_COOKIE)
        .cloned()
        .unwrap_or_default();
    let presented = headers
        .get(csrf::CSRF_HEADER)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    csrf::validate(&presented, &cookie)?;
    csrf::validate_session(headers, &presented)
}

fn request_id(headers: &HeaderMap) -> String {
    headers
        .get("x-request-id")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string())
        .unwrap_or_default()
}

#[derive(Deserialize)]
pub struct CreateOrgRequest {
    pub slug: String,
    pub display_name: String,
}

#[derive(Serialize)]
pub struct OperationResponse {
    pub operation_id: String,
    pub resource_id: String,
    pub status: String,
}

/// `POST /organizations` — create an organization (202 + durable operation).
pub async fn create_org(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<CreateOrgRequest>,
) -> Result<Response, ManagerError> {
    let services = state
        .services()
        .ok_or_else(|| ManagerError::api("SERVICE_UNAVAILABLE", "service unavailable"))?;
    let principal = authenticated_user(&headers, &services.session_manager).await?;
    require_csrf_for_browser(&principal, &headers).await?;
    let key = idempotency_key(&headers)?;

    let result = services
        .orgs_service
        .create_org(
            principal.user_id,
            &body.slug,
            &body.display_name,
            &key,
            &request_id(&headers),
        )
        .await?;

    Ok((
        StatusCode::ACCEPTED,
        Json(OperationResponse {
            operation_id: result.operation_id.to_string(),
            resource_id: result.resource_id.to_string(),
            status: result.status.to_string(),
        }),
    )
        .into_response())
}

/// `GET /organizations` — list the caller's memberships.
pub async fn list_orgs(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<MembersQuery>,
) -> Result<Json<serde_json::Value>, ManagerError> {
    let services = state
        .services()
        .ok_or_else(|| ManagerError::api("SERVICE_UNAVAILABLE", "service unavailable"))?;
    let principal = authenticated_user(&headers, &services.session_manager).await?;
    let limit = crate::pagination::PageLimit::new(q.limit);
    let after = q
        .after
        .as_deref()
        .map(crate::pagination::Cursor::decode)
        .transpose()?
        .map(|c| {
            c.tiebreaker
                .parse::<uuid::Uuid>()
                .map_err(|_| ManagerError::InvalidInput("malformed cursor".into()))
        })
        .transpose()?;
    let rows: Vec<(uuid::Uuid, String, String, String, String)> = sqlx::query_as(
        "SELECT o.id,o.slug,o.display_name,m.role,o.status FROM organizations o
         JOIN memberships m ON m.organization_id=o.id
         WHERE m.user_id=$1 AND m.status='active' AND o.status <> 'deleted'
           AND ($2::uuid IS NULL OR o.id>$2) ORDER BY o.id LIMIT $3",
    )
    .bind(principal.user_id.0)
    .bind(after)
    .bind(i64::from(limit.get()) + 1)
    .fetch_all(services.db.pool())
    .await
    .map_err(ManagerError::from)?;
    let more = rows.len() > limit.get() as usize;
    let items: Vec<_> = rows.into_iter().take(limit.get() as usize).map(|(id,slug,display_name,role,status)|
        serde_json::json!({"organization_id":id,"slug":slug,"display_name":display_name,"role":role,"status":status})).collect();
    let next_cursor = if more {
        items.last().map(|v| {
            crate::pagination::cursor_for(
                chrono::DateTime::UNIX_EPOCH,
                v["organization_id"].as_str().unwrap(),
            )
            .encode()
        })
    } else {
        None
    };
    Ok(Json(
        serde_json::json!({"items":items,"next_cursor":next_cursor}),
    ))
}

/// `GET /organizations/{org}` — organization detail.
pub async fn get_org(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(org): Path<String>,
) -> Result<Json<crate::dto::OrganizationDto>, ManagerError> {
    let services = state
        .services()
        .ok_or_else(|| ManagerError::api("SERVICE_UNAVAILABLE", "service unavailable"))?;
    let principal = authenticated_user(&headers, &services.session_manager).await?;
    let org_id = parse_org(&org)?;
    let dto = services
        .orgs_service
        .get_org(principal.user_id, org_id)
        .await?;
    Ok(Json(dto))
}

#[derive(Deserialize)]
pub struct UpdateOrgRequest {
    pub display_name: Option<String>,
}

/// `PATCH /organizations/{org}` — update allowed settings (admin only).
pub async fn update_org(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(org): Path<String>,
    Json(body): Json<UpdateOrgRequest>,
) -> Result<Json<crate::dto::OrganizationDto>, ManagerError> {
    let services = state
        .services()
        .ok_or_else(|| ManagerError::api("SERVICE_UNAVAILABLE", "service unavailable"))?;
    let principal = authenticated_user(&headers, &services.session_manager).await?;
    require_csrf_for_browser(&principal, &headers).await?;
    let org_id = parse_org(&org)?;
    let dto = services
        .orgs_service
        .update_org(
            principal.user_id,
            org_id,
            body.display_name.as_deref(),
            &request_id(&headers),
        )
        .await?;
    Ok(Json(dto))
}

/// `GET /organizations/{org}/members` — list members.
pub async fn list_members(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(org): Path<String>,
    Query(q): Query<MembersQuery>,
) -> Result<Json<serde_json::Value>, ManagerError> {
    let services = state
        .services()
        .ok_or_else(|| ManagerError::api("SERVICE_UNAVAILABLE", "service unavailable"))?;
    let principal = authenticated_user(&headers, &services.session_manager).await?;
    let org_id = parse_org(&org)?;
    let limit = crate::pagination::PageLimit::new(q.limit);
    let items = services
        .orgs_service
        .list_members(principal.user_id, org_id, limit, q.after)
        .await?;
    Ok(Json(
        serde_json::to_value(items).map_err(|e| ManagerError::Service(e.into()))?,
    ))
}

#[derive(Deserialize)]
pub struct MembersQuery {
    pub limit: Option<u32>,
    pub after: Option<String>,
}

#[derive(Deserialize)]
pub struct UpdateMemberRequest {
    pub role: Option<String>,
    pub status: Option<String>,
}

/// `PATCH /organizations/{org}/members/{user}` — update role/status (admin only).
pub async fn update_member(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((org, user)): Path<(String, String)>,
    Json(body): Json<UpdateMemberRequest>,
) -> Result<Json<crate::dto::MembershipDto>, ManagerError> {
    let services = state
        .services()
        .ok_or_else(|| ManagerError::api("SERVICE_UNAVAILABLE", "service unavailable"))?;
    let principal = authenticated_user(&headers, &services.session_manager).await?;
    require_csrf_for_browser(&principal, &headers).await?;
    let org_id = parse_org(&org)?;
    let target = parse_user(&user)?;
    let role = body
        .role
        .as_deref()
        .map(|r| r.parse::<crate::dto::MembershipRole>())
        .transpose()
        .map_err(|_| ManagerError::InvalidInput("invalid role".into()))?;
    let status = body
        .status
        .as_deref()
        .map(|s| s.parse::<crate::dto::MembershipStatus>())
        .transpose()
        .map_err(|_| ManagerError::InvalidInput("invalid status".into()))?;
    let member = services
        .orgs_service
        .update_member(
            principal.user_id,
            org_id,
            target,
            role,
            status,
            &request_id(&headers),
        )
        .await?;
    Ok(Json(member))
}

/// `DELETE /organizations/{org}/members/{user}` — remove a member (admin only).
pub async fn remove_member(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((org, user)): Path<(String, String)>,
) -> Result<StatusCode, ManagerError> {
    let services = state
        .services()
        .ok_or_else(|| ManagerError::api("SERVICE_UNAVAILABLE", "service unavailable"))?;
    let principal = authenticated_user(&headers, &services.session_manager).await?;
    require_csrf_for_browser(&principal, &headers).await?;
    let org_id = parse_org(&org)?;
    let target = parse_user(&user)?;
    services
        .orgs_service
        .remove_member(principal.user_id, org_id, target, &request_id(&headers))
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
pub struct CreateInvitationRequest {
    pub github_login: String,
    pub role: String,
}

#[derive(Serialize)]
pub struct InvitationResponse {
    pub invitation_id: String,
    pub invitation_url: Option<String>,
    pub expires_in_seconds: i64,
}

/// `POST /organizations/{org}/invitations` — create an invitation (admin only).
pub async fn create_invitation(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(org): Path<String>,
    Json(body): Json<CreateInvitationRequest>,
) -> Result<Json<InvitationResponse>, ManagerError> {
    let services = state
        .services()
        .ok_or_else(|| ManagerError::api("SERVICE_UNAVAILABLE", "service unavailable"))?;
    let principal = authenticated_user(&headers, &services.session_manager).await?;
    require_csrf_for_browser(&principal, &headers).await?;
    let org_id = parse_org(&org)?;
    let role = body
        .role
        .parse::<crate::dto::MembershipRole>()
        .map_err(|_| ManagerError::InvalidInput("invalid role".into()))?;
    let key = idempotency_key(&headers)?;
    let (id, raw_token) = services
        .orgs_service
        .create_invitation_idempotent(
            principal.user_id,
            org_id,
            &body.github_login,
            role,
            services.github.as_ref(),
            &key,
            &request_id(&headers),
        )
        .await?;
    let public_url = services.auth_config.public_url.clone();
    Ok(Json(InvitationResponse {
        invitation_id: id.to_string(),
        invitation_url: raw_token
            .map(|token| format!("{public_url}/invitations/accept?token={token}")),
        expires_in_seconds: 7 * 24 * 3600,
    }))
}

/// `DELETE /organizations/{org}/invitations/{id}` — revoke (admin only).
pub async fn revoke_invitation(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((org, id)): Path<(String, String)>,
) -> Result<StatusCode, ManagerError> {
    let services = state
        .services()
        .ok_or_else(|| ManagerError::api("SERVICE_UNAVAILABLE", "service unavailable"))?;
    let principal = authenticated_user(&headers, &services.session_manager).await?;
    require_csrf_for_browser(&principal, &headers).await?;
    let org_id = parse_org(&org)?;
    let invitation_id = id
        .parse::<InvitationId>()
        .map_err(|_| ManagerError::not_found("invitation"))?;
    services
        .orgs_service
        .revoke_invitation(
            principal.user_id,
            org_id,
            invitation_id,
            &request_id(&headers),
        )
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
pub struct TransferRequest {
    pub new_owner_user_id: String,
}

/// `POST /organizations/{org}/transfer-ownership` — owner + recent auth.
pub async fn transfer_ownership(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(org): Path<String>,
    Json(body): Json<TransferRequest>,
) -> Result<StatusCode, ManagerError> {
    let services = state
        .services()
        .ok_or_else(|| ManagerError::api("SERVICE_UNAVAILABLE", "service unavailable"))?;
    let principal = authenticated_user(&headers, &services.session_manager).await?;
    require_csrf_for_browser(&principal, &headers).await?;
    let org_id = parse_org(&org)?;
    let new_owner = parse_user(&body.new_owner_user_id)?;
    let has_recent_auth = services
        .session_manager
        .has_recent_auth(principal.session_id)
        .await?
        .unwrap_or(false);
    services
        .orgs_service
        .transfer_ownership(
            principal.user_id,
            org_id,
            new_owner,
            has_recent_auth,
            &request_id(&headers),
        )
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `DELETE /organizations/{org}` — request deletion (owner + recent auth, 202).
pub async fn delete_org(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(org): Path<String>,
) -> Result<Response, ManagerError> {
    let services = state
        .services()
        .ok_or_else(|| ManagerError::api("SERVICE_UNAVAILABLE", "service unavailable"))?;
    let principal = authenticated_user(&headers, &services.session_manager).await?;
    require_csrf_for_browser(&principal, &headers).await?;
    let org_id = parse_org(&org)?;
    let has_recent_auth = services
        .session_manager
        .has_recent_auth(principal.session_id)
        .await?
        .unwrap_or(false);
    let key = idempotency_key(&headers)?;
    let op_id = services
        .orgs_service
        .request_deletion_idempotent(
            principal.user_id,
            org_id,
            has_recent_auth,
            &key,
            &request_id(&headers),
        )
        .await?;
    Ok((
        StatusCode::ACCEPTED,
        Json(OperationResponse {
            operation_id: op_id.to_string(),
            resource_id: org_id.to_string(),
            status: "queued".to_string(),
        }),
    )
        .into_response())
}

fn parse_org(s: &str) -> Result<OrganizationId, ManagerError> {
    s.parse::<OrganizationId>()
        .map_err(|_| ManagerError::not_found("organization"))
}

fn parse_user(s: &str) -> Result<UserId, ManagerError> {
    s.parse::<UserId>()
        .map_err(|_| ManagerError::InvalidInput("invalid user id".into()))
}

/// `GET /operations/{id}` — scoped operation status. The caller must be an
/// active member of the operation's organization.
pub async fn get_operation(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(op): Path<String>,
) -> Result<Json<serde_json::Value>, ManagerError> {
    let services = state
        .services()
        .ok_or_else(|| ManagerError::api("SERVICE_UNAVAILABLE", "service unavailable"))?;
    let principal = authenticated_user(&headers, &services.session_manager).await?;
    let op_id = op
        .parse::<crate::id::OperationId>()
        .map_err(|_| ManagerError::not_found("operation"))?;

    // Resolve the operation's organization and require the caller be an active
    // member of it before returning any status.
    let op_rec =
        crate::operations::get_for_user(&services.orgs_service.pool, op_id, principal.user_id)
            .await?
            .ok_or_else(|| ManagerError::not_found("operation"))?;

    let org_id = crate::id::OrganizationId::from_uuid(op_rec.organization_id);
    let (membership, org_state) = services
        .org_repo
        .membership_and_org(org_id, principal.user_id)
        .await?;
    Policy::require_member(membership.as_ref(), org_state.as_ref())?;

    Ok(Json(serde_json::json!({
        "id": op_id.to_string(),
        "organization_id": org_id.to_string(),
        "kind": op_rec.kind,
        "state": op_rec.state,
        "current_step": op_rec.current_step,
        "attempt_count": op_rec.attempt_count,
        "error_code": op_rec.error_code,
    })))
}

pub fn idempotency_key(headers: &HeaderMap) -> Result<String, ManagerError> {
    let key = headers
        .get("idempotency-key")
        .and_then(|v| v.to_str().ok())
        .filter(|s| !s.trim().is_empty() && s.len() <= 255)
        .ok_or_else(|| {
            ManagerError::InvalidInput("Idempotency-Key header is required (1-255 bytes)".into())
        })?;
    Ok(key.to_string())
}

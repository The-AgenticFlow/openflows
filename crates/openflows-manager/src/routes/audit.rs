//! Organization audit-log read route.
//!
//! The audit trail is written inside the transaction of every audited change;
//! this route only reads it. Only active organization admins may read it, and
//! results are always scoped to the organization in the path.

use crate::error::ManagerError;
use crate::organizations::policy::Policy;
use crate::routes::organizations::{authenticated_user, parse_org, MembersQuery};
use crate::server::AppState;
use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use axum::Json;

/// `GET /organizations/{org}/audit` — newest-first audit events (admin only).
pub async fn list_audit(
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
    let (membership, org_state) = services
        .org_repo
        .membership_and_org(org_id, principal.user_id)
        .await?;
    Policy::require_admin_view(membership.as_ref(), org_state.as_ref())?;
    let limit = crate::pagination::PageLimit::new(q.limit);
    let after = q
        .after
        .as_deref()
        .map(crate::pagination::Cursor::decode)
        .transpose()?;
    let page = crate::audit::list_for_org(services.db.pool(), org_id, limit, after).await?;
    Ok(Json(
        serde_json::to_value(page).map_err(|e| ManagerError::Service(e.into()))?,
    ))
}

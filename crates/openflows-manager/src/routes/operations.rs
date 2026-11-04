//! Organization operation history: `GET /organizations/{org}/operations`.
//!
//! Lists the organization's background operations newest first, with keyset
//! pagination (`limit`, `after`) and optional `resource_id` / `state` filters.
//! Member view, like `GET /operations/{id}`; only sanitized fields are
//! returned (stable error codes, never upstream bodies). For tenant operations
//! the tenant's slug is included so clients can label rows without a second
//! lookup.

use crate::error::ManagerError;
use crate::organizations::Policy;
use crate::pagination::{cursor_for, Cursor, Page, PageLimit};
use crate::routes::organizations::{authenticated_user, parse_org};
use crate::server::AppState;
use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use axum::Json;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

const STATES: [&str; 6] = [
    "queued",
    "running",
    "succeeded",
    "failed",
    "cancelled",
    "retryable_failed",
];

#[derive(Deserialize)]
pub struct OperationsQuery {
    pub limit: Option<u32>,
    pub after: Option<String>,
    pub resource_id: Option<uuid::Uuid>,
    pub state: Option<String>,
}

#[derive(Serialize, sqlx::FromRow)]
pub struct OperationListItem {
    pub id: uuid::Uuid,
    pub kind: String,
    pub state: String,
    pub current_step: Option<String>,
    pub attempt_count: i32,
    pub error_code: Option<String>,
    pub resource_type: String,
    pub resource_id: Option<uuid::Uuid>,
    /// Tenant slug for tenant operations; null otherwise.
    pub resource_name: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

pub async fn list_operations(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(org): Path<String>,
    Query(q): Query<OperationsQuery>,
) -> Result<Json<Page<OperationListItem>>, ManagerError> {
    let services = state
        .services()
        .ok_or_else(|| ManagerError::api("SERVICE_UNAVAILABLE", "service unavailable"))?;
    let principal = authenticated_user(&headers, &services.session_manager).await?;
    let org_id = parse_org(&org)?;
    let (membership, org_state) = services
        .org_repo
        .membership_and_org(org_id, principal.user_id)
        .await?;
    Policy::require_member(membership.as_ref(), org_state.as_ref())?;

    if let Some(s) = q.state.as_deref() {
        if !STATES.contains(&s) {
            return Err(ManagerError::InvalidInput(format!("invalid state '{s}'")));
        }
    }
    let limit = PageLimit::new(q.limit);
    let after = q.after.as_deref().map(Cursor::decode).transpose()?;
    let after_id = after
        .as_ref()
        .map(|c| c.tiebreaker.parse::<uuid::Uuid>())
        .transpose()
        .map_err(|_| ManagerError::InvalidInput("malformed cursor".into()))?;

    let rows: Vec<OperationListItem> = sqlx::query_as(
        "SELECT o.id, o.kind, o.state, o.current_step, o.attempt_count, o.error_code,
                o.resource_type, o.resource_id,
                CASE WHEN o.resource_type = 'tenant' THEN t.slug END AS resource_name,
                o.created_at, o.updated_at
           FROM operations o
           LEFT JOIN tenants t ON t.id = o.resource_id AND t.organization_id = o.organization_id
          WHERE o.organization_id = $1
            AND ($2::uuid IS NULL OR o.resource_id = $2)
            AND ($3::text IS NULL OR o.state = $3)
            AND ($4::timestamptz IS NULL OR (o.created_at, o.id) < ($4, $5::uuid))
          ORDER BY o.created_at DESC, o.id DESC
          LIMIT $6",
    )
    .bind(org_id.0)
    .bind(q.resource_id)
    .bind(q.state.as_deref())
    .bind(after.as_ref().map(|c| c.created_at))
    .bind(after_id)
    .bind(i64::from(limit.get()) + 1)
    .fetch_all(services.db.pool())
    .await?;

    Ok(Json(Page::from_fetch(rows, limit, |r| {
        cursor_for(r.created_at, r.id)
    })))
}

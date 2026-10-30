//! Platform-operator read routes.
//!
//! `GET /platform/overview` summarizes the whole fleet across organizations.
//! It is restricted to designated platform operators; ordinary organization
//! admins get 403. Counts only: no tenant names, repositories or secrets.

use crate::error::ManagerError;
use crate::routes::organizations::authenticated_user;
use crate::server::AppState;
use axum::extract::State;
use axum::http::HeaderMap;
use axum::Json;
use serde_json::json;
use sqlx::PgPool;
use std::collections::BTreeMap;

/// Count rows grouped by a status column into a `{status: count}` map.
async fn counts(pool: &PgPool, sql: &str) -> Result<BTreeMap<String, i64>, ManagerError> {
    let rows: Vec<(String, i64)> = sqlx::query_as(sql)
        .fetch_all(pool)
        .await
        .map_err(ManagerError::from)?;
    Ok(rows.into_iter().collect())
}

/// `GET /platform/overview` — fleet-wide summary (platform operator only).
pub async fn overview(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>, ManagerError> {
    let services = state
        .services()
        .ok_or_else(|| ManagerError::api("SERVICE_UNAVAILABLE", "service unavailable"))?;
    let principal = authenticated_user(&headers, &services.session_manager).await?;
    if !services
        .release_approvals
        .is_platform_operator(principal.user_id)
        .await?
    {
        return Err(ManagerError::api(
            "PLATFORM_OPERATOR_REQUIRED",
            "only a platform operator may view the platform overview",
        ));
    }
    let pool = services.db.pool();
    let organizations = counts(
        pool,
        "SELECT status, count(*) FROM organizations WHERE status <> 'deleted' GROUP BY status",
    )
    .await?;
    let tenants = counts(
        pool,
        "SELECT observed_state, count(*) FROM tenants WHERE observed_state <> 'deleted' GROUP BY observed_state",
    )
    .await?;
    let connections = counts(
        pool,
        "SELECT status, count(*) FROM github_connections GROUP BY status",
    )
    .await?;
    // Operations from the last 24 hours, plus anything still unfinished.
    let operations = counts(
        pool,
        "SELECT state, count(*) FROM operations
          WHERE created_at > now() - interval '24 hours'
             OR state IN ('queued', 'running', 'retryable_failed')
          GROUP BY state",
    )
    .await?;
    let outbox_pending: i64 =
        sqlx::query_scalar("SELECT count(*) FROM outbox_events WHERE delivered_at IS NULL")
            .fetch_one(pool)
            .await
            .map_err(ManagerError::from)?;
    let templates: Vec<(String, String, i64)> = sqlx::query_as(
        "SELECT r.version, r.status, count(t.id)
           FROM template_releases r
           LEFT JOIN tenants t ON t.pinned_release_id = r.id AND t.observed_state <> 'deleted'
          GROUP BY r.id, r.version, r.status, r.created_at
          ORDER BY r.created_at DESC
          LIMIT 20",
    )
    .fetch_all(pool)
    .await
    .map_err(ManagerError::from)?;

    let total = |m: &BTreeMap<String, i64>| m.values().sum::<i64>();
    Ok(Json(json!({
        "organizations": { "total": total(&organizations), "by_status": organizations },
        "tenants": { "total": total(&tenants), "by_state": tenants },
        "github_connections": { "total": total(&connections), "by_status": connections },
        "operations": { "by_state": operations },
        "outbox_pending": outbox_pending,
        "templates": templates
            .into_iter()
            .map(|(version, status, tenants)| json!({"version": version, "status": status, "tenants": tenants}))
            .collect::<Vec<_>>(),
    })))
}

//! Platform-operator actions for the deployment-configured template release.

use crate::error::ManagerError;
use crate::id::{OrganizationId, ReleaseId};
use crate::operations::{self, NewOperation};
use crate::provisioning::OrgProvisionWorker;
use crate::releases::ReleaseApprovalService;
use crate::routes::organizations::{authenticated_user, require_csrf_for_browser};
use crate::server::AppState;
use axum::extract::{Path, State};
use axum::http::HeaderMap;
use axum::http::StatusCode;
use axum::Json;
use serde_json::{json, Value};
use sqlx::PgPool;
use std::str::FromStr;
use std::time::Duration;

/// Promote this Manager deployment's configured release to one organization.
/// Existing organizations stay pinned until a platform operator explicitly
/// invokes this action.
pub async fn promote_configured_release(
    State(state): State<AppState>,
    Path(org): Path<String>,
    headers: HeaderMap,
) -> Result<(StatusCode, Json<Value>), ManagerError> {
    let services = state
        .services()
        .ok_or_else(|| ManagerError::api("SERVICE_UNAVAILABLE", "service unavailable"))?;
    let principal = authenticated_user(&headers, &services.session_manager).await?;
    require_csrf_for_browser(&principal, &headers).await?;

    let organization_id = OrganizationId::from_str(&org)
        .map_err(|_| ManagerError::InvalidInput("malformed organization id".into()))?;
    let worker = services.org_provision.as_ref().ok_or_else(|| {
        ManagerError::api(
            "RELEASE_NOT_CONFIGURED",
            "no configured template release is available",
        )
    })?;
    let release_id = worker.release_id.ok_or_else(|| {
        ManagerError::api(
            "RELEASE_NOT_CONFIGURED",
            "no configured template release is available",
        )
    })?;
    let _artifacts = worker.artifacts.as_ref().ok_or_else(|| {
        ManagerError::api(
            "RELEASE_NOT_CONFIGURED",
            "no configured template release artifacts are available",
        )
    })?;

    if !services
        .release_approvals
        .is_platform_operator(principal.user_id)
        .await?
    {
        return Err(ManagerError::api(
            "PLATFORM_OPERATOR_REQUIRED",
            "only a platform operator may promote platform releases",
        ));
    }

    // Serialize promotion submissions per organization across Manager replicas.
    let mut tx = services.release_approvals.pool.begin().await?;
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1::text, 0))")
        .bind(organization_id.0)
        .execute(&mut *tx)
        .await?;
    let exists: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM organizations WHERE id = $1 AND status <> 'deleted')",
    )
    .bind(organization_id.0)
    .fetch_one(&mut *tx)
    .await?;
    if !exists {
        return Err(ManagerError::not_found("organization"));
    }
    let current: Option<uuid::Uuid> = sqlx::query_scalar(
        "SELECT release_id FROM organization_current_releases WHERE organization_id = $1",
    )
    .bind(organization_id.0)
    .fetch_optional(&mut *tx)
    .await?;
    if current == Some(release_id.0) {
        return Ok((
            StatusCode::OK,
            Json(
                json!({"organization_id": organization_id, "release_id": release_id, "status": "promoted"}),
            ),
        ));
    }
    let pending: Option<(uuid::Uuid, Option<uuid::Uuid>)> = sqlx::query_as(
        "SELECT id, resource_id FROM operations WHERE organization_id = $1 AND kind = 'template.promote'
         AND (state IN ('queued', 'running') OR (state = 'retryable_failed' AND retry_at IS NOT NULL))
         ORDER BY created_at DESC LIMIT 1"
    ).bind(organization_id.0).fetch_optional(&mut *tx).await?;
    let operation_id = if let Some((id, pending_release)) = pending {
        if pending_release != Some(release_id.0) {
            return Err(ManagerError::Conflict(
                "a different template release promotion is already running".into(),
            ));
        }
        id
    } else {
        operations::create_in_tx(
            &mut tx,
            &NewOperation {
                organization_id,
                resource_type: "template_release".into(),
                resource_id: Some(release_id.0),
                kind: "template.promote".into(),
                idempotency_ref: None,
            },
        )
        .await?
        .0
    };
    tx.commit().await?;
    Ok((
        StatusCode::ACCEPTED,
        Json(json!({
            "organization_id": organization_id,
            "release_id": release_id,
            "operation_id": operation_id,
            "status": "queued"
        })),
    ))
}

/// Durable promotion worker. A restarted or different Manager replica resumes
/// an expired lease; publishing and pinning are idempotent for the same release.
pub async fn run_promotion_worker(
    pool: PgPool,
    approvals: ReleaseApprovalService,
    worker: OrgProvisionWorker,
) -> Result<(), ManagerError> {
    loop {
        if let Err(error) = promote_once(&pool, &approvals, &worker).await {
            tracing::warn!(%error, "template promotion worker attempt failed");
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}

async fn promote_once(
    pool: &PgPool,
    approvals: &ReleaseApprovalService,
    worker: &OrgProvisionWorker,
) -> Result<(), ManagerError> {
    let owner = format!(
        "template-promotion-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    );
    let Some(guard) = operations::claim_kind(pool, &owner, Some("template.promote")).await? else {
        return Ok(());
    };
    let record = operations::get_scoped(pool, guard.organization_id, guard.operation_id)
        .await?
        .ok_or_else(|| ManagerError::not_found("operation"))?;
    let release_id = record.resource_id.map(ReleaseId::from_uuid);
    if release_id != worker.release_id || worker.artifacts.is_none() {
        guard.fail(false, Some("RELEASE_NOT_CONFIGURED")).await?;
        return Ok(());
    }
    if !guard.set_step("publish_templates").await? {
        return Ok(());
    }
    let heartbeat_guard = guard.clone();
    let heartbeat = async move {
        loop {
            tokio::time::sleep(
                operations::HEARTBEAT_INTERVAL
                    .to_std()
                    .unwrap_or(Duration::from_secs(10)),
            )
            .await;
            if !heartbeat_guard.heartbeat().await? {
                return Err::<(), ManagerError>(ManagerError::Service(anyhow::anyhow!(
                    "template promotion lease lost"
                )));
            }
        }
    };
    let publish = approvals.releases.promote_to_org(
        guard.organization_id,
        release_id.expect("checked above"),
        worker.adapter.as_ref(),
        worker.artifacts.as_ref().expect("checked above"),
    );
    let result = tokio::select! { result = publish => result, result = heartbeat => result };
    match result {
        Ok(()) => {
            guard
                .complete(Some(json!({"release_id": release_id})))
                .await?;
        }
        Err(error) => {
            let safe = publication_error(error);
            let (code, retryable) = match &safe {
                ManagerError::Api(api) => (api.code.as_str(), api.retryable),
                _ => ("TEMPLATE_PROMOTION_FAILED", true),
            };
            tracing::warn!(operation_id = %guard.operation_id, %safe, "template promotion failed");
            guard.fail(retryable, Some(code)).await?;
        }
    }
    Ok(())
}

/// Preserve a useful failure category without returning Coder's raw response
/// body (which may contain credentials, Terraform values, or internal URLs).
fn publication_error(error: ManagerError) -> ManagerError {
    let request_id = crate::server::REQUEST_ID
        .try_with(Clone::clone)
        .unwrap_or_default();
    match error {
        ManagerError::Service(source) => {
            if let Some(coder) = source
                .chain()
                .find_map(|cause| cause.downcast_ref::<coder_client::CoderHttpError>())
            {
                tracing::warn!(
                    %request_id,
                    operation = coder.operation,
                    coder_status = coder.status.as_u16(),
                    "Coder rejected template publication"
                );
                let (code, message, retryable) = match coder.status.as_u16() {
                    401 => (
                        "CODER_TEMPLATE_ACCESS_DENIED",
                        "Coder rejected the Manager operator credential (HTTP 401); renew or replace the operator token".to_owned(),
                        false,
                    ),
                    403 => (
                        "CODER_TEMPLATE_ACCESS_DENIED",
                        "Coder denied template publication by the Manager operator account (HTTP 403); check its permissions".to_owned(),
                        false,
                    ),
                    404 => (
                        "CODER_TEMPLATE_API_NOT_FOUND",
                        "Coder could not find a required template API or resource (HTTP 404)"
                            .to_owned(),
                        false,
                    ),
                    status if status >= 500 || status == 429 => (
                        "CODER_UNAVAILABLE",
                        format!("Coder template API is unavailable (HTTP {status})"),
                        true,
                    ),
                    status => (
                        "CODER_TEMPLATE_REJECTED",
                        format!("Coder rejected template publication (HTTP {status})"),
                        false,
                    ),
                };
                return ManagerError::api(code, message).retryable(retryable);
            }
            if let Some(upstream) = source
                .chain()
                .find_map(|cause| cause.downcast_ref::<reqwest::Error>())
            {
                let path = upstream.url().map(|url| url.path()).unwrap_or("unknown");
                tracing::warn!(
                    %request_id,
                    coder_path = path,
                    coder_status = ?upstream.status().map(|status| status.as_u16()),
                    timeout = upstream.is_timeout(),
                    connect = upstream.is_connect(),
                    builder = upstream.is_builder(),
                    request = upstream.is_request(),
                    decode = upstream.is_decode(),
                    body = upstream.is_body(),
                    "Coder template API request failed"
                );
                if upstream.is_decode() {
                    return ManagerError::api(
                        "CODER_INVALID_RESPONSE",
                        "Coder returned a template API response that OpenFlows could not read",
                    );
                }
                if upstream.is_builder() || upstream.is_request() {
                    return ManagerError::api(
                        "CODER_REQUEST_INVALID",
                        "Manager could not construct the Coder request; check its operator credential and Coder URL",
                    );
                }
                return ManagerError::api(
                    "CODER_UNAVAILABLE",
                    "Coder template API request failed; check Coder connectivity",
                )
                .retryable(true);
            }
            tracing::error!(%request_id, "template promotion service failed");
            ManagerError::api(
                "TEMPLATE_PROMOTION_FAILED",
                "template promotion failed; inspect Manager logs using the request ID",
            )
        }
        ManagerError::Database(_) => {
            tracing::error!(%request_id, "template promotion database operation failed");
            ManagerError::api(
                "TEMPLATE_PROMOTION_DB_FAILED",
                "template promotion could not be recorded; check the control-plane database",
            )
            .retryable(true)
        }
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coder_denial_is_actionable_without_exposing_upstream_body() {
        let upstream = anyhow::Error::new(coder_client::CoderHttpError {
            operation: "upload template archive",
            status: reqwest::StatusCode::FORBIDDEN,
        });
        let error = publication_error(ManagerError::Service(upstream));
        let (status, body) = crate::error::to_http_response(&error);
        assert_eq!(status, 502);
        assert_eq!(body["error"]["code"], "CODER_TEMPLATE_ACCESS_DENIED");
        assert!(body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("permissions"));
    }

    #[test]
    fn database_failure_is_not_reported_as_generic_internal_error() {
        let error = publication_error(ManagerError::Database(sqlx::Error::RowNotFound));
        let (status, body) = crate::error::to_http_response(&error);
        assert_eq!(status, 503);
        assert_eq!(body["error"]["code"], "TEMPLATE_PROMOTION_DB_FAILED");
    }
}

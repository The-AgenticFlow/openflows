//! Tenant lifecycle routes.
//!
//! All routes resolve the caller's current membership through the policy layer
//! and the database, and every tenant lookup is organization-scoped. Mutations
//! require an Idempotency-Key and return 202 with a durable operation
//! reference; external provisioning is never awaited synchronously.

use crate::error::ManagerError;
use crate::id::TenantId;
use crate::routes::organizations::{authenticated_user, idempotency_key, parse_org, request_id};
use crate::server::AppState;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;

/// `POST /organizations/{org}/tenants` body.
#[derive(Deserialize)]
pub struct CreateTenantRequest {
    #[serde(default)]
    pub repository_url: Option<String>,
    /// Retained for API clients that already submit GitHub's immutable ID.
    /// The browser test console uses `repository_url` instead.
    #[serde(default)]
    pub repository_id: Option<i64>,
    pub name: String,
    #[serde(default)]
    pub fleet: i32,
}

/// `GET /organizations/{org}/tenants` query.
#[derive(Deserialize)]
pub struct TenantListQuery {
    pub limit: Option<u32>,
    pub after: Option<String>,
}

#[derive(Deserialize, Default)]
pub struct CleanTenantRequest {
    #[serde(default)]
    pub reset_all: bool,
}

/// Create a tenant (admin/developer) bound to an accessible repository, 202.
pub async fn create_tenant(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(org): Path<String>,
    Json(body): Json<CreateTenantRequest>,
) -> Result<Response, ManagerError> {
    let services = state
        .services()
        .ok_or_else(|| ManagerError::api("SERVICE_UNAVAILABLE", "service unavailable"))?;
    let principal = authenticated_user(&headers, &services.session_manager).await?;
    crate::routes::organizations::require_csrf_for_browser(&principal, &headers).await?;
    let org_id = parse_org(&org)?;
    let key = idempotency_key(&headers)?;
    let fleet = if body.fleet == 0 { 1 } else { body.fleet };
    let repository_id = match (body.repository_url.as_deref(), body.repository_id) {
        (Some(url), None) => {
            let repository_name = parse_github_repository_url(url)?;
            services
                .connections
                .accessible_repository_id(org_id, &repository_name)
                .await?
                .ok_or_else(|| ManagerError::not_found("accessible GitHub repository"))?
        }
        (None, Some(repository_id)) if repository_id > 0 => repository_id,
        (Some(_), Some(_)) => {
            return Err(ManagerError::InvalidInput(
                "provide either repository_url or repository_id, not both".into(),
            ));
        }
        _ => {
            return Err(ManagerError::InvalidInput(
                "repository_url is required".into(),
            ));
        }
    };

    let result = services
        .tenants_service
        .create_tenant(
            principal.user_id,
            org_id,
            repository_id,
            &body.name,
            fleet,
            &key,
            &request_id(&headers),
        )
        .await?;

    Ok((
        StatusCode::ACCEPTED,
        Json(serde_json::json!({
            "operation_id": result.operation_id.to_string(),
            "tenant_id": result.resource_id.to_string(),
            "status": result.status,
        })),
    )
        .into_response())
}

fn parse_github_repository_url(value: &str) -> Result<String, ManagerError> {
    let url = url::Url::parse(value.trim())
        .map_err(|_| ManagerError::InvalidInput("enter a valid GitHub repository URL".into()))?;
    if url.scheme() != "https"
        || url.host_str() != Some("github.com")
        || !url.username().is_empty()
        || url.password().is_some()
        || url.port().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(ManagerError::InvalidInput(
            "repository URL must use https://github.com/OWNER/REPO".into(),
        ));
    }

    let segments: Vec<_> = url
        .path_segments()
        .ok_or_else(|| ManagerError::InvalidInput("invalid GitHub repository URL".into()))?
        .filter(|segment| !segment.is_empty())
        .collect();
    if segments.len() != 2 {
        return Err(ManagerError::InvalidInput(
            "repository URL must contain exactly an owner and repository".into(),
        ));
    }
    let owner = segments[0];
    let repository = segments[1].strip_suffix(".git").unwrap_or(segments[1]);
    if owner.is_empty()
        || repository.is_empty()
        || owner.contains('%')
        || repository.contains('%')
        || owner == "."
        || owner == ".."
        || repository == "."
        || repository == ".."
    {
        return Err(ManagerError::InvalidInput(
            "invalid GitHub repository URL".into(),
        ));
    }
    Ok(format!("{owner}/{repository}"))
}

/// `GET /organizations/{org}/tenants` — list the org's tenants (any active
/// member).
pub async fn list_tenants(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(org): Path<String>,
    Query(q): Query<TenantListQuery>,
) -> Result<Json<serde_json::Value>, ManagerError> {
    let services = state
        .services()
        .ok_or_else(|| ManagerError::api("SERVICE_UNAVAILABLE", "service unavailable"))?;
    let principal = authenticated_user(&headers, &services.session_manager).await?;
    let org_id = parse_org(&org)?;
    let limit = crate::pagination::PageLimit::new(q.limit);
    let page = services
        .tenants_service
        .list_tenants(principal.user_id, org_id, limit, q.after)
        .await?;
    Ok(Json(
        serde_json::to_value(page).map_err(|e| ManagerError::Service(e.into()))?,
    ))
}

/// `GET /organizations/{org}/tenants/{tenant}` — tenant detail (any active
/// member).
pub async fn get_tenant(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((org, tenant)): Path<(String, String)>,
) -> Result<Json<crate::dto::TenantDto>, ManagerError> {
    let services = state
        .services()
        .ok_or_else(|| ManagerError::api("SERVICE_UNAVAILABLE", "service unavailable"))?;
    let principal = authenticated_user(&headers, &services.session_manager).await?;
    let org_id = parse_org(&org)?;
    let tenant_id = parse_tenant(&tenant)?;
    let dto = services
        .tenants_service
        .get_tenant(principal.user_id, org_id, tenant_id)
        .await?;
    Ok(Json(dto))
}

/// `DELETE /organizations/{org}/tenants/{tenant}` — delete a tenant (admin,
/// 202). Revokes runtime issuance and enqueues durable cleanup.
pub async fn delete_tenant(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((org, tenant)): Path<(String, String)>,
) -> Result<Response, ManagerError> {
    let services = state
        .services()
        .ok_or_else(|| ManagerError::api("SERVICE_UNAVAILABLE", "service unavailable"))?;
    let principal = authenticated_user(&headers, &services.session_manager).await?;
    crate::routes::organizations::require_csrf_for_browser(&principal, &headers).await?;
    let org_id = parse_org(&org)?;
    let tenant_id = parse_tenant(&tenant)?;
    let key = idempotency_key(&headers)?;
    let op_id = services
        .tenants_service
        .delete_tenant(
            principal.user_id,
            org_id,
            tenant_id,
            &key,
            &request_id(&headers),
        )
        .await?;
    Ok((
        StatusCode::ACCEPTED,
        Json(serde_json::json!({
            "operation_id": op_id.to_string(),
            "tenant_id": tenant_id.to_string(),
            "status": "queued",
        })),
    )
        .into_response())
}

pub async fn start_tenant(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((org, tenant)): Path<(String, String)>,
) -> Result<Response, ManagerError> {
    tenant_action(state, headers, org, tenant, "start", false).await
}

pub async fn stop_tenant(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((org, tenant)): Path<(String, String)>,
) -> Result<Response, ManagerError> {
    tenant_action(state, headers, org, tenant, "stop", false).await
}

pub async fn clean_tenant(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((org, tenant)): Path<(String, String)>,
    Json(body): Json<CleanTenantRequest>,
) -> Result<Response, ManagerError> {
    tenant_action(state, headers, org, tenant, "clean", body.reset_all).await
}

async fn tenant_action(
    state: AppState,
    headers: HeaderMap,
    org: String,
    tenant: String,
    action: &str,
    reset_all: bool,
) -> Result<Response, ManagerError> {
    let services = state
        .services()
        .ok_or_else(|| ManagerError::api("SERVICE_UNAVAILABLE", "service unavailable"))?;
    let principal = authenticated_user(&headers, &services.session_manager).await?;
    crate::routes::organizations::require_csrf_for_browser(&principal, &headers).await?;
    let org_id = parse_org(&org)?;
    let tenant_id = parse_tenant(&tenant)?;
    let key = idempotency_key(&headers)?;
    let request_id = request_id(&headers);
    let operation_id = match action {
        "start" => {
            services
                .tenants_service
                .start_tenant(principal.user_id, org_id, tenant_id, &key, &request_id)
                .await?
        }
        "stop" => {
            services
                .tenants_service
                .stop_tenant(principal.user_id, org_id, tenant_id, &key, &request_id)
                .await?
        }
        "clean" => {
            services
                .tenants_service
                .clean_tenant(
                    principal.user_id,
                    org_id,
                    tenant_id,
                    reset_all,
                    &key,
                    &request_id,
                )
                .await?
        }
        _ => return Err(ManagerError::InvalidInput("invalid tenant action".into())),
    };
    Ok((
        StatusCode::ACCEPTED,
        Json(serde_json::json!({
            "operation_id": operation_id,
            "tenant_id": tenant_id,
            "status": "queued",
        })),
    )
        .into_response())
}

pub async fn tenant_status(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((org, tenant)): Path<(String, String)>,
) -> Result<Json<serde_json::Value>, ManagerError> {
    let services = state
        .services()
        .ok_or_else(|| ManagerError::api("SERVICE_UNAVAILABLE", "service unavailable"))?;
    let principal = authenticated_user(&headers, &services.session_manager).await?;
    let value = services
        .tenants_service
        .tenant_status(principal.user_id, parse_org(&org)?, parse_tenant(&tenant)?)
        .await?;
    Ok(Json(value))
}

fn parse_tenant(s: &str) -> Result<TenantId, ManagerError> {
    s.parse::<TenantId>()
        .map_err(|_| ManagerError::not_found("tenant"))
}

#[cfg(test)]
mod tests {
    use super::parse_github_repository_url;

    #[test]
    fn parses_github_repository_urls() {
        for (url, expected) in [
            ("https://github.com/Acme/project", "Acme/project"),
            ("https://github.com/Acme/project/", "Acme/project"),
            ("https://github.com/Acme/project.git", "Acme/project"),
        ] {
            assert_eq!(parse_github_repository_url(url).unwrap(), expected);
        }
    }

    #[test]
    fn rejects_urls_that_are_not_canonical_github_repository_urls() {
        for url in [
            "http://github.com/acme/project",
            "https://evil.example/acme/project",
            "https://github.com/acme/project/issues",
            "https://github.com/acme/project?tab=readme",
            "https://github.com/acme",
        ] {
            assert!(parse_github_repository_url(url).is_err(), "accepted {url}");
        }
    }
}

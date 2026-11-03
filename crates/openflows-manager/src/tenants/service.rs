//! Tenant lifecycle service implementation.

use crate::audit::{self, AuditEvent, AuditResult};
use crate::dto::TenantDto;
use crate::error::ManagerError;
use crate::id::{OperationId, OrganizationId, TenantId, UserId};
use crate::idempotency::{IdempotencyOutcome, IdempotencyService};
use crate::operations;
use crate::organizations::policy::Policy;
use crate::organizations::repository::OrganizationRepository;
use crate::pagination::{Cursor, PageLimit};
use crate::quotas::TenantQuotaService;
use crate::repositories::TenantsRepository;
use serde_json::json;
use sqlx::PgPool;
use std::str::FromStr;

/// The route string used for tenant-create idempotency records.
pub const TENANT_CREATE_ROUTE: &str = "/api/v1/organizations/{org}/tenants";
/// The route string used for tenant-delete idempotency records.
pub const TENANT_DELETE_ROUTE: &str = "/api/v1/organizations/{org}/tenants/{tenant}";
pub const TENANT_ACTION_ROUTE: &str = "/api/v1/organizations/{org}/tenants/{tenant}/{action}";

/// The response body for a durable tenant operation (202).
#[derive(Debug, Clone, serde::Serialize)]
pub struct TenantOperationResponse {
    pub operation_id: OperationId,
    pub resource_id: TenantId,
    pub status: &'static str,
}

/// Inputs shared by the tenant service (constructed once per request).
#[derive(Clone)]
pub struct TenantServiceInputs {
    pub pool: PgPool,
    pub tenants: TenantsRepository,
    pub org_repo: OrganizationRepository,
    pub idempotency: IdempotencyService,
    pub quotas: TenantQuotaService,
}

/// Validate a tenant slug (name). Lowercase letters, digits, hyphens; 1-63
/// chars; starts/ends alphanumeric.
pub fn validate_tenant_slug(slug: &str) -> Result<(), ManagerError> {
    let ok = !slug.is_empty()
        && slug.len() <= 63
        && slug
            .bytes()
            .next()
            .map(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
            == Some(true)
        && slug
            .bytes()
            .last()
            .map(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
            == Some(true)
        && slug
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
    if ok {
        Ok(())
    } else {
        Err(ManagerError::InvalidInput(
            "tenant name must be 1-63 chars of lowercase letters, digits, or hyphens, and start/end alphanumeric".into(),
        ))
    }
}

impl TenantServiceInputs {
    pub fn new(
        pool: PgPool,
        tenants: TenantsRepository,
        org_repo: OrganizationRepository,
        idempotency: IdempotencyService,
        quotas: TenantQuotaService,
    ) -> Self {
        TenantServiceInputs {
            pool,
            tenants,
            org_repo,
            idempotency,
            quotas,
        }
    }
}

/// The tenant lifecycle service.
#[derive(Clone)]
pub struct TenantService {
    pub inputs: TenantServiceInputs,
}

impl TenantService {
    pub fn new(inputs: TenantServiceInputs) -> Self {
        TenantService { inputs }
    }

    /// Create a tenant (admin/developer), bind it to an accessible immutable
    /// repository ID, reserve tenant capacity against the org quota, and enqueue
    /// a durable provisioning operation. Returns 202 with the operation and
    /// tenant IDs.
    #[allow(clippy::too_many_arguments)]
    pub async fn create_tenant(
        &self,
        actor: UserId,
        org_id: OrganizationId,
        repository_id: i64,
        slug: &str,
        fleet: i32,
        idempotency_key: &str,
        request_id: &str,
    ) -> Result<TenantOperationResponse, ManagerError> {
        validate_tenant_slug(slug)?;
        if fleet < 1 {
            return Err(ManagerError::InvalidInput(
                "fleet must be at least 1 worker".into(),
            ));
        }

        // Resolve current membership + org status (policy requires admin or
        // developer). A viewer or a non-member sees 404/403.
        let (membership, org_state) = self
            .inputs
            .org_repo
            .membership_and_org(org_id, actor)
            .await?;
        Policy::require_developer(membership.as_ref(), org_state.as_ref())?;

        // Bind values so the idempotency closure can capture them by value.
        let slug = slug.to_string();
        let idempotency_key = idempotency_key.to_string();
        let request_id = request_id.to_string();
        let route = TENANT_CREATE_ROUTE.to_string();
        let request_hash = crate::idempotency::IdempotencyRequest::hash_request(&json!({
            "repository_id": repository_id,
            "slug": slug,
            "fleet": fleet,
        }))
        .map_err(|_| ManagerError::InvalidInput("cannot hash request".into()))?;

        let tenant_id = TenantId::new();
        let tenants = self.inputs.tenants.clone();
        let quotas = self.inputs.quotas.clone();
        let outcome = self
            .inputs
            .idempotency
            .execute(
                &crate::idempotency::IdempotencyRequest {
                    actor_id: actor,
                    organization_id: org_id,
                    route,
                    key: idempotency_key.clone(),
                    request_hash,
                },
                move |conn| {
                    let slug = slug.clone();
                    let idempotency_key = idempotency_key.clone();
                    let request_id = request_id.clone();
                    let org_id = org_id;
                    let tenants = tenants.clone();
                    let quotas = quotas.clone();
                    Box::pin(async move {
                        // Revalidate org readiness and repository access inside
                        // the transaction, with the org row locked.
                        let scope = crate::db::tx::OrgScope::new(org_id);
                        let (status,): (String,) = sqlx::query_as(
                            "SELECT status FROM organizations WHERE id = $1 FOR UPDATE",
                        )
                        .bind(org_id.0)
                        .fetch_one(&mut *conn)
                        .await
                        .map_err(ManagerError::from)?;
                        if status != "ready" {
                            return Err(ManagerError::api(
                                "ORG_UNAVAILABLE",
                                "organization is not ready for tenant creation",
                            ));
                        }

                        // Resolve the connection for the repository and verify
                        // the repository is accessible via an active connection
                        // in this organization. Never trusts a caller-provided
                        // connection id.
                        let connection_id: Option<(uuid::Uuid,)> = sqlx::query_as(
                            "SELECT c.id
                               FROM github_repositories r
                               JOIN github_connections c
                                 ON c.id = r.connection_id AND c.organization_id = r.organization_id
                              WHERE r.organization_id = $1
                                AND r.github_repository_id = $2
                                AND r.accessible = true
                                AND c.status = 'active'",
                        )
                        .bind(org_id.0)
                        .bind(repository_id)
                        .fetch_optional(&mut *conn)
                        .await
                        .map_err(ManagerError::from)?;
                        let connection_id = connection_id
                            .ok_or_else(|| ManagerError::not_found("repository"))?
                            .0;

                        // Enforce the org tenant ceiling transactionally.
                        quotas.ensure_org_row_in_tx(conn, org_id).await?;
                        quotas.check_and_reserve_tenant_in_tx(conn, org_id).await?;

                        // Insert the tenant (desired=provisioning) and enqueue
                        // the durable provisioning operation. The provisioning
                        // slot is checked by the worker; if occupied the
                        // operation stays queued (concurrency, not rejection).
                        let inserted_id = tenants
                            .create_with_id_in_tx(
                                conn,
                                tenant_id,
                                scope,
                                &slug,
                                crate::id::ConnectionId::from_uuid(connection_id),
                                crate::id::GithubId(repository_id),
                                fleet,
                            )
                            .await?;
                        debug_assert_eq!(inserted_id, tenant_id);
                        let linked: bool = sqlx::query_scalar(
                            "SELECT EXISTS(SELECT 1 FROM coder_user_links WHERE user_id = $1)",
                        )
                        .bind(actor.0)
                        .fetch_one(&mut *conn)
                        .await?;
                        if !linked {
                            return Err(ManagerError::api(
                                "CODER_ACCOUNT_LINK_REQUIRED",
                                "Connect your GitHub-backed Coder account before creating a tenant",
                            ));
                        }
                        sqlx::query("UPDATE tenants SET owner_user_id = $1 WHERE id = $2")
                            .bind(actor.0)
                            .bind(tenant_id.0)
                            .execute(&mut *conn)
                            .await?;

                        let op_id = operations::create_in_tx(
                            conn,
                            &operations::NewOperation {
                                organization_id: org_id,
                                resource_type: "tenant".into(),
                                resource_id: Some(tenant_id.0),
                                kind: "tenant.provision".into(),
                                idempotency_ref: Some(idempotency_key.to_string()),
                            },
                        )
                        .await?;

                        audit::insert(
                            &mut *conn,
                            &AuditEvent::new("tenant.create")
                                .organization(org_id)
                                .actor(actor)
                                .resource("tenant", tenant_id.0)
                                .result(AuditResult::Success)
                                .request_id(request_id),
                        )
                        .await?;

                        Ok(format!("{}:{}:{}", tenant_id, op_id, "queued"))
                    })
                },
            )
            .await?;

        match outcome {
            IdempotencyOutcome::New { response_reference } => {
                Ok(parse_reference(&response_reference))
            }
            IdempotencyOutcome::Replay { response_reference } => {
                Ok(parse_reference(&response_reference))
            }
        }
    }

    /// List tenants in the organization, paginated with opaque cursors.
    pub async fn list_tenants(
        &self,
        actor: UserId,
        org_id: OrganizationId,
        limit: PageLimit,
        after: Option<String>,
    ) -> Result<crate::dto::CursorPage<TenantDto>, ManagerError> {
        let (membership, org_state) = self
            .inputs
            .org_repo
            .membership_and_org(org_id, actor)
            .await?;
        Policy::require_member(membership.as_ref(), org_state.as_ref())?;

        let cursor = match after {
            None => None,
            Some(a) => {
                let c = Cursor::decode(&a)
                    .map_err(|_| ManagerError::InvalidInput("malformed cursor".into()))?;
                Some(c)
            }
        };
        let scope = crate::db::tx::OrgScope::new(org_id);
        let page = self
            .inputs
            .tenants
            .list_scoped(scope, limit, cursor.as_ref())
            .await?;
        let items: Vec<TenantDto> = page.items.iter().map(TenantsRepository::to_dto).collect();
        Ok(crate::dto::CursorPage {
            items,
            next_cursor: page.next_cursor,
        })
    }

    /// Get a single tenant's detail, scoped to the organization.
    pub async fn get_tenant(
        &self,
        actor: UserId,
        org_id: OrganizationId,
        tenant_id: TenantId,
    ) -> Result<TenantDto, ManagerError> {
        let (membership, org_state) = self
            .inputs
            .org_repo
            .membership_and_org(org_id, actor)
            .await?;
        Policy::require_member(membership.as_ref(), org_state.as_ref())?;
        let scope = crate::db::tx::OrgScope::new(org_id);
        let row = self
            .inputs
            .tenants
            .get_scoped(scope, tenant_id)
            .await?
            .ok_or_else(|| ManagerError::not_found("tenant"))?;
        Ok(TenantsRepository::to_dto(&row))
    }

    /// Delete a tenant (admin only, 202). Records the desired `deleting` state,
    /// revokes runtime issuance, and enqueues a durable cleanup operation. The
    /// worker reconciles workspace and tenant resource cleanup safely.
    pub async fn delete_tenant(
        &self,
        actor: UserId,
        org_id: OrganizationId,
        tenant_id: TenantId,
        idempotency_key: &str,
        request_id: &str,
    ) -> Result<OperationId, ManagerError> {
        let (membership, org_state) = self
            .inputs
            .org_repo
            .membership_and_org(org_id, actor)
            .await?;
        Policy::require_admin(membership.as_ref(), org_state.as_ref())?;

        // Confirm the tenant exists and is in scope before idempotent mutation.
        let scope = crate::db::tx::OrgScope::new(org_id);
        self.inputs
            .tenants
            .get_scoped(scope, tenant_id)
            .await?
            .ok_or_else(|| ManagerError::not_found("tenant"))?;

        let idempotency_key = idempotency_key.to_string();
        let request_id = request_id.to_string();
        let route = TENANT_DELETE_ROUTE.to_string();
        let request_hash = crate::idempotency::IdempotencyRequest::hash_request(&json!({
            "tenant_id": tenant_id,
        }))
        .map_err(|_| ManagerError::InvalidInput("cannot hash request".into()))?;

        let outcome = self
            .inputs
            .idempotency
            .execute(
                &crate::idempotency::IdempotencyRequest {
                    actor_id: actor,
                    organization_id: org_id,
                    route,
                    key: idempotency_key.clone(),
                    request_hash,
                },
                |conn| {
                    let request_id = request_id.clone();
                    Box::pin(async move {
                        // Lock the tenant row and verify it is still present and
                        // in scope, then set the desired state to deleting and
                        // revoke runtime issuance. Revocation happens before any
                        // upstream cleanup so no new access can be issued.
                        let (current,): (String,) = sqlx::query_as(
                            "SELECT desired_state FROM tenants
                              WHERE id = $1 AND organization_id = $2 FOR UPDATE",
                        )
                        .bind(tenant_id.0)
                        .bind(org_id.0)
                        .fetch_one(&mut *conn)
                        .await
                        .map_err(|_| ManagerError::not_found("tenant"))?;
                        if current == "deleted" {
                            return Err(ManagerError::Conflict(
                                "tenant is already deleted".into(),
                            ));
                        }
                        if current == "deleting" {
                            // A second request must follow the original
                            // cleanup, not launch another Coder delete build.
                            let existing: Option<uuid::Uuid> = sqlx::query_scalar(
                                "SELECT id FROM operations
                                  WHERE organization_id = $1 AND resource_id = $2
                                    AND kind = 'tenant.delete'
                                    AND state IN ('queued', 'running', 'retryable_failed')
                                  ORDER BY created_at DESC LIMIT 1",
                            )
                            .bind(org_id.0)
                            .bind(tenant_id.0)
                            .fetch_optional(&mut *conn)
                            .await?;
                            if let Some(id) = existing {
                                return Ok(id.to_string());
                            }
                        }

                        sqlx::query(
                            "UPDATE tenants SET desired_state = 'deleting', updated_at = now()
                              WHERE id = $1 AND organization_id = $2",
                        )
                        .bind(tenant_id.0)
                        .bind(org_id.0)
                        .execute(&mut *conn)
                        .await?;

                        // Revoke runtime issuance now: workspace credentials,
                        // credential leases, tenant Redis credentials, and the
                        // tenant hook key. Nothing here relies on a workspace
                        // rebuild to revoke access.
                        sqlx::query(
                            "UPDATE runtime_credentials SET revoked_at = clock_timestamp()
                              WHERE workspace_id IN (SELECT id FROM workspaces WHERE tenant_id = $1)
                                AND revoked_at IS NULL",
                        )
                        .bind(tenant_id.0)
                        .execute(&mut *conn)
                        .await?;
                        sqlx::query(
                            "UPDATE credential_leases SET revoked_at = clock_timestamp(), encrypted_ref = NULL
                              WHERE tenant_id = $1 AND revoked_at IS NULL",
                        )
                        .bind(tenant_id.0)
                        .execute(&mut *conn)
                        .await?;
                        sqlx::query(
                            "UPDATE tenant_redis_credentials SET revoked_at = clock_timestamp(), updated_at = now()
                              WHERE tenant_id = $1 AND revoked_at IS NULL",
                        )
                        .bind(tenant_id.0)
                        .execute(&mut *conn)
                        .await?;
                        sqlx::query(
                            "UPDATE tenant_hook_keys SET revoked_at = clock_timestamp()
                              WHERE tenant_id = $1 AND revoked_at IS NULL",
                        )
                        .bind(tenant_id.0)
                        .execute(&mut *conn)
                        .await?;

                        let op_id = operations::create_in_tx(
                            conn,
                            &operations::NewOperation {
                                organization_id: org_id,
                                resource_type: "tenant".into(),
                                resource_id: Some(tenant_id.0),
                                kind: "tenant.delete".into(),
                                idempotency_ref: Some(idempotency_key.to_string()),
                            },
                        )
                        .await?;

                        audit::insert(
                            &mut *conn,
                            &AuditEvent::new("tenant.delete")
                                .organization(org_id)
                                .actor(actor)
                                .resource("tenant", tenant_id.0)
                                .result(AuditResult::Success)
                                .request_id(request_id),
                        )
                        .await?;

                        Ok(op_id.to_string())
                    })
                },
            )
            .await?;

        let op_id = match outcome {
            IdempotencyOutcome::New { response_reference }
            | IdempotencyOutcome::Replay { response_reference } => response_reference,
        };
        OperationId::from_str(&op_id)
            .map_err(|_| ManagerError::Conflict("stored operation reference is invalid".into()))
    }

    pub async fn start_tenant(
        &self,
        actor: UserId,
        org_id: OrganizationId,
        tenant_id: TenantId,
        idempotency_key: &str,
        request_id: &str,
    ) -> Result<OperationId, ManagerError> {
        self.enqueue_action(
            actor,
            org_id,
            tenant_id,
            "start",
            false,
            idempotency_key,
            request_id,
        )
        .await
    }

    pub async fn stop_tenant(
        &self,
        actor: UserId,
        org_id: OrganizationId,
        tenant_id: TenantId,
        idempotency_key: &str,
        request_id: &str,
    ) -> Result<OperationId, ManagerError> {
        self.enqueue_action(
            actor,
            org_id,
            tenant_id,
            "stop",
            false,
            idempotency_key,
            request_id,
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn clean_tenant(
        &self,
        actor: UserId,
        org_id: OrganizationId,
        tenant_id: TenantId,
        reset_all: bool,
        idempotency_key: &str,
        request_id: &str,
    ) -> Result<OperationId, ManagerError> {
        self.enqueue_action(
            actor,
            org_id,
            tenant_id,
            if reset_all { "clean-all" } else { "clean" },
            true,
            idempotency_key,
            request_id,
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    async fn enqueue_action(
        &self,
        actor: UserId,
        org_id: OrganizationId,
        tenant_id: TenantId,
        action: &'static str,
        admin_only: bool,
        idempotency_key: &str,
        request_id: &str,
    ) -> Result<OperationId, ManagerError> {
        let (membership, org_state) = self
            .inputs
            .org_repo
            .membership_and_org(org_id, actor)
            .await?;
        if admin_only {
            Policy::require_admin(membership.as_ref(), org_state.as_ref())?;
        } else {
            Policy::require_developer(membership.as_ref(), org_state.as_ref())?;
        }

        let request_hash = crate::idempotency::IdempotencyRequest::hash_request(&json!({
            "tenant_id": tenant_id,
            "action": action,
        }))
        .map_err(|_| ManagerError::InvalidInput("cannot hash request".into()))?;
        let key = idempotency_key.to_string();
        let request_id = request_id.to_string();
        let route = TENANT_ACTION_ROUTE.replace("{action}", action);
        let outcome = self
            .inputs
            .idempotency
            .execute(
                &crate::idempotency::IdempotencyRequest {
                    actor_id: actor,
                    organization_id: org_id,
                    route,
                    key: key.clone(),
                    request_hash,
                },
                |conn| {
                    let request_id = request_id.clone();
                    Box::pin(async move {
                        let (desired_state, observed_state): (String, String) = sqlx::query_as(
                            "SELECT desired_state, observed_state FROM tenants
                              WHERE id = $1 AND organization_id = $2 FOR UPDATE",
                        )
                        .bind(tenant_id.0)
                        .bind(org_id.0)
                        .fetch_optional(&mut *conn)
                        .await?
                        .ok_or_else(|| ManagerError::not_found("tenant"))?;
                        if matches!(desired_state.as_str(), "deleting" | "deleted") {
                            return Err(ManagerError::Conflict(
                                "tenant is being deleted".into(),
                            ));
                        }

                        let kind = match action {
                            "start" => {
                                if desired_state == "provisioning" || observed_state == "provisioning" {
                                    return Err(ManagerError::Conflict(
                                        "tenant provisioning is still in progress".into(),
                                    ));
                                }
                                let accessible: bool = sqlx::query_scalar(
                                    "SELECT EXISTS (
                                        SELECT 1 FROM tenants t
                                        JOIN github_connections c ON c.id = t.connection_id
                                        JOIN github_repositories r
                                          ON r.connection_id = c.id
                                         AND r.organization_id = t.organization_id
                                         AND r.github_repository_id = t.github_repository_id
                                       WHERE t.id = $1 AND t.organization_id = $2
                                         AND c.status = 'active' AND r.accessible = true)",
                                )
                                .bind(tenant_id.0)
                                .bind(org_id.0)
                                .fetch_one(&mut *conn)
                                .await?;
                                if !accessible {
                                    return Err(ManagerError::api(
                                        "REPOSITORY_UNAVAILABLE",
                                        "repository access must be restored before starting",
                                    ));
                                }
                                sqlx::query("UPDATE tenants SET desired_state = 'running', updated_at = now() WHERE id = $1")
                                    .bind(tenant_id.0).execute(&mut *conn).await?;
                                sqlx::query("UPDATE workspaces SET desired_state = 'running', updated_at = now() WHERE tenant_id = $1 AND desired_state = 'stopped'")
                                    .bind(tenant_id.0).execute(&mut *conn).await?;
                                "tenant.start"
                            }
                            "stop" => {
                                sqlx::query("UPDATE tenants SET desired_state = 'paused', updated_at = now() WHERE id = $1")
                                    .bind(tenant_id.0).execute(&mut *conn).await?;
                                sqlx::query("UPDATE workspaces SET desired_state = 'stopped', updated_at = now() WHERE tenant_id = $1 AND desired_state <> 'deleted'")
                                    .bind(tenant_id.0).execute(&mut *conn).await?;
                                sqlx::query("UPDATE runtime_credentials SET revoked_at = clock_timestamp() WHERE workspace_id IN (SELECT id FROM workspaces WHERE tenant_id = $1) AND revoked_at IS NULL")
                                    .bind(tenant_id.0).execute(&mut *conn).await?;
                                "tenant.stop"
                            }
                            "clean" => "tenant.clean",
                            "clean-all" => "tenant.clean_all",
                            _ => return Err(ManagerError::InvalidInput("invalid action".into())),
                        };
                        let op_id = operations::create_in_tx(
                            conn,
                            &operations::NewOperation {
                                organization_id: org_id,
                                resource_type: "tenant".into(),
                                resource_id: Some(tenant_id.0),
                                kind: kind.into(),
                                idempotency_ref: Some(key.clone()),
                            },
                        )
                        .await?;
                        audit::insert(
                            &mut *conn,
                            &AuditEvent::new(kind)
                                .organization(org_id)
                                .actor(actor)
                                .resource("tenant", tenant_id.0)
                                .result(AuditResult::Success)
                                .request_id(request_id),
                        )
                        .await?;
                        Ok(op_id.to_string())
                    })
                },
            )
            .await?;
        let reference = match outcome {
            IdempotencyOutcome::New { response_reference }
            | IdempotencyOutcome::Replay { response_reference } => response_reference,
        };
        OperationId::from_str(&reference)
            .map_err(|_| ManagerError::Conflict("stored operation reference is invalid".into()))
    }

    pub async fn tenant_status(
        &self,
        actor: UserId,
        org_id: OrganizationId,
        tenant_id: TenantId,
    ) -> Result<serde_json::Value, ManagerError> {
        self.get_tenant(actor, org_id, tenant_id).await?;
        let rows: Vec<(String, i64)> = sqlx::query_as(
            "SELECT observed_state, count(*) FROM workspaces
              WHERE organization_id = $1 AND tenant_id = $2
              GROUP BY observed_state ORDER BY observed_state",
        )
        .bind(org_id.0)
        .bind(tenant_id.0)
        .fetch_all(&self.inputs.pool)
        .await?;
        // Follow unfinished work first: a newer cancelled action must not hide
        // an older provisioning operation that can still run or be retried.
        let latest: Option<(uuid::Uuid, String, Option<String>)> = sqlx::query_as(
            "SELECT id, state, error_code FROM operations
              WHERE organization_id = $1 AND resource_id = $2
              ORDER BY CASE WHEN state IN ('queued', 'running', 'retryable_failed')
                            THEN 0 ELSE 1 END,
                       created_at DESC, id DESC LIMIT 1",
        )
        .bind(org_id.0)
        .bind(tenant_id.0)
        .fetch_optional(&self.inputs.pool)
        .await?;
        Ok(json!({
            "tenant_id": tenant_id,
            "workspaces": rows.into_iter().map(|(state, count)| json!({"state": state, "count": count})).collect::<Vec<_>>(),
            "latest_operation": latest.map(|(id, state, error_code)| json!({"id": id, "state": state, "error_code": error_code})),
        }))
    }
}

/// Parse a `tenant_id:operation_id:status` reference back into a response.
fn parse_reference(reference: &str) -> TenantOperationResponse {
    let mut parts = reference.split(':');
    let tenant = parts.next().unwrap_or_default();
    let operation = parts.next().unwrap_or_default();
    TenantOperationResponse {
        operation_id: OperationId::from_str(operation).unwrap_or_default(),
        resource_id: TenantId::from_str(tenant).unwrap_or_default(),
        status: "queued",
    }
}

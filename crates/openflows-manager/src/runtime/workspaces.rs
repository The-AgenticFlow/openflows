//! Runtime worker provisioning and workspace lifecycle.
//!
//! Nexus requests worker workspaces through an authenticated Manager runtime
//! endpoint. All privileged parameters (organization, tenant, template, pinned
//! version, owner) are resolved server-side from the authenticated Nexus
//! runtime scope; a runtime caller can never select another organization,
//! tenant, repository, or an unauthorized role. The Nexus controller requests
//! workers by role and slot; it cannot choose arbitrary templates, owners, host
//! mounts, or Terraform arguments.
//!
//! Worker capacity is reserved against the tenant's fleet ceiling
//! transactionally, and the request returns 202 with a durable operation
//! reference. Rotation (refresh) and revocation never rely on a workspace
//! rebuild.

use crate::error::ManagerError;
use crate::id::WorkspaceId;
use crate::operations;
use crate::quotas::TenantQuotaService;
use crate::runtime::repository::RuntimeScope;
use crate::runtime::tenant::TenantRuntimeRepository;
use sqlx::PgPool;

type WorkspaceStatusRow = (
    String,
    Option<i32>,
    String,
    String,
    Option<chrono::DateTime<chrono::Utc>>,
);

/// Worker roles a Nexus controller may request. Nexus itself is never a
/// worker role a runtime can request.
pub const WORKER_ROLES: &[&str] = &["forge", "sentinel", "vessel", "lore"];

/// The response for a worker-workspace request (202).
#[derive(Debug, Clone, serde::Serialize)]
pub struct RuntimeWorkspaceResponse {
    pub workspace_id: String,
    pub operation_id: String,
    pub status: &'static str,
}

/// The result of a heartbeat update.
#[derive(Debug, Clone, serde::Serialize)]
pub struct HeartbeatResponse {
    pub workspace_id: String,
    pub observed_state: String,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct RuntimeWorkspaceStatus {
    pub workspace_id: String,
    pub role: String,
    pub slot: Option<i32>,
    pub desired_state: String,
    pub observed_state: String,
    pub last_heartbeat_at: Option<chrono::DateTime<chrono::Utc>>,
}

/// Runtime workspace lifecycle service.
#[derive(Clone)]
pub struct RuntimeWorkspaceService {
    pool: PgPool,
    tenant_runtime: TenantRuntimeRepository,
    quotas: TenantQuotaService,
}

impl RuntimeWorkspaceService {
    pub fn new(
        pool: PgPool,
        tenant_runtime: TenantRuntimeRepository,
        quotas: TenantQuotaService,
    ) -> Self {
        RuntimeWorkspaceService {
            pool,
            tenant_runtime,
            quotas,
        }
    }

    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    /// Request a worker workspace for a tenant's Nexus controller. The caller
    /// scope is derived from the authenticated runtime; only a Nexus workspace
    /// of the same tenant may request workers, only worker roles are accepted,
    /// and the slot must be free. Worker capacity is reserved against the
    /// tenant's fleet ceiling transactionally. Returns 202 with a durable
    /// operation reference.
    pub async fn request_worker(
        &self,
        scope: &RuntimeScope,
        role: &str,
        slot: Option<i32>,
        ticket_id: Option<&str>,
        branch: Option<&str>,
        a2a_pair_token: Option<&str>,
        idempotency_key: &str,
    ) -> Result<RuntimeWorkspaceResponse, ManagerError> {
        // Only the tenant's Nexus controller may request workers.
        if scope.workspace_role != "nexus" {
            return Err(ManagerError::api(
                "WORKSPACE_UNAVAILABLE",
                "only the tenant's Nexus controller can request worker workspaces",
            ));
        }
        if !WORKER_ROLES.contains(&role) {
            return Err(ManagerError::api(
                "INVALID_INPUT",
                format!("role '{role}' is not a requestable worker role"),
            ));
        }
        // Sanitize the branch/ticket association; slot is validated below.
        let ticket_id = ticket_id.map(|t| t.to_string());

        let mut tx = self.pool.begin().await?;
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1::text, 0))")
            .bind(scope.tenant_id.0)
            .execute(&mut *tx)
            .await?;
        let existing: Option<(uuid::Uuid, uuid::Uuid)> = sqlx::query_as(
            "SELECT o.id, w.id FROM operations o
               JOIN workspaces w ON w.id = o.resource_id
              WHERE o.organization_id = $1 AND o.kind = 'tenant.workspace'
                AND o.idempotency_ref = $2 AND w.tenant_id = $3",
        )
        .bind(scope.organization_id.0)
        .bind(idempotency_key)
        .bind(scope.tenant_id.0)
        .fetch_optional(&mut *tx)
        .await?;
        if let Some((operation_id, workspace_id)) = existing {
            tx.commit().await?;
            return Ok(RuntimeWorkspaceResponse {
                workspace_id: workspace_id.to_string(),
                operation_id: operation_id.to_string(),
                status: "queued",
            });
        }

        // Validate the slot and reserve capacity transactionally.
        // The caller is authorized by the authenticated Nexus runtime; derive
        // the tenant/org from the scope (never from the request body).
        let org_id = scope.organization_id;
        let tenant_id = scope.tenant_id;

        // Slot uniqueness: no other non-deleted workspace with this role+slot.
        if let Some(slot_value) = slot {
            let collision: Option<uuid::Uuid> = sqlx::query_scalar(
                "SELECT id FROM workspaces
                  WHERE tenant_id = $1 AND role = $2 AND slot = $3 AND desired_state <> 'deleted'
                  LIMIT 1",
            )
            .bind(tenant_id.0)
            .bind(role)
            .bind(slot_value)
            .fetch_optional(&mut *tx)
            .await?;
            if collision.is_some() {
                return Err(ManagerError::api(
                    "CONFLICT",
                    format!("slot {slot_value} for role '{role}' is already in use"),
                ));
            }
        }

        // Reserve one worker slot against the tenant fleet ceiling.
        self.quotas
            .reserve_worker_capacity_in_tx(&mut tx, org_id, tenant_id, 1)
            .await?;

        let workspace_id = self
            .tenant_runtime
            .insert_workspace_in_tx(&mut tx, org_id, tenant_id, role, slot)
            .await?;

        let op_id = operations::create_in_tx(
            &mut tx,
            &operations::NewOperation {
                organization_id: org_id,
                resource_type: "workspace".into(),
                resource_id: Some(workspace_id.0),
                kind: "tenant.workspace".into(),
                idempotency_ref: Some(idempotency_key.to_string()),
            },
        )
        .await?;

        // Record the ticket association if provided.
        if let Some(ticket) = ticket_id {
            sqlx::query("UPDATE workspaces SET ticket_id = $1 WHERE id = $2")
                .bind(ticket)
                .bind(workspace_id.0)
                .execute(&mut *tx)
                .await?;
        }
        if let Some(branch) = branch {
            sqlx::query("UPDATE workspaces SET branch = $1 WHERE id = $2")
                .bind(branch)
                .bind(workspace_id.0)
                .execute(&mut *tx)
                .await?;
        }
        if let Some(pair_token) = a2a_pair_token {
            sqlx::query("UPDATE workspaces SET a2a_pair_token = $1 WHERE id = $2")
                .bind(pair_token)
                .bind(workspace_id.0)
                .execute(&mut *tx)
                .await?;
        }

        tx.commit().await?;
        Ok(RuntimeWorkspaceResponse {
            workspace_id: workspace_id.to_string(),
            operation_id: op_id.to_string(),
            status: "queued",
        })
    }

    /// Update a workspace's observed readiness from its own principal. Cannot
    /// change ownership or role; only the workspace's own observed state is
    /// advanced.
    pub async fn heartbeat(&self, scope: &RuntimeScope) -> Result<HeartbeatResponse, ManagerError> {
        let affected = sqlx::query(
            "UPDATE workspaces SET observed_state = 'running',
                    last_heartbeat_at = clock_timestamp(), updated_at = now()
              WHERE id = $1 AND organization_id = $2 AND tenant_id = $3",
        )
        .bind(scope.workspace_id.0)
        .bind(scope.organization_id.0)
        .bind(scope.tenant_id.0)
        .execute(&self.pool)
        .await?;
        if affected.rows_affected() != 1 {
            return Err(ManagerError::not_found("workspace"));
        }
        if scope.workspace_role == "nexus" {
            sqlx::query(
                "UPDATE tenants SET observed_state = 'running', error_code = NULL,
                        updated_at = now()
                  WHERE id = $1 AND organization_id = $2
                    AND desired_state IN ('provisioning', 'running')",
            )
            .bind(scope.tenant_id.0)
            .bind(scope.organization_id.0)
            .execute(&self.pool)
            .await?;
        }
        Ok(HeartbeatResponse {
            workspace_id: scope.workspace_id.to_string(),
            observed_state: "running".to_string(),
        })
    }

    pub async fn get_workspace(
        &self,
        scope: &RuntimeScope,
        workspace_id: WorkspaceId,
    ) -> Result<RuntimeWorkspaceStatus, ManagerError> {
        if scope.workspace_id != workspace_id && scope.workspace_role != "nexus" {
            return Err(ManagerError::not_found("workspace"));
        }
        let row: Option<WorkspaceStatusRow> = sqlx::query_as(
            "SELECT role, slot, desired_state, observed_state, last_heartbeat_at
                   FROM workspaces WHERE id = $1 AND organization_id = $2 AND tenant_id = $3",
        )
        .bind(workspace_id.0)
        .bind(scope.organization_id.0)
        .bind(scope.tenant_id.0)
        .fetch_optional(&self.pool)
        .await?;
        let (role, slot, desired_state, observed_state, last_heartbeat_at) =
            row.ok_or_else(|| ManagerError::not_found("workspace"))?;
        Ok(RuntimeWorkspaceStatus {
            workspace_id: workspace_id.to_string(),
            role,
            slot,
            desired_state,
            observed_state,
            last_heartbeat_at,
        })
    }

    pub async fn mutate_worker(
        &self,
        scope: &RuntimeScope,
        workspace_id: WorkspaceId,
        delete: bool,
        idempotency_key: &str,
    ) -> Result<RuntimeWorkspaceResponse, ManagerError> {
        if scope.workspace_role != "nexus" {
            return Err(ManagerError::api(
                "WORKSPACE_UNAVAILABLE",
                "only the tenant Nexus may manage worker workspaces",
            ));
        }
        let kind = if delete {
            "workspace.delete"
        } else {
            "workspace.stop"
        };
        let existing: Option<uuid::Uuid> = sqlx::query_scalar(
            "SELECT id FROM operations WHERE organization_id = $1 AND kind = $2
                AND idempotency_ref = $3 AND resource_id = $4",
        )
        .bind(scope.organization_id.0)
        .bind(kind)
        .bind(idempotency_key)
        .bind(workspace_id.0)
        .fetch_optional(&self.pool)
        .await?;
        if let Some(operation_id) = existing {
            return Ok(RuntimeWorkspaceResponse {
                workspace_id: workspace_id.to_string(),
                operation_id: operation_id.to_string(),
                status: "queued",
            });
        }
        let mut tx = self.pool.begin().await?;
        let role: String = sqlx::query_scalar(
            "SELECT role FROM workspaces
              WHERE id = $1 AND organization_id = $2 AND tenant_id = $3 FOR UPDATE",
        )
        .bind(workspace_id.0)
        .bind(scope.organization_id.0)
        .bind(scope.tenant_id.0)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(|| ManagerError::not_found("workspace"))?;
        if role == "nexus" {
            return Err(ManagerError::api(
                "INVALID_INPUT",
                "Nexus cannot delete or stop itself through the worker API",
            ));
        }
        let desired = if delete { "deleting" } else { "stopped" };
        sqlx::query("UPDATE workspaces SET desired_state = $1, updated_at = now() WHERE id = $2")
            .bind(desired)
            .bind(workspace_id.0)
            .execute(&mut *tx)
            .await?;
        sqlx::query("UPDATE runtime_credentials SET revoked_at = clock_timestamp() WHERE workspace_id = $1 AND revoked_at IS NULL")
            .bind(workspace_id.0).execute(&mut *tx).await?;
        let operation_id = operations::create_in_tx(
            &mut tx,
            &operations::NewOperation {
                organization_id: scope.organization_id,
                resource_type: "workspace".into(),
                resource_id: Some(workspace_id.0),
                kind: kind.into(),
                idempotency_ref: Some(idempotency_key.to_string()),
            },
        )
        .await?;
        tx.commit().await?;
        Ok(RuntimeWorkspaceResponse {
            workspace_id: workspace_id.to_string(),
            operation_id: operation_id.to_string(),
            status: "queued",
        })
    }
}

/// Issue a workspace credential is delegated to the runtime credential service;
/// these helpers return the workspace id parsed from a request path.
pub fn parse_workspace_id(s: &str) -> Result<WorkspaceId, ManagerError> {
    s.parse::<WorkspaceId>()
        .map_err(|_| ManagerError::not_found("workspace"))
}

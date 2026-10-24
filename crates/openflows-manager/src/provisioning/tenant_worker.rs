//! Durable tenant provisioning worker.
//!
//! Claims leased `tenant.provision` and `tenant.delete` operations and runs the
//! tenant runtime integration and isolation reconciliation. It:
//!
//! - honors the organization's provisioning-execution slot: when the slot is
//!   occupied it leaves the operation queued (concurrency, not a
//!   resource-limit rejection);
//! - ensures the tenant creator's linked Coder account, tenant-scoped Redis credentials with an
//!   immutable namespace, a tenant hook key, and the Nexus workspace with
//!   explicit organization/owner/template/version IDs;
//! - keeps external Coder calls outside database transactions, persists
//!   progress step-by-step, and fences stale lease holders;
//! - never fabricates machine owners, credentials, or upstream adoption — it
//!   records a sanitized error and requires the tenant-capable adapter/backend
//!   that a licensed deployment supplies.

use crate::coder::CoderAdapter;
use crate::error::ManagerError;
use crate::id::{OrganizationId, ReleaseId, TenantId};
use crate::operations::{self, LeaseGuard};
use crate::quotas::TenantQuotaService;
use crate::repositories::OrganizationsRepository;
use crate::runtime::tenant::TenantRuntimeRepository;
use crate::templates::packaging::Role;
use crate::templates::repository::ReleaseRepository;
use sqlx::PgPool;
use std::sync::Arc;
use std::time::Duration;

/// How long the worker sleeps between claim cycles.
const POLL_INTERVAL: Duration = Duration::from_secs(2);

/// Tenant-provision step keys (persisted as `operations.current_step`).
pub mod step {
    pub const RECHECK: &str = "recheck";
    pub const ENSURE_OWNER: &str = "ensure_owner";
    pub const ALLOCATE_REDIS: &str = "allocate_redis";
    pub const ALLOCATE_HOOK_KEY: &str = "allocate_hook_key";
    pub const RESERVE_NEXUS: &str = "reserve_nexus";
    pub const CREATE_NEXUS: &str = "create_nexus";
    pub const WAIT_NEXUS: &str = "wait_nexus";
    pub const MARK_RUNNING: &str = "mark_running";
    pub const STOP_WORKSPACES: &str = "stop_workspaces";
    pub const RELEASE: &str = "release";
    pub const MARK_DELETED: &str = "mark_deleted";
}

/// Tenant-provision error codes surfaced as sanitized reasons.
pub mod err {
    pub const ORG_NOT_READY: &str = "ORG_NOT_READY";
    pub const REPO_UNAVAILABLE: &str = "REPOSITORY_UNAVAILABLE";
    pub const TENANT_STATE_CHANGED: &str = "TENANT_STATE_CHANGED";
    pub const MACHINE_OWNER_FAILED: &str = "MACHINE_OWNER_FAILED";
    pub const RELEASE_NOT_RESOLVED: &str = "RELEASE_NOT_RESOLVED";
    pub const NEXUS_CREATE_FAILED: &str = "NEXUS_CREATE_FAILED";
    pub const NEXUS_NOT_READY: &str = "NEXUS_NOT_READY";
}

/// The outcome of one tenant-provisioning attempt, returned for tests to assert
/// on without running the polling loop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TenantProvisionOutcome {
    Provisioned(TenantId),
    Deleted(TenantId),
    Queued(TenantId),
    NotReady(TenantId, &'static str),
    Failed(TenantId, &'static str, bool),
    StaleLease,
}

#[derive(Clone)]
pub struct TenantProvisionWorker {
    pub pool: PgPool,
    pub orgs: OrganizationsRepository,
    pub quotas: TenantQuotaService,
    pub tenant_runtime: TenantRuntimeRepository,
    pub runtime_credentials: crate::runtime::RuntimeCredentialService,
    pub release_repo: ReleaseRepository,
    pub adapter: Arc<dyn CoderAdapter>,
    pub manager_url: String,
    pub runtime_redis_url: String,
}

impl TenantProvisionWorker {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        pool: PgPool,
        orgs: OrganizationsRepository,
        quotas: TenantQuotaService,
        tenant_runtime: TenantRuntimeRepository,
        runtime_credentials: crate::runtime::RuntimeCredentialService,
        release_repo: ReleaseRepository,
        adapter: Arc<dyn CoderAdapter>,
    ) -> Self {
        TenantProvisionWorker {
            pool,
            orgs,
            quotas,
            tenant_runtime,
            runtime_credentials,
            release_repo,
            adapter,
            manager_url: "http://127.0.0.1:3002".into(),
            runtime_redis_url: "redis://127.0.0.1:6379".into(),
        }
    }

    pub fn with_runtime_endpoints(
        mut self,
        manager_url: impl Into<String>,
        runtime_redis_url: impl Into<String>,
    ) -> Self {
        self.manager_url = manager_url.into();
        self.runtime_redis_url = runtime_redis_url.into();
        self
    }

    pub async fn run_forever(&self) -> Result<(), ManagerError> {
        loop {
            if let Err(e) = self.run_once().await {
                tracing::warn!(%e, "tenant provision worker poll failed; retrying");
            }
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    }

    /// Claim and process one `tenant.provision` or `tenant.delete` operation.
    pub async fn run_once(&self) -> Result<(), ManagerError> {
        let owner = format!("tenant-provision-worker-{}", std::process::id());
        let Some(guard) = operations::claim_kinds_with_org_slot(
            &self.pool,
            &owner,
            &[
                "tenant.provision",
                "tenant.delete",
                "tenant.workspace",
                "tenant.start",
                "tenant.stop",
                "tenant.clean",
                "tenant.clean_all",
                "workspace.stop",
                "workspace.delete",
            ],
        )
        .await?
        else {
            return Ok(());
        };
        let heartbeat_guard = guard.clone();
        let mut heartbeat = tokio::task::JoinSet::new();
        heartbeat.spawn(async move {
            let mut interval = tokio::time::interval(
                operations::HEARTBEAT_INTERVAL
                    .to_std()
                    .unwrap_or(Duration::from_secs(10)),
            );
            loop {
                interval.tick().await;
                if !heartbeat_guard.heartbeat().await? {
                    return Err(ManagerError::Service(anyhow::anyhow!(
                        "tenant provision worker lost its lease"
                    )));
                }
            }
            #[allow(unreachable_code)]
            Ok::<(), ManagerError>(())
        });
        let kind = operation_kind(&self.pool, guard.operation_id).await?;
        let result = match kind.as_deref() {
            Some("tenant.delete") => self.delete(&guard).await,
            Some("tenant.workspace") => self.provision_worker_workspace(&guard).await,
            Some("tenant.start") => self.start(&guard).await,
            Some("tenant.stop") => self.stop(&guard).await,
            Some("tenant.clean" | "tenant.clean_all") => self.clean(&guard).await,
            Some("workspace.stop") => self.mutate_worker_workspace(&guard, false).await,
            Some("workspace.delete") => self.mutate_worker_workspace(&guard, true).await,
            _ => self.provision(&guard).await,
        };
        heartbeat.abort_all();
        self.finish(&guard, kind.as_deref(), result).await?;
        Ok(())
    }

    async fn mutate_worker_workspace(
        &self,
        guard: &LeaseGuard,
        delete: bool,
    ) -> Result<TenantProvisionOutcome, ManagerError> {
        let row: (uuid::Uuid, uuid::Uuid, Option<String>, String) = sqlx::query_as(
            "SELECT w.id, w.tenant_id, w.coder_workspace_id, o.coder_organization_id
               FROM workspaces w JOIN organizations o ON o.id = w.organization_id
              WHERE w.id = (SELECT resource_id FROM operations WHERE id = $1)
                AND w.organization_id = $2",
        )
        .bind(guard.operation_id.0)
        .bind(guard.organization_id.0)
        .fetch_one(&self.pool)
        .await?;
        if let Some(coder_id) = &row.2 {
            let org = coder_client::CoderOrganizationId::new(row.3);
            if delete {
                self.adapter.delete_workspace(&org, coder_id).await?;
            } else {
                self.adapter.stop_workspace(&org, coder_id).await?;
            }
        }
        let mut tx = self.pool.begin().await?;
        if !guard.lock_in_tx(&mut tx).await? {
            return Ok(TenantProvisionOutcome::StaleLease);
        }
        let state = if delete { "deleted" } else { "stopped" };
        sqlx::query("UPDATE workspaces SET desired_state = $1, observed_state = $1, updated_at = now() WHERE id = $2")
            .bind(state).bind(row.0).execute(&mut *tx).await?;
        if delete {
            sqlx::query("UPDATE tenants SET reserved_worker_capacity = GREATEST(reserved_worker_capacity - 1, 0), updated_at = now() WHERE id = $1")
                .bind(row.1).execute(&mut *tx).await?;
        }
        if !guard
            .complete_in_tx(
                &mut tx,
                serde_json::json!({"workspace_id": row.0, "status": state}),
            )
            .await?
        {
            return Ok(TenantProvisionOutcome::StaleLease);
        }
        tx.commit().await?;
        Ok(TenantProvisionOutcome::Provisioned(TenantId::from_uuid(
            row.1,
        )))
    }

    async fn stop(&self, guard: &LeaseGuard) -> Result<TenantProvisionOutcome, ManagerError> {
        let tenant_id = self
            .resource_tenant(guard)
            .await?
            .ok_or_else(|| ManagerError::not_found("tenant"))?;
        let rows: Vec<(String, String)> = sqlx::query_as(
            "SELECT w.coder_workspace_id, o.coder_organization_id
               FROM workspaces w JOIN organizations o ON o.id = w.organization_id
              WHERE w.tenant_id = $1 AND w.organization_id = $2
                AND w.coder_workspace_id IS NOT NULL AND w.desired_state <> 'deleted'",
        )
        .bind(tenant_id.0)
        .bind(guard.organization_id.0)
        .fetch_all(&self.pool)
        .await?;
        for (workspace, org) in rows {
            self.adapter
                .stop_workspace(&coder_client::CoderOrganizationId::new(org), &workspace)
                .await?;
        }
        let mut tx = self.pool.begin().await?;
        if !guard.lock_in_tx(&mut tx).await? {
            return Ok(TenantProvisionOutcome::StaleLease);
        }
        sqlx::query("UPDATE workspaces SET desired_state = 'stopped', observed_state = 'stopped', registered_at = NULL, last_heartbeat_at = NULL, updated_at = now() WHERE tenant_id = $1 AND desired_state <> 'deleted'")
            .bind(tenant_id.0).execute(&mut *tx).await?;
        let changed = sqlx::query("UPDATE tenants SET desired_state = 'paused', observed_state = 'paused', updated_at = now() WHERE id = $1 AND desired_state <> 'deleted'")
            .bind(tenant_id.0).execute(&mut *tx).await?;
        if changed.rows_affected() != 1 {
            return Ok(TenantProvisionOutcome::StaleLease);
        }
        if !guard
            .complete_in_tx(
                &mut tx,
                serde_json::json!({"tenant_id": tenant_id, "status":"paused"}),
            )
            .await?
        {
            return Ok(TenantProvisionOutcome::StaleLease);
        }
        tx.commit().await?;
        Ok(TenantProvisionOutcome::Provisioned(tenant_id))
    }

    async fn start(&self, guard: &LeaseGuard) -> Result<TenantProvisionOutcome, ManagerError> {
        let tenant_id = self
            .resource_tenant(guard)
            .await?
            .ok_or_else(|| ManagerError::not_found("tenant"))?;
        let rows: Vec<(uuid::Uuid, String, String)> = sqlx::query_as(
            "SELECT w.id, w.coder_workspace_id, o.coder_organization_id
               FROM workspaces w JOIN organizations o ON o.id = w.organization_id
              WHERE w.tenant_id = $1 AND w.organization_id = $2
                AND w.coder_workspace_id IS NOT NULL AND w.desired_state <> 'deleted'",
        )
        .bind(tenant_id.0)
        .bind(guard.organization_id.0)
        .fetch_all(&self.pool)
        .await?;
        for (id, workspace, org) in &rows {
            let workspace_id = crate::id::WorkspaceId::from_uuid(*id);
            if self.runtime_registered_and_healthy(workspace_id).await? {
                continue;
            }
            let bootstrap_ref = self
                .runtime_credentials
                .ensure_bootstrap_ref(crate::id::WorkspaceId::from_uuid(*id))
                .await?;
            self.tenant_runtime
                .prepare_workspace_secret(
                    guard.organization_id,
                    tenant_id,
                    workspace_id,
                    &bootstrap_ref,
                    &self.manager_url,
                    &self.runtime_redis_url,
                )
                .await?;
            self.adapter
                .start_workspace(&coder_client::CoderOrganizationId::new(org), workspace)
                .await?;
        }
        let nexus: Option<uuid::Uuid> = sqlx::query_scalar(
            "SELECT id FROM workspaces WHERE tenant_id = $1 AND role = 'nexus' AND desired_state <> 'deleted'",
        )
        .bind(tenant_id.0)
        .fetch_optional(&self.pool)
        .await?;
        let Some(nexus) = nexus else {
            return Ok(TenantProvisionOutcome::NotReady(
                tenant_id,
                err::NEXUS_NOT_READY,
            ));
        };
        if !self
            .runtime_registered_and_healthy(crate::id::WorkspaceId::from_uuid(nexus))
            .await?
        {
            return Ok(TenantProvisionOutcome::NotReady(
                tenant_id,
                err::NEXUS_NOT_READY,
            ));
        }
        let mut tx = self.pool.begin().await?;
        if !guard.lock_in_tx(&mut tx).await? {
            return Ok(TenantProvisionOutcome::StaleLease);
        }
        let changed = sqlx::query("UPDATE tenants SET desired_state = 'running', observed_state = 'running', updated_at = now() WHERE id = $1 AND desired_state NOT IN ('deleted', 'deleting')")
            .bind(tenant_id.0).execute(&mut *tx).await?;
        if changed.rows_affected() != 1 {
            return Ok(TenantProvisionOutcome::StaleLease);
        }
        if !guard
            .complete_in_tx(
                &mut tx,
                serde_json::json!({"tenant_id": tenant_id, "status":"running"}),
            )
            .await?
        {
            return Ok(TenantProvisionOutcome::StaleLease);
        }
        tx.commit().await?;
        Ok(TenantProvisionOutcome::Provisioned(tenant_id))
    }

    async fn clean(&self, guard: &LeaseGuard) -> Result<TenantProvisionOutcome, ManagerError> {
        let tenant_id = self
            .resource_tenant(guard)
            .await?
            .ok_or_else(|| ManagerError::not_found("tenant"))?;
        let deleted = self
            .tenant_runtime
            .clear_redis_namespace(guard.organization_id, tenant_id)
            .await?;
        let mut tx = self.pool.begin().await?;
        if !guard.lock_in_tx(&mut tx).await? {
            return Ok(TenantProvisionOutcome::StaleLease);
        }
        if !guard
            .complete_in_tx(
                &mut tx,
                serde_json::json!({"tenant_id": tenant_id, "keys_deleted": deleted}),
            )
            .await?
        {
            return Ok(TenantProvisionOutcome::StaleLease);
        }
        tx.commit().await?;
        Ok(TenantProvisionOutcome::Provisioned(tenant_id))
    }

    async fn provision_worker_workspace(
        &self,
        guard: &LeaseGuard,
    ) -> Result<TenantProvisionOutcome, ManagerError> {
        let row: (
            uuid::Uuid,
            String,
            String,
            uuid::Uuid,
            String,
            Option<String>,
            Option<i32>,
            i32,
            Option<String>,
        ) = sqlx::query_as(
            "SELECT w.id, w.role, o.coder_organization_id, w.tenant_id,
            r.full_name, w.branch, w.slot, t.fleet_size, w.a2a_pair_token
               FROM workspaces w JOIN organizations o ON o.id = w.organization_id
               JOIN tenants t ON t.id = w.tenant_id AND t.organization_id = w.organization_id
               JOIN github_repositories r ON r.organization_id = t.organization_id
                    AND r.connection_id = t.connection_id
                    AND r.github_repository_id = t.github_repository_id
              WHERE w.id = (SELECT resource_id FROM operations WHERE id = $1)
                AND w.organization_id = $2 AND w.desired_state = 'provisioning'",
        )
        .bind(guard.operation_id.0)
        .bind(guard.organization_id.0)
        .fetch_one(&self.pool)
        .await?;
        let workspace_id = crate::id::WorkspaceId::from_uuid(row.0);
        let tenant_id = TenantId::from_uuid(row.3);
        // A previous attempt may have completed runtime registration and its
        // readiness heartbeat before losing the operation lease. Preserve that
        // successful runtime instead of issuing another single-use bootstrap,
        // which would clear registration and strand the adopted workspace.
        if self.runtime_registered_and_healthy(workspace_id).await? {
            return self
                .mark_worker_running(guard, tenant_id, workspace_id)
                .await;
        }
        let owner: (String,) = sqlx::query_as(
            "SELECT coder_owner_id FROM runtime_identities WHERE tenant_id = (SELECT tenant_id FROM workspaces WHERE id = $1) AND organization_id = $2",
        ).bind(row.0).bind(guard.organization_id.0).fetch_one(&self.pool).await?;
        let org = coder_client::CoderOrganizationId::new(row.2);
        let owner = crate::coder::types::CoderOwnerId::new(owner.0);
        let release_id = self
            .release_repo
            .get_org_binding(guard.organization_id)
            .await?
            .ok_or_else(|| {
                ManagerError::api(
                    err::RELEASE_NOT_RESOLVED,
                    "template release is not published",
                )
            })?;
        let role = match row.1.as_str() {
            "nexus" => Role::Nexus,
            "forge" => Role::Forge,
            "sentinel" => Role::Sentinel,
            "vessel" => Role::Vessel,
            "lore" => Role::Lore,
            _ => return Err(ManagerError::InvalidInput("invalid workspace role".into())),
        };
        let ticket_id: Option<String> =
            sqlx::query_scalar("SELECT ticket_id FROM workspaces WHERE id = $1")
                .bind(row.0)
                .fetch_one(&self.pool)
                .await?;
        let version = self
            .release_repo
            .get_org_template_version(guard.organization_id, release_id, role)
            .await?
            .ok_or_else(|| {
                ManagerError::api(err::RELEASE_NOT_RESOLVED, "template version is not ready")
            })?;
        let template =
            coder_client::CoderTemplateId::new(version.coder_template_id.ok_or_else(|| {
                ManagerError::api(err::RELEASE_NOT_RESOLVED, "template id missing")
            })?);
        let version_id =
            coder_client::CoderTemplateVersionId::new(version.coder_version_id.ok_or_else(
                || ManagerError::api(err::RELEASE_NOT_RESOLVED, "template version id missing"),
            )?);
        let workspace_image_digest = self.role_image_digest(release_id, role).await?;
        let runtime_secret_ref = self
            .ensure_workspace_runtime_secret(
                guard.organization_id,
                TenantId::from_uuid(row.3),
                crate::id::WorkspaceId::from_uuid(row.0),
            )
            .await?;
        let mut parameters = vec![
            coder_client::types::WorkspaceParameterValue {
                name: "hosted".into(),
                value: "true".into(),
            },
            coder_client::types::WorkspaceParameterValue {
                name: "runtime_secret_ref".into(),
                value: runtime_secret_ref,
            },
            coder_client::types::WorkspaceParameterValue {
                name: "workspace_image_digest".into(),
                value: workspace_image_digest,
            },
            coder_client::types::WorkspaceParameterValue {
                name: "role".into(),
                value: row.1.clone(),
            },
            coder_client::types::WorkspaceParameterValue {
                name: "tenant".into(),
                value: row.3.to_string(),
            },
            coder_client::types::WorkspaceParameterValue {
                name: "repo_url".into(),
                value: format!("https://github.com/{}.git", row.4),
            },
            coder_client::types::WorkspaceParameterValue {
                name: "branch".into(),
                value: row.5.clone().unwrap_or_default(),
            },
            coder_client::types::WorkspaceParameterValue {
                name: "slot".into(),
                value: row.6.map(|value| value.to_string()).unwrap_or_default(),
            },
            coder_client::types::WorkspaceParameterValue {
                name: "fleet_size".into(),
                value: row.7.to_string(),
            },
            coder_client::types::WorkspaceParameterValue {
                name: "a2a_relay_addr".into(),
                value: format!("openflows-nexus-{}:3000", row.3),
            },
        ];
        if let Some(ticket_id) = ticket_id {
            parameters.push(coder_client::types::WorkspaceParameterValue {
                name: "ticket_id".into(),
                value: ticket_id,
            });
        }
        if let Some(pair_token) = &row.8 {
            parameters.push(coder_client::types::WorkspaceParameterValue {
                name: "a2a_pair_token".into(),
                value: pair_token.clone(),
            });
        }
        let created = self
            .adapter
            .create_tenant_workspace_with_parameters(
                &org,
                &owner,
                &template,
                &version_id,
                &worker_workspace_name(&row.1, row.0),
                &parameters,
            )
            .await?;
        if created.organization_id != org.0 || created.owner_id != owner.0 {
            return Err(ManagerError::api(
                "CODER_SCOPE_MISMATCH",
                "worker workspace scope mismatch",
            ));
        }
        // Persist the upstream identifier before waiting for readiness so a
        // failed build can still be deleted during tenant cleanup.
        self.tenant_runtime
            .set_coder_workspace_id(workspace_id, &created.id)
            .await?;
        self.adapter.wait_workspace_ready(&org, &created.id).await?;
        if !self.runtime_registered_and_healthy(workspace_id).await? {
            return Ok(TenantProvisionOutcome::NotReady(
                tenant_id,
                err::NEXUS_NOT_READY,
            ));
        }
        self.mark_worker_running(guard, tenant_id, workspace_id)
            .await
    }

    async fn mark_worker_running(
        &self,
        guard: &LeaseGuard,
        tenant_id: TenantId,
        workspace_id: crate::id::WorkspaceId,
    ) -> Result<TenantProvisionOutcome, ManagerError> {
        let mut tx = self.pool.begin().await?;
        if !guard.lock_in_tx(&mut tx).await? {
            return Ok(TenantProvisionOutcome::StaleLease);
        }
        let changed = sqlx::query("UPDATE workspaces SET desired_state = 'running', observed_state = 'running', updated_at = now() WHERE id = $1 AND desired_state = 'provisioning'").bind(workspace_id.0).execute(&mut *tx).await?;
        if changed.rows_affected() != 1 {
            return Ok(TenantProvisionOutcome::StaleLease);
        }
        if !guard
            .complete_in_tx(
                &mut tx,
                serde_json::json!({"workspace_id": workspace_id, "status":"running"}),
            )
            .await?
        {
            return Ok(TenantProvisionOutcome::StaleLease);
        }
        tx.commit().await?;
        Ok(TenantProvisionOutcome::Provisioned(tenant_id))
    }

    /// Execute one provisioning attempt for a leased operation.
    pub async fn provision(
        &self,
        guard: &LeaseGuard,
    ) -> Result<TenantProvisionOutcome, ManagerError> {
        // Resolve the actual tenant id from the operation's resource_id.
        let tenant_id = match self.resource_tenant(guard).await? {
            Some(t) => t,
            None => {
                return Ok(TenantProvisionOutcome::NotReady(
                    TenantId::default(),
                    err::TENANT_STATE_CHANGED,
                ))
            }
        };
        let org_id = guard.organization_id;

        // Provisioning slot: only one active tenant operation per org. If the
        // slot is occupied, leave the operation queued (concurrency, not a
        // resource-limit rejection).
        if !self
            .quotas
            .provisioning_slot_available_for(org_id, Some(guard.operation_id))
            .await?
        {
            return Ok(TenantProvisionOutcome::Queued(tenant_id));
        }

        if !guard.set_step(step::RECHECK).await? {
            return Ok(TenantProvisionOutcome::StaleLease);
        }
        // Recheck org readiness, tenant desired state, and repository access.
        if !self.tenant_is_provisioning(tenant_id).await? {
            return Ok(TenantProvisionOutcome::NotReady(
                tenant_id,
                err::TENANT_STATE_CHANGED,
            ));
        }
        if !self.org_ready(org_id).await? {
            return Ok(TenantProvisionOutcome::NotReady(
                tenant_id,
                err::ORG_NOT_READY,
            ));
        }
        if !self.repo_accessible(tenant_id).await? {
            return Ok(TenantProvisionOutcome::NotReady(
                tenant_id,
                err::REPO_UNAVAILABLE,
            ));
        }

        // Resolve the immutable tenant creator, never the actor retrying the job.
        if !guard.set_step(step::ENSURE_OWNER).await? {
            return Ok(TenantProvisionOutcome::StaleLease);
        }
        let coder_org = self
            .coder_org_for_tenant(tenant_id)
            .await?
            .ok_or_else(|| ManagerError::not_found("organization"))?;
        let linked: Option<(String, String)> = sqlx::query_as(
            "SELECT l.coder_user_id, l.coder_url FROM tenants t
             JOIN coder_user_links l ON l.user_id = t.owner_user_id
             JOIN users u ON u.id = t.owner_user_id AND u.status = 'active'
             JOIN memberships m ON m.organization_id=t.organization_id AND m.user_id=t.owner_user_id
                AND m.status='active' AND m.role IN ('admin','developer')
             WHERE t.id = $1 AND t.organization_id = $2",
        )
        .bind(tenant_id.0)
        .bind(org_id.0)
        .fetch_optional(&self.pool)
        .await?;
        let Some((owner_id, deployment)) = linked else {
            self.record_error(guard, tenant_id, "OWNER_MIGRATION_REQUIRED")
                .await?;
            return Ok(TenantProvisionOutcome::NotReady(
                tenant_id,
                "OWNER_MIGRATION_REQUIRED",
            ));
        };
        if deployment
            != std::env::var("CODER_URL")
                .unwrap_or_default()
                .trim_end_matches('/')
        {
            self.record_error(guard, tenant_id, "CODER_DEPLOYMENT_MISMATCH")
                .await?;
            return Ok(TenantProvisionOutcome::NotReady(
                tenant_id,
                "CODER_DEPLOYMENT_MISMATCH",
            ));
        }
        let owner = crate::coder::types::CoderOwnerId::new(owner_id);
        match self.adapter.ensure_user_owner(&coder_org, &owner).await {
            Ok(()) => (),
            Err(error) => {
                tracing::warn!(
                    tenant_id = %tenant_id.0,
                    coder_org = %coder_org.0,
                    error = %error,
                    "failed to verify tenant creator's Coder account"
                );
                self.record_error(guard, tenant_id, "CODER_USER_OWNER_FAILED")
                    .await?;
                return Ok(TenantProvisionOutcome::NotReady(
                    tenant_id,
                    "CODER_USER_OWNER_FAILED",
                ));
            }
        };
        self.persist_owner(tenant_id, org_id, &owner.0).await?;

        // Allocate tenant-scoped Redis credentials (immutable namespace).
        if !guard.set_step(step::ALLOCATE_REDIS).await? {
            return Ok(TenantProvisionOutcome::StaleLease);
        }
        let secret_ref = format!("tenant-redis-{}", tenant_id.0);
        self.tenant_runtime
            .ensure_redis_credential(org_id, tenant_id, &secret_ref)
            .await?;

        // Allocate the tenant-scoped lifecycle-hook signing key.
        if !guard.set_step(step::ALLOCATE_HOOK_KEY).await? {
            return Ok(TenantProvisionOutcome::StaleLease);
        }
        let hook_secret_ref = format!("tenant-hook-{}", tenant_id.0);
        self.tenant_runtime
            .ensure_hook_key(org_id, tenant_id, &hook_secret_ref)
            .await?;

        // Reserve the Nexus workspace row and its runtime credential.
        if !guard.set_step(step::RESERVE_NEXUS).await? {
            return Ok(TenantProvisionOutcome::StaleLease);
        }
        let nexus_ws = self
            .tenant_runtime
            .ensure_workspace(org_id, tenant_id, "nexus", None)
            .await?;
        // A previous attempt may have created the workspace and its runtime
        // may already be healthy. Do not issue another bootstrap credential in
        // that case: issuing one deliberately clears `registered_at`, which
        // would make a successful runtime look unregistered on every retry.
        if self.runtime_registered_and_healthy(nexus_ws).await? {
            return self.mark_tenant_running(guard, tenant_id, nexus_ws).await;
        }
        let existing_coder_workspace_id: Option<String> =
            sqlx::query_scalar("SELECT coder_workspace_id FROM workspaces WHERE id = $1")
                .bind(nexus_ws.0)
                .fetch_optional(&self.pool)
                .await?
                .flatten();
        let runtime_secret_ref = self
            .ensure_workspace_runtime_secret(org_id, tenant_id, nexus_ws)
            .await?;

        // Create the Nexus workspace with explicit org/owner/template/version.
        if !guard.set_step(step::CREATE_NEXUS).await? {
            return Ok(TenantProvisionOutcome::StaleLease);
        }
        let (template_id, version_id, workspace_image_digest) =
            match self.resolve_nexus_template(org_id).await? {
                Some(x) => x,
                None => {
                    self.record_error(guard, tenant_id, err::RELEASE_NOT_RESOLVED)
                        .await?;
                    return Ok(TenantProvisionOutcome::NotReady(
                        tenant_id,
                        err::RELEASE_NOT_RESOLVED,
                    ));
                }
            };
        // Coder workspace names are limited and must be lowercase. Keep the
        // name deterministic for retry adoption while staying within Coder's
        // 32-character limit.
        let compact_tenant = tenant_id.0.simple().to_string();
        let workspace_name = format!("of-nexus-{}", &compact_tenant[..20]);
        let repository = self.tenant_repository_full_name(tenant_id).await?;
        let fleet_size: i32 = sqlx::query_scalar("SELECT fleet_size FROM tenants WHERE id = $1")
            .bind(tenant_id.0)
            .fetch_one(&self.pool)
            .await?;
        let created = match self
            .adapter
            .create_tenant_workspace_with_parameters(
                &coder_org,
                &owner,
                &template_id,
                &version_id,
                &workspace_name,
                &[
                    coder_client::types::WorkspaceParameterValue {
                        name: "hosted".into(),
                        value: "true".into(),
                    },
                    coder_client::types::WorkspaceParameterValue {
                        name: "runtime_secret_ref".into(),
                        value: runtime_secret_ref,
                    },
                    coder_client::types::WorkspaceParameterValue {
                        name: "workspace_image_digest".into(),
                        value: workspace_image_digest,
                    },
                    coder_client::types::WorkspaceParameterValue {
                        name: "tenant".into(),
                        value: tenant_id.to_string(),
                    },
                    coder_client::types::WorkspaceParameterValue {
                        name: "repo_url".into(),
                        value: format!("https://github.com/{repository}.git"),
                    },
                    coder_client::types::WorkspaceParameterValue {
                        name: "github_repository".into(),
                        value: repository,
                    },
                    coder_client::types::WorkspaceParameterValue {
                        name: "fleet_size".into(),
                        value: fleet_size.to_string(),
                    },
                ],
            )
            .await
        {
            Ok(ws) => ws,
            Err(error) => {
                tracing::warn!(
                    tenant_id = %tenant_id.0,
                    error = %error,
                    "failed to create Nexus workspace"
                );
                self.record_error(guard, tenant_id, err::NEXUS_CREATE_FAILED)
                    .await?;
                return Ok(TenantProvisionOutcome::NotReady(
                    tenant_id,
                    err::NEXUS_CREATE_FAILED,
                ));
            }
        };
        if created.organization_id != coder_org.0 || created.owner_id != owner.0 {
            return Err(ManagerError::api(
                "CODER_SCOPE_MISMATCH",
                "created workspace does not match the tenant's organization or owner",
            ));
        }
        self.tenant_runtime
            .set_coder_workspace_id(nexus_ws, &created.id)
            .await?;
        if existing_coder_workspace_id.as_deref() == Some(created.id.as_str()) {
            self.adapter
                .restart_workspace(&coder_org, &created.id)
                .await?;
        }

        // Wait for build success + agent connection (bounded).
        if !guard.set_step(step::WAIT_NEXUS).await? {
            return Ok(TenantProvisionOutcome::StaleLease);
        }
        if self
            .adapter
            .wait_workspace_ready(&coder_org, &created.id)
            .await
            .is_err()
        {
            self.record_error(guard, tenant_id, err::NEXUS_NOT_READY)
                .await?;
            return Ok(TenantProvisionOutcome::NotReady(
                tenant_id,
                err::NEXUS_NOT_READY,
            ));
        }
        if !self.runtime_registered_and_healthy(nexus_ws).await? {
            self.record_error(guard, tenant_id, err::NEXUS_NOT_READY)
                .await?;
            return Ok(TenantProvisionOutcome::NotReady(
                tenant_id,
                err::NEXUS_NOT_READY,
            ));
        }

        self.mark_tenant_running(guard, tenant_id, nexus_ws).await
    }

    /// Promote a successfully registered Nexus runtime atomically. This is
    /// shared by both the initial provisioning path and a retry that finds an
    /// already healthy workspace.
    async fn mark_tenant_running(
        &self,
        guard: &LeaseGuard,
        tenant_id: TenantId,
        nexus_ws: crate::id::WorkspaceId,
    ) -> Result<TenantProvisionOutcome, ManagerError> {
        // Mark running: tenant + Nexus workspace, then complete atomically.
        if !guard.set_step(step::MARK_RUNNING).await? {
            return Ok(TenantProvisionOutcome::StaleLease);
        }
        let mut tx = self.pool.begin().await?;
        if !guard.lock_in_tx(&mut tx).await? {
            return Ok(TenantProvisionOutcome::StaleLease);
        }
        let state: String =
            sqlx::query_scalar("SELECT desired_state FROM tenants WHERE id = $1 FOR UPDATE")
                .bind(tenant_id.0)
                .fetch_one(&mut *tx)
                .await?;
        if state != "provisioning" {
            return Ok(TenantProvisionOutcome::NotReady(
                tenant_id,
                err::TENANT_STATE_CHANGED,
            ));
        }
        sqlx::query(
            "UPDATE tenants SET desired_state = 'running', observed_state = 'running',
                    error_code = NULL, updated_at = now()
              WHERE id = $1",
        )
        .bind(tenant_id.0)
        .execute(&mut *tx)
        .await?;
        let changed = sqlx::query(
            "UPDATE workspaces SET desired_state = 'running', observed_state = 'running', updated_at = now()
              WHERE id = $1 AND desired_state = 'provisioning'",
        )
        .bind(nexus_ws.0)
        .execute(&mut *tx)
        .await?;
        if changed.rows_affected() != 1 {
            return Ok(TenantProvisionOutcome::StaleLease);
        }
        if !guard
            .complete_in_tx(
                &mut tx,
                serde_json::json!({"tenant_id": tenant_id.to_string(), "status":"running"}),
            )
            .await?
        {
            return Ok(TenantProvisionOutcome::StaleLease);
        }
        tx.commit().await?;
        tracing::info!(tenant_id = %tenant_id.0, "tenant marked running");
        Ok(TenantProvisionOutcome::Provisioned(tenant_id))
    }

    /// Execute one tenant-delete attempt.
    pub async fn delete(&self, guard: &LeaseGuard) -> Result<TenantProvisionOutcome, ManagerError> {
        let tenant_id = match self.resource_tenant(guard).await? {
            Some(t) => t,
            None => {
                return Ok(TenantProvisionOutcome::NotReady(
                    TenantId::default(),
                    err::TENANT_STATE_CHANGED,
                ))
            }
        };
        let org_id = guard.organization_id;

        // Deletion must not wait behind provisioning capacity. In particular,
        // it is the recovery path when a tenant provision is stuck and holds
        // the last slot. The worker's lease still serializes this cleanup
        // operation, while the tenant enters `deleting` before any upstream
        // work is performed.
        if !guard.set_step(step::RECHECK).await? {
            return Ok(TenantProvisionOutcome::StaleLease);
        }
        // Stop/delete workspaces (real Coder workspace teardown is a live-required
        // adapter operation; we record the row transitions here).
        if !guard.set_step(step::STOP_WORKSPACES).await? {
            return Ok(TenantProvisionOutcome::StaleLease);
        }
        let upstream: Vec<(String, String)> = sqlx::query_as(
            "SELECT w.coder_workspace_id, o.coder_organization_id
               FROM workspaces w JOIN organizations o ON o.id = w.organization_id
              WHERE w.tenant_id = $1 AND w.organization_id = $2 AND w.coder_workspace_id IS NOT NULL",
        ).bind(tenant_id.0).bind(org_id.0).fetch_all(&self.pool).await?;
        for (workspace_id, coder_org) in upstream {
            self.adapter
                .delete_workspace(
                    &coder_client::CoderOrganizationId::new(coder_org),
                    &workspace_id,
                )
                .await
                .map_err(|error| {
                    tracing::warn!(tenant_id = %tenant_id.0, %workspace_id, %error,
                        "Coder workspace teardown failed; tenant deletion will be retried");
                    ManagerError::api(
                        "CODER_WORKSPACE_DELETE_FAILED",
                        "Coder workspace deletion failed; inspect the workspace's delete build in Coder",
                    )
                    .retryable(true)
                })?;
        }
        self.tenant_runtime
            .revoke_redis_credential(org_id, tenant_id)
            .await?;
        self.tenant_runtime
            .delete_tenant_secrets(org_id, tenant_id)
            .await?;
        let owner: Option<(String, String)> = sqlx::query_as(
            "SELECT ri.coder_owner_id, o.coder_organization_id
               FROM runtime_identities ri
               JOIN organizations o ON o.id = ri.organization_id
              WHERE ri.organization_id = $1 AND ri.tenant_id = $2
                AND EXISTS (SELECT 1 FROM tenants t WHERE t.id = ri.tenant_id AND t.owner_user_id IS NULL)",
        )
        .bind(org_id.0)
        .bind(tenant_id.0)
        .fetch_optional(&self.pool)
        .await?;
        if let Some((owner_id, coder_org)) = owner {
            self.adapter
                .delete_machine_owner(
                    &coder_client::CoderOrganizationId::new(coder_org),
                    &crate::coder::types::CoderOwnerId::new(owner_id),
                )
                .await?;
        }
        let mut tx = self.pool.begin().await?;
        if !guard.lock_in_tx(&mut tx).await? {
            return Ok(TenantProvisionOutcome::StaleLease);
        }
        // Release worker reservations and mark tenant deleted.
        sqlx::query(
            "UPDATE tenants SET reserved_worker_capacity = 0, updated_at = now()
              WHERE id = $1 AND organization_id = $2",
        )
        .bind(tenant_id.0)
        .bind(org_id.0)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "UPDATE workspaces SET desired_state = 'deleted', observed_state = 'deleted', updated_at = now()
              WHERE tenant_id = $1 AND organization_id = $2",
        )
        .bind(tenant_id.0)
        .bind(org_id.0)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "UPDATE tenants SET desired_state = 'deleted', observed_state = 'deleted', updated_at = now()
              WHERE id = $1 AND organization_id = $2",
        )
        .bind(tenant_id.0)
        .bind(org_id.0)
        .execute(&mut *tx)
        .await?;
        if !guard
            .complete_in_tx(
                &mut tx,
                serde_json::json!({"tenant_id": tenant_id.to_string(), "status":"deleted"}),
            )
            .await?
        {
            return Ok(TenantProvisionOutcome::StaleLease);
        }
        tx.commit().await?;
        Ok(TenantProvisionOutcome::Deleted(tenant_id))
    }

    async fn finish(
        &self,
        guard: &LeaseGuard,
        kind: Option<&str>,
        outcome: Result<TenantProvisionOutcome, ManagerError>,
    ) -> Result<(), ManagerError> {
        match outcome {
            Ok(TenantProvisionOutcome::Provisioned(_)) | Ok(TenantProvisionOutcome::Deleted(_)) => {
            }
            Ok(TenantProvisionOutcome::Queued(_)) => {
                // A concurrency wait is not an execution attempt. Release the
                // lease so the active operation can finish and this one can be
                // claimed later without inflating its attempt count.
                let _ = guard.requeue("waiting_for_provisioning_slot").await?;
            }
            Ok(TenantProvisionOutcome::NotReady(_, code)) => {
                let _ = guard
                    .fail(code != err::TENANT_STATE_CHANGED, Some(code))
                    .await?;
            }
            Ok(TenantProvisionOutcome::Failed(_, code, retryable)) => {
                let _ = guard.fail(retryable, Some(code)).await?;
            }
            Ok(TenantProvisionOutcome::StaleLease) => {}
            Err(e) => {
                let coder_rejection = match &e {
                    ManagerError::Service(source) => source
                        .chain()
                        .find_map(|cause| cause.downcast_ref::<coder_client::CoderHttpError>()),
                    _ => None,
                };
                if let Some(coder) = coder_rejection {
                    tracing::warn!(
                        operation_id = %guard.operation_id,
                        operation = coder.operation,
                        coder_status = coder.status.as_u16(),
                        "Coder rejected tenant operation"
                    );
                } else {
                    tracing::warn!(operation_id = %guard.operation_id, error = %e, "tenant operation attempt failed");
                }
                let code = match &e {
                    ManagerError::Api(api) => api.code.as_str(),
                    ManagerError::Database(_) => "DATABASE_ERROR",
                    ManagerError::Config(_) => "CONFIGURATION_ERROR",
                    _ if coder_rejection.is_some() && kind == Some("tenant.delete") => {
                        "CODER_WORKSPACE_DELETE_FAILED"
                    }
                    _ => "TENANT_OPERATION_FAILED",
                };
                let _ = guard.fail(true, Some(code)).await?;
            }
        }
        let _ = kind;
        Ok(())
    }

    async fn resource_tenant(&self, guard: &LeaseGuard) -> Result<Option<TenantId>, ManagerError> {
        let row: Option<(Option<uuid::Uuid>,)> = sqlx::query_as(
            "SELECT resource_id FROM operations WHERE id = $1 AND organization_id = $2",
        )
        .bind(guard.operation_id.0)
        .bind(guard.organization_id.0)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.and_then(|r| r.0.map(TenantId::from_uuid)))
    }

    async fn tenant_is_provisioning(&self, tenant_id: TenantId) -> Result<bool, ManagerError> {
        let row: Option<(String,)> =
            sqlx::query_as("SELECT desired_state FROM tenants WHERE id = $1")
                .bind(tenant_id.0)
                .fetch_optional(&self.pool)
                .await?;
        Ok(row.map(|r| r.0 == "provisioning").unwrap_or(false))
    }

    async fn org_ready(&self, org_id: OrganizationId) -> Result<bool, ManagerError> {
        let row: Option<(String,)> =
            sqlx::query_as("SELECT status FROM organizations WHERE id = $1")
                .bind(org_id.0)
                .fetch_optional(&self.pool)
                .await?;
        Ok(row.map(|r| r.0 == "ready").unwrap_or(false))
    }

    async fn repo_accessible(&self, tenant_id: TenantId) -> Result<bool, ManagerError> {
        let row: Option<(bool,)> = sqlx::query_as(
            "SELECT r.accessible
               FROM tenants t
               JOIN github_connections c ON c.id = t.connection_id AND c.organization_id = t.organization_id
               JOIN github_repositories r ON r.connection_id = c.id AND r.organization_id = c.organization_id
                    AND r.github_repository_id = t.github_repository_id
              WHERE t.id = $1 AND c.status = 'active'",
        )
        .bind(tenant_id.0)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|r| r.0).unwrap_or(false))
    }

    async fn tenant_repository_full_name(
        &self,
        tenant_id: TenantId,
    ) -> Result<String, ManagerError> {
        sqlx::query_scalar(
            "SELECT r.full_name FROM tenants t
               JOIN github_repositories r ON r.organization_id = t.organization_id
                    AND r.connection_id = t.connection_id
                    AND r.github_repository_id = t.github_repository_id
              WHERE t.id = $1 AND r.accessible = true",
        )
        .bind(tenant_id.0)
        .fetch_optional(&self.pool)
        .await?
        .ok_or_else(|| ManagerError::api(err::REPO_UNAVAILABLE, "repository unavailable"))
    }

    async fn coder_org_for_tenant(
        &self,
        tenant_id: TenantId,
    ) -> Result<Option<coder_client::CoderOrganizationId>, ManagerError> {
        let row: Option<(String,)> = sqlx::query_as(
            "SELECT o.coder_organization_id
               FROM tenants t JOIN organizations o ON o.id = t.organization_id
              WHERE t.id = $1 AND o.coder_organization_id IS NOT NULL",
        )
        .bind(tenant_id.0)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|r| coder_client::CoderOrganizationId::new(r.0)))
    }

    async fn persist_owner(
        &self,
        tenant_id: TenantId,
        org_id: OrganizationId,
        owner_id: &str,
    ) -> Result<(), ManagerError> {
        sqlx::query(
            "INSERT INTO runtime_identities (id, tenant_id, organization_id, coder_owner_id, status)
             VALUES ($1, $2, $3, $4, 'ready')
             ON CONFLICT (tenant_id) DO UPDATE SET
                status = 'ready', updated_at = now()
             WHERE runtime_identities.coder_owner_id = EXCLUDED.coder_owner_id",
        )
        .bind(uuid::Uuid::new_v4())
        .bind(tenant_id.0)
        .bind(org_id.0)
        .bind(owner_id)
        .execute(&self.pool)
        .await?
        .rows_affected();
        let stored: String = sqlx::query_scalar(
            "SELECT coder_owner_id FROM runtime_identities WHERE tenant_id = $1",
        )
        .bind(tenant_id.0)
        .fetch_one(&self.pool)
        .await?;
        if stored != owner_id {
            return Err(ManagerError::api(
                "OWNER_MIGRATION_REQUIRED",
                "existing runtime ownership differs from tenant creator",
            ));
        }
        Ok(())
    }

    async fn resolve_nexus_template(
        &self,
        org_id: OrganizationId,
    ) -> Result<
        Option<(
            coder_client::CoderTemplateId,
            coder_client::CoderTemplateVersionId,
            String,
        )>,
        ManagerError,
    > {
        let release_id: Option<ReleaseId> = self.release_repo.get_org_binding(org_id).await?;
        let Some(release_id) = release_id else {
            return Ok(None);
        };
        let row = self
            .release_repo
            .get_org_template_version(org_id, release_id, Role::Nexus)
            .await?;
        match row {
            Some(r) if r.status == "ready" => match (r.coder_template_id, r.coder_version_id) {
                (Some(t), Some(v)) => Ok(Some((
                    coder_client::CoderTemplateId::new(t),
                    coder_client::CoderTemplateVersionId::new(v),
                    self.role_image_digest(release_id, Role::Nexus).await?,
                ))),
                _ => Ok(None),
            },
            _ => Ok(None),
        }
    }

    async fn role_image_digest(
        &self,
        release_id: ReleaseId,
        role: Role,
    ) -> Result<String, ManagerError> {
        let (_, roles) = self
            .release_repo
            .get_release(release_id)
            .await?
            .ok_or_else(|| ManagerError::api(err::RELEASE_NOT_RESOLVED, "release missing"))?;
        // Older local releases may have been imported before the operator
        // configured the image digest. Allow the explicit deployment setting
        // to repair that metadata during provisioning; hosted releases still
        // require a digest either in the release or in this setting.
        let configured_digest = std::env::var("OPENFLOWS_WORKSPACE_IMAGE_DIGEST").ok();
        let digest = roles
            .get(&role)
            .and_then(|entry| entry.image_digest.as_deref())
            .map(str::to_owned)
            .or(configured_digest)
            .ok_or_else(|| {
                ManagerError::api(
                    err::RELEASE_NOT_RESOLVED,
                    "hosted template release is missing its workspace image digest; set OPENFLOWS_WORKSPACE_IMAGE_DIGEST",
                )
            })?;
        let valid = digest
            .strip_prefix("sha256:")
            .is_some_and(|hex| hex.len() == 64 && hex.bytes().all(|byte| byte.is_ascii_hexdigit()));
        if !valid {
            return Err(ManagerError::api(
                err::RELEASE_NOT_RESOLVED,
                "hosted template release has an invalid workspace image digest",
            ));
        }
        Ok(digest)
    }

    async fn ensure_workspace_runtime_secret(
        &self,
        org_id: OrganizationId,
        tenant_id: TenantId,
        workspace_id: crate::id::WorkspaceId,
    ) -> Result<String, ManagerError> {
        // A workspace can be rebuilt after its single-use bootstrap has been
        // consumed. The mounted bundle is then deliberately scrubbed, so
        // reusing it would make the next startup fail with AUTH_FAILED. Ensure
        // there is a fresh live bootstrap for every provisioning attempt and
        // rewrite the mounted bundle before asking Coder to start the build.
        let bootstrap_ref = self
            .runtime_credentials
            .ensure_bootstrap_ref(workspace_id)
            .await?;
        self.tenant_runtime
            .prepare_workspace_secret(
                org_id,
                tenant_id,
                workspace_id,
                &bootstrap_ref,
                &self.manager_url,
                &self.runtime_redis_url,
            )
            .await
    }

    async fn record_error(
        &self,
        guard: &LeaseGuard,
        tenant_id: TenantId,
        code: &str,
    ) -> Result<(), ManagerError> {
        let mut tx = self.pool.begin().await?;
        if guard.lock_in_tx(&mut tx).await? {
            sqlx::query("UPDATE tenants SET error_code = $1, updated_at = now() WHERE id = $2")
                .bind(code)
                .bind(tenant_id.0)
                .execute(&mut *tx)
                .await?;
            tx.commit().await?;
        } else {
            tx.rollback().await?;
        }
        Ok(())
    }

    async fn runtime_registered_and_healthy(
        &self,
        workspace_id: crate::id::WorkspaceId,
    ) -> Result<bool, ManagerError> {
        let ready: bool = sqlx::query_scalar(
            "SELECT COALESCE(
                    registered_at IS NOT NULL
                    AND last_heartbeat_at > clock_timestamp() - interval '2 minutes',
                    false
                )
               FROM workspaces WHERE id = $1",
        )
        .bind(workspace_id.0)
        .fetch_optional(&self.pool)
        .await?
        .unwrap_or(false);
        Ok(ready)
    }
}

async fn operation_kind(
    pool: &PgPool,
    id: crate::id::OperationId,
) -> Result<Option<String>, ManagerError> {
    let row: Option<(String,)> = sqlx::query_as("SELECT kind FROM operations WHERE id = $1")
        .bind(id.0)
        .fetch_optional(pool)
        .await?;
    Ok(row.map(|r| r.0))
}

/// Coder limits workspace names to 32 lowercase characters. Keep names
/// deterministic so a retry can adopt the workspace created by an earlier
/// attempt instead of provisioning a duplicate.
fn worker_workspace_name(role: &str, workspace_id: uuid::Uuid) -> String {
    let compact_id = workspace_id.simple().to_string();
    let prefix = format!("of-{role}-");
    let suffix_len = 32 - prefix.len();
    format!("{prefix}{}", &compact_id[..suffix_len])
}

#[cfg(test)]
mod workspace_name_tests {
    use super::worker_workspace_name;

    #[test]
    fn worker_workspace_names_fit_coder_and_remain_distinct_by_role() {
        let id = uuid::Uuid::parse_str("ce164533-8eee-4f4a-a9d5-6ab0ee63cf14").unwrap();
        for role in ["forge", "sentinel", "vessel", "lore"] {
            let name = worker_workspace_name(role, id);
            assert!(name.len() <= 32);
            assert!(name.starts_with(&format!("of-{role}-")));
            assert!(name
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-'));
            assert_eq!(name, worker_workspace_name(role, id));
        }
    }
}

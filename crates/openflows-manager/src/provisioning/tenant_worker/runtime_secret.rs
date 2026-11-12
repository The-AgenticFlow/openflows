use super::TenantProvisionWorker;
use crate::error::ManagerError;
use crate::id::{OrganizationId, TenantId, WorkspaceId};
use coder_client::CoderOrganizationId;

impl TenantProvisionWorker {
    pub(super) async fn recover_pending_worker(
        &self,
        org_id: OrganizationId,
        tenant_id: TenantId,
        workspace_id: WorkspaceId,
        coder_org: &str,
    ) -> Result<(), ManagerError> {
        let existing: Option<String> =
            sqlx::query_scalar("SELECT coder_workspace_id FROM workspaces WHERE id = $1")
                .bind(workspace_id.0)
                .fetch_optional(&self.pool)
                .await?
                .flatten();
        if let Some(existing) = existing {
            let org = CoderOrganizationId::new(coder_org.to_owned());
            if self
                .adapter
                .workspace_startup_failed(&org, &existing)
                .await?
            {
                self.ensure_workspace_runtime_secret(org_id, tenant_id, workspace_id)
                    .await?;
                sqlx::query("UPDATE workspaces SET registered_at = NULL WHERE id = $1")
                    .bind(workspace_id.0)
                    .execute(&self.pool)
                    .await?;
                self.adapter.restart_workspace(&org, &existing).await?;
            }
        }
        Ok(())
    }

    pub(super) async fn recover_stalled_nexus(
        &self,
        org_id: OrganizationId,
        tenant_id: TenantId,
        workspace_id: WorkspaceId,
        coder_org: &CoderOrganizationId,
    ) -> Result<bool, ManagerError> {
        let stalled: bool = sqlx::query_scalar(
            "SELECT COALESCE(registered_at < clock_timestamp() - interval '5 minutes'
                    AND last_heartbeat_at IS NULL, false)
               FROM workspaces WHERE id = $1",
        )
        .bind(workspace_id.0)
        .fetch_optional(&self.pool)
        .await?
        .unwrap_or(false);
        if !stalled {
            return Ok(false);
        }
        let existing: Option<String> =
            sqlx::query_scalar("SELECT coder_workspace_id FROM workspaces WHERE id = $1")
                .bind(workspace_id.0)
                .fetch_optional(&self.pool)
                .await?;
        if let Some(existing) = existing {
            self.ensure_workspace_runtime_secret(org_id, tenant_id, workspace_id)
                .await?;
            sqlx::query("UPDATE workspaces SET registered_at = NULL WHERE id = $1")
                .bind(workspace_id.0)
                .execute(&self.pool)
                .await?;
            self.adapter.restart_workspace(coder_org, &existing).await?;
            return Ok(true);
        }
        Ok(false)
    }

    pub(super) async fn ensure_workspace_runtime_secret(
        &self,
        org_id: OrganizationId,
        tenant_id: TenantId,
        workspace_id: crate::id::WorkspaceId,
    ) -> Result<String, ManagerError> {
        // A workspace can be rebuilt after its single-use bootstrap has been
        // consumed. The mounted bundle is then deliberately scrubbed, so
        // reusing it would make the next startup fail with AUTH_FAILED. This
        // is called when preparing a new startup, never while adopting an
        // existing Nexus startup during a provisioning retry.
        let bootstrap_ref = self
            .runtime_credentials
            .ensure_bootstrap_ref(workspace_id)
            .await?;
        let secret_ref = self
            .tenant_runtime
            .prepare_workspace_secret(
                org_id,
                tenant_id,
                workspace_id,
                &bootstrap_ref,
                &self.manager_url,
                &self.runtime_redis_url,
            )
            .await?;
        // Coder's Docker provider interprets bind sources on the Docker host,
        // not inside the Manager container. Translate only the controlled
        // filesystem-secret prefix for connected-local deployments.
        if std::env::var("OPENFLOWS_CONNECTED_LOCAL_ENABLED").as_deref() == Ok("true") {
            let container_root = std::env::var("OPENFLOWS_SECRET_DIR")
                .map_err(|_| ManagerError::Config("OPENFLOWS_SECRET_DIR is required".into()))?;
            let host_root =
                std::env::var("OPENFLOWS_CONNECTED_LOCAL_SECRET_DIR").map_err(|_| {
                    ManagerError::Config("OPENFLOWS_CONNECTED_LOCAL_SECRET_DIR is required".into())
                })?;
            let relative = std::path::Path::new(&secret_ref)
                .strip_prefix(&container_root)
                .map_err(|_| {
                    ManagerError::Config(
                        "runtime secret is outside the configured secret directory".into(),
                    )
                })?;
            return Ok(std::path::Path::new(&host_root)
                .join("secrets")
                .join(relative)
                .to_string_lossy()
                .into_owned());
        }
        Ok(secret_ref)
    }
}

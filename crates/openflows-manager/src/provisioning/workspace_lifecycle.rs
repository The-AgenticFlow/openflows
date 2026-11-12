//! Workspace stop and delete transitions for the tenant provisioning worker.
//!
//! Deleting a workspace removes its Coder workspace when one exists and always
//! retires the runtime bundle. `ensure_workspace_runtime_secret` runs before
//! Coder creation, so a workspace that never got a Coder ID can still hold a
//! delivered copy and a stored bundle, and that has to go too.

use super::tenant_worker::{TenantProvisionOutcome, TenantProvisionWorker};
use crate::error::ManagerError;
use crate::id::{TenantId, WorkspaceId};
use crate::operations::LeaseGuard;

impl TenantProvisionWorker {
    pub(crate) async fn mutate_worker_workspace(
        &self,
        guard: &LeaseGuard,
        delete: bool,
    ) -> Result<TenantProvisionOutcome, ManagerError> {
        // `coder_organization_id` stays optional: retiring the runtime bundle
        // must not depend on the organization having been mapped to Coder yet,
        // because a workspace can hold delivered credentials before creation
        // ever reached Coder.
        let row: (
            uuid::Uuid,
            uuid::Uuid,
            Option<String>,
            Option<String>,
            String,
            String,
        ) = sqlx::query_as(
            "SELECT w.id, w.tenant_id, w.coder_workspace_id, o.coder_organization_id,
                    w.desired_state, w.observed_state
               FROM workspaces w JOIN organizations o ON o.id = w.organization_id
              WHERE w.id = (SELECT resource_id FROM operations WHERE id = $1)
                AND w.organization_id = $2",
        )
        .bind(guard.operation_id.0)
        .bind(guard.organization_id.0)
        .fetch_one(&self.pool)
        .await?;
        if !delete && crate::runtime::workspaces::deletion_started(&row.4, &row.5) {
            let mut tx = self.pool.begin().await?;
            if !guard.lock_in_tx(&mut tx).await? {
                return Ok(TenantProvisionOutcome::StaleLease);
            }
            if !guard
                .complete_in_tx(
                    &mut tx,
                    serde_json::json!({"workspace_id": row.0, "status": "superseded_by_delete"}),
                )
                .await?
            {
                return Ok(TenantProvisionOutcome::StaleLease);
            }
            tx.commit().await?;
            return Ok(TenantProvisionOutcome::Provisioned(TenantId::from_uuid(
                row.1,
            )));
        }
        if let Some(coder_id) = &row.2 {
            let coder_org = row.3.as_deref().ok_or_else(|| {
                ManagerError::Config(
                    "workspace has a Coder workspace id but its organization has no Coder organization id"
                        .into(),
                )
            })?;
            let org = coder_client::CoderOrganizationId::new(coder_org);
            if delete {
                self.adapter.delete_workspace(&org, coder_id).await?;
            } else {
                self.adapter.stop_workspace(&org, coder_id).await?;
            }
        }
        if delete {
            self.tenant_runtime
                .retire_workspace_secret(
                    guard.organization_id,
                    TenantId::from_uuid(row.1),
                    WorkspaceId::from_uuid(row.0),
                )
                .await?;
        }
        let mut tx = self.pool.begin().await?;
        if !guard.lock_in_tx(&mut tx).await? {
            return Ok(TenantProvisionOutcome::StaleLease);
        }
        let state = if delete { "deleted" } else { "stopped" };
        let expected = if delete { "deleting" } else { "stopped" };
        let changed = sqlx::query(
            "UPDATE workspaces
                SET desired_state = $1, observed_state = $1, updated_at = now()
              WHERE id = $2 AND desired_state = $3
                AND observed_state NOT IN ('deleting', 'deleted')",
        )
        .bind(state)
        .bind(row.0)
        .bind(expected)
        .execute(&mut *tx)
        .await?;
        if delete && changed.rows_affected() == 1 {
            sqlx::query("UPDATE tenants SET reserved_worker_capacity = GREATEST(reserved_worker_capacity - 1, 0), updated_at = now() WHERE id = $1")
                .bind(row.1).execute(&mut *tx).await?;
        }
        let result_status = if changed.rows_affected() == 1 {
            state
        } else {
            "superseded"
        };
        if !guard
            .complete_in_tx(
                &mut tx,
                serde_json::json!({"workspace_id": row.0, "status": result_status}),
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
}

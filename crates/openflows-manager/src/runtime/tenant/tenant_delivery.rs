//! Workspace credential delivery, and the retire/delete half of the tenant
//! secret lifecycle.
//!
//! This is a child module of [`super`] so it can reach the repository's private
//! delivery backend without widening that field to the whole crate. Every
//! function here is an `impl TenantRuntimeRepository`, so the public API is the
//! same as when the methods lived in `tenant.rs`.

use super::TenantRuntimeRepository;
use crate::error::ManagerError;
use crate::id::{OrganizationId, TenantId, WorkspaceId};
use crate::runtime::delivery::{DeliveryTarget, SecretDelivery};

fn delivery_target(
    organization_id: OrganizationId,
    tenant_id: TenantId,
    workspace_id: WorkspaceId,
) -> DeliveryTarget {
    DeliveryTarget {
        organization_id,
        tenant_id,
        workspace_id,
    }
}

impl TenantRuntimeRepository {
    /// Select how bundles reach workspaces; defaults to mounting the store file.
    pub fn with_delivery(mut self, delivery: std::sync::Arc<dyn SecretDelivery>) -> Self {
        self.delivery = delivery;
        self
    }

    /// Select how tenant namespaces are walled; defaults to none (Docker and local).
    pub fn with_isolation(
        mut self,
        isolation: std::sync::Arc<dyn crate::runtime::isolation::TenantIsolation>,
    ) -> Self {
        self.isolation = isolation;
        self
    }

    /// Hand `bundle` to the delivery backend and return the handle the template
    /// receives. The workspace row keeps the durable store reference.
    pub(super) async fn deliver_workspace_bundle(
        &self,
        org_id: OrganizationId,
        tenant_id: TenantId,
        workspace_id: WorkspaceId,
        store_reference: &str,
        bundle: &[u8],
    ) -> Result<String, ManagerError> {
        // The namespace and its rules must exist before the first Secret lands in it.
        self.isolation.ensure_tenant(org_id, tenant_id).await?;
        self.delivery
            .deliver(
                &delivery_target(org_id, tenant_id, workspace_id),
                store_reference,
                bundle,
            )
            .await
    }

    /// Remove one workspace's delivered copy and store object after its Coder
    /// workspace is deleted. Safe to repeat.
    pub async fn retire_workspace_secret(
        &self,
        org_id: OrganizationId,
        tenant_id: TenantId,
        workspace_id: WorkspaceId,
    ) -> Result<(), ManagerError> {
        self.delivery
            .remove(&delivery_target(org_id, tenant_id, workspace_id))
            .await?;
        let store_ref: Option<String> =
            sqlx::query_scalar("SELECT runtime_secret_ref FROM workspaces WHERE id = $1")
                .bind(workspace_id.0)
                .fetch_optional(&self.pool)
                .await?
                .flatten();
        if let Some(reference) = store_ref {
            match self.secret_provider.delete(&reference).await {
                Ok(()) | Err(crate::secrets::SecretError::NotFound(_)) => {}
                Err(error) => {
                    return Err(ManagerError::Config(format!(
                        "cannot delete workspace secret: {error}"
                    )))
                }
            }
            sqlx::query("UPDATE workspaces SET runtime_secret_ref = NULL WHERE id = $1")
                .bind(workspace_id.0)
                .execute(&self.pool)
                .await?;
        }
        Ok(())
    }

    /// Remove the delivered copy of every workspace a tenant owns, before the
    /// store objects behind them are deleted.
    async fn retire_tenant_workspace_secrets(
        &self,
        org_id: OrganizationId,
        tenant_id: TenantId,
    ) -> Result<(), ManagerError> {
        let workspaces: Vec<uuid::Uuid> = sqlx::query_scalar(
            "SELECT id FROM workspaces WHERE organization_id = $1 AND tenant_id = $2",
        )
        .bind(org_id.0)
        .bind(tenant_id.0)
        .fetch_all(&self.pool)
        .await?;
        for id in workspaces {
            self.delivery
                .remove(&delivery_target(
                    org_id,
                    tenant_id,
                    WorkspaceId::from_uuid(id),
                ))
                .await?;
        }
        Ok(())
    }

    /// Remove every secret-provider object owned by a tenant. Database rows
    /// remain as a sanitized audit trail, with their credentials revoked.
    pub async fn delete_tenant_secrets(
        &self,
        org_id: OrganizationId,
        tenant_id: TenantId,
    ) -> Result<(), ManagerError> {
        self.retire_tenant_workspace_secrets(org_id, tenant_id)
            .await?;
        let refs: Vec<String> = sqlx::query_scalar(
            "SELECT secret_ref FROM tenant_hook_keys
              WHERE organization_id = $1 AND tenant_id = $2 AND secret_ref IS NOT NULL
             UNION
             SELECT runtime_secret_ref FROM workspaces
              WHERE organization_id = $1 AND tenant_id = $2 AND runtime_secret_ref IS NOT NULL
             UNION
             SELECT b.secret_ref FROM workspace_bootstrap_credentials b
               JOIN workspaces w ON w.id = b.workspace_id
              WHERE w.organization_id = $1 AND w.tenant_id = $2",
        )
        .bind(org_id.0)
        .bind(tenant_id.0)
        .fetch_all(&self.pool)
        .await?;
        for reference in refs {
            match self.secret_provider.delete(&reference).await {
                Ok(()) | Err(crate::secrets::SecretError::NotFound(_)) => {}
                Err(error) => {
                    return Err(ManagerError::Config(format!(
                        "cannot delete tenant secret: {error}"
                    )))
                }
            }
        }
        sqlx::query(
            "UPDATE tenant_hook_keys SET revoked_at = COALESCE(revoked_at, now()), updated_at = now()
              WHERE organization_id = $1 AND tenant_id = $2",
        )
        .bind(org_id.0)
        .bind(tenant_id.0)
        .execute(&self.pool)
        .await?;
        self.isolation.remove_tenant(org_id, tenant_id).await
    }
}

//! Operator command for a hosted workspace that lost its runtime bootstrap.
//!
//! The durable bundle is rewritten in the secret store and the delivered copy is
//! refreshed in the same step, so the workspace keeps seeing one consistent
//! bundle whether it mounts the store file or a delivered copy.

use super::setting;
use anyhow::{bail, Context, Result};
use serde_json::Value;
use zeroize::{Zeroize, Zeroizing};

/// Operator-only recovery for a hosted workspace whose single-use bootstrap
/// was consumed and whose process no longer has its rotating refresh token.
/// The new bootstrap is written only to the existing workspace secret bundle.
pub async fn repair_runtime(workspace_id: uuid::Uuid) -> Result<()> {
    let db = crate::db::Db::connect(&setting("OPENFLOWS_DATABASE_URL")?).await?;
    let row: Option<(String, Option<String>, uuid::Uuid, uuid::Uuid)> = sqlx::query_as(
        "SELECT role, runtime_secret_ref, organization_id, tenant_id FROM workspaces
          WHERE id=$1 AND desired_state IN ('running','provisioning')
            AND observed_state NOT IN ('deleted','deleting')",
    )
    .bind(workspace_id)
    .fetch_optional(db.pool())
    .await?;
    let (role, bundle_ref, org_id, tenant_id) = row.context("recoverable workspace not found")?;
    if !matches!(
        role.as_str(),
        "nexus" | "forge" | "sentinel" | "vessel" | "lore"
    ) {
        bail!("runtime repair requires a hosted agent workspace");
    }
    let bundle_ref = bundle_ref.context("workspace has no mounted runtime secret")?;
    let provider_name = setting("OPENFLOWS_SECRET_PROVIDER")?;
    let provider: std::sync::Arc<dyn crate::secrets::SecretProvider> =
        crate::server::resolve_secret_provider(&provider_name)
            .await?
            .into();
    let repo = crate::runtime::RuntimeRepository::new(db.pool().clone());
    let credentials =
        crate::runtime::RuntimeCredentialService::with_secret_provider(repo, provider.clone());
    let mut bootstrap = credentials
        .issue_bootstrap(crate::id::WorkspaceId::from_uuid(workspace_id))
        .await?;
    let result = async {
        let bytes = Zeroizing::new(provider.get(&bundle_ref).await?);
        let mut payload: Value = serde_json::from_slice(&bytes)?;
        let object = payload
            .as_object_mut()
            .context("mounted runtime secret is not a JSON object")?;
        object.insert(
            "bootstrap_token".into(),
            Value::String(bootstrap.raw_secret.clone()),
        );
        object.insert(
            "manager_url".into(),
            Value::String(
                crate::config::runtime_manager_url(&setting("OPENFLOWS_PUBLIC_URL")?)
                    .map_err(anyhow::Error::msg)?,
            ),
        );
        let bytes = Zeroizing::new(serde_json::to_vec(&payload)?);
        provider.replace(&bundle_ref, &bytes).await?;
        // The mounted copy is the delivered one, so refresh it as well.
        let backend = std::env::var(crate::config::PROVISIONER_BACKEND_ENV)
            .unwrap_or_else(|_| crate::config::DEFAULT_PROVISIONER_BACKEND.to_string());
        let target = crate::runtime::delivery::DeliveryTarget {
            organization_id: crate::id::OrganizationId::from_uuid(org_id),
            tenant_id: crate::id::TenantId::from_uuid(tenant_id),
            workspace_id: crate::id::WorkspaceId::from_uuid(workspace_id),
        };
        crate::runtime::delivery::resolve_delivery(&backend, &provider_name)
            .await?
            .deliver(&target, &bundle_ref, &bytes)
            .await?;
        Ok::<_, anyhow::Error>(())
    }
    .await;
    bootstrap.raw_secret.zeroize();
    result?;
    println!("Runtime bootstrap reissued for workspace {workspace_id}. Restart that Coder workspace to register a new session.");
    Ok(())
}

//! Backend-aware release selection, compatibility validation, and pin
//! persistence for workspace provisioning.

use crate::error::ManagerError;
use crate::id::{OrganizationId, ReleaseId, TenantId, WorkspaceId};
use crate::provisioning::tenant_worker::err;
use crate::templates::backend::{
    is_valid_image_digest, validate_architecture, validate_role_variant, DEFAULT_ARCHITECTURE,
    MAX_PARAMETER_SCHEMA_VERSION, MIN_PARAMETER_SCHEMA_VERSION,
};
use crate::templates::packaging::{Role, RoleManifestEntry};
use crate::templates::repository::ReleaseRepository;
use crate::templates::service::ReleaseArtifacts;
use crate::templates::{ProvisionerBackend, ReleaseCompatibilityTarget};
use sqlx::PgPool;

/// Environment keys controlling target compatibility for workspace releases.
pub const WORKSPACE_ARCH_ENV: &str = "OPENFLOWS_WORKSPACE_ARCH";
pub const WORKSPACE_RUNTIME_RELEASE_ENV: &str = "OPENFLOWS_WORKSPACE_RUNTIME_RELEASE";
pub const WORKSPACE_IMAGE_DIGEST_ENV: &str = "OPENFLOWS_WORKSPACE_IMAGE_DIGEST";

/// Resolved and validated template release specification for creating a
/// workspace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceReleaseSpec {
    pub template_id: coder_client::CoderTemplateId,
    pub version_id: coder_client::CoderTemplateVersionId,
    pub release_id: ReleaseId,
    pub backend: ProvisionerBackend,
    pub image_digest: String,
    pub image_repository: Option<String>,
    pub architecture: String,
    pub workspace_runtime_release: Option<String>,
    pub parameter_schema_version: u32,
}

/// Build the deployment's [`ReleaseCompatibilityTarget`] from environment
/// variables and provisioner mode.
pub fn compatibility_target_from_env(
    backend: ProvisionerBackend,
    hosted: bool,
) -> Result<ReleaseCompatibilityTarget, ManagerError> {
    let raw_arch = std::env::var(WORKSPACE_ARCH_ENV)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| DEFAULT_ARCHITECTURE.to_string());
    let architecture = validate_architecture(&raw_arch)?;
    let expected_runtime_release = std::env::var(WORKSPACE_RUNTIME_RELEASE_ENV)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty());

    Ok(ReleaseCompatibilityTarget {
        backend,
        architecture,
        min_parameter_schema_version: MIN_PARAMETER_SCHEMA_VERSION,
        max_parameter_schema_version: MAX_PARAMETER_SCHEMA_VERSION,
        expected_runtime_release,
        require_image_digest: hosted || backend == ProvisionerBackend::Kubernetes,
    })
}

/// Resolve and validate `OPENFLOWS_PROVISIONER_BACKEND` from the environment.
pub fn resolve_backend_env() -> Result<String, String> {
    let raw = std::env::var(crate::config::PROVISIONER_BACKEND_ENV)
        .unwrap_or_else(|_| crate::config::DEFAULT_PROVISIONER_BACKEND.to_string());
    raw.parse::<ProvisionerBackend>()
        .map_err(|e| e.to_string())?;
    Ok(raw)
}

/// Build a [`ReleaseCompatibilityTarget`] from a configured backend string and
/// manager [`Mode`](crate::config::Mode).
pub fn release_target(
    backend: &str,
    mode: crate::config::Mode,
) -> Result<ReleaseCompatibilityTarget, ManagerError> {
    let parsed: ProvisionerBackend = backend.parse()?;
    compatibility_target_from_env(parsed, mode == crate::config::Mode::Hosted)
}

/// Format a variant archive fetch/verification error for [`ReleaseSource`].
pub fn variant_source_err(
    role: Role,
    backend: ProvisionerBackend,
    detail: &str,
    err: impl std::fmt::Display,
) -> ManagerError {
    ManagerError::Config(format!(
        "role '{}' ({}) {detail}: {err}",
        role.as_str(),
        backend
    ))
}

/// Verify that every backend listed in `manifest` includes all `required_roles`.
pub fn ensure_required_variants(
    manifest: &crate::templates::packaging::TemplateManifest,
    archives: &std::collections::BTreeMap<(ProvisionerBackend, Role), Vec<u8>>,
    reference: &str,
) -> Result<(), ManagerError> {
    for backend in manifest.backends() {
        for role in &manifest.required_roles {
            if !archives.contains_key(&(backend, *role)) {
                return Err(ManagerError::Config(format!(
                    "hosted release {reference} is missing required role '{}' ({})",
                    role.as_str(),
                    backend
                )));
            }
        }
    }
    Ok(())
}

/// Validate all required role variants and archives in `artifacts` against
/// `target` before touching Coder or binding an organization.
pub fn validate_artifacts_for_target(
    artifacts: &ReleaseArtifacts,
    target: &ReleaseCompatibilityTarget,
) -> Result<(), ManagerError> {
    for role in &artifacts.manifest.required_roles {
        let entry = artifacts
            .manifest
            .variant(target.backend, *role)
            .ok_or_else(|| {
                ManagerError::InvalidInput(format!(
                    "release '{}' has no '{}' variant for required role '{}'",
                    artifacts.manifest.release,
                    target.backend,
                    role.as_str()
                ))
            })?;
        validate_role_variant(entry, target)?;
        if artifacts.archive_for(target.backend, *role).is_none() {
            return Err(ManagerError::InvalidInput(format!(
                "artifact source did not provide archive for role '{}' ({})",
                role.as_str(),
                target.backend
            )));
        }
    }
    Ok(())
}

/// Resolve the release ID for a tenant, honoring any existing tenant pin and
/// rejecting cross-backend mutations (e.g. a Docker-pinned tenant cannot
/// silently move onto Kubernetes).
async fn resolve_tenant_release_id(
    release_repo: &ReleaseRepository,
    org_id: OrganizationId,
    tenant_id: TenantId,
    target: &ReleaseCompatibilityTarget,
) -> Result<Option<ReleaseId>, ManagerError> {
    if let Some((pinned_release_id, pinned_backend)) =
        release_repo.get_tenant_pin(org_id, tenant_id).await?
    {
        if let Some(backend) = pinned_backend {
            if backend != target.backend {
                return Err(ManagerError::api(
                    err::RELEASE_NOT_RESOLVED,
                    format!(
                        "tenant is pinned to backend '{}' and cannot use '{}'",
                        backend, target.backend
                    ),
                ));
            }
        }
        if let Some(release_id) = pinned_release_id {
            return Ok(Some(release_id));
        }
    }
    release_repo.get_org_binding(org_id).await
}

/// Validate a variant against `target` and resolve its workspace image digest.
///
/// `OPENFLOWS_WORKSPACE_IMAGE_DIGEST` fallback is permitted **only** on local
/// Docker (`!target.require_image_digest && target.backend == Docker`) when
/// `entry.image_digest` is `None`. It never bypasses Kubernetes or hosted
/// digest requirements or backend/architecture mismatches.
pub fn resolve_variant_image_digest(
    entry: &RoleManifestEntry,
    target: &ReleaseCompatibilityTarget,
) -> Result<String, ManagerError> {
    validate_role_variant(entry, target)
        .map_err(|e| ManagerError::api(err::RELEASE_NOT_RESOLVED, e.to_string()))?;

    let digest = match entry.image_digest.as_deref() {
        Some(d) => d.to_string(),
        None if target.backend == ProvisionerBackend::Docker && !target.require_image_digest => {
            std::env::var(WORKSPACE_IMAGE_DIGEST_ENV)
                .ok()
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty())
                .ok_or_else(|| {
                    ManagerError::api(
                        err::RELEASE_NOT_RESOLVED,
                        "template release is missing its workspace image digest; set OPENFLOWS_WORKSPACE_IMAGE_DIGEST",
                    )
                })?
        }
        None => {
            return Err(ManagerError::api(
                err::RELEASE_NOT_RESOLVED,
                "template release is missing required workspace image digest",
            ));
        }
    };

    if !is_valid_image_digest(&digest) {
        return Err(ManagerError::api(
            err::RELEASE_NOT_RESOLVED,
            "template release has an invalid workspace image digest",
        ));
    }
    Ok(digest)
}

async fn build_workspace_spec(
    release_repo: &ReleaseRepository,
    org_id: OrganizationId,
    release_id: ReleaseId,
    role: Role,
    target: &ReleaseCompatibilityTarget,
) -> Result<Option<WorkspaceReleaseSpec>, ManagerError> {
    let Some((_, variants)) = release_repo.get_release_variants(release_id).await? else {
        return Ok(None);
    };
    let entry = variants.get(&(target.backend, role)).ok_or_else(|| {
        ManagerError::api(
            err::RELEASE_NOT_RESOLVED,
            format!(
                "release has no '{}' variant for role '{}'",
                target.backend,
                role.as_str()
            ),
        )
    })?;
    let image_digest = resolve_variant_image_digest(entry, target)?;

    let version_row = release_repo
        .get_org_template_version_for_backend(org_id, release_id, target.backend, role)
        .await?;
    let Some(row) = version_row else {
        return Ok(None);
    };
    if row.status != "ready" {
        return Ok(None);
    }
    let (Some(template_id), Some(version_id)) = (row.coder_template_id, row.coder_version_id)
    else {
        return Ok(None);
    };

    Ok(Some(WorkspaceReleaseSpec {
        template_id: coder_client::CoderTemplateId::new(template_id),
        version_id: coder_client::CoderTemplateVersionId::new(version_id),
        release_id,
        backend: target.backend,
        image_digest,
        image_repository: entry.image_repository.clone(),
        architecture: entry.architecture.clone(),
        workspace_runtime_release: entry.workspace_runtime_release.clone(),
        parameter_schema_version: entry.parameter_schema_version,
    }))
}

/// Resolve and validate the Nexus template release for `tenant_id`.
///
/// Returns `Ok(None)` when the organization has no published/ready release yet,
/// and `Err(RELEASE_NOT_RESOLVED)` when a tenant pin conflicts with `target` or
/// the release variant fails compatibility validation.
pub async fn resolve_nexus_release_for_tenant(
    _pool: &PgPool,
    release_repo: &ReleaseRepository,
    org_id: OrganizationId,
    tenant_id: TenantId,
    target: &ReleaseCompatibilityTarget,
) -> Result<Option<WorkspaceReleaseSpec>, ManagerError> {
    let Some(release_id) =
        resolve_tenant_release_id(release_repo, org_id, tenant_id, target).await?
    else {
        return Ok(None);
    };
    build_workspace_spec(release_repo, org_id, release_id, Role::Nexus, target).await
}

/// Resolve and validate a worker role's template release for `tenant_id`.
///
/// Fails early with `RELEASE_NOT_RESOLVED` if the tenant's pin conflicts with
/// `target.backend`, the variant is missing/incompatible, or the organization's
/// template version for `(target.backend, role)` is not `"ready"`.
pub async fn resolve_worker_release_for_tenant(
    _pool: &PgPool,
    release_repo: &ReleaseRepository,
    org_id: OrganizationId,
    tenant_id: TenantId,
    role: Role,
    target: &ReleaseCompatibilityTarget,
) -> Result<WorkspaceReleaseSpec, ManagerError> {
    let release_id = resolve_tenant_release_id(release_repo, org_id, tenant_id, target)
        .await?
        .ok_or_else(|| {
            ManagerError::api(
                err::RELEASE_NOT_RESOLVED,
                "template release is not published",
            )
        })?;
    build_workspace_spec(release_repo, org_id, release_id, role, target)
        .await?
        .ok_or_else(|| {
            ManagerError::api(err::RELEASE_NOT_RESOLVED, "template version is not ready")
        })
}

/// Persist the tenant pin (if unset) and the workspace pin for a newly created
/// workspace.
pub async fn record_workspace_and_tenant_pin(
    _pool: &PgPool,
    release_repo: &ReleaseRepository,
    org_id: OrganizationId,
    tenant_id: TenantId,
    workspace_id: WorkspaceId,
    spec: &WorkspaceReleaseSpec,
) -> Result<(), ManagerError> {
    release_repo
        .set_tenant_pin_if_unset(org_id, tenant_id, spec.release_id, spec.backend)
        .await?;
    release_repo
        .set_workspace_pin(
            org_id,
            workspace_id,
            spec.release_id,
            spec.backend,
            Some(&spec.version_id.0),
        )
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn entry(backend: ProvisionerBackend, digest: Option<String>) -> RoleManifestEntry {
        RoleManifestEntry {
            role: Role::Nexus,
            backend,
            archive: Role::Nexus.archive_name_for_backend(backend),
            sha256: "a".repeat(64),
            parameter_schema_version: 2,
            architecture: "amd64".to_string(),
            image_repository: Some("ghcr.io/openflows/workspace-runtime".to_string()),
            image_digest: digest,
            workspace_runtime_release: Some("2026.11.0".to_string()),
        }
    }

    #[test]
    fn kubernetes_never_falls_back_to_env_digest() {
        let _guard = ENV_LOCK.lock().unwrap();
        let prev = std::env::var_os(WORKSPACE_IMAGE_DIGEST_ENV);
        std::env::set_var(
            WORKSPACE_IMAGE_DIGEST_ENV,
            format!("sha256:{}", "c".repeat(64)),
        );

        let target = ReleaseCompatibilityTarget::kubernetes_default("amd64");
        let res =
            resolve_variant_image_digest(&entry(ProvisionerBackend::Kubernetes, None), &target);
        assert!(res.is_err());

        match prev {
            Some(v) => std::env::set_var(WORKSPACE_IMAGE_DIGEST_ENV, v),
            None => std::env::remove_var(WORKSPACE_IMAGE_DIGEST_ENV),
        }
    }

    #[test]
    fn hosted_docker_never_falls_back_to_env_digest() {
        let _guard = ENV_LOCK.lock().unwrap();
        let prev = std::env::var_os(WORKSPACE_IMAGE_DIGEST_ENV);
        std::env::set_var(
            WORKSPACE_IMAGE_DIGEST_ENV,
            format!("sha256:{}", "c".repeat(64)),
        );

        let mut target = ReleaseCompatibilityTarget::docker_default();
        target.require_image_digest = true;
        let res = resolve_variant_image_digest(&entry(ProvisionerBackend::Docker, None), &target);
        assert!(res.is_err());

        match prev {
            Some(v) => std::env::set_var(WORKSPACE_IMAGE_DIGEST_ENV, v),
            None => std::env::remove_var(WORKSPACE_IMAGE_DIGEST_ENV),
        }
    }

    #[test]
    fn local_docker_uses_env_fallback_when_entry_digest_missing() {
        let _guard = ENV_LOCK.lock().unwrap();
        let prev = std::env::var_os(WORKSPACE_IMAGE_DIGEST_ENV);
        let expected = format!("sha256:{}", "c".repeat(64));
        std::env::set_var(WORKSPACE_IMAGE_DIGEST_ENV, &expected);

        let target = ReleaseCompatibilityTarget::docker_default();
        let actual =
            resolve_variant_image_digest(&entry(ProvisionerBackend::Docker, None), &target)
                .unwrap();
        assert_eq!(actual, expected);

        match prev {
            Some(v) => std::env::set_var(WORKSPACE_IMAGE_DIGEST_ENV, v),
            None => std::env::remove_var(WORKSPACE_IMAGE_DIGEST_ENV),
        }
    }
}

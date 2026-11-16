//! Operator-configured release artifact source.
//!
//! The template manifest and role archives are operator configuration, never
//! customer-supplied. `ReleaseProvider` locates the manifest and source
//! directories from environment configuration, packages the role archives
//! deterministically, and validates every archive digest against the manifest
//! before the release is imported.

use crate::error::ManagerError;
use crate::templates::backend::{
    is_valid_image_digest, validate_architecture, DEFAULT_ARCHITECTURE,
};
use crate::templates::packaging::{
    build_deterministic_archive, validate_archive, verify_archive_digest, Role, RoleManifestEntry,
    TemplateManifest,
};
use crate::templates::service::ReleaseArtifacts;
use crate::templates::ProvisionerBackend;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Error describing a release-source configuration or validation failure.
#[derive(Debug)]
pub struct ReleaseProviderError(pub String);

impl std::fmt::Display for ReleaseProviderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ReleaseProviderError {}

impl From<ReleaseProviderError> for ManagerError {
    fn from(e: ReleaseProviderError) -> Self {
        ManagerError::Config(e.0)
    }
}

/// Environment keys for the operator-configured release source.
pub const TEMPLATE_SOURCE_DIR_ENV: &str = "OPENFLOWS_TEMPLATE_SOURCE_DIR";
pub const TEMPLATE_MANIFEST_ENV: &str = "OPENFLOWS_TEMPLATE_MANIFEST";
pub const TEMPLATE_MANIFEST_SHA256_ENV: &str = "OPENFLOWS_TEMPLATE_MANIFEST_SHA256";
pub const RELEASE_NAME_ENV: &str = "OPENFLOWS_RELEASE_NAME";

/// Loads the operator-configured release artifacts.
#[derive(Clone)]
pub struct ReleaseProvider {
    source_dir: PathBuf,
    manifest_path: Option<PathBuf>,
    manifest_sha256: Option<String>,
    release_name: String,
}

impl ReleaseProvider {
    /// Build a provider from environment configuration. `source_dir` defaults
    /// to the checked-in `crates/coder-client/templates` directory.
    pub fn from_env() -> Result<Self, ReleaseProviderError> {
        let source_dir = std::env::var_os(TEMPLATE_SOURCE_DIR_ENV)
            .map(PathBuf::from)
            .unwrap_or_else(default_source_dir);
        let manifest_path = std::env::var_os(TEMPLATE_MANIFEST_ENV).map(PathBuf::from);
        let manifest_sha256 = std::env::var(TEMPLATE_MANIFEST_SHA256_ENV).ok();
        let release_name = std::env::var(RELEASE_NAME_ENV).unwrap_or_else(|_| "2026.10.0".into());
        Ok(ReleaseProvider {
            source_dir,
            manifest_path,
            manifest_sha256,
            release_name,
        })
    }

    /// Build a provider for tests/operator tooling from explicit paths.
    pub fn new(
        source_dir: PathBuf,
        manifest_path: Option<PathBuf>,
        manifest_sha256: Option<String>,
        release_name: String,
    ) -> Self {
        ReleaseProvider {
            source_dir,
            manifest_path,
            manifest_sha256,
            release_name,
        }
    }

    /// Package and validate the configured release. Returns the manifest and
    /// the deterministic role archives.
    pub fn load(&self) -> Result<ReleaseArtifacts, ReleaseProviderError> {
        let manifest = self.load_manifest()?;
        let mut variant_archives = BTreeMap::new();
        for ((backend, role), entry) in manifest.all_variants() {
            let dir = role_source_dir(&self.source_dir, backend, role).ok_or_else(|| {
                ReleaseProviderError(format!(
                    "template source directory missing for role '{}' ({})",
                    role.as_str(),
                    backend
                ))
            })?;
            let archive = build_deterministic_archive(&dir).map_err(|e| {
                ReleaseProviderError(format!(
                    "failed to package role '{}' ({}): {e}",
                    role.as_str(),
                    backend
                ))
            })?;
            validate_archive(&archive).map_err(|e| {
                ReleaseProviderError(format!(
                    "packaged role '{}' ({}) failed validation: {e}",
                    role.as_str(),
                    backend
                ))
            })?;
            verify_archive_digest(&archive, &entry.sha256).map_err(|e| {
                ReleaseProviderError(format!(
                    "role '{}' ({}) archive does not match manifest digest: {e}",
                    role.as_str(),
                    backend
                ))
            })?;
            variant_archives.insert((backend, role), archive);
        }

        for backend in manifest.backends() {
            for role in &manifest.required_roles {
                if !variant_archives.contains_key(&(backend, *role)) {
                    return Err(ReleaseProviderError(format!(
                        "manifest missing required role '{}' for backend '{}'",
                        role.as_str(),
                        backend
                    )));
                }
            }
        }

        Ok(ReleaseArtifacts::from_backend_archives(
            manifest,
            variant_archives,
        ))
    }

    /// The immutable release identity the operator pinned (manifest digest), if
    /// provided. Used to reject a changed artifact source at startup.
    pub fn pinned_manifest_sha256(&self) -> Option<&str> {
        self.manifest_sha256.as_deref()
    }

    /// The local source directory this provider reads from.
    pub fn source_dir(&self) -> &PathBuf {
        &self.source_dir
    }

    fn load_manifest(&self) -> Result<TemplateManifest, ReleaseProviderError> {
        let bytes = match &self.manifest_path {
            Some(path) => std::fs::read(path).map_err(|e| {
                ReleaseProviderError(format!(
                    "cannot read template manifest '{}': {e}",
                    path.display()
                ))
            })?,
            // No explicit manifest: build one deterministically from the source
            // dir, computing each role's archive digest.
            None => self.build_manifest_from_source()?,
        };
        let manifest = TemplateManifest::parse(&bytes)
            .map_err(|e| ReleaseProviderError(format!("template manifest is invalid: {e}")))?;

        // Verify the operator-pinned digest, when configured.
        if let Some(expected) = &self.manifest_sha256 {
            let actual = manifest
                .manifest_sha256()
                .map_err(|e| ReleaseProviderError(format!("cannot hash manifest: {e}")))?;
            if &actual != expected {
                return Err(ReleaseProviderError(format!(
                    "template manifest digest mismatch: expected {expected}, got {actual}"
                )));
            }
        }
        Ok(manifest)
    }

    fn build_manifest_from_source(&self) -> Result<Vec<u8>, ReleaseProviderError> {
        let image_digest = non_empty_env("OPENFLOWS_WORKSPACE_IMAGE_DIGEST");
        if let Some(digest) = image_digest.as_deref() {
            if !is_valid_image_digest(digest) {
                return Err(ReleaseProviderError(
                    "OPENFLOWS_WORKSPACE_IMAGE_DIGEST must be sha256 followed by 64 hexadecimal characters"
                        .into(),
                ));
            }
        }
        let architecture = validate_architecture(
            non_empty_env("OPENFLOWS_WORKSPACE_ARCH")
                .as_deref()
                .unwrap_or(DEFAULT_ARCHITECTURE),
        )
        .map_err(|e| ReleaseProviderError(e.to_string()))?;
        let image_repository = non_empty_env("OPENFLOWS_WORKSPACE_IMAGE_REPO");
        let workspace_runtime_release = non_empty_env("OPENFLOWS_WORKSPACE_RUNTIME_RELEASE");

        // The Kubernetes variants land role by role (issue #500). A manifest is
        // only valid when every backend it lists provides all required roles, so
        // the backend appears once all five sources exist; until then a source
        // install keeps producing a Docker-only manifest.
        let has_kubernetes = Role::ALL.iter().all(|role| {
            role_source_dir(&self.source_dir, ProvisionerBackend::Kubernetes, *role).is_some()
        });
        let is_v2 = has_kubernetes
            || image_repository.is_some()
            || workspace_runtime_release.is_some()
            || architecture != DEFAULT_ARCHITECTURE;
        let param_schema_version = if is_v2 { 2 } else { 1 };

        let mut docker_roles = BTreeMap::new();
        let mut variants = BTreeMap::new();

        for role in Role::ALL {
            let dir = role_source_dir(&self.source_dir, ProvisionerBackend::Docker, role)
                .ok_or_else(|| {
                    ReleaseProviderError(format!(
                        "template source directory missing for role '{}' (docker)",
                        role.as_str()
                    ))
                })?;
            let archive = build_deterministic_archive(&dir).map_err(|e| {
                ReleaseProviderError(format!("failed to package role '{}': {e}", role.as_str()))
            })?;
            let sha256 = crate::templates::packaging::sha256_hex(&archive);
            let archive_name = if is_v2 && has_kubernetes {
                role.archive_name_for_backend(ProvisionerBackend::Docker)
            } else {
                role.archive_name()
            };
            let entry = RoleManifestEntry {
                role,
                backend: ProvisionerBackend::Docker,
                archive: archive_name,
                sha256,
                parameter_schema_version: param_schema_version,
                architecture: architecture.clone(),
                image_repository: image_repository.clone(),
                image_digest: image_digest.clone(),
                workspace_runtime_release: workspace_runtime_release.clone(),
            };
            docker_roles.insert(role, entry.clone());
            variants.insert((ProvisionerBackend::Docker, role), entry);
        }

        if has_kubernetes {
            for role in Role::ALL {
                let dir = role_source_dir(&self.source_dir, ProvisionerBackend::Kubernetes, role)
                    .ok_or_else(|| {
                    ReleaseProviderError(format!(
                        "template source directory missing for role '{}' (kubernetes)",
                        role.as_str()
                    ))
                })?;
                let archive = build_deterministic_archive(&dir).map_err(|e| {
                    ReleaseProviderError(format!(
                        "failed to package role '{}' (kubernetes): {e}",
                        role.as_str()
                    ))
                })?;
                let sha256 = crate::templates::packaging::sha256_hex(&archive);
                let entry = RoleManifestEntry {
                    role,
                    backend: ProvisionerBackend::Kubernetes,
                    archive: role.archive_name_for_backend(ProvisionerBackend::Kubernetes),
                    sha256,
                    parameter_schema_version: 2,
                    architecture: architecture.clone(),
                    image_repository: image_repository.clone(),
                    image_digest: image_digest.clone(),
                    workspace_runtime_release: workspace_runtime_release.clone(),
                };
                variants.insert((ProvisionerBackend::Kubernetes, role), entry);
            }
        }

        let required_roles = vec![Role::Nexus, Role::Forge, Role::Sentinel, Role::Vessel];
        let manifest = if is_v2 {
            TemplateManifest {
                schema_version: TemplateManifest::CURRENT_V2_SCHEMA_VERSION,
                release: self.release_name.clone(),
                required_roles,
                roles: docker_roles,
                variants,
            }
        } else {
            TemplateManifest::from_v1_roles(self.release_name.clone(), required_roles, docker_roles)
        };
        manifest
            .to_canonical_json()
            .map_err(|e| ReleaseProviderError(format!("cannot serialize manifest: {e}")))
    }
}

fn non_empty_env(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

fn role_source_dir(source_dir: &Path, backend: ProvisionerBackend, role: Role) -> Option<PathBuf> {
    let role_dir_name = format!("openflows-{}", role.as_str());
    match backend {
        ProvisionerBackend::Docker => {
            let nested = source_dir.join("docker").join(&role_dir_name);
            if nested.is_dir() {
                return Some(nested);
            }
            let flat = source_dir.join(&role_dir_name);
            flat.is_dir().then_some(flat)
        }
        ProvisionerBackend::Kubernetes => {
            let nested = source_dir.join("kubernetes").join(&role_dir_name);
            nested.is_dir().then_some(nested)
        }
    }
}

/// Default source directory: the checked-in coder-client templates.
fn default_source_dir() -> PathBuf {
    // Best-effort: relative to the workspace root. Callers can override with
    // OPENFLOWS_TEMPLATE_SOURCE_DIR.
    PathBuf::from("crates/coder-client/templates")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A source tree with every role's Docker template, plus the Kubernetes
    /// roles named in `kubernetes_roles`.
    fn source_tree(kubernetes_roles: &[&str]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        for role in ["forge", "sentinel", "nexus", "vessel", "lore"] {
            let docker = dir.path().join(format!("openflows-{role}"));
            std::fs::create_dir_all(&docker).unwrap();
            std::fs::write(docker.join("main.tf"), "terraform {}\n").unwrap();
        }
        for role in kubernetes_roles {
            let kubernetes = dir
                .path()
                .join("kubernetes")
                .join(format!("openflows-{role}"));
            std::fs::create_dir_all(&kubernetes).unwrap();
            std::fs::write(kubernetes.join("main.tf"), "terraform {}\n").unwrap();
        }
        dir
    }

    fn provider(dir: &tempfile::TempDir) -> ReleaseProvider {
        ReleaseProvider::new(dir.path().to_path_buf(), None, None, "test".into())
    }

    #[test]
    fn a_partial_kubernetes_set_leaves_the_source_manifest_docker_only() {
        // A manifest that lists a backend must carry every required role for it,
        // so one Kubernetes template must not introduce the backend yet.
        let dir = source_tree(&["nexus"]);
        let artifacts = provider(&dir).load().unwrap();
        assert_eq!(
            artifacts.manifest.backends(),
            vec![ProvisionerBackend::Docker]
        );
    }

    #[test]
    fn the_checked_in_templates_publish_both_backends() {
        // The tree ships every role for both backends; a source install must
        // advertise both, or a Kubernetes tenant could never resolve a release.
        let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../coder-client/templates");
        let provider = ReleaseProvider::new(source, None, None, "test".into());
        let artifacts = provider.load().unwrap();
        assert_eq!(
            artifacts.manifest.backends(),
            vec![ProvisionerBackend::Docker, ProvisionerBackend::Kubernetes]
        );
        for role in Role::ALL {
            assert!(
                artifacts
                    .manifest
                    .variant(ProvisionerBackend::Kubernetes, role)
                    .is_some(),
                "{} is missing its Kubernetes variant",
                role.as_str()
            );
            assert!(
                artifacts
                    .archive_for(ProvisionerBackend::Kubernetes, role)
                    .is_some(),
                "{} Kubernetes archive was not packaged",
                role.as_str()
            );
        }
    }

    #[test]
    fn a_complete_kubernetes_set_publishes_kubernetes_variants() {
        let dir = source_tree(&["forge", "sentinel", "nexus", "vessel", "lore"]);
        let artifacts = provider(&dir).load().unwrap();
        assert_eq!(
            artifacts.manifest.backends(),
            vec![ProvisionerBackend::Docker, ProvisionerBackend::Kubernetes]
        );
        for role in Role::ALL {
            let entry = artifacts
                .manifest
                .variant(ProvisionerBackend::Kubernetes, role)
                .expect("every role has a Kubernetes variant");
            assert_eq!(
                entry.archive,
                role.archive_name_for_backend(ProvisionerBackend::Kubernetes)
            );
        }
    }
}

//! Provisioner backend types and release compatibility validation.
//!
//! Each template release can carry variants for multiple provisioner backends
//! (such as `docker` and `kubernetes`). Before publishing a release to an
//! organization or creating a workspace, Manager validates every required role
//! variant against the target [`ReleaseCompatibilityTarget`].

use crate::error::ManagerError;
use crate::templates::packaging::RoleManifestEntry;
use std::fmt;
use std::str::FromStr;

/// Canonical default CPU architecture for template release variants.
pub const DEFAULT_ARCHITECTURE: &str = "amd64";

/// Minimum supported parameter schema version.
pub const MIN_PARAMETER_SCHEMA_VERSION: u32 = 1;

/// Maximum supported parameter schema version.
pub const MAX_PARAMETER_SCHEMA_VERSION: u32 = 2;

/// Supported workspace provisioner backends.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "lowercase")]
pub enum ProvisionerBackend {
    Docker,
    Kubernetes,
}

impl ProvisionerBackend {
    pub const ALL: [ProvisionerBackend; 2] =
        [ProvisionerBackend::Docker, ProvisionerBackend::Kubernetes];

    pub fn as_str(self) -> &'static str {
        match self {
            ProvisionerBackend::Docker => "docker",
            ProvisionerBackend::Kubernetes => "kubernetes",
        }
    }
}

impl fmt::Display for ProvisionerBackend {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for ProvisionerBackend {
    type Err = ManagerError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "docker" => Ok(ProvisionerBackend::Docker),
            "kubernetes" => Ok(ProvisionerBackend::Kubernetes),
            other => Err(ManagerError::InvalidInput(format!(
                "unknown provisioner backend '{other}' (expected 'docker' or 'kubernetes')"
            ))),
        }
    }
}

/// Validate and canonicalize a target CPU architecture string.
///
/// Canonical values are `"amd64"` and `"arm64"`. `"x86_64"` is normalized to
/// `"amd64"`.
pub fn validate_architecture(arch: &str) -> Result<String, ManagerError> {
    match arch {
        "amd64" | "x86_64" => Ok("amd64".to_string()),
        "arm64" => Ok("arm64".to_string()),
        other => Err(ManagerError::InvalidInput(format!(
            "unsupported template architecture '{other}' (expected 'amd64' or 'arm64')"
        ))),
    }
}

/// Check that `digest` has the form `sha256:<64 hex chars>`.
pub fn is_valid_image_digest(digest: &str) -> bool {
    let Some(hex) = digest.strip_prefix("sha256:") else {
        return false;
    };
    hex.len() == 64 && hex.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Target cluster/provisioner capabilities used to validate a release variant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseCompatibilityTarget {
    pub backend: ProvisionerBackend,
    pub architecture: String,
    pub min_parameter_schema_version: u32,
    pub max_parameter_schema_version: u32,
    pub expected_runtime_release: Option<String>,
    pub require_image_digest: bool,
}

impl ReleaseCompatibilityTarget {
    /// Default compatibility target for the local Docker provisioner.
    pub fn docker_default() -> Self {
        Self {
            backend: ProvisionerBackend::Docker,
            architecture: DEFAULT_ARCHITECTURE.to_string(),
            min_parameter_schema_version: MIN_PARAMETER_SCHEMA_VERSION,
            max_parameter_schema_version: MAX_PARAMETER_SCHEMA_VERSION,
            expected_runtime_release: None,
            require_image_digest: false,
        }
    }

    /// Compatibility target for Kubernetes with pinned image digests required.
    pub fn kubernetes_default(architecture: impl Into<String>) -> Self {
        Self {
            backend: ProvisionerBackend::Kubernetes,
            architecture: architecture.into(),
            min_parameter_schema_version: MIN_PARAMETER_SCHEMA_VERSION,
            max_parameter_schema_version: MAX_PARAMETER_SCHEMA_VERSION,
            expected_runtime_release: None,
            require_image_digest: true,
        }
    }
}

impl Default for ReleaseCompatibilityTarget {
    fn default() -> Self {
        Self::docker_default()
    }
}

/// Validate a single `(backend, role)` manifest entry against `target`.
pub fn validate_role_variant(
    entry: &RoleManifestEntry,
    target: &ReleaseCompatibilityTarget,
) -> Result<(), ManagerError> {
    if entry.backend != target.backend {
        return Err(ManagerError::InvalidInput(format!(
            "role '{}' variant backend '{}' does not match target backend '{}'",
            entry.role, entry.backend, target.backend
        )));
    }

    let entry_arch = validate_architecture(&entry.architecture)?;
    let target_arch = validate_architecture(&target.architecture)?;
    if entry_arch != target_arch {
        return Err(ManagerError::InvalidInput(format!(
            "role '{}' ({}) architecture '{}' does not match target architecture '{}'",
            entry.role, entry.backend, entry_arch, target_arch
        )));
    }

    if entry.parameter_schema_version < target.min_parameter_schema_version
        || entry.parameter_schema_version > target.max_parameter_schema_version
    {
        return Err(ManagerError::InvalidInput(format!(
            "role '{}' ({}) parameter_schema_version {} is outside supported range {}..={}",
            entry.role,
            entry.backend,
            entry.parameter_schema_version,
            target.min_parameter_schema_version,
            target.max_parameter_schema_version
        )));
    }

    if target.require_image_digest || target.backend == ProvisionerBackend::Kubernetes {
        match entry.image_digest.as_deref() {
            Some(digest) if is_valid_image_digest(digest) => {}
            Some(digest) => {
                return Err(ManagerError::InvalidInput(format!(
                    "role '{}' ({}) has invalid image_digest '{digest}' (expected 'sha256:<64 hex>')",
                    entry.role, entry.backend
                )));
            }
            None => {
                return Err(ManagerError::InvalidInput(format!(
                    "role '{}' ({}) is missing required pinned image_digest",
                    entry.role, entry.backend
                )));
            }
        }
    }

    if let Some(expected_runtime) = target.expected_runtime_release.as_deref() {
        match entry.workspace_runtime_release.as_deref() {
            Some(actual) if actual == expected_runtime => {}
            Some(actual) => {
                return Err(ManagerError::InvalidInput(format!(
                    "role '{}' ({}) workspace_runtime_release '{}' does not match expected '{}'",
                    entry.role, entry.backend, actual, expected_runtime
                )));
            }
            None => {
                return Err(ManagerError::InvalidInput(format!(
                    "role '{}' ({}) is missing required workspace_runtime_release '{}'",
                    entry.role, entry.backend, expected_runtime
                )));
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::templates::packaging::Role;

    fn sample_entry(backend: ProvisionerBackend, role: Role) -> RoleManifestEntry {
        RoleManifestEntry {
            role,
            backend,
            archive: role.archive_name_for_backend(backend),
            sha256: "a".repeat(64),
            parameter_schema_version: 2,
            architecture: "amd64".to_string(),
            image_repository: Some("ghcr.io/openflows/workspace-runtime".to_string()),
            image_digest: Some(format!("sha256:{}", "b".repeat(64))),
            workspace_runtime_release: Some("2026.11.0".to_string()),
        }
    }

    #[test]
    fn backend_from_str_is_strict() {
        assert_eq!(
            "docker".parse::<ProvisionerBackend>().unwrap(),
            ProvisionerBackend::Docker
        );
        assert_eq!(
            "kubernetes".parse::<ProvisionerBackend>().unwrap(),
            ProvisionerBackend::Kubernetes
        );
        assert!("Docker".parse::<ProvisionerBackend>().is_err());
        assert!("k8s".parse::<ProvisionerBackend>().is_err());
        assert!("vm".parse::<ProvisionerBackend>().is_err());
    }

    #[test]
    fn architecture_validation_normalizes_and_rejects() {
        assert_eq!(validate_architecture("amd64").unwrap(), "amd64");
        assert_eq!(validate_architecture("x86_64").unwrap(), "amd64");
        assert_eq!(validate_architecture("arm64").unwrap(), "arm64");
        assert!(validate_architecture("386").is_err());
        assert!(validate_architecture("ARM64").is_err());
    }

    #[test]
    fn validate_role_variant_accepts_all_roles_for_docker_and_kubernetes() {
        for backend in ProvisionerBackend::ALL {
            let mut target = ReleaseCompatibilityTarget {
                backend,
                architecture: "amd64".to_string(),
                min_parameter_schema_version: 1,
                max_parameter_schema_version: 2,
                expected_runtime_release: Some("2026.11.0".to_string()),
                require_image_digest: backend == ProvisionerBackend::Kubernetes,
            };
            if backend == ProvisionerBackend::Docker {
                target.require_image_digest = false;
            }
            for role in Role::ALL {
                let entry = sample_entry(backend, role);
                validate_role_variant(&entry, &target).unwrap();
            }
        }
    }

    #[test]
    fn validate_role_variant_rejects_mismatches() {
        let k8s_target = ReleaseCompatibilityTarget {
            backend: ProvisionerBackend::Kubernetes,
            architecture: "amd64".to_string(),
            min_parameter_schema_version: 1,
            max_parameter_schema_version: 2,
            expected_runtime_release: Some("2026.11.0".to_string()),
            require_image_digest: true,
        };

        // Backend mismatch
        let docker_entry = sample_entry(ProvisionerBackend::Docker, Role::Nexus);
        assert!(validate_role_variant(&docker_entry, &k8s_target).is_err());

        // Architecture mismatch
        let mut arm_entry = sample_entry(ProvisionerBackend::Kubernetes, Role::Forge);
        arm_entry.architecture = "arm64".to_string();
        assert!(validate_role_variant(&arm_entry, &k8s_target).is_err());

        // Unsupported parameter_schema_version
        let mut bad_schema = sample_entry(ProvisionerBackend::Kubernetes, Role::Sentinel);
        bad_schema.parameter_schema_version = 3;
        assert!(validate_role_variant(&bad_schema, &k8s_target).is_err());

        // Missing image_digest on Kubernetes
        let mut missing_digest = sample_entry(ProvisionerBackend::Kubernetes, Role::Vessel);
        missing_digest.image_digest = None;
        assert!(validate_role_variant(&missing_digest, &k8s_target).is_err());

        // Malformed image_digest on Kubernetes
        let mut bad_digest = sample_entry(ProvisionerBackend::Kubernetes, Role::Lore);
        bad_digest.image_digest = Some("sha256:not-64-hex".to_string());
        assert!(validate_role_variant(&bad_digest, &k8s_target).is_err());

        // Runtime release mismatch
        let mut bad_runtime = sample_entry(ProvisionerBackend::Kubernetes, Role::Nexus);
        bad_runtime.workspace_runtime_release = Some("2026.10.0".to_string());
        assert!(validate_role_variant(&bad_runtime, &k8s_target).is_err());

        // Missing runtime release when expected
        let mut missing_runtime = sample_entry(ProvisionerBackend::Kubernetes, Role::Nexus);
        missing_runtime.workspace_runtime_release = None;
        assert!(validate_role_variant(&missing_runtime, &k8s_target).is_err());
    }
}

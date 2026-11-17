//! Parsing and compatibility validation for `workspace-runtime` release
//! manifests (`release-manifest.json`, schema version 2).

use crate::error::ManagerError;
use crate::templates::backend::{
    is_valid_image_digest, validate_architecture, ReleaseCompatibilityTarget,
};
use crate::templates::packaging::{Role, TemplateManifest};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

pub const WORKSPACE_RUNTIME_MANIFEST_SCHEMA_VERSION: u32 = 2;
pub const WORKSPACE_RUNTIME_BOOTSTRAP_CONTRACT_VERSION: u32 = 2;
pub const EXPECTED_ASSET_INSTALL_COMMAND: &str = "openflows-runtime install";
pub const EXPECTED_ROLE_ENV_VAR: &str = "OPENFLOWS_ROLE";
pub const EXPECTED_CREDENTIAL_FILE: &str = "/run/openflows/runtime.json";
pub const EXPECTED_ASSET_DIRECTORY: &str = ".openflows/artifacts";
pub const EXPECTED_HARNESS_NAME: &str = "openflows-harness";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WorkspaceRuntimeStatus {
    Candidate,
    Compatible,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceRuntimeImage {
    pub repository: String,
    pub digest: String,
    pub architectures: Vec<String>,
}

impl WorkspaceRuntimeImage {
    /// Returns the immutable `<repository>@<digest>` image reference.
    pub fn image_reference(&self) -> String {
        format!("{}@{}", self.repository, self.digest)
    }

    /// Returns true if this image supports the given canonical architecture (e.g. `amd64`).
    pub fn supports_architecture(&self, arch: &str) -> bool {
        let Ok(canonical) = validate_architecture(arch) else {
            return false;
        };
        let linux_platform = format!("linux/{canonical}");
        self.architectures.iter().any(|a| a == &linux_platform)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceRuntimeBootstrap {
    pub contract_version: u32,
    pub asset_install_command: String,
    pub role_environment_variable: String,
    pub credential_file: String,
    pub asset_directory: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceRuntimeHarness {
    pub name: String,
    pub openflows_release: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceRuntimeCompatibility {
    pub openflows_template_parameter_schema_versions: Vec<u32>,
    pub minimum_manager_version: Option<String>,
}

pub const DEFAULT_ALLOWED_REPO_PREFIXES: &[&str] = &["ghcr.io/the-agenticflow/"];
pub const WORKSPACE_RUNTIME_ALLOWED_REPO_PREFIXES_ENV: &str =
    "OPENFLOWS_WORKSPACE_RUNTIME_ALLOWED_REPO_PREFIXES";

/// Resolve the allowed OCI repository prefixes from
/// `OPENFLOWS_WORKSPACE_RUNTIME_ALLOWED_REPO_PREFIXES` (comma-separated),
/// defaulting to [`DEFAULT_ALLOWED_REPO_PREFIXES`].
pub fn allowed_repo_prefixes_from_env() -> Vec<String> {
    let custom: Vec<String> = std::env::var(WORKSPACE_RUNTIME_ALLOWED_REPO_PREFIXES_ENV)
        .ok()
        .map(|raw| {
            raw.split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(ToString::to_string)
                .collect()
        })
        .unwrap_or_default();
    if custom.is_empty() {
        DEFAULT_ALLOWED_REPO_PREFIXES
            .iter()
            .map(|s| (*s).to_string())
            .collect()
    } else {
        custom
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceRuntimeCompatibilityOptions {
    /// When true, requires `status == "compatible"` and at least one compatible
    /// entry in `openflows_template_parameter_schema_versions`.
    pub strict_compatibility: bool,
    pub manager_version: String,
    /// Allowed OCI repository prefixes (e.g. `["ghcr.io/the-agenticflow/"]`).
    pub allowed_repository_prefixes: Vec<String>,
}

impl Default for WorkspaceRuntimeCompatibilityOptions {
    fn default() -> Self {
        Self {
            strict_compatibility: true,
            manager_version: env!("CARGO_PKG_VERSION").to_string(),
            allowed_repository_prefixes: DEFAULT_ALLOWED_REPO_PREFIXES
                .iter()
                .map(|s| (*s).to_string())
                .collect(),
        }
    }
}

impl WorkspaceRuntimeCompatibilityOptions {
    pub fn allow_candidate() -> Self {
        Self {
            strict_compatibility: false,
            ..Self::default()
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceRuntimeManifest {
    pub schema_version: u32,
    pub runtime_version: String,
    pub status: WorkspaceRuntimeStatus,
    pub images: BTreeMap<Role, WorkspaceRuntimeImage>,
    pub roles: Vec<Role>,
    pub bootstrap: WorkspaceRuntimeBootstrap,
    pub harness: WorkspaceRuntimeHarness,
    pub compatibility: WorkspaceRuntimeCompatibility,
}

impl WorkspaceRuntimeManifest {
    pub fn parse(bytes: &[u8]) -> Result<Self, ManagerError> {
        let s = std::str::from_utf8(bytes).map_err(|e| {
            ManagerError::InvalidInput(format!(
                "workspace-runtime manifest is not valid UTF-8: {e}"
            ))
        })?;
        Self::parse_str(s)
    }

    pub fn parse_str(raw: &str) -> Result<Self, ManagerError> {
        let manifest: Self = serde_json::from_str(raw).map_err(|e| {
            ManagerError::InvalidInput(format!("invalid workspace-runtime manifest JSON: {e}"))
        })?;
        manifest.validate_structure()?;
        Ok(manifest)
    }

    fn validate_structure(&self) -> Result<(), ManagerError> {
        if self.schema_version != WORKSPACE_RUNTIME_MANIFEST_SCHEMA_VERSION {
            return Err(ManagerError::InvalidInput(format!(
                "unsupported workspace-runtime manifest schema_version {} (expected {})",
                self.schema_version, WORKSPACE_RUNTIME_MANIFEST_SCHEMA_VERSION
            )));
        }

        parse_semver(&self.runtime_version).ok_or_else(|| {
            ManagerError::InvalidInput(format!(
                "invalid workspace-runtime runtime_version '{}' (expected semver)",
                self.runtime_version
            ))
        })?;

        let role_set: BTreeSet<Role> = self.roles.iter().copied().collect();
        if role_set.len() != self.roles.len() || role_set.len() != Role::ALL.len() {
            return Err(ManagerError::InvalidInput(
                "workspace-runtime manifest roles must contain all 5 unique roles".into(),
            ));
        }

        for role in Role::ALL {
            let img = self.images.get(&role).ok_or_else(|| {
                ManagerError::InvalidInput(format!(
                    "workspace-runtime manifest missing image entry for role '{}'",
                    role.as_str()
                ))
            })?;
            let expected_suffix = format!("workspace-runtime-{}", role.as_str());
            if !is_valid_oci_repository(&img.repository)
                || !img.repository.ends_with(&expected_suffix)
            {
                return Err(ManagerError::InvalidInput(format!(
                    "role '{}' repository '{}' must be a valid OCI repository ending with '{}'",
                    role.as_str(),
                    img.repository,
                    expected_suffix
                )));
            }
            if !is_valid_image_digest(&img.digest) {
                return Err(ManagerError::InvalidInput(format!(
                    "role '{}' has invalid image digest '{}'",
                    role.as_str(),
                    img.digest
                )));
            }
            if img.architectures.is_empty() {
                return Err(ManagerError::InvalidInput(format!(
                    "role '{}' image architectures list cannot be empty",
                    role.as_str()
                )));
            }
        }

        if self.bootstrap.contract_version != WORKSPACE_RUNTIME_BOOTSTRAP_CONTRACT_VERSION {
            return Err(ManagerError::InvalidInput(format!(
                "unsupported workspace-runtime bootstrap contract_version {} (expected {})",
                self.bootstrap.contract_version, WORKSPACE_RUNTIME_BOOTSTRAP_CONTRACT_VERSION
            )));
        }
        if self.bootstrap.asset_install_command != EXPECTED_ASSET_INSTALL_COMMAND {
            return Err(ManagerError::InvalidInput(format!(
                "unexpected workspace-runtime asset_install_command '{}' (expected '{}')",
                self.bootstrap.asset_install_command, EXPECTED_ASSET_INSTALL_COMMAND
            )));
        }
        if self.bootstrap.role_environment_variable != EXPECTED_ROLE_ENV_VAR {
            return Err(ManagerError::InvalidInput(format!(
                "unexpected workspace-runtime role_environment_variable '{}' (expected '{}')",
                self.bootstrap.role_environment_variable, EXPECTED_ROLE_ENV_VAR
            )));
        }
        if self.bootstrap.credential_file != EXPECTED_CREDENTIAL_FILE {
            return Err(ManagerError::InvalidInput(format!(
                "unexpected workspace-runtime credential_file '{}' (expected '{}')",
                self.bootstrap.credential_file, EXPECTED_CREDENTIAL_FILE
            )));
        }
        if self.bootstrap.asset_directory != EXPECTED_ASSET_DIRECTORY {
            return Err(ManagerError::InvalidInput(format!(
                "unexpected workspace-runtime asset_directory '{}' (expected '{}')",
                self.bootstrap.asset_directory, EXPECTED_ASSET_DIRECTORY
            )));
        }
        if self.harness.name != EXPECTED_HARNESS_NAME {
            return Err(ManagerError::InvalidInput(format!(
                "unexpected workspace-runtime harness name '{}' (expected '{}')",
                self.harness.name, EXPECTED_HARNESS_NAME
            )));
        }
        if self.harness.openflows_release.trim().is_empty() {
            return Err(ManagerError::InvalidInput(
                "workspace-runtime harness.openflows_release cannot be empty".into(),
            ));
        }

        if let Some(min_mgr) = self.compatibility.minimum_manager_version.as_deref() {
            parse_semver(min_mgr).ok_or_else(|| {
                ManagerError::InvalidInput(format!(
                    "invalid workspace-runtime minimum_manager_version '{min_mgr}'"
                ))
            })?;
        }

        Ok(())
    }

    pub fn image_for(&self, role: Role) -> Result<&WorkspaceRuntimeImage, ManagerError> {
        self.images.get(&role).ok_or_else(|| {
            ManagerError::InvalidInput(format!(
                "workspace-runtime manifest missing image for role '{}'",
                role.as_str()
            ))
        })
    }

    pub fn validate_compatibility(
        &self,
        target: &ReleaseCompatibilityTarget,
        options: &WorkspaceRuntimeCompatibilityOptions,
    ) -> Result<(), ManagerError> {
        let canonical_arch = validate_architecture(&target.architecture)?;
        let linux_platform = format!("linux/{canonical_arch}");

        for role in Role::ALL {
            let img = self.image_for(role)?;
            if !options.allowed_repository_prefixes.is_empty()
                && !options
                    .allowed_repository_prefixes
                    .iter()
                    .any(|prefix| img.repository.starts_with(prefix))
            {
                return Err(ManagerError::InvalidInput(format!(
                    "workspace-runtime '{}' role '{}' repository '{}' is not under an allowed prefix ({})",
                    self.runtime_version,
                    role.as_str(),
                    img.repository,
                    options.allowed_repository_prefixes.join(", ")
                )));
            }
            if !img.supports_architecture(&canonical_arch) {
                return Err(ManagerError::InvalidInput(format!(
                    "workspace-runtime '{}' role '{}' does not support target platform '{}'",
                    self.runtime_version,
                    role.as_str(),
                    linux_platform
                )));
            }
        }

        if let Some(expected) = target.expected_runtime_release.as_deref() {
            if self.runtime_version != expected {
                return Err(ManagerError::InvalidInput(format!(
                    "workspace-runtime version '{}' does not match expected '{}'",
                    self.runtime_version, expected
                )));
            }
        }

        if let Some(min_mgr) = self.compatibility.minimum_manager_version.as_deref() {
            let min_tuple = parse_semver(min_mgr).ok_or_else(|| {
                ManagerError::InvalidInput(format!(
                    "invalid workspace-runtime minimum_manager_version '{min_mgr}'"
                ))
            })?;
            let current_tuple = parse_semver(&options.manager_version).ok_or_else(|| {
                ManagerError::InvalidInput(format!(
                    "invalid manager_version '{}'",
                    options.manager_version
                ))
            })?;
            if current_tuple < min_tuple {
                return Err(ManagerError::InvalidInput(format!(
                    "manager version '{}' is older than workspace-runtime minimum_manager_version '{}'",
                    options.manager_version, min_mgr
                )));
            }
        }

        if options.strict_compatibility {
            if self.status != WorkspaceRuntimeStatus::Compatible {
                return Err(ManagerError::InvalidInput(format!(
                    "workspace-runtime '{}' has status 'candidate' and is rejected under strict compatibility",
                    self.runtime_version
                )));
            }
            if self
                .compatibility
                .openflows_template_parameter_schema_versions
                .is_empty()
            {
                return Err(ManagerError::InvalidInput(format!(
                    "workspace-runtime '{}' declares no compatible openflows_template_parameter_schema_versions",
                    self.runtime_version
                )));
            }
        }

        if !self
            .compatibility
            .openflows_template_parameter_schema_versions
            .is_empty()
        {
            let has_compatible_schema = self
                .compatibility
                .openflows_template_parameter_schema_versions
                .iter()
                .any(|&v| {
                    v >= target.min_parameter_schema_version
                        && v <= target.max_parameter_schema_version
                });
            if !has_compatible_schema {
                return Err(ManagerError::InvalidInput(format!(
                    "workspace-runtime '{}' schema versions {:?} do not overlap supported range {}..={}",
                    self.runtime_version,
                    self.compatibility.openflows_template_parameter_schema_versions,
                    target.min_parameter_schema_version,
                    target.max_parameter_schema_version
                )));
            }
        }

        Ok(())
    }

    /// Populate per-role image repository, digest, and runtime release version on
    /// a [`TemplateManifest`], verifying each entry's schema version and architecture.
    pub fn apply_to_template_manifest(
        &self,
        manifest: &mut TemplateManifest,
    ) -> Result<(), ManagerError> {
        for (&role, entry) in manifest.roles.iter_mut() {
            self.apply_entry(role, entry)?;
        }
        for ((_, role), entry) in manifest.variants.iter_mut() {
            self.apply_entry(*role, entry)?;
        }
        Ok(())
    }

    fn apply_entry(
        &self,
        role: Role,
        entry: &mut crate::templates::packaging::RoleManifestEntry,
    ) -> Result<(), ManagerError> {
        let supported_schemas = &self
            .compatibility
            .openflows_template_parameter_schema_versions;
        if !supported_schemas.is_empty()
            && !supported_schemas.contains(&entry.parameter_schema_version)
        {
            return Err(ManagerError::InvalidInput(format!(
                "workspace-runtime '{}' schema versions {:?} do not support {} role '{}' parameter_schema_version {}",
                self.runtime_version,
                supported_schemas,
                entry.backend,
                role.as_str(),
                entry.parameter_schema_version
            )));
        }
        let img = self.image_for(role)?;
        if !img.supports_architecture(&entry.architecture) {
            return Err(ManagerError::InvalidInput(format!(
                "workspace-runtime '{}' {} role '{}' image does not support entry architecture '{}'",
                self.runtime_version,
                entry.backend,
                role.as_str(),
                entry.architecture
            )));
        }
        entry.image_repository = Some(img.repository.clone());
        entry.image_digest = Some(img.digest.clone());
        entry.workspace_runtime_release = Some(self.runtime_version.clone());
        Ok(())
    }
}

fn parse_semver(raw: &str) -> Option<semver::Version> {
    semver::Version::parse(raw.trim()).ok()
}

fn is_valid_oci_repository(repo: &str) -> bool {
    if repo.is_empty()
        || repo.contains('@')
        || repo.chars().any(|c| c.is_whitespace() || c.is_control())
    {
        return false;
    }
    let Some((registry_or_ns, last)) = repo.rsplit_once('/') else {
        return false;
    };
    if registry_or_ns.is_empty() || last.is_empty() {
        return false;
    }
    let segments: Vec<&str> = repo.split('/').collect();
    if segments
        .iter()
        .any(|s| s.is_empty() || *s == "." || *s == ".." || s.starts_with('.') || s.ends_with('.'))
    {
        return false;
    }
    // Registry host (first segment) may contain a port (e.g. localhost:5000);
    // path segments must not contain ':'.
    let valid_host = |s: &str| {
        s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | ':'))
    };
    let valid_path = |s: &str| {
        s.chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '_' | '-'))
    };
    valid_host(segments[0]) && segments[1..].iter().all(|s| valid_path(s))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::templates::ProvisionerBackend;

    const V0_2_0_MANIFEST: &str = r#"{
  "schema_version": 2,
  "runtime_version": "0.2.0",
  "status": "candidate",
  "images": {
    "nexus": {
      "repository": "ghcr.io/the-agenticflow/workspace-runtime-nexus",
      "digest": "sha256:76db4d6953b4ed5ba64cfd47e155fdb7aca38cd79ee89b600f9f46457cbe12cd",
      "architectures": ["linux/amd64", "linux/arm64"]
    },
    "forge": {
      "repository": "ghcr.io/the-agenticflow/workspace-runtime-forge",
      "digest": "sha256:660f6f401a13103e2012997f03ab7d864f8853ce06b280b0371fb07d05a752f0",
      "architectures": ["linux/amd64", "linux/arm64"]
    },
    "sentinel": {
      "repository": "ghcr.io/the-agenticflow/workspace-runtime-sentinel",
      "digest": "sha256:b824305d32ad15f087cf00f17880d36ef21271623336de83d5db917a9517d07f",
      "architectures": ["linux/amd64", "linux/arm64"]
    },
    "vessel": {
      "repository": "ghcr.io/the-agenticflow/workspace-runtime-vessel",
      "digest": "sha256:59bff0379fa95382725b74196be274a9cbaac0e85e3183ffa6e5e48766342e3e",
      "architectures": ["linux/amd64", "linux/arm64"]
    },
    "lore": {
      "repository": "ghcr.io/the-agenticflow/workspace-runtime-lore",
      "digest": "sha256:5f42f72b37c9c8ec687599f9f905554ccf71e452d5a13e8b2133475c6fe068fb",
      "architectures": ["linux/amd64", "linux/arm64"]
    }
  },
  "roles": ["nexus", "forge", "sentinel", "vessel", "lore"],
  "bootstrap": {
    "contract_version": 2,
    "asset_install_command": "openflows-runtime install",
    "role_environment_variable": "OPENFLOWS_ROLE",
    "credential_file": "/run/openflows/runtime.json",
    "asset_directory": ".openflows/artifacts"
  },
  "harness": {
    "name": "openflows-harness",
    "openflows_release": "openflows-1.8.2"
  },
  "compatibility": {
    "openflows_template_parameter_schema_versions": [],
    "minimum_manager_version": null
  }
}"#;

    #[test]
    fn parses_v0_2_0_manifest_and_enforces_strict_vs_candidate_compatibility() {
        let manifest = WorkspaceRuntimeManifest::parse_str(V0_2_0_MANIFEST).unwrap();
        assert_eq!(manifest.runtime_version, "0.2.0");
        assert_eq!(manifest.status, WorkspaceRuntimeStatus::Candidate);
        assert_eq!(
            manifest.image_for(Role::Nexus).unwrap().image_reference(),
            "ghcr.io/the-agenticflow/workspace-runtime-nexus@sha256:76db4d6953b4ed5ba64cfd47e155fdb7aca38cd79ee89b600f9f46457cbe12cd"
        );

        let target = ReleaseCompatibilityTarget::docker_default();
        // Strict mode rejects candidate + empty schema versions
        assert!(manifest
            .validate_compatibility(&target, &WorkspaceRuntimeCompatibilityOptions::default())
            .is_err());

        // Candidate-allowed mode accepts v0.2.0
        manifest
            .validate_compatibility(
                &target,
                &WorkspaceRuntimeCompatibilityOptions::allow_candidate(),
            )
            .unwrap();
    }

    #[test]
    fn compatible_manifest_passes_strict_mode_and_rejects_mismatches() {
        let mut manifest = WorkspaceRuntimeManifest::parse_str(V0_2_0_MANIFEST).unwrap();
        manifest.status = WorkspaceRuntimeStatus::Compatible;
        manifest
            .compatibility
            .openflows_template_parameter_schema_versions = vec![2];
        manifest.compatibility.minimum_manager_version = Some("0.1.0".to_string());

        let target = ReleaseCompatibilityTarget::kubernetes_default("arm64");
        manifest
            .validate_compatibility(&target, &WorkspaceRuntimeCompatibilityOptions::default())
            .unwrap();

        // Incompatible schema version
        manifest
            .compatibility
            .openflows_template_parameter_schema_versions = vec![99];
        assert!(manifest
            .validate_compatibility(&target, &WorkspaceRuntimeCompatibilityOptions::default())
            .is_err());

        // Too-high minimum_manager_version
        manifest
            .compatibility
            .openflows_template_parameter_schema_versions = vec![2];
        manifest.compatibility.minimum_manager_version = Some("99.0.0".to_string());
        assert!(manifest
            .validate_compatibility(&target, &WorkspaceRuntimeCompatibilityOptions::default())
            .is_err());
    }

    #[test]
    fn rejects_invalid_manifest_structures() {
        let bad_schema = V0_2_0_MANIFEST.replace("\"schema_version\": 2", "\"schema_version\": 1");
        assert!(WorkspaceRuntimeManifest::parse_str(&bad_schema).is_err());

        let bad_bootstrap = V0_2_0_MANIFEST.replace(
            "\"asset_install_command\": \"openflows-runtime install\"",
            "\"asset_install_command\": \"echo nope\"",
        );
        assert!(WorkspaceRuntimeManifest::parse_str(&bad_bootstrap).is_err());

        let bad_digest = V0_2_0_MANIFEST.replace(
            "sha256:76db4d6953b4ed5ba64cfd47e155fdb7aca38cd79ee89b600f9f46457cbe12cd",
            "sha256:invalid",
        );
        assert!(WorkspaceRuntimeManifest::parse_str(&bad_digest).is_err());

        let dup_roles = V0_2_0_MANIFEST.replace(
            r#"["nexus", "forge", "sentinel", "vessel", "lore"]"#,
            r#"["nexus", "nexus", "sentinel", "vessel", "lore"]"#,
        );
        assert!(WorkspaceRuntimeManifest::parse_str(&dup_roles).is_err());

        let bad_repo = V0_2_0_MANIFEST.replace(
            "ghcr.io/the-agenticflow/workspace-runtime-nexus",
            "ghcr.io/the-agenticflow/workspace-runtime-nexus:latest",
        );
        assert!(WorkspaceRuntimeManifest::parse_str(&bad_repo).is_err());
    }

    #[test]
    fn compatibility_rejects_arch_mismatch_and_apply_populates_manifest() {
        let mut manifest = WorkspaceRuntimeManifest::parse_str(V0_2_0_MANIFEST).unwrap();
        manifest.status = WorkspaceRuntimeStatus::Compatible;
        manifest
            .compatibility
            .openflows_template_parameter_schema_versions = vec![2];
        manifest.images.get_mut(&Role::Forge).unwrap().architectures =
            vec!["linux/amd64".to_string()];

        let arm64_target = ReleaseCompatibilityTarget::kubernetes_default("arm64");
        let err = manifest
            .validate_compatibility(
                &arm64_target,
                &WorkspaceRuntimeCompatibilityOptions::default(),
            )
            .unwrap_err();
        assert!(err.to_string().contains("linux/arm64"), "{err}");

        let mut roles = BTreeMap::new();
        for role in Role::ALL {
            roles.insert(
                role,
                crate::templates::packaging::RoleManifestEntry::docker(
                    role,
                    role.archive_name(),
                    "a".repeat(64),
                    2,
                    None,
                ),
            );
        }
        let mut tmpl = TemplateManifest::from_v1_roles("2026.11.0", Role::ALL.to_vec(), roles);
        manifest.apply_to_template_manifest(&mut tmpl).unwrap();
        let forge = tmpl
            .variants
            .get(&(ProvisionerBackend::Docker, Role::Forge))
            .unwrap();
        assert_eq!(
            forge.image_repository.as_deref(),
            Some("ghcr.io/the-agenticflow/workspace-runtime-forge")
        );
        assert_eq!(
            forge.image_digest.as_deref(),
            Some("sha256:660f6f401a13103e2012997f03ab7d864f8853ce06b280b0371fb07d05a752f0")
        );
        assert_eq!(forge.workspace_runtime_release.as_deref(), Some("0.2.0"));
    }

    #[test]
    fn apply_rejects_schema_version_and_architecture_mismatches() {
        let mut manifest = WorkspaceRuntimeManifest::parse_str(V0_2_0_MANIFEST).unwrap();
        manifest.status = WorkspaceRuntimeStatus::Compatible;
        manifest
            .compatibility
            .openflows_template_parameter_schema_versions = vec![2];
        manifest.images.get_mut(&Role::Forge).unwrap().architectures =
            vec!["linux/amd64".to_string()];

        let mut roles = BTreeMap::new();
        for role in Role::ALL {
            roles.insert(
                role,
                crate::templates::packaging::RoleManifestEntry::docker(
                    role,
                    role.archive_name(),
                    "a".repeat(64),
                    1,
                    None,
                ),
            );
        }
        let mut schema_mismatch =
            TemplateManifest::from_v1_roles("2026.11.0", Role::ALL.to_vec(), roles);
        let err = manifest
            .apply_to_template_manifest(&mut schema_mismatch)
            .unwrap_err();
        assert!(
            err.to_string().contains("parameter_schema_version 1"),
            "{err}"
        );

        let mut roles = BTreeMap::new();
        for role in Role::ALL {
            let mut entry = crate::templates::packaging::RoleManifestEntry::docker(
                role,
                role.archive_name(),
                "a".repeat(64),
                2,
                None,
            );
            if role == Role::Forge {
                entry.architecture = "arm64".to_string();
            }
            roles.insert(role, entry);
        }
        let mut arch_mismatch =
            TemplateManifest::from_v1_roles("2026.11.0", Role::ALL.to_vec(), roles);
        let err = manifest
            .apply_to_template_manifest(&mut arch_mismatch)
            .unwrap_err();
        assert!(err.to_string().contains("architecture 'arm64'"), "{err}");
    }

    #[test]
    fn rejects_invalid_oci_repository_segments() {
        for bad in [
            "ghcr.io/../workspace-runtime-nexus",
            "ghcr.io/./workspace-runtime-nexus",
            "ghcr.io//workspace-runtime-nexus",
            "../workspace-runtime-nexus",
            "ghcr.io/.hidden/workspace-runtime-nexus",
            "ghcr.io/org./workspace-runtime-nexus",
        ] {
            assert!(
                !is_valid_oci_repository(bad),
                "expected rejection for {bad}"
            );
        }
        assert!(is_valid_oci_repository(
            "ghcr.io/the-agenticflow/workspace-runtime-nexus"
        ));
        assert!(is_valid_oci_repository("localhost:5000/org/repo_v1.0"));
    }

    #[test]
    fn rejects_disallowed_repository_prefix_and_allows_explicit_override() {
        let evil_json = V0_2_0_MANIFEST.replace(
            "ghcr.io/the-agenticflow/workspace-runtime-nexus",
            "evil.example/attacker/workspace-runtime-nexus",
        );
        let manifest = WorkspaceRuntimeManifest::parse_str(&evil_json).unwrap();
        let target = ReleaseCompatibilityTarget::docker_default();
        let mut opts = WorkspaceRuntimeCompatibilityOptions::allow_candidate();
        let err = manifest.validate_compatibility(&target, &opts).unwrap_err();
        assert!(
            err.to_string().contains("not under an allowed prefix"),
            "{err}"
        );

        opts.allowed_repository_prefixes = vec![
            "ghcr.io/the-agenticflow/".to_string(),
            "evil.example/attacker/".to_string(),
        ];
        manifest.validate_compatibility(&target, &opts).unwrap();
    }
}

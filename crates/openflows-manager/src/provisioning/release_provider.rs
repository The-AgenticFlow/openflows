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
use crate::templates::{
    ProvisionerBackend, WorkspaceRuntimeCompatibilityOptions, WorkspaceRuntimeManifest,
};
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
pub const WORKSPACE_RUNTIME_MANIFEST_ENV: &str = "OPENFLOWS_WORKSPACE_RUNTIME_MANIFEST";
pub const WORKSPACE_RUNTIME_MANIFEST_SHA256_ENV: &str =
    "OPENFLOWS_WORKSPACE_RUNTIME_MANIFEST_SHA256";
pub const WORKSPACE_RUNTIME_ALLOW_CANDIDATE_ENV: &str =
    "OPENFLOWS_WORKSPACE_RUNTIME_ALLOW_CANDIDATE";

/// Loads the operator-configured release artifacts.
#[derive(Clone)]
pub struct ReleaseProvider {
    source_dir: PathBuf,
    manifest_path: Option<PathBuf>,
    manifest_sha256: Option<String>,
    release_name: String,
    workspace_runtime_manifest_path: Option<PathBuf>,
    workspace_runtime_manifest_sha256: Option<String>,
    allow_candidate_runtime: bool,
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
        let workspace_runtime_manifest_path =
            std::env::var_os(WORKSPACE_RUNTIME_MANIFEST_ENV).map(PathBuf::from);
        let workspace_runtime_manifest_sha256 =
            non_empty_env(WORKSPACE_RUNTIME_MANIFEST_SHA256_ENV);
        let allow_candidate_runtime = candidate_opt_in_from_env();
        Ok(ReleaseProvider {
            source_dir,
            manifest_path,
            manifest_sha256,
            release_name,
            workspace_runtime_manifest_path,
            workspace_runtime_manifest_sha256,
            allow_candidate_runtime,
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
            workspace_runtime_manifest_path: None,
            workspace_runtime_manifest_sha256: None,
            allow_candidate_runtime: false,
        }
    }

    /// Attach a `workspace-runtime` `release-manifest.json` path to resolve
    /// per-role image repositories and digests.
    pub fn with_workspace_runtime_manifest(
        mut self,
        path: PathBuf,
        allow_candidate_runtime: bool,
    ) -> Self {
        self.workspace_runtime_manifest_path = Some(path);
        self.allow_candidate_runtime = allow_candidate_runtime;
        self
    }

    /// Require a specific SHA-256 hex digest for the `workspace-runtime`
    /// manifest file.
    pub fn with_workspace_runtime_manifest_sha256(mut self, sha256: String) -> Self {
        self.workspace_runtime_manifest_sha256 = Some(sha256);
        self
    }

    /// Read the workspace-runtime manifest path and candidate opt-in from the
    /// environment (`OPENFLOWS_WORKSPACE_RUNTIME_MANIFEST`,
    /// `OPENFLOWS_WORKSPACE_RUNTIME_MANIFEST_SHA256`,
    /// `OPENFLOWS_WORKSPACE_RUNTIME_ALLOW_CANDIDATE`).
    pub fn with_workspace_runtime_from_env(mut self) -> Self {
        self.workspace_runtime_manifest_path =
            std::env::var_os(WORKSPACE_RUNTIME_MANIFEST_ENV).map(PathBuf::from);
        self.workspace_runtime_manifest_sha256 =
            non_empty_env(WORKSPACE_RUNTIME_MANIFEST_SHA256_ENV);
        self.allow_candidate_runtime = candidate_opt_in_from_env();
        self
    }

    /// Whether a workspace-runtime manifest will supply the pinned images.
    pub fn has_workspace_runtime_manifest(&self) -> bool {
        self.workspace_runtime_manifest_path.is_some()
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
        let mut manifest = TemplateManifest::parse(&bytes)
            .map_err(|e| ReleaseProviderError(format!("template manifest is invalid: {e}")))?;

        // Verify the operator-pinned template manifest digest BEFORE mutating
        // `manifest` with workspace-runtime overrides.
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

        if let Some(runtime_path) = &self.workspace_runtime_manifest_path {
            let runtime_bytes = std::fs::read(runtime_path).map_err(|e| {
                ReleaseProviderError(format!(
                    "cannot read workspace-runtime manifest '{}': {e}",
                    runtime_path.display()
                ))
            })?;
            if let Some(expected_runtime_sha) = &self.workspace_runtime_manifest_sha256 {
                let actual_runtime_sha =
                    hex::encode(<sha2::Sha256 as sha2::Digest>::digest(&runtime_bytes));
                if actual_runtime_sha != expected_runtime_sha.trim().to_ascii_lowercase() {
                    return Err(ReleaseProviderError(format!(
                        "workspace-runtime manifest digest mismatch: expected {expected_runtime_sha}, got {actual_runtime_sha}"
                    )));
                }
            }
            let runtime_manifest = WorkspaceRuntimeManifest::parse(&runtime_bytes)
                .map_err(|e| ReleaseProviderError(e.to_string()))?;
            let backend = crate::provisioning::selection::resolve_backend_env()
                .map_err(ReleaseProviderError)?;
            let mode_raw = std::env::var(crate::config::MODE_ENV).ok();
            let mode =
                crate::config::Mode::parse(mode_raw.as_deref()).map_err(ReleaseProviderError)?;
            let target = crate::provisioning::selection::release_target(&backend, mode)
                .map_err(|e| ReleaseProviderError(e.to_string()))?;
            let options = WorkspaceRuntimeCompatibilityOptions {
                strict_compatibility: !self.allow_candidate_runtime,
                ..WorkspaceRuntimeCompatibilityOptions::default()
            };
            runtime_manifest
                .validate_compatibility(&target, &options)
                .map_err(|e| ReleaseProviderError(e.to_string()))?;
            runtime_manifest
                .apply_to_template_manifest(&mut manifest)
                .map_err(|e| ReleaseProviderError(e.to_string()))?;
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
            || self.workspace_runtime_manifest_path.is_some()
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

        let required_roles = if self.workspace_runtime_manifest_path.is_some() {
            Role::ALL.to_vec()
        } else {
            vec![Role::Nexus, Role::Forge, Role::Sentinel, Role::Vessel]
        };
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

/// Candidate workspace-runtime releases are rejected unless the operator opts in.
fn candidate_opt_in_from_env() -> bool {
    std::env::var(WORKSPACE_RUNTIME_ALLOW_CANDIDATE_ENV)
        .ok()
        .is_some_and(|v| parse_candidate_opt_in(&v))
}

fn parse_candidate_opt_in(value: &str) -> bool {
    matches!(value.trim(), "1" | "true" | "TRUE" | "yes")
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

    fn source_provider(dir: &tempfile::TempDir) -> ReleaseProvider {
        ReleaseProvider::new(dir.path().to_path_buf(), None, None, "test".into())
    }

    #[test]
    fn a_partial_kubernetes_set_leaves_the_source_manifest_docker_only() {
        // A manifest that lists a backend must carry every required role for it,
        // so one Kubernetes template must not introduce the backend yet.
        let dir = source_tree(&["nexus"]);
        let artifacts = source_provider(&dir).load().unwrap();
        assert_eq!(
            artifacts.manifest.backends(),
            vec![ProvisionerBackend::Docker]
        );
    }

    #[test]
    fn a_complete_kubernetes_set_publishes_kubernetes_variants() {
        let dir = source_tree(&["forge", "sentinel", "nexus", "vessel", "lore"]);
        let artifacts = source_provider(&dir).load().unwrap();
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

    fn runtime_manifest(status: &str, drop_role: Option<&str>) -> String {
        let images: Vec<String> = ["nexus", "forge", "sentinel", "vessel", "lore"]
            .iter()
            .filter(|r| Some(**r) != drop_role)
            .enumerate()
            .map(|(i, role)| {
                format!(
                    r#""{role}": {{"repository": "ghcr.io/example/workspace-runtime-{role}", "digest": "sha256:{}", "architectures": ["linux/amd64", "linux/arm64"]}}"#,
                    format!("{:x}", i + 1).repeat(64)
                )
            })
            .collect();
        format!(
            r#"{{"schema_version": 2, "runtime_version": "0.2.0", "status": "{status}",
              "images": {{{}}}, "roles": ["nexus","forge","sentinel","vessel","lore"],
              "bootstrap": {{"contract_version": 2, "asset_install_command": "openflows-runtime install",
                "role_environment_variable": "OPENFLOWS_ROLE", "credential_file": "/run/openflows/runtime.json",
                "asset_directory": ".openflows/artifacts"}},
              "harness": {{"name": "openflows-harness", "openflows_release": "openflows-1.8.2"}},
              "compatibility": {{"openflows_template_parameter_schema_versions": [], "minimum_manager_version": null}}}}"#,
            images.join(",")
        )
    }

    fn provider(manifest: Option<&Path>, allow_candidate: bool) -> ReleaseProvider {
        let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../coder-client/templates");
        let provider = ReleaseProvider::new(source, None, None, "test".into());
        match manifest {
            Some(path) => {
                provider.with_workspace_runtime_manifest(path.to_path_buf(), allow_candidate)
            }
            None => provider,
        }
    }

    #[test]
    fn candidate_opt_in_is_strict_by_default() {
        for value in ["", "0", "false", "no", "off", "2", "True ", "enabled"] {
            assert!(!parse_candidate_opt_in(value), "{value:?} must not opt in");
        }
        for value in ["1", "true", "TRUE", "yes", " true "] {
            assert!(parse_candidate_opt_in(value), "{value:?} must opt in");
        }
        let target = ReleaseProvider::new(PathBuf::from("."), None, None, "test".into());
        assert!(!target.allow_candidate_runtime);
    }

    #[test]
    fn candidate_runtime_is_rejected_unless_the_operator_opts_in() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("release-manifest.json");
        std::fs::write(&path, runtime_manifest("candidate", None)).unwrap();
        let err = match provider(Some(&path), false).load() {
            Ok(_) => panic!("strict by default"),
            Err(err) => err,
        };
        assert!(err.to_string().contains("candidate"), "{err}");
        let artifacts = provider(Some(&path), true).load().expect("explicit opt-in");
        let forge = artifacts
            .manifest
            .roles
            .get(&Role::Forge)
            .expect("forge role");
        assert_eq!(
            forge.image_repository.as_deref(),
            Some("ghcr.io/example/workspace-runtime-forge")
        );
        assert!(forge
            .image_digest
            .as_deref()
            .is_some_and(is_valid_image_digest));
    }

    #[test]
    fn runtime_release_missing_a_role_image_or_file_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("release-manifest.json");
        std::fs::write(&path, runtime_manifest("candidate", Some("lore"))).unwrap();
        assert!(provider(Some(&path), true).load().is_err());
        let missing = dir.path().join("absent.json");
        let err = match provider(Some(&missing), true).load() {
            Ok(_) => panic!("missing file"),
            Err(err) => err,
        };
        assert!(err
            .to_string()
            .contains("cannot read workspace-runtime manifest"));
    }

    #[test]
    fn template_manifest_sha256_is_verified_before_runtime_overlay_and_runtime_sha256_is_checked() {
        let dir = tempfile::tempdir().unwrap();
        let source = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../coder-client/templates");
        let base_artifacts = ReleaseProvider::new(source.clone(), None, None, "test".into())
            .load()
            .unwrap();
        let base_manifest_bytes = base_artifacts.manifest.to_canonical_json().unwrap();
        let base_manifest_path = dir.path().join("template-manifest.json");
        std::fs::write(&base_manifest_path, &base_manifest_bytes).unwrap();
        let expected_template_sha = base_artifacts.manifest.manifest_sha256().unwrap();

        let runtime_path = dir.path().join("release-manifest.json");
        let runtime_json = runtime_manifest("candidate", None);
        std::fs::write(&runtime_path, &runtime_json).unwrap();
        let expected_runtime_sha = hex::encode(<sha2::Sha256 as sha2::Digest>::digest(
            runtime_json.as_bytes(),
        ));

        // Template manifest sha256 matches pre-overlay manifest even with runtime overlay.
        let loaded = ReleaseProvider::new(
            source.clone(),
            Some(base_manifest_path.clone()),
            Some(expected_template_sha.clone()),
            "test".into(),
        )
        .with_workspace_runtime_manifest(runtime_path.clone(), true)
        .with_workspace_runtime_manifest_sha256(expected_runtime_sha)
        .load()
        .unwrap();
        assert!(loaded
            .manifest
            .roles
            .get(&Role::Forge)
            .unwrap()
            .image_repository
            .is_some());

        // Bad runtime manifest sha256 is rejected.
        let err = match ReleaseProvider::new(
            source,
            Some(base_manifest_path),
            Some(expected_template_sha),
            "test".into(),
        )
        .with_workspace_runtime_manifest(runtime_path, true)
        .with_workspace_runtime_manifest_sha256("0".repeat(64))
        .load()
        {
            Ok(_) => panic!("bad runtime sha256 must fail"),
            Err(err) => err,
        };
        assert!(err
            .to_string()
            .contains("workspace-runtime manifest digest mismatch"));
    }

    #[test]
    fn release_provider_packages_dual_backend_v2_manifest() {
        let dir = tempfile::tempdir().unwrap();
        for backend in ProvisionerBackend::ALL {
            for role in Role::ALL {
                let role_dir = dir.path().join(backend.as_str()).join(role.template_name());
                std::fs::create_dir_all(&role_dir).unwrap();
                std::fs::write(
                    role_dir.join("main.tf"),
                    format!("resource \"role\" {{ backend = \"{backend}\" role = \"{role}\" }}\n"),
                )
                .unwrap();
            }
        }
        let provider =
            ReleaseProvider::new(dir.path().to_path_buf(), None, None, "2026.11.0".into());
        let artifacts = provider.load().expect("dual-backend packaging succeeds");
        assert_eq!(
            artifacts.manifest.schema_version,
            TemplateManifest::CURRENT_V2_SCHEMA_VERSION
        );
        assert_eq!(artifacts.manifest.release, "2026.11.0");
        assert_eq!(artifacts.manifest.variants.len(), 10);
        for backend in ProvisionerBackend::ALL {
            for role in Role::ALL {
                assert!(artifacts.manifest.variant(backend, role).is_some());
                assert!(artifacts.archive_for(backend, role).is_some());
            }
        }
        let reparsed =
            TemplateManifest::parse(&artifacts.manifest.to_canonical_json().unwrap()).unwrap();
        assert_eq!((reparsed.schema_version, reparsed.variants.len()), (2, 10));
    }
}

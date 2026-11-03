//! Operator-configured release artifact source.
//!
//! The template manifest and role archives are operator configuration, never
//! customer-supplied. `ReleaseProvider` locates the manifest and source
//! directories from environment configuration, packages the role archives
//! deterministically, and validates every archive digest against the manifest
//! before the release is imported.

use crate::error::ManagerError;
use crate::templates::packaging::{
    build_deterministic_archive, validate_archive, verify_archive_digest, Role, RoleManifestEntry,
    TemplateManifest,
};
use crate::templates::service::ReleaseArtifacts;
use std::collections::BTreeMap;
use std::path::PathBuf;

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
        let mut archives = BTreeMap::new();
        for role in Role::ALL {
            let dir = self.source_dir.join(format!("openflows-{}", role.as_str()));
            if !dir.is_dir() {
                return Err(ReleaseProviderError(format!(
                    "template source directory '{}' missing for role '{}'",
                    dir.display(),
                    role.as_str()
                )));
            }
            let archive = build_deterministic_archive(&dir).map_err(|e| {
                ReleaseProviderError(format!("failed to package role '{}': {e}", role.as_str()))
            })?;
            validate_archive(&archive).map_err(|e| {
                ReleaseProviderError(format!(
                    "packaged role '{}' failed validation: {e}",
                    role.as_str()
                ))
            })?;
            archives.insert(role, archive);
        }

        // Verify every required role's archive digest against the manifest.
        for role in &manifest.required_roles {
            let entry = manifest
                .roles
                .get(role)
                .ok_or_else(|| ReleaseProviderError("manifest missing required role".into()))?;
            let archive = archives.get(role).ok_or_else(|| {
                ReleaseProviderError(format!("no archive for role '{}'", role.as_str()))
            })?;
            verify_archive_digest(archive, &entry.sha256).map_err(|e| {
                ReleaseProviderError(format!(
                    "role '{}' archive does not match manifest digest: {e}",
                    role.as_str()
                ))
            })?;
        }

        Ok(ReleaseArtifacts { manifest, archives })
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
        let image_digest = std::env::var("OPENFLOWS_WORKSPACE_IMAGE_DIGEST")
            .ok()
            .filter(|value| !value.trim().is_empty());
        if let Some(digest) = image_digest.as_deref() {
            let valid = digest.strip_prefix("sha256:").is_some_and(|hex| {
                hex.len() == 64 && hex.bytes().all(|byte| byte.is_ascii_hexdigit())
            });
            if !valid {
                return Err(ReleaseProviderError(
                    "OPENFLOWS_WORKSPACE_IMAGE_DIGEST must be sha256 followed by 64 hexadecimal characters"
                        .into(),
                ));
            }
        }
        let mut roles = BTreeMap::new();
        for role in Role::ALL {
            let dir = self.source_dir.join(format!("openflows-{}", role.as_str()));
            let archive = build_deterministic_archive(&dir).map_err(|e| {
                ReleaseProviderError(format!("failed to package role '{}': {e}", role.as_str()))
            })?;
            let sha256 = crate::templates::packaging::sha256_hex(&archive);
            roles.insert(
                role,
                RoleManifestEntry {
                    role,
                    archive: role.archive_name(),
                    sha256,
                    parameter_schema_version: 1,
                    image_digest: image_digest.clone(),
                },
            );
        }
        let manifest = TemplateManifest {
            schema_version: TemplateManifest::CURRENT_SCHEMA_VERSION,
            release: self.release_name.clone(),
            required_roles: vec![Role::Nexus, Role::Forge, Role::Sentinel, Role::Vessel],
            roles,
        };
        manifest
            .to_canonical_json()
            .map_err(|e| ReleaseProviderError(format!("cannot serialize manifest: {e}")))
    }
}

/// Default source directory: the checked-in coder-client templates.
fn default_source_dir() -> PathBuf {
    // Best-effort: relative to the workspace root. Callers can override with
    // OPENFLOWS_TEMPLATE_SOURCE_DIR.
    PathBuf::from("crates/coder-client/templates")
}

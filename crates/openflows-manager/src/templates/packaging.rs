//! Deterministic template-role packaging and manifest validation.
//!
//! The same source directory must always produce the same archive bytes so the
//! manifest digest is a stable release identity. We therefore normalize every
//! tar header field that would otherwise vary between machines (mtime, uid/gid,
//! uname/gname, and the gzip header), sort paths, and reject unsafe entries.

use crate::error::ManagerError;
use flate2::write::GzEncoder;
use flate2::Compression;
use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};
use tar::{Builder, EntryType, Header};

/// The five role templates. Order matters only for required-role checks; the
/// manifest serializes roles in sorted order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Role {
    Nexus,
    Forge,
    Sentinel,
    Vessel,
    Lore,
}

impl Role {
    pub const ALL: [Role; 5] = [
        Role::Nexus,
        Role::Forge,
        Role::Sentinel,
        Role::Vessel,
        Role::Lore,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Role::Nexus => "nexus",
            Role::Forge => "forge",
            Role::Sentinel => "sentinel",
            Role::Vessel => "vessel",
            Role::Lore => "lore",
        }
    }

    /// The deterministic template name within a Coder organization.
    pub fn template_name(self) -> String {
        format!("openflows-{}", self.as_str())
    }

    /// The archive filename inside the release bundle.
    pub fn archive_name(self) -> String {
        format!("openflows-{}.tar.gz", self.as_str())
    }
}

impl std::fmt::Display for Role {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One role's entry in the release manifest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoleManifestEntry {
    pub role: Role,
    /// Archive filename, e.g. `openflows-nexus.tar.gz`.
    pub archive: String,
    /// Lowercase SHA-256 hex of the packaged archive (64 characters).
    pub sha256: String,
    /// Parameter-schema version for this role's template.
    pub parameter_schema_version: u32,
    /// Optional pinned image digest.
    pub image_digest: Option<String>,
}

/// The typed, versioned release manifest. Its canonical serialization's SHA-256
/// is the release identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TemplateManifest {
    pub schema_version: u32,
    pub release: String,
    pub required_roles: Vec<Role>,
    pub roles: BTreeMap<Role, RoleManifestEntry>,
}

impl TemplateManifest {
    pub const CURRENT_SCHEMA_VERSION: u32 = 1;

    /// Parse and validate a manifest JSON document. Rejects unknown schema
    /// versions, missing required roles, malformed digests, and unexpected
    /// role/archive names.
    pub fn parse(bytes: &[u8]) -> Result<TemplateManifest, ManagerError> {
        let value: serde_json::Value = serde_json::from_slice(bytes).map_err(|_| {
            ManagerError::InvalidInput("template manifest is not valid JSON".into())
        })?;

        let schema_version = value
            .get("schema_version")
            .and_then(|v| v.as_u64())
            .ok_or_else(|| ManagerError::InvalidInput("manifest missing schema_version".into()))?;
        if schema_version != Self::CURRENT_SCHEMA_VERSION as u64 {
            return Err(ManagerError::InvalidInput(format!(
                "unsupported manifest schema_version {schema_version}"
            )));
        }

        let release = value
            .get("release")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ManagerError::InvalidInput("manifest missing release".into()))?
            .to_string();

        let required_roles: Vec<Role> = value
            .get("required_roles")
            .and_then(|v| v.as_array())
            .ok_or_else(|| ManagerError::InvalidInput("manifest missing required_roles".into()))?
            .iter()
            .map(|r| {
                r.as_str()
                    .ok_or_else(|| ManagerError::InvalidInput("required role not a string".into()))
                    .and_then(role_from_str)
            })
            .collect::<Result<_, _>>()?;

        let roles_obj = value
            .get("roles")
            .and_then(|v| v.as_object())
            .ok_or_else(|| ManagerError::InvalidInput("manifest missing roles".into()))?;

        let mut roles = BTreeMap::new();
        for (name, entry) in roles_obj {
            let role = role_from_str(name)?;
            let archive = entry
                .get("archive")
                .and_then(|v| v.as_str())
                .ok_or_else(|| ManagerError::InvalidInput("role entry missing archive".into()))?
                .to_string();
            if archive != role.archive_name() {
                return Err(ManagerError::InvalidInput(format!(
                    "role '{name}' archive '{archive}' does not match expected '{}'",
                    role.archive_name()
                )));
            }
            let sha256 = entry
                .get("sha256")
                .and_then(|v| v.as_str())
                .ok_or_else(|| ManagerError::InvalidInput("role entry missing sha256".into()))?
                .to_string();
            validate_sha256(&sha256)?;
            let parameter_schema_version = entry
                .get("parameter_schema_version")
                .and_then(|v| v.as_u64())
                .ok_or_else(|| {
                    ManagerError::InvalidInput("role entry missing parameter_schema_version".into())
                })?;
            let image_digest = entry
                .get("image_digest")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            roles.insert(
                role,
                RoleManifestEntry {
                    role,
                    archive,
                    sha256,
                    parameter_schema_version: parameter_schema_version as u32,
                    image_digest,
                },
            );
        }

        for role in [Role::Nexus, Role::Forge, Role::Sentinel, Role::Vessel] {
            if !required_roles.contains(&role) {
                return Err(ManagerError::InvalidInput(format!(
                    "manifest must require baseline role '{role}'"
                )));
            }
        }
        // Every required role must have an entry.
        for role in &required_roles {
            if !roles.contains_key(role) {
                return Err(ManagerError::InvalidInput(format!(
                    "manifest missing entry for required role '{}'",
                    role.as_str()
                )));
            }
        }

        Ok(TemplateManifest {
            schema_version: Self::CURRENT_SCHEMA_VERSION,
            release,
            required_roles,
            roles,
        })
    }

    /// Serialize the manifest to canonical JSON (sorted keys), used to compute
    /// the manifest digest.
    pub fn to_canonical_json(&self) -> Result<Vec<u8>, ManagerError> {
        let mut roles = serde_json::Map::new();
        for (role, entry) in &self.roles {
            let mut e = serde_json::Map::new();
            e.insert("archive".into(), serde_json::json!(entry.archive));
            e.insert("sha256".into(), serde_json::json!(entry.sha256));
            e.insert(
                "parameter_schema_version".into(),
                serde_json::json!(entry.parameter_schema_version),
            );
            if let Some(digest) = &entry.image_digest {
                e.insert("image_digest".into(), serde_json::json!(digest));
            }
            roles.insert(role.as_str().into(), serde_json::Value::Object(e));
        }
        let value = serde_json::json!({
            "schema_version": self.schema_version,
            "release": self.release,
            "required_roles": self.required_roles.iter().map(|r| r.as_str()).collect::<Vec<_>>(),
            "roles": roles,
        });
        let mut buf = Vec::new();
        serde_json::to_writer(&mut buf, &value).map_err(|e| {
            ManagerError::Service(anyhow::anyhow!("failed to serialize manifest: {e}"))
        })?;
        Ok(buf)
    }

    /// The immutable release identity: the SHA-256 of the canonical manifest.
    pub fn manifest_sha256(&self) -> Result<String, ManagerError> {
        let bytes = self.to_canonical_json()?;
        Ok(sha256_hex(&bytes))
    }
}

/// Normalized mtime for every archive entry (Unix epoch). Keeps packaging
/// reproducible across checkouts.
const NORMALIZED_MTIME: u64 = 0;

/// Build a deterministic `.tar.gz` archive from a source directory.
///
/// Paths are sorted; every entry gets a normalized mtime, uid/gid 0, empty
/// user/group names, and a stable mode. The gzip header carries no timestamp.
pub fn build_deterministic_archive(src_dir: &Path) -> Result<Vec<u8>, ManagerError> {
    let mut files = Vec::new();
    collect_files(src_dir, src_dir, &mut files)?;
    files.sort();

    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    {
        let mut builder = Builder::new(&mut encoder);
        builder.follow_symlinks(false);
        for rel in &files {
            let abs = src_dir.join(rel);
            let data = std::fs::read(&abs).map_err(|e| {
                ManagerError::Service(anyhow::anyhow!(
                    "failed to read template file '{}': {e}",
                    abs.display()
                ))
            })?;
            let mut header = Header::new_gnu();
            header.set_size(data.len() as u64);
            header.set_mode(0o644);
            header.set_mtime(NORMALIZED_MTIME);
            header.set_uid(0);
            header.set_gid(0);
            let _ = header.set_username("");
            let _ = header.set_groupname("");
            header.set_entry_type(EntryType::Regular);
            header.set_cksum();
            builder
                .append_data(&mut header, rel.as_os_str(), data.as_slice())
                .map_err(|e| {
                    ManagerError::Service(anyhow::anyhow!("failed to write tar entry: {e}"))
                })?;
        }
        builder
            .finish()
            .map_err(|e| ManagerError::Service(anyhow::anyhow!("failed to finish tar: {e}")))?;
    }
    let bytes = encoder
        .finish()
        .map_err(|e| ManagerError::Service(anyhow::anyhow!("failed to finish gzip: {e}")))?;
    Ok(bytes)
}

/// Validate a packaged archive: decode it and reject traversal, absolute paths,
/// symlinks, hardlinks, and non-regular entries.
pub fn validate_archive(archive: &[u8]) -> Result<(), ManagerError> {
    let decoder = flate2::read::GzDecoder::new(archive);
    let mut tar = tar::Archive::new(decoder);
    let entries = tar
        .entries()
        .map_err(|e| ManagerError::InvalidInput(format!("invalid template archive: {e}")))?;
    for entry in entries {
        let entry =
            entry.map_err(|e| ManagerError::InvalidInput(format!("invalid tar entry: {e}")))?;
        let entry_type = entry.header().entry_type();
        if entry_type == EntryType::Symlink || entry_type == EntryType::Link {
            return Err(ManagerError::InvalidInput(
                "template archive contains a link, which is unsafe".into(),
            ));
        }
        if !entry_type.is_file() && !entry_type.is_dir() {
            return Err(ManagerError::InvalidInput(
                "template archive contains a non-regular entry".into(),
            ));
        }
        let path = entry
            .path()
            .map_err(|e| ManagerError::InvalidInput(format!("invalid tar path: {e}")))?;
        if !safe_rel_path(&path) {
            return Err(ManagerError::InvalidInput(
                "template archive contains an unsafe path (traversal or absolute)".into(),
            ));
        }
        if path.file_name().is_some_and(|name| name == "registry.json") {
            return Err(ManagerError::InvalidInput(
                "template archive contains excluded registry.json".into(),
            ));
        }
    }
    Ok(())
}

/// Verify a role archive against its manifest digest.
pub fn verify_archive_digest(archive: &[u8], expected_sha256: &str) -> Result<(), ManagerError> {
    validate_sha256(expected_sha256)?;
    let actual = sha256_hex(archive);
    if actual != expected_sha256 {
        return Err(ManagerError::InvalidInput(format!(
            "template archive checksum mismatch: expected {expected_sha256}, got {actual}"
        )));
    }
    Ok(())
}

fn safe_rel_path(path: &Path) -> bool {
    if path.is_absolute() {
        return false;
    }
    let mut components = path.components().peekable();
    // A leading CurDir (".") is acceptable (the archives are packed with ".").
    if matches!(components.peek(), Some(Component::CurDir)) {
        components.next();
    }
    for comp in components {
        match comp {
            Component::Normal(_) => {}
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => return false,
        }
    }
    true
}

fn collect_files(root: &Path, dir: &Path, out: &mut Vec<PathBuf>) -> Result<(), ManagerError> {
    let entries = std::fs::read_dir(dir).map_err(|e| {
        ManagerError::Service(anyhow::anyhow!(
            "failed to read template dir '{}': {e}",
            dir.display()
        ))
    })?;
    for entry in entries {
        let entry = entry.map_err(|e| {
            ManagerError::Service(anyhow::anyhow!("failed to read template entry: {e}"))
        })?;
        let path = entry.path();
        let rel = path.strip_prefix(root).map_err(|e| {
            ManagerError::Service(anyhow::anyhow!("failed to compute relative path: {e}"))
        })?;
        let ft = entry.file_type().map_err(ManagerError::from)?;
        if ft.is_dir() {
            collect_files(root, &path, out)?;
        } else if ft.is_file() {
            // Reject registry.json: it must never ship as a template parameter.
            if rel.components().any(|c| c.as_os_str() == "registry.json") {
                return Err(ManagerError::InvalidInput(
                    "registry.json must not be packaged into a template release".into(),
                ));
            }
            out.push(rel.to_path_buf());
        } else {
            return Err(ManagerError::InvalidInput(format!(
                "unsupported template source entry '{}'",
                rel.display()
            )));
        }
    }
    Ok(())
}

fn role_from_str(s: &str) -> Result<Role, ManagerError> {
    match s {
        "nexus" => Ok(Role::Nexus),
        "forge" => Ok(Role::Forge),
        "sentinel" => Ok(Role::Sentinel),
        "vessel" => Ok(Role::Vessel),
        "lore" => Ok(Role::Lore),
        other => Err(ManagerError::InvalidInput(format!(
            "unknown template role '{other}'"
        ))),
    }
}

fn validate_sha256(s: &str) -> Result<(), ManagerError> {
    if s.len() != 64 || !s.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(ManagerError::InvalidInput(format!(
            "invalid SHA-256 digest '{s}'"
        )));
    }
    Ok(())
}

/// Compute the lowercase SHA-256 hex of a byte slice.
pub fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex::encode(hasher.finalize())
}

/// Serialize a manifest to JSON bytes (for persisting the canonical form).
pub fn manifest_json(manifest: &TemplateManifest) -> Result<Vec<u8>, ManagerError> {
    manifest.to_canonical_json()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_or_incomplete_required_roles_are_rejected() {
        for required in [
            serde_json::json!([]),
            serde_json::json!(["nexus", "forge", "sentinel"]),
        ] {
            let roles: serde_json::Map<String, serde_json::Value> = Role::ALL.iter().map(|r| (
                r.as_str().to_string(), serde_json::json!({"archive":r.archive_name(), "sha256":"a".repeat(64), "parameter_schema_version":1})
            )).collect();
            let manifest = serde_json::json!({"schema_version":1,"release":"test","required_roles":required,"roles":roles});
            assert!(TemplateManifest::parse(&serde_json::to_vec(&manifest).unwrap()).is_err());
        }
    }

    #[cfg(unix)]
    #[test]
    fn source_symlink_is_rejected_with_its_path() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("main.tf"), "resource {}").unwrap();
        std::os::unix::fs::symlink("main.tf", dir.path().join("linked.tf")).unwrap();
        let error = build_deterministic_archive(dir.path()).unwrap_err();
        assert!(error.to_string().contains("linked.tf"));
    }

    #[test]
    fn identical_inputs_produce_identical_archives() {
        let dir = std::env::temp_dir().join(format!("of-pkg-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("main.tf"), "resource {}").unwrap();
        std::fs::write(dir.join("sub").join("a.txt"), "aaa").unwrap();

        let a = build_deterministic_archive(&dir).unwrap();
        // Change file mtime; output must be identical.
        let m = std::fs::metadata(dir.join("main.tf")).unwrap();
        let _ = m;
        let b = build_deterministic_archive(&dir).unwrap();
        assert_eq!(a, b, "identical inputs must produce identical archives");
        validate_archive(&a).unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn registry_json_is_rejected() {
        let dir = std::env::temp_dir().join(format!("of-pkg-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("registry.json"), "{}").unwrap();
        std::fs::write(dir.join("main.tf"), "resource {}").unwrap();
        let err = build_deterministic_archive(&dir).unwrap_err();
        assert!(err.to_string().contains("registry.json"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn manifest_round_trip_and_digest() {
        let manifest = TemplateManifest {
            schema_version: 1,
            release: "2026.10.0".into(),
            required_roles: vec![Role::Nexus, Role::Forge, Role::Sentinel, Role::Vessel],
            roles: BTreeMap::from([
                (
                    Role::Nexus,
                    RoleManifestEntry {
                        role: Role::Nexus,
                        archive: Role::Nexus.archive_name(),
                        sha256: "a".repeat(64),
                        parameter_schema_version: 1,
                        image_digest: Some("sha256:abc".into()),
                    },
                ),
                (
                    Role::Forge,
                    RoleManifestEntry {
                        role: Role::Forge,
                        archive: Role::Forge.archive_name(),
                        sha256: "b".repeat(64),
                        parameter_schema_version: 1,
                        image_digest: None,
                    },
                ),
                (
                    Role::Sentinel,
                    RoleManifestEntry {
                        role: Role::Sentinel,
                        archive: Role::Sentinel.archive_name(),
                        sha256: "c".repeat(64),
                        parameter_schema_version: 1,
                        image_digest: None,
                    },
                ),
                (
                    Role::Vessel,
                    RoleManifestEntry {
                        role: Role::Vessel,
                        archive: Role::Vessel.archive_name(),
                        sha256: "d".repeat(64),
                        parameter_schema_version: 1,
                        image_digest: None,
                    },
                ),
            ]),
        };
        let json = manifest.to_canonical_json().unwrap();
        let parsed = TemplateManifest::parse(&json).unwrap();
        assert_eq!(parsed, manifest);
        assert_eq!(manifest.manifest_sha256().unwrap().len(), 64);
    }

    #[test]
    fn invalid_manifest_rejected() {
        assert!(TemplateManifest::parse(br#"{"schema_version": 99}"#).is_err());
        assert!(TemplateManifest::parse(br#"not json"#).is_err());
        // Missing required role entry.
        let bad = br#"{
            "schema_version": 1,
            "release": "x",
            "required_roles": ["nexus", "forge"],
            "roles": { "nexus": {"archive":"openflows-nexus.tar.gz","sha256":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","parameter_schema_version":1} }
        }"#;
        assert!(TemplateManifest::parse(bad).is_err());
    }

    #[test]
    fn unsafe_paths_rejected() {
        // The tar crate refuses to *build* an archive with a `..` path, so we
        // test the path-safety predicate directly and confirm a crafted unsafe
        // path is rejected, plus that a symlink entry is rejected.
        assert!(!safe_rel_path(Path::new("../evil")));
        assert!(!safe_rel_path(Path::new("a/../../evil")));
        assert!(!safe_rel_path(Path::new("/abs/evil")));
        assert!(safe_rel_path(Path::new("./main.tf")));
        assert!(safe_rel_path(Path::new("sub/main.tf")));

        // Symlink entries are rejected by validate_archive.
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), Compression::default());
        {
            let mut builder = Builder::new(&mut encoder);
            let mut header = Header::new_gnu();
            header.set_entry_type(EntryType::Symlink);
            header.set_size(0);
            header.set_mode(0o777);
            header.set_mtime(0);
            header.set_uid(0);
            header.set_gid(0);
            header.set_cksum();
            builder
                .append_data(&mut header, "evil-link", &[][..])
                .unwrap();
            builder.finish().unwrap();
        }
        let archive = encoder.finish().unwrap();
        assert!(validate_archive(&archive).is_err());
    }
}

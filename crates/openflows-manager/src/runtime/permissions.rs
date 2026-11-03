//! Server-controlled runtime permission profiles for the GitHub credential
//! broker.
//!
//! Permission profiles are derived entirely from trusted server state — the
//! workspace's stored agent role and the requested runtime purpose. A caller
//! can never supply or override an organization, installation, repository, or
//! permission set: the broker resolves the profile itself and the only
//! caller-controlled input is the coarse `purpose` (`git` or `api`).
//!
//! Profiles are explicit allowlists grounded in the actual runtime needs from
//! the centralized-deployment specification (02-github-app.md §2). They are
//! server-controlled and operator-versioned, never supplied by a worker.

use crate::error::ManagerError;
use std::collections::BTreeMap;

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum Permission {
    Contents,
    Issues,
    PullRequests,
    Checks,
    Statuses,
    Actions,
    Workflows,
    Metadata,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PermissionLevel {
    Read,
    Write,
}

/// Server profiles use typed names and levels; strings exist only at the API boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Permissions(pub BTreeMap<Permission, PermissionLevel>);

impl Permissions {
    pub fn to_wire(&self) -> BTreeMap<String, String> {
        serde_json::from_value(serde_json::to_value(&self.0).expect("permission serialization"))
            .expect("permission map")
    }
}

/// GitHub automatically includes read-only metadata; no other surplus grant is allowed.
pub fn grants_match(
    actual: &BTreeMap<String, String>,
    expected: &BTreeMap<String, String>,
) -> bool {
    let mut actual = actual.clone();
    if actual.get("metadata").map(String::as_str) == Some("read")
        && !expected.contains_key("metadata")
    {
        actual.remove("metadata");
    }
    actual == *expected
}

/// The coarse runtime purpose accepted by the broker endpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimePurpose {
    /// A credential for Git protocol operations (clone/fetch/push).
    Git,
    /// A credential for REST API operations (issues, PRs, checks, CI).
    Api,
}

impl RuntimePurpose {
    pub fn parse(raw: &str) -> Result<Self, ManagerError> {
        match raw {
            "git" => Ok(RuntimePurpose::Git),
            "api" => Ok(RuntimePurpose::Api),
            other => Err(ManagerError::api(
                "INVALID_INPUT",
                format!("invalid runtime purpose '{other}': expected 'git' or 'api'"),
            )),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            RuntimePurpose::Git => "git",
            RuntimePurpose::Api => "api",
        }
    }
}

/// A resolved, server-controlled permission profile: the exact GitHub
/// permission map to request and a stable hash recorded on the credential
/// lease.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PermissionProfile {
    /// Human/operator-readable profile name, e.g. "forge.git".
    pub name: String,
    /// GitHub permission name -> level ("read"/"write").
    pub permissions: Permissions,
    /// SHA-256 hex digest of the canonical (sorted) permission map. Recorded
    /// on the lease and used as part of the single-flight cache key.
    pub profile_hash: String,
}

impl PermissionProfile {
    /// Canonicalize and hash the permission map so equal profiles always hash
    /// equally regardless of insertion order.
    fn hash(permissions: &BTreeMap<String, String>) -> String {
        use sha2::{Digest, Sha256};
        let mut hasher = Sha256::new();
        for (k, v) in permissions {
            hasher.update(k.as_bytes());
            hasher.update(b"=");
            hasher.update(v.as_bytes());
            hasher.update(b";");
        }
        format!("{:x}", hasher.finalize())
    }

    fn build(role: &str, purpose: RuntimePurpose, permissions: Permissions) -> Self {
        let name = format!("{role}.{}", purpose.as_str());
        let profile_hash = Self::hash(&permissions.to_wire());
        PermissionProfile {
            name,
            permissions,
            profile_hash,
        }
    }
}

/// Resolve the approved permission profile for a workspace agent `role` and a
/// runtime `purpose`. Unknown roles fail closed (no default/wildcard profile).
pub fn resolve_profile(
    role: &str,
    purpose: RuntimePurpose,
) -> Result<PermissionProfile, ManagerError> {
    use Permission::*;
    use PermissionLevel::*;
    use RuntimePurpose::*;
    let perms = match (role, purpose) {
        // Git protocol: Forge/Vessel/Lore need to push; Nexus/Sentinel read.
        ("nexus", Git) | ("sentinel", Git) => map(&[(Contents, Read)]),
        // Forge alone may update workflow files; GitHub requires the separate
        // workflows:write grant in addition to contents:write for those pushes.
        ("forge", Git) => map(&[(Contents, Write), (Workflows, Write)]),
        ("vessel", Git) | ("lore", Git) => map(&[(Contents, Write)]),
        // API: Nexus coordinates issues/PRs; Forge/Sentinel review; Vessel CI.
        ("nexus", Api) => map(&[(Contents, Read), (Issues, Write), (PullRequests, Write)]),
        ("forge", Api) => map(&[(Contents, Write), (PullRequests, Write)]),
        ("sentinel", Api) => map(&[(Contents, Read), (PullRequests, Write), (Checks, Read)]),
        ("vessel", Api) => map(&[
            (Contents, Write),
            (PullRequests, Write),
            (Checks, Read),
            (Statuses, Read),
            (Actions, Read),
        ]),
        ("lore", Api) => map(&[(Contents, Write), (PullRequests, Write)]),
        _ => {
            return Err(ManagerError::api(
                "INVALID_INPUT",
                format!(
                    "no permission profile for role '{role}' and purpose '{}'",
                    purpose.as_str()
                ),
            ))
        }
    };
    // `metadata` is implicitly granted by GitHub for installation tokens and is
    // never requested explicitly.
    Ok(PermissionProfile::build(role, purpose, perms))
}

fn map(pairs: &[(Permission, PermissionLevel)]) -> Permissions {
    Permissions(pairs.iter().copied().collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use Permission::*;
    use PermissionLevel::*;

    #[test]
    fn vessel_uses_statuses_and_only_implicit_read_metadata_is_accepted() {
        let expected = resolve_profile("vessel", RuntimePurpose::Api)
            .unwrap()
            .permissions
            .to_wire();
        assert_eq!(expected.get("statuses").map(String::as_str), Some("read"));
        assert!(!expected.contains_key("commit_statuses"));
        let mut actual = expected.clone();
        actual.insert("metadata".into(), "read".into());
        assert!(grants_match(&actual, &expected));
        actual.insert("metadata".into(), "write".into());
        assert!(!grants_match(&actual, &expected));
        actual.insert("metadata".into(), "read".into());
        actual.insert("administration".into(), "write".into());
        assert!(!grants_match(&actual, &expected));
    }

    #[test]
    fn profile_hash_is_order_independent() {
        let a = PermissionProfile::build(
            "forge",
            RuntimePurpose::Api,
            map(&[(Contents, Write), (PullRequests, Write)]),
        );
        let b = PermissionProfile::build(
            "forge",
            RuntimePurpose::Api,
            map(&[(PullRequests, Write), (Contents, Write)]),
        );
        assert_eq!(a.profile_hash, b.profile_hash);
    }

    #[test]
    fn unknown_role_fails_closed() {
        assert!(resolve_profile("unknown", RuntimePurpose::Git).is_err());
        assert!(resolve_profile("", RuntimePurpose::Api).is_err());
    }

    #[test]
    fn metadata_is_never_requested_explicitly() {
        let profile = resolve_profile("forge", RuntimePurpose::Git).unwrap();
        assert!(!profile.permissions.to_wire().contains_key("metadata"));
        assert_eq!(
            profile
                .permissions
                .to_wire()
                .get("contents")
                .map(String::as_str),
            Some("write")
        );
    }

    #[test]
    fn workflow_write_is_limited_to_forge_git_profile() {
        let forge_git = resolve_profile("forge", RuntimePurpose::Git)
            .unwrap()
            .permissions
            .to_wire();
        assert_eq!(
            forge_git.get("workflows").map(String::as_str),
            Some("write")
        );

        for (role, purpose) in [
            ("nexus", RuntimePurpose::Git),
            ("sentinel", RuntimePurpose::Git),
            ("vessel", RuntimePurpose::Git),
            ("lore", RuntimePurpose::Git),
            ("forge", RuntimePurpose::Api),
        ] {
            let permissions = resolve_profile(role, purpose)
                .unwrap()
                .permissions
                .to_wire();
            assert!(
                !permissions.contains_key("workflows"),
                "{role}.{} must not receive workflows permission",
                purpose.as_str()
            );
        }
    }

    #[test]
    fn purpose_changes_git_vs_api_scope() {
        let git = resolve_profile("forge", RuntimePurpose::Git).unwrap();
        let api = resolve_profile("forge", RuntimePurpose::Api).unwrap();
        assert_eq!(git.permissions.0.len(), 2);
        assert!(api.permissions.to_wire().contains_key("pull_requests"));
    }
}

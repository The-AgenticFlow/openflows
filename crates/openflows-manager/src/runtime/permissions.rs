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
    pub permissions: BTreeMap<String, String>,
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

    fn build(role: &str, purpose: RuntimePurpose, permissions: BTreeMap<String, String>) -> Self {
        let name = format!("{role}.{}", purpose.as_str());
        let profile_hash = Self::hash(&permissions);
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
    use RuntimePurpose::*;
    let perms: BTreeMap<String, String> = match (role, purpose) {
        // Git protocol: Forge/Vessel/Lore need to push; Nexus/Sentinel read.
        ("nexus", Git) | ("sentinel", Git) => map(&[("contents", "read")]),
        ("forge", Git) | ("vessel", Git) | ("lore", Git) => map(&[("contents", "write")]),
        // API: Nexus coordinates issues/PRs; Forge/Sentinel review; Vessel CI.
        ("nexus", Api) => map(&[
            ("contents", "read"),
            ("issues", "write"),
            ("pull_requests", "write"),
        ]),
        ("forge", Api) => map(&[("contents", "write"), ("pull_requests", "write")]),
        ("sentinel", Api) => map(&[
            ("contents", "read"),
            ("pull_requests", "write"),
            ("checks", "read"),
        ]),
        ("vessel", Api) => map(&[
            ("contents", "write"),
            ("pull_requests", "write"),
            ("checks", "read"),
            ("commit_statuses", "read"),
            ("actions", "read"),
        ]),
        ("lore", Api) => map(&[("contents", "write"), ("pull_requests", "write")]),
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

fn map(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profile_hash_is_order_independent() {
        let a = PermissionProfile::build(
            "forge",
            RuntimePurpose::Api,
            map(&[("contents", "write"), ("pull_requests", "write")]),
        );
        let b = PermissionProfile::build(
            "forge",
            RuntimePurpose::Api,
            map(&[("pull_requests", "write"), ("contents", "write")]),
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
        assert!(!profile.permissions.contains_key("metadata"));
        assert_eq!(
            profile.permissions.get("contents").map(String::as_str),
            Some("write")
        );
    }

    #[test]
    fn purpose_changes_git_vs_api_scope() {
        let git = resolve_profile("forge", RuntimePurpose::Git).unwrap();
        let api = resolve_profile("forge", RuntimePurpose::Api).unwrap();
        assert_eq!(git.permissions.len(), 1);
        assert!(api.permissions.contains_key("pull_requests"));
    }
}

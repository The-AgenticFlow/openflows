// crates/pocketflow-core/src/types.rs
//
// Shared types used across multiple agents.
// Centralized here to avoid duplication and ensure consistency.

use serde::{Deserialize, Serialize};

/// CI check status - returned by GitHub API for check suites and combined status.
/// Used by VESSEL for merge gate logic, potentially by other agents for status checks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CiStatus {
    /// Checks are still running
    Pending,
    /// All checks passed
    Success,
    /// One or more checks failed
    Failure,
    /// Check encountered an error
    Error,
}

impl CiStatus {
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            CiStatus::Success | CiStatus::Failure | CiStatus::Error
        )
    }

    pub fn is_success(&self) -> bool {
        matches!(self, CiStatus::Success)
    }
}

/// Result of a PR merge operation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MergeResult {
    /// Whether the merge was successful
    pub merged: bool,
    /// SHA of the merge commit (if successful)
    pub sha: Option<String>,
    /// Human-readable message from GitHub
    pub message: String,
}

/// Configuration for CI polling behavior.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CiPollConfig {
    /// Interval between polls in seconds
    pub interval_secs: u64,
    /// Maximum number of poll attempts before timeout
    pub max_attempts: u32,
    /// Per-repository polling interval overrides (repo_name -> interval_secs)
    /// Allows faster polling for repos with known-fast CI
    #[serde(default)]
    pub repo_intervals: std::collections::HashMap<String, u64>,
}

impl Default for CiPollConfig {
    fn default() -> Self {
        Self {
            interval_secs: 10,
            max_attempts: 60, // 10 minutes total
            repo_intervals: std::collections::HashMap::new(),
        }
    }
}

impl CiPollConfig {
    /// Get the polling interval for a specific repository.
    /// Returns the per-repo override if configured, otherwise the default interval.
    pub fn interval_for_repo(&self, repo: &str) -> u64 {
        self.repo_intervals
            .get(repo)
            .copied()
            .unwrap_or(self.interval_secs)
    }
}

/// Merge method for PR merges.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MergeMethod {
    /// Create a merge commit
    Merge,
    /// Squash all commits into one
    #[default]
    Squash,
    /// Rebase commits onto target branch
    Rebase,
}

/// PR information relevant for merge gate operations.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrInfo {
    pub number: u64,
    pub head_sha: String,
    pub head_branch: String,
    pub base_branch: String,
    pub ticket_id: Option<String>,
    pub title: String,
    pub body: Option<String>,
    pub state: PrState,
    /// Whether the PR can be merged without conflicts.
    /// `None` means GitHub hasn't computed it yet (retry later).
    pub mergeable: Option<bool>,
}

impl PrInfo {
    /// Returns true if the PR is confirmed to have merge conflicts.
    pub fn has_conflicts(&self) -> bool {
        self.mergeable == Some(false)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PrState {
    Open,
    Closed,
    Merged,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ci_status_is_terminal() {
        assert!(!CiStatus::Pending.is_terminal());
        assert!(CiStatus::Success.is_terminal());
        assert!(CiStatus::Failure.is_terminal());
        assert!(CiStatus::Error.is_terminal());
    }

    #[test]
    fn test_ci_status_is_success() {
        assert!(CiStatus::Success.is_success());
        assert!(!CiStatus::Pending.is_success());
        assert!(!CiStatus::Failure.is_success());
    }

    #[test]
    fn test_ci_status_serde() {
        let status = CiStatus::Success;
        let json = serde_json::to_string(&status).unwrap();
        assert_eq!(json, "\"success\"");

        let parsed: CiStatus = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, CiStatus::Success);
    }

    #[test]
    fn test_merge_method_default() {
        assert!(matches!(MergeMethod::default(), MergeMethod::Squash));
    }

    #[test]
    fn test_merge_method_serde() {
        let method = MergeMethod::Squash;
        let json = serde_json::to_string(&method).unwrap();
        assert_eq!(json, "\"squash\"");

        let parsed: MergeMethod = serde_json::from_str(&json).unwrap();
        assert!(matches!(parsed, MergeMethod::Squash));
    }

    #[test]
    fn test_ci_poll_config_default() {
        let config = CiPollConfig::default();
        assert_eq!(config.interval_secs, 10);
        assert_eq!(config.max_attempts, 60);
    }

    #[test]
    fn test_pr_info_serde() {
        let pr_info = PrInfo {
            number: 42,
            head_sha: "abc123".to_string(),
            head_branch: "feature-branch".to_string(),
            base_branch: "main".to_string(),
            ticket_id: Some("T-42".to_string()),
            title: "Add feature".to_string(),
            body: Some("Test body".to_string()),
            state: PrState::Open,
            mergeable: Some(true),
        };

        let json = serde_json::to_string(&pr_info).unwrap();
        let parsed: PrInfo = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.number, 42);
        assert_eq!(parsed.head_sha, "abc123");
        assert_eq!(parsed.ticket_id, Some("T-42".to_string()));
        assert_eq!(parsed.mergeable, Some(true));
    }

    #[test]
    fn test_pr_info_has_conflicts() {
        let conflicting = PrInfo {
            number: 1,
            head_sha: "a".to_string(),
            head_branch: "f".to_string(),
            base_branch: "main".to_string(),
            ticket_id: None,
            title: "t".to_string(),
            body: None,
            state: PrState::Open,
            mergeable: Some(false),
        };
        assert!(conflicting.has_conflicts());

        let clean = PrInfo {
            number: 1,
            head_sha: "a".to_string(),
            head_branch: "f".to_string(),
            base_branch: "main".to_string(),
            ticket_id: None,
            title: "t".to_string(),
            body: None,
            state: PrState::Open,
            mergeable: Some(true),
        };
        assert!(!clean.has_conflicts());

        let unknown = PrInfo {
            number: 1,
            head_sha: "a".to_string(),
            head_branch: "f".to_string(),
            base_branch: "main".to_string(),
            ticket_id: None,
            title: "t".to_string(),
            body: None,
            state: PrState::Open,
            mergeable: None,
        };
        assert!(!unknown.has_conflicts());
    }

    #[test]
    fn test_merge_result_serde() {
        let result = MergeResult {
            merged: true,
            sha: Some("def456".to_string()),
            message: "Pull request successfully merged".to_string(),
        };

        let json = serde_json::to_string(&result).unwrap();
        let parsed: MergeResult = serde_json::from_str(&json).unwrap();
        assert!(parsed.merged);
        assert_eq!(parsed.sha, Some("def456".to_string()));
    }
}

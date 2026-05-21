// crates/agent-vessel/src/ci_poller.rs
//
// CI Status Polling — separated for modularity and reusability.
// Handles the "gate" phase of VESSEL's workflow.

use anyhow::Result;
use pocketflow_core::{CiPollConfig, CiStatus, PrInfo};
use tracing::{debug, info, warn};

/// CI Poller — polls GitHub for CI status until terminal state or timeout.
/// Also detects merge conflicts early via the `mergeable` field.
pub struct CiPoller {
    config: CiPollConfig,
    client: github::GithubRestClient,
}

/// How often (in poll attempts) to re-check mergeability.
/// GitHub may compute `mergeable: null` initially and fill it in later.
const MERGEABLE_CHECK_INTERVAL: u32 = 3;

impl CiPoller {
    pub fn new(config: CiPollConfig, client: github::GithubRestClient) -> Self {
        Self { config, client }
    }

    pub fn client(&self) -> &github::GithubRestClient {
        &self.client
    }

    /// Poll CI status until it reaches a terminal state or times out.
    /// Also checks mergeability on each iteration — if the PR has conflicts,
    /// returns `Conflicts` immediately instead of waiting for timeout.
    /// Uses per-repo polling interval if configured, otherwise default.
    pub async fn poll_until_terminal(
        &self,
        owner: &str,
        repo: &str,
        pr_info: &PrInfo,
    ) -> Result<CiPollResult> {
        let mut attempts = 0u32;
        // Use per-repo interval if configured, otherwise default
        let interval_secs = self.config.interval_for_repo(repo);

        loop {
            if attempts >= self.config.max_attempts {
                warn!(
                    pr = pr_info.number,
                    attempts, "CI polling timed out after {} attempts", attempts
                );
                return Ok(CiPollResult::Timeout);
            }

            let status = self
                .client
                .get_ci_status(owner, repo, &pr_info.head_sha)
                .await?;

            debug!(pr = pr_info.number, status = ?status, attempt = attempts, "CI status check");

            if status.is_terminal() {
                info!(pr = pr_info.number, status = ?status, "CI reached terminal state");
                return Ok(CiPollResult::Status(status));
            }

            if attempts.is_multiple_of(MERGEABLE_CHECK_INTERVAL) {
                let mergeable = self.check_mergeability(owner, repo, pr_info).await?;
                if mergeable == Some(false) {
                    warn!(
                        pr = pr_info.number,
                        "PR has merge conflicts — short-circuiting CI poll"
                    );
                    return Ok(CiPollResult::Conflicts);
                }
                if mergeable.is_none() {
                    debug!(
                        pr = pr_info.number,
                        "PR mergeability unknown (GitHub still computing) — continuing poll"
                    );
                }
            }

            attempts += 1;
            tokio::time::sleep(std::time::Duration::from_secs(interval_secs)).await;
        }
    }

    /// Re-fetch the PR to check current mergeability state.
    /// Returns `Some(true)` if mergeable, `Some(false)` if conflicting, `None` if unknown.
    async fn check_mergeability(
        &self,
        owner: &str,
        repo: &str,
        pr_info: &PrInfo,
    ) -> Result<Option<bool>> {
        let fresh_pr = self
            .client
            .get_pull_request(owner, repo, pr_info.number)
            .await?;
        Ok(fresh_pr.mergeable)
    }
}

/// Result of CI polling.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CiPollResult {
    /// CI reached a terminal status
    Status(CiStatus),
    /// Polling timed out before terminal state
    Timeout,
    /// PR has merge conflicts — CI won't run until resolved
    Conflicts,
}

impl CiPollResult {
    pub fn is_success(&self) -> bool {
        matches!(self, CiPollResult::Status(CiStatus::Success))
    }

    pub fn is_failure(&self) -> bool {
        matches!(
            self,
            CiPollResult::Status(CiStatus::Failure | CiStatus::Error)
        )
    }

    pub fn is_timeout(&self) -> bool {
        matches!(self, CiPollResult::Timeout)
    }

    pub fn is_conflicts(&self) -> bool {
        matches!(self, CiPollResult::Conflicts)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ci_poll_result_is_success() {
        assert!(CiPollResult::Status(CiStatus::Success).is_success());
        assert!(!CiPollResult::Status(CiStatus::Failure).is_success());
        assert!(!CiPollResult::Timeout.is_success());
    }

    #[test]
    fn test_ci_poll_result_is_failure() {
        assert!(CiPollResult::Status(CiStatus::Failure).is_failure());
        assert!(CiPollResult::Status(CiStatus::Error).is_failure());
        assert!(!CiPollResult::Status(CiStatus::Success).is_failure());
        assert!(!CiPollResult::Timeout.is_failure());
    }

    #[test]
    fn test_ci_poll_result_is_timeout() {
        assert!(CiPollResult::Timeout.is_timeout());
        assert!(!CiPollResult::Status(CiStatus::Success).is_timeout());
    }

    #[test]
    fn test_ci_poll_result_is_conflicts() {
        assert!(CiPollResult::Conflicts.is_conflicts());
        assert!(!CiPollResult::Timeout.is_conflicts());
        assert!(!CiPollResult::Status(CiStatus::Success).is_conflicts());
    }
}

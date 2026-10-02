// crates/agent-vessel/src/merger.rs
//
// PR Merge Execution — separated for modularity and reusability.
// Handles the "merge" phase of VESSEL's workflow.

use anyhow::Result;
use pocketflow_core::{MergeMethod, MergeResult, PrInfo};
use tracing::{info, warn};

/// PR Merger — executes PR merges via GitHub API.
pub struct PrMerger {
    client: github::GithubRestClient,
    default_method: MergeMethod,
}

impl PrMerger {
    pub fn new(client: github::GithubRestClient, default_method: MergeMethod) -> Self {
        Self {
            client,
            default_method,
        }
    }

    /// Merge a PR with the configured method.
    /// Commit message includes ticket reference for GitHub issue linking.
    pub async fn merge(&self, owner: &str, repo: &str, pr_info: &PrInfo) -> Result<MergeResult> {
        let commit_title = build_merge_commit_title(pr_info);

        info!(
            pr = pr_info.number,
            ticket_id = ?pr_info.ticket_id,
            method = ?self.default_method,
            "Attempting to merge PR"
        );

        let result = self
            .client
            .merge_pull_request(
                owner,
                repo,
                pr_info.number,
                &commit_title,
                self.default_method,
                &pr_info.head_sha,
            )
            .await?;

        if result.merged {
            info!(
                pr = pr_info.number,
                sha = ?result.sha,
                "PR merged successfully"
            );
        } else {
            warn!(
                pr = pr_info.number,
                message = %result.message,
                "Merge failed"
            );
        }

        Ok(result)
    }
}

/// Build the commit title for a merge.
/// Format: "Merge PR #123: Ticket title (Closes #456)"
///
/// Uses `Closes #N` with the GitHub issue number so GitHub auto-closes
/// the corresponding issue when the PR is merged. The issue number is
/// extracted from the ticket_id format `T-{issue_number:03}`.
fn build_merge_commit_title(pr_info: &PrInfo) -> String {
    let mut title = format!("Merge PR #{}: {}", pr_info.number, pr_info.title);

    if let Some(ticket_id) = &pr_info.ticket_id {
        if let Some(issue_number) = ticket_id.strip_prefix("T-") {
            title.push_str(&format!(" (Closes #{})", issue_number));
        } else {
            title.push_str(&format!(" (Resolves {})", ticket_id));
        }
    }

    title
}

#[cfg(test)]
mod tests {
    use super::*;
    use pocketflow_core::PrState;

    #[test]
    fn test_build_merge_commit_title_with_ticket() {
        let pr_info = PrInfo {
            number: 42,
            head_sha: "abc123".to_string(),
            head_branch: "feature".to_string(),
            base_branch: "main".to_string(),
            ticket_id: Some("T-100".to_string()),
            title: "Add new feature".to_string(),
            body: None,
            state: PrState::Open,
            mergeable: Some(true),
        };

        let title = build_merge_commit_title(&pr_info);
        assert_eq!(title, "Merge PR #42: Add new feature (Closes #100)");
    }

    #[test]
    fn test_build_merge_commit_title_without_ticket() {
        let pr_info = PrInfo {
            number: 42,
            head_sha: "abc123".to_string(),
            head_branch: "feature".to_string(),
            base_branch: "main".to_string(),
            ticket_id: None,
            title: "Add new feature".to_string(),
            body: None,
            state: PrState::Open,
            mergeable: None,
        };

        let title = build_merge_commit_title(&pr_info);
        assert_eq!(title, "Merge PR #42: Add new feature");
    }
}

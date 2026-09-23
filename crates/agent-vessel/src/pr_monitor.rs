// crates/agent-vessel/src/pr_monitor.rs
//
// PR-state lifecycle monitor — observes GitHub-native PR states
// (conflicts, changes_requested, approved, comments, ci_running,
// ready_for_merge) and computes the rework directive to dispatch to FORGE.

use anyhow::Result;
use github::{effective_review_state, PrReviewState};
use pocketflow_core::{CiStatus, PrInfo};
use tracing::{debug, warn};

/// The observed lifecycle state of a pull request, in priority order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrMonitorState {
    /// PR has merge conflicts with the base branch.
    Conflicts,
    /// CI failed (or errored).
    CiFailed,
    /// CI is still running.
    CiRunning,
    /// A reviewer has requested changes on GitHub.
    ChangesRequested,
    /// Inline review comments are present (not yet approved).
    Comments,
    /// A reviewer has approved but the PR is not yet merge-ready (e.g. CI pending).
    Approved,
    /// Approved + no conflicts + CI success — safe to merge.
    ReadyForMerge,
}

impl PrMonitorState {
    pub fn as_str(&self) -> &'static str {
        match self {
            PrMonitorState::Conflicts => "conflicts",
            PrMonitorState::CiFailed => "ci_failed",
            PrMonitorState::CiRunning => "ci_running",
            PrMonitorState::ChangesRequested => "changes_requested",
            PrMonitorState::Comments => "comments",
            PrMonitorState::Approved => "approved",
            PrMonitorState::ReadyForMerge => "ready_for_merge",
        }
    }

    /// Whether this state requires a rework directive to be dispatched to FORGE.
    pub fn needs_rework(&self) -> bool {
        matches!(
            self,
            PrMonitorState::Conflicts | PrMonitorState::ChangesRequested | PrMonitorState::Comments
        )
    }
}

/// A structured directive to hand to FORGE via `/address_review`.
///
/// Kept intentionally **minimal**: VESSEL only points FORGE at the PR and its
/// review state. FORGE has the `/address_review` command and its skills to
/// fetch the full review context (inline comments, conflicts) itself — VESSEL
/// does not embed the whole review text, which would bloat the chat message
/// and duplicate what FORGE can read directly from GitHub.
#[derive(Debug, Clone)]
pub struct ReworkDirective {
    pub state: PrMonitorState,
    pub pr_number: u64,
    /// Optional one-line reason (latest review body) as a short pointer.
    pub reason: Option<String>,
}

/// Pure state classifier over the inputs that determine the PR lifecycle.
///
/// Priority (per plan):
/// 1. conflicts > 2. ci_failed > 3. ci_running > 4. changes_requested >
/// 5. comments > 6. ready_for_merge > 7. approved.
///
/// Comments are considered "unaddressed" (and thus rework-worthy) only when
/// the PR is not currently approved — an approved PR's comments are treated
/// as addressed so approval can still progress to merge.
pub fn classify_from_parts(
    mergeable: Option<bool>,
    ci_status: CiStatus,
    review_state: PrReviewState,
    has_unaddressed_comments: bool,
) -> PrMonitorState {
    if mergeable == Some(false) {
        return PrMonitorState::Conflicts;
    }
    match ci_status {
        CiStatus::Failure | CiStatus::Error => return PrMonitorState::CiFailed,
        CiStatus::Pending => return PrMonitorState::CiRunning,
        CiStatus::Success => {}
    }
    match review_state {
        PrReviewState::ChangesRequested => return PrMonitorState::ChangesRequested,
        PrReviewState::Approved => {}
        _ => {}
    }
    // Comments are only a rework trigger while the PR is not approved. An
    // approved PR's comments are treated as addressed so approval can merge.
    if has_unaddressed_comments && review_state != PrReviewState::Approved {
        return PrMonitorState::Comments;
    }
    match review_state {
        PrReviewState::Approved => PrMonitorState::ReadyForMerge,
        _ => PrMonitorState::Approved,
    }
}

/// Classify the lifecycle state of a PR by querying GitHub for reviews and
/// inline comments in addition to the CI status and mergeability snapshot.
pub async fn classify(
    client: &github::GithubRestClient,
    owner: &str,
    repo: &str,
    pr_info: &PrInfo,
    ci_status: CiStatus,
) -> Result<PrMonitorState> {
    let reviews = match client.list_pr_reviews(owner, repo, pr_info.number).await {
        Ok(r) => r,
        Err(e) => {
            warn!(
                pr = pr_info.number,
                error = %e,
                "Failed to list PR reviews — treating as no reviews"
            );
            Vec::new()
        }
    };
    let review_state = effective_review_state(&reviews);

    let comments = match client
        .list_review_comments(owner, repo, pr_info.number)
        .await
    {
        Ok(c) => c,
        Err(e) => {
            warn!(
                pr = pr_info.number,
                error = %e,
                "Failed to list review comments — treating as none"
            );
            Vec::new()
        }
    };

    // v1 heuristic: comments are "unaddressed" when there are any and the PR
    // is not yet approved. Refinable later (e.g. compare to last review_ready).
    let has_unaddressed_comments = !comments.is_empty() && review_state != PrReviewState::Approved;

    debug!(
        pr = pr_info.number,
        mergeable = ?pr_info.mergeable,
        ci = ?ci_status,
        review_state = ?review_state,
        comments = comments.len(),
        "PR lifecycle classification inputs"
    );

    Ok(classify_from_parts(
        pr_info.mergeable,
        ci_status,
        review_state,
        has_unaddressed_comments,
    ))
}

/// Gather a short pointer (latest review body) for the `/address_review`
/// directive. Full inline comments are NOT fetched here — FORGE reads them
/// directly from GitHub via the `/address_review` command / skills.
pub async fn collect_rework(
    client: &github::GithubRestClient,
    owner: &str,
    repo: &str,
    pr_number: u64,
) -> ReworkDirective {
    let reason = client
        .list_pr_reviews(owner, repo, pr_number)
        .await
        .unwrap_or_default()
        .into_iter()
        .filter(|r| !r.body.as_deref().unwrap_or("").trim().is_empty())
        .max_by(|a, b| a.submitted_at.cmp(&b.submitted_at))
        .and_then(|r| r.body);

    ReworkDirective {
        state: PrMonitorState::Comments,
        pr_number,
        reason,
    }
}

/// Build the lightweight `/address_review` chat directive for FORGE.
///
/// This is a **pointer**, not a dump: it names the PR, its review state, and
/// tells FORGE to use the `/address_review` command to pull the full review
/// context (inline comments / conflicts) itself, then re-arm the PR.
pub fn build_directive(
    state: PrMonitorState,
    pr_number: u64,
    reason: Option<&str>,
) -> String {
    let mut out = String::from("/address_review\n");
    out.push_str(&format!("state: {}\n", state.as_str()));
    out.push_str(&format!("pr: {}\n", pr_number));
    if let Some(reason) = reason.filter(|r| !r.trim().is_empty()) {
        out.push_str(&format!("reason: {}\n", reason.trim()));
    }
    out.push_str(
        "\nFetch the full review context (inline comments / conflicted files) \
         for this PR and address them, then re-run: openflows-harness status set review_ready",
    );
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_priority_conflicts_first() {
        assert_eq!(
            classify_from_parts(
                Some(false),
                CiStatus::Success,
                PrReviewState::Approved,
                false
            ),
            PrMonitorState::Conflicts
        );
        assert_eq!(
            classify_from_parts(Some(false), CiStatus::Pending, PrReviewState::None, false),
            PrMonitorState::Conflicts
        );
    }

    #[test]
    fn classify_ci_takes_precedence_over_review() {
        assert_eq!(
            classify_from_parts(
                Some(true),
                CiStatus::Failure,
                PrReviewState::Approved,
                false
            ),
            PrMonitorState::CiFailed
        );
        assert_eq!(
            classify_from_parts(
                Some(true),
                CiStatus::Pending,
                PrReviewState::ChangesRequested,
                true
            ),
            PrMonitorState::CiRunning
        );
    }

    #[test]
    fn classify_changes_requested_over_comments() {
        assert_eq!(
            classify_from_parts(
                Some(true),
                CiStatus::Success,
                PrReviewState::ChangesRequested,
                true
            ),
            PrMonitorState::ChangesRequested
        );
    }

    #[test]
    fn classify_comments_when_not_approved() {
        assert_eq!(
            classify_from_parts(Some(true), CiStatus::Success, PrReviewState::None, true),
            PrMonitorState::Comments
        );
    }

    #[test]
    fn classify_approved_comments_do_not_block_merge() {
        // An approved PR with comments is merge-ready (comments treated as addressed).
        assert_eq!(
            classify_from_parts(Some(true), CiStatus::Success, PrReviewState::Approved, true),
            PrMonitorState::ReadyForMerge
        );
    }

    #[test]
    fn classify_ready_for_merge_when_approved_green() {
        assert_eq!(
            classify_from_parts(
                Some(true),
                CiStatus::Success,
                PrReviewState::Approved,
                false
            ),
            PrMonitorState::ReadyForMerge
        );
    }

    #[test]
    fn classify_approved_but_ci_running() {
        assert_eq!(
            classify_from_parts(
                Some(true),
                CiStatus::Pending,
                PrReviewState::Approved,
                false
            ),
            PrMonitorState::CiRunning
        );
    }

    #[test]
    fn build_directive_is_minimal_pointer() {
        let d = build_directive(
            PrMonitorState::ChangesRequested,
            42,
            Some("missing pagination per spec"),
        );
        assert!(d.contains("/address_review"));
        assert!(d.contains("state: changes_requested"));
        assert!(d.contains("pr: 42"));
        assert!(d.contains("reason: missing pagination per spec"));
        // Pointer, not a dump: no inline comments are embedded.
        assert!(!d.contains("comments:"));
        assert!(d.contains("openflows-harness status set review_ready"));
        assert!(d.contains("Fetch the full review context"));
    }

    #[test]
    fn build_directive_omits_empty_reason() {
        let d = build_directive(PrMonitorState::Comments, 42, None);
        assert!(d.contains("/address_review"));
        assert!(d.contains("state: comments"));
        assert!(d.contains("pr: 42"));
        assert!(!d.contains("reason:"));
    }

    #[test]
    fn build_directive_conflicts_state() {
        let d = build_directive(PrMonitorState::Conflicts, 42, None);
        assert!(d.contains("state: conflicts"));
        // Conflicted files are intentionally NOT embedded — FORGE fetches them.
        assert!(!d.contains("Conflicts:"));
    }
}

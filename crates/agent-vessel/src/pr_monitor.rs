// crates/agent-vessel/src/pr_monitor.rs
//
// PR-state lifecycle monitor — observes GitHub-native PR states
// (conflicts, changes_requested, approved, comments, ci_running,
// ready_for_merge) and computes the rework directive to dispatch to FORGE.

use anyhow::Result;
use chrono::DateTime;
use github::{latest_review_per_user, PrReview, PrReviewState};
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
    /// A separate authorized reviewer has not approved the PR on GitHub.
    /// SENTINEL's internal review does not count as this approval.
    NeedsReview,
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
            PrMonitorState::NeedsReview => "needs_review",
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

/// Evidence of prior `/address_review` dispatch used to determine whether
/// past feedback (reviews or comments) has already been addressed by rework.
#[derive(Debug, Clone, Default)]
pub struct ReworkEvidence {
    /// PR head SHA for which `/address_review` was last dispatched to FORGE.
    pub dispatched_sha: Option<String>,
    /// RFC 3339 timestamp when `/address_review` was last dispatched to FORGE.
    pub dispatched_at: Option<String>,
}

/// Determine whether a review or inline comment has already been addressed by prior rework.
///
/// Returns `true` only if:
/// 1. The feedback was not submitted against the current candidate head commit.
/// 2. Evidence proves rework was dispatched (`dispatched_at` and `dispatched_sha`).
/// 3. The current head differs from `dispatched_sha` (FORGE pushed rework).
/// 4. The feedback was submitted before or at the time rework was dispatched
///    (`feedback_timestamp <= dispatched_at`). If feedback was submitted *after*
///    the dispatch, it represents fresh feedback that the rework commit could not
///    have addressed.
pub(crate) fn is_feedback_addressed(
    feedback_commit_id: Option<&str>,
    feedback_timestamp_str: Option<&str>,
    pr_head_sha: &str,
    evidence: Option<&ReworkEvidence>,
) -> bool {
    // If the feedback is explicitly on the current head commit, it has not been addressed.
    if feedback_commit_id == Some(pr_head_sha) {
        return false;
    }

    let Some(ev) = evidence else {
        return false;
    };

    let (Some(dispatched_sha), Some(dispatched_at_str)) = (&ev.dispatched_sha, &ev.dispatched_at)
    else {
        return false;
    };

    // If the candidate head hasn't changed since the dispatch, FORGE has not pushed rework.
    if pr_head_sha == dispatched_sha {
        return false;
    }

    // Now verify the timestamp: rework occurred AFTER the feedback was submitted.
    if let (Some(fb_ts_str), Ok(dispatched_at)) = (
        feedback_timestamp_str,
        DateTime::parse_from_rfc3339(dispatched_at_str),
    ) {
        if let Ok(fb_ts) = DateTime::parse_from_rfc3339(fb_ts_str) {
            return fb_ts <= dispatched_at;
        }
    }

    // Fallback if timestamps cannot be parsed: if feedback was on dispatched_sha and head has moved,
    // it was likely addressed.
    feedback_commit_id == Some(dispatched_sha)
}

/// The reviews that still apply to the PR's candidate head.
///
/// Takes each reviewer's latest review and drops change requests that prior
/// rework has addressed (see [`is_feedback_addressed`]). Every review-gating
/// decision must aggregate over this list, so an addressed change request from
/// one reviewer cannot hide another reviewer's valid approval (#396).
pub(crate) fn applicable_reviews<'a>(
    reviews: &'a [PrReview],
    head_sha: &str,
    evidence: Option<&ReworkEvidence>,
) -> Vec<&'a PrReview> {
    latest_review_per_user(reviews)
        .into_iter()
        .filter(|r| {
            r.state_enum() != PrReviewState::ChangesRequested
                || !is_feedback_addressed(
                    r.commit_id.as_deref(),
                    r.submitted_at.as_deref(),
                    head_sha,
                    evidence,
                )
        })
        .collect()
}

/// An approval that counts for `head_sha`: from an `OWNER`, `MEMBER` or
/// `COLLABORATOR`, and submitted on exactly that commit. GitHub documents the
/// review `commit_id` as nullable, so an approval without a commit cannot be
/// shown to cover the candidate head and does not count (fail closed).
/// Classification and approval reconciliation share this rule.
pub(crate) fn is_authorized_head_approval(review: &PrReview, head_sha: &str) -> bool {
    review.state_enum() == PrReviewState::Approved
        && review.is_authorized_reviewer()
        && review.commit_id.as_deref() == Some(head_sha)
}

/// Aggregate [`applicable_reviews`] into the review verdict for `head_sha`.
///
/// - Any remaining (unaddressed) change request blocks: `ChangesRequested`.
/// - Otherwise `Approved` requires an authorized reviewer's approval of
///   exactly the candidate head (see [`is_authorized_head_approval`]).
/// - Otherwise `None`: the PR still needs review.
pub(crate) fn candidate_review_state(applicable: &[&PrReview], head_sha: &str) -> PrReviewState {
    if applicable
        .iter()
        .any(|r| r.state_enum() == PrReviewState::ChangesRequested)
    {
        return PrReviewState::ChangesRequested;
    }
    if applicable
        .iter()
        .any(|r| is_authorized_head_approval(r, head_sha))
    {
        PrReviewState::Approved
    } else {
        PrReviewState::None
    }
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
        // Only an explicit GitHub APPROVE review makes the PR merge-ready. A PR
        // with no approve review (SENTINEL has not yet submitted its final
        // review) must NOT fall through to merge — it is gated on NeedsReview
        // until the approve review lands.
        PrReviewState::Approved => PrMonitorState::ReadyForMerge,
        _ => PrMonitorState::NeedsReview,
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
    evidence: Option<&ReworkEvidence>,
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
    // Change requests addressed by rework are dropped before aggregating, so a
    // remaining authorized approval of the current head still counts.
    let applicable = applicable_reviews(&reviews, &pr_info.head_sha, evidence);
    let effective_review = candidate_review_state(&applicable, &pr_info.head_sha);

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

    // Comments are "unaddressed" when there are comments that have not been addressed by prior
    // rework and the PR is not yet approved.
    let has_unaddressed_comments = !comments.is_empty()
        && comments.iter().any(|c| {
            !is_feedback_addressed(
                c.commit_id.as_deref(),
                c.created_at.as_deref(),
                &pr_info.head_sha,
                evidence,
            )
        })
        && effective_review != PrReviewState::Approved;

    debug!(
        pr = pr_info.number,
        mergeable = ?pr_info.mergeable,
        ci = ?ci_status,
        review_state = ?effective_review,
        comments = comments.len(),
        "PR lifecycle classification inputs"
    );

    Ok(classify_from_parts(
        pr_info.mergeable,
        ci_status,
        effective_review,
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
    if owner.is_empty() || repo.is_empty() {
        return ReworkDirective {
            state: PrMonitorState::Comments,
            pr_number,
            reason: None,
        };
    }

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
pub fn build_directive(state: PrMonitorState, pr_number: u64, reason: Option<&str>) -> String {
    let mut out = String::from("/address_review\n");
    out.push_str(&format!("state: {}\n", state.as_str()));
    out.push_str(&format!("pr: {}\n", pr_number));
    if let Some(reason) = reason.filter(|r| !r.trim().is_empty()) {
        out.push_str(&format!("reason: {}\n", reason.trim()));
    }
    out.push_str(
        "\nFetch the full review context (inline comments / conflicted files) \
         for this PR and address them, then re-run: openflows-harness status set testing",
    );
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use pocketflow_core::PrState;

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
    fn classify_no_review_is_needs_review_not_merge_ready() {
        // No SENTINEL approve review on GitHub must NOT merge. It is gated on
        // NeedsReview so the cycle waits for the final review to be submitted.
        assert_eq!(
            classify_from_parts(Some(true), CiStatus::Success, PrReviewState::None, false),
            PrMonitorState::NeedsReview
        );
        // Commented-only review (no approve) with no inline comments is also
        // not merge-ready.
        assert_eq!(
            classify_from_parts(
                Some(true),
                CiStatus::Success,
                PrReviewState::Commented,
                false
            ),
            PrMonitorState::NeedsReview
        );
    }

    #[test]
    fn classify_no_review_with_comments_is_comments() {
        // Inline comments on an unapproved PR still route to rework.
        assert_eq!(
            classify_from_parts(Some(true), CiStatus::Success, PrReviewState::None, true),
            PrMonitorState::Comments
        );
    }

    #[test]
    fn needs_review_is_not_rework() {
        assert!(!PrMonitorState::NeedsReview.needs_rework());
        assert_eq!(PrMonitorState::NeedsReview.as_str(), "needs_review");
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
        assert!(d.contains("openflows-harness status set testing"));
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

    #[tokio::test]
    async fn classify_stale_changes_requested_on_older_commit_is_needs_review() {
        let mut server = mockito::Server::new_async().await;
        let _reviews = server
            .mock("GET", "/repos/org/repo/pulls/42/reviews?per_page=100&page=1")
            .with_status(200)
            .with_body(r#"[{"id":1,"user":{"login":"alice","id":100},"body":"Fix this","state":"CHANGES_REQUESTED","submitted_at":"2026-10-08T12:00:00Z","commit_id":"commit_old","author_association":"MEMBER"}]"#)
            .expect_at_least(1)
            .create_async()
            .await;
        let _comments = server
            .mock(
                "GET",
                "/repos/org/repo/pulls/42/comments?per_page=100&page=1",
            )
            .with_status(200)
            .with_body("[]")
            .expect_at_least(1)
            .create_async()
            .await;

        let client = github::GithubRestClient::with_api_base("test", server.url());
        let pr_info = PrInfo {
            number: 42,
            head_sha: "commit_new".to_string(),
            head_branch: "feature".to_string(),
            base_branch: "main".to_string(),
            title: "Feature".to_string(),
            body: None,
            state: PrState::Open,
            mergeable: Some(true),
            ticket_id: Some("T-42".to_string()),
        };

        let evidence = ReworkEvidence {
            dispatched_sha: Some("commit_old".to_string()),
            dispatched_at: Some("2026-10-08T12:30:00Z".to_string()),
        };

        let state = classify(
            &client,
            "org",
            "repo",
            &pr_info,
            CiStatus::Success,
            Some(&evidence),
        )
        .await
        .unwrap();
        assert_eq!(state, PrMonitorState::NeedsReview);
    }

    fn reviews(json: &str) -> Vec<PrReview> {
        serde_json::from_str(json).unwrap()
    }

    /// Rework for `commit_old` was dispatched at 12:30 and FORGE pushed `commit_new`.
    fn rework_evidence() -> ReworkEvidence {
        ReworkEvidence {
            dispatched_sha: Some("commit_old".to_string()),
            dispatched_at: Some("2026-10-08T12:30:00Z".to_string()),
        }
    }

    fn review_state_for(json: &str) -> PrReviewState {
        let all = reviews(json);
        let evidence = rework_evidence();
        let applicable = applicable_reviews(&all, "commit_new", Some(&evidence));
        candidate_review_state(&applicable, "commit_new")
    }

    #[test]
    fn addressed_change_request_does_not_hide_authorized_head_approval() {
        // #396: alice's change request on commit_old was addressed by rework;
        // bob approved the current head.
        assert_eq!(
            review_state_for(
                r#"[{"state":"CHANGES_REQUESTED","user":{"login":"alice"},"commit_id":"commit_old","submitted_at":"2026-10-08T12:00:00Z","author_association":"MEMBER"},
                    {"state":"APPROVED","user":{"login":"bob"},"commit_id":"commit_new","submitted_at":"2026-10-08T13:00:00Z","author_association":"COLLABORATOR"}]"#
            ),
            PrReviewState::Approved
        );
    }

    #[test]
    fn unaddressed_change_request_still_blocks_head_approval() {
        // carol requested changes on the current head; the rework could not
        // have addressed it, so bob's approval does not make the PR approved.
        assert_eq!(
            review_state_for(
                r#"[{"state":"CHANGES_REQUESTED","user":{"login":"alice"},"commit_id":"commit_old","submitted_at":"2026-10-08T12:00:00Z","author_association":"MEMBER"},
                    {"state":"CHANGES_REQUESTED","user":{"login":"carol"},"commit_id":"commit_new","submitted_at":"2026-10-08T13:30:00Z","author_association":"MEMBER"},
                    {"state":"APPROVED","user":{"login":"bob"},"commit_id":"commit_new","submitted_at":"2026-10-08T13:00:00Z","author_association":"COLLABORATOR"}]"#
            ),
            PrReviewState::ChangesRequested
        );
    }

    #[test]
    fn change_request_submitted_after_dispatch_still_blocks() {
        // alice reviewed commit_old after rework had already been dispatched,
        // so that rework cannot have addressed her request.
        assert_eq!(
            review_state_for(
                r#"[{"state":"CHANGES_REQUESTED","user":{"login":"alice"},"commit_id":"commit_old","submitted_at":"2026-10-08T12:45:00Z","author_association":"MEMBER"},
                    {"state":"APPROVED","user":{"login":"bob"},"commit_id":"commit_new","submitted_at":"2026-10-08T13:00:00Z","author_association":"COLLABORATOR"}]"#
            ),
            PrReviewState::ChangesRequested
        );
    }

    #[test]
    fn addressed_change_request_does_not_admit_ineligible_approvals() {
        // Unauthorized approvals of the head and stale approvals of older
        // commits still do not unblock the PR.
        assert_eq!(
            review_state_for(
                r#"[{"state":"CHANGES_REQUESTED","user":{"login":"alice"},"commit_id":"commit_old","submitted_at":"2026-10-08T12:00:00Z","author_association":"MEMBER"},
                    {"state":"APPROVED","user":{"login":"mallory"},"commit_id":"commit_new","submitted_at":"2026-10-08T13:00:00Z","author_association":"NONE"},
                    {"state":"APPROVED","user":{"login":"carol"},"commit_id":"commit_old","submitted_at":"2026-10-08T12:10:00Z","author_association":"OWNER"}]"#
            ),
            PrReviewState::None
        );
    }

    #[test]
    fn approval_without_commit_does_not_count_for_the_head() {
        // GitHub documents the review commit_id as nullable. An approval that
        // names no commit cannot be shown to cover the candidate head, and
        // reconciliation applies the same rule.
        assert_eq!(
            review_state_for(
                r#"[{"state":"APPROVED","user":{"login":"bob"},"commit_id":null,"submitted_at":"2026-10-08T13:00:00Z","author_association":"COLLABORATOR"}]"#
            ),
            PrReviewState::None
        );
    }

    #[test]
    fn change_request_is_not_addressed_without_rework_evidence() {
        // With no evidence that rework was dispatched, nothing is filtered.
        let all = reviews(
            r#"[{"state":"CHANGES_REQUESTED","user":{"login":"alice"},"commit_id":"commit_old","submitted_at":"2026-10-08T12:00:00Z","author_association":"MEMBER"},
                {"state":"APPROVED","user":{"login":"bob"},"commit_id":"commit_new","submitted_at":"2026-10-08T13:00:00Z","author_association":"COLLABORATOR"}]"#,
        );
        let applicable = applicable_reviews(&all, "commit_new", None);
        assert_eq!(
            candidate_review_state(&applicable, "commit_new"),
            PrReviewState::ChangesRequested
        );
    }

    #[tokio::test]
    async fn classify_addressed_change_request_with_head_approval_is_ready_for_merge() {
        // #396 end to end through classify(): previously NeedsReview.
        let mut server = mockito::Server::new_async().await;
        let _reviews = server
            .mock("GET", "/repos/org/repo/pulls/42/reviews?per_page=100&page=1")
            .with_status(200)
            .with_body(r#"[{"id":1,"user":{"login":"alice","id":100},"body":"Fix this","state":"CHANGES_REQUESTED","submitted_at":"2026-10-08T12:00:00Z","commit_id":"commit_old","author_association":"MEMBER"},
                           {"id":2,"user":{"login":"bob","id":101},"body":"LGTM","state":"APPROVED","submitted_at":"2026-10-08T13:00:00Z","commit_id":"commit_new","author_association":"COLLABORATOR"}]"#)
            .expect_at_least(1)
            .create_async()
            .await;
        let _comments = server
            .mock(
                "GET",
                "/repos/org/repo/pulls/42/comments?per_page=100&page=1",
            )
            .with_status(200)
            .with_body("[]")
            .expect_at_least(1)
            .create_async()
            .await;

        let client = github::GithubRestClient::with_api_base("test", server.url());
        let pr_info = PrInfo {
            number: 42,
            head_sha: "commit_new".to_string(),
            head_branch: "feature".to_string(),
            base_branch: "main".to_string(),
            title: "Feature".to_string(),
            body: None,
            state: PrState::Open,
            mergeable: Some(true),
            ticket_id: Some("T-42".to_string()),
        };
        let evidence = rework_evidence();

        let state = classify(
            &client,
            "org",
            "repo",
            &pr_info,
            CiStatus::Success,
            Some(&evidence),
        )
        .await
        .unwrap();
        assert_eq!(state, PrMonitorState::ReadyForMerge);
    }

    #[tokio::test]
    async fn classify_changes_requested_race_condition_triggers_rework() {
        let mut server = mockito::Server::new_async().await;
        // Review submitted against commit_old AFTER rework was already dispatched at 12:30
        let _reviews = server
            .mock("GET", "/repos/org/repo/pulls/42/reviews?per_page=100&page=1")
            .with_status(200)
            .with_body(r#"[{"id":1,"user":{"login":"alice","id":100},"body":"Fix this race","state":"CHANGES_REQUESTED","submitted_at":"2026-10-08T12:45:00Z","commit_id":"commit_old","author_association":"MEMBER"}]"#)
            .expect_at_least(1)
            .create_async()
            .await;
        let _comments = server
            .mock(
                "GET",
                "/repos/org/repo/pulls/42/comments?per_page=100&page=1",
            )
            .with_status(200)
            .with_body("[]")
            .expect_at_least(1)
            .create_async()
            .await;

        let client = github::GithubRestClient::with_api_base("test", server.url());
        let pr_info = PrInfo {
            number: 42,
            head_sha: "commit_new".to_string(),
            head_branch: "feature".to_string(),
            base_branch: "main".to_string(),
            title: "Feature".to_string(),
            body: None,
            state: PrState::Open,
            mergeable: Some(true),
            ticket_id: Some("T-42".to_string()),
        };

        let evidence = ReworkEvidence {
            dispatched_sha: Some("commit_old".to_string()),
            dispatched_at: Some("2026-10-08T12:30:00Z".to_string()),
        };

        let state = classify(
            &client,
            "org",
            "repo",
            &pr_info,
            CiStatus::Success,
            Some(&evidence),
        )
        .await
        .unwrap();
        assert_eq!(state, PrMonitorState::ChangesRequested);
    }
}

//! Approval reconciliation after rework addresses change requests (#396).

use super::*;

pub(super) async fn run_reconcile_with_evidence(
    node: &VesselNode,
    store: &SharedStore,
    evidence: Option<&ReworkEvidence>,
) -> bool {
    let state = store.lifecycle("T-42").await.unwrap();
    node.reconcile_github_approval(store, "T-42", &state, "org", "repo", 42, "head", evidence)
        .await
        .unwrap()
}

/// FORGE was sent `/address_review` for `old-head` at 12:00 on 2026-10-04
/// and has since pushed `head`.
fn rework_evidence() -> ReworkEvidence {
    ReworkEvidence {
        dispatched_sha: Some("old-head".to_string()),
        dispatched_at: Some("2026-10-04T12:00:00Z".to_string()),
    }
}

#[tokio::test]
async fn reconcile_github_approval_ignores_change_request_addressed_by_rework() {
    // #396: alice requested changes on old-head before rework was dispatched;
    // FORGE pushed head, and bob (authorized) approved head. Alice's addressed
    // change request must not hide bob's valid approval.
    let (_server, node, store) = reconcile_fixture(
        200,
        r#"[{"state":"CHANGES_REQUESTED","user":{"login":"alice"},"author_association":"MEMBER","commit_id":"old-head","submitted_at":"2026-10-04T10:00:00Z"},
            {"state":"APPROVED","user":{"login":"bob"},"author_association":"COLLABORATOR","commit_id":"head","submitted_at":"2026-10-05T00:00:00Z"}]"#,
    )
    .await;
    let evidence = rework_evidence();
    assert!(run_reconcile_with_evidence(&node, &store, Some(&evidence)).await);
    let pr_human = store.lifecycle("T-42").await.unwrap().pr_human.unwrap();
    assert!(pr_human.approved);
    assert!(pr_human.report.contains("bob"));
}

#[tokio::test]
async fn reconcile_github_approval_still_vetoed_by_unaddressed_change_request() {
    // alice's change request arrived after rework was dispatched, so the
    // rework could not have addressed it: it still blocks bob's approval.
    let (_server, node, store) = reconcile_fixture(
        200,
        r#"[{"state":"CHANGES_REQUESTED","user":{"login":"alice"},"author_association":"MEMBER","commit_id":"old-head","submitted_at":"2026-10-04T13:00:00Z"},
            {"state":"APPROVED","user":{"login":"bob"},"author_association":"COLLABORATOR","commit_id":"head","submitted_at":"2026-10-05T00:00:00Z"}]"#,
    )
    .await;
    let evidence = rework_evidence();
    assert!(!run_reconcile_with_evidence(&node, &store, Some(&evidence)).await);
    assert!(store.lifecycle("T-42").await.unwrap().pr_human.is_none());
}

#[tokio::test]
async fn reconcile_github_approval_addressed_change_request_does_not_admit_ineligible_approval() {
    // Dropping alice's addressed change request must not relax approval
    // eligibility: an outside account's approval and a stale approval of
    // old-head still do not reconcile.
    let (_server, node, store) = reconcile_fixture(
        200,
        r#"[{"state":"CHANGES_REQUESTED","user":{"login":"alice"},"author_association":"MEMBER","commit_id":"old-head","submitted_at":"2026-10-04T10:00:00Z"},
            {"state":"APPROVED","user":{"login":"mallory"},"author_association":"NONE","commit_id":"head","submitted_at":"2026-10-05T00:00:00Z"},
            {"state":"APPROVED","user":{"login":"carol"},"author_association":"OWNER","commit_id":"old-head","submitted_at":"2026-10-04T11:00:00Z"}]"#,
    )
    .await;
    let evidence = rework_evidence();
    assert!(!run_reconcile_with_evidence(&node, &store, Some(&evidence)).await);
    assert!(store.lifecycle("T-42").await.unwrap().pr_human.is_none());
}

#[tokio::test]
async fn reconcile_keeps_rework_evidence_while_an_addressed_change_request_remains() {
    // alice's addressed change request is still on GitHub. Later passes (for
    // example while CI is still running) must still see it as addressed, so
    // reconciling bob's approval keeps the dispatch evidence.
    let (_server, node, store) = reconcile_fixture(
        200,
        r#"[{"state":"CHANGES_REQUESTED","user":{"login":"alice"},"author_association":"MEMBER","commit_id":"old-head","submitted_at":"2026-10-04T10:00:00Z"},
            {"state":"APPROVED","user":{"login":"bob"},"author_association":"COLLABORATOR","commit_id":"head","submitted_at":"2026-10-05T00:00:00Z"}]"#,
    )
    .await;
    store
        .set(&address_review_dispatched_key(42), json!("old-head"))
        .await;
    store
        .set(
            &address_review_dispatched_at_key(42),
            json!("2026-10-04T12:00:00Z"),
        )
        .await;
    let evidence = rework_evidence();
    assert!(run_reconcile_with_evidence(&node, &store, Some(&evidence)).await);

    let kept = VesselNode::get_rework_evidence(&store, 42)
        .await
        .expect("rework evidence must survive reconciliation");
    assert_eq!(kept.dispatched_sha.as_deref(), Some("old-head"));
    assert_eq!(kept.dispatched_at.as_deref(), Some("2026-10-04T12:00:00Z"));
}

//! LORE docs PR tests: opening the PR only for a sealed, current approval,
//! following it on GitHub, cutover and role rules.

use super::*;

async fn approved_docs_store(secret: &str) -> SharedStore {
    let store = lore_store(lore_slot(WorkerStatus::Idle, None)).await;
    submit_docs(&store, "T-9", "abc").await;
    decide_docs_sealed(&store, "T-9", true, "", secret).await;
    store
}

#[tokio::test]
async fn nexus_opens_the_docs_pr_for_a_sealed_approval() {
    let mut server = mockito::Server::new_async().await;
    let _head = server
        .mock("GET", "/repos/org/repo/git/ref/heads/lore/T-9")
        .with_status(200)
        .with_body(r#"{"object":{"sha":"abc"}}"#)
        .create_async()
        .await;
    let _none = server
        .mock("GET", "/repos/org/repo/pulls")
        .match_query(mockito::Matcher::Any)
        .with_status(200)
        .with_body("[]")
        .create_async()
        .await;
    let _repo = server
        .mock("GET", "/repos/org/repo")
        .with_status(200)
        .with_body(r#"{"default_branch":"main"}"#)
        .create_async()
        .await;
    let _compare = mock_compare(&mut server, &["CHANGELOG.md"]).await;
    let create = server
        .mock("POST", "/repos/org/repo/pulls")
        .match_body(mockito::Matcher::Regex(
            r#""base":"main".*"head":"lore/T-9"|"head":"lore/T-9".*"base":"main""#.into(),
        ))
        .with_status(201)
        .with_body(r#"{"number":77,"title":"docs","body":null,"head":{"sha":"abc","ref":"lore/T-9"},"base":{"ref":"main","sha":"b"},"state":"open"}"#)
        .expect(1)
        .create_async()
        .await;
    let store = approved_docs_store(DOCS_SECRET).await;
    let node = lore_node().with_github_override("t", server.url());
    poll(&node, &store, &[merged_ticket("T-9")], 1).await;
    create.assert_async().await;
    let record = store.docs_record("T-9").await.unwrap();
    assert_eq!(record.phase, config::docs_lifecycle::DocsPhase::Published);
    assert_eq!(record.pr_number, Some(77));
}

/// GitHub's diff of the approved head `abc` against `main` touches `files`.
async fn mock_compare(server: &mut mockito::Server, files: &[&str]) -> mockito::Mock {
    let files: Vec<String> = files
        .iter()
        .map(|f| format!(r#"{{"filename":"{f}","status":"modified"}}"#))
        .collect();
    server
        .mock("GET", "/repos/org/repo/compare/main...abc?per_page=1")
        .with_status(200)
        .with_body(format!(r#"{{"files":[{}]}}"#, files.join(",")))
        .create_async()
        .await
}

async fn mock_docs_branch(server: &mut mockito::Server) -> Vec<mockito::Mock> {
    vec![
        server
            .mock("GET", "/repos/org/repo/git/ref/heads/lore/T-9")
            .with_status(200)
            .with_body(r#"{"object":{"sha":"abc"}}"#)
            .create_async()
            .await,
        server
            .mock("GET", "/repos/org/repo")
            .with_status(200)
            .with_body(r#"{"default_branch":"main"}"#)
            .create_async()
            .await,
    ]
}

#[tokio::test]
async fn approved_heads_that_change_code_on_github_are_never_published() {
    use config::docs_lifecycle::DocsPhase;
    let mut server = mockito::Server::new_async().await;
    let _branch = mock_docs_branch(&mut server).await;
    // LORE reported only CHANGELOG.md; GitHub shows what the PR would merge.
    let _compare = mock_compare(&mut server, &["CHANGELOG.md", "src/lib.rs"]).await;
    let create = server
        .mock("POST", "/repos/org/repo/pulls")
        .expect(0)
        .create_async()
        .await;
    let store = approved_docs_store(DOCS_SECRET).await;
    let node = lore_node().with_github_override("t", server.url());
    poll(&node, &store, &[merged_ticket("T-9")], 1).await;
    let record = store.docs_record("T-9").await.unwrap();
    assert_eq!(record.phase, DocsPhase::Drafting);
    assert!(record.feedback.unwrap().contains("src/lib.rs"));
    create.assert_async().await;
}

#[tokio::test]
async fn docs_prs_into_another_base_are_never_adopted() {
    let mut server = mockito::Server::new_async().await;
    let _branch = mock_docs_branch(&mut server).await;
    let _compare = mock_compare(&mut server, &["CHANGELOG.md"]).await;
    // A PR LORE opened itself from the docs branch into a release branch.
    let _foreign = server
        .mock("GET", "/repos/org/repo/pulls")
        .match_query(mockito::Matcher::Any)
        .with_status(200)
        .with_body(r#"[{"number":5,"title":"docs","body":null,"head":{"sha":"abc","ref":"lore/T-9"},"base":{"ref":"release/1","sha":"b"},"state":"open"}]"#)
        .create_async()
        .await;
    let create = server
        .mock("POST", "/repos/org/repo/pulls")
        .with_status(201)
        .with_body(r#"{"number":77,"title":"docs","body":null,"head":{"sha":"abc","ref":"lore/T-9"},"base":{"ref":"main","sha":"b"},"state":"open"}"#)
        .expect(1)
        .create_async()
        .await;
    let store = approved_docs_store(DOCS_SECRET).await;
    let node = lore_node().with_github_override("t", server.url());
    poll(&node, &store, &[merged_ticket("T-9")], 1).await;
    create.assert_async().await;
    assert_eq!(store.docs_record("T-9").await.unwrap().pr_number, Some(77));
}

#[tokio::test]
async fn unverifiable_approvals_alert_the_operator_once_and_can_be_resealed() {
    use config::docs_lifecycle::{DocsEvent, DocsPhase};
    let store = approved_docs_store("rotated-out-secret").await;
    let node = lore_node();
    poll(&node, &store, &[merged_ticket("T-9")], 2).await;
    let record = store.docs_record("T-9").await.unwrap();
    assert_eq!(record.phase, DocsPhase::Approved);
    assert_eq!(
        delivery(&store, "T-9").await.seal_alert,
        Some(record.version)
    );

    // The operator approves again with the controller's current key.
    let head = record.head.clone().unwrap();
    docs_event(
        &store,
        "T-9",
        "human",
        DocsEvent::Decide {
            revision: record.revision,
            seal: config::docs_lifecycle::approval_seal(DOCS_SECRET, "T-9", record.revision, &head),
            head,
            approved: true,
            report: String::new(),
        },
    )
    .await;
    let resealed = store.docs_record("T-9").await.unwrap();
    assert_eq!(resealed.phase, DocsPhase::Approved);
    assert!(config::docs_lifecycle::verify_approval(&resealed, "T-9", DOCS_SECRET).is_ok());
}

#[tokio::test]
async fn forged_approvals_and_moved_branches_never_publish() {
    let mut server = mockito::Server::new_async().await;
    let create = server
        .mock("POST", "/repos/org/repo/pulls")
        .expect(0)
        .create_async()
        .await;
    let _moved = server
        .mock("GET", "/repos/org/repo/git/ref/heads/lore/T-9")
        .with_status(200)
        .with_body(r#"{"object":{"sha":"pushed-later"}}"#)
        .create_async()
        .await;

    // Sealed with a key the controller does not hold: nothing happens.
    let forged = approved_docs_store("workspace-guess").await;
    let node = lore_node().with_github_override("t", server.url());
    poll(&node, &forged, &[merged_ticket("T-9")], 1).await;
    assert_eq!(
        forged.docs_record("T-9").await.unwrap().phase,
        config::docs_lifecycle::DocsPhase::Approved
    );

    // Genuine approval, but the branch moved: back to LORE for a new review.
    let moved = approved_docs_store(DOCS_SECRET).await;
    poll(&node, &moved, &[merged_ticket("T-9")], 1).await;
    let record = moved.docs_record("T-9").await.unwrap();
    assert_eq!(record.phase, config::docs_lifecycle::DocsPhase::Drafting);
    assert!(record.feedback.unwrap().contains("pushed-later"));
    create.assert_async().await;
}

#[tokio::test]
async fn published_docs_follow_the_pr_on_github() {
    use config::docs_lifecycle::{DocsEvent, DocsPhase};
    let mut server = mockito::Server::new_async().await;
    let _merged = server
        .mock("GET", "/repos/org/repo/pulls/77")
        .with_status(200)
        .with_body(r#"{"number":77,"title":"docs","body":null,"head":{"sha":"abc","ref":"lore/T-9"},"base":{"ref":"main","sha":"b"},"state":"closed","merged":true,"merge_commit_sha":"m1"}"#)
        .create_async()
        .await;
    let _closed = server
        .mock("GET", "/repos/org/repo/pulls/78")
        .with_status(200)
        .with_body(r#"{"number":78,"title":"docs","body":null,"head":{"sha":"abc","ref":"lore/T-10"},"base":{"ref":"main","sha":"b"},"state":"closed","merged":false}"#)
        .create_async()
        .await;
    let store = lore_store(lore_slot(WorkerStatus::Idle, None)).await;
    for (ticket, number) in [("T-9", 77), ("T-10", 78)] {
        submit_docs(&store, ticket, "abc").await;
        decide_docs(&store, ticket, true, "").await;
        docs_event(&store, ticket, "nexus", DocsEvent::Published { number }).await;
    }
    // A pause stops LORE and automatic merges, not the record following
    // a human's merge on GitHub.
    docs_event(
        &store,
        "T-9",
        "human",
        DocsEvent::Pause {
            reason: "release freeze".into(),
        },
    )
    .await;
    let node = lore_node().with_github_override("t", server.url());
    poll(
        &node,
        &store,
        &[merged_ticket("T-9"), merged_ticket("T-10")],
        1,
    )
    .await;
    let merged = store.docs_record("T-9").await.unwrap();
    assert_eq!(merged.phase, DocsPhase::Merged);
    assert_eq!(merged.merge_sha.as_deref(), Some("m1"));
    assert_eq!(
        store.docs_record("T-10").await.unwrap().phase,
        DocsPhase::Abandoned
    );
}

#[tokio::test]
async fn closed_docs_are_not_reread_until_reopened() {
    let store = lore_store(lore_slot(WorkerStatus::Idle, None)).await;
    docs_event(
        &store,
        "T-9",
        "human",
        config::docs_lifecycle::DocsEvent::Skip {
            reason: "trivial".into(),
        },
    )
    .await;
    let node = lore_node();
    let ticket = [merged_ticket("T-9")];
    poll(&node, &store, &ticket, 1).await;
    // Cached as closed: delivery state planted now is not cleaned up.
    set_delivery(
        &store,
        "T-9",
        lore::LoreDelivery {
            step: "x".into(),
            ..Default::default()
        },
    )
    .await;
    poll(&node, &store, &ticket, 1).await;
    assert_eq!(delivery(&store, "T-9").await.step, "x");
    // A reopen marker invalidates the cache and the record is read again.
    store
        .set(config::state::KEY_DOCS_REOPENED, json!({"ticket": "T-0"}))
        .await;
    poll(&node, &store, &ticket, 1).await;
    assert_eq!(delivery(&store, "T-9").await, lore::LoreDelivery::default());
}

#[tokio::test]
async fn disabled_lore_runs_neither_cutover_nor_driver() {
    let store = SharedStore::new_in_memory();
    store.set(KEY_WORKER_SLOTS, json!({})).await;
    poll(&lore_node(), &store, &[merged_ticket("T-9")], 1).await;
    assert!(store.get(config::state::KEY_DOCS_CUTOVER).await.is_none());
    assert_eq!(store.docs_record("T-9").await.unwrap().version, 0);
}

#[tokio::test]
async fn docs_cutover_skips_tickets_merged_before_the_gate_once() {
    let store = SharedStore::new_in_memory();
    store
        .set(&full_ticket_key_flat("T-2", "lore_done"), json!(true))
        .await;
    submit_docs(&store, "T-3", "abc").await;
    let node = lore_node();
    let before = [
        merged_ticket("T-1"),
        merged_ticket("T-2"),
        merged_ticket("T-3"),
    ];
    node.docs_cutover(&store, &before).await;
    use config::docs_lifecycle::DocsPhase;
    assert_eq!(
        store.docs_record("T-1").await.unwrap().phase,
        DocsPhase::Skipped
    );
    let legacy = store.docs_record("T-2").await.unwrap();
    assert_eq!(legacy.phase, DocsPhase::Skipped);
    assert!(legacy.skip_reason.unwrap().contains("legacy"));
    assert_eq!(
        store.docs_record("T-3").await.unwrap().phase,
        DocsPhase::DocsReady,
        "docs already in flight are kept"
    );
    assert!(store.get(config::state::KEY_DOCS_CUTOVER).await.is_some());

    // Tickets merged after the cutover are documented.
    node.docs_cutover(&store, &[merged_ticket("T-4")]).await;
    assert_eq!(store.docs_record("T-4").await.unwrap().version, 0);
}

#[tokio::test]
async fn terminal_ticket_is_never_escalated_to_awaiting_human() {
    for (worker, slot_status) in [("lore", bound_to("T-9")), ("forge-1", bound_to("T-9"))] {
        let store = SharedStore::new_in_memory();
        store.set(KEY_TICKETS, json!([merged_ticket("T-9")])).await;
        store
            .set(
                KEY_WORKER_SLOTS,
                json!({ worker: slot(worker, slot_status) }),
            )
            .await;
        let node = lore_node();
        node.mark_ticket_awaiting_human(&store, "T-9", worker, "workspace crashed 3 times")
            .await;
        let tickets: Vec<Ticket> = store.get_typed(KEY_TICKETS).await.unwrap();
        assert!(
            matches!(tickets[0].status, TicketStatus::Merged { .. }),
            "{worker}: a merged ticket stays merged"
        );
        let slots: HashMap<String, WorkerSlot> = store.get_typed(KEY_WORKER_SLOTS).await.unwrap();
        assert!(
            matches!(slots[worker].status, WorkerStatus::Idle),
            "{worker}: the worker is released"
        );
    }
}

#[tokio::test]
async fn recovery_keeps_lore_slot_only_while_lore_is_drafting() {
    let store = SharedStore::new_in_memory();
    store.set(KEY_TICKETS, json!([merged_ticket("T-9")])).await;
    store
        .set(
            KEY_WORKER_SLOTS,
            json!({ "lore": lore_slot(bound_to("T-9"), None) }),
        )
        .await;
    let node = lore_node();

    node.recover_orphans(&store).await.unwrap();
    let slots = lore_slots(&store).await;
    assert!(
        matches!(lore_slot_status(&slots), WorkerStatus::Assigned { .. }),
        "a merged ticket is LORE's normal binding while it drafts"
    );
    let recovery = NexusNode::reconcile(&[merged_ticket("T-9")], &slots, &[]);
    assert!(recovery.stale_workers.is_empty());

    submit_docs(&store, "T-9", "abc").await;
    node.recover_orphans(&store).await.unwrap();
    let slots = lore_slots(&store).await;
    assert!(matches!(lore_slot_status(&slots), WorkerStatus::Idle));
}

#[tokio::test]
async fn create_chat_for_assignment_never_hijacks_lore_slot() {
    let mut server = mockito::Server::new_async().await;
    let create = server
        .mock("POST", "/api/v2/chats")
        .expect(0)
        .create_async()
        .await;
    let store = local_lore_store(&server, lore_slot(bound_to("T-9"), Some("ws"))).await;
    let node = lore_node();
    node.create_chat_for_assignment(&store, "lore", &merged_ticket("T-9"))
        .await;
    create.assert_async().await;
    assert!(store
        .get(&full_ticket_key("T-9", KEY_TICKET_DISPATCH, "lore"))
        .await
        .is_none());
}

#[test]
fn role_traits_hold_the_per_role_rules() {
    let lore = RoleTraits::of("lore-1");
    assert!(!lore.uses_a2a && lore.starts_from_default_branch && lore.scheduler_owns_chats);
    assert!(lore.serves_terminal_tickets && !lore.crashes_count_against_ticket);
    let sentinel = RoleTraits::of("sentinel-2");
    assert!(sentinel.scheduler_owns_chats && sentinel.uses_a2a);
    assert!(!sentinel.serves_terminal_tickets && sentinel.crashes_count_against_ticket);
    assert_eq!(RoleTraits::of("forge-1"), RoleTraits::BUILDER);
}

#[tokio::test]
async fn docs_prs_reach_vessel_only_when_mergeable_or_unowned() {
    use config::docs_lifecycle::DocsEvent;
    let store = SharedStore::new_in_memory();
    submit_docs(&store, "T-9", "abc").await;
    let pr = |number: u64, ticket: &str, branch: &str| json!({"number": number, "ticket_id": ticket, "head_branch": branch});
    let prs = [pr(42, "T-9", "lore/T-9"), pr(43, "T-5", "lore/T-5")];
    let ready = NexusNode::pending_prs_ready_for_vessel(&store, &prs).await;
    assert_eq!(
        ready
            .iter()
            .map(|p| p["number"].as_u64().unwrap())
            .collect::<Vec<_>>(),
        vec![43],
        "awaiting the operator is held; an unowned docs PR goes to VESSEL to hand to a human"
    );
    decide_docs(&store, "T-9", true, "").await;
    docs_event(&store, "T-9", "nexus", DocsEvent::Published { number: 42 }).await;
    let ready = NexusNode::pending_prs_ready_for_vessel(&store, &prs[..1]).await;
    assert_eq!(ready.len(), 1, "published docs PRs go to VESSEL");
}

#[tokio::test]
async fn unmanaged_docs_prs_are_not_rediscovered() {
    let store = SharedStore::new_in_memory();
    store.set("pr:42:unmanaged", json!({"reason": "x"})).await;
    assert!(!NexusNode::should_discover_pr(&store, 42, Some("T-9"), "lore/T-9").await);
    assert!(NexusNode::should_discover_pr(&store, 42, Some("T-9"), "forge-1/T-9").await);
}

//! LORE docs driver tests: helpers, drafting, parking and hosted delivery.
//! Docs PR publication and approval tests live in [`publish`].

use super::*;

mod publish;

const DOCS_SECRET: &str = "test-secret";

fn merged_ticket(id: &str) -> Ticket {
    Ticket {
        id: id.to_string(),
        title: "Add retries".to_string(),
        body: "Retry transient failures".to_string(),
        priority: 1,
        branch: Some(format!("forge-1/{id}")),
        status: TicketStatus::Merged {
            pr_number: 7,
            worker_id: "forge-1".to_string(),
        },
        issue_url: None,
        attempts: 0,
    }
}

fn lore_slot(status: WorkerStatus, workspace: Option<&str>) -> WorkerSlot {
    let mut slot = slot("lore", status);
    slot.workspace_id = workspace.map(str::to_string);
    slot
}

fn bound_to(ticket: &str) -> WorkerStatus {
    WorkerStatus::Assigned {
        ticket_id: ticket.into(),
        issue_url: None,
    }
}

fn lore_node() -> NexusNode {
    NexusNode::new("missing-persona", "missing-registry")
        .with_docs_gate_secret(Some(DOCS_SECRET.into()))
}

fn hosted_lore_node(server: &mockito::Server) -> NexusNode {
    lore_node().with_runtime_control(runtime_control::RuntimeControlClient::for_test(
        server.url(),
    ))
}

/// A store past the docs cutover, so merged tickets are documentable.
async fn lore_store(slot: WorkerSlot) -> SharedStore {
    let store = SharedStore::new_in_memory();
    store
        .set(config::state::KEY_DOCS_CUTOVER, json!({ "ts": 1 }))
        .await;
    store.set(KEY_WORKER_SLOTS, json!({ "lore": slot })).await;
    store.set("repository", json!("org/repo")).await;
    store
}

async fn local_lore_store(server: &mockito::Server, slot: WorkerSlot) -> SharedStore {
    let store = lore_store(slot).await;
    store.set("coder_url", json!(server.url())).await;
    store.set("coder_session_token", json!("test")).await;
    store
}

async fn mock_ready_workspace(server: &mut mockito::Server) -> mockito::Mock {
    server
        .mock("GET", "/api/v2/workspaces/ws")
        .with_status(200)
        .with_body(
            r#"{"id":"ws","status":"running","latest_build":{"status":"running","resources":[{"agents":[{"name":"main","status":"connected","lifecycle_state":"ready"}]}]}}"#,
        )
        .create_async()
        .await
}

async fn mock_lore_chat(server: &mut mockito::Server, status: &str) -> mockito::Mock {
    server
        .mock("GET", "/api/v2/chats/lore-chat")
        .with_status(200)
        .with_body(format!(
            r#"{{"id":"lore-chat","workspace_id":"ws","status":"{status}"}}"#
        ))
        .create_async()
        .await
}

async fn mock_no_chats(server: &mut mockito::Server) -> mockito::Mock {
    server
        .mock("GET", "/api/v2/chats")
        .with_status(200)
        .with_body(r#"{"chats":[]}"#)
        .create_async()
        .await
}

async fn mock_org(server: &mut mockito::Server) -> mockito::Mock {
    server
        .mock("GET", "/api/v2/organizations")
        .with_status(200)
        .with_body(r#"[{"id":"org","name":"default","is_default":true}]"#)
        .create_async()
        .await
}

async fn set_delivery(store: &SharedStore, ticket: &str, delivery: lore::LoreDelivery) {
    store
        .set(
            &full_ticket_key_flat(ticket, config::state::KEY_TICKET_DOCS_DELIVERY),
            json!(delivery),
        )
        .await;
}

async fn delivery(store: &SharedStore, ticket: &str) -> lore::LoreDelivery {
    store
        .get_typed(&full_ticket_key_flat(
            ticket,
            config::state::KEY_TICKET_DOCS_DELIVERY,
        ))
        .await
        .unwrap_or_default()
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

async fn docs_event(
    store: &SharedStore,
    ticket: &str,
    actor: &str,
    event: config::docs_lifecycle::DocsEvent,
) {
    let record = store.docs_record(ticket).await.unwrap();
    store
        .docs_transition(ticket, record.version, actor, event)
        .await
        .unwrap();
}

async fn submit_docs(store: &SharedStore, ticket: &str, head: &str) {
    let record = store.docs_record(ticket).await.unwrap();
    docs_event(
        store,
        ticket,
        "lore",
        config::docs_lifecycle::DocsEvent::Submit {
            branch: record
                .branch
                .clone()
                .unwrap_or_else(|| record.expected_branch(ticket)),
            head: head.into(),
            base: "main".into(),
            report: "CHANGELOG entry".into(),
            files: vec!["CHANGELOG.md".into()],
        },
    )
    .await;
}

async fn decide_docs_sealed(
    store: &SharedStore,
    ticket: &str,
    approved: bool,
    notes: &str,
    secret: &str,
) {
    let record = store.docs_record(ticket).await.unwrap();
    let head = record.head.clone().unwrap();
    docs_event(
        store,
        ticket,
        "human",
        config::docs_lifecycle::DocsEvent::Decide {
            revision: record.revision,
            seal: if approved {
                config::docs_lifecycle::approval_seal(secret, ticket, record.revision, &head)
            } else {
                String::new()
            },
            head,
            approved,
            report: notes.into(),
        },
    )
    .await;
    // As the operator CLI does, record the decision controller-side.
    let record = store.docs_record(ticket).await.unwrap();
    store
        .set_docs_control(
            ticket,
            &config::docs_lifecycle::DocsControl::from_record(&record),
        )
        .await
        .unwrap();
}

async fn decide_docs(store: &SharedStore, ticket: &str, approved: bool, notes: &str) {
    decide_docs_sealed(store, ticket, approved, notes, DOCS_SECRET).await;
}

async fn poll(node: &NexusNode, store: &SharedStore, tickets: &[Ticket], times: usize) {
    for _ in 0..times {
        Box::pin(node.poll_harness_status_and_spawn_agents(store, tickets)).await;
    }
}

async fn lore_slots(store: &SharedStore) -> HashMap<String, WorkerSlot> {
    store.get_typed(KEY_WORKER_SLOTS).await.unwrap()
}

fn lore_slot_status(slots: &HashMap<String, WorkerSlot>) -> &WorkerStatus {
    &slots["lore"].status
}

fn managed_status(workspace: &str, observed: &str) -> String {
    let heartbeat = if observed == "running" {
        r#","last_heartbeat_at":"2026-11-05T00:00:00Z""#
    } else {
        ""
    };
    format!(
        r#"{{"workspace_id":"{workspace}","role":"lore","slot":null,"desired_state":"running","observed_state":"{observed}"{heartbeat}}}"#
    )
}

/// Manager mocks: every workspace request yields `ws-new`, which is ready.
async fn mock_managed_provisioning(server: &mut mockito::Server) -> Vec<mockito::Mock> {
    vec![
        server
            .mock("POST", "/api/v1/runtime/workspaces")
            .with_status(202)
            .with_body(r#"{"workspace_id":"ws-new","operation_id":"op","status":"queued"}"#)
            .create_async()
            .await,
        server
            .mock("GET", "/api/v1/runtime/workspaces/ws-new")
            .with_status(200)
            .with_body(managed_status("ws-new", "running"))
            .create_async()
            .await,
    ]
}

#[tokio::test]
async fn merged_ticket_spawns_one_prompted_lore_chat() {
    let mut server = mockito::Server::new_async().await;
    let _org = mock_org(&mut server).await;
    let _ws = mock_ready_workspace(&mut server).await;
    let _list = mock_no_chats(&mut server).await;
    let create = server
        .mock("POST", "/api/v2/chats")
        .match_body(mockito::Matcher::Regex(
            r#"lore/T-9.*docs submit --report /tmp/docs-report.md.*Never open a PR yourself.*"role":"lore""#.into(),
        ))
        .with_status(201)
        .with_body(r#"{"id":"lore-chat","workspace_id":"ws"}"#)
        .expect(1)
        .create_async()
        .await;
    let _existing = mock_lore_chat(&mut server, "running").await;
    let store = local_lore_store(&server, lore_slot(bound_to("T-9"), Some("ws"))).await;
    let node = lore_node();
    poll(&node, &store, &[merged_ticket("T-9")], 2).await;
    create.assert_async().await;
    assert_eq!(
        store
            .get_typed::<String>(&full_ticket_key("T-9", KEY_TICKET_CHAT, "lore"))
            .await
            .as_deref(),
        Some("lore-chat")
    );
    let delivered = delivery(&store, "T-9").await;
    assert_eq!(delivered.step, "start:e0");
    assert_eq!(delivered.epoch, 0);
    assert!(delivered.started > 0);
    let dispatch: Value = store
        .get(&full_ticket_key("T-9", KEY_TICKET_DISPATCH, "lore"))
        .await
        .unwrap();
    assert_eq!(dispatch["branch"], "lore/T-9");
}

#[tokio::test]
async fn failed_lore_chat_creation_records_nothing_and_retries() {
    let mut server = mockito::Server::new_async().await;
    let _org = mock_org(&mut server).await;
    let _ws = mock_ready_workspace(&mut server).await;
    let _list = mock_no_chats(&mut server).await;
    let create = server
        .mock("POST", "/api/v2/chats")
        .with_status(500)
        .expect(2)
        .create_async()
        .await;
    let store = local_lore_store(&server, lore_slot(bound_to("T-9"), Some("ws"))).await;
    let node = lore_node();
    poll(&node, &store, &[merged_ticket("T-9")], 2).await;
    create.assert_async().await;
    assert!(store
        .get(&full_ticket_key("T-9", KEY_TICKET_CHAT, "lore"))
        .await
        .is_none());
    // Nothing was delivered; only the start of the failure streak is kept.
    let failed = delivery(&store, "T-9").await;
    assert!(failed.failing_since.is_some());
    assert_eq!(
        lore::LoreDelivery {
            failing_since: None,
            ..failed
        },
        lore::LoreDelivery::default()
    );
}

#[tokio::test]
async fn lore_that_never_comes_up_is_parked() {
    let mut server = mockito::Server::new_async().await;
    let _org = mock_org(&mut server).await;
    let _ws = mock_ready_workspace(&mut server).await;
    let _list = mock_no_chats(&mut server).await;
    let _create = server
        .mock("POST", "/api/v2/chats")
        .with_status(500)
        .create_async()
        .await;
    let store = local_lore_store(&server, lore_slot(bound_to("T-9"), Some("ws"))).await;
    let node = lore_node();
    poll(&node, &store, &[merged_ticket("T-9")], 1).await;
    assert!(!store.docs_record("T-9").await.unwrap().paused);

    let mut failing = delivery(&store, "T-9").await;
    failing.failing_since = Some(now() - lore::LORE_PROVISION_MAX_SECS - 1);
    set_delivery(&store, "T-9", failing).await;
    poll(&node, &store, &[merged_ticket("T-9")], 1).await;
    let record = store.docs_record("T-9").await.unwrap();
    assert!(record.paused);
    assert!(
        record.pause_reason.unwrap().contains("has not come up"),
        "parked with the reason"
    );

    // Resuming starts a new epoch with the failure streak cleared (once
    // the parked workspace is torn down, simulated here).
    store
        .set(
            KEY_WORKER_SLOTS,
            json!({ "lore": lore_slot(WorkerStatus::Idle, None) }),
        )
        .await;
    docs_event(
        &store,
        "T-9",
        "human",
        config::docs_lifecycle::DocsEvent::Resume,
    )
    .await;
    poll(&node, &store, &[merged_ticket("T-9")], 1).await;
    let resumed = delivery(&store, "T-9").await;
    assert_eq!(resumed.epoch, 1);
    assert!(resumed.failing_since.unwrap() > now() - 60);
    assert!(!store.docs_record("T-9").await.unwrap().paused);
}

#[tokio::test]
async fn repeated_lore_crashes_park_the_record() {
    let store = lore_store(lore_slot(bound_to("T-9"), None)).await;
    let node = lore_node();
    for _ in 0..lore::LORE_MAX_CRASHES {
        node.recover_lore_workspace(&store, "lore", "T-9").await;
    }
    assert_eq!(
        delivery(&store, "T-9").await.crashes,
        lore::LORE_MAX_CRASHES
    );
    poll(&node, &store, &[merged_ticket("T-9")], 1).await;
    let record = store.docs_record("T-9").await.unwrap();
    assert!(record.paused);
    assert!(record.pause_reason.unwrap().contains("crashed 3 times"));
}

#[tokio::test]
async fn lore_chat_whose_id_was_lost_is_adopted_not_duplicated() {
    let mut server = mockito::Server::new_async().await;
    let _ws = mock_ready_workspace(&mut server).await;
    let _list = server
        .mock("GET", "/api/v2/chats")
        .with_status(200)
        .with_body(
            r#"{"chats":[{"id":"orphan","workspace_id":"ws","status":"waiting","labels":{"role":"lore","ticket_id":"T-9"}},{"id":"other","workspace_id":"ws","status":"waiting","labels":{"role":"lore","ticket_id":"T-8"}}]}"#,
        )
        .create_async()
        .await;
    let create = server
        .mock("POST", "/api/v2/chats")
        .expect(0)
        .create_async()
        .await;
    let resend = server
        .mock("POST", "/api/v2/chats/orphan/messages")
        .match_body(mockito::Matcher::Regex("Document merged ticket T-9".into()))
        .with_status(200)
        .with_body(r#"{"id":"m1"}"#)
        .expect(1)
        .create_async()
        .await;
    let store = local_lore_store(&server, lore_slot(bound_to("T-9"), Some("ws"))).await;
    poll(&lore_node(), &store, &[merged_ticket("T-9")], 1).await;
    create.assert_async().await;
    resend.assert_async().await;
    assert_eq!(
        store
            .get_typed::<String>(&full_ticket_key("T-9", KEY_TICKET_CHAT, "lore"))
            .await
            .as_deref(),
        Some("orphan")
    );
    assert_eq!(delivery(&store, "T-9").await.step, "start:e0");
}

#[tokio::test]
async fn idle_lore_slot_never_reuses_another_tickets_workspace() {
    let mut server = mockito::Server::new_async().await;
    let create = server
        .mock("POST", "/api/v2/chats")
        .expect(0)
        .create_async()
        .await;
    let _gone = server
        .mock("GET", "/api/v2/workspaces/ws-other")
        .with_status(404)
        .create_async()
        .await;
    let store = local_lore_store(&server, lore_slot(WorkerStatus::Idle, Some("ws-other"))).await;
    poll(&lore_node(), &store, &[merged_ticket("T-9")], 1).await;
    create.assert_async().await;
    let slots = lore_slots(&store).await;
    assert!(matches!(lore_slot_status(&slots), WorkerStatus::Idle));
    assert_eq!(slots["lore"].workspace_id, None);
}

#[tokio::test]
async fn busy_lore_slot_makes_other_tickets_wait() {
    let mut server = mockito::Server::new_async().await;
    let create = server
        .mock("POST", "/api/v2/chats")
        .expect(0)
        .create_async()
        .await;
    let _chat = mock_lore_chat(&mut server, "running").await;
    let store = local_lore_store(&server, lore_slot(bound_to("T-9"), Some("ws"))).await;
    store
        .set(
            &full_ticket_key("T-9", KEY_TICKET_CHAT, "lore"),
            json!("lore-chat"),
        )
        .await;
    set_delivery(
        &store,
        "T-9",
        lore::LoreDelivery {
            step: "start:e0".into(),
            ts: now(),
            started: now(),
            ..Default::default()
        },
    )
    .await;
    poll(
        &lore_node(),
        &store,
        &[merged_ticket("T-9"), merged_ticket("T-10")],
        1,
    )
    .await;
    create.assert_async().await;
    let slots = lore_slots(&store).await;
    assert_eq!(lore_slot_status(&slots).ticket_id(), Some("T-9"));
    assert_eq!(
        delivery(&store, "T-10").await,
        lore::LoreDelivery::default()
    );
}

#[tokio::test]
async fn pausing_one_ticket_frees_lore_for_the_others() {
    let mut server = mockito::Server::new_async().await;
    let deleted = server
        .mock("DELETE", "/api/v1/runtime/workspaces/ws-1")
        .with_status(202)
        .expect(1)
        .create_async()
        .await;
    let _provision = mock_managed_provisioning(&mut server).await;
    let spawn_t10 = server
        .mock("POST", "/api/v1/runtime/chats")
        .match_body(mockito::Matcher::Regex(
            "Document merged ticket T-10".into(),
        ))
        .with_status(200)
        .with_body(r#"{"chat_id":"chat-10","workspace_id":"ws-new","created":true}"#)
        .expect(1)
        .create_async()
        .await;
    let store = lore_store(lore_slot(bound_to("T-9"), Some("ws-1"))).await;
    docs_event(
        &store,
        "T-9",
        "human",
        config::docs_lifecycle::DocsEvent::Pause {
            reason: "release freeze".into(),
        },
    )
    .await;
    let node = hosted_lore_node(&server);
    poll(
        &node,
        &store,
        &[merged_ticket("T-9"), merged_ticket("T-10")],
        3,
    )
    .await;
    deleted.assert_async().await;
    spawn_t10.assert_async().await;
    let slots = lore_slots(&store).await;
    assert_eq!(lore_slot_status(&slots).ticket_id(), Some("T-10"));
    assert_eq!(delivery(&store, "T-10").await.step, "start:e0");
}

#[tokio::test]
async fn resume_and_restart_replace_lore_on_a_fresh_workspace() {
    let mut server = mockito::Server::new_async().await;
    let _provision = mock_managed_provisioning(&mut server).await;
    let deleted = server
        .mock("DELETE", "/api/v1/runtime/workspaces/ws-new")
        .with_status(202)
        .expect(1)
        .create_async()
        .await;
    let spawned = server
        .mock("POST", "/api/v1/runtime/chats")
        .match_body(mockito::Matcher::Regex("Document merged ticket T-9".into()))
        .with_status(200)
        .with_body(r#"{"chat_id":"chat","workspace_id":"ws-new","created":true}"#)
        .expect(2)
        .create_async()
        .await;
    let store = lore_store(lore_slot(WorkerStatus::Idle, None)).await;
    // LORE had been told to start under epoch 0, then the operator paused it.
    set_delivery(
        &store,
        "T-9",
        lore::LoreDelivery {
            step: "start:e0".into(),
            ts: now(),
            started: now(),
            ..Default::default()
        },
    )
    .await;
    use config::docs_lifecycle::DocsEvent;
    docs_event(
        &store,
        "T-9",
        "human",
        DocsEvent::Pause {
            reason: "hold".into(),
        },
    )
    .await;
    let ticket = [merged_ticket("T-9")];
    let node = hosted_lore_node(&server);
    poll(&node, &store, &ticket, 1).await;
    assert_eq!(
        delivery(&store, "T-9").await.step,
        "start:e0",
        "paused: nothing sent"
    );

    docs_event(&store, "T-9", "human", DocsEvent::Resume).await;
    poll(&node, &store, &ticket, 2).await;
    let resumed = delivery(&store, "T-9").await;
    assert_eq!((resumed.step.as_str(), resumed.epoch), ("start:e1", 1));

    // Restart tears the live workspace down before re-delivering.
    docs_event(
        &store,
        "T-9",
        "human",
        DocsEvent::Restart {
            reason: "chat lost its context".into(),
        },
    )
    .await;
    poll(&node, &store, &ticket, 2).await;
    deleted.assert_async().await;
    spawned.assert_async().await;
    let restarted = delivery(&store, "T-9").await;
    assert_eq!((restarted.step.as_str(), restarted.epoch), ("start:e2", 2));
}

#[tokio::test]
async fn stalled_lore_is_nudged_then_parked_but_never_while_busy() {
    let mut server = mockito::Server::new_async().await;
    let mut chat = mock_lore_chat(&mut server, "running").await;
    let nudge = server
        .mock("POST", "/api/v2/chats/lore-chat/messages")
        .match_body(mockito::Matcher::Regex(
            "No documentation progress has been recorded".into(),
        ))
        .with_status(200)
        .with_body(r#"{"id":"n1"}"#)
        .expect(1)
        .create_async()
        .await;
    let store = local_lore_store(&server, lore_slot(bound_to("T-9"), Some("ws"))).await;
    store
        .set(
            &full_ticket_key("T-9", KEY_TICKET_CHAT, "lore"),
            json!("lore-chat"),
        )
        .await;
    let stale = now() - 2 * lore::LORE_STALL_SECS;
    let stalled = |nudges| lore::LoreDelivery {
        step: "start:e0".into(),
        ts: stale,
        started: stale,
        nudges,
        ..Default::default()
    };
    let ticket = [merged_ticket("T-9")];
    let node = lore_node();

    // A running turn is working, however long it takes (below the hard limit).
    set_delivery(&store, "T-9", stalled(0)).await;
    poll(&node, &store, &ticket, 1).await;
    assert_eq!(delivery(&store, "T-9").await.nudges, 0);

    chat.remove_async().await;
    chat = mock_lore_chat(&mut server, "waiting").await;
    poll(&node, &store, &ticket, 2).await;
    let after = delivery(&store, "T-9").await;
    assert_eq!(after.nudges, 1);
    assert!(after.ts > stale, "a nudge restarts the stall timer");
    nudge.assert_async().await;

    // After the last nudge LORE is parked: the record is paused for the operator.
    set_delivery(&store, "T-9", stalled(lore::LORE_MAX_NUDGES)).await;
    poll(&node, &store, &ticket, 1).await;
    let record = store.docs_record("T-9").await.unwrap();
    assert!(record.paused);
    assert!(record.pause_reason.unwrap().contains("nudges"));
    drop(chat);
}

#[tokio::test]
async fn a_step_running_past_the_hard_limit_is_parked() {
    let mut server = mockito::Server::new_async().await;
    let _chat = mock_lore_chat(&mut server, "running").await;
    let store = local_lore_store(&server, lore_slot(bound_to("T-9"), Some("ws"))).await;
    store
        .set(
            &full_ticket_key("T-9", KEY_TICKET_CHAT, "lore"),
            json!("lore-chat"),
        )
        .await;
    set_delivery(
        &store,
        "T-9",
        lore::LoreDelivery {
            step: "start:e0".into(),
            ts: now(),
            started: now() - lore::LORE_STEP_MAX_SECS - 1,
            ..Default::default()
        },
    )
    .await;
    poll(&lore_node(), &store, &[merged_ticket("T-9")], 1).await;
    let record = store.docs_record("T-9").await.unwrap();
    assert!(record.paused, "a running chat cannot hold the slot forever");
}

#[tokio::test]
async fn paused_docs_do_not_spawn_lore() {
    let mut server = mockito::Server::new_async().await;
    let create = server
        .mock("POST", "/api/v2/chats")
        .expect(0)
        .create_async()
        .await;
    let store = local_lore_store(&server, lore_slot(WorkerStatus::Idle, None)).await;
    docs_event(
        &store,
        "T-9",
        "human",
        config::docs_lifecycle::DocsEvent::Pause {
            reason: "release freeze".into(),
        },
    )
    .await;
    poll(&lore_node(), &store, &[merged_ticket("T-9")], 1).await;
    create.assert_async().await;
    let slots = lore_slots(&store).await;
    assert!(matches!(lore_slot_status(&slots), WorkerStatus::Idle));
}

#[tokio::test]
async fn hosted_lore_binds_slot_then_spawns_after_manager_reports_ready() {
    let mut server = mockito::Server::new_async().await;
    let request = server
        .mock("POST", "/api/v1/runtime/workspaces")
        .match_body(mockito::Matcher::Regex(r#""ticket_id":"T-9""#.into()))
        .with_status(202)
        .with_body(r#"{"workspace_id":"managed-lore","operation_id":"op","status":"queued"}"#)
        .expect(1)
        .create_async()
        .await;
    let starting = server
        .mock("GET", "/api/v1/runtime/workspaces/managed-lore")
        .with_status(200)
        .with_body(managed_status("managed-lore", "starting"))
        .expect(1)
        .create_async()
        .await;
    let create = server
        .mock("POST", "/api/v1/runtime/chats")
        .match_body(mockito::Matcher::Regex("docs submit".into()))
        .with_status(200)
        .with_body(r#"{"chat_id":"lore-chat","workspace_id":"managed-lore","created":true}"#)
        .expect(1)
        .create_async()
        .await;
    let store = lore_store(lore_slot(WorkerStatus::Idle, None)).await;
    let ticket = [merged_ticket("T-9")];
    let node = hosted_lore_node(&server);
    // Bind + request, then still starting.
    poll(&node, &store, &ticket, 2).await;
    request.assert_async().await;
    starting.assert_async().await;
    let slots = lore_slots(&store).await;
    assert_eq!(lore_slot_status(&slots).ticket_id(), Some("T-9"));
    assert_eq!(slots["lore"].workspace_id.as_deref(), Some("managed-lore"));

    starting.remove_async().await;
    let _ready = server
        .mock("GET", "/api/v1/runtime/workspaces/managed-lore")
        .with_status(200)
        .with_body(managed_status("managed-lore", "running"))
        .create_async()
        .await;
    poll(&node, &store, &ticket, 2).await;
    create.assert_async().await;
}

#[tokio::test]
async fn a_vanished_hosted_chat_is_forgotten_and_eventually_parks_lore() {
    let mut server = mockito::Server::new_async().await;
    let gone = server
        .mock("POST", "/api/v1/runtime/chats/chat-1/messages")
        .with_status(404)
        .with_body(r#"{"error":"chat not found"}"#)
        .expect(1)
        .create_async()
        .await;
    let _deleted = server
        .mock("DELETE", "/api/v1/runtime/workspaces/ws-1")
        .with_status(202)
        .create_async()
        .await;
    let store = lore_store(lore_slot(bound_to("T-9"), Some("ws-1"))).await;
    let chat_key = full_ticket_key("T-9", KEY_TICKET_CHAT, "lore");
    store.set(&chat_key, json!("chat-1")).await;
    let ticket = [merged_ticket("T-9")];
    let node = hosted_lore_node(&server);

    // The step cannot be delivered: the stored chat is forgotten (the next
    // poll asks the Manager again) and the failure streak starts.
    poll(&node, &store, &ticket, 2).await;
    gone.assert_async().await;
    assert!(store.get_typed::<String>(&chat_key).await.is_none());
    let streak = delivery(&store, "T-9").await.failing_since;
    assert!(
        streak.is_some(),
        "a failed delivery must count as a failure"
    );

    // A streak past the limit parks LORE for the operator and frees the slot.
    let key = full_ticket_key_flat("T-9", config::state::KEY_TICKET_DOCS_DELIVERY);
    let mut stale = delivery(&store, "T-9").await;
    stale.failing_since = Some(streak.unwrap() - lore::LORE_PROVISION_MAX_SECS - 1);
    store.set(&key, json!(stale)).await;
    poll(&node, &store, &ticket, 3).await;
    assert!(store.docs_record("T-9").await.unwrap().paused);
}

#[tokio::test]
async fn docs_ready_releases_lore_and_rejection_respawns_it_with_feedback() {
    let mut server = mockito::Server::new_async().await;
    let deleted = server
        .mock("DELETE", "/api/v1/runtime/workspaces/ws-1")
        .with_status(202)
        .expect(1)
        .create_async()
        .await;
    let _provision = mock_managed_provisioning(&mut server).await;
    let feedback = server
        .mock("POST", "/api/v1/runtime/chats")
        .match_body(mockito::Matcher::Regex(
            "needs another revision. Feedback: Explain the migration".into(),
        ))
        .with_status(200)
        .with_body(r#"{"chat_id":"chat-2","workspace_id":"ws-new","created":true}"#)
        .expect(1)
        .create_async()
        .await;
    let store = lore_store(lore_slot(bound_to("T-9"), Some("ws-1"))).await;
    store
        .set(
            &full_ticket_key("T-9", KEY_TICKET_CHAT, "lore"),
            json!("chat-1"),
        )
        .await;
    let ticket = [merged_ticket("T-9")];
    let node = hosted_lore_node(&server);

    submit_docs(&store, "T-9", "abc").await;
    poll(&node, &store, &ticket, 2).await;
    deleted.assert_async().await;
    let slots = lore_slots(&store).await;
    assert!(
        matches!(lore_slot_status(&slots), WorkerStatus::Idle),
        "waiting on the operator holds no slot or workspace"
    );
    assert_eq!(delivery(&store, "T-9").await.ready_notified, Some(1));

    decide_docs(&store, "T-9", false, "Explain the migration").await;
    poll(&node, &store, &ticket, 3).await;
    feedback.assert_async().await;
    assert_eq!(
        store
            .get_typed::<String>(&full_ticket_key("T-9", KEY_TICKET_CHAT, "lore"))
            .await
            .as_deref(),
        Some("chat-2")
    );
}

//! Run against a disposable Redis: TEST_REDIS_URL=redis://127.0.0.1:6379 cargo test
//! -p openflows-harness --test lifecycle_redis -- --ignored
use config::lifecycle::{Event, Lifecycle, Phase};
use openflows_harness::HarnessStore;
use pocketflow_core::SharedStore;

#[tokio::test]
#[ignore = "requires disposable Redis in TEST_REDIS_URL"]
async fn real_redis_plan_round_trip_and_atomic_review_cycle() {
    let url = std::env::var("TEST_REDIS_URL").expect("TEST_REDIS_URL");
    let tenant = format!("lifecycle-test-{}", uuid::Uuid::new_v4());
    let harness = HarnessStore::new(&url, &tenant).await.unwrap();
    let a = SharedStore::new_redis_with_tenant(&url, Some(tenant.clone()))
        .await
        .unwrap();
    let b = SharedStore::new_redis_with_tenant(&url, Some(tenant))
        .await
        .unwrap();
    let plan = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(plan.path(), "# Plan\n\nVerify the target workflow.").unwrap();
    harness.plan_write("T-1", plan.path()).await.unwrap();
    let state = a.lifecycle("T-1").await.unwrap();
    assert_eq!(state.plan, "# Plan\n\nVerify the target workflow.");
    assert_eq!(
        a.get_typed::<String>("pair:T-1:plan").await.as_deref(),
        Some(state.plan.as_str())
    );
    harness
        .status_set("T-1", "forge", "plan_ready")
        .await
        .unwrap();
    let state = a.lifecycle("T-1").await.unwrap();
    let decision = Event::Decide {
        round: state.review_round,
        phase: Phase::PlanReady,
        approved: true,
        report: "Reviewed".into(),
        revision: state.revision,
        head: None,
    };
    let (first, second) = tokio::join!(
        a.transition("T-1", state.version, "sentinel", decision.clone()),
        b.transition("T-1", state.version, "sentinel", decision)
    );
    assert_ne!(
        first.is_ok(),
        second.is_ok(),
        "only one competing decision may commit"
    );
    assert_eq!(a.lifecycle("T-1").await.unwrap().phase, Phase::Building);
    assert!(harness.status_set("T-1", "forge", "submit").await.is_err());
    assert!(harness.plan_write("T-1", plan.path()).await.is_err());
    let state = a.lifecycle("T-1").await.unwrap();
    assert_eq!(state.phase, Phase::Building);
    assert_eq!(state.history.len(), 3);
    // Fault injection belongs only on this disposable Redis instance.
    // A rejected write must not consume approval or advance the phase.
    use fred::prelude::*;
    let client = Builder::from_config(Config::from_url(&url).unwrap())
        .build()
        .unwrap();
    client.init().await.unwrap();
    client
        .config_set("maxmemory-policy", "noeviction")
        .await
        .unwrap();
    client.config_set("maxmemory", "1").await.unwrap();
    let failed = harness.status_set("T-1", "forge", "blocked").await;
    client.config_set("maxmemory", "0").await.unwrap();
    let error = failed.expect_err("Redis must reject a write over maxmemory");
    assert!(
        format!("{error:#}").contains("OOM"),
        "unexpected error: {error:#}"
    );
    assert_eq!(a.lifecycle("T-1").await.unwrap(), state);
    a.del("ticket:T-1:status").await;
    a.del("pair:T-1:plan").await;
}

#[tokio::test]
#[ignore = "requires disposable Redis in TEST_REDIS_URL"]
async fn redis_review_lease_serializes_controllers_and_checks_owner() {
    let url = std::env::var("TEST_REDIS_URL").expect("TEST_REDIS_URL");
    let tenant = format!("review-lease-test-{}", uuid::Uuid::new_v4());
    let a = SharedStore::new_redis_with_tenant(&url, Some(tenant.clone()))
        .await
        .unwrap();
    let b = SharedStore::new_redis_with_tenant(&url, Some(tenant))
        .await
        .unwrap();
    let key = "ticket:T-1:review_poll_lease";
    let (first, second) =
        tokio::join!(a.try_claim(key, "first", 1), b.try_claim(key, "second", 1),);
    let first = first.unwrap();
    assert_ne!(first, second.unwrap());
    let (owner, other) = if first {
        ("first", "second")
    } else {
        ("second", "first")
    };
    b.release_claim(key, other).await.unwrap();
    assert!(!a.try_claim(key, "third", 1).await.unwrap());
    tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
    assert!(b.try_claim(key, "new-owner", 30).await.unwrap());
    a.release_claim(key, owner).await.unwrap();
    assert!(!a.try_claim(key, "third", 1).await.unwrap());
    b.release_claim(key, "new-owner").await.unwrap();
    assert!(a.try_claim(key, "third", 30).await.unwrap());
    a.release_claim(key, "third").await.unwrap();
}

async fn advance(store: &SharedStore, actor: &str, event: Event) -> Lifecycle {
    let state = store.lifecycle("T-1").await.unwrap();
    store
        .transition("T-1", state.version, actor, event)
        .await
        .unwrap();
    store.lifecycle("T-1").await.unwrap()
}

async fn review(store: &SharedStore, actor: &str) -> Lifecycle {
    let state = store.lifecycle("T-1").await.unwrap();
    advance(
        store,
        actor,
        Event::Decide {
            round: state.review_round,
            phase: state.phase,
            approved: true,
            report: "Container integration review".into(),
            revision: state.revision,
            head: state.head,
        },
    )
    .await
}

#[tokio::test]
#[ignore = "requires disposable Redis in TEST_REDIS_URL"]
async fn persisted_workflow_requires_current_evidence_and_survives_reconnection() {
    let url = std::env::var("TEST_REDIS_URL").expect("TEST_REDIS_URL");
    let tenant = format!("workflow-test-{}", uuid::Uuid::new_v4());
    let store = SharedStore::new_redis_with_tenant(&url, Some(tenant.clone()))
        .await
        .unwrap();
    advance(
        &store,
        "forge",
        Event::Plan {
            content: "Plan".into(),
        },
    )
    .await;
    advance(
        &store,
        "forge",
        Event::Move {
            phase: Phase::PlanReady,
            head: None,
        },
    )
    .await;
    review(&store, "sentinel").await;
    let testing = advance(
        &store,
        "forge",
        Event::Move {
            phase: Phase::Testing,
            head: Some("candidate-1".into()),
        },
    )
    .await;
    // An approval without verification cannot authorize publication.
    assert!(store
        .transition(
            "T-1",
            testing.version,
            "sentinel",
            Event::Decide {
                round: testing.review_round,
                phase: Phase::Testing,
                approved: true,
                report: "Missing evidence".into(),
                revision: testing.revision,
                head: testing.head.clone(),
            }
        )
        .await
        .is_err());
    advance(
        &store,
        "sentinel",
        Event::Verified {
            head: "candidate-1".into(),
            task: "verification-1".into(),
        },
    )
    .await;
    review(&store, "sentinel").await;
    advance(&store, "forge", Event::Pr { number: 42 }).await;
    review(&store, "sentinel").await;
    let approved = review(&store, "human").await;
    assert!(
        !approved.merge_ready("candidate-1"),
        "review delivery is still pending"
    );
    let ready = advance(
        &store,
        "sentinel",
        Event::ReviewDelivered {
            round: approved.review_round,
        },
    )
    .await;
    assert!(ready.merge_ready("candidate-1"));
    assert!(!ready.merge_ready("candidate-2"));
    assert!(store
        .transition(
            "T-1",
            ready.version,
            "vessel",
            Event::BeginMerge {
                head: "candidate-2".into(),
            }
        )
        .await
        .is_err());

    // A fresh client must recover the same authoritative state from Redis.
    drop(store);
    let resumed = SharedStore::new_redis_with_tenant(&url, Some(tenant.clone()))
        .await
        .unwrap();
    assert_eq!(resumed.lifecycle("T-1").await.unwrap(), ready);
    let isolated = SharedStore::new_redis_with_tenant(&url, Some(format!("{tenant}-other")))
        .await
        .unwrap();
    assert_eq!(
        isolated.lifecycle("T-1").await.unwrap(),
        Lifecycle::default()
    );
    advance(
        &resumed,
        "vessel",
        Event::BeginMerge {
            head: "candidate-1".into(),
        },
    )
    .await;
    let done = advance(
        &resumed,
        "vessel",
        Event::Merged {
            number: 42,
            head: "candidate-1".into(),
            sha: "merge-commit".into(),
        },
    )
    .await;
    assert_eq!(done.phase, Phase::Done);
    assert_eq!(done.merge_sha.as_deref(), Some("merge-commit"));
    assert!(
        resumed
            .transition(
                "T-1",
                done.version,
                "forge",
                Event::Move {
                    phase: Phase::Planning,
                    head: None,
                }
            )
            .await
            .is_err(),
        "completed tickets cannot restart"
    );
}

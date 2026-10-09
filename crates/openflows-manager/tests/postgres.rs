//! Isolated PostgreSQL integration tests for the WP-01 manager foundations.
//!
//! These tests exercise the real migration runner, database constraints, and
//! repository/transaction query paths against a real PostgreSQL 16 instance —
//! not SQLite or mocks. They require an Openflows control-plane test database
//! (see `common/mod.rs` and the PR description for setup).
//!
//! Evidence mapping:
//!   1. Migrations apply to an empty database; rerunning is safe.
//!   2. Constraints enforce relationships and uniqueness.
//!   3. A failed transaction leaves no partial membership/audit/outbox writes.
//!   4. Organization A cannot read or modify organization B's records.
//!   5. Concurrent workers cannot claim the same active lease; expired leases
//!      are recoverable; a stale worker cannot complete a reclaimed operation.
//!   6. Idempotent retries do not duplicate; conflicting reuse is rejected.
//!   7. Pagination has stable ordering and preserves organization scope.

mod common;

use chrono::{Duration, Utc};
use common::*;
use openflows_manager::db::tx::OrgScope;
use openflows_manager::error::ManagerError;
use openflows_manager::id::*;
use openflows_manager::idempotency::{IdempotencyOutcome, IdempotencyRequest, IdempotencyService};
use openflows_manager::operations;
use openflows_manager::pagination::PageLimit;
use openflows_manager::repositories::{OrganizationsRepository, TenantsRepository};
use openflows_manager::{audit, outbox};
use serde_json::json;

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn outbox_claims_reject_stale_acknowledgments_and_releases() {
    let db = TestDb::new().await.unwrap();
    db.migrate().await.unwrap();
    let pool = db.pool();
    let mut tx = pool.begin().await.unwrap();
    let id = outbox::insert_in_tx(&mut tx, None, "test", None)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let a = outbox::claim(pool, "same-worker", 1)
        .await
        .unwrap()
        .remove(0);
    assert!(outbox::claim(pool, "other-worker", 1)
        .await
        .unwrap()
        .is_empty());
    sqlx::query(
        "UPDATE outbox_events SET lease_expires_at = now() - interval '1 second' WHERE id = $1",
    )
    .bind(id.0)
    .execute(pool)
    .await
    .unwrap();
    assert!(!outbox::mark_delivered(pool, &a).await.unwrap());
    assert!(!outbox::release_lease(pool, &a).await.unwrap());
    let b = outbox::claim(pool, "same-worker", 1)
        .await
        .unwrap()
        .remove(0);
    assert_eq!(b.attempts, 2);
    assert!(!outbox::mark_delivered(pool, &a).await.unwrap());
    assert!(!outbox::release_lease(pool, &a).await.unwrap());
    assert!(outbox::release_lease(pool, &b).await.unwrap());
    let c = outbox::claim(pool, "same-worker", 1)
        .await
        .unwrap()
        .remove(0);
    assert!(!outbox::mark_delivered(pool, &b).await.unwrap());
    assert!(outbox::mark_delivered(pool, &c).await.unwrap());
    assert!(!outbox::mark_delivered(pool, &c).await.unwrap());
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn idempotency_rolls_back_failed_mutations_and_serializes_retries() {
    let db = TestDb::new().await.unwrap();
    db.migrate().await.unwrap();
    let pool = db.pool();
    let user = insert_user(pool).await.unwrap();
    let org = insert_organization_with_owner(pool, "atomic-idem", user)
        .await
        .unwrap();
    let conn = insert_connection(pool, org, 42).await.unwrap();
    insert_repository(pool, org, conn, 43, "atomic/repo")
        .await
        .unwrap();
    let service = IdempotencyService::new(pool.clone());
    let req = IdempotencyRequest {
        actor_id: UserId::from_uuid(user),
        organization_id: OrganizationId::from_uuid(org),
        route: "tenant.create".into(),
        key: "retry".into(),
        request_hash: IdempotencyRequest::hash_request(&json!({"slug":"atomic"})).unwrap(),
    };
    let tenants = TenantsRepository::new(pool.clone());
    let fail = service
        .execute(&req, move |db_conn| {
            Box::pin(async move {
                let tenant = tenants
                    .create_in_tx(
                        db_conn,
                        OrgScope::new(OrganizationId::from_uuid(org)),
                        "atomic",
                        ConnectionId::from_uuid(conn),
                        GithubId(43),
                        1,
                    )
                    .await?;
                operations::create_in_tx(
                    db_conn,
                    &operations::NewOperation {
                        organization_id: OrganizationId::from_uuid(org),
                        resource_type: "tenant".into(),
                        resource_id: Some(tenant.0),
                        kind: "provision".into(),
                        idempotency_ref: None,
                    },
                )
                .await?;
                audit::insert(
                    &mut *db_conn,
                    &audit::AuditEvent::new("tenant.create")
                        .organization(OrganizationId::from_uuid(org)),
                )
                .await?;
                outbox::insert_in_tx(
                    db_conn,
                    Some(OrganizationId::from_uuid(org)),
                    "tenant.created",
                    None,
                )
                .await?;
                Err(ManagerError::Conflict("forced rollback".into()))
            })
        })
        .await;
    assert!(fail.is_err());
    for table in [
        "tenants",
        "operations",
        "idempotency_keys",
        "audit_events",
        "outbox_events",
    ] {
        let n: i64 = sqlx::query_scalar(&format!("SELECT count(*) FROM {table}"))
            .fetch_one(pool)
            .await
            .unwrap();
        assert_eq!(n, 0, "{table} must roll back");
    }
    let mut tasks = Vec::new();
    for _ in 0..8 {
        let service = service.clone();
        let req = req.clone();
        let tenants = TenantsRepository::new(pool.clone());
        tasks.push(tokio::spawn(async move {
            service
                .execute(&req, move |db_conn| {
                    Box::pin(async move {
                        let id = tenants
                            .create_in_tx(
                                db_conn,
                                OrgScope::new(OrganizationId::from_uuid(org)),
                                "atomic",
                                ConnectionId::from_uuid(conn),
                                GithubId(43),
                                1,
                            )
                            .await?;
                        Ok(id.to_string())
                    })
                })
                .await
                .unwrap()
        }));
    }
    let mut new_count = 0;
    let mut references = std::collections::HashSet::new();
    for task in tasks {
        match task.await.unwrap() {
            IdempotencyOutcome::New { response_reference } => {
                new_count += 1;
                references.insert(response_reference);
            }
            IdempotencyOutcome::Replay { response_reference } => {
                references.insert(response_reference);
            }
        }
    }
    assert_eq!(new_count, 1);
    assert_eq!(references.len(), 1);
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM tenants")
        .fetch_one(pool)
        .await
        .unwrap();
    assert_eq!(n, 1);

    // Expiry allows a new logical request without an out-of-band prune job.
    sqlx::query("UPDATE idempotency_keys SET expires_at = now() - interval '1 second'")
        .execute(pool)
        .await
        .unwrap();
    assert!(matches!(
        service
            .execute(&req, |_| Box::pin(async { Ok("replacement".into()) }))
            .await
            .unwrap(),
        IdempotencyOutcome::New { .. }
    ));
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn database_rejects_cross_organization_relationships() {
    let db = TestDb::new().await.unwrap();
    db.migrate().await.unwrap();
    let pool = db.pool();
    let user = insert_user(pool).await.unwrap();
    let a = insert_organization_with_owner(pool, "a", user)
        .await
        .unwrap();
    let b = insert_organization_with_owner(pool, "b", user)
        .await
        .unwrap();
    let conn = insert_connection(pool, b, 100).await.unwrap();
    assert!(insert_repository(pool, a, conn, 101, "b/repo")
        .await
        .is_err());
    insert_repository(pool, b, conn, 101, "b/repo")
        .await
        .unwrap();
    let invalid = sqlx::query("INSERT INTO tenants (id, organization_id, slug, connection_id, github_repository_id, fleet_size) VALUES ($1, $2, 'wrong', $3, 101, 1)")
        .bind(uuid::Uuid::new_v4()).bind(a).bind(conn).execute(pool).await;
    assert!(invalid.is_err());
    let tenant = TenantsRepository::new(pool.clone())
        .create(
            OrgScope::new(OrganizationId::from_uuid(b)),
            "correct",
            ConnectionId::from_uuid(conn),
            GithubId(101),
            1,
        )
        .await
        .unwrap();
    let invalid = sqlx::query("INSERT INTO runtime_identities (id, tenant_id, organization_id, coder_owner_id) VALUES ($1, $2, $3, 'owner')")
        .bind(uuid::Uuid::new_v4()).bind(tenant.0).bind(a).execute(pool).await;
    assert!(invalid.is_err());
    sqlx::query("INSERT INTO runtime_identities (id, tenant_id, organization_id, coder_owner_id) VALUES ($1, $2, $3, 'owner')")
        .bind(uuid::Uuid::new_v4()).bind(tenant.0).bind(b).execute(pool).await.unwrap();
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn completed_deletion_releases_tenant_names_and_workspace_slots() {
    let db = TestDb::new().await.unwrap();
    db.migrate().await.unwrap();
    let pool = db.pool();
    let user = insert_user(pool).await.unwrap();
    let org = insert_organization_with_owner(pool, "reuse", user)
        .await
        .unwrap();
    let conn = insert_connection(pool, org, 101).await.unwrap();
    insert_repository(pool, org, conn, 102, "reuse/repo")
        .await
        .unwrap();
    let tenants = TenantsRepository::new(pool.clone());
    let scope = OrgScope::new(OrganizationId::from_uuid(org));
    let old = tenants
        .create(
            scope,
            "reuse",
            ConnectionId::from_uuid(conn),
            GithubId(102),
            1,
        )
        .await
        .unwrap();
    sqlx::query(
        "UPDATE tenants SET desired_state = 'deleted', observed_state = 'deleting' WHERE id = $1",
    )
    .bind(old.0)
    .execute(pool)
    .await
    .unwrap();
    assert!(tenants
        .create(
            scope,
            "reuse",
            ConnectionId::from_uuid(conn),
            GithubId(102),
            1
        )
        .await
        .is_err());
    sqlx::query("UPDATE tenants SET observed_state = 'deleted' WHERE id = $1")
        .bind(old.0)
        .execute(pool)
        .await
        .unwrap();
    let new = tenants
        .create(
            scope,
            "reuse",
            ConnectionId::from_uuid(conn),
            GithubId(102),
            1,
        )
        .await
        .unwrap();
    assert_ne!(old, new);
    for slot in [None, Some(0)] {
        let id = uuid::Uuid::new_v4();
        sqlx::query("INSERT INTO workspaces (id, organization_id, tenant_id, role, slot) VALUES ($1, $2, $3, 'nexus', $4)")
            .bind(id).bind(org).bind(new.0).bind(slot).execute(pool).await.unwrap();
        let duplicate = sqlx::query("INSERT INTO workspaces (id, organization_id, tenant_id, role, slot) VALUES ($1, $2, $3, 'nexus', $4)")
            .bind(uuid::Uuid::new_v4()).bind(org).bind(new.0).bind(slot).execute(pool).await;
        assert!(duplicate.is_err(), "duplicate slot {slot:?}");
        sqlx::query("UPDATE workspaces SET desired_state = 'deleted', observed_state = 'deleted' WHERE id = $1")
            .bind(id).execute(pool).await.unwrap();
        sqlx::query("INSERT INTO workspaces (id, organization_id, tenant_id, role, slot) VALUES ($1, $2, $3, 'nexus', $4)")
            .bind(uuid::Uuid::new_v4()).bind(org).bind(new.0).bind(slot).execute(pool).await.unwrap();
    }
}

// ---------------------------------------------------------------------------
// 1. Migrations
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore = "requires a live PostgreSQL (see OPENFLOWS_TEST_DATABASE_URL); run via scripts/run-integration-tests.sh"]
async fn migrations_apply_to_empty_database() {
    let db = TestDb::new().await.expect("create test schema");
    db.migrate()
        .await
        .expect("migrations apply to empty database");

    let count = db.applied_migrations().await.expect("count migrations");
    assert!(
        count >= 4,
        "expected at least 4 numbered migrations, got {count}"
    );
}

#[tokio::test]
#[ignore = "requires a live PostgreSQL (see OPENFLOWS_TEST_DATABASE_URL); run via scripts/run-integration-tests.sh"]
async fn rerunning_migration_runner_is_safe() {
    let db = TestDb::new().await.expect("create test schema");
    db.migrate().await.expect("first migrate");
    let first = db.applied_migrations().await.expect("count first");

    // Rerunning against an already-migrated schema must be a no-op success and
    // must not duplicate or alter applied migrations.
    db.migrate().await.expect("rerun migrate is safe");
    let second = db.applied_migrations().await.expect("count second");
    assert_eq!(first, second, "rerun must not add or change migrations");
}

// ---------------------------------------------------------------------------
// 2. Constraints
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore = "requires a live PostgreSQL (see OPENFLOWS_TEST_DATABASE_URL); run via scripts/run-integration-tests.sh"]
async fn constraints_enforce_uniqueness_and_relationships() {
    let db = TestDb::new().await.expect("create test schema");
    db.migrate().await.expect("migrate");
    let pool = db.pool();

    let owner = insert_user(pool).await.expect("insert user");
    let org = insert_organization_with_owner(pool, "acme", owner)
        .await
        .expect("insert org");

    // Duplicate slug is rejected (raw helper surfaces the DB unique violation;
    // the repository layer translates this to a Conflict).
    let dup = insert_organization_with_owner(pool, "acme", owner).await;
    assert!(
        dup.is_err(),
        "duplicate slug must be rejected by the database"
    );

    // A tenant cannot reference a connection from another organization: the
    // composite FK (organization_id, connection) is enforced by the scoped
    // repository lookup, and the tenant uniqueness constraints are enforced at
    // the database level.
    let conn_a = insert_connection(pool, org, 9001).await.expect("conn A");
    let repo_a = 4001;
    insert_repository(pool, org, conn_a, repo_a, "acme/backend")
        .await
        .expect("repo A");

    // Same repository twice => unique live(org, repo) violation -> conflict.
    let tenants = TenantsRepository::new(pool.clone());
    let scope = OrgScope::new(OrganizationId::from_uuid(org));
    let first = tenants
        .create(
            scope,
            "backend",
            ConnectionId::from_uuid(conn_a),
            GithubId(repo_a),
            2,
        )
        .await;
    assert!(first.is_ok(), "first tenant creation should succeed");
    let second = tenants
        .create(
            scope,
            "backend-2",
            ConnectionId::from_uuid(conn_a),
            GithubId(repo_a),
            2,
        )
        .await;
    assert!(
        matches!(second, Err(ManagerError::Conflict(_))),
        "duplicate live tenant for repository must conflict"
    );

    // Same slug twice => unique live(org, slug) violation -> conflict.
    let repo_b = 4002;
    insert_repository(pool, org, conn_a, repo_b, "acme/other")
        .await
        .expect("repo B");
    let third = tenants
        .create(
            scope,
            "backend",
            ConnectionId::from_uuid(conn_a),
            GithubId(repo_b),
            2,
        )
        .await;
    assert!(
        matches!(third, Err(ManagerError::Conflict(_))),
        "duplicate tenant slug must conflict"
    );

    // Idempotency key uniqueness: same (actor, org, route, key) is unique.
    let idem = IdempotencyService::new(pool.clone());
    let actor = UserId::from_uuid(owner);
    let req = IdempotencyRequest {
        actor_id: actor,
        organization_id: OrganizationId::from_uuid(org),
        route: "/api/v1/organizations/acme/tenants".to_string(),
        key: "key-1".to_string(),
        request_hash: IdempotencyRequest::hash_request(&json!({"a": 1})).unwrap(),
    };
    assert!(matches!(
        idem.execute(&req, |_| Box::pin(async { Ok("result:1".into()) }))
            .await
            .expect("first request"),
        IdempotencyOutcome::New { .. }
    ));
    // Replay with identical body returns Replay, not a duplicate.
    assert!(matches!(
        idem.execute(&req, |_| Box::pin(async { panic!("must replay") }))
            .await
            .expect("second request"),
        IdempotencyOutcome::Replay { .. }
    ));
}

// ---------------------------------------------------------------------------
// 3. Transaction rollback
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore = "requires a live PostgreSQL (see OPENFLOWS_TEST_DATABASE_URL); run via scripts/run-integration-tests.sh"]
async fn failed_transaction_leaves_no_partial_membership_audit_or_outbox() {
    let db = TestDb::new().await.expect("create test schema");
    db.migrate().await.expect("migrate");
    let pool = db.pool();

    let owner = insert_user(pool).await.expect("insert user");
    let org_id = OrganizationId::from_uuid(uuid::Uuid::new_v4());
    let scope = OrgScope::new(org_id);

    // Begin a transaction that inserts the org, membership, audit event, and
    // outbox event, then force a failure on the audit insert so the whole
    // transaction must roll back.
    let tx = pool.begin().await.expect("begin");
    let mut tx_scope = OrganizationsRepository::new(pool.clone())
        .create_with_owner(
            scope,
            "rollback-org",
            "Rollback",
            UserId::from_uuid(owner),
            tx,
        )
        .await
        .expect("create org + membership");

    // Successful audit + outbox inserts within the transaction.
    audit::insert_in_tx(
        tx_scope.tx(),
        &audit::AuditEvent::new("org.create")
            .organization(org_id)
            .actor(UserId::from_uuid(owner)),
    )
    .await
    .expect("insert audit");
    outbox::insert_in_tx(
        tx_scope.tx(),
        Some(org_id),
        "org.provisioned",
        Some(&json!({"org": org_id.to_string()})),
    )
    .await
    .expect("insert outbox");

    // Force a failure: insert a second membership with an invalid role that
    // violates the CHECK constraint. This must fail the transaction.
    let err = sqlx::query(
        "INSERT INTO memberships (organization_id, user_id, role, status)
         VALUES ($1, $2, 'not-a-role', 'active')",
    )
    .bind(org_id.0)
    .bind(owner)
    .execute(&mut **tx_scope.tx())
    .await;
    assert!(err.is_err(), "invalid role must fail the transaction");

    // Drop the transaction without committing (rolls back).
    drop(tx_scope);

    // Nothing should be persisted: no org, no membership, no audit, no outbox.
    let orgs: i64 = sqlx::query_scalar("SELECT count(*) FROM organizations WHERE id = $1")
        .bind(org_id.0)
        .fetch_one(pool)
        .await
        .expect("count orgs");
    let memberships: i64 =
        sqlx::query_scalar("SELECT count(*) FROM memberships WHERE organization_id = $1")
            .bind(org_id.0)
            .fetch_one(pool)
            .await
            .expect("count memberships");
    let audits: i64 =
        sqlx::query_scalar("SELECT count(*) FROM audit_events WHERE organization_id = $1")
            .bind(org_id.0)
            .fetch_one(pool)
            .await
            .expect("count audits");
    let outboxes: i64 =
        sqlx::query_scalar("SELECT count(*) FROM outbox_events WHERE organization_id = $1")
            .bind(org_id.0)
            .fetch_one(pool)
            .await
            .expect("count outboxes");

    assert_eq!(orgs, 0, "no partial organization row");
    assert_eq!(memberships, 0, "no partial membership row");
    assert_eq!(audits, 0, "no partial audit row");
    assert_eq!(outboxes, 0, "no partial outbox row");
}

// ---------------------------------------------------------------------------
// 4. Organization isolation
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore = "requires a live PostgreSQL (see OPENFLOWS_TEST_DATABASE_URL); run via scripts/run-integration-tests.sh"]
async fn organization_a_cannot_read_or_modify_organization_b() {
    let db = TestDb::new().await.expect("create test schema");
    db.migrate().await.expect("migrate");
    let pool = db.pool();

    let owner_a = insert_user(pool).await.expect("user A");
    let owner_b = insert_user(pool).await.expect("user B");
    let org_a = insert_organization_with_owner(pool, "org-a", owner_a)
        .await
        .expect("org A");
    let org_b = insert_organization_with_owner(pool, "org-b", owner_b)
        .await
        .expect("org B");

    let scope_a = OrgScope::new(OrganizationId::from_uuid(org_a));
    let scope_b = OrgScope::new(OrganizationId::from_uuid(org_b));

    let tenants = TenantsRepository::new(pool.clone());

    // Tenant in org A.
    let conn_a = insert_connection(pool, org_a, 7001).await.expect("conn A");
    insert_repository(pool, org_a, conn_a, 5001, "org-a/repo")
        .await
        .expect("repo A");
    let tenant_a = tenants
        .create(
            scope_a,
            "backend",
            ConnectionId::from_uuid(conn_a),
            GithubId(5001),
            2,
        )
        .await
        .expect("create tenant in A");

    // Org B (scope_b) must not be able to read org A's tenant by id.
    let read_as_b = tenants
        .get_scoped(scope_b, tenant_a)
        .await
        .expect("scoped get");
    assert!(read_as_b.is_none(), "org B must not read org A's tenant");

    // Org B must not be able to modify org A's tenant.
    let updated = tenants
        .set_observed_state(scope_b, tenant_a, "running")
        .await
        .expect("scoped update");
    assert!(!updated, "org B must not modify org A's tenant");

    // Org A must still be able to read its own tenant and the state is intact.
    let read_as_a = tenants
        .get_scoped(scope_a, tenant_a)
        .await
        .expect("scoped get A");
    assert!(read_as_a.is_some(), "org A reads its own tenant");
    assert_eq!(read_as_a.unwrap().observed_state, "provisioning");

    // Organization repository: org B cannot fetch org A's organization.
    let orgs = OrganizationsRepository::new(pool.clone());
    let org_a_as_b = orgs
        .get_scoped(scope_b, OrganizationId::from_uuid(org_a))
        .await
        .expect("scoped org get");
    assert!(
        org_a_as_b.is_none(),
        "org B must not read org A's organization"
    );

    // Org B cannot set org A's status.
    let status_ok = orgs
        .set_status(scope_b, OrganizationId::from_uuid(org_a), "suspended")
        .await
        .expect("scoped status update");
    assert!(!status_ok, "org B must not modify org A's organization");
}

// ---------------------------------------------------------------------------
// 5. Operation leasing
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore = "requires a live PostgreSQL (see OPENFLOWS_TEST_DATABASE_URL); run via scripts/run-integration-tests.sh"]
async fn concurrent_workers_cannot_claim_the_same_active_lease() {
    let db = TestDb::new().await.expect("create test schema");
    db.migrate().await.expect("migrate");
    let pool = db.pool();

    let owner = insert_user(pool).await.expect("user");
    let org = insert_organization_with_owner(pool, "lease-org", owner)
        .await
        .expect("org");

    let op_id = operations::create(
        pool,
        &operations::NewOperation {
            organization_id: OrganizationId::from_uuid(org),
            resource_type: "tenant".to_string(),
            resource_id: None,
            kind: "provision".to_string(),
            idempotency_ref: None,
        },
    )
    .await
    .expect("create operation");

    // Spawn many concurrent workers racing to claim the single operation.
    let mut handles = Vec::new();
    for i in 0..16 {
        let pool = pool.clone();
        let owner = format!("worker-{i}");
        handles.push(tokio::spawn(async move {
            operations::claim(&pool, &owner).await.expect("claim")
        }));
    }
    let results: Vec<Option<operations::LeaseGuard>> = futures::future::join_all(handles)
        .await
        .into_iter()
        .collect::<Result<_, _>>()
        .expect("join");

    let claimed: Vec<_> = results.into_iter().flatten().collect();
    assert_eq!(
        claimed.len(),
        1,
        "exactly one worker may claim the operation"
    );

    // The lone claimant is the active lease owner.
    let claimant = &claimed[0];
    assert_eq!(claimant.operation_id, op_id);
    assert!(
        operations::is_leased_to(pool, op_id, claimant.owner())
            .await
            .expect("is leased"),
        "the single claimant must own the live lease"
    );

    // The claimant completes the operation.
    assert!(
        claimant.complete(None).await.expect("complete"),
        "owner can complete"
    );
    assert!(
        operations::get_scoped(pool, OrganizationId::from_uuid(org), op_id)
            .await
            .expect("get op")
            .unwrap()
            .state
            == "succeeded"
    );
}

#[tokio::test]
#[ignore = "requires a live PostgreSQL (see OPENFLOWS_TEST_DATABASE_URL); run via scripts/run-integration-tests.sh"]
async fn expired_leases_are_recoverable_and_stale_worker_cannot_complete_reclaimed() {
    let db = TestDb::new().await.expect("create test schema");
    db.migrate().await.expect("migrate");
    let pool = db.pool();

    let owner = insert_user(pool).await.expect("user");
    let org = insert_organization_with_owner(pool, "lease-org-2", owner)
        .await
        .expect("org");
    let org_id = OrganizationId::from_uuid(org);

    let op_id = operations::create(
        pool,
        &operations::NewOperation {
            organization_id: org_id,
            resource_type: "tenant".to_string(),
            resource_id: None,
            kind: "provision".to_string(),
            idempotency_ref: None,
        },
    )
    .await
    .expect("create op");

    // Worker A claims.
    let guard_a = operations::claim(pool, "worker-a")
        .await
        .expect("claim A")
        .unwrap();
    assert!(guard_a.heartbeat().await.expect("heartbeat A"));

    // Force the lease to expire.
    sqlx::query("UPDATE operations SET lease_expires_at = $1 WHERE id = $2")
        .bind(Utc::now() - Duration::seconds(5))
        .bind(op_id.0)
        .execute(pool)
        .await
        .expect("expire lease");

    // Worker B can now reclaim the expired lease.
    let guard_b = operations::claim(pool, "worker-a")
        .await
        .expect("claim B")
        .unwrap();
    assert_eq!(guard_b.operation_id, op_id);
    assert!(operations::is_leased_to(pool, op_id, "worker-a")
        .await
        .expect("B owns lease"));

    // Worker A is now stale: it can no longer heartbeat, complete, or overwrite
    // the reclaimed operation.
    assert!(
        !guard_a.heartbeat().await.expect("stale heartbeat"),
        "stale worker must not extend the reclaimed lease"
    );
    assert!(
        !guard_a.complete(None).await.expect("stale complete"),
        "stale worker must not complete a reclaimed operation"
    );
    assert!(!guard_a.fail(true, Some("stale")).await.unwrap());
    assert!(
        !guard_a
            .set_step("reclaimed-step")
            .await
            .expect("stale step"),
        "stale worker must not advance a reclaimed operation"
    );

    // Reusing a worker name must not revive the old guard. Retry once more
    // and check that each claim contributes to the attempt counter.
    assert!(guard_b.fail(true, Some("retry")).await.unwrap());
    sqlx::query("UPDATE operations SET retry_at = now() - interval '1 second' WHERE id = $1")
        .bind(op_id.0)
        .execute(pool)
        .await
        .unwrap();
    let guard_c = operations::claim(pool, "worker-a").await.unwrap().unwrap();
    assert!(!guard_b.complete(None).await.unwrap());
    // The newest claimant completes successfully.
    assert!(guard_c.complete(None).await.expect("C completes"));
    let rec = operations::get_scoped(pool, org_id, op_id)
        .await
        .expect("get op")
        .unwrap();
    assert_eq!(rec.state, "succeeded");
    assert_eq!(rec.attempt_count, 3);
    assert!(rec.current_step.is_none(), "stale step write was rejected");
}

// ---------------------------------------------------------------------------
// 6. Idempotency
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore = "requires a live PostgreSQL (see OPENFLOWS_TEST_DATABASE_URL); run via scripts/run-integration-tests.sh"]
async fn idempotent_retries_do_not_duplicate_and_conflict_reuse_is_rejected() {
    let db = TestDb::new().await.expect("create test schema");
    db.migrate().await.expect("migrate");
    let pool = db.pool();

    let owner = insert_user(pool).await.expect("user");
    let org = insert_organization_with_owner(pool, "idem-org", owner)
        .await
        .expect("org");
    let idem = IdempotencyService::new(pool.clone());

    let req = IdempotencyRequest {
        actor_id: UserId::from_uuid(owner),
        organization_id: OrganizationId::from_uuid(org),
        route: "/api/v1/organizations/idem-org/tenants".to_string(),
        key: "idem-key-1".to_string(),
        request_hash: IdempotencyRequest::hash_request(&json!({"name": "x"})).unwrap(),
    };

    // First attempt is new; the caller performs the mutation once.
    assert!(matches!(
        idem.execute(&req, |_| Box::pin(async { Ok("tenant:123".into()) }))
            .await
            .expect("first"),
        IdempotencyOutcome::New { .. }
    ));

    // A retry with the identical body returns the original result, not a
    // duplicate.
    let retry = idem
        .execute(&req, |_| Box::pin(async { panic!("must replay") }))
        .await
        .expect("retry");
    match retry {
        IdempotencyOutcome::Replay { response_reference } => {
            assert_eq!(response_reference, "tenant:123");
        }
        other => panic!("expected Replay, got {other:?}"),
    }

    // A conflicting reuse (same key, different body) is rejected with a 409.
    let conflicting = IdempotencyRequest {
        request_hash: IdempotencyRequest::hash_request(&json!({"name": "y"})).unwrap(),
        ..req.clone()
    };
    assert!(
        matches!(
            idem.execute(&conflicting, |_| Box::pin(async {
                panic!("must conflict")
            }))
            .await,
            Err(ManagerError::Conflict(_))
        ),
        "same key with different body must be rejected"
    );
}

// ---------------------------------------------------------------------------
// 7. Pagination
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore = "requires a live PostgreSQL (see OPENFLOWS_TEST_DATABASE_URL); run via scripts/run-integration-tests.sh"]
async fn pagination_is_stable_and_preserves_organization_scope() {
    let db = TestDb::new().await.expect("create test schema");
    db.migrate().await.expect("migrate");
    let pool = db.pool();

    let owner_a = insert_user(pool).await.expect("user A");
    let owner_b = insert_user(pool).await.expect("user B");
    let org_a = insert_organization_with_owner(pool, "page-a", owner_a)
        .await
        .expect("org A");
    let org_b = insert_organization_with_owner(pool, "page-b", owner_b)
        .await
        .expect("org B");

    let scope_a = OrgScope::new(OrganizationId::from_uuid(org_a));
    let scope_b = OrgScope::new(OrganizationId::from_uuid(org_b));
    let tenants = TenantsRepository::new(pool.clone());

    let conn_a = insert_connection(pool, org_a, 8001).await.expect("conn A");
    let conn_b = insert_connection(pool, org_b, 8002).await.expect("conn B");

    // Org A: 3 tenants; Org B: 2 tenants.
    for i in 1..=3 {
        insert_repository(pool, org_a, conn_a, 6000 + i, &format!("page-a/repo{i}"))
            .await
            .expect("repo A");
        tenants
            .create(
                scope_a,
                &format!("a{i}"),
                ConnectionId::from_uuid(conn_a),
                GithubId(6000 + i),
                1,
            )
            .await
            .expect("tenant A");
    }
    for i in 1..=2 {
        insert_repository(pool, org_b, conn_b, 7000 + i, &format!("page-b/repo{i}"))
            .await
            .expect("repo B");
        tenants
            .create(
                scope_b,
                &format!("b{i}"),
                ConnectionId::from_uuid(conn_b),
                GithubId(7000 + i),
                1,
            )
            .await
            .expect("tenant B");
    }

    // Page through org A with limit 2: expect [a1, a2] then [a3] and no more.
    let page1 = tenants
        .list_scoped(scope_a, PageLimit::new(Some(2)), None)
        .await
        .expect("page1");
    assert_eq!(page1.items.len(), 2);
    let next = page1.next_cursor.clone().expect("next cursor");
    let cursor = openflows_manager::pagination::Cursor::decode(&next).expect("decode cursor");

    let page2 = tenants
        .list_scoped(scope_a, PageLimit::new(Some(2)), Some(&cursor))
        .await
        .expect("page2");
    assert_eq!(page2.items.len(), 1, "second page has the final tenant");
    assert!(page2.next_cursor.is_none(), "no further pages");

    // Stable ordering: concatenate and verify sorted by (created_at, id) and
    // that no tenant from org B leaks in.
    let mut all: Vec<String> = page1.items.iter().map(|t| t.slug.clone()).collect();
    all.extend(page2.items.iter().map(|t| t.slug.clone()));
    assert_eq!(
        all,
        vec!["a1".to_string(), "a2".to_string(), "a3".to_string()]
    );

    // Org B's pagination returns only org B tenants.
    let page_b = tenants
        .list_scoped(scope_b, PageLimit::new(Some(10)), None)
        .await
        .expect("page B");
    assert_eq!(page_b.items.len(), 2);
    assert!(page_b.items.iter().all(|t| t.organization_id == org_b));
}

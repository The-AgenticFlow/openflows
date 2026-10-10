//! PostgreSQL integration tests for WP-02 concurrency and invariants: OAuth
//! callback single consumption, last-admin preservation, owner-transfer
//! requirement, invitation single-use/wrong-identity, org-creation rollback,
//! idempotent retries, recoverable provisioning, and secret hashing.
//!
//! These are `#[ignore]`d and run by `scripts/run-integration-tests.sh` in CI.

mod common;

use common::*;
use openflows_manager::auth::repository::AuthTransactionRepository;
use openflows_manager::db::tx::OrgScope;
use openflows_manager::dto::{MembershipRole, MembershipStatus};
use openflows_manager::id::{OrganizationId, UserId};
use openflows_manager::organizations::service::OrganizationService;
use openflows_manager::organizations::OrganizationRepository;
use openflows_manager::repositories::OrganizationsRepository;

fn user(id: u64) -> UserId {
    // A deterministic user id (valid uuid).
    UserId::from_uuid(uuid::Uuid::from_u128(id as u128))
}

async fn ensure_user(pool: &sqlx::PgPool, id: u64, github: i64, login: &str) -> UserId {
    let uid = user(id);
    sqlx::query("INSERT INTO users (id, display_name, status) VALUES ($1,$2,'active') ON CONFLICT (id) DO NOTHING")
        .bind(uid.0)
        .bind(login)
        .execute(pool)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO identities (id, user_id, provider, subject, login_snapshot) VALUES ($1,$2,'github',$3,$4) ON CONFLICT (provider, subject) DO NOTHING",
    )
    .bind(uuid::Uuid::new_v4())
    .bind(uid.0)
    .bind(github.to_string())
    .bind(login)
    .execute(pool)
    .await
    .unwrap();
    uid
}

async fn add_member(
    pool: &sqlx::PgPool,
    org: OrganizationId,
    uid: UserId,
    role: &str,
    status: &str,
) {
    sqlx::query(
        "INSERT INTO memberships (organization_id, user_id, role, status) VALUES ($1,$2,$3,$4)
         ON CONFLICT (organization_id, user_id) DO UPDATE SET role=$3, status=$4",
    )
    .bind(org.0)
    .bind(uid.0)
    .bind(role)
    .bind(status)
    .execute(pool)
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires isolated PostgreSQL"]
async fn concurrent_oauth_callback_consumption_creates_at_most_one_session() {
    let db = TestDb::new().await.unwrap();
    db.migrate().await.unwrap();
    let pool = db.pool();
    let tx_repo = AuthTransactionRepository::new(pool.clone());

    // Create one login transaction.
    let state_hash = "deadbeef".to_string();
    let verifier = b"encrypted-verifier";
    let tx_id = tx_repo
        .create_login(&state_hash, verifier, 1)
        .await
        .unwrap();

    // Two concurrent consumers of the same state.
    let mut handles = Vec::new();
    for _ in 0..2 {
        let repo = tx_repo.clone();
        let state_hash = state_hash.clone();
        handles.push(tokio::spawn(async move {
            repo.consume_login(&state_hash).await.unwrap()
        }));
    }
    let results: Vec<Option<(uuid::Uuid, Vec<u8>, i32)>> = futures::future::join_all(handles)
        .await
        .into_iter()
        .collect::<Result<_, _>>()
        .unwrap();
    let successes = results.into_iter().filter(|r| r.is_some()).count();
    assert_eq!(
        successes, 1,
        "exactly one concurrent callback may consume the transaction"
    );
    let _ = tx_id;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires isolated PostgreSQL"]
async fn concurrent_admin_demotions_preserve_an_active_admin() {
    let db = TestDb::new().await.unwrap();
    db.migrate().await.unwrap();
    let pool = db.pool();
    let owner = ensure_user(pool, 1, 100, "alice").await;
    let admin2 = ensure_user(pool, 2, 101, "bob").await;
    let org = insert_organization_with_owner(pool, "admins", owner.0)
        .await
        .unwrap();
    let org_id = OrganizationId::from_uuid(org);
    add_member(pool, org_id, admin2, "admin", "active").await;

    let repo = OrganizationRepository::new(pool.clone());
    let scope = OrgScope::new(org_id);

    // Repeated concurrent demotions of one admin converge while a second
    // admin remains. The review regressions separately race two distinct admins.
    let mut handles = Vec::new();
    for _ in 0..8 {
        let repo = repo.clone();
        handles.push(tokio::spawn(async move {
            repo.update_member(
                scope,
                admin2,
                MembershipRole::Developer,
                MembershipStatus::Active,
            )
            .await
        }));
    }
    let results: Vec<_> = futures::future::join_all(handles)
        .await
        .into_iter()
        .collect::<Result<_, _>>()
        .unwrap();
    // All should succeed (admin2 was not the last admin; the owner remains),
    // and the owner must still be an active admin.
    for r in &results {
        assert!(r.is_ok(), "demotion of non-owner admin succeeds");
    }
    let owner_admin: (i64,) = sqlx::query_as(
        "SELECT count(*) FROM memberships WHERE organization_id=$1 AND user_id=$2 AND role='admin' AND status='active'",
    )
    .bind(org)
    .bind(owner.0)
    .fetch_one(pool)
    .await
    .unwrap();
    assert_eq!(owner_admin.0, 1, "owner remains active admin");

    // Demoting the remaining admin must fail, regardless of ownership.
    let demote_owner = repo
        .update_member(
            scope,
            owner,
            MembershipRole::Developer,
            MembershipStatus::Active,
        )
        .await;
    assert!(
        demote_owner.is_err(),
        "demoting the last remaining admin owner must fail"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires isolated PostgreSQL"]
async fn owner_removal_or_suspension_requires_ownership_transfer() {
    let db = TestDb::new().await.unwrap();
    db.migrate().await.unwrap();
    let pool = db.pool();
    let owner = ensure_user(pool, 1, 100, "alice").await;
    let admin2 = ensure_user(pool, 2, 101, "bob").await;
    let org = insert_organization_with_owner(pool, "transfer", owner.0)
        .await
        .unwrap();
    let org_id = OrganizationId::from_uuid(org);
    add_member(pool, org_id, admin2, "admin", "active").await;
    let repo = OrganizationRepository::new(pool.clone());
    let scope = OrgScope::new(org_id);

    // Removing or suspending the owner must fail.
    assert!(repo.remove_member(scope, owner).await.is_err());
    assert!(
        repo.update_member(
            scope,
            owner,
            MembershipRole::Admin,
            MembershipStatus::Suspended,
        )
        .await
        .is_err(),
        "suspending the owner requires transfer"
    );

    // Transfer ownership to admin2 (active member) succeeds.
    repo.transfer_ownership(scope, admin2).await.unwrap();

    // Now the old owner can be removed (no longer the owner).
    repo.remove_member(scope, owner).await.unwrap();
    let old_role: (String,) =
        sqlx::query_as("SELECT status FROM memberships WHERE organization_id=$1 AND user_id=$2")
            .bind(org)
            .bind(owner.0)
            .fetch_one(pool)
            .await
            .unwrap();
    assert_eq!(old_role.0, "removed");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires isolated PostgreSQL"]
async fn invitation_wrong_invitee_expired_revoked_and_concurrent_acceptance() {
    let db = TestDb::new().await.unwrap();
    db.migrate().await.unwrap();
    let pool = db.pool();
    let owner = ensure_user(pool, 1, 100, "alice").await;
    let bob = ensure_user(pool, 2, 101, "bob").await;
    let carol = ensure_user(pool, 3, 102, "carol").await;
    let org = insert_organization_with_owner(pool, "invites", owner.0)
        .await
        .unwrap();
    let org_id = OrganizationId::from_uuid(org);
    let repo = OrganizationRepository::new(pool.clone());
    let scope = OrgScope::new(org_id);

    // Invite bob (github id 101).
    let inv_id = repo
        .create_invitation(scope, 101, MembershipRole::Viewer, "token-hash-bob", owner)
        .await
        .unwrap();

    // Wrong identity (carol, github 102) cannot accept.
    assert!(
        repo.accept_invitation(inv_id, 102, carol).await.is_err(),
        "wrong invitee rejected"
    );

    // Correct identity (bob) accepts.
    repo.accept_invitation(inv_id, 101, bob).await.unwrap();

    // Concurrent acceptance of the SAME (now accepted) invitation: both fail or
    // at most one succeeds, and bob's membership is added exactly once.
    let dave = ensure_user(pool, 4, 103, "dave").await;
    let inv2 = repo
        .create_invitation(scope, 103, MembershipRole::Viewer, "token-hash-dave", owner)
        .await
        .unwrap();
    let mut handles = Vec::new();
    for _ in 0..4 {
        let repo = repo.clone();
        handles.push(tokio::spawn(async move {
            repo.accept_invitation(inv2, 103, dave).await
        }));
    }
    let results = futures::future::join_all(handles).await;
    let ok = results.iter().filter(|r| matches!(r, Ok(Ok(())))).count();
    // Diagnostic: what is the DB state after the concurrent accepts?
    let consumed: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM invitations WHERE id=$1 AND accepted_at IS NOT NULL",
    )
    .bind(inv2.0)
    .fetch_one(pool)
    .await
    .unwrap();
    assert_eq!(ok, 1, "exactly one concurrent acceptance succeeds");
    assert_eq!(consumed, 1, "invitation accepted exactly once");

    // Expired/revoked invitation fails.
    let inv3 = repo
        .create_invitation(
            scope,
            102,
            MembershipRole::Viewer,
            "token-hash-carol",
            owner,
        )
        .await
        .unwrap();
    sqlx::query("UPDATE invitations SET expires_at = now() - interval '1 second' WHERE id=$1")
        .bind(inv3.0)
        .execute(pool)
        .await
        .unwrap();
    assert!(
        repo.accept_invitation(inv3, 102, carol).await.is_err(),
        "expired invitation fails"
    );

    let inv4 = repo
        .create_invitation(
            scope,
            102,
            MembershipRole::Viewer,
            "token-hash-carol2",
            owner,
        )
        .await
        .unwrap();
    sqlx::query("UPDATE invitations SET revoked_at = now() WHERE id=$1")
        .bind(inv4.0)
        .execute(pool)
        .await
        .unwrap();
    assert!(
        repo.accept_invitation(inv4, 102, carol).await.is_err(),
        "revoked invitation fails"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires isolated PostgreSQL"]
async fn org_creation_rolls_back_fully_on_failure() {
    let db = TestDb::new().await.unwrap();
    db.migrate().await.unwrap();
    let pool = db.pool();
    let owner = ensure_user(pool, 1, 100, "alice").await;

    let orgs = OrganizationsRepository::new(pool.clone());
    let repo = OrganizationRepository::new(pool.clone());
    let service = OrganizationService::new(
        pool.clone(),
        repo,
        orgs,
        openflows_manager::idempotency::IdempotencyService::new(pool.clone()),
    );

    // Pre-insert an org with the same slug to force a uniqueness failure.
    insert_organization_with_owner(pool, "rollback", owner.0)
        .await
        .unwrap();

    // The create_org mutation must fail (slug conflict) and the whole
    // transaction (including the idempotency key) must roll back.
    let before_idem: i64 = sqlx::query_scalar("SELECT count(*) FROM idempotency_keys")
        .fetch_one(pool)
        .await
        .unwrap();
    assert_eq!(before_idem, 0);

    let err = service
        .create_org(owner, "rollback", "Rollback", "key-rollback", "req")
        .await;
    assert!(err.is_err(), "duplicate slug must fail");

    // No partial state: no new idempotency key, no operation, no audit.
    let after_idem: i64 = sqlx::query_scalar("SELECT count(*) FROM idempotency_keys")
        .fetch_one(pool)
        .await
        .unwrap();
    let ops: i64 = sqlx::query_scalar("SELECT count(*) FROM operations")
        .fetch_one(pool)
        .await
        .unwrap();
    let audits: i64 = sqlx::query_scalar("SELECT count(*) FROM audit_events")
        .fetch_one(pool)
        .await
        .unwrap();
    assert_eq!(after_idem, 0, "idempotency key rolled back");
    assert_eq!(ops, 0, "no operation persisted on failed creation");
    assert_eq!(audits, 0, "no audit persisted on failed creation");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires isolated PostgreSQL"]
async fn idempotent_org_retries_do_not_duplicate() {
    let db = TestDb::new().await.unwrap();
    db.migrate().await.unwrap();
    let pool = db.pool();
    let owner = ensure_user(pool, 1, 100, "alice").await;

    let orgs = OrganizationsRepository::new(pool.clone());
    let repo = OrganizationRepository::new(pool.clone());
    let service = OrganizationService::new(
        pool.clone(),
        repo,
        orgs,
        openflows_manager::idempotency::IdempotencyService::new(pool.clone()),
    );

    // Create twice with the same idempotency key.
    let r1 = service
        .create_org(owner, "idemorg", "Idem", "same-key", "req")
        .await
        .unwrap();
    let r2 = service
        .create_org(owner, "idemorg", "Idem", "same-key", "req")
        .await
        .unwrap();
    assert_eq!(r1.resource_id, r2.resource_id, "same org returned");

    // Exactly one organization and one operation.
    let org_count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM organizations WHERE slug='idemorg'")
            .fetch_one(pool)
            .await
            .unwrap();
    let op_count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM operations o JOIN organizations g ON g.id=o.organization_id WHERE g.slug='idemorg'",
    )
    .fetch_one(pool)
    .await
    .unwrap();
    assert_eq!(org_count, 1, "no duplicate organizations");
    assert_eq!(op_count, 1, "no duplicate operations");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires isolated PostgreSQL"]
async fn provisioning_remains_recoverable_without_live_coder() {
    let db = TestDb::new().await.unwrap();
    db.migrate().await.unwrap();
    let pool = db.pool();
    let owner = ensure_user(pool, 1, 100, "alice").await;

    let orgs = OrganizationsRepository::new(pool.clone());
    let repo = OrganizationRepository::new(pool.clone());
    let service = OrganizationService::new(
        pool.clone(),
        repo,
        orgs,
        openflows_manager::idempotency::IdempotencyService::new(pool.clone()),
    );

    let created = service
        .create_org(owner, "recover", "Recover", "key", "req")
        .await
        .unwrap();

    // The org is provisioning with no coder id assigned, and a queued
    // org.provision operation exists that a worker can claim later.
    let row: (String, Option<String>) =
        sqlx::query_as("SELECT status, coder_organization_id FROM organizations WHERE id=$1")
            .bind(created.resource_id.0)
            .fetch_one(pool)
            .await
            .unwrap();
    assert_eq!(row.0, "provisioning");
    assert!(row.1.is_none(), "no Coder org assigned synchronously");

    let op: (String, String) = sqlx::query_as("SELECT kind, state FROM operations WHERE id=$1")
        .bind(created.operation_id.0)
        .fetch_one(pool)
        .await
        .unwrap();
    assert_eq!(op.0, "org.provision");
    assert_eq!(op.1, "queued");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires isolated PostgreSQL"]
async fn secrets_are_stored_hashed_or_encrypted_not_plaintext() {
    let db = TestDb::new().await.unwrap();
    db.migrate().await.unwrap();
    let pool = db.pool();
    let user = ensure_user(pool, 1, 100, "alice").await;

    // A session's stored access_hash must not equal the raw token, and is a
    // SHA-256 hex digest.
    let sessions = openflows_manager::auth::repository::SessionsRepository::new(pool.clone());
    let creds = sessions.create_cli_session(user).await.unwrap();
    let stored_hash: String = sqlx::query_scalar("SELECT access_hash FROM sessions WHERE id=$1")
        .bind(creds.session_id.0)
        .fetch_one(pool)
        .await
        .unwrap();
    assert_ne!(stored_hash, creds.access_token, "access token is hashed");
    assert_eq!(stored_hash.len(), 64, "SHA-256 hex digest");

    // An OAuth login transaction stores the PKCE verifier encrypted, never the
    // plaintext verifier, and records the key version alongside.
    let tx_repo = AuthTransactionRepository::new(pool.clone());
    let raw_verifier = b"the-plaintext-pkce-verifier-must-not-be-stored";
    // The service encrypts the verifier with the envelope cipher before the
    // repository persists it; mirror that here.
    let envelope = test_auth_config("http://127.0.0.1:3002")
        .envelope_cipher("oauth_pkce")
        .unwrap();
    let encrypted_verifier = envelope.seal(raw_verifier).unwrap();
    let tx_id = tx_repo
        .create_login(
            "state-hash-xyz",
            &encrypted_verifier,
            envelope.version() as i32,
        )
        .await
        .unwrap();
    let (encrypted, version): (Option<Vec<u8>>, Option<i32>) = sqlx::query_as(
        "SELECT encrypted_pkce_verifier, pkce_key_version FROM auth_transactions WHERE id=$1",
    )
    .bind(tx_id)
    .fetch_one(pool)
    .await
    .unwrap();
    let encrypted = encrypted.expect("encrypted verifier stored");
    assert_ne!(
        &encrypted, raw_verifier,
        "PKCE verifier is encrypted, not plaintext"
    );
    assert_eq!(version, Some(1), "key version stored alongside ciphertext");
}

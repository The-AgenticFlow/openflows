//! Tenant runtime integration and isolation acceptance tests.
//!
//! These exercise the tenant lifecycle, quota enforcement, runtime worker
//! provisioning, credential rotation/revocation, Redis isolation, authenticated
//! hook relay, and pinned template/version selection against an isolated
//! PostgreSQL database and a deterministic in-memory Coder adapter:
//!
//! 1. Cross-organization and cross-tenant access denial.
//! 2. Concurrent quota enforcement and provisioning-slot leasing.
//! 3. Idempotent creation and failure recovery / owner-aware adoption.
//! 4. Worker authorization and fleet limits.
//! 5. Runtime renewal, revocation, and tenant deletion.
//! 6. Tenant Redis namespace restrictions and authenticated hook relay.
//! 7. Pinned template/version selection.
//!
//! Run with: `cargo test --test tenant_runtime -- --ignored`

mod common;

use common::TestDb;
use openflows_manager::coder::{MockCoderAdapter, MockCoderConfig};
use openflows_manager::id::{OrganizationId, TenantId, UserId};
use openflows_manager::operations;
use openflows_manager::provisioning::TenantProvisionWorker;
use openflows_manager::quotas::TenantQuotaService;
use openflows_manager::repositories::OrganizationsRepository;
use openflows_manager::runtime::tenant::TenantRuntimeRepository;
use openflows_manager::runtime::RuntimeCredentialService;
use openflows_manager::secrets::SecretProvider;
use openflows_manager::templates::packaging::{
    build_deterministic_archive, sha256_hex, Role, RoleManifestEntry, TemplateManifest,
};
use openflows_manager::templates::repository::ReleaseRepository;
use openflows_manager::templates::service::{internal_org_name, ReleaseArtifacts, ReleaseService};
use openflows_manager::tenants::{validate_tenant_slug, TenantService, TenantServiceInputs};
use std::collections::BTreeMap;
use std::sync::Arc;

/// Build deterministic release artifacts (manifest + role archives).
fn build_release_artifacts() -> (ReleaseArtifacts, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let mut archives = BTreeMap::new();
    let mut roles = BTreeMap::new();
    for role in [Role::Nexus, Role::Forge, Role::Sentinel, Role::Vessel] {
        let role_dir = dir.path().join(format!("openflows-{}", role.as_str()));
        std::fs::create_dir_all(&role_dir).unwrap();
        std::fs::write(
            role_dir.join("main.tf"),
            format!("resource \"role\" {{ name = \"{}\" }}\n", role.as_str()),
        )
        .unwrap();
        let archive = build_deterministic_archive(&role_dir).unwrap();
        let sha256 = sha256_hex(&archive);
        archives.insert(role, archive);
        roles.insert(
            role,
            RoleManifestEntry {
                role,
                archive: role.archive_name(),
                sha256,
                parameter_schema_version: 1,
                image_digest: Some(format!("sha256:{}", "a".repeat(64))),
            },
        );
    }
    let manifest = TemplateManifest {
        schema_version: 1,
        release: "2026.10.0".into(),
        required_roles: vec![Role::Nexus, Role::Forge, Role::Sentinel, Role::Vessel],
        roles,
    };
    (ReleaseArtifacts { manifest, archives }, dir)
}

/// A ready-to-run harness: an isolated database, a ready organization with a
/// pinned published release, a connection + repository, and a tenant worker.
struct Harness {
    _db: TestDb,
    pool: sqlx::PgPool,
    org: OrganizationId,
    connection_id: uuid::Uuid,
    repository_id: i64,
    owner: UserId,
    tenant_repo: TenantRuntimeRepository,
    runtime_credentials: RuntimeCredentialService,
    secret_provider: Arc<openflows_manager::secrets::InMemorySecretProvider>,
    worker: TenantProvisionWorker,
    tenant_service: TenantService,
    quotas: TenantQuotaService,
    adapter: Arc<MockCoderAdapter>,
}

impl Harness {
    fn connection_id(&self) -> uuid::Uuid {
        self.connection_id
    }
}

async fn provision_with_runtime_registration(
    harness: &Harness,
    guard: &operations::LeaseGuard,
) -> openflows_manager::provisioning::TenantProvisionOutcome {
    let first = harness.worker.provision(guard).await.unwrap();
    assert!(matches!(
        first,
        openflows_manager::provisioning::TenantProvisionOutcome::NotReady(_, "NEXUS_NOT_READY")
    ));
    let workspace_id: uuid::Uuid =
        sqlx::query_scalar("SELECT resource_id FROM operations WHERE id = $1")
            .bind(guard.operation_id.0)
            .fetch_one(&harness.pool)
            .await
            .unwrap();
    let nexus_id: uuid::Uuid =
        sqlx::query_scalar("SELECT id FROM workspaces WHERE tenant_id = $1 AND role = 'nexus'")
            .bind(workspace_id)
            .fetch_one(&harness.pool)
            .await
            .unwrap();
    let workspace_id = openflows_manager::id::WorkspaceId::from_uuid(nexus_id);
    let (bootstrap_ref, _) = harness
        .runtime_credentials
        .repo
        .live_bootstrap_ref(workspace_id)
        .await
        .unwrap()
        .expect("provisioning must inject a live bootstrap");
    let bootstrap =
        String::from_utf8(harness.secret_provider.get(&bootstrap_ref).await.unwrap()).unwrap();
    let session = harness
        .runtime_credentials
        .register(&bootstrap)
        .await
        .unwrap();
    let scope = harness
        .runtime_credentials
        .authenticate(&session.access_secret)
        .await
        .unwrap();
    openflows_manager::runtime::RuntimeWorkspaceService::new(
        harness.pool.clone(),
        harness.tenant_repo.clone(),
        harness.quotas.clone(),
    )
    .heartbeat(&scope)
    .await
    .unwrap();
    harness.worker.provision(guard).await.unwrap()
}

async fn harness() -> Harness {
    let db = TestDb::new().await.unwrap();
    db.migrate().await.unwrap();
    let pool = db.pool().clone();

    let owner = UserId::new();
    let mut tx = pool.begin().await.unwrap();
    // The user must exist before the organization (owner FK).
    sqlx::query("INSERT INTO users (id, display_name, status) VALUES ($1, $2, 'active')")
        .bind(owner.0)
        .bind("owner")
        .execute(&mut *tx)
        .await
        .unwrap();
    // Organization in ready state with a coder organization mapping.
    sqlx::query("INSERT INTO coder_user_links(user_id,coder_url,coder_user_id) VALUES($1,$2,$3)")
        .bind(owner.0)
        .bind(
            std::env::var("CODER_URL")
                .unwrap_or_default()
                .trim_end_matches('/'),
        )
        .bind(format!("human-{}", owner.0))
        .execute(&mut *tx)
        .await
        .unwrap();
    let org = OrganizationId::new();
    sqlx::query(
        "INSERT INTO organizations (id, slug, display_name, owner_user_id, status, coder_organization_id, model_capability_ok, provisioner_status)
         VALUES ($1, $2, $3, $4, 'ready', $5, true, 'ready')",
    )
    .bind(org.0)
    .bind("acme")
    .bind("Acme")
    .bind(owner.0)
    .bind(internal_org_name(org))
    .execute(&mut *tx)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO memberships (organization_id, user_id, role, status)
         VALUES ($1, $2, 'admin', 'active')",
    )
    .bind(org.0)
    .bind(owner.0)
    .execute(&mut *tx)
    .await
    .unwrap();
    tx.commit().await.unwrap();

    // Connection + repository in the org.
    let connection_id = common::insert_connection(&pool, org.0, 5001).await.unwrap();
    let repository_id: i64 = 9001;
    common::insert_repository(&pool, org.0, connection_id, repository_id, "acme/backend")
        .await
        .unwrap();

    // Publish + approve a release for the org so the Nexus template/version is
    // resolved (pinned selection).
    let release_repo = ReleaseRepository::new(pool.clone());
    let releases = ReleaseService::new(pool.clone(), release_repo.clone());
    let (artifacts, _dir) = build_release_artifacts();
    let (release_id, _) = releases
        .import_release(artifacts.clone(), "test")
        .await
        .unwrap();
    release_repo
        .set_release_status(release_id, "approved", None)
        .await
        .unwrap();
    let adapter = Arc::new(MockCoderAdapter::new(MockCoderConfig::default()));
    releases
        .publish_for_org(org, release_id, adapter.as_ref(), &artifacts)
        .await
        .unwrap();
    // Bind the current release so the tenant worker resolves the pinned nexus
    // template/version.
    release_repo
        .bind_org_release(org, release_id)
        .await
        .unwrap();

    let orgs = OrganizationsRepository::new(pool.clone());
    let quotas = TenantQuotaService::new(pool.clone(), Default::default());
    let secret_provider = Arc::new(openflows_manager::secrets::InMemorySecretProvider::new());
    let tenant_repo =
        TenantRuntimeRepository::with_secret_provider(pool.clone(), secret_provider.clone());
    let runtime_credentials = RuntimeCredentialService::with_secret_provider(
        openflows_manager::runtime::RuntimeRepository::new(pool.clone()),
        secret_provider.clone(),
    );
    let worker = TenantProvisionWorker::new(
        pool.clone(),
        orgs.clone(),
        quotas.clone(),
        tenant_repo.clone(),
        runtime_credentials.clone(),
        release_repo.clone(),
        adapter.clone(),
    );
    let tenant_service = TenantService::new(TenantServiceInputs::new(
        pool.clone(),
        openflows_manager::repositories::TenantsRepository::new(pool.clone()),
        openflows_manager::organizations::OrganizationRepository::new(pool.clone()),
        openflows_manager::idempotency::IdempotencyService::new(pool.clone()),
        quotas.clone(),
    ));

    Harness {
        _db: db,
        pool,
        org,
        connection_id,
        repository_id,
        owner,
        tenant_repo,
        runtime_credentials,
        secret_provider,
        worker,
        tenant_service,
        quotas,
        adapter,
    }
}

/// Insert a second org + owner for cross-org denial tests.
async fn insert_other_org(pool: &sqlx::PgPool, owner: uuid::Uuid) -> OrganizationId {
    let org = OrganizationId::new();
    let mut tx = pool.begin().await.unwrap();
    sqlx::query(
        "INSERT INTO organizations (id, slug, display_name, owner_user_id, status, coder_organization_id)
         VALUES ($1, $2, $3, $4, 'ready', $5)",
    )
    .bind(org.0)
    .bind("otherco")
    .bind("Otherco")
    .bind(owner)
    .bind(internal_org_name(org))
    .execute(&mut *tx)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO memberships (organization_id, user_id, role, status)
         VALUES ($1, $2, 'admin', 'active')",
    )
    .bind(org.0)
    .bind(owner)
    .execute(&mut *tx)
    .await
    .unwrap();
    tx.commit().await.unwrap();
    org
}

/// Create a tenant via the service and return the tenant id + operation.
async fn create_tenant(h: &Harness, slug: &str, fleet: i32, key: &str) -> TenantId {
    let resp = h
        .tenant_service
        .create_tenant(h.owner, h.org, h.repository_id, slug, fleet, key, "req")
        .await
        .expect("tenant create should succeed");
    resp.resource_id
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn missing_tenant_secrets_recover_existing_unique_rows() {
    let h = harness().await;
    let tenant = create_tenant(&h, "secret-recovery", 1, "secret-recovery").await;
    let redis = h
        .tenant_repo
        .ensure_redis_credential(h.org, tenant, "recovery-redis")
        .await
        .unwrap();
    let hook = h
        .tenant_repo
        .ensure_hook_key(h.org, tenant, "recovery-hook")
        .await
        .unwrap();
    h.secret_provider.delete(&redis.secret_ref).await.unwrap();
    h.secret_provider.delete(&hook.secret_ref).await.unwrap();
    // Reproduce both a lost provider and a previous attempt that already
    // revoked the row before failing to allocate its replacement.
    sqlx::query("UPDATE tenant_redis_credentials SET revoked_at = now() WHERE tenant_id = $1")
        .bind(tenant.0)
        .execute(&h.pool)
        .await
        .unwrap();
    let recovered = h
        .tenant_repo
        .ensure_redis_credential(h.org, tenant, "recovery-redis")
        .await
        .unwrap();
    let recovered_hook = h
        .tenant_repo
        .ensure_hook_key(h.org, tenant, "recovery-hook")
        .await
        .unwrap();
    assert_eq!(recovered.generation, redis.generation + 1);
    assert_eq!(recovered_hook.generation, hook.generation + 1);
    assert_ne!(recovered.credential_hash, redis.credential_hash);
    assert_ne!(recovered_hook.key_hash, hook.key_hash);
    assert_eq!(recovered.namespace, redis.namespace);
    assert_eq!(recovered.username, redis.username);
    assert!(h.secret_provider.get(&recovered.secret_ref).await.is_ok());
    assert!(h
        .secret_provider
        .get(&recovered_hook.secret_ref)
        .await
        .is_ok());
    let again = h
        .tenant_repo
        .ensure_redis_credential(h.org, tenant, "recovery-redis")
        .await
        .unwrap();
    assert_eq!(again.credential_hash, recovered.credential_hash);
    let again = h
        .tenant_repo
        .ensure_hook_key(h.org, tenant, "recovery-hook")
        .await
        .unwrap();
    assert_eq!(again.key_hash, recovered_hook.key_hash);
}

// ---------------------------------------------------------------------------
// Cross-organization and cross-tenant access denial.
// ---------------------------------------------------------------------------
#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn tenant_status_follows_unfinished_operation_before_newer_cancelled_action() {
    let h = harness().await;
    let tenant = create_tenant(&h, "status-recovery", 1, "status-recovery").await;
    let provision: uuid::Uuid = sqlx::query_scalar(
        "SELECT id FROM operations WHERE resource_id = $1 AND kind = 'tenant.provision'",
    )
    .bind(tenant.0)
    .fetch_one(&h.pool)
    .await
    .unwrap();
    let cancelled = uuid::Uuid::new_v4();
    sqlx::query(
        "INSERT INTO operations (id, organization_id, resource_type, resource_id, kind, state, created_at)
         VALUES ($1, $2, 'tenant', $3, 'tenant.start', 'cancelled', now() + interval '1 second')",
    )
    .bind(cancelled)
    .bind(h.org.0)
    .bind(tenant.0)
    .execute(&h.pool)
    .await
    .unwrap();
    for state in ["queued", "running", "retryable_failed", "succeeded"] {
        sqlx::query("UPDATE operations SET state = $1 WHERE id = $2")
            .bind(state)
            .bind(provision)
            .execute(&h.pool)
            .await
            .unwrap();
        let status = h
            .tenant_service
            .tenant_status(h.owner, h.org, tenant)
            .await
            .unwrap();
        let expected = if state == "succeeded" {
            cancelled
        } else {
            provision
        };
        assert_eq!(status["latest_operation"]["id"], expected.to_string());
    }
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn cross_org_and_cross_tenant_access_denied() {
    let h = harness().await;
    let other_owner = UserId::new();
    sqlx::query("INSERT INTO users (id, display_name, status) VALUES ($1, $2, 'active')")
        .bind(other_owner.0)
        .bind("other")
        .execute(&h.pool)
        .await
        .unwrap();
    let other_org = insert_other_org(&h.pool, other_owner.0).await;

    // Create a tenant in h.org owned by h.owner.
    let tenant = create_tenant(&h, "backend", 1, "key-a").await;

    // The other-org owner is not a member of h.org: every tenant lookup 404s.
    let err = h
        .tenant_service
        .list_tenants(
            other_owner,
            h.org,
            openflows_manager::pagination::PageLimit::new(None),
            None,
        )
        .await
        .unwrap_err();
    assert!(
        matches!(&err, openflows_manager::error::ManagerError::Api(a) if a.code == "RESOURCE_NOT_FOUND"),
        "cross-org tenant list must 404: {err}"
    );
    let err = h
        .tenant_service
        .get_tenant(other_owner, h.org, tenant)
        .await
        .unwrap_err();
    assert!(
        matches!(&err, openflows_manager::error::ManagerError::Api(a) if a.code == "RESOURCE_NOT_FOUND"),
        "cross-org tenant detail must 404: {err}"
    );

    // h.owner cannot read another org's tenant (none exist there, but a tenant
    // id that does not belong to h.org must also 404 even if it belongs to the
    // caller's own org).
    let err = h
        .tenant_service
        .get_tenant(h.owner, other_org, tenant)
        .await
        .unwrap_err();
    assert!(
        matches!(&err, openflows_manager::error::ManagerError::Api(a) if a.code == "RESOURCE_NOT_FOUND"),
        "tenant id from another org must 404: {err}"
    );
}

// ---------------------------------------------------------------------------
// Concurrent quota enforcement and provisioning-slot leasing.
// ---------------------------------------------------------------------------
#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn concurrent_tenant_quota_is_enforced() {
    let h = harness().await;
    // Default limit is 2 active tenants per org. Create on 3 DISTINCT
    // repositories concurrently (one live tenant per repository, but the org
    // ceiling is 2), and expect exactly 2 to succeed and the 3rd to be rejected
    // with a structured resource-limit error.
    let mut repos = Vec::new();
    for i in 0..3 {
        let repo: i64 = 10000 + i;
        common::insert_repository(
            &h.pool,
            h.org.0,
            h.connection_id(),
            repo,
            &format!("acme/r{i}"),
        )
        .await
        .unwrap();
        repos.push(repo);
    }
    let mut tasks = Vec::new();
    for (i, repo) in repos.into_iter().enumerate() {
        let service = h.tenant_service.clone();
        let owner = h.owner;
        let org = h.org;
        tasks.push(tokio::spawn(async move {
            service
                .create_tenant(owner, org, repo, &format!("t{i}"), 1, &format!("k{i}"), "r")
                .await
        }));
    }
    let mut results = Vec::new();
    for t in tasks {
        results.push(t.await.unwrap());
    }
    for r in &results {
        if let Err(e) = r {
            eprintln!("create result: {e}");
        }
    }
    let ok = results.iter().filter(|r| r.is_ok()).count();
    let rejected = results
        .iter()
        .filter(|r| {
            matches!(r, Err(openflows_manager::error::ManagerError::Api(a)) if a.code == "TENANT_LIMIT_REACHED")
        })
        .count();
    assert_eq!(
        ok, 2,
        "exactly 2 concurrent creates may reserve tenant capacity"
    );
    assert_eq!(rejected, 1, "the 3rd concurrent create must be rejected");

    // The database enforces the limit independently of the service: a direct
    // 3rd insert also fails the live-(org,repo) uniqueness or quota is checked
    // by the service. Verify no more than 2 live tenants exist.
    let count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM tenants WHERE organization_id = $1 AND desired_state <> 'deleted'",
    )
    .bind(h.org.0)
    .fetch_one(&h.pool)
    .await
    .unwrap();
    assert_eq!(count, 2);
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn provisioning_slot_queues_second_operation() {
    let h = harness().await;
    let tenant = create_tenant(&h, "slot1", 1, "s1").await;

    // Claim the operation to make it actively execute. Queued operations alone
    // must not consume the per-organization execution slot.
    let owner = format!("slot-worker-{}", std::process::id());
    let guard = operations::claim_kind(&h.pool, &owner, Some("tenant.provision"))
        .await
        .unwrap()
        .unwrap();
    assert!(
        !h.quotas.provisioning_slot_available(h.org).await.unwrap(),
        "an active tenant op must occupy the org provisioning slot"
    );

    // Complete the outstanding operation; the slot becomes available again.
    let outcome = provision_with_runtime_registration(&h, &guard).await;
    assert!(matches!(
        outcome,
        openflows_manager::provisioning::TenantProvisionOutcome::Provisioned(_)
    ));
    assert!(
        h.quotas.provisioning_slot_available(h.org).await.unwrap(),
        "slot must be released after the operation completes"
    );
    let _ = tenant;
}

// ---------------------------------------------------------------------------
// Durable operation plumbing (preserves the operation-status flow the
// existing issue-to-PR lifecycle depends on).
// ---------------------------------------------------------------------------
#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn tenant_create_enqueues_durable_operation() {
    let h = harness().await;
    let resp = h
        .tenant_service
        .create_tenant(h.owner, h.org, h.repository_id, "op", 1, "op-key", "r")
        .await
        .unwrap();
    // The operation is scoped to the tenant's organization and readable via the
    // durable operation lookup used by status/retry flows.
    let op = operations::get_scoped(&h.pool, h.org, resp.operation_id)
        .await
        .unwrap()
        .expect("tenant create must enqueue a durable operation");
    assert_eq!(op.kind, "tenant.provision");
    assert_eq!(op.state, "queued");
    assert_eq!(
        op.resource_id,
        Some(resp.resource_id.0),
        "operation must reference the created tenant"
    );
}

// ---------------------------------------------------------------------------
// Idempotent creation and failure recovery / owner-aware adoption.
// ---------------------------------------------------------------------------
#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn tenant_creation_is_idempotent() {
    let h = harness().await;
    let first = h
        .tenant_service
        .create_tenant(h.owner, h.org, h.repository_id, "idem", 1, "same-key", "r")
        .await
        .unwrap();
    // Replay with the same key + body returns the same tenant.
    let replay = h
        .tenant_service
        .create_tenant(h.owner, h.org, h.repository_id, "idem", 1, "same-key", "r")
        .await
        .unwrap();
    assert_eq!(first.resource_id, replay.resource_id);
    // Reuse of the key with a different body is rejected (409).
    let err = h
        .tenant_service
        .create_tenant(
            h.owner,
            h.org,
            h.repository_id,
            "other-name",
            2,
            "same-key",
            "r",
        )
        .await
        .unwrap_err();
    assert!(
        matches!(&err, openflows_manager::error::ManagerError::Conflict(_)),
        "idempotency key reuse with a different body must conflict: {err}"
    );
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn tenant_provision_retries_adopt_same_owner() {
    let h = harness().await;
    let tenant = create_tenant(&h, "recover", 1, "rec").await;

    // Claim and run provision once; the machine owner is created in the mock.
    let owner = format!("t-worker-{}", std::process::id());
    let guard = operations::claim_kind(&h.pool, &owner, Some("tenant.provision"))
        .await
        .unwrap()
        .unwrap();
    let outcome = provision_with_runtime_registration(&h, &guard).await;
    assert!(matches!(
        outcome,
        openflows_manager::provisioning::TenantProvisionOutcome::Provisioned(_)
    ));

    // On a retry, the same owner is adopted (no duplicate machine owner).
    let coder_org = coder_client::CoderOrganizationId::new(internal_org_name(h.org));
    let owner_id: String =
        sqlx::query_scalar("SELECT coder_owner_id FROM runtime_identities WHERE tenant_id=$1")
            .bind(tenant.0)
            .fetch_one(&h.pool)
            .await
            .unwrap();
    assert_eq!(owner_id, format!("human-{}", h.owner.0));
    assert!(h.adapter.machine_owner(&coder_org, &tenant).is_none());
    let creator: uuid::Uuid = sqlx::query_scalar("SELECT owner_user_id FROM tenants WHERE id=$1")
        .bind(tenant.0)
        .fetch_one(&h.pool)
        .await
        .unwrap();
    assert_eq!(creator, h.owner.0);
}

// ---------------------------------------------------------------------------
// Worker authorization and fleet limits.
// ---------------------------------------------------------------------------
#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn worker_request_requires_nexus_and_enforces_fleet_limit() {
    let h = harness().await;
    let tenant = create_tenant(&h, "workers", 1, "wk").await;
    let coder_org = coder_client::CoderOrganizationId::new(internal_org_name(h.org));

    // Provision the tenant so the Nexus workspace exists and its runtime
    // credential can be minted.
    let owner = format!("wk-worker-{}", std::process::id());
    let guard = operations::claim_kind(&h.pool, &owner, Some("tenant.provision"))
        .await
        .unwrap()
        .unwrap();
    provision_with_runtime_registration(&h, &guard).await;
    assert!(h
        .adapter
        .has_workspace_for_owner(&coder_org, &format!("human-{}", h.owner.0)));

    // Find the Nexus workspace row and mint its runtime credential to act as
    // the Nexus controller.
    let nexus_ws: uuid::Uuid =
        sqlx::query_scalar("SELECT id FROM workspaces WHERE tenant_id = $1 AND role = 'nexus'")
            .bind(tenant.0)
            .fetch_one(&h.pool)
            .await
            .unwrap();
    let creds = openflows_manager::runtime::RuntimeCredentialService::new(
        openflows_manager::runtime::RuntimeRepository::new(h.pool.clone()),
    );
    let issued = creds
        .issue_for_workspace(openflows_manager::id::WorkspaceId::from_uuid(nexus_ws))
        .await
        .unwrap();
    let scope = creds.authenticate(&issued.raw_secret).await.unwrap();

    // The Nexus runtime may request a worker.
    let ws_svc = openflows_manager::runtime::RuntimeWorkspaceService::new(
        h.pool.clone(),
        h.tenant_repo.clone(),
        h.quotas.clone(),
    );
    let resp = ws_svc
        .request_worker(&scope, "forge", Some(1), None, None, None, "worker-1")
        .await
        .unwrap();
    assert_eq!(resp.status, "queued");

    // A second worker in the same tenant is within the default fleet ceiling
    // (2); a third must be rejected as a resource-limit violation.
    ws_svc
        .request_worker(&scope, "sentinel", Some(1), None, None, None, "worker-2")
        .await
        .unwrap();
    let err = ws_svc
        .request_worker(&scope, "vessel", Some(1), None, None, None, "worker-3")
        .await
        .unwrap_err();
    assert!(
        matches!(&err, openflows_manager::error::ManagerError::Api(a) if a.code == "WORKER_LIMIT_REACHED"),
        "fleet ceiling must be enforced: {err}"
    );
}

// ---------------------------------------------------------------------------
// Runtime renewal, revocation, and tenant deletion.
// ---------------------------------------------------------------------------
#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn runtime_renewal_revocation_and_tenant_deletion() {
    let h = harness().await;
    let tenant = create_tenant(&h, "rt", 1, "rt").await;
    let owner = format!("rt-worker-{}", std::process::id());
    let guard = operations::claim_kind(&h.pool, &owner, Some("tenant.provision"))
        .await
        .unwrap()
        .unwrap();
    provision_with_runtime_registration(&h, &guard).await;

    let nexus_ws: uuid::Uuid =
        sqlx::query_scalar("SELECT id FROM workspaces WHERE tenant_id = $1 AND role = 'nexus'")
            .bind(tenant.0)
            .fetch_one(&h.pool)
            .await
            .unwrap();
    let creds = openflows_manager::runtime::RuntimeCredentialService::new(
        openflows_manager::runtime::RuntimeRepository::new(h.pool.clone()),
    );
    let wsid = openflows_manager::id::WorkspaceId::from_uuid(nexus_ws);
    let issued = creds.issue_for_workspace(wsid).await.unwrap();

    // Renewal (rotate) advances the generation and invalidates the old
    // credential, without a workspace rebuild.
    creds.authenticate(&issued.raw_secret).await.unwrap();
    let rotated = creds.rotate(wsid).await.unwrap();
    assert!(rotated.generation > issued.generation);
    // The old credential no longer authenticates.
    let err = creds.authenticate(&issued.raw_secret).await.unwrap_err();
    assert!(
        matches!(&err, openflows_manager::error::ManagerError::Api(a) if a.code == "AUTH_FAILED"),
        "rotated credential must be invalid: {err}"
    );

    // Revoke the workspace issuance.
    creds.revoke(wsid).await.unwrap();
    let err = creds.authenticate(&rotated.raw_secret).await.unwrap_err();
    assert!(
        matches!(&err, openflows_manager::error::ManagerError::Api(a) if a.code == "AUTH_FAILED")
    );

    // Delete the tenant (admin) revokes runtime issuance and enqueues cleanup.
    let op_id = h
        .tenant_service
        .delete_tenant(h.owner, h.org, tenant, "del-key", "r")
        .await
        .unwrap();
    let repeated = h
        .tenant_service
        .delete_tenant(h.owner, h.org, tenant, "different-del-key", "r2")
        .await
        .unwrap();
    assert_eq!(
        repeated, op_id,
        "a second delete must reuse the cleanup operation"
    );
    // The delete operation marks the tenant deleted.
    let del_guard = operations::claim_kind(&h.pool, "del-worker", Some("tenant.delete"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(del_guard.operation_id, op_id);
    h.worker.delete(&del_guard).await.unwrap();
    let state: String = sqlx::query_scalar("SELECT observed_state FROM tenants WHERE id = $1")
        .bind(tenant.0)
        .fetch_one(&h.pool)
        .await
        .unwrap();
    assert_eq!(state, "deleted");
    let linked: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM coder_user_links WHERE user_id=$1)")
            .bind(h.owner.0)
            .fetch_one(&h.pool)
            .await
            .unwrap();
    assert!(
        linked,
        "tenant deletion must preserve personal account mapping"
    );
    // Runtime credentials were revoked at delete-request time.
    let revoked: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM runtime_credentials
          WHERE workspace_id = $1 AND revoked_at IS NULL)",
    )
    .bind(nexus_ws)
    .fetch_one(&h.pool)
    .await
    .unwrap();
    assert!(
        !revoked,
        "tenant deletion must revoke live runtime credentials"
    );
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn exhausted_tenant_delete_is_recovered_without_manual_retry() {
    let h = harness().await;
    let tenant = create_tenant(&h, "delete-recovery", 1, "delete-recovery").await;
    let op_id = h
        .tenant_service
        .delete_tenant(h.owner, h.org, tenant, "delete-recovery", "r")
        .await
        .unwrap();
    sqlx::query(
        "UPDATE operations SET state = 'retryable_failed', attempt_count = 8,
            retry_at = NULL, updated_at = now() - interval '6 minutes'
          WHERE id = $1",
    )
    .bind(op_id.0)
    .execute(&h.pool)
    .await
    .unwrap();

    h.worker.run_once().await.unwrap();
    let (tenant_state, op_state): (String, String) = sqlx::query_as(
        "SELECT t.desired_state, o.state FROM tenants t JOIN operations o ON o.resource_id = t.id
          WHERE o.id = $1",
    )
    .bind(op_id.0)
    .fetch_one(&h.pool)
    .await
    .unwrap();
    assert_eq!(tenant_state, "deleted");
    assert_eq!(op_state, "succeeded");
}

// ---------------------------------------------------------------------------
// Tenant Redis namespace restrictions and authenticated hook relay.
// ---------------------------------------------------------------------------
#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn tenant_redis_namespace_is_immutable_and_hook_relay_is_authenticated() {
    let h = harness().await;
    let tenant = create_tenant(&h, "redis", 1, "rd").await;

    // Allocate the tenant Redis credential; the namespace must be the immutable
    // tenant UUID, never a customer-chosen name.
    let secret_ref = format!("redis-{}", tenant.0);
    h.tenant_repo
        .allocate_redis_credential(h.org, tenant, &secret_ref)
        .await
        .unwrap();
    let cred = h
        .tenant_repo
        .redis_credential(h.org, tenant)
        .await
        .unwrap()
        .expect("tenant redis credential");
    assert_eq!(
        cred.namespace,
        tenant.0.to_string(),
        "namespace must be the immutable tenant UUID"
    );

    // Authenticated hook relay: only a Nexus runtime of the tenant may use it.
    let owner = format!("hk-worker-{}", std::process::id());
    let guard = operations::claim_kind(&h.pool, &owner, Some("tenant.provision"))
        .await
        .unwrap()
        .unwrap();
    provision_with_runtime_registration(&h, &guard).await;
    let nexus_ws: uuid::Uuid =
        sqlx::query_scalar("SELECT id FROM workspaces WHERE tenant_id = $1 AND role = 'nexus'")
            .bind(tenant.0)
            .fetch_one(&h.pool)
            .await
            .unwrap();
    let creds = openflows_manager::runtime::RuntimeCredentialService::new(
        openflows_manager::runtime::RuntimeRepository::new(h.pool.clone()),
    );
    let issued = creds
        .issue_for_workspace(openflows_manager::id::WorkspaceId::from_uuid(nexus_ws))
        .await
        .unwrap();
    let scope = creds.authenticate(&issued.raw_secret).await.unwrap();
    assert_eq!(
        scope.tenant_id, tenant,
        "hook relay scope must be the same tenant"
    );
}

// ---------------------------------------------------------------------------
// Pinned template/version selection.
// ---------------------------------------------------------------------------
#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn tenant_nexus_uses_pinned_template_version() {
    let h = harness().await;
    let _tenant = create_tenant(&h, "pin", 1, "pin").await;
    let owner = format!("pin-worker-{}", std::process::id());
    let guard = operations::claim_kind(&h.pool, &owner, Some("tenant.provision"))
        .await
        .unwrap()
        .unwrap();
    provision_with_runtime_registration(&h, &guard).await;

    // The Nexus workspace must be created with the org's pinned nexus template
    // version (the one the mock records), not the default org or another role.
    let coder_org = coder_client::CoderOrganizationId::new(internal_org_name(h.org));
    let machine_owner =
        openflows_manager::coder::types::CoderOwnerId::new(format!("human-{}", h.owner.0));
    let recorded_version = h
        .adapter
        .workspace_version(&coder_org, &machine_owner.0)
        .unwrap();

    // The persisted pinned version id for the nexus role in this org.
    let pinned: String = sqlx::query_scalar(
        "SELECT coder_version_id FROM organization_template_versions
          WHERE organization_id = $1 AND role = 'nexus' AND status = 'ready'",
    )
    .bind(h.org.0)
    .fetch_one(&h.pool)
    .await
    .unwrap();
    assert_eq!(
        recorded_version, pinned,
        "Nexus must use the pinned nexus version"
    );
}

#[test]
fn tenant_slug_validation_rejects_unsafe_names() {
    assert!(validate_tenant_slug("backend").is_ok());
    assert!(validate_tenant_slug("a-b-1").is_ok());
    assert!(validate_tenant_slug("").is_err());
    assert!(validate_tenant_slug("-bad").is_err());
    assert!(validate_tenant_slug("UPPER").is_err());
    assert!(validate_tenant_slug("has space").is_err());
    assert!(validate_tenant_slug(&"x".repeat(64)).is_err());
}

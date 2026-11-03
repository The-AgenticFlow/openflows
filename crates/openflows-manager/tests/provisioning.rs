//! Provisioning acceptance tests: Coder adapter and template releases.
//!
//! These exercise the provisioning acceptance criteria plus the supporting
//! readiness and security behaviors against an isolated PostgreSQL database and
//! a deterministic in-memory Coder adapter:
//!
//! 1. Cross-organization template names cannot collide.
//! 2. Failed imports prevent readiness.
//! 3. Duplicate publication retries converge.
//! 4. Version pins survive active-pointer changes.
//!
//! Plus: wrong-scope rejection, missing license/provisioner prevents
//! readiness, suspension during provisioning cannot restore readiness,
//! customer users cannot approve/promote releases, and secrets do not appear
//! in persisted operational records.
//!
//! Run with: `cargo test --test provisioning -- --ignored`

mod common;

use common::TestDb;
use openflows_manager::coder::{MockCoderAdapter, MockCoderConfig};
use openflows_manager::db::tx::OrgScope;
use openflows_manager::id::{OrganizationId, ReleaseId, UserId};
use openflows_manager::operations::{self, LeaseGuard};
use openflows_manager::provisioning::OrgProvisionWorker;
use openflows_manager::releases::ReleaseApprovalService;
use openflows_manager::repositories::OrganizationsRepository;
use openflows_manager::templates::packaging::{
    build_deterministic_archive, sha256_hex, Role, RoleManifestEntry, TemplateManifest,
};
use openflows_manager::templates::repository::ReleaseRepository;
use openflows_manager::templates::service::{internal_org_name, ReleaseArtifacts, ReleaseService};
use std::collections::BTreeMap;
use std::sync::Arc;

/// A deterministic barrier around one external call, so the tests can inspect
/// queued work and invalidate authorization while provisioning is in flight.
struct PausedAdapter {
    inner: Arc<MockCoderAdapter>,
    entered: Arc<tokio::sync::Notify>,
    release: Arc<tokio::sync::Notify>,
    at_templates: bool,
}

#[async_trait::async_trait]
impl openflows_manager::coder::CoderAdapter for PausedAdapter {
    async fn verify_capabilities(
        &self,
    ) -> Result<
        openflows_manager::coder::types::CoderCapabilities,
        openflows_manager::error::ManagerError,
    > {
        if !self.at_templates {
            self.entered.notify_one();
            self.release.notified().await;
        }
        self.inner.verify_capabilities().await
    }
    async fn ensure_organization(
        &self,
        name: &str,
        display: &str,
    ) -> Result<coder_client::CoderOrganizationId, openflows_manager::error::ManagerError> {
        self.inner.ensure_organization(name, display).await
    }
    async fn ensure_provisioner_ready(
        &self,
        org: &coder_client::CoderOrganizationId,
        backend: &str,
    ) -> Result<(), openflows_manager::error::ManagerError> {
        self.inner.ensure_provisioner_ready(org, backend).await
    }
    async fn ensure_template_version(
        &self,
        org: &coder_client::CoderOrganizationId,
        name: &str,
        version: &str,
        archive: &[u8],
    ) -> Result<
        openflows_manager::coder::TemplatePublicationResult,
        openflows_manager::error::ManagerError,
    > {
        if self.at_templates && name == "openflows-vessel" {
            self.entered.notify_one();
            self.release.notified().await;
        }
        self.inner
            .ensure_template_version(org, name, version, archive)
            .await
    }
    async fn verify_org_models(
        &self,
        org: &coder_client::CoderOrganizationId,
    ) -> Result<bool, openflows_manager::error::ManagerError> {
        let _ = org;
        panic!("organization provisioning must not query models")
    }
}

fn pause_worker(
    h: &WorkerHarness,
    at_templates: bool,
) -> (
    OrgProvisionWorker,
    Arc<tokio::sync::Notify>,
    Arc<tokio::sync::Notify>,
) {
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let mut worker = h.worker.clone();
    worker.adapter = Arc::new(PausedAdapter {
        inner: h.adapter.clone(),
        entered: entered.clone(),
        release: release.clone(),
        at_templates,
    });
    (worker, entered, release)
}

/// Build deterministic release artifacts (manifest + role archives) in a temp
/// directory.
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
                image_digest: None,
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

/// A ready-to-run worker bundle for a test. Holds the `TestDb` so the isolated
/// database is not dropped while the test uses the pool.
struct WorkerHarness {
    _db: TestDb,
    pool: sqlx::PgPool,
    orgs: OrganizationsRepository,
    releases: ReleaseService,
    approvals: ReleaseApprovalService,
    adapter: Arc<MockCoderAdapter>,
    worker: OrgProvisionWorker,
    artifacts: Arc<ReleaseArtifacts>,
    release_id: ReleaseId,
}

async fn harness(config: MockCoderConfig) -> WorkerHarness {
    let db = TestDb::new().await.unwrap();
    db.migrate().await.unwrap();
    let pool = db.pool().clone();

    let orgs = OrganizationsRepository::new(pool.clone());
    let release_repo = ReleaseRepository::new(pool.clone());
    let releases = ReleaseService::new(pool.clone(), release_repo.clone());
    let approvals =
        ReleaseApprovalService::new(pool.clone(), releases.clone(), release_repo.clone());

    let (artifacts, _dir) = build_release_artifacts();
    let (release_id, _digest) = releases
        .import_release(artifacts.clone(), "test")
        .await
        .unwrap();
    release_repo
        .set_release_status(release_id, "approved", None)
        .await
        .unwrap();

    let adapter = Arc::new(MockCoderAdapter::new(config));
    let worker = OrgProvisionWorker::new(
        pool.clone(),
        orgs.clone(),
        releases.clone(),
        release_repo.clone(),
        adapter.clone(),
        Some(Arc::new(artifacts.clone())),
        Some(release_id),
        "docker".into(),
        "v2.37.3".into(),
    );

    WorkerHarness {
        _db: db,
        pool,
        orgs,
        releases,
        approvals,
        adapter,
        worker,
        artifacts: Arc::new(artifacts),
        release_id,
    }
}

/// Insert a provisioning organization + a queued `org.provision` operation and
/// claim it. Returns the org id and a lease guard.
async fn provision_org(
    pool: &sqlx::PgPool,
    slug: &str,
    owner: uuid::Uuid,
) -> (OrganizationId, LeaseGuard) {
    let org = uuid::Uuid::new_v4();
    let mut tx = pool.begin().await.unwrap();
    sqlx::query(
        "INSERT INTO organizations (id, slug, display_name, owner_user_id, status)
         VALUES ($1, $2, $3, $4, 'provisioning')",
    )
    .bind(org)
    .bind(slug)
    .bind(slug)
    .bind(owner)
    .execute(&mut *tx)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO memberships (organization_id, user_id, role, status)
         VALUES ($1, $2, 'admin', 'active')",
    )
    .bind(org)
    .bind(owner)
    .execute(&mut *tx)
    .await
    .unwrap();
    let op = operations::create_in_tx(
        &mut tx,
        &operations::NewOperation {
            organization_id: OrganizationId::from_uuid(org),
            resource_type: "organization".into(),
            resource_id: Some(org),
            kind: "org.provision".into(),
            idempotency_ref: None,
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();

    let owner = format!("test-worker-{}", std::process::id());
    let guard = operations::claim(pool, &owner).await.unwrap().unwrap();
    assert_eq!(guard.organization_id.0, org);
    let _ = op;
    (OrganizationId::from_uuid(org), guard)
}

async fn org_status(pool: &sqlx::PgPool, org: OrganizationId) -> String {
    sqlx::query_scalar("SELECT status FROM organizations WHERE id = $1")
        .bind(org.0)
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn readiness_error(pool: &sqlx::PgPool, org: OrganizationId) -> Option<String> {
    sqlx::query_scalar("SELECT last_readiness_error FROM organizations WHERE id = $1")
        .bind(org.0)
        .fetch_one(pool)
        .await
        .unwrap()
}

/// The published template id for a role in an organization, if recorded.
async fn published_template_id(
    pool: &sqlx::PgPool,
    org: OrganizationId,
    role: &str,
) -> Option<String> {
    sqlx::query_scalar(
        "SELECT coder_template_id FROM organization_template_versions
          WHERE organization_id = $1 AND role = $2",
    )
    .bind(org.0)
    .bind(role)
    .fetch_optional(pool)
    .await
    .unwrap()
}

async fn published_version_id(
    pool: &sqlx::PgPool,
    org: OrganizationId,
    role: &str,
) -> Option<String> {
    sqlx::query_scalar(
        "SELECT coder_version_id FROM organization_template_versions
          WHERE organization_id = $1 AND role = $2",
    )
    .bind(org.0)
    .bind(role)
    .fetch_optional(pool)
    .await
    .unwrap()
}

async fn insert_user(pool: &sqlx::PgPool, login: &str) -> uuid::Uuid {
    let id = uuid::Uuid::new_v4();
    sqlx::query("INSERT INTO users (id, display_name, status) VALUES ($1, $2, 'active')")
        .bind(id)
        .bind(login)
        .execute(pool)
        .await
        .unwrap();
    id
}

// ---------------------------------------------------------------------------
// Acceptance criterion 1: cross-organization template names cannot collide.
// ---------------------------------------------------------------------------
#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn cross_org_template_names_resolve_to_distinct_ids() {
    let h = harness(MockCoderConfig::default()).await;
    let owner = insert_user(&h.pool, "owner").await;

    let (org_a, guard_a) = provision_org(&h.pool, "acme-a", owner).await;
    sqlx::query("UPDATE organizations SET display_name = $1 WHERE id = $2")
        .bind("Acme Research & Development")
        .bind(org_a.0)
        .execute(&h.pool)
        .await
        .unwrap();
    let outcome = h.worker.provision(&guard_a).await.unwrap();
    assert_eq!(
        h.adapter
            .organization_display_name(&coder_org_for(org_a))
            .as_deref(),
        Some("Acme Research & Development")
    );
    assert_eq!(
        outcome,
        openflows_manager::provisioning::OrgProvisionOutcome::Ready(org_a)
    );

    let (org_b, guard_b) = provision_org(&h.pool, "acme-b", owner).await;
    let outcome = h.worker.provision(&guard_b).await.unwrap();
    assert_eq!(
        outcome,
        openflows_manager::provisioning::OrgProvisionOutcome::Ready(org_b)
    );

    // Same template name `openflows-nexus` in two organizations must resolve to
    // two different, correct Coder template ids.
    let tpl_a = published_template_id(&h.pool, org_a, "nexus")
        .await
        .expect("A nexus template");
    let tpl_b = published_template_id(&h.pool, org_b, "nexus")
        .await
        .expect("B nexus template");
    assert_ne!(tpl_a, tpl_b, "template names must not collide across orgs");

    let ver_a = published_version_id(&h.pool, org_a, "nexus").await.unwrap();
    let ver_b = published_version_id(&h.pool, org_b, "nexus").await.unwrap();
    assert_ne!(ver_a, ver_b);

    // Both orgs became ready and hold their own distinct coder org mapping.
    assert_eq!(org_status(&h.pool, org_a).await, "ready");
    assert_eq!(org_status(&h.pool, org_b).await, "ready");
    let coder_a: String =
        sqlx::query_scalar("SELECT coder_organization_id FROM organizations WHERE id=$1")
            .bind(org_a.0)
            .fetch_one(&h.pool)
            .await
            .unwrap();
    let coder_b: String =
        sqlx::query_scalar("SELECT coder_organization_id FROM organizations WHERE id=$1")
            .bind(org_b.0)
            .fetch_one(&h.pool)
            .await
            .unwrap();
    assert_eq!(coder_a, internal_org_name(org_a));
    assert_eq!(coder_b, internal_org_name(org_b));
    assert_ne!(coder_a, coder_b);
}

// ---------------------------------------------------------------------------
// Acceptance criterion 2: failed imports prevent readiness.
// ---------------------------------------------------------------------------
#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn failed_org_import_can_be_requeued_but_other_terminal_failures_cannot() {
    for (slug, code, allowed) in [
        ("retry-import", "IMPORT_FAILED", true),
        ("retry-other", "CODER_ORG_FAILED", false),
    ] {
        let h = harness(MockCoderConfig::default()).await;
        let owner = insert_user(&h.pool, "owner").await;
        let (org, guard) = provision_org(&h.pool, slug, owner).await;
        assert!(guard.fail(false, Some(code)).await.unwrap());
        let id: uuid::Uuid =
            sqlx::query_scalar("SELECT id FROM operations WHERE organization_id = $1")
                .bind(org.0)
                .fetch_one(&h.pool)
                .await
                .unwrap();
        let mut conn = h.pool.acquire().await.unwrap();
        let wrong_scope = operations::retry_in_tx(
            &mut conn,
            openflows_manager::id::OperationId::from_uuid(id),
            OrganizationId::from_uuid(uuid::Uuid::new_v4()),
        )
        .await;
        assert!(wrong_scope.is_err());
        let result = operations::retry_in_tx(
            &mut conn,
            openflows_manager::id::OperationId::from_uuid(id),
            org,
        )
        .await;
        assert_eq!(result.is_ok(), allowed);
        let state: String = sqlx::query_scalar("SELECT state FROM operations WHERE id = $1")
            .bind(id)
            .fetch_one(&mut *conn)
            .await
            .unwrap();
        assert_eq!(state, if allowed { "queued" } else { "failed" });
    }
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn failed_import_prevents_ready() {
    let config = MockCoderConfig {
        import_status_override: Some("failed".to_string()),
        ..Default::default()
    };
    let h = harness(config).await;
    let owner = insert_user(&h.pool, "owner").await;

    let (org, guard) = provision_org(&h.pool, "acme-fail", owner).await;
    let outcome = h.worker.provision(&guard).await.unwrap();
    assert_eq!(
        outcome,
        openflows_manager::provisioning::OrgProvisionOutcome::Failed(
            org,
            openflows_manager::provisioning::err::IMPORT_FAILED,
            false
        )
    );
    // The organization must NOT become ready.
    assert_eq!(org_status(&h.pool, org).await, "provisioning");
    assert_eq!(
        readiness_error(&h.pool, org).await.as_deref(),
        Some(openflows_manager::provisioning::err::IMPORT_FAILED)
    );
    // No template version may be recorded as ready.
    let row: Option<String> = sqlx::query_scalar(
        "SELECT status FROM organization_template_versions WHERE organization_id=$1 AND role='nexus'",
    )
    .bind(org.0)
    .fetch_optional(&h.pool)
    .await
    .unwrap();
    assert!(
        row.is_none(),
        "no publication record should be ready on failed import"
    );
}

// ---------------------------------------------------------------------------
// Acceptance criterion 3: duplicate publication retries converge.
// ---------------------------------------------------------------------------
#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn duplicate_publication_retries_converge() {
    let h = harness(MockCoderConfig::default()).await;
    let owner = insert_user(&h.pool, "owner").await;
    let (org, guard) = provision_org(&h.pool, "acme-dup", owner).await;

    // First attempt creates the versions.
    let first = h.worker.provision(&guard).await.unwrap();
    assert_eq!(
        first,
        openflows_manager::provisioning::OrgProvisionOutcome::Ready(org)
    );

    let nexus_template = published_template_id(&h.pool, org, "nexus").await.unwrap();
    let nexus_version = published_version_id(&h.pool, org, "nexus").await.unwrap();

    // Simulate a retry: publish again for the same org/release/role directly
    // through the release service (idempotent rediscovery).
    h.releases
        .publish_for_org(org, h.release_id, h.adapter.as_ref(), &h.artifacts)
        .await
        .unwrap();

    // No unbounded duplicate versions appended.
    assert_eq!(
        h.adapter
            .version_count(&coder_org_for(org), "openflows-nexus"),
        1
    );
    // The persisted IDs are stable.
    assert_eq!(
        published_template_id(&h.pool, org, "nexus").await.unwrap(),
        nexus_template
    );
    assert_eq!(
        published_version_id(&h.pool, org, "nexus").await.unwrap(),
        nexus_version
    );
}

fn coder_org_for(org: OrganizationId) -> openflows_manager::coder::OrgId {
    openflows_manager::coder::OrgId::new(internal_org_name(org))
}

// ---------------------------------------------------------------------------
// Acceptance criterion 4: version pins survive active-pointer changes.
// ---------------------------------------------------------------------------
#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn version_pins_survive_republication() {
    let h = harness(MockCoderConfig::default()).await;
    let owner = insert_user(&h.pool, "owner").await;
    let (org, guard) = provision_org(&h.pool, "acme-pin", owner).await;
    let first = h.worker.provision(&guard).await.unwrap();
    assert_eq!(
        first,
        openflows_manager::provisioning::OrgProvisionOutcome::Ready(org)
    );

    let pinned = published_version_id(&h.pool, org, "nexus").await.unwrap();
    let template = published_template_id(&h.pool, org, "nexus").await.unwrap();
    let mut next = (*h.artifacts).clone();
    next.manifest.release = "2026.10.1".into();
    let (next_id, _) = h
        .releases
        .import_release(next.clone(), "test")
        .await
        .unwrap();
    h.releases
        .publish_for_org(org, next_id, h.adapter.as_ref(), &next)
        .await
        .unwrap();
    let next_version = h
        .releases
        .repo
        .get_org_template_version(org, next_id, Role::Nexus)
        .await
        .unwrap()
        .unwrap()
        .coder_version_id
        .unwrap();
    assert_ne!(pinned, next_version);
    h.adapter.set_active_version(&template, &next_version);

    // Republication (e.g. after a restart or a pointer change upstream) must
    // rediscover the exact same version by stable identity rather than moving
    // the pin to a new version.
    h.releases
        .publish_for_org(org, h.release_id, h.adapter.as_ref(), &h.artifacts)
        .await
        .unwrap();
    assert_eq!(
        h.releases
            .repo
            .get_org_template_version(org, h.release_id, Role::Nexus)
            .await
            .unwrap()
            .unwrap()
            .coder_version_id
            .unwrap(),
        pinned
    );
    assert_eq!(h.adapter.active_version(&template), Some(next_version));
    // Republishing the old release neither changes the active pointer nor appends a version.
    assert_eq!(
        h.adapter
            .version_count(&coder_org_for(org), "openflows-nexus"),
        2
    );
}

// ---------------------------------------------------------------------------
// License and provisioner are prerequisites; model configuration is independent.
// ---------------------------------------------------------------------------
#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn missing_license_prevents_ready() {
    let config = MockCoderConfig {
        license_ok: false,
        ..Default::default()
    };
    let h = harness(config).await;
    let owner = insert_user(&h.pool, "owner").await;
    let (org, guard) = provision_org(&h.pool, "acme-lic", owner).await;
    let outcome = h.worker.provision(&guard).await.unwrap();
    assert_eq!(
        outcome,
        openflows_manager::provisioning::OrgProvisionOutcome::NotReady(
            org,
            openflows_manager::provisioning::err::CODER_CAPABILITIES_UNMET
        )
    );
    assert_eq!(org_status(&h.pool, org).await, "provisioning");
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn missing_provisioner_heartbeat_prevents_ready() {
    let config = MockCoderConfig {
        provisioner_ready: false,
        ..Default::default()
    };
    let h = harness(config).await;
    let owner = insert_user(&h.pool, "owner").await;
    let (org, guard) = provision_org(&h.pool, "acme-prov", owner).await;
    let outcome = h.worker.provision(&guard).await.unwrap();
    assert_eq!(
        outcome,
        openflows_manager::provisioning::OrgProvisionOutcome::NotReady(
            org,
            openflows_manager::provisioning::err::PROVISIONER_NOT_READY
        )
    );
    assert_eq!(org_status(&h.pool, org).await, "provisioning");
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn missing_org_models_does_not_prevent_ready() {
    let config = MockCoderConfig {
        models_ok: false,
        ..Default::default()
    };
    let h = harness(config).await;
    let owner = insert_user(&h.pool, "owner").await;
    let (org, guard) = provision_org(&h.pool, "acme-model", owner).await;
    let outcome = h.worker.provision(&guard).await.unwrap();
    assert_eq!(
        outcome,
        openflows_manager::provisioning::OrgProvisionOutcome::Ready(org)
    );
    assert_eq!(org_status(&h.pool, org).await, "ready");
    let model_ok: Option<bool> =
        sqlx::query_scalar("SELECT model_capability_ok FROM organizations WHERE id = $1")
            .bind(org.0)
            .fetch_one(&h.pool)
            .await
            .unwrap();
    assert_eq!(
        model_ok, None,
        "provisioning must not claim model readiness"
    );
}

// ---------------------------------------------------------------------------
// Suspension/deletion during provisioning cannot restore readiness.
// ---------------------------------------------------------------------------
#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn suspension_during_provisioning_cannot_restore_ready() {
    let h = harness(MockCoderConfig::default()).await;
    let owner = insert_user(&h.pool, "owner").await;
    let (org, guard) = provision_org(&h.pool, "acme-susp", owner).await;

    // Suspend the organization while provisioning is in flight.
    h.orgs
        .set_status(OrgScope::new(org), org, "suspended")
        .await
        .unwrap();

    let outcome = h.worker.provision(&guard).await.unwrap();
    assert_eq!(
        outcome,
        openflows_manager::provisioning::OrgProvisionOutcome::NotReady(
            org,
            openflows_manager::provisioning::err::ORG_STATE_CHANGED
        )
    );
    assert_eq!(org_status(&h.pool, org).await, "suspended");
}

// ---------------------------------------------------------------------------
// Customer users cannot approve/promote platform releases.
// ---------------------------------------------------------------------------
#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn customer_users_cannot_approve_or_promote_releases() {
    let h = harness(MockCoderConfig::default()).await;
    let customer = UserId::from_uuid(insert_user(&h.pool, "customer-admin").await);
    let operator = UserId::from_uuid(insert_user(&h.pool, "platform-op").await);

    // The customer org admin is NOT a platform operator.
    let err = h
        .approvals
        .approve(customer, h.release_id)
        .await
        .unwrap_err();
    assert!(
        matches!(&err, openflows_manager::error::ManagerError::Api(a) if a.code == "PLATFORM_OPERATOR_REQUIRED"),
        "customer admin must not approve releases: {err}"
    );

    // Grant platform-operator status (only an existing operator can; bootstrap
    // an operator by inserting directly, since no operator exists yet).
    sqlx::query("INSERT INTO platform_operators (user_id) VALUES ($1)")
        .bind(operator.0)
        .execute(&h.pool)
        .await
        .unwrap();

    // A customer admin still cannot approve even after an operator exists.
    let err = h
        .approvals
        .approve(customer, h.release_id)
        .await
        .unwrap_err();
    assert!(
        matches!(&err, openflows_manager::error::ManagerError::Api(a) if a.code == "PLATFORM_OPERATOR_REQUIRED")
    );

    // The platform operator can approve.
    h.approvals.approve(operator, h.release_id).await.unwrap();
    let status: String = sqlx::query_scalar("SELECT status FROM template_releases WHERE id=$1")
        .bind(h.release_id.0)
        .fetch_one(&h.pool)
        .await
        .unwrap();
    assert_eq!(status, "approved");
}

// ---------------------------------------------------------------------------
// Secrets do not appear in persisted operational records.
// ---------------------------------------------------------------------------
#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn secrets_do_not_appear_in_persisted_records() {
    let h = harness(MockCoderConfig::default()).await;
    let owner = insert_user(&h.pool, "owner").await;
    let (org, guard) = provision_org(&h.pool, "acme-secret", owner).await;
    let outcome = h.worker.provision(&guard).await.unwrap();
    assert!(matches!(
        outcome,
        openflows_manager::provisioning::OrgProvisionOutcome::Ready(_)
    ));

    // A secret marker must not appear anywhere in the release publication
    // records, the org row, or operation records. The mock's version ids are
    // deterministic and contain no secret material; assert the persisted rows
    // carry only identifiers/digests and no credential-like values.
    let digest_len: Option<String> =
        sqlx::query_scalar("SELECT manifest_sha256 FROM template_releases WHERE id=$1")
            .bind(h.release_id.0)
            .fetch_one(&h.pool)
            .await
            .ok();
    if let Some(digest) = digest_len {
        assert_eq!(digest.len(), 64, "only a digest is persisted");
    }
    let rows: Vec<(String, String, String)> = sqlx::query_as(
        "SELECT coder_template_id, coder_version_id, import_job_id
           FROM organization_template_versions WHERE organization_id=$1",
    )
    .bind(org.0)
    .fetch_all(&h.pool)
    .await
    .unwrap();
    for (tpl, ver, job) in rows {
        for v in [&tpl, &ver, &job] {
            assert!(
                !v.contains("token") && !v.contains("secret") && !v.contains("password"),
                "persisted publication record must not carry secret-like material"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Stale lease holders cannot finalize the operation.
// ---------------------------------------------------------------------------
#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn stale_lease_holder_cannot_finalize_operation() {
    let h = harness(MockCoderConfig::default()).await;
    let owner = insert_user(&h.pool, "owner").await;
    let (org, guard_a) = provision_org(&h.pool, "acme-stale", owner).await;

    // Claim with a first owner, then force-release the lease by clearing the
    // owner/token (simulating an expired lease reclaimed by another worker).
    sqlx::query(
        "UPDATE operations SET lease_owner = NULL, lease_token = NULL, lease_expires_at = now() - interval '1 minute'",
    )
    .execute(&h.pool)
    .await
    .unwrap();

    // The stale guard's writes must be rejected (owner/token no longer match).
    let completed = guard_a.complete(None).await.unwrap();
    assert!(
        !completed,
        "a stale lease holder must not finalize the operation"
    );
    let failed = guard_a.fail(false, Some("X")).await.unwrap();
    assert!(!failed, "a stale lease holder must not fail the operation");
    assert_eq!(
        h.worker.provision(&guard_a).await.unwrap(),
        openflows_manager::provisioning::OrgProvisionOutcome::StaleLease
    );
    assert_eq!(org_status(&h.pool, org).await, "provisioning");

    let state: String = sqlx::query_scalar("SELECT state FROM operations WHERE organization_id=$1")
        .bind(org.0)
        .fetch_one(&h.pool)
        .await
        .unwrap();
    assert_eq!(
        state, "running",
        "operation must remain running for the real holder"
    );
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn concurrent_release_imports_converge_and_version_conflicts_fail() {
    let h = harness(MockCoderConfig::default()).await;
    let mut next = (*h.artifacts).clone();
    next.manifest.release = "concurrent-release".into();
    let mut tasks = Vec::new();
    for _ in 0..8 {
        let releases = h.releases.clone();
        let artifacts = next.clone();
        tasks.push(tokio::spawn(async move {
            releases.import_release(artifacts, "test").await.unwrap().0
        }));
    }
    let mut ids = Vec::new();
    for task in tasks {
        ids.push(task.await.unwrap());
    }
    assert!(ids.iter().all(|id| *id == ids[0]));
    next.manifest
        .roles
        .get_mut(&Role::Nexus)
        .unwrap()
        .parameter_schema_version = 2;
    assert!(h.releases.import_release(next, "test").await.is_err());
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn configured_release_restart_preserves_retirement_and_approval_metadata() {
    let h = harness(MockCoderConfig::default()).await;
    let repo = &h.releases.repo;
    let operator = uuid::Uuid::new_v4();
    repo.set_release_status(h.release_id, "approved", Some(operator))
        .await
        .unwrap();
    let approved_at = repo
        .get_release(h.release_id)
        .await
        .unwrap()
        .unwrap()
        .0
        .approved_at;
    repo.approve_configured_release(h.release_id).await.unwrap();
    assert_eq!(
        repo.get_release(h.release_id)
            .await
            .unwrap()
            .unwrap()
            .0
            .approved_at,
        approved_at
    );
    repo.set_release_status(h.release_id, "retired", None)
        .await
        .unwrap();
    let (same_id, _) = h
        .releases
        .import_release((*h.artifacts).clone(), "test")
        .await
        .unwrap();
    repo.approve_configured_release(same_id).await.unwrap();
    let row = repo.get_release(same_id).await.unwrap().unwrap().0;
    assert_eq!(row.status, "retired");
    assert_eq!(row.approved_by, Some(operator));
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn promotion_validates_manifest_and_only_switches_current_release_after_success() {
    let h = harness(MockCoderConfig::default()).await;
    let owner = insert_user(&h.pool, "owner").await;
    let (org, guard) = provision_org(&h.pool, "promote", owner).await;
    h.worker.provision(&guard).await.unwrap();
    let mut next = (*h.artifacts).clone();
    next.manifest.release = "next".into();
    let (next_id, _) = h
        .releases
        .import_release(next.clone(), "test")
        .await
        .unwrap();
    h.releases
        .repo
        .set_release_status(next_id, "approved", None)
        .await
        .unwrap();
    // Valid archive bytes with a different manifest must be rejected.
    assert!(h
        .releases
        .promote_to_org(org, next_id, h.adapter.as_ref(), &h.artifacts)
        .await
        .is_err());
    assert_eq!(
        h.releases.repo.get_org_binding(org).await.unwrap(),
        Some(h.release_id)
    );
    // Failed imports must also preserve the old current binding.
    let failed = MockCoderAdapter::new(MockCoderConfig {
        import_status_override: Some("failed".into()),
        ..Default::default()
    });
    assert!(h
        .releases
        .promote_to_org(org, next_id, &failed, &next)
        .await
        .is_err());
    assert_eq!(
        h.releases.repo.get_org_binding(org).await.unwrap(),
        Some(h.release_id)
    );
    h.releases
        .promote_to_org(org, next_id, h.adapter.as_ref(), &next)
        .await
        .unwrap();
    assert_eq!(
        h.releases.repo.get_org_binding(org).await.unwrap(),
        Some(next_id)
    );
    let history: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM organization_release_bindings WHERE organization_id = $1",
    )
    .bind(org.0)
    .fetch_one(&h.pool)
    .await
    .unwrap();
    assert_eq!(history, 2);
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn publication_uses_persisted_mapping_and_rejects_unmapped_organizations() {
    let h = harness(MockCoderConfig::default()).await;
    let owner = insert_user(&h.pool, "owner").await;
    let (org, _) = provision_org(&h.pool, "mapped", owner).await;
    assert!(h
        .releases
        .publish_for_org(org, h.release_id, h.adapter.as_ref(), &h.artifacts)
        .await
        .is_err());
    h.orgs
        .set_coder_organization_id(OrgScope::new(org), org, "saved-coder-id")
        .await
        .unwrap();
    h.releases
        .publish_for_org(org, h.release_id, h.adapter.as_ref(), &h.artifacts)
        .await
        .unwrap();
    assert_eq!(
        h.adapter.version_count(
            &coder_client::CoderOrganizationId::new("saved-coder-id"),
            "openflows-nexus"
        ),
        1
    );
    assert_eq!(
        h.adapter
            .version_count(&coder_org_for(org), "openflows-nexus"),
        0
    );
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn readiness_completion_rolls_back_and_retry_clears_error() {
    let h = harness(MockCoderConfig::default()).await;
    let owner = insert_user(&h.pool, "owner").await;
    let (org, guard) = provision_org(&h.pool, "atomic", owner).await;
    h.orgs
        .set_last_readiness_error(OrgScope::new(org), org, Some("OLD_ERROR"))
        .await
        .unwrap();
    // Reject the final binding after the readiness UPDATE, proving all final
    // writes roll back together when the database fails halfway through.
    sqlx::query("ALTER TABLE organization_current_releases ADD CONSTRAINT reject_test_binding CHECK (false)")
        .execute(&h.pool).await.unwrap();
    assert!(h.worker.provision(&guard).await.is_err());
    assert_eq!(org_status(&h.pool, org).await, "provisioning");
    assert_eq!(
        readiness_error(&h.pool, org).await.as_deref(),
        Some("OLD_ERROR")
    );
    assert_eq!(
        operations::get_scoped(&h.pool, org, guard.operation_id)
            .await
            .unwrap()
            .unwrap()
            .state,
        "running"
    );
    assert!(h
        .releases
        .repo
        .get_org_binding(org)
        .await
        .unwrap()
        .is_none());
    sqlx::query("ALTER TABLE organization_current_releases DROP CONSTRAINT reject_test_binding")
        .execute(&h.pool)
        .await
        .unwrap();
    h.worker.provision(&guard).await.unwrap();
    assert_eq!(org_status(&h.pool, org).await, "ready");
    assert_eq!(readiness_error(&h.pool, org).await, None);
    let operation = operations::get_scoped(&h.pool, org, guard.operation_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(operation.state, "succeeded");
    assert_eq!(operation.current_step.as_deref(), Some("mark_ready"));
    let provisioner = h.releases.repo.get_provisioner(org).await.unwrap().unwrap();
    assert_eq!(provisioner.status, "ready");
    assert!(provisioner.last_heartbeat_at.is_some());
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn unavailable_provisioner_is_recorded_without_fabricated_heartbeat() {
    let h = harness(MockCoderConfig {
        provisioner_ready: false,
        ..Default::default()
    })
    .await;
    let owner = insert_user(&h.pool, "owner").await;
    let (org, guard) = provision_org(&h.pool, "unavailable", owner).await;
    h.worker.provision(&guard).await.unwrap();
    let provisioner = h.releases.repo.get_provisioner(org).await.unwrap().unwrap();
    assert_eq!(provisioner.status, "unavailable");
    assert!(provisioner.last_heartbeat_at.is_none());
    assert_eq!(
        operations::get_scoped(&h.pool, org, guard.operation_id)
            .await
            .unwrap()
            .unwrap()
            .current_step
            .as_deref(),
        Some("ensure_provisioner")
    );
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn worker_claims_only_the_operation_it_can_process_now() {
    let h = harness(MockCoderConfig::default()).await;
    let owner = insert_user(&h.pool, "owner").await;
    let (first, _) = provision_org(&h.pool, "first", owner).await;
    let (second, _) = provision_org(&h.pool, "second", owner).await;
    sqlx::query("UPDATE operations SET state = 'queued', lease_owner = NULL, lease_token = NULL, lease_expires_at = NULL, attempt_count = 0")
        .execute(&h.pool).await.unwrap();
    let (worker, entered, _) = pause_worker(&h, false);
    let task = tokio::spawn(async move { worker.run_once().await });
    tokio::time::timeout(std::time::Duration::from_secs(5), entered.notified())
        .await
        .unwrap();
    let rows: Vec<(uuid::Uuid, String, i32, Option<String>)> = sqlx::query_as("SELECT organization_id, state, attempt_count, current_step FROM operations ORDER BY created_at")
        .fetch_all(&h.pool).await.unwrap();
    assert_eq!(
        rows[0],
        (
            first.0,
            "running".into(),
            1,
            Some("verify_capabilities".into())
        )
    );
    assert_eq!(rows[1], (second.0, "queued".into(), 0, None));
    let other = operations::claim_kind(&h.pool, "other-worker", Some("org.provision"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(other.organization_id, second);
    task.abort();
    let _ = task.await;
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn suspension_or_lost_lease_during_final_external_call_prevents_readiness() {
    for suspend in [true, false] {
        let h = harness(MockCoderConfig::default()).await;
        let owner = insert_user(&h.pool, "owner").await;
        let (org, guard) = provision_org(&h.pool, "inflight", owner).await;
        let (worker, entered, release) = pause_worker(&h, true);
        let task = tokio::spawn(async move { worker.provision(&guard).await });
        tokio::time::timeout(std::time::Duration::from_secs(5), entered.notified())
            .await
            .unwrap();
        if suspend {
            h.orgs
                .set_status(OrgScope::new(org), org, "suspended")
                .await
                .unwrap();
        } else {
            sqlx::query("UPDATE operations SET lease_token = NULL WHERE organization_id = $1")
                .bind(org.0)
                .execute(&h.pool)
                .await
                .unwrap();
        }
        release.notify_one();
        let outcome = task.await.unwrap().unwrap();
        assert!(!matches!(
            outcome,
            openflows_manager::provisioning::OrgProvisionOutcome::Ready(_)
        ));
        assert_eq!(
            org_status(&h.pool, org).await,
            if suspend { "suspended" } else { "provisioning" }
        );
        assert!(h
            .releases
            .repo
            .get_org_binding(org)
            .await
            .unwrap()
            .is_none());
    }
}

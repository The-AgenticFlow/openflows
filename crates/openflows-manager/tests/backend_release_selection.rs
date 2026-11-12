//! Backend-aware template release selection, compatibility validation,
//! pinning, and safe upgrade/rollback acceptance tests (Issue #504).
//!
//! Run live-DB tests with:
//! `cargo test -p openflows-manager --test backend_release_selection -- --ignored`

mod common;

use common::TestDb;
use openflows_manager::coder::types::{CoderCapabilities, CoderOwnerId};
use openflows_manager::coder::{
    CoderAdapter, MockCoderAdapter, MockCoderConfig, TemplatePublicationResult,
};
use openflows_manager::config::TenantLimits;
use openflows_manager::error::ManagerError;
use openflows_manager::id::{OrganizationId, ReleaseId, TenantId, UserId, WorkspaceId};
use openflows_manager::operations::{self, LeaseGuard};
use openflows_manager::organizations::OrganizationRepository;
use openflows_manager::provisioning::{
    OrgProvisionOutcome, OrgProvisionWorker, ReleaseProvider, TenantProvisionWorker,
};
use openflows_manager::quotas::TenantQuotaService;
use openflows_manager::repositories::{OrganizationsRepository, TenantsRepository};
use openflows_manager::runtime::tenant::TenantRuntimeRepository;
use openflows_manager::runtime::{
    RuntimeCredentialService, RuntimeRepository, RuntimeScope, RuntimeWorkspaceService,
};
use openflows_manager::secrets::{InMemorySecretProvider, SecretProvider};
use openflows_manager::templates::packaging::{
    build_deterministic_archive, sha256_hex, Role, RoleManifestEntry, TemplateManifest,
};
use openflows_manager::templates::repository::ReleaseRepository;
use openflows_manager::templates::service::{internal_org_name, ReleaseArtifacts, ReleaseService};
use openflows_manager::templates::{ProvisionerBackend, ReleaseCompatibilityTarget};
use openflows_manager::tenants::{TenantService, TenantServiceInputs};
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

/// Wraps `MockCoderAdapter` and counts workspace creation calls so early-rejection
/// tests can prove no Coder workspace is created when compatibility fails.
struct CountingCoderAdapter {
    inner: Arc<MockCoderAdapter>,
    create_workspace_calls: Arc<AtomicUsize>,
}

impl CountingCoderAdapter {
    fn new(inner: Arc<MockCoderAdapter>) -> Self {
        Self {
            inner,
            create_workspace_calls: Arc::new(AtomicUsize::new(0)),
        }
    }

    fn create_calls(&self) -> usize {
        self.create_workspace_calls.load(Ordering::SeqCst)
    }

    fn reset_create_calls(&self) {
        self.create_workspace_calls.store(0, Ordering::SeqCst);
    }
}

#[rustfmt::skip]
#[async_trait::async_trait]
impl CoderAdapter for CountingCoderAdapter {
    async fn reconcile_provisioners(&self) -> Result<(), ManagerError> { self.inner.reconcile_provisioners().await }
    async fn verify_capabilities(&self) -> Result<CoderCapabilities, ManagerError> { self.inner.verify_capabilities().await }
    async fn ensure_organization(&self, name: &str, display: &str) -> Result<coder_client::CoderOrganizationId, ManagerError> { self.inner.ensure_organization(name, display).await }
    async fn ensure_provisioner_ready(&self, org: &coder_client::CoderOrganizationId, backend: &str) -> Result<(), ManagerError> { self.inner.ensure_provisioner_ready(org, backend).await }
    async fn ensure_template_version(&self, org: &coder_client::CoderOrganizationId, name: &str, version: &str, archive: &[u8]) -> Result<TemplatePublicationResult, ManagerError> { self.inner.ensure_template_version(org, name, version, archive).await }
    async fn verify_org_models(&self, org: &coder_client::CoderOrganizationId) -> Result<bool, ManagerError> { self.inner.verify_org_models(org).await }
    async fn ensure_machine_owner(&self, org: &coder_client::CoderOrganizationId, tenant_id: TenantId) -> Result<CoderOwnerId, ManagerError> { self.inner.ensure_machine_owner(org, tenant_id).await }
    async fn ensure_user_owner(&self, org: &coder_client::CoderOrganizationId, owner: &CoderOwnerId) -> Result<(), ManagerError> { self.inner.ensure_user_owner(org, owner).await }
    async fn create_tenant_workspace(&self, org: &coder_client::CoderOrganizationId, owner: &CoderOwnerId, template_id: &coder_client::CoderTemplateId, version_id: &coder_client::CoderTemplateVersionId, name: &str) -> Result<coder_client::CoderWorkspaceDetail, ManagerError> {
        self.create_workspace_calls.fetch_add(1, Ordering::SeqCst);
        self.inner.create_tenant_workspace(org, owner, template_id, version_id, name).await
    }
    async fn create_tenant_workspace_with_parameters(&self, org: &coder_client::CoderOrganizationId, owner: &CoderOwnerId, template_id: &coder_client::CoderTemplateId, version_id: &coder_client::CoderTemplateVersionId, name: &str, parameters: &[coder_client::types::WorkspaceParameterValue]) -> Result<coder_client::CoderWorkspaceDetail, ManagerError> {
        self.create_workspace_calls.fetch_add(1, Ordering::SeqCst);
        self.inner.create_tenant_workspace_with_parameters(org, owner, template_id, version_id, name, parameters).await
    }
    async fn delete_machine_owner(&self, org: &coder_client::CoderOrganizationId, owner: &CoderOwnerId) -> Result<(), ManagerError> { self.inner.delete_machine_owner(org, owner).await }
    async fn wait_workspace_ready(&self, org: &coder_client::CoderOrganizationId, ws: &str) -> Result<(), ManagerError> { self.inner.wait_workspace_ready(org, ws).await }
    async fn workspace_startup_failed(&self, org: &coder_client::CoderOrganizationId, ws: &str) -> Result<bool, ManagerError> { self.inner.workspace_startup_failed(org, ws).await }
    async fn delete_workspace(&self, org: &coder_client::CoderOrganizationId, ws: &str) -> Result<(), ManagerError> { self.inner.delete_workspace(org, ws).await }
    async fn start_workspace(&self, org: &coder_client::CoderOrganizationId, ws: &str) -> Result<(), ManagerError> { self.inner.start_workspace(org, ws).await }
    async fn restart_workspace(&self, org: &coder_client::CoderOrganizationId, ws: &str) -> Result<(), ManagerError> { self.inner.restart_workspace(org, ws).await }
    async fn stop_workspace(&self, org: &coder_client::CoderOrganizationId, ws: &str) -> Result<(), ManagerError> { self.inner.stop_workspace(org, ws).await }
}

fn digest_char(ch: char) -> String {
    format!("sha256:{}", std::iter::repeat_n(ch, 64).collect::<String>())
}

struct VariantSpec<'a> {
    release_name: &'a str,
    architecture: &'a str,
    parameter_schema_version: u32,
    image_digest: Option<String>,
    workspace_runtime_release: Option<&'a str>,
}

fn package_role_dir(dir: &std::path::Path, tf: &str) -> (Vec<u8>, String) {
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(dir.join("main.tf"), tf).unwrap();
    let archive = build_deterministic_archive(dir).unwrap();
    let sha256 = sha256_hex(&archive);
    (archive, sha256)
}

fn build_dual_backend_v2_artifacts(
    spec: &VariantSpec<'_>,
) -> (ReleaseArtifacts, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let mut docker_roles = BTreeMap::new();
    let mut variants = BTreeMap::new();
    let mut archives = BTreeMap::new();

    for backend in ProvisionerBackend::ALL {
        for role in Role::ALL {
            let role_dir = dir.path().join(backend.as_str()).join(role.template_name());
            let tf = format!(
                "resource \"role\" {{ backend = \"{backend}\" name = \"{role}\" release = \"{}\" }}\n",
                spec.release_name
            );
            let (archive, sha256) = package_role_dir(&role_dir, &tf);
            let entry = RoleManifestEntry {
                role,
                backend,
                archive: role.archive_name_for_backend(backend),
                sha256,
                parameter_schema_version: spec.parameter_schema_version,
                architecture: spec.architecture.to_string(),
                image_repository: Some("ghcr.io/openflows/workspace-runtime".to_string()),
                image_digest: spec.image_digest.clone(),
                workspace_runtime_release: spec.workspace_runtime_release.map(str::to_string),
            };
            if backend == ProvisionerBackend::Docker {
                docker_roles.insert(role, entry.clone());
            }
            variants.insert((backend, role), entry);
            archives.insert((backend, role), archive);
        }
    }

    let manifest = TemplateManifest {
        schema_version: TemplateManifest::CURRENT_V2_SCHEMA_VERSION,
        release: spec.release_name.to_string(),
        required_roles: Role::ALL.to_vec(),
        roles: docker_roles,
        variants,
    };
    (
        ReleaseArtifacts::from_backend_archives(manifest, archives),
        dir,
    )
}

fn build_docker_only_v1_artifacts(
    release_name: &str,
    image_digest: Option<String>,
) -> (ReleaseArtifacts, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let mut archives = BTreeMap::new();
    let mut roles = BTreeMap::new();
    for role in Role::ALL {
        let tf =
            format!("resource \"role\" {{ name = \"{role}\" release = \"{release_name}\" }}\n");
        let (archive, sha256) = package_role_dir(&dir.path().join(role.template_name()), &tf);
        archives.insert(role, archive);
        roles.insert(
            role,
            RoleManifestEntry::docker(role, role.archive_name(), sha256, 1, image_digest.clone()),
        );
    }
    let manifest = TemplateManifest::from_v1_roles(release_name, Role::ALL.to_vec(), roles);
    (ReleaseArtifacts::new(manifest, archives), dir)
}

struct TestEnv {
    _db: TestDb,
    pool: sqlx::PgPool,
    owner: UserId,
    orgs: OrganizationsRepository,
    release_repo: ReleaseRepository,
    releases: ReleaseService,
    tenant_repo: TenantRuntimeRepository,
    runtime_credentials: RuntimeCredentialService,
    secret_provider: Arc<InMemorySecretProvider>,
    quotas: TenantQuotaService,
    tenant_service: TenantService,
    adapter: Arc<CountingCoderAdapter>,
}

impl TestEnv {
    async fn new() -> Self {
        let db = TestDb::new().await.unwrap();
        db.migrate().await.unwrap();
        let pool = db.pool().clone();

        let owner = UserId::from_uuid(common::insert_user(&pool).await.unwrap());
        sqlx::query(
            "INSERT INTO coder_user_links(user_id, coder_url, coder_user_id) VALUES ($1, $2, $3)",
        )
        .bind(owner.0)
        .bind(
            std::env::var("CODER_URL")
                .unwrap_or_default()
                .trim_end_matches('/'),
        )
        .bind(format!("human-{}", owner.0))
        .execute(&pool)
        .await
        .unwrap();

        let orgs = OrganizationsRepository::new(pool.clone());
        let release_repo = ReleaseRepository::new(pool.clone());
        let releases = ReleaseService::new(pool.clone(), release_repo.clone());
        let quotas = TenantQuotaService::new(
            pool.clone(),
            TenantLimits {
                max_tenants_per_org: 5,
                max_workers_per_tenant: 5,
                ..Default::default()
            },
        );
        let secret_provider = Arc::new(InMemorySecretProvider::new());
        let tenant_repo =
            TenantRuntimeRepository::with_secret_provider(pool.clone(), secret_provider.clone());
        let runtime_credentials = RuntimeCredentialService::with_secret_provider(
            RuntimeRepository::new(pool.clone()),
            secret_provider.clone(),
        );
        let tenant_service = TenantService::new(TenantServiceInputs::new(
            pool.clone(),
            TenantsRepository::new(pool.clone()),
            OrganizationRepository::new(pool.clone()),
            openflows_manager::idempotency::IdempotencyService::new(pool.clone()),
            quotas.clone(),
        ));
        let adapter = Arc::new(CountingCoderAdapter::new(Arc::new(MockCoderAdapter::new(
            MockCoderConfig::default(),
        ))));

        Self {
            _db: db,
            pool,
            owner,
            orgs,
            release_repo,
            releases,
            tenant_repo,
            runtime_credentials,
            secret_provider,
            quotas,
            tenant_service,
            adapter,
        }
    }

    fn tenant_worker(&self, target: ReleaseCompatibilityTarget) -> TenantProvisionWorker {
        TenantProvisionWorker::new(
            self.pool.clone(),
            self.orgs.clone(),
            self.quotas.clone(),
            self.tenant_repo.clone(),
            self.runtime_credentials.clone(),
            self.release_repo.clone(),
            self.adapter.clone(),
        )
        .with_compatibility_target(target)
    }

    async fn insert_org_with_membership(
        &self,
        slug: &str,
        status: &str,
        coder_org_id: Option<String>,
    ) -> OrganizationId {
        let org = OrganizationId::new();
        let mut tx = self.pool.begin().await.unwrap();
        sqlx::query(
            "INSERT INTO organizations
                (id, slug, display_name, owner_user_id, status, coder_organization_id, model_capability_ok, provisioner_status)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
        )
        .bind(org.0)
        .bind(slug)
        .bind(slug)
        .bind(self.owner.0)
        .bind(status)
        .bind(coder_org_id)
        .bind(status == "ready")
        .bind(if status == "ready" { Some("ready") } else { None })
        .execute(&mut *tx)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO memberships (organization_id, user_id, role, status)
             VALUES ($1, $2, 'admin', 'active')",
        )
        .bind(org.0)
        .bind(self.owner.0)
        .execute(&mut *tx)
        .await
        .unwrap();
        tx.commit().await.unwrap();
        org
    }

    async fn create_ready_org(&self, slug: &str, install_id: i64, repo_id: i64) -> OrganizationId {
        let org = self
            .insert_org_with_membership(
                slug,
                "ready",
                Some(internal_org_name(OrganizationId::new())),
            )
            .await;
        // Ensure coder_organization_id matches internal_org_name(org).
        sqlx::query("UPDATE organizations SET coder_organization_id = $1 WHERE id = $2")
            .bind(internal_org_name(org))
            .bind(org.0)
            .execute(&self.pool)
            .await
            .unwrap();
        let conn_id = common::insert_connection(&self.pool, org.0, install_id)
            .await
            .unwrap();
        common::insert_repository(&self.pool, org.0, conn_id, repo_id, &format!("{slug}/repo"))
            .await
            .unwrap();
        org
    }

    async fn create_provisioning_org(&self, slug: &str) -> (OrganizationId, LeaseGuard) {
        let org = self
            .insert_org_with_membership(slug, "provisioning", None)
            .await;
        operations::create(
            &self.pool,
            &operations::NewOperation {
                organization_id: org,
                resource_type: "organization".into(),
                resource_id: Some(org.0),
                kind: "org.provision".into(),
                idempotency_ref: Some(format!("org-{slug}")),
            },
        )
        .await
        .unwrap();
        let guard =
            operations::claim_kind(&self.pool, &format!("worker-{slug}"), Some("org.provision"))
                .await
                .unwrap()
                .unwrap();
        (org, guard)
    }

    async fn import_and_approve(&self, artifacts: &ReleaseArtifacts) -> ReleaseId {
        let (release_id, _) = self
            .releases
            .import_release(artifacts.clone(), "test")
            .await
            .unwrap();
        self.release_repo
            .set_release_status(release_id, "approved", None)
            .await
            .unwrap();
        release_id
    }

    async fn register_and_heartbeat(&self, workspace_id: WorkspaceId) -> RuntimeScope {
        let (bootstrap_ref, _) = self
            .runtime_credentials
            .repo
            .live_bootstrap_ref(workspace_id)
            .await
            .unwrap()
            .expect("provisioning must inject a live bootstrap");
        let bootstrap =
            String::from_utf8(self.secret_provider.get(&bootstrap_ref).await.unwrap()).unwrap();
        let session = self.runtime_credentials.register(&bootstrap).await.unwrap();
        let scope = self
            .runtime_credentials
            .authenticate(&session.access_secret)
            .await
            .unwrap();
        RuntimeWorkspaceService::new(
            self.pool.clone(),
            self.tenant_repo.clone(),
            self.quotas.clone(),
        )
        .heartbeat(&scope)
        .await
        .unwrap();
        scope
    }

    async fn provision_nexus_until_running(
        &self,
        worker: &TenantProvisionWorker,
        org: OrganizationId,
        repo_id: i64,
        slug: &str,
        fleet_size: i32,
    ) -> (TenantId, WorkspaceId, RuntimeScope) {
        let resp = self
            .tenant_service
            .create_tenant(
                self.owner,
                org,
                repo_id,
                slug,
                fleet_size,
                &format!("key-{slug}"),
                "req",
            )
            .await
            .unwrap();
        let tenant_id = resp.resource_id;
        worker.run_once().await.unwrap();

        let nexus_uuid: uuid::Uuid =
            sqlx::query_scalar("SELECT id FROM workspaces WHERE tenant_id = $1 AND role = 'nexus'")
                .bind(tenant_id.0)
                .fetch_one(&self.pool)
                .await
                .unwrap();
        let nexus_id = WorkspaceId::from_uuid(nexus_uuid);
        let scope = self.register_and_heartbeat(nexus_id).await;
        sqlx::query(
            "UPDATE operations SET retry_at = NULL WHERE resource_id = $1 AND kind = 'tenant.provision'",
        )
        .bind(tenant_id.0)
        .execute(&self.pool)
        .await
        .unwrap();
        worker.run_once().await.unwrap();
        let state: String = sqlx::query_scalar("SELECT observed_state FROM tenants WHERE id = $1")
            .bind(tenant_id.0)
            .fetch_one(&self.pool)
            .await
            .unwrap();
        assert_eq!(state, "running", "tenant {slug} must reach running");
        (tenant_id, nexus_id, scope)
    }

    async fn provision_worker_until_running(
        &self,
        worker: &TenantProvisionWorker,
        nexus_scope: &RuntimeScope,
        role: Role,
        slot: i32,
        key: &str,
    ) -> WorkspaceId {
        let resp = RuntimeWorkspaceService::new(
            self.pool.clone(),
            self.tenant_repo.clone(),
            self.quotas.clone(),
        )
        .request_worker(
            nexus_scope,
            role.as_str(),
            Some(slot),
            None,
            None,
            None,
            key,
        )
        .await
        .unwrap();
        worker.run_once().await.unwrap();

        let ws_uuid: uuid::Uuid = resp.workspace_id.parse().unwrap();
        let ws_id = WorkspaceId::from_uuid(ws_uuid);
        self.register_and_heartbeat(ws_id).await;
        sqlx::query(
            "UPDATE operations SET retry_at = NULL WHERE resource_id = $1 AND kind = 'tenant.workspace'",
        )
        .bind(ws_id.0)
        .execute(&self.pool)
        .await
        .unwrap();
        worker.run_once().await.unwrap();
        let state: String =
            sqlx::query_scalar("SELECT observed_state FROM workspaces WHERE id = $1")
                .bind(ws_id.0)
                .fetch_one(&self.pool)
                .await
                .unwrap();
        assert_eq!(state, "running", "worker {role} must reach running");
        ws_id
    }
}

fn build_dual_backend_source_dir() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    for backend in ProvisionerBackend::ALL {
        for role in Role::ALL {
            let role_dir = dir.path().join(backend.as_str()).join(role.template_name());
            package_role_dir(
                &role_dir,
                &format!("resource \"role\" {{ backend = \"{backend}\" role = \"{role}\" }}\n"),
            );
        }
    }
    dir
}

#[test]
fn release_provider_packages_dual_backend_v2_manifest() {
    let dir = build_dual_backend_source_dir();
    let provider = ReleaseProvider::new(dir.path().to_path_buf(), None, None, "2026.11.0".into());
    let artifacts = provider.load().expect("dual-backend packaging succeeds");
    assert_eq!(
        artifacts.manifest.schema_version,
        TemplateManifest::CURRENT_V2_SCHEMA_VERSION
    );
    assert_eq!(artifacts.manifest.release, "2026.11.0");
    assert_eq!(artifacts.manifest.variants.len(), 10);
    for backend in ProvisionerBackend::ALL {
        for role in Role::ALL {
            assert!(artifacts.manifest.variant(backend, role).is_some());
            assert!(artifacts.archive_for(backend, role).is_some());
        }
    }
    let reparsed =
        TemplateManifest::parse(&artifacts.manifest.to_canonical_json().unwrap()).unwrap();
    assert_eq!((reparsed.schema_version, reparsed.variants.len()), (2, 10));
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn release_provider_dual_backend_import_roundtrip() {
    let env = TestEnv::new().await;
    let dir = build_dual_backend_source_dir();
    let provider = ReleaseProvider::new(dir.path().to_path_buf(), None, None, "2026.11.0".into());
    let artifacts = provider.load().unwrap();
    let (release_id, _) = env
        .releases
        .import_release(artifacts, "test")
        .await
        .unwrap();

    let (_, variants) = env
        .release_repo
        .get_release_variants(release_id)
        .await
        .unwrap()
        .expect("imported release must exist");
    assert_eq!(variants.len(), 10);
    for backend in ProvisionerBackend::ALL {
        for role in Role::ALL {
            assert!(variants.contains_key(&(backend, role)));
        }
    }
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn all_five_roles_provision_on_docker_and_kubernetes() {
    let env = TestEnv::new().await;
    let (artifacts, _dir) = build_dual_backend_v2_artifacts(&VariantSpec {
        release_name: "2026.11.0",
        architecture: "amd64",
        parameter_schema_version: 2,
        image_digest: Some(digest_char('a')),
        workspace_runtime_release: Some("2026.11.0"),
    });
    let release_id = env.import_and_approve(&artifacts).await;

    let docker_target = ReleaseCompatibilityTarget::docker_default();
    let k8s_target = ReleaseCompatibilityTarget::kubernetes_default("amd64");

    let docker_org = env.create_ready_org("docker-org", 101, 201).await;
    let k8s_org = env.create_ready_org("k8s-org", 102, 202).await;

    for (org, target, expected_backend) in [
        (docker_org, &docker_target, "docker"),
        (k8s_org, &k8s_target, "kubernetes"),
    ] {
        env.releases
            .promote_to_org_for_backend(org, release_id, target, env.adapter.as_ref(), &artifacts)
            .await
            .unwrap();
        let backends: Vec<(String, String)> = sqlx::query_as(
            "SELECT role, backend FROM organization_template_versions
              WHERE organization_id = $1 AND release_id = $2
              ORDER BY role",
        )
        .bind(org.0)
        .bind(release_id.0)
        .fetch_all(&env.pool)
        .await
        .unwrap();
        assert_eq!(backends.len(), 5);
        assert!(backends.iter().all(|(_, b)| b == expected_backend));
    }

    for (org, repo_id, target, slug, expected_backend) in [
        (docker_org, 201, docker_target, "docker-t", "docker"),
        (k8s_org, 202, k8s_target, "k8s-t", "kubernetes"),
    ] {
        let worker = env.tenant_worker(target.clone());
        let (tenant_id, nexus_id, nexus_scope) = env
            .provision_nexus_until_running(&worker, org, repo_id, slug, 5)
            .await;

        let mut ws_ids = vec![(Role::Nexus, nexus_id)];
        for (slot, role) in [Role::Forge, Role::Sentinel, Role::Vessel, Role::Lore]
            .into_iter()
            .enumerate()
        {
            let ws_id = env
                .provision_worker_until_running(
                    &worker,
                    &nexus_scope,
                    role,
                    (slot + 1) as i32,
                    &format!("{slug}-{}", role.as_str()),
                )
                .await;
            ws_ids.push((role, ws_id));
        }

        for (role, ws_id) in ws_ids {
            let expected_ver = env
                .release_repo
                .get_org_template_version_for_backend(org, release_id, target.backend, role)
                .await
                .unwrap()
                .unwrap()
                .coder_version_id
                .unwrap();
            assert_pins(
                &env.pool,
                tenant_id,
                ws_id,
                release_id,
                expected_backend,
                &expected_ver,
            )
            .await;
        }
    }
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn early_rejection_before_workspace_creation() {
    let env = TestEnv::new().await;
    let k8s_amd64 = ReleaseCompatibilityTarget::kubernetes_default("amd64");

    // 1. Docker-only v1 release rejected on Kubernetes at OrgProvisionWorker,
    //    publish_for_org_backend, and TenantProvisionWorker before create_workspace.
    let (v1_artifacts, _d1) =
        build_docker_only_v1_artifacts("2026.10.0-v1", Some(digest_char('a')));
    let v1_id = env.import_and_approve(&v1_artifacts).await;

    let (prov_org, prov_guard) = env.create_provisioning_org("k8s-v1-org").await;
    let org_worker = OrgProvisionWorker::new(
        env.pool.clone(),
        env.orgs.clone(),
        env.releases.clone(),
        env.release_repo.clone(),
        env.adapter.clone(),
        Some(Arc::new(v1_artifacts.clone())),
        Some(v1_id),
        "kubernetes".to_string(),
        "v2.37.3".to_string(),
    )
    .with_compatibility_target(k8s_amd64.clone());
    let org_outcome = org_worker.provision(&prov_guard).await.unwrap();
    assert!(matches!(
        org_outcome,
        OrgProvisionOutcome::NotReady(id, "RELEASE_NOT_CONFIGURED") if id == prov_org
    ));

    let ready_org = env.create_ready_org("k8s-ready", 110, 210).await;
    assert!(env
        .releases
        .publish_for_org_backend(
            ready_org,
            v1_id,
            &k8s_amd64,
            env.adapter.as_ref(),
            &v1_artifacts
        )
        .await
        .is_err());

    // Bind v1 release to ready_org so TenantProvisionWorker hits the variant check.
    env.release_repo
        .bind_org_release(ready_org, v1_id)
        .await
        .unwrap();
    env.adapter.reset_create_calls();
    env.tenant_service
        .create_tenant(env.owner, ready_org, 210, "t-v1-k8s", 1, "k-v1", "req")
        .await
        .unwrap();
    let t_guard = operations::claim_kind(&env.pool, "w-v1", Some("tenant.provision"))
        .await
        .unwrap()
        .unwrap();
    let k8s_worker = env.tenant_worker(k8s_amd64.clone());
    assert!(k8s_worker.provision(&t_guard).await.is_err());
    assert_eq!(env.adapter.create_calls(), 0);
    let _ = prov_org;

    // 2. Architecture mismatch, runtime release mismatch, unsupported parameter_schema_version,
    //    and missing image_digest on Kubernetes / hosted Docker.
    let spec = |name: &'static str,
                arch: &'static str,
                schema: u32,
                d: Option<char>,
                rt: Option<&'static str>| {
        VariantSpec {
            release_name: name,
            architecture: arch,
            parameter_schema_version: schema,
            image_digest: d.map(digest_char),
            workspace_runtime_release: rt,
        }
    };
    let mut rt_target = ReleaseCompatibilityTarget::kubernetes_default("amd64");
    rt_target.expected_runtime_release = Some("2026.11.0".to_string());
    let mut hosted_docker = ReleaseCompatibilityTarget::docker_default();
    hosted_docker.require_image_digest = true;

    let bad_specs = [
        (
            "arm64-mismatch",
            spec("2026.11.0-arm64", "arm64", 2, Some('b'), Some("2026.11.0")),
            ReleaseCompatibilityTarget::kubernetes_default("amd64"),
        ),
        (
            "runtime-mismatch",
            spec("2026.11.0-rt", "amd64", 2, Some('c'), Some("2026.10.0")),
            rt_target,
        ),
        (
            "schema-v3",
            spec("2026.11.0-schema3", "amd64", 3, Some('d'), None),
            ReleaseCompatibilityTarget::kubernetes_default("amd64"),
        ),
        (
            "missing-digest-k8s",
            spec("2026.11.0-nodigest", "amd64", 2, None, None),
            ReleaseCompatibilityTarget::kubernetes_default("amd64"),
        ),
        (
            "missing-digest-hosted-docker",
            spec("2026.11.0-hosted-nodigest", "amd64", 2, None, None),
            hosted_docker,
        ),
    ];

    for (idx, (label, spec, target)) in bad_specs.into_iter().enumerate() {
        let (artifacts, _dir) = build_dual_backend_v2_artifacts(&spec);
        let rel_id = env.import_and_approve(&artifacts).await;
        let org = env
            .create_ready_org(&format!("bad-{idx}"), 120 + idx as i64, 220 + idx as i64)
            .await;
        assert!(
            env.releases
                .publish_for_org_backend(org, rel_id, &target, env.adapter.as_ref(), &artifacts)
                .await
                .is_err(),
            "{label}: publish_for_org_backend must reject"
        );
        env.release_repo
            .bind_org_release(org, rel_id)
            .await
            .unwrap();
        env.adapter.reset_create_calls();
        env.tenant_service
            .create_tenant(
                env.owner,
                org,
                220 + idx as i64,
                &format!("t-bad-{idx}"),
                1,
                &format!("k-bad-{idx}"),
                "req",
            )
            .await
            .unwrap();
        let guard =
            operations::claim_kind(&env.pool, &format!("wb-{idx}"), Some("tenant.provision"))
                .await
                .unwrap()
                .unwrap();
        assert!(
            env.tenant_worker(target).provision(&guard).await.is_err(),
            "{label}: TenantProvisionWorker must reject"
        );
        assert_eq!(
            env.adapter.create_calls(),
            0,
            "{label}: must not call create_workspace"
        );
    }

    // 3. Cross-backend mutation rejected: tenant pinned to Docker cannot provision on Kubernetes.
    let (dual_artifacts, _d2) = build_dual_backend_v2_artifacts(&VariantSpec {
        release_name: "2026.11.0-dual",
        architecture: "amd64",
        parameter_schema_version: 2,
        image_digest: Some(digest_char('e')),
        workspace_runtime_release: Some("2026.11.0"),
    });
    let dual_id = env.import_and_approve(&dual_artifacts).await;
    let org = env.create_ready_org("cross-backend", 150, 250).await;
    env.releases
        .promote_to_org_for_backend(
            org,
            dual_id,
            &ReleaseCompatibilityTarget::docker_default(),
            env.adapter.as_ref(),
            &dual_artifacts,
        )
        .await
        .unwrap();
    let docker_worker = env.tenant_worker(ReleaseCompatibilityTarget::docker_default());
    let (_tenant_id, _nexus_id, nexus_scope) = env
        .provision_nexus_until_running(&docker_worker, org, 250, "docker-pinned", 2)
        .await;

    let resp = RuntimeWorkspaceService::new(
        env.pool.clone(),
        env.tenant_repo.clone(),
        env.quotas.clone(),
    )
    .request_worker(
        &nexus_scope,
        "forge",
        Some(1),
        None,
        None,
        None,
        "cross-forge",
    )
    .await
    .unwrap();
    let op_id: uuid::Uuid = resp.operation_id.parse().unwrap();
    env.adapter.reset_create_calls();
    env.tenant_worker(ReleaseCompatibilityTarget::kubernetes_default("amd64"))
        .run_once()
        .await
        .unwrap();
    let err_code: Option<String> =
        sqlx::query_scalar("SELECT error_code FROM operations WHERE id = $1")
            .bind(op_id)
            .fetch_one(&env.pool)
            .await
            .unwrap();
    assert_eq!(err_code.as_deref(), Some("RELEASE_NOT_RESOLVED"));
    assert_eq!(env.adapter.create_calls(), 0);
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn safe_upgrade_and_rollback_for_docker_workspaces() {
    let env = TestEnv::new().await;
    let docker_target = ReleaseCompatibilityTarget::docker_default();
    let org = env.create_ready_org("upgrade-org", 160, 260).await;
    let conn_id: uuid::Uuid =
        sqlx::query_scalar("SELECT id FROM github_connections WHERE organization_id = $1")
            .bind(org.0)
            .fetch_one(&env.pool)
            .await
            .unwrap();
    for (repo_id, name) in [(261, "upgrade-org/repo-b"), (262, "upgrade-org/repo-c")] {
        common::insert_repository(&env.pool, org.0, conn_id, repo_id, name)
            .await
            .unwrap();
    }

    let (r1_artifacts, _d1) =
        build_docker_only_v1_artifacts("2026.10.0-r1", Some(digest_char('1')));
    let r1_id = env.import_and_approve(&r1_artifacts).await;
    env.releases
        .promote_to_org_for_backend(
            org,
            r1_id,
            &docker_target,
            env.adapter.as_ref(),
            &r1_artifacts,
        )
        .await
        .unwrap();

    let worker = env.tenant_worker(docker_target.clone());
    let (tenant_a, ws_a, _) = env
        .provision_nexus_until_running(&worker, org, 260, "tenant-a", 1)
        .await;
    let v1_id = env
        .release_repo
        .get_org_template_version_for_backend(org, r1_id, ProvisionerBackend::Docker, Role::Nexus)
        .await
        .unwrap()
        .unwrap()
        .coder_version_id
        .unwrap();

    // Promote R2 to the org.
    let (r2_artifacts, _d2) = build_dual_backend_v2_artifacts(&VariantSpec {
        release_name: "2026.11.0-r2",
        architecture: "amd64",
        parameter_schema_version: 2,
        image_digest: Some(digest_char('2')),
        workspace_runtime_release: Some("2026.11.0"),
    });
    let r2_id = env.import_and_approve(&r2_artifacts).await;
    env.releases
        .promote_to_org_for_backend(
            org,
            r2_id,
            &docker_target,
            env.adapter.as_ref(),
            &r2_artifacts,
        )
        .await
        .unwrap();
    let v2_id = env
        .release_repo
        .get_org_template_version_for_backend(org, r2_id, ProvisionerBackend::Docker, Role::Nexus)
        .await
        .unwrap()
        .unwrap()
        .coder_version_id
        .unwrap();
    assert_ne!(v1_id, v2_id);

    // Tenant A and its workspace remain pinned to R1 / v1_id.
    assert_pins(&env.pool, tenant_a, ws_a, r1_id, "docker", &v1_id).await;

    // New Tenant B picks up R2.
    let (tenant_b, ws_b, _) = env
        .provision_nexus_until_running(&worker, org, 261, "tenant-b", 1)
        .await;
    assert_pins(&env.pool, tenant_b, ws_b, r2_id, "docker", &v2_id).await;

    // Roll back org binding to R1; new Tenant C picks up R1 cleanly, while B stays on R2.
    env.releases
        .promote_to_org_for_backend(
            org,
            r1_id,
            &docker_target,
            env.adapter.as_ref(),
            &r1_artifacts,
        )
        .await
        .unwrap();
    let (tenant_c, ws_c, _) = env
        .provision_nexus_until_running(&worker, org, 262, "tenant-c", 1)
        .await;
    assert_pins(&env.pool, tenant_c, ws_c, r1_id, "docker", &v1_id).await;
    assert_pins(&env.pool, tenant_b, ws_b, r2_id, "docker", &v2_id).await;
}

async fn assert_pins(
    pool: &sqlx::PgPool,
    tenant_id: TenantId,
    ws_id: WorkspaceId,
    expected_release: ReleaseId,
    expected_backend: &str,
    expected_version_id: &str,
) {
    let t_pin: (Option<uuid::Uuid>, Option<String>) =
        sqlx::query_as("SELECT pinned_release_id, pinned_backend FROM tenants WHERE id = $1")
            .bind(tenant_id.0)
            .fetch_one(pool)
            .await
            .unwrap();
    assert_eq!(
        (t_pin.0, t_pin.1.as_deref()),
        (Some(expected_release.0), Some(expected_backend))
    );
    let w_pin: (Option<uuid::Uuid>, Option<String>, Option<String>) = sqlx::query_as(
        "SELECT pinned_release_id, pinned_backend, pinned_version_id FROM workspaces WHERE id = $1",
    )
    .bind(ws_id.0)
    .fetch_one(pool)
    .await
    .unwrap();
    assert_eq!(
        (w_pin.0, w_pin.1.as_deref(), w_pin.2.as_deref()),
        (
            Some(expected_release.0),
            Some(expected_backend),
            Some(expected_version_id)
        )
    );
}

//! Regression tests for Coder template version isolation (same-org Docker +
//! Kubernetes publishing), legacy NULL backend pin handling, migration 0024
//! backfill safety, and tenant/workspace backend pinning.
//!
//! Run with:
//! `cargo test -p openflows-manager --test backend_collision_and_pinning -- --ignored`

mod common;

use axum::{routing::get, routing::post, Json, Router};
use common::TestDb;
use openflows_manager::coder::RealCoderAdapter;
use openflows_manager::error::ManagerError;
use openflows_manager::id::{OrganizationId, ReleaseId, TenantId, WorkspaceId};
use openflows_manager::provisioning::{
    record_workspace_and_tenant_pin, resolve_nexus_release_for_tenant,
    resolve_worker_release_for_tenant,
};
use openflows_manager::templates::packaging::{
    build_deterministic_archive, sha256_hex, Role, RoleManifestEntry, TemplateManifest,
};
use openflows_manager::templates::repository::ReleaseRepository;
use openflows_manager::templates::service::{
    version_name_for_backend, ReleaseArtifacts, ReleaseService,
};
use openflows_manager::templates::{ProvisionerBackend, ReleaseCompatibilityTarget};
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};

fn digest_char(ch: char) -> String {
    format!("sha256:{}", std::iter::repeat_n(ch, 64).collect::<String>())
}

fn build_dual_backend_artifacts(release_name: &str) -> (ReleaseArtifacts, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let mut roles = BTreeMap::new();
    let mut variants = BTreeMap::new();
    let mut archives = BTreeMap::new();

    for backend in ProvisionerBackend::ALL {
        for role in Role::ALL {
            let role_dir = dir.path().join(backend.as_str()).join(role.template_name());
            std::fs::create_dir_all(&role_dir).unwrap();
            std::fs::write(
                role_dir.join("main.tf"),
                format!("resource \"role\" {{ backend = \"{backend}\" role = \"{role}\" }}\n"),
            )
            .unwrap();
            let archive = build_deterministic_archive(&role_dir).unwrap();
            let entry = RoleManifestEntry {
                role,
                backend,
                archive: role.archive_name_for_backend(backend),
                sha256: sha256_hex(&archive),
                parameter_schema_version: 2,
                architecture: "amd64".to_string(),
                image_repository: Some("ghcr.io/openflows/workspace-runtime".to_string()),
                image_digest: Some(digest_char('d')),
                workspace_runtime_release: Some("2026.11.0".to_string()),
            };
            if backend == ProvisionerBackend::Docker {
                roles.insert(role, entry.clone());
            }
            variants.insert((backend, role), entry);
            archives.insert((backend, role), archive);
        }
    }

    let manifest = TemplateManifest {
        schema_version: TemplateManifest::CURRENT_V2_SCHEMA_VERSION,
        release: release_name.to_string(),
        required_roles: Role::ALL.to_vec(),
        roles,
        variants,
    };
    (
        ReleaseArtifacts::from_backend_archives(manifest, archives),
        dir,
    )
}

#[derive(Default)]
struct FakeCoderState {
    templates: Vec<Value>,
    versions: Vec<Value>,
    uploads: HashMap<String, usize>,
    next_id: usize,
}

async fn start_fake_coder(
    state: Arc<Mutex<FakeCoderState>>,
) -> (RealCoderAdapter, tokio::task::JoinHandle<()>) {
    let s_tpl = state.clone();
    let s_create_tpl = state.clone();
    let s_list_ver = state.clone();
    let s_create_ver = state.clone();
    let s_get_ver = state.clone();
    let s_upload = state.clone();

    let router = Router::new()
        .route(
            "/api/v2/organizations/{org}/templates",
            get(move || {
                let s = s_tpl.clone();
                async move { Json(s.lock().unwrap().templates.clone()) }
            })
            .post(
                move |axum::extract::Path(org): axum::extract::Path<String>,
                      Json(body): Json<Value>| {
                    let s = s_create_tpl.clone();
                    async move {
                        let mut g = s.lock().unwrap();
                        g.next_id += 1;
                        let id = format!("tpl-{}", g.next_id);
                        if let Some(active_id) = body
                            .get("template_version_id")
                            .or_else(|| body.get("active_version_id"))
                            .and_then(|v| v.as_str())
                        {
                            for v in &mut g.versions {
                                if v["id"] == active_id {
                                    v["template_id"] = json!(id);
                                }
                            }
                        }
                        let tpl = json!({"id": id, "name": body["name"], "organization_id": org});
                        g.templates.push(tpl.clone());
                        Json(tpl)
                    }
                },
            ),
        )
        .route(
            "/api/v2/templates/{id}/versions",
            get(move |axum::extract::Path(tid): axum::extract::Path<String>| {
                let s = s_list_ver.clone();
                async move {
                    let vers = s
                        .lock()
                        .unwrap()
                        .versions
                        .iter()
                        .filter(|v| v["template_id"] == tid)
                        .cloned()
                        .collect::<Vec<_>>();
                    Json(vers)
                }
            }),
        )
        .route(
            "/api/v2/files",
            post(move |body: axum::body::Bytes| {
                let s = s_upload.clone();
                async move {
                    let hash = sha256_hex(&body);
                    *s.lock().unwrap().uploads.entry(hash.clone()).or_default() += 1;
                    Json(json!({"hash": hash}))
                }
            }),
        )
        .route(
            "/api/v2/organizations/{org}/templateversions",
            post(move |axum::extract::Path(org): axum::extract::Path<String>, Json(body): Json<Value>| {
                let s = s_create_ver.clone();
                async move {
                    let mut g = s.lock().unwrap();
                    g.next_id += 1;
                    let id = format!("ver-{}", g.next_id);
                    let ver = json!({
                        "id": id,
                        "name": body["name"],
                        "template_id": body.get("template_id").cloned().unwrap_or(json!("tpl-1")),
                        "organization_id": org,
                        "job": {"id": format!("job-{}", g.next_id), "status": "succeeded"}
                    });
                    g.versions.push(ver.clone());
                    Json(ver)
                }
            }),
        )
        .route(
            "/api/v2/templateversions/{id}",
            get(move |axum::extract::Path(id): axum::extract::Path<String>| {
                let s = s_get_ver.clone();
                async move {
                    let ver = s
                        .lock()
                        .unwrap()
                        .versions
                        .iter()
                        .find(|v| v["id"] == id)
                        .cloned()
                        .unwrap();
                    Json(ver)
                }
            }),
        );

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let client = coder_client::CoderClient::new(&format!("http://{addr}"), "test-token");
    let adapter = RealCoderAdapter::new(client, "2.25.0");
    (adapter, server)
}

async fn seed_org_with_coder(
    pool: &sqlx::PgPool,
    slug: &str,
    coder_org: &str,
) -> (OrganizationId, uuid::Uuid, i64) {
    let user = common::insert_user(pool).await.unwrap();
    let org_uuid = common::insert_organization_with_owner(pool, slug, user)
        .await
        .unwrap();
    sqlx::query("UPDATE organizations SET coder_organization_id = $2 WHERE id = $1")
        .bind(org_uuid)
        .bind(coder_org)
        .execute(pool)
        .await
        .unwrap();
    let conn = common::insert_connection(pool, org_uuid, 100)
        .await
        .unwrap();
    let repo_id = 200;
    common::insert_repository(pool, org_uuid, conn, repo_id, "test/repo")
        .await
        .unwrap();
    (OrganizationId::from_uuid(org_uuid), conn, repo_id)
}

static NEXT_REPO_ID: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(300);

async fn seed_tenant(
    pool: &sqlx::PgPool,
    org_id: OrganizationId,
    conn: uuid::Uuid,
    _repo_id: i64,
    slug: &str,
) -> TenantId {
    let repo_id = NEXT_REPO_ID.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    common::insert_repository(pool, org_id.0, conn, repo_id, &format!("test/{slug}"))
        .await
        .unwrap();
    TenantId::from_uuid(
        common::insert_running_tenant(pool, org_id.0, conn, repo_id, slug)
            .await
            .unwrap(),
    )
}

async fn seed_workspace(
    pool: &sqlx::PgPool,
    org_id: OrganizationId,
    tenant_id: TenantId,
    pinned_version_id: Option<&str>,
    pinned_backend: Option<&str>,
    pinned_release_id: Option<ReleaseId>,
) -> WorkspaceId {
    let ws_uuid = common::insert_running_workspace(pool, org_id.0, tenant_id.0, "nexus", 0)
        .await
        .unwrap();
    sqlx::query(
        "UPDATE workspaces
            SET pinned_version_id = $2,
                pinned_backend = $3,
                pinned_release_id = $4
          WHERE id = $1",
    )
    .bind(ws_uuid)
    .bind(pinned_version_id)
    .bind(pinned_backend)
    .bind(pinned_release_id.map(|r| r.0))
    .execute(pool)
    .await
    .unwrap();
    WorkspaceId::from_uuid(ws_uuid)
}

#[tokio::test]
#[ignore = "requires PostgreSQL"]
async fn same_org_docker_and_kubernetes_publish_without_version_collision_and_remain_idempotent() {
    let db = TestDb::new().await.unwrap();
    db.migrate().await.unwrap();
    let pool = db.pool();
    let repo = ReleaseRepository::new(pool.clone());
    let service = ReleaseService::new(pool.clone(), repo.clone());
    let (org_id, _, _) = seed_org_with_coder(pool, "shared-coder-org", "shared-org").await;

    let (artifacts, _dir) = build_dual_backend_artifacts("2026.11.1-dual");
    let (artifacts_copy, _dir2) = build_dual_backend_artifacts("2026.11.1-dual");
    let (release_id, _) = service
        .import_release(artifacts_copy, "local://dual")
        .await
        .unwrap();
    repo.approve_configured_release(release_id).await.unwrap();

    // Seed a pre-existing legacy Docker version in Coder named `{release_id}`
    // on `openflows-nexus` so we also verify existing Docker versions are reused.
    let legacy_ver_name = release_id.to_string();
    let state = Arc::new(Mutex::new(FakeCoderState {
        templates: vec![json!({
            "id": "tpl-nexus",
            "name": "openflows-nexus",
            "organization_id": "shared-org"
        })],
        versions: vec![json!({
            "id": "ver-legacy-docker",
            "name": legacy_ver_name,
            "template_id": "tpl-nexus",
            "organization_id": "shared-org",
            "job": {"id": "job-legacy", "status": "succeeded"}
        })],
        ..Default::default()
    }));
    let (adapter, server) = start_fake_coder(state.clone()).await;

    let docker_target = ReleaseCompatibilityTarget::docker_default();
    let k8s_target = ReleaseCompatibilityTarget::kubernetes_default("amd64");

    // Publish Docker and Kubernetes into the SAME Coder organization.
    service
        .publish_for_org_backend(org_id, release_id, &docker_target, &adapter, &artifacts)
        .await
        .unwrap();
    service
        .publish_for_org_backend(org_id, release_id, &k8s_target, &adapter, &artifacts)
        .await
        .unwrap();

    let docker_nexus = repo
        .get_org_template_version_for_backend(
            org_id,
            release_id,
            ProvisionerBackend::Docker,
            Role::Nexus,
        )
        .await
        .unwrap()
        .unwrap();
    let k8s_nexus = repo
        .get_org_template_version_for_backend(
            org_id,
            release_id,
            ProvisionerBackend::Kubernetes,
            Role::Nexus,
        )
        .await
        .unwrap()
        .unwrap();

    // Existing Docker version must be reused, and Kubernetes must get its own distinct version.
    assert_eq!(
        docker_nexus.coder_version_id.as_deref(),
        Some("ver-legacy-docker")
    );
    assert_ne!(k8s_nexus.coder_version_id, docker_nexus.coder_version_id);

    // Every role must have distinct Coder version IDs and distinct version names.
    for role in Role::ALL {
        let d = repo
            .get_org_template_version_for_backend(
                org_id,
                release_id,
                ProvisionerBackend::Docker,
                role,
            )
            .await
            .unwrap()
            .unwrap();
        let k = repo
            .get_org_template_version_for_backend(
                org_id,
                release_id,
                ProvisionerBackend::Kubernetes,
                role,
            )
            .await
            .unwrap()
            .unwrap();
        assert_ne!(d.coder_version_id, k.coder_version_id);
    }

    let count_after_first_publish = state.lock().unwrap().versions.len();
    assert_eq!(
        count_after_first_publish,
        Role::ALL.len() * 2,
        "unexpected versions: {:?}",
        state.lock().unwrap().versions
    );

    // Re-publishing both backends in the same org must be idempotent (no new versions created).
    service
        .publish_for_org_backend(org_id, release_id, &docker_target, &adapter, &artifacts)
        .await
        .unwrap();
    service
        .publish_for_org_backend(org_id, release_id, &k8s_target, &adapter, &artifacts)
        .await
        .unwrap();
    assert_eq!(
        state.lock().unwrap().versions.len(),
        count_after_first_publish
    );
    assert_eq!(
        version_name_for_backend(release_id, ProvisionerBackend::Docker),
        release_id.to_string()
    );
    assert_eq!(
        version_name_for_backend(release_id, ProvisionerBackend::Kubernetes),
        format!("{release_id}-kubernetes")
    );

    server.abort();
}

#[tokio::test]
#[ignore = "requires PostgreSQL"]
async fn legacy_null_backend_pin_and_workspace_isolation() {
    let db = TestDb::new().await.unwrap();
    db.migrate().await.unwrap();
    let pool = db.pool();
    let repo = ReleaseRepository::new(pool.clone());
    let service = ReleaseService::new(pool.clone(), repo.clone());
    let (org_id, conn, repo_id) = seed_org_with_coder(pool, "legacy-pin-org", "legacy-org").await;

    let (artifacts, _dir) = build_dual_backend_artifacts("2026.11.2-dual");
    let (release_id, _) = service
        .import_release(artifacts, "local://dual-2")
        .await
        .unwrap();
    repo.approve_configured_release(release_id).await.unwrap();
    repo.bind_org_release(org_id, release_id).await.unwrap();

    for backend in ProvisionerBackend::ALL {
        for role in Role::ALL {
            repo.upsert_org_template_version_for_backend(
                org_id,
                release_id,
                backend,
                role,
                &format!("tpl-{}", role.as_str()),
                &format!("ver-{backend}-{}", role.as_str()),
                "job-1",
                "ready",
                None,
            )
            .await
            .unwrap();
        }
    }

    let docker_target = ReleaseCompatibilityTarget::docker_default();
    let k8s_target = ReleaseCompatibilityTarget::kubernetes_default("amd64");

    // 1. Legacy tenant with pinned_release_id != NULL and pinned_backend = NULL.
    let legacy_tenant = seed_tenant(pool, org_id, conn, repo_id, "legacy-null-backend").await;
    sqlx::query("UPDATE tenants SET pinned_release_id = $2, pinned_backend = NULL WHERE id = $1")
        .bind(legacy_tenant.0)
        .bind(release_id.0)
        .execute(pool)
        .await
        .unwrap();

    let err = resolve_nexus_release_for_tenant(pool, &repo, org_id, legacy_tenant, &k8s_target)
        .await
        .unwrap_err();
    assert!(
        matches!(err, ManagerError::Api(ref e) if e.code == "RELEASE_NOT_RESOLVED"),
        "expected RELEASE_NOT_RESOLVED, got {err:?}"
    );
    assert!(
        !repo
            .set_tenant_pin_if_unset(
                org_id,
                legacy_tenant,
                release_id,
                ProvisionerBackend::Kubernetes
            )
            .await
            .unwrap(),
        "legacy NULL backend pin must never initialize to kubernetes"
    );
    let ok_docker =
        resolve_nexus_release_for_tenant(pool, &repo, org_id, legacy_tenant, &docker_target)
            .await
            .unwrap()
            .unwrap();
    assert_eq!(ok_docker.backend, ProvisionerBackend::Docker);

    // 2. Legacy Docker workspace (pinned_backend = NULL, pinned_version_id != NULL) blocks Kubernetes on that tenant.
    let ws_tenant = seed_tenant(pool, org_id, conn, repo_id, "legacy-ws-null").await;
    let legacy_ws = seed_workspace(pool, org_id, ws_tenant, Some("legacy-ver-1"), None, None).await;
    let err = resolve_nexus_release_for_tenant(pool, &repo, org_id, ws_tenant, &k8s_target)
        .await
        .unwrap_err();
    assert!(matches!(err, ManagerError::Api(ref e) if e.code == "RELEASE_NOT_RESOLVED"));
    assert!(
        !repo
            .set_workspace_pin(
                org_id,
                legacy_ws,
                release_id,
                ProvisionerBackend::Kubernetes,
                Some("ver-k8s"),
            )
            .await
            .unwrap(),
        "legacy workspace must not silently switch to kubernetes"
    );

    // 3. Tenant pinned to Docker rejects Kubernetes.
    let docker_tenant = seed_tenant(pool, org_id, conn, repo_id, "docker-pinned").await;
    repo.set_tenant_pin_if_unset(
        org_id,
        docker_tenant,
        release_id,
        ProvisionerBackend::Docker,
    )
    .await
    .unwrap();
    let err = resolve_worker_release_for_tenant(
        pool,
        &repo,
        org_id,
        docker_tenant,
        Role::Forge,
        &k8s_target,
    )
    .await
    .unwrap_err();
    assert!(matches!(err, ManagerError::Api(ref e) if e.code == "RELEASE_NOT_RESOLVED"));

    // 4. Tenant pinned to Kubernetes rejects Docker.
    let k8s_tenant = seed_tenant(pool, org_id, conn, repo_id, "k8s-pinned").await;
    repo.set_tenant_pin_if_unset(
        org_id,
        k8s_tenant,
        release_id,
        ProvisionerBackend::Kubernetes,
    )
    .await
    .unwrap();
    let err = resolve_worker_release_for_tenant(
        pool,
        &repo,
        org_id,
        k8s_tenant,
        Role::Forge,
        &docker_target,
    )
    .await
    .unwrap_err();
    assert!(matches!(err, ManagerError::Api(ref e) if e.code == "RELEASE_NOT_RESOLVED"));

    // 5. New tenants receive the intended configured backend pin.
    let fresh_k8s_tenant = seed_tenant(pool, org_id, conn, repo_id, "fresh-k8s").await;
    let fresh_ws = seed_workspace(pool, org_id, fresh_k8s_tenant, None, None, None).await;
    let spec = resolve_nexus_release_for_tenant(pool, &repo, org_id, fresh_k8s_tenant, &k8s_target)
        .await
        .unwrap()
        .unwrap();
    record_workspace_and_tenant_pin(pool, &repo, org_id, fresh_k8s_tenant, fresh_ws, &spec)
        .await
        .unwrap();
    let (_, pinned_backend) = repo
        .get_tenant_pin(org_id, fresh_k8s_tenant)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(pinned_backend, Some(ProvisionerBackend::Kubernetes));
}

#[tokio::test]
#[ignore = "requires PostgreSQL"]
async fn migration_0024_backfills_legacy_docker_pins_on_existing_db() {
    let db = TestDb::new().await.unwrap();
    let pool = db.pool();

    // Apply migrations 0001..=0023 (pre-0024 schema) directly.
    let migrator = sqlx::migrate!("./migrations");
    for m in migrator.iter() {
        if m.version < 24 {
            sqlx::raw_sql(&m.sql).execute(pool).await.unwrap();
        }
    }

    let (org_id, conn, repo_id) = seed_org_with_coder(pool, "mig-0024-org", "mig-org").await;
    let release_id = ReleaseId::new();
    sqlx::query(
        "INSERT INTO template_releases (id, version, artifact_uri, manifest_sha256, status)
         VALUES ($1, '2026.11.3-mig', 'local://mig', $2, 'approved')",
    )
    .bind(release_id.0)
    .bind("a".repeat(64))
    .execute(pool)
    .await
    .unwrap();

    let pinned_legacy_t = seed_tenant(pool, org_id, conn, repo_id, "mig-pinned").await;
    sqlx::query("UPDATE tenants SET pinned_release_id = $2 WHERE id = $1")
        .bind(pinned_legacy_t.0)
        .bind(release_id.0)
        .execute(pool)
        .await
        .unwrap();

    let ws_legacy_t = seed_tenant(pool, org_id, conn, repo_id, "mig-has-ws").await;
    let legacy_ws = WorkspaceId::from_uuid(
        common::insert_running_workspace(pool, org_id.0, ws_legacy_t.0, "nexus", 0)
            .await
            .unwrap(),
    );

    let unprovisioned_t = seed_tenant(pool, org_id, conn, repo_id, "mig-fresh").await;

    // Now apply migration 0024 against the existing pre-0024 database state.
    let migration_sql = include_str!("../migrations/0024_template_release_backend_variants.sql");
    sqlx::raw_sql(migration_sql).execute(pool).await.unwrap();

    let repo = ReleaseRepository::new(pool.clone());
    assert_eq!(
        repo.get_tenant_pin(org_id, pinned_legacy_t).await.unwrap(),
        Some((Some(release_id), Some(ProvisionerBackend::Docker)))
    );
    assert_eq!(
        repo.get_tenant_pin(org_id, ws_legacy_t).await.unwrap(),
        Some((None, Some(ProvisionerBackend::Docker)))
    );
    assert_eq!(
        repo.get_tenant_pin(org_id, unprovisioned_t).await.unwrap(),
        Some((None, None))
    );
    let ws_backend: Option<String> =
        sqlx::query_scalar("SELECT pinned_backend FROM workspaces WHERE id = $1")
            .bind(legacy_ws.0)
            .fetch_one(pool)
            .await
            .unwrap();
    assert_eq!(ws_backend.as_deref(), Some("docker"));
}

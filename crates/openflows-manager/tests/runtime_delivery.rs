//! Lifecycle tests for workspace credential delivery, independent of the durable store.
//!
//! Run with: `cargo test --test runtime_delivery -- --ignored`

mod common;

use async_trait::async_trait;
use common::{
    insert_connection, insert_organization_with_owner, insert_repository, insert_running_tenant,
    insert_running_workspace, insert_user, TestDb,
};
use openflows_manager::error::ManagerError;
use openflows_manager::id::{OrganizationId, TenantId, WorkspaceId};
use openflows_manager::runtime::delivery::{DeliveryTarget, SecretDelivery};
use openflows_manager::runtime::repository::RuntimeRepository;
use openflows_manager::runtime::tenant::TenantRuntimeRepository;
use openflows_manager::runtime::RuntimeCredentialService;
use openflows_manager::secrets::{InMemorySecretProvider, SecretProvider};
use serde_json::Value;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

/// Stands in for a Docker file or Kubernetes Secret: one bundle per workspace.
#[derive(Default)]
struct RecordingDelivery(Mutex<BTreeMap<WorkspaceId, Vec<u8>>>);

impl RecordingDelivery {
    fn bundle(&self, ws: WorkspaceId) -> Option<Value> {
        let map = self.0.lock().unwrap();
        map.get(&ws).map(|b| serde_json::from_slice(b).unwrap())
    }
    fn lose_everything(&self) {
        self.0.lock().unwrap().clear();
    }
}

#[async_trait]
impl SecretDelivery for RecordingDelivery {
    async fn deliver(
        &self,
        target: &DeliveryTarget,
        _store_ref: &str,
        bundle: &[u8],
    ) -> Result<String, ManagerError> {
        self.0
            .lock()
            .unwrap()
            .insert(target.workspace_id, bundle.to_vec());
        Ok(format!("delivered-{}", target.workspace_id))
    }
    async fn remove(&self, target: &DeliveryTarget) -> Result<(), ManagerError> {
        self.0.lock().unwrap().remove(&target.workspace_id);
        Ok(())
    }
}

struct Ctx {
    _db: TestDb,
    org: OrganizationId,
    tenant: TenantId,
    workspace: WorkspaceId,
    provider: Arc<InMemorySecretProvider>,
    delivery: Arc<RecordingDelivery>,
    tenants: TenantRuntimeRepository,
    creds: RuntimeCredentialService,
}

fn wire(
    pool: &sqlx::PgPool,
    provider: &Arc<InMemorySecretProvider>,
    delivery: &Arc<RecordingDelivery>,
) -> (TenantRuntimeRepository, RuntimeCredentialService) {
    let store: Arc<dyn SecretProvider> = provider.clone();
    let out: Arc<dyn SecretDelivery> = delivery.clone();
    (
        TenantRuntimeRepository::with_secret_provider(pool.clone(), store.clone())
            .with_delivery(out.clone()),
        RuntimeCredentialService::with_secret_provider(RuntimeRepository::new(pool.clone()), store)
            .with_delivery(out),
    )
}

/// A tenant with one nexus workspace plus its Redis and hook credentials.
async fn add_tenant(
    ctx: &Ctx,
    pool: &sqlx::PgPool,
    repo_id: i64,
    slug: &str,
) -> (TenantId, WorkspaceId) {
    insert_repository(
        pool,
        ctx.org.0,
        connection_of(pool, ctx.org).await,
        repo_id,
        &format!("acme/{slug}"),
    )
    .await
    .unwrap();
    let tenant = insert_running_tenant(
        pool,
        ctx.org.0,
        connection_of(pool, ctx.org).await,
        repo_id,
        slug,
    )
    .await
    .unwrap();
    let ws = insert_running_workspace(pool, ctx.org.0, tenant, "nexus", 0)
        .await
        .unwrap();
    let (tenant, ws) = (TenantId::from_uuid(tenant), WorkspaceId::from_uuid(ws));
    ctx.tenants
        .ensure_redis_credential(ctx.org, tenant, &format!("redis-{slug}"))
        .await
        .unwrap();
    ctx.tenants
        .ensure_hook_key(ctx.org, tenant, &format!("hook-{slug}"))
        .await
        .unwrap();
    (tenant, ws)
}

async fn connection_of(pool: &sqlx::PgPool, org: OrganizationId) -> uuid::Uuid {
    sqlx::query_scalar("SELECT id FROM github_connections WHERE organization_id = $1")
        .bind(org.0)
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn setup() -> Ctx {
    // The bundle's hook URL is built from this; every test sets the same value.
    std::env::set_var("OPENFLOWS_RUNTIME_RELAY_BASE_URL", "http://relay.test");
    let db = TestDb::new().await.unwrap();
    db.migrate().await.unwrap();
    let pool = db.pool().clone();
    let user = insert_user(&pool).await.unwrap();
    let org = insert_organization_with_owner(&pool, "acme", user)
        .await
        .unwrap();
    insert_connection(&pool, org, 42).await.unwrap();
    let provider = Arc::new(InMemorySecretProvider::new());
    let delivery = Arc::new(RecordingDelivery::default());
    let (tenants, creds) = wire(&pool, &provider, &delivery);
    let mut ctx = Ctx {
        _db: db,
        org: OrganizationId::from_uuid(org),
        tenant: TenantId::new(),
        workspace: WorkspaceId::new(),
        provider,
        delivery,
        tenants,
        creds,
    };
    let (tenant, ws) = add_tenant(&ctx, &pool, 1001, "alpha").await;
    ctx.tenant = tenant;
    ctx.workspace = ws;
    ctx
}

/// Prepare a startup: fresh bootstrap token, bundle stored then delivered.
async fn prepare(ctx: &Ctx, tenant: TenantId, ws: WorkspaceId) -> String {
    let bootstrap = ctx.creds.ensure_bootstrap_ref(ws).await.unwrap();
    ctx.tenants
        .prepare_workspace_secret(
            ctx.org,
            tenant,
            ws,
            &bootstrap,
            "http://manager.test",
            "redis://redis.test:6379",
        )
        .await
        .unwrap()
}

fn token(bundle: &Value) -> String {
    bundle["bootstrap_token"].as_str().unwrap().to_string()
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn template_gets_the_delivery_handle_and_never_the_bundle() {
    let ctx = setup().await;
    let handle = prepare(&ctx, ctx.tenant, ctx.workspace).await;
    assert_eq!(handle, format!("delivered-{}", ctx.workspace));
    let delivered = ctx.delivery.bundle(ctx.workspace).unwrap();
    assert!(!token(&delivered).is_empty());
    let store_ref: String =
        sqlx::query_scalar("SELECT runtime_secret_ref FROM workspaces WHERE id = $1")
            .bind(ctx.workspace.0)
            .fetch_one(ctx._db.pool())
            .await
            .unwrap();
    assert_ne!(
        store_ref, handle,
        "durable reference stays out of the template"
    );
    let stored: Value =
        serde_json::from_slice(&ctx.provider.get(&store_ref).await.unwrap()).unwrap();
    assert_eq!(stored, delivered);
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn consuming_the_token_scrubs_it_but_keeps_redis_and_hook_credentials() {
    let ctx = setup().await;
    prepare(&ctx, ctx.tenant, ctx.workspace).await;
    let before = ctx.delivery.bundle(ctx.workspace).unwrap();
    ctx.creds.register(&token(&before)).await.unwrap();
    let after = ctx.delivery.bundle(ctx.workspace).unwrap();
    assert_eq!(token(&after), "");
    for key in ["redis_url", "hook_key", "hook_url", "redis_namespace"] {
        assert_eq!(after[key], before[key], "{key} must survive registration");
    }
    assert!(
        ctx.creds.register(&token(&before)).await.is_err(),
        "replay fails"
    );
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn rebuild_gets_a_fresh_token_at_the_same_handle() {
    let ctx = setup().await;
    let first = prepare(&ctx, ctx.tenant, ctx.workspace).await;
    let old = token(&ctx.delivery.bundle(ctx.workspace).unwrap());
    ctx.creds.register(&old).await.unwrap();
    let second = prepare(&ctx, ctx.tenant, ctx.workspace).await;
    let fresh = token(&ctx.delivery.bundle(ctx.workspace).unwrap());
    assert_eq!(first, second);
    assert!(!fresh.is_empty() && fresh != old);
    ctx.creds.register(&fresh).await.unwrap();
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn manager_restart_restores_a_lost_delivered_copy() {
    let ctx = setup().await;
    prepare(&ctx, ctx.tenant, ctx.workspace).await;
    ctx.delivery.lose_everything();
    // A new Manager process has fresh in-process state and re-reads the database.
    let (tenants, creds) = wire(ctx._db.pool(), &ctx.provider, &ctx.delivery);
    let restarted = Ctx {
        tenants,
        creds,
        ..ctx
    };
    prepare(&restarted, restarted.tenant, restarted.workspace).await;
    assert!(restarted.delivery.bundle(restarted.workspace).is_some());
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn tenant_delete_removes_only_that_tenants_copies() {
    let ctx = setup().await;
    let pool = ctx._db.pool().clone();
    let (other_tenant, other_ws) = add_tenant(&ctx, &pool, 1002, "beta").await;
    prepare(&ctx, ctx.tenant, ctx.workspace).await;
    prepare(&ctx, other_tenant, other_ws).await;
    let a = ctx.delivery.bundle(ctx.workspace).unwrap();
    let b = ctx.delivery.bundle(other_ws).unwrap();
    assert_ne!(a["tenant_id"], b["tenant_id"]);
    assert_ne!(
        a["redis_url"], b["redis_url"],
        "each workspace has its own credentials"
    );

    ctx.tenants
        .delete_tenant_secrets(ctx.org, ctx.tenant)
        .await
        .unwrap();
    assert!(ctx.delivery.bundle(ctx.workspace).is_none());
    assert!(ctx.delivery.bundle(other_ws).is_some());
    // Deleting twice is a no-op, so cleanup can be retried.
    ctx.tenants
        .delete_tenant_secrets(ctx.org, ctx.tenant)
        .await
        .unwrap();
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn deleting_one_workspace_retires_its_copy_and_store_object() {
    let ctx = setup().await;
    let pool = ctx._db.pool().clone();
    let (other_tenant, other_ws) = add_tenant(&ctx, &pool, 1002, "beta").await;
    prepare(&ctx, ctx.tenant, ctx.workspace).await;
    prepare(&ctx, other_tenant, other_ws).await;
    let store_ref: String =
        sqlx::query_scalar("SELECT runtime_secret_ref FROM workspaces WHERE id = $1")
            .bind(ctx.workspace.0)
            .fetch_one(&pool)
            .await
            .unwrap();

    ctx.tenants
        .retire_workspace_secret(ctx.org, ctx.tenant, ctx.workspace)
        .await
        .unwrap();
    assert!(ctx.delivery.bundle(ctx.workspace).is_none());
    assert!(ctx.provider.get(&store_ref).await.is_err());
    assert!(
        ctx.delivery.bundle(other_ws).is_some(),
        "neighbours are untouched"
    );
    // Retrying after a partial failure is a no-op.
    ctx.tenants
        .retire_workspace_secret(ctx.org, ctx.tenant, ctx.workspace)
        .await
        .unwrap();
}

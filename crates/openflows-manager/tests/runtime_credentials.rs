//! WP-04 runtime authentication and GitHub credential broker tests.
//!
//! Uses HTTP fixtures (a controllable `GithubAppApi` implementation) and
//! isolated PostgreSQL databases. Covers runtime authentication, restricted
//! token exchange, negative authorization paths, cross-boundary isolation,
//! revocation/disconnect during an in-flight exchange (with deterministic
//! barriers), rotation, cleanup retries, and secret redaction / no-store.

mod common;

use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use common::*;
use openflows_manager::{
    connections::{
        app_jwt::{AppJwtClaims, AppSigner, FixtureAppSigner},
        github_app::{GithubAppApi, Installation, InstallationToken},
    },
    error::ManagerError,
    id::{ConnectionId, WorkspaceId},
    runtime,
    server::{create_router, ManagerServices},
};
use std::collections::BTreeMap;
use std::sync::{
    atomic::{AtomicBool, AtomicI64, Ordering},
    Arc, Mutex,
};
use tokio::sync::oneshot;
use tower::ServiceExt;

// ---------------------------------------------------------------------------
// Fixture GithubAppApi
// ---------------------------------------------------------------------------

/// A controllable `GithubAppApi` for the credential broker. It echoes back the
/// requested repository scope and permissions so validation passes, and can be
/// overridden to simulate wrong scope, excessive permissions, invalid expiry,
/// upstream failure, and deterministic in-flight blocking.
#[derive(Clone)]
struct FixtureBrokerApi {
    token: String,
    expires_in_secs: Arc<AtomicI64>,
    override_repositories: Arc<Mutex<Option<Vec<i64>>>>,
    override_permissions: Arc<Mutex<Option<BTreeMap<String, String>>>>,
    fail_exchange: Arc<AtomicBool>,
    revocations: Arc<Mutex<Vec<String>>>,
    started: Arc<Mutex<Option<oneshot::Sender<()>>>>,
    release: Arc<Mutex<Option<oneshot::Receiver<()>>>>,
    last_requested_permissions: Arc<Mutex<Option<BTreeMap<String, String>>>>,
    last_requested_repos: Arc<Mutex<Option<Vec<i64>>>>,
}

impl FixtureBrokerApi {
    fn new() -> Self {
        FixtureBrokerApi {
            token: "ghs_fixture_broker_token".to_string(),
            expires_in_secs: Arc::new(AtomicI64::new(3600)),
            override_repositories: Arc::new(Mutex::new(None)),
            override_permissions: Arc::new(Mutex::new(None)),
            fail_exchange: Arc::new(AtomicBool::new(false)),
            revocations: Arc::new(Mutex::new(Vec::new())),
            started: Arc::new(Mutex::new(None)),
            release: Arc::new(Mutex::new(None)),
            last_requested_permissions: Arc::new(Mutex::new(None)),
            last_requested_repos: Arc::new(Mutex::new(None)),
        }
    }

    /// Configure the fixture to block the exchange until the caller signals,
    /// notifying `started` first. Used to deterministically test revocation
    /// during an in-flight exchange.
    fn block_on_exchange(&self) -> (oneshot::Receiver<()>, oneshot::Sender<()>) {
        let (started_tx, started_rx) = oneshot::channel();
        let (release_tx, release_rx) = oneshot::channel();
        *self.started.lock().unwrap() = Some(started_tx);
        *self.release.lock().unwrap() = Some(release_rx);
        (started_rx, release_tx)
    }

    fn revocations(&self) -> Vec<String> {
        self.revocations.lock().unwrap().clone()
    }

    fn last_requested_permissions(&self) -> Option<BTreeMap<String, String>> {
        self.last_requested_permissions.lock().unwrap().clone()
    }

    fn last_requested_repos(&self) -> Option<Vec<i64>> {
        self.last_requested_repos.lock().unwrap().clone()
    }
}

#[async_trait::async_trait]
impl GithubAppApi for FixtureBrokerApi {
    async fn installation(
        &self,
        _: &openflows_manager::connections::SignedAppJwt,
        _: i64,
    ) -> Result<Option<Installation>, ManagerError> {
        Ok(None)
    }
    async fn accessible_installations(
        &self,
        _: &str,
    ) -> Result<Vec<openflows_manager::connections::github_app::AccessibleInstallation>, ManagerError>
    {
        Ok(Vec::new())
    }
    async fn fetch_user(
        &self,
        _: &str,
    ) -> Result<openflows_manager::auth::github::GithubUser, ManagerError> {
        Err(ManagerError::Config("not configured".into()))
    }
    async fn organization_membership(
        &self,
        _: &str,
        _: &str,
    ) -> Result<Option<openflows_manager::connections::github_app::OrgMembership>, ManagerError>
    {
        Ok(None)
    }
    async fn installation_repositories(
        &self,
        _: &openflows_manager::connections::SignedAppJwt,
        _: i64,
        _: u32,
        _: u32,
    ) -> Result<Vec<openflows_manager::connections::github_app::RepoRef>, ManagerError> {
        Ok(Vec::new())
    }
    async fn exchange_installation_token(
        &self,
        _: &openflows_manager::connections::SignedAppJwt,
        _: i64,
        repository_ids: &[i64],
        permissions: &BTreeMap<String, String>,
    ) -> Result<InstallationToken, ManagerError> {
        if self.fail_exchange.load(Ordering::SeqCst) {
            return Err(ManagerError::api(
                "GITHUB_UNAVAILABLE",
                "upstream exchange failed",
            ));
        }
        *self.last_requested_permissions.lock().unwrap() = Some(permissions.clone());
        *self.last_requested_repos.lock().unwrap() = Some(repository_ids.to_vec());
        // Deterministic barrier: signal started, wait for release.
        if let Some(tx) = self.started.lock().unwrap().take() {
            let _ = tx.send(());
        }
        // Take the receiver out of the lock before awaiting so no guard is held
        // across the await (required for the future to be Send).
        let release = { self.release.lock().unwrap().take() };
        if let Some(rx) = release {
            let _ = rx.await;
        }
        let repo_ids = self
            .override_repositories
            .lock()
            .unwrap()
            .clone()
            .unwrap_or_else(|| repository_ids.to_vec());
        let perms = self
            .override_permissions
            .lock()
            .unwrap()
            .clone()
            .unwrap_or_else(|| permissions.clone());
        Ok(InstallationToken {
            token: self.token.clone(),
            expires_at: chrono::Utc::now()
                + chrono::Duration::seconds(self.expires_in_secs.load(Ordering::SeqCst)),
            repository_ids: repo_ids,
            permissions: perms,
        })
    }
    async fn revoke_installation_token(
        &self,
        _: &openflows_manager::connections::SignedAppJwt,
        _: i64,
        token: &str,
    ) -> Result<(), ManagerError> {
        self.revocations.lock().unwrap().push(token.to_string());
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Setup helpers
// ---------------------------------------------------------------------------

struct RuntimeCtx {
    services: ManagerServices,
    workspace_id: WorkspaceId,
    raw_credential: String,
    router: axum::Router<()>,
    api: FixtureBrokerApi,
}

fn fixture_signer() -> Arc<dyn AppSigner> {
    Arc::new(FixtureAppSigner::new(AppJwtClaims {
        iss: 1001,
        iat: 1,
        exp: 2,
    }))
}

fn build_services(pool: sqlx::PgPool, api: FixtureBrokerApi, public_url: &str) -> ManagerServices {
    let db = openflows_manager::db::Db::from_pool(pool);
    let github: Arc<dyn openflows_manager::auth::github::GithubAuth> =
        Arc::new(FixtureGithubAuth::new(FixtureUser {
            id: 1,
            login: "alice".to_string(),
            display_name: "Alice".to_string(),
        }));
    ManagerServices::from_db_with_app(
        db,
        test_auth_config(public_url),
        github,
        1001,
        fixture_signer(),
        Arc::new(api),
        None,
    )
}

fn router_for(services: ManagerServices) -> axum::Router<()> {
    let state = openflows_manager::server::AppState::with_services(
        pocketflow_core::SharedStore::new_in_memory_with_tenant("test"),
        Arc::new(OkProbe),
        services,
        openflows_manager::config::local_test_config(),
    );
    create_router(state)
}

/// Create the full runtime fixture: org -> connection -> repository -> tenant
/// -> workspace, issue a runtime credential, and return the context.
async fn setup(pool: &sqlx::PgPool) -> RuntimeCtx {
    setup_with_role_and_states(pool, "forge", "running", "running", "ready").await
}

async fn setup_with_role_and_states(
    pool: &sqlx::PgPool,
    role: &str,
    workspace_state: &str,
    tenant_state: &str,
    org_state: &str,
) -> RuntimeCtx {
    let api = FixtureBrokerApi::new();
    let services = build_services(pool.clone(), api.clone(), "http://127.0.0.1:3002");
    let user = insert_user(pool).await.unwrap();
    let org = insert_organization_with_owner(pool, "acme", user)
        .await
        .unwrap();
    // Adjust org status if requested.
    if org_state != "ready" {
        sqlx::query("UPDATE organizations SET status = $1 WHERE id = $2")
            .bind(org_state)
            .bind(org)
            .execute(pool)
            .await
            .unwrap();
    }
    let connection_id = insert_connection(pool, org, 42).await.unwrap();
    let repo_id: i64 = 9001;
    insert_repository(pool, org, connection_id, repo_id, "acme/backend")
        .await
        .unwrap();
    let tenant_id = insert_running_tenant(pool, org, connection_id, repo_id, "acme-backend")
        .await
        .unwrap();
    if tenant_state != "running" {
        sqlx::query("UPDATE tenants SET desired_state = $1, observed_state = $1 WHERE id = $2")
            .bind(tenant_state)
            .bind(tenant_id)
            .execute(pool)
            .await
            .unwrap();
    }
    let workspace_id = insert_running_workspace(pool, org, tenant_id, role, 0)
        .await
        .unwrap();
    if workspace_state != "running" {
        sqlx::query("UPDATE workspaces SET desired_state = $1, observed_state = $1 WHERE id = $2")
            .bind(workspace_state)
            .bind(workspace_id)
            .execute(pool)
            .await
            .unwrap();
    }
    let issued = services
        .runtime_credentials
        .issue_for_workspace(WorkspaceId::from_uuid(workspace_id))
        .await
        .unwrap();
    let router = router_for(services.clone());
    RuntimeCtx {
        services,
        workspace_id: WorkspaceId::from_uuid(workspace_id),
        raw_credential: issued.raw_secret,
        router,
        api,
    }
}

fn broker_request(purpose: &str, bearer: &str) -> Request<Body> {
    Request::post("/api/v1/runtime/github-credentials")
        .header("content-type", "application/json")
        .header("authorization", format!("Bearer {bearer}"))
        .body(Body::from(format!(r#"{{"purpose":"{purpose}"}}"#)))
        .unwrap()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn successful_runtime_exchange_returns_scoped_token() {
    let db = TestDb::new().await.unwrap();
    db.migrate().await.unwrap();
    let ctx = setup(db.pool()).await;

    let resp = ctx
        .router
        .clone()
        .oneshot(broker_request("git", &ctx.raw_credential))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(
        resp.headers()["cache-control"],
        "no-store",
        "credential responses must be no-store"
    );
    let body: serde_json::Value = serde_json::from_slice(
        &axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(body["token"], "ghs_fixture_broker_token");
    assert_eq!(body["repository_id"], 9001);
    assert_eq!(body["username"], "x-access-token");
    // The requested permission profile for forge.git is contents:write.
    let requested = ctx.api.last_requested_permissions().unwrap();
    assert_eq!(requested.get("contents").map(String::as_str), Some("write"));
    assert_eq!(ctx.api.last_requested_repos().unwrap(), vec![9001]);

    // A lease was recorded with the fingerprint and generations.
    let (fingerprint, cg, wg): (String, i64, i64) = sqlx::query_as(
        "SELECT token_fingerprint, connection_generation, workspace_generation
           FROM credential_leases WHERE repository_id = 9001",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(
        fingerprint,
        runtime::repository::token_fingerprint("ghs_fixture_broker_token")
    );
    assert_eq!(cg, 0);
    assert_eq!(wg, 0);
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn api_purpose_requests_api_permission_profile() {
    let db = TestDb::new().await.unwrap();
    db.migrate().await.unwrap();
    let ctx = setup_with_role_and_states(db.pool(), "vessel", "running", "running", "ready").await;

    let resp = ctx
        .router
        .clone()
        .oneshot(broker_request("api", &ctx.raw_credential))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let requested = ctx.api.last_requested_permissions().unwrap();
    assert_eq!(requested.get("checks").map(String::as_str), Some("read"));
    assert_eq!(requested.get("actions").map(String::as_str), Some("read"));
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn human_credentials_are_rejected_at_runtime_endpoint() {
    let db = TestDb::new().await.unwrap();
    db.migrate().await.unwrap();
    let ctx = setup(db.pool()).await;

    // A real human CLI access token must never authorize the runtime endpoint.
    let human_token = login_as_user(&ctx.services, 99, "mallory").await;
    let resp = ctx
        .router
        .clone()
        .oneshot(broker_request("git", &human_token))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);

    // A missing credential is also rejected.
    let resp = ctx
        .router
        .clone()
        .oneshot(broker_request("git", ""))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn missing_or_invalid_audience_is_rejected() {
    let db = TestDb::new().await.unwrap();
    db.migrate().await.unwrap();
    let ctx = setup(db.pool()).await;

    // Insert a credential with the wrong audience directly.
    let secret = openflows_manager::auth::crypto::Secret::generate();
    sqlx::query(
        "INSERT INTO runtime_credentials
            (id, workspace_id, credential_hash, audience, expires_at, generation)
         VALUES ($1, $2, $3, 'other-audience', clock_timestamp() + interval '1 day', 0)",
    )
    .bind(uuid::Uuid::new_v4())
    .bind(ctx.workspace_id.0)
    .bind(secret.hash())
    .execute(db.pool())
    .await
    .unwrap();
    let resp = ctx
        .router
        .clone()
        .oneshot(broker_request("git", &secret.encode()))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn expired_credential_is_rejected() {
    let db = TestDb::new().await.unwrap();
    db.migrate().await.unwrap();
    let ctx = setup(db.pool()).await;

    let secret = openflows_manager::auth::crypto::Secret::generate();
    sqlx::query(
        "INSERT INTO runtime_credentials
            (id, workspace_id, credential_hash, audience, expires_at, generation)
         VALUES ($1, $2, $3, 'openflows-workspace', clock_timestamp() - interval '1 second', 0)",
    )
    .bind(uuid::Uuid::new_v4())
    .bind(ctx.workspace_id.0)
    .bind(secret.hash())
    .execute(db.pool())
    .await
    .unwrap();
    let resp = ctx
        .router
        .clone()
        .oneshot(broker_request("git", &secret.encode()))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn revoked_credential_is_rejected() {
    let db = TestDb::new().await.unwrap();
    db.migrate().await.unwrap();
    let ctx = setup(db.pool()).await;

    let secret = openflows_manager::auth::crypto::Secret::generate();
    sqlx::query(
        "INSERT INTO runtime_credentials
            (id, workspace_id, credential_hash, audience, expires_at, generation, revoked_at)
         VALUES ($1, $2, $3, 'openflows-workspace', clock_timestamp() + interval '1 day', 0,
                 clock_timestamp())",
    )
    .bind(uuid::Uuid::new_v4())
    .bind(ctx.workspace_id.0)
    .bind(secret.hash())
    .execute(db.pool())
    .await
    .unwrap();
    let resp = ctx
        .router
        .clone()
        .oneshot(broker_request("git", &secret.encode()))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn stale_generation_credential_is_rejected() {
    let db = TestDb::new().await.unwrap();
    db.migrate().await.unwrap();
    let ctx = setup(db.pool()).await;

    // Bump the workspace generation so the issued credential (generation 0) is
    // stale.
    ctx.services
        .runtime_repo
        .bump_workspace_generation(ctx.workspace_id)
        .await
        .unwrap();
    let resp = ctx
        .router
        .clone()
        .oneshot(broker_request("git", &ctx.raw_credential))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn rotation_invalidates_previous_runtime_credential() {
    let db = TestDb::new().await.unwrap();
    db.migrate().await.unwrap();
    let ctx = setup(db.pool()).await;

    // Old credential works.
    let resp = ctx
        .router
        .clone()
        .oneshot(broker_request("git", &ctx.raw_credential))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // Rotate.
    let rotated = ctx
        .services
        .runtime_credentials
        .rotate(ctx.workspace_id)
        .await
        .unwrap();
    assert_eq!(rotated.generation, 1);

    // Old credential now fails (stale generation).
    let resp = ctx
        .router
        .clone()
        .oneshot(broker_request("git", &ctx.raw_credential))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);

    // New credential works.
    let resp = ctx
        .router
        .clone()
        .oneshot(broker_request("git", &rotated.raw_secret))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn cross_tenant_scope_is_derived_not_caller_supplied() {
    let db = TestDb::new().await.unwrap();
    db.migrate().await.unwrap();
    let ctx = setup(db.pool()).await;

    // The runtime credential belongs to a single workspace whose tenant binds a
    // single repository. The broker derives the scope; the request body can
    // never override repository or permission. A different tenant's repository
    // cannot be reached.
    let resp = ctx
        .router
        .clone()
        .oneshot(
            Request::post("/api/v1/runtime/github-credentials")
                .header("content-type", "application/json")
                .header("authorization", format!("Bearer {}", ctx.raw_credential))
                .body(Body::from(
                    r#"{"purpose":"git","repository_id":9999}"#.to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    // Extra fields in the request body are ignored by serde; the broker still
    // derives repository 9001 (never 9999).
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(ctx.api.last_requested_repos().unwrap(), vec![9001]);
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn suspended_and_deleted_resources_are_rejected() {
    let db = TestDb::new().await.unwrap();
    db.migrate().await.unwrap();
    let ctx = setup(db.pool()).await;

    // Disconnected connection (bumps access generation, blocks issuance).
    let org = org_for(db.pool()).await;
    let conn_id = ctx
        .services
        .connections
        .list_connections(org)
        .await
        .unwrap()
        .remove(0)
        .id;
    sqlx::query(
        "UPDATE github_connections SET status = 'disconnected', access_generation = access_generation + 1 WHERE id = $1",
    )
    .bind(conn_id.0)
    .execute(db.pool())
    .await
    .unwrap();
    let resp = ctx
        .router
        .clone()
        .oneshot(broker_request("git", &ctx.raw_credential))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn suspended_org_is_rejected() {
    let db = TestDb::new().await.unwrap();
    db.migrate().await.unwrap();
    let ctx =
        setup_with_role_and_states(db.pool(), "forge", "running", "running", "suspended").await;
    let resp = ctx
        .router
        .clone()
        .oneshot(broker_request("git", &ctx.raw_credential))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn non_running_workspace_is_rejected() {
    let db = TestDb::new().await.unwrap();
    db.migrate().await.unwrap();
    let ctx = setup_with_role_and_states(db.pool(), "forge", "stopped", "running", "ready").await;
    let resp = ctx
        .router
        .clone()
        .oneshot(broker_request("git", &ctx.raw_credential))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn inaccessible_repository_is_rejected() {
    let db = TestDb::new().await.unwrap();
    db.migrate().await.unwrap();
    let ctx = setup(db.pool()).await;
    sqlx::query(
        "UPDATE github_repositories SET accessible = false WHERE github_repository_id = 9001",
    )
    .execute(db.pool())
    .await
    .unwrap();
    let resp = ctx
        .router
        .clone()
        .oneshot(broker_request("git", &ctx.raw_credential))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn incorrect_repository_scope_from_upstream_fails_closed() {
    let db = TestDb::new().await.unwrap();
    db.migrate().await.unwrap();
    let ctx = setup(db.pool()).await;
    ctx.api
        .override_repositories
        .lock()
        .unwrap()
        .replace(vec![9999]);
    let resp = ctx
        .router
        .clone()
        .oneshot(broker_request("git", &ctx.raw_credential))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn excessive_permissions_from_upstream_fails_closed() {
    let db = TestDb::new().await.unwrap();
    db.migrate().await.unwrap();
    let ctx = setup(db.pool()).await;
    let mut excessive = BTreeMap::new();
    excessive.insert("contents".to_string(), "write".to_string());
    excessive.insert("administration".to_string(), "write".to_string());
    ctx.api
        .override_permissions
        .lock()
        .unwrap()
        .replace(excessive);
    let resp = ctx
        .router
        .clone()
        .oneshot(broker_request("git", &ctx.raw_credential))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn invalid_upstream_expiry_fails_closed() {
    let db = TestDb::new().await.unwrap();
    db.migrate().await.unwrap();
    let ctx = setup(db.pool()).await;
    ctx.api.expires_in_secs.store(60, Ordering::SeqCst); // well under the 30-minute minimum
    let resp = ctx
        .router
        .clone()
        .oneshot(broker_request("git", &ctx.raw_credential))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn revocation_during_inflight_exchange_discards_token() {
    let db = TestDb::new().await.unwrap();
    db.migrate().await.unwrap();
    let ctx = setup(db.pool()).await;

    // Configure the fixture to block on exchange.
    let (started_rx, release_tx) = ctx.api.block_on_exchange();

    // Start the exchange in the background.
    let router = ctx.router.clone();
    let bearer = ctx.raw_credential.clone();
    let handle = tokio::spawn(async move {
        router
            .oneshot(broker_request("git", &bearer))
            .await
            .unwrap()
    });

    // Wait for the exchange to start (deterministic barrier).
    started_rx.await.unwrap();

    // Revoke the workspace credential generation (simulates rotation/revocation
    // during the in-flight exchange). This invalidates future issuance.
    ctx.services
        .runtime_credentials
        .revoke(ctx.workspace_id)
        .await
        .unwrap();

    // Release the exchange; it must now detect the changed generation, discard
    // the token, attempt upstream revocation, and fail closed.
    release_tx.send(()).unwrap();
    let resp = handle.await.unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    // The token must have been discarded (upstream revocation attempted).
    assert!(
        !ctx.api.revocations().is_empty(),
        "upstream revocation must be attempted on a discarded token"
    );
    // No lease must remain for the discarded token.
    let count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM credential_leases WHERE repository_id = 9001 AND revoked_at IS NULL",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(count, 0);
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn disconnect_during_inflight_exchange_discards_token() {
    let db = TestDb::new().await.unwrap();
    db.migrate().await.unwrap();
    let ctx = setup(db.pool()).await;

    let (started_rx, release_tx) = ctx.api.block_on_exchange();
    let router = ctx.router.clone();
    let bearer = ctx.raw_credential.clone();
    let handle = tokio::spawn(async move {
        router
            .oneshot(broker_request("git", &bearer))
            .await
            .unwrap()
    });
    started_rx.await.unwrap();

    // Disconnect the connection: bumps the connection access generation.
    let org = org_for(ctx.services.db.pool()).await;
    let conn = ctx
        .services
        .connections
        .list_connections(org)
        .await
        .unwrap()
        .remove(0);
    sqlx::query(
        "UPDATE github_connections SET status = 'disconnected',
         access_generation = access_generation + 1 WHERE id = $1",
    )
    .bind(conn.id.0)
    .execute(db.pool())
    .await
    .unwrap();

    release_tx.send(()).unwrap();
    let resp = handle.await.unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    assert!(!ctx.api.revocations().is_empty());
}

// A helper to find the org id for the fixture's connection list.
async fn org_for(pool: &sqlx::PgPool) -> openflows_manager::id::OrganizationId {
    let row: (uuid::Uuid,) = sqlx::query_as("SELECT id FROM organizations LIMIT 1")
        .fetch_one(pool)
        .await
        .unwrap();
    openflows_manager::id::OrganizationId::from_uuid(row.0)
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn invalid_purpose_is_rejected() {
    let db = TestDb::new().await.unwrap();
    db.migrate().await.unwrap();
    let ctx = setup(db.pool()).await;
    let resp = ctx
        .router
        .clone()
        .oneshot(broker_request("admin", &ctx.raw_credential))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn cleanup_retries_after_temporary_failure_and_worker_restart() {
    let db = TestDb::new().await.unwrap();
    db.migrate().await.unwrap();
    let ctx = setup(db.pool()).await;

    // Record a lease that needs cleanup (e.g. after a disconnect).
    let org = org_for(ctx.services.db.pool()).await;
    let conn = ctx
        .services
        .connections
        .list_connections(org)
        .await
        .unwrap()
        .remove(0);
    let conn_id: ConnectionId = conn.id;

    // Insert an unrevoked lease.
    let lease = runtime::NewCredentialLease {
        organization_id: org,
        connection_id: conn_id,
        tenant_id: None,
        workspace_id: None,
        installation_id: 42,
        repository_id: 9001,
        permission_profile_hash: "hash".to_string(),
        connection_generation: 0,
        workspace_generation: 0,
        purpose: "git".to_string(),
        token_fingerprint: "fp".to_string(),
        encrypted_ref: Some("cipher".to_string()),
        expires_at: chrono::Utc::now() + chrono::Duration::hours(1),
    };
    ctx.services
        .runtime_repo
        .insert_lease(&lease)
        .await
        .unwrap();

    // Enqueue the disconnect cleanup event.
    let mut tx = db.pool().begin().await.unwrap();
    openflows_manager::outbox::insert_in_tx(
        &mut tx,
        Some(org),
        "github.disconnect_cleanup",
        Some(&serde_json::json!({
            "connection_id": conn_id.to_string(),
            "organization_id": org.to_string(),
        })),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();

    // A worker starts processing but crashes before delivery (temporary
    // failure). Its lease is released without delivering.
    let claims = openflows_manager::outbox::claim_filtered(
        db.pool(),
        "worker-a",
        10,
        Some(&["github.disconnect_cleanup"]),
    )
    .await
    .unwrap();
    assert_eq!(claims.len(), 1);
    assert!(
        openflows_manager::outbox::release_lease(db.pool(), &claims[0])
            .await
            .unwrap()
    );
    // The event is still pending and reclaimable by a restarted worker.
    let pending: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM outbox_events
          WHERE event_type = 'github.disconnect_cleanup' AND delivered_at IS NULL AND failed_at IS NULL",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(pending, 1);

    // A restarted worker reclaims and completes the cleanup, revoking the lease.
    ctx.services.connection_worker.run_once().await.unwrap();
    let outstanding: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM credential_leases WHERE revoked_at IS NULL AND encrypted_ref IS NOT NULL",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(outstanding, 0, "cleanup must revoke outstanding leases");
    let delivered: bool = sqlx::query_scalar(
        "SELECT delivered_at IS NOT NULL FROM outbox_events WHERE event_type = 'github.disconnect_cleanup'",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert!(delivered, "cleanup event must be delivered after retry");
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn secret_redaction_and_no_plaintext_in_audit_or_db() {
    let db = TestDb::new().await.unwrap();
    db.migrate().await.unwrap();
    let ctx = setup(db.pool()).await;

    let resp = ctx
        .router
        .clone()
        .oneshot(broker_request("git", &ctx.raw_credential))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(resp.headers()["cache-control"], "no-store");

    // The raw token must not appear in any ordinary DB column.
    let token_plaintext: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM credential_leases WHERE token_fingerprint = 'ghs_fixture_broker_token' OR encrypted_ref LIKE '%ghs_fixture_broker_token%'",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(
        token_plaintext, 0,
        "raw token must never be persisted in plaintext"
    );

    // Audit events must not contain the token.
    let audited: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM audit_events WHERE action LIKE 'runtime.github_credential%'",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert!(audited >= 1);

    // The stored encrypted_ref decrypts back to the token (round-trip via the
    // envelope cipher), proving it is the encrypted secret mechanism.
    let (encrypted_ref,): (String,) =
        sqlx::query_as("SELECT encrypted_ref FROM credential_leases WHERE repository_id = 9001")
            .fetch_one(db.pool())
            .await
            .unwrap();
    let sealed =
        base64::Engine::decode(&base64::engine::general_purpose::STANDARD, &encrypted_ref).unwrap();
    let cipher = ctx
        .services
        .auth_config
        .envelope_cipher("runtime_lease")
        .unwrap();
    let plain = cipher.open(&sealed).unwrap();
    assert_eq!(
        String::from_utf8(plain).unwrap(),
        "ghs_fixture_broker_token"
    );
}

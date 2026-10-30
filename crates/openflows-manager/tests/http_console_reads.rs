//! HTTP-level integration tests for the read endpoints the Console depends on:
//! audit log, pending invitations, connection repositories and the platform
//! overview. Real handlers + PostgreSQL + fixture GitHub; calls use CLI Bearer
//! credentials (CSRF-exempt, only mutations need CSRF anyway).
//!
//! These are `#[ignore]`d and run by `scripts/run-integration-tests.sh` in CI.

mod common;

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use common::*;
use openflows_manager::auth::github::GithubAuth;
use openflows_manager::id::OrganizationId;
use std::sync::Arc;
use tower::ServiceExt;

struct App {
    router: axum::Router<()>,
}

impl App {
    async fn get(&self, path: &str, token: &str) -> (StatusCode, serde_json::Value) {
        let req = Request::builder()
            .method("GET")
            .uri(path)
            .header(header::AUTHORIZATION, format!("Bearer {token}"))
            .body(Body::empty())
            .unwrap();
        let resp = self.router.clone().oneshot(req).await.unwrap();
        let status = resp.status();
        let bytes = axum::body::to_bytes(resp.into_body(), 1024 * 128)
            .await
            .unwrap();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null),
        )
    }

    async fn send(
        &self,
        method: &str,
        path: &str,
        token: &str,
        body: Option<serde_json::Value>,
    ) -> (StatusCode, serde_json::Value) {
        let mut req = Request::builder()
            .method(method)
            .uri(path)
            .header(header::AUTHORIZATION, format!("Bearer {token}"))
            .header("idempotency-key", uuid::Uuid::new_v4().to_string());
        let payload = match body {
            Some(b) => {
                req = req.header(header::CONTENT_TYPE, "application/json");
                Body::from(b.to_string())
            }
            None => Body::empty(),
        };
        let resp = self
            .router
            .clone()
            .oneshot(req.body(payload).unwrap())
            .await
            .unwrap();
        let status = resp.status();
        let bytes = axum::body::to_bytes(resp.into_body(), 1024 * 128)
            .await
            .unwrap();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null),
        )
    }
}

fn fixture() -> Arc<dyn GithubAuth> {
    Arc::new(FixtureGithubAuth::new(FixtureUser {
        id: 1,
        login: "alice".into(),
        display_name: "Alice".into(),
    }))
}

async fn create_org(app: &App, token: &str, slug: &str) -> OrganizationId {
    let (status, body) = app
        .send(
            "POST",
            "/api/v1/organizations",
            token,
            Some(serde_json::json!({ "slug": slug, "display_name": slug })),
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "org creation 202");
    body["resource_id"].as_str().unwrap().parse().unwrap()
}

/// Add `github_id` as an active member with `role` without going through the
/// invitation flow (the flow itself is covered in `http_org.rs`).
async fn add_member(
    services: &openflows_manager::server::ManagerServices,
    org: OrganizationId,
    github_id: i64,
    role: &str,
) {
    sqlx::query(
        "INSERT INTO memberships (organization_id, user_id, role, status)
         SELECT $1, user_id, $3, 'active' FROM identities
          WHERE provider = 'github' AND subject = $2",
    )
    .bind(org.0)
    .bind(github_id.to_string())
    .bind(role)
    .execute(services.db.pool())
    .await
    .expect("insert membership");
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn audit_is_admin_only_newest_first_and_paged() {
    let db = TestDb::new().await.unwrap();
    db.migrate().await.unwrap();
    let (services, router) = build_context(db.pool().clone(), fixture(), "http://127.0.0.1:3002");
    let app = App { router };
    let alice = login_as_user(&services, 1, "alice").await;
    let bob = login_as_user(&services, 2, "bob").await;
    let org = create_org(&app, &alice, "orga").await;
    add_member(&services, org, 2, "viewer").await;
    for action in ["test.one", "test.two", "test.three"] {
        openflows_manager::audit::insert_with_pool(
            db.pool(),
            &openflows_manager::audit::AuditEvent::new(action).organization(org),
        )
        .await
        .unwrap();
    }

    let (status, body) = app
        .get(&format!("/api/v1/organizations/{org}/audit"), &alice)
        .await;
    assert_eq!(status, StatusCode::OK, "admin reads audit");
    let items = body["items"].as_array().unwrap();
    assert!(items.len() >= 4, "creation plus three test events");
    let times: Vec<&str> = items
        .iter()
        .map(|i| i["occurred_at"].as_str().unwrap())
        .collect();
    assert!(times.windows(2).all(|w| w[0] >= w[1]), "newest first");

    // Paging: two pages of 2 never repeat an id.
    let (_, first) = app
        .get(
            &format!("/api/v1/organizations/{org}/audit?limit=2"),
            &alice,
        )
        .await;
    let cursor = first["next_cursor"]
        .as_str()
        .expect("more pages")
        .to_string();
    let (_, second) = app
        .get(
            &format!("/api/v1/organizations/{org}/audit?limit=2&after={cursor}"),
            &alice,
        )
        .await;
    let ids = |v: &serde_json::Value| -> Vec<String> {
        v["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|i| i["id"].as_str().unwrap().to_string())
            .collect()
    };
    assert!(ids(&first).iter().all(|id| !ids(&second).contains(id)));

    let (status, _) = app
        .get(&format!("/api/v1/organizations/{org}/audit"), &bob)
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "viewer cannot read audit");
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn pending_invitations_list_for_admins_and_hide_revoked() {
    let db = TestDb::new().await.unwrap();
    db.migrate().await.unwrap();
    let (services, router) = build_context(db.pool().clone(), fixture(), "http://127.0.0.1:3002");
    let app = App { router };
    let alice = login_as_user(&services, 1, "alice").await;
    let bob = login_as_user(&services, 2, "bob").await;
    let org = create_org(&app, &alice, "orga").await;
    add_member(&services, org, 2, "viewer").await;

    let (status, created) = app
        .send(
            "POST",
            &format!("/api/v1/organizations/{org}/invitations"),
            &alice,
            Some(serde_json::json!({ "github_login": "bob", "role": "developer" })),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "invitation created");

    let path = format!("/api/v1/organizations/{org}/invitations");
    let (status, body) = app.get(&path, &alice).await;
    assert_eq!(status, StatusCode::OK);
    let items = body["items"].as_array().unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["invitation_id"], created["invitation_id"]);
    assert_eq!(items[0]["role"], "developer");
    assert_eq!(items[0]["status"], "pending");
    assert!(items[0].get("token").is_none() && items[0].get("token_hash").is_none());

    let (status, _) = app.get(&path, &bob).await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "viewer cannot list invitations"
    );

    let id = created["invitation_id"].as_str().unwrap();
    let (status, _) = app
        .send("DELETE", &format!("{path}/{id}"), &alice, None)
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (_, body) = app.get(&path, &alice).await;
    assert!(
        body["items"].as_array().unwrap().is_empty(),
        "revoked hidden"
    );
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn repositories_list_is_scoped_to_the_connection_and_organization() {
    let db = TestDb::new().await.unwrap();
    db.migrate().await.unwrap();
    let (services, router) = build_context(db.pool().clone(), fixture(), "http://127.0.0.1:3002");
    let app = App { router };
    let alice = login_as_user(&services, 1, "alice").await;
    let bob = login_as_user(&services, 2, "bob").await;
    let org = create_org(&app, &alice, "orga").await;
    let other = create_org(&app, &bob, "orgb").await;

    let connection = uuid::Uuid::new_v4();
    sqlx::query(
        "INSERT INTO github_connections
            (id, organization_id, app_id, installation_id, github_account_id,
             account_type, account_login, status)
         VALUES ($1, $2, 1, 100, 200, 'User', 'alice', 'active')",
    )
    .bind(connection)
    .bind(org.0)
    .execute(db.pool())
    .await
    .expect("seed connection");
    for (id, name, accessible) in [
        (1_i64, "alice/zeta", true),
        (2, "alice/alpha", true),
        (3, "alice/gone", false),
    ] {
        sqlx::query(
            "INSERT INTO github_repositories
                (organization_id, connection_id, github_repository_id, full_name, accessible)
             VALUES ($1, $2, $3, $4, $5)",
        )
        .bind(org.0)
        .bind(connection)
        .bind(id)
        .bind(name)
        .bind(accessible)
        .execute(db.pool())
        .await
        .expect("seed repository");
    }

    let path = format!("/api/v1/organizations/{org}/github/connections/{connection}/repositories");
    let (status, body) = app.get(&path, &alice).await;
    assert_eq!(status, StatusCode::OK);
    let names: Vec<&str> = body["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["full_name"].as_str().unwrap())
        .collect();
    assert_eq!(
        names,
        ["alice/alpha", "alice/zeta"],
        "accessible only, sorted"
    );

    // Another organization cannot see it, and the connection id does not leak
    // across organizations.
    let (status, _) = app.get(&path, &bob).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "outsider gets 404");
    let (status, _) = app
        .get(
            &format!("/api/v1/organizations/{other}/github/connections/{connection}/repositories"),
            &bob,
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "wrong organization gets 404");
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn platform_overview_is_operator_only() {
    let db = TestDb::new().await.unwrap();
    db.migrate().await.unwrap();
    let (services, router) = build_context(db.pool().clone(), fixture(), "http://127.0.0.1:3002");
    let app = App { router };
    let alice = login_as_user(&services, 1, "alice").await;
    create_org(&app, &alice, "orga").await;

    let (status, _) = app.get("/api/v1/platform/overview", &alice).await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "org admin is not an operator"
    );

    sqlx::query(
        "INSERT INTO platform_operators (user_id, created_by)
         SELECT user_id, user_id FROM identities WHERE provider = 'github' AND subject = '1'",
    )
    .execute(db.pool())
    .await
    .expect("grant operator");
    let (status, body) = app.get("/api/v1/platform/overview", &alice).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["organizations"]["total"], 1);
    assert_eq!(body["tenants"]["total"], 0);
    assert!(body["templates"].is_array());
}

//! HTTP-level integration tests for organization authorization and membership
//! (WP-02). Real handlers + PostgreSQL + fixture GitHub. Mutations use CLI
//! Bearer credentials (CSRF-exempt); invitation-acceptance is exercised via the
//! service (the HTTP accept path requires browser+CSRF and is covered at the
//! repository level in `org_policy.rs`).
//!
//! These are `#[ignore]`d and run by `scripts/run-integration-tests.sh` in CI.

mod common;

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use common::*;
use openflows_manager::auth::github::GithubAuth;
use openflows_manager::id::{OrganizationId, UserId};
use std::sync::Arc;
use tower::ServiceExt;

struct App {
    router: axum::Router<()>,
}

impl App {
    async fn request(&self, req: Request<Body>) -> axum::response::Response {
        self.router
            .clone()
            .oneshot(req)
            .await
            .expect("route responds")
    }
    fn get(&self, path: &str) -> Request<Body> {
        Request::builder()
            .method("GET")
            .uri(path)
            .body(Body::empty())
            .unwrap()
    }
    fn post_json(&self, path: &str, body: serde_json::Value) -> Request<Body> {
        Request::builder()
            .method("POST")
            .uri(path)
            .header(header::CONTENT_TYPE, "application/json")
            .header("idempotency-key", uuid::Uuid::new_v4().to_string())
            .body(Body::from(body.to_string()))
            .unwrap()
    }
    fn patch_json(&self, path: &str, body: serde_json::Value) -> Request<Body> {
        Request::builder()
            .method("PATCH")
            .uri(path)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body.to_string()))
            .unwrap()
    }
    fn delete(&self, path: &str) -> Request<Body> {
        Request::builder()
            .method("DELETE")
            .uri(path)
            .body(Body::empty())
            .unwrap()
    }
}

fn with_bearer(mut req: Request<Body>, token: &str) -> Request<Body> {
    req.headers_mut().insert(
        header::AUTHORIZATION,
        format!("Bearer {token}").parse().unwrap(),
    );
    req
}

async fn read_json(resp: axum::response::Response) -> serde_json::Value {
    let bytes = axum::body::to_bytes(resp.into_body(), 1024 * 128)
        .await
        .unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

/// Resolve a user id from the identities table by GitHub id.
async fn user_id_for(
    services: &openflows_manager::server::ManagerServices,
    github_id: i64,
) -> UserId {
    let row: Option<(uuid::Uuid,)> =
        sqlx::query_as("SELECT user_id FROM identities WHERE provider='github' AND subject=$1")
            .bind(github_id.to_string())
            .fetch_optional(services.db.pool())
            .await
            .expect("identity query");
    UserId::from_uuid(row.expect("user exists").0)
}

/// Create an organization via the API and return org_id.
async fn create_org(app: &App, token: &str, slug: &str) -> OrganizationId {
    let resp = app
        .request(with_bearer(
            app.post_json(
                "/api/v1/organizations",
                serde_json::json!({ "slug": slug, "display_name": slug }),
            ),
            token,
        ))
        .await;
    assert_eq!(resp.status(), StatusCode::ACCEPTED, "org creation 202");
    let body = read_json(resp).await;
    body["resource_id"]
        .as_str()
        .unwrap()
        .parse::<OrganizationId>()
        .unwrap()
}

/// Create an invitation and accept it via the service (browser+CSRF path is
/// repository-tested). Returns the raw invitation token.
async fn invite_and_accept(
    app: &App,
    services: &openflows_manager::server::ManagerServices,
    admin_token: &str,
    org: OrganizationId,
    github_login: &str,
    invitee_github_id: i64,
    role: &str,
) -> String {
    let resp = app
        .request(with_bearer(
            app.post_json(
                &format!("/api/v1/organizations/{org}/invitations"),
                serde_json::json!({ "github_login": github_login, "role": role }),
            ),
            admin_token,
        ))
        .await;
    assert_eq!(resp.status(), StatusCode::OK, "invitation created");
    let body = read_json(resp).await;
    let url = body["invitation_url"].as_str().unwrap().to_string();
    let token = url.rsplit('=').next().unwrap().to_string();
    let invitee_user = user_id_for(services, invitee_github_id).await;
    services
        .orgs_service
        .accept_invitation(invitee_user, invitee_github_id, &token, "test")
        .await
        .expect("accept invitation");
    token
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn user_a_cannot_read_or_mutate_organization_b() {
    let db = TestDb::new().await.unwrap();
    db.migrate().await.unwrap();
    let github: Arc<dyn GithubAuth> = Arc::new(FixtureGithubAuth::new(FixtureUser {
        id: 1,
        login: "alice".into(),
        display_name: "Alice".into(),
    }));
    let (services, router) = build_context(db.pool().clone(), github, "http://127.0.0.1:3002");
    let app = App { router };
    let alice = login_as_user(&services, 1, "alice").await;
    let bob = login_as_user(&services, 2, "bob").await;

    let org_a = create_org(&app, &alice, "orga").await;

    // Bob (not a member of A) gets 404, never 403/200.
    let get_as_bob = app
        .request(with_bearer(
            app.get(&format!("/api/v1/organizations/{org_a}")),
            &bob,
        ))
        .await;
    assert_eq!(
        get_as_bob.status(),
        StatusCode::NOT_FOUND,
        "B cannot read A"
    );

    // Bob cannot mutate A's members (404, outside membership).
    let bob_id = user_id_for(&services, 2).await;
    let member_update = app
        .request(with_bearer(
            app.patch_json(
                &format!("/api/v1/organizations/{org_a}/members/{bob_id}"),
                serde_json::json!({ "role": "admin" }),
            ),
            &bob,
        ))
        .await;
    assert_eq!(
        member_update.status(),
        StatusCode::NOT_FOUND,
        "B cannot mutate A"
    );
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn one_user_has_different_roles_in_two_organizations() {
    let db = TestDb::new().await.unwrap();
    db.migrate().await.unwrap();
    let github: Arc<dyn GithubAuth> = Arc::new(FixtureGithubAuth::new(FixtureUser {
        id: 1,
        login: "alice".into(),
        display_name: "Alice".into(),
    }));
    let (services, router) = build_context(db.pool().clone(), github, "http://127.0.0.1:3002");
    let app = App { router };
    let alice = login_as_user(&services, 1, "alice").await;
    let bob = login_as_user(&services, 2, "bob").await;
    let org_a = create_org(&app, &alice, "orga").await;
    let org_b = create_org(&app, &alice, "orgb").await;

    // Bob is a developer in A and a viewer in B.
    invite_and_accept(&app, &services, &alice, org_a, "bob", 2, "developer").await;
    invite_and_accept(&app, &services, &alice, org_b, "bob", 2, "viewer").await;

    let me = app.request(with_bearer(app.get("/api/v1/me"), &bob)).await;
    assert_eq!(me.status(), StatusCode::OK);
    let body = read_json(me).await;
    let memberships = body["memberships"].as_array().unwrap();
    assert_eq!(memberships.len(), 2);
    let role_in = |org: &str| -> String {
        memberships
            .iter()
            .find(|m| m["organization_id"].as_str().unwrap() == org)
            .map(|m| m["role"].as_str().unwrap().to_string())
            .unwrap()
    };
    assert_eq!(role_in(&org_a.to_string()), "developer");
    assert_eq!(role_in(&org_b.to_string()), "viewer");
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn last_admin_cannot_be_demoted_and_owner_removal_requires_transfer() {
    let db = TestDb::new().await.unwrap();
    db.migrate().await.unwrap();
    let github: Arc<dyn GithubAuth> = Arc::new(FixtureGithubAuth::new(FixtureUser {
        id: 1,
        login: "alice".into(),
        display_name: "Alice".into(),
    }));
    let (services, router) = build_context(db.pool().clone(), github, "http://127.0.0.1:3002");
    let app = App { router };
    let alice = login_as_user(&services, 1, "alice").await;
    let alice_id = user_id_for(&services, 1).await;

    let org_a = create_org(&app, &alice, "orga").await;

    // Alice is the only admin + owner. Demoting herself must fail (last-admin).
    let demote = app
        .request(with_bearer(
            app.patch_json(
                &format!("/api/v1/organizations/{org_a}/members/{alice_id}"),
                serde_json::json!({ "role": "developer" }),
            ),
            &alice,
        ))
        .await;
    assert_eq!(
        demote.status(),
        StatusCode::CONFLICT,
        "last admin demotion rejected"
    );

    // Removing the owner without transfer must fail.
    let remove_owner = app
        .request(with_bearer(
            app.delete(&format!("/api/v1/organizations/{org_a}/members/{alice_id}")),
            &alice,
        ))
        .await;
    assert_eq!(
        remove_owner.status(),
        StatusCode::CONFLICT,
        "owner removal requires transfer"
    );
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn viewer_cannot_manage_members_but_can_view() {
    let db = TestDb::new().await.unwrap();
    db.migrate().await.unwrap();
    let github: Arc<dyn GithubAuth> = Arc::new(FixtureGithubAuth::new(FixtureUser {
        id: 1,
        login: "alice".into(),
        display_name: "Alice".into(),
    }));
    let (services, router) = build_context(db.pool().clone(), github, "http://127.0.0.1:3002");
    let app = App { router };
    let alice = login_as_user(&services, 1, "alice").await;
    let bob = login_as_user(&services, 2, "bob").await;

    let org_a = create_org(&app, &alice, "orga").await;
    invite_and_accept(&app, &services, &alice, org_a, "bob", 2, "viewer").await;

    // Bob (viewer) can list members (any active member may view).
    let members = app
        .request(with_bearer(
            app.get(&format!("/api/v1/organizations/{org_a}/members")),
            &bob,
        ))
        .await;
    assert_eq!(members.status(), StatusCode::OK, "viewer can list members");

    // Bob (viewer) cannot invite -> 403.
    let invite_forbidden = app
        .request(with_bearer(
            app.post_json(
                &format!("/api/v1/organizations/{org_a}/invitations"),
                serde_json::json!({ "github_login": "carol", "role": "viewer" }),
            ),
            &bob,
        ))
        .await;
    assert_eq!(
        invite_forbidden.status(),
        StatusCode::FORBIDDEN,
        "viewer cannot invite"
    );
}

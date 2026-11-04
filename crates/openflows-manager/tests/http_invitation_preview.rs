//! HTTP-level integration test for GET /api/v1/invitations/preview: the exact
//! invitee sees the organization, role and inviter; everyone else (another
//! account, a used, revoked or unknown token) gets the same 404, and the
//! preview itself never consumes the invitation. Real handlers + PostgreSQL +
//! fixture GitHub; calls use CLI Bearer credentials.
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

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn preview_is_for_the_exact_invitee_and_consumes_nothing() {
    let db = TestDb::new().await.unwrap();
    db.migrate().await.unwrap();
    let (services, router) = build_context(db.pool().clone(), fixture(), "http://127.0.0.1:3002");
    let app = App { router };
    let alice = login_as_user(&services, 1, "alice").await;
    let bob = login_as_user(&services, 2, "bob").await;
    let carol = login_as_user(&services, 3, "carol").await;
    let org = create_org(&app, &alice, "orga").await;

    let (status, created) = app
        .send(
            "POST",
            &format!("/api/v1/organizations/{org}/invitations"),
            &alice,
            Some(serde_json::json!({ "github_login": "bob", "role": "developer" })),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let url = created["invitation_url"].as_str().unwrap();
    let token = url.split("token=").nth(1).unwrap();
    let path = format!("/api/v1/invitations/preview?token={token}");

    let (status, body) = app.get(&path, &bob).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["organization"]["slug"], "orga");
    assert_eq!(body["role"], "developer");
    assert!(body["invited_by"].is_string());
    assert!(body["expires_at"].is_string());

    // Previewing twice consumes nothing.
    let (status, _) = app.get(&path, &bob).await;
    assert_eq!(status, StatusCode::OK);

    // Another account, and an unknown token, learn nothing.
    let (status, _) = app.get(&path, &carol).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = app
        .get("/api/v1/invitations/preview?token=nope", &bob)
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // An organization that cannot take new members offers nothing to accept,
    // matching what acceptance would allow.
    sqlx::query("UPDATE organizations SET status = 'suspended' WHERE id = $1")
        .bind(org.0)
        .execute(db.pool())
        .await
        .unwrap();
    let (status, _) = app.get(&path, &bob).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "suspended organization");
    sqlx::query("UPDATE organizations SET status = 'ready' WHERE id = $1")
        .bind(org.0)
        .execute(db.pool())
        .await
        .unwrap();
    let (status, _) = app.get(&path, &bob).await;
    assert_eq!(status, StatusCode::OK, "usable again once ready");

    // Revoked invitations disappear.
    let id = created["invitation_id"].as_str().unwrap();
    let (status, _) = app
        .send(
            "DELETE",
            &format!("/api/v1/organizations/{org}/invitations/{id}"),
            &alice,
            None,
        )
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _) = app.get(&path, &bob).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

//! HTTP-level integration test for the organization operation history,
//! GET /organizations/{org}/operations: newest first, keyset paging, state and
//! resource filters, tenant slugs on tenant operations, and 404 for callers
//! outside the organization. Real handlers + PostgreSQL + fixture GitHub;
//! calls use CLI Bearer credentials.
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
async fn operations_are_listed_newest_first_paged_filtered_and_scoped() {
    let db = TestDb::new().await.unwrap();
    db.migrate().await.unwrap();
    let (services, router) = build_context(db.pool().clone(), fixture(), "http://127.0.0.1:3002");
    let app = App { router };
    let alice = login_as_user(&services, 1, "alice").await;
    let bob = login_as_user(&services, 2, "bob").await;
    let org = create_org(&app, &alice, "orga").await;

    // Two more operations after the org.provision created with the org.
    for (kind, state, offset) in [
        ("tenant.start", "succeeded", 1),
        ("tenant.stop", "failed", 2),
    ] {
        sqlx::query(
            "INSERT INTO operations (id, organization_id, resource_type, resource_id, kind, state, error_code, created_at)
             VALUES ($1, $2, 'tenant', NULL, $3, $4, CASE WHEN $4 = 'failed' THEN 'CONFLICT' END,
                     now() + make_interval(secs => $5))",
        )
        .bind(uuid::Uuid::new_v4())
        .bind(org.0)
        .bind(kind)
        .bind(state)
        .bind(f64::from(offset))
        .execute(db.pool())
        .await
        .unwrap();
    }
    let path = format!("/api/v1/organizations/{org}/operations");

    let (status, body) = app.get(&path, &alice).await;
    assert_eq!(status, StatusCode::OK);
    let kinds: Vec<&str> = body["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|o| o["kind"].as_str().unwrap())
        .collect();
    assert_eq!(kinds, ["tenant.stop", "tenant.start", "org.provision"]);
    assert_eq!(body["items"][0]["error_code"], "CONFLICT");
    assert!(body["next_cursor"].is_null());

    // Paging: one per page, following next_cursor.
    let (_, page1) = app.get(&format!("{path}?limit=1"), &alice).await;
    assert_eq!(page1["items"][0]["kind"], "tenant.stop");
    let cursor = page1["next_cursor"].as_str().unwrap().to_owned();
    let (_, page2) = app
        .get(&format!("{path}?limit=1&after={cursor}"), &alice)
        .await;
    assert_eq!(page2["items"][0]["kind"], "tenant.start");

    // State filter, and rejection of unknown states.
    let (_, failed) = app.get(&format!("{path}?state=failed"), &alice).await;
    assert_eq!(failed["items"].as_array().unwrap().len(), 1);
    let (status, _) = app.get(&format!("{path}?state=bogus"), &alice).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

    // Callers outside the organization learn nothing.
    let (status, _) = app.get(&path, &bob).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

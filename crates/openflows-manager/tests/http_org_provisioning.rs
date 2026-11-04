//! HTTP-level integration test for the provisioning summary on
//! GET /organizations/{org}: while an organization is not ready the response
//! carries its latest org.provision operation (state, step, attempts, stable
//! error code) so clients can show a failure and offer a retry. Real handlers +
//! PostgreSQL + fixture GitHub; calls use CLI Bearer credentials.
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
async fn org_detail_reports_provisioning_until_ready() {
    let db = TestDb::new().await.unwrap();
    db.migrate().await.unwrap();
    let (services, router) = build_context(db.pool().clone(), fixture(), "http://127.0.0.1:3002");
    let app = App { router };
    let alice = login_as_user(&services, 1, "alice").await;
    let org = create_org(&app, &alice, "orga").await;
    let path = format!("/api/v1/organizations/{org}");

    let (status, body) = app.get(&path, &alice).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["status"], "provisioning");
    assert_eq!(body["provisioning"]["state"], "queued");
    assert!(body["provisioning"]["operation_id"].is_string());

    // A failed attempt surfaces its stable error code, never an upstream body.
    sqlx::query(
        "UPDATE operations SET state='retryable_failed', error_code='CODER_ORG_FAILED'
          WHERE organization_id=$1 AND kind='org.provision'",
    )
    .bind(org.0)
    .execute(db.pool())
    .await
    .unwrap();
    let (_, body) = app.get(&path, &alice).await;
    assert_eq!(body["provisioning"]["state"], "retryable_failed");
    assert_eq!(body["provisioning"]["error_code"], "CODER_ORG_FAILED");

    sqlx::query("UPDATE organizations SET status='ready' WHERE id=$1")
        .bind(org.0)
        .execute(db.pool())
        .await
        .unwrap();
    let (_, body) = app.get(&path, &alice).await;
    assert!(body.get("provisioning").is_none(), "ready orgs omit it");
}

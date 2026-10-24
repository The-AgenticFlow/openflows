//! HTTP response regressions for the real Coder adapter, without live credentials.
use axum::{routing::get, Json, Router};
use coder_client::CoderOrganizationId;
use openflows_manager::coder::{CoderAdapter, RealCoderAdapter};
use serde_json::json;
use std::time::Duration;

#[tokio::test]
async fn failed_imports_are_replaced_boundedly_and_success_is_reused() {
    let archive = openflows_manager::templates::packaging::build_deterministic_archive(
        &std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../coder-client/templates/openflows-nexus"),
    )
    .unwrap();
    use axum::routing::post;
    use std::sync::{Arc, Mutex};
    for failures in 0..=3 {
        let versions = Arc::new(Mutex::new((0..failures).map(|attempt| {
            let name = if attempt == 0 { "release".to_string() } else { format!("release-retry-{attempt}") };
            json!({"id":name, "name":name, "template_id":"template", "organization_id":"org",
                "job":{"id":format!("job-{attempt}"), "status":"failed"}})
        }).collect::<Vec<_>>()));
        let listed = versions.clone();
        let created = versions.clone();
        let fetched = versions.clone();
        let router = Router::new()
            .route("/api/v2/organizations/org/templates", get(|| async {
                Json(json!([{"id":"template", "name":"openflows-nexus", "organization_id":"org"}]))
            }))
            .route("/api/v2/templates/template/versions", get(move || {
                let listed = listed.clone();
                async move { Json(listed.lock().unwrap().clone()) }
            }))
            .route("/api/v2/files", post(|| async { Json(json!({"hash":"archive"})) }))
            .route("/api/v2/organizations/org/templateversions", post(move |Json(body): Json<serde_json::Value>| {
                let created = created.clone();
                async move {
                    assert_eq!(body["template_id"], "template");
                    let version = json!({"id":body["name"], "name":body["name"], "template_id":"template", "organization_id":"org",
                        "job":{"id":"successful-job", "status":"succeeded"}});
                    created.lock().unwrap().push(version.clone());
                    Json(version)
                }
            }))
            .route("/api/v2/templateversions/{id}", get(move |axum::extract::Path(id): axum::extract::Path<String>| {
                let fetched = fetched.clone();
                async move { Json(fetched.lock().unwrap().iter().find(|v| v["id"] == id).unwrap().clone()) }
            }));
        let (adapter, server) = fixture(router).await;
        for _ in 0..2 {
            let result = adapter
                .ensure_template_version(
                    &CoderOrganizationId::new("org"),
                    "openflows-nexus",
                    "release",
                    &archive,
                )
                .await;
            if failures == 3 {
                assert!(
                    matches!(result.unwrap_err(), openflows_manager::error::ManagerError::Api(e) if e.code == "IMPORT_FAILED")
                );
            } else {
                let result = result.unwrap();
                let expected = if failures == 0 {
                    "release".to_string()
                } else {
                    format!("release-retry-{failures}")
                };
                assert_eq!(result.published.version_id.0, expected);
            }
        }
        assert_eq!(
            versions.lock().unwrap().len(),
            if failures == 3 { 3 } else { failures + 1 }
        );
        server.abort();
    }
}

async fn fixture(router: Router) -> (RealCoderAdapter, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    (
        RealCoderAdapter::from_parts(&base, "fixture-token", "v2.37.3"),
        server,
    )
}

#[tokio::test]
async fn idle_and_busy_daemons_with_the_expected_key_are_online() {
    for status in ["idle", "busy"] {
        let router = Router::new().route(
            "/api/v2/organizations/org/provisionerdaemons",
            get(move || async move {
                Json(json!([
                    {"id":"unkeyed", "status":null, "key_name":null, "last_seen_at":null},
                    {"id":"daemon", "status":status, "key_name":"openflows-provisioner-org"}
                ]))
            }),
        );
        let (adapter, server) = fixture(router).await;
        let result = tokio::time::timeout(
            Duration::from_secs(1),
            adapter.ensure_provisioner_ready(&CoderOrganizationId::new("org"), "docker"),
        )
        .await;
        server.abort();
        result
            .expect("online daemon must pass without polling")
            .unwrap();
    }
}

#[tokio::test]
async fn wrong_key_and_offline_daemons_do_not_pass_readiness() {
    for (status, key) in [
        ("idle", "another-key"),
        ("offline", "openflows-provisioner-org"),
        ("ready", "openflows-provisioner-org"),
    ] {
        let router =
            Router::new().route(
                "/api/v2/organizations/org/provisionerdaemons",
                get(move || async move {
                    Json(json!([{"id":"daemon", "status":status, "key_name":key}]))
                }),
            );
        let (adapter, server) = fixture(router).await;
        let result = tokio::time::timeout(
            Duration::from_millis(100),
            adapter.ensure_provisioner_ready(&CoderOrganizationId::new("org"), "docker"),
        )
        .await;
        server.abort();
        assert!(result.is_err(), "unqualified daemon must remain pending");
    }
}

#[tokio::test]
async fn existing_versions_require_import_job_evidence() {
    for job in [
        serde_json::Value::Null,
        json!({"id":"", "status":"succeeded"}),
    ] {
        let router = Router::new()
            .route("/api/v2/organizations/org/templates", get(|| async {
                Json(json!([{"id":"template", "name":"openflows-nexus", "organization_id":"org"}]))
            }))
            .route("/api/v2/templates/template/versions", get(move || {
                let job = job.clone();
                async move {
                    Json(json!([{
                        "id":"version",
                        "name":"release",
                        "template_id":"template",
                        "organization_id":"org",
                        "job":job
                    }]))
                }
            }));
        let (adapter, server) = fixture(router).await;
        let result = adapter
            .ensure_template_version(
                &CoderOrganizationId::new("org"),
                "openflows-nexus",
                "release",
                &[],
            )
            .await;
        server.abort();
        let error = result.unwrap_err();
        assert!(
            matches!(error, openflows_manager::error::ManagerError::Api(ref e) if e.code == "IMPORT_JOB_UNKNOWN")
        );
    }
}

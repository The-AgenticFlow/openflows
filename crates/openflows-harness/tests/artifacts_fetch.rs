//! The artifact fetch path over real HTTP.
//!
//! These tests drive `fetch` against a stub Manager that speaks the same
//! contract as the real route, so the request, the digest header, the failure
//! modes and the install are exercised end to end rather than against a mock
//! of our own code.

use axum::extract::State;
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use openflows_harness::artifacts::fetch;
use std::sync::Arc;

#[derive(Clone)]
struct Stub {
    archive: Arc<Vec<u8>>,
    /// The digest the stub reports, which a misbehaving Manager could get wrong.
    digest: String,
    token: String,
}

async fn artifacts(State(stub): State<Stub>, headers: HeaderMap) -> Response {
    let authorized = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value == format!("Bearer {}", stub.token));
    if !authorized {
        return (StatusCode::UNAUTHORIZED, "no runtime credential").into_response();
    }
    let mut response = (*stub.archive).clone().into_response();
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        header::HeaderValue::from_static("application/gzip"),
    );
    headers.insert(
        header::HeaderName::from_static("x-artifact-digest"),
        header::HeaderValue::from_str(&stub.digest).unwrap(),
    );
    response
}

/// Start the stub Manager on an ephemeral port and return its base URL.
async fn serve(digest: String, token: &str) -> String {
    let stub = Stub {
        archive: Arc::new(workspace_assets::archive().unwrap()),
        digest,
        token: token.to_string(),
    };
    let app = Router::new()
        .route("/api/v1/runtime/artifacts", get(artifacts))
        .with_state(stub);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    format!("http://{address}")
}

#[tokio::test]
async fn installs_the_bundle_the_manager_serves() {
    let archive = workspace_assets::archive().unwrap();
    let digest = workspace_assets::digest(&archive);
    let url = serve(digest.clone(), "runtime-token").await;
    let dir = tempfile::tempdir().unwrap();

    let installed = fetch(&url, "runtime-token", None, dir.path())
        .await
        .unwrap();

    assert_eq!(installed.digest, digest);
    assert_eq!(
        installed.files,
        workspace_assets::tree().len(),
        "every bundled file should be installed"
    );
    for path in [
        "orchestration/agent/agents/forge.agent.md",
        "orchestration/agent/roles/forge.skills",
        "orchestration/plugin/commands/plan.md",
        "orchestration/agent/standards/SECURITY.md",
        "plugin/hooks/forge/session_start.sh",
    ] {
        assert!(dir.path().join(path).is_file(), "missing {path}");
    }
}

#[tokio::test]
async fn a_rejected_credential_fails_without_writing_anything() {
    let archive = workspace_assets::archive().unwrap();
    let digest = workspace_assets::digest(&archive);
    let url = serve(digest, "runtime-token").await;
    let dir = tempfile::tempdir().unwrap();

    let error = fetch(&url, "wrong-token", None, dir.path())
        .await
        .unwrap_err();

    assert!(
        error.to_string().contains("401"),
        "the failure should name the rejection: {error}"
    );
    assert_eq!(
        std::fs::read_dir(dir.path()).unwrap().count(),
        0,
        "a rejected fetch must not leave a partial bundle"
    );
}

#[tokio::test]
async fn a_bundle_that_does_not_match_its_digest_is_refused() {
    let url = serve("0".repeat(64), "runtime-token").await;
    let dir = tempfile::tempdir().unwrap();

    let error = fetch(&url, "runtime-token", None, dir.path())
        .await
        .unwrap_err();

    assert!(
        error.to_string().contains("integrity check failed"),
        "unexpected error: {error}"
    );
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
}

/// A pinned digest the Manager does not serve is answered with 404 by the real
/// route, before any bytes are sent. The client-side pin check that covers a
/// Manager which ignores the pin is exercised by the unit tests.
#[tokio::test]
async fn a_pinned_digest_that_the_manager_does_not_serve_is_refused() {
    let archive = workspace_assets::archive().unwrap();
    let digest = workspace_assets::digest(&archive);
    let url = serve(digest, "runtime-token").await;
    let dir = tempfile::tempdir().unwrap();

    let error = fetch(&url, "runtime-token", Some(&"1".repeat(64)), dir.path())
        .await
        .unwrap_err();

    assert!(
        error.to_string().contains("404"),
        "unexpected error: {error}"
    );
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
}

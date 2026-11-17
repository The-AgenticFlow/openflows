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
use openflows_harness::artifacts::{fetch, fetch_own_release};
use std::sync::Arc;

#[derive(Clone)]
struct Stub {
    archive: Arc<Vec<u8>>,
    /// The digest the stub reports, which a misbehaving Manager could get wrong.
    digest: String,
    token: String,
}

fn authorized(headers: &HeaderMap, token: &str) -> bool {
    headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value == format!("Bearer {token}"))
}

fn bundle(stub: &Stub) -> Response {
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

async fn artifacts(State(stub): State<Stub>, headers: HeaderMap) -> Response {
    if !authorized(&headers, &stub.token) {
        return (StatusCode::UNAUTHORIZED, "no runtime credential").into_response();
    }
    bundle(&stub)
}

/// The pinned route, faithful to the real one: only the digest it carries, and
/// a 404 that names the digest it does serve otherwise.
async fn pinned(
    State(stub): State<Stub>,
    axum::extract::Path(digest): axum::extract::Path<String>,
    headers: HeaderMap,
) -> Response {
    if !authorized(&headers, &stub.token) {
        return (StatusCode::UNAUTHORIZED, "no runtime credential").into_response();
    }
    if digest != stub.digest {
        return (
            StatusCode::NOT_FOUND,
            format!(
                "workspace artifact bundle {digest} is not served by this Manager; \
                 available digest is {}",
                stub.digest
            ),
        )
            .into_response();
    }
    bundle(&stub)
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
        .route("/api/v1/runtime/artifacts/{digest}", get(pinned))
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

#[tokio::test]
async fn installs_the_release_this_binary_was_built_with() {
    let url = serve(release_digest(), "runtime-token").await;
    let dir = tempfile::tempdir().unwrap();

    let installed = fetch_own_release(&url, "runtime-token", dir.path())
        .await
        .unwrap();

    assert_eq!(installed.digest, release_digest());
    assert_eq!(installed.files, workspace_assets::tree().len());
}

/// A Manager on another release must not hand assets to a harness that would
/// run them, and the failure has to name the release and the way out.
#[tokio::test]
async fn refuses_a_manager_serving_a_different_release() {
    let url = serve(
        workspace_assets::digest(b"another release"),
        "runtime-token",
    )
    .await;
    let dir = tempfile::tempdir().unwrap();

    let error = fetch_own_release(&url, "runtime-token", dir.path())
        .await
        .unwrap_err()
        .to_string();
    let expected = release_digest();

    assert!(
        error.contains(&expected),
        "error should name the release: {error}"
    );
    assert!(
        error.contains("--any-release"),
        "error should name the way out: {error}"
    );
    assert_eq!(
        std::fs::read_dir(dir.path()).unwrap().count(),
        0,
        "a refused release must not leave a partial bundle"
    );
}

fn release_digest() -> String {
    openflows_harness::artifacts::release_digest().unwrap()
}

//! Workspace artifact bundle delivery.
//!
//! Every role workspace needs the same immutable assets at boot: the persona,
//! the skill and command trees, the standards, and the hooks. They used to
//! arrive only through a shared volume that the Nexus workspace seeded, so a
//! worker that started first read an empty volume. The Manager carries the same
//! bundle as the release that built it, so a workspace can fetch and verify it
//! without depending on another workspace being up.
//!
//! The bundle is content-addressed: [`current`] serves the release the Manager
//! runs, and [`pinned`] serves that exact archive only. A workspace that pins a
//! digest it does not get back refuses to start rather than running a mix of
//! versions.

use crate::error::ManagerError;
use crate::routes::auth::bearer_token;
use crate::server::AppState;
use axum::extract::{Path, State};
use axum::http::{header, HeaderMap};
use axum::response::{IntoResponse, Response};

/// Response header carrying the digest of the archive in the body. A workspace
/// compares it to the digest it asked for before unpacking.
pub const ARTIFACT_DIGEST_HEADER: &str = "x-artifact-digest";

struct Bundle {
    archive: Vec<u8>,
    digest: String,
    /// The digest is derived, so it is reported for diagnostics alongside it.
    entry_count: usize,
}

/// The bundle is built once per process. It is the same bytes for every tenant,
/// so it is neither tenant-scoped nor cacheable per caller.
fn bundle() -> &'static Bundle {
    static BUNDLE: std::sync::OnceLock<Bundle> = std::sync::OnceLock::new();
    BUNDLE.get_or_init(|| {
        let archive = workspace_assets::archive()
            .expect("the embedded workspace artifact bundle must serialize");
        Bundle {
            digest: workspace_assets::digest(&archive),
            entry_count: workspace_assets::files().len(),
            archive,
        }
    })
}

fn archive_response(bundle: &Bundle, immutable: bool) -> Response {
    let mut response = bundle.archive.clone().into_response();
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        header::HeaderValue::from_static("application/gzip"),
    );
    headers.insert(
        header::HeaderName::from_static(ARTIFACT_DIGEST_HEADER),
        header::HeaderValue::from_str(&bundle.digest)
            .expect("a sha256 hex digest is a valid header value"),
    );
    headers.insert(
        header::CACHE_CONTROL,
        header::HeaderValue::from_static(if immutable {
            "public, max-age=31536000, immutable"
        } else {
            "no-cache"
        }),
    );
    response
}

/// Authenticate without consulting the human session manager, exactly like the
/// rest of the runtime surface.
async fn authenticate(state: &AppState, headers: &HeaderMap) -> Result<(), ManagerError> {
    let services = state
        .services()
        .ok_or_else(|| ManagerError::api("SERVICE_UNAVAILABLE", "service unavailable"))?;
    let bearer = bearer_token(headers)
        .ok_or_else(|| ManagerError::api("UNAUTHORIZED", "runtime authentication required"))?;
    services.runtime_credentials.authenticate(&bearer).await?;
    Ok(())
}

/// `GET /runtime/artifacts` — the bundle of the release the Manager runs.
pub async fn current(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Response, ManagerError> {
    authenticate(&state, &headers).await?;
    Ok(archive_response(bundle(), false))
}

/// `GET /runtime/artifacts/{digest}` — only that exact bundle. A workspace pins
/// the digest its release expects, so a Manager serving a different release
/// fails the pin instead of handing over an unexpected version.
pub async fn pinned(
    State(state): State<AppState>,
    Path(digest): Path<String>,
    headers: HeaderMap,
) -> Result<Response, ManagerError> {
    authenticate(&state, &headers).await?;
    let bundle = bundle();
    if digest != bundle.digest {
        return Err(ManagerError::api(
            "RESOURCE_NOT_FOUND",
            format!(
                "workspace artifact bundle {digest} is not served by this Manager; \
                 available digest is {} ({} files). Rebuild the workspace from a \
                 release that matches, or restart it to fetch the current bundle.",
                bundle.digest, bundle.entry_count
            ),
        ));
    }
    Ok(archive_response(bundle, true))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_served_bundle_is_content_addressed_and_stable() {
        let first = bundle();
        let second = bundle();
        assert_eq!(first.digest, second.digest);
        assert_eq!(first.digest, workspace_assets::digest(&first.archive));
        assert!(first.entry_count > 0);
    }

    #[test]
    fn the_response_carries_the_digest_header() {
        let bundle = bundle();
        let response = archive_response(bundle, true);
        assert_eq!(
            response
                .headers()
                .get(ARTIFACT_DIGEST_HEADER)
                .unwrap()
                .to_str()
                .unwrap(),
            bundle.digest
        );
        assert_eq!(
            response.headers().get(header::CONTENT_TYPE).unwrap(),
            "application/gzip"
        );
    }
}

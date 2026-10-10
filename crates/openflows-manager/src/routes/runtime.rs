//! Runtime credential routes.
//!
//! These endpoints are authenticated by a workspace runtime bearer credential
//! — a principal distinct from human browser/CLI sessions and human API
//! credentials. They deliberately do not consult the human session manager, so
//! a browser cookie or human access token can never authorize a runtime
//! endpoint. Newly issued secrets are returned only to the authenticated
//! runtime, with `Cache-Control: no-store`.

use crate::error::ManagerError;
use crate::routes::auth::bearer_token;
use crate::runtime::permissions::RuntimePurpose;
use crate::server::AppState;
use axum::extract::State;
use axum::http::{header, HeaderMap};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::{Deserialize, Serialize};

/// The body of `POST /runtime/github-credentials`. Only the coarse `purpose` is
/// caller-supplied; all authorization scope (organization, installation,
/// repository, permission profile) is derived server-side.
#[derive(Deserialize)]
pub struct BrokerRequest {
    pub purpose: String,
}

/// The response returned to the authorized runtime. The token must never be
/// logged or persisted by the client beyond its short-lived use.
#[derive(Serialize)]
pub struct BrokerResponse {
    pub token: String,
    pub expires_at: chrono::DateTime<chrono::Utc>,
    pub repository_id: i64,
    pub username: String,
    pub permissions: std::collections::BTreeMap<String, String>,
}

/// POST /api/v1/runtime/github-credentials
///
/// Authenticated exclusively by a workspace runtime credential. Returns a
/// short-lived GitHub installation token scoped to the workspace's authorized
/// repository and permission profile. Human credentials are rejected.
pub async fn github_credentials(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<BrokerRequest>,
) -> Result<Response, ManagerError> {
    let services = state
        .services()
        .ok_or_else(|| ManagerError::api("SERVICE_UNAVAILABLE", "service unavailable"))?;

    let bearer = bearer_token(&headers)
        .ok_or_else(|| ManagerError::api("UNAUTHORIZED", "runtime authentication required"))?;

    // Authenticate as a workspace runtime principal only. This never touches
    // the human session manager, so browser cookies / human tokens are rejected.
    let scope = services.runtime_credentials.authenticate(&bearer).await?;

    let purpose = RuntimePurpose::parse(&body.purpose)?;
    let request_id = crate::routes::organizations::request_id(&headers);

    let credential = services
        .broker
        .exchange(&scope, purpose, &request_id)
        .await?;

    let body = BrokerResponse {
        token: credential.token,
        expires_at: credential.expires_at,
        repository_id: credential.repository_id,
        username: credential.username,
        permissions: credential.permissions,
    };
    let mut response = Json(body).into_response();
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        "no-store"
            .parse()
            .map_err(|_| ManagerError::Service(anyhow::anyhow!("invalid cache-control header")))?,
    );
    Ok(response)
}

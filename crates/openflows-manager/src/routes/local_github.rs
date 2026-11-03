//! Local-only GitHub user credential broker for trusted local workspaces.

use crate::auth::crypto::ct_eq;
use crate::error::ManagerError;
use crate::server::AppState;
use axum::extract::State;
use axum::http::{header, HeaderMap, HeaderValue};
use axum::Json;
use serde::{Deserialize, Serialize};

#[derive(Deserialize)]
pub struct CredentialRequest {
    pub repository: String,
}

#[derive(Serialize)]
pub struct CredentialResponse {
    pub token: String,
    pub repository_id: i64,
}

pub async fn credential(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<CredentialRequest>,
) -> Result<(HeaderMap, Json<CredentialResponse>), ManagerError> {
    let Some(credentials) = state.local_github() else {
        return Err(ManagerError::not_found("local GitHub credential"));
    };
    let expected = state.local_broker_key().ok_or_else(|| {
        ManagerError::api("SERVICE_UNAVAILABLE", "local GitHub broker is unavailable")
    })?;
    let presented = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .unwrap_or_default();
    if !ct_eq(presented.as_bytes(), expected.as_bytes()) {
        return Err(ManagerError::api(
            "UNAUTHORIZED",
            "invalid local broker credential",
        ));
    }
    if body.repository.len() > 200 || body.repository.split('/').count() != 2 {
        return Err(ManagerError::InvalidInput(
            "invalid GitHub repository".into(),
        ));
    }
    let token = credentials.access_token().await?;
    // Do not trust a caller's repository ID. GitHub's user-token installation
    // endpoint supplies the intersection of user and App repository access.
    let github = crate::auth::local_github::LocalGithubAuth::new(
        state
            .local_github_client_id()
            .ok_or_else(|| {
                ManagerError::api("SERVICE_UNAVAILABLE", "local GitHub broker is unavailable")
            })?
            .to_owned(),
    )?
    .with_api_base(state.github_api_base().to_owned());
    let matched = github
        .accessible_repositories(&token)
        .await?
        .into_iter()
        .find(|repo| repo.full_name.eq_ignore_ascii_case(&body.repository))
        .ok_or_else(|| ManagerError::not_found("GitHub repository"))?;
    let mut response_headers = HeaderMap::new();
    response_headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok((
        response_headers,
        Json(CredentialResponse {
            token,
            repository_id: matched.id,
        }),
    ))
}

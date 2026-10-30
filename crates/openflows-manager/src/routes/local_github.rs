//! Local-only GitHub user credential broker for trusted local workspaces.

use crate::auth::crypto::ct_eq;
use crate::auth::local_github::{DeviceChallenge, LocalGithubAuth, PollResult};
use crate::error::ManagerError;
use crate::server::AppState;
use axum::extract::State;
use axum::http::{header, HeaderMap, HeaderValue};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

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
    let expected = crate::auth::local_credentials::read_broker_key().map_err(|_| {
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

/// Browser-facing routes used only by the local Manager console. The browser
/// never receives a GitHub access or refresh token; it only receives the device
/// verification code and repository names GitHub makes available to that user.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/github", get(status))
        .route("/github/device", post(start_device_authorization))
        .route("/github/device/poll", post(poll_device_authorization))
        .route("/github/repositories", get(repositories))
        .route("/github/logout", post(logout))
}

#[derive(Serialize)]
pub struct LocalGithubStatus {
    pub enabled: bool,
    pub configured: bool,
    pub authorized: bool,
    pub installation_url: Option<String>,
}

#[derive(Serialize)]
pub struct DeviceAuthorization {
    pub user_code: String,
    pub verification_uri: String,
    pub expires_in: u64,
    pub interval: u64,
    /// Kept by the browser only while it polls. It is never persisted or logged.
    pub device_code: String,
}

#[derive(Deserialize)]
pub struct DevicePollRequest {
    pub device_code: String,
}

#[derive(Serialize)]
pub struct DevicePollResponse {
    pub status: &'static str,
    pub interval: Option<u64>,
}

#[derive(Serialize)]
pub struct RepositoryResponse {
    pub id: i64,
    pub full_name: String,
    pub private: bool,
}

fn local_github(
    state: &AppState,
) -> Result<
    (
        Arc<crate::auth::local_credentials::LocalCredentials>,
        LocalGithubAuth,
    ),
    ManagerError,
> {
    let credentials = state.local_github().cloned().ok_or_else(|| {
        ManagerError::api(
            "CONNECTED_LOCAL_UNAVAILABLE",
            "The OpenFlows GitHub App is not configured for this local installation",
        )
    })?;
    let client_id = state.local_github_client_id().ok_or_else(|| {
        ManagerError::api(
            "CONNECTED_LOCAL_UNAVAILABLE",
            "The OpenFlows GitHub App is not configured for this local installation",
        )
    })?;
    let github = LocalGithubAuth::new(client_id.to_owned())?
        .with_api_base(state.github_api_base().to_owned());
    Ok((credentials, github))
}

async fn status(State(state): State<AppState>) -> Json<LocalGithubStatus> {
    let enabled = state.connected_local_enabled();
    let configured = state.local_github().is_some();
    let authorized = match state.local_github() {
        Some(credentials) => credentials.access_token().await.is_ok(),
        None => false,
    };
    let installation_url = state
        .local_github_app_slug()
        .map(|slug| format!("https://github.com/apps/{slug}/installations/new"));
    Json(LocalGithubStatus {
        enabled,
        configured,
        authorized,
        installation_url,
    })
}

async fn start_device_authorization(
    State(state): State<AppState>,
) -> Result<Json<DeviceAuthorization>, ManagerError> {
    let (_, github) = local_github(&state)?;
    let challenge = github.start().await?;
    Ok(Json(DeviceAuthorization {
        user_code: challenge.user_code,
        verification_uri: challenge.verification_uri,
        expires_in: challenge.expires_in,
        interval: challenge.interval.max(1),
        device_code: challenge.device_code,
    }))
}

async fn poll_device_authorization(
    State(state): State<AppState>,
    Json(body): Json<DevicePollRequest>,
) -> Result<Json<DevicePollResponse>, ManagerError> {
    if body.device_code.trim().is_empty() || body.device_code.len() > 256 {
        return Err(ManagerError::InvalidInput("invalid device code".into()));
    }
    let (credentials, github) = local_github(&state)?;
    let challenge = DeviceChallenge {
        device_code: body.device_code,
        user_code: String::new(),
        verification_uri: String::new(),
        expires_in: 0,
        interval: 0,
    };
    match github.poll(&challenge).await? {
        PollResult::Pending => Ok(Json(DevicePollResponse {
            status: "authorization_pending",
            interval: None,
        })),
        PollResult::SlowDown(interval) => Ok(Json(DevicePollResponse {
            status: "slow_down",
            interval,
        })),
        PollResult::Authorized(token) => {
            credentials.save_authorization(token).await?;
            crate::auth::local_credentials::ensure_broker_key()?;
            Ok(Json(DevicePollResponse {
                status: "authorized",
                interval: None,
            }))
        }
    }
}

async fn repositories(
    State(state): State<AppState>,
) -> Result<Json<Vec<RepositoryResponse>>, ManagerError> {
    let (credentials, github) = local_github(&state)?;
    let token = credentials.access_token().await?;
    let repositories = github
        .accessible_repositories(&token)
        .await?
        .into_iter()
        .map(|repo| RepositoryResponse {
            id: repo.id,
            full_name: repo.full_name,
            private: repo.private,
        })
        .collect();
    Ok(Json(repositories))
}

async fn logout(State(state): State<AppState>) -> Result<(), ManagerError> {
    let (credentials, _) = local_github(&state)?;
    credentials.sign_out().await
}

#[cfg(test)]
mod tests {
    use super::*;
    use pocketflow_core::SharedStore;

    #[tokio::test]
    async fn local_console_reports_unconnected_app_without_needing_a_broker_key() {
        let directory = tempfile::tempdir().unwrap();
        let github = Arc::new(LocalGithubAuth::new("test-public-client-id".into()).unwrap());
        let state = AppState::with_local_github_for_tests(
            SharedStore::new_in_memory_with_tenant("test"),
            directory.path().join("github.json"),
            github,
            "openflows-test",
        )
        .unwrap();
        let Json(result) = status(State(state)).await;
        assert!(result.enabled);
        assert!(result.configured);
        assert!(!result.authorized);
        assert_eq!(
            result.installation_url.as_deref(),
            Some("https://github.com/apps/openflows-test/installations/new")
        );
    }
}

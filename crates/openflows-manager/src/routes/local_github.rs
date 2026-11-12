//! Local-only GitHub user credential broker for trusted local workspaces.

use crate::auth::crypto::ct_eq;
use crate::auth::local_github::{DeviceChallenge, LocalGithubAuth, PollResult};
use crate::error::ManagerError;
use crate::server::AppState;
use axum::extract::State;
use axum::http::{header, uri::Authority, HeaderMap, HeaderValue};
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

pub(crate) async fn status(State(state): State<AppState>) -> Json<LocalGithubStatus> {
    let enabled = state.connected_local_enabled();
    let configured = state.local_github().is_some();
    let authorized = match state.local_github() {
        Some(credentials) => credentials.access_token().await.is_ok(),
        None => false,
    };
    let installation_url = state.local_github_app_slug().map(|slug| {
        format!(
            "https://github.com/apps/{slug}/installations/new?state={}",
            crate::routes::connections::LOCAL_INSTALL_STATE
        )
    });
    Json(LocalGithubStatus {
        enabled,
        configured,
        authorized,
        installation_url,
    })
}

pub(crate) async fn start_device_authorization(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<DeviceAuthorization>, ManagerError> {
    require_local_console_request(&headers)?;
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

pub(crate) async fn poll_device_authorization(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<DevicePollRequest>,
) -> Result<(HeaderMap, Json<DevicePollResponse>), ManagerError> {
    require_local_console_request(&headers)?;
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
        PollResult::Pending => Ok((
            HeaderMap::new(),
            Json(DevicePollResponse {
                status: "authorization_pending",
                interval: None,
            }),
        )),
        PollResult::SlowDown(interval) => Ok((
            HeaderMap::new(),
            Json(DevicePollResponse {
                status: "slow_down",
                interval,
            }),
        )),
        PollResult::Authorized(token) => {
            let services = state.services().ok_or_else(|| {
                ManagerError::api(
                    "SERVICE_UNAVAILABLE",
                    "Manager session service is unavailable",
                )
            })?;
            let user = services.github.fetch_user(&token.access_token).await?;
            let display_name = user
                .display_name
                .clone()
                .unwrap_or_else(|| user.login.clone());
            let (user_id, _) = services
                .users
                .find_or_create_user(
                    services.db.pool(),
                    "github",
                    &user.id.to_string(),
                    &user.login,
                    &display_name,
                )
                .await?;
            let session = services.sessions.create_browser_session(user_id).await?;
            services
                .session_manager
                .record_recent_auth(session.session_id)
                .await?;
            credentials.save_authorization(token).await?;
            crate::auth::local_credentials::ensure_broker_key()?;
            let mut headers = HeaderMap::new();
            headers.insert(
                header::SET_COOKIE,
                crate::routes::auth::session_cookie_header(
                    crate::routes::auth::SESSION_COOKIE,
                    &session.access_token,
                    services.auth_config.cookie_secure,
                    12 * 3600,
                )
                .parse()
                .map_err(|_| ManagerError::Service(anyhow::anyhow!("invalid session cookie")))?,
            );
            Ok((
                headers,
                Json(DevicePollResponse {
                    status: "authorized",
                    interval: None,
                }),
            ))
        }
    }
}

fn require_local_console_request(headers: &HeaderMap) -> Result<(), ManagerError> {
    // The local console is exposed on the host loopback interface. A custom
    // header prevents cross-site simple requests, while the Host check also
    // rejects DNS rebinding through a non-loopback hostname.
    let host = headers
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<Authority>().ok());
    let loopback_host = host.as_ref().is_some_and(|host| {
        host.host().eq_ignore_ascii_case("localhost") || host.host() == "127.0.0.1"
    });
    let console_header = headers
        .get("x-openflows-local-console")
        .is_some_and(|value| value == "1");
    let same_origin = headers.get(header::ORIGIN).is_none_or(|origin| {
        origin.to_str().is_ok_and(|origin| {
            host.as_ref()
                .is_some_and(|host| origin.eq_ignore_ascii_case(&format!("http://{host}")))
        })
    });
    if !loopback_host || !console_header || !same_origin {
        return Err(ManagerError::api(
            "UNAUTHORIZED",
            "request must come from the local console",
        ));
    }
    Ok(())
}

pub(crate) async fn repositories(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Vec<RepositoryResponse>>, ManagerError> {
    require_local_console_request(&headers)?;
    let services = state
        .services()
        .ok_or_else(|| ManagerError::api("SERVICE_UNAVAILABLE", "service unavailable"))?;
    crate::routes::organizations::authenticated_user(&headers, &services.session_manager).await?;
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

pub(crate) async fn logout(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<(), ManagerError> {
    require_local_console_request(&headers)?;
    let services = state
        .services()
        .ok_or_else(|| ManagerError::api("SERVICE_UNAVAILABLE", "service unavailable"))?;
    let principal =
        crate::routes::organizations::authenticated_user(&headers, &services.session_manager)
            .await?;
    crate::routes::organizations::require_csrf_for_browser(&principal, &headers).await?;
    let (credentials, _) = local_github(&state)?;
    credentials.sign_out().await
}

#[cfg(test)]
mod tests {
    use super::*;
    use pocketflow_core::SharedStore;

    #[test]
    fn local_console_accepts_loopback_and_rejects_cross_site_requests() {
        let mut headers = HeaderMap::new();
        headers.insert(header::HOST, HeaderValue::from_static("localhost:3002"));
        headers.insert("x-openflows-local-console", HeaderValue::from_static("1"));
        headers.insert(
            header::ORIGIN,
            HeaderValue::from_static("http://localhost:3002"),
        );
        assert!(require_local_console_request(&headers).is_ok());
        headers.insert(
            header::ORIGIN,
            HeaderValue::from_static("https://another-site.example"),
        );
        assert!(require_local_console_request(&headers).is_err());
        headers.remove(header::ORIGIN);
        headers.insert(
            header::HOST,
            HeaderValue::from_static("another-site.example"),
        );
        assert!(require_local_console_request(&headers).is_err());
        headers.insert(header::HOST, HeaderValue::from_static("127.0.0.1:3002"));
        assert!(require_local_console_request(&headers).is_ok());
        headers.remove("x-openflows-local-console");
        assert!(require_local_console_request(&headers).is_err());
    }

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
            Some("https://github.com/apps/openflows-test/installations/new?state=of-local-v1")
        );
    }
}

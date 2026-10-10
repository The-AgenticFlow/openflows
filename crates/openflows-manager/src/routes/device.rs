//! CLI device-approval routes.
//!
//! `GET /auth/cli/verify` renders the approval page; `POST /auth/cli/approve`
//! performs the approval with CSRF protection. A GET must never approve. The
//! CLI polls `POST /auth/cli/token` with its device secret; credentials are
//! delivered once. Start, approve (verification), and polling are rate-limited
//! independently.

use crate::auth::csrf;
use crate::error::ManagerError;
use crate::rate_limit::LimitScope;
use crate::routes::auth::{parse_cookies, SESSION_COOKIE};
use crate::server::AppState;
use axum::extract::{Form, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::{Deserialize, Serialize};

const START_LIMIT: u32 = 20;
const VERIFY_LIMIT: u32 = 20;
const POLL_LIMIT: u32 = 120;

#[derive(Serialize)]
pub struct DeviceStartResponse {
    pub device_code: String,
    pub user_code: String,
    pub verification_url: String,
    pub expires_in: i64,
    pub interval: i64,
}

/// `POST /auth/cli/start` — begin a device request. Public.
pub async fn device_start(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<DeviceStartResponse>, ManagerError> {
    let services = state
        .services()
        .ok_or_else(|| ManagerError::api("SERVICE_UNAVAILABLE", "service unavailable"))?;
    let bucket = crate::rate_limit::peer_bucket(&headers);
    if !services
        .rate_limiter
        .allow(&bucket, LimitScope::DeviceStart, START_LIMIT)
        .await?
    {
        return Err(
            ManagerError::api("RATE_LIMITED", "too many device start requests").retryable(true),
        );
    }
    let req = services.device.start().await?;
    Ok(Json(DeviceStartResponse {
        device_code: req.device_secret,
        user_code: req.user_code,
        verification_url: req.verification_url,
        expires_in: req.expires_in_seconds,
        interval: req.polling_interval_seconds,
    }))
}

/// `GET /auth/cli/verify?code=XXXX-XXXX` — render the approval page. Never
/// approves anything.
pub async fn device_verify_page(
    State(state): State<AppState>,
    axum::extract::Query(q): axum::extract::Query<VerifyQuery>,
    headers: HeaderMap,
) -> Result<Response, ManagerError> {
    let services = state
        .services()
        .ok_or_else(|| ManagerError::api("SERVICE_UNAVAILABLE", "service unavailable"))?;
    let code = q.code.clone().unwrap_or_default();
    // The user must be signed in to approve.
    let authenticated = match parse_cookies(&headers).get(SESSION_COOKIE) {
        Some(cookie) => match services.session_manager.validate_browser(cookie).await? {
            Some(p) => services
                .session_manager
                .has_recent_auth(p.session_id)
                .await?
                .unwrap_or(false),
            None => false,
        },
        None => false,
    };

    // Issue a CSRF cookie for the approve form.
    let csrf_token = parse_cookies(&headers)
        .get(SESSION_COOKIE)
        .map(|s| csrf::for_session(s))
        .unwrap_or_default();
    let html = if authenticated {
        crate::pages::device_verify_page(&code, &csrf_token)
    } else {
        // Provide a sign-in link but never approve.
        let target = format!(
            "/auth/cli/verify?{}",
            url::form_urlencoded::Serializer::new(String::new())
                .append_pair("code", &code)
                .finish()
        );
        let authorize_url = crate::routes::auth::login_link(&target);
        crate::pages::device_needs_login_page(&authorize_url)
    };
    let mut response = axum::response::Html(html).into_response();
    response.headers_mut().insert(
        header::SET_COOKIE,
        crate::routes::auth::session_cookie_header(
            csrf::CSRF_COOKIE,
            &csrf_token,
            services.auth_config.cookie_secure,
            12 * 3600,
        )
        .parse()
        .map_err(|_| ManagerError::Service(anyhow::anyhow!("invalid cookie")))?,
    );
    Ok(response)
}

#[derive(Deserialize)]
pub struct VerifyQuery {
    pub code: Option<String>,
}

/// `POST /auth/cli/approve` — approve a device request. Requires a valid
/// browser session AND a matching CSRF token. A GET cannot approve.
#[derive(Deserialize)]
pub struct ApproveForm {
    pub code: String,
    #[serde(default)]
    pub _csrf: String,
}

pub async fn device_approve(
    State(state): State<AppState>,
    headers: HeaderMap,
    Form(form): Form<ApproveForm>,
) -> Result<Response, ManagerError> {
    let services = state
        .services()
        .ok_or_else(|| ManagerError::api("SERVICE_UNAVAILABLE", "service unavailable"))?;

    // CSRF: the header token must match the cookie token.
    let cookie_csrf = parse_cookies(&headers)
        .get(csrf::CSRF_COOKIE)
        .cloned()
        .unwrap_or_default();
    let header_csrf = headers
        .get(csrf::CSRF_HEADER)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    // The form carries the token too (double submit); require it to match.
    let presented = if !header_csrf.is_empty() {
        header_csrf
    } else {
        form._csrf.clone()
    };
    csrf::validate(&presented, &cookie_csrf)?;
    csrf::validate_session(&headers, &presented)?;

    // Rate-limit code verification independently.
    let bucket = crate::rate_limit::peer_bucket(&headers);
    if !services
        .rate_limiter
        .allow(&bucket, LimitScope::Verify, VERIFY_LIMIT)
        .await?
    {
        return Err(
            ManagerError::api("RATE_LIMITED", "too many verification attempts").retryable(true),
        );
    }

    // Require an authenticated browser session.
    let session_cookie = parse_cookies(&headers)
        .get(SESSION_COOKIE)
        .cloned()
        .ok_or_else(|| ManagerError::api("UNAUTHORIZED", "sign in required to approve"))?;
    let principal = services
        .session_manager
        .validate_browser(&session_cookie)
        .await?
        .ok_or_else(|| ManagerError::api("UNAUTHORIZED", "session invalid or expired"))?;

    let authenticated_at: Option<chrono::DateTime<chrono::Utc>> = sqlx::query_scalar(
        "SELECT last_authenticated_at FROM sessions WHERE id=$1 AND revoked_at IS NULL
         AND last_authenticated_at > clock_timestamp()-interval '10 minutes'",
    )
    .bind(principal.session_id.0)
    .fetch_optional(services.db.pool())
    .await
    .map_err(ManagerError::from)?
    .flatten();
    let authenticated_at = authenticated_at.ok_or_else(|| {
        ManagerError::api(
            "REAUTH_REQUIRED",
            "sign in again at /auth/github/start before approving the CLI",
        )
    })?;
    services
        .device
        .approve_authenticated(&form.code, principal.user_id, authenticated_at)
        .await?;
    let (_, html) = crate::pages::device_result_page(
        "Device approved. You may close this window.",
        StatusCode::OK.as_u16(),
    );
    Ok(axum::response::Html(html).into_response())
}

/// `POST /auth/cli/token` — poll for credentials with the device secret.
#[derive(Deserialize)]
pub struct TokenRequest {
    pub device_code: String,
}

#[derive(Serialize)]
pub struct TokenResponse {
    pub status: &'static str,
    pub access_token: Option<String>,
    pub refresh_token: Option<String>,
    pub expires_in: Option<i64>,
}

pub async fn device_token(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<TokenRequest>,
) -> Result<Json<TokenResponse>, ManagerError> {
    let services = state
        .services()
        .ok_or_else(|| ManagerError::api("SERVICE_UNAVAILABLE", "service unavailable"))?;
    let bucket = crate::rate_limit::peer_bucket(&headers);
    if !services
        .rate_limiter
        .allow(&bucket, LimitScope::Poll, POLL_LIMIT)
        .await?
    {
        return Err(ManagerError::api("RATE_LIMITED", "too many polling requests").retryable(true));
    }

    match services.device.poll(&body.device_code).await? {
        crate::auth::device::Poll::Pending => Ok(Json(TokenResponse {
            status: "authorization_pending",
            access_token: None,
            refresh_token: None,
            expires_in: None,
        })),
        crate::auth::device::Poll::Expired => Ok(Json(TokenResponse {
            status: "expired_token",
            access_token: None,
            refresh_token: None,
            expires_in: None,
        })),
        crate::auth::device::Poll::Denied => Ok(Json(TokenResponse {
            status: "access_denied",
            access_token: None,
            refresh_token: None,
            expires_in: None,
        })),
        crate::auth::device::Poll::Credentials(creds) => Ok(Json(TokenResponse {
            status: "authorized",
            access_token: Some(creds.access_token),
            refresh_token: Some(creds.refresh_token),
            expires_in: Some(15 * 60),
        })),
    }
}

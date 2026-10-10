//! Authentication routes: GitHub login, session refresh, logout, and `/me`.
//!
//! Cookie handling is explicit: browser session and transaction cookies are
//! `Secure; HttpOnly; SameSite=Lax`. CLI calls use `Authorization: Bearer
//! <access token>`. Sensitive values (cookies, OAuth codes, tokens) are never
//! written to logs, including request paths.

use crate::error::ManagerError;
use crate::rate_limit::LimitScope;
use crate::server::AppState;
use axum::extract::{Query, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Redirect, Response};
use axum::Json;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// The browser session cookie name.
pub const SESSION_COOKIE: &str = "of_session";
/// The transaction cookie set during login start.
pub const TX_COOKIE: &str = "of_login_tx";

/// The maximum attempts per window for each public entry point.
const START_LIMIT: u32 = 30;
const REFRESH_LIMIT: u32 = 60;

/// Parse the `Cookie` header into a map.
pub fn parse_cookies(headers: &HeaderMap) -> HashMap<String, String> {
    let mut out = HashMap::new();
    for value in headers.get_all(header::COOKIE) {
        if let Ok(s) = value.to_str() {
            for cookie in s.split(';') {
                let mut parts = cookie.trim().splitn(2, '=');
                if let (Some(k), Some(v)) = (parts.next(), parts.next()) {
                    out.insert(k.trim().to_string(), v.trim().to_string());
                }
            }
        }
    }
    out
}

/// Build a `Set-Cookie` header value for a session cookie.
pub fn session_cookie_header(
    name: &str,
    value: &str,
    secure: bool,
    max_age_seconds: i64,
) -> String {
    let mut parts = vec![
        format!("{name}={value}"),
        "Path=/".to_string(),
        "HttpOnly".to_string(),
        "SameSite=Lax".to_string(),
    ];
    if secure {
        parts.push("Secure".to_string());
    }
    parts.push(format!("Max-Age={max_age_seconds}"));
    parts.join("; ")
}

/// Extract a CLI bearer token from the Authorization header.
pub fn bearer_token(headers: &HeaderMap) -> Option<String> {
    let auth = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    let stripped = auth
        .strip_prefix("Bearer ")
        .or_else(|| auth.strip_prefix("bearer "))?;
    Some(stripped.trim().to_string())
}

#[derive(Deserialize)]
pub struct OAuthCallbackQuery {
    pub code: Option<String>,
    pub state: Option<String>,
    #[serde(default)]
    pub next: Option<String>,
}

/// `GET /auth/github/callback` — complete GitHub login.
pub async fn github_callback(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<OAuthCallbackQuery>,
) -> Result<Response, ManagerError> {
    let services = state
        .services()
        .ok_or_else(|| ManagerError::api("SERVICE_UNAVAILABLE", "service unavailable"))?;
    let (Some(code), Some(state_param)) = (q.code.clone(), q.state.clone()) else {
        return Err(ManagerError::api("AUTH_FAILED", "missing OAuth parameters"));
    };

    let cookies = parse_cookies(&headers);
    let cookie_state = cookies.get(TX_COOKIE).cloned().unwrap_or_default();
    let next = q.next.clone().unwrap_or_default();

    let (target, access_token) = match services
        .login
        .callback(&state_param, &cookie_state, &code, &next)
        .await
    {
        Ok(result) => result,
        Err(error) => {
            crate::audit::insert_with_pool(
                services.db.pool(),
                &crate::audit::AuditEvent::new("auth.login_failed")
                    .result(crate::audit::AuditResult::Denied)
                    .request_id(
                        crate::server::REQUEST_ID
                            .try_with(Clone::clone)
                            .unwrap_or_default(),
                    ),
            )
            .await?;
            return Err(error);
        }
    };

    let secure = services.auth_config.cookie_secure;
    let cookie = session_cookie_header(SESSION_COOKIE, &access_token, secure, 12 * 3600);
    let mut response = Redirect::temporary(&target).into_response();
    response.headers_mut().insert(
        header::SET_COOKIE,
        cookie
            .parse()
            .map_err(|_| ManagerError::Service(anyhow::anyhow!("invalid cookie")))?,
    );
    // Clear the transaction cookie (single-use).
    response.headers_mut().append(
        header::SET_COOKIE,
        format!("{TX_COOKIE}=; Path=/; HttpOnly; Max-Age=0")
            .parse()
            .map_err(|_| ManagerError::Service(anyhow::anyhow!("invalid cookie")))?,
    );
    Ok(response)
}

/// `POST /auth/refresh` — refresh a CLI credential with rotation/reuse detection.
#[derive(Deserialize)]
pub struct RefreshRequest {
    pub refresh_token: String,
}

#[derive(Serialize)]
pub struct RefreshResponse {
    pub access_token: String,
    pub refresh_token: String,
    pub expires_in: i64,
    pub refresh_expires_in: i64,
}

pub async fn refresh(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<RefreshRequest>,
) -> Result<Json<RefreshResponse>, ManagerError> {
    let services = state
        .services()
        .ok_or_else(|| ManagerError::api("SERVICE_UNAVAILABLE", "service unavailable"))?;
    let bucket = crate::rate_limit::peer_bucket(&headers);
    if !services
        .rate_limiter
        .allow(&bucket, LimitScope::Refresh, REFRESH_LIMIT)
        .await?
    {
        return Err(ManagerError::api("RATE_LIMITED", "too many refresh attempts").retryable(true));
    }

    let Some(creds) = services
        .session_manager
        .refresh_cli(&body.refresh_token)
        .await?
    else {
        return Err(ManagerError::api(
            "UNAUTHORIZED",
            "refresh credential is invalid, expired, or was reused",
        )
        .retryable(false));
    };

    let refresh_expires = creds.refresh_expires_at;
    Ok(Json(RefreshResponse {
        access_token: creds.access_token,
        refresh_token: creds.refresh_token,
        expires_in: (creds.access_expires_at - chrono::Utc::now())
            .num_seconds()
            .max(0),
        refresh_expires_in: (refresh_expires - chrono::Utc::now()).num_seconds(),
    }))
}

/// `POST /auth/logout` — revoke the current session server-side.
#[derive(Deserialize)]
pub struct LogoutRequest {
    #[serde(default)]
    pub csrf_token: Option<String>,
}

pub async fn logout(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Response, ManagerError> {
    let services = state
        .services()
        .ok_or_else(|| ManagerError::api("SERVICE_UNAVAILABLE", "service unavailable"))?;
    let principal =
        crate::routes::organizations::authenticated_user(&headers, &services.session_manager)
            .await?;
    crate::routes::organizations::require_csrf_for_browser(&principal, &headers).await?;
    let cookies = parse_cookies(&headers);

    if let Some(cookie) = cookies.get(SESSION_COOKIE) {
        if let Some(principal) = services.session_manager.validate_browser(cookie).await? {
            services
                .session_manager
                .revoke_session(principal.session_id)
                .await?;
        }
    }
    if let Some(bearer) = bearer_token(&headers) {
        if let Some(principal) = services.session_manager.validate_cli(&bearer).await? {
            services
                .session_manager
                .revoke_session(principal.session_id)
                .await?;
        }
    }

    let mut response = StatusCode::NO_CONTENT.into_response();
    response.headers_mut().insert(
        header::SET_COOKIE,
        format!("{SESSION_COOKIE}=; Path=/; HttpOnly; Max-Age=0")
            .parse()
            .map_err(|_| ManagerError::Service(anyhow::anyhow!("invalid cookie")))?,
    );
    Ok(response)
}

/// `GET /auth/login` — minimal server-rendered login page.
pub async fn login_page(State(state): State<AppState>) -> Result<Response, ManagerError> {
    let services = state
        .services()
        .ok_or_else(|| ManagerError::api("SERVICE_UNAVAILABLE", "service unavailable"))?;
    let _ = services;
    let authorize_url = "/auth/github/start".to_string();
    // This page only redirects to GitHub; it does not approve anything.
    let html = crate::pages::login_page(&authorize_url);
    Ok(axum::response::Html(html).into_response())
}

#[derive(Deserialize)]
pub struct LoginQuery {
    pub next: Option<String>,
}

/// Build an application sign-in link; the return path is saved server-side.
pub fn login_link(next: &str) -> String {
    let encoded: String = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("next", next)
        .finish();
    format!("/auth/github/start?{encoded}")
}

/// `GET /auth/github/start` — begin GitHub login, set transaction cookie, redirect.
pub async fn github_start(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<LoginQuery>,
) -> Result<Response, ManagerError> {
    let services = state
        .services()
        .ok_or_else(|| ManagerError::api("SERVICE_UNAVAILABLE", "service unavailable"))?;
    let bucket = crate::rate_limit::peer_bucket(&headers);
    if !services
        .rate_limiter
        .allow(&bucket, LimitScope::GithubStart, START_LIMIT)
        .await?
    {
        return Err(ManagerError::api("RATE_LIMITED", "too many login attempts").retryable(true));
    }
    let (authorize_url, tx) = services
        .login
        .start_with_next(q.next.as_deref().unwrap_or("/api/v1/me"))
        .await?;
    let secure = services.auth_config.cookie_secure;
    let mut response = Redirect::temporary(&authorize_url).into_response();
    response.headers_mut().insert(
        header::SET_COOKIE,
        session_cookie_header(TX_COOKIE, &tx, secure, 10 * 60)
            .parse()
            .map_err(|_| ManagerError::Service(anyhow::anyhow!("invalid cookie")))?,
    );
    Ok(response)
}

/// `GET /me` — return the current user and their memberships.
#[derive(Serialize)]
pub struct MeResponse {
    pub id: String,
    pub display_name: String,
    pub status: String,
    pub memberships: Vec<crate::dto::MembershipDto>,
}

pub async fn me(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<MeResponse>, ManagerError> {
    let services = state
        .services()
        .ok_or_else(|| ManagerError::api("SERVICE_UNAVAILABLE", "service unavailable"))?;
    let cookies = parse_cookies(&headers);

    // Resolve the principal from either the browser cookie or a CLI bearer.
    let user_id = if let Some(cookie) = cookies.get(SESSION_COOKIE) {
        let Some(p) = services.session_manager.validate_browser(cookie).await? else {
            return Err(ManagerError::api(
                "UNAUTHORIZED",
                "session is invalid or expired",
            ));
        };
        p.user_id
    } else if let Some(bearer) = bearer_token(&headers) {
        let Some(p) = services.session_manager.validate_cli(&bearer).await? else {
            return Err(ManagerError::api(
                "UNAUTHORIZED",
                "access token is invalid or expired",
            ));
        };
        p.user_id
    } else {
        return Err(ManagerError::api("UNAUTHORIZED", "authentication required"));
    };

    let memberships = services.orgs_service.list_orgs(user_id).await?;
    let user = services
        .users
        .user_dto(user_id)
        .await?
        .ok_or_else(|| ManagerError::api("UNAUTHORIZED", "user not found"))?;

    Ok(Json(MeResponse {
        id: user.id.to_string(),
        display_name: user.display_name,
        status: user.status,
        memberships,
    }))
}

pub async fn csrf_token(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Response, ManagerError> {
    let services = state
        .services()
        .ok_or_else(|| ManagerError::api("SERVICE_UNAVAILABLE", "service unavailable"))?;
    let principal =
        crate::routes::organizations::authenticated_user(&headers, &services.session_manager)
            .await?;
    if !principal.is_browser {
        return Err(ManagerError::api(
            "UNAUTHORIZED",
            "browser session required",
        ));
    }
    let cookies = parse_cookies(&headers);
    let token =
        crate::auth::csrf::for_session(cookies.get(SESSION_COOKIE).expect("browser session"));
    let mut response = Json(serde_json::json!({"csrf_token":token})).into_response();
    response.headers_mut().insert(
        header::SET_COOKIE,
        session_cookie_header(
            crate::auth::csrf::CSRF_COOKIE,
            &token,
            services.auth_config.cookie_secure,
            12 * 3600,
        )
        .parse()
        .unwrap(),
    );
    Ok(response)
}

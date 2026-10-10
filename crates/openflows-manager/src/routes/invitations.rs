//! Invitation acceptance routes.
//!
//! The invitation URL is the delivery mechanism, but possession of the URL
//! alone grants no membership: acceptance requires the authenticated user's
//! GitHub identity to match the invitee recorded on the invitation. The page is
//! rendered by GET; acceptance happens only via POST with a CSRF token.

use crate::auth::csrf;
use crate::error::ManagerError;
use crate::rate_limit::LimitScope;
use crate::routes::organizations::authenticated_user;
use crate::server::AppState;
use axum::extract::{Form, Query, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;

#[derive(Deserialize)]
pub struct AcceptQuery {
    pub token: Option<String>,
}

/// `GET /invitations/accept?token=...` — render the acceptance page. Does not
/// accept anything.
pub async fn accept_page(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<AcceptQuery>,
) -> Result<Response, ManagerError> {
    let services = state
        .services()
        .ok_or_else(|| ManagerError::api("SERVICE_UNAVAILABLE", "service unavailable"))?;
    let Some(raw_token) = q.token.clone() else {
        return Err(ManagerError::not_found("invitation"));
    };

    // Must be signed in as a browser user.
    let principal = match authenticated_user(&headers, &services.session_manager).await {
        Ok(p) => p,
        Err(ManagerError::Api(ref e)) if e.code == "UNAUTHORIZED" => {
            let target = format!(
                "/invitations/accept?{}",
                url::form_urlencoded::Serializer::new(String::new())
                    .append_pair("token", &raw_token)
                    .finish()
            );
            return Ok(
                axum::response::Redirect::to(&crate::routes::auth::login_link(&target))
                    .into_response(),
            );
        }
        Err(e) => return Err(e),
    };
    if !principal.is_browser {
        return Err(ManagerError::api(
            "UNAUTHORIZED",
            "open the invitation link in your browser while signed in",
        ));
    }

    // Resolve the invitation's organization display name for the page.
    let token_hash = crate::auth::crypto::hash_token(&raw_token);
    let inv = services.org_repo.invitation_by_token(&token_hash).await?;
    let Some(inv) = inv else {
        return Err(ManagerError::not_found("invitation"));
    };
    let subject = services.users.github_subject(principal.user_id).await?;
    if subject != Some(inv.invitee_github_user_id)
        || inv.accepted_at.is_some()
        || inv.revoked_at.is_some()
        || inv.expires_at <= chrono::Utc::now()
    {
        return Err(ManagerError::not_found("invitation"));
    }
    let display: Option<(String,)> =
        sqlx::query_as("SELECT display_name FROM organizations WHERE id = $1")
            .bind(inv.organization_id.0)
            .fetch_one(&services.orgs_service.pool)
            .await
            .ok();

    // Issue a CSRF cookie and render the accept form.
    let csrf_token = csrf::for_session(
        crate::routes::auth::parse_cookies(&headers)
            .get(crate::routes::auth::SESSION_COOKIE)
            .expect("validated browser"),
    );
    let html = crate::pages::invitation_page(
        &display
            .map(|(d,)| d)
            .unwrap_or_else(|| "the organization".to_string()),
        &inv.role.to_string(),
        &csrf_token,
        &raw_token,
    );
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
pub struct AcceptForm {
    pub token: Option<String>,
    pub _csrf: String,
}

/// `POST /invitations/accept` — accept an invitation (single-use).
pub async fn accept(
    State(state): State<AppState>,
    headers: HeaderMap,
    Form(form): Form<AcceptForm>,
) -> Result<Response, ManagerError> {
    let services = state
        .services()
        .ok_or_else(|| ManagerError::api("SERVICE_UNAVAILABLE", "service unavailable"))?;
    let principal = authenticated_user(&headers, &services.session_manager).await?;
    if !principal.is_browser {
        return Err(ManagerError::api(
            "UNAUTHORIZED",
            "accept the invitation in your browser while signed in",
        ));
    }
    csrf::validate_session(&headers, &form._csrf)?;

    let Some(raw_token) = form.token.clone() else {
        return Err(ManagerError::not_found("invitation"));
    };

    let bucket = principal.user_id.to_string();
    if !services
        .rate_limiter
        .allow(&bucket, LimitScope::Invitation, 30)
        .await?
    {
        return Err(ManagerError::api("RATE_LIMITED", "too many attempts").retryable(true));
    }

    // Resolve the caller's GitHub identity to verify the invitee.
    let github_user_id = services
        .users
        .github_subject(principal.user_id)
        .await?
        .ok_or_else(|| ManagerError::api("UNAUTHORIZED", "no GitHub identity on this account"))?;

    services
        .orgs_service
        .accept_invitation(
            principal.user_id,
            github_user_id,
            &raw_token,
            &request_id(&headers),
        )
        .await?;

    let (_, html) = crate::pages::device_result_page(
        "Invitation accepted. You can close this window.",
        StatusCode::OK.as_u16(),
    );
    Ok(axum::response::Html(html).into_response())
}

fn request_id(headers: &HeaderMap) -> String {
    headers
        .get("x-request-id")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string())
        .unwrap_or_default()
}

#[derive(Deserialize)]
pub struct AcceptJson {
    pub token: String,
}

pub async fn accept_json(
    State(state): State<AppState>,
    headers: HeaderMap,
    axum::Json(body): axum::Json<AcceptJson>,
) -> Result<axum::Json<serde_json::Value>, ManagerError> {
    let services = state
        .services()
        .ok_or_else(|| ManagerError::api("SERVICE_UNAVAILABLE", "service unavailable"))?;
    let principal = authenticated_user(&headers, &services.session_manager).await?;
    crate::routes::organizations::require_csrf_for_browser(&principal, &headers).await?;
    if !services
        .rate_limiter
        .allow(&principal.user_id.to_string(), LimitScope::Invitation, 30)
        .await?
    {
        return Err(ManagerError::api("RATE_LIMITED", "too many attempts").retryable(true));
    }
    let subject = services
        .users
        .github_subject(principal.user_id)
        .await?
        .ok_or_else(|| ManagerError::api("UNAUTHORIZED", "no GitHub identity"))?;
    let org = services
        .orgs_service
        .accept_invitation(
            principal.user_id,
            subject,
            &body.token,
            &request_id(&headers),
        )
        .await?;
    Ok(axum::Json(serde_json::json!({"organization_id":org})))
}

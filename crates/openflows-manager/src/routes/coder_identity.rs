//! Explicit proof-of-control linking; never infer identity from a name/email.
use super::organizations::{authenticated_user, require_csrf_for_browser};
use crate::{error::ManagerError, server::AppState};
use axum::{
    extract::{Query, State},
    http::{header, HeaderMap},
    response::{IntoResponse, Redirect, Response},
    Json,
};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use rand::RngCore;
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use zeroize::Zeroizing;

const COOKIE: &str = "of_coder_link";
fn hash(value: &str) -> String {
    hex::encode(Sha256::digest(value.as_bytes()))
}
fn random() -> String {
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}
fn failure() -> ManagerError {
    ManagerError::api("CODER_LINK_FAILED", "Coder account linking failed; start again and use an active GitHub-backed personal account")
}
fn configured() -> Result<(String, String, String), ManagerError> {
    let base = std::env::var("CODER_URL").map_err(|_| failure())?;
    let url = url::Url::parse(&base).map_err(|_| failure())?;
    let loopback = matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "[::1]"));
    if !(url.scheme() == "https" || (url.scheme() == "http" && loopback))
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || url.path() != "/"
    {
        return Err(failure());
    }
    let client = std::env::var("OPENFLOWS_CODER_OAUTH_CLIENT_ID").map_err(|_| failure())?;
    let secret = std::env::var("OPENFLOWS_CODER_OAUTH_CLIENT_SECRET_REF").map_err(|_| failure())?;
    if client.is_empty() || secret.is_empty() {
        return Err(failure());
    }
    Ok((base.trim_end_matches('/').into(), client, secret))
}
fn client() -> Result<reqwest::Client, ManagerError> {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(std::time::Duration::from_secs(20))
        .build()
        .map_err(|_| failure())
}

/// Exchange a linked user's retained OAuth refresh credential for a
/// short-lived Coder access token. The token is intentionally returned only to
/// Manager-internal callers; it must never be sent to a runtime or browser.
pub async fn user_access_token(
    services: &crate::server::ManagerServices,
    user_id: crate::id::UserId,
) -> Result<Zeroizing<String>, ManagerError> {
    let (base, client_id, client_secret_ref) = configured()?;
    let refresh_ref: Option<String> = sqlx::query_scalar(
        "SELECT coder_refresh_secret_ref FROM coder_user_links WHERE user_id=$1",
    )
    .bind(user_id.0)
    .fetch_optional(services.db.pool())
    .await?
    .flatten();
    let refresh_ref = refresh_ref.ok_or_else(|| {
        ManagerError::api(
            "CODER_REAUTH_REQUIRED",
            "reconnect the Coder account to authorize user-owned runtime operations",
        )
    })?;
    let refresh = Zeroizing::new(
        String::from_utf8(
            services
                .secret_provider
                .get(&refresh_ref)
                .await
                .map_err(|_| {
                    ManagerError::api("CODER_REAUTH_REQUIRED", "Coder authorization has expired")
                })?,
        )
        .map_err(|_| {
            ManagerError::api("CODER_REAUTH_REQUIRED", "Coder authorization is invalid")
        })?,
    );
    let client_secret = Zeroizing::new(
        String::from_utf8(
            services
                .secret_provider
                .get(&client_secret_ref)
                .await
                .map_err(|_| failure())?,
        )
        .map_err(|_| failure())?,
    );
    #[derive(Deserialize)]
    struct Token {
        access_token: String,
        #[serde(default)]
        refresh_token: String,
    }
    let response = client()?
        .post(format!("{base}/oauth2/tokens"))
        .basic_auth(&client_id, Some(client_secret.trim()))
        .form(&[
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh.as_str()),
        ])
        .send()
        .await
        .map_err(|_| {
            ManagerError::api(
                "CODER_REAUTH_REQUIRED",
                "Coder authorization could not be renewed",
            )
        })?;
    if !response.status().is_success() {
        return Err(ManagerError::api(
            "CODER_REAUTH_REQUIRED",
            "Coder authorization was revoked or expired; reconnect the Coder account",
        ));
    }
    let token: Token = response.json().await.map_err(|_| failure())?;
    if token.access_token.trim().is_empty() {
        return Err(failure());
    }
    if !token.refresh_token.is_empty() {
        services
            .secret_provider
            .replace(&refresh_ref, token.refresh_token.as_bytes())
            .await
            .map_err(|_| failure())?;
    }
    Ok(Zeroizing::new(token.access_token))
}

pub async fn status(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, ManagerError> {
    let services = state.services().ok_or_else(failure)?;
    let principal = authenticated_user(&headers, &services.session_manager).await?;
    let link: Option<(String, String)> =
        sqlx::query_as("SELECT coder_user_id, coder_url FROM coder_user_links WHERE user_id=$1")
            .bind(principal.user_id.0)
            .fetch_optional(services.db.pool())
            .await?;
    Ok(Json(
        json!({"configured": configured().is_ok(), "account": link.map(|(id,url)| json!({"id":id,"deployment":url}))}),
    ))
}

pub async fn start(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Response, ManagerError> {
    let services = state.services().ok_or_else(failure)?;
    let principal = authenticated_user(&headers, &services.session_manager).await?;
    require_csrf_for_browser(&principal, &headers).await?;
    // This flow requires a browser session, not a CLI bearer credential.
    if !super::auth::parse_cookies(&headers).contains_key(super::auth::SESSION_COOKIE) {
        return Err(failure());
    }
    let (base, client_id, _) = configured()?;
    let state_value = random();
    let browser = random();
    let verifier = random();
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    let mut tx = services.db.pool().begin().await?;
    sqlx::query("DELETE FROM coder_link_attempts WHERE user_id=$1 OR expires_at < now()")
        .bind(principal.user_id.0)
        .execute(&mut *tx)
        .await?;
    sqlx::query("INSERT INTO coder_link_attempts(state_hash,user_id,browser_hash,verifier,expires_at) VALUES($1,$2,$3,$4,now()+interval '10 minutes')")
        .bind(hash(&state_value)).bind(principal.user_id.0).bind(hash(&browser)).bind(verifier).execute(&mut *tx).await?;
    tx.commit().await?;
    let callback = format!(
        "{}/auth/coder/callback",
        services.auth_config.public_url.trim_end_matches('/')
    );
    let mut url = url::Url::parse(&format!("{base}/oauth2/authorize")).map_err(|_| failure())?;
    url.query_pairs_mut().extend_pairs([
        ("client_id", client_id.as_str()),
        ("response_type", "code"),
        ("redirect_uri", callback.as_str()),
        ("state", state_value.as_str()),
        ("code_challenge", challenge.as_str()),
        ("code_challenge_method", "S256"),
        ("scope", crate::operator::CODER_USER_SCOPES),
    ]);
    let cookie = super::auth::session_cookie_header(
        COOKIE,
        &browser,
        services.auth_config.cookie_secure,
        600,
    );
    Ok((
        [(header::SET_COOKIE, cookie)],
        Json(json!({"authorization_url":url.as_str()})),
    )
        .into_response())
}

#[derive(Deserialize)]
pub struct Callback {
    state: String,
    code: Option<String>,
}
pub async fn callback(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<Callback>,
) -> Result<Response, ManagerError> {
    let services = state.services().ok_or_else(failure)?;
    let principal = authenticated_user(&headers, &services.session_manager).await?;
    let cookies = super::auth::parse_cookies(&headers);
    let browser = cookies.get(COOKIE).ok_or_else(failure)?;
    let verifier: Option<String> = sqlx::query_scalar(
        "DELETE FROM coder_link_attempts WHERE state_hash=$1 AND user_id=$2 AND browser_hash=$3 AND expires_at>now() RETURNING verifier")
        .bind(hash(&query.state)).bind(principal.user_id.0).bind(hash(browser)).fetch_optional(services.db.pool()).await?;
    let verifier = Zeroizing::new(verifier.ok_or_else(failure)?);
    let code = query.code.ok_or_else(failure)?;
    let (base, client_id, secret_ref) = configured()?;
    let secret = Zeroizing::new(
        services
            .secret_provider
            .get(&secret_ref)
            .await
            .map_err(|_| failure())?,
    );
    let secret = std::str::from_utf8(&secret).map_err(|_| failure())?;
    let callback = format!(
        "{}/auth/coder/callback",
        services.auth_config.public_url.trim_end_matches('/')
    );
    let http = client()?;
    let response = http
        .post(format!("{base}/oauth2/tokens"))
        .basic_auth(&client_id, Some(secret.trim()))
        .form(&[
            ("grant_type", "authorization_code"),
            ("code", code.as_str()),
            ("redirect_uri", callback.as_str()),
            ("code_verifier", verifier.as_str()),
        ])
        .send()
        .await
        .map_err(|_| failure())?;
    if !response.status().is_success() {
        return Err(failure());
    }
    #[derive(Deserialize)]
    struct Token {
        access_token: String,
        #[serde(default)]
        refresh_token: String,
    }
    let token: Token = response.json().await.map_err(|_| failure())?;
    let refresh_token = Zeroizing::new(token.refresh_token);
    if refresh_token.is_empty() {
        return Err(ManagerError::api(
            "CODER_LINK_FAILED",
            "Coder did not issue a renewable authorization; reconnect the Coder account after the OAuth application is configured for refresh tokens",
        ));
    }
    let token = Zeroizing::new(token.access_token);
    let response = http
        .get(format!("{base}/api/v2/users/me"))
        .bearer_auth(token.as_str())
        .send()
        .await
        .map_err(|_| failure())?;
    if !response.status().is_success() {
        return Err(failure());
    }
    let user: Value = response.json().await.map_err(|_| failure())?;
    let id = verified_user_id(&user)?;
    // Retain only the renewable credential, behind the configured secret
    // provider. The short-lived access token is never persisted or returned.
    let existing: Option<(String, String, Option<String>)> = sqlx::query_as(
        "SELECT coder_url, coder_user_id, coder_refresh_secret_ref
           FROM coder_user_links WHERE user_id=$1",
    )
    .bind(principal.user_id.0)
    .fetch_optional(services.db.pool())
    .await?;
    if let Some((existing_url, existing_id, _)) = &existing {
        if existing_url != &base || existing_id != &id {
            return Err(ManagerError::Conflict(
                "Coder account already linked; ownership changes require an explicit migration"
                    .into(),
            ));
        }
    }
    let refresh_ref = match existing
        .as_ref()
        .and_then(|(_, _, reference)| reference.clone())
    {
        Some(reference) => reference,
        None => services
            .secret_provider
            .put(
                &format!("coder-user-refresh-{}", principal.user_id.0),
                refresh_token.as_bytes(),
            )
            .await
            .map_err(|_| failure())?,
    };
    if let Some((_, _, Some(reference))) = existing.as_ref() {
        services
            .secret_provider
            .replace(reference, refresh_token.as_bytes())
            .await
            .map_err(|_| failure())?;
    }
    if let Some((_, _, None)) = existing.as_ref() {
        // Legacy links created before refresh-token storage are repaired by
        // the same callback instead of failing on a nullable column.
        services
            .secret_provider
            .put(&refresh_ref, refresh_token.as_bytes())
            .await
            .map_err(|_| failure())?;
    }
    let mut tx = services.db.pool().begin().await?;
    let inserted = sqlx::query("INSERT INTO coder_user_links(user_id,coder_url,coder_user_id,coder_refresh_secret_ref) VALUES($1,$2,$3,$4) ON CONFLICT DO NOTHING")
        .bind(principal.user_id.0).bind(&base).bind(id).bind(&refresh_ref).execute(&mut *tx).await?;
    if inserted.rows_affected() == 0 {
        let same: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM coder_user_links WHERE user_id=$1 AND coder_url=$2 AND coder_user_id=$3)")
            .bind(principal.user_id.0).bind(&base).bind(id).fetch_one(&mut *tx).await?;
        if !same {
            // The INSERT was a no-op because (coder_url, coder_user_id) is
            // already claimed by a *different* OpenFlows user. The earlier
            // check only inspects the current user's link, so it cannot see
            // this conflict; surface it instead of panicking.
            return Err(ManagerError::Conflict(
                "This Coder account is already linked to another OpenFlows user".into(),
            ));
        }
        sqlx::query("UPDATE coder_user_links SET coder_refresh_secret_ref=$2 WHERE user_id=$1")
            .bind(principal.user_id.0)
            .bind(&refresh_ref)
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;
    Ok((
        [(
            header::SET_COOKIE,
            super::auth::session_cookie_header(COOKIE, "", services.auth_config.cookie_secure, 0),
        )],
        Redirect::to("/"),
    )
        .into_response())
}

fn verified_user_id(user: &Value) -> Result<&str, ManagerError> {
    let id = user["id"].as_str().ok_or_else(failure)?;
    if uuid::Uuid::parse_str(id).is_err()
        || user["login_type"] != "github"
        || user["status"] != "active"
        || user["is_service_account"].as_bool().unwrap_or(false)
    {
        return Err(failure());
    }
    Ok(id)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_active_github_humans_can_link() {
        let mut user = json!({"id":uuid::Uuid::new_v4(),"login_type":"github","status":"active","is_service_account":false});
        assert!(verified_user_id(&user).is_ok());
        for status in ["dormant", "suspended"] {
            user["status"] = json!(status);
            assert!(verified_user_id(&user).is_err());
        }
        user["status"] = json!("active");
        user["login_type"] = json!("password");
        assert!(verified_user_id(&user).is_err());
        user["login_type"] = json!("github");
        user["is_service_account"] = json!(true);
        assert!(verified_user_id(&user).is_err());
    }
}

//! Injectable GitHub user-authorization adapter.
//!
//! Human sign-in uses GitHub App user authorization (the web application flow)
//! to obtain a short-lived user access token and identify the person by their
//! immutable numeric GitHub user id. This module defines a narrow async trait
//! so handlers never talk to GitHub directly, upstream calls are bounded, and
//! tests use deterministic fixtures.
//!
//! Security rules enforced by the surrounding flow (see `oauth.rs`):
//!   * Provider access tokens are discarded after ordinary login; they are not
//!     runtime credentials and are never stored as plaintext application rows.
//!   * No database transaction is ever held across an upstream network call:
//!     handlers fetch/validate a transaction, then perform the exchange with
//!     the adapter, then commit the session atomically in a fresh transaction.
//!   * All adapter calls are bounded by an explicit timeout.

use crate::error::ManagerError;
use async_trait::async_trait;
use serde::Deserialize;
use std::time::Duration;

/// The GitHub App OAuth authorization endpoint.
pub const GITHUB_AUTHORIZE_URL: &str = "https://github.com/login/oauth/authorize";
/// The GitHub App OAuth token endpoint (code exchange).
pub const GITHUB_TOKEN_URL: &str = "https://github.com/login/oauth/access_token";
/// The authenticated user endpoint.
pub const GITHUB_USER_URL: &str = "https://api.github.com/user";

/// The result of exchanging an authorization code for a user access token.
#[derive(Clone, PartialEq, Eq)]
pub struct UserToken {
    /// The short-lived user access token (`ghu_...`).
    pub access_token: String,
    /// Optional refresh token, present when expiring tokens are enabled.
    pub refresh_token: Option<String>,
    /// Optional seconds until the access token expires.
    pub expires_in: Option<i64>,
}

/// The authenticated GitHub user, keyed by the immutable numeric id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GithubUser {
    /// GitHub's immutable numeric user id. This is the authorization subject.
    pub id: i64,
    /// The current login snapshot (never used for identity matching).
    pub login: String,
    pub display_name: Option<String>,
}

/// The result of resolving a GitHub login to an immutable id (invitations).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GithubUserLookup {
    pub id: i64,
    pub login: String,
}

/// Parameters needed to build a GitHub OAuth authorization URL.
#[derive(Debug, Clone)]
pub struct AuthorizeRequest {
    pub client_id: String,
    pub redirect_uri: String,
    pub state: String,
    pub code_challenge: String,
}

/// The GitHub user-authorization adapter. Implementations must bound every
/// network call (the real client uses a timeout; fixtures return immediately).
#[async_trait]
pub trait GithubAuth: Send + Sync {
    /// Build the URL a user is redirected to for authorization.
    fn authorize_url(&self, req: &AuthorizeRequest) -> String;

    /// Exchange an authorization code for a user access token (server side).
    async fn exchange_code(&self, code: &str, verifier: &str) -> Result<UserToken, ManagerError>;

    /// Fetch the authenticated user for a user access token.
    async fn fetch_user(&self, token: &str) -> Result<GithubUser, ManagerError>;

    /// Resolve a GitHub login to its immutable numeric id. Used when an admin
    /// invites a person by login.
    async fn resolve_login(&self, login: &str) -> Result<GithubUserLookup, ManagerError>;
}

/// A real adapter backed by `reqwest`, with a bounded timeout.
pub struct RealGithubAuth {
    client: reqwest::Client,
    client_id: String,
    client_secret: String,
    api_base: String,
    timeout: Duration,
    redirect_uri: Option<String>,
}

impl RealGithubAuth {
    pub fn with_redirect_uri(mut self, uri: String) -> Self {
        self.redirect_uri = Some(uri);
        self
    }

    pub fn new(client_id: String, client_secret: String, api_base: String) -> Self {
        RealGithubAuth {
            client: reqwest::Client::builder()
                .user_agent("openflows-manager")
                .timeout(Duration::from_secs(10))
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .expect("failed to build http client"),
            client_id,
            client_secret,
            api_base,
            timeout: Duration::from_secs(10),
            redirect_uri: None,
        }
    }
}

#[async_trait]
impl GithubAuth for RealGithubAuth {
    fn authorize_url(&self, req: &AuthorizeRequest) -> String {
        // PKCE S256 (RFC 7636). GitHub requires S256; `plain` is rejected.
        format!(
            "{GITHUB_AUTHORIZE_URL}?client_id={}&redirect_uri={}&state={}&code_challenge={}&code_challenge_method=S256&prompt=select_account",
            urlencode(&req.client_id),
            urlencode(&req.redirect_uri),
            urlencode(&req.state),
            urlencode(&req.code_challenge),
        )
    }

    async fn exchange_code(&self, code: &str, verifier: &str) -> Result<UserToken, ManagerError> {
        let mut params = vec![
            ("client_id", self.client_id.clone()),
            ("client_secret", self.client_secret.clone()),
            ("code", code.to_string()),
            ("code_verifier", verifier.to_string()),
        ];
        if let Some(uri) = &self.redirect_uri {
            params.push(("redirect_uri", uri.clone()));
        }
        let resp = tokio::time::timeout(
            self.timeout,
            self.client
                .post(GITHUB_TOKEN_URL)
                .header("Accept", "application/json")
                .form(&params)
                .send(),
        )
        .await
        .map_err(|_| ManagerError::Service(anyhow::anyhow!("github token exchange timed out")))?
        .map_err(|e| ManagerError::Service(anyhow::anyhow!("github token exchange failed: {e}")))?;

        let status = resp.status();
        let body: TokenResponse = resp
            .json()
            .await
            .map_err(|_| ManagerError::Service(anyhow::anyhow!("invalid token response")))?;

        if body.error.is_some() {
            return Err(ManagerError::api(
                "GITHUB_OAUTH_ERROR",
                "GitHub authorization failed",
            ));
        }
        if !status.is_success() {
            return Err(ManagerError::api(
                "GITHUB_OAUTH_ERROR",
                "GitHub authorization failed",
            ));
        }
        Ok(UserToken {
            access_token: body.access_token.ok_or_else(|| {
                ManagerError::api("GITHUB_OAUTH_ERROR", "GitHub returned no access token")
            })?,
            refresh_token: body.refresh_token,
            expires_in: body.expires_in,
        })
    }

    async fn fetch_user(&self, token: &str) -> Result<GithubUser, ManagerError> {
        let resp = tokio::time::timeout(
            self.timeout,
            self.client
                .get(format!("{}/user", self.api_base))
                .bearer_auth(token)
                .header("Accept", "application/vnd.github+json")
                .send(),
        )
        .await
        .map_err(|_| ManagerError::Service(anyhow::anyhow!("github user fetch timed out")))?
        .map_err(|e| ManagerError::Service(anyhow::anyhow!("github user fetch failed: {e}")))?;

        if !resp.status().is_success() {
            return Err(ManagerError::api(
                "GITHUB_OAUTH_ERROR",
                "GitHub could not identify the user",
            ));
        }
        let u: GithubUserResponse = resp
            .json()
            .await
            .map_err(|_| ManagerError::Service(anyhow::anyhow!("invalid user response")))?;
        Ok(GithubUser {
            id: u.id,
            login: u.login,
            display_name: u.name,
        })
    }

    async fn resolve_login(&self, login: &str) -> Result<GithubUserLookup, ManagerError> {
        if login.is_empty()
            || login.len() > 39
            || !login
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        {
            return Err(ManagerError::InvalidInput("invalid GitHub login".into()));
        }
        let resp = tokio::time::timeout(
            self.timeout,
            self.client
                .get(format!("{}/users/{}", self.api_base, urlencode(login)))
                .header("Accept", "application/vnd.github+json")
                .send(),
        )
        .await
        .map_err(|_| ManagerError::Service(anyhow::anyhow!("github login lookup timed out")))?
        .map_err(|e| ManagerError::Service(anyhow::anyhow!("github login lookup failed: {e}")))?;

        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Err(ManagerError::InvalidInput(format!(
                "GitHub login '{login}' not found"
            )));
        }
        if !resp.status().is_success() {
            return Err(ManagerError::api(
                "GITHUB_OAUTH_ERROR",
                "GitHub could not resolve the login",
            ));
        }
        let u: GithubUserResponse = resp
            .json()
            .await
            .map_err(|_| ManagerError::Service(anyhow::anyhow!("invalid user response")))?;
        Ok(GithubUserLookup {
            id: u.id,
            login: u.login,
        })
    }
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: Option<String>,
    refresh_token: Option<String>,
    expires_in: Option<i64>,
    error: Option<String>,
}

#[derive(Deserialize)]
struct GithubUserResponse {
    id: i64,
    login: String,
    name: Option<String>,
}

fn urlencode(s: &str) -> String {
    use url::form_urlencoded::byte_serialize;
    byte_serialize(s.as_bytes()).collect()
}

impl std::fmt::Debug for UserToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("UserToken([REDACTED])")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn authorize_url_uses_s256_and_state() {
        let auth = RealGithubAuth::new(
            "client".into(),
            "secret".into(),
            "https://api.github.com".into(),
        );
        let url = auth.authorize_url(&AuthorizeRequest {
            client_id: "client".into(),
            redirect_uri: "https://of.example/cb".into(),
            state: "state123".into(),
            code_challenge: "challenge".into(),
        });
        assert!(url.starts_with(GITHUB_AUTHORIZE_URL));
        assert!(url.contains("client_id=client"));
        assert!(url.contains("state=state123"));
        assert!(url.contains("code_challenge=challenge"));
        assert!(url.contains("code_challenge_method=S256"));
        assert!(url.contains("redirect_uri=https%3A%2F%2Fof.example%2Fcb"));
        assert!(
            !url.contains("client_secret"),
            "never leak the secret in the URL"
        );
    }
}

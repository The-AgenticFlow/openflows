//! GitHub App API client, isolated behind a testable trait.
//!
//! This covers the App-level and user-level GitHub REST endpoints used by the
//! connection lifecycle: installation metadata, accessible installations,
//! organization membership/owner verification, installation repository listing
//! (with pagination), and installation access-token exchange. The real client
//! authenticates App-level calls with a short-lived App JWT (never the private
//! key or an App JWT persisted/logged) and user-level calls with the transient
//! human OAuth token — the two are kept strictly separate.

use crate::connections::app_jwt::SignedAppJwt;
use crate::error::ManagerError;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use reqwest::StatusCode;
use std::time::Duration;

/// The account target type of an installation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccountType {
    User,
    Organization,
}

impl AccountType {
    pub fn as_str(&self) -> &'static str {
        match self {
            AccountType::User => "User",
            AccountType::Organization => "Organization",
        }
    }
}

/// Installation metadata, keyed by the immutable GitHub installation id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Installation {
    pub id: i64,
    pub app_id: i64,
    pub account_id: i64,
    pub account_type: AccountType,
    pub account_login: String,
    pub suspended: bool,
    pub suspended_by: Option<String>,
    pub repository_selection: Option<String>,
}

/// A GitHub App installation the authenticated user can *see*. This is a
/// visibility check, not proof of installation administration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccessibleInstallation {
    pub id: i64,
    pub account_id: i64,
    pub account_type: AccountType,
    pub account_login: String,
    pub suspended: bool,
}

/// The authenticated GitHub user (reused from the human auth domain).
pub use crate::auth::github::GithubUser;

/// The result of an organization membership check for a user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrgMembership {
    pub organization_login: String,
    pub organization_id: i64,
    pub role: String,  // "admin" for owners, "member" otherwise
    pub state: String, // "active" when a current member
}

impl OrgMembership {
    /// Whether the user is an active organization owner (admin role).
    pub fn is_active_owner(&self) -> bool {
        self.state == "active" && self.role == "admin"
    }
}

/// A repository owned by an installation, keyed by the immutable GitHub id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoRef {
    pub id: i64,
    pub owner_login: String,
    pub name: String,
    pub full_name: String,
    pub private: bool,
}

/// A short-lived installation access token returned by the exchange.
#[derive(Clone)]
pub struct InstallationToken {
    pub token: String,
    pub expires_at: DateTime<Utc>,
    pub repository_ids: Vec<i64>,
}

impl std::fmt::Debug for InstallationToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InstallationToken")
            .field("expires_at", &self.expires_at)
            .field("repository_ids", &self.repository_ids)
            .field("token", &"[REDACTED]")
            .finish()
    }
}

/// The App-level and user-level GitHub API surface used by the connection
/// lifecycle. Implementations must bound every network call and never return
/// raw upstream error bodies containing credentials.
#[async_trait]
pub trait GithubAppApi: Send + Sync {
    /// Fetch installation metadata by id, authenticating with the App JWT.
    /// Returns `None` when the installation is not found for this App.
    async fn installation(
        &self,
        jwt: &SignedAppJwt,
        installation_id: i64,
    ) -> Result<Option<Installation>, ManagerError>;

    /// List the installations the authenticated user can see (visibility check
    /// only), authenticating with the transient user OAuth token.
    async fn accessible_installations(
        &self,
        user_token: &str,
    ) -> Result<Vec<AccessibleInstallation>, ManagerError>;

    /// Fetch the authenticated GitHub user for a user token.
    async fn fetch_user(&self, user_token: &str) -> Result<GithubUser, ManagerError>;

    /// Fetch an organization membership for the user identified by `user_token`
    /// in the organization `org_login`. Returns `None` when the user is not a
    /// member (or the organization does not exist / is hidden).
    async fn organization_membership(
        &self,
        user_token: &str,
        org_login: &str,
    ) -> Result<Option<OrgMembership>, ManagerError>;

    /// Page through the repositories available to an installation. Each call
    /// returns up to `per_page` repositories starting at 1-based `page`; the
    /// caller drives pagination. Authenticates App-level with the App JWT.
    async fn installation_repositories(
        &self,
        jwt: &SignedAppJwt,
        installation_id: i64,
        page: u32,
        per_page: u32,
    ) -> Result<Vec<RepoRef>, ManagerError>;

    /// Exchange the App JWT for a short-lived installation access token,
    /// limited to the explicit `repository_ids`. Validates the returned
    /// repository scope so a partial/omitted scope is never silently accepted.
    async fn exchange_installation_token(
        &self,
        jwt: &SignedAppJwt,
        installation_id: i64,
        repository_ids: &[i64],
    ) -> Result<InstallationToken, ManagerError>;
}

/// The real GitHub API client backed by `reqwest`, with a bounded timeout.
pub struct RealGithubAppApi {
    client: reqwest::Client,
    api_base: String,
    timeout: Duration,
}

impl RealGithubAppApi {
    pub fn new(api_base: String) -> Self {
        RealGithubAppApi {
            client: reqwest::Client::builder()
                .user_agent("openflows-manager")
                .timeout(Duration::from_secs(10))
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .expect("failed to build http client"),
            api_base,
            timeout: Duration::from_secs(10),
        }
    }

    fn app_url(&self, path: &str) -> String {
        format!("{}{}", self.api_base.trim_end_matches('/'), path)
    }

    async fn get_json<T: serde::de::DeserializeOwned>(
        &self,
        url: &str,
        bearer: &str,
        accept: &str,
    ) -> Result<T, ManagerError> {
        let resp = tokio::time::timeout(
            self.timeout,
            self.client
                .get(url)
                .bearer_auth(bearer)
                .header("Accept", accept)
                .send(),
        )
        .await
        .map_err(|_| ManagerError::api("GITHUB_UNAVAILABLE", "GitHub request timed out"))?
        .map_err(|_| ManagerError::api("GITHUB_UNAVAILABLE", "GitHub request failed"))?;
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        if status.is_success() {
            serde_json::from_str(&text)
                .map_err(|_| ManagerError::api("GITHUB_UNAVAILABLE", "invalid GitHub response"))
        } else {
            Err(map_github_status(status))
        }
    }

    async fn post_json<T: serde::de::DeserializeOwned>(
        &self,
        url: &str,
        bearer: &str,
        body: &serde_json::Value,
        accept: &str,
    ) -> Result<T, ManagerError> {
        let resp = tokio::time::timeout(
            self.timeout,
            self.client
                .post(url)
                .bearer_auth(bearer)
                .header("Accept", accept)
                .json(body)
                .send(),
        )
        .await
        .map_err(|_| ManagerError::api("GITHUB_UNAVAILABLE", "GitHub request timed out"))?
        .map_err(|_| ManagerError::api("GITHUB_UNAVAILABLE", "GitHub request failed"))?;
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        if status.is_success() {
            serde_json::from_str(&text)
                .map_err(|_| ManagerError::api("GITHUB_UNAVAILABLE", "invalid GitHub response"))
        } else {
            Err(map_github_status(status))
        }
    }
}

fn map_github_status(status: StatusCode) -> ManagerError {
    match status {
        StatusCode::NOT_FOUND => {
            ManagerError::api("GITHUB_INSTALLATION_NOT_FOUND", "GitHub resource not found")
        }
        StatusCode::FORBIDDEN => ManagerError::api("FORBIDDEN", "GitHub denied the request"),
        StatusCode::UNAUTHORIZED => {
            ManagerError::api("AUTH_FAILED", "GitHub authentication failed")
        }
        StatusCode::TOO_MANY_REQUESTS => {
            ManagerError::api("RATE_LIMITED", "GitHub rate limit exceeded")
        }
        _ => ManagerError::api("GITHUB_UNAVAILABLE", "GitHub request failed"),
    }
}

#[async_trait]
impl GithubAppApi for RealGithubAppApi {
    async fn installation(
        &self,
        jwt: &SignedAppJwt,
        installation_id: i64,
    ) -> Result<Option<Installation>, ManagerError> {
        let url = self.app_url(&format!("/app/installations/{installation_id}"));
        match self
            .get_json::<InstallationResponse>(&url, jwt.raw(), "application/vnd.github+json")
            .await
        {
            Ok(i) => Ok(Some(Installation {
                id: i.id,
                app_id: i.app_id.unwrap_or(0),
                account_id: i.account.id,
                account_type: parse_account_type(&i.account.type_),
                account_login: i.account.login,
                suspended: i.suspended_at.is_some(),
                suspended_by: i.suspended_by.map(|user| user.login),
                repository_selection: i.repository_selection,
            })),
            Err(e) if is_not_found(&e) => Ok(None),
            Err(e) => Err(e),
        }
    }

    async fn accessible_installations(
        &self,
        user_token: &str,
    ) -> Result<Vec<AccessibleInstallation>, ManagerError> {
        let mut all = Vec::new();
        for page in 1..=100u32 {
            let url = self.app_url(&format!("/user/installations?per_page=100&page={page}"));
            let resp: InstallationsResponse = self
                .get_json(&url, user_token, "application/vnd.github+json")
                .await?;
            let count = resp.installations.len();
            all.extend(resp.installations);
            if count < 100 {
                break;
            }
        }
        Ok(all
            .into_iter()
            .map(|i| AccessibleInstallation {
                id: i.id,
                account_id: i.account.id,
                account_type: parse_account_type(&i.account.type_),
                account_login: i.account.login,
                suspended: i.suspended_at.is_some(),
            })
            .collect())
    }

    async fn fetch_user(&self, user_token: &str) -> Result<GithubUser, ManagerError> {
        let url = self.app_url("/user");
        let resp: UserResponse = self
            .get_json(&url, user_token, "application/vnd.github+json")
            .await?;
        Ok(GithubUser {
            id: resp.id,
            login: resp.login,
            display_name: resp.name,
        })
    }

    async fn organization_membership(
        &self,
        user_token: &str,
        org_login: &str,
    ) -> Result<Option<OrgMembership>, ManagerError> {
        let url = self.app_url(&format!("/user/memberships/orgs/{}", urlencode(org_login)));
        match self
            .get_json::<OrgMembershipResponse>(&url, user_token, "application/vnd.github+json")
            .await
        {
            Ok(m) => Ok(Some(OrgMembership {
                organization_login: m.organization.login,
                organization_id: m.organization.id,
                role: m.role,
                state: m.state,
            })),
            Err(e) if is_not_found(&e) => Ok(None),
            Err(e) => Err(e),
        }
    }

    async fn installation_repositories(
        &self,
        jwt: &SignedAppJwt,
        installation_id: i64,
        page: u32,
        per_page: u32,
    ) -> Result<Vec<RepoRef>, ManagerError> {
        // Discovery needs an installation token, not the App JWT. Keep this
        // metadata-only credential local to the manager; runtime exchanges
        // continue to require an explicit nonempty repository scope.
        let token: TokenResponse = self
            .post_json(
                &self.app_url(&format!(
                    "/app/installations/{installation_id}/access_tokens"
                )),
                jwt.raw(),
                &serde_json::json!({"permissions": {"metadata": "read"}}),
                "application/vnd.github+json",
            )
            .await?;
        let url = self.app_url(&format!(
            "/installation/repositories?per_page={per_page}&page={page}"
        ));
        let resp: RepositoriesResponse = self
            .get_json(&url, &token.token, "application/vnd.github+json")
            .await?;
        Ok(resp
            .repositories
            .into_iter()
            .map(|r| RepoRef {
                id: r.id,
                owner_login: r.owner.login,
                name: r.name,
                full_name: r.full_name,
                private: r.private.unwrap_or(false),
            })
            .collect())
    }

    async fn exchange_installation_token(
        &self,
        jwt: &SignedAppJwt,
        installation_id: i64,
        repository_ids: &[i64],
    ) -> Result<InstallationToken, ManagerError> {
        if repository_ids.is_empty() {
            return Err(ManagerError::Service(anyhow::anyhow!(
                "cannot exchange an installation token for an empty repository set"
            )));
        }
        let url = self.app_url(&format!(
            "/app/installations/{installation_id}/access_tokens"
        ));
        let body = serde_json::json!({ "repository_ids": repository_ids });
        let resp: TokenResponse = self
            .post_json(&url, jwt.raw(), &body, "application/vnd.github+json")
            .await?;
        let expires_at = DateTime::parse_from_rfc3339(&resp.expires_at)
            .map_err(|_| {
                ManagerError::api("GITHUB_UNAVAILABLE", "invalid token expiry from GitHub")
            })?
            .with_timezone(&Utc);
        // Validate the returned repository scope: it must cover every requested
        // repository id, otherwise fail closed (never silently narrow scope).
        let returned: std::collections::BTreeSet<i64> =
            resp.repositories.into_iter().map(|r| r.id).collect();
        let requested: std::collections::BTreeSet<i64> = repository_ids.iter().copied().collect();
        if requested != returned {
            return Err(ManagerError::api(
                "GITHUB_UNAVAILABLE",
                "GitHub returned a different repository scope than requested",
            ));
        }
        Ok(InstallationToken {
            token: resp.token,
            expires_at,
            repository_ids: returned.into_iter().collect(),
        })
    }
}

fn parse_account_type(t: &str) -> AccountType {
    match t {
        "Organization" => AccountType::Organization,
        _ => AccountType::User,
    }
}

fn is_not_found(e: &ManagerError) -> bool {
    matches!(e, ManagerError::Api(a) if a.code == "GITHUB_INSTALLATION_NOT_FOUND" || a.code == "NOT_FOUND")
}

fn urlencode(s: &str) -> String {
    use url::form_urlencoded::byte_serialize;
    byte_serialize(s.as_bytes()).collect()
}

#[derive(serde::Deserialize)]
struct AccountResponse {
    id: i64,
    login: String,
    #[serde(rename = "type")]
    type_: String,
}

#[derive(serde::Deserialize)]
struct InstallationResponse {
    id: i64,
    app_id: Option<i64>,
    account: AccountResponse,
    suspended_at: Option<String>,
    suspended_by: Option<AccountResponse>,
    repository_selection: Option<String>,
}

#[derive(serde::Deserialize)]
struct InstallationsResponse {
    installations: Vec<InstallationResponse>,
}

#[derive(serde::Deserialize)]
struct UserResponse {
    id: i64,
    login: String,
    name: Option<String>,
}

#[derive(serde::Deserialize)]
struct OrganizationResponse {
    id: i64,
    login: String,
}

#[derive(serde::Deserialize)]
struct OrgMembershipResponse {
    organization: OrganizationResponse,
    role: String,
    state: String,
}

#[derive(serde::Deserialize)]
struct RepoOwnerResponse {
    login: String,
}

#[derive(serde::Deserialize)]
struct RepoResponse {
    id: i64,
    owner: RepoOwnerResponse,
    name: String,
    full_name: String,
    private: Option<bool>,
}

#[derive(serde::Deserialize)]
struct RepositoriesResponse {
    repositories: Vec<RepoResponse>,
}

#[derive(serde::Deserialize)]
struct TokenResponse {
    token: String,
    expires_at: String,
    #[serde(default)]
    repositories: Vec<TokenRepository>,
}

#[derive(serde::Deserialize)]
struct TokenRepository {
    id: i64,
}

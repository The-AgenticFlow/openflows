//! GitHub authority verification for installation binding.
//!
//! A personal GitHub account may bind an installation only when the
//! installation's account id equals the authenticated GitHub user id. A GitHub
//! organization requires the authenticated user to be an *active organization
//! owner* (admin role) and the organization id to match the installation
//! metadata. A repository reader or ordinary organization member is never
//! accepted as an installation administrator.
//!
//! The service fails closed: any upstream or SSO failure prevents verification,
//! returning a sanitized domain error rather than silently accepting.

use crate::connections::github_app::{AccountType, GithubAppApi, Installation};
use crate::error::ManagerError;
use std::sync::Arc;

/// The result of a successful authority check, carrying the authenticated
/// GitHub user and the verified account identity.
#[derive(Debug, Clone)]
pub struct AuthorityVerdict {
    pub github_user_id: i64,
    pub account_id: i64,
    pub account_type: AccountType,
}

/// Verifies that a human (proved by a transient GitHub user OAuth token) has
/// the authority to bind a given installation. The user token is never used as
/// a runtime repository credential.
#[derive(Clone)]
pub struct AuthorityService {
    api: Arc<dyn GithubAppApi>,
}

impl AuthorityService {
    pub fn new(api: Arc<dyn GithubAppApi>) -> Self {
        AuthorityService { api }
    }

    /// Verify the authenticated user has installation-authority over the given
    /// installation. Performs only network calls (never holds a DB transaction)
    /// so the final binding transaction can recheck the Openflows admin
    /// membership with authoritative row locks.
    pub async fn verify(
        &self,
        user_token: &str,
        installation: &Installation,
    ) -> Result<AuthorityVerdict, ManagerError> {
        // Fetch the authenticated GitHub user. A failure here (including SSO
        // authorization failures) fails closed.
        let user = self.api.fetch_user(user_token).await.map_err(|_| {
            ManagerError::api(
                "GITHUB_OWNER_REQUIRED",
                "could not verify your GitHub account; sign in again",
            )
        })?;

        // Visibility check only: the user must be able to see the installation.
        let accessible = self
            .api
            .accessible_installations(user_token)
            .await
            .map_err(|_| {
                ManagerError::api(
                    "GITHUB_INSTALLATION_NOT_FOUND",
                    "GitHub installation could not be verified",
                )
            })?;
        if !accessible.iter().any(|a| a.id == installation.id) {
            return Err(ManagerError::api(
                "GITHUB_INSTALLATION_NOT_FOUND",
                "GitHub installation could not be verified",
            ));
        }

        match installation.account_type {
            AccountType::User => {
                if installation.account_id != user.id {
                    return Err(ManagerError::api(
                        "GITHUB_OWNER_REQUIRED",
                        "you must own the GitHub account the App is installed on",
                    ));
                }
                Ok(AuthorityVerdict {
                    github_user_id: user.id,
                    account_id: installation.account_id,
                    account_type: AccountType::User,
                })
            }
            AccountType::Organization => {
                // Require an active organization owner and verify the
                // organization id matches the installation metadata. Fail
                // closed on upstream membership failures (including SSO).
                let membership = self
                    .api
                    .organization_membership(user_token, &installation.account_login)
                    .await
                    .map_err(|_| {
                        ManagerError::api(
                            "GITHUB_OWNER_REQUIRED",
                            "could not verify organization membership",
                        )
                    })?;
                let Some(m) = membership else {
                    return Err(ManagerError::api(
                        "GITHUB_OWNER_REQUIRED",
                        "you are not a member of the GitHub organization",
                    ));
                };
                if m.organization_id != installation.account_id {
                    return Err(ManagerError::api(
                        "GITHUB_OWNER_REQUIRED",
                        "the GitHub organization does not match the installation",
                    ));
                }
                if !m.is_active_owner() {
                    return Err(ManagerError::api(
                        "GITHUB_OWNER_REQUIRED",
                        "a GitHub organization owner must approve the connection",
                    ));
                }
                Ok(AuthorityVerdict {
                    github_user_id: user.id,
                    account_id: installation.account_id,
                    account_type: AccountType::Organization,
                })
            }
        }
    }
}

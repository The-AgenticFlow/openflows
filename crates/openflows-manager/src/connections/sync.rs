//! Repository synchronization for a connected installation.
//!
//! Fetches the installation's repositories from GitHub with correct pagination,
//! persists immutable GitHub repository ids, updates owner/name when GitHub
//! renames a repository, and marks repositories that disappear from the
//! authoritative response as inaccessible. Synchronization is idempotent and
//! never holds a database transaction open while making GitHub requests.

use crate::connections::app_jwt::AppSigner;
use crate::connections::github_app::GithubAppApi;
use crate::connections::repository::{ConnectionRepository, ConnectionRow};
use crate::error::ManagerError;
use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

/// GitHub's maximum `per_page` for the repositories endpoint.
const PER_PAGE: u32 = 100;
/// Maximum repository pages fetched in one sync (defensive bound).
const MAX_PAGES: u32 = 100;
/// Number of bounded retry attempts for transient upstream failures.
const MAX_ATTEMPTS: u32 = 3;

/// Synchronizes repositories from an authoritative GitHub installation.
#[derive(Clone)]
pub struct SyncService {
    pub repo: ConnectionRepository,
    pub api: Arc<dyn GithubAppApi>,
    pub signer: Arc<dyn AppSigner>,
}

impl SyncService {
    pub fn new(
        repo: ConnectionRepository,
        api: Arc<dyn GithubAppApi>,
        signer: Arc<dyn AppSigner>,
    ) -> Self {
        SyncService { repo, api, signer }
    }

    /// Synchronize a connection's repositories from the authoritative GitHub
    /// response, with pagination and immutable-id persistence. Returns the
    /// number of repositories currently accessible.
    pub async fn sync_connection(&self, conn: &ConnectionRow) -> Result<usize, ManagerError> {
        let jwt = self.signer.sign().await?;
        let mut present: BTreeSet<i64> = BTreeSet::new();
        let mut page: u32 = 1;

        loop {
            if page > MAX_PAGES {
                return Err(ManagerError::Service(anyhow::anyhow!(
                    "repository synchronization exceeded the page limit"
                )));
            }
            let repos = self
                .with_retry(|| {
                    let jwt = jwt.clone();
                    let api = self.api.clone();
                    let installation_id = conn.installation_id;
                    Box::pin(async move {
                        api.installation_repositories(&jwt, installation_id, page, PER_PAGE)
                            .await
                    })
                })
                .await?;

            if repos.is_empty() {
                break;
            }
            for r in &repos {
                present.insert(r.id);
            }
            // Persist after the network call; no transaction is held open.
            self.repo
                .upsert_repositories(conn.organization_id, conn.id, &repos)
                .await?;

            if (repos.len() as u32) < PER_PAGE {
                break;
            }
            page += 1;
        }

        // Mark repositories no longer present as inaccessible (conservative).
        self.repo
            .mark_missing_inaccessible(conn.organization_id, conn.id, &present)
            .await?;
        self.repo
            .touch_reconciled(conn.organization_id, conn.id)
            .await?;
        Ok(present.len())
    }

    /// A tiny bounded retry that only retries transient upstream failures.
    async fn with_retry<F, T>(&self, f: F) -> Result<T, ManagerError>
    where
        F: Fn() -> std::pin::Pin<
                Box<dyn std::future::Future<Output = Result<T, ManagerError>> + Send>,
            > + Send,
    {
        let mut attempt = 0;
        loop {
            match f().await {
                Ok(v) => return Ok(v),
                Err(e) if attempt + 1 < MAX_ATTEMPTS && is_transient(&e) => {
                    attempt += 1;
                    tokio::time::sleep(Duration::from_millis(100 * u64::from(attempt))).await;
                }
                Err(e) => return Err(e),
            }
        }
    }
}

/// Whether an upstream error is worth a bounded retry.
fn is_transient(e: &ManagerError) -> bool {
    matches!(e, ManagerError::Api(a) if a.retryable || a.code == "GITHUB_UNAVAILABLE" || a.code == "RATE_LIMITED")
}

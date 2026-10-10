//! Immutable installation binding.
//!
//! Binds a verified GitHub App installation to exactly one Openflows
//! organization. The unique `(app_id, installation_id)` constraint guarantees
//! immutability at the database level; a connection can never move between
//! organizations through a callback or upsert. The final transaction rechecks
//! the active Openflows admin membership and the installation is not already
//! bound elsewhere, then inserts the connection, consumes the attempt, and
//! enqueues repository synchronization atomically.

use crate::audit::{self, AuditEvent};
use crate::connections::app_jwt::AppSigner;
use crate::connections::authority::AuthorityService;
use crate::connections::github_app::GithubAppApi;
use crate::connections::repository::{AttemptRow, ConnectionRepository, ConnectionRow};
use crate::dto::MembershipStatus;
use crate::error::ManagerError;
use crate::id::ConnectionId;
use crate::outbox;
use serde_json::json;
use sqlx::PgPool;
use std::sync::Arc;

/// The binding service drives the final installation binding transaction.
#[derive(Clone)]
pub struct BindingService {
    pub pool: PgPool,
    pub repo: ConnectionRepository,
    pub authority: AuthorityService,
    pub api: Arc<dyn GithubAppApi>,
    pub signer: Arc<dyn AppSigner>,
    pub app_id: i64,
}

impl BindingService {
    pub fn new(
        pool: PgPool,
        repo: ConnectionRepository,
        authority: AuthorityService,
        api: Arc<dyn GithubAppApi>,
        signer: Arc<dyn AppSigner>,
        app_id: i64,
    ) -> Self {
        BindingService {
            pool,
            repo,
            authority,
            api,
            signer,
            app_id,
        }
    }

    /// Bind the candidate installation for a fully-proven attempt using the
    /// transient user token for authority verification. Returns the bound
    /// connection. Never uses the user token as a runtime repository credential.
    pub async fn bind(
        &self,
        attempt: &AttemptRow,
        user_token: &str,
        request_id: &str,
    ) -> Result<ConnectionRow, ManagerError> {
        let installation_id = attempt.candidate_installation.ok_or_else(|| {
            ManagerError::api(
                "GITHUB_INSTALLATION_NOT_FOUND",
                "no installation was selected",
            )
        })?;

        // Fetch the authoritative installation metadata (App-level call; no DB
        // transaction held across the network request).
        let jwt = self.signer.sign().await?;
        let installation = self
            .api
            .installation(&jwt, installation_id)
            .await?
            .ok_or_else(|| {
                ManagerError::api(
                    "GITHUB_INSTALLATION_NOT_FOUND",
                    "GitHub installation not found",
                )
            })?;
        if installation.app_id != self.app_id {
            // Do not reveal the identity of the owning app.
            return Err(ManagerError::api(
                "GITHUB_INSTALLATION_NOT_FOUND",
                "GitHub installation not found",
            ));
        }
        if installation.suspended {
            return Err(ManagerError::api(
                "GITHUB_INSTALLATION_SUSPENDED",
                "this GitHub installation is suspended",
            ));
        }

        // Verify the human has authority over the installation (personal-account
        // owner or organization owner).
        let verdict = self.authority.verify(user_token, &installation).await?;

        let mut tx = self.pool.begin().await?;

        // Recheck the active Openflows admin membership with authoritative row
        // locks so concurrent revocation cannot race the final write. Lock the
        // organization row first, then the membership row (same order as every
        // other organization mutation).
        let org_id = attempt.organization_id;
        let org_status: Option<String> =
            sqlx::query_scalar("SELECT status FROM organizations WHERE id = $1 FOR UPDATE")
                .bind(org_id.0)
                .fetch_optional(&mut *tx)
                .await?;
        let Some(org_status) = org_status else {
            return Err(ManagerError::not_found("organization"));
        };
        if org_status != "ready" && org_status != "provisioning" {
            return Err(ManagerError::api(
                "ORG_UNAVAILABLE",
                "organization is not available for connection",
            ));
        }
        let member: Option<(String, String)> = sqlx::query_as(
            "SELECT m.role, m.status
               FROM memberships m
               JOIN users u ON u.id = m.user_id
              WHERE m.organization_id = $1 AND m.user_id = $2 AND u.status = 'active'
              FOR UPDATE OF m",
        )
        .bind(org_id.0)
        .bind(attempt.initiated_by.0)
        .fetch_optional(&mut *tx)
        .await?;
        let Some((role, status)) = member else {
            return Err(ManagerError::not_found("organization"));
        };
        if status != MembershipStatus::Active.as_str() || role != "admin" {
            return Err(ManagerError::api(
                "ORG_ADMIN_REQUIRED",
                "an organization admin must complete the connection",
            ));
        }

        // Recheck the installation is not already bound elsewhere (immutable).
        // If already bound to this org, return the existing connection
        // idempotently (no duplicate provisioning).
        if let Some(existing) = self
            .repo
            .connection_by_installation(self.app_id, installation_id)
            .await?
        {
            if existing.organization_id == org_id {
                if !ConnectionRepository::consume_attempt_in_tx(&mut tx, attempt.id).await? {
                    return Err(ManagerError::api(
                        "CONNECTION_ATTEMPT_EXPIRED",
                        "connection attempt is no longer valid",
                    ));
                }
                sqlx::query("UPDATE github_connections SET status = 'active',
                    access_revoked_reason = NULL, verified_at = clock_timestamp(), updated_at = now()
                    WHERE id = $1 AND organization_id = $2
                      AND access_generation = $3")
                    .bind(existing.id.0).bind(org_id.0).bind(existing.access_generation)
                    .execute(&mut *tx).await?;
                outbox::insert_in_tx(&mut tx, Some(org_id), "github.repository_sync",
                    Some(&json!({"connection_id": existing.id.to_string(), "organization_id": org_id.to_string()}))).await?;
                audit::insert(
                    &mut *tx,
                    &AuditEvent::new("github.connection_reconnected")
                        .organization(org_id)
                        .actor(attempt.initiated_by)
                        .resource("github_connection", existing.id.0)
                        .request_id(request_id),
                )
                .await?;
                tx.commit().await?;
                return self
                    .repo
                    .get_connection_scoped(org_id, existing.id)
                    .await?
                    .ok_or_else(|| ManagerError::not_found("connection"));
            }
            return Err(ManagerError::api(
                "INSTALLATION_ALREADY_BOUND",
                "this GitHub installation is already connected to an organization",
            ));
        }

        let connection_id = ConnectionId::new();
        let account_login = Some(installation.account_login.clone());
        match ConnectionRepository::insert_connection_in_tx(
            &mut tx,
            connection_id,
            org_id,
            self.app_id,
            installation_id,
            verdict.account_id,
            verdict.account_type.as_str(),
            account_login.as_deref(),
            attempt.initiated_by,
        )
        .await
        {
            Ok(()) => {}
            Err(ManagerError::Api(api)) if api.code == "INSTALLATION_ALREADY_BOUND" => {
                // A concurrent binding won the unique constraint. Report the
                // sanitized error without revealing the other organization.
                return Err(ManagerError::api(
                    "INSTALLATION_ALREADY_BOUND",
                    "this GitHub installation is already connected to an organization",
                ));
            }
            Err(e) => return Err(e),
        }

        // Consume the attempt so a replayed callback cannot re-bind or mint
        // credentials.
        if !ConnectionRepository::consume_attempt_in_tx(&mut tx, attempt.id).await? {
            return Err(ManagerError::api(
                "CONNECTION_ATTEMPT_EXPIRED",
                "connection attempt is no longer valid",
            ));
        }

        audit::insert(
            &mut *tx,
            &AuditEvent::new("github.connection_bound")
                .organization(org_id)
                .actor(attempt.initiated_by)
                .resource("github_connection", connection_id.0)
                .request_id(request_id),
        )
        .await?;

        // Enqueue repository synchronization atomically with the binding. A
        // leased worker runs it only after this transaction commits.
        outbox::insert_in_tx(
            &mut tx,
            Some(org_id),
            "github.repository_sync",
            Some(&json!({
                "connection_id": connection_id.to_string(),
                "organization_id": org_id.to_string(),
                "installation_id": installation_id,
                "app_id": self.app_id,
            })),
        )
        .await?;

        tx.commit().await?;

        let connection = self
            .repo
            .get_connection_scoped(org_id, connection_id)
            .await?
            .ok_or_else(|| ManagerError::Service(anyhow::anyhow!("bound connection missing")))?;
        Ok(connection)
    }
}

impl std::fmt::Debug for BindingService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("BindingService([REDACTED])")
    }
}

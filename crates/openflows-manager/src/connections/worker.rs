//! Durable outbox worker for GitHub connection lifecycle events.
//!
//! Claims leased outbox events and processes them: webhook deliveries,
//! repository synchronization after binding, reconciliation, and disconnect
//! cleanup. Processing is retryable and leased through the outbox foundation
//! (WP-01). A failed worker's lease expires and another worker reclaims it.

use crate::connections::repository::ConnectionRepository;
use crate::connections::sync::SyncService;
use crate::connections::webhooks::WebhookService;
use crate::error::ManagerError;
use crate::id::{ConnectionId, OrganizationId};
use crate::outbox::{self, OutboxClaim};
use sqlx::PgPool;
use std::time::Duration;

/// How long the worker sleeps between claim cycles.
const POLL_INTERVAL: Duration = Duration::from_secs(2);

/// The durable connection worker.
#[derive(Clone)]
pub struct ConnectionWorker {
    pub pool: PgPool,
    pub repo: ConnectionRepository,
    pub sync: SyncService,
    pub webhooks: WebhookService,
}

impl ConnectionWorker {
    pub fn new(
        pool: PgPool,
        repo: ConnectionRepository,
        sync: SyncService,
        webhooks: WebhookService,
    ) -> Self {
        ConnectionWorker {
            pool,
            repo,
            sync,
            webhooks,
        }
    }

    /// Run the worker loop until the passed future completes (or forever).
    pub async fn run_forever(&self) -> Result<(), ManagerError> {
        loop {
            if let Err(error) = self.run_once().await {
                tracing::warn!(%error, "connection worker poll failed; retrying");
            }
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    }

    /// Claim and process up to 10 pending events once.
    pub async fn run_once(&self) -> Result<(), ManagerError> {
        // Remove expired OAuth/setup credentials even when no lifecycle event
        // is queued, so abandoned attempts do not retain secrets indefinitely.
        self.repo.expire_attempts().await?;
        let owner = format!("connection-worker-{}", std::process::id());
        let claims = outbox::claim_filtered(
            &self.pool,
            &owner,
            1,
            Some(&[
                "webhook.process",
                "github.repository_sync",
                "github.reconcile",
                "github.disconnect_cleanup",
            ]),
        )
        .await?;
        for claim in claims {
            let process = self.process(&claim);
            tokio::pin!(process);
            let result = loop {
                tokio::select! {
                    result = &mut process => break result,
                    _ = tokio::time::sleep(Duration::from_secs(10)) => {
                        if !outbox::heartbeat(&self.pool, &claim).await? {
                            return Err(ManagerError::Service(anyhow::anyhow!("connection worker lost its lease")));
                        }
                    }
                }
            };
            match result {
                Ok(_) => {
                    outbox::complete_operation_delivery(&self.pool, &claim).await?;
                }
                Err(e) => {
                    tracing::warn!(event = %claim.event_type, error = %e, "webhook/connection event failed");
                    let permanent = matches!(&e, ManagerError::Api(a) if matches!(a.code.as_str(), "GITHUB_INSTALLATION_NOT_FOUND" | "NOT_FOUND" | "RESOURCE_NOT_FOUND" | "INVALID_INPUT"));
                    outbox::fail_delivery(&self.pool, &claim, permanent).await?;
                }
            }
        }
        Ok(())
    }

    async fn process(&self, claim: &OutboxClaim) -> Result<(), ManagerError> {
        let payload = claim.payload.clone().unwrap_or_default();
        match claim.event_type.as_str() {
            "webhook.process" => {
                let delivery_id = payload
                    .get("delivery_id")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| ManagerError::Service(anyhow::anyhow!("missing delivery_id")))?;
                self.webhooks.process_delivery(delivery_id).await?;
                Ok(())
            }
            "github.repository_sync" => {
                let conn = self.connection_from_payload(&payload).await?;
                self.sync.sync_connection(&conn).await?;
                Ok(())
            }
            "github.reconcile" => {
                let conn = self.connection_from_payload(&payload).await?;
                self.webhooks.reconcile(&conn).await?;
                Ok(())
            }
            "github.disconnect_cleanup" => {
                // Access was already disabled synchronously at disconnect time.
                // WP-04 revokes credential leases here; for WP-03 the operation
                // completes and the connection remains disconnected (binding
                // history is preserved, no row deletion).
                let _conn = self.connection_from_payload(&payload).await?;
                Ok(())
            }
            other => Err(ManagerError::Service(anyhow::anyhow!(
                "unsupported connection event: {other}"
            ))),
        }
    }

    async fn connection_from_payload(
        &self,
        payload: &serde_json::Value,
    ) -> Result<crate::connections::repository::ConnectionRow, ManagerError> {
        let organization_id = payload
            .get("organization_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ManagerError::Service(anyhow::anyhow!("missing organization_id")))?
            .parse::<OrganizationId>()
            .map_err(|_| ManagerError::Service(anyhow::anyhow!("invalid organization_id")))?;
        let connection_id = payload
            .get("connection_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ManagerError::Service(anyhow::anyhow!("missing connection_id")))?
            .parse::<ConnectionId>()
            .map_err(|_| ManagerError::Service(anyhow::anyhow!("invalid connection_id")))?;
        self.repo
            .get_connection_scoped(organization_id, connection_id)
            .await?
            .ok_or_else(|| ManagerError::not_found("connection"))
    }
}

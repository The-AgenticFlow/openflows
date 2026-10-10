//! GitHub webhook ingress, durable processing, and reconciliation.
//!
//! Ingress verifies `X-Hub-Signature-256` against the exact raw request bytes
//! (constant-time), enforces a body-size limit and event allowlist, and records
//! the delivery id / event / installation / payload digest / processing state
//! atomically with a durable processing outbox event before returning 202.
//! Duplicate deliveries with the same digest are accepted idempotently; the
//! same delivery id with a different digest is rejected and audited. Processing
//! is retryable and leased through the outbox. Access-removing events take
//! effect immediately and conservatively; stale "added"/"unsuspended" events
//! never reactivate access by themselves — reactivation requires fresh
//! authoritative GitHub reconciliation.

use crate::audit::{self, AuditEvent};
use crate::connections::app_jwt::AppSigner;
use crate::connections::github_app::GithubAppApi;
use crate::connections::repository::{ConnectionRepository, ConnectionRow};
use crate::connections::sync::SyncService;
use crate::error::ManagerError;
use crate::outbox;
use crate::runtime::repository::RuntimeRepository;
use hmac::{Hmac, Mac};
use serde_json::Value;
use sha2::Sha256;
use sqlx::PgPool;

type HmacSha256 = Hmac<Sha256>;

/// The `X-Hub-Signature-256` header name.
pub const SIGNATURE_HEADER: &str = "x-hub-signature-256";
/// The delivery id header GitHub sends.
pub const DELIVERY_HEADER: &str = "x-github-delivery";
/// The event name header GitHub sends.
pub const EVENT_HEADER: &str = "x-github-event";
/// The hook id header GitHub sends.
pub const HOOK_ID_HEADER: &str = "x-github-hook-id";

/// Supported webhook event names. Anything else is rejected before parsing.
pub fn is_allowed_event(event: &str) -> bool {
    matches!(
        event,
        "installation" | "installation_repositories" | "github_app_authorization"
    )
}

/// Verify a GitHub webhook signature against the exact raw request bytes.
/// `provided` is the `X-Hub-Signature-256` header value (`sha256=<hex>`).
pub fn verify_signature(raw: &[u8], secret: &[u8], provided: &str) -> bool {
    let expected_hex = provided.strip_prefix("sha256=");
    let Some(expected_hex) = expected_hex else {
        return false;
    };
    let mut mac = match HmacSha256::new_from_slice(secret) {
        Ok(m) => m,
        Err(_) => return false,
    };
    mac.update(raw);
    let computed = mac.finalize().into_bytes();
    // Constant-time comparison of the hex-encoded digests.
    let computed_hex = hex_encode(&computed);
    constant_time_eq(computed_hex.as_bytes(), expected_hex.as_bytes())
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    use subtle::ConstantTimeEq;
    a.ct_eq(b).into()
}

fn hex_encode(bytes: &[u8]) -> String {
    use std::fmt::Write;
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        write!(s, "{b:02x}").unwrap();
    }
    s
}

/// Compute the SHA-256 hex digest of the raw body (payload digest).
pub fn payload_digest(raw: &[u8]) -> String {
    use sha2::Digest;
    let d = Sha256::digest(raw);
    hex_encode(&d)
}

/// The outcome of a webhook receive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IngressOutcome {
    /// Newly persisted and durably queued for processing.
    Accepted { delivery_id: String },
    /// A duplicate delivery id with the same digest (idempotent accept).
    Duplicate,
    /// The same delivery id presented with a different digest (rejected).
    DigestMismatch,
}

/// The webhook ingress and durable processing service.
#[derive(Clone)]
pub struct WebhookService {
    pub pool: PgPool,
    pub repo: ConnectionRepository,
    pub secret: Vec<u8>,
    pub body_limit: usize,
    pub app_id: i64,
    pub sync: SyncService,
    pub runtime_repo: RuntimeRepository,
}

impl WebhookService {
    pub fn new(
        pool: PgPool,
        repo: ConnectionRepository,
        secret: Vec<u8>,
        body_limit: usize,
        app_id: i64,
        sync: SyncService,
        runtime_repo: RuntimeRepository,
    ) -> Self {
        WebhookService {
            pool,
            repo,
            secret,
            body_limit,
            app_id,
            sync,
            runtime_repo,
        }
    }

    /// Revoke access on the connection (incrementing the generation so new
    /// issuance stops) and revoke any outstanding credential leases for that
    /// connection (discarding already-issued tokens).
    async fn revoke_access_and_leases(
        &self,
        conn: &ConnectionRow,
        status: &str,
        reason: &str,
    ) -> Result<(), ManagerError> {
        self.repo
            .revoke_access(self.app_id, conn.installation_id, status, reason)
            .await?;
        self.runtime_repo
            .revoke_leases_for_connection(conn.organization_id, conn.id)
            .await?;
        Ok(())
    }

    /// Durable webhook ingress. Returns 503 (via error) when persistence is
    /// unavailable so GitHub retries. Returns 202 only after the delivery and
    /// durable processing event are committed.
    pub async fn receive(
        &self,
        delivery_id: &str,
        event: &str,
        raw: &[u8],
    ) -> Result<IngressOutcome, ManagerError> {
        if raw.len() > self.body_limit {
            return Err(ManagerError::api(
                "INVALID_INPUT",
                "webhook body exceeds the size limit",
            ));
        }
        if !is_allowed_event(event) {
            return Err(ManagerError::api(
                "INVALID_INPUT",
                "unsupported webhook event",
            ));
        }

        // Parse enough to extract the installation id without trusting it for
        // authorization. Unknown installation ids are recorded for
        // reconciliation but never auto-bound.
        let parsed: Value = serde_json::from_slice(raw)
            .map_err(|_| ManagerError::api("INVALID_INPUT", "invalid webhook JSON body"))?;
        let installation_id = parsed.pointer("/installation/id").and_then(Value::as_i64);
        let action = parsed
            .get("action")
            .and_then(Value::as_str)
            .map(String::from);
        let digest = payload_digest(raw);

        let delivery = uuid::Uuid::parse_str(delivery_id)
            .map_err(|_| ManagerError::api("INVALID_INPUT", "invalid webhook delivery id"))?;

        let mut tx = self.pool.begin().await.map_err(|_| {
            ManagerError::api("SERVICE_UNAVAILABLE", "webhook storage unavailable").retryable(true)
        })?;

        // Serialize duplicate delivery ids before the lookup/insert pair.
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
            .bind(delivery.to_string())
            .execute(&mut *tx)
            .await?;

        // Dedup: same id + same digest is accepted idempotently.
        let existing: Option<(String, String)> = sqlx::query_as(
            "SELECT payload_digest, processing_state FROM webhook_deliveries WHERE delivery_id = $1",
        )
        .bind(delivery)
        .fetch_optional(&mut *tx)
        .await?;

        let outcome = if let Some((stored_digest, _)) = existing {
            if constant_time_eq(stored_digest.as_bytes(), digest.as_bytes()) {
                IngressOutcome::Duplicate
            } else {
                // Same delivery id, different digest: reject and audit.
                audit::insert(
                    &mut *tx,
                    &AuditEvent::new("webhook.digest_mismatch")
                        .resource("webhook_delivery", delivery)
                        .request_id(delivery_id),
                )
                .await?;
                tx.commit().await?;
                return Ok(IngressOutcome::DigestMismatch);
            }
        } else {
            sqlx::query(
                "INSERT INTO webhook_deliveries
                    (delivery_id, event_type, action, installation_id, payload_digest,
                     digest_sha256, payload, processing_state)
                 VALUES ($1, $2, $3, $4, $5, $5, $6, 'received')",
            )
            .bind(delivery)
            .bind(event)
            .bind(action)
            .bind(installation_id)
            .bind(&digest)
            // Event metadata and the digest suffice for processing. Do not
            // retain raw webhook contents in plaintext.
            .bind(Option::<Vec<u8>>::None)
            .execute(&mut *tx)
            .await?;

            // Enqueue the durable processing event in the same transaction.
            outbox::insert_in_tx(
                &mut tx,
                None,
                "webhook.process",
                Some(&serde_json::json!({
                    "delivery_id": delivery_id,
                    "event_type": event,
                    "installation_id": installation_id,
                })),
            )
            .await?;
            IngressOutcome::Accepted {
                delivery_id: delivery_id.to_string(),
            }
        };

        tx.commit().await.map_err(|_| {
            ManagerError::api("SERVICE_UNAVAILABLE", "webhook storage unavailable").retryable(true)
        })?;
        Ok(outcome)
    }

    /// Mark a delivery processed (called after a successful durable apply).
    pub async fn mark_processed(&self, delivery_id: &str) -> Result<(), ManagerError> {
        let delivery = uuid::Uuid::parse_str(delivery_id)
            .map_err(|_| ManagerError::Service(anyhow::anyhow!("invalid delivery id")))?;
        sqlx::query(
            "UPDATE webhook_deliveries
                SET processing_state = 'processed', processed_at = clock_timestamp()
              WHERE delivery_id = $1 AND processing_state <> 'processed'",
        )
        .bind(delivery)
        .execute(&self.pool)
        .await
        .map_err(ManagerError::from)?;
        Ok(())
    }

    /// Process a single webhook delivery durably. Returns `false` when the
    /// delivery is unknown or already processed (idempotent).
    pub async fn process_delivery(&self, delivery_id: &str) -> Result<bool, ManagerError> {
        let delivery = uuid::Uuid::parse_str(delivery_id)
            .map_err(|_| ManagerError::Service(anyhow::anyhow!("invalid delivery id")))?;
        let row: Option<(String, Option<String>, Option<i64>)> = sqlx::query_as(
            "SELECT event_type, action, installation_id FROM webhook_deliveries WHERE delivery_id = $1 AND processing_state <> 'processed'",
        )
        .bind(delivery)
        .fetch_optional(&self.pool)
        .await?;
        let Some((event, action, installation_id)) = row else {
            return Ok(false);
        };
        if self
            .apply_event(&event, action.as_deref(), installation_id)
            .await?
        {
            self.mark_processed(delivery_id).await?;
            return Ok(true);
        }
        Ok(false)
    }

    /// Apply a webhook event to the affected connection(s), conservatively.
    /// Returns `true` if a delivery was actually applied.
    async fn apply_event(
        &self,
        event: &str,
        action: Option<&str>,
        installation_id: Option<i64>,
    ) -> Result<bool, ManagerError> {
        let Some(installation_id) = installation_id else {
            // No installation id: nothing scoped to apply (e.g. a malformed or
            // authorization-level event without an installation).
            return Ok(true);
        };
        let connection = self
            .repo
            .connection_by_installation(self.app_id, installation_id)
            .await?;

        match (event, action) {
            ("installation", Some("deleted")) => {
                if let Some(conn) = &connection {
                    self.revoke_access_and_leases(conn, "deleted", "installation deleted")
                        .await?;
                }
                Ok(true)
            }
            ("installation", Some("suspend")) => {
                if let Some(conn) = &connection {
                    self.revoke_access_and_leases(conn, "suspended", "installation suspended")
                        .await?;
                }
                Ok(true)
            }
            ("installation", Some("unsuspend")) | ("installation", Some("created")) => {
                // Never reactivate from a stale event alone: reconcile
                // authoritative state and only reactivate if GitHub confirms
                // the installation is active and repos are accessible.
                if let Some(conn) = &connection {
                    self.reconcile(conn).await?;
                }
                Ok(true)
            }
            ("installation_repositories", Some("removed")) => {
                if let Some(conn) = &connection {
                    // Conservatively revoke access and mark repos inaccessible.
                    self.revoke_access_and_leases(conn, "suspended", "repositories removed")
                        .await?;
                    self.reconcile(conn).await?;
                }
                Ok(true)
            }
            ("installation_repositories", Some("added")) => {
                // Adding repositories must not activate access by itself;
                // reconcile authoritative state and only reactivate if GitHub
                // confirms the installation and repos.
                if let Some(conn) = &connection {
                    self.reconcile(conn).await?;
                }
                Ok(true)
            }
            ("github_app_authorization", Some("revoked")) => {
                // User authorization revocation does not remove an
                // independently granted installation. Nothing to revoke on the
                // connection; the delivery is recorded.
                Ok(true)
            }
            _ => Ok(true),
        }
    }

    /// Reconcile a connection against authoritative GitHub state, then
    /// reactivate only if the installation is active and reachable. A stale
    /// "added"/"unsuspended" event can never reactivate access by itself.
    pub async fn reconcile(&self, conn: &ConnectionRow) -> Result<(), ManagerError> {
        if conn.status == "disconnected" || conn.status == "deleted" {
            return Ok(());
        }
        let jwt = self.signer().sign().await?;
        let installation = self.api().installation(&jwt, conn.installation_id).await?;
        let Some(installation) = installation else {
            self.revoke_access_and_leases(conn, "deleted", "installation no longer exists")
                .await?;
            return Ok(());
        };

        if installation.app_id != self.app_id || installation.suspended {
            // Fail closed: keep access revoked and record the reason.
            self.revoke_access_and_leases(conn, "suspended", "reconciliation found suspended")
                .await?;
            return Ok(());
        }

        // Synchronize repositories from authoritative state.
        self.sync.sync_connection(conn).await?;

        // Only reactivate when GitHub confirms an active installation. Clear the
        // revoked reason as part of the fresh-authority reactivation.
        self.repo
            .reactivate_connection(self.app_id, conn.installation_id, conn.access_generation)
            .await?;
        Ok(())
    }

    fn signer(&self) -> &dyn AppSigner {
        self.sync.signer.as_ref()
    }
    fn api(&self) -> &dyn GithubAppApi {
        self.sync.api.as_ref()
    }
}

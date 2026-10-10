//! High-level GitHub connection lifecycle service.
//!
//! Orchestrates connection attempts, OAuth/setup callbacks, immutable binding,
//! listing, reconciliation, and disconnect. Only active Openflows organization
//! admins may initiate/complete/reconcile/disconnect; members may view
//! sanitized status. Human GitHub authorization is kept strictly separate from
//! GitHub App authentication.

use crate::audit::{self, AuditEvent};
use crate::auth::crypto::Secret;
use crate::connections::attempts::AttemptService;
use crate::connections::authority::AuthorityService;
use crate::connections::binding::BindingService;
use crate::connections::github_app::GithubAppApi;
use crate::connections::repository::{AttemptRow, ConnectionRepository, ConnectionRow, FlowType};
use crate::connections::webhooks::WebhookService;
use crate::dto::MembershipStatus;
use crate::error::ManagerError;
use crate::id::{ConnectionId, OperationId, OrganizationId, UserId};
use crate::idempotency::{IdempotencyOutcome, IdempotencyRequest, IdempotencyService};
use crate::operations;
use crate::organizations::{Membership, OrganizationState, Policy};
use crate::pagination::PageLimit;
use chrono::{DateTime, Utc};
use serde_json::json;
use sqlx::PgPool;
use std::sync::Arc;

/// The result of completing one leg of the connection flow.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CallbackOutcome {
    /// Both proofs are present and the connection is now bound and active.
    Connected { connection_id: ConnectionId },
    /// Only one proof is present; the admin must complete the other leg.
    PendingAnotherLeg,
}

/// The response to starting a connection flow. Raw authorization/setup URLs are
/// disclosed only on the first (new) attempt; a replayed idempotency key
/// returns the attempt id with no URLs.
#[derive(Debug, Clone)]
pub struct ConnectResponse {
    pub attempt_id: uuid::Uuid,
    pub authorization_url: Option<String>,
    pub setup_url: Option<String>,
}

/// A sanitized connection DTO (never carries tokens, keys, or raw upstream
/// bodies).
#[derive(Debug, Clone, serde::Serialize)]
pub struct ConnectionDto {
    pub id: ConnectionId,
    pub organization_id: OrganizationId,
    pub installation_id: i64,
    pub account_type: String,
    pub account_login: Option<String>,
    pub status: String,
    pub connected_at: Option<DateTime<Utc>>,
    pub last_reconciled_at: Option<DateTime<Utc>>,
}

/// The GitHub connection lifecycle service.
#[derive(Clone)]
pub struct ConnectionService {
    pub pool: PgPool,
    pub repo: ConnectionRepository,
    pub attempts: AttemptService,
    pub authority: AuthorityService,
    pub binding: BindingService,
    pub webhooks: WebhookService,
    pub idempotency: IdempotencyService,
    pub org_repo: crate::organizations::OrganizationRepository,
    pub auth_config: crate::config::AuthConfig,
    pub app_id: i64,
    pub api: Arc<dyn GithubAppApi>,
}

impl ConnectionService {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        pool: PgPool,
        repo: ConnectionRepository,
        attempts: AttemptService,
        authority: AuthorityService,
        binding: BindingService,
        webhooks: WebhookService,
        idempotency: IdempotencyService,
        org_repo: crate::organizations::OrganizationRepository,
        auth_config: crate::config::AuthConfig,
        app_id: i64,
        api: Arc<dyn GithubAppApi>,
    ) -> Self {
        ConnectionService {
            pool,
            repo,
            attempts,
            authority,
            binding,
            webhooks,
            idempotency,
            org_repo,
            auth_config,
            app_id,
            api,
        }
    }

    // ------------------------------------------------------------------
    // Admin helpers
    // ------------------------------------------------------------------

    async fn require_active_admin(
        &self,
        caller: UserId,
        org_id: OrganizationId,
    ) -> Result<(Membership, OrganizationState), ManagerError> {
        let (membership, org) = self.org_repo.membership_and_org(org_id, caller).await?;
        Policy::require_admin(membership.as_ref(), org.as_ref())?;
        Ok((
            membership.expect("require_admin guarantees membership"),
            org.expect("require_admin guarantees org"),
        ))
    }

    async fn require_active_member(
        &self,
        caller: UserId,
        org_id: OrganizationId,
    ) -> Result<(), ManagerError> {
        let (membership, org) = self.org_repo.membership_and_org(org_id, caller).await?;
        Policy::require_member(membership.as_ref(), org.as_ref())?;
        Ok(())
    }

    // ------------------------------------------------------------------
    // Connect
    // ------------------------------------------------------------------

    /// Start a connection flow (admin only, idempotency-key required). The raw
    /// authorization/setup URLs are disclosed once; a replay returns the attempt
    /// id only.
    pub async fn start_connect(
        &self,
        actor: UserId,
        org_id: OrganizationId,
        flow_type: FlowType,
        idempotency_key: &str,
        request_id: &str,
    ) -> Result<ConnectResponse, ManagerError> {
        if flow_type != FlowType::Both {
            return Err(ManagerError::api(
                "UNSUPPORTED_FLOW_TYPE",
                "only the combined GitHub authorization and installation flow is supported",
            ));
        }
        self.require_active_admin(actor, org_id).await?;

        let req = IdempotencyRequest {
            actor_id: actor,
            organization_id: org_id,
            route: "POST /organizations/github/connect".into(),
            key: idempotency_key.into(),
            request_hash: IdempotencyRequest::hash_request(&json!({
                "flow_type": flow_type.as_str(),
            }))
            .map_err(|_| ManagerError::Service(anyhow::anyhow!("failed to hash request")))?,
        };

        if let Some(reference) = self.idempotency.replay(&req).await? {
            let attempt_id = reference
                .parse()
                .map_err(|_| ManagerError::Service(anyhow::anyhow!("invalid attempt reference")))?;
            return Ok(ConnectResponse {
                attempt_id,
                authorization_url: None,
                setup_url: None,
            });
        }

        // Build the flow (fresh states + URLs) before the idempotency insert so
        // the raw URLs can be disclosed exactly once.
        let material = self.attempts.build_flow(actor, flow_type)?;
        let oauth_hash = Secret::hash_raw(&material.oauth_state);
        let setup_hash = Secret::hash_raw(&material.setup_state);
        let verifier = material.encrypted_verifier.clone();
        let attempt_id = material.attempt_id;
        let urls = ConnectResponse {
            attempt_id,
            authorization_url: Some(material.authorization_url),
            setup_url: Some(material.setup_url),
        };

        // Own the request id so the closure can capture it across the
        // higher-ranked idempotency lifetime.
        let request_id = request_id.to_string();
        let result = self
            .idempotency
            .execute(&req, move |conn| {
                Box::pin(async move {
                    // Recheck the admin inside the transaction with a row lock.
                    let member: Option<(String, String)> = sqlx::query_as(
                        "SELECT m.role, m.status FROM memberships m
                          JOIN users u ON u.id = m.user_id
                         WHERE m.organization_id = $1 AND m.user_id = $2 AND u.status = 'active'
                         FOR UPDATE OF m",
                    )
                    .bind(org_id.0)
                    .bind(actor.0)
                    .fetch_optional(&mut *conn)
                    .await?;
                    let Some((role, status)) = member else {
                        return Err(ManagerError::not_found("organization"));
                    };
                    if status != MembershipStatus::Active.as_str() || role != "admin" {
                        return Err(ManagerError::api(
                            "ORG_ADMIN_REQUIRED",
                            "an organization admin is required for this action",
                        ));
                    }
                    ConnectionRepository::create_attempt_in_tx(
                        conn,
                        attempt_id,
                        org_id,
                        actor,
                        flow_type,
                        Some(&oauth_hash),
                        Some(&setup_hash),
                        Some(&verifier),
                        crate::connections::attempts::ATTEMPT_LIFETIME_MINUTES,
                    )
                    .await?;
                    audit::insert(
                        &mut *conn,
                        &AuditEvent::new("github.connection_started")
                            .organization(org_id)
                            .actor(actor)
                            .resource("connection_attempt", attempt_id)
                            .request_id(request_id),
                    )
                    .await?;
                    Ok(attempt_id.to_string())
                })
            })
            .await?;

        match result {
            IdempotencyOutcome::New { .. } => Ok(urls),
            IdempotencyOutcome::Replay { response_reference } => {
                let attempt_id = response_reference.parse().map_err(|_| {
                    ManagerError::Service(anyhow::anyhow!("invalid attempt reference"))
                })?;
                Ok(ConnectResponse {
                    attempt_id,
                    authorization_url: None,
                    setup_url: None,
                })
            }
        }
    }

    // ------------------------------------------------------------------
    // OAuth callback
    // ------------------------------------------------------------------

    /// Complete the user-OAuth leg of a connection flow. The browser session
    /// user must be an active admin of the attempt's organization.
    pub async fn oauth_callback(
        &self,
        session_user: UserId,
        raw_state: &str,
        code: &str,
        request_id: &str,
    ) -> Result<CallbackOutcome, ManagerError> {
        let attempt = self
            .attempts
            .resolve_oauth(raw_state)
            .await?
            .ok_or_else(|| {
                ManagerError::api(
                    "CONNECTION_ATTEMPT_EXPIRED",
                    "the connection attempt is invalid, expired, or already used",
                )
            })?;
        self.require_active_admin(session_user, attempt.organization_id)
            .await?;

        if session_user != attempt.initiated_by {
            return Err(ManagerError::api(
                "AUTH_FAILED",
                "connection must be completed by its initiating user",
            ));
        }

        let verifier = self.attempts.decrypt_verifier(&attempt)?;
        let redirect_uri = format!(
            "{}/api/v1/github/oauth/callback",
            self.auth_config.public_url.trim_end_matches('/')
        );
        let token = self
            .attempts
            .exchange_code(code, &verifier, &redirect_uri)
            .await?;

        // Identify the user and capture the transient token (encrypted) so the
        // authority check can run now or when the setup leg arrives second.
        let user = self.api.fetch_user(&token.access_token).await?;

        let cipher = self
            .auth_config
            .envelope_cipher("oauth_pkce")
            .map_err(|_| ManagerError::Config("envelope cipher unavailable".into()))?;
        let encrypted_token = cipher.seal(token.access_token.as_bytes())?;
        drop(token);

        let mut tx = self.pool.begin().await?;
        let consumed = ConnectionRepository::consume_oauth_proof_in_tx(
            &mut tx,
            attempt.id,
            user.id,
            None,
            Some(&encrypted_token),
        )
        .await?;
        if !consumed {
            return Err(ManagerError::api(
                "CONNECTION_CALLBACK_REPLAYED",
                "this callback was already processed",
            ));
        }
        audit::insert(
            &mut *tx,
            &AuditEvent::new("github.connection_oauth")
                .organization(attempt.organization_id)
                .actor(session_user)
                .resource("connection_attempt", attempt.id)
                .request_id(request_id),
        )
        .await?;
        tx.commit().await?;

        let updated = self.repo.attempt_by_id(attempt.id).await?.ok_or_else(|| {
            ManagerError::api(
                "CONNECTION_ATTEMPT_EXPIRED",
                "the connection attempt is no longer valid",
            )
        })?;
        self.maybe_bind(updated, request_id).await
    }

    // ------------------------------------------------------------------
    // Setup callback
    // ------------------------------------------------------------------

    /// Complete the installation-setup leg of a connection flow. The browser
    /// session user must be an active admin of the attempt's organization.
    /// `setup_action` and `installation_id` are never trusted as proof of
    /// authority — full owner/authority verification runs at binding time.
    pub async fn setup_callback(
        &self,
        session_user: UserId,
        raw_state: &str,
        installation_id: i64,
        request_id: &str,
    ) -> Result<CallbackOutcome, ManagerError> {
        let attempt = self
            .attempts
            .resolve_setup(raw_state)
            .await?
            .ok_or_else(|| {
                ManagerError::api(
                    "CONNECTION_ATTEMPT_EXPIRED",
                    "the connection attempt is invalid, expired, or already used",
                )
            })?;
        self.require_active_admin(session_user, attempt.organization_id)
            .await?;

        if session_user != attempt.initiated_by {
            return Err(ManagerError::api(
                "AUTH_FAILED",
                "connection must be completed by its initiating user",
            ));
        }

        let mut tx = self.pool.begin().await?;
        let consumed =
            ConnectionRepository::consume_setup_proof_in_tx(&mut tx, attempt.id, installation_id)
                .await?;
        if !consumed {
            return Err(ManagerError::api(
                "CONNECTION_CALLBACK_REPLAYED",
                "this callback was already processed",
            ));
        }
        audit::insert(
            &mut *tx,
            &AuditEvent::new("github.connection_setup")
                .organization(attempt.organization_id)
                .actor(session_user)
                .resource("connection_attempt", attempt.id)
                .request_id(request_id),
        )
        .await?;
        tx.commit().await?;

        let updated = self.repo.attempt_by_id(attempt.id).await?.ok_or_else(|| {
            ManagerError::api(
                "CONNECTION_ATTEMPT_EXPIRED",
                "the connection attempt is no longer valid",
            )
        })?;
        self.maybe_bind(updated, request_id).await
    }

    /// If both proofs are present, run the authoritative binding. Otherwise
    /// return `PendingAnotherLeg`.
    async fn maybe_bind(
        &self,
        attempt: AttemptRow,
        request_id: &str,
    ) -> Result<CallbackOutcome, ManagerError> {
        if !attempt.proofs_complete() {
            return Ok(CallbackOutcome::PendingAnotherLeg);
        }
        // Retrieve the encrypted transient user token stored at the OAuth leg.
        let (token_ref, _) = self
            .repo
            .attempt_credentials(attempt.id)
            .await?
            .ok_or_else(|| {
                ManagerError::api(
                    "CONNECTION_ATTEMPT_EXPIRED",
                    "the connection attempt is no longer valid",
                )
            })?;
        let token_ref = token_ref.ok_or_else(|| {
            ManagerError::api(
                "AUTH_FAILED",
                "user authorization is required to complete the connection",
            )
        })?;
        let cipher = self
            .auth_config
            .envelope_cipher("oauth_pkce")
            .map_err(|_| ManagerError::Config("envelope cipher unavailable".into()))?;
        let plain = cipher.open(&token_ref)?;
        let user_token = String::from_utf8(plain)
            .map_err(|_| ManagerError::api("AUTH_FAILED", "invalid user authorization"))?;

        let conn = self.binding.bind(&attempt, &user_token, request_id).await?;
        Ok(CallbackOutcome::Connected {
            connection_id: conn.id,
        })
    }

    // ------------------------------------------------------------------
    // List / reconcile / disconnect
    // ------------------------------------------------------------------

    /// List connections for an organization (member view; sanitized status
    /// only).
    pub async fn list_connections(
        &self,
        caller: UserId,
        org_id: OrganizationId,
        _limit: PageLimit,
    ) -> Result<Vec<ConnectionDto>, ManagerError> {
        self.require_active_member(caller, org_id).await?;
        let rows = self.repo.list_connections(org_id).await?;
        Ok(rows.into_iter().map(ConnectionDto::from).collect())
    }

    /// Enqueue a reconciliation operation (admin only).
    pub async fn reconcile_connection(
        &self,
        caller: UserId,
        org_id: OrganizationId,
        connection_id: ConnectionId,
        idempotency_key: &str,
        request_id: &str,
    ) -> Result<OperationId, ManagerError> {
        self.require_active_admin(caller, org_id).await?;
        let _conn = self
            .repo
            .get_connection_scoped(org_id, connection_id)
            .await?
            .ok_or_else(|| ManagerError::not_found("connection"))?;

        let req = IdempotencyRequest {
            actor_id: caller,
            organization_id: org_id,
            route: "POST /organizations/github/connections/reconcile".into(),
            key: idempotency_key.into(),
            request_hash: IdempotencyRequest::hash_request(&json!({
                "connection_id": connection_id.to_string(),
            }))
            .map_err(|_| ManagerError::Service(anyhow::anyhow!("failed to hash request")))?,
        };
        let request_id = request_id.to_string();
        let idempotency_key = idempotency_key.to_string();
        let result = self
            .idempotency
            .execute(&req, move |conn_tx| {
                Box::pin(async move {
                    let member: Option<(String, String)> = sqlx::query_as(
                        "SELECT m.role, m.status FROM memberships m
                           JOIN users u ON u.id = m.user_id
                          WHERE m.organization_id = $1 AND m.user_id = $2
                            AND u.status = 'active' FOR UPDATE OF m",
                    )
                    .bind(org_id.0).bind(caller.0).fetch_optional(&mut *conn_tx).await?;
                    if !matches!(member, Some((ref role, ref status)) if role == "admin" && status == MembershipStatus::Active.as_str()) {
                        return Err(ManagerError::api("ORG_ADMIN_REQUIRED", "an organization admin is required for this action"));
                    }
                    let op = operations::create_in_tx(
                        conn_tx,
                        &operations::NewOperation {
                            organization_id: org_id,
                            resource_type: "github_connection".into(),
                            resource_id: Some(connection_id.0),
                            kind: "github.reconcile".into(),
                            idempotency_ref: Some(idempotency_key.clone()),
                        },
                    )
                    .await?;
                    audit::insert(
                        &mut *conn_tx,
                        &AuditEvent::new("github.connection_reconcile")
                            .organization(org_id)
                            .actor(caller)
                            .resource("github_connection", connection_id.0)
                            .request_id(request_id),
                    )
                    .await?;
                    crate::outbox::insert_in_tx(
                        conn_tx,
                        Some(org_id),
                        "github.reconcile",
                        Some(&json!({
                            "operation_id": op.to_string(),
                            "connection_id": connection_id.to_string(),
                            "organization_id": org_id.to_string(),
                        })),
                    )
                    .await?;
                    Ok(op.to_string())
                })
            })
            .await?;
        let reference = match result {
            IdempotencyOutcome::New { response_reference }
            | IdempotencyOutcome::Replay { response_reference } => response_reference,
        };
        reference
            .parse()
            .map_err(|_| ManagerError::Service(anyhow::anyhow!("invalid operation reference")))
    }

    /// Disconnect a connection (admin only): immediately disable local access
    /// (status + access-generation increment) so new runtime credential
    /// issuance stops before asynchronous cleanup, then enqueue cleanup.
    pub async fn disconnect(
        &self,
        caller: UserId,
        org_id: OrganizationId,
        connection_id: ConnectionId,
        idempotency_key: &str,
        request_id: &str,
    ) -> Result<OperationId, ManagerError> {
        self.require_active_admin(caller, org_id).await?;
        let _conn = self
            .repo
            .get_connection_scoped(org_id, connection_id)
            .await?
            .ok_or_else(|| ManagerError::not_found("connection"))?;

        let req = IdempotencyRequest {
            actor_id: caller,
            organization_id: org_id,
            route: "DELETE /organizations/github/connections/{id}".into(),
            key: idempotency_key.into(),
            request_hash: IdempotencyRequest::hash_request(&json!({
                "connection_id": connection_id.to_string(),
            }))
            .map_err(|_| ManagerError::Service(anyhow::anyhow!("failed to hash request")))?,
        };
        let request_id = request_id.to_string();
        let idempotency_key = idempotency_key.to_string();
        let result = self
            .idempotency
            .execute(&req, move |conn_tx| {
                Box::pin(async move {
                    let member: Option<(String, String)> = sqlx::query_as(
                        "SELECT m.role, m.status FROM memberships m
                           JOIN users u ON u.id = m.user_id
                          WHERE m.organization_id = $1 AND m.user_id = $2
                            AND u.status = 'active' FOR UPDATE OF m",
                    )
                    .bind(org_id.0).bind(caller.0).fetch_optional(&mut *conn_tx).await?;
                    if !matches!(member, Some((ref role, ref status)) if role == "admin" && status == MembershipStatus::Active.as_str()) {
                        return Err(ManagerError::api("ORG_ADMIN_REQUIRED", "an organization admin is required for this action"));
                    }
                    let op = operations::create_in_tx(
                        conn_tx,
                        &operations::NewOperation {
                            organization_id: org_id,
                            resource_type: "github_connection".into(),
                            resource_id: Some(connection_id.0),
                            kind: "github.disconnect_cleanup".into(),
                            idempotency_ref: Some(idempotency_key.clone()),
                        },
                    )
                    .await?;
                    // Commit revocation with idempotency, operation and outbox.
                    // A replay must never revoke a subsequently reconnected installation.
                    sqlx::query(
                        "UPDATE github_connections SET status = 'disconnected',
                        access_generation = access_generation + 1,
                        access_revoked_reason = 'disconnected', updated_at = now()
                        WHERE id = $1 AND organization_id = $2
                          AND status IN ('active','suspended','pending')",
                    )
                    .bind(connection_id.0)
                    .bind(org_id.0)
                    .execute(&mut *conn_tx)
                    .await?;
                    audit::insert(
                        &mut *conn_tx,
                        &AuditEvent::new("github.connection_disconnect")
                            .organization(org_id)
                            .actor(caller)
                            .resource("github_connection", connection_id.0)
                            .request_id(request_id),
                    )
                    .await?;
                    crate::outbox::insert_in_tx(
                        conn_tx,
                        Some(org_id),
                        "github.disconnect_cleanup",
                        Some(&json!({
                            "operation_id": op.to_string(),
                            "connection_id": connection_id.to_string(),
                            "organization_id": org_id.to_string(),
                        })),
                    )
                    .await?;
                    Ok(op.to_string())
                })
            })
            .await?;
        let reference = match result {
            IdempotencyOutcome::New { response_reference }
            | IdempotencyOutcome::Replay { response_reference } => response_reference,
        };
        reference
            .parse()
            .map_err(|_| ManagerError::Service(anyhow::anyhow!("invalid operation reference")))
    }
}

impl From<ConnectionRow> for ConnectionDto {
    fn from(c: ConnectionRow) -> Self {
        ConnectionDto {
            id: c.id,
            organization_id: c.organization_id,
            installation_id: c.installation_id,
            account_type: c.account_type,
            account_login: c.account_login,
            status: c.status,
            connected_at: c.verified_at,
            last_reconciled_at: c.last_reconciled_at,
        }
    }
}

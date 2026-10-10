//! Audit-event persistence.
//!
//! Audit records include organization, actor type/ID, action, resource type/ID,
//! result, request ID, and timestamp. They are append-only to application
//! callers and must be inserted in the same transaction as the state change
//! they describe so a failed mutation leaves no orphaned audit row.

use crate::error::ManagerError;
use crate::id::{AuditEventId, OrganizationId, UserId};
use serde::Serialize;
use sqlx::{PgPool, Postgres, Transaction};

/// The outcome of an audited action.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum AuditResult {
    Success,
    Denied,
    Failure,
}

impl AuditResult {
    fn as_str(&self) -> &'static str {
        match self {
            AuditResult::Success => "success",
            AuditResult::Denied => "denied",
            AuditResult::Failure => "failure",
        }
    }
}

/// An audit event to be recorded.
#[derive(Debug, Clone)]
pub struct AuditEvent {
    pub organization_id: Option<OrganizationId>,
    pub actor_type: String,
    pub actor_id: Option<UserId>,
    pub action: String,
    pub resource_type: Option<String>,
    pub resource_id: Option<uuid::Uuid>,
    pub result: AuditResult,
    pub request_id: Option<String>,
}

impl AuditEvent {
    pub fn new(action: impl Into<String>) -> Self {
        AuditEvent {
            organization_id: None,
            actor_type: "user".to_string(),
            actor_id: None,
            action: action.into(),
            resource_type: None,
            resource_id: None,
            result: AuditResult::Success,
            request_id: None,
        }
    }

    pub fn organization(mut self, id: OrganizationId) -> Self {
        self.organization_id = Some(id);
        self
    }

    pub fn actor(mut self, id: UserId) -> Self {
        self.actor_id = Some(id);
        self
    }

    /// Set the actor type (defaults to "user"). Runtime principals record
    /// "runtime"; platform/operator actors may use other stable types.
    pub fn actor_type(mut self, actor_type: impl Into<String>) -> Self {
        self.actor_type = actor_type.into();
        self
    }

    pub fn resource(mut self, resource_type: impl Into<String>, id: impl Into<uuid::Uuid>) -> Self {
        self.resource_type = Some(resource_type.into());
        self.resource_id = Some(id.into());
        self
    }

    pub fn result(mut self, result: AuditResult) -> Self {
        self.result = result;
        self
    }

    pub fn request_id(mut self, request_id: impl Into<String>) -> Self {
        self.request_id = Some(request_id.into());
        self
    }
}

/// Insert an audit event, optionally within a transaction so it commits
/// atomically with the state change it records.
pub async fn insert<'a, E>(executor: E, event: &AuditEvent) -> Result<(), ManagerError>
where
    E: sqlx::Executor<'a, Database = Postgres>,
{
    sqlx::query(
        "INSERT INTO audit_events
            (id, organization_id, actor_type, actor_id, action, resource_type, resource_id, result, request_id, occurred_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, now())",
    )
    .bind(AuditEventId::new().0)
    .bind(event.organization_id.map(|o| o.0))
    .bind(&event.actor_type)
    .bind(event.actor_id.map(|a| a.0))
    .bind(&event.action)
    .bind(&event.resource_type)
    .bind(event.resource_id)
    .bind(event.result.as_str())
    .bind(&event.request_id)
    .execute(executor)
    .await
    .map_err(ManagerError::from)?;
    Ok(())
}

/// Persist an audit event inside a transaction (must be committed by caller).
pub async fn insert_in_tx(
    tx: &mut Transaction<'_, Postgres>,
    event: &AuditEvent,
) -> Result<(), ManagerError> {
    insert(&mut **tx, event).await
}

/// Insert an audit event using the pool directly (no transaction).
pub async fn insert_with_pool(pool: &PgPool, event: &AuditEvent) -> Result<(), ManagerError> {
    insert(pool, event).await
}

//! Agent activity for one tenant: `GET /organizations/{org}/tenants/{tenant}/agent-activity`.
//!
//! Answers "what are this tenant's agents doing, and are they stuck?" for the
//! Console. Each row is one managed worker chat (one agent working one ticket):
//! its role, ticket, status and, when it failed, a stable error code. The
//! Manager already records which Coder chat belongs to which ticket
//! (`runtime_chats`); the live status comes from Coder using the operator
//! credential, which never leaves the Manager.
//!
//! Member view, like the tenant status endpoint. Nothing from the agent's
//! conversation is returned, and a failure is reduced to a code: Coder's error
//! detail comes from the AI provider and can contain account identifiers or
//! keys, so it is never forwarded. If Coder cannot be reached the rows still
//! come back, with status `unknown`.

use crate::error::ManagerError;
use crate::routes::organizations::{authenticated_user, parse_org};
use crate::server::AppState;
use axum::extract::{Path, State};
use axum::http::HeaderMap;
use axum::Json;
use chrono::{DateTime, Utc};
use serde::Serialize;
use std::str::FromStr;

const MAX_CHATS: i64 = 20;

/// What Coder said about one chat.
enum Live {
    Found(Box<coder_client::types::Chat>),
    /// Coder answered and the chat no longer exists.
    Gone,
    /// Coder could not be asked, or failed.
    Unknown,
}

#[derive(Serialize)]
pub struct AgentChat {
    pub ticket_id: String,
    pub role: String,
    /// pending, running, waiting, requires_action, error, gone (the chat no
    /// longer exists in Coder) or unknown (Coder could not be asked).
    pub status: String,
    /// Stable code for a failed chat; null otherwise.
    pub error_code: Option<&'static str>,
    /// Whether Coder says retrying the same chat could succeed.
    pub retryable: Option<bool>,
    pub started_at: DateTime<Utc>,
    pub updated_at: Option<DateTime<Utc>>,
}

#[derive(Serialize)]
pub struct AgentActivity {
    pub chats: Vec<AgentChat>,
}

/// Map Coder's provider-independent failure kind to a stable OpenFlows code.
fn error_code(kind: Option<&str>) -> &'static str {
    match kind {
        Some("usage_limit") => "AI_USAGE_LIMIT",
        Some("rate_limit") => "AI_RATE_LIMITED",
        Some("auth") | Some("authentication") | Some("unauthorized") => "AI_AUTH_FAILED",
        _ => "AGENT_ERROR",
    }
}

pub async fn agent_activity(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((org, tenant)): Path<(String, String)>,
) -> Result<Json<AgentActivity>, ManagerError> {
    let services = state
        .services()
        .ok_or_else(|| ManagerError::api("SERVICE_UNAVAILABLE", "service unavailable"))?;
    let principal = authenticated_user(&headers, &services.session_manager).await?;
    let org_id = parse_org(&org)?;
    let tenant_id =
        crate::id::TenantId::from_str(&tenant).map_err(|_| ManagerError::not_found("tenant"))?;
    // Membership and tenant scope: 404 for anyone outside the organization.
    services
        .tenants_service
        .get_tenant(principal.user_id, org_id, tenant_id)
        .await?;

    let rows = load_chat_rows(services.db.pool(), org_id, tenant_id).await?;
    if rows.is_empty() {
        return Ok(Json(AgentActivity { chats: Vec::new() }));
    }

    // Ask Coder, as the operator, about each chat. Any failure here degrades
    // to "unknown" rows rather than failing the whole page.
    let client = operator_client(services).await.ok();
    let lookups = rows.iter().map(|(_, _, chat_id, _)| {
        let client = client.clone();
        let chat_id = chat_id.clone();
        async move {
            let Some(client) = client else {
                return Live::Unknown;
            };
            match client.get_chat_opt(&chat_id).await {
                Ok(Some(chat)) => Live::Found(Box::new(chat)),
                Ok(None) => Live::Gone,
                Err(_) => Live::Unknown,
            }
        }
    });
    let live = futures::future::join_all(lookups).await;

    let chats = rows
        .into_iter()
        .zip(live)
        .map(|((ticket_id, role, _, started_at), live)| match live {
            Live::Gone | Live::Unknown => AgentChat {
                ticket_id,
                role,
                status: if matches!(live, Live::Gone) {
                    "gone"
                } else {
                    "unknown"
                }
                .into(),
                error_code: None,
                retryable: None,
                started_at,
                updated_at: None,
            },
            Live::Found(chat) => {
                let status = chat.status().as_str().to_string();
                let failed = status == "error";
                AgentChat {
                    ticket_id,
                    role,
                    error_code: failed.then(|| error_code(chat.error_kind())),
                    retryable: if failed { chat.error_retryable() } else { None },
                    updated_at: DateTime::parse_from_rfc3339(&chat.updated_at_raw)
                        .ok()
                        .map(|t| t.with_timezone(&Utc)),
                    status,
                    started_at,
                }
            }
        })
        .collect();
    Ok(Json(AgentActivity { chats }))
}

/// A tenant's managed worker chats, chats of live workspaces first and then
/// newest first, so a long-running chat is never pushed out by retired ones:
/// (ticket id, workspace role, Coder chat id, created at).
pub async fn load_chat_rows(
    pool: &sqlx::PgPool,
    org_id: crate::id::OrganizationId,
    tenant_id: crate::id::TenantId,
) -> Result<Vec<(String, String, String, DateTime<Utc>)>, ManagerError> {
    Ok(sqlx::query_as(
        "SELECT rc.ticket_id, w.role, rc.coder_chat_id, rc.created_at
           FROM runtime_chats rc
           JOIN workspaces w ON w.id = rc.workspace_id
          WHERE w.organization_id = $1 AND w.tenant_id = $2
          ORDER BY (w.desired_state = 'deleted') ASC, rc.created_at DESC
          LIMIT $3",
    )
    .bind(org_id.0)
    .bind(tenant_id.0)
    .bind(MAX_CHATS)
    .fetch_all(pool)
    .await?)
}

async fn operator_client(
    services: &crate::server::ManagerServices,
) -> Result<coder_client::CoderClient, ManagerError> {
    let token_ref = std::env::var("OPENFLOWS_CODER_OPERATOR_TOKEN_REF")
        .map_err(|_| ManagerError::Config("Coder operator token reference is required".into()))?;
    let bytes = services
        .secret_provider
        .get(&token_ref)
        .await
        .map_err(|_| {
            ManagerError::api(
                "CODER_UNAVAILABLE",
                "Coder operator credential is unavailable",
            )
        })?;
    let token = String::from_utf8(bytes)
        .map_err(|_| ManagerError::Config("Coder operator token is invalid".into()))?;
    let coder_url = std::env::var("CODER_URL")
        .map_err(|_| ManagerError::Config("CODER_URL is required".into()))?;
    Ok(coder_client::CoderClient::new(&coder_url, token.trim()))
}

#[cfg(test)]
mod tests {
    use super::error_code;

    #[test]
    fn coder_failure_detail_is_never_part_of_the_chat_view() {
        // Shape of a real failed chat from Coder: the detail carries a provider
        // URL with a key id and must not be reachable through what we return.
        let chat: coder_client::types::Chat = serde_json::from_value(serde_json::json!({
            "id": "c1", "status": "error", "title": "t",
            "last_error": {"message": "limit", "detail": "visit https://openrouter.ai/keys/SECRET",
                           "kind": "usage_limit", "retryable": false, "status_code": 402}
        }))
        .unwrap();
        assert_eq!(chat.status().as_str(), "error");
        assert_eq!(chat.error_kind(), Some("usage_limit"));
        assert_eq!(chat.error_retryable(), Some(false));
        let body = serde_json::to_string(&super::AgentChat {
            ticket_id: "T-1".into(),
            role: "forge".into(),
            status: chat.status().as_str().into(),
            error_code: Some(error_code(chat.error_kind())),
            retryable: chat.error_retryable(),
            started_at: chrono::Utc::now(),
            updated_at: None,
        })
        .unwrap();
        assert!(!body.contains("SECRET") && !body.contains("openrouter"));
        assert!(body.contains("AI_USAGE_LIMIT"));
    }

    #[test]
    fn failure_kinds_map_to_stable_codes() {
        assert_eq!(error_code(Some("usage_limit")), "AI_USAGE_LIMIT");
        assert_eq!(error_code(Some("rate_limit")), "AI_RATE_LIMITED");
        assert_eq!(error_code(Some("auth")), "AI_AUTH_FAILED");
        assert_eq!(error_code(Some("something_new")), "AGENT_ERROR");
        assert_eq!(error_code(None), "AGENT_ERROR");
    }
}

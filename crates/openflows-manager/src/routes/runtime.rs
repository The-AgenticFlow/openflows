//! Runtime credential routes.
//!
//! These endpoints are authenticated by a workspace runtime bearer credential
//! — a principal distinct from human browser/CLI sessions and human API
//! credentials. They deliberately do not consult the human session manager, so
//! a browser cookie or human access token can never authorize a runtime
//! endpoint. Newly issued secrets are returned only to the authenticated
//! runtime, with `Cache-Control: no-store`.

use crate::error::ManagerError;
use crate::routes::auth::bearer_token;
use crate::runtime::permissions::RuntimePurpose;
use crate::server::AppState;
use axum::extract::{Path, State};
use axum::http::{header, HeaderMap};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::{Deserialize, Serialize};

#[derive(Deserialize)]
pub struct RegisterRequest {
    pub bootstrap: String,
}

#[derive(Serialize)]
pub struct RegisterResponse {
    pub workspace_id: String,
    pub access_token: String,
    pub access_expires_at: chrono::DateTime<chrono::Utc>,
    pub refresh_token: String,
    pub refresh_expires_at: chrono::DateTime<chrono::Utc>,
    pub generation: i64,
}

/// Exchange the single-use secret mounted into a workspace for its first
/// short-lived access/refresh session. Bootstrap replay is rejected.
pub async fn register(
    State(state): State<AppState>,
    Json(body): Json<RegisterRequest>,
) -> Result<Response, ManagerError> {
    let services = state
        .services()
        .ok_or_else(|| ManagerError::api("SERVICE_UNAVAILABLE", "service unavailable"))?;
    let issued = services
        .runtime_credentials
        .register(&body.bootstrap)
        .await?;
    let mut response = Json(RegisterResponse {
        workspace_id: issued.workspace_id.to_string(),
        access_token: issued.access_secret,
        access_expires_at: issued.access_expires_at,
        refresh_token: issued.refresh_secret,
        refresh_expires_at: issued.refresh_expires_at,
        generation: issued.generation,
    })
    .into_response();
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, "no-store".parse().unwrap());
    Ok(response)
}

/// The body of `POST /runtime/github-credentials`. Only the coarse `purpose` is
/// caller-supplied; all authorization scope (organization, installation,
/// repository, permission profile) is derived server-side.
#[derive(Deserialize)]
pub struct BrokerRequest {
    pub purpose: String,
}

/// The response returned to the authorized runtime. The token must never be
/// logged or persisted by the client beyond its short-lived use.
#[derive(Serialize)]
pub struct BrokerResponse {
    pub token: String,
    pub expires_at: chrono::DateTime<chrono::Utc>,
    pub repository_id: i64,
    pub username: String,
    pub permissions: std::collections::BTreeMap<String, String>,
}

/// POST /api/v1/runtime/github-credentials
///
/// Authenticated exclusively by a workspace runtime credential. Returns a
/// short-lived GitHub installation token scoped to the workspace's authorized
/// repository and permission profile. Human credentials are rejected.
pub async fn github_credentials(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<BrokerRequest>,
) -> Result<Response, ManagerError> {
    let services = state
        .services()
        .ok_or_else(|| ManagerError::api("SERVICE_UNAVAILABLE", "service unavailable"))?;

    let bearer = bearer_token(&headers)
        .ok_or_else(|| ManagerError::api("UNAUTHORIZED", "runtime authentication required"))?;

    // Authenticate as a workspace runtime principal only. This never touches
    // the human session manager, so browser cookies / human tokens are rejected.
    let scope = services.runtime_credentials.authenticate(&bearer).await?;
    if !crate::rate_limit::RateLimiter::new(services.db.pool().clone())
        .allow(
            &scope.workspace_id.to_string(),
            crate::rate_limit::LimitScope::RuntimeBroker,
            30,
        )
        .await?
    {
        return Err(ManagerError::api(
            "RATE_LIMITED",
            "runtime credential request limit exceeded",
        ));
    }

    let purpose = RuntimePurpose::parse(&body.purpose)?;
    let request_id = crate::routes::organizations::request_id(&headers);

    let credential = services
        .broker
        .exchange(&scope, purpose, &request_id)
        .await?;

    let body = BrokerResponse {
        token: credential.token,
        expires_at: credential.expires_at,
        repository_id: credential.repository_id,
        username: credential.username,
        permissions: credential.permissions,
    };
    let mut response = Json(body).into_response();
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        "no-store"
            .parse()
            .map_err(|_| ManagerError::Service(anyhow::anyhow!("invalid cache-control header")))?,
    );
    Ok(response)
}

/// Authenticate a request as a workspace runtime principal and resolve its full
/// scope. Rejects human credentials (browser cookies / CLI tokens).
async fn authenticate_runtime(
    services: &crate::server::ManagerServices,
    headers: &HeaderMap,
) -> Result<crate::runtime::RuntimeScope, ManagerError> {
    let bearer = bearer_token(headers)
        .ok_or_else(|| ManagerError::api("UNAUTHORIZED", "runtime authentication required"))?;
    services.runtime_credentials.authenticate(&bearer).await
}

/// `POST /runtime/workspaces` — a tenant's Nexus controller requests a worker
/// workspace. All privileged parameters are resolved server-side from the
/// authenticated Nexus runtime scope; a runtime caller cannot select another
/// organization, tenant, repository, or an unauthorized role. Returns 202.
#[derive(Deserialize)]
pub struct WorkerRequest {
    pub role: String,
    #[serde(default)]
    pub slot: Option<i32>,
    #[serde(default)]
    pub ticket_id: Option<String>,
    #[serde(default)]
    pub branch: Option<String>,
    #[serde(default)]
    pub a2a_pair_token: Option<String>,
}

pub async fn request_worker(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<WorkerRequest>,
) -> Result<Response, ManagerError> {
    let services = state
        .services()
        .ok_or_else(|| ManagerError::api("SERVICE_UNAVAILABLE", "service unavailable"))?;
    let scope = authenticate_runtime(services, &headers).await?;
    let idempotency_key = crate::routes::organizations::idempotency_key(&headers)?;
    if body.branch.is_some() && !safe_branch(body.branch.as_deref().unwrap_or("")) {
        return Err(ManagerError::InvalidInput(
            "branch contains unsafe characters".into(),
        ));
    }
    let result = services
        .runtime_workspaces
        .request_worker(
            &scope,
            &body.role,
            body.slot,
            body.ticket_id.as_deref(),
            body.branch.as_deref(),
            body.a2a_pair_token.as_deref(),
            &idempotency_key,
        )
        .await?;
    let mut response = (
        axum::http::StatusCode::ACCEPTED,
        Json(serde_json::json!({
            "workspace_id": result.workspace_id,
            "operation_id": result.operation_id,
            "status": result.status,
        })),
    )
        .into_response();
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        "no-store"
            .parse()
            .map_err(|_| ManagerError::Service(anyhow::anyhow!("invalid cache-control header")))?,
    );
    Ok(response)
}

#[derive(Deserialize)]
pub struct CreateRuntimeChatRequest {
    pub workspace_id: String,
    pub ticket_id: String,
    pub prompt: String,
}

/// Nexus asks Manager to create a Chat owned by the tenant creator. Manager
/// uses its operator credential; Coder verifies impersonation authority and
/// executes the Chat with the creator's permissions. No Coder token reaches
/// the workspace.
pub async fn create_chat(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<CreateRuntimeChatRequest>,
) -> Result<Response, ManagerError> {
    let services = state
        .services()
        .ok_or_else(|| ManagerError::api("SERVICE_UNAVAILABLE", "service unavailable"))?;
    let scope = authenticate_runtime(services, &headers).await?;
    if scope.workspace_role != "nexus" {
        return Err(ManagerError::api(
            "FORBIDDEN",
            "only Nexus can start worker chats",
        ));
    }
    let workspace_id = crate::runtime::workspaces::parse_workspace_id(&body.workspace_id)?;
    if body.ticket_id.is_empty()
        || body.ticket_id.len() > 128
        || body.prompt.trim().is_empty()
        || body.prompt.len() > 32_768
    {
        return Err(ManagerError::InvalidInput("invalid chat assignment".into()));
    }
    let mut tx = services.db.pool().begin().await?;
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1::text, 0))")
        .bind(workspace_id.0)
        .execute(&mut *tx)
        .await?;
    let row: Option<(
        Option<String>,
        Option<String>,
        Option<uuid::Uuid>,
        Option<String>,
        Option<String>,
        String,
    )> = sqlx::query_as(
        "SELECT w.coder_workspace_id, w.ticket_id, t.owner_user_id,
                o.coder_organization_id, l.coder_user_id, w.role
           FROM workspaces w
           JOIN tenants t ON t.id=w.tenant_id AND t.organization_id=w.organization_id
           JOIN organizations o ON o.id=w.organization_id
           LEFT JOIN coder_user_links l ON l.user_id=t.owner_user_id
          WHERE w.id=$1 AND w.tenant_id=$2 AND w.organization_id=$3
            AND w.desired_state='running' AND w.observed_state='running'",
    )
    .bind(workspace_id.0)
    .bind(scope.tenant_id.0)
    .bind(scope.organization_id.0)
    .fetch_optional(&mut *tx)
    .await?;
    let (coder_workspace, assigned_ticket, user_id, coder_org, coder_owner, role) =
        row.ok_or_else(|| ManagerError::not_found("running worker workspace"))?;
    if role == "nexus" || assigned_ticket.as_deref() != Some(&body.ticket_id) {
        return Err(ManagerError::api(
            "FORBIDDEN",
            "workspace assignment does not match",
        ));
    }
    let coder_workspace =
        coder_workspace.ok_or_else(|| ManagerError::not_found("Coder workspace"))?;
    let coder_org = coder_org.ok_or_else(|| ManagerError::not_found("Coder organization"))?;
    let coder_owner = coder_owner.ok_or_else(|| {
        ManagerError::api(
            "CODER_REAUTH_REQUIRED",
            "tenant creator must link a Coder account",
        )
    })?;
    user_id.ok_or_else(|| {
        ManagerError::api(
            "CODER_REAUTH_REQUIRED",
            "tenant creator must link a Coder account",
        )
    })?;
    let token_ref = std::env::var("OPENFLOWS_CODER_OPERATOR_TOKEN_REF")
        .map_err(|_| ManagerError::Config("Coder operator token reference is required".into()))?;
    let token_bytes = services
        .secret_provider
        .get(&token_ref)
        .await
        .map_err(|_| {
            ManagerError::api(
                "CODER_CHAT_UNAVAILABLE",
                "Coder operator credential is unavailable",
            )
            .retryable(true)
        })?;
    let token = zeroize::Zeroizing::new(
        String::from_utf8(token_bytes)
            .map_err(|_| ManagerError::Config("Coder operator token is invalid".into()))?,
    );
    if token.trim().is_empty() {
        return Err(ManagerError::Config("Coder operator token is empty".into()));
    }
    let coder_url = std::env::var("CODER_URL")
        .map_err(|_| ManagerError::Config("CODER_URL is required".into()))?;
    let client = coder_client::CoderClient::new(&coder_url, token.trim());
    let labels = coder_client::types::build_chat_labels(
        &body.ticket_id,
        &role,
        "openflows",
        &scope.tenant_id.to_string(),
    );
    let existing: Option<(String, String)> = sqlx::query_as(
        "SELECT coder_chat_id, coder_workspace_id FROM runtime_chats WHERE workspace_id=$1 AND ticket_id=$2",
    )
    .bind(workspace_id.0)
    .bind(&body.ticket_id)
    .fetch_optional(&mut *tx)
    .await?;
    if let Some((chat_id, bound_workspace)) = existing {
        if bound_workspace == coder_workspace {
            let live_chat = client.get_chat_opt(&chat_id).await.map_err(|error| {
                tracing::warn!(%error, workspace_id = %body.workspace_id, "Coder Chat lookup failed");
                ManagerError::api("CODER_CHAT_UNAVAILABLE", "could not verify the worker Chat")
                    .retryable(true)
            })?;
            if let Some(chat) = live_chat {
                if chat.owner_id != coder_owner
                    || chat.workspace_id != coder_workspace
                    || chat.labels != labels
                {
                    return Err(ManagerError::api(
                        "CODER_SCOPE_MISMATCH",
                        "stored Coder Chat does not match the tenant workspace",
                    ));
                }
                tx.commit().await?;
                return Ok(Json(
                    serde_json::json!({"chat_id":chat_id,"workspace_id":body.workspace_id}),
                )
                .into_response());
            }
        }
        sqlx::query("DELETE FROM runtime_chats WHERE workspace_id=$1 AND ticket_id=$2")
            .bind(workspace_id.0)
            .bind(&body.ticket_id)
            .execute(&mut *tx)
            .await?;
    }
    // Reconcile a Coder Chat created just before a Manager crash, then persist
    // the ID before returning it to Nexus.
    let chats = client.list_chats().await.map_err(|error| {
        tracing::warn!(%error, workspace_id = %body.workspace_id, "Coder Chat reconciliation failed");
        ManagerError::api("CODER_CHAT_UNAVAILABLE", "could not inspect the tenant creator's Coder chats").retryable(true)
    })?;
    let chat = if let Some(chat) = chats.into_iter().find(|chat| {
        chat.workspace_id == coder_workspace
            && chat.owner_id == coder_owner
            && chat.labels == labels
    }) {
        chat
    } else {
        client
            .create_chat(&coder_client::types::CreateChatRequest {
                organization_id: Some(coder_org),
                owner_id: Some(coder_owner.clone()),
                workspace_id: coder_workspace.clone(),
                model_config_id: None,
                content: vec![coder_client::types::ChatInputPart::text(&body.prompt)],
                labels: Some(labels),
            })
            .await
            .map_err(|error| {
                tracing::warn!(%error, workspace_id = %body.workspace_id, "Coder Chat creation failed");
                if error.to_string().contains("403 Forbidden") {
                    ManagerError::api("CODER_CHAT_FORBIDDEN", "Coder refused to create a Chat owned by the tenant creator")
                } else {
                    ManagerError::api("CODER_CHAT_UNAVAILABLE", "Coder could not create the worker Chat").retryable(true)
                }
            })?
    };
    if chat.owner_id != coder_owner {
        return Err(ManagerError::api(
            "CODER_SCOPE_MISMATCH",
            "Coder Chat owner differs from tenant creator",
        ));
    }
    sqlx::query("INSERT INTO runtime_chats(workspace_id,ticket_id,coder_workspace_id,coder_chat_id) VALUES($1,$2,$3,$4)")
        .bind(workspace_id.0)
        .bind(&body.ticket_id)
        .bind(&coder_workspace)
        .bind(&chat.id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(
        Json(serde_json::json!({"chat_id":chat.id,"workspace_id":body.workspace_id}))
            .into_response(),
    )
}

pub async fn get_workspace(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(workspace): Path<String>,
) -> Result<Json<crate::runtime::workspaces::RuntimeWorkspaceStatus>, ManagerError> {
    let services = state
        .services()
        .ok_or_else(|| ManagerError::api("SERVICE_UNAVAILABLE", "service unavailable"))?;
    let scope = authenticate_runtime(services, &headers).await?;
    let workspace_id = crate::runtime::workspaces::parse_workspace_id(&workspace)?;
    Ok(Json(
        services
            .runtime_workspaces
            .get_workspace(&scope, workspace_id)
            .await?,
    ))
}

pub async fn stop_workspace(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(workspace): Path<String>,
) -> Result<Response, ManagerError> {
    mutate_workspace(state, headers, workspace, false).await
}

pub async fn delete_workspace(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(workspace): Path<String>,
) -> Result<Response, ManagerError> {
    mutate_workspace(state, headers, workspace, true).await
}

async fn mutate_workspace(
    state: AppState,
    headers: HeaderMap,
    workspace: String,
    delete: bool,
) -> Result<Response, ManagerError> {
    let services = state
        .services()
        .ok_or_else(|| ManagerError::api("SERVICE_UNAVAILABLE", "service unavailable"))?;
    let scope = authenticate_runtime(services, &headers).await?;
    let idempotency_key = crate::routes::organizations::idempotency_key(&headers)?;
    let workspace_id = crate::runtime::workspaces::parse_workspace_id(&workspace)?;
    let result = services
        .runtime_workspaces
        .mutate_worker(&scope, workspace_id, delete, &idempotency_key)
        .await?;
    Ok((axum::http::StatusCode::ACCEPTED, Json(result)).into_response())
}

/// `POST /runtime/heartbeat` — a workspace principal reports its observed
/// readiness. Cannot change ownership or role.
pub async fn heartbeat(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<crate::runtime::HeartbeatResponse>, ManagerError> {
    let services = state
        .services()
        .ok_or_else(|| ManagerError::api("SERVICE_UNAVAILABLE", "service unavailable"))?;
    let scope = authenticate_runtime(services, &headers).await?;
    let result = services.runtime_workspaces.heartbeat(&scope).await?;
    Ok(Json(result))
}

/// `POST /runtime/refresh` — rotate the workspace's runtime credential. The new
/// secret is returned exactly once; rotation does not rely on a workspace
/// rebuild.
#[derive(Serialize)]
pub struct RefreshResponse {
    pub workspace_id: String,
    pub access_token: String,
    pub access_expires_at: chrono::DateTime<chrono::Utc>,
    pub refresh_token: String,
    pub refresh_expires_at: chrono::DateTime<chrono::Utc>,
    pub generation: i64,
}

pub async fn refresh(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Response, ManagerError> {
    let services = state
        .services()
        .ok_or_else(|| ManagerError::api("SERVICE_UNAVAILABLE", "service unavailable"))?;
    let bearer = bearer_token(&headers)
        .ok_or_else(|| ManagerError::api("UNAUTHORIZED", "refresh credential required"))?;
    let issued = services
        .runtime_credentials
        .refresh_session(&bearer)
        .await?;
    let body = RefreshResponse {
        workspace_id: issued.workspace_id.to_string(),
        access_token: issued.access_secret,
        access_expires_at: issued.access_expires_at,
        refresh_token: issued.refresh_secret,
        refresh_expires_at: issued.refresh_expires_at,
        generation: issued.generation,
    };
    let mut response = Json(body).into_response();
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        "no-store"
            .parse()
            .map_err(|_| ManagerError::Service(anyhow::anyhow!("invalid cache-control header")))?,
    );
    Ok(response)
}

/// `POST /runtime/revoke` — a workspace principal revokes its own runtime
/// issuance (advances the workspace generation so no previously issued
/// credential can authenticate). Used on shutdown and to support rotation
/// without a rebuild.
pub async fn revoke(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<axum::http::StatusCode, ManagerError> {
    let services = state
        .services()
        .ok_or_else(|| ManagerError::api("SERVICE_UNAVAILABLE", "service unavailable"))?;
    let scope = authenticate_runtime(services, &headers).await?;
    services
        .runtime_credentials
        .revoke(scope.workspace_id)
        .await?;
    Ok(axum::http::StatusCode::NO_CONTENT)
}

/// A conservative safe-branch check: letters, digits, underscores, hyphens,
/// slashes, dots; reject whitespace, control chars, `..`, and leading `-`.
fn safe_branch(branch: &str) -> bool {
    !branch.is_empty()
        && !branch.starts_with('-')
        && !branch.contains("..")
        && branch
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-' || b == b'/' || b == b'.')
}

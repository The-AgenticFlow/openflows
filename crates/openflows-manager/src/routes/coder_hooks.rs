//! Trusted Coder lifecycle-hook ingress and tenant-scoped relay.

use crate::error::ManagerError;
use crate::server::AppState;
use axum::body::{to_bytes, Body};
use axum::extract::{Request, State};
use axum::http::{header, Response, StatusCode};
use jsonwebtoken::{Algorithm, DecodingKey, EncodingKey, Header, Validation};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const MAX_HOOK_BODY: usize = 2 * 1024 * 1024;

#[derive(Debug, Clone, Deserialize, Serialize)]
struct HookClaims {
    #[serde(default)]
    iss: String,
    aud: String,
    exp: usize,
    jti: String,
    #[serde(default)]
    r#type: Option<String>,
    #[serde(default)]
    sub: Option<String>,
    #[serde(default)]
    body_sha256: Option<String>,
}

#[derive(Deserialize)]
struct HookEnvelope {
    #[serde(rename = "type")]
    event_type: String,
    meta: HookMeta,
}

#[derive(Deserialize)]
struct HookMeta {
    dispatch_id: String,
    chat_id: String,
    #[serde(default)]
    owner_id: String,
    #[serde(default)]
    workspace_id: Option<String>,
}

pub async fn receive(
    State(state): State<AppState>,
    request: Request,
) -> Result<Response<Body>, ManagerError> {
    let services = state
        .services()
        .ok_or_else(|| ManagerError::api("SERVICE_UNAVAILABLE", "service unavailable"))?;
    let secret = services.coder_hook_secret.as_deref().ok_or_else(|| {
        ManagerError::api(
            "SERVICE_UNAVAILABLE",
            "Coder hook ingress is not configured",
        )
    })?;
    if secret.len() < 32 {
        return Err(ManagerError::api(
            "SERVICE_UNAVAILABLE",
            "Coder hook ingress secret is invalid",
        ));
    }
    let auth = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .map(str::to_owned)
        .ok_or_else(|| ManagerError::api("UNAUTHORIZED", "missing Coder hook JWT"))?;
    let body = to_bytes(request.into_body(), MAX_HOOK_BODY)
        .await
        .map_err(|_| ManagerError::InvalidInput("invalid hook body".into()))?;
    let envelope: HookEnvelope = serde_json::from_slice(&body)
        .map_err(|_| ManagerError::InvalidInput("invalid hook JSON".into()))?;
    let expected_audience = crate::config::coder_hook_url(&services.auth_config.public_url)
        .map_err(ManagerError::Config)?;
    let claims = verify(&auth, secret, &expected_audience, &envelope, body.as_ref())?;

    let targets: Vec<(uuid::Uuid, uuid::Uuid, String)> = sqlx::query_as(
        "SELECT source.organization_id, source.tenant_id,
                hook.secret_ref
           FROM workspaces source
           JOIN workspaces nexus ON nexus.organization_id = source.organization_id
                AND nexus.tenant_id = source.tenant_id AND nexus.role = 'nexus'
                AND nexus.desired_state <> 'deleted'
           JOIN runtime_identities identity ON identity.tenant_id = source.tenant_id
                AND identity.organization_id = source.organization_id
           JOIN tenant_hook_keys hook ON hook.tenant_id = source.tenant_id
                AND hook.organization_id = source.organization_id AND hook.revoked_at IS NULL
          WHERE (($1::text IS NOT NULL AND source.coder_workspace_id = $1)
                 OR ($1::text IS NULL AND identity.coder_owner_id = $2))
            AND source.desired_state <> 'deleted'",
    )
    .bind(envelope.meta.workspace_id.as_deref())
    .bind(&envelope.meta.owner_id)
    .fetch_all(services.db.pool())
    .await?;
    // The owner may own several workspaces within one tenant (e.g. Nexus plus
    // a worker), and every matching workspace joins to the same Nexus row, so
    // the raw result can contain duplicate tenant targets. Deduplicate by
    // tenant before deciding the owner match is ambiguous.
    let mut seen = std::collections::HashSet::new();
    let targets: Vec<_> = targets
        .into_iter()
        .filter(|(organization_id, tenant_id, _)| seen.insert((*organization_id, *tenant_id)))
        .collect();
    let target = if envelope.meta.workspace_id.is_none() && targets.len() != 1 {
        return Err(ManagerError::api(
            "AMBIGUOUS_HOOK_WORKSPACE",
            "workspace_id is required when the Coder owner has multiple tenants",
        ));
    } else {
        targets.into_iter().next()
    };
    let (_organization_id, tenant_id, hook_secret_ref) =
        target.ok_or_else(|| ManagerError::not_found("hook workspace"))?;

    // Best-effort: ensure a Zep thread exists for this conversation so the
    // agents can persist and later recall the discussion. Keyed by our chat id
    // and the Coder owner id. Non-fatal; Zep is never on the relay path.
    if let Some(zep) = services.zep.clone() {
        let chat_id = envelope.meta.chat_id.clone();
        let owner_id = envelope.meta.owner_id.clone();
        let db = services.db.pool().clone();
        tokio::spawn(async move {
            let user_id: Option<uuid::Uuid> = sqlx::query_scalar(
                "SELECT user_id FROM coder_user_links WHERE coder_user_id = $1 LIMIT 1",
            )
            .bind(owner_id)
            .fetch_optional(&db)
            .await
            .ok()
            .flatten();
            if let Some(user_id) = user_id {
                zep.thread_create(&chat_id, &user_id.to_string()).await;
            }
        });
    }

    let tenant_secret = zeroize::Zeroizing::new(
        services
            .secret_provider
            .get(&hook_secret_ref)
            .await
            .map_err(|_| {
                ManagerError::api("SERVICE_UNAVAILABLE", "tenant hook channel unavailable")
            })?,
    );
    let relay_url = crate::config::runtime_hook_url(tenant_id).map_err(ManagerError::Config)?;
    let relay_claims = HookClaims {
        iss: "openflows-manager".into(),
        aud: relay_url.clone(),
        exp: claims.exp,
        jti: claims.jti,
        r#type: claims.r#type,
        sub: claims.sub,
        body_sha256: claims.body_sha256,
    };
    let token = jsonwebtoken::encode(
        &Header::new(Algorithm::HS256),
        &relay_claims,
        &EncodingKey::from_secret(&tenant_secret),
    )
    .map_err(|_| ManagerError::api("SERVICE_UNAVAILABLE", "cannot sign tenant hook relay"))?;
    let upstream = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        reqwest::Client::new()
            .post(&relay_url)
            .bearer_auth(token)
            .header(header::CONTENT_TYPE.as_str(), "application/json")
            .body(body.to_vec())
            .send(),
    )
    .await
    .map_err(|_| ManagerError::api("SERVICE_UNAVAILABLE", "tenant hook relay timed out"))?
    .map_err(|_| ManagerError::api("SERVICE_UNAVAILABLE", "tenant hook relay failed"))?;
    let status = StatusCode::from_u16(upstream.status().as_u16())
        .map_err(|_| ManagerError::api("SERVICE_UNAVAILABLE", "invalid relay status"))?;
    let content_type = upstream
        .headers()
        .get(header::CONTENT_TYPE)
        .cloned()
        .unwrap_or_else(|| header::HeaderValue::from_static("application/json"));
    let response_body = upstream
        .bytes()
        .await
        .map_err(|_| ManagerError::api("SERVICE_UNAVAILABLE", "invalid relay response"))?;
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, content_type)
        .body(Body::from(response_body))
        .map_err(|e| ManagerError::Service(e.into()))
}

fn verify(
    token: &str,
    secret: &[u8],
    audience: &str,
    envelope: &HookEnvelope,
    body: &[u8],
) -> Result<HookClaims, ManagerError> {
    let mut validation = Validation::new(Algorithm::HS256);
    validation.set_audience(&[audience]);
    let claims =
        jsonwebtoken::decode::<HookClaims>(token, &DecodingKey::from_secret(secret), &validation)
            .map_err(|_| ManagerError::api("UNAUTHORIZED", "invalid Coder hook JWT"))?
            .claims;
    let digest = format!("{:x}", Sha256::digest(body));
    let expected_sub = format!("coder:chat:{}", envelope.meta.chat_id);
    if claims.iss.trim().is_empty()
        || claims.jti != envelope.meta.dispatch_id
        || claims.r#type.as_deref() != Some(envelope.event_type.as_str())
        || claims.sub.as_deref() != Some(expected_sub.as_str())
        || claims
            .body_sha256
            .as_deref()
            .is_none_or(|value| !value.eq_ignore_ascii_case(&digest))
    {
        return Err(ManagerError::api(
            "UNAUTHORIZED",
            "Coder hook JWT does not match its request",
        ));
    }
    Ok(claims)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn signed(body: &[u8], workspace_id: &str) -> (String, HookEnvelope) {
        let envelope: HookEnvelope = serde_json::from_slice(body).unwrap();
        let claims = HookClaims {
            iss: "coder".into(),
            aud: "https://manager.example/api/v1/hooks/coder/chat".into(),
            exp: (chrono::Utc::now().timestamp() + 60) as usize,
            jti: envelope.meta.dispatch_id.clone(),
            r#type: Some(envelope.event_type.clone()),
            sub: Some(format!("coder:chat:{}", envelope.meta.chat_id)),
            body_sha256: Some(format!("{:x}", Sha256::digest(body))),
        };
        let token = jsonwebtoken::encode(
            &Header::new(Algorithm::HS256),
            &claims,
            &EncodingKey::from_secret(workspace_id.as_bytes()),
        )
        .unwrap();
        (token, envelope)
    }

    #[test]
    fn hook_jwt_is_bound_to_the_exact_payload() {
        let secret = b"01234567890123456789012345678901";
        let body = br#"{"type":"chat.updated","meta":{"dispatch_id":"d1","chat_id":"c1","workspace_id":"w1"}}"#;
        let (token, envelope) = signed(body, std::str::from_utf8(secret).unwrap());
        verify(
            &token,
            secret,
            "https://manager.example/api/v1/hooks/coder/chat",
            &envelope,
            body,
        )
        .unwrap();
        assert!(verify(
            &token,
            secret,
            "https://manager.example/api/v1/hooks/coder/chat",
            &envelope,
            b"{}",
        )
        .is_err());
    }

    #[test]
    fn hook_jwt_from_another_signing_boundary_is_rejected() {
        let body = br#"{"type":"chat.updated","meta":{"dispatch_id":"d2","chat_id":"c2","workspace_id":"w2"}}"#;
        let (token, envelope) = signed(body, "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
        assert!(verify(
            &token,
            b"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            "https://manager.example/api/v1/hooks/coder/chat",
            &envelope,
            body,
        )
        .is_err());
    }
}

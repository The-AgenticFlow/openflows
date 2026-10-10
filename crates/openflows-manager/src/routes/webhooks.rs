//! Public GitHub webhook ingress.
//!
//! Authenticates exclusively by the verified `X-Hub-Signature-256` signature
//! against the exact raw request bytes (constant-time), enforces a body-size
//! limit and event allowlist, and durably records the delivery + processing
//! event before returning 202. Returns 503 when durable persistence is
//! unavailable so GitHub can retry. Webhook identity is never proof that an
//! installation may be bound; unknown installations are never auto-bound.

use crate::connections::webhooks::IngressOutcome;
use crate::error::ManagerError;
use axum::{
    body::to_bytes,
    extract::{Request, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use serde::Serialize;

/// POST /api/v1/webhooks/github — signed webhook ingress.
pub async fn receive_webhook(
    State(state): State<crate::server::AppState>,
    headers: HeaderMap,
    request: Request,
) -> Result<Response, ManagerError> {
    let services = state
        .services()
        .ok_or_else(|| ManagerError::api("SERVICE_UNAVAILABLE", "webhook service unavailable"))?;

    let Some(secret) = &services.webhook_secret else {
        return Err(ManagerError::api(
            "SERVICE_UNAVAILABLE",
            "webhook service is not configured",
        ));
    };

    // Bound collection itself, including chunked bodies without Content-Length.
    let body = match to_bytes(request.into_body(), services.webhooks.body_limit).await {
        Ok(body) => body,
        Err(_) => return Ok(StatusCode::PAYLOAD_TOO_LARGE.into_response()),
    };

    let delivery_id = header(&headers, "x-github-delivery")
        .ok_or_else(|| ManagerError::api("UNAUTHORIZED", "missing webhook delivery id"))?;
    let event = header(&headers, "x-github-event")
        .ok_or_else(|| ManagerError::api("UNAUTHORIZED", "missing webhook event"))?;
    let provided_signature = header(&headers, "x-hub-signature-256")
        .ok_or_else(|| ManagerError::api("UNAUTHORIZED", "missing webhook signature"))?;

    // Verify the signature against the exact raw bytes before any parsing.
    if !crate::connections::webhooks::verify_signature(&body, secret, &provided_signature) {
        return Err(ManagerError::api(
            "UNAUTHORIZED",
            "invalid webhook signature",
        ));
    }

    let outcome = services
        .webhooks
        .receive(&delivery_id, &event, &body)
        .await?;

    Ok(match outcome {
        IngressOutcome::Accepted { .. } => (
            StatusCode::ACCEPTED,
            Json(WebhookResponse { status: "accepted" }),
        )
            .into_response(),
        IngressOutcome::Duplicate => (
            StatusCode::OK,
            Json(WebhookResponse {
                status: "duplicate",
            }),
        )
            .into_response(),
        IngressOutcome::DigestMismatch => (
            StatusCode::CONFLICT,
            Json(WebhookResponse {
                status: "digest_mismatch",
            }),
        )
            .into_response(),
    })
}

#[derive(Serialize)]
struct WebhookResponse {
    status: &'static str,
}

fn header(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get(name)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string())
}

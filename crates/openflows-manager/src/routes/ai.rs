//! HTTP route handlers for AI Provider, Model, and Model Assignment Policy APIs (Issue #290).

use crate::{
    error::ManagerError,
    models::{
        AiModelCreateRequest, AiModelSummary, AiModelUpdateRequest, AiProviderCreateRequest,
        AiProviderSummary, AiProviderUpdateRequest, ModelPolicy, ResolvedModel,
    },
    server::AppState,
};
use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, patch},
    Json, Router,
};
use serde::Deserialize;

pub fn router() -> Router<AppState> {
    Router::new()
        // Providers
        .route("/ai/providers", get(list_providers).post(create_provider))
        .route(
            "/ai/providers/{id}",
            get(get_provider)
                .patch(update_provider)
                .delete(delete_provider),
        )
        // Models
        .route("/ai/models", get(list_models).post(create_model))
        .route("/ai/models/{id}", patch(update_model).delete(delete_model))
        // Global Policy
        .route(
            "/ai/model-policy",
            get(get_global_policy).put(set_global_policy),
        )
        .route("/ai/model-policy/resolve", get(resolve_model))
        // Tenant-scoped Policy
        .route(
            "/tenants/{tenant}/ai/model-policy",
            get(get_tenant_policy).put(set_tenant_policy),
        )
}

// ── Provider Handlers ──────────────────────────────────────────────────────

async fn list_providers(
    State(state): State<AppState>,
) -> Result<Json<Vec<AiProviderSummary>>, ManagerError> {
    let providers = state.ai_service().list_providers().await?;
    Ok(Json(providers))
}

async fn get_provider(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<AiProviderSummary>, ManagerError> {
    let provider = state.ai_service().get_provider(&id).await?;
    Ok(Json(provider))
}

async fn create_provider(
    State(state): State<AppState>,
    Json(payload): Json<AiProviderCreateRequest>,
) -> Result<impl IntoResponse, ManagerError> {
    let summary = state.ai_service().create_provider(payload).await?;
    Ok((StatusCode::CREATED, Json(summary)))
}

async fn update_provider(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(payload): Json<AiProviderUpdateRequest>,
) -> Result<Json<AiProviderSummary>, ManagerError> {
    let summary = state.ai_service().update_provider(&id, payload).await?;
    Ok(Json(summary))
}

async fn delete_provider(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ManagerError> {
    state.ai_service().delete_provider(&id).await?;
    Ok(Json(serde_json::json!({
        "status": "deleted",
        "id": id,
    })))
}

// ── Model Handlers ─────────────────────────────────────────────────────────

async fn list_models(
    State(state): State<AppState>,
) -> Result<Json<Vec<AiModelSummary>>, ManagerError> {
    let models = state.ai_service().list_models().await?;
    Ok(Json(models))
}

async fn create_model(
    State(state): State<AppState>,
    Json(payload): Json<AiModelCreateRequest>,
) -> Result<impl IntoResponse, ManagerError> {
    let summary = state.ai_service().create_model(payload).await?;
    Ok((StatusCode::CREATED, Json(summary)))
}

async fn update_model(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(payload): Json<AiModelUpdateRequest>,
) -> Result<Json<AiModelSummary>, ManagerError> {
    let summary = state.ai_service().update_model(&id, payload).await?;
    Ok(Json(summary))
}

async fn delete_model(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ManagerError> {
    state.ai_service().delete_model(&id).await?;
    Ok(Json(serde_json::json!({
        "status": "deleted",
        "id": id,
    })))
}

// ── Policy Handlers ────────────────────────────────────────────────────────

async fn get_global_policy(
    State(state): State<AppState>,
) -> Result<Json<ModelPolicy>, ManagerError> {
    let policy = state.ai_service().get_global_model_policy().await?;
    Ok(Json(policy))
}

async fn set_global_policy(
    State(state): State<AppState>,
    Json(policy): Json<ModelPolicy>,
) -> Result<Json<ModelPolicy>, ManagerError> {
    state
        .ai_service()
        .set_global_model_policy(policy.clone())
        .await?;
    Ok(Json(policy))
}

async fn get_tenant_policy(
    State(state): State<AppState>,
    Path(tenant): Path<String>,
) -> Result<Json<ModelPolicy>, ManagerError> {
    let policy = state.ai_service().get_tenant_model_policy(&tenant).await?;
    Ok(Json(policy))
}

async fn set_tenant_policy(
    State(state): State<AppState>,
    Path(tenant): Path<String>,
    Json(policy): Json<ModelPolicy>,
) -> Result<Json<ModelPolicy>, ManagerError> {
    state
        .ai_service()
        .set_tenant_model_policy(&tenant, policy.clone())
        .await?;
    Ok(Json(policy))
}

#[derive(Debug, Deserialize)]
pub struct ResolveQuery {
    pub role: String,
    #[serde(default)]
    pub tenant: Option<String>,
}

async fn resolve_model(
    State(state): State<AppState>,
    Query(query): Query<ResolveQuery>,
) -> Result<Json<ResolvedModel>, ManagerError> {
    let resolved = state
        .ai_service()
        .resolve_model_for_role(&query.role, query.tenant.as_deref())
        .await?;
    Ok(Json(resolved))
}

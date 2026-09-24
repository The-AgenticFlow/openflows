//! Error types and structured JSON responses for the OpenFlows Manager API.

use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Standard machine-readable error response envelope.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ApiErrorEnvelope {
    pub error: ApiError,
}

/// Detailed error payload.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ApiError {
    pub code: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
}

impl ApiErrorEnvelope {
    pub fn new(
        code: impl Into<String>,
        message: impl Into<String>,
        request_id: Option<String>,
    ) -> Self {
        Self {
            error: ApiError {
                code: code.into(),
                message: message.into(),
                request_id,
            },
        }
    }
}

/// Manager domain and HTTP error type.
#[derive(Debug, Error)]
pub enum ManagerError {
    #[error("configuration error: {0}")]
    Config(String),

    #[error("tenant '{0}' not found")]
    TenantNotFound(String),

    #[error("tenant '{0}' already exists")]
    TenantAlreadyExists(String),

    #[error("invalid tenant name: {0}")]
    InvalidTenantName(String),

    #[error("invalid repository: {0}")]
    InvalidRepository(String),

    #[error("invalid request: {0}")]
    InvalidRequest(String),

    #[error("ticket '{ticket}' not found in tenant '{tenant}'")]
    TicketNotFound { tenant: String, ticket: String },

    #[error("AI provider '{0}' not found")]
    ProviderNotFound(String),

    #[error("AI model '{0}' not found")]
    ModelNotFound(String),

    #[error("store error: {0}")]
    Store(String),

    #[error(transparent)]
    Io(#[from] std::io::Error),

    #[error(transparent)]
    Service(#[from] anyhow::Error),
}

impl ManagerError {
    pub fn error_code(&self) -> &'static str {
        match self {
            Self::Config(_) => "config_error",
            Self::TenantNotFound(_) => "tenant_not_found",
            Self::TenantAlreadyExists(_) => "tenant_already_exists",
            Self::InvalidTenantName(_) => "invalid_tenant_name",
            Self::InvalidRepository(_) => "invalid_repository",
            Self::InvalidRequest(_) => "invalid_request",
            Self::TicketNotFound { .. } => "ticket_not_found",
            Self::ProviderNotFound(_) => "provider_not_found",
            Self::ModelNotFound(_) => "model_not_found",
            Self::Store(_) => "store_error",
            Self::Io(_) => "io_error",
            Self::Service(_) => "internal_error",
        }
    }

    pub fn status_code(&self) -> StatusCode {
        match self {
            Self::TenantNotFound(_)
            | Self::TicketNotFound { .. }
            | Self::ProviderNotFound(_)
            | Self::ModelNotFound(_) => StatusCode::NOT_FOUND,
            Self::TenantAlreadyExists(_) => StatusCode::CONFLICT,
            Self::InvalidTenantName(_) | Self::InvalidRepository(_) | Self::InvalidRequest(_) => {
                StatusCode::BAD_REQUEST
            }
            Self::Config(_) => StatusCode::INTERNAL_SERVER_ERROR,
            Self::Store(_) => StatusCode::SERVICE_UNAVAILABLE,
            Self::Io(_) => StatusCode::INTERNAL_SERVER_ERROR,
            Self::Service(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    pub fn into_response_with_request_id(self, request_id: Option<String>) -> Response {
        let status = self.status_code();
        let code = self.error_code().to_string();
        let message = self.to_string();

        let body = ApiErrorEnvelope::new(code, message, request_id);
        (status, Json(body)).into_response()
    }

    pub fn to_api_response(
        &self,
        request_id: Option<&crate::middleware::RequestId>,
    ) -> (StatusCode, Json<ApiErrorEnvelope>) {
        let status = self.status_code();
        let code = self.error_code().to_string();
        let message = self.to_string();
        let req_id = request_id.map(|r| r.0.clone());
        (status, Json(ApiErrorEnvelope::new(code, message, req_id)))
    }
}

impl IntoResponse for ManagerError {
    fn into_response(self) -> Response {
        self.into_response_with_request_id(None)
    }
}

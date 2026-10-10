//! Error types shared by the OpenFlows Manager services and HTTP server.

use thiserror::Error;

/// The canonical error type for Manager services.
///
/// The [`ApiError`] variant carries a stable error code and a sanitized message
/// suitable for the HTTP envelope defined by the shared API conventions.
/// Database and validation failures are translated here so handlers never leak
/// SQL or internal details to clients.
#[derive(Debug, Error)]
pub enum ManagerError {
    #[error("configuration error: {0}")]
    Config(String),

    #[error(transparent)]
    Io(#[from] std::io::Error),

    #[error(transparent)]
    Service(#[from] anyhow::Error),

    /// An API-level error with a stable code and sanitized message.
    #[error(transparent)]
    Api(ApiError),

    /// A database-layer failure that has not been translated to an API error.
    #[error(transparent)]
    Database(#[from] sqlx::Error),

    /// A migration could not be applied. The manager must remain unready.
    #[error("database migration failed: {0}")]
    Migration(String),

    /// A resource was not found, or exists outside the caller's scope.
    #[error("resource not found")]
    NotFound,

    /// A uniqueness or state-transition conflict (HTTP 409).
    #[error("conflict: {0}")]
    Conflict(String),

    /// Invalid caller-supplied input (HTTP 422).
    #[error("invalid input: {0}")]
    InvalidInput(String),
}

impl ManagerError {
    /// Build an API error from a stable code and sanitized message.
    pub fn api(code: &str, message: impl Into<String>) -> Self {
        ManagerError::Api(ApiError::new(code, message))
    }

    /// Convenience constructor for resource-not-found with a resource kind.
    pub fn not_found(resource: &str) -> Self {
        ManagerError::Api(ApiError::new(
            "RESOURCE_NOT_FOUND",
            format!("{resource} not found or outside your organization"),
        ))
    }

    /// Mark the error as retryable (HTTP 429 / 503 with retry guidance).
    pub fn retryable(self, retryable: bool) -> Self {
        match self {
            ManagerError::Api(mut api) => {
                api.retryable = retryable;
                ManagerError::Api(api)
            }
            other => other,
        }
    }
}

/// A sanitized, HTTP-safe API error envelope.
#[derive(Debug, Clone)]
pub struct ApiError {
    pub code: String,
    pub message: String,
    pub retryable: bool,
}

impl ApiError {
    pub fn new(code: &str, message: impl Into<String>) -> Self {
        ApiError {
            code: code.to_string(),
            message: message.into(),
            retryable: false,
        }
    }

    pub fn retryable(mut self, retryable: bool) -> Self {
        self.retryable = retryable;
        self
    }
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for ApiError {}

/// Map a [`ManagerError`] to an HTTP status code and sanitized JSON body.
///
/// Never include the internal error text, SQL, connection URLs, or credentials
/// in the response. The request_id is filled in by the caller from the request.
pub fn to_http_response(error: &ManagerError) -> (u16, serde_json::Value) {
    let (status, code, message, retryable) = match error {
        ManagerError::Api(api) => {
            // Domain API errors carry their own code and message.
            let status = api_status(&api.code);
            (status, api.code.clone(), api.message.clone(), api.retryable)
        }
        ManagerError::NotFound => (
            404,
            "RESOURCE_NOT_FOUND".to_string(),
            "resource not found or outside your organization".to_string(),
            false,
        ),
        ManagerError::Conflict(msg) => (409, "CONFLICT".to_string(), msg.clone(), false),
        ManagerError::InvalidInput(msg) => (422, "INVALID_INPUT".to_string(), msg.clone(), false),
        ManagerError::Config(_) | ManagerError::Io(_) | ManagerError::Migration(_) => (
            503,
            "INTERNAL".to_string(),
            "internal service error".to_string(),
            false,
        ),
        ManagerError::Service(_) | ManagerError::Database(_) => (
            503,
            "INTERNAL".to_string(),
            "internal service error".to_string(),
            false,
        ),
    };

    let body = serde_json::json!({
        "error": {
            "code": code,
            "message": message,
            "request_id": crate::server::REQUEST_ID.try_with(Clone::clone).unwrap_or_default(),
            "retryable": retryable,
        }
    });
    (status, body)
}

fn api_status(code: &str) -> u16 {
    match code {
        "NOT_FOUND" | "RESOURCE_NOT_FOUND" => 404,
        "CONFLICT" | "INSTALLATION_ALREADY_BOUND" => 409,
        "INVALID_INPUT" | "DEVICE_CODE_INVALID" => 422,
        "UNAUTHORIZED" | "MISSING_AUTH" | "AUTH_FAILED" | "CSRF_FAILED" => 401,
        "FORBIDDEN"
        | "ORG_ADMIN_REQUIRED"
        | "GITHUB_OWNER_REQUIRED"
        | "MEMBER_SUSPENDED"
        | "OWNER_REQUIRED"
        | "INVITATION_WRONG_USER"
        | "REAUTH_REQUIRED"
        | "ORG_UNAVAILABLE"
        | "TENANT_UNAVAILABLE"
        | "WORKSPACE_UNAVAILABLE"
        | "CONNECTION_UNAVAILABLE"
        | "REPOSITORY_UNAVAILABLE"
        | "AUTH_CHANGED" => 403,
        "RATE_LIMITED" | "TOO_MANY_REQUESTS" => 429,
        "SERVICE_UNAVAILABLE" | "GITHUB_UNAVAILABLE" => 503,
        "GITHUB_OAUTH_ERROR" => 401,
        _ => 500,
    }
}

/// Render a [`ManagerError`] as an HTTP response using the shared error
/// envelope, so axum handlers can return `Result<_, ManagerError>` directly.
impl axum::response::IntoResponse for ManagerError {
    fn into_response(self) -> axum::response::Response {
        let (status, body) = to_http_response(&self);
        let mut response = (
            axum::http::StatusCode::from_u16(status).expect("mapped HTTP status"),
            axum::Json(body),
        )
            .into_response();
        if status == 429 {
            response
                .headers_mut()
                .insert("retry-after", "60".parse().unwrap());
        }
        response
    }
}

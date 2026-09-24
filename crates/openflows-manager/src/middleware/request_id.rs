//! Request correlation ID extractor and injector.

use axum::{extract::Request, http::HeaderValue, middleware::Next, response::Response};
use rand::{distributions::Alphanumeric, Rng};

pub const REQUEST_ID_HEADER: &str = "x-request-id";

/// Newtype wrapper for a request/correlation ID.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestId(pub String);

impl RequestId {
    pub fn new() -> Self {
        let rand_part: String = rand::thread_rng()
            .sample_iter(&Alphanumeric)
            .take(16)
            .map(char::from)
            .collect();
        Self(format!("req_{}", rand_part))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Default for RequestId {
    fn default() -> Self {
        Self::new()
    }
}

/// Axum middleware that ensures every incoming request has a correlation ID,
/// makes it available in request extensions, and includes it on the outgoing response.
pub async fn request_id_middleware(mut req: Request, next: Next) -> Response {
    let request_id = req
        .headers()
        .get(REQUEST_ID_HEADER)
        .and_then(|v| v.to_str().ok())
        .filter(|s| !s.trim().is_empty())
        .map(|s| RequestId(s.trim().to_string()))
        .unwrap_or_else(RequestId::new);

    req.extensions_mut().insert(request_id.clone());

    let mut response = next.run(req).await;

    if let Ok(header_val) = HeaderValue::from_str(request_id.as_str()) {
        response.headers_mut().insert(REQUEST_ID_HEADER, header_val);
    }

    response
}

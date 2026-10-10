//! Session-bound double-submit CSRF protection for browser mutations.
//!
//! Pages and the authenticated CSRF endpoint return a digest derived from the
//! opaque browser session, and set the same value as a cookie. Mutations must
//! echo it in a form/header and validate its binding to the current session.
//! A cookie planted for another session therefore cannot authorize a mutation.
//! SameSite=Lax complements these checks; it is not the sole CSRF boundary.

use crate::auth::crypto::{ct_eq, Secret};
use crate::error::ManagerError;

/// The header a browser mutation must carry with the CSRF token.
pub const CSRF_HEADER: &str = "x-csrf-token";
/// The name of the CSRF cookie.
pub const CSRF_COOKIE: &str = "of_csrf";

/// A fresh CSRF token (the raw value is set in the cookie; the same value must
/// be echoed in the header).
pub fn new_token() -> String {
    Secret::generate().encode()
}

/// Validate a presented header token against the cookie token in constant time.
pub fn validate(presented: &str, cookie: &str) -> Result<(), ManagerError> {
    if presented.trim().is_empty() || cookie.trim().is_empty() {
        return Err(ManagerError::api("CSRF_FAILED", "missing CSRF token"));
    }
    if !ct_eq(presented.as_bytes(), cookie.as_bytes()) {
        return Err(ManagerError::api("CSRF_FAILED", "CSRF token mismatch"));
    }
    Ok(())
}

pub fn for_session(session: &str) -> String {
    crate::auth::crypto::hash_token(&format!("openflows-csrf:{session}"))
}

/// Bind the double-submit value to the authenticated browser session.
pub fn validate_session(
    headers: &axum::http::HeaderMap,
    presented: &str,
) -> Result<(), ManagerError> {
    let cookies = crate::routes::auth::parse_cookies(headers);
    let session = cookies
        .get(crate::routes::auth::SESSION_COOKIE)
        .ok_or_else(|| ManagerError::api("CSRF_FAILED", "missing session"))?;
    validate(presented, &for_session(session))
}

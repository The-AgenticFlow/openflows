//! Secret-provider abstraction.
//!
//! The Manager keeps long-lived and recoverable secrets (GitHub App private
//! key, OAuth client secret, webhook secret, Coder operator token, and
//! installation-token references) behind a [`SecretProvider`] trait rather than
//! in plaintext database columns or application code. The concrete backend is a
//! deployment decision (D2 in compatibility-report.md); WP-01 provides the
//! trait and an isolated in-memory test implementation, and the selected
//! production adapter is supplied later via `OPENFLOWS_SECRET_PROVIDER`.
//!
//! Secrets are referenced by an opaque name. The trait is async so adapters
//! backed by network key-management services can be used directly.

use async_trait::async_trait;

/// A reference to a stored secret. Adapters persist the secret value under this
/// name; callers store only the reference in their own records.
pub type SecretRef = String;

/// The interface for storing and retrieving secrets.
#[async_trait]
pub trait SecretProvider: Send + Sync {
    /// Store `value` under `name`, returning a stable reference the caller can
    /// persist. If the name already exists, implementations should reject or
    /// require an explicit rotation rather than silently overwrite.
    async fn put(&self, name: &str, value: &[u8]) -> Result<SecretRef, SecretError>;

    /// Retrieve the secret for `ref_name`.
    async fn get(&self, ref_name: &str) -> Result<Vec<u8>, SecretError>;

    /// Delete a secret. Used during cleanup; not all backends support it.
    async fn delete(&self, ref_name: &str) -> Result<(), SecretError>;
}

/// Errors surfaced by a [`SecretProvider`].
#[derive(Debug, thiserror::Error)]
pub enum SecretError {
    #[error("secret {0} not found")]
    NotFound(String),
    #[error("secret already exists: {0}")]
    AlreadyExists(String),
    #[error("secret provider unavailable: {0}")]
    Unavailable(String),
    #[error("secret provider rejected operation: {0}")]
    Rejected(String),
}

/// The adapter name for the isolated in-memory provider used in tests and local
/// development. A production deployment selects a different adapter.
pub const IN_MEMORY_PROVIDER: &str = "in-memory";

/// An isolated in-memory [`SecretProvider`] for tests and local development.
///
/// This is intentionally not a production backend: it stores secrets in process
/// memory, so it is only suitable for tests and ephemeral local runs. It
/// enforces the same "put does not silently overwrite" contract as production
/// adapters.
#[derive(Debug, Default)]
pub struct InMemorySecretProvider {
    secrets: std::sync::Mutex<std::collections::HashMap<String, Vec<u8>>>,
}

impl InMemorySecretProvider {
    pub fn new() -> Self {
        InMemorySecretProvider::default()
    }
}

#[async_trait]
impl SecretProvider for InMemorySecretProvider {
    async fn put(&self, name: &str, value: &[u8]) -> Result<SecretRef, SecretError> {
        let mut map = self.secrets.lock().unwrap();
        if map.contains_key(name) {
            return Err(SecretError::AlreadyExists(name.to_string()));
        }
        map.insert(name.to_string(), value.to_vec());
        Ok(name.to_string())
    }

    async fn get(&self, ref_name: &str) -> Result<Vec<u8>, SecretError> {
        let map = self.secrets.lock().unwrap();
        map.get(ref_name)
            .cloned()
            .ok_or_else(|| SecretError::NotFound(ref_name.to_string()))
    }

    async fn delete(&self, ref_name: &str) -> Result<(), SecretError> {
        let mut map = self.secrets.lock().unwrap();
        map.remove(ref_name)
            .map(|_| ())
            .ok_or_else(|| SecretError::NotFound(ref_name.to_string()))
    }
}

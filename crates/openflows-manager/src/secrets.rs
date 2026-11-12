//! Secret-provider abstraction.
//!
//! The Manager keeps long-lived and recoverable secrets (GitHub App private
//! key, OAuth client secret, webhook secret, Coder operator token, and
//! installation-token references) behind a [`SecretProvider`] trait rather than
//! in plaintext database columns or application code. The concrete backend is a
//! deployment decision (D2 in compatibility-report.md). AWS deployments use
//! Secrets Manager; local deployments use mode-0600 files; tests can use an
//! isolated in-memory implementation.
//!
//! Secrets are referenced by an opaque name. The trait is async so adapters
//! backed by network key-management services can be used directly.

use async_trait::async_trait;

/// A reference to a stored secret. Adapters persist the secret value under this
/// name; callers store only the reference in their own records.
pub type SecretRef = String;

#[cfg(all(test, unix))]
mod file_mode_tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn dir_with_mode(mode: u32) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(mode)).unwrap();
        dir
    }

    #[tokio::test]
    async fn default_mode_is_owner_only() {
        let dir = dir_with_mode(0o700);
        let p = FilesystemSecretProvider::new(dir.path()).unwrap();
        let r = p.put("a", b"x").await.unwrap();
        let mode = std::fs::metadata(r).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }

    #[tokio::test]
    async fn wider_mode_applies_exactly_in_a_closed_directory() {
        let dir = dir_with_mode(0o700);
        let p = FilesystemSecretProvider::new(dir.path())
            .unwrap()
            .with_file_mode(0o644)
            .unwrap();
        let r = p.put("a", b"x").await.unwrap();
        let mode = std::fs::metadata(r).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o644);
    }

    #[test]
    fn wider_mode_is_refused_when_the_directory_is_open() {
        let dir = dir_with_mode(0o755);
        let err = FilesystemSecretProvider::new(dir.path())
            .unwrap()
            .with_file_mode(0o644)
            .unwrap_err();
        assert!(matches!(err, SecretError::Rejected(_)));
    }

    #[test]
    fn invalid_modes_are_refused() {
        let dir = dir_with_mode(0o700);
        for mode in [0o400, 0o1000, 0o200] {
            let p = FilesystemSecretProvider::new(dir.path()).unwrap();
            assert!(p.with_file_mode(mode).is_err(), "{mode:o}");
        }
    }
}

#[cfg(test)]
mod reference_tests {
    use super::*;

    #[tokio::test]
    async fn filesystem_retry_reference_stays_absolute() {
        let dir = tempfile::tempdir().unwrap();
        let provider = FilesystemSecretProvider::new(dir.path()).unwrap();
        let name = "workspace-runtime-test";
        let original = provider.put(name, b"first").await.unwrap();
        assert!(matches!(
            provider.put(name, b"second").await,
            Err(SecretError::AlreadyExists(_))
        ));
        provider.replace(name, b"second").await.unwrap();
        let recovered = provider.canonical_reference(name).unwrap();
        assert_eq!(recovered, original);
        assert!(std::path::Path::new(&recovered).is_absolute());
        assert_eq!(provider.canonical_reference(&recovered).unwrap(), recovered);
        assert_eq!(provider.get(&recovered).await.unwrap(), b"second");
        assert!(provider.canonical_reference("../outside").is_err());
    }

    #[test]
    fn aws_references_remain_inside_the_deployment_namespace() {
        let config = aws_sdk_secretsmanager::Config::builder()
            .region(aws_sdk_secretsmanager::config::Region::new("us-east-1"))
            .behavior_version(aws_sdk_secretsmanager::config::BehaviorVersion::latest())
            .build();
        let client = aws_sdk_secretsmanager::Client::from_conf(config);
        let provider = AwsSecretsManagerSecretProvider::new(client, "openflows/staging").unwrap();
        assert_eq!(
            provider
                .canonical_reference("coder-operator-token")
                .unwrap(),
            "openflows/staging/coder-operator-token"
        );
        assert_eq!(
            provider
                .canonical_reference("openflows/staging/coder-operator-token")
                .unwrap(),
            "openflows/staging/coder-operator-token"
        );
        assert!(provider
            .canonical_reference("openflows/production/key")
            .is_err());
        assert!(provider
            .canonical_reference("arn:aws:secretsmanager:example")
            .is_err());
        assert!(provider.canonical_reference("../outside").is_err());
    }

    #[tokio::test]
    async fn aws_provider_round_trips_binary_secrets_without_a_live_account() {
        use axum::{extract::State, http::HeaderMap, routing::any, Json, Router};
        use serde_json::{json, Value};
        use std::{
            collections::HashMap,
            sync::{Arc, Mutex},
        };

        async fn api(
            State(values): State<Arc<Mutex<HashMap<String, String>>>>,
            headers: HeaderMap,
            body: axum::body::Bytes,
        ) -> Json<Value> {
            let body: Value = serde_json::from_slice(&body).unwrap();
            let action = headers
                .get("x-amz-target")
                .and_then(|value| value.to_str().ok())
                .unwrap_or("")
                .rsplit('.')
                .next()
                .unwrap_or("");
            let mut values = values.lock().unwrap();
            match action {
                "CreateSecret" => {
                    let name = body["Name"].as_str().unwrap().to_owned();
                    values.insert(name.clone(), body["SecretBinary"].as_str().unwrap().into());
                    Json(
                        json!({"ARN": format!("arn:aws:secretsmanager:us-east-1:123456789012:secret:{name}-abcdef"), "Name": name}),
                    )
                }
                "GetSecretValue" => {
                    let name = body["SecretId"].as_str().unwrap();
                    Json(json!({"SecretBinary": values.get(name).unwrap()}))
                }
                "PutSecretValue" => {
                    let name = body["SecretId"].as_str().unwrap();
                    values.insert(name.into(), body["SecretBinary"].as_str().unwrap().into());
                    Json(json!({"Name": name, "VersionId": "00000000-0000-0000-0000-000000000001"}))
                }
                "DeleteSecret" => {
                    let name = body["SecretId"].as_str().unwrap();
                    values.remove(name);
                    Json(json!({"Name": name}))
                }
                _ => panic!("unexpected AWS action {action}"),
            }
        }

        let values = Arc::new(Mutex::new(HashMap::new()));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let app = Router::new().fallback(any(api)).with_state(values);
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let config = aws_sdk_secretsmanager::Config::builder()
            .region(aws_sdk_secretsmanager::config::Region::new("us-east-1"))
            .credentials_provider(aws_sdk_secretsmanager::config::Credentials::new(
                "test-access",
                "test-secret",
                None,
                None,
                "unit-test",
            ))
            .endpoint_url(format!("http://{address}"))
            .behavior_version(aws_sdk_secretsmanager::config::BehaviorVersion::latest())
            .build();
        let provider = AwsSecretsManagerSecretProvider::new(
            aws_sdk_secretsmanager::Client::from_conf(config),
            "openflows/staging",
        )
        .unwrap();
        let reference = provider
            .put("coder-operator-token", b"first")
            .await
            .unwrap();
        assert_eq!(reference, "openflows/staging/coder-operator-token");
        assert_eq!(provider.get(&reference).await.unwrap(), b"first");
        provider.replace(&reference, b"second").await.unwrap();
        assert_eq!(provider.get(&reference).await.unwrap(), b"second");
        provider.delete(&reference).await.unwrap();
        server.abort();
    }
}

/// The interface for storing and retrieving secrets.
#[async_trait]
pub trait SecretProvider: Send + Sync {
    /// Resolve a logical name to the provider's persistent reference without
    /// reading or modifying the secret value.
    fn canonical_reference(&self, reference: &str) -> Result<SecretRef, SecretError> {
        Ok(reference.to_owned())
    }

    /// Store `value` under `name`, returning a stable reference the caller can
    /// persist. If the name already exists, implementations should reject or
    /// require an explicit rotation rather than silently overwrite.
    async fn put(&self, name: &str, value: &[u8]) -> Result<SecretRef, SecretError>;

    /// Replace an existing secret while preserving its stable reference.
    /// Filesystem bind mounts observe the replacement; AWS consumers must
    /// explicitly re-fetch the new AWSCURRENT version.
    async fn replace(&self, ref_name: &str, value: &[u8]) -> Result<(), SecretError>;

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
pub const FILESYSTEM_PROVIDER: &str = "filesystem";
pub const AWS_SECRETS_MANAGER_PROVIDER: &str = "aws-secrets-manager";

/// An AWS Secrets Manager namespace for one OpenFlows deployment. Persisted
/// references are full names, never user-supplied ARNs or cross-prefix names.
#[derive(Debug)]
pub struct AwsSecretsManagerSecretProvider {
    client: aws_sdk_secretsmanager::Client,
    prefix: String,
}

impl AwsSecretsManagerSecretProvider {
    pub fn new(
        client: aws_sdk_secretsmanager::Client,
        prefix: impl Into<String>,
    ) -> Result<Self, SecretError> {
        let prefix = prefix.into();
        let prefix = prefix.trim_matches('/');
        if prefix.is_empty()
            || prefix.len() > 400
            || !prefix
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"/_+=.@-".contains(&b))
        {
            return Err(SecretError::Rejected(
                "invalid AWS Secrets Manager deployment prefix".into(),
            ));
        }
        Ok(Self {
            client,
            prefix: prefix.to_owned(),
        })
    }

    fn name(&self, reference: &str) -> Result<String, SecretError> {
        let leaf = reference
            .strip_prefix(&format!("{}/", self.prefix))
            .unwrap_or(reference);
        if leaf.is_empty()
            || leaf.len() > 100
            || !leaf
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"_+=.@-".contains(&b))
            || leaf == "."
            || leaf == ".."
        {
            return Err(SecretError::Rejected(
                "secret reference must be a name within the deployment prefix".into(),
            ));
        }
        Ok(format!("{}/{leaf}", self.prefix))
    }
}

#[async_trait]
impl SecretProvider for AwsSecretsManagerSecretProvider {
    fn canonical_reference(&self, reference: &str) -> Result<SecretRef, SecretError> {
        self.name(reference)
    }

    async fn put(&self, name: &str, value: &[u8]) -> Result<SecretRef, SecretError> {
        let name = self.name(name)?;
        self.client
            .create_secret()
            .name(&name)
            .secret_binary(aws_sdk_secretsmanager::primitives::Blob::new(value))
            .send()
            .await
            .map_err(|error| {
                if error
                    .as_service_error()
                    .is_some_and(|e| e.is_resource_exists_exception())
                {
                    SecretError::AlreadyExists(name.clone())
                } else {
                    SecretError::Unavailable("AWS Secrets Manager CreateSecret failed".into())
                }
            })?;
        Ok(name)
    }

    async fn replace(&self, ref_name: &str, value: &[u8]) -> Result<(), SecretError> {
        let name = self.name(ref_name)?;
        self.client
            .put_secret_value()
            .secret_id(&name)
            .secret_binary(aws_sdk_secretsmanager::primitives::Blob::new(value))
            .send()
            .await
            .map_err(|error| {
                if error
                    .as_service_error()
                    .is_some_and(|e| e.is_resource_not_found_exception())
                {
                    SecretError::NotFound(name)
                } else {
                    SecretError::Unavailable("AWS Secrets Manager PutSecretValue failed".into())
                }
            })?;
        Ok(())
    }

    async fn get(&self, ref_name: &str) -> Result<Vec<u8>, SecretError> {
        let name = self.name(ref_name)?;
        let response = self
            .client
            .get_secret_value()
            .secret_id(&name)
            .send()
            .await
            .map_err(|error| {
                if error
                    .as_service_error()
                    .is_some_and(|e| e.is_resource_not_found_exception())
                {
                    SecretError::NotFound(name)
                } else {
                    SecretError::Unavailable("AWS Secrets Manager GetSecretValue failed".into())
                }
            })?;
        if let Some(binary) = response.secret_binary() {
            return Ok(binary.as_ref().to_vec());
        }
        if let Some(string) = response.secret_string() {
            return Ok(string.as_bytes().to_vec());
        }
        Err(SecretError::Unavailable(
            "AWS secret has no string or binary value".into(),
        ))
    }

    async fn delete(&self, ref_name: &str) -> Result<(), SecretError> {
        let name = self.name(ref_name)?;
        let result = self
            .client
            .delete_secret()
            .secret_id(&name)
            .recovery_window_in_days(7)
            .send()
            .await;
        match result {
            Ok(_) => Ok(()),
            Err(error)
                if error
                    .as_service_error()
                    .is_some_and(|e| e.is_resource_not_found_exception()) =>
            {
                Err(SecretError::NotFound(name))
            }
            Err(error)
                if error
                    .as_service_error()
                    .is_some_and(|e| e.is_invalid_request_exception()) =>
            {
                // Retrying a cleanup after an interrupted DB update is safe
                // when AWS already scheduled the same secret for deletion.
                match self.client.describe_secret().secret_id(&name).send().await {
                    Ok(secret) if secret.deleted_date().is_some() => Ok(()),
                    _ => Err(SecretError::Unavailable(
                        "AWS Secrets Manager DeleteSecret failed".into(),
                    )),
                }
            }
            Err(_) => Err(SecretError::Unavailable(
                "AWS Secrets Manager DeleteSecret failed".into(),
            )),
        }
    }
}

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

    async fn replace(&self, ref_name: &str, value: &[u8]) -> Result<(), SecretError> {
        let mut map = self.secrets.lock().unwrap();
        let secret = map
            .get_mut(ref_name)
            .ok_or_else(|| SecretError::NotFound(ref_name.to_string()))?;
        use zeroize::Zeroize;
        secret.zeroize();
        *secret = value.to_vec();
        Ok(())
    }

    async fn delete(&self, ref_name: &str) -> Result<(), SecretError> {
        let mut map = self.secrets.lock().unwrap();
        map.remove(ref_name)
            .map(|_| ())
            .ok_or_else(|| SecretError::NotFound(ref_name.to_string()))
    }
}

/// Durable local/Docker secret backend. Each secret is a file under one
/// operator-selected directory (mode 0600 by default); the returned reference
/// is directly mountable by the Docker provisioner. Kubernetes deployments
/// should provide their own Secret-backed adapter rather than sharing this
/// host directory.
///
/// A mounted file keeps the host owner and mode. On Linux the workspace user
/// (uid 1000) is not the host user, so a 0600 runtime credential is unreadable
/// inside the workspace and the agent harness cannot start. `with_file_mode`
/// lets the operator widen the file mode (for example 0644). That is only
/// accepted when the secret directory itself denies group and other access
/// (mode 0700): the files stay unreachable to other host users, and only the
/// container, which bind-mounts the single file, can read it.
#[derive(Debug)]
pub struct FilesystemSecretProvider {
    root: std::path::PathBuf,
    file_mode: u32,
}

/// Default secret file mode: owner read/write only.
pub const DEFAULT_SECRET_FILE_MODE: u32 = 0o600;

/// `OPENFLOWS_SECRET_FILE_MODE` shared by the secret store and the per-workspace
/// delivery directory. Unset keeps each caller's default.
pub fn file_mode_from_env() -> Result<Option<u32>, SecretError> {
    match std::env::var("OPENFLOWS_SECRET_FILE_MODE") {
        Ok(raw) => u32::from_str_radix(raw.trim().trim_start_matches("0o"), 8)
            .map(Some)
            .map_err(|_| {
                SecretError::Rejected(
                    "OPENFLOWS_SECRET_FILE_MODE must be an octal mode such as 0644".into(),
                )
            }),
        Err(_) => Ok(None),
    }
}

impl FilesystemSecretProvider {
    /// Build from `OPENFLOWS_SECRET_DIR`, honouring the optional
    /// `OPENFLOWS_SECRET_FILE_MODE` override for the workspace user.
    pub fn from_env() -> Result<Self, SecretError> {
        let root = std::env::var("OPENFLOWS_SECRET_DIR").map_err(|_| {
            SecretError::Rejected("filesystem secret provider requires OPENFLOWS_SECRET_DIR".into())
        })?;
        let mut provider = Self::new(root)?;
        if let Some(mode) = file_mode_from_env()? {
            provider = provider.with_file_mode(mode)?;
        }
        Ok(provider)
    }

    pub fn new(root: impl Into<std::path::PathBuf>) -> Result<Self, SecretError> {
        let root = root.into();
        std::fs::create_dir_all(&root).map_err(|e| {
            SecretError::Unavailable(format!("cannot create secret directory: {e}"))
        })?;
        let root = root.canonicalize().map_err(|e| {
            SecretError::Unavailable(format!("cannot resolve secret directory: {e}"))
        })?;
        Ok(FilesystemSecretProvider {
            root,
            file_mode: DEFAULT_SECRET_FILE_MODE,
        })
    }

    /// Use `mode` for newly created secret files. The owner must keep
    /// read/write. Any group/other bit requires a secret directory that is
    /// closed to group and other (no `0o077` bits), so widening a file never
    /// exposes it to other host users.
    pub fn with_file_mode(mut self, mode: u32) -> Result<Self, SecretError> {
        if mode > 0o777 || mode & 0o600 != 0o600 {
            return Err(SecretError::Rejected(
                "secret file mode must be at most 0777 and keep owner read/write".into(),
            ));
        }
        #[cfg(unix)]
        if mode & 0o077 != 0 {
            use std::os::unix::fs::PermissionsExt;
            let dir_mode = std::fs::metadata(&self.root)
                .map_err(|e| {
                    SecretError::Unavailable(format!("cannot inspect secret directory: {e}"))
                })?
                .permissions()
                .mode();
            if dir_mode & 0o077 != 0 {
                return Err(SecretError::Rejected(
                    "a secret file mode open to group or other requires the secret directory to be mode 0700".into(),
                ));
            }
        }
        self.file_mode = mode;
        Ok(self)
    }

    fn path(&self, reference: &str) -> Result<std::path::PathBuf, SecretError> {
        let candidate = std::path::Path::new(reference);
        let path = if candidate.is_absolute() {
            candidate.to_path_buf()
        } else {
            if reference.is_empty()
                || reference.contains('/')
                || reference.contains('\\')
                || reference == "."
                || reference == ".."
            {
                return Err(SecretError::Rejected("invalid secret reference".into()));
            }
            self.root.join(reference)
        };
        if !path.starts_with(&self.root) {
            return Err(SecretError::Rejected(
                "secret reference escapes root".into(),
            ));
        }
        Ok(path)
    }
}

#[async_trait]
impl SecretProvider for FilesystemSecretProvider {
    fn canonical_reference(&self, reference: &str) -> Result<SecretRef, SecretError> {
        Ok(self.path(reference)?.to_string_lossy().into_owned())
    }

    async fn put(&self, name: &str, value: &[u8]) -> Result<SecretRef, SecretError> {
        use std::io::Write;
        let path = self.path(name)?;
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            // Create closed, then set the exact mode: the umask would
            // otherwise silently trim a wider requested mode.
            options.mode(0o600);
        }
        let mut file = options.open(&path).map_err(|e| match e.kind() {
            std::io::ErrorKind::AlreadyExists => SecretError::AlreadyExists(name.into()),
            _ => SecretError::Unavailable(format!("cannot create secret: {e}")),
        })?;
        #[cfg(unix)]
        if self.file_mode != 0o600 {
            use std::os::unix::fs::PermissionsExt;
            file.set_permissions(std::fs::Permissions::from_mode(self.file_mode))
                .map_err(|e| SecretError::Unavailable(format!("cannot set secret mode: {e}")))?;
        }
        file.write_all(value)
            .and_then(|_| file.sync_all())
            .map_err(|e| SecretError::Unavailable(format!("cannot write secret: {e}")))?;
        Ok(path.to_string_lossy().into_owned())
    }

    async fn get(&self, ref_name: &str) -> Result<Vec<u8>, SecretError> {
        let path = self.path(ref_name)?;
        std::fs::read(&path).map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => SecretError::NotFound(ref_name.into()),
            _ => SecretError::Unavailable(format!("cannot read secret: {e}")),
        })
    }

    async fn replace(&self, ref_name: &str, value: &[u8]) -> Result<(), SecretError> {
        use std::io::Write;
        let path = self.path(ref_name)?;
        // Preserve the inode so an existing Docker bind mount observes the
        // new bytes. The directory itself is not mounted into the workload.
        let mut options = std::fs::OpenOptions::new();
        options.write(true).truncate(true);
        let mut file = options.open(&path).map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => SecretError::NotFound(ref_name.into()),
            _ => SecretError::Unavailable(format!("cannot replace secret: {e}")),
        })?;
        file.write_all(value)
            .and_then(|_| file.sync_all())
            .map_err(|e| SecretError::Unavailable(format!("cannot replace secret: {e}")))
    }

    async fn delete(&self, ref_name: &str) -> Result<(), SecretError> {
        let path = self.path(ref_name)?;
        std::fs::remove_file(&path).map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => SecretError::NotFound(ref_name.into()),
            _ => SecretError::Unavailable(format!("cannot delete secret: {e}")),
        })
    }
}

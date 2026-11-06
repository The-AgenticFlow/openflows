//! Delivers a workspace's runtime credential bundle, separate from the durable store.
//! Templates receive only the returned handle (file path or Secret name), never the bundle.

use crate::error::ManagerError;
use crate::id::{OrganizationId, TenantId, WorkspaceId};
use async_trait::async_trait;
use k8s_openapi::api::core::v1::Secret;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta;
use k8s_openapi::ByteString;
use kube::api::{DeleteParams, Patch, PatchParams};
use kube::{Api, Client};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Host directory for per-workspace bundle files on the Docker backend.
pub const DELIVERY_DIR_ENV: &str = "OPENFLOWS_RUNTIME_DELIVERY_DIR";
/// Prefix of the per-organization workspace namespace on the Kubernetes backend.
pub const NAMESPACE_PREFIX_ENV: &str = "OPENFLOWS_K8S_NAMESPACE_PREFIX";
const DEFAULT_NAMESPACE_PREFIX: &str = "openflows-";
/// Key of the bundle inside the Secret; mounted as `runtime.json`.
pub const SECRET_KEY: &str = "runtime.json";
const FIELD_MANAGER: &str = "openflows-manager";

/// The workspace a bundle is delivered to; names and labels derive from these IDs only.
#[derive(Debug, Clone, Copy)]
pub struct DeliveryTarget {
    pub organization_id: OrganizationId,
    pub tenant_id: TenantId,
    pub workspace_id: WorkspaceId,
}

/// Makes a bundle available to one workspace. Both calls are idempotent for retries.
#[async_trait]
pub trait SecretDelivery: Send + Sync {
    /// Deliver `bundle`, replacing any earlier copy, and return the template handle.
    async fn deliver(
        &self,
        target: &DeliveryTarget,
        store_ref: &str,
        bundle: &[u8],
    ) -> Result<String, ManagerError>;

    /// Remove the delivered copy; a missing copy is not an error.
    async fn remove(&self, target: &DeliveryTarget) -> Result<(), ManagerError>;
}

/// The store reference is already a host file path (filesystem store, local Docker).
#[derive(Debug, Default)]
pub struct StoreReferenceDelivery;

#[async_trait]
impl SecretDelivery for StoreReferenceDelivery {
    async fn deliver(
        &self,
        _target: &DeliveryTarget,
        store_ref: &str,
        _bundle: &[u8],
    ) -> Result<String, ManagerError> {
        Ok(store_ref.to_string())
    }

    async fn remove(&self, _target: &DeliveryTarget) -> Result<(), ManagerError> {
        Ok(())
    }
}

/// One restricted bundle file per workspace under a host directory (Docker).
#[derive(Debug)]
pub struct FileDelivery {
    root: PathBuf,
    mode: u32,
}

impl FileDelivery {
    pub fn new(root: impl Into<PathBuf>, mode: u32) -> Result<Self, ManagerError> {
        let root = root.into();
        if !root.is_absolute() {
            return Err(ManagerError::Config(format!(
                "{DELIVERY_DIR_ENV} must be an absolute path"
            )));
        }
        if mode > 0o777 || mode & 0o600 != 0o600 {
            return Err(ManagerError::Config(
                "runtime secret file mode must be at most 0777 and keep owner read/write".into(),
            ));
        }
        std::fs::create_dir_all(&root)
            .map_err(|e| ManagerError::Config(format!("cannot create {DELIVERY_DIR_ENV}: {e}")))?;
        // Same rule as the filesystem store: a widened file needs a closed directory.
        if mode & 0o077 != 0 {
            use std::os::unix::fs::PermissionsExt;
            let dir_mode = std::fs::metadata(&root)
                .map_err(|e| {
                    ManagerError::Config(format!("cannot inspect {DELIVERY_DIR_ENV}: {e}"))
                })?
                .permissions()
                .mode();
            if dir_mode & 0o077 != 0 {
                return Err(ManagerError::Config(format!(
                    "a runtime secret file mode open to group or other requires {DELIVERY_DIR_ENV} to be mode 0700"
                )));
            }
        }
        Ok(Self { root, mode })
    }

    fn path(&self, target: &DeliveryTarget) -> PathBuf {
        self.root
            .join(format!("workspace-runtime-{}.json", target.workspace_id))
    }
}

#[async_trait]
impl SecretDelivery for FileDelivery {
    async fn deliver(
        &self,
        target: &DeliveryTarget,
        _store_ref: &str,
        bundle: &[u8],
    ) -> Result<String, ManagerError> {
        let path = self.path(target);
        let (dest, mode, bundle) = (path.clone(), self.mode, bundle.to_vec());
        tokio::task::spawn_blocking(move || write_private(&dest, mode, &bundle))
            .await
            .map_err(|e| ManagerError::Config(format!("runtime secret delivery failed: {e}")))?
            .map_err(|e| ManagerError::Config(format!("cannot deliver runtime secret: {e}")))?;
        Ok(path.to_string_lossy().into_owned())
    }

    async fn remove(&self, target: &DeliveryTarget) -> Result<(), ManagerError> {
        match tokio::fs::remove_file(self.path(target)).await {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(ManagerError::Config(format!(
                "cannot remove delivered runtime secret: {e}"
            ))),
        }
    }
}

// Write to a sibling file then rename, so a reader never sees a partial bundle.
fn write_private(dest: &Path, mode: u32, bundle: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    // Unique per call so concurrent delivers to one workspace cannot collide.
    let tmp = dest.with_extension(format!("{}.tmp", uuid::Uuid::new_v4().simple()));
    let _ = std::fs::remove_file(&tmp);
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&tmp)?;
    file.write_all(bundle)?;
    file.sync_all()?;
    // Wider than 0600 only when the operator opts in for the workspace user.
    std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(mode))?;
    std::fs::rename(&tmp, dest)
}

/// One labelled Secret per workspace in its organization's namespace. The Coder
/// provisioner cannot read Secrets; it is handed only the Secret name.
#[derive(Clone)]
pub struct KubernetesSecretDelivery {
    client: Client,
    namespace_prefix: String,
}

impl KubernetesSecretDelivery {
    pub fn new(client: Client, namespace_prefix: impl Into<String>) -> Self {
        Self {
            client,
            namespace_prefix: namespace_prefix.into(),
        }
    }

    /// Connect with the in-cluster or kubeconfig identity.
    pub async fn connect(namespace_prefix: String) -> Result<Self, ManagerError> {
        let client = Client::try_default().await.map_err(|e| {
            ManagerError::Config(format!(
                "kubernetes runtime secret delivery needs cluster access: {e}"
            ))
        })?;
        Ok(Self::new(client, namespace_prefix))
    }

    fn api(&self, target: &DeliveryTarget) -> Api<Secret> {
        let namespace = format!("{}{}", self.namespace_prefix, target.organization_id);
        Api::namespaced(self.client.clone(), &namespace)
    }
}

/// Name of a workspace's runtime Secret; also the template parameter value.
pub fn secret_name(workspace_id: WorkspaceId) -> String {
    format!("openflows-runtime-{workspace_id}")
}

/// Pure so the name, labels and key are unit-tested without a cluster.
pub fn build_secret(target: &DeliveryTarget, bundle: &[u8]) -> Secret {
    let labels: BTreeMap<String, String> = [
        ("app.kubernetes.io/managed-by", FIELD_MANAGER.to_string()),
        (
            "openflows.io/organization",
            target.organization_id.to_string(),
        ),
        ("openflows.io/tenant", target.tenant_id.to_string()),
        ("openflows.io/workspace", target.workspace_id.to_string()),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v))
    .collect();
    Secret {
        metadata: ObjectMeta {
            name: Some(secret_name(target.workspace_id)),
            labels: Some(labels),
            ..Default::default()
        },
        type_: Some("Opaque".into()),
        data: Some([(SECRET_KEY.to_string(), ByteString(bundle.to_vec()))].into()),
        ..Default::default()
    }
}

#[async_trait]
impl SecretDelivery for KubernetesSecretDelivery {
    async fn deliver(
        &self,
        target: &DeliveryTarget,
        _store_ref: &str,
        bundle: &[u8],
    ) -> Result<String, ManagerError> {
        let name = secret_name(target.workspace_id);
        // Server-side apply makes create, retry and rotation one idempotent call.
        self.api(target)
            .patch(
                &name,
                &PatchParams::apply(FIELD_MANAGER).force(),
                &Patch::Apply(&build_secret(target, bundle)),
            )
            .await
            .map_err(|e| ManagerError::Config(format!("cannot deliver runtime secret: {e}")))?;
        Ok(name)
    }

    async fn remove(&self, target: &DeliveryTarget) -> Result<(), ManagerError> {
        match self
            .api(target)
            .delete(&secret_name(target.workspace_id), &DeleteParams::default())
            .await
        {
            Ok(_) => Ok(()),
            Err(kube::Error::Api(status)) if status.code == 404 => Ok(()),
            Err(e) => Err(ManagerError::Config(format!(
                "cannot remove delivered runtime secret: {e}"
            ))),
        }
    }
}

/// Pick the adapter for the workspace backend. Docker with a non-filesystem store
/// must name a delivery directory, since its references (an ARN) are not host paths.
pub async fn resolve_delivery(
    backend: &str,
    secret_provider: &str,
) -> Result<std::sync::Arc<dyn SecretDelivery>, ManagerError> {
    let dir = std::env::var(DELIVERY_DIR_ENV)
        .ok()
        .filter(|v| !v.is_empty());
    if !matches!(backend, "docker" | "kubernetes") {
        return Err(ManagerError::Config(format!(
            "provisioner backend '{backend}' is not supported for runtime secret delivery \
             (expected docker or kubernetes)"
        )));
    }
    match (backend, dir) {
        ("kubernetes", _) => {
            let prefix = std::env::var(NAMESPACE_PREFIX_ENV)
                .unwrap_or_else(|_| DEFAULT_NAMESPACE_PREFIX.to_string());
            Ok(std::sync::Arc::new(
                KubernetesSecretDelivery::connect(prefix).await?,
            ))
        }
        (_, Some(dir)) => {
            let mode = match std::env::var("OPENFLOWS_SECRET_FILE_MODE") {
                Ok(raw) => {
                    u32::from_str_radix(raw.trim().trim_start_matches("0o"), 8).map_err(|_| {
                        ManagerError::Config(
                            "OPENFLOWS_SECRET_FILE_MODE must be an octal mode such as 0644".into(),
                        )
                    })?
                }
                Err(_) => 0o600,
            };
            Ok(std::sync::Arc::new(FileDelivery::new(dir, mode)?))
        }
        (_, None)
            if secret_provider == crate::secrets::FILESYSTEM_PROVIDER
                || secret_provider == crate::secrets::IN_MEMORY_PROVIDER =>
        {
            Ok(std::sync::Arc::new(StoreReferenceDelivery))
        }
        _ => Err(ManagerError::Config(format!(
            "secret provider '{secret_provider}' with the docker backend needs {DELIVERY_DIR_ENV} \
             (an absolute host directory for per-workspace bundle files)"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    fn target() -> DeliveryTarget {
        DeliveryTarget {
            organization_id: OrganizationId::new(),
            tenant_id: TenantId::new(),
            workspace_id: WorkspaceId::new(),
        }
    }

    #[test]
    fn secret_carries_scope_labels_and_single_key() {
        let t = target();
        let secret = build_secret(&t, b"{\"a\":1}");
        let labels = secret.metadata.labels.unwrap();
        assert_eq!(labels["openflows.io/workspace"], t.workspace_id.to_string());
        assert_eq!(labels["openflows.io/tenant"], t.tenant_id.to_string());
        assert_eq!(
            labels["openflows.io/organization"],
            t.organization_id.to_string()
        );
        assert_eq!(secret.metadata.name.unwrap(), secret_name(t.workspace_id));
        let data = secret.data.unwrap();
        assert_eq!(data.len(), 1);
        assert_eq!(data[SECRET_KEY].0, b"{\"a\":1}");
    }

    #[tokio::test]
    async fn file_delivery_is_private_replaceable_and_idempotent_to_remove() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let d = FileDelivery::new(dir.path(), 0o600).unwrap();
        let t = target();
        let path = d.deliver(&t, "arn:aws:x", b"one").await.unwrap();
        assert!(path.starts_with(dir.path().to_str().unwrap()));
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        // Rotation or rebuild replaces the copy at the same path.
        assert_eq!(d.deliver(&t, "x", b"two").await.unwrap(), path);
        assert_eq!(std::fs::read(&path).unwrap(), b"two");
        d.remove(&t).await.unwrap();
        d.remove(&t).await.unwrap();
        assert!(!Path::new(&path).exists());
    }

    #[tokio::test]
    async fn file_delivery_separates_workspaces() {
        let dir = tempfile::tempdir().unwrap();
        let d = FileDelivery::new(dir.path(), 0o600).unwrap();
        let (a, b) = (target(), target());
        let pa = d.deliver(&a, "", b"a").await.unwrap();
        let pb = d.deliver(&b, "", b"b").await.unwrap();
        assert_ne!(pa, pb);
        d.remove(&a).await.unwrap();
        assert_eq!(std::fs::read(&pb).unwrap(), b"b");
    }

    #[test]
    fn widened_mode_needs_a_closed_directory() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(FileDelivery::new(dir.path(), 0o644).is_err());
        assert!(FileDelivery::new(dir.path(), 0o600).is_ok());
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        assert!(FileDelivery::new(dir.path(), 0o644).is_ok());
        assert!(FileDelivery::new(dir.path(), 0o400).is_err());
        assert!(FileDelivery::new(dir.path(), 0o1777).is_err());
    }

    type Seen = Arc<Mutex<Vec<(String, String, Vec<u8>)>>>;

    /// A Kubernetes client whose API server replies with a fixed status and body
    /// and records (method, path and query, request body) for each call.
    fn mock_cluster(status: u16, reply: &'static str) -> (KubernetesSecretDelivery, Seen) {
        let seen: Seen = Arc::default();
        let log = seen.clone();
        let service = tower::service_fn(move |req: http::Request<kube::client::Body>| {
            let log = log.clone();
            async move {
                let (parts, body) = req.into_parts();
                let bytes = body.collect_bytes().await.unwrap().to_vec();
                log.lock()
                    .unwrap()
                    .push((parts.method.to_string(), parts.uri.to_string(), bytes));
                Ok::<_, std::convert::Infallible>(
                    http::Response::builder()
                        .status(status)
                        .body(kube::client::Body::from(reply.as_bytes().to_vec()))
                        .unwrap(),
                )
            }
        });
        (
            KubernetesSecretDelivery::new(Client::new(service, "default"), "openflows-"),
            seen,
        )
    }

    const SECRET_REPLY: &str = r#"{"apiVersion":"v1","kind":"Secret","metadata":{"name":"x"}}"#;
    const NOT_FOUND: &str = r#"{"apiVersion":"v1","kind":"Status","status":"Failure","message":"gone","reason":"NotFound","code":404,"metadata":{}}"#;
    const BOOM: &str = r#"{"apiVersion":"v1","kind":"Status","status":"Failure","message":"boom","reason":"InternalError","code":500,"metadata":{}}"#;

    #[tokio::test]
    async fn kubernetes_delivery_applies_one_secret_in_the_organization_namespace() {
        let (d, seen) = mock_cluster(200, SECRET_REPLY);
        let t = target();
        let handle = d.deliver(&t, "arn:aws:x", b"bundle").await.unwrap();
        assert_eq!(handle, secret_name(t.workspace_id));
        let calls = seen.lock().unwrap();
        assert_eq!(calls.len(), 1);
        let (method, uri, body) = &calls[0];
        assert_eq!(method, "PATCH");
        let path = format!(
            "/api/v1/namespaces/openflows-{}/secrets/{}",
            t.organization_id, handle
        );
        assert!(uri.starts_with(&path), "{uri}");
        assert!(uri.contains("fieldManager=openflows-manager") && uri.contains("force=true"));
        let sent: serde_json::Value = serde_json::from_slice(body).unwrap();
        assert_eq!(
            sent["metadata"]["labels"]["openflows.io/workspace"],
            t.workspace_id.to_string()
        );
        // Opaque bytes travel base64-encoded; the store reference never leaves Manager.
        assert_eq!(sent["data"][SECRET_KEY], "YnVuZGxl");
        assert!(!String::from_utf8_lossy(body).contains("arn:aws"));
    }

    #[tokio::test]
    async fn kubernetes_delivery_surfaces_api_failures() {
        let (d, _) = mock_cluster(500, BOOM);
        assert!(d.deliver(&target(), "", b"x").await.is_err());
    }

    #[tokio::test]
    async fn kubernetes_remove_deletes_the_workspace_secret_and_tolerates_missing() {
        let t = target();
        let (d, seen) = mock_cluster(404, NOT_FOUND);
        d.remove(&t).await.unwrap();
        d.remove(&t).await.unwrap();
        {
            let calls = seen.lock().unwrap();
            assert_eq!(calls.len(), 2);
            assert_eq!(calls[0].0, "DELETE");
            assert!(calls[0].1.starts_with(&format!(
                "/api/v1/namespaces/openflows-{}/secrets/{}",
                t.organization_id,
                secret_name(t.workspace_id)
            )));
        }
        let (failing, _) = mock_cluster(500, BOOM);
        assert!(
            failing.remove(&t).await.is_err(),
            "real errors are not hidden"
        );
    }

    #[tokio::test]
    async fn unknown_backend_fails_instead_of_taking_the_docker_path() {
        let err = resolve_delivery("kubernetse", "aws-secrets-manager").await;
        assert!(err.err().unwrap().to_string().contains("not supported"));
    }

    #[test]
    fn file_delivery_rejects_relative_directory() {
        assert!(FileDelivery::new("relative/dir", 0o600).is_err());
    }
}

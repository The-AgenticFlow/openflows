//! Tenant isolation on the Kubernetes workspace backend.
//!
//! Each OpenFlows organization gets one namespace. The Manager walls it with a
//! pod security level, a quota and network rules that deny everything by default
//! and then allow only DNS, the platform services, the public internet and
//! traffic inside one tenant. Pods must carry the [`manifests::TENANT_LABEL`]
//! label; an unlabelled pod can reach nothing but DNS and the public internet.
//!
//! In hosted mode the Manager also proves, at startup, that the cluster really
//! enforces these rules ([`probe`]) and refuses to start when it does not.

pub mod manifests;
mod probe;

use crate::config::{ManagerConfig, Mode};
use crate::error::ManagerError;
use crate::id::{OrganizationId, TenantId};
use async_trait::async_trait;
use k8s_openapi::api::authorization::v1::SubjectAccessReview;
use k8s_openapi::api::core::v1::{LimitRange, Namespace, ResourceQuota, ServiceAccount};
use k8s_openapi::api::networking::v1::NetworkPolicy;
use kube::api::{DeleteParams, Patch, PatchParams, PostParams};
use kube::{Api, Client};
use serde::de::DeserializeOwned;
use serde::Serialize;
use std::fmt::Debug;
use std::sync::Arc;

pub use probe::run as run_probe;

const FIELD_MANAGER: &str = "openflows-manager";

/// Connect with the in-cluster or kubeconfig identity. The build holds two TLS crypto
/// providers and rustls will not choose between them, so this installs `ring` first.
pub async fn cluster_client() -> Result<Client, kube::Error> {
    let _ = rustls::crypto::ring::default_provider().install_default();
    Client::try_default().await
}

/// Namespace of the platform services (Manager, Coder, Redis) pods may reach.
pub const PLATFORM_NAMESPACE_ENV: &str = "OPENFLOWS_K8S_PLATFORM_NAMESPACE";
/// Comma-separated TCP ports of those services.
pub const PLATFORM_PORTS_ENV: &str = "OPENFLOWS_K8S_PLATFORM_EGRESS_PORTS";
/// Comma-separated CIDRs of platform services outside the cluster, such as a managed Redis.
pub const PLATFORM_CIDRS_ENV: &str = "OPENFLOWS_K8S_PLATFORM_EGRESS_CIDRS";
/// `key=value` label of the Coder provisioner pods, which get their own policy.
pub const PROVISIONER_SELECTOR_ENV: &str = "OPENFLOWS_K8S_PROVISIONER_SELECTOR";
/// Comma-separated CIDRs of the Kubernetes API, which the provisioner must reach.
pub const API_CIDRS_ENV: &str = "OPENFLOWS_K8S_API_EGRESS_CIDRS";
/// `namespace/name` of the Coder provisioner's service account; it must not be able to read Secrets.
pub const PROVISIONER_ACCOUNT_ENV: &str = "OPENFLOWS_K8S_PROVISIONER_SERVICE_ACCOUNT";
/// Image with `nc` and `sh` used by the readiness probe.
pub const PROBE_IMAGE_ENV: &str = "OPENFLOWS_K8S_PROBE_IMAGE";

const DEFAULT_PLATFORM_NAMESPACE: &str = "openflows";
const DEFAULT_PLATFORM_PORTS: [u16; 3] = [3002, 6379, 7080];
const DEFAULT_PROBE_IMAGE: &str = "docker.io/library/busybox:1.37.0";
/// `(resource, environment variable, default)`.
const QUOTA_SETTINGS: [(&str, &str, &str); 7] = [
    ("pods", "OPENFLOWS_K8S_QUOTA_PODS", "50"),
    ("requests.cpu", "OPENFLOWS_K8S_QUOTA_REQUESTS_CPU", "20"),
    (
        "requests.memory",
        "OPENFLOWS_K8S_QUOTA_REQUESTS_MEMORY",
        "64Gi",
    ),
    ("limits.cpu", "OPENFLOWS_K8S_QUOTA_LIMITS_CPU", "40"),
    (
        "limits.memory",
        "OPENFLOWS_K8S_QUOTA_LIMITS_MEMORY",
        "128Gi",
    ),
    ("persistentvolumeclaims", "OPENFLOWS_K8S_QUOTA_PVCS", "50"),
    ("requests.storage", "OPENFLOWS_K8S_QUOTA_STORAGE", "500Gi"),
];

/// Operator settings for the walls around organization namespaces.
#[derive(Debug, Clone)]
pub struct IsolationSettings {
    pub namespace_prefix: String,
    pub platform_namespace: String,
    pub platform_ports: Vec<u16>,
    pub platform_cidrs: Vec<String>,
    pub provisioner_selector: Option<(String, String)>,
    pub api_cidrs: Vec<String>,
    /// Namespace and name of the provisioner's service account.
    pub provisioner_account: Option<(String, String)>,
    pub probe_image: String,
    /// Resource name and amount of every quota line.
    pub quota: Vec<(String, String)>,
}

impl Default for IsolationSettings {
    fn default() -> Self {
        Self {
            namespace_prefix: crate::runtime::delivery::DEFAULT_NAMESPACE_PREFIX.to_string(),
            platform_namespace: DEFAULT_PLATFORM_NAMESPACE.to_string(),
            platform_ports: DEFAULT_PLATFORM_PORTS.to_vec(),
            platform_cidrs: Vec::new(),
            provisioner_selector: None,
            api_cidrs: Vec::new(),
            provisioner_account: None,
            probe_image: DEFAULT_PROBE_IMAGE.to_string(),
            quota: QUOTA_SETTINGS
                .iter()
                .map(|(resource, _, default)| (resource.to_string(), default.to_string()))
                .collect(),
        }
    }
}

impl IsolationSettings {
    pub fn from_env() -> Result<Self, ManagerError> {
        Self::from_lookup(|name| std::env::var(name).ok().filter(|v| !v.trim().is_empty()))
    }

    fn from_lookup(get: impl Fn(&str) -> Option<String>) -> Result<Self, ManagerError> {
        let mut settings = Self::default();
        if let Some(prefix) = get(crate::runtime::delivery::NAMESPACE_PREFIX_ENV) {
            settings.namespace_prefix = valid_prefix(prefix)?;
        }
        if let Some(namespace) = get(PLATFORM_NAMESPACE_ENV) {
            settings.platform_namespace = namespace;
        }
        if let Some(ports) = get(PLATFORM_PORTS_ENV) {
            settings.platform_ports = parse_ports(&ports)?;
        }
        if let Some(cidrs) = get(PLATFORM_CIDRS_ENV) {
            settings.platform_cidrs = parse_cidrs(PLATFORM_CIDRS_ENV, &cidrs)?;
        }
        if let Some(cidrs) = get(API_CIDRS_ENV) {
            settings.api_cidrs = parse_cidrs(API_CIDRS_ENV, &cidrs)?;
        }
        if let Some(selector) = get(PROVISIONER_SELECTOR_ENV) {
            settings.provisioner_selector = Some(parse_selector(&selector)?);
        }
        if let Some(account) = get(PROVISIONER_ACCOUNT_ENV) {
            settings.provisioner_account = Some(parse_account(&account)?);
        }
        if let Some(image) = get(PROBE_IMAGE_ENV) {
            settings.probe_image = image;
        }
        for (slot, (_, var, _)) in settings.quota.iter_mut().zip(QUOTA_SETTINGS) {
            if let Some(amount) = get(var) {
                if !amount
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '.')
                {
                    return Err(ManagerError::Config(format!(
                        "{var} must be a Kubernetes quantity such as 20 or 64Gi"
                    )));
                }
                slot.1 = amount;
            }
        }
        Ok(settings)
    }
}

/// A prefix that keeps `<prefix><organization id>` a valid namespace name.
fn valid_prefix(prefix: String) -> Result<String, ManagerError> {
    let valid = prefix.len() <= 20
        && prefix.ends_with('-')
        && prefix.starts_with(|c: char| c.is_ascii_lowercase())
        && prefix
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
    if valid {
        Ok(prefix)
    } else {
        Err(ManagerError::Config(format!(
            "{} must be at most 20 lowercase letters, digits or dashes, start with a letter and end with '-'",
            crate::runtime::delivery::NAMESPACE_PREFIX_ENV
        )))
    }
}

fn parse_ports(value: &str) -> Result<Vec<u16>, ManagerError> {
    let ports: Result<Vec<u16>, _> = value.split(',').map(|p| p.trim().parse::<u16>()).collect();
    match ports {
        Ok(ports) if !ports.is_empty() && !ports.contains(&0) => Ok(ports),
        _ => Err(ManagerError::Config(format!(
            "{PLATFORM_PORTS_ENV} must be comma-separated ports between 1 and 65535"
        ))),
    }
}

fn parse_cidrs(var: &str, value: &str) -> Result<Vec<String>, ManagerError> {
    value
        .split(',')
        .map(|cidr| {
            let cidr = cidr.trim();
            let valid = cidr.split_once('/').is_some_and(|(addr, bits)| {
                let max = match addr.parse::<std::net::IpAddr>() {
                    Ok(std::net::IpAddr::V4(_)) => 32,
                    Ok(std::net::IpAddr::V6(_)) => 128,
                    Err(_) => return false,
                };
                bits.parse::<u8>().is_ok_and(|bits| bits <= max)
            });
            if valid {
                Ok(cidr.to_string())
            } else {
                Err(ManagerError::Config(format!(
                    "{var} must list CIDRs such as 10.1.2.3/32, got '{cidr}'"
                )))
            }
        })
        .collect()
}

fn parse_account(value: &str) -> Result<(String, String), ManagerError> {
    let allowed = |s: &str| {
        !s.is_empty()
            && s.chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-'))
    };
    match value.split_once('/') {
        Some((namespace, name)) if allowed(namespace) && allowed(name) => {
            Ok((namespace.into(), name.into()))
        }
        _ => Err(ManagerError::Config(format!(
            "{PROVISIONER_ACCOUNT_ENV} must look like openflows/coder-provisioner"
        ))),
    }
}

fn parse_selector(value: &str) -> Result<(String, String), ManagerError> {
    let allowed = |s: &str| {
        !s.is_empty()
            && s.chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '/'))
    };
    match value.split_once('=') {
        Some((key, val)) if allowed(key) && allowed(val) => Ok((key.into(), val.into())),
        _ => Err(ManagerError::Config(format!(
            "{PROVISIONER_SELECTOR_ENV} must look like app.kubernetes.io/name=coder-provisioner"
        ))),
    }
}

/// Walls a tenant's namespace before any of its workloads exist.
#[async_trait]
pub trait TenantIsolation: Send + Sync {
    /// Create or repair the organization namespace and the tenant's rules. Idempotent.
    async fn ensure_tenant(
        &self,
        organization_id: OrganizationId,
        tenant_id: TenantId,
    ) -> Result<(), ManagerError>;

    /// Remove the tenant's rules; the organization namespace stays. A missing rule is not an error.
    async fn remove_tenant(
        &self,
        organization_id: OrganizationId,
        tenant_id: TenantId,
    ) -> Result<(), ManagerError>;
}

/// Docker and local backends have no namespaces to wall.
#[derive(Debug, Default)]
pub struct NoIsolation;

#[async_trait]
impl TenantIsolation for NoIsolation {
    async fn ensure_tenant(&self, _: OrganizationId, _: TenantId) -> Result<(), ManagerError> {
        Ok(())
    }

    async fn remove_tenant(&self, _: OrganizationId, _: TenantId) -> Result<(), ManagerError> {
        Ok(())
    }
}

/// Applies the isolation objects to a cluster.
#[derive(Clone)]
pub struct KubernetesIsolation {
    client: Client,
    settings: IsolationSettings,
}

impl KubernetesIsolation {
    pub fn new(client: Client, settings: IsolationSettings) -> Self {
        Self { client, settings }
    }

    /// Fail when the provisioner's service account can read Secrets in the namespace, since
    /// the tenants' runtime credentials live there.
    async fn provisioner_cannot_read_secrets(
        &self,
        namespace: &str,
        account_namespace: &str,
        account: &str,
    ) -> Result<(), ManagerError> {
        let reviews = Api::<SubjectAccessReview>::all(self.client.clone());
        for verb in manifests::SECRET_READ_VERBS {
            let review =
                manifests::secret_access_review(namespace, account_namespace, account, verb)?;
            let answer = reviews
                .create(&PostParams::default(), &review)
                .await
                .map_err(|e| {
                    ManagerError::Config(format!("cannot check provisioner access: {e}"))
                })?;
            if answer.status.is_some_and(|status| status.allowed) {
                return Err(ManagerError::Config(format!(
                    "the provisioner service account {account_namespace}/{account} can {verb} Secrets \
                     in namespace {namespace}; remove that permission so it cannot read tenant credentials"
                )));
            }
        }
        Ok(())
    }
}

async fn apply<K>(api: &Api<K>, name: &str, object: &K) -> Result<(), ManagerError>
where
    K: Clone + DeserializeOwned + Debug + Serialize,
{
    api.patch(
        name,
        &PatchParams::apply(FIELD_MANAGER).force(),
        &Patch::Apply(object),
    )
    .await
    .map(drop)
    .map_err(|e| ManagerError::Config(format!("cannot apply tenant isolation '{name}': {e}")))
}

#[async_trait]
impl TenantIsolation for KubernetesIsolation {
    async fn ensure_tenant(
        &self,
        organization_id: OrganizationId,
        tenant_id: TenantId,
    ) -> Result<(), ManagerError> {
        let settings = &self.settings;
        let name = manifests::namespace_name(&settings.namespace_prefix, organization_id);
        apply(
            &Api::<Namespace>::all(self.client.clone()),
            &name,
            &manifests::namespace(settings, organization_id)?,
        )
        .await?;
        apply(
            &Api::<ResourceQuota>::namespaced(self.client.clone(), &name),
            manifests::QUOTA_NAME,
            &manifests::quota(settings, &name)?,
        )
        .await?;
        apply(
            &Api::<LimitRange>::namespaced(self.client.clone(), &name),
            manifests::LIMIT_RANGE_NAME,
            &manifests::limit_range(&name)?,
        )
        .await?;
        apply(
            &Api::<ServiceAccount>::namespaced(self.client.clone(), &name),
            "default",
            &manifests::default_service_account(&name)?,
        )
        .await?;
        let policies = Api::<NetworkPolicy>::namespaced(self.client.clone(), &name);
        let mut wanted = manifests::base_policies(settings, &name)?;
        wanted.push(manifests::tenant_policy(&name, tenant_id)?);
        for policy in &wanted {
            let policy_name = policy.metadata.name.as_deref().unwrap_or_default();
            apply(&policies, policy_name, policy).await?;
        }
        if let Some((account_namespace, account)) = &settings.provisioner_account {
            self.provisioner_cannot_read_secrets(&name, account_namespace, account)
                .await?;
        }
        tracing::info!(%organization_id, %tenant_id, namespace = %name, "tenant isolation in place");
        Ok(())
    }

    async fn remove_tenant(
        &self,
        organization_id: OrganizationId,
        tenant_id: TenantId,
    ) -> Result<(), ManagerError> {
        let name = manifests::namespace_name(&self.settings.namespace_prefix, organization_id);
        let policies = Api::<NetworkPolicy>::namespaced(self.client.clone(), &name);
        match policies
            .delete(
                &manifests::tenant_policy_name(tenant_id),
                &DeleteParams::default(),
            )
            .await
        {
            Ok(_) => Ok(()),
            Err(kube::Error::Api(status)) if status.code == 404 => Ok(()),
            Err(e) => Err(ManagerError::Config(format!(
                "cannot remove tenant isolation: {e}"
            ))),
        }
    }
}

/// Pick the isolation for the workspace backend. On Kubernetes in hosted mode the
/// cluster must pass the enforcement probe, or the Manager does not start.
pub async fn resolve(config: &ManagerConfig) -> Result<Arc<dyn TenantIsolation>, ManagerError> {
    if config.provisioner_backend != "kubernetes" {
        return Ok(Arc::new(NoIsolation));
    }
    let settings = IsolationSettings::from_env()?;
    let client = cluster_client()
        .await
        .map_err(|e| ManagerError::Config(format!("tenant isolation needs cluster access: {e}")))?;
    if config.mode == Mode::Hosted {
        probe::run(&client, &settings).await?;
    }
    Ok(Arc::new(KubernetesIsolation::new(client, settings)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn settings(vars: &[(&str, &str)]) -> Result<IsolationSettings, ManagerError> {
        let vars: HashMap<String, String> = vars
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        IsolationSettings::from_lookup(|name| vars.get(name).cloned())
    }

    #[test]
    fn defaults_apply_when_nothing_is_set() {
        let settings = settings(&[]).expect("defaults");

        assert_eq!(settings.platform_namespace, "openflows");
        assert_eq!(settings.platform_ports, vec![3002, 6379, 7080]);
        assert!(settings.provisioner_selector.is_none());
    }

    #[test]
    fn operator_values_override_defaults() {
        let settings = settings(&[
            (PLATFORM_NAMESPACE_ENV, "platform"),
            (PLATFORM_PORTS_ENV, "8080, 6380"),
            (PLATFORM_CIDRS_ENV, "10.1.2.3/32,10.9.0.0/16"),
            (PROVISIONER_SELECTOR_ENV, "app=coder-provisioner"),
            (PROVISIONER_ACCOUNT_ENV, "platform/provisioner"),
            ("OPENFLOWS_K8S_QUOTA_PODS", "10"),
        ])
        .expect("settings");

        assert_eq!(settings.platform_namespace, "platform");
        assert_eq!(settings.platform_ports, vec![8080, 6380]);
        assert_eq!(settings.platform_cidrs.len(), 2);
        assert_eq!(
            settings.provisioner_selector,
            Some(("app".into(), "coder-provisioner".into()))
        );
        assert_eq!(settings.quota[0], ("pods".into(), "10".into()));
        assert_eq!(
            settings.provisioner_account,
            Some(("platform".into(), "provisioner".into()))
        );
    }

    #[test]
    fn malformed_values_are_rejected_with_the_variable_name() {
        for (var, value) in [
            (PLATFORM_PORTS_ENV, "0"),
            (PLATFORM_PORTS_ENV, "http"),
            (PLATFORM_CIDRS_ENV, "10.1.2.3"),
            (API_CIDRS_ENV, "10.1.2.3/40"),
            (PROVISIONER_SELECTOR_ENV, "no-equals-sign"),
            (PROVISIONER_ACCOUNT_ENV, "no-namespace"),
            ("OPENFLOWS_K8S_NAMESPACE_PREFIX", "Bad_Prefix-"),
            ("OPENFLOWS_K8S_QUOTA_PODS", "ten; drop"),
        ] {
            let error = settings(&[(var, value)]).expect_err(value).to_string();

            assert!(error.contains(var) || error.contains("PREFIX"), "{error}");
        }
    }
}

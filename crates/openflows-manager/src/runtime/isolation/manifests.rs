//! Kubernetes objects that wall one organization namespace and its tenants.
//!
//! Every builder is pure so names, labels and rules are unit-tested without a
//! cluster. The rules deny all traffic by default and then allow only DNS, the
//! platform services, the public internet and traffic inside one tenant.

use super::IsolationSettings;
use crate::error::ManagerError;
use crate::id::{OrganizationId, TenantId};
use k8s_openapi::api::authorization::v1::SubjectAccessReview;
use k8s_openapi::api::core::v1::{LimitRange, Namespace, ResourceQuota, ServiceAccount};
use k8s_openapi::api::networking::v1::NetworkPolicy;
use serde::de::DeserializeOwned;
use serde_json::{json, Value};

/// Pods that belong to one tenant carry this label; the tenant policy selects on it.
pub const TENANT_LABEL: &str = "openflows.io/tenant";
/// Organization the namespace and its objects belong to.
pub const ORGANIZATION_LABEL: &str = "openflows.io/organization";
const MANAGED_BY_LABEL: &str = "app.kubernetes.io/managed-by";
const MANAGER: &str = "openflows-manager";
/// Private, link-local and carrier-grade NAT ranges. Pod, node and cloud metadata
/// addresses live here, so none of them is reachable through the public rule.
pub const NON_PUBLIC_CIDRS: [&str; 5] = [
    "10.0.0.0/8",
    "172.16.0.0/12",
    "192.168.0.0/16",
    "169.254.0.0/16",
    "100.64.0.0/10",
];

pub const DEFAULT_DENY: &str = "openflows-default-deny";
pub const ALLOW_DNS: &str = "openflows-allow-dns";
pub const ALLOW_PLATFORM: &str = "openflows-allow-platform";
pub const ALLOW_PUBLIC: &str = "openflows-allow-public-egress";
pub const PROVISIONER_POLICY: &str = "openflows-provisioner";
pub const QUOTA_NAME: &str = "openflows-quota";
pub const LIMIT_RANGE_NAME: &str = "openflows-defaults";
/// Verbs the Coder provisioner must not hold on Secrets.
pub const SECRET_READ_VERBS: [&str; 3] = ["get", "list", "watch"];

/// Name of an organization's namespace.
pub fn namespace_name(prefix: &str, organization_id: OrganizationId) -> String {
    format!("{prefix}{organization_id}")
}

/// Name of the policy that lets one tenant's pods talk to each other.
pub fn tenant_policy_name(tenant_id: TenantId) -> String {
    format!("openflows-tenant-{tenant_id}")
}

fn typed<T: DeserializeOwned>(value: Value) -> Result<T, ManagerError> {
    serde_json::from_value(value)
        .map_err(|e| ManagerError::Config(format!("invalid tenant isolation object: {e}")))
}

fn meta(name: &str, namespace: Option<&str>, organization: Option<OrganizationId>) -> Value {
    let mut labels = json!({ MANAGED_BY_LABEL: MANAGER });
    if let Some(org) = organization {
        labels[ORGANIZATION_LABEL] = json!(org.to_string());
    }
    let mut meta = json!({ "name": name, "labels": labels });
    if let Some(ns) = namespace {
        meta["namespace"] = json!(ns);
    }
    meta
}

/// The organization namespace, enforcing the `restricted` Pod Security Standard.
pub fn namespace(
    settings: &IsolationSettings,
    organization_id: OrganizationId,
) -> Result<Namespace, ManagerError> {
    let name = namespace_name(&settings.namespace_prefix, organization_id);
    let mut metadata = meta(&name, None, Some(organization_id));
    for key in ["enforce", "audit", "warn"] {
        metadata["labels"][format!("pod-security.kubernetes.io/{key}")] = json!("restricted");
    }
    metadata["labels"]["pod-security.kubernetes.io/enforce-version"] = json!("latest");
    typed(json!({ "apiVersion": "v1", "kind": "Namespace", "metadata": metadata }))
}

/// Ceiling on what the organization's namespace may consume.
pub fn quota(settings: &IsolationSettings, namespace: &str) -> Result<ResourceQuota, ManagerError> {
    let hard: serde_json::Map<String, Value> = settings
        .quota
        .iter()
        .map(|(resource, amount)| (resource.clone(), json!(amount)))
        .collect();
    typed(json!({
        "apiVersion": "v1", "kind": "ResourceQuota",
        "metadata": meta(QUOTA_NAME, Some(namespace), None),
        "spec": { "hard": hard },
    }))
}

/// Container defaults, so pods without explicit limits still fit the quota.
pub fn limit_range(namespace: &str) -> Result<LimitRange, ManagerError> {
    typed(json!({
        "apiVersion": "v1", "kind": "LimitRange",
        "metadata": meta(LIMIT_RANGE_NAME, Some(namespace), None),
        "spec": { "limits": [{
            "type": "Container",
            "default": { "cpu": "1", "memory": "2Gi" },
            "defaultRequest": { "cpu": "100m", "memory": "256Mi" },
        }]},
    }))
}

/// The namespace's default service account, so pods that name none get no API token.
pub fn default_service_account(namespace: &str) -> Result<ServiceAccount, ManagerError> {
    typed(json!({
        "apiVersion": "v1", "kind": "ServiceAccount",
        "metadata": meta("default", Some(namespace), None),
        "automountServiceAccountToken": false,
    }))
}

/// Asks the API server whether a service account may use `verb` on Secrets in `namespace`.
pub fn secret_access_review(
    namespace: &str,
    account_namespace: &str,
    account: &str,
    verb: &str,
) -> Result<SubjectAccessReview, ManagerError> {
    typed(json!({
        "apiVersion": "authorization.k8s.io/v1", "kind": "SubjectAccessReview",
        "spec": {
            "user": format!("system:serviceaccount:{account_namespace}:{account}"),
            "resourceAttributes": { "namespace": namespace, "verb": verb, "resource": "secrets" },
        },
    }))
}

fn policy(name: &str, namespace: &str, spec: Value) -> Result<NetworkPolicy, ManagerError> {
    typed(json!({
        "apiVersion": "networking.k8s.io/v1", "kind": "NetworkPolicy",
        "metadata": meta(name, Some(namespace), None),
        "spec": spec,
    }))
}

fn tcp_ports(ports: &[u16]) -> Vec<Value> {
    ports
        .iter()
        .map(|port| json!({ "protocol": "TCP", "port": port }))
        .collect()
}

/// Egress peers for the platform services: their namespace plus any external CIDRs.
fn platform_peers(settings: &IsolationSettings) -> Vec<Value> {
    let mut peers = vec![json!({ "namespaceSelector": { "matchLabels": {
        "kubernetes.io/metadata.name": settings.platform_namespace } } })];
    peers.extend(
        settings
            .platform_cidrs
            .iter()
            .map(|cidr| json!({ "ipBlock": { "cidr": cidr } })),
    );
    peers
}

/// Policies every organization namespace gets, whichever tenants it holds.
pub fn base_policies(
    settings: &IsolationSettings,
    namespace: &str,
) -> Result<Vec<NetworkPolicy>, ManagerError> {
    let all_pods = json!({});
    let mut policies = vec![
        policy(
            DEFAULT_DENY,
            namespace,
            json!({ "podSelector": all_pods, "policyTypes": ["Ingress", "Egress"] }),
        )?,
        policy(
            ALLOW_DNS,
            namespace,
            json!({ "podSelector": all_pods, "policyTypes": ["Egress"], "egress": [{
                "to": [{
                    "namespaceSelector": { "matchLabels": { "kubernetes.io/metadata.name": "kube-system" } },
                    "podSelector": { "matchLabels": { "k8s-app": "kube-dns" } },
                }],
                "ports": [{ "protocol": "UDP", "port": 53 }, { "protocol": "TCP", "port": 53 }],
            }]}),
        )?,
        policy(
            ALLOW_PLATFORM,
            namespace,
            json!({ "podSelector": all_pods, "policyTypes": ["Egress"], "egress": [{
                "to": platform_peers(settings),
                "ports": tcp_ports(&settings.platform_ports),
            }]}),
        )?,
        policy(
            ALLOW_PUBLIC,
            namespace,
            json!({ "podSelector": all_pods, "policyTypes": ["Egress"], "egress": [{
                "to": [{ "ipBlock": { "cidr": "0.0.0.0/0", "except": NON_PUBLIC_CIDRS } }],
            }]}),
        )?,
    ];
    if let Some((key, value)) = &settings.provisioner_selector {
        policies.push(provisioner_policy(settings, namespace, key, value)?);
    }
    Ok(policies)
}

/// The provisioner is not tenant workload. It also needs the Kubernetes API,
/// so it gets its own egress policy instead of the workspace rules.
fn provisioner_policy(
    settings: &IsolationSettings,
    namespace: &str,
    key: &str,
    value: &str,
) -> Result<NetworkPolicy, ManagerError> {
    let mut peers = platform_peers(settings);
    peers.extend(
        settings
            .api_cidrs
            .iter()
            .map(|cidr| json!({ "ipBlock": { "cidr": cidr } })),
    );
    policy(
        PROVISIONER_POLICY,
        namespace,
        json!({
            "podSelector": { "matchLabels": { key: value } },
            "policyTypes": ["Egress"],
            "egress": [{ "to": peers }],
        }),
    )
}

/// Lets the pods of one tenant, and only those, reach each other.
pub fn tenant_policy(namespace: &str, tenant_id: TenantId) -> Result<NetworkPolicy, ManagerError> {
    let same_tenant = json!({ "matchLabels": { TENANT_LABEL: tenant_id.to_string() } });
    policy(
        &tenant_policy_name(tenant_id),
        namespace,
        json!({
            "podSelector": same_tenant,
            "policyTypes": ["Ingress", "Egress"],
            "ingress": [{ "from": [{ "podSelector": same_tenant }] }],
            "egress": [{ "to": [{ "podSelector": same_tenant }] }],
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn org() -> OrganizationId {
        OrganizationId::from_uuid(uuid::Uuid::from_u128(1))
    }

    fn tenant(n: u128) -> TenantId {
        TenantId::from_uuid(uuid::Uuid::from_u128(n))
    }

    fn spec(policy: &NetworkPolicy) -> Value {
        serde_json::to_value(policy.spec.as_ref().expect("policy has a spec")).expect("serialize")
    }

    #[test]
    fn namespace_enforces_the_restricted_pod_standard() {
        let ns = namespace(&IsolationSettings::default(), org()).expect("namespace");
        let labels = ns.metadata.labels.expect("labels");

        assert_eq!(
            ns.metadata.name.as_deref(),
            Some("openflows-00000000-0000-0000-0000-000000000001")
        );
        assert_eq!(labels["pod-security.kubernetes.io/enforce"], "restricted");
        assert_eq!(labels[ORGANIZATION_LABEL], org().to_string());
    }

    #[test]
    fn default_deny_selects_every_pod_and_both_directions() {
        let policies = base_policies(&IsolationSettings::default(), "ns").expect("policies");
        let deny = policies
            .iter()
            .find(|p| p.metadata.name.as_deref() == Some(DEFAULT_DENY))
            .expect("default deny");
        let spec = spec(deny);

        assert_eq!(spec["podSelector"], json!({}));
        assert_eq!(spec["policyTypes"], json!(["Ingress", "Egress"]));
        assert!(spec.get("ingress").is_none() && spec.get("egress").is_none());
    }

    #[test]
    fn public_egress_excludes_private_pod_node_and_metadata_ranges() {
        let policies = base_policies(&IsolationSettings::default(), "ns").expect("policies");
        let public = policies
            .iter()
            .find(|p| p.metadata.name.as_deref() == Some(ALLOW_PUBLIC))
            .expect("public egress");
        let spec = spec(public);
        let block = &spec["egress"][0]["to"][0]["ipBlock"];

        assert_eq!(block["cidr"], "0.0.0.0/0");
        for cidr in ["10.0.0.0/8", "192.168.0.0/16", "169.254.0.0/16"] {
            assert!(block["except"]
                .as_array()
                .expect("except list")
                .contains(&json!(cidr)));
        }
    }

    #[test]
    fn tenant_policy_only_admits_the_same_tenant() {
        let policy = tenant_policy("ns", tenant(7)).expect("policy");
        let spec = spec(&policy);
        let label = json!({ "matchLabels": { TENANT_LABEL: tenant(7).to_string() } });

        assert_eq!(spec["podSelector"], label);
        assert_eq!(spec["ingress"][0]["from"][0]["podSelector"], label);
        assert_eq!(spec["egress"][0]["to"][0]["podSelector"], label);
    }

    #[test]
    fn two_tenants_get_different_policy_names() {
        assert_ne!(tenant_policy_name(tenant(1)), tenant_policy_name(tenant(2)));
    }

    #[test]
    fn provisioner_policy_exists_only_when_a_selector_is_configured() {
        let mut settings = IsolationSettings::default();
        let without = base_policies(&settings, "ns").expect("policies").len();
        settings.provisioner_selector = Some(("app".into(), "provisioner".into()));
        settings.api_cidrs = vec!["172.20.0.1/32".into()];

        let with = base_policies(&settings, "ns").expect("policies");

        assert_eq!(with.len(), without + 1);
        let provisioner = with.last().expect("provisioner policy");
        assert_eq!(
            provisioner.metadata.name.as_deref(),
            Some(PROVISIONER_POLICY)
        );
        assert!(spec(provisioner)["egress"][0]["to"]
            .as_array()
            .expect("peers")
            .contains(&json!({ "ipBlock": { "cidr": "172.20.0.1/32" } })));
    }

    #[test]
    fn default_service_account_hands_out_no_api_token() {
        let account = default_service_account("ns").expect("account");

        assert_eq!(account.metadata.name.as_deref(), Some("default"));
        assert_eq!(account.automount_service_account_token, Some(false));
    }

    #[test]
    fn access_review_names_the_service_account_and_the_secrets_resource() {
        let review =
            secret_access_review("org-ns", "platform", "provisioner", "get").expect("review");
        let spec = review.spec;

        assert_eq!(
            spec.user.as_deref(),
            Some("system:serviceaccount:platform:provisioner")
        );
        let attributes = spec.resource_attributes.expect("attributes");
        assert_eq!(attributes.namespace.as_deref(), Some("org-ns"));
        assert_eq!(attributes.resource.as_deref(), Some("secrets"));
        assert_eq!(attributes.verb.as_deref(), Some("get"));
    }

    #[test]
    fn quota_carries_every_configured_limit() {
        let settings = IsolationSettings::default();
        let quota = quota(&settings, "ns").expect("quota");
        let hard = quota.spec.expect("spec").hard.expect("hard limits");

        assert_eq!(hard.len(), settings.quota.len());
        assert!(hard.contains_key("pods"));
    }
}

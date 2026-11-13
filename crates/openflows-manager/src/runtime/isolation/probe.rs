//! Proves that the cluster enforces the tenant rules, not merely accepts them.
//!
//! A cluster whose network plugin ignores NetworkPolicy takes the objects
//! without complaint and then lets every pod talk to every other. Two probes run
//! side by side in throwaway namespaces.
//!
//! The tenant probe applies the real rules and checks three connections to a
//! tenant A server: from tenant A (must work, the control that shows the probe
//! itself is sound), from tenant B and from an unlabelled pod (both must fail).
//!
//! A connection between two pods needs the source's egress rules and the
//! destination's ingress rules to both allow it, so a block alone does not say
//! which half is enforced. The direction probe tests each half on its own: a pod
//! guarded only by an ingress rule must be unreachable from a free pod, and a pod
//! guarded only by an egress rule must be unable to reach a free pod.

use super::{apply, manifests, IsolationSettings};
use crate::error::ManagerError;
use crate::id::TenantId;
use k8s_openapi::api::core::v1::{Namespace, Pod};
use k8s_openapi::api::networking::v1::NetworkPolicy;
use kube::api::{DeleteParams, PostParams};
use kube::{Api, Client};
use serde::de::DeserializeOwned;
use serde_json::json;
use std::fmt::Debug;
use std::time::Duration;
use tokio::time::{sleep, Instant};

const PORT: u16 = 8080;
const SERVER_TIMEOUT: Duration = Duration::from_secs(120);
const CLIENT_TIMEOUT: Duration = Duration::from_secs(90);
const POLL: Duration = Duration::from_secs(2);
const PROBE_LABEL: &str = "openflows.io/probe";
const INGRESS_GUARDED: &str = "ingress-guarded";
const EGRESS_GUARDED: &str = "egress-guarded";
const TENANT_PODS: [&str; 4] = ["server", "same-tenant", "other-tenant", "unlabelled"];
const DIRECTION_PODS: [&str; 5] = [
    "server-free",
    "server-ingress-guarded",
    "free-to-free",
    "free-to-ingress-guarded",
    "egress-guarded-to-free",
];
const DIRECTION_POLICIES: [&str; 2] = [
    "openflows-probe-deny-ingress",
    "openflows-probe-deny-egress",
];

fn fail(message: impl Into<String>) -> ManagerError {
    ManagerError::Config(format!("tenant isolation probe failed: {}", message.into()))
}

/// What each probe connection did: `true` means the client reached the server.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Outcome {
    same_tenant: bool,
    other_tenant: bool,
    unlabelled: bool,
    free_to_free: bool,
    free_to_ingress_guarded: bool,
    egress_guarded_to_free: bool,
}

/// Run both probes. By default each runs in a throwaway namespace of its own that is removed
/// whatever the outcome. With a probe namespace configured they run one after the other in it,
/// and each removes its own pods and rules, so the Manager needs no right to delete namespaces.
pub async fn run(client: &Client, settings: &IsolationSettings) -> Result<(), ManagerError> {
    let (tenants, directions) = match &settings.probe_namespace {
        Some(namespace) => {
            require_namespace(client, namespace).await?;
            let tenants = tenant_probe_in(client, settings, namespace).await;
            let directions = direction_probe_in(client, settings, namespace).await;
            (tenants, directions)
        }
        None => tokio::join!(
            tenant_probe(client, settings),
            direction_probe(client, settings)
        ),
    };
    let (same_tenant, other_tenant, unlabelled) = tenants?;
    let (free_to_free, free_to_ingress_guarded, egress_guarded_to_free) = directions?;
    judge(Outcome {
        same_tenant,
        other_tenant,
        unlabelled,
        free_to_free,
        free_to_ingress_guarded,
        egress_guarded_to_free,
    })?;
    binding_probe(client, settings).await
}

/// The Secret-writer binding rule, in the probe namespace or a throwaway one.
async fn binding_probe(client: &Client, settings: &IsolationSettings) -> Result<(), ManagerError> {
    match &settings.probe_namespace {
        Some(namespace) => super::check_binding_rule(client, settings, namespace).await,
        None => {
            let name = create_namespace(client, settings, "openflows-probe-bind").await?;
            let outcome = super::check_binding_rule(client, settings, &name).await;
            remove_namespace(client, &name).await;
            outcome
        }
    }
}

async fn create_namespace(
    client: &Client,
    settings: &IsolationSettings,
    prefix: &str,
) -> Result<String, ManagerError> {
    let name = format!(
        "{prefix}-{}",
        &uuid::Uuid::new_v4().simple().to_string()[..8]
    );
    let mut labels = manifests::pod_security_labels(settings);
    labels["app.kubernetes.io/managed-by"] = json!("openflows-manager");
    // The Secret-writer binding rule only allows bindings in namespaces labelled as an
    // organization's, so the binding check needs this label to try the allowed binding.
    labels[manifests::ORGANIZATION_LABEL] = json!("probe");
    let namespace: Namespace = serde_json::from_value(json!({
        "apiVersion": "v1", "kind": "Namespace",
        "metadata": { "name": name, "labels": labels },
    }))
    .map_err(|e| fail(e.to_string()))?;
    Api::<Namespace>::all(client.clone())
        .create(&PostParams::default(), &namespace)
        .await
        .map_err(|e| fail(format!("cannot create a probe namespace: {e}")))?;
    Ok(name)
}

async fn remove_namespace(client: &Client, name: &str) {
    if let Err(error) = Api::<Namespace>::all(client.clone())
        .delete(name, &DeleteParams::default())
        .await
    {
        tracing::warn!(namespace = %name, %error, "could not remove the isolation probe namespace");
    }
}

async fn require_namespace(client: &Client, name: &str) -> Result<(), ManagerError> {
    Api::<Namespace>::all(client.clone())
        .get(name)
        .await
        .map(drop)
        .map_err(|e| {
            fail(format!(
                "the probe namespace '{name}' cannot be used ({e}); create it empty, or unset \
                 OPENFLOWS_K8S_PROBE_NAMESPACE to let the probe create its own"
            ))
        })
}

fn new_tenant() -> TenantId {
    TenantId::from_uuid(uuid::Uuid::new_v4())
}

async fn tenant_probe(
    client: &Client,
    settings: &IsolationSettings,
) -> Result<(bool, bool, bool), ManagerError> {
    let name = create_namespace(client, settings, "openflows-probe").await?;
    let outcome = tenant_checks(client, settings, &name, new_tenant(), new_tenant()).await;
    remove_namespace(client, &name).await;
    outcome
}

/// The tenant probe in a namespace that already exists, cleaning up after itself.
async fn tenant_probe_in(
    client: &Client,
    settings: &IsolationSettings,
    namespace: &str,
) -> Result<(bool, bool, bool), ManagerError> {
    let (tenant_a, tenant_b) = (new_tenant(), new_tenant());
    let mut policies = base_policy_names(settings, namespace)?;
    policies.push(manifests::tenant_policy_name(tenant_a));
    policies.push(manifests::tenant_policy_name(tenant_b));
    clean(client, namespace, &TENANT_PODS, &policies).await;
    let outcome = tenant_checks(client, settings, namespace, tenant_a, tenant_b).await;
    clean(client, namespace, &TENANT_PODS, &policies).await;
    outcome
}

/// The direction probe in a namespace that already exists, cleaning up after itself.
async fn direction_probe_in(
    client: &Client,
    settings: &IsolationSettings,
    namespace: &str,
) -> Result<(bool, bool, bool), ManagerError> {
    let policies: Vec<String> = DIRECTION_POLICIES.iter().map(|p| p.to_string()).collect();
    clean(client, namespace, &DIRECTION_PODS, &policies).await;
    let outcome = direction_checks(client, settings, namespace).await;
    clean(client, namespace, &DIRECTION_PODS, &policies).await;
    outcome
}

fn base_policy_names(
    settings: &IsolationSettings,
    namespace: &str,
) -> Result<Vec<String>, ManagerError> {
    Ok(manifests::base_policies(settings, namespace)?
        .into_iter()
        .filter_map(|policy| policy.metadata.name)
        .collect())
}

/// Remove what a probe made, and any leftovers of an earlier run that was cut short.
async fn clean(client: &Client, namespace: &str, pods: &[&str], policies: &[String]) {
    let policy_api = Api::<NetworkPolicy>::namespaced(client.clone(), namespace);
    for name in policies {
        delete_quietly(&policy_api, name, &DeleteParams::default()).await;
    }
    let pod_api = Api::<Pod>::namespaced(client.clone(), namespace);
    for name in pods {
        delete_quietly(&pod_api, name, &DeleteParams::default().grace_period(0)).await;
    }
}

async fn delete_quietly<K>(api: &Api<K>, name: &str, params: &DeleteParams)
where
    K: Clone + DeserializeOwned + Debug,
{
    match api.delete(name, params).await {
        Ok(_) => {}
        Err(kube::Error::Api(status)) if status.code == 404 => {}
        Err(error) => tracing::warn!(%name, %error, "could not remove an isolation probe object"),
    }
}

async fn direction_probe(
    client: &Client,
    settings: &IsolationSettings,
) -> Result<(bool, bool, bool), ManagerError> {
    let name = create_namespace(client, settings, "openflows-probe-dir").await?;
    let outcome = direction_checks(client, settings, &name).await;
    remove_namespace(client, &name).await;
    outcome
}

/// Same-tenant, other-tenant and unlabelled connections to a tenant A server.
async fn tenant_checks(
    client: &Client,
    settings: &IsolationSettings,
    namespace: &str,
    tenant_a: TenantId,
    tenant_b: TenantId,
) -> Result<(bool, bool, bool), ManagerError> {
    let policies = Api::<NetworkPolicy>::namespaced(client.clone(), namespace);
    let mut wanted = manifests::base_policies(settings, namespace)?;
    wanted.push(manifests::tenant_policy(namespace, tenant_a)?);
    wanted.push(manifests::tenant_policy(namespace, tenant_b)?);
    for policy in &wanted {
        apply(
            &policies,
            policy.metadata.name.as_deref().unwrap_or_default(),
            policy,
        )
        .await?;
    }

    let (a, b) = (tenant_a.to_string(), tenant_b.to_string());
    let pods = Api::<Pod>::namespaced(client.clone(), namespace);
    let image = &settings.probe_image;
    let server = pod(
        "server",
        namespace,
        &[(manifests::TENANT_LABEL, &a)],
        image,
        &serve(),
    )?;
    create_pod(&pods, server).await?;
    let server_ip = wait_for_ip(&pods, "server").await?;

    let reach = reach(&server_ip);
    for (name, labels) in [
        ("same-tenant", vec![(manifests::TENANT_LABEL, a.as_str())]),
        ("other-tenant", vec![(manifests::TENANT_LABEL, b.as_str())]),
        ("unlabelled", Vec::new()),
    ] {
        create_pod(&pods, pod(name, namespace, &labels, image, &reach)?).await?;
    }
    tokio::try_join!(
        wait_for_result(&pods, "same-tenant"),
        wait_for_result(&pods, "other-tenant"),
        wait_for_result(&pods, "unlabelled"),
    )
}

/// Free to free (control), free to an ingress-guarded pod and an egress-guarded pod to a free one.
async fn direction_checks(
    client: &Client,
    settings: &IsolationSettings,
    namespace: &str,
) -> Result<(bool, bool, bool), ManagerError> {
    let policies = Api::<NetworkPolicy>::namespaced(client.clone(), namespace);
    for (name, guarded, direction) in [
        ("openflows-probe-deny-ingress", INGRESS_GUARDED, "Ingress"),
        ("openflows-probe-deny-egress", EGRESS_GUARDED, "Egress"),
    ] {
        let policy: NetworkPolicy = serde_json::from_value(json!({
            "apiVersion": "networking.k8s.io/v1", "kind": "NetworkPolicy",
            "metadata": { "name": name, "namespace": namespace },
            "spec": {
                "podSelector": { "matchLabels": { PROBE_LABEL: guarded } },
                "policyTypes": [direction],
            },
        }))
        .map_err(|e| fail(e.to_string()))?;
        apply(&policies, name, &policy).await?;
    }

    let pods = Api::<Pod>::namespaced(client.clone(), namespace);
    let image = &settings.probe_image;
    create_pod(&pods, pod("server-free", namespace, &[], image, &serve())?).await?;
    create_pod(
        &pods,
        pod(
            "server-ingress-guarded",
            namespace,
            &[(PROBE_LABEL, INGRESS_GUARDED)],
            image,
            &serve(),
        )?,
    )
    .await?;
    let (free_ip, guarded_ip) = tokio::try_join!(
        wait_for_ip(&pods, "server-free"),
        wait_for_ip(&pods, "server-ingress-guarded"),
    )?;

    create_pod(
        &pods,
        pod("free-to-free", namespace, &[], image, &reach(&free_ip))?,
    )
    .await?;
    create_pod(
        &pods,
        pod(
            "free-to-ingress-guarded",
            namespace,
            &[],
            image,
            &reach(&guarded_ip),
        )?,
    )
    .await?;
    create_pod(
        &pods,
        pod(
            "egress-guarded-to-free",
            namespace,
            &[(PROBE_LABEL, EGRESS_GUARDED)],
            image,
            &reach(&free_ip),
        )?,
    )
    .await?;
    tokio::try_join!(
        wait_for_result(&pods, "free-to-free"),
        wait_for_result(&pods, "free-to-ingress-guarded"),
        wait_for_result(&pods, "egress-guarded-to-free"),
    )
}

fn serve() -> String {
    format!("while true; do echo ok | nc -l -p {PORT}; done")
}

fn reach(server_ip: &str) -> String {
    format!("echo hi | nc -w 3 {server_ip} {PORT} | grep -q ok")
}

/// Decide from which connections succeeded. Pure so every outcome is tested.
fn judge(outcome: Outcome) -> Result<(), ManagerError> {
    if !outcome.same_tenant {
        return Err(fail(
            "a pod could not reach another pod of its own tenant. Check that the network plugin \
             is healthy and that OPENFLOWS_K8S_PROBE_IMAGE contains nc and sh",
        ));
    }
    if !outcome.free_to_free {
        return Err(fail(
            "a pod with no rules could not reach another pod with no rules. The network plugin \
             looks unhealthy",
        ));
    }
    if outcome.other_tenant {
        return Err(fail(
            "a pod of one tenant reached a pod of another. The cluster does not enforce \
             NetworkPolicy; install a network plugin that does (for example Calico or Cilium) \
             before hosting several tenants",
        ));
    }
    if outcome.unlabelled {
        return Err(fail(
            "a pod without a tenant label reached a tenant pod. The default-deny rule is not \
             being enforced",
        ));
    }
    if outcome.free_to_ingress_guarded {
        return Err(fail(
            "a pod guarded by an ingress rule was still reachable. The cluster does not enforce \
             ingress NetworkPolicy; check the network plugin's policy settings",
        ));
    }
    if outcome.egress_guarded_to_free {
        return Err(fail(
            "a pod guarded by an egress rule could still connect out. The cluster does not \
             enforce egress NetworkPolicy; check the network plugin's policy settings",
        ));
    }
    Ok(())
}

fn pod(
    name: &str,
    namespace: &str,
    extra_labels: &[(&str, &str)],
    image: &str,
    script: &str,
) -> Result<Pod, ManagerError> {
    let mut labels = json!({ "app.kubernetes.io/managed-by": "openflows-manager" });
    for (key, value) in extra_labels {
        labels[*key] = json!(value);
    }
    serde_json::from_value(json!({
        "apiVersion": "v1", "kind": "Pod",
        "metadata": { "name": name, "namespace": namespace, "labels": labels },
        "spec": {
            "restartPolicy": "Never",
            "automountServiceAccountToken": false,
            "securityContext": {
                "runAsNonRoot": true, "runAsUser": 65534,
                "seccompProfile": { "type": "RuntimeDefault" },
            },
            "containers": [{
                "name": "probe", "image": image, "command": ["sh", "-c", script],
                "securityContext": {
                    "allowPrivilegeEscalation": false,
                    "capabilities": { "drop": ["ALL"] },
                },
                "resources": {
                    "requests": { "cpu": "10m", "memory": "16Mi" },
                    "limits": { "cpu": "100m", "memory": "64Mi" },
                },
            }],
        },
    }))
    .map_err(|e| fail(e.to_string()))
}

async fn create_pod(pods: &Api<Pod>, pod: Pod) -> Result<(), ManagerError> {
    let name = pod.metadata.name.clone().unwrap_or_default();
    pods.create(&PostParams::default(), &pod)
        .await
        .map(drop)
        .map_err(|e| fail(format!("cannot create probe pod '{name}': {e}")))
}

fn phase(pod: &Pod) -> &str {
    pod.status
        .as_ref()
        .and_then(|status| status.phase.as_deref())
        .unwrap_or("Pending")
}

async fn wait_for_ip(pods: &Api<Pod>, name: &str) -> Result<String, ManagerError> {
    let deadline = Instant::now() + SERVER_TIMEOUT;
    loop {
        let pod = pods
            .get(name)
            .await
            .map_err(|e| fail(format!("cannot read probe pod '{name}': {e}")))?;
        match (
            phase(&pod),
            pod.status.as_ref().and_then(|s| s.pod_ip.clone()),
        ) {
            ("Running", Some(ip)) => return Ok(ip),
            ("Failed", _) => return Err(fail(format!("probe pod '{name}' failed to start"))),
            _ if Instant::now() >= deadline => {
                return Err(fail(format!(
                    "probe pod '{name}' did not start in time; check that the cluster can pull \
                     the probe image (OPENFLOWS_K8S_PROBE_IMAGE) and that its pods are allowed to run"
                )))
            }
            _ => sleep(POLL).await,
        }
    }
}

/// `true` when the client reached the server, `false` when it could not.
async fn wait_for_result(pods: &Api<Pod>, name: &str) -> Result<bool, ManagerError> {
    let deadline = Instant::now() + CLIENT_TIMEOUT;
    loop {
        let pod = pods
            .get(name)
            .await
            .map_err(|e| fail(format!("cannot read probe pod '{name}': {e}")))?;
        match phase(&pod) {
            "Succeeded" => return Ok(true),
            "Failed" => return Ok(false),
            _ if Instant::now() >= deadline => {
                return Err(fail(format!("probe pod '{name}' did not finish in time")))
            }
            _ => sleep(POLL).await,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn healthy() -> Outcome {
        Outcome {
            same_tenant: true,
            other_tenant: false,
            unlabelled: false,
            free_to_free: true,
            free_to_ingress_guarded: false,
            egress_guarded_to_free: false,
        }
    }

    fn error_of(outcome: Outcome) -> String {
        judge(outcome).expect_err("must fail").to_string()
    }

    #[test]
    fn isolation_holds_when_only_the_allowed_connections_get_through() {
        assert!(judge(healthy()).is_ok());
    }

    #[test]
    fn a_cross_tenant_connection_means_the_cluster_does_not_enforce_policy() {
        let error = error_of(Outcome {
            other_tenant: true,
            ..healthy()
        });

        assert!(error.contains("does not enforce"), "{error}");
        assert!(error.contains("Calico"), "{error}");
    }

    #[test]
    fn a_blocked_tenant_control_means_the_probe_cannot_be_trusted() {
        let error = error_of(Outcome {
            same_tenant: false,
            ..healthy()
        });

        assert!(error.contains("own tenant"), "{error}");
    }

    #[test]
    fn a_blocked_free_control_means_the_network_is_unhealthy() {
        let error = error_of(Outcome {
            free_to_free: false,
            ..healthy()
        });

        assert!(error.contains("unhealthy"), "{error}");
    }

    #[test]
    fn an_unlabelled_pod_getting_through_is_a_failure() {
        assert!(judge(Outcome {
            unlabelled: true,
            ..healthy()
        })
        .is_err());
    }

    #[test]
    fn missing_ingress_enforcement_is_reported_even_when_egress_blocks_tenants() {
        let error = error_of(Outcome {
            free_to_ingress_guarded: true,
            ..healthy()
        });

        assert!(error.contains("ingress"), "{error}");
        assert!(error.contains("does not enforce"), "{error}");
    }

    #[test]
    fn missing_egress_enforcement_is_reported_even_when_ingress_blocks_tenants() {
        let error = error_of(Outcome {
            egress_guarded_to_free: true,
            ..healthy()
        });

        assert!(error.contains("egress"), "{error}");
        assert!(error.contains("does not enforce"), "{error}");
    }

    #[test]
    fn probe_pods_satisfy_the_restricted_pod_standard() {
        let tenant = TenantId::from_uuid(uuid::Uuid::from_u128(5)).to_string();
        let pod = pod(
            "p",
            "ns",
            &[(manifests::TENANT_LABEL, &tenant)],
            "img",
            "true",
        )
        .expect("pod");
        let spec = serde_json::to_value(pod.spec.expect("spec")).expect("serialize");

        assert_eq!(spec["securityContext"]["runAsNonRoot"], true);
        assert_eq!(
            spec["containers"][0]["securityContext"]["allowPrivilegeEscalation"],
            false
        );
        assert_eq!(
            spec["containers"][0]["securityContext"]["capabilities"]["drop"][0],
            "ALL"
        );
        assert_eq!(
            pod.metadata.labels.expect("labels")[manifests::TENANT_LABEL],
            tenant
        );
    }

    #[test]
    fn a_probe_pod_without_extra_labels_has_no_tenant_label() {
        let pod = pod("p", "ns", &[], "img", "true").expect("pod");

        assert!(!pod
            .metadata
            .labels
            .expect("labels")
            .contains_key(manifests::TENANT_LABEL));
    }
}

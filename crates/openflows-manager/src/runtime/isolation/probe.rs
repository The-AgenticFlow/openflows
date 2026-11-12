//! Proves that the cluster enforces the tenant rules, not merely accepts them.
//!
//! A cluster whose network plugin ignores NetworkPolicy takes the objects
//! without complaint and then lets every pod talk to every other. The probe
//! applies the real rules in a throwaway namespace and checks three
//! connections to a tenant A server: from tenant A (must work, the control that
//! shows the probe itself is sound), from tenant B and from an unlabelled pod
//! (both must fail).

use super::{apply, manifests, IsolationSettings};
use crate::error::ManagerError;
use crate::id::TenantId;
use k8s_openapi::api::core::v1::{Namespace, Pod};
use k8s_openapi::api::networking::v1::NetworkPolicy;
use kube::api::{DeleteParams, PostParams};
use kube::{Api, Client};
use serde_json::json;
use std::time::Duration;
use tokio::time::{sleep, Instant};

const PORT: u16 = 8080;
const SERVER_TIMEOUT: Duration = Duration::from_secs(120);
const CLIENT_TIMEOUT: Duration = Duration::from_secs(90);
const POLL: Duration = Duration::from_secs(2);

fn fail(message: impl Into<String>) -> ManagerError {
    ManagerError::Config(format!("tenant isolation probe failed: {}", message.into()))
}

/// Run the probe; the throwaway namespace is removed whatever the outcome.
pub async fn run(client: &Client, settings: &IsolationSettings) -> Result<(), ManagerError> {
    let name = format!(
        "openflows-probe-{}",
        &uuid::Uuid::new_v4().simple().to_string()[..8]
    );
    let namespaces = Api::<Namespace>::all(client.clone());
    let namespace: Namespace = serde_json::from_value(json!({
        "apiVersion": "v1", "kind": "Namespace",
        "metadata": { "name": name, "labels": {
            "pod-security.kubernetes.io/enforce": "restricted",
            "app.kubernetes.io/managed-by": "openflows-manager",
        }},
    }))
    .map_err(|e| fail(e.to_string()))?;
    namespaces
        .create(&PostParams::default(), &namespace)
        .await
        .map_err(|e| fail(format!("cannot create a probe namespace: {e}")))?;
    let outcome = probe_in(client, settings, &name).await;
    if let Err(error) = namespaces.delete(&name, &DeleteParams::default()).await {
        tracing::warn!(namespace = %name, %error, "could not remove the isolation probe namespace");
    }
    outcome
}

async fn probe_in(
    client: &Client,
    settings: &IsolationSettings,
    namespace: &str,
) -> Result<(), ManagerError> {
    let tenant_a = TenantId::from_uuid(uuid::Uuid::new_v4());
    let tenant_b = TenantId::from_uuid(uuid::Uuid::new_v4());
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

    let pods = Api::<Pod>::namespaced(client.clone(), namespace);
    let serve = format!("while true; do echo ok | nc -l -p {PORT}; done");
    create_pod(
        &pods,
        pod(
            "server",
            namespace,
            Some(tenant_a),
            &settings.probe_image,
            &serve,
        )?,
    )
    .await?;
    let server_ip = wait_for_ip(&pods, "server").await?;

    let reach = format!("echo hi | nc -w 3 {server_ip} {PORT} | grep -q ok");
    for (name, tenant) in [
        ("same-tenant", Some(tenant_a)),
        ("other-tenant", Some(tenant_b)),
        ("unlabelled", None),
    ] {
        create_pod(
            &pods,
            pod(name, namespace, tenant, &settings.probe_image, &reach)?,
        )
        .await?;
    }
    let (same, other, unlabelled) = tokio::try_join!(
        wait_for_result(&pods, "same-tenant"),
        wait_for_result(&pods, "other-tenant"),
        wait_for_result(&pods, "unlabelled"),
    )?;
    judge(same, other, unlabelled)
}

/// Decide from which connections succeeded. Pure so every outcome is tested.
fn judge(same_tenant: bool, other_tenant: bool, unlabelled: bool) -> Result<(), ManagerError> {
    if !same_tenant {
        return Err(fail(
            "a pod could not reach another pod of its own tenant. Check that the network plugin \
             is healthy and that OPENFLOWS_K8S_PROBE_IMAGE contains nc and sh",
        ));
    }
    if other_tenant {
        return Err(fail(
            "a pod of one tenant reached a pod of another. The cluster does not enforce \
             NetworkPolicy; install a network plugin that does (for example Calico or Cilium) \
             before hosting several tenants",
        ));
    }
    if unlabelled {
        return Err(fail(
            "a pod without a tenant label reached a tenant pod. The default-deny rule is not \
             being enforced",
        ));
    }
    Ok(())
}

fn pod(
    name: &str,
    namespace: &str,
    tenant: Option<TenantId>,
    image: &str,
    script: &str,
) -> Result<Pod, ManagerError> {
    let mut labels = json!({ "app.kubernetes.io/managed-by": "openflows-manager" });
    if let Some(tenant) = tenant {
        labels[manifests::TENANT_LABEL] = json!(tenant.to_string());
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
        match (phase(&pod), pod.status.as_ref().and_then(|s| s.pod_ip.clone())) {
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

    #[test]
    fn isolation_holds_when_only_the_same_tenant_gets_through() {
        assert!(judge(true, false, false).is_ok());
    }

    #[test]
    fn a_cross_tenant_connection_means_the_cluster_does_not_enforce_policy() {
        let error = judge(true, true, false).expect_err("must fail").to_string();

        assert!(error.contains("does not enforce"), "{error}");
        assert!(error.contains("Calico"), "{error}");
    }

    #[test]
    fn a_blocked_control_means_the_probe_cannot_be_trusted() {
        let error = judge(false, false, false)
            .expect_err("must fail")
            .to_string();

        assert!(error.contains("own tenant"), "{error}");
    }

    #[test]
    fn an_unlabelled_pod_getting_through_is_a_failure() {
        assert!(judge(true, false, true).is_err());
    }

    #[test]
    fn probe_pods_satisfy_the_restricted_pod_standard() {
        let tenant = TenantId::from_uuid(uuid::Uuid::from_u128(5));
        let pod = pod("p", "ns", Some(tenant), "img", "true").expect("pod");
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
            tenant.to_string()
        );
    }

    #[test]
    fn an_unlabelled_probe_pod_has_no_tenant_label() {
        let pod = pod("p", "ns", None, "img", "true").expect("pod");

        assert!(!pod
            .metadata
            .labels
            .expect("labels")
            .contains_key(manifests::TENANT_LABEL));
    }
}

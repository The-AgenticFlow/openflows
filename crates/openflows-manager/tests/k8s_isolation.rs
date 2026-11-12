//! Tenant isolation against a real Kubernetes cluster.
//!
//! These tests use the cluster in the current kubeconfig context, so they are
//! ignored by default. See docs/implementation/centralized-deployment/tenant-isolation.md.
//!
//! On a cluster whose network plugin enforces NetworkPolicy (for example kind
//! with Calico):
//!   `cargo test --test k8s_isolation enforcing -- --ignored --test-threads=1`
//! On one that does not (for example kind with its default plugin):
//!   `cargo test --test k8s_isolation ignoring -- --ignored --test-threads=1`

use k8s_openapi::api::core::v1::{Namespace, ServiceAccount};
use k8s_openapi::api::networking::v1::NetworkPolicy;
use k8s_openapi::api::rbac::v1::{Role, RoleBinding};
use kube::api::{DeleteParams, ListParams, PostParams};
use kube::{Api, Client};
use openflows_manager::id::{OrganizationId, TenantId};
use openflows_manager::runtime::isolation::{
    cluster_client, manifests, run_probe, IsolationSettings, KubernetesIsolation, TenantIsolation,
};

fn settings(prefix: &str) -> IsolationSettings {
    IsolationSettings {
        namespace_prefix: prefix.to_string(),
        ..IsolationSettings::default()
    }
}

async fn client() -> Client {
    cluster_client()
        .await
        .expect("a reachable Kubernetes cluster in the current kubeconfig context")
}

fn ids() -> (OrganizationId, TenantId, TenantId) {
    (
        OrganizationId::from_uuid(uuid::Uuid::new_v4()),
        TenantId::from_uuid(uuid::Uuid::new_v4()),
        TenantId::from_uuid(uuid::Uuid::new_v4()),
    )
}

async fn policy_names(client: &Client, namespace: &str) -> Vec<String> {
    Api::<NetworkPolicy>::namespaced(client.clone(), namespace)
        .list(&ListParams::default())
        .await
        .expect("list policies")
        .items
        .into_iter()
        .filter_map(|policy| policy.metadata.name)
        .collect()
}

#[tokio::test]
#[ignore = "needs a Kubernetes cluster"]
async fn enforcing_cluster_walls_an_organization_namespace() {
    let client = client().await;
    let settings = settings("of507t-");
    let (org, tenant_a, tenant_b) = ids();
    let namespace = manifests::namespace_name(&settings.namespace_prefix, org);
    let isolation = KubernetesIsolation::new(client.clone(), settings);

    isolation
        .ensure_tenant(org, tenant_a)
        .await
        .expect("tenant a");
    isolation
        .ensure_tenant(org, tenant_b)
        .await
        .expect("tenant b");
    isolation
        .ensure_tenant(org, tenant_a)
        .await
        .expect("repeating is safe");

    let ns = Api::<Namespace>::all(client.clone())
        .get(&namespace)
        .await
        .expect("namespace exists");
    let labels = ns.metadata.labels.expect("labels");
    assert_eq!(labels["pod-security.kubernetes.io/enforce"], "restricted");
    let names = policy_names(&client, &namespace).await;
    for expected in [
        manifests::DEFAULT_DENY.to_string(),
        manifests::ALLOW_DNS.to_string(),
        manifests::tenant_policy_name(tenant_a),
        manifests::tenant_policy_name(tenant_b),
    ] {
        assert!(names.contains(&expected), "missing {expected} in {names:?}");
    }

    let account = Api::<ServiceAccount>::namespaced(client.clone(), &namespace)
        .get("default")
        .await
        .expect("default service account");
    assert_eq!(account.automount_service_account_token, Some(false));

    isolation
        .remove_tenant(org, tenant_a)
        .await
        .expect("remove");
    isolation
        .remove_tenant(org, tenant_a)
        .await
        .expect("removing twice is safe");
    let names = policy_names(&client, &namespace).await;
    assert!(!names.contains(&manifests::tenant_policy_name(tenant_a)));
    assert!(names.contains(&manifests::tenant_policy_name(tenant_b)));

    let _ = Api::<Namespace>::all(client)
        .delete(&namespace, &DeleteParams::default())
        .await;
}

#[tokio::test]
#[ignore = "needs a Kubernetes cluster that enforces NetworkPolicy"]
async fn enforcing_cluster_passes_the_probe() {
    let client = client().await;

    run_probe(&client, &IsolationSettings::default())
        .await
        .expect("the probe passes where policy is enforced");
}

#[tokio::test]
#[ignore = "needs a Kubernetes cluster that ignores NetworkPolicy"]
async fn ignoring_cluster_fails_the_probe() {
    let client = client().await;

    let error = run_probe(&client, &IsolationSettings::default())
        .await
        .expect_err("the probe must fail where policy is ignored")
        .to_string();

    assert!(error.contains("does not enforce"), "{error}");
}

#[tokio::test]
#[ignore = "needs a Kubernetes cluster"]
async fn provisioner_able_to_read_secrets_is_refused() {
    let client = client().await;
    let (org, tenant, _) = ids();
    let settings = IsolationSettings {
        provisioner_account: Some(("default".into(), "of507-provisioner".into())),
        ..settings("of507p-")
    };
    let namespace = manifests::namespace_name(&settings.namespace_prefix, org);
    let isolation = KubernetesIsolation::new(client.clone(), settings);
    let accounts = Api::<ServiceAccount>::namespaced(client.clone(), "default");
    accounts
        .create(
            &PostParams::default(),
            &serde_json::from_value(serde_json::json!({
                "apiVersion": "v1", "kind": "ServiceAccount",
                "metadata": { "name": "of507-provisioner" },
            }))
            .expect("service account"),
        )
        .await
        .expect("create provisioner account");

    isolation
        .ensure_tenant(org, tenant)
        .await
        .expect("a provisioner with no Secret access is accepted");

    let role: Role = serde_json::from_value(serde_json::json!({
        "apiVersion": "rbac.authorization.k8s.io/v1", "kind": "Role",
        "metadata": { "name": "read-secrets", "namespace": namespace },
        "rules": [{ "apiGroups": [""], "resources": ["secrets"], "verbs": ["get"] }],
    }))
    .expect("role");
    let binding: RoleBinding = serde_json::from_value(serde_json::json!({
        "apiVersion": "rbac.authorization.k8s.io/v1", "kind": "RoleBinding",
        "metadata": { "name": "read-secrets", "namespace": namespace },
        "roleRef": { "apiGroup": "rbac.authorization.k8s.io", "kind": "Role", "name": "read-secrets" },
        "subjects": [{ "kind": "ServiceAccount", "name": "of507-provisioner", "namespace": "default" }],
    }))
    .expect("binding");
    Api::<Role>::namespaced(client.clone(), &namespace)
        .create(&PostParams::default(), &role)
        .await
        .expect("create role");
    Api::<RoleBinding>::namespaced(client.clone(), &namespace)
        .create(&PostParams::default(), &binding)
        .await
        .expect("create binding");

    let error = isolation
        .ensure_tenant(org, tenant)
        .await
        .expect_err("a provisioner that can read Secrets must be refused")
        .to_string();

    let _ = Api::<Namespace>::all(client)
        .delete(&namespace, &DeleteParams::default())
        .await;
    let _ = accounts
        .delete("of507-provisioner", &DeleteParams::default())
        .await;
    assert!(error.contains("can get Secrets"), "{error}");
}

//! The network walls, the startup probe and the provisioner Secret check.

use crate::support::*;
use k8s_openapi::api::core::v1::{Namespace, Pod, ServiceAccount};
use k8s_openapi::api::rbac::v1::{Role, RoleBinding};
use kube::api::{DeleteParams, ListParams, PostParams};
use kube::Api;
use openflows_manager::runtime::isolation::{
    manifests, run_probe, IsolationSettings, KubernetesIsolation, TenantIsolation,
};

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

/// Set a tenant up with a provisioner account that has no Secret access (accepted), grant
/// `subject` read access to Secrets in the organization namespace, and return the error the
/// next setup fails with.
async fn refusal_after_granting(prefix: &str, account: &str, subject: serde_json::Value) -> String {
    let client = client().await;
    let (org, tenant, _) = ids();
    let settings = IsolationSettings {
        provisioner_account: Some(("default".into(), account.into())),
        ..settings(prefix)
    };
    let namespace = manifests::namespace_name(&settings.namespace_prefix, org);
    let isolation = KubernetesIsolation::new(client.clone(), settings);
    let accounts = Api::<ServiceAccount>::namespaced(client.clone(), "default");
    accounts
        .create(
            &PostParams::default(),
            &serde_json::from_value(serde_json::json!({
                "apiVersion": "v1", "kind": "ServiceAccount",
                "metadata": { "name": account },
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
        "subjects": [subject],
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

    let result = isolation.ensure_tenant(org, tenant).await;

    let _ = Api::<Namespace>::all(client)
        .delete(&namespace, &DeleteParams::default())
        .await;
    let _ = accounts.delete(account, &DeleteParams::default()).await;
    result
        .expect_err("a provisioner that can read Secrets must be refused")
        .to_string()
}

#[tokio::test]
#[ignore = "needs a Kubernetes cluster"]
async fn provisioner_able_to_read_secrets_is_refused() {
    let error = refusal_after_granting(
        "of507p-",
        "of507-provisioner",
        serde_json::json!({ "kind": "ServiceAccount", "name": "of507-provisioner", "namespace": "default" }),
    )
    .await;

    assert!(error.contains("can get Secrets"), "{error}");
}

#[tokio::test]
#[ignore = "needs a Kubernetes cluster"]
async fn provisioner_able_to_read_secrets_through_a_group_is_refused() {
    let error = refusal_after_granting(
        "of507g-",
        "of507-group-provisioner",
        serde_json::json!({ "kind": "Group", "apiGroup": "rbac.authorization.k8s.io", "name": "system:serviceaccounts:default" }),
    )
    .await;

    assert!(error.contains("can get Secrets"), "{error}");
}

#[tokio::test]
#[ignore = "needs a Kubernetes cluster that enforces NetworkPolicy"]
async fn probe_in_a_pre_created_namespace_passes_and_leaves_it_empty() {
    let client = client().await;
    let name = format!("of507-probe-{}", short());
    let settings = IsolationSettings {
        probe_namespace: Some(name.clone()),
        ..IsolationSettings::default()
    };
    create_namespace(&client, &name, manifests::pod_security_labels(&settings)).await;

    let outcome = run_probe(&client, &settings).await;

    let pods = Api::<Pod>::namespaced(client.clone(), &name)
        .list(&ListParams::default())
        .await
        .expect("list pods")
        .items
        .len();
    let policies = policy_names(&client, &name).await;
    let still_there = Api::<Namespace>::all(client.clone())
        .get(&name)
        .await
        .is_ok();
    delete_namespace(&client, &name).await;
    outcome.expect("the probe passes in a pre-created namespace");
    assert_eq!(pods, 0, "pods left behind");
    assert!(policies.is_empty(), "policies left behind: {policies:?}");
    assert!(still_there, "the probe namespace must not be deleted");
}

#[tokio::test]
#[ignore = "needs a Kubernetes cluster that enforces NetworkPolicy"]
async fn platform_access_is_gated_by_the_tenant_label() {
    let client = client().await;
    let platform = format!("of507-platform-{}", short());
    let settings = IsolationSettings {
        namespace_prefix: "of507q-".into(),
        platform_namespace: platform.clone(),
        platform_ports: vec![8080],
        ..IsolationSettings::default()
    };
    let (org, tenant, _) = ids();
    let org_namespace = manifests::namespace_name(&settings.namespace_prefix, org);
    create_namespace(&client, &platform, serde_json::json!({})).await;
    let platform_pods = Api::<Pod>::namespaced(client.clone(), &platform);
    create_pod(
        &platform_pods,
        "service",
        serde_json::json!({}),
        "while true; do echo ok | nc -l -p 8080; done",
    )
    .await;
    let service_ip = wait_for_ip(&platform_pods, "service").await;
    KubernetesIsolation::new(client.clone(), settings)
        .ensure_tenant(org, tenant)
        .await
        .expect("tenant set up");

    let reach = format!("echo hi | nc -w 3 {service_ip} 8080 | grep -q ok");
    let pods = Api::<Pod>::namespaced(client.clone(), &org_namespace);
    create_pod(
        &pods,
        "labelled",
        serde_json::json!({ manifests::TENANT_LABEL: tenant.to_string() }),
        &reach,
    )
    .await;
    create_pod(&pods, "unlabelled", serde_json::json!({}), &reach).await;
    let labelled = finished_ok(&pods, "labelled").await;
    let unlabelled = finished_ok(&pods, "unlabelled").await;

    delete_namespace(&client, &org_namespace).await;
    delete_namespace(&client, &platform).await;
    assert!(labelled, "a tenant pod must reach the platform service");
    assert!(
        !unlabelled,
        "an unlabelled pod must not reach the platform service"
    );
}

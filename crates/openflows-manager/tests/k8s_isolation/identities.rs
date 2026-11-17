//! The two Manager identities and the checks that keep them apart.

use crate::support::*;
use k8s_openapi::api::core::v1::{Namespace, Secret, ServiceAccount};
use k8s_openapi::api::networking::v1::NetworkPolicy;
use k8s_openapi::api::rbac::v1::{ClusterRole, ClusterRoleBinding, Role, RoleBinding};
use kube::api::{DeleteParams, PostParams};
use kube::Api;
use openflows_manager::id::WorkspaceId;
use openflows_manager::runtime::delivery::{
    secret_name, DeliveryTarget, KubernetesSecretDelivery, SecretDelivery,
};
use openflows_manager::runtime::isolation::{
    check_separation, identity, manifests, IsolationSettings, KubernetesIsolation, TenantIsolation,
};

#[tokio::test]
#[ignore = "needs a Kubernetes cluster"]
async fn separate_identities_each_do_only_their_own_job() {
    let rig = rig().await;
    let settings = IsolationSettings {
        delivery_account: Some(("default".into(), rig.delivery_account.clone())),
        ..settings("of507d-")
    };
    let (org, tenant, _) = ids();
    let namespace = manifests::namespace_name(&settings.namespace_prefix, org);
    let workspace = WorkspaceId::from_uuid(uuid::Uuid::new_v4());

    let separated = check_separation(&rig.namespace, &rig.delivery).await;
    let username = identity::username(&rig.delivery).await;
    let set_up = KubernetesIsolation::new(rig.namespace.clone(), settings.clone())
        .ensure_tenant(org, tenant)
        .await;
    let target = DeliveryTarget {
        organization_id: org,
        tenant_id: tenant,
        workspace_id: workspace,
    };
    let delivered =
        KubernetesSecretDelivery::new(rig.delivery.clone(), settings.namespace_prefix.clone())
            .deliver(&target, "reference", b"{}")
            .await;
    let namespace_reads = Api::<Secret>::namespaced(rig.namespace.clone(), &namespace)
        .get(&secret_name(workspace))
        .await;
    let delivery_edits_walls = Api::<NetworkPolicy>::namespaced(rig.delivery.clone(), &namespace)
        .delete(
            &manifests::tenant_policy_name(tenant),
            &DeleteParams::default(),
        )
        .await;

    delete_namespace(&rig.admin, &namespace).await;
    teardown(&rig).await;
    separated.expect("two separate identities are accepted");
    assert_eq!(
        username.expect("username"),
        format!("system:serviceaccount:default:{}", rig.delivery_account)
    );
    set_up.expect("the namespace identity sets the tenant up and binds the delivery account");
    delivered.expect("the delivery identity writes the Secret");
    assert!(
        forbidden(namespace_reads),
        "the namespace identity must not read the Secret"
    );
    assert!(
        forbidden(delivery_edits_walls),
        "the delivery identity must not change the rules"
    );
}

#[tokio::test]
#[ignore = "needs a Kubernetes cluster"]
async fn one_identity_doing_both_jobs_is_refused() {
    let rig = rig().await;

    let result = check_separation(&rig.admin, &rig.admin).await;

    teardown(&rig).await;
    let error = result
        .expect_err("one identity for both jobs must be refused")
        .to_string();
    assert!(error.contains("separate identities"), "{error}");
}

#[tokio::test]
#[ignore = "needs a Kubernetes cluster"]
async fn a_delivery_identity_that_administers_namespaces_is_refused() {
    let rig = rig().await;

    let result = check_separation(&rig.namespace, &rig.namespace).await;

    teardown(&rig).await;
    let error = result
        .expect_err("a delivery identity with namespace rights must be refused")
        .to_string();
    assert!(error.contains("namespace administration"), "{error}");
}

/// Set a tenant up with two separate identities (accepted), then bind `subject` to `rule` inside
/// the organization namespace only, and return the error the next setup fails with. A binding
/// made in one namespace is invisible to a cluster-wide access review.
async fn refusal_after_namespace_binding(
    prefix: &str,
    delivery_gets_it: bool,
    rule: serde_json::Value,
) -> String {
    let rig = rig().await;
    let settings = IsolationSettings {
        delivery_account: Some(("default".into(), rig.delivery_account.clone())),
        ..settings(prefix)
    };
    let (org, tenant, _) = ids();
    let namespace = manifests::namespace_name(&settings.namespace_prefix, org);
    let isolation = KubernetesIsolation::new(rig.namespace.clone(), settings);
    isolation
        .ensure_tenant(org, tenant)
        .await
        .expect("two separate identities are accepted");
    let subject = if delivery_gets_it {
        rig.delivery_account.clone()
    } else {
        format!("of507-namespace-{}", rig.tag)
    };
    let role: Role = serde_json::from_value(serde_json::json!({
        "apiVersion": "rbac.authorization.k8s.io/v1", "kind": "Role",
        "metadata": { "name": "extra", "namespace": namespace },
        "rules": [rule],
    }))
    .expect("role");
    let binding: RoleBinding = serde_json::from_value(serde_json::json!({
        "apiVersion": "rbac.authorization.k8s.io/v1", "kind": "RoleBinding",
        "metadata": { "name": "extra", "namespace": namespace },
        "roleRef": { "apiGroup": "rbac.authorization.k8s.io", "kind": "Role", "name": "extra" },
        "subjects": [{ "kind": "ServiceAccount", "name": subject, "namespace": "default" }],
    }))
    .expect("binding");
    Api::<Role>::namespaced(rig.admin.clone(), &namespace)
        .create(&PostParams::default(), &role)
        .await
        .expect("create role");
    Api::<RoleBinding>::namespaced(rig.admin.clone(), &namespace)
        .create(&PostParams::default(), &binding)
        .await
        .expect("create binding");
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;

    let result = isolation.ensure_tenant(org, tenant).await;

    delete_namespace(&rig.admin, &namespace).await;
    teardown(&rig).await;
    result
        .expect_err("a grant inside the namespace must be refused")
        .to_string()
}

#[tokio::test]
#[ignore = "needs a Kubernetes cluster"]
async fn a_namespace_identity_that_can_read_secrets_in_one_namespace_is_refused() {
    let error = refusal_after_namespace_binding(
        "of507n-",
        false,
        serde_json::json!({ "apiGroups": [""], "resources": ["secrets"], "verbs": ["get"] }),
    )
    .await;

    assert!(error.contains("can get secrets in namespace"), "{error}");
}

#[tokio::test]
#[ignore = "needs a Kubernetes cluster"]
async fn a_delivery_identity_that_can_edit_rules_in_one_namespace_is_refused() {
    let error = refusal_after_namespace_binding(
        "of507r-",
        true,
        serde_json::json!({
            "apiGroups": ["networking.k8s.io"], "resources": ["networkpolicies"], "verbs": ["patch"],
        }),
    )
    .await;

    assert!(
        error.contains("can patch networkpolicies in namespace"),
        "{error}"
    );
}

#[tokio::test]
#[ignore = "needs a Kubernetes cluster"]
async fn a_delivery_identity_that_can_read_secrets_in_one_namespace_is_refused() {
    let error = refusal_after_namespace_binding(
        "of507s-",
        true,
        serde_json::json!({ "apiGroups": [""], "resources": ["secrets"], "verbs": ["get"] }),
    )
    .await;

    assert!(error.contains("can get secrets in namespace"), "{error}");
}

#[tokio::test]
#[ignore = "needs a Kubernetes cluster"]
async fn a_refused_provisioner_leaves_nothing_behind() {
    let client = client().await;
    let tag = short();
    let account = format!("of507-early-{tag}");
    let accounts = Api::<ServiceAccount>::namespaced(client.clone(), "default");
    accounts
        .create(
            &PostParams::default(),
            &serde_json::from_value(serde_json::json!({
                "apiVersion": "v1", "kind": "ServiceAccount", "metadata": { "name": account },
            }))
            .expect("service account"),
        )
        .await
        .expect("create service account");
    let role: ClusterRole = serde_json::from_value(serde_json::json!({
        "apiVersion": "rbac.authorization.k8s.io/v1", "kind": "ClusterRole",
        "metadata": { "name": account },
        "rules": [{ "apiGroups": [""], "resources": ["secrets"], "verbs": ["get"] }],
    }))
    .expect("cluster role");
    let binding: ClusterRoleBinding = serde_json::from_value(serde_json::json!({
        "apiVersion": "rbac.authorization.k8s.io/v1", "kind": "ClusterRoleBinding",
        "metadata": { "name": account },
        "roleRef": { "apiGroup": "rbac.authorization.k8s.io", "kind": "ClusterRole", "name": account },
        "subjects": [{ "kind": "ServiceAccount", "name": account, "namespace": "default" }],
    }))
    .expect("cluster role binding");
    Api::<ClusterRole>::all(client.clone())
        .create(&PostParams::default(), &role)
        .await
        .expect("create role");
    Api::<ClusterRoleBinding>::all(client.clone())
        .create(&PostParams::default(), &binding)
        .await
        .expect("create binding");
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    let settings = IsolationSettings {
        provisioner_account: Some(("default".into(), account.clone())),
        ..settings("of507e-")
    };
    let (org, tenant, _) = ids();
    let namespace = manifests::namespace_name(&settings.namespace_prefix, org);

    let result = KubernetesIsolation::new(client.clone(), settings)
        .ensure_tenant(org, tenant)
        .await;
    let created = Api::<Namespace>::all(client.clone()).get(&namespace).await;

    let _ = Api::<ClusterRoleBinding>::all(client.clone())
        .delete(&account, &DeleteParams::default())
        .await;
    let _ = Api::<ClusterRole>::all(client.clone())
        .delete(&account, &DeleteParams::default())
        .await;
    let _ = accounts.delete(&account, &DeleteParams::default()).await;
    delete_namespace(&client, &namespace).await;
    assert!(
        result.is_err(),
        "a provisioner that can read Secrets must be refused"
    );
    assert!(
        matches!(created, Err(kube::Error::Api(status)) if status.code == 404),
        "the namespace must not have been created"
    );
}

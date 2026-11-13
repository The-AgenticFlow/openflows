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

use k8s_openapi::api::authentication::v1::TokenRequest;
use k8s_openapi::api::core::v1::{Namespace, Pod, Secret, ServiceAccount};
use k8s_openapi::api::networking::v1::NetworkPolicy;
use k8s_openapi::api::rbac::v1::{ClusterRole, ClusterRoleBinding, Role, RoleBinding};
use kube::api::{DeleteParams, ListParams, PostParams};
use kube::{Api, Client};
use openflows_manager::id::{OrganizationId, TenantId, WorkspaceId};
use openflows_manager::runtime::delivery::{
    secret_name, DeliveryTarget, KubernetesSecretDelivery, SecretDelivery,
};
use openflows_manager::runtime::isolation::{
    check_separation, cluster_client, identity, manifests, run_probe, IsolationSettings,
    KubernetesIsolation, TenantIsolation,
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

fn short() -> String {
    uuid::Uuid::new_v4().simple().to_string()[..8].to_string()
}

async fn create_namespace(client: &Client, name: &str, labels: serde_json::Value) {
    let namespace: Namespace = serde_json::from_value(serde_json::json!({
        "apiVersion": "v1", "kind": "Namespace",
        "metadata": { "name": name, "labels": labels },
    }))
    .expect("namespace");
    Api::<Namespace>::all(client.clone())
        .create(&PostParams::default(), &namespace)
        .await
        .expect("create namespace");
}

async fn delete_namespace(client: &Client, name: &str) {
    let _ = Api::<Namespace>::all(client.clone())
        .delete(name, &DeleteParams::default())
        .await;
}

async fn create_pod(pods: &Api<Pod>, name: &str, labels: serde_json::Value, script: &str) {
    let pod: Pod = serde_json::from_value(serde_json::json!({
        "apiVersion": "v1", "kind": "Pod",
        "metadata": { "name": name, "labels": labels },
        "spec": {
            "restartPolicy": "Never",
            "automountServiceAccountToken": false,
            "securityContext": {
                "runAsNonRoot": true, "runAsUser": 65534,
                "seccompProfile": { "type": "RuntimeDefault" },
            },
            "containers": [{
                "name": "c", "image": IsolationSettings::default().probe_image,
                "command": ["sh", "-c", script],
                "securityContext": {
                    "allowPrivilegeEscalation": false,
                    "capabilities": { "drop": ["ALL"] },
                },
            }],
        },
    }))
    .expect("pod");
    pods.create(&PostParams::default(), &pod)
        .await
        .expect("create pod");
}

/// The pod's IP once it runs.
async fn wait_for_ip(pods: &Api<Pod>, name: &str) -> String {
    for _ in 0..60 {
        let pod = pods.get(name).await.expect("get pod");
        if let Some(ip) = pod.status.as_ref().and_then(|s| s.pod_ip.clone()) {
            if pod.status.and_then(|s| s.phase).as_deref() == Some("Running") {
                return ip;
            }
        }
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    }
    panic!("pod {name} did not start");
}

/// `true` when the pod ran to success, `false` when it failed.
async fn finished_ok(pods: &Api<Pod>, name: &str) -> bool {
    for _ in 0..60 {
        let phase = pods
            .get(name)
            .await
            .expect("get pod")
            .status
            .and_then(|s| s.phase);
        match phase.as_deref() {
            Some("Succeeded") => return true,
            Some("Failed") => return false,
            _ => tokio::time::sleep(std::time::Duration::from_secs(2)).await,
        }
    }
    panic!("pod {name} did not finish");
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

/// Two real identities: one that sets namespaces up and one that writes Secrets.
struct Rig {
    admin: Client,
    namespace: Client,
    delivery: Client,
    delivery_account: String,
    tag: String,
}

async fn client_for(accounts: &Api<ServiceAccount>, name: &str) -> Client {
    let request: TokenRequest = serde_json::from_value(serde_json::json!({
        "apiVersion": "authentication.k8s.io/v1", "kind": "TokenRequest",
        "spec": { "audiences": [], "expirationSeconds": 3600 },
    }))
    .expect("token request");
    let token = accounts
        .create_token_request(name, &PostParams::default(), &request)
        .await
        .expect("token")
        .status
        .expect("token status")
        .token;
    let mut config = kube::Config::infer().await.expect("kube config");
    config.auth_info =
        serde_json::from_value(serde_json::json!({ "token": token })).expect("auth info");
    Client::try_from(config).expect("client")
}

async fn rig() -> Rig {
    let admin = client().await;
    let tag = short();
    let accounts = Api::<ServiceAccount>::namespaced(admin.clone(), "default");
    let namespace_account = format!("of507-namespace-{tag}");
    let delivery_account = format!("of507-delivery-{tag}");
    for name in [&namespace_account, &delivery_account] {
        accounts
            .create(
                &PostParams::default(),
                &serde_json::from_value(serde_json::json!({
                    "apiVersion": "v1", "kind": "ServiceAccount", "metadata": { "name": name },
                }))
                .expect("service account"),
            )
            .await
            .expect("create service account");
    }
    let role: ClusterRole = serde_json::from_value(serde_json::json!({
        "apiVersion": "rbac.authorization.k8s.io/v1", "kind": "ClusterRole",
        "metadata": { "name": namespace_account },
        "rules": [
            { "apiGroups": [""], "resources": ["namespaces"], "verbs": ["get", "create", "patch"] },
            { "apiGroups": [""], "resources": ["resourcequotas", "limitranges", "serviceaccounts"],
              "verbs": ["get", "create", "patch"] },
            { "apiGroups": ["networking.k8s.io"], "resources": ["networkpolicies"],
              "verbs": ["get", "create", "patch", "delete"] },
            { "apiGroups": ["authorization.k8s.io"], "resources": ["subjectaccessreviews"],
              "verbs": ["create"] },
            { "apiGroups": ["rbac.authorization.k8s.io"], "resources": ["rolebindings"],
              "verbs": ["get", "create", "patch"] },
            { "apiGroups": ["rbac.authorization.k8s.io"], "resources": ["clusterroles"],
              "verbs": ["bind"], "resourceNames": [manifests::SECRET_WRITER_ROLE] },
        ],
    }))
    .expect("cluster role");
    let binding: ClusterRoleBinding = serde_json::from_value(serde_json::json!({
        "apiVersion": "rbac.authorization.k8s.io/v1", "kind": "ClusterRoleBinding",
        "metadata": { "name": namespace_account },
        "roleRef": {
            "apiGroup": "rbac.authorization.k8s.io", "kind": "ClusterRole", "name": namespace_account,
        },
        "subjects": [{ "kind": "ServiceAccount", "name": namespace_account, "namespace": "default" }],
    }))
    .expect("cluster role binding");
    let secret_writer: ClusterRole = serde_json::from_value(serde_json::json!({
        "apiVersion": "rbac.authorization.k8s.io/v1", "kind": "ClusterRole",
        "metadata": { "name": manifests::SECRET_WRITER_ROLE },
        "rules": [{ "apiGroups": [""], "resources": ["secrets"], "verbs": ["create", "patch", "delete"] }],
    }))
    .expect("secret writer role");
    let roles = Api::<ClusterRole>::all(admin.clone());
    roles
        .create(&PostParams::default(), &role)
        .await
        .expect("create role");
    let _ = roles.create(&PostParams::default(), &secret_writer).await;
    Api::<ClusterRoleBinding>::all(admin.clone())
        .create(&PostParams::default(), &binding)
        .await
        .expect("create binding");
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    Rig {
        namespace: client_for(&accounts, &namespace_account).await,
        delivery: client_for(&accounts, &delivery_account).await,
        admin,
        delivery_account,
        tag,
    }
}

async fn teardown(rig: &Rig) {
    let name = format!("of507-namespace-{}", rig.tag);
    let _ = Api::<ClusterRoleBinding>::all(rig.admin.clone())
        .delete(&name, &DeleteParams::default())
        .await;
    let roles = Api::<ClusterRole>::all(rig.admin.clone());
    let _ = roles.delete(&name, &DeleteParams::default()).await;
    let _ = roles
        .delete(manifests::SECRET_WRITER_ROLE, &DeleteParams::default())
        .await;
    let accounts = Api::<ServiceAccount>::namespaced(rig.admin.clone(), "default");
    for account in [&name, &rig.delivery_account] {
        let _ = accounts.delete(account, &DeleteParams::default()).await;
    }
}

fn forbidden<T>(result: Result<T, kube::Error>) -> bool {
    matches!(result, Err(kube::Error::Api(status)) if status.code == 403)
}

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

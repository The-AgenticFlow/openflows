//! Shared helpers for the cluster tests.

use k8s_openapi::api::authentication::v1::TokenRequest;
use k8s_openapi::api::core::v1::{Namespace, Pod, ServiceAccount};
use k8s_openapi::api::networking::v1::NetworkPolicy;
use k8s_openapi::api::rbac::v1::{ClusterRole, ClusterRoleBinding};
use kube::api::{DeleteParams, ListParams, PostParams};
use kube::{Api, Client};
use openflows_manager::id::{OrganizationId, TenantId};
use openflows_manager::runtime::isolation::{cluster_client, manifests, IsolationSettings};

pub fn settings(prefix: &str) -> IsolationSettings {
    IsolationSettings {
        namespace_prefix: prefix.to_string(),
        ..IsolationSettings::default()
    }
}

pub async fn client() -> Client {
    cluster_client()
        .await
        .expect("a reachable Kubernetes cluster in the current kubeconfig context")
}

pub fn ids() -> (OrganizationId, TenantId, TenantId) {
    (
        OrganizationId::from_uuid(uuid::Uuid::new_v4()),
        TenantId::from_uuid(uuid::Uuid::new_v4()),
        TenantId::from_uuid(uuid::Uuid::new_v4()),
    )
}

pub async fn policy_names(client: &Client, namespace: &str) -> Vec<String> {
    Api::<NetworkPolicy>::namespaced(client.clone(), namespace)
        .list(&ListParams::default())
        .await
        .expect("list policies")
        .items
        .into_iter()
        .filter_map(|policy| policy.metadata.name)
        .collect()
}

pub fn short() -> String {
    uuid::Uuid::new_v4().simple().to_string()[..8].to_string()
}

pub async fn create_namespace(client: &Client, name: &str, labels: serde_json::Value) {
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

pub async fn delete_namespace(client: &Client, name: &str) {
    let _ = Api::<Namespace>::all(client.clone())
        .delete(name, &DeleteParams::default())
        .await;
}

pub async fn create_pod(pods: &Api<Pod>, name: &str, labels: serde_json::Value, script: &str) {
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
pub async fn wait_for_ip(pods: &Api<Pod>, name: &str) -> String {
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
pub async fn finished_ok(pods: &Api<Pod>, name: &str) -> bool {
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

/// Two real identities: one that sets namespaces up and one that writes Secrets.
pub struct Rig {
    pub admin: Client,
    pub namespace: Client,
    pub delivery: Client,
    pub delivery_account: String,
    pub tag: String,
}

pub async fn client_for(accounts: &Api<ServiceAccount>, name: &str) -> Client {
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

pub async fn rig() -> Rig {
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

pub async fn teardown(rig: &Rig) {
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

pub fn forbidden<T>(result: Result<T, kube::Error>) -> bool {
    matches!(result, Err(kube::Error::Api(status)) if status.code == 403)
}

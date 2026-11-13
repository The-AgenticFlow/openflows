//! Keeps the Manager's two Kubernetes identities apart.
//!
//! The namespace identity builds each organization's namespace, quota and rules. The
//! delivery identity writes the runtime Secrets into it. Neither may do the other's job:
//! someone who can build namespaces must not be able to read a tenant's credentials, and
//! the identity that writes credentials must not be able to change the walls around them.
//! The check asks the API server what each identity may do, so it holds however the two
//! were set up, and it fails when they are in effect one identity.

use super::manifests;
use crate::error::ManagerError;
use k8s_openapi::api::authentication::v1::SelfSubjectReview;
use k8s_openapi::api::authorization::v1::{SelfSubjectAccessReview, SubjectAccessReview};
use kube::api::PostParams;
use kube::config::{KubeConfigOptions, Kubeconfig};
use kube::{Api, Client, Config};
use serde_json::json;

/// Path of a kubeconfig for the Secret-delivery identity. Unset, delivery uses the same
/// identity as everything else, which hosted mode refuses.
pub const DELIVERY_KUBECONFIG_ENV: &str = "OPENFLOWS_K8S_DELIVERY_KUBECONFIG";

/// `(api group, resource, verb)`
type Right = (&'static str, &'static str, &'static str);

/// What the namespace identity must not be able to do: touch Secrets at all.
pub const NAMESPACE_IDENTITY_MUST_LACK: [Right; 7] = [
    ("", "secrets", "get"),
    ("", "secrets", "list"),
    ("", "secrets", "watch"),
    ("", "secrets", "create"),
    ("", "secrets", "update"),
    ("", "secrets", "patch"),
    ("", "secrets", "delete"),
];

/// What the delivery identity must not be able to do: administer namespaces and their walls,
/// or read Secrets. It only writes them.
pub const DELIVERY_IDENTITY_MUST_LACK: [Right; 12] = [
    ("", "secrets", "get"),
    ("", "secrets", "list"),
    ("", "secrets", "watch"),
    ("", "namespaces", "create"),
    ("", "namespaces", "patch"),
    ("", "namespaces", "delete"),
    ("networking.k8s.io", "networkpolicies", "create"),
    ("networking.k8s.io", "networkpolicies", "patch"),
    ("", "resourcequotas", "create"),
    ("", "limitranges", "create"),
    ("", "serviceaccounts", "create"),
    ("rbac.authorization.k8s.io", "rolebindings", "create"),
];

/// The client that writes runtime Secrets: the one in `OPENFLOWS_K8S_DELIVERY_KUBECONFIG`
/// when set, otherwise the default in-cluster or kubeconfig identity.
pub async fn delivery_client() -> Result<Client, ManagerError> {
    let Some(path) = delivery_kubeconfig() else {
        return super::cluster_client().await.map_err(|e| {
            ManagerError::Config(format!(
                "kubernetes runtime secret delivery needs cluster access: {e}"
            ))
        });
    };
    super::install_crypto_provider();
    let failed = |e: &dyn std::fmt::Display| {
        ManagerError::Config(format!(
            "cannot use the delivery kubeconfig {DELIVERY_KUBECONFIG_ENV}={path}: {e}"
        ))
    };
    let kubeconfig = Kubeconfig::read_from(&path).map_err(|e| failed(&e))?;
    let config = Config::from_custom_kubeconfig(kubeconfig, &KubeConfigOptions::default())
        .await
        .map_err(|e| failed(&e))?;
    Client::try_from(config).map_err(|e| failed(&e))
}

/// The configured delivery kubeconfig path, if any.
pub fn delivery_kubeconfig() -> Option<String> {
    std::env::var(DELIVERY_KUBECONFIG_ENV)
        .ok()
        .filter(|path| !path.trim().is_empty())
}

/// The name the API server knows this client by.
pub async fn username(client: &Client) -> Result<String, ManagerError> {
    let review: SelfSubjectReview = serde_json::from_value(json!({
        "apiVersion": "authentication.k8s.io/v1", "kind": "SelfSubjectReview",
    }))
    .map_err(|e| ManagerError::Config(format!("invalid identity review: {e}")))?;
    let answer = Api::<SelfSubjectReview>::all(client.clone())
        .create(&PostParams::default(), &review)
        .await
        .map_err(|e| ManagerError::Config(format!("cannot read the delivery identity: {e}")))?;
    answer
        .status
        .and_then(|status| status.user_info)
        .and_then(|user| user.username)
        .ok_or_else(|| {
            ManagerError::Config("the API server did not name the delivery identity".into())
        })
}

/// With no namespace the review asks about cluster-wide grants only: a binding inside one
/// namespace does not count. Name the namespace to see those.
fn self_review(
    namespace: Option<&str>,
    right: Right,
) -> Result<SelfSubjectAccessReview, ManagerError> {
    let (group, resource, verb) = right;
    let mut attributes = json!({ "group": group, "resource": resource, "verb": verb });
    if let Some(namespace) = namespace {
        attributes["namespace"] = json!(namespace);
    }
    serde_json::from_value(json!({
        "apiVersion": "authorization.k8s.io/v1", "kind": "SelfSubjectAccessReview",
        "spec": { "resourceAttributes": attributes },
    }))
    .map_err(|e| ManagerError::Config(format!("invalid access review: {e}")))
}

/// Whether the client's own identity may do `right`, cluster-wide or in `namespace`.
async fn may(client: &Client, namespace: Option<&str>, right: Right) -> Result<bool, ManagerError> {
    let answer = Api::<SelfSubjectAccessReview>::all(client.clone())
        .create(&PostParams::default(), &self_review(namespace, right)?)
        .await
        .map_err(|e| ManagerError::Config(format!("cannot check the Manager's access: {e}")))?;
    Ok(answer.status.is_some_and(|status| status.allowed))
}

/// Fail unless the namespace identity cannot touch Secrets and the delivery identity cannot
/// administer namespaces.
pub async fn check_separation(namespace: &Client, delivery: &Client) -> Result<(), ManagerError> {
    for (group, resource, verb) in NAMESPACE_IDENTITY_MUST_LACK {
        if may(namespace, None, (group, resource, verb)).await? {
            return Err(ManagerError::Config(format!(
                "the identity that sets up namespaces can {verb} {resource}, so one identity \
                 does both jobs. Run the namespace setup and the Secret delivery under separate \
                 identities, and set {DELIVERY_KUBECONFIG_ENV} for the delivery one"
            )));
        }
    }
    for (group, resource, verb) in DELIVERY_IDENTITY_MUST_LACK {
        if may(delivery, None, (group, resource, verb)).await? {
            return Err(ManagerError::Config(format!(
                "the Secret delivery identity can {verb} {resource}, which is namespace \
                 administration. It must only write Secrets. If it is the same identity as the \
                 namespace setup, set {DELIVERY_KUBECONFIG_ENV} to a separate one"
            )));
        }
    }
    Ok(())
}

/// The same separation, inside one organization namespace. The cluster-wide check cannot see a
/// binding made in a single namespace, and the delivery identity gets its Secret rights through
/// exactly such bindings. Run as the namespace identity, which asks about the delivery account.
pub async fn check_namespace_separation(
    namespace_client: &Client,
    namespace: &str,
    delivery_account: (&str, &str),
) -> Result<(), ManagerError> {
    for (group, resource, verb) in NAMESPACE_IDENTITY_MUST_LACK {
        if may(namespace_client, Some(namespace), (group, resource, verb)).await? {
            return Err(ManagerError::Config(format!(
                "the identity that sets up namespaces can {verb} {resource} in namespace \
                 {namespace}, so one identity does both jobs. Remove that binding: it must not \
                 touch Secrets"
            )));
        }
    }
    let (account_namespace, account) = delivery_account;
    let reviews = Api::<SubjectAccessReview>::all(namespace_client.clone());
    for (group, resource, verb) in DELIVERY_IDENTITY_MUST_LACK
        .iter()
        .filter(|(_, resource, _)| *resource != "namespaces")
    {
        let review = manifests::account_review(
            namespace,
            account_namespace,
            account,
            (group, resource, verb),
        )?;
        let answer = reviews
            .create(&PostParams::default(), &review)
            .await
            .map_err(|e| ManagerError::Config(format!("cannot check delivery access: {e}")))?;
        if answer.status.is_some_and(|status| status.allowed) {
            return Err(ManagerError::Config(format!(
                "the Secret delivery account {account_namespace}/{account} can {verb} {resource} \
                 in namespace {namespace}. It must only write Secrets: remove that binding"
            )));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_namespace_identity_may_not_use_any_secret_verb() {
        let verbs: Vec<&str> = NAMESPACE_IDENTITY_MUST_LACK
            .iter()
            .map(|(_, _, verb)| *verb)
            .collect();

        for verb in [
            "get", "list", "watch", "create", "update", "patch", "delete",
        ] {
            assert!(verbs.contains(&verb), "{verb} is not checked");
        }
        assert!(NAMESPACE_IDENTITY_MUST_LACK
            .iter()
            .all(|(_, resource, _)| *resource == "secrets"));
    }

    #[test]
    fn the_delivery_identity_may_not_administer_namespaces_or_their_rules() {
        let resources: Vec<&str> = DELIVERY_IDENTITY_MUST_LACK
            .iter()
            .map(|(_, resource, _)| *resource)
            .collect();

        for resource in [
            "namespaces",
            "networkpolicies",
            "resourcequotas",
            "rolebindings",
        ] {
            assert!(resources.contains(&resource), "{resource} is not checked");
        }
        assert!(
            resources.contains(&"secrets"),
            "reading Secrets is not checked"
        );
    }

    #[test]
    fn a_review_asks_about_the_identity_itself_across_all_namespaces() {
        let review =
            self_review(None, ("networking.k8s.io", "networkpolicies", "create")).expect("review");
        let attributes = review.spec.resource_attributes.expect("attributes");

        assert_eq!(attributes.group.as_deref(), Some("networking.k8s.io"));
        assert_eq!(attributes.resource.as_deref(), Some("networkpolicies"));
        assert_eq!(attributes.verb.as_deref(), Some("create"));
        assert_eq!(attributes.namespace, None);
    }

    #[test]
    fn a_review_can_name_a_namespace_to_see_bindings_made_in_it() {
        let review = self_review(Some("org-ns"), ("", "secrets", "get")).expect("review");

        assert_eq!(
            review
                .spec
                .resource_attributes
                .expect("attributes")
                .namespace
                .as_deref(),
            Some("org-ns")
        );
    }

    #[test]
    fn the_delivery_identity_may_not_read_secrets() {
        let verbs: Vec<&str> = DELIVERY_IDENTITY_MUST_LACK
            .iter()
            .filter(|(_, resource, _)| *resource == "secrets")
            .map(|(_, _, verb)| *verb)
            .collect();

        assert_eq!(verbs, ["get", "list", "watch"]);
    }
}

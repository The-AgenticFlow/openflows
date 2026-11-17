//! Proves the cluster limits who may be bound to the Secret-writer role.
//!
//! The namespace identity may create role bindings and bind `openflows-secret-writer`. Without a
//! rule it could bind that role to itself in any namespace and so write or delete tenant
//! Secrets. The operator installs an admission rule that allows exactly one binding: the fixed
//! name, a namespace labelled as an organization's, and only the delivery account. This check
//! tries bindings the rule must refuse, as dry runs that save nothing, and the Manager does not
//! start unless the cluster refuses them.

use super::{manifests, IsolationSettings};
use crate::error::ManagerError;
use k8s_openapi::api::rbac::v1::RoleBinding;
use kube::api::PostParams;
use kube::{Api, Client};

const PROBE_BINDING: &str = "openflows-binding-probe";

fn fail(message: impl Into<String>) -> ManagerError {
    ManagerError::Config(format!(
        "secret writer binding rule check failed: {}",
        message.into()
    ))
}

/// What the cluster did with a binding.
#[derive(Debug, PartialEq, Eq)]
enum Verdict {
    Accepted,
    /// Refused by an admission policy.
    Refused,
    /// Failed for another reason, such as missing permissions or a missing namespace.
    Other(String),
}

fn verdict(result: Result<RoleBinding, kube::Error>) -> Verdict {
    match result {
        Ok(_) => Verdict::Accepted,
        Err(kube::Error::Api(status)) if status.message.contains("ValidatingAdmissionPolicy") => {
            Verdict::Refused
        }
        Err(error) => Verdict::Other(error.to_string()),
    }
}

async fn try_binding(
    client: &Client,
    namespace: &str,
    name: &str,
    account_namespace: &str,
    account: &str,
) -> Result<Verdict, ManagerError> {
    let binding = manifests::secret_writer_binding(name, namespace, account_namespace, account)?;
    let params = PostParams {
        dry_run: true,
        ..PostParams::default()
    };
    Ok(verdict(
        Api::<RoleBinding>::namespaced(client.clone(), namespace)
            .create(&params, &binding)
            .await,
    ))
}

/// Check the rule in `namespace`, which must exist and carry the `openflows.io/organization`
/// label. Does nothing when no delivery account is configured.
pub async fn check_binding_rule(
    client: &Client,
    settings: &IsolationSettings,
    namespace: &str,
) -> Result<(), ManagerError> {
    let Some((account_namespace, account)) = &settings.delivery_account else {
        return Ok(());
    };
    match try_binding(
        client,
        namespace,
        manifests::SECRET_DELIVERY_BINDING,
        account_namespace,
        account,
    )
    .await?
    {
        Verdict::Accepted => {}
        Verdict::Refused => {
            return Err(fail(format!(
                "the rule refuses the delivery account's own binding in namespace {namespace}. \\
                 Check that the account in the rule is {account_namespace}/{account}, as in \\
                 OPENFLOWS_K8S_DELIVERY_SERVICE_ACCOUNT, and that the probe namespace is labelled \\
                 {}",
                manifests::ORGANIZATION_LABEL
            )))
        }
        Verdict::Other(error) => {
            return Err(fail(format!(
                "cannot try a binding in namespace {namespace}: {error}"
            )))
        }
    }
    let forbidden = [
        (
            "to another service account",
            manifests::SECRET_DELIVERY_BINDING,
            "default",
            PROBE_BINDING,
        ),
        (
            "under another name",
            PROBE_BINDING,
            account_namespace.as_str(),
            account.as_str(),
        ),
    ];
    for (what, name, subject_namespace, subject) in forbidden {
        match try_binding(client, namespace, name, subject_namespace, subject).await? {
            Verdict::Refused => {}
            Verdict::Accepted => {
                return Err(fail(format!(
                    "a binding of {} {what} was accepted. The cluster does not enforce the rule \\
                     that limits who may be bound to it; install the admission policy \\
                     {} (see tenant-isolation.md)",
                    manifests::SECRET_WRITER_ROLE,
                    manifests::BINDING_RULE_NAME
                )))
            }
            Verdict::Other(error) => {
                return Err(fail(format!(
                    "a binding {what} failed for another reason than the rule: {error}"
                )))
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use kube::core::Status;

    fn api_error(message: &str, code: u16) -> kube::Error {
        kube::Error::Api(Box::new(
            Status::failure(message, "Invalid").with_code(code),
        ))
    }

    #[test]
    fn a_policy_denial_counts_as_a_refusal_by_the_rule() {
        let denial = api_error(
            "rolebindings is forbidden: ValidatingAdmissionPolicy 'openflows-secret-writer-bindings' \
             with binding 'x' denied request: not allowed",
            422,
        );

        assert_eq!(verdict(Err(denial)), Verdict::Refused);
    }

    #[test]
    fn a_plain_permission_error_is_not_a_refusal_by_the_rule() {
        let forbidden = api_error(
            "rolebindings is forbidden: User cannot create resource rolebindings",
            403,
        );

        assert!(matches!(verdict(Err(forbidden)), Verdict::Other(_)));
    }

    #[test]
    fn a_saved_binding_is_accepted() {
        let binding =
            manifests::secret_delivery_binding("ns", "default", "account").expect("binding");

        assert_eq!(verdict(Ok(binding)), Verdict::Accepted);
    }
}

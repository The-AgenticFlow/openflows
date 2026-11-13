//! The admission rule that limits who may bind the Secret-writer role.

use crate::support::*;
use k8s_openapi::api::admissionregistration::v1::{
    ValidatingAdmissionPolicy, ValidatingAdmissionPolicyBinding,
};
use k8s_openapi::api::core::v1::Secret;
use k8s_openapi::api::rbac::v1::RoleBinding;
use kube::api::{DeleteParams, PostParams};
use kube::{Api, Client};
use openflows_manager::runtime::isolation::{
    check_binding_rule, manifests, run_probe, IsolationSettings,
};

async fn remove_binding_rule(admin: &Client) {
    let _ = Api::<ValidatingAdmissionPolicyBinding>::all(admin.clone())
        .delete(manifests::BINDING_RULE_NAME, &DeleteParams::default())
        .await;
    let _ = Api::<ValidatingAdmissionPolicy>::all(admin.clone())
        .delete(manifests::BINDING_RULE_NAME, &DeleteParams::default())
        .await;
}

/// Whether the rule refuses a dry-run binding of the Secret writer to a stranger.
async fn rule_refuses_a_stranger(client: &Client, namespace: &str) -> bool {
    let binding = manifests::secret_writer_binding(
        manifests::SECRET_DELIVERY_BINDING,
        namespace,
        "default",
        "of507-stranger",
    )
    .expect("binding");
    let params = PostParams {
        dry_run: true,
        ..PostParams::default()
    };
    matches!(
        Api::<RoleBinding>::namespaced(client.clone(), namespace)
            .create(&params, &binding)
            .await,
        Err(kube::Error::Api(status)) if status.message.contains("ValidatingAdmissionPolicy")
    )
}

async fn install_binding_rule(admin: &Client, client: &Client, namespace: &str, account: &str) {
    remove_binding_rule(admin).await;
    wait_for_rule(client, namespace, false).await;
    let (policy, binding) = manifests::secret_writer_rule("default", account).expect("rule");
    Api::<ValidatingAdmissionPolicy>::all(admin.clone())
        .create(&PostParams::default(), &policy)
        .await
        .expect("create policy");
    Api::<ValidatingAdmissionPolicyBinding>::all(admin.clone())
        .create(&PostParams::default(), &binding)
        .await
        .expect("create policy binding");
    wait_for_rule(client, namespace, true).await;
}

/// The rule takes a moment to start and to stop; wait until it is (not) in force.
async fn wait_for_rule(client: &Client, namespace: &str, enforced: bool) {
    for _ in 0..40 {
        if rule_refuses_a_stranger(client, namespace).await == enforced {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }
    panic!(
        "the binding rule did not become {}",
        if enforced { "active" } else { "inactive" }
    );
}

async fn labelled_namespace(admin: &Client, tag: &str) -> String {
    let name = format!("of507b-{tag}");
    create_namespace(
        admin,
        &name,
        serde_json::json!({ manifests::ORGANIZATION_LABEL: "test" }),
    )
    .await;
    name
}

fn binding_settings(rig: &Rig) -> IsolationSettings {
    IsolationSettings {
        delivery_account: Some(("default".into(), rig.delivery_account.clone())),
        ..settings("of507b-")
    }
}

#[tokio::test]
#[ignore = "needs a Kubernetes cluster with admission policies (1.30 or newer)"]
async fn the_binding_rule_stops_the_namespace_identity_binding_the_role_to_itself() {
    let rig = rig().await;
    let namespace = labelled_namespace(&rig.admin, &rig.tag).await;
    install_binding_rule(
        &rig.admin,
        &rig.namespace,
        &namespace,
        &rig.delivery_account,
    )
    .await;
    let own_account = format!("of507-namespace-{}", rig.tag);
    let bindings = Api::<RoleBinding>::namespaced(rig.namespace.clone(), &namespace);
    let refused = |result: Result<RoleBinding, kube::Error>| matches!(result, Err(kube::Error::Api(status)) if status.message.contains("ValidatingAdmissionPolicy"));

    let to_itself = bindings
        .create(
            &PostParams::default(),
            &manifests::secret_writer_binding(
                manifests::SECRET_DELIVERY_BINDING,
                &namespace,
                "default",
                &own_account,
            )
            .expect("binding"),
        )
        .await;
    let other_name = bindings
        .create(
            &PostParams::default(),
            &manifests::secret_writer_binding(
                "extra",
                &namespace,
                "default",
                &rig.delivery_account,
            )
            .expect("binding"),
        )
        .await;
    let intended = bindings
        .create(
            &PostParams::default(),
            &manifests::secret_delivery_binding(&namespace, "default", &rig.delivery_account)
                .expect("binding"),
        )
        .await;
    let secret: Secret = serde_json::from_value(serde_json::json!({
        "apiVersion": "v1", "kind": "Secret", "metadata": { "name": "planted" },
    }))
    .expect("secret");
    let writes = Api::<Secret>::namespaced(rig.namespace.clone(), &namespace)
        .create(&PostParams::default(), &secret)
        .await;

    remove_binding_rule(&rig.admin).await;
    delete_namespace(&rig.admin, &namespace).await;
    teardown(&rig).await;
    assert!(
        refused(to_itself),
        "binding the role to itself must be refused"
    );
    assert!(
        refused(other_name),
        "a binding under another name must be refused"
    );
    intended.expect("the binding for the delivery account is accepted");
    assert!(
        forbidden(writes),
        "the namespace identity must not be able to write Secrets"
    );
}

#[tokio::test]
#[ignore = "needs a Kubernetes cluster with admission policies (1.30 or newer)"]
async fn the_startup_check_passes_when_the_rule_is_installed() {
    let rig = rig().await;
    let namespace = labelled_namespace(&rig.admin, &rig.tag).await;
    install_binding_rule(
        &rig.admin,
        &rig.namespace,
        &namespace,
        &rig.delivery_account,
    )
    .await;

    let result = check_binding_rule(&rig.namespace, &binding_settings(&rig), &namespace).await;

    remove_binding_rule(&rig.admin).await;
    delete_namespace(&rig.admin, &namespace).await;
    teardown(&rig).await;
    result.expect("the rule is enforced and the delivery account's own binding is accepted");
}

#[tokio::test]
#[ignore = "needs a Kubernetes cluster"]
async fn the_startup_check_fails_when_the_rule_is_missing() {
    let rig = rig().await;
    let namespace = labelled_namespace(&rig.admin, &rig.tag).await;
    remove_binding_rule(&rig.admin).await;
    wait_for_rule(&rig.namespace, &namespace, false).await;

    let result = check_binding_rule(&rig.namespace, &binding_settings(&rig), &namespace).await;

    delete_namespace(&rig.admin, &namespace).await;
    teardown(&rig).await;
    let error = result
        .expect_err("a cluster without the rule must be refused")
        .to_string();
    assert!(error.contains("does not enforce the rule"), "{error}");
}

#[tokio::test]
#[ignore = "needs a Kubernetes cluster with admission policies (1.30 or newer)"]
async fn the_startup_check_fails_when_the_rule_names_another_account() {
    let rig = rig().await;
    let namespace = labelled_namespace(&rig.admin, &rig.tag).await;
    install_binding_rule(&rig.admin, &rig.namespace, &namespace, "of507-someone-else").await;

    let result = check_binding_rule(&rig.namespace, &binding_settings(&rig), &namespace).await;

    remove_binding_rule(&rig.admin).await;
    delete_namespace(&rig.admin, &namespace).await;
    teardown(&rig).await;
    let error = result
        .expect_err("a rule for another account must be refused")
        .to_string();
    assert!(
        error.contains("refuses the delivery account's own binding"),
        "{error}"
    );
}

#[tokio::test]
#[ignore = "needs a Kubernetes cluster"]
async fn without_the_rule_the_namespace_identity_can_give_itself_secret_write() {
    let rig = rig().await;
    let namespace = labelled_namespace(&rig.admin, &rig.tag).await;
    remove_binding_rule(&rig.admin).await;
    wait_for_rule(&rig.namespace, &namespace, false).await;
    let own_account = format!("of507-namespace-{}", rig.tag);

    let bound = Api::<RoleBinding>::namespaced(rig.namespace.clone(), &namespace)
        .create(
            &PostParams::default(),
            &manifests::secret_writer_binding(
                manifests::SECRET_DELIVERY_BINDING,
                &namespace,
                "default",
                &own_account,
            )
            .expect("binding"),
        )
        .await;
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    let secret: Secret = serde_json::from_value(serde_json::json!({
        "apiVersion": "v1", "kind": "Secret", "metadata": { "name": "overwritten" },
    }))
    .expect("secret");
    let wrote = Api::<Secret>::namespaced(rig.namespace.clone(), &namespace)
        .create(&PostParams::default(), &secret)
        .await;

    delete_namespace(&rig.admin, &namespace).await;
    teardown(&rig).await;
    bound.expect("without the rule the namespace identity can bind the role to itself");
    wrote.expect("and then it can write Secrets: this is what the rule prevents");
}

#[tokio::test]
#[ignore = "needs a Kubernetes cluster that enforces NetworkPolicy and admission policies"]
async fn the_startup_probe_includes_the_binding_rule() {
    let admin = client().await;
    let tag = short();
    let account = format!("of507-probe-account-{tag}");
    let settings = IsolationSettings {
        delivery_account: Some(("default".into(), account.clone())),
        ..IsolationSettings::default()
    };
    let probe_namespace = labelled_namespace(&admin, &tag).await;
    remove_binding_rule(&admin).await;
    wait_for_rule(&admin, &probe_namespace, false).await;

    let without_rule = run_probe(&admin, &settings).await;
    install_binding_rule(&admin, &admin, &probe_namespace, &account).await;
    let with_rule = run_probe(&admin, &settings).await;

    remove_binding_rule(&admin).await;
    delete_namespace(&admin, &probe_namespace).await;
    let error = without_rule
        .expect_err("the probe must fail without the binding rule")
        .to_string();
    assert!(error.contains("does not enforce the rule"), "{error}");
    with_rule.expect("the probe passes with the binding rule installed");
}

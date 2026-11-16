//! Structure of the Kubernetes workspace templates.
//!
//! The hosted variant must start through the same workspace runtime and mount
//! the delivered Secret where the harness reads it, while staying inside the
//! namespace's restricted Pod Security Standard. What the startup steps do is
//! covered by tests/workspace_runtime.rs; this file guards the infrastructure.

const NEXUS: &str = include_str!("../templates/kubernetes/openflows-nexus/main.tf");

/// The startup script of a template, one trimmed line per entry.
fn startup(template: &'static str) -> Vec<&'static str> {
    template
        .split_once("startup_script = <<-EOT\n")
        .expect("the template has a startup script")
        .1
        .split_once("\n  EOT")
        .expect("the startup script is terminated")
        .0
        .lines()
        .map(str::trim)
        .collect()
}

#[test]
fn the_hosted_variant_uses_kubernetes_and_not_docker() {
    assert!(NEXUS.contains(r#"source = "hashicorp/kubernetes""#));
    for docker in [
        r#"resource "docker_container""#,
        r#"resource "docker_volume""#,
        r#"source = "kreuzwerker/docker""#,
        "host_path",
        "docker_network",
    ] {
        assert!(
            !NEXUS.contains(docker),
            "the Kubernetes variant must not depend on Docker ({docker})"
        );
    }
}

#[test]
fn the_pod_follows_coder_start_count_and_the_amd64_image() {
    let deployment = NEXUS
        .split_once(r#"resource "kubernetes_deployment_v1" "workspace""#)
        .expect("the template runs a Deployment")
        .1;
    assert!(deployment.contains("count = data.coder_workspace.me.start_count"));
    assert!(deployment.contains(r#""kubernetes.io/arch" = "amd64""#));
    assert!(
        NEXUS.contains(r#"image = "${var.workspace_image_repository}@${data.coder_parameter.workspace_image_digest.value}""#),
        "the pod must run the release-pinned image digest"
    );
}

#[test]
fn the_workspace_home_survives_stop_and_start() {
    // A stopped workspace runs Terraform with start_count = 0. The home volume
    // must not be tied to that count or every stop would discard it.
    let claim = NEXUS
        .split_once(r#"resource "kubernetes_persistent_volume_claim_v1" "workspace""#)
        .expect("the template declares a PVC")
        .1
        .split_once("\nresource ")
        .expect("the PVC block ends")
        .0;
    assert!(
        !claim.contains("count ="),
        "the workspace PVC must outlive a stop, not follow start_count"
    );
    assert!(claim.contains(r#"access_modes = ["ReadWriteOnce"]"#));
}

#[test]
fn the_pod_mounts_the_delivered_secret_where_the_harness_reads_it() {
    assert!(
        NEXUS.contains(r#"name  = "OPENFLOWS_RUNTIME_CREDENTIAL_FILE""#)
            && NEXUS.contains(r#"value = "/run/openflows/runtime.json""#),
        "the harness must be pointed at the mounted bundle"
    );
    assert!(
        NEXUS.contains("secret_name  = local.runtime_secret_name"),
        "the bundle comes from the Secret the Manager writes, named by the parameter"
    );
    assert!(
        NEXUS.contains(r#"key  = "runtime.json""#),
        "the Secret key is the runtime's contract"
    );
    assert!(
        NEXUS.contains("mount_path = \"/run/openflows\"") && NEXUS.contains("read_only  = true"),
        "the bundle must be mounted read-only at /run/openflows"
    );
    assert!(
        NEXUS.contains("data.coder_parameter.runtime_secret_ref.value"),
        "the template receives only the Secret's name"
    );
}

#[test]
fn the_namespace_comes_from_the_manager_when_it_passes_one() {
    // The Manager delivers the Secret into the organization's namespace, so the
    // pod has to land beside it; the template variable is the operator default.
    assert!(NEXUS.contains(
        r#"namespace = data.coder_parameter.runtime_namespace.value != "" ? data.coder_parameter.runtime_namespace.value : var.namespace"#
    ));
}

#[test]
fn the_pod_stays_inside_the_restricted_pod_security_standard() {
    for rule in [
        "automount_service_account_token = false",
        "run_as_non_root = true",
        "allow_privilege_escalation = false",
        r#"drop = ["ALL"]"#,
        r#"type = "RuntimeDefault""#,
        "fs_group = 1000",
    ] {
        assert!(
            NEXUS.contains(rule),
            "the pod must satisfy the restricted standard ({rule})"
        );
    }
}

#[test]
fn the_pod_carries_the_tenant_label_the_network_policy_selects() {
    assert!(
        NEXUS.contains(r#""openflows.io/tenant"        = data.coder_parameter.tenant.value"#),
        "the per-tenant NetworkPolicy selects pods by openflows.io/tenant"
    );
    assert!(
        NEXUS.contains(r#""openflows.io/organization"  = data.coder_parameter.organization.value"#),
        "the isolation docs require the organization label on every pod"
    );
    // The selector must not include the tenant: changing it would make the pod
    // unaddressable for the running Service.
    let selector = NEXUS
        .split_once("selector_labels = {")
        .expect("the template declares stable selector labels")
        .1
        .split_once('}')
        .expect("the selector block ends")
        .0;
    assert!(!selector.contains("openflows.io/tenant"));
}

#[test]
fn the_relay_service_is_named_for_the_workers_parameter() {
    // The Manager hands workers `openflows-nexus-<tenant>:3000` as a2a_relay_addr.
    assert!(
        NEXUS.contains(r#"service_name = "openflows-nexus-${data.coder_parameter.tenant.value}""#)
    );
    let service = NEXUS
        .split_once(r#"resource "kubernetes_service_v1" "relay""#)
        .expect("the template declares the relay Service")
        .1
        .split_once("\nresource ")
        .expect("the Service block ends")
        .0;
    assert!(service.contains("port        = 3000"));
    assert!(service.contains("selector = local.selector_labels"));
}

#[test]
fn nexus_starts_through_the_shared_runtime_and_the_controller_bootstrap() {
    let lines = startup(NEXUS);
    let load = lines
        .iter()
        .position(|line| line.starts_with(r#". "$OF_RUNTIME_LIB" || {"#))
        .expect("the template loads the workspace runtime");
    let check = lines
        .iter()
        .position(|line| line.starts_with(r#"[ "$${OF_RUNTIME_API:-}" = 1 ]"#))
        .expect("the template checks the runtime API");
    let bootstrap = lines
        .iter()
        .position(|line| {
            line.starts_with(
                "of_bootstrap_controller CODER_CHAT_HOOK_SECRET=hook_key CODER_CHAT_HOOK_URL=hook_url",
            )
        })
        .expect("the template runs the controller bootstrap");
    let controller = lines
        .iter()
        .position(|line| line.starts_with("nohup openflows run"))
        .expect("the template starts the controller");
    assert!(load < check, "the API is checked after loading the runtime");
    assert!(check < bootstrap, "the API is checked before bootstrapping");
    assert!(
        bootstrap < controller,
        "the controller starts after bootstrap"
    );
    assert!(
        !NEXUS.contains("sudo "),
        "the template leaves privileged preparation to the runtime"
    );
}

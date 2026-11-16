//! Structure of the Kubernetes workspace templates.
//!
//! The hosted variants must start through the same workspace runtime and mount
//! the delivered Secret where the harness reads it, while staying inside the
//! namespace's restricted Pod Security Standard. What the startup steps do is
//! covered by tests/workspace_runtime.rs; this file guards the infrastructure.

const NEXUS: &str = include_str!("../templates/kubernetes/openflows-nexus/main.tf");
const FORGE: &str = include_str!("../templates/kubernetes/openflows-forge/main.tf");
const SENTINEL: &str = include_str!("../templates/kubernetes/openflows-sentinel/main.tf");
const VESSEL: &str = include_str!("../templates/kubernetes/openflows-vessel/main.tf");
const LORE: &str = include_str!("../templates/kubernetes/openflows-lore/main.tf");

/// Every hosted role variant, as (role, template).
const ALL: [(&str, &str); 5] = [
    ("nexus", NEXUS),
    ("forge", FORGE),
    ("sentinel", SENTINEL),
    ("vessel", VESSEL),
    ("lore", LORE),
];

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
fn the_hosted_variants_use_kubernetes_and_not_docker() {
    for (role, template) in ALL {
        assert!(
            template.contains(r#"source = "hashicorp/kubernetes""#),
            "{role} must use the Kubernetes provider"
        );
        for docker in [
            r#"resource "docker_container""#,
            r#"resource "docker_volume""#,
            r#"source = "kreuzwerker/docker""#,
            "host_path",
            "docker_network",
        ] {
            assert!(
                !template.contains(docker),
                "{role} must not depend on Docker ({docker})"
            );
        }
    }
}

#[test]
fn the_pods_follow_coder_start_count_and_the_amd64_image() {
    for (role, template) in ALL {
        let deployment = template
            .split_once(r#"resource "kubernetes_deployment_v1" "workspace""#)
            .unwrap_or_else(|| panic!("{role} has no Deployment"))
            .1;
        assert!(
            deployment.contains("count = data.coder_workspace.me.start_count"),
            "{role} must stop its pod when Coder stops the workspace"
        );
        assert!(
            deployment.contains(r#""kubernetes.io/arch" = "amd64""#),
            "{role} must run on an amd64 node"
        );
        assert!(
            template.contains(
                r#"image = "${var.workspace_image_repository}@${data.coder_parameter.workspace_image_digest.value}""#
            ),
            "{role} must run the release-pinned image digest"
        );
    }
}

#[test]
fn the_workspace_home_survives_stop_and_start() {
    for (role, template) in ALL {
        // A stopped workspace runs Terraform with start_count = 0. The home
        // volume must not be tied to that count or every stop would discard it.
        let claim = template
            .split_once(r#"resource "kubernetes_persistent_volume_claim_v1" "workspace""#)
            .unwrap_or_else(|| panic!("{role} has no PVC"))
            .1
            .split_once("\nresource ")
            .unwrap_or_else(|| panic!("{role} PVC block does not end"))
            .0;
        assert!(
            !claim.contains("count ="),
            "{role} PVC must outlive a stop, not follow start_count"
        );
        assert!(claim.contains(r#"access_modes = ["ReadWriteOnce"]"#));
        // A WaitForFirstConsumer class only binds once the pod exists, and the
        // pod is created after this claim: waiting here would deadlock.
        assert!(
            claim.contains("wait_until_bound = false"),
            "{role} must not wait for a consumer that cannot exist yet"
        );
    }
}

#[test]
fn the_pods_mount_the_delivered_secret_where_the_harness_reads_it() {
    for (role, template) in ALL {
        assert!(
            template.contains(r#"name  = "OPENFLOWS_RUNTIME_CREDENTIAL_FILE""#)
                && template.contains(r#"value = "/run/openflows/runtime.json""#),
            "{role} must point the harness at the mounted bundle"
        );
        assert!(
            template.contains("secret_name  = local.runtime_secret_name"),
            "{role} must mount the Secret the Manager writes, named by the parameter"
        );
        assert!(
            template.contains(r#"key  = "runtime.json""#),
            "{role} must mount the Secret's contract key"
        );
        assert!(
            template.contains("mount_path = \"/run/openflows\"")
                && template.contains("read_only  = true"),
            "{role} must mount the bundle read-only at /run/openflows"
        );
        assert!(
            template.contains("data.coder_parameter.runtime_secret_ref.value"),
            "{role} must receive only the Secret's name"
        );
    }
}

#[test]
fn the_namespace_comes_from_the_manager_when_it_passes_one() {
    // The Manager delivers the Secret into the organization's namespace, so the
    // pod has to land beside it; the template variable is the operator default.
    for (role, template) in ALL {
        assert!(
            template.contains(
                r#"namespace = data.coder_parameter.runtime_namespace.value != "" ? data.coder_parameter.runtime_namespace.value : var.namespace"#
            ),
            "{role} must accept the namespace the Manager passes"
        );
    }
}

#[test]
fn the_pods_stay_inside_the_restricted_pod_security_standard() {
    for (role, template) in ALL {
        for rule in [
            "automount_service_account_token = false",
            "run_as_non_root = true",
            "allow_privilege_escalation = false",
            r#"drop = ["ALL"]"#,
            r#"type = "RuntimeDefault""#,
            "fs_group = 1000",
        ] {
            assert!(
                template.contains(rule),
                "{role} must satisfy the restricted standard ({rule})"
            );
        }
        assert!(
            !template.contains("sudo "),
            "{role} must leave privileged preparation to the runtime"
        );
    }
}

#[test]
fn the_pods_carry_the_labels_the_isolation_policies_select() {
    for (role, template) in ALL {
        assert!(
            template.contains(r#""openflows.io/tenant""#)
                && template.contains("data.coder_parameter.tenant.value"),
            "{role} must label its pods with the tenant"
        );
        assert!(
            template.contains(r#""openflows.io/organization""#),
            "{role} must label its pods with the organization"
        );
        // The selector must not include the tenant: changing it would make the
        // pod unaddressable for a running Service.
        let selector = template
            .split_once("selector_labels = {")
            .unwrap_or_else(|| panic!("{role} has no stable selector labels"))
            .1
            .split_once('}')
            .unwrap_or_else(|| panic!("{role} selector does not end"))
            .0;
        assert!(
            !selector.contains("openflows.io/tenant"),
            "{role} selector labels must stay stable for the pod's lifetime"
        );
    }
}

#[test]
fn every_role_starts_through_the_shared_runtime() {
    for (role, template) in ALL {
        let lines = startup(template);
        let load = lines
            .iter()
            .position(|line| line.starts_with(r#". "$OF_RUNTIME_LIB" || {"#))
            .unwrap_or_else(|| panic!("{role} must load the workspace runtime"));
        let check = lines
            .iter()
            .position(|line| line.starts_with(r#"[ "$${OF_RUNTIME_API:-}" = 1 ]"#))
            .unwrap_or_else(|| panic!("{role} must check the runtime API"));
        let bootstrap = lines
            .iter()
            .position(|line| line.starts_with("of_bootstrap_"))
            .unwrap_or_else(|| panic!("{role} must bootstrap"));
        assert!(load < check, "{role} checks the API after loading");
        assert!(
            check < bootstrap,
            "{role} checks the API before bootstrapping"
        );
        assert!(
            lines.contains(&"OF_HOSTED=true"),
            "{role} runs hosted by definition"
        );
    }
}

#[test]
fn nexus_keeps_the_relay_and_the_controller_bootstrap() {
    assert!(
        NEXUS.contains(r#"service_name = "openflows-nexus-${data.coder_parameter.tenant.value}""#)
    );
    let service = NEXUS
        .split_once(r#"resource "kubernetes_service_v1" "relay""#)
        .expect("Nexus declares the relay Service")
        .1
        .split_once("\nresource ")
        .expect("the Service block ends")
        .0;
    assert!(service.contains("port        = 3000"));
    assert!(service.contains("selector = local.selector_labels"));

    let lines = startup(NEXUS);
    assert!(lines.iter().any(|line| line.starts_with(
        "of_bootstrap_controller CODER_CHAT_HOOK_SECRET=hook_key CODER_CHAT_HOOK_URL=hook_url"
    )));
    assert!(lines
        .iter()
        .any(|line| line.starts_with("nohup openflows run")));
}

#[test]
fn workers_start_the_heartbeat_and_keep_their_role_settings() {
    for (role, template) in [
        ("forge", FORGE),
        ("sentinel", SENTINEL),
        ("vessel", VESSEL),
        ("lore", LORE),
    ] {
        assert!(
            template.contains("of_bootstrap_worker"),
            "{role} must use the worker bootstrap"
        );
        assert!(
            startup(template)
                .iter()
                .any(|line| line.starts_with("nohup openflows-harness heartbeat start")),
            "{role} must start the heartbeat daemon"
        );
        assert!(
            template.contains("OPENFLOWS_ROLE"),
            "{role} must tell the harness its role"
        );
    }

    // Forge commits and opens PRs, so its startup requires credentials, the
    // author identity, hooks and the assignment checkout.
    for setting in [
        "OF_GITHUB_ACCESS=required",
        "OF_GITHUB_API_TOKEN=true",
        "OF_GIT_IDENTITY=required",
        "OF_ROLE_HOOKS=true",
        "OF_CHECKOUT=assignment",
    ] {
        assert!(FORGE.contains(setting), "forge needs {setting}");
    }
    // A PR branch is interpolated into bash, so it must be validated first.
    assert!(FORGE.contains(r#"can(regex("^[A-Za-z0-9._/+@=-]+$", local.requested_branch))"#));
    assert!(FORGE.contains("OF_TARGET_BRANCH=\"${local.safe_branch}\""));

    // Sentinel reviews and the read-only roles never push.
    for template in [SENTINEL, VESSEL, LORE] {
        assert!(template.contains("OF_GITHUB_ACCESS=optional"));
        assert!(template.contains("OF_CHECKOUT=default"));
    }

    // Only the A2A pair talks over the relay.
    for (role, template) in [("forge", FORGE), ("sentinel", SENTINEL)] {
        assert!(
            template.contains("data.coder_parameter.a2a_relay_addr.value")
                && template.contains("data.coder_parameter.a2a_pair_token.value"),
            "{role} must receive the relay address and pair token"
        );
    }
    for (role, template) in [("vessel", VESSEL), ("lore", LORE)] {
        assert!(
            !template.contains("A2A_RELAY_ADDR"),
            "{role} does not talk to the relay"
        );
    }
}

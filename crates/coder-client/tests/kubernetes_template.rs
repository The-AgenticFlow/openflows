//! Structure of the Kubernetes workspace templates.
//!
//! The hosted variants must start through the same workspace runtime and mount
//! the delivered Secret where the harness reads it, while staying inside the
//! namespace's restricted Pod Security Standard. What the startup steps do is
//! covered by tests/workspace_runtime.rs; this file guards the infrastructure.

const NEXUS: &str = include_str!("../templates/kubernetes/openflows-nexus/main.tf");
const NEXUS_DOCKER: &str = include_str!("../templates/openflows-nexus/main.tf");
const FORGE_DOCKER: &str = include_str!("../templates/openflows-forge/main.tf");
const SENTINEL_DOCKER: &str = include_str!("../templates/openflows-sentinel/main.tf");
const VESSEL_DOCKER: &str = include_str!("../templates/openflows-vessel/main.tf");
const LORE_DOCKER: &str = include_str!("../templates/openflows-lore/main.tf");
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

/// Docker template each hosted variant replaces, as (role, docker, kubernetes).
const PAIRS: [(&str, &str, &str); 5] = [
    ("nexus", NEXUS_DOCKER, NEXUS),
    ("forge", FORGE_DOCKER, FORGE),
    ("sentinel", SENTINEL_DOCKER, SENTINEL),
    ("vessel", VESSEL_DOCKER, VESSEL),
    ("lore", LORE_DOCKER, LORE),
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
                r#"image = "${data.coder_parameter.workspace_image_repository.value}@${data.coder_parameter.workspace_image_digest.value}""#
            ),
            "{role} must run the release-pinned image digest"
        );
    }
}

#[test]
fn a_pod_is_replaced_only_after_the_previous_one_stopped() {
    // One pod owns the ReadWriteOnce home. The default rolling update starts the
    // replacement first, which either blocks on the volume on another node or
    // lets both pods write the same checkout on the same node.
    for (role, template) in ALL {
        let deployment = template
            .split_once(r#"resource "kubernetes_deployment_v1" "workspace""#)
            .unwrap_or_else(|| panic!("{role} has no Deployment"))
            .1;
        assert!(
            deployment.contains(r#"type = "Recreate""#),
            "{role} must replace its pod only after the previous one stopped"
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

/// The init container block of a hosted template.
fn init_container(role: &str, template: &'static str) -> &'static str {
    template
        .split_once("init_container {")
        .unwrap_or_else(|| panic!("{role} prepares the workspace directory first"))
        .1
        .split_once("\n        container {")
        .unwrap_or_else(|| panic!("{role} declares its container after the init container"))
        .0
}

/// The shell script the init container runs, with its directory pointed at `dir`.
fn init_script(role: &str, template: &'static str, dir: &str) -> String {
    let command = init_container(role, template)
        .split_once(r#"command     = ["sh", "-c", ""#)
        .unwrap_or_else(|| panic!("{role} runs a script"))
        .1
        .split_once(r#""]"#)
        .unwrap()
        .0;
    command
        .replace(r#"\""#, "\"")
        .replace("/home/coder/workspace", dir)
}

#[test]
fn a_kept_claim_replaces_an_empty_workspace_directory_and_never_one_with_files() {
    // The home claim survives stop and start, so it may already hold the directory the earlier bug
    // left behind. A directory without write access stands in for a root-owned one; root bypasses
    // permissions, so the check cannot say anything there.
    let uid = std::process::Command::new("id").arg("-u").output().unwrap();
    if String::from_utf8_lossy(&uid.stdout).trim() == "0" {
        return;
    }
    use std::os::unix::fs::PermissionsExt;
    let mode = |path: &std::path::Path, bits| {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(bits)).unwrap()
    };
    let run = |script: &str| {
        std::process::Command::new("sh")
            .args(["-c", script])
            .status()
            .unwrap()
            .success()
    };
    for (role, template) in ALL {
        let base = std::env::temp_dir().join(format!("of-wd-{}-{role}", std::process::id()));
        let dir = base.join("workspace");
        let script = init_script(role, template, dir.to_str().unwrap());

        // Empty and not writable: replaced by a directory the workspace user can write.
        std::fs::create_dir_all(&dir).unwrap();
        mode(&dir, 0o555);
        assert!(run(&script), "{role}: an empty directory is replaced");
        std::fs::create_dir(dir.join("probe")).unwrap_or_else(|_| panic!("{role}: not writable"));
        assert_eq!(
            std::fs::read_dir(&base).unwrap().count(),
            1,
            "{role}: nothing is left aside"
        );

        // Writable and populated: left alone, and the rerun succeeds.
        assert!(run(&script), "{role}: a writable directory is a no-op");
        assert!(
            dir.join("probe").exists(),
            "{role}: contents survive a rerun"
        );

        // Not writable and holding files: the pod must stop, with the files untouched.
        mode(&dir, 0o555);
        assert!(
            !run(&script),
            "{role}: a populated directory must fail the init"
        );
        assert!(
            dir.join("probe").exists(),
            "{role}: the files are not moved or deleted"
        );

        mode(&dir, 0o755);
        std::fs::remove_dir_all(&base).unwrap();
    }
}

#[test]
fn the_workspace_directory_is_created_by_the_workspace_user_before_the_role_starts() {
    // The image WORKDIR is /home/coder/workspace, which the home PVC hides. Left to the
    // container runtime it is created root-owned and the repository clone is denied.
    for (role, template) in ALL {
        let init = init_container(role, template);
        assert!(
            init.contains("mkdir -p $d"),
            "{role}: creates the directory"
        );
        assert!(
            init.contains(r#"working_dir = "/tmp""#),
            "{role}: a neutral workdir"
        );
        assert!(
            init.contains("run_as_user                = 1000"),
            "{role}: as the user"
        );
        assert!(
            init.contains(r#"mount_path = "/home/coder""#),
            "{role}: on the home volume"
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
    // A template import has no tenant yet, and an empty name is an invalid Service.
    assert!(service.contains(r#"count = data.coder_parameter.tenant.value != "" ? 1 : 0"#));

    let lines = startup(NEXUS);
    assert!(lines.iter().any(|line| line.starts_with(
        "of_bootstrap_controller CODER_CHAT_HOOK_SECRET=hook_key CODER_CHAT_HOOK_URL=hook_url"
    )));
    assert!(lines
        .iter()
        .any(|line| line.starts_with("nohup openflows run")));
}

/// Names of the runtime settings a startup script assigns, in order. Only a
/// line that starts with an `OF_` identifier counts, so shell tests such as
/// `[ -f ... ] || OF_X=...` are not mistaken for settings.
fn runtime_settings(template: &'static str) -> Vec<&'static str> {
    template
        .lines()
        .filter_map(|line| line.trim().split_once('='))
        .map(|(name, _)| name)
        .filter(|name| {
            name.starts_with("OF_")
                && name
                    .chars()
                    .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
        })
        .collect()
}

#[test]
fn the_pod_start_up_keeps_every_runtime_setting_its_container_sets() {
    // The pod must drive the shared runtime exactly like the container does;
    // dropping a setting here silently changes how the role starts.
    for (role, docker, kubernetes) in PAIRS {
        let hosted_settings = runtime_settings(kubernetes);
        for name in runtime_settings(docker) {
            assert!(
                hosted_settings.contains(&name),
                "{role} pod start-up drops {name}, which the container sets"
            );
        }
    }
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

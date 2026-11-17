//! Structure of the five role templates: what each one declares and how it
//! starts the shared workspace runtime. What the runtime steps do, and their
//! order, is tested by running them (tests/workspace_runtime.rs).

use std::io::Write;
use std::process::{Command, Stdio};

/// The startup steps every role template sources from the workspace image.
const RUNTIME: &str = include_str!("../../../runtime/lib/bootstrap.sh");
/// The hosted session and credential refresh loop the runtime starts.
const REFRESH: &str = include_str!("../../../runtime/bin/openflows-credential-refresh");

const ALL: [(&str, &str); 5] = [
    (
        "nexus",
        include_str!("../templates/openflows-nexus/main.tf"),
    ),
    (
        "forge",
        include_str!("../templates/openflows-forge/main.tf"),
    ),
    (
        "sentinel",
        include_str!("../templates/openflows-sentinel/main.tf"),
    ),
    (
        "vessel",
        include_str!("../templates/openflows-vessel/main.tf"),
    ),
    ("lore", include_str!("../templates/openflows-lore/main.tf")),
];

fn workers() -> impl Iterator<Item = (&'static str, &'static str)> {
    ALL.into_iter().filter(|(role, _)| *role != "nexus")
}

/// The startup script of a template, one trimmed line per entry.
fn startup(role: &str, template: &'static str) -> Vec<&'static str> {
    template
        .split_once("startup_script = <<-EOT\n")
        .unwrap_or_else(|| panic!("{role} has no startup script"))
        .1
        .split_once("\n  EOT")
        .unwrap_or_else(|| panic!("{role} startup script is not terminated"))
        .0
        .lines()
        .map(str::trim)
        .collect()
}

/// Index of the first startup line that starts with CALL, ignoring indentation.
fn step(role: &str, template: &'static str, call: &str) -> usize {
    startup(role, template)
        .iter()
        .position(|line| line.starts_with(call))
        .unwrap_or_else(|| panic!("{role} does not run {call}"))
}

/// The value a startup script assigns to NAME, without quotes.
fn setting(role: &str, template: &'static str, name: &str) -> Option<String> {
    startup(role, template)
        .iter()
        .find_map(|line| line.strip_prefix(&format!("{name}=")))
        .map(|value| value.trim_matches('"').to_owned())
}

#[test]
fn runtime_containers_follow_coder_start_count() {
    for (role, template) in ALL {
        let container = template
            .split_once("resource \"docker_container\" \"workspace\" {")
            .unwrap_or_else(|| panic!("{role} has no workspace container"))
            .1;
        assert!(
            container.contains("count = data.coder_workspace.me.start_count"),
            "{role} container must be absent when Coder stops its workspace"
        );
    }
}

#[test]
fn every_template_mounts_the_runtime_bundle_where_the_harness_reads_it() {
    // Delivery hands a template an opaque handle. This is the contract that makes
    // the handle usable: the bundle shows up inside the container at the path the
    // harness reads at startup, from the parameter the Manager sets, read-only.
    const BUNDLE_PATH: &str = "/run/openflows/runtime.json";
    for (role, template) in ALL {
        let volume = template
            .split("dynamic \"volumes\"")
            .map(|block| block.split_whitespace().collect::<Vec<_>>().join(" "))
            .find(|block| {
                block.contains("host_path = data.coder_parameter.runtime_secret_ref.value")
            })
            .unwrap_or_else(|| panic!("{role} has no runtime bundle volume"));
        assert!(
            volume.contains(&format!("container_path = \"{BUNDLE_PATH}\"")),
            "{role} must mount the bundle at {BUNDLE_PATH}"
        );
        assert!(
            volume.contains("read_only = true"),
            "{role} must mount the bundle read-only"
        );
        assert!(
            template.contains(&format!(
                "\"OPENFLOWS_RUNTIME_CREDENTIAL_FILE={BUNDLE_PATH}\""
            )),
            "{role} must point the harness at the mounted bundle"
        );
        assert!(
            !template.contains("OPENFLOWS_RUNTIME_CREDENTIAL_FILE\""),
            "{role} must leave reading the runtime secret to the runtime"
        );
    }
    assert!(
        RUNTIME
            .contains(r#"cat "$OPENFLOWS_RUNTIME_CREDENTIAL_FILE" > "$RUNTIME_CREDENTIAL_COPY""#),
        "the runtime must read the bundle into its private copy"
    );
    assert!(
        RUNTIME.contains(
            r#"sudo cat "$OPENFLOWS_RUNTIME_CREDENTIAL_FILE" > "$RUNTIME_CREDENTIAL_COPY""#
        ),
        "the runtime must fall back to its privileged copy step for a root-owned mount"
    );
}

#[test]
fn every_role_starts_from_the_shared_runtime_in_the_workspace_image() {
    let api = RUNTIME
        .lines()
        .find_map(|line| line.strip_prefix("OF_RUNTIME_API="))
        .expect("the runtime declares OF_RUNTIME_API");
    for (role, template) in ALL {
        let load = step(role, template, r#". "$OF_RUNTIME_LIB" || {"#);
        let check = step(role, template, r#"[ "$${OF_RUNTIME_API:-}" = "#);
        assert!(
            load < check,
            "{role} checks the runtime API after loading it"
        );
        assert!(
            startup(role, template)[check]
                .starts_with(&format!(r#"[ "$${{OF_RUNTIME_API:-}}" = {api} ]"#)),
            "{role} must require runtime API {api}, the one runtime/lib/bootstrap.sh provides"
        );
        assert_eq!(
            setting(role, template, "OF_RUNTIME_LIB").as_deref(),
            Some("/opt/openflows/runtime/lib/bootstrap.sh"),
            "{role} must source the runtime shipped in the workspace image"
        );
        for stale in [
            "enterprise-base",
            "releases/download",
            "harness_version",
            "harness-edge",
        ] {
            assert!(
                !template.contains(stale),
                "{role} still depends on {stale}; programs come from the workspace image"
            );
        }
        assert!(
            template.contains(
                r#""${var.workspace_image_repository}@${data.coder_parameter.workspace_image_digest.value}""#
            ),
            "{role} must run the release-pinned OpenFlows workspace image in hosted mode"
        );
        let variable = template
            .split_once("variable \"workspace_image\" {")
            .unwrap_or_else(|| panic!("{role} has no workspace_image variable"))
            .1
            .split_once("\n}")
            .unwrap()
            .0;
        assert!(
            variable.contains("default     = \"\""),
            "{role} must not default local workspaces to a floating image"
        );
        assert!(
            !template.contains("openflows-workspace:latest"),
            "{role} still refers to a floating workspace image"
        );
        assert!(
            template.contains(": local.local_workspace_image\n"),
            "{role} must run local workspaces from the given image or the explanatory reference"
        );
        let dev_mount = template
            .split("dynamic \"volumes\"")
            .map(|block| block.split_whitespace().collect::<Vec<_>>().join(" "))
            .find(|block| block.contains("container_path = \"/opt/openflows-dev\""))
            .unwrap_or_else(|| panic!("{role} has no development override mount"));
        assert!(
            dev_mount.contains(r#"for_each = var.dev_binary_host_path != "" ? [1] : []"#),
            "{role} must mount development programs only when asked to"
        );
        assert!(dev_mount.contains("read_only = true"));
    }
}

#[test]
fn workers_declare_their_settings_and_start_through_one_bootstrap() {
    // role -> (GitHub access, API token, git identity, hooks, checkout)
    let expected = [
        (
            "forge",
            ["required", "true", "required", "true", "assignment"],
        ),
        ("sentinel", ["optional", "", "", "", "default"]),
        ("vessel", ["optional", "", "", "", "default"]),
        ("lore", ["optional", "", "optional", "", "default"]),
    ];
    let names = [
        "OF_GITHUB_ACCESS",
        "OF_GITHUB_API_TOKEN",
        "OF_GIT_IDENTITY",
        "OF_ROLE_HOOKS",
        "OF_CHECKOUT",
    ];
    for (role, values) in expected {
        let template = workers().find(|(name, _)| *name == role).unwrap().1;
        for (name, value) in names.iter().zip(values) {
            assert_eq!(
                setting(role, template, name).unwrap_or_default(),
                value,
                "{role} must set {name}={value}"
            );
        }
        assert!(
            setting(role, template, "OF_ROLE").is_some(),
            "{role} sets OF_ROLE"
        );
        assert!(
            setting(role, template, "OF_REPO_URL").is_some(),
            "{role} sets OF_REPO_URL"
        );
        let bootstrap = step(role, template, "of_bootstrap_worker");
        let heartbeat = step(role, template, "nohup openflows-harness heartbeat start");
        assert!(
            bootstrap < heartbeat,
            "{role} starts its role command before bootstrap reports readiness"
        );
        for line in startup(role, template) {
            assert!(
                !(line.starts_with("of_") && line != "of_bootstrap_worker"),
                "{role} runs the startup step '{line}' itself; the order belongs to of_bootstrap_worker"
            );
        }
    }
}

#[test]
fn forge_resumes_the_assignment_branch_the_controller_passes() {
    let template = include_str!("../templates/openflows-forge/main.tf");
    assert_eq!(
        setting("forge", template, "OF_TARGET_BRANCH").as_deref(),
        Some("${local.safe_branch}")
    );
    assert_eq!(
        setting("forge", template, "OF_WORKER").as_deref(),
        Some("$ROLE")
    );
    assert_eq!(
        setting("forge", template, "OF_TICKET").as_deref(),
        Some("$TICKET_ID")
    );
    assert!(template.contains(
        "OPENFLOWS_GITHUB_API_TOKEN_FILE=/home/coder/.config/openflows/github-api-token"
    ));
}

#[test]
fn sentinel_clones_from_the_workspace_parameter() {
    let template = include_str!("../templates/openflows-sentinel/main.tf");
    assert_eq!(
        setting("sentinel", template, "OF_REPO_URL").as_deref(),
        Some("${data.coder_parameter.repo_url.value}")
    );
    assert!(!template.contains("var.repo_url"));
}

#[test]
fn nexus_starts_through_the_controller_bootstrap() {
    let template = include_str!("../templates/openflows-nexus/main.tf");
    let bootstrap = step(
        "nexus",
        template,
        "of_bootstrap_controller CODER_CHAT_HOOK_SECRET=hook_key CODER_CHAT_HOOK_URL=hook_url",
    );
    let controller = step("nexus", template, "nohup openflows run");
    assert!(bootstrap < controller);
    assert!(
        !template.contains("sudo "),
        "nexus must leave privileged preparation to the runtime"
    );
    assert!(template.contains("OPENFLOWS_GITHUB_API_TOKEN_FILE"));
    assert!(!template.contains("data.coder_external_auth.github"));
    assert!(!template.contains("CODER_EXTERNAL_AUTH_PRIMARY_GITHUB_ACCESS_TOKEN="));
}

#[test]
fn no_startup_source_uses_gh_authentication() {
    for source in ALL.iter().map(|(_, t)| *t).chain([RUNTIME, REFRESH]) {
        assert!(!source.contains("gh auth login"));
        assert!(!source.contains("/home/coder/.config/gh/hosts.yml"));
        assert!(!source.contains("apt-get install -y -qq gh"));
    }
}

#[test]
fn role_hook_settings_keep_other_settings() {
    let dir = tempfile::tempdir().unwrap();
    let hooks = dir.path().join("hooks");
    std::fs::create_dir(&hooks).unwrap();
    let settings = dir.path().join("settings.json");
    std::fs::write(&settings, r#"{"theme":"dark"}"#).unwrap();
    let runtime =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../runtime/lib/bootstrap.sh");
    for installed in [false, true] {
        if installed {
            std::fs::write(hooks.join("session_start.sh"), "#!/bin/sh\n").unwrap();
        }
        let mut child = Command::new("bash")
            .args(["-s", "--"])
            .arg(&runtime)
            .arg(&hooks)
            .arg(&settings)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(
                b"set -e\n. \"$1\"\nof_write_hook_settings \"$2\" \"$3\"\nprintf hooks-complete\n",
            )
            .unwrap();
        let output = child.wait_with_output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(output.stdout, b"hooks-complete");
        let settings: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&settings).unwrap()).unwrap();
        assert_eq!(settings["theme"], "dark");
        assert_eq!(
            settings["hooks"].as_object().unwrap().len(),
            usize::from(installed)
        );
        if installed {
            assert_eq!(
                settings["hooks"]["SessionStart"][0]["hooks"][0]["command"],
                hooks.join("session_start.sh").to_str().unwrap()
            );
        }
    }
}

#[test]
fn hosted_workers_never_read_instructions_from_the_shared_volume() {
    for (role, template) in workers() {
        assert!(
            !template.contains("/home/coder/.openflows/artifacts/orchestration"),
            "{role} still reads instructions from the shared artifacts volume"
        );
    }
}

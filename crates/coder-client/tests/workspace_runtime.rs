//! Behaviour of the shared workspace runtime (runtime/lib/bootstrap.sh and
//! runtime/bin/openflows-credential-refresh).
//!
//! Each test sources the library in bash, as the role templates do, with `sudo`
//! replaced by a function that records the call and runs the command as the test
//! user (ownership changes are skipped), so the steps run against a temporary
//! directory. Repository tests use real git repositories; hosted tests talk to a
//! stub Manager over HTTP.

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn runtime() -> PathBuf {
    repo_root().join("runtime/lib/bootstrap.sh")
}

/// Run SCRIPT after sourcing the runtime, with `$1` set to DIR. Every `sudo`
/// call is appended to DIR/sudo.log.
fn run(dir: &Path, script: &str) -> Output {
    run_with(dir, script, &[])
}

fn run_with(dir: &Path, script: &str, env: &[(&str, &str)]) -> Output {
    let prelude = r#"set -e
. "$RUNTIME"
sudo() {
    printf '%s\n' "$*" >> "$SUDO_LOG"
    case "$1" in chown|find) return 0 ;; -H) shift; "$@" ;; *) "$@" ;; esac
}
"#;
    let mut command = Command::new("bash");
    command
        .args(["-c", &format!("{prelude}{script}"), "runtime-test"])
        .arg(dir)
        .env("RUNTIME", runtime())
        .env("SUDO_LOG", dir.join("sudo.log"))
        .env("HOME", dir)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env_remove("GIT_DIR");
    for (name, value) in env {
        command.env(name, value);
    }
    command.output().unwrap()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn sudo_calls(dir: &Path) -> String {
    std::fs::read_to_string(dir.join("sudo.log")).unwrap_or_default()
}

/// Run git in DIR with a fixed identity, isolated from the user's config.
fn git(home: &Path, dir: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "test")
        .env("GIT_AUTHOR_EMAIL", "test@example.com")
        .env("GIT_COMMITTER_NAME", "test")
        .env("GIT_COMMITTER_EMAIL", "test@example.com")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

/// A bare `origin.git` with `main` and a PR branch `pr-branch`, and a golden
/// seed cloned from it. Returns the origin path.
fn fixture_repositories(dir: &Path) -> PathBuf {
    let source = dir.join("source");
    std::fs::create_dir_all(&source).unwrap();
    git(dir, &source, &["init", "-q", "-b", "main"]);
    std::fs::write(source.join("README.md"), "hello\n").unwrap();
    git(dir, &source, &["add", "."]);
    git(dir, &source, &["commit", "-qm", "initial"]);
    git(dir, &source, &["checkout", "-qb", "pr-branch"]);
    std::fs::write(source.join("pr.txt"), "pr work\n").unwrap();
    git(dir, &source, &["add", "."]);
    git(dir, &source, &["commit", "-qm", "pr work"]);
    git(dir, &source, &["checkout", "-q", "main"]);
    let origin = dir.join("origin.git");
    git(
        dir,
        dir,
        &[
            "clone",
            "-q",
            "--bare",
            source.to_str().unwrap(),
            origin.to_str().unwrap(),
        ],
    );
    git(
        dir,
        dir,
        &[
            "clone",
            "-q",
            origin.to_str().unwrap(),
            dir.join("golden").to_str().unwrap(),
        ],
    );
    origin
}

/// The runtime paths of a test workspace under `$1`.
const PATHS: &str = r#"OF_HOME="$1" OF_WORKSPACE="$1/ws" OF_STATE_DIR="$1/state" OF_GOLDEN_REPO="$1/golden"
OF_GIT_CREDENTIALS="$1/.git-credentials" OF_API_TOKEN_FILE="$1/.config/openflows/github-api-token"
mkdir -p "$OF_WORKSPACE" "$OF_STATE_DIR"
"#;

fn running_as_root() -> bool {
    let output = Command::new("id").arg("-u").output().unwrap();
    String::from_utf8_lossy(&output.stdout).trim() == "0"
}

#[test]
fn only_linux_x86_64_programs_replace_the_image_copy() {
    let dir = tempfile::tempdir().unwrap();
    let mut elf = vec![0x7f, b'E', b'L', b'F', 2, 1];
    elf.resize(18, 0);
    let mut x86_64 = elf.clone();
    x86_64.extend([0x3e, 0x00]);
    let mut aarch64 = elf;
    aarch64.extend([0xb7, 0x00]);
    let dev = dir.path().join("dev");
    let bin = dir.path().join("bin");
    std::fs::create_dir_all(&dev).unwrap();
    std::fs::create_dir_all(&bin).unwrap();
    std::fs::write(dev.join("openflows-harness"), &x86_64).unwrap();
    std::fs::write(dev.join("openflows"), &aarch64).unwrap();
    for program in ["openflows", "openflows-harness"] {
        std::fs::write(bin.join(program), "image copy\n").unwrap();
        Command::new("chmod")
            .args(["0755", bin.join(program).to_str().unwrap()])
            .status()
            .unwrap();
    }
    let output = run(
        dir.path(),
        r#"OF_DEV_DIR="$1/dev" OF_BIN_DIR="$1/bin"
of_install_programs openflows-harness openflows"#,
    );
    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(
        std::fs::read(bin.join("openflows-harness")).unwrap(),
        x86_64
    );
    assert_eq!(
        std::fs::read_to_string(bin.join("openflows")).unwrap(),
        "image copy\n"
    );
    assert!(stderr(&output).contains("Using the mounted development copy of openflows-harness"));
    assert!(
        stderr(&output).contains("is not a Linux x86-64 program; keeping the image's openflows")
    );

    let missing = run(
        dir.path(),
        r#"OF_DEV_DIR="$1/none" OF_BIN_DIR="$1/none"
of_install_programs openflows
echo continued"#,
    );
    assert!(!missing.status.success());
    assert!(stderr(&missing).contains("FATAL: openflows is not installed"));
    assert!(!stdout(&missing).contains("continued"));
}

#[test]
fn runtime_credentials_are_exported_and_the_private_copy_removed() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("runtime.json"),
        r#"{"manager_url":"http://manager:3002","bootstrap_token":"one use; $(not run)",
            "redis_url":"redis://tenant@redis:6379","redis_namespace":"tenant-a",
            "hook_key":"hook-secret","hook_url":"http://nexus/hooks"}"#,
    )
    .unwrap();
    let output = run(
        dir.path(),
        r#"export OPENFLOWS_RUNTIME_CREDENTIAL_FILE="$1/runtime.json" TMPDIR="$1/tmp"
           mkdir -p "$TMPDIR"
           of_load_runtime_credentials CODER_CHAT_HOOK_URL=hook_url
           printf '%s\n' "$OF_MANAGER_URL" "$OF_BOOTSTRAP" "$REDIS_URL" "$OPENFLOWS_TENANT" "$CODER_CHAT_HOOK_URL"
           ls -A "$TMPDIR""#,
    );
    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(
        stdout(&output),
        "http://manager:3002\none use; $(not run)\nredis://tenant@redis:6379\ntenant-a\nhttp://nexus/hooks\n",
        "values must be exported verbatim, never evaluated, and the copy removed"
    );
}

#[test]
fn a_runtime_secret_without_a_required_field_ends_startup_without_its_values() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("runtime.json"),
        r#"{"manager_url":"http://manager:3002","bootstrap_token":"do-not-print"}"#,
    )
    .unwrap();
    let output = run(
        dir.path(),
        r#"export OPENFLOWS_RUNTIME_CREDENTIAL_FILE="$1/runtime.json" TMPDIR="$1/tmp"
           mkdir -p "$TMPDIR"
           of_load_runtime_credentials
           echo continued"#,
    );
    assert!(!output.status.success());
    let log = stderr(&output);
    assert!(log.contains("FATAL: runtime secret is unreadable or lacks a required field"));
    assert!(!log.contains("do-not-print"));
    assert!(!stdout(&output).contains("continued"));
    let leftovers = std::fs::read_dir(dir.path().join("tmp")).unwrap().count();
    assert_eq!(leftovers, 0, "private copy left behind");
}

#[test]
fn a_missing_runtime_secret_mount_ends_startup() {
    let dir = tempfile::tempdir().unwrap();
    let output = run(
        dir.path(),
        r#"export OPENFLOWS_RUNTIME_CREDENTIAL_FILE="$1/absent.json" TMPDIR="$1"
           of_load_runtime_credentials 2>"$1/log" || true
           echo unreachable"#,
    );
    // `of_fatal` exits the startup shell even when the caller tolerates failure.
    assert!(!output.status.success());
    assert!(!stdout(&output).contains("unreachable"));
    let log = std::fs::read_to_string(dir.path().join("log")).unwrap();
    assert!(
        log.contains("FATAL: runtime secret is not mounted"),
        "{log}"
    );
}

#[test]
fn a_missing_github_token_is_fatal_only_where_the_role_requires_one() {
    let dir = tempfile::tempdir().unwrap();
    let optional = run(
        dir.path(),
        &format!("{PATHS}GIT_TOKEN= OF_CONNECTED_LOCAL=false\nof_store_git_credentials optional\necho continued"),
    );
    assert!(optional.status.success(), "{}", stderr(&optional));
    assert!(stderr(&optional).contains("WARNING: No GitHub token available"));
    assert!(stdout(&optional).contains("continued"));

    let required = run(
        dir.path(),
        &format!("{PATHS}GIT_TOKEN= OF_CONNECTED_LOCAL=false\nof_store_git_credentials required\necho continued"),
    );
    assert!(!required.status.success());
    assert!(stderr(&required).contains("FATAL: no GitHub credential is available"));
    assert!(!stdout(&required).contains("continued"));

    let identity = run(
        dir.path(),
        &format!(
            "{PATHS}GIT_TOKEN= OF_HOSTED=false\nof_configure_git_identity optional\necho continued"
        ),
    );
    assert!(identity.status.success(), "{}", stderr(&identity));
    assert!(stderr(&identity).contains("WARNING: no GitHub token yet"));
}

#[test]
fn git_credentials_are_configured_once_across_restarts() {
    let dir = tempfile::tempdir().unwrap();
    let store = format!(
        "{PATHS}GIT_TOKEN=tok-123 OF_CONNECTED_LOCAL=false\nof_store_git_credentials required\n"
    );
    for _ in 0..2 {
        let output = run(dir.path(), &store);
        assert!(output.status.success(), "{}", stderr(&output));
    }
    let helpers = git(
        dir.path(),
        dir.path(),
        &["config", "--global", "--get-all", "credential.helper"],
    );
    assert_eq!(helpers, "store");
    let credentials = dir.path().join(".git-credentials");
    assert_eq!(
        std::fs::read_to_string(&credentials).unwrap(),
        "https://x-access-token:tok-123@github.com\n"
    );
    let mode = Command::new("stat")
        .args(["-c", "%a", credentials.to_str().unwrap()])
        .output()
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&mode.stdout).trim(), "600");

    // Connected-local mode replaces the stored token with the helper.
    let helper = format!(
        "{PATHS}GIT_TOKEN=tok-123 OF_CONNECTED_LOCAL=true\nof_store_git_credentials required\n"
    );
    for _ in 0..2 {
        let output = run(dir.path(), &helper);
        assert!(output.status.success(), "{}", stderr(&output));
    }
    let helpers = git(
        dir.path(),
        dir.path(),
        &["config", "--global", "--get-all", "credential.helper"],
    );
    assert_eq!(
        helpers,
        "!python3 /opt/openflows/connected-local-github.py credential"
    );
    assert!(
        !credentials.exists(),
        "connected-local mode must not keep a token on disk"
    );
}

#[test]
fn github_tokens_come_from_the_broker_or_the_connected_local_helper() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("helper.py"),
        "import sys\nif sys.argv[1] == 'token':\n    print('local-token')\n",
    )
    .unwrap();
    let cases = [
        (
            "true",
            "false",
            "--api",
            "git=brokered-git api=brokered-api",
        ),
        ("true", "true", "--api", "git=brokered-git api=brokered-git"),
        ("true", "false", "", "git=brokered-git api="),
        ("false", "true", "--api", "git=local-token api=local-token"),
    ];
    for (hosted, connected, flag, expected) in cases {
        let output = run(
            dir.path(),
            &format!(
                r#"OF_HOSTED={hosted} OF_CONNECTED_LOCAL={connected} OF_CONNECTED_LOCAL_HELPER="$1/helper.py"
of_broker_token() {{ echo "brokered-$1"; }}
of_resolve_github_tokens {flag}
echo "git=$GIT_TOKEN api=${{API_TOKEN:-}}""#
            ),
        );
        assert!(output.status.success(), "{}", stderr(&output));
        assert_eq!(
            stdout(&output).trim(),
            expected,
            "hosted={hosted} connected={connected} {flag}"
        );
    }
}

/// Settings and recorded step calls for an of_bootstrap_worker run.
fn recorded_bootstrap(dir: &Path, settings: &str) -> (Vec<String>, String) {
    let script = format!(
        r#"STEPS="$1/steps"
for step in of_prepare of_install_programs of_load_runtime_credentials of_register_runtime \
    of_resolve_github_tokens of_store_git_credentials of_store_api_token of_configure_git_identity \
    of_install_artifact_bundle of_install_role_hooks of_acquire_repository of_checkout_assignment_branch \
    of_start_credential_refresh of_require_repository of_install_role_instructions of_signal_manager_ready \
    of_sync_controller_repository of_refresh_golden_seed of_release_privileges; do
    eval "$step() {{ echo \"$step\${{*:+ \$*}}\" >> \"\$STEPS\"; }}"
done
OF_RUNTIME_ACCESS_TOKEN=access OF_RUNTIME_REFRESH_TOKEN=refresh OF_BOOTSTRAP=boot
{settings}
echo "session=${{OF_RUNTIME_ACCESS_TOKEN:-}}${{OF_RUNTIME_REFRESH_TOKEN:-}}${{OF_BOOTSTRAP:-}}""#
    );
    let _ = std::fs::remove_file(dir.join("steps"));
    let output = run(dir, &script);
    assert!(output.status.success(), "{}", stderr(&output));
    let steps = std::fs::read_to_string(dir.join("steps")).unwrap();
    (steps.lines().map(str::to_owned).collect(), stdout(&output))
}

#[test]
fn worker_bootstrap_reports_readiness_only_after_checkout_and_instructions() {
    let dir = tempfile::tempdir().unwrap();
    let (steps, out) = recorded_bootstrap(
        dir.path(),
        "OF_HOSTED=true OF_ROLE=forge OF_REPO_URL=https://example/repo.git OF_GITHUB_ACCESS=required \
OF_GITHUB_API_TOKEN=true OF_GIT_IDENTITY=required OF_ROLE_HOOKS=true OF_CHECKOUT=assignment \
OF_TARGET_BRANCH=pr OF_WORKER=forge-1 OF_TICKET=T-1 of_bootstrap_worker",
    );
    assert_eq!(
        steps,
        [
            "of_prepare",
            "of_install_programs openflows-harness",
            "of_load_runtime_credentials",
            "of_register_runtime",
            "of_resolve_github_tokens --api",
            "of_store_git_credentials required",
            "of_store_api_token",
            "of_configure_git_identity required",
            "of_install_artifact_bundle",
            "of_install_role_hooks forge",
            "of_acquire_repository https://example/repo.git",
            "of_checkout_assignment_branch pr forge-1 T-1",
            "of_start_credential_refresh --api-token",
            "of_require_repository",
            "of_install_role_instructions forge",
            "of_release_privileges",
            "of_signal_manager_ready",
        ]
    );
    assert_eq!(
        out.trim(),
        "session=",
        "the role commands must not inherit the runtime session"
    );

    let (steps, _) = recorded_bootstrap(
        dir.path(),
        "OF_HOSTED=false OF_ROLE=sentinel OF_REPO_URL=https://example/repo.git of_bootstrap_worker",
    );
    assert_eq!(
        steps,
        [
            "of_prepare",
            "of_install_programs openflows-harness",
            "of_resolve_github_tokens",
            "of_store_git_credentials optional",
            "of_install_artifact_bundle",
            "of_acquire_repository https://example/repo.git --checkout-default",
            "of_start_credential_refresh",
            "of_require_repository",
            "of_install_role_instructions sentinel",
            "of_release_privileges",
            "of_signal_manager_ready",
        ]
    );

    let (steps, _) = recorded_bootstrap(
        dir.path(),
        "OF_HOSTED=false OF_ROLE=lore OF_GIT_IDENTITY=optional of_bootstrap_worker",
    );
    assert!(steps.contains(&"of_configure_git_identity optional".to_owned()));
}

#[test]
fn worker_bootstrap_requires_a_role_and_a_known_checkout() {
    let dir = tempfile::tempdir().unwrap();
    let output = run(dir.path(), "OF_ROLE= of_bootstrap_worker");
    assert!(stderr(&output).contains("FATAL: OF_ROLE is not set"));
    let output = run(
        dir.path(),
        r#"for s in of_prepare of_install_programs of_resolve_github_tokens of_store_git_credentials of_install_artifact_bundle; do eval "$s() { :; }"; done
OF_ROLE=vessel OF_CHECKOUT=sideways of_bootstrap_worker"#,
    );
    assert!(stderr(&output).contains("FATAL: unknown OF_CHECKOUT 'sideways'"));
}

#[test]
fn controller_bootstrap_keeps_its_session_and_seeds_workers_only_locally() {
    let dir = tempfile::tempdir().unwrap();
    let (steps, out) = recorded_bootstrap(
        dir.path(),
        "OF_HOSTED=true OF_REPO_URL=https://example/repo.git of_bootstrap_controller HOOK=hook_key",
    );
    assert_eq!(
        steps,
        [
            "of_prepare --own-artifacts",
            "of_install_programs openflows openflows-harness",
            "of_load_runtime_credentials HOOK=hook_key",
            "of_register_runtime",
            "of_resolve_github_tokens",
            "of_store_git_credentials required",
            "of_sync_controller_repository https://example/repo.git",
            "of_release_privileges",
        ]
    );
    assert_eq!(
        out.trim(),
        "session=accessrefreshboot",
        "the controller calls the Manager itself"
    );
    let (steps, _) = recorded_bootstrap(
        dir.path(),
        "OF_HOSTED=false OF_REPO_URL=https://example/repo.git of_bootstrap_controller",
    );
    assert_eq!(
        steps[steps.len() - 2..],
        [
            "of_refresh_golden_seed https://example/repo.git",
            "of_release_privileges"
        ]
    );
}

/// Run of_release_privileges with a sudo stand-in that grants root only while
/// the sudoers file exists, or always when SUDO_ALWAYS is set.
fn release_privileges(dir: &Path, hosted: bool, sudo_always: bool) -> Output {
    std::fs::write(dir.join("sudoers"), "coder ALL=(ALL) NOPASSWD:ALL\n").unwrap();
    run(
        dir,
        &format!(
            r#"OF_SUDOERS_FILE="$1/sudoers" OF_HOSTED={hosted}
sudo() {{
    if [ "$1" = -n ]; then {check}; return; fi
    "$@"
}}
of_release_privileges
echo continued"#,
            check = if sudo_always {
                "return 0"
            } else {
                r#"[ -f "$OF_SUDOERS_FILE" ]"#
            }
        ),
    )
}

#[test]
fn hosted_workspaces_give_up_sudo_before_the_role_runs() {
    let dir = tempfile::tempdir().unwrap();
    let hosted = release_privileges(dir.path(), true, false);
    assert!(hosted.status.success(), "{}", stderr(&hosted));
    assert!(!dir.path().join("sudoers").exists());
    assert!(stderr(&hosted).contains("Released sudo"));

    let local = release_privileges(dir.path(), false, false);
    assert!(local.status.success(), "{}", stderr(&local));
    assert!(
        dir.path().join("sudoers").exists(),
        "local workspaces keep sudo"
    );

    let stuck = release_privileges(dir.path(), true, true);
    assert!(!stuck.status.success());
    assert!(stderr(&stuck).contains("FATAL: sudo is still available"));
    assert!(!stdout(&stuck).contains("continued"));
}

#[test]
fn a_restarted_worker_resumes_its_assignment_branch_and_keeps_its_work() {
    let dir = tempfile::tempdir().unwrap();
    let origin = fixture_repositories(dir.path());
    let script = format!(
        "{PATHS}of_acquire_repository \"$1/origin.git\"\nof_checkout_assignment_branch '' forge-1 T-1\nof_require_repository\n"
    );
    let first = run(dir.path(), &script);
    assert!(first.status.success(), "{}", stderr(&first));
    assert!(stderr(&first).contains("Copied golden repo seed"));
    let ws = dir.path().join("ws");
    assert_eq!(
        git(dir.path(), &ws, &["rev-parse", "--abbrev-ref", "HEAD"]),
        "forge-1/T-1"
    );

    // Work done before the restart must survive it.
    std::fs::write(ws.join("work.txt"), "in progress\n").unwrap();
    git(dir.path(), &ws, &["add", "work.txt"]);
    git(dir.path(), &ws, &["commit", "-qm", "work"]);
    let before = git(dir.path(), &ws, &["rev-parse", "HEAD"]);

    let restart = run(dir.path(), &script);
    assert!(restart.status.success(), "{}", stderr(&restart));
    assert!(
        !stderr(&restart).contains("Copied golden repo seed"),
        "a restart must reuse the checkout"
    );
    assert_eq!(
        git(dir.path(), &ws, &["rev-parse", "--abbrev-ref", "HEAD"]),
        "forge-1/T-1"
    );
    assert_eq!(git(dir.path(), &ws, &["rev-parse", "HEAD"]), before);
    assert_eq!(
        git(dir.path(), &ws, &["remote", "get-url", "origin"]),
        origin.to_str().unwrap()
    );
    assert_eq!(
        sudo_calls(dir.path()),
        "",
        "repository steps must run without sudo"
    );
}

#[test]
fn a_failed_golden_copy_falls_back_to_a_direct_clone() {
    if running_as_root() {
        eprintln!("skipped: root can read the unreadable seed file");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    fixture_repositories(dir.path());
    let unreadable = dir.path().join("golden/unreadable");
    std::fs::write(&unreadable, "x").unwrap();
    Command::new("chmod")
        .args(["000", unreadable.to_str().unwrap()])
        .status()
        .unwrap();
    let output = run(
        dir.path(),
        &format!("{PATHS}of_acquire_repository \"$1/origin.git\" --checkout-default\nof_require_repository"),
    );
    Command::new("chmod")
        .args(["644", unreadable.to_str().unwrap()])
        .status()
        .unwrap();
    assert!(output.status.success(), "{}", stderr(&output));
    let log = stderr(&output);
    assert!(log.contains("copying the golden repo seed failed"), "{log}");
    assert!(log.contains("falling back to a direct clone"), "{log}");
    assert!(
        !log.contains("Copied golden repo seed"),
        "a failed copy must not be reported as done"
    );
    assert!(log.contains("Cloned repository"), "{log}");
    let ws = dir.path().join("ws");
    assert!(ws.join("README.md").is_file());
    assert!(
        !ws.join("unreadable").exists(),
        "the partial copy must be cleared before the clone"
    );
}

#[test]
fn a_direct_clone_stays_on_its_branch_and_pulls_on_restart() {
    let dir = tempfile::tempdir().unwrap();
    fixture_repositories(dir.path());
    std::fs::remove_dir_all(dir.path().join("golden")).unwrap();
    let script = format!(
        "{PATHS}of_acquire_repository \"$1/origin.git\" --checkout-default\nof_require_repository"
    );
    let first = run(dir.path(), &script);
    assert!(first.status.success(), "{}", stderr(&first));
    assert!(stderr(&first).contains("Cloned repository"));
    let ws = dir.path().join("ws");
    assert_eq!(
        git(dir.path(), &ws, &["rev-parse", "--abbrev-ref", "HEAD"]),
        "main",
        "a direct clone must keep its tracking branch"
    );

    // An upstream change must reach the checkout on the next start.
    let source = dir.path().join("source");
    std::fs::write(source.join("upstream.txt"), "new\n").unwrap();
    git(dir.path(), &source, &["add", "."]);
    git(dir.path(), &source, &["commit", "-qm", "upstream"]);
    git(
        dir.path(),
        &source,
        &[
            "push",
            "-q",
            dir.path().join("origin.git").to_str().unwrap(),
            "main",
        ],
    );

    let restart = run(dir.path(), &script);
    assert!(restart.status.success(), "{}", stderr(&restart));
    assert!(ws.join("upstream.txt").is_file(), "{}", stderr(&restart));
}

#[test]
fn rework_resumes_the_pr_branch_or_flags_a_missing_one() {
    let dir = tempfile::tempdir().unwrap();
    fixture_repositories(dir.path());
    let ws = dir.path().join("ws");
    let rework = |target: &str| {
        run(
            dir.path(),
            &format!("{PATHS}of_acquire_repository \"$1/origin.git\"\nof_checkout_assignment_branch {target} forge-1 T-1\n"),
        )
    };
    for _ in 0..2 {
        let output = rework("pr-branch");
        assert!(output.status.success(), "{}", stderr(&output));
        assert_eq!(
            git(dir.path(), &ws, &["rev-parse", "--abbrev-ref", "HEAD"]),
            "pr-branch"
        );
        assert!(
            ws.join("pr.txt").is_file(),
            "rework must start from the PR's commits"
        );
    }
    assert!(!ws.join("REWORK_BRANCH_MISMATCH.md").exists());

    let output = rework("gone-branch");
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(stderr(&output).contains("NOT the intended PR branch 'gone-branch'"));
    assert!(ws.join("REWORK_BRANCH_MISMATCH.md").is_file());
    let branches = git(dir.path(), &ws, &["branch", "--list", "gone-branch"]);
    assert_eq!(
        branches, "",
        "a missing PR branch must not be invented at the default head"
    );
}

#[test]
fn role_instructions_install_once_and_stay_out_of_commits_across_restarts() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("assets/orchestration");
    for path in [
        "agent/agents",
        "agent/roles",
        "agent/standards",
        "plugin/skills/vessel-ci-gate",
        "plugin/commands",
    ] {
        std::fs::create_dir_all(root.join(path)).unwrap();
    }
    let hooks = dir.path().join("assets/plugin/hooks/vessel");
    std::fs::create_dir_all(&hooks).unwrap();
    std::fs::write(root.join("agent/agents/vessel.agent.md"), "# VESSEL\n").unwrap();
    std::fs::write(root.join("agent/roles/vessel.skills"), "vessel-ci-gate\n\n").unwrap();
    std::fs::write(root.join("agent/standards/CODING.md"), "coding\n").unwrap();
    std::fs::write(
        root.join("plugin/skills/vessel-ci-gate/SKILL.md"),
        "skill\n",
    )
    .unwrap();
    std::fs::write(root.join("plugin/commands/merge.md"), "merge\n").unwrap();
    std::fs::write(hooks.join("session_start.sh"), "#!/bin/sh\n").unwrap();
    let workspace = dir.path().join("ws");
    std::fs::create_dir_all(&workspace).unwrap();
    git(dir.path(), &workspace, &["init", "-q"]);

    let script = format!(
        "{PATHS}ASSETS_DIR=\"$1/assets\"\n\
of_require_repository\nof_install_role_hooks vessel\nof_install_role_instructions vessel"
    );
    // The second run is a workspace restart over the same volume.
    for _ in 0..2 {
        let output = run(dir.path(), &script);
        assert!(output.status.success(), "{}", stderr(&output));
        assert!(stderr(&output).contains("Installed hosted instructions for vessel"));
    }
    assert_eq!(
        std::fs::read_to_string(workspace.join("AGENTS.md")).unwrap(),
        "# VESSEL\n"
    );
    assert!(workspace
        .join(".agents/skills/vessel-ci-gate/SKILL.md")
        .is_file());
    assert!(workspace.join(".agents/commands/merge.md").is_file());
    assert!(workspace.join("CODING.md").is_file());
    assert!(dir
        .path()
        .join(".openflows/hooks/session_start.sh")
        .is_file());
    let exclude = std::fs::read_to_string(workspace.join(".git/info/exclude")).unwrap();
    for pattern in [
        "/AGENTS.md",
        "/vessel.agent.md",
        "/.agents/skills/",
        "/.agents/commands/",
        "/CODING.md",
        "/SECURITY.md",
        "/REVIEW.md",
    ] {
        assert_eq!(
            exclude.lines().filter(|line| *line == pattern).count(),
            1,
            "{pattern} must be excluded exactly once after a restart:\n{exclude}"
        );
    }
    assert_eq!(git(dir.path(), &workspace, &["status", "--porcelain"]), "");
    assert_eq!(
        sudo_calls(dir.path()),
        "",
        "instructions and hooks must install without sudo"
    );
}

#[test]
fn a_workspace_without_a_persona_ends_startup() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("ws/.git")).unwrap();
    let output = run(
        dir.path(),
        &format!("{PATHS}ASSETS_DIR=\"$1/assets\"\nof_install_role_instructions lore"),
    );
    assert!(!output.status.success());
    assert!(stderr(&output).contains("FATAL: hosted instruction bundle is missing"));
}

#[test]
fn only_the_privileged_preparation_uses_sudo() {
    let dir = tempfile::tempdir().unwrap();
    let output = run(dir.path(), &format!("{PATHS}of_prepare --own-artifacts"));
    assert!(output.status.success(), "{}", stderr(&output));
    let calls = sudo_calls(dir.path());
    for call in calls.lines() {
        assert!(
            call.starts_with("mkdir ") || call.starts_with("chown ") || call.starts_with("find "),
            "of_prepare ran an unexpected privileged command: {call}"
        );
    }
    assert!(
        calls.contains("chown -R coder:coder"),
        "--own-artifacts must take the artifacts volume"
    );
    assert!(
        stderr(&output).contains("Workspace runtime"),
        "startup must log which runtime it loaded"
    );
}

// ── Hosted flows against a stub Manager ─────────────────────────────────────

/// A stub Manager: registration, refresh, heartbeat and the credential broker.
/// It logs `PATH AUTHORIZATION BODY` for every request.
const STUB_MANAGER: &str = r#"
import http.server, json, sys
port_file, log_file = sys.argv[1], sys.argv[2]
state = {"access": None, "refresh": "refresh-0", "n": 0}
class Handler(http.server.BaseHTTPRequestHandler):
    def log_message(self, *args):
        pass
    def reply(self, status, body=None):
        data = json.dumps(body or {}).encode()
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)
    def issue(self):
        state["n"] += 1
        state["access"] = "access-%d" % state["n"]
        state["refresh"] = "refresh-%d" % state["n"]
        return {"access_token": state["access"], "refresh_token": state["refresh"]}
    def do_POST(self):
        length = int(self.headers.get("Content-Length") or 0)
        body = self.rfile.read(length).decode()
        auth = (self.headers.get("Authorization") or "").replace("Bearer ", "")
        with open(log_file, "a") as log:
            log.write("%s %s %s\n" % (self.path, auth or "-", body or "-"))
        if self.path == "/api/v1/runtime/register":
            if json.loads(body).get("bootstrap") != "boot-1":
                return self.reply(401)
            return self.reply(200, self.issue())
        if self.path == "/api/v1/runtime/refresh":
            if auth != state["refresh"] or json.loads(body).get("refresh_token") != auth:
                return self.reply(401)
            return self.reply(200, self.issue())
        if auth != state["access"]:
            return self.reply(401)
        if self.path == "/api/v1/runtime/heartbeat":
            return self.reply(200)
        if self.path == "/api/v1/runtime/github-credentials":
            purpose = json.loads(body)["purpose"]
            return self.reply(200, {"token": "%s-token-%d" % (purpose, state["n"])})
        self.reply(404)
server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
open(port_file, "w").write(str(server.server_address[1]))
server.serve_forever()
"#;

struct Stub {
    child: Child,
    url: String,
    log: PathBuf,
}

impl Drop for Stub {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn wait_for(what: &str, timeout: Duration, mut done: impl FnMut() -> bool) {
    let start = Instant::now();
    while !done() {
        assert!(start.elapsed() < timeout, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn stub_manager(dir: &Path) -> Stub {
    let script = dir.join("stub_manager.py");
    std::fs::write(&script, STUB_MANAGER).unwrap();
    let port_file = dir.join("stub.port");
    let log = dir.join("stub.log");
    let child = Command::new("python3")
        .arg("-I")
        .arg(&script)
        .arg(&port_file)
        .arg(&log)
        .spawn()
        .unwrap();
    wait_for("the stub Manager", Duration::from_secs(10), || {
        std::fs::read_to_string(&port_file).is_ok_and(|port| !port.is_empty())
    });
    let port = std::fs::read_to_string(&port_file).unwrap();
    Stub {
        child,
        url: format!("http://127.0.0.1:{port}"),
        log,
    }
}

#[test]
fn hosted_registration_brokering_and_readiness_keep_tokens_out_of_the_log() {
    let dir = tempfile::tempdir().unwrap();
    let stub = stub_manager(dir.path());
    let output = run_with(
        dir.path(),
        r#"OF_HOSTED=true OF_CONNECTED_LOCAL=false OF_READY_FILE="$1/ready"
export OF_BOOTSTRAP=boot-1
of_register_runtime
echo "bootstrap=${OF_BOOTSTRAP:-forgotten} access=$OF_RUNTIME_ACCESS_TOKEN refresh=$OF_RUNTIME_REFRESH_TOKEN"
of_resolve_github_tokens --api
echo "git=$GIT_TOKEN api=$API_TOKEN"
of_signal_manager_ready
[ -f "$1/ready" ] && echo ready"#,
        &[("OF_MANAGER_URL", &stub.url)],
    );
    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(
        stdout(&output),
        "bootstrap=forgotten access=access-1 refresh=refresh-1\ngit=git-token-1 api=api-token-1\nready\n"
    );
    for secret in ["access-1", "refresh-1", "boot-1", "git-token-1"] {
        assert!(
            !stderr(&output).contains(secret),
            "{secret} reached the startup log"
        );
    }
    let requests = std::fs::read_to_string(&stub.log).unwrap();
    assert!(
        requests.contains(r#"/api/v1/runtime/register - {"bootstrap": "boot-1"}"#),
        "{requests}"
    );
    assert!(requests.contains(r#"/api/v1/runtime/github-credentials access-1 {"purpose":"git"}"#));
    assert!(requests.contains("/api/v1/runtime/heartbeat access-1 -"));

    let rejected = run_with(
        dir.path(),
        "OF_HOSTED=true\nexport OF_BOOTSTRAP=spent\nof_register_runtime",
        &[("OF_MANAGER_URL", &stub.url)],
    );
    assert!(!rejected.status.success());
    assert!(stderr(&rejected).contains("FATAL: Manager runtime registration failed"));
}

/// Run the refresh loop against a stub Manager until it has written FILE.
fn refresh_until(dir: &Path, stub: &Stub, connected_local: bool, file: &Path) {
    std::fs::create_dir_all(dir.join("config")).unwrap();
    std::fs::write(dir.join("ready"), "").unwrap();
    let mut child = Command::new(repo_root().join("runtime/bin/openflows-credential-refresh"))
        .arg("--api-token")
        .env("OF_MANAGER_URL", &stub.url)
        // Not a token the stub issued: the first heartbeat is refused (401).
        .env("OF_RUNTIME_ACCESS_TOKEN", "expired-access")
        .env("OF_RUNTIME_REFRESH_TOKEN", "refresh-0")
        .env("OF_CONNECTED_LOCAL", connected_local.to_string())
        .env("OF_READY_FILE", dir.join("ready"))
        .env("OF_API_TOKEN_FILE", dir.join("config/github-api-token"))
        .env("OF_GIT_CREDENTIALS", dir.join("git-credentials"))
        .env("OF_REFRESH_INTERVAL", "0.1")
        .env("OF_REFRESH_ROTATE_EVERY", "2")
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let stderr = child.stderr.take().unwrap();
    wait_for(
        "the refresh loop to rotate tokens",
        Duration::from_secs(20),
        || file.exists(),
    );
    let _ = child.kill();
    let _ = child.wait();
    let errors: Vec<_> = BufReader::new(stderr)
        .lines()
        .map_while(Result::ok)
        .collect();
    assert!(errors.is_empty(), "the refresh loop printed: {errors:?}");
}

#[test]
fn the_refresh_loop_recovers_a_rejected_session_and_rotates_github_tokens() {
    let dir = tempfile::tempdir().unwrap();
    let stub = stub_manager(dir.path());
    let api_file = dir.path().join("config/github-api-token");
    refresh_until(dir.path(), &stub, false, &api_file);
    let requests = std::fs::read_to_string(&stub.log).unwrap();
    let lines: Vec<_> = requests.lines().collect();
    assert_eq!(lines[0], "/api/v1/runtime/heartbeat expired-access -");
    assert_eq!(
        lines[1],
        r#"/api/v1/runtime/refresh refresh-0 {"refresh_token":"refresh-0"}"#
    );
    assert_eq!(lines[2], "/api/v1/runtime/heartbeat access-1 -");
    let api = std::fs::read_to_string(&api_file).unwrap();
    assert!(api.starts_with("api-token-"), "{api}");
    let git = std::fs::read_to_string(dir.path().join("git-credentials")).unwrap();
    assert!(
        git.starts_with("https://x-access-token:git-token-"),
        "{git}"
    );
}

#[test]
fn the_connected_local_refresh_loop_keeps_no_git_token_on_disk() {
    let dir = tempfile::tempdir().unwrap();
    let stub = stub_manager(dir.path());
    let api_file = dir.path().join("config/github-api-token");
    refresh_until(dir.path(), &stub, true, &api_file);
    assert!(!dir.path().join("git-credentials").exists());
    let api = std::fs::read_to_string(&api_file).unwrap();
    assert!(
        api.starts_with("git-token-"),
        "connected-local uses the git-purpose token: {api}"
    );
    let requests = std::fs::read_to_string(&stub.log).unwrap();
    assert!(!requests.contains(r#"{"purpose":"api"}"#));
}

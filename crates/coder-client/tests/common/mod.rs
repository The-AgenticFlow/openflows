//! Helpers shared by the workspace runtime tests: they source
//! runtime/lib/bootstrap.sh in bash with `sudo` replaced by a recorder.

#![allow(dead_code)] // each test binary uses only some of them

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

pub fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

pub fn runtime() -> PathBuf {
    repo_root().join("runtime/lib/bootstrap.sh")
}

/// Run SCRIPT after sourcing the runtime, with `$1` set to DIR. Every `sudo`
/// call is appended to DIR/sudo.log.
pub fn run(dir: &Path, script: &str) -> Output {
    run_with(dir, script, &[])
}

pub fn run_with(dir: &Path, script: &str, env: &[(&str, &str)]) -> Output {
    let prelude = r#"set -e
. "$RUNTIME"
sudo() {
    printf '%s\n' "$*" >> "$SUDO_LOG"
    case "$1" in chown|find) return 0 ;; -n|-H) shift; "$@" ;; *) "$@" ;; esac
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

pub fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

pub fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

pub fn sudo_calls(dir: &Path) -> String {
    std::fs::read_to_string(dir.join("sudo.log")).unwrap_or_default()
}

/// Run git in DIR with a fixed identity, isolated from the user's config.
pub fn git(home: &Path, dir: &Path, args: &[&str]) -> String {
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
pub fn fixture_repositories(dir: &Path) -> PathBuf {
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
pub const PATHS: &str = r#"OF_HOME="$1" OF_WORKSPACE="$1/ws" OF_STATE_DIR="$1/state" OF_GOLDEN_REPO="$1/golden"
OF_GIT_CREDENTIALS="$1/.git-credentials" OF_API_TOKEN_FILE="$1/.config/openflows/github-api-token"
mkdir -p "$OF_WORKSPACE" "$OF_STATE_DIR"
"#;

pub fn running_as_root() -> bool {
    let output = Command::new("id").arg("-u").output().unwrap();
    String::from_utf8_lossy(&output.stdout).trim() == "0"
}

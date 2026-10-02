//! Verification checkout tests using the workspace's Git and shell; no Docker required.
use openflows_harness::sandbox::Sandbox;
use std::process::Command;

fn repository() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    for args in [
        vec!["init"],
        vec!["config", "user.email", "test@example.com"],
        vec!["config", "user.name", "Test"],
    ] {
        assert!(Command::new("git")
            .current_dir(dir.path())
            .args(args)
            .output()
            .unwrap()
            .status
            .success());
    }
    std::fs::write(dir.path().join("source.txt"), "candidate\n").unwrap();
    std::fs::write(dir.path().join(".gitignore"), "private.env\n").unwrap();
    std::fs::write(dir.path().join("private.env"), "TEST_SECRET=fixture-only\n").unwrap();
    assert!(Command::new("git")
        .current_dir(dir.path())
        .args(["add", "."])
        .status()
        .unwrap()
        .success());
    assert!(Command::new("git")
        .current_dir(dir.path())
        .args(["commit", "-m", "candidate"])
        .output()
        .unwrap()
        .status
        .success());
    dir
}

#[test]
fn checkout_runs_with_workspace_tools_and_collects_artifacts() {
    let repo = repository();
    let argv = [
        "sh",
        "-c",
        r#"
        test -d .git && test ! -e private.env &&
        test "$OPENFLOWS_TOOL_PROBE" = fixture-only &&
        test -n "$HOME" && test "$CI" = true &&
        git rev-parse HEAD &&
        echo build > generated.txt &&
        echo harmless-output
    "#,
    ]
    .map(str::to_owned);
    let sandbox = Sandbox::create(repo.path(), &argv).unwrap();
    let output = sandbox
        .command()
        .env("OPENFLOWS_TOOL_PROBE", "fixture-only")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("harmless-output"));
    assert!(String::from_utf8_lossy(&output.stdout).contains(&sandbox.head));
    assert!(sandbox.source_unchanged().unwrap());
    let artifacts = sandbox.artifacts(&["generated.txt".into()]).unwrap();
    assert_eq!(artifacts.len(), 1);
    let bytes: Vec<u8> = serde_json::from_str(&artifacts[0].1).unwrap();
    assert_eq!(bytes, b"build\n");
    assert!(sandbox.artifacts(&["../../etc/passwd".into()]).is_err());
    assert!(!repo.path().join("generated.txt").exists());
}

#[test]
fn sandbox_source_mutation_cannot_be_used_as_clean_head_evidence() {
    let repo = repository();
    let argv = ["sh", "-c", "echo changed > source.txt"].map(str::to_owned);
    let sandbox = Sandbox::create(repo.path(), &argv).unwrap();
    assert!(sandbox.command().status().unwrap().success());
    assert!(!sandbox.source_unchanged().unwrap());
    assert_eq!(
        std::fs::read_to_string(repo.path().join("source.txt")).unwrap(),
        "candidate\n"
    );
}

#[test]
fn checkout_preserves_failure_exit_and_cleans_up_directory() {
    let repo = repository();
    let argv = ["sh", "-c", "exit 7"].map(str::to_owned);
    let sandbox = Sandbox::create(repo.path(), &argv).unwrap();
    let checkout = sandbox.checkout_path();
    assert!(checkout.exists());
    assert_eq!(sandbox.command().status().unwrap().code(), Some(7));
    drop(sandbox);
    assert!(!checkout.exists());
    assert!(repo.path().join("source.txt").exists());
}

#[test]
fn dirty_candidate_is_rejected() {
    let repo = repository();
    std::fs::write(repo.path().join("source.txt"), "uncommitted").unwrap();
    let error = Sandbox::create(repo.path(), &["true".into()])
        .err()
        .unwrap();
    assert!(error.to_string().contains("clean candidate"));
}

#[test]
fn artifacts_cannot_follow_links_outside_checkout() {
    let repo = repository();
    let sandbox = Sandbox::create(repo.path(), &["true".into()]).unwrap();
    std::os::unix::fs::symlink(repo.path(), sandbox.checkout_path().join("outside")).unwrap();
    assert!(sandbox.artifacts(&["outside/source.txt".into()]).is_err());
    std::os::unix::fs::symlink("source.txt", sandbox.checkout_path().join("link")).unwrap();
    assert!(sandbox.artifacts(&["link".into()]).is_err());
}

#[test]
fn checkout_git_changes_do_not_modify_forge_branch_or_index() {
    let repo = repository();
    let original = Command::new("git")
        .current_dir(repo.path())
        .args(["symbolic-ref", "HEAD"])
        .output()
        .unwrap()
        .stdout;
    let sandbox = Sandbox::create(
        repo.path(),
        &[
            "git".into(),
            "checkout".into(),
            "-b".into(),
            "test-branch".into(),
        ],
    )
    .unwrap();
    assert!(sandbox.command().status().unwrap().success());
    assert_eq!(
        Command::new("git")
            .current_dir(repo.path())
            .args(["symbolic-ref", "HEAD"])
            .output()
            .unwrap()
            .stdout,
        original
    );
    assert!(Command::new("git")
        .current_dir(repo.path())
        .args(["status", "--porcelain"])
        .output()
        .unwrap()
        .stdout
        .is_empty());
}

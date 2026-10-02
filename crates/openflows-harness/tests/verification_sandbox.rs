//! Docker-backed boundary tests. Run with --ignored; uses the local redis:7-alpine image.
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
#[ignore = "requires Docker and redis:7-alpine"]
fn sandbox_protects_checkout_credentials_network_and_host_files() {
    let repo = repository();
    let argv = [
        "busybox",
        "sh",
        "-c",
        r#"
        test -z "$REDIS_URL$CODER_AGENT_TOKEN$GITHUB_TOKEN" &&
        test ! -e /var/run/docker.sock && test ! -e /home/coder &&
        test ! -e /workspace/repo/.git && test ! -e private.env &&
        test -z "$REDIS_VERSION$OPENFLOWS_SECRET_PROBE" &&
        test "$(wc -l </proc/net/route)" -eq 1 &&
        echo build > generated.txt &&
        echo harmless-output
    "#,
    ]
    .map(str::to_owned);
    let sandbox = Sandbox::create_with_image(repo.path(), &argv, "redis:7-alpine").unwrap();
    let output = sandbox
        .command()
        .env("OPENFLOWS_SECRET_PROBE", "fixture-only")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("harmless-output"));
    assert!(sandbox.source_unchanged().unwrap());
    let artifacts = sandbox.artifacts(&["generated.txt".into()]).unwrap();
    assert_eq!(artifacts.len(), 1);
    let bytes: Vec<u8> = serde_json::from_str(&artifacts[0].1).unwrap();
    assert_eq!(bytes, b"build\n");
    assert!(sandbox.artifacts(&["../../etc/passwd".into()]).is_err());
    assert!(!repo.path().join("generated.txt").exists());
}

#[test]
#[ignore = "requires Docker and redis:7-alpine"]
fn sandbox_source_mutation_cannot_be_used_as_clean_head_evidence() {
    let repo = repository();
    let argv = ["busybox", "sh", "-c", "echo changed > source.txt"].map(str::to_owned);
    let sandbox = Sandbox::create_with_image(repo.path(), &argv, "redis:7-alpine").unwrap();
    assert!(sandbox.command().status().unwrap().success());
    assert!(!sandbox.source_unchanged().unwrap());
    assert_eq!(
        std::fs::read_to_string(repo.path().join("source.txt")).unwrap(),
        "candidate\n"
    );
}

#[test]
#[ignore = "requires Docker and redis:7-alpine"]
fn sandbox_preserves_failure_exit_and_cleans_up_container() {
    let repo = repository();
    let argv = ["busybox", "sh", "-c", "exit 7"].map(str::to_owned);
    let sandbox = Sandbox::create_with_image(repo.path(), &argv, "redis:7-alpine").unwrap();
    let command = sandbox.command();
    let name = command.get_args().last().unwrap().to_owned();
    assert_eq!(sandbox.command().status().unwrap().code(), Some(7));
    drop(sandbox);
    assert!(!Command::new("docker")
        .arg("inspect")
        .arg(name)
        .output()
        .unwrap()
        .status
        .success());
}

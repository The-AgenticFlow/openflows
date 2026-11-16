//! How the shared workspace runtime acquires and refreshes the repository
//! checkout (runtime/lib/bootstrap.sh), against real git repositories.

mod common;

use common::{fixture_repositories, git, run, running_as_root, stderr, sudo_calls, PATHS};
use std::process::Command;

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

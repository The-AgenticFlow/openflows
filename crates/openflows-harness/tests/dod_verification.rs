//! Execution/gate regressions, not a deterministic SENTINEL strategy selector.
//! Model reasoning is evaluated separately with tests/evals/sentinel-testing.md.
use config::lifecycle::{Event, Lifecycle, Phase};
use openflows_harness::sandbox::Sandbox;
use std::path::Path;
use std::process::{Command, Output};

fn git(repo: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .current_dir(repo)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

fn candidate(files: &[(&str, &str)]) -> tempfile::TempDir {
    let repo = tempfile::tempdir().unwrap();
    git(repo.path(), &["init"]);
    git(repo.path(), &["config", "user.name", "Fixture"]);
    git(
        repo.path(),
        &["config", "user.email", "fixture@example.com"],
    );
    for (path, content) in files {
        std::fs::write(repo.path().join(path), content).unwrap();
    }
    git(repo.path(), &["add", "."]);
    git(repo.path(), &["commit", "-m", "candidate"]);
    repo
}

fn check(repo: &Path, argv: &[&str]) -> (Sandbox, Output) {
    let sandbox = Sandbox::create(
        repo,
        &argv.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
    )
    .unwrap();
    let result = sandbox.command().output().unwrap();
    assert!(sandbox.source_unchanged().unwrap());
    (sandbox, result)
}

#[test]
fn prose_assertion_establishes_content_at_candidate_without_rust() {
    let home = tempfile::tempdir().unwrap();
    // Isolate executable lookup after login-shell initialization. System tool
    // directories may contain Cargo, so expose only the content check's tool.
    let bin = home.path().join("bin");
    std::fs::create_dir(&bin).unwrap();
    let grep = Command::new("bash")
        .args(["-c", "type -P grep"])
        .output()
        .unwrap();
    assert!(grep.status.success());
    let grep = std::fs::canonicalize(String::from_utf8(grep.stdout).unwrap().trim()).unwrap();
    std::os::unix::fs::symlink(grep, bin.join("grep")).unwrap();
    std::fs::write(
        home.path().join(".bash_profile"),
        "export PATH=\"$HOME/bin\"\n",
    )
    .unwrap();
    for content in [
        "# Setup\nSupported port: 8080\n",
        "# Setup\nSupported port: 9999\n",
    ] {
        let repo = candidate(&[("README.md", content)]);
        // This asserts the DoD property, unlike printing the document or echo.
        let sandbox = Sandbox::create(
            repo.path(),
            &["grep", "-Fx", "Supported port: 8080", "README.md"].map(str::to_owned),
        )
        .unwrap();
        let result = sandbox.command().env("HOME", home.path()).output().unwrap();
        let rust_probe =
            Sandbox::create(repo.path(), &["cargo".into(), "--version".into()]).unwrap();
        assert_eq!(
            rust_probe
                .command()
                .env("HOME", home.path())
                .output()
                .unwrap()
                .status
                .code(),
            Some(127)
        );
        assert!(sandbox.source_unchanged().unwrap());
        assert_eq!(sandbox.head, git(repo.path(), &["rev-parse", "HEAD"]));
        assert_eq!(result.status.success(), content.contains("8080"));
        assert!(!sandbox.checkout_path().join("target").exists());
    }
}

#[test]
fn executable_documentation_needs_behavior_beyond_content_presence() {
    for example in ["printf '%s\\n' 42", "printf '%s\\n' 41"] {
        let repo = candidate(&[
            ("README.md", "Run: sh example.sh\nExpected: 42\n"),
            ("example.sh", example),
        ]);
        let (_, static_result) = check(repo.path(), &["grep", "-Fx", "Expected: 42", "README.md"]);
        assert!(
            static_result.status.success(),
            "presence alone misses the broken example"
        );
        let (_, behavior) = check(repo.path(), &["sh", "-c", "test \"$(sh example.sh)\" = 42"]);
        assert_eq!(behavior.status.success(), example.contains("42"));
    }
}

#[test]
fn rust_behavior_check_catches_bad_implementation() {
    for actual in [42, 41] {
        let source = format!("fn answer() -> i32 {{ {actual} }}\n#[test] fn expected_answer() {{ assert_eq!(answer(), 42); }}\n");
        let repo = candidate(&[("example.rs", &source)]);
        // A targeted compiler/test command selected for this fixture's DoD.
        let (_, result) = check(
            repo.path(),
            &[
                "sh",
                "-c",
                "rustc --test example.rs -o example-test && ./example-test",
            ],
        );
        assert_eq!(
            result.status.success(),
            actual == 42,
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
    }
}

#[test]
fn non_rust_repository_uses_its_own_behavior_check() {
    for actual in [42, 41] {
        let source = format!("def answer():\n    return {actual}\n\nassert answer() == 42\n");
        let repo = candidate(&[("test_answer.py", &source)]);
        let (_, result) = check(repo.path(), &["python3", "test_answer.py"]);
        assert_eq!(result.status.success(), actual == 42);
    }
}

#[test]
fn irrelevant_missing_tool_can_be_replaced_but_necessary_tool_leaves_gap() {
    let repo = candidate(&[("README.md", "Supported port: 8080\n")]);
    let (_, missing) = check(repo.path(), &["openflows-dod-fixture-missing-tool"]);
    assert_eq!(missing.status.code(), Some(127));
    assert!(String::from_utf8_lossy(&missing.stderr).contains("EXECUTOR_SETUP"));
    // The harness does not automatically repair or classify tools: the reviewer
    // selects a meaningful replacement only when it establishes the same DoD.
    let (_, relevant) = check(
        repo.path(),
        &["grep", "-Fx", "Supported port: 8080", "README.md"],
    );
    assert!(relevant.status.success());
    // For a compiled-example DoD, this content assertion would leave a gap.
    let mut state = testing("candidate");
    let result = state.apply(
        "sentinel",
        decision(
            &state,
            true,
            "Required compiler unavailable; runtime criterion unverified",
        ),
        1,
    );
    assert!(
        result.is_err(),
        "missing successful A2A evidence must still block approval"
    );
    state = state
        .apply(
            "sentinel",
            decision(&state, false, "Repair required compiler in building"),
            1,
        )
        .unwrap();
    assert_eq!(state.phase, Phase::Building);
    assert!(state.verification_task.is_none());
    assert_eq!(state.plan, "# DoD\nRequired evidence for this candidate");
}

fn testing(head: &str) -> Lifecycle {
    Lifecycle {
        phase: Phase::Testing,
        revision: 3,
        review_round: 7,
        head: Some(head.into()),
        plan: "# DoD\nRequired evidence for this candidate".into(),
        ..Default::default()
    }
}

fn decision(state: &Lifecycle, approved: bool, report: &str) -> Event {
    Event::Decide {
        round: state.review_round,
        phase: Phase::Testing,
        approved,
        report: report.into(),
        revision: state.revision,
        head: state.head.clone(),
    }
}

#[test]
fn inspection_only_report_cannot_replace_a2a_or_stale_review_bindings() {
    let state = testing("candidate");
    assert!(state
        .apply(
            "sentinel",
            decision(&state, true, "Inspected all prose criteria"),
            1
        )
        .is_err());
    assert!(state
        .apply(
            "sentinel",
            Event::Verified {
                head: "old-candidate".into(),
                task: "task-old".into()
            },
            1
        )
        .is_err());
    let verified = state
        .apply(
            "sentinel",
            Event::Verified {
                head: "candidate".into(),
                task: "task-content-check".into(),
            },
            1,
        )
        .unwrap();
    for (revision, round, head) in [
        (2, 7, "candidate"),
        (3, 6, "candidate"),
        (3, 7, "old-candidate"),
    ] {
        let decision = Event::Decide {
            revision,
            round,
            head: Some(head.into()),
            phase: Phase::Testing,
            approved: true,
            report: "Content established".into(),
        };
        assert!(verified.apply("sentinel", decision, 1).is_err());
    }
    // Successful execution does not replace a reviewer's sufficiency decision.
    let rejected = verified
        .apply(
            "sentinel",
            decision(&verified, false, "One criterion still lacks evidence"),
            1,
        )
        .unwrap();
    assert_eq!(rejected.phase, Phase::Building);
    assert!(rejected.verified_head.is_none());
    assert!(rejected.verification_task.is_none());
    // Clarifying the contract uses FORGE's existing explicit planning path,
    // never a fabricated A2A result or SENTINEL editing the approved plan.
    let planning = rejected
        .apply(
            "forge",
            Event::Move {
                phase: Phase::Planning,
                head: None,
            },
            1,
        )
        .unwrap();
    let revised = planning
        .apply(
            "forge",
            Event::Plan {
                content: "# Clarified DoD\nAssert the agreed content property".into(),
            },
            1,
        )
        .unwrap();
    assert_eq!(revised.revision, 4);
    assert!(revised.head.is_none());
    assert!(revised.verified_head.is_none());
    assert!(
        revised.plan_decision.is_none(),
        "revised DoD requires fresh approval"
    );
    assert_eq!(
        verified
            .apply(
                "sentinel",
                decision(&verified, true, "All criteria established"),
                1
            )
            .unwrap()
            .phase,
        Phase::Submit
    );
}

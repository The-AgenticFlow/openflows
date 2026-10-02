//! Shared A2A protocol types for delegated verification between SENTINEL and
//! FORGE (tracking issue: The-AgenticFlow/openflows#143).
//!
//! Scope (task 1 of `.kilo/plans/1785948146715-sentinel-forge-a2a-verify.md`):
//! serde types + Redis key helpers for the `verify` A2A task type, shared by
//! the nexus relay (crates/agent-nexus) and the harness CLI
//! (crates/openflows-harness). This crate defines the wire contract only; it
//! does not implement the relay, the executor sandboxing, or the harness
//! subcommands — those are later tasks in the plan.

mod keys;
mod verify;

pub use keys::{audit_rejected_key, audit_task_key, verification_key};
pub use verify::{
    ExecutorInfo, VerifyArtifact, VerifyCwd, VerifyExpect, VerifyKind, VerifyProgressEvent,
    VerifyRequest, VerifyResult,
};

/// The only `task_type` value this crate currently defines. Reserved so the
/// wire format can add other task types later without a breaking change to
/// this constant's meaning.
pub const TASK_TYPE_VERIFY: &str = "verify";

/// Compatibility predicate: unknown tools are permitted; known dangerous operations are denied.
/// This is a diagnostic guard, not a sandbox. All execution requires isolation.
pub fn is_allowlisted(argv: &[String]) -> bool {
    !argv.is_empty() && prohibited_operation(argv).is_none()
}

fn prohibited_operation(argv: &[String]) -> Option<&'static str> {
    let program = argv.first()?.rsplit('/').next()?;
    let args = &argv[1..];
    if matches!(
        program,
        "sudo"
            | "su"
            | "redis-cli"
            | "docker"
            | "podman"
            | "mount"
            | "umount"
            | "mkfs"
            | "dd"
            | "shutdown"
            | "reboot"
            | "openflows-harness"
    ) {
        return Some("privileged, destructive, or coordination-control operation");
    }
    if program == "rm" && args.iter().any(|a| a == "/" || a == "/*") {
        return Some("filesystem-root deletion");
    }
    if program == "git" && args.first().is_some_and(|a| a == "push") {
        return Some("verification cannot push repository changes");
    }
    let subcommand = args.first().map(String::as_str);
    let script = args.get(1).map(String::as_str);
    if (matches!(program, "cargo" | "npm" | "pnpm")
        && matches!(subcommand, Some("publish" | "deploy")))
        || (matches!(program, "npm" | "pnpm")
            && subcommand == Some("run")
            && matches!(script, Some("publish" | "deploy")))
    {
        return Some("publishing or deployment is not verification");
    }
    None
}

/// Validate literal argv without splitting strings or invoking a shell.
/// Report malformed command tokenization separately from unsupported commands.
pub fn validate_command_argv(argv: &[String]) -> anyhow::Result<()> {
    anyhow::ensure!(
        !argv.is_empty(),
        "Missing verification command. Use: verify request --expect-exit 0 -- cargo test"
    );
    anyhow::ensure!(
        !argv[0].chars().any(char::is_whitespace),
        "The executable contains whitespace: the command was passed as a single argument. Use `verify request --expect-exit 0 -- cargo test --workspace --all-features`, or one --argv per token. Do not quote the whole command. Received argv: {:?}", argv
    );
    if let Some(reason) = prohibited_operation(argv) {
        anyhow::bail!("verification policy denied: {reason}; argv: {:?}", argv);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allowlist_accepts_known_prefixes() {
        assert!(is_allowlisted(&[
            "cargo".into(),
            "test".into(),
            "--package".into(),
            "foo".into()
        ]));
        assert!(is_allowlisted(&["npm".into(), "test".into()]));
    }

    #[test]
    fn policy_rejects_known_dangerous_or_empty() {
        assert!(!is_allowlisted(&["rm".into(), "-rf".into(), "/".into()]));
        assert!(!is_allowlisted(&[]));
        assert!(!is_allowlisted(&["cargo".into(), "publish".into()]));
    }
}

#[cfg(test)]
mod verification_script_tests {
    use super::*;
    #[test]
    fn permits_only_named_npm_verification_scripts() {
        for script in ["test", "test:unit", "build", "lint"] {
            assert!(
                is_allowlisted(&["npm".into(), "run".into(), script.into()]),
                "{script}"
            );
        }
        for script in ["deploy", "publish"] {
            assert!(!is_allowlisted(&[
                "npm".into(),
                "run".into(),
                script.into()
            ]));
        }
    }
}

#[cfg(test)]
mod sandbox_policy_tests {
    use super::*;
    #[test]
    fn unfamiliar_project_commands_are_not_policy_failures() {
        for argv in [
            vec!["pytest", "-q"],
            vec!["go", "test", "./..."],
            vec!["./scripts/check-custom"],
            vec!["python3", "-m", "unittest"],
            vec!["npm", "run", "typecheck"],
            vec!["echo", "hello"],
            vec!["cargo", "test", "publish"],
            vec!["git", "log", "--grep", "push"],
            vec!["sh", "-c", "printf test"],
        ] {
            validate_command_argv(&argv.into_iter().map(str::to_owned).collect::<Vec<_>>())
                .unwrap();
        }
    }
}

//! `verify` A2A task payloads.
//!
//! See `.kilo/plans/1785948146715-sentinel-forge-a2a-verify.md` for the
//! full design (section "A2A task type: `verify`") and
//! `docs/architecture/a2a-verification.md` (task 7) for the narrative
//! version of this schema.

use serde::{Deserialize, Serialize};

/// Kind of verification being requested. Only `Command` is implemented in
/// v1; `ArtifactCheck` is reserved for a future task that verifies file
/// hashes without running a command.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VerifyKind {
    Command,
    ArtifactCheck,
}

/// Where the executor should run the command, relative to its own
/// workspace. The relay rejects any request whose resolved path would
/// escape the pair's repo/worktree root.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VerifyCwd {
    Repo,
    Worktree,
}

/// Expected outcome, used by the requester to decide pass/fail without
/// re-deriving it from raw exit codes at every call site.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct VerifyExpect {
    /// Expected process exit code. `None` means "any exit code is
    /// acceptable" (rare; mainly for smoke-test style commands).
    pub exit_code: Option<i32>,
    /// Optional list of repo-relative paths whose sha256 should be
    /// reported back in the result (not diffed here; the caller compares).
    #[serde(default)]
    pub artifacts: Vec<String>,
}

/// Sentinel → Forge (via nexus relay) verification request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerifyRequest {
    pub pair_id: String,
    pub kind: VerifyKind,
    pub cwd: VerifyCwd,
    pub argv: Vec<String>,
    pub timeout_secs: u64,
    #[serde(default)]
    pub env_allowlist: Vec<String>,
    #[serde(default)]
    pub expect: VerifyExpect,
}

impl VerifyRequest {
    /// Idempotency key per the plan's dedup rule:
    /// `(pair_id, sha256(request_body))`. Callers hash the canonical JSON
    /// encoding of this struct; exposed here so both the relay and any
    /// client share the exact same derivation.
    pub fn idempotency_seed(&self) -> anyhow::Result<String> {
        let body = serde_json::to_string(self)?;
        Ok(format!("{}:{}", self.pair_id, body))
    }
}

/// A single content-addressed artifact reported back by the executor.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerifyArtifact {
    pub path: String,
    pub sha256: String,
}

/// Identifies which workspace actually ran the command. v1 is always
/// `role: "forge"`; kept structured so a future dedicated `verifier` role
/// slots in without a schema change.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutorInfo {
    pub role: String,
    pub workspace: String,
}

/// Terminal result of a `verify` task, mirrored to
/// `pair:{id}:verification` in Redis before the A2A task is acked complete.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerifyResult {
    /// Clean checkout commit before and after execution; absent results cannot authorize lifecycle gates.
    #[serde(default)]
    pub head_sha: Option<String>,
    pub task_id: String,
    pub exit_code: Option<i32>,
    pub timed_out: bool,
    pub duration_ms: u64,
    pub stdout_ref: String,
    pub stderr_ref: String,
    #[serde(default)]
    pub artifacts: Vec<VerifyArtifact>,
    pub executor: ExecutorInfo,
}

impl VerifyResult {
    /// True iff the result satisfies the request's `expect` clause.
    /// A timed-out task never passes, regardless of `expect.exit_code`.
    pub fn satisfies(&self, expect: &VerifyExpect) -> bool {
        if self.timed_out {
            return false;
        }
        match expect.exit_code {
            Some(expected) => self.exit_code == Some(expected),
            None => true,
        }
    }
}

/// Incremental SSE progress event streamed while a `verify` task runs.
/// Only the bounded tail of these is persisted (see
/// `docs/architecture/a2a-verification.md` size caps).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "stream", rename_all = "snake_case")]
pub enum VerifyProgressEvent {
    Stdout { chunk: String },
    Stderr { chunk: String },
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_request() -> VerifyRequest {
        VerifyRequest {
            pair_id: "T-048".to_string(),
            kind: VerifyKind::Command,
            cwd: VerifyCwd::Repo,
            argv: vec!["cargo".into(), "test".into()],
            timeout_secs: 600,
            env_allowlist: vec!["CI".into()],
            expect: VerifyExpect {
                exit_code: Some(0),
                artifacts: vec![],
            },
        }
    }

    #[test]
    fn request_roundtrips_through_json() {
        let req = sample_request();
        let json = serde_json::to_string(&req).unwrap();
        let decoded: VerifyRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.pair_id, "T-048");
        assert_eq!(decoded.argv, vec!["cargo", "test"]);
        assert_eq!(decoded.expect.exit_code, Some(0));
    }

    #[test]
    fn idempotency_seed_is_stable_and_pair_scoped() {
        let a = sample_request();
        let mut b = sample_request();
        b.pair_id = "T-049".to_string();

        let seed_a1 = a.idempotency_seed().unwrap();
        let seed_a2 = a.idempotency_seed().unwrap();
        let seed_b = b.idempotency_seed().unwrap();

        assert_eq!(seed_a1, seed_a2);
        assert_ne!(seed_a1, seed_b);
        assert!(seed_a1.starts_with("T-048:"));
    }

    #[test]
    fn result_satisfies_checks_exit_code_and_timeout() {
        let expect = VerifyExpect {
            exit_code: Some(0),
            artifacts: vec![],
        };
        let ok = VerifyResult {
            head_sha: None,
            task_id: "t1".into(),
            exit_code: Some(0),
            timed_out: false,
            duration_ms: 10,
            stdout_ref: "audit:a2a:t1:stdout".into(),
            stderr_ref: "audit:a2a:t1:stderr".into(),
            artifacts: vec![],
            executor: ExecutorInfo {
                role: "forge".into(),
                workspace: "forge-T-048".into(),
            },
        };
        assert!(ok.satisfies(&expect));

        let mut failed = ok.clone();
        failed.exit_code = Some(1);
        assert!(!failed.satisfies(&expect));

        let mut timed_out = ok.clone();
        timed_out.timed_out = true;
        timed_out.exit_code = Some(0);
        assert!(!timed_out.satisfies(&expect));

        let any_expect = VerifyExpect::default();
        assert!(failed.satisfies(&any_expect));
    }

    #[test]
    fn progress_event_serde_tag_shape() {
        let ev = VerifyProgressEvent::Stdout {
            chunk: "hello".into(),
        };
        let json = serde_json::to_value(&ev).unwrap();
        assert_eq!(json["stream"], "stdout");
        assert_eq!(json["chunk"], "hello");
    }
}

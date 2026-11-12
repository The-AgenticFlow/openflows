//! LORE's post-merge documentation commands (`openflows-harness docs`) and
//! the operator's `openflows gate docs` writes.

use super::*;

/// What a docs checkout submits: see [`HarnessStore::docs_submission`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocsSubmission {
    pub branch: String,
    pub head: String,
    pub base: String,
    pub files: Vec<String>,
}

/// Only LORE submits post-merge documentation; approvals belong to the
/// operator CLI (and are sealed with the controller's docs gate keys), and the
/// controller opens the docs PR, so a worker can never approve its own docs.
pub fn authorize_docs_author(role: &str) -> Result<()> {
    if !role.eq_ignore_ascii_case("lore") {
        bail!(
            "Docs command rejected: role '{}' is not LORE. Only LORE submits \
             documentation; operator decisions use `openflows gate docs`.",
            role
        );
    }
    Ok(())
}

impl HarnessStore {
    /// Print the post-merge documentation gate record for this ticket.
    pub async fn docs_status(&self, ticket: &str) -> Result<()> {
        println!(
            "{}",
            serde_json::to_string(&self.lifecycle.docs_record(ticket).await?)?
        );
        Ok(())
    }

    /// Print every docs record that waits on the operator (`docs_ready`) or
    /// is paused, one JSON object per line, so nothing waits unnoticed.
    pub async fn docs_pending(&self) -> Result<()> {
        let mut tickets: Vec<String> = self
            .lifecycle
            .keys("ticket:*:docs")
            .await
            .iter()
            .filter_map(|key| {
                let rest = &key[key.rfind("ticket:")? + "ticket:".len()..];
                rest.strip_suffix(":docs").map(str::to_string)
            })
            .collect();
        tickets.sort();
        tickets.dedup();
        for ticket in tickets {
            let record = self.lifecycle.docs_record(&ticket).await?;
            if record.phase == DocsPhase::DocsReady || record.paused {
                println!(
                    "{}",
                    serde_json::json!({
                        "ticket": ticket,
                        "phase": record.phase.as_str(),
                        "paused": record.paused,
                        "pause_reason": record.pause_reason,
                        "revision": record.revision,
                        "branch": record.branch,
                        "head": record.head,
                        "files": record.files,
                        "report": record.report,
                    })
                );
            }
        }
        Ok(())
    }

    /// Operator decision, pause, resume, restart, skip or reopen of the
    /// documentation gate. The reducer enforces the human actor and the exact
    /// revision/head binding. A decision may name the head by a unique prefix
    /// (at least 7 characters, as GitHub shows it); approvals are sealed with
    /// `keys`, the controller's docs gate keys, which NEXUS and VESSEL verify.
    pub async fn docs_operator(
        &self,
        ticket: &str,
        event: DocsEvent,
        keys: Option<&DocsGateKeys>,
    ) -> Result<DocsRecord> {
        let state = self.lifecycle.docs_record(ticket).await?;
        let event = Self::complete_docs_decision(ticket, &state, event, keys)?;
        // The docs record and the operator's latest decision (kept where
        // workspaces cannot write it) commit together under one version
        // check; see `config::docs_lifecycle::control`.
        self.lifecycle
            .docs_operator_transition(ticket, state.version, event)
            .await
    }

    /// Expand a short head and seal an approval for [`Self::docs_operator`].
    pub fn complete_docs_decision(
        ticket: &str,
        state: &DocsRecord,
        event: DocsEvent,
        keys: Option<&DocsGateKeys>,
    ) -> Result<DocsEvent> {
        let DocsEvent::Decide {
            revision,
            head,
            approved,
            report,
            ..
        } = event
        else {
            return Ok(event);
        };
        let head = head.trim().to_ascii_lowercase();
        let head = match state.head.as_deref() {
            Some(submitted) if head.len() >= 7 && submitted.starts_with(&head) => {
                submitted.to_string()
            }
            _ => head,
        };
        let seal = if approved {
            let keys = keys.context(
                "Approving docs requires the controller's docs gate secret (OPENFLOWS_DOCS_GATE_SECRET, or CODER_CHAT_HOOK_SECRET) in this environment; run `openflows gate docs` where the controller runs",
            )?;
            keys.seal(ticket, revision, &head)
        } else {
            String::new()
        };
        Ok(DocsEvent::Decide {
            revision,
            head,
            approved,
            report,
            seal,
        })
    }

    /// LORE submits a pushed, clean docs branch for operator review.
    pub async fn docs_submit(&self, ticket: &str, role: &str, report_path: &Path) -> Result<()> {
        authorize_docs_author(role)?;
        let report = std::fs::read_to_string(report_path).context(format!(
            "Failed to read report file: {}",
            report_path.display()
        ))?;
        let state = self.lifecycle.docs_record(ticket).await?;
        anyhow::ensure!(
            !state.paused,
            "Documentation is paused by the operator; stop and wait"
        );
        let submission = Self::docs_submission(Path::new("."), &state.expected_branch(ticket))?;
        let next = self
            .lifecycle
            .docs_transition(
                ticket,
                state.version,
                role,
                DocsEvent::Submit {
                    branch: submission.branch,
                    head: submission.head,
                    base: submission.base,
                    report,
                    files: submission.files,
                },
            )
            .await?;
        println!(
            "Docs revision {} submitted at {} ({} files); wait for the operator decision",
            next.revision,
            next.head.as_deref().unwrap_or_default(),
            next.files.len()
        );
        Ok(())
    }

    /// Inspect the docs checkout in `dir`: it must be clean, on `expected`,
    /// and pushed; returns what the docs branch changes against the default
    /// branch. Renames are listed as a deletion plus an addition, so moving a
    /// source file into `docs/` is caught by the documentation-only policy.
    pub fn docs_submission(dir: &Path, expected: &str) -> Result<DocsSubmission> {
        let head = Self::clean_head_in(dir, "Commit all documentation changes before submitting")?;
        let branch = Self::git(dir, &["rev-parse", "--abbrev-ref", "HEAD"])
            .context("Cannot identify checkout branch")?;
        anyhow::ensure!(
            branch == expected,
            "Docs must be submitted from branch {expected}; current branch is {branch}"
        );
        anyhow::ensure!(
            Self::remote_head(dir, &branch)?.as_deref() == Some(head.as_str()),
            "Push branch {branch} to origin before submitting; the operator reviews the pushed head {head}"
        );
        let base = Self::default_branch(dir)?;
        let _ = Self::git(dir, &["fetch", "--quiet", "origin", &base]);
        let range = format!("origin/{base}...{head}");
        let files = Self::git(
            dir,
            &[
                "diff",
                "--no-renames",
                "--no-ext-diff",
                "--name-only",
                // NUL-separated raw paths: without -z, Git quotes and escapes
                // non-ASCII names (`"docs/fran\303\247ais.md"`).
                "-z",
                &range,
            ],
        )
        .context("Cannot list the files changed by the docs branch")?
        .split('\0')
        .filter(|path| !path.is_empty())
        .map(str::to_string)
        .collect();
        Ok(DocsSubmission {
            branch,
            head,
            base,
            files,
        })
    }

    /// The repository default branch, from `origin/HEAD` (set by clone).
    fn default_branch(dir: &Path) -> Result<String> {
        let read = || {
            Self::git(
                dir,
                &["symbolic-ref", "--short", "refs/remotes/origin/HEAD"],
            )
        };
        let symbolic = match read() {
            Ok(value) => value,
            Err(_) => {
                Self::git(dir, &["remote", "set-head", "origin", "--auto"])
                    .context("Cannot determine the default branch of origin")?;
                read()?
            }
        };
        symbolic
            .strip_prefix("origin/")
            .filter(|branch| !branch.is_empty())
            .map(str::to_string)
            .context("origin/HEAD does not name an origin branch")
    }

    fn remote_head(dir: &Path, branch: &str) -> Result<Option<String>> {
        let listing = Self::git(dir, &["ls-remote", "--heads", "origin", branch])
            .with_context(|| format!("Cannot read branch {branch} from origin"))?;
        let wanted = format!("refs/heads/{branch}");
        Ok(listing.lines().find_map(|line| {
            let (sha, reference) = line.split_once('\t')?;
            (reference == wanted).then(|| sha.to_string())
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A clone of a bare origin whose `main` holds `src/lib.rs`, checked out
    /// on a pushed `lore/T-1` branch.
    fn docs_checkout() -> (tempfile::TempDir, std::path::PathBuf) {
        let root = tempfile::tempdir().unwrap();
        let run = |dir: &Path, args: &[&str]| {
            let out = std::process::Command::new("git")
                .current_dir(dir)
                .args(["-c", "user.email=t@t", "-c", "user.name=t"])
                .args(args)
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "git {args:?}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        };
        let seed = root.path().join("seed");
        std::fs::create_dir_all(seed.join("src")).unwrap();
        run(&seed, &["init", "-q", "-b", "main"]);
        std::fs::write(seed.join("src/lib.rs"), "fn main() {}\n").unwrap();
        std::fs::write(seed.join("CHANGELOG.md"), "# Changelog\n").unwrap();
        run(&seed, &["add", "."]);
        run(&seed, &["commit", "-qm", "init"]);
        run(
            root.path(),
            &["clone", "-q", "--bare", "seed", "origin.git"],
        );
        run(root.path(), &["clone", "-q", "origin.git", "work"]);
        let work = root.path().join("work");
        run(&work, &["checkout", "-qb", "lore/T-1"]);
        run(&work, &["push", "-q", "origin", "lore/T-1"]);
        (root, work)
    }

    fn commit_and_push(dir: &Path, message: &str) {
        for args in [
            vec!["add", "-A"],
            vec!["commit", "-qm", message],
            vec!["push", "-q", "origin", "lore/T-1"],
        ] {
            let out = std::process::Command::new("git")
                .current_dir(dir)
                .args(["-c", "user.email=t@t", "-c", "user.name=t"])
                .args(&args)
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "{}",
                String::from_utf8_lossy(&out.stderr)
            );
        }
    }

    #[test]
    fn docs_submission_lists_pushed_docs_changes() {
        let (_root, work) = docs_checkout();
        std::fs::write(work.join("CHANGELOG.md"), "# Changelog\n- retries\n").unwrap();
        commit_and_push(&work, "docs: changelog");
        let submission = HarnessStore::docs_submission(&work, "lore/T-1").unwrap();
        assert_eq!(submission.branch, "lore/T-1");
        assert_eq!(submission.base, "main");
        assert_eq!(submission.files, vec!["CHANGELOG.md".to_string()]);
        assert!(HarnessStore::docs_submission(&work, "lore/T-1--c2")
            .unwrap_err()
            .to_string()
            .contains("must be submitted from branch lore/T-1--c2"));
    }

    #[test]
    fn docs_submission_keeps_non_ascii_names_as_written() {
        let (_root, work) = docs_checkout();
        std::fs::create_dir_all(work.join("docs")).unwrap();
        std::fs::write(work.join("docs/français.md"), "# Guide\n").unwrap();
        std::fs::write(work.join("docs/two words.md"), "# Notes\n").unwrap();
        commit_and_push(&work, "docs: guides");
        let mut files = HarnessStore::docs_submission(&work, "lore/T-1")
            .unwrap()
            .files;
        files.sort();
        assert_eq!(files, vec!["docs/français.md", "docs/two words.md"]);
        assert!(
            config::docs_lifecycle::non_docs_paths(files.iter().map(String::as_str)).is_empty()
        );
    }

    #[test]
    fn docs_submission_reports_both_sides_of_a_rename() {
        let (_root, work) = docs_checkout();
        std::fs::create_dir_all(work.join("docs")).unwrap();
        std::fs::rename(work.join("src/lib.rs"), work.join("docs/lib.md")).unwrap();
        commit_and_push(&work, "docs: move");
        let files = HarnessStore::docs_submission(&work, "lore/T-1")
            .unwrap()
            .files;
        assert!(files.contains(&"src/lib.rs".to_string()), "{files:?}");
        let refused = DocsRecord::default().apply(
            "lore",
            DocsEvent::Submit {
                branch: "lore/T-1".into(),
                head: "h".into(),
                base: "main".into(),
                report: "r".into(),
                files,
            },
            1,
        );
        assert!(refused.unwrap_err().to_string().contains("src/lib.rs"));
    }

    #[test]
    fn docs_submission_requires_a_clean_pushed_checkout() {
        let (_root, work) = docs_checkout();
        std::fs::write(work.join("CHANGELOG.md"), "dirty\n").unwrap();
        assert!(HarnessStore::docs_submission(&work, "lore/T-1")
            .unwrap_err()
            .to_string()
            .contains("checkout must be clean"));
        let out = std::process::Command::new("git")
            .current_dir(&work)
            .args([
                "-c",
                "user.email=t@t",
                "-c",
                "user.name=t",
                "commit",
                "-qam",
                "local",
            ])
            .output()
            .unwrap();
        assert!(out.status.success());
        assert!(HarnessStore::docs_submission(&work, "lore/T-1")
            .unwrap_err()
            .to_string()
            .contains("Push branch lore/T-1"));
    }

    #[test]
    fn operator_decisions_expand_short_heads_and_seal_approvals() {
        let head = "0123456789abcdef0123456789abcdef01234567";
        let state = DocsRecord {
            phase: DocsPhase::DocsReady,
            revision: 2,
            head: Some(head.into()),
            ..Default::default()
        };
        let decide = |head: &str, approved| DocsEvent::Decide {
            revision: 2,
            head: head.into(),
            approved,
            report: "notes".into(),
            seal: String::new(),
        };
        assert!(
            HarnessStore::complete_docs_decision("T-1", &state, decide("0123456", true), None)
                .unwrap_err()
                .to_string()
                .contains("docs gate secret")
        );
        let DocsEvent::Decide {
            head: full, seal, ..
        } = HarnessStore::complete_docs_decision(
            "T-1",
            &state,
            decide("0123456", true),
            Some(&DocsGateKeys::new("s3cret")),
        )
        .unwrap()
        else {
            unreachable!()
        };
        assert_eq!(full, head);
        assert_eq!(
            seal,
            config::docs_lifecycle::approval_seal("s3cret", "T-1", 2, head)
        );
        // Too-short prefixes are not expanded; rejections are never sealed.
        let DocsEvent::Decide {
            head: short, seal, ..
        } = HarnessStore::complete_docs_decision("T-1", &state, decide("012345", false), None)
            .unwrap()
        else {
            unreachable!()
        };
        assert_eq!((short.as_str(), seal.as_str()), ("012345", ""));
    }

    #[test]
    fn test_docs_author_is_lore_only() {
        assert!(authorize_docs_author("lore").is_ok());
        assert!(authorize_docs_author("LORE").is_ok());
        assert!(authorize_docs_author("forge").is_err());
        assert!(authorize_docs_author("sentinel").is_err());
        assert!(authorize_docs_author("human").is_err());
    }
}

use super::gate::{hex, DocsGateKeys, SEAL_DOMAIN};
use super::paths::{docs_branch, is_docs_path, ticket_from_docs_branch};
use super::*;
use anyhow::Result;
use hmac::{Hmac, Mac};
use sha2::Sha256;

const SECRET: &str = "controller-secret";

fn submit(record: &DocsRecord, head: &str) -> Result<DocsRecord> {
    record.apply(
        "lore",
        DocsEvent::Submit {
            branch: record.expected_branch("T-1"),
            head: head.into(),
            base: "main".into(),
            report: "CHANGELOG entry and ADR-0007".into(),
            files: vec!["CHANGELOG.md".into(), "docs/adr/0007-retries.md".into()],
        },
        1,
    )
}

fn decide(record: &DocsRecord, approved: bool, report: &str) -> Result<DocsRecord> {
    let head = record.head.clone().unwrap_or_default();
    record.apply(
        "human",
        DocsEvent::Decide {
            revision: record.revision,
            seal: if approved {
                approval_seal(SECRET, "T-1", record.revision, &head)
            } else {
                String::new()
            },
            head,
            approved,
            report: report.into(),
        },
        2,
    )
}

fn published() -> DocsRecord {
    let approved = decide(&submit(&DocsRecord::default(), "abc").unwrap(), true, "").unwrap();
    approved
        .apply("nexus", DocsEvent::Published { number: 9 }, 3)
        .unwrap()
}

fn merged() -> DocsRecord {
    published()
        .apply(
            "vessel",
            DocsEvent::Merged {
                number: 9,
                sha: "m1".into(),
            },
            4,
        )
        .unwrap()
}

#[test]
fn approved_docs_are_published_by_nexus_then_merged() {
    let approved = decide(&submit(&DocsRecord::default(), "abc").unwrap(), true, "").unwrap();
    assert!(approved
        .apply("lore", DocsEvent::Published { number: 9 }, 3)
        .is_err());
    let published = published();
    assert_eq!(published.phase, DocsPhase::Published);
    assert!(!published.phase.is_terminal());
    assert_eq!(published.pr_number, Some(9));
    assert!(published
        .apply(
            "vessel",
            DocsEvent::Merged {
                number: 10,
                sha: "x".into()
            },
            4
        )
        .is_err());
    let merged = merged();
    assert_eq!(merged.phase, DocsPhase::Merged);
    assert_eq!(merged.merge_sha.as_deref(), Some("m1"));
    assert!(merged
        .apply("human", DocsEvent::Pause { reason: "x".into() }, 5)
        .is_err());
}

#[test]
fn seals_bind_ticket_revision_and_head() {
    let approved = decide(&submit(&DocsRecord::default(), "abc").unwrap(), true, "").unwrap();
    assert_eq!(verify_approval(&approved, "T-1", SECRET).unwrap(), "abc");
    assert!(verify_approval(&approved, "T-2", SECRET).is_err());
    assert!(verify_approval(&approved, "T-1", "other-secret").is_err());

    let mut forged = approved.clone();
    forged.decision.as_mut().unwrap().seal = "00".repeat(32);
    assert!(verify_approval(&forged, "T-1", SECRET).is_err());

    let mut moved = approved.clone();
    moved.decision.as_mut().unwrap().head = "evil".into();
    moved.head = Some("evil".into());
    assert!(verify_approval(&moved, "T-1", SECRET).is_err());

    let ready = submit(&DocsRecord::default(), "abc").unwrap();
    let unsealed = ready.apply(
        "human",
        DocsEvent::Decide {
            revision: 1,
            head: "abc".into(),
            approved: true,
            report: String::new(),
            seal: " ".into(),
        },
        2,
    );
    assert!(unsealed.is_err());
    assert!(verify_approval(&ready, "T-1", SECRET).is_err());
}

#[test]
fn rejection_returns_feedback_to_drafting() {
    let ready = submit(&DocsRecord::default(), "abc").unwrap();
    assert!(decide(&ready, false, " ").is_err());
    let rejected = decide(&ready, false, "Explain the migration").unwrap();
    assert_eq!(rejected.phase, DocsPhase::Drafting);
    assert_eq!(rejected.feedback.as_deref(), Some("Explain the migration"));
    assert_eq!(rejected.head, None);
    assert_eq!(rejected.decision.as_ref().unwrap().seal, "");
    let resubmitted = submit(&rejected, "def").unwrap();
    assert_eq!(resubmitted.revision, 2);
    assert_eq!(resubmitted.feedback, None);
}

#[test]
fn decisions_are_bound_to_the_submitted_artifact() {
    let ready = submit(&DocsRecord::default(), "abc").unwrap();
    let decide_at = |revision, head: &str| {
        ready.apply(
            "human",
            DocsEvent::Decide {
                revision,
                head: head.into(),
                approved: true,
                report: String::new(),
                seal: "s".into(),
            },
            2,
        )
    };
    assert!(decide_at(0, "abc").is_err());
    assert!(decide_at(1, "other").is_err());
    assert!(decide_at(1, "abc").is_ok());
}

#[test]
fn actors_are_enforced() {
    let draft = DocsRecord::default();
    assert!(draft
        .apply(
            "forge",
            DocsEvent::Submit {
                branch: "lore/T-1".into(),
                head: "h".into(),
                base: "main".into(),
                report: "r".into(),
                files: vec!["CHANGELOG.md".into()],
            },
            1
        )
        .is_err());
    let ready = submit(&draft, "abc").unwrap();
    let self_approval = ready.apply(
        "lore",
        DocsEvent::Decide {
            revision: 1,
            head: "abc".into(),
            approved: true,
            report: String::new(),
            seal: "s".into(),
        },
        2,
    );
    assert!(self_approval.is_err());
    assert!(draft
        .apply("lore", DocsEvent::Pause { reason: "x".into() }, 1)
        .is_err());
    assert!(draft
        .apply("lore", DocsEvent::Restart { reason: "x".into() }, 1)
        .is_err());
    assert!(draft
        .apply("lore", DocsEvent::Skip { reason: "x".into() }, 1)
        .is_err());
    assert!(published()
        .apply("lore", DocsEvent::Rework { reason: "x".into() }, 4)
        .is_err());
    assert!(published()
        .apply("vessel", DocsEvent::Abandoned { reason: "x".into() }, 4)
        .is_err());
    assert!(merged()
        .apply("vessel", DocsEvent::Reopen { reason: "x".into() }, 5)
        .is_err());
}

#[test]
fn pause_holds_lore_and_publication_but_not_external_facts() {
    let paused = DocsRecord::default()
        .apply(
            "human",
            DocsEvent::Pause {
                reason: "hold".into(),
            },
            1,
        )
        .unwrap();
    assert!(paused.paused);
    assert!(submit(&paused, "abc").is_err());
    assert!(paused
        .apply("human", DocsEvent::Restart { reason: "x".into() }, 2)
        .is_err());
    assert!(paused
        .apply(
            "human",
            DocsEvent::Pause {
                reason: "again".into()
            },
            2
        )
        .is_err());
    let resumed = paused.apply("human", DocsEvent::Resume, 2).unwrap();
    assert!(!resumed.paused);
    assert_eq!(resumed.epoch, 1);
    assert!(submit(&resumed, "abc").is_ok());
    assert!(DocsRecord::default()
        .apply("human", DocsEvent::Pause { reason: " ".into() }, 1)
        .is_err());

    // NEXUS parks a stalled LORE; a held published PR still records its merge.
    let parked = published()
        .apply(
            "nexus",
            DocsEvent::Pause {
                reason: "stalled".into(),
            },
            4,
        )
        .unwrap();
    assert!(parked
        .apply("nexus", DocsEvent::Published { number: 9 }, 5)
        .is_err());
    assert_eq!(
        parked
            .apply(
                "nexus",
                DocsEvent::Merged {
                    number: 9,
                    sha: "m".into()
                },
                5
            )
            .unwrap()
            .phase,
        DocsPhase::Merged
    );
}

#[test]
fn the_operator_can_close_or_reopen_paused_docs() {
    let pause = |record: &DocsRecord| {
        record
            .apply(
                "human",
                DocsEvent::Pause {
                    reason: "hold".into(),
                },
                6,
            )
            .unwrap()
    };
    // A pause that lands just before the merge is recorded must not strand
    // the closed record: resume is refused (terminal), so reopen must work.
    let merged_paused = pause(&published())
        .apply(
            "vessel",
            DocsEvent::Merged {
                number: 9,
                sha: "m".into(),
            },
            7,
        )
        .unwrap();
    assert!(merged_paused.paused);
    assert!(merged_paused.apply("human", DocsEvent::Resume, 8).is_err());
    let reopened = merged_paused
        .apply(
            "human",
            DocsEvent::Reopen {
                reason: "redo".into(),
            },
            8,
        )
        .unwrap();
    assert!(!reopened.paused && reopened.pause_reason.is_none());
    assert_eq!(reopened.phase, DocsPhase::Drafting);

    // A parked, unsubmitted draft can be skipped without resuming it first,
    // but only by the operator.
    let parked = DocsRecord::default()
        .apply(
            "nexus",
            DocsEvent::Pause {
                reason: "stalled".into(),
            },
            1,
        )
        .unwrap();
    assert!(parked
        .apply("nexus", DocsEvent::Skip { reason: "x".into() }, 2)
        .is_err());
    let skipped = parked
        .apply(
            "human",
            DocsEvent::Skip {
                reason: "legacy".into(),
            },
            2,
        )
        .unwrap();
    assert_eq!(skipped.phase, DocsPhase::Skipped);
    assert!(!skipped.paused);
}

#[test]
fn only_the_latest_operator_decision_confirms_an_approval() {
    let ready = submit(&DocsRecord::default(), "abc").unwrap();
    let approved = decide(&ready, true, "").unwrap();
    let control = DocsControl::from_record(&approved);
    assert!(DocsControl::confirm(Some(&control), &approved, "T-1").is_ok());
    // Nothing recorded where workspaces cannot write: refused.
    assert!(DocsControl::confirm(None, &approved, "T-1").is_err());

    // The operator rejects; a workspace restores the old approved record.
    let rejected = decide(&approved, false, "explain the retry budget").unwrap();
    let control = DocsControl::from_record(&rejected);
    assert!(control.approved.is_none());
    let error = DocsControl::confirm(Some(&control), &approved, "T-1").unwrap_err();
    assert!(format!("{error:#}").contains("not the operator's latest decision"));

    // A pause recorded by the operator holds even if the record is unpaused.
    let paused = approved
        .apply(
            "human",
            DocsEvent::Pause {
                reason: "freeze".into(),
            },
            3,
        )
        .unwrap();
    let control = DocsControl::from_record(&paused);
    assert!(DocsControl::confirm(Some(&control), &approved, "T-1").is_err());
}

#[test]
fn restart_redrives_drafting_only() {
    let restarted = DocsRecord::default()
        .apply(
            "human",
            DocsEvent::Restart {
                reason: "chat died".into(),
            },
            1,
        )
        .unwrap();
    assert_eq!(restarted.epoch, 1);
    assert_eq!(restarted.phase, DocsPhase::Drafting);
    let ready = submit(&DocsRecord::default(), "abc").unwrap();
    assert!(ready
        .apply("human", DocsEvent::Restart { reason: "x".into() }, 2)
        .is_err());
    assert!(decide(&ready, true, "")
        .unwrap()
        .apply("human", DocsEvent::Restart { reason: "x".into() }, 3)
        .is_err());
}

#[test]
fn rework_keeps_the_branch_and_pr_and_needs_a_new_approval() {
    let reworked = published()
        .apply(
            "vessel",
            DocsEvent::Rework {
                reason: "conflicts with main".into(),
            },
            4,
        )
        .unwrap();
    assert_eq!(reworked.phase, DocsPhase::Drafting);
    assert_eq!(reworked.pr_number, Some(9), "the same PR is updated");
    assert_eq!(reworked.branch.as_deref(), Some("lore/T-1"));
    assert_eq!(reworked.cycle, 0);
    assert_eq!(reworked.epoch, 1);
    assert_eq!(reworked.decision, None);
    assert!(verify_approval(&reworked, "T-1", SECRET).is_err());
    let again = decide(&submit(&reworked, "def").unwrap(), true, "").unwrap();
    assert_eq!(
        again
            .apply("nexus", DocsEvent::Published { number: 9 }, 6)
            .unwrap()
            .pr_number,
        Some(9)
    );
    assert!(DocsRecord::default()
        .apply("vessel", DocsEvent::Rework { reason: "x".into() }, 1)
        .is_err());
}

#[test]
fn submissions_must_change_only_documentation() {
    let draft = DocsRecord::default();
    let submit_files = |files: Vec<&str>| {
        draft.apply(
            "lore",
            DocsEvent::Submit {
                branch: "lore/T-1".into(),
                head: "abc".into(),
                base: "main".into(),
                report: "r".into(),
                files: files.into_iter().map(str::to_string).collect(),
            },
            1,
        )
    };
    assert!(submit_files(vec![]).is_err());
    let error = submit_files(vec!["CHANGELOG.md", "src/main.rs"])
        .unwrap_err()
        .to_string();
    assert!(error.contains("src/main.rs"), "{error}");
    assert_eq!(
        submit_files(vec!["CHANGELOG.md", "docs/adr/1.md"])
            .unwrap()
            .files
            .len(),
        2
    );
}

#[test]
fn closed_records_reopen_on_a_new_cycle() {
    let skipped = DocsRecord::default()
        .apply(
            "nexus",
            DocsEvent::Skip {
                reason: "merged before the docs gate".into(),
            },
            1,
        )
        .unwrap();
    assert_eq!(skipped.phase, DocsPhase::Skipped);
    assert!(submit(&skipped, "abc").is_err());
    assert!(submit(&DocsRecord::default(), "abc")
        .unwrap()
        .apply("nexus", DocsEvent::Skip { reason: "x".into() }, 2)
        .is_err());

    let abandoned = published()
        .apply(
            "nexus",
            DocsEvent::Abandoned {
                reason: "closed on GitHub".into(),
            },
            4,
        )
        .unwrap();
    assert!(abandoned.phase.is_terminal());

    for closed in [merged(), abandoned] {
        let reopened = closed
            .apply(
                "human",
                DocsEvent::Reopen {
                    reason: "document the follow-up".into(),
                },
                5,
            )
            .unwrap();
        assert_eq!(reopened.phase, DocsPhase::Drafting);
        assert_eq!(reopened.cycle, 1);
        assert_eq!(reopened.pr_number, None);
        assert_eq!(reopened.revision, 1, "revisions stay monotonic");
        assert_eq!(reopened.expected_branch("T-1"), "lore/T-1--c2");
        assert!(reopened.feedback.is_some());
    }
    assert!(published()
        .apply("human", DocsEvent::Reopen { reason: "x".into() }, 4)
        .is_err());
}

#[test]
fn docs_branches_round_trip_to_their_ticket() {
    assert_eq!(docs_branch("T-9", 0), "lore/T-9");
    assert_eq!(docs_branch("T-9", 1), "lore/T-9--c2");
    assert_eq!(ticket_from_docs_branch("lore/T-9"), Some("T-9"));
    assert_eq!(ticket_from_docs_branch("lore/T-9--c2"), Some("T-9"));
    assert_eq!(ticket_from_docs_branch("lore/docs-123"), Some("docs-123"));
    assert_eq!(ticket_from_docs_branch("lore/a/b"), None);
    assert_eq!(ticket_from_docs_branch("forge-1/T-9"), None);
    assert_eq!(ticket_from_docs_branch("lore/"), None);
}

#[test]
fn docs_paths_are_documentation_only() {
    for path in [
        "CHANGELOG.md",
        "CHANGELOG",
        "changelog.rst",
        "docs/adr/0001.md",
        "docs/img/flow.png",
        "README.md",
        "crates/x/README.md",
        "docs/notes.txt",
        "CHANGELOG.txt",
        "docs/diagram.svg",
    ] {
        assert!(is_docs_path(path), "{path}");
    }
    for path in [
        "src/lib.rs",
        "Cargo.toml",
        "",
        "docs/../src/lib.rs",
        "/etc/passwd.md",
        "docs/conf.py",
        "docs/build.sh",
        "docs/_config.yml",
        ".github/PULL_REQUEST_TEMPLATE.md",
        ".github/workflows/ci.yml",
        "orchestration/agent/agents/forge.agent.md",
        "orchestration/plugin/skills/forge/SKILL.md",
        "crates/x/SKILL.md",
        "CLAUDE.md",
        "AGENTS.md",
        "sub/agents.md",
        "x.agent.md",
        "changelog.py",
        "src/main.rs.png",
        // MDX is JSX, run by docs sites at build time.
        "crates/x/README.mdx",
        // Text outside docs/, and build or site inputs even under it.
        "notes.txt",
        "requirements.txt",
        "docs/requirements.txt",
        "docs/requirements-dev.txt",
        "CMakeLists.txt",
        "docs/robots.txt",
        // Dot-directories and dotfiles are tooling configuration.
        ".cursor/rules/style.md",
        ".changeset/brave-cats.md",
        "docs/.vitepress/config.md",
        ".cursorrules",
        // Legal and ownership files.
        "LICENSE",
        "LICENSE.md",
        "docs/COPYING.txt",
        "LICENSE-MIT.md",
        "COPYING-LESSER.md",
        "docs/licenses/apache.md",
        "NOTICES.md",
        "NOTICE.md",
        "CODEOWNERS",
        "docs/CODEOWNERS",
    ] {
        assert!(!is_docs_path(path), "{path}");
    }
}

#[test]
fn rotated_keys_still_verify_and_unknown_keys_are_named() {
    let approved = decide(&submit(&DocsRecord::default(), "abc").unwrap(), true, "").unwrap();
    let rotated = DocsGateKeys::new("new-secret").with_previous(SECRET);
    assert_eq!(rotated.verify(&approved, "T-1").unwrap(), "abc");

    let unknown = DocsGateKeys::new("new-secret");
    let error = unknown.verify(&approved, "T-1").unwrap_err().to_string();
    let sealed_with = DocsGateKeys::new(SECRET).key_id().to_string();
    assert!(error.contains(&sealed_with), "{error}");
    assert!(error.contains("approve revision 1 again"), "{error}");
    assert_ne!(unknown.key_id(), sealed_with);
}

#[test]
fn seals_never_use_the_raw_secret_as_the_key() {
    let raw = {
        let mut mac = Hmac::<Sha256>::new_from_slice(SECRET.as_bytes()).unwrap();
        mac.update(SEAL_DOMAIN);
        mac.update(b"T-1\x001\x00abc");
        hex(&mac.finalize().into_bytes())
    };
    let seal = approval_seal(SECRET, "T-1", 1, "abc");
    assert!(seal.starts_with("v2:"), "{seal}");
    assert!(
        !seal.ends_with(&raw),
        "the seal key is derived, not the secret"
    );
    assert!(format!("{:?}", DocsGateKeys::new(SECRET)).contains("DocsGateKeys"));
    assert!(!format!("{:?}", DocsGateKeys::new(SECRET)).contains(SECRET));
}

#[test]
fn the_operator_can_reseal_or_revoke_an_approval() {
    let approved = decide(&submit(&DocsRecord::default(), "abc").unwrap(), true, "").unwrap();
    // Approving again (e.g. after a key rotation) keeps the phase.
    let resealed = approved
        .apply(
            "human",
            DocsEvent::Decide {
                revision: 1,
                head: "abc".into(),
                approved: true,
                report: String::new(),
                seal: DocsGateKeys::new("new-secret").seal("T-1", 1, "abc"),
            },
            3,
        )
        .unwrap();
    assert_eq!(resealed.phase, DocsPhase::Approved);
    assert!(DocsGateKeys::new("new-secret")
        .verify(&resealed, "T-1")
        .is_ok());

    // Rejecting a published record revokes the approval but keeps the PR.
    let published = published();
    let revoked = decide(&published, false, "Wrong release number").unwrap();
    assert_eq!(revoked.phase, DocsPhase::Drafting);
    assert_eq!(revoked.pr_number, Some(9));
    assert!(!revoked.merge_candidate(9));
    assert!(verify_approval(&revoked, "T-1", SECRET).is_err());
    // Only the operator decides, and only on the submitted head.
    assert!(published
        .apply(
            "nexus",
            DocsEvent::Decide {
                revision: 1,
                head: "abc".into(),
                approved: false,
                report: "x".into(),
                seal: String::new(),
            },
            4
        )
        .is_err());
}

#[test]
fn a_reopened_cycle_can_be_skipped_until_it_submits() {
    let reopened = merged()
        .apply(
            "human",
            DocsEvent::Reopen {
                reason: "by mistake".into(),
            },
            5,
        )
        .unwrap();
    assert_eq!(reopened.cycle_start_revision, 1);
    let skipped = reopened
        .apply(
            "human",
            DocsEvent::Skip {
                reason: "reopened by mistake".into(),
            },
            6,
        )
        .unwrap();
    assert_eq!(skipped.phase, DocsPhase::Skipped);

    let resubmitted = submit(&reopened, "def").unwrap();
    let rejected = decide(&resubmitted, false, "redo").unwrap();
    assert!(rejected
        .apply("human", DocsEvent::Skip { reason: "x".into() }, 7)
        .is_err());
}

#[test]
fn history_is_capped() {
    let mut record = DocsRecord::default();
    for ts in 0..(MAX_DOCS_HISTORY as u64 + 20) {
        let event = if record.paused {
            DocsEvent::Resume
        } else {
            DocsEvent::Pause {
                reason: "hold".into(),
            }
        };
        record = record.apply("human", event, ts).unwrap();
    }
    assert_eq!(record.history.len(), MAX_DOCS_HISTORY);
    assert_eq!(record.history.last().unwrap().version, record.version);
}

#[test]
fn decode_refuses_malformed_records() {
    assert_eq!(DocsRecord::decode(None).unwrap(), DocsRecord::default());
    assert!(DocsRecord::decode(Some(serde_json::json!("published"))).is_err());
}

//! Post-merge documentation gate for a ticket, owned by LORE and the operator.
//!
//! The ticket lifecycle is terminal at `Done`, so LORE's changelog/ADR work is
//! tracked in its own versioned record (`ticket:{id}:docs`). It follows the same
//! rules as [`crate::lifecycle`]: every change is a reducer step that returns an
//! error instead of guessing, decisions are bound to the exact submitted
//! revision and head, and the store commits each step with compare-and-set.
//!
//! Path classification and approval sealing live in [`paths`] and [`gate`].

pub mod gate;
pub mod paths;

pub use gate::{
    approval_seal, gate_keys_from_env_logged, verify_approval, DocsGateKeys, MIN_GATE_SECRET_LEN,
};
pub use paths::{
    docs_branch, is_docs_path, non_docs_paths, ticket_from_docs_branch, DOCS_BRANCH_PREFIX,
    DOCS_TICKET, MAX_DOCS_HISTORY,
};

use anyhow::{ensure, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum DocsPhase {
    /// LORE is writing (or rewriting after a rejection or rework) the documentation.
    #[default]
    Drafting,
    /// LORE pushed a docs branch and is waiting for the operator.
    DocsReady,
    /// The operator approved the submitted head; NEXUS opens the docs PR.
    Approved,
    /// The docs PR is open; VESSEL merges it at the approved head.
    Published,
    /// The docs PR merged. Terminal unless reopened.
    Merged,
    /// The docs PR was closed without merging. Terminal unless reopened.
    Abandoned,
    /// No documentation will be produced (for example, the ticket merged
    /// before the docs gate existed). Terminal unless reopened.
    Skipped,
}
impl DocsPhase {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Drafting => "drafting",
            Self::DocsReady => "docs_ready",
            Self::Approved => "approved",
            Self::Published => "published",
            Self::Merged => "merged",
            Self::Abandoned => "abandoned",
            Self::Skipped => "skipped",
        }
    }

    /// The record is closed: only an operator reopen changes it.
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Merged | Self::Abandoned | Self::Skipped)
    }

    /// LORE has work to do (a workspace and chat are needed).
    pub fn needs_lore(self) -> bool {
        self == Self::Drafting
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DocsDecision {
    pub actor: String,
    pub approved: bool,
    pub report: String,
    pub revision: u64,
    pub head: String,
    /// [`approval_seal`] for approvals; empty for rejections.
    #[serde(default)]
    pub seal: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DocsTransition {
    pub version: u64,
    pub actor: String,
    pub from: DocsPhase,
    pub to: DocsPhase,
    pub ts: u64,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(default)]
pub struct DocsRecord {
    pub version: u64,
    pub phase: DocsPhase,
    /// Incremented by every LORE submission; decisions must name it.
    pub revision: u64,
    /// Incremented by every resume, restart, rework and reopen. NEXUS keys
    /// LORE instructions on it and replaces LORE's workspace when it changes.
    pub epoch: u64,
    /// Incremented by every reopen; selects the docs branch ([`docs_branch`]).
    pub cycle: u64,
    /// `revision` when the current docs cycle began. Nothing has been
    /// submitted in this cycle while `revision` still equals it.
    pub cycle_start_revision: u64,
    pub branch: Option<String>,
    pub head: Option<String>,
    /// Default branch the submission was diffed against (informational; NEXUS
    /// takes the PR base from GitHub).
    pub base: Option<String>,
    pub report: String,
    /// Files the submitted head changes relative to the default branch.
    pub files: Vec<String>,
    pub decision: Option<DocsDecision>,
    /// Operator (or VESSEL/NEXUS) feedback for LORE to address while drafting.
    pub feedback: Option<String>,
    /// Hold: while set, NEXUS frees LORE and neither opens nor merges the
    /// docs PR, and LORE cannot submit.
    pub paused: bool,
    pub pause_reason: Option<String>,
    /// The docs PR. Kept across a rework so the same PR is updated.
    pub pr_number: Option<u64>,
    pub merge_sha: Option<String>,
    pub skip_reason: Option<String>,
    /// The most recent [`MAX_DOCS_HISTORY`] transitions.
    pub history: Vec<DocsTransition>,
    pub role: String,
    pub ts: u64,
}

#[derive(Debug, Clone)]
pub enum DocsEvent {
    Submit {
        branch: String,
        head: String,
        base: String,
        report: String,
        files: Vec<String>,
    },
    /// The operator approves or rejects the submitted revision and head. It
    /// may also decide again after an approval (while approved or published):
    /// approving again replaces the seal (for example after a key rotation),
    /// and rejecting revokes the approval and returns the docs to LORE.
    Decide {
        revision: u64,
        head: String,
        approved: bool,
        report: String,
        /// Required when approving; see [`approval_seal`].
        seal: String,
    },
    /// Operator hold, or NEXUS parking a stalled LORE.
    Pause {
        reason: String,
    },
    Resume,
    /// Operator re-drives a stuck LORE on a fresh workspace.
    Restart {
        reason: String,
    },
    /// NEXUS opened (or found) the docs PR for the approved head.
    Published {
        number: u64,
    },
    /// The approved docs cannot be merged as they are (conflicts, failed CI,
    /// branch moved after approval): LORE revises the same branch and PR.
    Rework {
        reason: String,
    },
    /// The docs PR merged.
    Merged {
        number: u64,
        sha: String,
    },
    /// The docs PR was closed without merging.
    Abandoned {
        reason: String,
    },
    /// Close out a record without documentation.
    Skip {
        reason: String,
    },
    /// Start a new docs cycle on a fresh branch after a closed record.
    Reopen {
        reason: String,
    },
}

impl DocsEvent {
    /// Facts about the docs PR are recorded even while paused.
    fn records_external_fact(&self) -> bool {
        matches!(
            self,
            Self::Merged { .. } | Self::Abandoned { .. } | Self::Rework { .. }
        )
    }
}

impl DocsRecord {
    /// An absent record is a fresh draft; a malformed one is never reset.
    pub fn decode(value: Option<serde_json::Value>) -> Result<Self> {
        let Some(value) = value else {
            return Ok(Self::default());
        };
        ensure!(
            value.get("version").is_some(),
            "Malformed docs record; refusing to reset it"
        );
        Ok(serde_json::from_value(value)?)
    }

    /// The branch LORE must submit for this record's current cycle.
    pub fn expected_branch(&self, ticket: &str) -> String {
        docs_branch(ticket, self.cycle)
    }

    /// Whether this open record accounts for docs PR `number` from `branch`
    /// (before it is recorded, while it is published, or during a rework).
    /// A docs PR no open record owns needs a human.
    pub fn owns_pr(&self, branch: &str, number: u64) -> bool {
        !self.phase.is_terminal()
            && self.branch.as_deref() == Some(branch)
            && self.pr_number.is_none_or(|recorded| recorded == number)
    }

    /// VESSEL may try to merge docs PR `number` now. The approval seal is
    /// verified separately ([`verify_approval`]).
    pub fn merge_candidate(&self, number: u64) -> bool {
        self.phase == DocsPhase::Published && !self.paused && self.pr_number == Some(number)
    }

    pub fn apply(&self, actor: &str, event: DocsEvent, ts: u64) -> Result<Self> {
        let actor = actor.to_ascii_lowercase();
        ensure!(
            !self.phase.is_terminal() || matches!(event, DocsEvent::Reopen { .. }),
            "Documentation is {}; only `--reopen` can start a new docs cycle",
            self.phase.as_str()
        );
        ensure!(
            !self.paused
                || matches!(event, DocsEvent::Resume | DocsEvent::Pause { .. })
                || event.records_external_fact(),
            "Documentation is paused; wait for `openflows gate docs --resume`"
        );
        let detail = match &event {
            DocsEvent::Submit { head, .. } => {
                format!("docs revision {} submitted at {head}", self.revision + 1)
            }
            DocsEvent::Decide {
                approved, report, ..
            } => format!("{}: {report}", if *approved { "approve" } else { "reject" }),
            DocsEvent::Pause { reason } => format!("paused: {reason}"),
            DocsEvent::Resume => "resumed".into(),
            DocsEvent::Restart { reason } => format!("restarted: {reason}"),
            DocsEvent::Published { number } => format!("docs PR #{number} opened"),
            DocsEvent::Rework { reason } => format!("rework: {reason}"),
            DocsEvent::Merged { number, sha } => format!("docs PR #{number} merged at {sha}"),
            DocsEvent::Abandoned { reason } => format!("abandoned: {reason}"),
            DocsEvent::Skip { reason } => format!("skipped: {reason}"),
            DocsEvent::Reopen { reason } => format!("reopened: {reason}"),
        };
        let mut next = self.clone();
        match event {
            DocsEvent::Submit {
                branch,
                head,
                base,
                report,
                files,
            } => {
                ensure!(
                    actor == "lore" && self.phase == DocsPhase::Drafting,
                    "Only LORE may submit documentation, and only while drafting"
                );
                ensure!(
                    !branch.trim().is_empty() && !head.trim().is_empty(),
                    "Docs submission must identify the pushed branch and head"
                );
                ensure!(
                    !report.trim().is_empty(),
                    "Docs submission requires a summary report"
                );
                ensure!(
                    !files.is_empty(),
                    "Docs submission changes no files relative to the default branch"
                );
                let outside = non_docs_paths(files.iter().map(String::as_str));
                ensure!(
                    outside.is_empty(),
                    "LORE may change documentation only (Markdown/reST/AsciiDoc, changelogs, text and images under docs/; never dot-directories, orchestration/, agent instructions, legal or build files); revert: {}",
                    outside.join(", ")
                );
                next.revision += 1;
                next.branch = Some(branch);
                next.head = Some(head);
                next.base = (!base.trim().is_empty()).then_some(base);
                next.report = report;
                next.files = files;
                next.decision = None;
                next.feedback = None;
                next.phase = DocsPhase::DocsReady;
            }
            DocsEvent::Decide {
                revision,
                head,
                approved,
                report,
                seal,
            } => {
                ensure!(
                    actor == "human",
                    "Only the operator may decide documentation"
                );
                ensure!(
                    matches!(
                        self.phase,
                        DocsPhase::DocsReady | DocsPhase::Approved | DocsPhase::Published
                    ),
                    "No documentation is awaiting a decision; current phase is {}",
                    self.phase.as_str()
                );
                ensure!(revision == self.revision, "Stale docs revision");
                ensure!(
                    self.head.as_deref() == Some(head.as_str()),
                    "Decision must identify the submitted docs head"
                );
                ensure!(
                    approved || !report.trim().is_empty(),
                    "Rejecting documentation requires feedback for LORE"
                );
                ensure!(
                    !approved || !seal.trim().is_empty(),
                    "Approving documentation requires the operator seal"
                );
                next.decision = Some(DocsDecision {
                    actor: actor.clone(),
                    approved,
                    report: report.clone(),
                    revision,
                    head,
                    seal: if approved { seal } else { String::new() },
                });
                if approved {
                    // Approving again keeps an approved or published record where it is.
                    if self.phase == DocsPhase::DocsReady {
                        next.phase = DocsPhase::Approved;
                    }
                } else {
                    // A published PR is kept, as for a rework.
                    next.feedback = Some(report);
                    next.head = None;
                    next.phase = DocsPhase::Drafting;
                }
            }
            DocsEvent::Pause { reason } => {
                ensure!(
                    matches!(actor.as_str(), "human" | "nexus"),
                    "Only the operator (or NEXUS parking a stalled LORE) may pause documentation"
                );
                ensure!(!self.paused, "Documentation is already paused");
                ensure!(
                    !reason.trim().is_empty(),
                    "Pausing documentation requires a reason"
                );
                next.paused = true;
                next.pause_reason = Some(reason);
            }
            DocsEvent::Resume => {
                ensure!(
                    actor == "human",
                    "Only the operator may resume documentation"
                );
                ensure!(self.paused, "Documentation is not paused");
                next.paused = false;
                next.pause_reason = None;
                next.epoch += 1;
            }
            DocsEvent::Restart { reason } => {
                ensure!(actor == "human", "Only the operator may restart LORE");
                ensure!(
                    self.phase == DocsPhase::Drafting,
                    "LORE has nothing to redo while documentation is {}",
                    self.phase.as_str()
                );
                ensure!(
                    !reason.trim().is_empty(),
                    "Restarting LORE requires a reason"
                );
                next.epoch += 1;
            }
            DocsEvent::Published { number } => {
                ensure!(
                    actor == "nexus" && self.phase == DocsPhase::Approved && number > 0,
                    "Only NEXUS records the docs PR, and only after operator approval"
                );
                // A rework keeps the PR; if it was closed meanwhile, NEXUS
                // opens a replacement from the same branch.
                next.pr_number = Some(number);
                next.phase = DocsPhase::Published;
            }
            DocsEvent::Rework { reason } => {
                ensure!(
                    matches!(actor.as_str(), "vessel" | "nexus"),
                    "Only VESSEL or NEXUS may send approved docs back for rework"
                );
                ensure!(
                    matches!(self.phase, DocsPhase::Approved | DocsPhase::Published),
                    "Only approved or published docs can be reworked; current phase is {}",
                    self.phase.as_str()
                );
                ensure!(
                    !reason.trim().is_empty(),
                    "Reworking documentation requires a reason for LORE"
                );
                next.epoch += 1;
                next.head = None;
                next.decision = None;
                next.feedback = Some(reason);
                next.phase = DocsPhase::Drafting;
            }
            DocsEvent::Merged { number, sha } => {
                ensure!(
                    matches!(actor.as_str(), "vessel" | "nexus"),
                    "Only VESSEL or NEXUS may record a docs merge"
                );
                ensure!(
                    self.phase == DocsPhase::Published && self.pr_number == Some(number),
                    "Docs PR #{number} is not the published docs PR"
                );
                next.merge_sha = (!sha.is_empty()).then_some(sha);
                next.phase = DocsPhase::Merged;
            }
            DocsEvent::Abandoned { reason } => {
                ensure!(
                    actor == "nexus" && self.phase == DocsPhase::Published,
                    "Only NEXUS may record an abandoned docs PR"
                );
                next.skip_reason = Some(reason);
                next.phase = DocsPhase::Abandoned;
            }
            DocsEvent::Skip { reason } => {
                ensure!(
                    matches!(actor.as_str(), "human" | "nexus"),
                    "Only the operator or NEXUS may skip documentation"
                );
                ensure!(
                    self.phase == DocsPhase::Drafting
                        && self.revision == self.cycle_start_revision,
                    "Documentation was already submitted in this docs cycle; reject it instead of skipping"
                );
                ensure!(
                    !reason.trim().is_empty(),
                    "Skipping documentation requires a reason"
                );
                next.skip_reason = Some(reason);
                next.phase = DocsPhase::Skipped;
            }
            DocsEvent::Reopen { reason } => {
                ensure!(
                    actor == "human",
                    "Only the operator may reopen documentation"
                );
                ensure!(
                    self.phase.is_terminal(),
                    "Documentation is still {}; reopen applies to merged, abandoned or skipped docs",
                    self.phase.as_str()
                );
                ensure!(
                    !reason.trim().is_empty(),
                    "Reopening documentation requires a reason for LORE"
                );
                next.cycle += 1;
                next.cycle_start_revision = self.revision;
                next.epoch += 1;
                next.branch = None;
                next.head = None;
                next.base = None;
                next.files.clear();
                next.decision = None;
                next.pr_number = None;
                next.merge_sha = None;
                next.skip_reason = None;
                next.feedback = Some(reason);
                next.phase = DocsPhase::Drafting;
            }
        }
        next.version += 1;
        next.history.push(DocsTransition {
            version: next.version,
            actor: actor.clone(),
            from: self.phase,
            to: next.phase,
            ts,
            detail,
        });
        if next.history.len() > MAX_DOCS_HISTORY {
            let excess = next.history.len() - MAX_DOCS_HISTORY;
            next.history.drain(..excess);
        }
        next.role = actor;
        next.ts = ts;
        Ok(next)
    }
}

#[cfg(test)]
mod tests;

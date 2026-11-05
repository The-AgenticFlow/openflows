//! Post-merge documentation gate for a ticket, owned by LORE and the operator.
//!
//! The ticket lifecycle is terminal at `Done`, so LORE's changelog/ADR work is
//! tracked in its own versioned record (`ticket:{id}:docs`). It follows the same
//! rules as [`crate::lifecycle`]: every change is a reducer step that returns an
//! error instead of guessing, decisions are bound to the exact submitted
//! revision and head, and the store commits each step with compare-and-set.
//!
//! Approvals carry a **seal**: an HMAC over the ticket, revision and head,
//! keyed with a key derived from the controller's secret ([`DocsGateKeys`]),
//! which workspaces never receive. The reducer only checks that a seal is
//! present; NEXUS (before opening the docs PR) and VESSEL (before merging it)
//! verify it, so a workspace that can write the store still cannot forge an
//! approval.
//!
//! What a submission changes (`files`, `base`) is reported by the LORE
//! workspace and is therefore only advisory. The controller re-derives the
//! changed paths from GitHub and checks them with [`non_docs_paths`] before it
//! opens or merges a docs PR.
use anyhow::{bail, ensure, Result};
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fmt;

/// Synthetic ticket id VESSEL assigns to docs PRs. LORE never documents it.
pub const DOCS_TICKET: &str = "T-DOCS";
/// Every LORE docs branch starts with this prefix; VESSEL keys docs PRs on it.
pub const DOCS_BRANCH_PREFIX: &str = "lore/";
/// Separator for the docs cycle suffix (`lore/T-9--c2`). A single `/` cannot
/// be used: git refuses `lore/T-9` and `lore/T-9/c2` side by side.
const CYCLE_SEPARATOR: &str = "--c";
/// Transitions kept in a record's history; older ones are dropped.
pub const MAX_DOCS_HISTORY: usize = 100;
/// Domain separator for approval seals.
const SEAL_DOMAIN: &[u8] = b"openflows-docs-approval-v1\0";

/// The branch LORE must use for `ticket` in docs cycle `cycle`. The first
/// cycle uses `lore/{ticket}`; each reopen moves to a fresh branch so the new
/// docs PR never collides with an earlier one.
pub fn docs_branch(ticket: &str, cycle: u64) -> String {
    if cycle == 0 {
        format!("{DOCS_BRANCH_PREFIX}{ticket}")
    } else {
        format!("{DOCS_BRANCH_PREFIX}{ticket}{CYCLE_SEPARATOR}{}", cycle + 1)
    }
}

/// The ticket a LORE docs branch documents, or `None` for other branches.
///
/// Assumes ticket ids never end in `--c<digits>` themselves; such an id would
/// be read as a cycle suffix.
pub fn ticket_from_docs_branch(branch: &str) -> Option<&str> {
    let rest = branch.strip_prefix(DOCS_BRANCH_PREFIX)?;
    let ticket = match rest.rsplit_once(CYCLE_SEPARATOR) {
        Some((ticket, cycle)) if !cycle.is_empty() && cycle.bytes().all(|b| b.is_ascii_digit()) => {
            ticket
        }
        _ => rest,
    };
    (!ticket.is_empty() && !ticket.contains('/')).then_some(ticket)
}

/// Paths LORE may never change, even when they look like documentation:
/// agent personas, skills and instructions steer other agents, and legal
/// files are not LORE's to rewrite. Every dot-directory and dotfile
/// (`.github/`, `.claude/`, `.cursor/`, `.changeset/`, `.cursorrules`, ...) is
/// refused too, since tooling reads those as configuration.
const PROTECTED_PREFIXES: &[&str] = &["orchestration/"];
const PROTECTED_NAMES: &[&str] = &[
    "claude.md",
    "agents.md",
    "gemini.md",
    "copilot-instructions.md",
    "codeowners",
];
/// Legal files (`LICENSE`, `LICENSE.md`, `COPYING.txt`, `NOTICE`).
const PROTECTED_NAME_STEMS: &[&str] = &["license", "licence", "copying", "notice"];
/// Prose formats that are documentation wherever they live. MDX is not one:
/// it is JSX that docs sites execute at build time.
const DOC_EXTENSIONS: &[&str] = &[".md", ".rst", ".adoc"];
/// Allowed only under `docs/`: plain text and diagrams or screenshots.
const DOCS_DIR_EXTENSIONS: &[&str] = &[".txt", ".png", ".jpg", ".jpeg", ".gif", ".svg", ".webp"];
/// Text files that are build or site inputs, not prose (`docs/requirements.txt`
/// installs the docs build's dependencies, for example).
const BUILD_INPUT_STEMS: &[&str] = &["requirements", "constraints", "cmakelists", "robots"];

/// Whether a repository path is documentation LORE may change: Markdown,
/// reStructuredText and AsciiDoc files, changelogs, and text files and images
/// under `docs/`. Executable, configuration and build-input files never
/// qualify (even under `docs/`), nor do dot-directories, agent instructions,
/// `orchestration/` and legal files.
pub fn is_docs_path(path: &str) -> bool {
    let path = path.trim();
    if path.is_empty()
        || path.starts_with('/')
        || path
            .split('/')
            .any(|part| part.is_empty() || part == ".." || part.starts_with('.'))
    {
        return false;
    }
    let lower = path.to_ascii_lowercase();
    let name = lower.rsplit('/').next().unwrap_or(&lower);
    let stem = name.split('.').next().unwrap_or(name);
    if PROTECTED_PREFIXES.iter().any(|p| lower.starts_with(p))
        || PROTECTED_NAMES.contains(&name)
        || PROTECTED_NAME_STEMS.contains(&stem)
        || BUILD_INPUT_STEMS.iter().any(|s| stem.starts_with(s))
        || name.ends_with(".agent.md")
        || name == "skill.md"
    {
        return false;
    }
    let has = |extensions: &[&str]| extensions.iter().any(|e| name.ends_with(e));
    let is_changelog = stem.starts_with("changelog")
        && (!name.contains('.') || has(DOC_EXTENSIONS) || name.ends_with(".txt"));
    is_changelog || has(DOC_EXTENSIONS) || (lower.starts_with("docs/") && has(DOCS_DIR_EXTENSIONS))
}

/// The paths in `paths` that are not documentation ([`is_docs_path`]).
pub fn non_docs_paths<'a>(paths: impl IntoIterator<Item = &'a str>) -> Vec<String> {
    paths
        .into_iter()
        .filter(|path| !is_docs_path(path))
        .map(str::to_string)
        .collect()
}

/// Minimum length of a docs gate secret: the floor the hook server enforces
/// for HS256 secrets.
pub const MIN_GATE_SECRET_LEN: usize = 32;
/// Label for deriving the seal key from the configured secret, so the bytes
/// that verify Coder hook JWTs are never used directly as a second key.
const KEY_LABEL: &[u8] = b"openflows-docs-gate-key-v1";
/// Seal format version: `v2:<key id>:<hex HMAC>`.
const SEAL_VERSION: &str = "v2";

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// One seal key, derived from a configured secret, with a short public id.
#[derive(Clone)]
struct GateKey {
    id: String,
    key: [u8; 32],
}

impl GateKey {
    fn derive(secret: &str) -> Self {
        let mut mac =
            Hmac::<Sha256>::new_from_slice(secret.as_bytes()).expect("HMAC accepts any key length");
        mac.update(KEY_LABEL);
        let key: [u8; 32] = mac.finalize().into_bytes().into();
        let id = hex(&Sha256::digest(key)[..4]);
        Self { id, key }
    }

    fn mac(&self, ticket: &str, revision: u64, head: &str) -> Hmac<Sha256> {
        let mut mac =
            Hmac::<Sha256>::new_from_slice(&self.key).expect("HMAC accepts any key length");
        mac.update(SEAL_DOMAIN);
        mac.update(ticket.as_bytes());
        mac.update(b"\0");
        mac.update(revision.to_string().as_bytes());
        mac.update(b"\0");
        mac.update(head.as_bytes());
        mac
    }
}

/// The keys that seal and verify operator docs approvals.
///
/// The secret is `OPENFLOWS_DOCS_GATE_SECRET`, falling back to
/// `CODER_CHAT_HOOK_SECRET` for existing deployments. Either way the seal key
/// is derived from it with a separate label, and both must be at least
/// [`MIN_GATE_SECRET_LEN`] bytes. During a rotation,
/// `OPENFLOWS_DOCS_GATE_SECRET_PREVIOUS` still verifies approvals sealed
/// before it; each seal names its key id, so an approval sealed with a key the
/// controller no longer holds is reported as such and can be re-approved.
#[derive(Clone)]
pub struct DocsGateKeys {
    current: GateKey,
    previous: Option<GateKey>,
}

impl fmt::Debug for DocsGateKeys {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DocsGateKeys")
            .field("current", &self.current.id)
            .field("previous", &self.previous.as_ref().map(|k| &k.id))
            .finish()
    }
}

impl DocsGateKeys {
    /// Keys sealing with (and verifying) `secret` only.
    pub fn new(secret: &str) -> Self {
        Self {
            current: GateKey::derive(secret),
            previous: None,
        }
    }

    /// Also verify approvals sealed with `secret` (a rotated-out secret).
    pub fn with_previous(mut self, secret: &str) -> Self {
        self.previous = Some(GateKey::derive(secret));
        self
    }

    /// Public id of the key new approvals are sealed with.
    pub fn key_id(&self) -> &str {
        &self.current.id
    }

    /// The controller's docs gate keys, or `None` when no secret is configured.
    /// A configured secret that is too short is an error, never ignored.
    pub fn from_env() -> Result<Option<Self>> {
        let env = crate::EnvConfig::from_env()?;
        let configured = |value: Option<String>| value.filter(|s| !s.trim().is_empty());
        let (secret, source) = match configured(env.docs_gate.secret) {
            Some(secret) => (secret, "OPENFLOWS_DOCS_GATE_SECRET"),
            None => match configured(env.hooks.chat_hook_secret) {
                Some(secret) => (secret, "CODER_CHAT_HOOK_SECRET"),
                None => return Ok(None),
            },
        };
        ensure!(
            secret.len() >= MIN_GATE_SECRET_LEN,
            "{source} must be at least {MIN_GATE_SECRET_LEN} bytes to seal docs approvals"
        );
        let mut keys = Self::new(&secret);
        if let Some(previous) = configured(env.docs_gate.previous_secret) {
            ensure!(
                previous.len() >= MIN_GATE_SECRET_LEN,
                "OPENFLOWS_DOCS_GATE_SECRET_PREVIOUS must be at least {MIN_GATE_SECRET_LEN} bytes"
            );
            keys = keys.with_previous(&previous);
        }
        Ok(Some(keys))
    }

    /// Seal an operator approval of `ticket` docs `revision` at `head`.
    pub fn seal(&self, ticket: &str, revision: u64, head: &str) -> String {
        let mac = self
            .current
            .mac(ticket, revision, head)
            .finalize()
            .into_bytes();
        format!("{SEAL_VERSION}:{}:{}", self.current.id, hex(&mac))
    }

    /// The approved head of `record`, if its decision is an approval whose seal
    /// verifies for `ticket` with one of these keys. Callers must use this (not
    /// the raw record fields) before opening or merging a docs PR.
    pub fn verify<'a>(&self, record: &'a DocsRecord, ticket: &str) -> Result<&'a str> {
        let Some(decision) = record.decision.as_ref().filter(|d| d.approved) else {
            bail!("Docs record for {ticket} carries no operator approval");
        };
        let Some((key_id, mac)) = decision
            .seal
            .strip_prefix(SEAL_VERSION)
            .and_then(|rest| rest.strip_prefix(':'))
            .and_then(|rest| rest.split_once(':'))
        else {
            bail!("Docs approval for {ticket} has an invalid seal; it was not issued by the operator CLI");
        };
        let Some(key) = std::iter::once(&self.current)
            .chain(self.previous.as_ref())
            .find(|key| key.id == key_id)
        else {
            bail!(
                "Docs approval for {ticket} was sealed with key {key_id}, which this controller does not hold (current key {}); if the secret was rotated, approve revision {} again with `openflows gate docs`",
                self.current.id,
                decision.revision
            );
        };
        ensure!(
            key.mac(ticket, decision.revision, &decision.head)
                .verify_slice(&decode_hex(mac).unwrap_or_default())
                .is_ok(),
            "Docs approval for {ticket} has an invalid seal; it was not issued by the operator CLI"
        );
        ensure!(
            decision.revision == record.revision
                && record.head.as_deref() == Some(decision.head.as_str()),
            "Docs approval for {ticket} does not match the submitted revision and head"
        );
        Ok(&decision.head)
    }
}

/// [`DocsGateKeys::from_env`] for long-running components: a misconfigured
/// secret is logged and treated as absent, so NEXUS and VESSEL
/// refuse to open or merge docs PRs instead of failing to start.
pub fn gate_keys_from_env_logged() -> Option<DocsGateKeys> {
    match DocsGateKeys::from_env() {
        Ok(keys) => keys,
        Err(error) => {
            tracing::error!(%error, "Docs gate disabled: the docs gate secret is misconfigured");
            None
        }
    }
}

/// Seal with `secret` alone; see [`DocsGateKeys::seal`].
pub fn approval_seal(secret: &str, ticket: &str, revision: u64, head: &str) -> String {
    DocsGateKeys::new(secret).seal(ticket, revision, head)
}

fn decode_hex(value: &str) -> Option<Vec<u8>> {
    if !value.len().is_multiple_of(2) {
        return None;
    }
    (0..value.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(value.get(i..i + 2)?, 16).ok())
        .collect()
}

/// Verify with `secret` alone; see [`DocsGateKeys::verify`].
pub fn verify_approval<'a>(record: &'a DocsRecord, ticket: &str, secret: &str) -> Result<&'a str> {
    DocsGateKeys::new(secret).verify(record, ticket)
}

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
mod tests {
    use super::*;

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
}

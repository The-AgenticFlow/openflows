//! LORE post-merge documentation driver.
//!
//! LORE is not a flow node. For every merged ticket NEXUS reads the durable
//! docs record (`ticket:{id}:docs`, reducer [`config::docs_lifecycle`]) and
//! moves it at most one step per poll:
//!
//! - **drafting** — claim the LORE slot, provision its workspace, and deliver
//!   the instruction the record calls for into LORE's Coder chat. A changed
//!   record epoch (resume, restart, rework, reopen) replaces the workspace.
//! - **docs_ready** — notify the operator once per revision, then release the
//!   slot and workspace (the work is pushed), so one ticket waiting on a human
//!   never blocks documentation for the others.
//! - **approved** — verify the operator's sealed approval, that the branch on
//!   GitHub is still at the approved head, and (from GitHub, not from what
//!   LORE reported) that the head changes only documentation relative to the
//!   default branch; then open (or find) the docs PR into the default branch
//!   and record it. No workspace is involved.
//! - **published** — follow the docs PR on GitHub: a merge closes the record,
//!   a PR closed without merging abandons it. VESSEL merges and reworks it.
//! - **paused** — release LORE so other tickets can use the slot; send nothing.
//! - **merged / abandoned / skipped** — release LORE, forget delivery state,
//!   and stop reading the record until it is reopened ([`ClosedDocsCache`]).
//!
//! A step with no recorded progress for [`LORE_STALL_SECS`] is nudged up to
//! [`LORE_MAX_NUDGES`] times; after that, or after [`LORE_STEP_MAX_SECS`] on
//! one step even while the chat reports running, NEXUS parks the record
//! (pauses it), frees the slot, and asks the operator to resume or skip. The
//! same happens when LORE's workspace or chat has not come up for
//! [`LORE_PROVISION_MAX_SECS`], or its workspace crashed [`LORE_MAX_CRASHES`]
//! times, so a workspace that never boots cannot loop silently.

use super::*;
use std::collections::HashSet;

mod drafting;
mod publish;

/// A delivered step with no recorded progress for this long is nudged.
pub(crate) const LORE_STALL_SECS: u64 = 30 * 60;
/// Nudges per step before LORE is parked for the operator.
pub(crate) const LORE_MAX_NUDGES: u32 = 3;
/// Hard ceiling for one step, even while the chat reports it is running.
pub(crate) const LORE_STEP_MAX_SECS: u64 = 4 * 60 * 60;
/// How long LORE's workspace or chat may keep failing to come up before LORE
/// is parked for the operator.
pub(crate) const LORE_PROVISION_MAX_SECS: u64 = 30 * 60;
/// Workspace crashes per record epoch before LORE is parked for the operator.
pub(crate) const LORE_MAX_CRASHES: u32 = 3;
/// Closed docs records are re-read at least this often, as a safety net for
/// a reopen whose marker write was lost.
const CLOSED_DOCS_REFRESH_SECS: u64 = 60 * 60;

/// Merged tickets whose docs records are closed. Cleared whenever the reopen
/// marker ([`KEY_DOCS_REOPENED`]) changes, and at least hourly.
#[derive(Debug, Default)]
pub(crate) struct ClosedDocsCache {
    tickets: HashSet<String>,
    marker: Option<Value>,
    refreshed_at: u64,
}

/// Control plane hosting LORE's chat: direct Coder (local) or Manager (hosted).
pub(crate) enum LoreBackend<'a> {
    Coder(CoderClient),
    Managed(&'a runtime_control::RuntimeControlClient),
}

/// NEXUS's private record of what it last told LORE for a ticket.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct LoreDelivery {
    /// Instruction key last delivered (see [`NexusNode::lore_instruction`]).
    pub step: String,
    /// When `step` (or its last nudge) was delivered, in seconds.
    pub ts: u64,
    /// When `step` was first delivered, in seconds (the hard step limit).
    pub started: u64,
    /// Nudges sent for `step`.
    pub nudges: u32,
    /// Docs revision the operator was last notified about.
    pub ready_notified: Option<u64>,
    /// Record epoch the current workspace and chat serve.
    pub epoch: u64,
    /// Since when LORE's workspace or chat has failed to come up, in seconds.
    pub failing_since: Option<u64>,
    /// Workspace crashes in this epoch.
    pub crashes: u32,
    /// Record version whose unverifiable approval the operator was told about.
    pub seal_alert: Option<u64>,
}

/// What became of a LORE chat for this ticket.
pub(crate) enum LoreChat {
    /// A new chat was created with the instruction as its first message.
    Created,
    /// A live chat already serves this ticket's bound workspace.
    Existing {
        chat_id: String,
        status: Option<ChatStatus>,
    },
}

/// Marker the removed inline LORE wrote for documented tickets
/// (`ticket:{id}:lore_done`). Read only by the one-time cutover; remove with it.
pub(crate) const LEGACY_LORE_DONE_KEY: &str = "lore_done";

pub(crate) fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

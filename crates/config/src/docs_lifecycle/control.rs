//! The operator's latest docs decision, kept where workspaces cannot write.
//!
//! Workspaces hold tenant Redis credentials, so they can rewrite
//! `ticket:{id}:docs` at will, including restoring an older record whose sealed
//! approval the operator has since revoked. The seal proves that an approval
//! was issued once; it cannot prove that it is still the operator's latest word.
//!
//! [`DocsControl`] records that latest word: the approved revision and head (if
//! any) and the pause flag, as of the operator's last action. The operator CLI
//! writes it to the controller-only key prefix (`ctl:{tenant}:`, see the store)
//! in the same atomic step as the docs record, under the docs record's version
//! check (`SharedStore::docs_operator_transition`): a command that loses the
//! check writes neither, so it can never replace a newer decision. NEXUS
//! (before opening a docs PR) and VESSEL (before merging one) require the docs
//! record to agree with it.
use anyhow::{ensure, Result};
use serde::{Deserialize, Serialize};

use super::{DocsEvent, DocsRecord};

/// An approved docs revision and the head the operator approved.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ApprovedDocs {
    pub revision: u64,
    pub head: String,
}

/// The operator's latest docs decision for one ticket.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(default)]
pub struct DocsControl {
    /// The approval in force, or `None` after a rejection, rework, reopen or
    /// before any approval.
    pub approved: Option<ApprovedDocs>,
    pub paused: bool,
    /// The docs record version this decision produced (informational).
    pub version: u64,
    pub ts: u64,
}

impl DocsControl {
    /// The operator state a docs record expresses: its approval counts only if
    /// it is an approval of the record's current revision and head.
    pub fn from_record(record: &DocsRecord) -> Self {
        let approved = record
            .decision
            .as_ref()
            .filter(|d| {
                d.approved
                    && d.revision == record.revision
                    && record.head.as_deref() == Some(d.head.as_str())
            })
            .map(|d| ApprovedDocs {
                revision: d.revision,
                head: d.head.clone(),
            });
        Self {
            approved,
            paused: record.paused,
            version: record.version,
            ts: record.ts,
        }
    }

    /// The operator's latest decision after an operator `event` produced
    /// `next`. Only a decision can grant an approval; any other operator
    /// action (pause, resume, restart, skip, reopen) keeps the approval
    /// already in force, or revokes it if `next` no longer carries it. The
    /// record is workspace-writable, so a sealed approval a workspace restored
    /// after a rejection must never be promoted by a non-decision.
    pub fn after_operator(previous: Option<&Self>, event: &DocsEvent, next: &DocsRecord) -> Self {
        let derived = Self::from_record(next);
        if matches!(event, DocsEvent::Decide { .. }) {
            return derived;
        }
        let approved = previous
            .and_then(|p| p.approved.clone())
            .filter(|kept| derived.approved.as_ref() == Some(kept));
        Self {
            approved,
            ..derived
        }
    }

    /// Refuse `record`'s approval unless it is the operator's latest decision
    /// and the operator has not paused the docs. `control` is `None` when the
    /// controller-only record is absent.
    pub fn confirm(control: Option<&Self>, record: &DocsRecord, ticket: &str) -> Result<()> {
        let Some(control) = control else {
            anyhow::bail!(
                "No operator decision for {ticket} is recorded where workspaces cannot write it; approve revision {} again with `openflows gate docs` from the controller",
                record.revision
            );
        };
        ensure!(
            !control.paused,
            "The operator paused documentation for {ticket}; resume it with `openflows gate docs --resume`"
        );
        let current = Self::from_record(record).approved;
        ensure!(
            current.is_some() && current == control.approved,
            "The docs record for {ticket} carries an approval that is not the operator's latest decision ({}); it was changed outside the operator CLI",
            match &control.approved {
                Some(a) => format!("approved revision {} at {}", a.revision, a.head),
                None => "no approval in force".into(),
            }
        );
        Ok(())
    }
}

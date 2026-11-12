//! Per-role controller rules ([`RoleTraits`]).

use crate::NexusNode;

/// Controller rules that differ by worker role, kept in one place so a new
/// role (or a change to one) does not mean hunting for string comparisons.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RoleTraits {
    /// Takes part in FORGE/SENTINEL A2A verification (gets a pair token).
    pub uses_a2a: bool,
    /// Starts its workspace from the default branch, never a ticket's PR branch.
    pub starts_from_default_branch: bool,
    /// Its chats belong to a dedicated scheduler (SENTINEL reviews, the LORE
    /// docs driver), never to generic chat creation or recovery.
    pub scheduler_owns_chats: bool,
    /// Works on merged (terminal) tickets, so terminal-ticket recycling must
    /// not reclaim it while its work is outstanding.
    pub serves_terminal_tickets: bool,
    /// Workspace crashes count against (and may escalate) the ticket.
    pub crashes_count_against_ticket: bool,
}

impl RoleTraits {
    pub(crate) const BUILDER: Self = Self {
        uses_a2a: true,
        starts_from_default_branch: false,
        scheduler_owns_chats: false,
        serves_terminal_tickets: false,
        crashes_count_against_ticket: true,
    };

    pub(crate) fn of(worker_id: &str) -> Self {
        match NexusNode::worker_role(worker_id) {
            "sentinel" => Self {
                scheduler_owns_chats: true,
                ..Self::BUILDER
            },
            "lore" => Self {
                uses_a2a: false,
                starts_from_default_branch: true,
                scheduler_owns_chats: true,
                serves_terminal_tickets: true,
                crashes_count_against_ticket: false,
            },
            _ => Self::BUILDER,
        }
    }
}

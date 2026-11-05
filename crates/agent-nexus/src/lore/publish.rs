//! LORE docs publication: operator notification, approval verification and
//! opening and reconciling the docs PR. See [`super`] for the lifecycle.

use super::*;
use config::docs_lifecycle::{non_docs_paths, DocsEvent, DocsRecord};
use config::state::{KEY_TICKET_WORKSPACE, KEY_WORKER_SLOTS};

impl NexusNode {
    pub(super) async fn docs_compare_link(
        &self,
        store: &SharedStore,
        record: &DocsRecord,
    ) -> Option<String> {
        let head = record.head.as_deref()?;
        let (github, owner, repo) = self.docs_github(store).await?;
        let base = match github.get_default_branch(&owner, &repo).await {
            Ok(base) => base,
            Err(error) => {
                warn!(%error, "Cannot read the default branch for the docs review link");
                return None;
            }
        };
        Some(format!(
            "https://github.com/{owner}/{repo}/compare/{base}...{head}"
        ))
    }

    /// Tell the operator, once per docs revision, that LORE's docs await a decision.
    pub(super) async fn notify_lore_docs_ready(
        &self,
        store: &SharedStore,
        ticket: &Ticket,
        record: &DocsRecord,
    ) {
        let key = Self::lore_delivery_key(&ticket.id);
        let mut delivery: LoreDelivery = store.get_typed(&key).await.unwrap_or_default();
        if delivery.ready_notified == Some(record.revision) {
            return;
        }
        let head = record.head.as_deref().unwrap_or_default();
        let compare = self.docs_compare_link(store, record).await;
        if !NotificationService::from_env().is_configured() {
            warn!(ticket_id = %ticket.id, "No notification channel is configured; operators find docs awaiting a decision with `openflows gate docs --pending`");
        }
        let reason = format!(
            "LORE docs revision {} on {} ({} files reported by LORE; the controller re-checks them on GitHub before publishing) awaiting approval{}: openflows gate docs --tenant {} --ticket {} --verdict approve|reject --revision {} --head {} --notes <notes>",
            record.revision,
            record.branch.as_deref().unwrap_or_default(),
            record.files.len(),
            compare
                .as_deref()
                .map(|link| format!(" (review exactly this head: {link})"))
                .unwrap_or_default(),
            store.tenant(),
            ticket.id,
            record.revision,
            head
        );
        let live_slots: HashMap<String, WorkerSlot> =
            store.get_typed(KEY_WORKER_SLOTS).await.unwrap_or_default();
        let worker_id = Self::lore_slot_for(&ticket.id, &live_slots)
            .filter(|(_, _, bound)| *bound)
            .map(|(id, _, _)| id);
        self.notify_awaiting_human(
            store,
            &ticket.id,
            worker_id.as_deref(),
            &reason,
            compare.or_else(|| ticket.issue_url.clone()),
        )
        .await;
        delivery.ready_notified = Some(record.revision);
        store.set(&key, json!(delivery)).await;
        info!(ticket_id = %ticket.id, revision = record.revision, head, "LORE docs awaiting operator decision");
    }

    /// The approved head of `record` if its seal verifies and it is the
    /// operator's latest decision (`SharedStore::verified_docs_head`). Otherwise the
    /// operator is told, once per record version, why nothing will be opened
    /// or merged and how to approve again.
    pub(super) async fn verified_docs_head(
        &self,
        store: &SharedStore,
        ticket: &Ticket,
        record: &DocsRecord,
    ) -> Option<String> {
        let Some(keys) = self.docs_gate_keys.as_ref() else {
            warn!(ticket_id = %ticket.id, "No docs gate secret (OPENFLOWS_DOCS_GATE_SECRET or CODER_CHAT_HOOK_SECRET) is configured; NEXUS cannot verify docs approvals and will not open docs PRs");
            return None;
        };
        let error = match store.verified_docs_head(&ticket.id, record, keys).await {
            Ok(head) => return Some(head),
            Err(error) => error,
        };
        error!(ticket_id = %ticket.id, %error, "Refusing to open or merge a docs PR without a valid operator approval");
        let key = Self::lore_delivery_key(&ticket.id);
        let mut delivery: LoreDelivery = store.get_typed(&key).await.unwrap_or_default();
        if delivery.seal_alert != Some(record.version) {
            let reason = format!(
                "{error:#}. The docs PR will be neither opened nor merged. If you approved it, approve it again with `openflows gate docs --tenant {tenant} --ticket {id} --verdict approve --revision {revision} --head {head}`; otherwise reject it with `--verdict reject`.",
                tenant = store.tenant(),
                id = ticket.id,
                revision = record.revision,
                head = record.head.as_deref().unwrap_or_default(),
            );
            self.notify_awaiting_human(store, &ticket.id, None, &reason, ticket.issue_url.clone())
                .await;
            delivery.seal_alert = Some(record.version);
            store.set(&key, json!(delivery)).await;
        }
        None
    }

    /// Send approved docs back to LORE because the controller will not publish
    /// them as they are, and tell the operator who approved them.
    pub(super) async fn refuse_docs_publication(
        &self,
        store: &SharedStore,
        ticket: &Ticket,
        record: &DocsRecord,
        reason: String,
    ) {
        if Self::docs_event(
            store,
            &ticket.id,
            record,
            DocsEvent::Rework {
                reason: reason.clone(),
            },
        )
        .await
        .is_some()
        {
            let notice = format!(
                "NEXUS refused to publish the approved LORE docs for {}: {reason} LORE was asked to fix it; review the next submission.",
                ticket.id
            );
            self.notify_awaiting_human(store, &ticket.id, None, &notice, ticket.issue_url.clone())
                .await;
        }
    }

    /// Open (or find) the docs PR for an approved record and record it. The
    /// approval seal must verify, the branch on GitHub must still be at the
    /// approved head, and GitHub's own diff of that head against the default
    /// branch must touch documentation only (the file list LORE submitted is
    /// not trusted); otherwise the docs go back to LORE for a new review. The
    /// PR always targets the default branch: an open PR from the docs branch
    /// into any other base is never adopted (VESSEL hands it to a human).
    pub(super) async fn publish_docs_pr(
        &self,
        store: &SharedStore,
        ticket: &Ticket,
        record: &DocsRecord,
    ) {
        let Some(approved_head) = self.verified_docs_head(store, ticket, record).await else {
            return;
        };
        let Some(branch) = record.branch.clone() else {
            return;
        };
        let Some((github, owner, repo)) = self.docs_github(store).await else {
            return;
        };
        match github.get_branch_head(&owner, &repo, &branch).await {
            Ok(Some(head)) if head == approved_head => {}
            Ok(current) => {
                let reason = format!(
                    "Branch {branch} is at {} on GitHub, not the approved head {approved_head}. Make the branch hold exactly the documentation you want reviewed, push, and submit again.",
                    current.as_deref().unwrap_or("nothing (deleted)")
                );
                Self::docs_event(store, &ticket.id, record, DocsEvent::Rework { reason }).await;
                return;
            }
            Err(error) => {
                warn!(ticket_id = %ticket.id, %error, "Cannot read docs branch head; retrying next poll");
                return;
            }
        }
        let base = match github.get_default_branch(&owner, &repo).await {
            Ok(base) => base,
            Err(error) => {
                warn!(ticket_id = %ticket.id, %error, "Cannot read default branch; retrying next poll");
                return;
            }
        };
        match github
            .compare_file_paths(&owner, &repo, &base, &approved_head)
            .await
        {
            Ok(Some(paths)) if paths.is_empty() => {
                let reason = format!(
                    "The approved head {approved_head} changes nothing relative to {base}. Commit the documentation on {branch}, push, and submit again."
                );
                self.refuse_docs_publication(store, ticket, record, reason)
                    .await;
                return;
            }
            Ok(Some(paths)) => {
                let outside = non_docs_paths(paths.iter().map(String::as_str));
                if !outside.is_empty() {
                    error!(ticket_id = %ticket.id, head = %approved_head, files = ?outside, "Approved docs head changes files that are not documentation");
                    let reason = format!(
                        "On GitHub the approved head {approved_head} changes files that are not documentation relative to {base}: {}. Make {branch} change documentation only (based on {base}), push, and submit again.",
                        outside.join(", ")
                    );
                    self.refuse_docs_publication(store, ticket, record, reason)
                        .await;
                    return;
                }
            }
            Ok(None) => {
                let reason = format!(
                    "The approved head {approved_head} changes too many files relative to {base} for GitHub to list them all. Split the documentation into a smaller change, push, and submit again."
                );
                self.refuse_docs_publication(store, ticket, record, reason)
                    .await;
                return;
            }
            Err(error) => {
                warn!(ticket_id = %ticket.id, %error, "Cannot compare the docs head with the default branch; retrying next poll");
                return;
            }
        }
        let number = match github
            .find_open_pr_by_head(&owner, &repo, &branch, &base)
            .await
        {
            Ok(Some(pr)) => pr.number,
            Ok(None) => {
                let title = format!("docs({}): {}", ticket.id, ticket.title);
                let body = format!(
                    "{}\n\n---\nLORE documentation for {}, revision {} at `{approved_head}`, approved by the operator. VESSEL merges it only at that head.",
                    record.report, ticket.id, record.revision
                );
                match github
                    .create_pull_request(&owner, &repo, &title, &branch, &base, Some(&body))
                    .await
                {
                    Ok(number) => number,
                    Err(error) => {
                        warn!(ticket_id = %ticket.id, %error, "Could not open docs PR; retrying next poll");
                        return;
                    }
                }
            }
            Err(error) => {
                warn!(ticket_id = %ticket.id, %error, "Cannot look up docs PR; retrying next poll");
                return;
            }
        };
        if Self::docs_event(store, &ticket.id, record, DocsEvent::Published { number })
            .await
            .is_some()
        {
            info!(ticket_id = %ticket.id, pr_number = number, "Opened docs PR at the approved head");
        }
    }

    /// Follow a published docs PR: a merge (by VESSEL or a human) closes the
    /// record, and a PR closed without merging abandons it.
    pub(super) async fn reconcile_docs_pr(
        &self,
        store: &SharedStore,
        ticket: &Ticket,
        record: &DocsRecord,
    ) {
        let Some(number) = record.pr_number else {
            return;
        };
        let Some((github, owner, repo)) = self.docs_github(store).await else {
            return;
        };
        let pr = match github.get_pull_request(&owner, &repo, number).await {
            Ok(pr) => pr,
            Err(error) => {
                debug!(ticket_id = %ticket.id, %error, "Cannot read docs PR; retrying next poll");
                return;
            }
        };
        match pr.state {
            pocketflow_core::PrState::Open => {}
            pocketflow_core::PrState::Merged => {
                let sha = github
                    .confirmed_merge_sha(&owner, &repo, number)
                    .await
                    .ok()
                    .flatten()
                    .unwrap_or_default();
                Self::docs_event(store, &ticket.id, record, DocsEvent::Merged { number, sha })
                    .await;
            }
            pocketflow_core::PrState::Closed => {
                let reason = format!("Docs PR #{number} was closed without merging");
                if Self::docs_event(
                    store,
                    &ticket.id,
                    record,
                    DocsEvent::Abandoned {
                        reason: reason.clone(),
                    },
                )
                .await
                .is_some()
                {
                    let reason = format!(
                        "{reason}; the docs record is closed. Reopen it with `openflows gate docs --tenant {} --ticket {} --reopen <reason>` to document it again.",
                        store.tenant(),
                        ticket.id
                    );
                    self.notify_awaiting_human(
                        store,
                        &ticket.id,
                        None,
                        &reason,
                        ticket.issue_url.clone(),
                    )
                    .await;
                }
            }
        }
    }

    /// Release the LORE slot bound to `ticket` (destroying its workspace) and
    /// forget its chat. Returns whether no LORE slot remains bound. A failed
    /// teardown keeps the slot reserved and is retried on the next poll.
    pub(super) async fn release_lore_slot(
        &self,
        store: &SharedStore,
        ticket: &Ticket,
        why: &str,
    ) -> bool {
        let live_slots: HashMap<String, WorkerSlot> =
            store.get_typed(KEY_WORKER_SLOTS).await.unwrap_or_default();
        let Some((worker_id, _, true)) = Self::lore_slot_for(&ticket.id, &live_slots) else {
            return true;
        };
        self.release_worker_slot(store, &worker_id).await;
        let released = store
            .get_typed::<HashMap<String, WorkerSlot>>(KEY_WORKER_SLOTS)
            .await
            .unwrap_or_default()
            .get(&worker_id)
            .is_some_and(|slot| matches!(slot.status, WorkerStatus::Idle));
        if released {
            store.del(&Self::lore_chat_key(&ticket.id)).await;
            store
                .del(&full_ticket_key(&ticket.id, KEY_TICKET_WORKSPACE, "lore"))
                .await;
            info!(ticket_id = %ticket.id, worker_id, why, "Released LORE slot");
        }
        released
    }

    /// Closed docs: release LORE and drop delivery state. Returns whether
    /// nothing is left to clean up.
    pub(super) async fn finish_lore_docs(&self, store: &SharedStore, ticket: &Ticket) -> bool {
        if !self.release_lore_slot(store, ticket, "docs closed").await {
            return false;
        }
        store.del(&Self::lore_delivery_key(&ticket.id)).await;
        true
    }

    /// A worker that serves terminal tickets (LORE) keeps its slot while it
    /// still has work on one: terminal-ticket recycling must not reclaim it.
    /// An unreadable docs record keeps the slot too; recovery retries.
    pub(crate) async fn terminal_work_outstanding(
        store: &SharedStore,
        worker_id: &str,
        ticket_id: &str,
    ) -> bool {
        if !RoleTraits::of(worker_id).serves_terminal_tickets {
            return false;
        }
        match store.docs_record(ticket_id).await {
            Ok(record) => record.phase.needs_lore() && !record.paused,
            Err(error) => {
                warn!(worker_id, ticket_id, %error, "Cannot read docs record; keeping LORE slot");
                true
            }
        }
    }
}

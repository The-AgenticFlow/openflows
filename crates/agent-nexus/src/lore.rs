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
use config::docs_lifecycle::{non_docs_paths, DocsEvent, DocsPhase, DocsRecord, DOCS_TICKET};
use config::state::{KEY_DOCS_CUTOVER, KEY_DOCS_REOPENED, KEY_TICKET_DOCS_DELIVERY};
use std::collections::HashSet;

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

enum LoreChat {
    /// A new chat was created with the instruction as its first message.
    Created,
    /// A live chat already serves this ticket's bound workspace.
    Existing {
        chat_id: String,
        status: Option<ChatStatus>,
    },
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

impl NexusNode {
    fn lore_delivery_key(ticket_id: &str) -> String {
        full_ticket_key_flat(ticket_id, KEY_TICKET_DOCS_DELIVERY)
    }

    fn lore_chat_key(ticket_id: &str) -> String {
        full_ticket_key(ticket_id, KEY_TICKET_CHAT, "lore")
    }

    fn is_documentable(ticket: &Ticket) -> bool {
        matches!(ticket.status, TicketStatus::Merged { .. }) && ticket.id != DOCS_TICKET
    }

    /// LORE is enabled when the registry gives it at least one worker slot.
    pub(crate) async fn lore_enabled(store: &SharedStore) -> bool {
        store
            .get_typed::<HashMap<String, WorkerSlot>>(KEY_WORKER_SLOTS)
            .await
            .unwrap_or_default()
            .keys()
            .any(|id| Self::worker_role(id) == "lore")
    }

    /// Run once per poll: invalidate the closed-docs cache after a reopen.
    pub(crate) async fn refresh_closed_docs(&self, store: &SharedStore) {
        let marker = store.get(KEY_DOCS_REOPENED).await;
        let now = now_secs();
        let mut cache = self.docs_closed.lock().unwrap();
        if cache.marker != marker
            || now.saturating_sub(cache.refreshed_at) >= CLOSED_DOCS_REFRESH_SECS
        {
            cache.tickets.clear();
            cache.marker = marker;
            cache.refreshed_at = now;
        }
    }

    fn docs_known_closed(&self, ticket_id: &str) -> bool {
        self.docs_closed.lock().unwrap().tickets.contains(ticket_id)
    }

    fn remember_docs_closed(&self, ticket_id: &str) {
        self.docs_closed
            .lock()
            .unwrap()
            .tickets
            .insert(ticket_id.to_string());
    }

    /// One-time cutover: tickets that merged before the docs gate existed are
    /// skipped instead of all being documented on the first boot. Operators
    /// can still document one with `openflows gate docs --reopen`. The marker
    /// is written only after every such ticket was handled, so a store error
    /// retries the cutover on the next poll.
    pub(crate) async fn docs_cutover(&self, store: &SharedStore, tickets: &[Ticket]) {
        if store.get(KEY_DOCS_CUTOVER).await.is_some() {
            return;
        }
        let mut complete = true;
        let mut skipped = 0usize;
        for ticket in tickets.iter().filter(|t| Self::is_documentable(t)) {
            match store.docs_record(&ticket.id).await {
                Ok(record) if record.version == 0 => {
                    let legacy = store
                        .get(&full_ticket_key_flat(&ticket.id, LEGACY_LORE_DONE_KEY))
                        .await
                        .is_some();
                    let reason = if legacy {
                        "documented by the legacy inline LORE".to_string()
                    } else {
                        "merged before the LORE docs gate; document it with `openflows gate docs --reopen`".to_string()
                    };
                    match store
                        .docs_transition(&ticket.id, 0, "nexus", DocsEvent::Skip { reason })
                        .await
                    {
                        Ok(_) => skipped += 1,
                        Err(error) => {
                            warn!(ticket_id = %ticket.id, %error, "Docs cutover could not skip ticket; retrying next poll");
                            complete = false;
                        }
                    }
                }
                Ok(_) => {}
                Err(error) => {
                    warn!(ticket_id = %ticket.id, %error, "Docs cutover cannot read docs record; retrying next poll");
                    complete = false;
                }
            }
        }
        if complete {
            store
                .set(
                    KEY_DOCS_CUTOVER,
                    json!({ "ts": now_secs(), "skipped": skipped }),
                )
                .await;
            info!(
                skipped,
                "LORE docs cutover complete; earlier merged tickets were skipped"
            );
        }
    }

    async fn lore_backend(&self, store: &SharedStore) -> Option<LoreBackend<'_>> {
        match &self.runtime_control {
            Some(runtime) => Some(LoreBackend::Managed(runtime)),
            None => Self::coder_client_from_store(store)
                .await
                .map(LoreBackend::Coder),
        }
    }

    /// The tenant repository as `(owner, name)`.
    async fn docs_repository(store: &SharedStore) -> Option<(String, String)> {
        let repository = config::EnvConfig::from_env()
            .ok()
            .and_then(|env| env.github.repository)
            .or(store.get_typed::<String>("repository").await)?;
        let (owner, name) = repository.trim().split_once('/')?;
        (!owner.is_empty() && !name.is_empty() && !name.contains('/'))
            .then(|| (owner.to_string(), name.to_string()))
    }

    /// GitHub client and repository for the controller's docs PR work.
    async fn docs_github(
        &self,
        store: &SharedStore,
    ) -> Option<(github::GithubRestClient, String, String)> {
        let Some((owner, repo)) = Self::docs_repository(store).await else {
            warn!("Tenant repository is not configured; cannot manage docs PRs");
            return None;
        };
        let client = match &self.github_override {
            Some((token, api_base)) => github::GithubRestClient::with_api_base(token, api_base),
            None => match self.github_api_token().await {
                Ok(token) => github::GithubRestClient::new(&token),
                Err(error) => {
                    warn!(%error, "No GitHub token; cannot manage docs PRs");
                    return None;
                }
            },
        };
        Some((client, owner, repo))
    }

    async fn docs_event(
        store: &SharedStore,
        ticket_id: &str,
        record: &DocsRecord,
        event: DocsEvent,
    ) -> Option<DocsRecord> {
        match store
            .docs_transition(ticket_id, record.version, "nexus", event)
            .await
        {
            Ok(next) => Some(next),
            Err(error) => {
                warn!(ticket_id, %error, "Docs record transition failed; retrying next poll");
                None
            }
        }
    }

    /// Move one merged ticket's documentation forward by at most one step.
    /// Every failure is logged and retried on the next poll.
    pub(crate) async fn drive_lore_docs(&self, store: &SharedStore, ticket: &Ticket) {
        if !Self::is_documentable(ticket) || self.docs_known_closed(&ticket.id) {
            return;
        }
        let record = match store.docs_record(&ticket.id).await {
            Ok(record) => record,
            Err(error) => {
                warn!(ticket_id = %ticket.id, %error, "Cannot load docs record; skipping LORE");
                return;
            }
        };
        if record.phase.is_terminal() {
            if self.finish_lore_docs(store, ticket).await {
                self.remember_docs_closed(&ticket.id);
            }
            return;
        }
        if record.paused {
            // A hold must not keep the (usually single) LORE slot from others.
            self.release_lore_slot(store, ticket, "docs paused").await;
            return;
        }
        match record.phase {
            DocsPhase::Drafting => Box::pin(self.drive_lore_drafting(store, ticket, &record)).await,
            DocsPhase::DocsReady => {
                self.notify_lore_docs_ready(store, ticket, &record).await;
                self.release_lore_slot(store, ticket, "docs await the operator")
                    .await;
            }
            DocsPhase::Approved => {
                self.release_lore_slot(store, ticket, "docs approved").await;
                Box::pin(self.publish_docs_pr(store, ticket, &record)).await;
            }
            DocsPhase::Published => {
                self.release_lore_slot(store, ticket, "docs PR open").await;
                // VESSEL will not merge an approval that does not verify; say so.
                self.verified_docs_head(store, ticket, &record).await;
                Box::pin(self.reconcile_docs_pr(store, ticket, &record)).await;
            }
            DocsPhase::Merged | DocsPhase::Abandoned | DocsPhase::Skipped => {}
        }
    }

    async fn drive_lore_drafting(&self, store: &SharedStore, ticket: &Ticket, record: &DocsRecord) {
        let Some((step_key, step)) = Self::lore_instruction(ticket, record) else {
            return;
        };
        let delivery_key = Self::lore_delivery_key(&ticket.id);
        let delivery: LoreDelivery = store.get_typed(&delivery_key).await.unwrap_or_default();
        if delivery.epoch != record.epoch {
            // Resume, restart, rework or reopen: LORE starts over on a fresh
            // workspace and chat instead of a possibly wedged one, with its
            // failure and crash counts cleared.
            if !self
                .release_lore_slot(store, ticket, "docs epoch changed")
                .await
            {
                return;
            }
            store
                .set(
                    &delivery_key,
                    json!(LoreDelivery {
                        epoch: record.epoch,
                        ready_notified: delivery.ready_notified,
                        ..Default::default()
                    }),
                )
                .await;
        } else if delivery.crashes >= LORE_MAX_CRASHES {
            let reason = format!(
                "LORE's workspace crashed {} times while documenting {}",
                delivery.crashes, ticket.id
            );
            self.park_lore(store, ticket, record, &reason).await;
            return;
        }
        let Some(backend) = self.lore_backend(store).await else {
            debug!("No Coder client available — cannot drive LORE");
            return;
        };
        Box::pin(self.deliver_lore_step(store, ticket, &backend, record, &step_key, &step)).await;
    }

    fn lore_start_prompt(ticket: &Ticket, branch: &str) -> String {
        format!(
            "Document merged ticket {id}: {title}. Read `openflows-harness dispatch read` and `openflows-harness docs status`. Follow the lore-changelog and lore-documentation skills: update the changelog, and add an ADR under docs/ only for architectural decisions. Change only documentation (Markdown/reST/AsciiDoc, changelogs, text and images under docs/; never dot-directories such as .github/, orchestration/, agent instructions, legal files, MDX, or build inputs such as requirements.txt); `docs submit` refuses anything else and the controller re-checks it on GitHub. {branch_rule} Commit, push the branch to origin, write a short summary to /tmp/docs-report.md (outside the repository), and run `openflows-harness docs submit --report /tmp/docs-report.md`. Then stop: the operator approves or rejects the submitted head, and after approval the controller opens and merges the docs PR. Never open a PR yourself.",
            id = ticket.id,
            title = ticket.title,
            branch_rule = Self::lore_branch_rule(branch),
        )
    }

    fn lore_branch_rule(branch: &str) -> String {
        format!(
            "Work on branch `{branch}`: if it exists on origin, check it out and continue from it (pull it first); otherwise create it from the repository default branch."
        )
    }

    /// The instruction the docs record calls for, keyed so that it is
    /// delivered once per revision, docs cycle and epoch.
    pub(crate) fn lore_instruction(
        ticket: &Ticket,
        record: &DocsRecord,
    ) -> Option<(String, String)> {
        if !record.phase.needs_lore() {
            return None;
        }
        let branch = record
            .branch
            .clone()
            .unwrap_or_else(|| record.expected_branch(&ticket.id));
        let epoch = record.epoch;
        Some(match record.feedback.as_deref() {
            None => (format!("start:e{epoch}"), Self::lore_start_prompt(ticket, &branch)),
            Some(feedback) => (
                format!("feedback:{}:c{}:e{epoch}", record.revision, record.cycle),
                format!(
                    "The documentation for {id} needs another revision. Feedback: {feedback}\n{branch_rule} Address the feedback (documentation only), commit, push, write a summary to /tmp/docs-report.md, and run `openflows-harness docs submit --report /tmp/docs-report.md` again. Then stop.",
                    id = ticket.id,
                    branch_rule = Self::lore_branch_rule(&branch),
                ),
            ),
        })
    }

    /// Prefer the LORE slot already bound to this ticket, else an idle one.
    pub(crate) fn lore_slot_for(
        ticket_id: &str,
        slots: &HashMap<String, WorkerSlot>,
    ) -> Option<(String, WorkerSlot, bool)> {
        let mut lore: Vec<(&String, &WorkerSlot)> = slots
            .iter()
            .filter(|(id, _)| Self::worker_role(id) == "lore")
            .collect();
        // Deterministic choice, so a fairness problem is reproducible.
        lore.sort_by(|a, b| a.0.cmp(b.0));
        lore.iter()
            .find(|(_, slot)| slot.status.ticket_id() == Some(ticket_id))
            .map(|(id, slot)| ((*id).clone(), (*slot).clone(), true))
            .or_else(|| {
                lore.iter()
                    .find(|(_, slot)| matches!(slot.status, WorkerStatus::Idle))
                    .map(|(id, slot)| ((*id).clone(), (*slot).clone(), false))
            })
    }

    /// Bind a LORE slot to `ticket` *before* provisioning, so no other ticket
    /// can pick up a workspace provisioned for this one (its harness is bound
    /// to the ticket it was created for). An idle slot that still holds a
    /// workspace is released first (taking one extra poll); it is never
    /// reused across tickets.
    async fn claim_lore_slot(&self, store: &SharedStore, ticket: &Ticket) -> Option<String> {
        let slots: HashMap<String, WorkerSlot> =
            store.get_typed(KEY_WORKER_SLOTS).await.unwrap_or_default();
        let Some((worker_id, slot, bound)) = Self::lore_slot_for(&ticket.id, &slots) else {
            debug!(ticket_id = %ticket.id, "No LORE slot available for merged ticket");
            return None;
        };
        if bound {
            return Some(worker_id);
        }
        if slot.workspace_id.is_some() {
            info!(ticket_id = %ticket.id, worker_id, "Idle LORE slot still holds a workspace — releasing it before reuse");
            self.release_worker_slot(store, &worker_id).await;
            return None;
        }
        let mut slots: HashMap<String, WorkerSlot> =
            store.get_typed(KEY_WORKER_SLOTS).await.unwrap_or_default();
        let slot = slots.get_mut(&worker_id)?;
        if !matches!(slot.status, WorkerStatus::Idle) || slot.workspace_id.is_some() {
            return None;
        }
        slot.status = WorkerStatus::Assigned {
            ticket_id: ticket.id.clone(),
            issue_url: ticket.issue_url.clone(),
        };
        store.set(KEY_WORKER_SLOTS, json!(slots)).await;
        info!(ticket_id = %ticket.id, worker_id, "Bound LORE slot to merged ticket");
        Some(worker_id)
    }

    async fn deliver_lore_step(
        &self,
        store: &SharedStore,
        ticket: &Ticket,
        backend: &LoreBackend<'_>,
        record: &DocsRecord,
        step_key: &str,
        step: &str,
    ) {
        let Some(worker_id) = self.claim_lore_slot(store, ticket).await else {
            return;
        };
        let chat = match Box::pin(
            self.ensure_lore_chat(store, ticket, backend, &worker_id, record, step_key, step),
        )
        .await
        {
            Ok(chat) => chat,
            Err(error) => {
                warn!(ticket_id = %ticket.id, worker_id, %error, "LORE chat not ready; will retry next poll");
                self.lore_not_ready(store, ticket, record, &error).await;
                return;
            }
        };
        let delivery_key = Self::lore_delivery_key(&ticket.id);
        let mut delivery: LoreDelivery = store.get_typed(&delivery_key).await.unwrap_or_default();
        // The workspace and chat are up: any earlier failure streak is over.
        let recovered = delivery.failing_since.take().is_some();
        let now = now_secs();
        let fresh = |delivery: &LoreDelivery| LoreDelivery {
            step: step_key.to_string(),
            ts: now,
            started: now,
            nudges: 0,
            ready_notified: delivery.ready_notified,
            epoch: record.epoch,
            failing_since: None,
            crashes: delivery.crashes,
            seal_alert: delivery.seal_alert,
        };
        match chat {
            LoreChat::Created => {
                delivery = fresh(&delivery);
                info!(ticket_id = %ticket.id, worker_id, step_key, "Spawned LORE chat for merged ticket documentation");
            }
            LoreChat::Existing { chat_id, .. } if delivery.step != step_key => {
                if let Err(error) =
                    Self::send_lore_message(backend, &ticket.id, &chat_id, step_key, step).await
                {
                    warn!(ticket_id = %ticket.id, chat_id, %error, "Could not deliver docs step to LORE; will retry next poll");
                    return;
                }
                delivery = fresh(&delivery);
                info!(ticket_id = %ticket.id, step_key, "Delivered docs step to LORE");
            }
            LoreChat::Existing { chat_id, status } => {
                let started = if delivery.started == 0 {
                    delivery.ts
                } else {
                    delivery.started
                };
                if now.saturating_sub(started) >= LORE_STEP_MAX_SECS {
                    let reason = format!(
                        "LORE ran `{step_key}` for {} hours without submitting",
                        now.saturating_sub(started) / 3600
                    );
                    self.park_lore(store, ticket, record, &reason).await;
                    return;
                }
                let busy = matches!(status, Some(ChatStatus::Running | ChatStatus::Pending));
                if busy || now.saturating_sub(delivery.ts) < LORE_STALL_SECS {
                    if recovered {
                        store.set(&delivery_key, json!(delivery)).await;
                    }
                    return;
                }
                if delivery.nudges >= LORE_MAX_NUDGES {
                    let reason = format!(
                        "LORE made no docs progress on `{step_key}` after {LORE_MAX_NUDGES} nudges"
                    );
                    self.park_lore(store, ticket, record, &reason).await;
                    return;
                }
                let nudge = format!(
                    "No documentation progress has been recorded for {} minutes. Run `openflows-harness docs status` and continue with your current instruction:\n\n{step}",
                    now.saturating_sub(delivery.ts) / 60
                );
                let key = format!("{step_key}:nudge{}", delivery.nudges + 1);
                if let Err(error) =
                    Self::send_lore_message(backend, &ticket.id, &chat_id, &key, &nudge).await
                {
                    warn!(ticket_id = %ticket.id, chat_id, %error, "Could not nudge stalled LORE; will retry next poll");
                    return;
                }
                delivery.nudges += 1;
                delivery.ts = now;
                warn!(ticket_id = %ticket.id, step_key, nudges = delivery.nudges, "Nudged stalled LORE");
            }
        }
        store.set(&delivery_key, json!(delivery)).await;
    }

    /// LORE's workspace or chat failed to come up. Retried every poll, but after
    /// [`LORE_PROVISION_MAX_SECS`] of failures LORE is parked for the operator.
    async fn lore_not_ready(
        &self,
        store: &SharedStore,
        ticket: &Ticket,
        record: &DocsRecord,
        error: &anyhow::Error,
    ) {
        let key = Self::lore_delivery_key(&ticket.id);
        let mut delivery: LoreDelivery = store.get_typed(&key).await.unwrap_or_default();
        let now = now_secs();
        let since = *delivery.failing_since.get_or_insert(now);
        delivery.epoch = record.epoch;
        let failing = now.saturating_sub(since);
        if failing >= LORE_PROVISION_MAX_SECS {
            let reason = format!(
                "LORE's workspace or chat has not come up for {} minutes (last error: {error:#})",
                failing / 60
            );
            self.park_lore(store, ticket, record, &reason).await;
            return;
        }
        store.set(&key, json!(delivery)).await;
    }

    /// Stop a stuck LORE: pause the record (so the operator decides), free
    /// the slot and workspace, and tell the operator how to continue.
    async fn park_lore(
        &self,
        store: &SharedStore,
        ticket: &Ticket,
        record: &DocsRecord,
        why: &str,
    ) {
        let Some(_) = Self::docs_event(
            store,
            &ticket.id,
            record,
            DocsEvent::Pause {
                reason: why.to_string(),
            },
        )
        .await
        else {
            return;
        };
        let reason = format!(
            "{why}. LORE was parked and its workspace released. Resume on a fresh workspace with `openflows gate docs --tenant {tenant} --ticket {id} --resume`, or `--skip <reason>` if nothing was submitted.",
            tenant = store.tenant(),
            id = ticket.id,
        );
        self.notify_awaiting_human(store, &ticket.id, None, &reason, ticket.issue_url.clone())
            .await;
        self.release_lore_slot(store, ticket, "LORE parked").await;
        warn!(ticket_id = %ticket.id, why, "Parked stalled LORE for the operator");
    }

    async fn send_lore_message(
        backend: &LoreBackend<'_>,
        ticket_id: &str,
        chat_id: &str,
        message_key: &str,
        text: &str,
    ) -> Result<()> {
        match backend {
            LoreBackend::Coder(client) => client
                .send_chat_message(
                    chat_id,
                    vec![coder_client::types::ChatInputPart::text(text)],
                )
                .await
                .map(|_| ()),
            LoreBackend::Managed(runtime) => {
                runtime
                    .send_chat_message(chat_id, &format!("lore:{ticket_id}:{message_key}"), text)
                    .await
            }
        }
    }

    /// A LORE chat for this ticket already living in `workspace_id`, found by
    /// its labels. Recovers a chat whose id was never stored (crash or lost
    /// response after creation) instead of creating a second LORE.
    async fn adopt_lore_chat(
        client: &CoderClient,
        ticket_id: &str,
        workspace_id: &str,
    ) -> Result<Option<coder_client::types::Chat>> {
        let label = |chat: &coder_client::types::Chat, key: &str| {
            chat.labels
                .get(key)
                .and_then(Value::as_str)
                .map(str::to_string)
        };
        Ok(client.list_chats().await?.into_iter().find(|chat| {
            chat.workspace_id == workspace_id
                && label(chat, CHAT_LABEL_ROLE).as_deref() == Some("lore")
                && label(chat, CHAT_LABEL_TICKET).as_deref() == Some(ticket_id)
        }))
    }

    /// Reuse the live chat in the slot's workspace (stored, or adopted by its
    /// labels), or provision the workspace and create a chat whose first
    /// message is this step. On the local Coder path the chat id is stored
    /// right after creation, and a chat whose id was lost is adopted by its
    /// labels on the next poll, so a crash cannot leave two LORE chats.
    /// Manager makes chat creation idempotent per workspace and ticket.
    #[allow(clippy::too_many_arguments)]
    async fn ensure_lore_chat(
        &self,
        store: &SharedStore,
        ticket: &Ticket,
        backend: &LoreBackend<'_>,
        worker_id: &str,
        record: &DocsRecord,
        step_key: &str,
        step: &str,
    ) -> Result<LoreChat> {
        let chat_key = Self::lore_chat_key(&ticket.id);
        let slots: HashMap<String, WorkerSlot> =
            store.get_typed(KEY_WORKER_SLOTS).await.unwrap_or_default();
        let workspace = slots.get(worker_id).and_then(|s| s.workspace_id.clone());
        if let Some(chat_id) = store.get_typed::<String>(&chat_key).await {
            match backend {
                LoreBackend::Coder(client) => match client.get_chat_opt(&chat_id).await? {
                    Some(chat) if Some(chat.workspace_id.as_str()) == workspace.as_deref() => {
                        return Ok(LoreChat::Existing {
                            status: Some(chat.status()),
                            chat_id,
                        });
                    }
                    _ => {
                        info!(ticket_id = %ticket.id, chat_id, "Stored LORE chat is gone or bound to another workspace — clearing before respawn");
                        store.del(&chat_key).await;
                    }
                },
                // Manager owns the chat and binds it to the slot's workspace.
                LoreBackend::Managed(_) if workspace.is_some() => {
                    return Ok(LoreChat::Existing {
                        chat_id,
                        status: None,
                    });
                }
                LoreBackend::Managed(_) => store.del(&chat_key).await,
            }
        }

        // The existing-workspace path re-verifies readiness (Coder) or the
        // Manager heartbeat (hosted) before the workspace is reused.
        let workspace_id = Box::pin(self.provision_coder_workspace(store, worker_id, &ticket.id))
            .await?
            .context(
                "LORE workspace provisioning made no progress (no Coder client or missing slot)",
            )?;

        if let LoreBackend::Coder(client) = backend {
            if let Some(chat) = Self::adopt_lore_chat(client, &ticket.id, &workspace_id).await? {
                info!(ticket_id = %ticket.id, chat_id = %chat.id, "Adopted existing LORE chat");
                store.set(&chat_key, json!(chat.id)).await;
                return Ok(LoreChat::Existing {
                    status: Some(chat.status()),
                    chat_id: chat.id,
                });
            }
        }

        let deployment: Option<Value> = store
            .get_typed(&full_ticket_key_flat(&ticket.id, KEY_TICKET_DEPLOYMENT))
            .await;
        let merged = deployment
            .as_ref()
            .map(|d| {
                format!(
                    "\n\nMerged PR #{} at {}.",
                    d["pr_number"].as_u64().unwrap_or_default(),
                    d["sha"].as_str().unwrap_or_default()
                )
            })
            .unwrap_or_default();
        store
            .set(
                &full_ticket_key(&ticket.id, KEY_TICKET_DISPATCH, "lore"),
                json!({
                    "ticket_id": ticket.id,
                    "title": ticket.title,
                    "body": format!("{}{merged}", ticket.body),
                    "branch": record.branch.clone().unwrap_or_else(|| record.expected_branch(&ticket.id)),
                }),
            )
            .await;

        let prompt = if step_key.starts_with("start:") {
            step.to_string()
        } else {
            format!(
                "You are LORE documenting merged ticket {}: {}. `openflows-harness docs status` is the authority for what to do next.\n\n{step}",
                ticket.id, ticket.title
            )
        };
        let chat_id = match backend {
            LoreBackend::Coder(client) => {
                let organization_id = client.get_default_organization_id().await.context(
                    "Failed to resolve organization_id for LORE chat; ensure Coder has a default organization",
                )?;
                self.ensure_workspace_instructions(store, client, &workspace_id, worker_id)
                    .await
                    .context("LORE instructions unavailable; deferring chat")?;
                let mut labels = serde_json::Map::new();
                labels.insert(CHAT_LABEL_FLOW.to_string(), json!("openflows"));
                labels.insert(CHAT_LABEL_ROLE.to_string(), json!("lore"));
                labels.insert(CHAT_LABEL_TICKET.to_string(), json!(ticket.id));
                client
                    .create_chat(&coder_client::types::CreateChatRequest {
                        organization_id: Some(organization_id),
                        owner_id: None,
                        workspace_id: workspace_id.clone(),
                        model_config_id: None,
                        content: vec![coder_client::types::ChatInputPart::text(&prompt)],
                        labels: Some(labels),
                    })
                    .await?
                    .id
                    .to_string()
            }
            LoreBackend::Managed(runtime) => {
                let chat = runtime
                    .create_chat(&workspace_id, &ticket.id, &prompt)
                    .await?;
                if !chat.created {
                    // An existing Manager chat continues with this step.
                    runtime
                        .send_chat_message(
                            &chat.chat_id,
                            &format!("lore:{}:{step_key}", ticket.id),
                            &prompt,
                        )
                        .await?;
                }
                chat.chat_id
            }
        };
        store.set(&chat_key, json!(chat_id)).await;
        Ok(LoreChat::Created)
    }

    /// Where the operator reviews exactly the submitted head: its diff against
    /// the default branch as GitHub reports it. Never built from the base LORE
    /// reported, which could hide changes the docs PR would merge.
    async fn docs_compare_link(&self, store: &SharedStore, record: &DocsRecord) -> Option<String> {
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
    async fn notify_lore_docs_ready(
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

    /// The approved head of `record` if its seal verifies. Otherwise the
    /// operator is told, once per record version, why nothing will be opened
    /// or merged and how to approve again.
    async fn verified_docs_head(
        &self,
        store: &SharedStore,
        ticket: &Ticket,
        record: &DocsRecord,
    ) -> Option<String> {
        let Some(keys) = self.docs_gate_keys.as_ref() else {
            warn!(ticket_id = %ticket.id, "No docs gate secret (OPENFLOWS_DOCS_GATE_SECRET or CODER_CHAT_HOOK_SECRET) is configured; NEXUS cannot verify docs approvals and will not open docs PRs");
            return None;
        };
        let error = match keys.verify(record, &ticket.id) {
            Ok(head) => return Some(head.to_string()),
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
    async fn refuse_docs_publication(
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
    async fn publish_docs_pr(&self, store: &SharedStore, ticket: &Ticket, record: &DocsRecord) {
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
    async fn reconcile_docs_pr(&self, store: &SharedStore, ticket: &Ticket, record: &DocsRecord) {
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
    async fn release_lore_slot(&self, store: &SharedStore, ticket: &Ticket, why: &str) -> bool {
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

    /// A crashed LORE workspace that needs replacing: release the slot (which
    /// destroys the workspace) and forget the chat, so the docs driver binds,
    /// provisions and re-delivers on its next poll. The ticket is untouched.
    pub(crate) async fn recover_lore_workspace(
        &self,
        store: &SharedStore,
        worker_id: &str,
        ticket_id: &str,
    ) {
        warn!(
            worker_id,
            ticket_id, "LORE workspace crashed; releasing it for the docs driver to re-provision"
        );
        self.release_worker_slot(store, worker_id).await;
        store.del(&Self::lore_chat_key(ticket_id)).await;
        // Count the crash against this epoch; the driver parks LORE at the limit.
        let key = Self::lore_delivery_key(ticket_id);
        let mut delivery: LoreDelivery = store.get_typed(&key).await.unwrap_or_default();
        if let Ok(record) = store.docs_record(ticket_id).await {
            if delivery.epoch != record.epoch {
                delivery = LoreDelivery {
                    epoch: record.epoch,
                    ready_notified: delivery.ready_notified,
                    ..Default::default()
                };
            }
        }
        delivery.crashes += 1;
        store.set(&key, json!(delivery)).await;
    }

    /// Closed docs: release LORE and drop delivery state. Returns whether
    /// nothing is left to clean up.
    async fn finish_lore_docs(&self, store: &SharedStore, ticket: &Ticket) -> bool {
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

/// Marker the removed inline LORE wrote for documented tickets
/// (`ticket:{id}:lore_done`). Read only by the one-time cutover; remove with it.
const LEGACY_LORE_DONE_KEY: &str = "lore_done";

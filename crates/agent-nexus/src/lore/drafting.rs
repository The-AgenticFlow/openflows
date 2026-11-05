//! LORE drafting, workspace and chat handling. See the module doc in
//! [`super`] for the overall docs lifecycle.

use super::*;
use config::docs_lifecycle::{DocsEvent, DocsPhase, DocsRecord, DOCS_TICKET};
use config::state::{
    KEY_DOCS_CUTOVER, KEY_DOCS_REOPENED, KEY_TICKET_CHAT, KEY_TICKET_DEPLOYMENT,
    KEY_TICKET_DISPATCH, KEY_TICKET_DOCS_DELIVERY,
};

impl NexusNode {
    pub(super) fn lore_delivery_key(ticket_id: &str) -> String {
        full_ticket_key_flat(ticket_id, KEY_TICKET_DOCS_DELIVERY)
    }

    pub(super) fn lore_chat_key(ticket_id: &str) -> String {
        full_ticket_key(ticket_id, KEY_TICKET_CHAT, "lore")
    }

    pub(super) fn is_documentable(ticket: &Ticket) -> bool {
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

    pub(super) fn docs_known_closed(&self, ticket_id: &str) -> bool {
        self.docs_closed.lock().unwrap().tickets.contains(ticket_id)
    }

    pub(super) fn remember_docs_closed(&self, ticket_id: &str) {
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

    pub(super) async fn lore_backend(&self, store: &SharedStore) -> Option<LoreBackend<'_>> {
        match &self.runtime_control {
            Some(runtime) => Some(LoreBackend::Managed(runtime)),
            None => Self::coder_client_from_store(store)
                .await
                .map(LoreBackend::Coder),
        }
    }

    /// The tenant repository as `(owner, name)`. `prep` already copies
    /// `GITHUB_REPOSITORY` into the store, so the store is the single source
    /// (as in VESSEL); reading the env here would point tenants and tests at
    /// whatever repository the host process happens to run in.
    pub(super) async fn docs_repository(store: &SharedStore) -> Option<(String, String)> {
        let repository = store.get_typed::<String>("repository").await?;
        let (owner, name) = repository.trim().split_once('/')?;
        (!owner.is_empty() && !name.is_empty() && !name.contains('/'))
            .then(|| (owner.to_string(), name.to_string()))
    }

    /// GitHub client and repository for the controller's docs PR work.
    pub(super) async fn docs_github(
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

    pub(super) async fn docs_event(
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
            // A pause stops LORE and automatic merges, not the facts: a human
            // may still merge or close the docs PR, and the record follows it.
            if record.phase == DocsPhase::Published {
                Box::pin(self.reconcile_docs_pr(store, ticket, &record)).await;
            }
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

    pub(super) async fn drive_lore_drafting(
        &self,
        store: &SharedStore,
        ticket: &Ticket,
        record: &DocsRecord,
    ) {
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

    pub(super) fn lore_start_prompt(ticket: &Ticket, branch: &str) -> String {
        format!(
            "Document merged ticket {id}: {title}. Read `openflows-harness dispatch read` and `openflows-harness docs status`. Follow the lore-changelog and lore-documentation skills: update the changelog, and add an ADR under docs/ only for architectural decisions. Change only documentation (Markdown/reST/AsciiDoc, changelogs, text and images under docs/; never dot-directories such as .github/, orchestration/, agent instructions, legal files, MDX, or build inputs such as requirements.txt); `docs submit` refuses anything else and the controller re-checks it on GitHub. {branch_rule} Commit, push the branch to origin, write a short summary to /tmp/docs-report.md (outside the repository), and run `openflows-harness docs submit --report /tmp/docs-report.md`. Then stop: the operator approves or rejects the submitted head, and after approval the controller opens and merges the docs PR. Never open a PR yourself.",
            id = ticket.id,
            title = ticket.title,
            branch_rule = Self::lore_branch_rule(branch),
        )
    }

    pub(super) fn lore_branch_rule(branch: &str) -> String {
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
    pub(super) async fn claim_lore_slot(
        &self,
        store: &SharedStore,
        ticket: &Ticket,
    ) -> Option<String> {
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

    pub(super) async fn deliver_lore_step(
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
                    self.lore_delivery_failed(store, ticket, backend, record, &error)
                        .await;
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
                    self.lore_delivery_failed(store, ticket, backend, record, &error)
                        .await;
                    return;
                }
                delivery.nudges += 1;
                delivery.ts = now;
                warn!(ticket_id = %ticket.id, step_key, nudges = delivery.nudges, "Nudged stalled LORE");
            }
        }
        store.set(&delivery_key, json!(delivery)).await;
    }

    /// A message could not reach LORE's chat. The hosted path cannot see
    /// whether the stored chat still exists, so it forgets it: the next poll
    /// asks the Manager again (idempotent per workspace and ticket), which
    /// reuses a live chat or creates one. Either way the failure counts toward
    /// [`LORE_PROVISION_MAX_SECS`], so a chat that never comes back parks LORE
    /// instead of holding the slot forever.
    pub(super) async fn lore_delivery_failed(
        &self,
        store: &SharedStore,
        ticket: &Ticket,
        backend: &LoreBackend<'_>,
        record: &DocsRecord,
        error: &anyhow::Error,
    ) {
        if matches!(backend, LoreBackend::Managed(_)) {
            store.del(&Self::lore_chat_key(&ticket.id)).await;
        }
        self.lore_not_ready(store, ticket, record, error).await;
    }

    /// LORE's workspace or chat failed to come up. Retried every poll, but after
    /// [`LORE_PROVISION_MAX_SECS`] of failures LORE is parked for the operator.
    pub(super) async fn lore_not_ready(
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
    pub(super) async fn park_lore(
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

    pub(super) async fn send_lore_message(
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
    pub(super) async fn adopt_lore_chat(
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
    pub(super) async fn ensure_lore_chat(
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
}

//! The LORE documentation gate on [`SharedStore`]: the versioned docs record
//! (`ns:{tenant}:ticket:{id}:docs`) and the operator's latest decision under
//! the controller-only prefix (`ctl:{tenant}:ticket:{id}:docs_control`).

use super::*;

impl SharedStore {
    /// Strict read of the post-merge documentation gate for a ticket.
    pub async fn docs_record(&self, ticket: &str) -> Result<config::docs_lifecycle::DocsRecord> {
        let key = self.ns_key(&config::state::full_ticket_key_flat(
            ticket,
            config::state::KEY_TICKET_DOCS,
        ));
        config::docs_lifecycle::DocsRecord::decode(self.read_record(&key, "docs GET").await?)
    }

    /// Compare-and-set the documentation gate, mirroring [`Self::transition`].
    pub async fn docs_transition(
        &self,
        ticket: &str,
        version: u64,
        actor: &str,
        event: config::docs_lifecycle::DocsEvent,
    ) -> Result<config::docs_lifecycle::DocsRecord> {
        self.docs_commit(ticket, version, actor, event, false).await
    }

    /// An operator action: commit the docs record and the operator's latest
    /// decision ([`config::docs_lifecycle::DocsControl`], derived from the new
    /// record) in one atomic step under the same version check. A command
    /// that loses the check writes neither, so it can never replace a newer
    /// decision, and no crash can leave the two out of step.
    pub async fn docs_operator_transition(
        &self,
        ticket: &str,
        version: u64,
        event: config::docs_lifecycle::DocsEvent,
    ) -> Result<config::docs_lifecycle::DocsRecord> {
        self.docs_commit(ticket, version, "human", event, true)
            .await
    }

    async fn docs_commit(
        &self,
        ticket: &str,
        version: u64,
        actor: &str,
        event: config::docs_lifecycle::DocsEvent,
        record_control: bool,
    ) -> Result<config::docs_lifecycle::DocsRecord> {
        use config::docs_lifecycle::{DocsControl, DocsRecord};
        let control_key = self.docs_control_key(ticket);
        let key = self.ns_key(&config::state::full_ticket_key_flat(
            ticket,
            config::state::KEY_TICKET_DOCS,
        ));
        // Kept for the control record: only a decision may grant approval.
        let operator_event = event.clone();
        let mut was_closed = false;
        let next = self
            .compare_and_set(
                &key,
                "docs transition",
                "Docs record changed; refresh before retrying",
                |raw| {
                    let current = DocsRecord::decode(raw)?;
                    anyhow::ensure!(
                        current.version == version,
                        "Docs record changed; refresh before retrying"
                    );
                    Ok(current)
                },
                |current, ts| {
                    was_closed = current.phase.is_terminal();
                    current.apply(actor, event, ts)
                },
                record_control.then(|| {
                    let derive: DeriveCompanion<'_, DocsRecord> =
                        Box::new(move |next, previous| {
                            let previous: Option<DocsControl> = previous
                                .map(serde_json::from_value)
                                .transpose()
                                .context("Malformed operator docs decision")?;
                            Ok(serde_json::to_value(DocsControl::after_operator(
                                previous.as_ref(),
                                &operator_event,
                                next,
                            ))?)
                        });
                    (control_key, derive)
                }),
            )
            .await?;
        if was_closed && !next.phase.is_terminal() {
            // Readers that cache closed docs records (NEXUS) watch this marker.
            self.set(
                config::state::KEY_DOCS_REOPENED,
                serde_json::json!({ "ticket": ticket, "version": next.version, "ts": next.ts }),
            )
            .await;
        }
        Ok(next)
    }

    /// Build a controller-only key: `ctl:{tenant}:{key}`. Hosted tenants give
    /// workspaces a Redis user limited to `ns:{tenant}:*`, so only the
    /// controller (NEXUS, VESSEL, the operator CLI) can write these.
    fn ctl_key(&self, key: &str) -> String {
        format!("ctl:{}:{}", self.tenant, key)
    }

    fn docs_control_key(&self, ticket: &str) -> String {
        self.ctl_key(&config::state::full_ticket_key_flat(
            ticket,
            config::state::KEY_TICKET_DOCS_CONTROL,
        ))
    }

    /// Strict read of the operator's latest docs decision (controller-only).
    pub async fn docs_control(
        &self,
        ticket: &str,
    ) -> Result<Option<config::docs_lifecycle::DocsControl>> {
        let key = self.docs_control_key(ticket);
        self.read_record(&key, "docs control GET")
            .await?
            .map(serde_json::from_value)
            .transpose()
            .context("Malformed operator docs decision")
    }

    /// Record the operator's latest docs decision. Fails (never silently) when
    /// the credential cannot write the controller-only prefix.
    pub async fn set_docs_control(
        &self,
        ticket: &str,
        control: &config::docs_lifecycle::DocsControl,
    ) -> Result<()> {
        let key = self.docs_control_key(ticket);
        let value = serde_json::to_value(control)?;
        match &self.backend {
            Backend::InMemory(b) => {
                b.map.write().await.insert(key, value);
            }
            Backend::Redis(b) => {
                use fred::prelude::*;
                bounded_redis(
                    "docs control SET",
                    b.client
                        .set::<(), _, _>(key, value.to_string(), None, None, false),
                )
                .await
                .context("Cannot record the operator docs decision under the controller-only prefix; run `openflows gate docs` with the controller's Redis credential")?;
            }
        }
        Ok(())
    }

    /// The approved head of `record` for `ticket`, only if its seal verifies
    /// **and** it is the operator's latest decision (see
    /// [`config::docs_lifecycle::DocsControl`]). NEXUS and VESSEL call this
    /// before opening or merging a docs PR.
    pub async fn verified_docs_head(
        &self,
        ticket: &str,
        record: &config::docs_lifecycle::DocsRecord,
        keys: &config::docs_lifecycle::DocsGateKeys,
    ) -> Result<String> {
        let head = keys.verify(record, ticket)?.to_string();
        let control = self.docs_control(ticket).await?;
        config::docs_lifecycle::DocsControl::confirm(control.as_ref(), record, ticket)?;
        Ok(head)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use config::docs_lifecycle::{DocsEvent, DocsPhase};

    /// A rejection commits; an approval that read the same version then loses
    /// the version check. The losing command must leave the operator's
    /// latest decision (the rejection) in place.
    async fn stale_operator_commands_never_replace_a_newer_decision(store: SharedStore) {
        let ready = store
            .docs_transition(
                "T-1",
                0,
                "lore",
                DocsEvent::Submit {
                    branch: "lore/T-1".into(),
                    head: "abc".into(),
                    base: "main".into(),
                    report: "changelog".into(),
                    files: vec!["CHANGELOG.md".into()],
                },
            )
            .await
            .unwrap();
        let decide = |approved: bool| DocsEvent::Decide {
            revision: ready.revision,
            head: "abc".into(),
            approved,
            report: "operator notes".into(),
            seal: "seal".into(),
        };
        let rejected = store
            .docs_operator_transition("T-1", ready.version, decide(false))
            .await
            .unwrap();
        assert_eq!(rejected.phase, DocsPhase::Drafting);
        assert!(store
            .docs_operator_transition("T-1", ready.version, decide(true))
            .await
            .is_err());
        let control = store.docs_control("T-1").await.unwrap().unwrap();
        assert!(control.approved.is_none(), "{control:?}");
        assert_eq!(control.version, rejected.version);
    }

    /// After a rejection, a workspace restores the older sealed approval in the
    /// workspace-writable record. Pausing and resuming must not promote it:
    /// only an explicit decision grants an approval. An approval the operator
    /// did grant survives a pause and resume.
    async fn only_a_decision_grants_the_protected_approval(store: SharedStore) {
        let ticket = "T-2";
        let ready = store
            .docs_transition(
                ticket,
                0,
                "lore",
                DocsEvent::Submit {
                    branch: "lore/T-2".into(),
                    head: "abc".into(),
                    base: "main".into(),
                    report: "changelog".into(),
                    files: vec!["CHANGELOG.md".into()],
                },
            )
            .await
            .unwrap();
        let decide =
            |version_rec: &config::docs_lifecycle::DocsRecord, approved: bool| DocsEvent::Decide {
                revision: version_rec.revision,
                head: "abc".into(),
                approved,
                report: "operator notes".into(),
                seal: "seal".into(),
            };
        let pause_resume = |store: SharedStore, version: u64| async move {
            let paused = store
                .docs_operator_transition(
                    ticket,
                    version,
                    DocsEvent::Pause {
                        reason: "hold".into(),
                    },
                )
                .await
                .unwrap();
            store
                .docs_operator_transition(ticket, paused.version, DocsEvent::Resume)
                .await
                .unwrap()
        };
        let approved = store
            .docs_operator_transition(ticket, ready.version, decide(&ready, true))
            .await
            .unwrap();

        // An approval the operator granted is kept across pause and resume.
        let resumed = pause_resume(store.clone(), approved.version).await;
        let control = store.docs_control(ticket).await.unwrap().unwrap();
        assert_eq!(
            control.approved,
            config::docs_lifecycle::DocsControl::from_record(&approved).approved
        );
        assert!(!control.paused);

        let rejected = store
            .docs_operator_transition(ticket, resumed.version, decide(&resumed, false))
            .await
            .unwrap();
        assert_eq!(rejected.phase, DocsPhase::Drafting);

        // A workspace restores the sealed approval over the rejection.
        let docs_key = config::state::full_ticket_key_flat(ticket, config::state::KEY_TICKET_DOCS);
        store
            .set(&docs_key, serde_json::to_value(&approved).unwrap())
            .await;
        let restored = store.docs_record(ticket).await.unwrap();
        assert_eq!(restored.phase, DocsPhase::Approved);

        let resumed = pause_resume(store.clone(), restored.version).await;
        let control = store.docs_control(ticket).await.unwrap().unwrap();
        assert!(control.approved.is_none(), "{control:?}");
        let error = config::docs_lifecycle::DocsControl::confirm(Some(&control), &resumed, ticket)
            .unwrap_err();
        assert!(
            format!("{error:#}").contains("not the operator's latest decision"),
            "{error:#}"
        );
    }

    #[tokio::test]
    async fn only_a_decision_grants_the_protected_approval_in_memory() {
        only_a_decision_grants_the_protected_approval(SharedStore::new_in_memory()).await;
    }

    #[tokio::test]
    async fn stale_operator_commands_never_replace_a_newer_decision_in_memory() {
        stale_operator_commands_never_replace_a_newer_decision(SharedStore::new_in_memory()).await;
    }

    /// Same, through the Redis script. Needs `OPENFLOWS_TEST_REDIS_URL`.
    #[tokio::test]
    async fn stale_operator_commands_never_replace_a_newer_decision_in_redis() {
        let Ok(url) = std::env::var("OPENFLOWS_TEST_REDIS_URL") else {
            eprintln!("skipping: OPENFLOWS_TEST_REDIS_URL unset");
            return;
        };
        let tenant = format!("docs-race-{}", std::process::id());
        let store = SharedStore::new_redis_with_tenant(&url, Some(tenant.clone()))
            .await
            .unwrap();
        stale_operator_commands_never_replace_a_newer_decision(store.clone()).await;
        only_a_decision_grants_the_protected_approval(store.clone()).await;
        for pattern in [format!("ns:{tenant}:*"), format!("ctl:{tenant}:*")] {
            for key in store.raw_keys(&pattern).await {
                store.raw_del(&key).await;
            }
        }
    }
}

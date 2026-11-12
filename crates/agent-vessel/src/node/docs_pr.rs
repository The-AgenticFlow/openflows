//! LORE docs PRs: VESSEL merges them at the operator's docs gate, not
//! through SENTINEL review. See [`VesselNode::process_docs_pr`].

use super::*;

/// What VESSEL may do with a LORE docs PR ([`VesselNode::docs_route`]).
enum DocsRoute {
    /// No open docs record owns the PR: a human handles it.
    Unmanaged(String),
    /// The record owns the PR but it is not mergeable yet (drafting, awaiting
    /// the operator, paused, or the approval cannot be verified).
    Hold(String),
    /// Published, unpaused and sealed: may merge at `approved_head`.
    Ready {
        ticket: String,
        approved_head: String,
    },
}

impl VesselNode {
    pub(super) fn is_docs_pr(pr_info: &PrInfo) -> bool {
        pr_info.head_branch.starts_with(DOCS_BRANCH_PREFIX)
            || pr_info.ticket_id.as_deref() == Some(DOCS_TICKET)
    }

    /// Decide what VESSEL may do with a LORE docs PR from its docs record.
    async fn docs_route(&self, store: &SharedStore, pr_info: &PrInfo) -> Result<DocsRoute> {
        let pr_number = pr_info.number;
        let Some(ticket) = ticket_from_docs_branch(&pr_info.head_branch) else {
            return Ok(DocsRoute::Unmanaged(format!(
                "Docs PR branch {} names no ticket; manual review and merge required",
                pr_info.head_branch
            )));
        };
        let record = store.docs_record(ticket).await?;
        if !record.owns_pr(&pr_info.head_branch, pr_number) {
            return Ok(DocsRoute::Unmanaged(format!(
                "No open docs record for {ticket} owns PR #{pr_number} ({}); manual review and merge required",
                pr_info.head_branch
            )));
        }
        if !record.merge_candidate(pr_number) {
            return Ok(DocsRoute::Hold(format!(
                "docs for {ticket} are {}{}; waiting until they are published and unpaused",
                record.phase.as_str(),
                if record.paused { " (paused)" } else { "" }
            )));
        }
        let Some(keys) = self.config.docs_gate_keys.as_ref() else {
            warn!(
                pr_number,
                ticket,
                "No docs gate secret (OPENFLOWS_DOCS_GATE_SECRET or CODER_CHAT_HOOK_SECRET) is configured; VESSEL cannot verify docs approvals and will not merge docs PRs"
            );
            return Ok(DocsRoute::Hold("no docs gate secret is configured".into()));
        };
        match store.verified_docs_head(ticket, &record, keys).await {
            Ok(head) => Ok(DocsRoute::Ready {
                ticket: ticket.to_string(),
                approved_head: head,
            }),
            Err(error) => {
                tracing::error!(pr_number, ticket, %error, "Refusing to merge docs PR without a valid operator approval");
                Ok(DocsRoute::Hold(format!("{error:#}")))
            }
        }
    }

    /// LORE docs PRs are reviewed at the operator's docs gate, not by
    /// SENTINEL. They merge only while published, unpaused, sealed by the
    /// operator, exactly at the approved head, into the default branch, and
    /// only when GitHub shows they change documentation only; any other
    /// outcome goes back to the docs record (rework) or to a human, never to
    /// FORGE. An error (store or GitHub) holds just this PR: it never aborts
    /// VESSEL's pass over the other pending PRs.
    pub(super) async fn process_docs_pr(
        &self,
        owner: &str,
        repo: &str,
        pr_info: PrInfo,
    ) -> VesselOutcome {
        let pr_number = pr_info.number;
        match Box::pin(self.route_docs_pr(owner, repo, pr_info)).await {
            Ok(outcome) => outcome,
            Err(error) => {
                let reason = format!("{error:#}");
                warn!(
                    pr_number,
                    reason, "Cannot process docs PR; holding it for the next pass"
                );
                VesselOutcome::DocsPrHeld { pr_number, reason }
            }
        }
    }

    async fn route_docs_pr(
        &self,
        owner: &str,
        repo: &str,
        pr_info: PrInfo,
    ) -> Result<VesselOutcome> {
        let pr_number = pr_info.number;
        let store = self
            .lifecycle_store
            .lock()
            .unwrap()
            .clone()
            .context("Missing lifecycle store")?;
        let hold = |reason: String| VesselOutcome::DocsPrHeld { pr_number, reason };
        let ticket = match Box::pin(self.docs_merge_gate(owner, repo, &store, &pr_info)).await? {
            Ok(ticket) => ticket,
            Err(outcome) => return Ok(outcome),
        };
        let rework = |reason: String| VesselOutcome::DocsPrRework {
            pr_number,
            reason,
            docs_ticket: ticket.clone(),
        };
        match Box::pin(self.poller.poll_until_terminal(owner, repo, &pr_info)).await? {
            CiPollResult::Status(CiStatus::Success) => {}
            CiPollResult::Conflicts => return Ok(rework(Self::docs_conflict_reason(&pr_info))),
            CiPollResult::Timeout => {
                info!(pr_number, ticket, "Docs PR CI still running; holding");
                return Ok(hold("CI is still running".into()));
            }
            CiPollResult::Status(status) => {
                let checks = self
                    .client
                    .get_failed_checks_detail_structured(owner, repo, &pr_info.head_sha)
                    .await
                    .map(|detail| {
                        detail
                            .failed_checks
                            .iter()
                            .map(|check| check.name.clone())
                            .collect::<Vec<_>>()
                            .join(", ")
                    })
                    .unwrap_or_default();
                return Ok(rework(format!(
                    "CI on docs PR #{pr_number} ended {status:?}{}. Fix the documentation so CI passes (documentation only), push, and submit again.",
                    if checks.is_empty() { String::new() } else { format!(" (failed checks: {checks})") }
                )));
            }
        }
        // CI can take minutes. Meanwhile the operator may have paused or
        // rejected the docs, and anyone with access may have retargeted the
        // PR (GitHub's merge call pins the head, not the base). So read the
        // PR and the docs gate again and repeat every check right before merging.
        let checked_head = pr_info.head_sha.clone();
        let pr_info = self.client.get_pull_request(owner, repo, pr_number).await?;
        // CI passed for `checked_head` only. A different head (even a newly
        // approved one) has not passed CI: hold, and poll its CI next pass.
        if pr_info.head_sha != checked_head {
            info!(pr_number, checked_head, head = %pr_info.head_sha, "Docs PR head changed while CI ran; holding");
            return Ok(hold(format!(
                "head moved from {checked_head} to {} while CI ran; CI is checked again for the new head",
                pr_info.head_sha
            )));
        }
        let ticket = match Box::pin(self.docs_merge_gate(owner, repo, &store, &pr_info)).await? {
            Ok(ticket) => ticket,
            Err(outcome) => return Ok(outcome),
        };
        let rework = |reason: String| VesselOutcome::DocsPrRework {
            pr_number,
            reason,
            docs_ticket: ticket.clone(),
        };
        // `merge` pins the PR head, so GitHub refuses if it moved since.
        match Box::pin(self.merger.merge(owner, repo, &pr_info)).await {
            Ok(result) if result.merged => Ok(VesselOutcome::DocsPrMerged {
                pr_number,
                sha: result.sha.unwrap_or_default(),
                docs_ticket: ticket.clone(),
            }),
            Ok(result) if is_merge_conflict_message(&result.message) => {
                Ok(rework(Self::docs_conflict_reason(&pr_info)))
            }
            Err(error) if is_merge_conflict_message(&error.to_string()) => {
                Ok(rework(Self::docs_conflict_reason(&pr_info)))
            }
            Ok(result) => {
                warn!(pr_number, message = %result.message, "Docs PR merge did not complete; holding");
                Ok(hold(format!("merge did not complete: {}", result.message)))
            }
            Err(error) => {
                warn!(pr_number, %error, "Docs PR merge failed; holding");
                Ok(hold(format!("merge failed: {error:#}")))
            }
        }
    }

    fn docs_conflict_reason(pr_info: &PrInfo) -> String {
        format!(
            "Docs PR #{} conflicts with {base}. On branch {branch}, merge origin/{base} and resolve the conflicts keeping documentation only, push, and submit again.",
            pr_info.number,
            base = pr_info.base_branch,
            branch = pr_info.head_branch,
        )
    }

    /// Every check a docs PR must pass to be merged as `pr_info` shows it: an
    /// operator approval that is still in force (sealed, latest, unpaused),
    /// the approved head, the default branch as base, documentation-only
    /// files on GitHub, and no conflicts. `Ok(ticket)` when it may merge;
    /// otherwise the outcome to report.
    async fn docs_merge_gate(
        &self,
        owner: &str,
        repo: &str,
        store: &SharedStore,
        pr_info: &PrInfo,
    ) -> Result<std::result::Result<String, VesselOutcome>> {
        let pr_number = pr_info.number;
        let (ticket, approved_head) = match Box::pin(self.docs_route(store, pr_info)).await? {
            DocsRoute::Unmanaged(reason) => {
                return Ok(Err(VesselOutcome::Unmanaged { pr_number, reason }))
            }
            DocsRoute::Hold(reason) => {
                return Ok(Err(VesselOutcome::DocsPrHeld { pr_number, reason }))
            }
            DocsRoute::Ready {
                ticket,
                approved_head,
            } => (ticket, approved_head),
        };
        let rework = |reason: String| {
            Ok(Err(VesselOutcome::DocsPrRework {
                pr_number,
                reason,
                docs_ticket: ticket.clone(),
            }))
        };
        if pr_info.head_sha != approved_head {
            return rework(format!(
                "Docs PR #{pr_number} head {} is not the approved head {approved_head}; commits were pushed after approval. Review the branch, keep documentation only, and submit it again.",
                pr_info.head_sha
            ));
        }
        // LORE docs merge into the default branch only, whatever the PR says.
        let default_branch = self.client.get_default_branch(owner, repo).await?;
        if pr_info.base_branch != default_branch {
            return Ok(Err(VesselOutcome::Unmanaged {
                pr_number,
                reason: format!(
                    "Docs PR #{pr_number} targets {}, not the default branch {default_branch}; LORE docs merge only into the default branch. Manual review required.",
                    pr_info.base_branch
                ),
            }));
        }
        // What the PR merges, from GitHub: the file list LORE submitted is not trusted.
        let paths = self
            .client
            .list_pr_file_paths(owner, repo, pr_number)
            .await?;
        let outside = non_docs_paths(paths.iter().map(String::as_str));
        if !outside.is_empty() {
            tracing::error!(pr_number, ticket, files = ?outside, "Docs PR changes files that are not documentation");
            return rework(format!(
                "Docs PR #{pr_number} changes files that are not documentation: {}. Make {} change documentation only, push, and submit again.",
                outside.join(", "),
                pr_info.head_branch
            ));
        }
        let conflicted = pr_info.has_conflicts()
            || (pr_info.mergeable.is_none()
                && self
                    .client
                    .get_pull_request(owner, repo, pr_number)
                    .await?
                    .has_conflicts());
        if conflicted {
            return rework(Self::docs_conflict_reason(pr_info));
        }
        Ok(Ok(ticket))
    }

    /// Record a docs PR outcome on its docs record, only while the record
    /// still publishes that PR (a record that moved on is left alone).
    pub(super) async fn record_docs_outcome(
        store: &SharedStore,
        ticket: &str,
        pr_number: u64,
        event: DocsEvent,
    ) {
        let record = match store.docs_record(ticket).await {
            Ok(record) => record,
            Err(error) => {
                warn!(ticket, pr_number, %error, "Cannot read docs record to record the docs PR outcome");
                return;
            }
        };
        if record.phase != config::docs_lifecycle::DocsPhase::Published
            || record.pr_number != Some(pr_number)
        {
            return;
        }
        match store
            .docs_transition(ticket, record.version, "vessel", event)
            .await
        {
            Ok(next) => info!(
                ticket,
                pr_number,
                phase = next.phase.as_str(),
                "Recorded docs PR outcome"
            ),
            Err(error) => {
                warn!(ticket, pr_number, %error, "Could not record docs PR outcome; NEXUS reconciles it from GitHub")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::VesselConfig;

    const DOCS_SECRET: &str = "controller-secret";

    async fn docs_test_node(server: &mockito::Server, store: &SharedStore) -> VesselNode {
        docs_node_with_secret(server, store, Some(DOCS_SECRET)).await
    }

    async fn docs_node_with_secret(
        server: &mockito::Server,
        store: &SharedStore,
        secret: Option<&str>,
    ) -> VesselNode {
        let client = github::GithubRestClient::with_api_base("test", server.url());
        let config = VesselConfig {
            github_token: "test".to_string(),
            docs_gate_keys: secret.map(config::docs_lifecycle::DocsGateKeys::new),
            ..Default::default()
        };
        VesselNode {
            lifecycle_store: std::sync::Mutex::new(Some(store.clone())),
            poller: CiPoller::new(config.ci_poll.clone(), client.clone()),
            merger: PrMerger::new(client.clone(), config.merge_method),
            client,
            config,
            slots_lock: tokio::sync::Mutex::new(()),
        }
    }

    fn docs_pr(head_sha: &str) -> PrInfo {
        PrInfo {
            number: 42,
            head_sha: head_sha.to_string(),
            head_branch: "lore/T-9".to_string(),
            base_branch: "main".to_string(),
            title: "docs: retries (T-12 follow-up)".to_string(),
            body: None,
            state: pocketflow_core::PrState::Open,
            mergeable: Some(true),
            ticket_id: Some("T-9".to_string()),
        }
    }

    async fn docs_step(store: &SharedStore, actor: &str, event: DocsEvent) {
        let record = store.docs_record("T-9").await.unwrap();
        store
            .docs_transition("T-9", record.version, actor, event)
            .await
            .unwrap();
    }

    /// A docs record for T-9 submitted at `abc` on `lore/T-9`.
    async fn submitted_docs_store() -> SharedStore {
        let store = SharedStore::new_in_memory();
        docs_step(
            &store,
            "lore",
            DocsEvent::Submit {
                branch: "lore/T-9".into(),
                head: "abc".into(),
                base: "main".into(),
                report: "changelog".into(),
                files: vec!["CHANGELOG.md".into()],
            },
        )
        .await;
        store
    }

    /// A docs record published as PR #42 from the operator-approved (sealed) head `abc`.
    async fn published_docs_store_with_seal(seal: &str) -> SharedStore {
        let store = submitted_docs_store().await;
        docs_step(
            &store,
            "human",
            DocsEvent::Decide {
                revision: 1,
                head: "abc".into(),
                approved: true,
                report: String::new(),
                seal: seal.into(),
            },
        )
        .await;
        record_operator_decision(&store).await;
        docs_step(&store, "nexus", DocsEvent::Published { number: 42 }).await;
        store
    }

    /// What the operator CLI records under the controller-only prefix.
    async fn record_operator_decision(store: &SharedStore) {
        let record = store.docs_record("T-9").await.unwrap();
        store
            .set_docs_control(
                "T-9",
                &config::docs_lifecycle::DocsControl::from_record(&record),
            )
            .await
            .unwrap();
    }

    async fn published_docs_store() -> SharedStore {
        published_docs_store_with_seal(&config::docs_lifecycle::approval_seal(
            DOCS_SECRET,
            "T-9",
            1,
            "abc",
        ))
        .await
    }

    async fn mock_ci(server: &mut mockito::Server, sha: &str, state: &str) -> Vec<mockito::Mock> {
        vec![
            server
                .mock(
                    "GET",
                    format!("/repos/org/repo/commits/{sha}/status").as_str(),
                )
                .with_status(200)
                .with_body(format!(r#"{{"state":"{state}","total_count":1}}"#))
                .create_async()
                .await,
            server
                .mock(
                    "GET",
                    format!("/repos/org/repo/commits/{sha}/check-suites?per_page=100&page=1")
                        .as_str(),
                )
                .with_status(200)
                .with_body(r#"{"total_count":0,"check_suites":[]}"#)
                .create_async()
                .await,
        ]
    }

    async fn merge_mock(server: &mut mockito::Server, expect: usize) -> mockito::Mock {
        server
            .mock("PUT", "/repos/org/repo/pulls/42/merge")
            .match_body(mockito::Matcher::Regex(r#""sha":"abc""#.into()))
            .with_status(200)
            .with_body(r#"{"merged":true,"sha":"m1","message":"merged"}"#)
            .expect(expect)
            .create_async()
            .await
    }

    /// GitHub's current view of docs PR #42, read again right before merging.
    async fn mock_docs_pr(server: &mut mockito::Server, head: &str, base: &str) -> mockito::Mock {
        server
            .mock("GET", "/repos/org/repo/pulls/42")
            .with_status(200)
            .with_body(format!(r#"{{"number":42,"title":"docs: retries","body":null,"head":{{"sha":"{head}","ref":"lore/T-9"}},"base":{{"ref":"{base}","sha":"base"}},"state":"open","mergeable":true,"merged":false}}"#))
            .create_async()
            .await
    }

    /// GitHub reports `main` as the default branch and PR #42 changing `files`.
    async fn mock_docs_repo(server: &mut mockito::Server, files: &[&str]) -> Vec<mockito::Mock> {
        let files: Vec<String> = files
            .iter()
            .map(|f| format!(r#"{{"filename":"{f}","status":"modified"}}"#))
            .collect();
        vec![
            server
                .mock("GET", "/repos/org/repo")
                .with_status(200)
                .with_body(r#"{"default_branch":"main"}"#)
                .create_async()
                .await,
            server
                .mock("GET", "/repos/org/repo/pulls/42/files?per_page=100&page=1")
                .with_status(200)
                .with_body(format!("[{}]", files.join(",")))
                .create_async()
                .await,
        ]
    }

    #[tokio::test]
    async fn docs_pr_merges_only_at_the_sealed_approved_head() {
        let mut server = mockito::Server::new_async().await;
        let _repo = mock_docs_repo(&mut server, &["CHANGELOG.md"]).await;
        let _ci = mock_ci(&mut server, "abc", "success").await;
        let _pr = mock_docs_pr(&mut server, "abc", "main").await;
        let merge = merge_mock(&mut server, 1).await;
        let store = published_docs_store().await;
        let node = docs_test_node(&server, &store).await;

        // Commits pushed after approval go back to LORE for a new review.
        let moved = node.process_docs_pr("org", "repo", docs_pr("later")).await;
        assert!(
            matches!(&moved, VesselOutcome::DocsPrRework { reason, .. } if reason.contains("not the approved head")),
            "{moved:?}"
        );

        let merged = node.process_docs_pr("org", "repo", docs_pr("abc")).await;
        assert!(
            matches!(&merged, VesselOutcome::DocsPrMerged { docs_ticket, pr_number: 42, .. } if docs_ticket == "T-9"),
            "{merged:?}"
        );
        merge.assert_async().await;
    }

    #[tokio::test]
    async fn a_pr_retargeted_while_ci_runs_is_not_merged() {
        let mut server = mockito::Server::new_async().await;
        let _repo = mock_docs_repo(&mut server, &["CHANGELOG.md"]).await;
        let _ci = mock_ci(&mut server, "abc", "success").await;
        // Before CI the PR targets main; by the time CI passes it targets release.
        let _pr = mock_docs_pr(&mut server, "abc", "release").await;
        let merge = merge_mock(&mut server, 0).await;
        let store = published_docs_store().await;
        let node = docs_test_node(&server, &store).await;
        let outcome = node.process_docs_pr("org", "repo", docs_pr("abc")).await;
        assert!(
            matches!(&outcome, VesselOutcome::Unmanaged { reason, .. } if reason.contains("targets release")),
            "{outcome:?}"
        );
        merge.assert_async().await;
    }

    #[tokio::test]
    async fn a_head_that_changes_while_ci_runs_is_held_not_merged() {
        let mut server = mockito::Server::new_async().await;
        let _repo = mock_docs_repo(&mut server, &["CHANGELOG.md"]).await;
        let _ci = mock_ci(&mut server, "abc", "success").await;
        // CI passed for abc; by the time it finished the PR points at def.
        let _pr = mock_docs_pr(&mut server, "def", "main").await;
        let merge = server
            .mock("PUT", "/repos/org/repo/pulls/42/merge")
            .expect(0)
            .create_async()
            .await;
        let store = published_docs_store().await;
        let node = docs_test_node(&server, &store).await;
        let outcome = node.process_docs_pr("org", "repo", docs_pr("abc")).await;
        assert!(
            matches!(&outcome, VesselOutcome::DocsPrHeld { reason, .. } if reason.contains("while CI ran")),
            "{outcome:?}"
        );
        merge.assert_async().await;
    }

    #[tokio::test]
    async fn a_restored_approval_the_operator_revoked_never_merges() {
        let mut server = mockito::Server::new_async().await;
        let _repo = mock_docs_repo(&mut server, &["CHANGELOG.md"]).await;
        let _ci = mock_ci(&mut server, "abc", "success").await;
        let _pr = mock_docs_pr(&mut server, "abc", "main").await;
        let merge = merge_mock(&mut server, 0).await;
        let store = published_docs_store().await;
        let approved = store.docs_record("T-9").await.unwrap();
        // The operator rejects (the CLI records it controller-side first)...
        docs_step(
            &store,
            "human",
            DocsEvent::Decide {
                revision: 1,
                head: "abc".into(),
                approved: false,
                report: "explain the retry budget".into(),
                seal: String::new(),
            },
        )
        .await;
        record_operator_decision(&store).await;
        // ...then a workspace writes the old approved record back verbatim.
        store
            .set(
                &config::state::full_ticket_key_flat("T-9", config::state::KEY_TICKET_DOCS),
                serde_json::to_value(&approved).unwrap(),
            )
            .await;
        let node = docs_test_node(&server, &store).await;
        let outcome = node.process_docs_pr("org", "repo", docs_pr("abc")).await;
        assert!(
            matches!(&outcome, VesselOutcome::DocsPrHeld { reason, .. } if reason.contains("not the operator's latest decision")),
            "{outcome:?}"
        );
        merge.assert_async().await;
    }

    #[tokio::test]
    async fn forged_or_unverifiable_approvals_never_merge() {
        let mut server = mockito::Server::new_async().await;
        let merge = merge_mock(&mut server, 0).await;
        // A workspace that can write the store forges an approval.
        let forged = published_docs_store_with_seal(&"00".repeat(32)).await;
        let node = docs_test_node(&server, &forged).await;
        let outcome = node.process_docs_pr("org", "repo", docs_pr("abc")).await;
        assert!(
            matches!(outcome, VesselOutcome::DocsPrHeld { .. }),
            "{outcome:?}"
        );

        // Without the controller secret VESSEL cannot verify, so it holds.
        let genuine = published_docs_store().await;
        let node = docs_node_with_secret(&server, &genuine, None).await;
        let outcome = node.process_docs_pr("org", "repo", docs_pr("abc")).await;
        assert!(
            matches!(outcome, VesselOutcome::DocsPrHeld { .. }),
            "{outcome:?}"
        );
        merge.assert_async().await;
    }

    #[tokio::test]
    async fn docs_prs_without_an_owning_record_go_to_a_human() {
        let server = mockito::Server::new_async().await;
        let node = docs_test_node(&server, &SharedStore::new_in_memory()).await;
        let outcome = node.process_docs_pr("org", "repo", docs_pr("abc")).await;
        assert!(
            matches!(outcome, VesselOutcome::Unmanaged { .. }),
            "{outcome:?}"
        );

        // A PR from another branch than the record's is not LORE's.
        let store = published_docs_store().await;
        let node = docs_test_node(&server, &store).await;
        let mut foreign = docs_pr("abc");
        foreign.head_branch = "lore/T-9--c2".into();
        assert!(matches!(
            node.process_docs_pr("org", "repo", foreign).await,
            VesselOutcome::Unmanaged { .. }
        ));
    }

    #[tokio::test]
    async fn unapproved_or_paused_docs_prs_are_held_without_ci_polling() {
        let mut server = mockito::Server::new_async().await;
        let merge = merge_mock(&mut server, 0).await;
        // Awaiting the operator: the record owns the PR branch but has not published it.
        let store = submitted_docs_store().await;
        let node = docs_test_node(&server, &store).await;
        assert!(matches!(
            node.process_docs_pr("org", "repo", docs_pr("abc")).await,
            VesselOutcome::DocsPrHeld { .. }
        ));
        // The operator holds a published PR.
        let store = published_docs_store().await;
        docs_step(
            &store,
            "human",
            DocsEvent::Pause {
                reason: "release freeze".into(),
            },
        )
        .await;
        let node = docs_test_node(&server, &store).await;
        assert!(matches!(
            node.process_docs_pr("org", "repo", docs_pr("abc")).await,
            VesselOutcome::DocsPrHeld { .. }
        ));
        merge.assert_async().await;
    }

    #[tokio::test]
    async fn conflicting_or_failing_docs_prs_go_back_to_lore_not_forge() {
        let mut server = mockito::Server::new_async().await;
        let _repo = mock_docs_repo(&mut server, &["CHANGELOG.md"]).await;
        let merge = merge_mock(&mut server, 0).await;
        let store = published_docs_store().await;
        let node = docs_test_node(&server, &store).await;

        let mut conflicted = docs_pr("abc");
        conflicted.mergeable = Some(false);
        let outcome = node.process_docs_pr("org", "repo", conflicted).await;
        assert!(
            matches!(&outcome, VesselOutcome::DocsPrRework { reason, .. } if reason.contains("conflicts with main")),
            "{outcome:?}"
        );

        let _ci = mock_ci(&mut server, "abc", "failure").await;
        let outcome = node.process_docs_pr("org", "repo", docs_pr("abc")).await;
        assert!(
            matches!(&outcome, VesselOutcome::DocsPrRework { reason, docs_ticket, .. } if reason.contains("CI on docs PR #42") && docs_ticket == "T-9"),
            "{outcome:?}"
        );
        assert_eq!(outcome.ticket_id(), None, "never routed to a FORGE ticket");
        merge.assert_async().await;
    }

    /// `VesselNode::post`'s unoptimized frame alone is ~1.7 MB, so this runs
    /// on an 8 MiB stack like `test_post_reviews_resolves_real_forge_slot_*`.
    #[test]
    fn post_records_docs_rework_and_merge_on_the_docs_record() {
        std::thread::Builder::new()
            .stack_size(8 * 1024 * 1024)
            .spawn(|| {
                tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .unwrap()
                    .block_on(post_records_docs_rework_and_merge_on_the_docs_record_body())
            })
            .unwrap()
            .join()
            .unwrap();
    }

    async fn post_records_docs_rework_and_merge_on_the_docs_record_body() {
        use config::docs_lifecycle::DocsPhase;
        let store = published_docs_store().await;
        store
            .set(KEY_PENDING_PRS, json!([{"number": 42, "ticket_id": "T-9"}]))
            .await;
        let node = VesselNode::new(VesselConfig::default());
        let rework = json!({
            "outcomes": [VesselOutcome::DocsPrRework {
                pr_number: 42,
                reason: "conflicts with main".into(),
                docs_ticket: "T-9".into(),
            }],
            "has_work": true,
        });
        node.post(&store, rework.clone()).await.unwrap();
        let record = store.docs_record("T-9").await.unwrap();
        assert_eq!(record.phase, DocsPhase::Drafting);
        assert_eq!(record.pr_number, Some(42), "LORE updates the same PR");
        assert_eq!(record.feedback.as_deref(), Some("conflicts with main"));
        assert_eq!(
            store
                .get_typed::<Vec<Value>>(KEY_PENDING_PRS)
                .await
                .unwrap()
                .len(),
            1,
            "the reworked PR stays open and pending"
        );
        // A repeated outcome for a record that moved on is ignored.
        node.post(&store, rework).await.unwrap();
        assert_eq!(
            store.docs_record("T-9").await.unwrap().version,
            record.version
        );

        let store = published_docs_store().await;
        store
            .set(KEY_PENDING_PRS, json!([{"number": 42, "ticket_id": "T-9"}]))
            .await;
        node.post(
            &store,
            json!({
                "outcomes": [VesselOutcome::DocsPrMerged {
                    pr_number: 42,
                    sha: "m1".into(),
                    docs_ticket: "T-9".into(),
                }],
                "has_work": true,
            }),
        )
        .await
        .unwrap();
        let record = store.docs_record("T-9").await.unwrap();
        assert_eq!(record.phase, DocsPhase::Merged);
        assert_eq!(record.merge_sha.as_deref(), Some("m1"));
        assert!(store
            .get_typed::<Vec<Value>>(KEY_PENDING_PRS)
            .await
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    async fn docs_prs_changing_non_docs_files_go_back_to_lore() {
        let mut server = mockito::Server::new_async().await;
        let _repo = mock_docs_repo(&mut server, &["CHANGELOG.md", "src/lib.rs"]).await;
        let merge = merge_mock(&mut server, 0).await;
        let store = published_docs_store().await;
        let node = docs_test_node(&server, &store).await;
        // LORE reported only CHANGELOG.md; GitHub is the authority.
        let outcome = node.process_docs_pr("org", "repo", docs_pr("abc")).await;
        assert!(
            matches!(&outcome, VesselOutcome::DocsPrRework { reason, .. } if reason.contains("src/lib.rs")),
            "{outcome:?}"
        );
        merge.assert_async().await;
    }

    #[tokio::test]
    async fn docs_prs_into_another_base_go_to_a_human() {
        let mut server = mockito::Server::new_async().await;
        let _repo = mock_docs_repo(&mut server, &["CHANGELOG.md"]).await;
        let merge = merge_mock(&mut server, 0).await;
        let store = published_docs_store().await;
        let node = docs_test_node(&server, &store).await;
        let mut release = docs_pr("abc");
        release.base_branch = "release/1".into();
        let outcome = node.process_docs_pr("org", "repo", release).await;
        assert!(
            matches!(&outcome, VesselOutcome::Unmanaged { reason, .. } if reason.contains("release/1")),
            "{outcome:?}"
        );
        merge.assert_async().await;
    }

    #[tokio::test]
    async fn docs_pr_errors_hold_only_that_pr() {
        let mut server = mockito::Server::new_async().await;
        let merge = merge_mock(&mut server, 0).await;
        // A malformed docs record is never reset, so reading it fails every pass.
        let store = SharedStore::new_in_memory();
        store.set("ticket:T-9:docs", json!("garbage")).await;
        let node = docs_test_node(&server, &store).await;
        let outcome = node.process_docs_pr("org", "repo", docs_pr("abc")).await;
        assert!(
            matches!(&outcome, VesselOutcome::DocsPrHeld { reason, .. } if reason.contains("Malformed docs record")),
            "{outcome:?}"
        );

        // GitHub failing mid-way (no default-branch mock: 501) holds too.
        let store = published_docs_store().await;
        let node = docs_test_node(&server, &store).await;
        assert!(matches!(
            node.process_docs_pr("org", "repo", docs_pr("abc")).await,
            VesselOutcome::DocsPrHeld { .. }
        ));
        merge.assert_async().await;

        // A pass with the broken docs PR still processes the other pending PRs.
        let store = SharedStore::new_in_memory();
        store.set("ticket:T-9:docs", json!("garbage")).await;
        let _pr = server
            .mock("GET", "/repos/org/repo/pulls/42")
            .with_status(200)
            .with_body(r#"{"number":42,"title":"docs","body":null,"head":{"sha":"abc","ref":"lore/T-9"},"base":{"ref":"main","sha":"base"},"state":"open","mergeable":true,"merged":false}"#)
            .create_async()
            .await;
        let node = docs_test_node(&server, &store).await;
        let result = node
            .exec(json!({
                "owner": "org",
                "repo": "repo",
                "pending_prs": [
                    {"number": 42, "ticket_id": "T-9", "head_branch": "lore/T-9"},
                    {"number": 43, "ticket_id": ""}
                ],
                "has_ci_workflows": true,
            }))
            .await
            .unwrap();
        let outcomes: Vec<VesselOutcome> =
            serde_json::from_value(result["outcomes"].clone()).unwrap();
        assert!(
            matches!(
                outcomes.first(),
                Some(VesselOutcome::DocsPrHeld { pr_number: 42, .. })
            ),
            "{outcomes:?}"
        );
    }

    #[tokio::test]
    async fn post_keeps_held_docs_prs_pending() {
        let store = published_docs_store().await;
        store
            .set(KEY_PENDING_PRS, json!([{"number": 42, "ticket_id": "T-9"}]))
            .await;
        let node = VesselNode::new(VesselConfig::default());
        let action = node
            .post(
                &store,
                json!({
                    "outcomes": [VesselOutcome::DocsPrHeld {
                        pr_number: 42,
                        reason: "awaiting the operator".into(),
                    }],
                    "has_work": true,
                }),
            )
            .await
            .unwrap();
        assert_eq!(action.as_str(), PAUSE_SIGNAL);
        assert_eq!(
            store
                .get_typed::<Vec<Value>>(KEY_PENDING_PRS)
                .await
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            store.docs_record("T-9").await.unwrap().phase,
            config::docs_lifecycle::DocsPhase::Published
        );
    }

    #[tokio::test]
    async fn exec_routes_docs_prs_past_the_merged_tickets_lifecycle() {
        let mut server = mockito::Server::new_async().await;
        let _repo = mock_docs_repo(&mut server, &["CHANGELOG.md"]).await;
        let _pr = server
            .mock("GET", "/repos/org/repo/pulls/42")
            .with_status(200)
            .with_body(r#"{"number":42,"title":"docs: retries","body":null,"head":{"sha":"abc","ref":"lore/T-9"},"base":{"ref":"main","sha":"base"},"state":"open","mergeable":true,"merged":false}"#)
            .create_async()
            .await;
        let _ci = mock_ci(&mut server, "abc", "success").await;
        let merge = merge_mock(&mut server, 1).await;
        // T-9's own lifecycle is not in submit (it merged long ago).
        let store = published_docs_store().await;
        let node = docs_test_node(&server, &store).await;
        let result = node
            .exec(json!({
                "owner": "org",
                "repo": "repo",
                "pending_prs": [{"number": 42, "ticket_id": "T-9", "head_branch": "lore/T-9"}],
                "has_ci_workflows": true,
            }))
            .await
            .unwrap();
        let outcomes: Vec<VesselOutcome> =
            serde_json::from_value(result["outcomes"].clone()).unwrap();
        assert!(
            matches!(
                outcomes.as_slice(),
                [VesselOutcome::DocsPrMerged { pr_number: 42, .. }]
            ),
            "{outcomes:?}"
        );
        merge.assert_async().await;
    }
}

// crates/agent-lore/src/lib.rs
//
// LORE Agent — Documenter and Institutional Memory Keeper.
//
// Responsible for preserving project knowledge through:
// - Architecture Decision Records (ADRs)
// - Changelog maintenance
// - Sprint retrospectives
// - Documentation-as-code management

pub mod adr;
pub mod changelog;
pub mod docs;
pub mod readme;
pub mod retrospective;
pub mod types;

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use config::{KEY_PENDING_PRS, KEY_TICKETS};
use github::GithubRestClient;
use pocketflow_core::{Action, Node, SharedStore};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::Command as StdCommand;
use tracing::{debug, info, warn};

use crate::types::ChangeCategory;
use adr::AdrGenerator;
use changelog::ChangelogManager;
use config::Ticket;
use docs::DocsManager;
use readme::ReadmeManager;
use retrospective::RetrospectiveGenerator;
use types::{ArchitecturalDecision, LoreConfig, LoreOutcome, LoreTask, MergedTicketInfo};

pub use types::{LoreOutcome as LoreStatus, MergedTicketInfo as LoreTicketInfo};

pub struct LoreNode {
    config: LoreConfig,
    adr_generator: AdrGenerator,
    changelog_manager: ChangelogManager,
    readme_manager: ReadmeManager,
    docs_manager: DocsManager,
    #[allow(dead_code)]
    retrospective_generator: RetrospectiveGenerator,
}

impl LoreNode {
    pub fn new(workspace_root: impl Into<PathBuf>, persona_path: impl Into<PathBuf>) -> Self {
        let config = LoreConfig::new(workspace_root, persona_path);
        Self::from_config(config)
    }

    /// Create LORE node with token resolved from registry.
    pub fn new_with_registry(
        workspace_root: impl Into<PathBuf>,
        persona_path: impl Into<PathBuf>,
        registry_path: impl AsRef<std::path::Path>,
    ) -> anyhow::Result<Self> {
        let config = LoreConfig::new_with_registry(workspace_root, persona_path, registry_path)?;
        Ok(Self::from_config(config))
    }

    pub fn from_config(config: LoreConfig) -> Self {
        let adr_generator = AdrGenerator::new(config.adr_dir.clone());
        let changelog_manager = ChangelogManager::new(config.docs_dir.clone());
        let readme_manager = ReadmeManager::new(config.workspace_root.clone());
        let docs_manager = DocsManager::new(config.docs_dir.clone());
        let retrospective_generator = RetrospectiveGenerator::new(config.docs_dir.clone());

        Self {
            config,
            adr_generator,
            changelog_manager,
            readme_manager,
            docs_manager,
            retrospective_generator,
        }
    }

    pub fn from_env() -> Self {
        Self::from_config(LoreConfig::from_env())
    }

    async fn load_persona(&self) -> Result<String> {
        let content = tokio::fs::read_to_string(&self.config.persona_path)
            .await
            .map_err(|e| {
                anyhow!(
                    "Failed to load lore persona from {:?}: {}",
                    self.config.persona_path,
                    e
                )
            })?;
        Ok(content)
    }

    async fn get_merged_tickets_from_store(&self, store: &SharedStore) -> Vec<MergedTicketInfo> {
        let events = store.get_events_since(0).await;
        events
            .iter()
            .filter(|e| e.event_type == "ticket_merged")
            .filter_map(|e| {
                let ticket_id = e.payload["ticket_id"].as_str()?.to_string();
                let pr_number = e.payload["pr_number"].as_u64()?;
                let sha = e.payload["sha"].as_str().unwrap_or("").to_string();
                let pr_title = e.payload["pr_title"]
                    .as_str()
                    .unwrap_or(&format!("PR #{}", pr_number))
                    .to_string();
                let pr_body = e.payload["pr_body"].as_str().map(String::from);
                Some(MergedTicketInfo {
                    ticket_id,
                    pr_number,
                    pr_title,
                    pr_body,
                    sha,
                    merged_at: chrono::Utc::now().to_rfc3339(),
                    changes: Vec::new(),
                })
            })
            .collect()
    }

    async fn get_issue_body(&self, store: &SharedStore, ticket_id: &str) -> Option<String> {
        let tickets: Vec<Ticket> = store.get_typed(KEY_TICKETS).await.unwrap_or_default();
        tickets
            .iter()
            .find(|t| t.id == ticket_id)
            .map(|t| t.body.clone())
    }

    const KEY_LORE_PROCESSED_EVENTS: &str = "lore_processed_events";

    fn is_docs_pr(ticket_id: &str, pr_title: &str) -> bool {
        ticket_id == "T-DOCS"
            || ticket_id.starts_with("T-DOCS")
            || pr_title.starts_with("docs:")
            || pr_title.starts_with("Documentation Update")
    }

    async fn get_documentation_tasks(&self, store: &SharedStore) -> Vec<LoreTask> {
        let mut tasks = Vec::new();

        if let Some(queue) = store.get_typed::<Vec<Value>>("documentation_queue").await {
            for item in queue {
                if let Ok(task) = serde_json::from_value(item.clone()) {
                    tasks.push(task);
                }
            }
        }

        let processed: std::collections::HashSet<String> = store
            .get_typed::<Vec<String>>(Self::KEY_LORE_PROCESSED_EVENTS)
            .await
            .unwrap_or_default()
            .into_iter()
            .collect();

        let mut new_processed = processed.clone();

        let events = store.get_events_since(0).await;
        for event in events.iter() {
            if event.event_type == "ticket_merged" {
                if let (Some(ticket_id), Some(pr_number)) = (
                    event.payload["ticket_id"].as_str(),
                    event.payload["pr_number"].as_u64(),
                ) {
                    let event_key = format!("ticket_merged:{}:{}", ticket_id, pr_number);
                    if processed.contains(&event_key) {
                        debug!(event_key, "Skipping already processed ticket_merged event");
                        continue;
                    }

                    let pr_title = event.payload["pr_title"]
                        .as_str()
                        .map(String::from)
                        .unwrap_or_else(|| format!("Ticket {}", ticket_id));

                    if Self::is_docs_pr(ticket_id, &pr_title) {
                        info!(
                            ticket_id,
                            pr_number,
                            pr_title,
                            "Skipping docs PR in LORE — docs PRs don't need documentation"
                        );
                        new_processed.insert(event_key);
                        continue;
                    }

                    let pr_body = event.payload["pr_body"].as_str().map(String::from);

                    let issue_body = self.get_issue_body(store, ticket_id).await;
                    let enriched_body = issue_body.or(pr_body);

                    let needs_adr = !self.adr_generator.adr_exists_for_ticket(ticket_id).await;

                    if needs_adr {
                        let adr_title = pr_title
                            .strip_prefix(&format!("[{}] ", ticket_id))
                            .unwrap_or(&pr_title);

                        let (context, decision_summary, consequences) = if let Some(ref b) =
                            enriched_body
                        {
                            let lines: Vec<&str> = b
                                .lines()
                                .map(|l| l.trim())
                                .filter(|l| {
                                    !l.is_empty() && !l.starts_with('#') && !l.starts_with("---")
                                })
                                .take(8)
                                .collect();

                            let ctx = lines.join("\n");

                            let decision = if lines.iter().any(|l| {
                                l.contains("implement") || l.contains("add") || l.contains("create")
                            }) {
                                format!(
                                    "Adopt the implementation approach described in {}.",
                                    adr_title
                                )
                            } else if lines
                                .iter()
                                .any(|l| l.contains("fix") || l.contains("resolve"))
                            {
                                format!("Apply the fix described in {}.", adr_title)
                            } else {
                                format!(
                                    "Implement changes described in PR #{} for ticket {}.",
                                    pr_number, ticket_id
                                )
                            };

                            let conseq = format!(
                                "{} is now implemented and merged into the main branch. This resolves ticket {}.",
                                adr_title, ticket_id
                            );

                            (ctx, decision, conseq)
                        } else {
                            (
                                format!(
                                    "Changes merged in PR #{} for ticket {}.",
                                    pr_number, ticket_id
                                ),
                                format!(
                                    "Implement changes described in PR #{} for ticket {}.",
                                    pr_number, ticket_id
                                ),
                                format!(
                                    "Ticket {} is now resolved and merged into main branch.",
                                    ticket_id
                                ),
                            )
                        };

                        tasks.push(LoreTask::AdrGeneration {
                            decision: ArchitecturalDecision::new(
                                adr_title,
                                context,
                                decision_summary,
                                consequences,
                                ticket_id,
                                Some(pr_number),
                            ),
                        });
                    }

                    tasks.push(LoreTask::ChangelogUpdate {
                        ticket_id: ticket_id.to_string(),
                        pr_number,
                        changes: Vec::new(),
                        pr_title: Some(pr_title),
                        pr_body: enriched_body,
                    });

                    new_processed.insert(event_key);
                }
            }
        }

        if new_processed.len() > processed.len() {
            let processed_vec: Vec<String> = new_processed.into_iter().collect();
            store
                .set(Self::KEY_LORE_PROCESSED_EVENTS, json!(processed_vec))
                .await;
        }

        tasks
    }

    async fn process_changelog_update(&self, task: &LoreTask) -> Result<LoreOutcome> {
        let LoreTask::ChangelogUpdate {
            ticket_id,
            pr_number,
            changes: _,
            pr_title,
            pr_body,
        } = task
        else {
            return Ok(LoreOutcome::NoWork);
        };

        self.changelog_manager.ensure_changelog_exists().await?;

        let raw_title = pr_title
            .clone()
            .unwrap_or_else(|| format!("Ticket {}", ticket_id));
        let category = self
            .changelog_manager
            .categorize_from_pr(&raw_title, pr_body.as_deref());

        let entry =
            self.generate_changelog_entry(&raw_title, pr_body.as_deref(), ticket_id, category);

        self.changelog_manager
            .add_entry(category, &entry, *pr_number)
            .await?;

        Ok(LoreOutcome::ChangelogUpdated {
            entry: format!("{}: {} (#{})", category.as_str(), entry, pr_number),
        })
    }

    fn generate_changelog_entry(
        &self,
        title: &str,
        body: Option<&str>,
        ticket_id: &str,
        category: ChangeCategory,
    ) -> String {
        let clean_title = title
            .strip_prefix(&format!("[{}] ", ticket_id))
            .unwrap_or(title)
            .trim();

        if let Some(b) = body {
            let lines: Vec<&str> = b
                .lines()
                .map(|l| l.trim())
                .filter(|l| {
                    !l.is_empty()
                        && !l.starts_with('#')
                        && !l.starts_with("---")
                        && !l.starts_with("## ")
                        && !l.starts_with("Resolves #")
                        && !l.starts_with("- [")
                        && !l.starts_with("* [")
                })
                .collect();

            if !lines.is_empty() {
                let section_keywords = [
                    "Description",
                    "Summary",
                    "What",
                    "Changes",
                    "Implementation",
                ];
                let impl_section = lines
                    .iter()
                    .position(|l| section_keywords.iter().any(|kw| l.contains(kw)));
                let start = impl_section.map(|p| p + 1).unwrap_or(0);

                let content_lines: Vec<&&str> = lines[start..]
                    .iter()
                    .filter(|l| (!l.starts_with('-') && !l.starts_with('*')) || l.len() > 20)
                    .take(3)
                    .collect();

                if !content_lines.is_empty() {
                    let summary = content_lines[0]
                        .trim_start_matches('-')
                        .trim_start_matches('*')
                        .trim();
                    if summary.len() > 15 {
                        let mut entry = summary.to_string();
                        if let Some(first) = entry.get_mut(0..1) {
                            first.make_ascii_uppercase();
                        }
                        if !entry.ends_with('.') && !entry.ends_with('!') && !entry.ends_with('?') {
                            entry.push('.');
                        }
                        return entry;
                    }
                }
            }
        }

        let action_word = match category {
            ChangeCategory::Added => "Added",
            ChangeCategory::Fixed => "Fixed",
            ChangeCategory::Changed => "Updated",
            ChangeCategory::Removed => "Removed",
            ChangeCategory::Deprecated => "Deprecated",
            ChangeCategory::Security => "Secured",
        };

        format!("{} {}.", action_word, clean_title)
    }

    async fn process_adr_generation(&self, task: &LoreTask) -> Result<LoreOutcome> {
        let LoreTask::AdrGeneration { decision } = task else {
            return Ok(LoreOutcome::NoWork);
        };

        let path = self.adr_generator.generate(decision).await?;
        let adr_id = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("unknown")
            .to_string();

        Ok(LoreOutcome::AdrWritten {
            path: path.to_string_lossy().to_string(),
            adr_id,
        })
    }

    #[allow(dead_code)]
    async fn process_retrospective(
        &self,
        task: &LoreTask,
        store: &SharedStore,
    ) -> Result<LoreOutcome> {
        let LoreTask::Retrospective { sprint_id } = task else {
            return Ok(LoreOutcome::NoWork);
        };

        let tickets = RetrospectiveGenerator::read_sprint_history(store).await;
        let path = self
            .retrospective_generator
            .generate(sprint_id, &tickets, None)
            .await?;

        Ok(LoreOutcome::RetrospectiveGenerated {
            path: path.to_string_lossy().to_string(),
        })
    }

    async fn process_doc_sync(&self, task: &LoreTask) -> Result<LoreOutcome> {
        let LoreTask::DocSync { scope } = task else {
            return Ok(LoreOutcome::NoWork);
        };

        self.docs_manager.ensure_structure().await?;
        let docs = self.docs_manager.list_docs(*scope).await?;

        Ok(LoreOutcome::DocsSynced {
            updated: docs
                .iter()
                .map(|p| p.to_string_lossy().to_string())
                .collect(),
        })
    }

    async fn process_readme_update(&self, task: &LoreTask) -> Result<LoreOutcome> {
        let LoreTask::ReadmeUpdate {
            ticket_id: _,
            feature_summary,
        } = task
        else {
            return Ok(LoreOutcome::NoWork);
        };

        let updated = self
            .readme_manager
            .update_feature_section(feature_summary, feature_summary)
            .await?;

        if updated {
            Ok(LoreOutcome::ReadmeUpdated {
                sections: vec![feature_summary.clone()],
            })
        } else {
            Ok(LoreOutcome::NoWork)
        }
    }

    /// Detect the repository's default branch by reading origin/HEAD symref,
    /// falling back to checking remote refs, then defaulting to "main".
    fn detect_default_branch(project_root: &Path) -> String {
        // Method 1: Read origin/HEAD symref (most reliable)
        let output = StdCommand::new("git")
            .args(["symbolic-ref", "refs/remotes/origin/HEAD"])
            .current_dir(project_root)
            .output();

        if let Ok(o) = output {
            if o.status.success() {
                let refname = String::from_utf8_lossy(&o.stdout).trim().to_string();
                if let Some(branch) = refname.strip_prefix("refs/remotes/origin/") {
                    if !branch.is_empty() {
                        return branch.to_string();
                    }
                }
            }
        }

        // Method 2: Try git rev-parse for each candidate
        for candidate in ["main", "master"] {
            let output = StdCommand::new("git")
                .args(["rev-parse", "--verify", &format!("origin/{}", candidate)])
                .current_dir(project_root)
                .output();
            if let Ok(o) = output {
                if o.status.success() {
                    return candidate.to_string();
                }
            }
        }

        // Final fallback
        warn!("Could not detect default branch, falling back to 'main'");
        "main".to_string()
    }

    async fn commit_and_push_docs(&self, changed_files: &[PathBuf]) -> Result<()> {
        if changed_files.is_empty() {
            info!("LORE: No files to commit");
            return Ok(());
        }

        let workspace = &self.config.workspace_root;
        let timestamp = chrono::Utc::now().format("%Y%m%d-%H%M%S");
        let branch_name = format!("lore/docs-{}", timestamp);

        info!(
            branch = %branch_name,
            file_count = changed_files.len(),
            "LORE: Creating docs branch"
        );

        let output = StdCommand::new("git")
            .args(["rev-parse", "--abbrev-ref", "HEAD"])
            .current_dir(workspace)
            .output()?;
        let original_branch = String::from_utf8_lossy(&output.stdout).trim().to_string();

        // Stash uncommitted doc changes so we can switch branches cleanly.
        let stash_output = StdCommand::new("git")
            .args(["stash", "--include-untracked"])
            .current_dir(workspace)
            .output()?;
        let stashed = stash_output.status.success()
            && !String::from_utf8_lossy(&stash_output.stdout).contains("No local changes");

        // Fetch origin/{default_branch} so the docs branch is based on the latest default,
        // not on the forge branch that was previously checked out.
        // This prevents merge conflicts when the PR is opened against the default branch.
        let default_branch = Self::detect_default_branch(workspace);
        let fetch_output = StdCommand::new("git")
            .args(["fetch", "origin", &default_branch])
            .current_dir(workspace)
            .output()?;
        if !fetch_output.status.success() {
            let stderr = String::from_utf8_lossy(&fetch_output.stderr);
            warn!(error = %stderr, "LORE: git fetch origin/{} failed — will try creating branch from current HEAD", default_branch);
        }

        let origin_default = format!("origin/{}", default_branch);
        let base_arg = if fetch_output.status.success() {
            origin_default.as_str()
        } else {
            ""
        };

        let checkout_args = if base_arg.is_empty() {
            vec!["checkout", "-b", &branch_name]
        } else {
            vec!["checkout", "-b", &branch_name, base_arg]
        };

        let output = StdCommand::new("git")
            .args(&checkout_args)
            .current_dir(workspace)
            .output()?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            if stashed {
                let _ = StdCommand::new("git")
                    .args(["stash", "pop"])
                    .current_dir(workspace)
                    .output();
            }
            return Err(anyhow!(
                "Failed to create branch {}: {}",
                branch_name,
                stderr
            ));
        }
        info!(
            branch = %branch_name,
            base = if base_arg.is_empty() { "HEAD" } else { base_arg },
            "LORE: Created docs branch from {}",
            if base_arg.is_empty() { "current HEAD" } else { "origin/main" }
        );

        // Restore stashed doc changes on top of the new branch.
        if stashed {
            let pop_output = StdCommand::new("git")
                .args(["stash", "pop"])
                .current_dir(workspace)
                .output()?;
            if !pop_output.status.success() {
                let stderr = String::from_utf8_lossy(&pop_output.stderr);
                warn!(error = %stderr, "LORE: git stash pop had issues — some doc changes may need manual resolution");
            }
        }

        let output = StdCommand::new("git")
            .args(["add", "-A"])
            .current_dir(workspace)
            .output()?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(anyhow!("Failed to git add: {}", stderr));
        }

        let commit_msg = "docs: update documentation for merged PRs".to_string();
        let output = StdCommand::new("git")
            .args(["commit", "-m", &commit_msg])
            .current_dir(workspace)
            .output()?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            if stderr.contains("nothing to commit") {
                info!("LORE: No changes to commit");
                let _ = StdCommand::new("git")
                    .args(["checkout", &original_branch])
                    .current_dir(workspace)
                    .output();
                return Ok(());
            }
            return Err(anyhow!("Failed to commit: {}", stderr));
        }
        info!("LORE: Committed docs changes");

        let output = StdCommand::new("git")
            .args(["push", "-u", "origin", &branch_name])
            .current_dir(workspace)
            .output()?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(anyhow!("Failed to push branch {}: {}", branch_name, stderr));
        }
        info!("LORE: Pushed branch {}", branch_name);

        // Switch back to the original branch (or the default branch) so the workspace
        // is in a clean state for the next pipeline cycle.
        let default_branch = Self::detect_default_branch(workspace);
        let restore_branch = if original_branch == branch_name {
            default_branch.clone()
        } else {
            original_branch.clone()
        };
        let _ = StdCommand::new("git")
            .args(["checkout", &restore_branch])
            .current_dir(workspace)
            .output();

        Ok(())
    }

    async fn open_docs_pr(
        &self,
        store: &SharedStore,
        branch_name: &str,
        changed_files: &[PathBuf],
    ) -> Result<u64> {
        let repo = store
            .get_typed::<String>("repository")
            .await
            .ok_or_else(|| anyhow!("No repository configured in store"))?;

        let parts: Vec<&str> = repo.split('/').collect();
        if parts.len() != 2 {
            return Err(anyhow!("Invalid repository format: {}", repo));
        }
        let owner = parts[0];
        let repo_name = parts[1];

        let client = GithubRestClient::new(&self.config.github_token);

        let mut pr_body = String::from("## Documentation Update\n\n");
        pr_body.push_str("This PR contains automated documentation updates generated by LORE.\n\n");
        pr_body.push_str("### Files Changed\n\n");
        for file in changed_files {
            if let Ok(rel) = file.strip_prefix(&self.config.workspace_root) {
                pr_body.push_str(&format!("- `{}`\n", rel.display()));
            }
        }

        // Detect the default branch for PR base instead of hardcoding "main"
        let default_branch = Self::detect_default_branch(&self.config.workspace_root);

        let title = format!(
            "docs: update documentation ({})",
            chrono::Utc::now().format("%Y-%m-%d %H:%M")
        );
        let pr_number = client
            .create_pull_request(
                owner,
                repo_name,
                &title,
                branch_name,
                &default_branch,
                Some(&pr_body),
            )
            .await?;

        info!(pr_number, "LORE: Created documentation PR");

        let pr_entry = json!({
            "number": pr_number,
            "head_branch": branch_name,
            "head_sha": "",
            "base_branch": default_branch,
            "ticket_id": "T-DOCS",
            "title": title,
            "worker_id": "lore",
            "is_docs_pr": true,
        });

        let mut pending_prs: Vec<Value> =
            store.get_typed(KEY_PENDING_PRS).await.unwrap_or_default();
        pending_prs.push(pr_entry);
        store.set(KEY_PENDING_PRS, json!(pending_prs)).await;

        info!(pr_number, "LORE: Added docs PR to pending_prs");

        Ok(pr_number)
    }
}

#[async_trait]
impl Node for LoreNode {
    fn name(&self) -> &str {
        "lore"
    }

    async fn prep(&self, store: &SharedStore) -> Result<Value> {
        debug!("LORE prep: gathering documentation tasks");

        let tasks = self.get_documentation_tasks(store).await;
        let merged_tickets = self.get_merged_tickets_from_store(store).await;

        let persona = self.load_persona().await.ok();

        Ok(json!({
            "tasks": tasks,
            "merged_tickets": merged_tickets,
            "persona": persona,
            "workspace_root": self.config.workspace_root,
        }))
    }

    async fn exec(&self, prep_result: Value) -> Result<Value> {
        let tasks: Vec<LoreTask> =
            serde_json::from_value(prep_result["tasks"].clone()).unwrap_or_default();

        if tasks.is_empty() {
            info!("LORE: No documentation tasks to process");
            return Ok(json!({ "outcomes": [], "has_work": false }));
        }

        info!(count = tasks.len(), "LORE: Processing documentation tasks");

        let mut outcomes = Vec::new();

        for task in &tasks {
            let outcome = match task {
                LoreTask::ChangelogUpdate { .. } => self.process_changelog_update(task).await,
                LoreTask::AdrGeneration { .. } => self.process_adr_generation(task).await,
                LoreTask::Retrospective { .. } => Ok(LoreOutcome::NoWork),
                LoreTask::DocSync { .. } => self.process_doc_sync(task).await,
                LoreTask::ReadmeUpdate { .. } => self.process_readme_update(task).await,
            };

            match outcome {
                Ok(o) => {
                    info!(outcome = ?o, "Task completed");
                    outcomes.push(json!(o));
                }
                Err(e) => {
                    warn!(error = %e, task = ?task, "Task failed");
                    outcomes.push(json!({ "error": e.to_string() }));
                }
            }
        }

        Ok(json!({
            "outcomes": outcomes,
            "has_work": !outcomes.is_empty(),
        }))
    }

    async fn post(&self, store: &SharedStore, exec_result: Value) -> Result<Action> {
        let outcomes: Vec<Value> = exec_result["outcomes"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        let has_work = exec_result["has_work"].as_bool().unwrap_or(false);

        if !has_work {
            debug!("LORE: No documentation was generated");
            return Ok(Action::new("no_work"));
        }

        let mut changelog_updated = false;
        let mut adrs_written = Vec::new();
        let mut changed_files = Vec::new();

        for outcome in &outcomes {
            if let Some(obj) = outcome.as_object() {
                if obj.contains_key("ChangelogUpdated") {
                    changelog_updated = true;
                }
                if let Some(adr) = obj.get("AdrWritten") {
                    adrs_written.push(adr.clone());
                }
                if let Some(path_str) = obj.get("path").and_then(|v| v.as_str()) {
                    changed_files.push(PathBuf::from(path_str));
                }
            }
        }

        if changelog_updated {
            changed_files.push(self.config.docs_dir.join("CHANGELOG.md"));
        }

        if !changed_files.is_empty() {
            info!(
                file_count = changed_files.len(),
                "LORE: Committing and pushing documentation changes"
            );

            let branch_name = format!("lore/docs-{}", chrono::Utc::now().format("%Y%m%d-%H%M%S"));

            match self.commit_and_push_docs(&changed_files).await {
                Ok(()) => match self.open_docs_pr(store, &branch_name, &changed_files).await {
                    Ok(pr_number) => {
                        info!(pr_number, "LORE: Documentation PR created successfully");
                    }
                    Err(e) => {
                        warn!(error = %e, "LORE: Failed to create docs PR — changes committed but not opened as PR");
                    }
                },
                Err(e) => {
                    warn!(error = %e, "LORE: Failed to commit/push docs — changes remain local only");
                }
            }
        }

        if changelog_updated {
            store
                .emit(
                    "lore",
                    "changelog_updated",
                    json!({
                        "updated": true,
                    }),
                )
                .await;
        }

        if !adrs_written.is_empty() {
            store
                .emit(
                    "lore",
                    "adr_written",
                    json!({
                        "count": adrs_written.len(),
                        "adrs": adrs_written,
                    }),
                )
                .await;
        }

        info!(
            changelog_updated,
            adrs_count = adrs_written.len(),
            "LORE: Documentation tasks completed"
        );

        Ok(Action::new("docs_complete"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pocketflow_core::SharedStore;

    #[tokio::test]
    async fn test_lore_node_no_work() {
        let store = SharedStore::new_in_memory();
        let node = LoreNode::from_env();

        let action = node.run(&store).await.unwrap();
        assert_eq!(action.as_str(), "no_work");
    }
}

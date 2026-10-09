// crates/agent-vessel/src/node.rs
//
// VesselNode — orchestrates CI polling, merging, and notification.
// Implements the Node trait for integration with the Flow.

use anyhow::{Context, Result};
use async_trait::async_trait;
use coder_client::CoderClient;
use config::{
    state::{
        address_review_attempts_key, address_review_dispatched_at_key,
        address_review_dispatched_key, address_review_rearmed_key, full_ticket_key,
        full_ticket_key_flat, KEY_MERGE_READY_PRS, KEY_PENDING_PRS, KEY_TICKETS, KEY_TICKET_CHAT,
        KEY_TICKET_DEPLOYMENT, KEY_TICKET_REWORK_DIRECTIVE, KEY_TICKET_WORKSPACE, KEY_WORKER_SLOTS,
    },
    Envconfig, Ticket, TicketStatus, WorkerSlot, WorkerStatus, ACTION_ADDRESS_REVIEW_DISPATCHED,
    ACTION_CI_FIX_NEEDED, ACTION_CONFLICTS_DETECTED, ACTION_REWORK_PROVISION_NEEDED,
};
use openflows_notifier::{NotificationMessage, NotificationService};
use pocketflow_core::{node::PAUSE_SIGNAL, Action, CiStatus, Node, PrInfo, SharedStore};
use provisioner::transport::{CoderTransport, WorkspaceTransport};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use tracing::{debug, info, warn};

use crate::ci_poller::CiPollResult;
use crate::conflict_resolver::ConflictResolver;
use crate::pr_monitor::{
    build_directive, classify, collect_rework, PrMonitorState, ReworkEvidence,
};

use crate::types::{VesselConfig, VesselOutcome};
use crate::{CiPoller, PrMerger, VesselNotifier};

/// VESSEL Node — DevOps Specialist and Merge Gatekeeper.
///
/// Three-phase workflow:
/// 1. prep: Read pending PRs from SharedStore
/// 2. exec: Poll CI, detect conflicts, resolve if possible, merge if green, return outcomes
/// 3. post: Emit events, update tickets, return routing action
pub struct VesselNode {
    lifecycle_store: std::sync::Mutex<Option<SharedStore>>,
    config: VesselConfig,
    client: github::GithubRestClient,
    poller: CiPoller,
    merger: PrMerger,
    slots_lock: tokio::sync::Mutex<()>,
}

/// Maximum number of conflict resolution attempts before giving up.
const MAX_CONFLICT_RESOLUTION_ATTEMPTS: u32 = 3;

/// Maximum number of CI fix attempts before giving up.
const MAX_CI_FIX_ATTEMPTS: u32 = 3;

/// Maximum number of `/address_review` dispatch attempts before the PR is
/// surfaced to a human instead of looping forever.
const MAX_ADDRESS_REVIEW_ATTEMPTS: u32 = 3;

/// Lightweight struct to carry PR identification info for CI_FIX.md writing.
struct CiFixPrInfo {
    pr_number: u64,
    head_branch: String,
    /// Actual ticket_id from PR title (may differ from branch name).
    ticket_id: Option<String>,
}

impl VesselNode {
    pub fn new(config: VesselConfig) -> Self {
        let client = github::GithubRestClient::new(&config.github_token);

        Self {
            lifecycle_store: std::sync::Mutex::new(None),
            poller: CiPoller::new(config.ci_poll.clone(), client.clone()),
            merger: PrMerger::new(client.clone(), config.merge_method),
            client,
            config,
            slots_lock: tokio::sync::Mutex::new(()),
        }
    }

    pub fn from_env() -> Self {
        // Search for registry in OPENFLOWS_HOME first, then workspace root, then CWD
        let oh_path = config::TenantConfig::init_from_env()
            .map(|tenant| tenant.openflows_home())
            .unwrap_or_else(|_| std::path::PathBuf::from(".openflows"))
            .join("orchestration")
            .join("agent")
            .join("registry.json");
        let workspace_root = config::AgentConfig::init_from_env()
            .ok()
            .and_then(|agent| agent.effective_workspace_root())
            .map(std::path::PathBuf::from);
        let ws_path = workspace_root
            .as_ref()
            .map(|p| p.join("orchestration").join("agent").join("registry.json"));
        let cwd_path = std::env::current_dir()
            .ok()
            .map(|p| p.join("orchestration").join("agent").join("registry.json"));

        let registry_path = if oh_path.exists() {
            Some(oh_path)
        } else if let Some(ref p) = ws_path {
            if p.exists() {
                Some(p.clone())
            } else {
                cwd_path
            }
        } else {
            cwd_path
        };

        let config = match registry_path {
            Some(ref path) if path.exists() => {
                info!(registry_path = %path.display(), "VESSEL loading config from registry");
                VesselConfig::from_registry(path).unwrap_or_else(|e| {
                    warn!(error = %e, "VESSEL failed to load from registry, falling back to GITHUB_TOKEN");
                    VesselConfig::from_env()
                })
            }
            Some(path) => {
                warn!(path = %path.display(), "VESSEL registry path does not exist, using fallback token");
                VesselConfig::from_env()
            }
            _ => {
                warn!("VESSEL could not determine registry path, using fallback token");
                VesselConfig::from_env()
            }
        };
        info!(
            token_prefix = &config.github_token[..20.min(config.github_token.len())],
            "VESSEL token loaded"
        );
        Self::new(config)
    }

    fn resolve_worktree_path(&self, pr_info: &PrInfo) -> Option<PathBuf> {
        let workspace_root = config::AgentConfig::init_from_env()
            .ok()
            .and_then(|agent| agent.effective_workspace_root())?;
        let branch = &pr_info.head_branch;
        let parts: Vec<&str> = branch.splitn(2, '/').collect();
        if parts.len() != 2 {
            return None;
        }
        let pair_id = parts[0];
        // Worktrees are keyed by pair_id only (not pair_id-ticket_id).
        // See WorktreeManager::create_worktree which uses `worktrees_dir.join(pair_id)`.
        Some(
            PathBuf::from(workspace_root)
                .join("worktrees")
                .join(pair_id),
        )
    }

    fn worker_role(worker_id: &str) -> &str {
        worker_id
            .rsplit_once('-')
            .map(|(base, _)| base)
            .unwrap_or(worker_id)
    }

    async fn coder_client_from_store(store: &SharedStore) -> Option<CoderClient> {
        let coder = config::CoderConfig::init_from_env().ok();
        let coder_url: Option<String> = store
            .get_typed("coder_url")
            .await
            .or_else(|| coder.as_ref().map(|c| c.url.clone()));
        let coder_token: Option<String> = coder
            .and_then(|c| c.session_token)
            .or_else(|| std::env::var("CODER_API_TOKEN").ok());
        let coder_token = if coder_token.as_deref().is_some_and(|t| !t.is_empty()) {
            coder_token
        } else {
            store.get_typed("coder_api_token").await
        };
        match (coder_url, coder_token) {
            (Some(url), Some(token)) if !url.is_empty() && !token.is_empty() => {
                let client = CoderClient::new(&url, &token);
                client.resolve_current_user().await.ok();
                Some(client)
            }
            _ => None,
        }
    }

    #[allow(dead_code)]
    async fn worker_slot_for_pr(
        &self,
        store: &SharedStore,
        pr_info: &PrInfo,
    ) -> Option<(String, WorkerSlot)> {
        let pending_prs: Vec<Value> = store.get_typed(KEY_PENDING_PRS).await.unwrap_or_default();
        let worker_id = pending_prs
            .iter()
            .find(|p| p["number"].as_u64() == Some(pr_info.number))
            .and_then(|pr| {
                let wid = pr["worker_id"].as_str().unwrap_or("");
                if !wid.is_empty() {
                    return Some(wid.to_string());
                }
                Self::derive_worker_id_from_branch(pr["head_branch"].as_str().unwrap_or(""))
            })
            .or_else(|| Self::derive_worker_id_from_branch(&pr_info.head_branch))?;

        let slots: HashMap<String, WorkerSlot> =
            store.get_typed(KEY_WORKER_SLOTS).await.unwrap_or_default();
        slots.get(&worker_id).cloned().map(|slot| (worker_id, slot))
    }

    #[allow(dead_code)]
    async fn coder_transport_for_pr(
        &self,
        store: &SharedStore,
        pr_info: &PrInfo,
    ) -> Option<(String, CoderTransport)> {
        let (worker_id, slot) = self.worker_slot_for_pr(store, pr_info).await?;
        let workspace_id = slot.workspace_id?;
        let client = Self::coder_client_from_store(store).await?;
        Some((worker_id, CoderTransport::new(client, &workspace_id)))
    }

    async fn stop_coder_workspace_for_worker(
        &self,
        store: &SharedStore,
        worker_id: &str,
        workspace_id: &str,
    ) {
        if let Some(client) = Self::coder_client_from_store(store).await {
            if let Err(e) = client.stop_workspace(workspace_id).await {
                warn!(
                    worker_id,
                    workspace_id,
                    error = %e,
                    "Failed to stop Coder workspace"
                );
            }
        }

        self.clear_slot_workspace(store, worker_id, workspace_id)
            .await;
    }

    /// Atomically clears the workspace ID from worker slots in SharedStore.
    /// Serialized via `slots_lock` so that concurrent cleanup tasks cannot race on
    /// reading/modifying/writing `worker_slots` and inadvertently restore stale workspace references.
    async fn clear_slot_workspace(&self, store: &SharedStore, worker_id: &str, workspace_id: &str) {
        let _guard = self.slots_lock.lock().await;
        let mut slots: HashMap<String, WorkerSlot> =
            store.get_typed(KEY_WORKER_SLOTS).await.unwrap_or_default();
        let mut changed = false;
        if let Some(slot) = slots.get_mut(worker_id) {
            if slot.workspace_id.as_deref() == Some(workspace_id) {
                slot.workspace_id = None;
                changed = true;
            }
        }
        for slot in slots.values_mut() {
            if slot.workspace_id.as_deref() == Some(workspace_id) {
                slot.workspace_id = None;
                changed = true;
            }
        }
        if changed {
            store.set(KEY_WORKER_SLOTS, json!(slots)).await;
        }
    }

    /// Destroy a Coder workspace and archive all associated chats.
    /// Called during merge/cleanup to tear down ephemeral workspaces.
    /// Returns `true` if the workspace was successfully deleted.
    async fn destroy_coder_workspace(
        &self,
        store: &SharedStore,
        worker_id: &str,
        workspace_id: &str,
    ) -> bool {
        let client = match Self::coder_client_from_store(store).await {
            Some(c) => c,
            None => {
                warn!(
                    worker_id,
                    workspace_id,
                    "Coder client unavailable — retaining workspace reference for later cleanup"
                );
                return false;
            }
        };

        // Archive all chats associated with this workspace
        let chats = client.list_chats().await.unwrap_or_default();
        let ws_chats: Vec<_> = chats
            .iter()
            .filter(|c| c.workspace_id == workspace_id)
            .collect();

        let mut archived = 0;
        for chat in &ws_chats {
            if client.archive_chat(&chat.id).await.is_ok() {
                archived += 1;
            }
        }

        if !ws_chats.is_empty() {
            info!(
                workspace_id,
                archived,
                total = ws_chats.len(),
                "Archived chats before workspace destruction"
            );
        }

        // Delete the workspace
        match client.delete_workspace(workspace_id).await {
            Ok(_) => {
                info!(worker_id, workspace_id, "Destroyed Coder workspace");
                self.clear_slot_workspace(store, worker_id, workspace_id)
                    .await;
                true
            }
            Err(e) => {
                warn!(
                    worker_id,
                    workspace_id,
                    error = %e,
                    "Failed to delete Coder workspace — retaining workspace reference for later cleanup"
                );
                false
            }
        }
    }

    fn resolve_worker_id_from_pr(pr_entry: &Value) -> Option<String> {
        pr_entry["worker_id"]
            .as_str()
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .or_else(|| {
                Self::derive_worker_id_from_branch(pr_entry["head_branch"].as_str().unwrap_or(""))
            })
    }

    /// Stop Coder workspace(s) for the workers associated with a merged PR.
    async fn stop_coder_workspace_for_pr(
        &self,
        store: &SharedStore,
        pending_prs: &[Value],
        pr_number: u64,
    ) {
        let pr_entry = match pending_prs
            .iter()
            .find(|p| p["number"].as_u64() == Some(pr_number))
        {
            Some(e) => e,
            None => return,
        };
        let target_ticket = pr_entry["ticket_id"].as_str();
        let target_worker = Self::resolve_worker_id_from_pr(pr_entry);

        let slots: HashMap<String, WorkerSlot> =
            store.get_typed(KEY_WORKER_SLOTS).await.unwrap_or_default();
        let mut to_stop: Vec<(String, String)> = Vec::new();
        for (worker_id, slot) in &slots {
            if slot.status.ticket_id().is_some() && slot.status.ticket_id() != target_ticket {
                continue;
            }
            let matches_worker = target_worker.as_deref() == Some(worker_id.as_str());
            let matches_ticket =
                target_ticket.is_some() && slot.status.ticket_id() == target_ticket;
            if matches_ticket || (matches_worker && slot.status.ticket_id().is_none()) {
                if let Some(ref ws_id) = slot.workspace_id {
                    if !to_stop.iter().any(|(_, w)| w == ws_id) {
                        to_stop.push((worker_id.clone(), ws_id.clone()));
                    }
                }
            }
        }

        if let Some(tid) = target_ticket {
            for role in ["forge", "sentinel"] {
                let ws_key = full_ticket_key(tid, KEY_TICKET_WORKSPACE, role);
                if let Some(ws_id) = store.get_typed::<String>(&ws_key).await {
                    if !to_stop.iter().any(|(_, w)| w == &ws_id) {
                        let w_id = if role == "sentinel" {
                            "sentinel".to_string()
                        } else {
                            target_worker.as_deref().unwrap_or(role).to_string()
                        };
                        to_stop.push((w_id, ws_id));
                    }
                }
            }
        }

        for (worker_id, ws_id) in to_stop {
            self.stop_coder_workspace_for_worker(store, &worker_id, &ws_id)
                .await;
        }
    }

    /// Destroy Coder workspace(s) for the workers associated with a merged PR.
    /// Archives all chats and deletes the workspace.
    async fn destroy_coder_workspace_for_pr(
        &self,
        store: &SharedStore,
        pending_prs: &[Value],
        pr_number: u64,
    ) {
        let pr_entry = match pending_prs
            .iter()
            .find(|p| p["number"].as_u64() == Some(pr_number))
        {
            Some(e) => e,
            None => return,
        };
        let target_ticket = pr_entry["ticket_id"].as_str();
        let target_worker = Self::resolve_worker_id_from_pr(pr_entry);

        let slots: HashMap<String, WorkerSlot> =
            store.get_typed(KEY_WORKER_SLOTS).await.unwrap_or_default();
        let mut to_destroy: Vec<(String, String, String)> = Vec::new(); // (worker_id, ws_id, role)
        for (worker_id, slot) in &slots {
            if slot.status.ticket_id().is_some() && slot.status.ticket_id() != target_ticket {
                continue;
            }
            let matches_worker = target_worker.as_deref() == Some(worker_id.as_str());
            let matches_ticket =
                target_ticket.is_some() && slot.status.ticket_id() == target_ticket;
            if matches_ticket || (matches_worker && slot.status.ticket_id().is_none()) {
                if let Some(ref ws_id) = slot.workspace_id {
                    if !to_destroy.iter().any(|(_, w, _)| w == ws_id) {
                        let role = Self::worker_role(worker_id).to_string();
                        to_destroy.push((worker_id.clone(), ws_id.clone(), role));
                    }
                }
            }
        }

        if let Some(tid) = target_ticket {
            for role in ["forge", "sentinel"] {
                let ws_key = full_ticket_key(tid, KEY_TICKET_WORKSPACE, role);
                if let Some(ws_id) = store.get_typed::<String>(&ws_key).await {
                    if !to_destroy.iter().any(|(_, w, _)| w == &ws_id) {
                        let w_id = if role == "sentinel" {
                            "sentinel".to_string()
                        } else {
                            target_worker.as_deref().unwrap_or(role).to_string()
                        };
                        to_destroy.push((w_id, ws_id, role.to_string()));
                    }
                }
            }
        }

        for (worker_id, ws_id, role) in to_destroy {
            if self
                .destroy_coder_workspace(store, &worker_id, &ws_id)
                .await
            {
                if let Some(tid) = target_ticket {
                    let ws_key = full_ticket_key(tid, KEY_TICKET_WORKSPACE, &role);
                    if store.get_typed::<String>(&ws_key).await.as_deref() == Some(&ws_id) {
                        store.del(&ws_key).await;
                    }
                }
            } else if let Some(tid) = target_ticket {
                // Deletion failed: ensure the workspace ID is preserved in the ticket key
                // so the undeleted workspace can be cleaned up later by recovery
                let ws_key = full_ticket_key(tid, KEY_TICKET_WORKSPACE, &role);
                if store.get_typed::<String>(&ws_key).await.is_none() {
                    store.set(&ws_key, json!(ws_id)).await;
                }
            }
        }
    }

    /// Scan terminal tickets (Merged, Exhausted, or Completed) and retry deletion for any
    /// workspaces whose previous deletion attempt failed and whose ID is still
    /// retained in `full_ticket_key(ticket_id, KEY_TICKET_WORKSPACE, role)`.
    /// Runs all candidate deletions concurrently so slow Coder builds do not stack latency.
    pub async fn cleanup_terminal_ticket_workspaces(&self, store: &SharedStore) {
        let raw_tickets: Vec<Value> = store.get_typed(KEY_TICKETS).await.unwrap_or_default();
        let slots: HashMap<String, WorkerSlot> =
            store.get_typed(KEY_WORKER_SLOTS).await.unwrap_or_default();

        let mut to_cleanup = Vec::new();

        for raw_ticket in &raw_tickets {
            let ticket_id = match raw_ticket["id"].as_str() {
                Some(id) => id,
                None => continue,
            };

            let status_type = raw_ticket["status"]["type"].as_str().unwrap_or("");
            let is_terminal =
                match serde_json::from_value::<TicketStatus>(raw_ticket["status"].clone()) {
                    Ok(status) => status.is_terminal(),
                    Err(_) => matches!(status_type, "merged" | "exhausted" | "completed"),
                } || store
                    .lifecycle(ticket_id)
                    .await
                    .ok()
                    .is_some_and(|s| s.phase == config::lifecycle::Phase::Done);

            if !is_terminal {
                continue;
            }

            for role in ["forge", "sentinel"] {
                let ws_key = full_ticket_key(ticket_id, KEY_TICKET_WORKSPACE, role);
                if let Some(ws_id) = store.get_typed::<String>(&ws_key).await {
                    let target_worker = slots
                        .iter()
                        .find(|(_, s)| s.workspace_id.as_deref() == Some(&ws_id))
                        .map(|(w, _)| w.as_str())
                        .unwrap_or(role);

                    to_cleanup.push((
                        ticket_id.to_string(),
                        role.to_string(),
                        target_worker.to_string(),
                        ws_id,
                        ws_key,
                    ));
                }
            }
        }

        if to_cleanup.is_empty() {
            return;
        }

        let cleanup_futures = to_cleanup.into_iter().map(
            |(ticket_id, role, target_worker, ws_id, ws_key)| async move {
                info!(
                    ticket_id = %ticket_id,
                    role = %role,
                    worker_id = %target_worker,
                    workspace_id = %ws_id,
                    "Retrying deletion of retained workspace for terminal ticket"
                );

                if self
                    .destroy_coder_workspace(store, &target_worker, &ws_id)
                    .await
                    && store.get_typed::<String>(&ws_key).await.as_deref() == Some(&ws_id)
                {
                    store.del(&ws_key).await;
                }
            },
        );

        futures::future::join_all(cleanup_futures).await;
    }

    #[allow(dead_code)]
    fn build_conflict_resolution_content(
        pr_info: &PrInfo,
        conflicted_files: &[String],
        fallback_ticket_id: Option<&str>,
    ) -> String {
        let branch = &pr_info.head_branch;
        let _ticket_id = pr_info.ticket_id.clone().unwrap_or_else(|| {
            fallback_ticket_id
                .map(|fb| fb.to_string())
                .unwrap_or_else(|| format!("T-{}", pr_info.number))
        });

        let files_list = if conflicted_files.is_empty() {
            "No specific conflicted files detected — resolve all conflict markers.".to_string()
        } else {
            conflicted_files
                .iter()
                .map(|f| format!("- {}", f))
                .collect::<Vec<_>>()
                .join("\n")
        };

        format!(
             "# Conflict Resolution Required\n\n\
              VESSEL detected merge conflicts between your branch and the default branch.\n\
              `git merge origin/<default>` has been run in your worktree — conflict markers are present.\n\n\
             ## Instructions\n\n\
             1. Open each conflicted file listed below\n\
             2. Resolve all conflict markers (`<<<<<<<`, `=======`, `>>>>>>>`)\n\
             3. Choose the correct integration of both sides — do NOT just pick one\n\
             4. Stage the resolved files: `git add -A`\n\
             5. Commit: `git commit -m \"resolve merge conflicts\"`\n\
             6. Push: `git push`\n\
             7. Write STATUS.json with `\"status\": \"PR_OPENED\"` and your PR number\n\n\
             ## Context\n\n\
             Branch: {}\n\n\
             ## Conflicted Files\n\n{}\n\n\
             ## Important\n\n\
             - Do NOT abort the merge — the conflict markers are there for you to resolve\n\
             - Resolve ALL conflict markers before committing\n\
             - After you push, VESSEL will re-monitor CI automatically",
            branch,
            files_list,
        )
    }

    #[allow(dead_code)]
    async fn write_conflict_resolution_md_to_transport<T: WorkspaceTransport + ?Sized>(
        &self,
        transport: &T,
        pr_info: &PrInfo,
        conflicted_files: &[String],
        fallback_ticket_id: Option<&str>,
    ) -> bool {
        let content =
            Self::build_conflict_resolution_content(pr_info, conflicted_files, fallback_ticket_id);
        match transport
            .write_file(".pair-shared/CONFLICT_RESOLUTION.md", &content)
            .await
        {
            Ok(()) => {
                info!(
                    pr_number = pr_info.number,
                    "Wrote CONFLICT_RESOLUTION.md via workspace transport"
                );
                true
            }
            Err(e) => {
                warn!(
                    pr_number = pr_info.number,
                    error = %e,
                    "Failed to write CONFLICT_RESOLUTION.md via workspace transport"
                );
                false
            }
        }
    }

    #[allow(dead_code)]
    async fn detect_default_branch_via_transport<T: WorkspaceTransport + ?Sized>(
        transport: &T,
    ) -> String {
        if let Ok(output) = transport
            .execute("git symbolic-ref refs/remotes/origin/HEAD")
            .await
        {
            if output.exit_code == 0 {
                let refname = output.stdout.trim();
                if let Some(branch) = refname.strip_prefix("refs/remotes/origin/") {
                    if !branch.is_empty() {
                        return branch.to_string();
                    }
                }
            }
        }

        for candidate in ["main", "master"] {
            if let Ok(output) = transport
                .execute(&format!("git rev-parse --verify origin/{}", candidate))
                .await
            {
                if output.exit_code == 0 {
                    return candidate.to_string();
                }
            }
        }

        warn!("Could not detect default branch via transport, falling back to 'main'");
        "main".to_string()
    }

    #[allow(dead_code)]
    async fn list_conflicted_files_via_transport<T: WorkspaceTransport + ?Sized>(
        transport: &T,
    ) -> Vec<String> {
        match transport
            .execute("git diff --name-only --diff-filter=U")
            .await
        {
            Ok(output) if output.exit_code == 0 => output
                .stdout
                .lines()
                .map(|l| l.trim().to_string())
                .filter(|l| !l.is_empty())
                .collect(),
            _ => vec![],
        }
    }

    #[allow(dead_code)]
    async fn merge_origin_main_via_transport<T: WorkspaceTransport + ?Sized>(
        &self,
        transport: &T,
        branch: &str,
    ) -> Vec<String> {
        let _ = transport.execute("git rebase --abort || true").await;
        let default_branch = Self::detect_default_branch_via_transport(transport).await;
        let origin_ref = format!("origin/{}", default_branch);

        let fetch = match transport
            .execute(&format!("git fetch origin {}", default_branch))
            .await
        {
            Ok(output) => output,
            Err(e) => {
                warn!(branch, error = %e, "git fetch {} failed in workspace transport", origin_ref);
                return vec!["unknown — fetch failed".to_string()];
            }
        };
        if fetch.exit_code != 0 {
            warn!(
                branch,
                stderr = %fetch.stderr,
                "git fetch {} failed in workspace transport",
                origin_ref
            );
            return vec!["unknown — fetch failed".to_string()];
        }

        let merge = match transport
            .execute(&format!("git merge {} --no-edit", origin_ref))
            .await
        {
            Ok(output) => output,
            Err(e) => {
                warn!(branch, error = %e, "git merge {} failed in workspace transport", origin_ref);
                return vec!["unknown — merge failed".to_string()];
            }
        };
        if merge.exit_code == 0 {
            info!(branch, "{} merged cleanly — no conflicts", origin_ref);
            return vec![];
        }

        if merge
            .stderr
            .contains("refusing to merge unrelated histories")
        {
            warn!(
                branch,
                "Unrelated histories — retrying with --allow-unrelated-histories"
            );
            let retry = match transport
                .execute(&format!(
                    "git merge {} --no-edit --allow-unrelated-histories",
                    origin_ref
                ))
                .await
            {
                Ok(output) => output,
                Err(_) => return vec!["unknown — merge failed".to_string()],
            };
            if retry.exit_code == 0 {
                info!(
                    branch,
                    "{} merged cleanly with --allow-unrelated-histories", origin_ref
                );
                return vec![];
            }
            let files = Self::list_conflicted_files_via_transport(transport).await;
            info!(
                branch,
                files = files.len(),
                "Merge with --allow-unrelated-histories produced conflict markers"
            );
            return files;
        }

        let files = Self::list_conflicted_files_via_transport(transport).await;
        info!(
            branch,
            files = files.len(),
            "Merge produced conflict markers in workspace transport"
        );
        files
    }
}

#[async_trait]
impl Node for VesselNode {
    fn name(&self) -> &str {
        "vessel"
    }

    /// Phase 1: Read pending PRs and CI readiness from SharedStore.
    async fn prep(&self, store: &SharedStore) -> Result<Value> {
        *self.lifecycle_store.lock().unwrap() = Some(store.clone());
        debug!("VESSEL prep: reading pending PRs and CI readiness");

        let repository: Option<String> = store.get_typed("repository").await;
        let pending_prs: Vec<Value> = store.get_typed(KEY_PENDING_PRS).await.unwrap_or_default();
        let merge_ready_prs: Option<Vec<Value>> = store.get_typed(KEY_MERGE_READY_PRS).await;
        let ci_readiness: Option<crate::types::CiReadiness> = store.get_typed("ci_readiness").await;

        let (owner, repo) = parse_repository(repository.as_deref());

        // Event-driven re-arm: FORGE writes `_address_review_rearmed_{pr}` after it
        // addresses a `/address_review` and re-arms the PR. Re-add those PRs so we
        // resume polling them without depending on NEXUS re-discovery.
        self.rearm_review_prs(store, owner, repo).await;

        // The NEXUS merge-ready handoff is a snapshot from the last NEXUS pass.
        // A PR can reach `submit` via FORGE/SENTINEL (or leave the queue) between
        // passes, so when a snapshot exists, refresh it against the live pending
        // queue: keep the snapshot, then re-include any pending PR whose lifecycle
        // has since reached submit. When no snapshot exists, fall back to the full
        // pending queue (VESSEL re-validates the phase before merging, so this is
        // never unsafe).
        let effective_merge_ready = match merge_ready_prs {
            Some(snapshot) => {
                let lifecycle_store = self.lifecycle_store.lock().unwrap().clone();
                let mut effective = snapshot;
                for pr in &pending_prs {
                    if effective.iter().any(|m| m["number"] == pr["number"]) {
                        continue;
                    }
                    let ticket = pr["ticket_id"].as_str().unwrap_or_default();
                    let is_submit = match lifecycle_store.as_ref() {
                        Some(s) => s
                            .lifecycle(ticket)
                            .await
                            .map(|state| state.phase == config::lifecycle::Phase::Submit)
                            .unwrap_or(false),
                        None => false,
                    };
                    if is_submit {
                        effective.push(pr.clone());
                    }
                }
                effective
            }
            None => pending_prs,
        };

        let has_ci_workflows = match ci_readiness {
            Some(crate::types::CiReadiness::Ready) => true,
            Some(crate::types::CiReadiness::Missing)
            | Some(crate::types::CiReadiness::SetupInProgress) => false,
            None => {
                if !owner.is_empty() && !repo.is_empty() {
                    self.client.has_workflows(owner, repo).await.unwrap_or(true)
                } else {
                    true
                }
            }
        };

        Ok(json!({
            "owner": owner,
            "repo": repo,
            "pending_prs": effective_merge_ready,
            "has_ci_workflows": has_ci_workflows,
        }))
    }

    /// Phase 2: Process each pending PR (check CI readiness → poll CI → merge → return outcome).
    async fn exec(&self, prep_result: Value) -> Result<Value> {
        let owner = prep_result["owner"].as_str().unwrap_or("");
        let repo = prep_result["repo"].as_str().unwrap_or("");
        let pending_prs = prep_result["pending_prs"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        let has_ci_workflows = prep_result["has_ci_workflows"].as_bool().unwrap_or(true);

        if pending_prs.is_empty() {
            info!("No pending PRs to process");
            return Ok(json!({ "outcomes": [], "has_work": false }));
        }

        info!(
            count = pending_prs.len(),
            has_ci_workflows, "Processing pending PRs"
        );

        let mut outcomes = Vec::new();

        for pr in pending_prs {
            let pr_number = pr["number"].as_u64().unwrap_or(0);

            if pr_number == 0 {
                warn!(pr = ?pr, "Skipping invalid PR entry");
                continue;
            }

            debug!(pr_number, "Fetching PR details");

            let mut pr_info = match self.client.get_pull_request(owner, repo, pr_number).await {
                Ok(info) => info,
                Err(e) => {
                    warn!(pr_number, error = %e, "Failed to fetch PR details, skipping");
                    continue;
                }
            };

            let store = self
                .lifecycle_store
                .lock()
                .unwrap()
                .clone()
                .context("VESSEL lifecycle store not initialized")?;
            let ticket = pr["ticket_id"].as_str().or(pr_info.ticket_id.as_deref());
            let Some(ticket) = ticket
                .filter(|ticket| !ticket.trim().is_empty())
                .map(str::to_owned)
            else {
                outcomes.push(VesselOutcome::Unmanaged {
                    pr_number,
                    reason: "PR has no lifecycle ticket; manual review and merge required".into(),
                });
                continue;
            };
            pr_info.ticket_id = Some(ticket.clone());
            let ticket = ticket.as_str();
            let state = store.lifecycle(ticket).await?;
            if let Some(sha) = self
                .client
                .confirmed_merge_sha(owner, repo, pr_number)
                .await?
            {
                if state.phase != config::lifecycle::Phase::Done {
                    store
                        .transition(
                            ticket,
                            state.version,
                            "vessel",
                            config::lifecycle::Event::ReconcileMerged {
                                number: pr_number,
                                sha: sha.clone(),
                            },
                        )
                        .await?;
                }
                outcomes.push(VesselOutcome::Merged {
                    ticket_id: ticket.to_owned(),
                    pr_number,
                    sha,
                    pr_title: pr_info.title.clone(),
                    pr_body: pr_info.body.clone(),
                });
                continue;
            }
            // An unmerged snapshot cannot prove that an earlier timed-out request
            // has stopped executing. Only confirmed completion clears an unknown outcome.
            if state.merge_pending {
                warn!(
                    ticket,
                    "Merge outcome uncertain; waiting for reconciliation"
                );
                continue;
            }
            if state.phase != config::lifecycle::Phase::Submit {
                info!(
                    pr_number,
                    ticket,
                    phase = state.phase.as_str(),
                    "Deferring PR: lifecycle is not in submit phase"
                );
                continue;
            }
            if state.head.as_deref() != Some(&pr_info.head_sha) {
                info!(pr_number, ticket, expected_head = ?state.head, actual_head = %pr_info.head_sha,
                    "Deferring PR: head changed; returning ticket to building");
                if !state.merge_pending {
                    store
                        .transition(
                            ticket,
                            state.version,
                            "vessel",
                            config::lifecycle::Event::Move {
                                phase: config::lifecycle::Phase::Building,
                                head: None,
                            },
                        )
                        .await?;
                }
                continue;
            }
            if !state.merge_ready(&pr_info.head_sha) {
                // Reconcile a human's GitHub approval into the lifecycle before
                // deferring. SENTINEL shares the PR author's GitHub identity, so
                // it cannot self-approve; an authorized reviewer's APPROVED review
                // of the current head therefore satisfies the `pr_human` gate.
                // Review-fetch failures defer only this PR, never the batch.
                let reconciled = self
                    .reconcile_github_approval(
                        &store,
                        ticket,
                        &state,
                        owner,
                        repo,
                        pr_number,
                        &pr_info.head_sha,
                    )
                    .await?;
                let ready = if reconciled {
                    store
                        .lifecycle(ticket)
                        .await?
                        .merge_ready(&pr_info.head_sha)
                } else {
                    false
                };
                if !ready {
                    let needs_rework = self.check_pr_needs_rework(owner, repo, &pr_info).await;
                    if !needs_rework {
                        info!(
                            pr_number,
                            ticket,
                            review_approved =
                                state.pr_decision.as_ref().is_some_and(|d| d.approved),
                            human_approved = state.pr_human.as_ref().is_some_and(|d| d.approved),
                            delivery_pending = state.pr_delivery.is_some(),
                            "Deferring PR: waiting for lifecycle approvals or review delivery"
                        );
                        continue;
                    }
                    info!(
                        pr_number,
                        ticket,
                        "PR is not merge_ready but requires rework (reviews/CI/conflicts); proceeding to process"
                    );
                }
            }

            let evidence = Self::get_rework_evidence(&store, pr_number).await;
            let outcome = self
                .process_single_pr(owner, repo, pr_info, evidence.as_ref())
                .await?;
            outcomes.push(outcome);
        }

        Ok(json!({
            "outcomes": outcomes,
            "has_work": !outcomes.is_empty(),
        }))
    }

    /// Phase 3: Emit events, update SharedStore, recycle workers, return routing action.
    async fn post(&self, store: &SharedStore, exec_result: Value) -> Result<Action> {
        let outcomes: Vec<VesselOutcome> =
            serde_json::from_value(exec_result["outcomes"].clone()).unwrap_or_default();
        let has_work = exec_result["has_work"].as_bool().unwrap_or(false);

        let pending_prs: Vec<Value> = store.get_typed(KEY_PENDING_PRS).await.unwrap_or_default();
        if !has_work {
            if !pending_prs.is_empty() {
                // Returning no_work routes immediately back to Nexus, which
                // would send the unchanged queue straight back to Vessel.
                info!(
                    pending_count = pending_prs.len(),
                    "Pending PRs deferred; pausing until the next controller poll"
                );
                return Ok(Action::new(PAUSE_SIGNAL));
            }
            debug!("No PRs were processed");
            self.cleanup_terminal_ticket_workspaces(store).await;
            return Ok(Action::new("no_work"));
        }

        let mut any_success = false;
        let mut any_failure = false;
        let mut any_conflicts = false;
        let mut any_ci_fix = false;
        let mut any_awaiting_human = false;
        let mut any_address_review = false;
        let mut any_rework_provision = false;
        let mut any_needs_review = false;
        let mut failed_ticket_ids: Vec<String> = Vec::new();

        for outcome in outcomes {
            let rework = matches!(
                &outcome,
                VesselOutcome::CiFailed { .. }
                    | VesselOutcome::CiTimeout { .. }
                    | VesselOutcome::Conflicts { .. }
            ) || matches!(&outcome, VesselOutcome::Reviews { state, .. } if state != "needs_review");
            if rework {
                if let Some(ticket) = outcome.ticket_id() {
                    let pr_num = outcome.pr_number();
                    let is_already_dispatched = match &outcome {
                        VesselOutcome::Reviews { head_sha, .. } => {
                            let curr = head_sha
                                .clone()
                                .or(self.pending_pr_head_sha(store, pr_num).await);
                            let last = self.get_address_review_dispatched_sha(store, pr_num).await;
                            match curr {
                                Some(ref sha) => last.as_deref() == Some(sha.as_str()),
                                None => false,
                            }
                        }
                        _ => false,
                    };
                    if !is_already_dispatched {
                        let current = store.lifecycle(ticket).await?;
                        if current.phase == config::lifecycle::Phase::Submit
                            && !current.merge_pending
                        {
                            store
                                .transition(
                                    ticket,
                                    current.version,
                                    "vessel",
                                    config::lifecycle::Event::Move {
                                        phase: config::lifecycle::Phase::Building,
                                        head: None,
                                    },
                                )
                                .await?;
                        }
                    }
                }
            }
            match &outcome {
                VesselOutcome::Merged {
                    ticket_id,
                    pr_number,
                    sha,
                    pr_title,
                    pr_body,
                } => {
                    VesselNotifier::emit_ticket_merged(
                        store,
                        ticket_id,
                        *pr_number,
                        sha,
                        pr_title,
                        pr_body.as_deref(),
                    )
                    .await;
                    VesselNotifier::set_ticket_status_merged(store, ticket_id).await;

                    // Write deployment key to SharedStore (§6.1 schema)
                    {
                        let dep_key = full_ticket_key_flat(ticket_id, KEY_TICKET_DEPLOYMENT);
                        store
                            .set(
                                &dep_key,
                                json!({
                                    "merged": true,
                                    "pr_number": *pr_number,
                                    "sha": sha,
                                }),
                            )
                            .await;
                        info!(ticket_id, pr_number, "Wrote deployment key to SharedStore");
                    }

                    // Destroy Coder workspace (archive chats + delete) for this worker
                    self.destroy_coder_workspace_for_pr(store, &pending_prs, *pr_number)
                        .await;

                    // Parallelize post-merge operations for reduced latency
                    // Run GitHub issue close concurrently with store updates
                    let ticket_id_clone = ticket_id.clone();
                    let pr_number_val = *pr_number;
                    tokio::join!(
                        // GitHub API call - network I/O
                        async {
                            self.close_github_issue(store, &ticket_id_clone).await;
                        },
                        // Store operations - local I/O
                        async {
                            self.update_ticket_status(store, &ticket_id_clone, "merged")
                                .await;
                            self.remove_from_pending_prs(store, pr_number_val).await;
                        }
                    );

                    let mut pr_val = pending_prs
                        .iter()
                        .find(|p| p["number"].as_u64() == Some(*pr_number))
                        .cloned()
                        .unwrap_or_else(|| json!({ "number": pr_number }));
                    if pr_val["ticket_id"].as_str().is_none() {
                        pr_val["ticket_id"] = json!(ticket_id);
                    }
                    self.recycle_worker(store, &pr_val).await;

                    any_success = true;
                }
                VesselOutcome::CiFailed {
                    ticket_id,
                    pr_number,
                    reason,
                    failure_detail,
                } => {
                    VesselNotifier::emit_ci_failed(store, ticket_id.as_deref(), *pr_number, reason)
                        .await;
                    let tid = ticket_id
                        .clone()
                        .unwrap_or_else(|| format!("T-{}", pr_number));

                    let current_ci_attempts = self.get_ci_fix_attempts(store, *pr_number).await;
                    info!(
                        pr_number,
                        ticket_id = %tid,
                        current_ci_attempts,
                        max = MAX_CI_FIX_ATTEMPTS,
                        "CI fix attempt counter check (CiFailed)"
                    );

                    if current_ci_attempts >= MAX_CI_FIX_ATTEMPTS {
                        warn!(
                            pr_number,
                            ticket_id = %tid,
                            attempts = current_ci_attempts,
                            "Max CI fix attempts exceeded — marking ticket as failed"
                        );
                        if !failed_ticket_ids.contains(&tid) {
                            self.mark_ticket_failed(
                                store,
                                &tid,
                                &format!(
                                    "CI failed for PR #{} after {} fix attempts",
                                    pr_number, current_ci_attempts
                                ),
                            )
                            .await;
                            failed_ticket_ids.push(tid);
                        }
                        self.remove_from_pending_prs(store, *pr_number).await;
                        any_failure = true;
                        continue;
                    }

                    let worker_id = pending_prs
                        .iter()
                        .find(|p| p["number"].as_u64() == Some(*pr_number))
                        .and_then(|pr| {
                            let wid = pr["worker_id"].as_str().unwrap_or("");
                            if !wid.is_empty() {
                                return Some(wid.to_string());
                            }
                            Self::derive_worker_id_from_branch(
                                pr["head_branch"].as_str().unwrap_or(""),
                            )
                        });

                    let pr_entry = pending_prs
                        .iter()
                        .find(|p| p["number"].as_u64() == Some(*pr_number));
                    let head_branch = pr_entry
                        .and_then(|p| p["head_branch"].as_str().map(|s| s.to_string()))
                        .unwrap_or_else(|| {
                            ticket_id
                                .as_deref()
                                .map(|tid| format!("unknown-pair/{}", tid))
                                .unwrap_or_else(|| format!("unknown/{}", pr_number))
                        });

                    // Prefer chat-based `/ci_fix` dispatch into the existing FORGE
                    // chat/workspace so the existing forge is reused (it checks out
                    // the already-existing branch) instead of spawning a fresh
                    // workspace and starting from scratch. If no forge chat / Coder
                    // client is available, fall back to the file-based `CI_FIX.md`
                    // marker + worker reassignment below (which lets NEXUS
                    // provision/reuse a forge for the issue).
                    let ci_chat_dispatched = self
                        .dispatch_ci_fix_for_pr(
                            store,
                            *pr_number,
                            &tid,
                            &head_branch,
                            reason,
                            failure_detail.as_ref(),
                        )
                        .await;

                    let ci_fix_md_written = if ci_chat_dispatched {
                        info!(
                            pr_number,
                            ticket_id = %tid,
                            "Dispatched /ci_fix to existing forge chat — reusing forge workspace"
                        );
                        false
                    } else {
                        self.write_ci_fix_md(
                            &CiFixPrInfo {
                                pr_number: *pr_number,
                                head_branch: head_branch.clone(),
                                ticket_id: ticket_id.clone(),
                            },
                            reason,
                            failure_detail.as_ref(),
                        )
                        .await
                    };

                    if ci_fix_md_written {
                        info!(
                            pr_number,
                            ticket_id = %tid,
                            "Wrote CI_FIX.md — routing to forge_pair for CI fix"
                        );
                    } else if !ci_chat_dispatched {
                        warn!(
                            pr_number,
                            ticket_id = %tid,
                            "CI_FIX.md NOT written — CI fix may be incomplete"
                        );
                    }

                    let worker_reassigned = if let Some(ref wid) = worker_id {
                        if self.assign_worker_for_ci_fix(store, wid, &tid).await {
                            true
                        } else {
                            info!(
                                derived_worker = %wid,
                                "Derived worker not available for CI fix, finding idle forge worker as fallback"
                            );
                            if let Some(fallback_id) = self.find_idle_forge_worker(store).await {
                                self.assign_worker_for_ci_fix(store, &fallback_id, &tid)
                                    .await
                            } else {
                                false
                            }
                        }
                    } else {
                        if let Some(fallback_id) = self.find_idle_forge_worker(store).await {
                            self.assign_worker_for_ci_fix(store, &fallback_id, &tid)
                                .await
                        } else {
                            false
                        }
                    };

                    self.remove_from_pending_prs(store, *pr_number).await;

                    if worker_reassigned {
                        self.increment_ci_fix_attempts(store, *pr_number).await;
                        any_ci_fix = true;
                    } else {
                        warn!(
                            pr_number,
                            ticket_id = %tid,
                            "No worker available for CI fix — marking ticket as failed"
                        );
                        if !failed_ticket_ids.contains(&tid) {
                            self.mark_ticket_failed(
                                store,
                                &tid,
                                &format!(
                                    "CI failed for PR #{} — no worker available for fix",
                                    pr_number
                                ),
                            )
                            .await;
                            failed_ticket_ids.push(tid);
                        }
                        any_failure = true;
                    }
                }
                VesselOutcome::MergeBlocked {
                    ticket_id,
                    pr_number,
                    reason,
                } => {
                    VesselNotifier::emit_merge_blocked(
                        store,
                        ticket_id.as_deref(),
                        *pr_number,
                        reason,
                    )
                    .await;

                    if is_merge_conflict_message(reason) {
                        warn!(
                            pr_number,
                            ticket_id = ?ticket_id,
                            "MergeBlocked reason indicates conflicts — tracking to prevent re-add loop"
                        );
                        self.increment_merge_blocked_attempts(store, *pr_number)
                            .await;
                    }

                    let tid = ticket_id
                        .clone()
                        .unwrap_or_else(|| format!("T-{}", pr_number));
                    if !failed_ticket_ids.contains(&tid) {
                        self.mark_ticket_failed(
                            store,
                            &tid,
                            &format!("Merge blocked for PR #{}: {}", pr_number, reason),
                        )
                        .await;
                        failed_ticket_ids.push(tid);
                    }
                    self.remove_from_pending_prs(store, *pr_number).await;
                    any_failure = true;
                }
                VesselOutcome::CiTimeout {
                    ticket_id,
                    pr_number,
                } => {
                    VesselNotifier::emit_ci_timeout(store, ticket_id.as_deref(), *pr_number).await;
                    let tid = ticket_id
                        .clone()
                        .unwrap_or_else(|| format!("T-{}", pr_number));

                    let current_ci_attempts = self.get_ci_fix_attempts(store, *pr_number).await;
                    info!(
                        pr_number,
                        ticket_id = %tid,
                        current_ci_attempts,
                        max = MAX_CI_FIX_ATTEMPTS,
                        "CI fix attempt counter check (CiTimeout)"
                    );

                    if current_ci_attempts >= MAX_CI_FIX_ATTEMPTS {
                        warn!(
                            pr_number,
                            ticket_id = %tid,
                            attempts = current_ci_attempts,
                            "Max CI fix attempts exceeded after timeout — marking ticket as failed"
                        );
                        if !failed_ticket_ids.contains(&tid) {
                            self.mark_ticket_failed(
                                store,
                                &tid,
                                &format!(
                                    "CI timed out for PR #{} after {} fix attempts",
                                    pr_number, current_ci_attempts
                                ),
                            )
                            .await;
                            failed_ticket_ids.push(tid);
                        }
                        self.remove_from_pending_prs(store, *pr_number).await;
                        any_failure = true;
                        continue;
                    }

                    let pr_entry = pending_prs
                        .iter()
                        .find(|p| p["number"].as_u64() == Some(*pr_number));
                    let worker_id = pr_entry.and_then(|p| {
                        let wid = p["worker_id"].as_str().unwrap_or("");
                        if !wid.is_empty() {
                            return Some(wid.to_string());
                        }
                        Self::derive_worker_id_from_branch(p["head_branch"].as_str().unwrap_or(""))
                    });
                    let head_branch = pr_entry
                        .and_then(|p| p["head_branch"].as_str().map(|s| s.to_string()))
                        .unwrap_or_else(|| {
                            ticket_id
                                .as_deref()
                                .map(|tid| format!("unknown-pair/{}", tid))
                                .unwrap_or_else(|| format!("unknown/{}", pr_number))
                        });

                    let ci_chat_dispatched = self
                        .dispatch_ci_fix_for_pr(
                            store,
                            *pr_number,
                            &tid,
                            &head_branch,
                            "CI timed out — possible stuck or flaky CI run",
                            None,
                        )
                        .await;

                    let ci_fix_md_written = if ci_chat_dispatched {
                        info!(
                            pr_number,
                            ticket_id = %tid,
                            "Dispatched /ci_fix to existing forge chat — reusing forge workspace (timeout)"
                        );
                        false
                    } else {
                        self.write_ci_fix_md(
                            &CiFixPrInfo {
                                pr_number: *pr_number,
                                head_branch: head_branch.clone(),
                                ticket_id: ticket_id.clone(),
                            },
                            "CI timed out — possible stuck or flaky CI run",
                            None,
                        )
                        .await
                    };

                    if ci_fix_md_written {
                        info!(
                            pr_number,
                            ticket_id = %tid,
                            "Wrote CI_FIX.md — routing to forge_pair for CI timeout fix"
                        );
                    }

                    let worker_reassigned = if let Some(ref wid) = worker_id {
                        if self.assign_worker_for_ci_fix(store, wid, &tid).await {
                            true
                        } else {
                            info!(
                                derived_worker = %wid,
                                "Derived worker not available for CI timeout fix, finding idle forge worker as fallback"
                            );
                            if let Some(fallback_id) = self.find_idle_forge_worker(store).await {
                                self.assign_worker_for_ci_fix(store, &fallback_id, &tid)
                                    .await
                            } else {
                                false
                            }
                        }
                    } else {
                        if let Some(fallback_id) = self.find_idle_forge_worker(store).await {
                            self.assign_worker_for_ci_fix(store, &fallback_id, &tid)
                                .await
                        } else {
                            false
                        }
                    };

                    self.remove_from_pending_prs(store, *pr_number).await;

                    if worker_reassigned {
                        self.increment_ci_fix_attempts(store, *pr_number).await;
                        any_ci_fix = true;
                    } else {
                        warn!(
                            pr_number,
                            ticket_id = %tid,
                            "No worker available for CI timeout fix — marking ticket as failed"
                        );
                        if !failed_ticket_ids.contains(&tid) {
                            self.mark_ticket_failed(
                                store,
                                &tid,
                                &format!(
                                    "CI timed out for PR #{} — no worker available for fix",
                                    pr_number
                                ),
                            )
                            .await;
                            failed_ticket_ids.push(tid);
                        }
                        any_failure = true;
                    }
                }
                VesselOutcome::CiMissing {
                    ticket_id,
                    pr_number,
                } => {
                    VesselNotifier::emit_ci_missing(store, ticket_id.as_deref(), *pr_number).await;
                    let tid = ticket_id
                        .clone()
                        .unwrap_or_else(|| format!("T-{}", pr_number));
                    VesselNotifier::emit_ticket_merged(
                        store,
                        &tid,
                        *pr_number,
                        "",
                        "Merged without CI validation",
                        None,
                    )
                    .await;
                    VesselNotifier::set_ticket_status_merged(store, &tid).await;

                    // Stop Coder workspace if this worker was using one
                    self.stop_coder_workspace_for_pr(store, &pending_prs, *pr_number)
                        .await;

                    self.update_ticket_status(store, &tid, "merged_no_ci").await;
                    self.close_github_issue(store, &tid).await;
                    self.remove_from_pending_prs(store, *pr_number).await;

                    let mut pr_val = pending_prs
                        .iter()
                        .find(|p| p["number"].as_u64() == Some(*pr_number))
                        .cloned()
                        .unwrap_or_else(|| json!({ "number": pr_number }));
                    if pr_val["ticket_id"].as_str().is_none() {
                        pr_val["ticket_id"] = json!(tid);
                    }
                    self.recycle_worker(store, &pr_val).await;

                    any_success = true;
                }
                VesselOutcome::Conflicts {
                    ticket_id,
                    pr_number,
                    conflicted_files,
                } => {
                    let tid = ticket_id
                        .clone()
                        .unwrap_or_else(|| format!("T-{}", pr_number));

                    let current_attempts = self
                        .get_conflict_resolution_attempts(store, *pr_number)
                        .await;

                    if current_attempts >= MAX_CONFLICT_RESOLUTION_ATTEMPTS {
                        warn!(
                            pr_number,
                            ticket_id = %tid,
                            attempts = current_attempts,
                            "Max conflict resolution attempts exceeded — escalating to human intervention"
                        );
                        VesselNotifier::emit_conflicts_detected(
                            store,
                            ticket_id.as_deref(),
                            *pr_number,
                            conflicted_files,
                        )
                        .await;
                        self.mark_ticket_awaiting_human(
                            store,
                            &tid,
                            &format!(
                                "Merge conflicts on PR #{} not resolved after {} attempts — requires human intervention",
                                pr_number, current_attempts
                            ),
                        )
                        .await;
                        self.remove_from_pending_prs(store, *pr_number).await;
                        any_awaiting_human = true;
                        continue;
                    }

                    VesselNotifier::emit_conflicts_detected(
                        store,
                        ticket_id.as_deref(),
                        *pr_number,
                        conflicted_files,
                    )
                    .await;

                    // Additive `/address_review` chat dispatch for conflicts.
                    // The existing CONFLICT_RESOLUTION.md file fallback remains.
                    self.dispatch_address_review_for_pr(
                        store,
                        *pr_number,
                        PrMonitorState::Conflicts,
                        &tid,
                    )
                    .await;

                    let derived_worker_id = pending_prs
                        .iter()
                        .find(|p| p["number"].as_u64() == Some(*pr_number))
                        .and_then(|pr| {
                            let wid = pr["worker_id"].as_str().unwrap_or("");
                            if !wid.is_empty() {
                                return Some(wid.to_string());
                            }
                            Self::derive_worker_id_from_branch(
                                pr["head_branch"].as_str().unwrap_or(""),
                            )
                        });

                    let worker_reassigned = if let Some(ref wid) = derived_worker_id {
                        if self
                            .assign_worker_for_conflict_rework(store, wid, &tid)
                            .await
                        {
                            true
                        } else {
                            info!(
                                derived_worker = %wid,
                                "Derived worker not available, finding idle forge worker as fallback"
                            );
                            if let Some(fallback_id) = self.find_idle_forge_worker(store).await {
                                self.assign_worker_for_conflict_rework(store, &fallback_id, &tid)
                                    .await
                            } else {
                                false
                            }
                        }
                    } else {
                        if let Some(fallback_id) = self.find_idle_forge_worker(store).await {
                            self.assign_worker_for_conflict_rework(store, &fallback_id, &tid)
                                .await
                        } else {
                            false
                        }
                    };

                    self.remove_from_pending_prs(store, *pr_number).await;

                    if worker_reassigned {
                        self.increment_conflict_resolution_attempts(store, *pr_number)
                            .await;
                        any_conflicts = true;
                    } else {
                        warn!(
                            pr_number,
                            ticket_id = %tid,
                            "No worker available for conflict rework — marking ticket as failed"
                        );
                        self.mark_ticket_failed(
                            store,
                            &tid,
                            &format!(
                                "Merge conflicts on PR #{} — no worker available for rework",
                                pr_number
                            ),
                        )
                        .await;
                        any_failure = true;
                    }
                }
                VesselOutcome::Reviews {
                    ticket_id,
                    pr_number,
                    state,
                    head_sha,
                } => {
                    let tid = ticket_id
                        .clone()
                        .unwrap_or_else(|| format!("T-{}", pr_number));
                    let current_head_sha = head_sha
                        .clone()
                        .or(self.pending_pr_head_sha(store, *pr_number).await);
                    let last_dispatched_sha = self
                        .get_address_review_dispatched_sha(store, *pr_number)
                        .await;

                    // Note: Attempts counter is preserved across automated rework commits
                    // so an unresolvable automated review loop halts at MAX_ADDRESS_REVIEW_ATTEMPTS
                    // instead of looping indefinitely. The counter is reset only upon approval
                    // or explicit human intervention.
                    let current_attempts =
                        self.get_address_review_attempts(store, *pr_number).await;

                    let monitor_state = match state.as_str() {
                        "changes_requested" => PrMonitorState::ChangesRequested,
                        "comments" => PrMonitorState::Comments,
                        "needs_review" => PrMonitorState::NeedsReview,
                        _ => PrMonitorState::Comments,
                    };

                    // ── NeedsReview: SENTINEL has not yet submitted an approve
                    // review on GitHub. This is a gating wait, NOT a rework
                    // request to FORGE. Check it BEFORE the attempt cap so a PR
                    // that is simply awaiting SENTINEL's approve review stays
                    // pending instead of being escalated to human after the
                    // rework-attempt cap is exhausted. We must not dispatch
                    // `/address_review`, must not mark the ticket failed, and
                    // must not drop the PR from pending_prs — it stays queued so
                    // the next poll re-classifies once SENTINEL's review lands.
                    if monitor_state == PrMonitorState::NeedsReview {
                        info!(
                            pr_number,
                            ticket_id = %tid,
                            "PR not yet approved by SENTINEL — keeping pending until the final review is submitted"
                        );
                        any_needs_review = true;
                        continue;
                    }

                    // Dedup guard: if we already dispatched `/address_review` for
                    // this exact PR head SHA, FORGE is still addressing it and has
                    // not pushed new changes. Re-dispatching would overwhelm the
                    // builder, so we do nothing and let it finish.
                    let already_dispatched_for_head = match current_head_sha {
                        Some(ref sha) => last_dispatched_sha.as_deref() == Some(sha.as_str()),
                        None => false,
                    };
                    if already_dispatched_for_head {
                        info!(
                            pr_number,
                            ticket_id = %tid,
                            head_sha = current_head_sha.as_deref().unwrap_or(""),
                            "Already dispatched /address_review for this head — waiting for FORGE to finish"
                        );
                        self.remove_from_pending_prs(store, *pr_number).await;
                        continue;
                    }

                    if current_attempts >= MAX_ADDRESS_REVIEW_ATTEMPTS {
                        warn!(
                            pr_number,
                            ticket_id = %tid,
                            attempts = current_attempts,
                            "Max /address_review dispatch attempts exceeded — surfacing to human"
                        );
                        self.mark_ticket_awaiting_human(
                            store,
                            &tid,
                            &format!(
                                "PR #{} in review-rework state ({}) not addressed after {} dispatch attempts",
                                pr_number, state, current_attempts
                            ),
                        )
                        .await;
                        self.remove_from_pending_prs(store, *pr_number).await;
                        any_awaiting_human = true;
                        continue;
                    }

                    let dispatched = self
                        .dispatch_address_review_for_pr(store, *pr_number, monitor_state, &tid)
                        .await;

                    if dispatched {
                        info!(
                            pr_number,
                            ticket_id = %tid,
                            state,
                            "Dispatched /address_review to forge chat"
                        );
                        let mut tickets: Vec<Ticket> =
                            store.get_typed(KEY_TICKETS).await.unwrap_or_default();
                        if let Some(ticket) = tickets.iter_mut().find(|t| t.id == tid) {
                            if !matches!(ticket.status, TicketStatus::InProgress { .. }) {
                                let slots: HashMap<String, WorkerSlot> =
                                    store.get_typed(KEY_WORKER_SLOTS).await.unwrap_or_default();
                                let worker_id = match &ticket.status {
                                    TicketStatus::Assigned { worker_id }
                                    | TicketStatus::InProgress { worker_id }
                                    | TicketStatus::Completed { worker_id, .. }
                                        if worker_id != "nexus-reconciliation"
                                            && slots.contains_key(worker_id) =>
                                    {
                                        worker_id.clone()
                                    }
                                    _ => slots
                                        .iter()
                                        .find(|(id, slot)| {
                                            Self::worker_role(id) == "forge"
                                                && matches!(
                                                    &slot.status,
                                                    WorkerStatus::Assigned { ticket_id: t, .. }
                                                        | WorkerStatus::Working { ticket_id: t, .. }
                                                        if t == &tid
                                                )
                                        })
                                        .map(|(id, _)| id.clone())
                                        .or_else(|| {
                                            pending_prs
                                                .iter()
                                                .find(|p| p["number"].as_u64() == Some(*pr_number))
                                                .and_then(|pr| {
                                                    let wid =
                                                        pr["worker_id"].as_str().unwrap_or("");
                                                    if !wid.is_empty() && slots.contains_key(wid) {
                                                        return Some(wid.to_string());
                                                    }
                                                    Self::derive_worker_id_from_branch(
                                                        pr["head_branch"].as_str().unwrap_or(""),
                                                    )
                                                    .filter(|wid| slots.contains_key(wid))
                                                })
                                        })
                                        .or_else(|| {
                                            slots.keys().find(|k| k.starts_with("forge")).cloned()
                                        })
                                        .unwrap_or_else(|| "forge-1".to_string()),
                                };
                                ticket.status = TicketStatus::InProgress { worker_id };
                                store.set(KEY_TICKETS, json!(tickets)).await;
                            }
                        }
                        if let Some(sha) = &current_head_sha {
                            self.set_address_review_dispatched_sha(store, *pr_number, sha)
                                .await;
                        }
                        let now = chrono::Utc::now().to_rfc3339();
                        self.set_address_review_dispatched_at(store, *pr_number, &now)
                            .await;
                        self.increment_address_review_attempts(store, *pr_number)
                            .await;
                        any_address_review = true;
                    } else {
                        warn!(
                            pr_number,
                            ticket_id = %tid,
                            "Could not dispatch /address_review (no forge chat or client) — persisting directive for NEXUS to provision a FORGE"
                        );
                        // No live FORGE chat to dispatch to. Persist the targeted
                        // `/address_review` directive (with the actual review
                        // feedback) so NEXUS provisions a FORGE worker whose new
                        // chat starts with it as the initial prompt. Do NOT fall
                        // through to the conflict handler here: that would tell
                        // FORGE to resolve conflict markers and drop the review
                        // feedback entirely.
                        let repository: Option<String> = store.get_typed("repository").await;
                        let (owner, repo) = parse_repository(repository.as_deref());
                        let reason = collect_rework(&self.client, owner, repo, *pr_number)
                            .await
                            .reason;
                        let directive =
                            build_directive(monitor_state, *pr_number, reason.as_deref());
                        self.persist_rework_directive(store, &tid, "forge", &directive)
                            .await;
                        if let Some(sha) = &current_head_sha {
                            self.set_address_review_dispatched_sha(store, *pr_number, sha)
                                .await;
                        }
                        let now = chrono::Utc::now().to_rfc3339();
                        self.set_address_review_dispatched_at(store, *pr_number, &now)
                            .await;
                        self.increment_address_review_attempts(store, *pr_number)
                            .await;
                        any_rework_provision = true;
                    }

                    self.remove_from_pending_prs(store, *pr_number).await;
                }
                VesselOutcome::Unmanaged { pr_number, reason } => {
                    warn!(pr_number, reason, "PR requires manual handling");
                    store
                        .set(
                            &format!("pr:{pr_number}:unmanaged"),
                            json!({"reason": reason}),
                        )
                        .await;
                    self.remove_from_pending_prs(store, *pr_number).await;
                    any_awaiting_human = true;
                }
                VesselOutcome::DocsPrClosed { pr_number, reason } => {
                    info!(
                        pr_number,
                        reason,
                        "Docs PR closed due to conflicts — lore will regenerate on next deployment"
                    );
                    self.remove_from_pending_prs(store, *pr_number).await;
                    any_success = true;
                }
            }
        }

        if any_awaiting_human {
            Ok(Action::new(Action::AWAITING_HUMAN))
        } else if any_conflicts {
            Ok(Action::new(ACTION_CONFLICTS_DETECTED))
        } else if any_rework_provision {
            // No live FORGE chat existed to dispatch `/address_review` into, so
            // route to NEXUS to provision a FORGE worker that delivers the
            // persisted rework directive as its chat initial prompt.
            Ok(Action::new(ACTION_REWORK_PROVISION_NEEDED))
        } else if any_address_review {
            Ok(Action::new(ACTION_ADDRESS_REVIEW_DISPATCHED))
        } else if any_needs_review {
            // Waiting on SENTINEL's GitHub approve review. Keep the PR pending
            // and let the controller poll again — do not treat as success/failure.
            Ok(Action::new(PAUSE_SIGNAL))
        } else if any_success {
            Ok(Action::DEPLOYED.into())
        } else if any_ci_fix {
            Ok(Action::new(ACTION_CI_FIX_NEEDED))
        } else if any_failure {
            Ok(Action::DEPLOY_FAILED.into())
        } else {
            Ok(Action::new("no_work"))
        }
    }
}

impl VesselNode {
    /// Check if any check runs exist for a commit by querying check-runs API.
    /// Returns true if total_count > 0.
    async fn process_single_pr(
        &self,
        owner: &str,
        repo: &str,
        pr_info: PrInfo,
        evidence: Option<&ReworkEvidence>,
    ) -> Result<VesselOutcome> {
        let pr_number = pr_info.number;

        let ticket_id = if Self::is_docs_pr(&pr_info) {
            Some("T-DOCS".to_string())
        } else {
            pr_info.ticket_id.clone()
        };

        info!(pr_number, ticket_id = ?ticket_id, "Processing PR");

        // Short-circuit for Docs PRs: check mergeability first to skip CI polling if conflicts exist
        if Self::is_docs_pr(&pr_info) {
            if pr_info.has_conflicts() {
                warn!(
                    pr_number,
                    "Docs PR has conflicts — short-circuiting CI poll and closing"
                );
                return self
                    .close_docs_pr_with_conflicts(owner, repo, &pr_info)
                    .await;
            }
            // Re-fetch PR to get fresh mergeability status if not yet computed
            if pr_info.mergeable.is_none() {
                let fresh_pr = self.client.get_pull_request(owner, repo, pr_number).await?;
                if fresh_pr.has_conflicts() {
                    warn!(
                        pr_number,
                        "Docs PR has conflicts (after re-fetch) — short-circuiting CI poll and closing"
                    );
                    return self
                        .close_docs_pr_with_conflicts(owner, repo, &fresh_pr)
                        .await;
                }
            }
        }

        let poll_result = self
            .poller
            .poll_until_terminal(owner, repo, &pr_info)
            .await?;

        match poll_result {
            CiPollResult::Status(CiStatus::Success) => {
                // Monitor the GitHub-native PR lifecycle: before merging, check
                // the review state. If a reviewer requested changes or left
                // unaddressed review comments, dispatch `/address_review` to the
                // responsible FORGE instead of merging. The actual chat dispatch
                // happens in post() where the SharedStore (forge chat binding) is
                // available; here we only decide and carry the rework directive.
                let monitor_state = classify(
                    &self.client,
                    owner,
                    repo,
                    &pr_info,
                    CiStatus::Success,
                    evidence,
                )
                .await
                .unwrap_or(if pr_info.has_conflicts() {
                    PrMonitorState::Conflicts
                } else {
                    PrMonitorState::NeedsReview
                });

                if monitor_state == PrMonitorState::Conflicts || pr_info.has_conflicts() {
                    warn!(
                        pr_number,
                        "PR has conflicts after CI success — routing to conflict handler"
                    );
                    return self.handle_conflicts(owner, repo, pr_info).await;
                }

                // SENTINEL has not yet approved the PR on GitHub. Do not merge
                // and do not ask FORGE to rework — keep the PR pending so the
                // cycle waits for the final review to land.
                if monitor_state == PrMonitorState::NeedsReview {
                    debug!(
                        pr_number,
                        "PR not yet approved by SENTINEL — keeping pending for final review"
                    );
                    return Ok(VesselOutcome::Reviews {
                        ticket_id,
                        pr_number,
                        state: PrMonitorState::NeedsReview.as_str().to_string(),
                        head_sha: Some(pr_info.head_sha.clone()),
                    });
                }

                if matches!(
                    monitor_state,
                    PrMonitorState::ChangesRequested | PrMonitorState::Comments
                ) {
                    let directive = collect_rework(&self.client, owner, repo, pr_number).await;
                    warn!(
                        pr_number,
                        state = monitor_state.as_str(),
                        reason = directive.reason.as_deref().unwrap_or(""),
                        "PR in review-rework state — dispatching /address_review to FORGE"
                    );
                    return Ok(VesselOutcome::Reviews {
                        ticket_id,
                        pr_number,
                        state: monitor_state.as_str().to_string(),
                        head_sha: Some(pr_info.head_sha.clone()),
                    });
                }

                // Verify lifecycle merge readiness before attempting merge.
                // A PR might be approved on GitHub but not yet merge_ready in
                // the lifecycle store. Keep it pending rather than failing with MergeBlocked.
                let store = self.lifecycle_store.lock().unwrap().clone();
                if let Some(store) = store {
                    if let Some(ref tid) = ticket_id {
                        if let Ok(state) = store.lifecycle(tid).await {
                            if !state.merge_ready(&pr_info.head_sha) {
                                info!(
                                    pr_number,
                                    ticket = %tid,
                                    "PR approved on GitHub but waiting for lifecycle approvals; keeping pending"
                                );
                                return Ok(VesselOutcome::Reviews {
                                    ticket_id: ticket_id.clone(),
                                    pr_number,
                                    state: PrMonitorState::NeedsReview.as_str().to_string(),
                                    head_sha: Some(pr_info.head_sha.clone()),
                                });
                            }
                        }
                    }
                }

                match self.merge_reviewed(owner, repo, &pr_info).await {
                    Ok(result) if result.merged => Ok(VesselOutcome::Merged {
                        ticket_id: ticket_id.unwrap_or_else(|| format!("T-{}", pr_number)),
                        pr_number,
                        sha: result.sha.unwrap_or_default(),
                        pr_title: pr_info.title,
                        pr_body: pr_info.body,
                    }),
                    Ok(result) if is_merge_conflict_message(&result.message) => {
                        warn!(
                            pr_number,
                            message = %result.message,
                            "Merge blocked by conflicts — routing to conflict handler"
                        );
                        self.handle_conflicts(owner, repo, pr_info).await
                    }
                    Ok(result) => Ok(VesselOutcome::MergeBlocked {
                        ticket_id,
                        pr_number,
                        reason: result.message,
                    }),
                    Err(e) if is_merge_conflict_message(&e.to_string()) => {
                        warn!(
                            pr_number,
                            error = %e,
                            "Merge API error indicates conflicts — routing to conflict handler"
                        );
                        self.handle_conflicts(owner, repo, pr_info).await
                    }
                    Err(e) => Ok(VesselOutcome::MergeBlocked {
                        ticket_id,
                        pr_number,
                        reason: e.to_string(),
                    }),
                }
            }
            CiPollResult::Status(status) => {
                let detail_result = self
                    .poller
                    .client()
                    .get_failed_checks_detail_structured(owner, repo, &pr_info.head_sha)
                    .await;

                let (reason, failure_detail) = match detail_result {
                    Ok(detail) => {
                        let reason = if detail.failed_checks.is_empty() {
                            format!("CI status: {:?}", status)
                        } else {
                            let check_names: Vec<&str> = detail
                                .failed_checks
                                .iter()
                                .map(|c| c.name.as_str())
                                .collect();
                            format!(
                                "CI status: {:?} — failed checks: {}",
                                status,
                                check_names.join(", ")
                            )
                        };
                        (reason, Some(detail))
                    }
                    Err(e) => {
                        warn!(error = %e, "Failed to get detailed CI failure info — using basic reason");
                        (format!("CI status: {:?}", status), None)
                    }
                };
                Ok(VesselOutcome::CiFailed {
                    ticket_id,
                    pr_number,
                    reason,
                    failure_detail,
                })
            }
            CiPollResult::Conflicts => {
                warn!(
                    pr_number,
                    "Merge conflicts detected during CI poll — attempting resolution"
                );
                self.handle_conflicts(owner, repo, pr_info).await
            }
            CiPollResult::Timeout => Ok(VesselOutcome::CiTimeout {
                ticket_id,
                pr_number,
            }),
        }
    }

    async fn handle_conflicts(
        &self,
        owner: &str,
        repo: &str,
        pr_info: PrInfo,
    ) -> Result<VesselOutcome> {
        let pr_number = pr_info.number;

        if Self::is_docs_pr(&pr_info) {
            info!(
                pr_number,
                branch = %pr_info.head_branch,
                "Docs PR has merge conflicts — closing to allow lore to regenerate"
            );
            return self
                .close_docs_pr_with_conflicts(owner, repo, &pr_info)
                .await;
        }

        let ticket_id = pr_info.ticket_id.clone();
        // Release the frozen candidate before touching its worktree. A merge with
        // an unknown outcome must remain reserved until GitHub confirms it.
        let store = self
            .lifecycle_store
            .lock()
            .unwrap()
            .clone()
            .context("Missing lifecycle store for conflict rework")?;
        let ticket = ticket_id.as_deref().context("Conflict PR has no ticket")?;
        let current = store.lifecycle(ticket).await?;
        anyhow::ensure!(!current.merge_pending, "Merge outcome is still unresolved");
        anyhow::ensure!(
            current.phase == config::lifecycle::Phase::Submit,
            "Conflict rework requires a submitted candidate"
        );
        store
            .transition(
                ticket,
                current.version,
                "vessel",
                config::lifecycle::Event::Move {
                    phase: config::lifecycle::Phase::Building,
                    head: None,
                },
            )
            .await?;
        let worktree_path = self.resolve_worktree_path(&pr_info);

        let conflicted_files = match &worktree_path {
            Some(wt) if wt.exists() => {
                self.merge_origin_main_in_worktree(wt, &pr_info.head_branch)
                    .await
            }
            Some(wt) => {
                warn!(
                    path = %wt.display(),
                    pr_number,
                    "Worktree path resolved but directory missing — falling back to GitHub API"
                );
                self.fetch_conflicted_files_from_github(owner, repo, &pr_info)
                    .await
            }
            None => {
                warn!(
                    pr_number,
                    "No worktree path — cannot merge origin/main locally"
                );
                self.fetch_conflicted_files_from_github(owner, repo, &pr_info)
                    .await
            }
        };

        if let Some(ref wt) = worktree_path {
            let _ = ConflictResolver::abort_rebase(wt).await;
        }

        let resolution_md_written = self
            .write_conflict_resolution_md(&pr_info, &conflicted_files, None)
            .await;

        if resolution_md_written {
            info!(
                pr_number,
                files = conflicted_files.len(),
                "Wrote CONFLICT_RESOLUTION.md — routing to forge_pair for conflict rework"
            );
        } else {
            warn!(
                pr_number,
                files = conflicted_files.len(),
                "CONFLICT_RESOLUTION.md NOT written (workspace root unavailable) — conflict rework may be incomplete"
            );
        }

        Ok(VesselOutcome::Conflicts {
            ticket_id,
            pr_number,
            conflicted_files,
        })
    }

    async fn merge_origin_main_in_worktree(
        &self,
        worktree_path: &PathBuf,
        branch: &str,
    ) -> Vec<String> {
        let _ = ConflictResolver::abort_rebase(worktree_path).await;

        // Detect the default branch instead of hardcoding "main"
        let default_branch = Self::detect_default_branch(worktree_path);
        let origin_ref = format!("origin/{}", default_branch);

        let fetch = tokio::process::Command::new("git")
            .args(["fetch", "origin", &default_branch])
            .current_dir(worktree_path)
            .output()
            .await;

        match fetch {
            Ok(output) if output.status.success() => {}
            Ok(output) => {
                let stderr = String::from_utf8_lossy(&output.stderr);
                warn!(branch, %stderr, "git fetch {} failed in worktree", origin_ref);
                return vec!["unknown — fetch failed".to_string()];
            }
            Err(e) => {
                warn!(branch, error = %e, "git fetch {} failed in worktree", origin_ref);
                return vec!["unknown — fetch failed".to_string()];
            }
        }

        let merge = tokio::process::Command::new("git")
            .args(["merge", &origin_ref, "--no-edit"])
            .current_dir(worktree_path)
            .output()
            .await;

        match merge {
            Ok(output) if output.status.success() => {
                info!(branch, "{} merged cleanly — no conflicts", origin_ref);
                vec![]
            }
            Ok(output) => {
                let stderr = String::from_utf8_lossy(&output.stderr);
                if stderr.contains("refusing to merge unrelated histories") {
                    warn!(
                        branch,
                        "Unrelated histories — retrying with --allow-unrelated-histories"
                    );
                    let retry = tokio::process::Command::new("git")
                        .args([
                            "merge",
                            &origin_ref,
                            "--no-edit",
                            "--allow-unrelated-histories",
                        ])
                        .current_dir(worktree_path)
                        .output()
                        .await;

                    return match retry {
                        Ok(o) if o.status.success() => {
                            info!(
                                branch,
                                "{} merged cleanly with --allow-unrelated-histories", origin_ref
                            );
                            vec![]
                        }
                        Ok(_) => {
                            let files = self.list_conflicted_files(worktree_path).await;
                            info!(
                                branch,
                                files = files.len(),
                                "Merge with --allow-unrelated-histories produced conflict markers"
                            );
                            files
                        }
                        Err(e) => {
                            warn!(branch, error = %e, "git merge --allow-unrelated-histories failed");
                            vec!["unknown — merge failed".to_string()]
                        }
                    };
                }
                let files = self.list_conflicted_files(worktree_path).await;
                info!(
                    branch,
                    files = files.len(),
                    "Merge produced conflict markers in worktree"
                );
                files
            }
            Err(e) => {
                warn!(branch, error = %e, "git merge {} failed", origin_ref);
                vec!["unknown — merge failed".to_string()]
            }
        }
    }

    /// Detect the repository's default branch by reading origin/HEAD symref,
    /// falling back to checking remote refs, then defaulting to "main".
    fn detect_default_branch(project_root: &Path) -> String {
        // Method 1: Read origin/HEAD symref (most reliable)
        let output = std::process::Command::new("git")
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
            let output = std::process::Command::new("git")
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

    async fn list_conflicted_files(&self, worktree_path: &PathBuf) -> Vec<String> {
        let output = tokio::process::Command::new("git")
            .args(["diff", "--name-only", "--diff-filter=U"])
            .current_dir(worktree_path)
            .output()
            .await;

        match output {
            Ok(o) => {
                let stdout = String::from_utf8_lossy(&o.stdout);
                stdout
                    .lines()
                    .map(|l| l.trim().to_string())
                    .filter(|l| !l.is_empty())
                    .collect()
            }
            Err(_) => vec![],
        }
    }

    async fn fetch_conflicted_files_from_github(
        &self,
        owner: &str,
        repo: &str,
        pr_info: &PrInfo,
    ) -> Vec<String> {
        match self
            .client
            .list_conflicted_files(owner, repo, pr_info.number)
            .await
        {
            Ok(files) => files,
            Err(e) => {
                warn!(pr = pr_info.number, error = %e, "Failed to fetch conflicted files from GitHub");
                vec!["unknown — worktree not available, GitHub API failed".to_string()]
            }
        }
    }

    async fn write_conflict_resolution_md(
        &self,
        pr_info: &PrInfo,
        conflicted_files: &[String],
        fallback_ticket_id: Option<&str>,
    ) -> bool {
        let workspace_root = match config::AgentConfig::init_from_env()
            .ok()
            .and_then(|agent| agent.effective_workspace_root())
        {
            Some(root) => root,
            None => {
                warn!("AGENTFLOW_WORKSPACE_ROOT not set — cannot write CONFLICT_RESOLUTION.md");
                return false;
            }
        };

        let branch = &pr_info.head_branch;
        let parts: Vec<&str> = branch.splitn(2, '/').collect();
        if parts.len() != 2 {
            warn!(
                branch,
                "Cannot parse branch for pair_id — skipping CONFLICT_RESOLUTION.md"
            );
            return false;
        }
        let pair_id = parts[0];

        let _ticket_id = pr_info.ticket_id.clone().unwrap_or_else(|| {
            if let Some(fb) = fallback_ticket_id {
                info!(
                    branch,
                    fallback_ticket_id = fb,
                    "Using fallback ticket_id for CONFLICT_RESOLUTION.md"
                );
                fb.to_string()
            } else {
                let synthetic = format!("T-{}", pr_info.number);
                info!(
                    branch,
                    synthetic_ticket_id = %synthetic,
                    "Using synthetic ticket_id for CONFLICT_RESOLUTION.md"
                );
                synthetic
            }
        });

        let shared_dir = PathBuf::from(&workspace_root)
            .join("worktrees")
            .join(pair_id)
            .join(".pair-shared");

        if !shared_dir.exists() {
            if let Err(e) = tokio::fs::create_dir_all(&shared_dir).await {
                warn!(
                    path = %shared_dir.display(),
                    error = %e,
                    "Failed to create shared directory for CONFLICT_RESOLUTION.md"
                );
                return false;
            }
            info!(path = %shared_dir.display(), "Created shared directory for CONFLICT_RESOLUTION.md");
        }

        let files_list = if conflicted_files.is_empty() {
            "No specific conflicted files detected — resolve all conflict markers.".to_string()
        } else {
            conflicted_files
                .iter()
                .map(|f| format!("- {}", f))
                .collect::<Vec<_>>()
                .join("\n")
        };

        let content = format!(
             "# Conflict Resolution Required\n\n\
              VESSEL detected merge conflicts between your branch and the default branch.\n\
              `git merge origin/<default>` has been run in your worktree — conflict markers are present.\n\n\
             ## Instructions\n\n\
             1. Open each conflicted file listed below\n\
             2. Resolve all conflict markers (`<<<<<<<`, `=======`, `>>>>>>>`)\n\
             3. Choose the correct integration of both sides — do NOT just pick one\n\
             4. Stage the resolved files: `git add -A`\n\
             5. Commit: `git commit -m \"resolve merge conflicts\"`\n\
             6. Push: `git push`\n\
             7. Write STATUS.json with `\"status\": \"PR_OPENED\"` and your PR number\n\n\
             ## Conflicted Files\n\n{}\n\n\
             ## Important\n\n\
             - Do NOT abort the merge — the conflict markers are there for you to resolve\n\
             - Resolve ALL conflict markers before committing\n\
             - After you push, VESSEL will re-monitor CI automatically",
            files_list,
        );

        let path = shared_dir.join("CONFLICT_RESOLUTION.md");
        if let Err(e) = tokio::fs::write(&path, &content).await {
            warn!(path = %path.display(), error = %e, "Failed to write CONFLICT_RESOLUTION.md");
            false
        } else {
            info!(path = %path.display(), "Wrote CONFLICT_RESOLUTION.md for forge conflict rework");
            true
        }
    }

    /// Checks whether an unapproved PR requires rework (merge conflicts, failing CI,
    /// changes requested, or unaddressed review comments) before gating on `merge_ready`.
    async fn check_pr_needs_rework(&self, owner: &str, repo: &str, pr_info: &PrInfo) -> bool {
        // 1. Merge conflicts
        if pr_info.has_conflicts() || pr_info.mergeable == Some(false) {
            return true;
        }

        // 2. Failing CI
        if let Ok(ci_status) = self
            .client
            .get_ci_status(owner, repo, &pr_info.head_sha)
            .await
        {
            if matches!(ci_status, CiStatus::Failure | CiStatus::Error) {
                return true;
            }
        }

        // 3. Reviews: changes requested
        let reviews = self
            .client
            .list_pr_reviews(owner, repo, pr_info.number)
            .await
            .unwrap_or_default();
        let review_state = github::effective_review_state(&reviews);
        if review_state == github::PrReviewState::ChangesRequested {
            return true;
        }

        // 4. Inline review comments (only considered rework if PR is not approved on current head
        // by an authorized reviewer — stale approvals or outside drive-by approvals do not hide feedback)
        let is_authorized_head_approved = review_state == github::PrReviewState::Approved
            && github::latest_review_per_user(&reviews)
                .into_iter()
                .any(|r| {
                    r.state_enum() == github::PrReviewState::Approved
                        && r.is_authorized_reviewer()
                        && (r.commit_id.as_deref() == Some(&pr_info.head_sha)
                            || r.commit_id.is_none())
                });

        if !is_authorized_head_approved {
            let comments = self
                .client
                .list_review_comments(owner, repo, pr_info.number)
                .await
                .unwrap_or_default();
            if !comments.is_empty() {
                return true;
            }
        }

        false
    }

    /// Reconcile a human's GitHub approval into the lifecycle `pr_human` record.
    ///
    /// SENTINEL shares the PR author's GitHub identity, and GitHub blocks
    /// self-review, so an `APPROVED` review can only originate from a distinct
    /// account. That approval satisfies the `pr_human` merge gate without a
    /// manual operator CLI entry, provided that:
    /// - no reviewer currently requests changes (fail closed),
    /// - the approving reviewer is an `OWNER`, `MEMBER`, or `COLLABORATOR`, and
    /// - their latest review is `APPROVED` on exactly `head_sha`.
    ///
    /// Returns true if the lifecycle was advanced. No-op (false) when the ticket
    /// is not in `submit`, `pr_human` is already present, the PR has no
    /// qualifying approval, or reviews could not be fetched (the PR is simply
    /// deferred to the next pass so other queued PRs keep processing).
    #[allow(clippy::too_many_arguments)]
    async fn reconcile_github_approval(
        &self,
        store: &SharedStore,
        ticket: &str,
        state: &config::lifecycle::Lifecycle,
        owner: &str,
        repo: &str,
        pr_number: u64,
        head_sha: &str,
    ) -> Result<bool> {
        if state.phase != config::lifecycle::Phase::Submit
            || (state.pr_human.is_some() && state.pr_decision.is_some())
        {
            return Ok(false);
        }
        let reviews = match self.client.list_pr_reviews(owner, repo, pr_number).await {
            Ok(reviews) => reviews,
            Err(e) => {
                warn!(pr_number, ticket, error = %e,
                    "Failed to list PR reviews; deferring approval reconciliation");
                return Ok(false);
            }
        };
        // Any reviewer currently requesting changes vetoes reconciliation.
        if github::effective_review_state(&reviews) != github::PrReviewState::Approved {
            return Ok(false);
        }
        // The effective approver: an authorized reviewer whose *latest* review
        // approves the current head. Stale approvals of older commits and
        // drive-by approvals from outside accounts do not count.
        let Some(approver) = github::latest_review_per_user(&reviews)
            .into_iter()
            .filter(|r| {
                r.state_enum() == github::PrReviewState::Approved
                    && r.is_authorized_reviewer()
                    && r.commit_id.as_deref() == Some(head_sha)
            })
            .max_by(|a, b| a.submitted_at.cmp(&b.submitted_at))
            .and_then(|r| r.user.clone())
        else {
            info!(
                pr_number,
                ticket,
                head = head_sha,
                "No authorized GitHub approval of the current head; not reconciling"
            );
            return Ok(false);
        };
        let report = format!("Approved on GitHub by {approver} at {head_sha}");

        let current = store.lifecycle(ticket).await?;
        if (current.pr_human.is_some() && current.pr_decision.is_some())
            || current.head.as_deref() != Some(head_sha)
        {
            return Ok(false);
        }
        // Only reconcile if Sentinel already verified the candidate in Testing phase.
        // If the testing gate has not passed, do not reconcile as Sentinel's automated test
        // verification cannot be bypassed.
        if current.pr_decision.is_none()
            && !current.test_decision.as_ref().is_some_and(|t| t.approved)
        {
            warn!(
                pr_number,
                ticket,
                "Cannot reconcile GitHub approval into pr_decision: Sentinel test gate has not passed"
            );
            return Ok(false);
        }

        // Record the human approval, satisfy pr_decision if not yet recorded,
        // then complete the review-delivery handshake so `merge_ready()` passes.
        let mut cur = current.clone();
        if cur.pr_human.is_none() {
            cur = store
                .transition(
                    ticket,
                    cur.version,
                    "human",
                    config::lifecycle::Event::Decide {
                        round: cur.review_round,
                        phase: config::lifecycle::Phase::Submit,
                        approved: true,
                        report,
                        revision: cur.revision,
                        head: cur.head.clone(),
                    },
                )
                .await?;
        }
        if cur.pr_decision.is_none() {
            let sentinel_report =
                format!("Satisfied by verified testing review and GitHub approval by {approver}");
            cur = store
                .transition(
                    ticket,
                    cur.version,
                    "sentinel",
                    config::lifecycle::Event::Decide {
                        round: cur.review_round,
                        phase: config::lifecycle::Phase::Submit,
                        approved: true,
                        report: sentinel_report,
                        revision: cur.revision,
                        head: cur.head.clone(),
                    },
                )
                .await?;
        }
        if cur.pr_delivery.is_some() {
            store
                .transition(
                    ticket,
                    cur.version,
                    "sentinel",
                    config::lifecycle::Event::ReviewDelivered {
                        round: cur.review_round,
                    },
                )
                .await?;
        }
        self.reset_address_review_attempts(store, pr_number).await;
        self.clear_address_review_dispatched(store, pr_number).await;

        let mut tickets: Vec<Ticket> = store.get_typed(KEY_TICKETS).await.unwrap_or_default();
        let mut modified = false;
        for t in tickets.iter_mut() {
            if t.id == ticket && matches!(t.status, TicketStatus::AwaitingHuman { .. }) {
                info!(
                    ticket_id = ticket,
                    pr_number,
                    "Clearing AwaitingHuman status on ticket following authorized GitHub approval"
                );
                t.status = TicketStatus::InProgress {
                    worker_id: "vessel".to_string(),
                };
                modified = true;
            }
        }
        if modified {
            store.set(KEY_TICKETS, json!(tickets)).await;
        }

        info!(
            pr_number,
            ticket,
            approver = %approver,
            "Reconciled GitHub approval into lifecycle (pr_human and pr_decision)"
        );
        Ok(true)
    }

    /// Reserve and merge only the reviewed candidate with successful current-head CI.
    async fn merge_reviewed(
        &self,
        owner: &str,
        repo: &str,
        pr: &pocketflow_core::PrInfo,
    ) -> Result<pocketflow_core::MergeResult> {
        let store = self
            .lifecycle_store
            .lock()
            .unwrap()
            .clone()
            .context("Missing lifecycle store")?;
        let ticket = pr.ticket_id.as_deref().context("PR has no ticket")?;
        let state = store.lifecycle(ticket).await?;
        anyhow::ensure!(
            state.merge_ready(&pr.head_sha) && state.pr_number == Some(pr.number),
            "Current head lacks lifecycle approval"
        );
        // Empty check sets and timeouts are never proof of success.
        anyhow::ensure!(
            self.client.get_ci_status(owner, repo, &pr.head_sha).await?
                == pocketflow_core::CiStatus::Success,
            "CI must succeed for the candidate head"
        );
        let state = if state.merge_pending {
            state
        } else {
            store
                .transition(
                    ticket,
                    state.version,
                    "vessel",
                    config::lifecycle::Event::BeginMerge {
                        head: pr.head_sha.clone(),
                    },
                )
                .await?
        };
        let result = self.merger.merge(owner, repo, pr).await;
        match &result {
            Ok(result) if result.merged => {
                store
                    .transition(
                        ticket,
                        state.version,
                        "vessel",
                        config::lifecycle::Event::Merged {
                            number: pr.number,
                            head: pr.head_sha.clone(),
                            sha: result.sha.clone().context("Merge response missing SHA")?,
                        },
                    )
                    .await?;
            }
            Ok(_) => {
                store
                    .transition(
                        ticket,
                        state.version,
                        "vessel",
                        config::lifecycle::Event::CancelMerge,
                    )
                    .await?;
            }
            Err(_) => { /* Outcome may be unknown: preserve reservation for reconciliation. */ }
        }
        result
    }

    /// Update ticket status in SharedStore.
    async fn update_ticket_status(&self, store: &SharedStore, ticket_id: &str, status: &str) {
        let mut tickets: Vec<Value> = store.get_typed("tickets").await.unwrap_or_default();

        for ticket in tickets.iter_mut() {
            if ticket["id"].as_str() == Some(ticket_id) {
                let worker = ticket["status"]["worker_id"]
                    .as_str()
                    .unwrap_or("vessel")
                    .to_owned();
                let lifecycle = store.lifecycle(ticket_id).await.ok();
                ticket["status"] = json!({ "type": if status=="merged_no_ci" {"merged"} else {status}, "worker_id":worker, "pr_number":lifecycle.and_then(|s|s.pr_number).unwrap_or(0) });
                break;
            }
        }

        store.set(KEY_TICKETS, json!(tickets)).await;
    }

    /// Close the corresponding GitHub issue after a successful merge.
    /// Extracts the issue number from the ticket_id format `T-{issue_number:03}`.
    async fn close_github_issue(&self, store: &SharedStore, ticket_id: &str) {
        let issue_number: u64 = match ticket_id.strip_prefix("T-").and_then(|n| n.parse().ok()) {
            Some(n) => n,
            None => {
                warn!(
                    ticket_id,
                    "Cannot extract GitHub issue number from ticket_id — skipping issue close"
                );
                return;
            }
        };

        let repository: Option<String> = store.get_typed("repository").await;
        let (owner, repo) = parse_repository(repository.as_deref());

        if owner.is_empty() || repo.is_empty() {
            warn!(
                ticket_id,
                "Repository info missing — cannot close GitHub issue"
            );
            return;
        }

        match self.client.close_issue(owner, repo, issue_number).await {
            Ok(()) => info!(ticket_id, issue_number, "GitHub issue closed after merge"),
            Err(e) => {
                warn!(ticket_id, issue_number, error = %e, "Failed to close GitHub issue — merge still succeeded")
            }
        }
    }

    async fn mark_ticket_failed(&self, store: &SharedStore, ticket_id: &str, reason: &str) {
        let mut tickets: Vec<Ticket> = store.get_typed(KEY_TICKETS).await.unwrap_or_default();

        for ticket in tickets.iter_mut() {
            if ticket.id == ticket_id {
                let attempts = ticket.attempts + 1;
                ticket.attempts = attempts;
                ticket.status = TicketStatus::Failed {
                    worker_id: String::from("vessel"),
                    reason: reason.to_string(),
                    attempts,
                };
                break;
            }
        }

        store.set(KEY_TICKETS, json!(tickets)).await;
    }

    async fn workspace_link_for_worker(
        &self,
        store: &SharedStore,
        worker_id: Option<&str>,
    ) -> String {
        let Some(worker_id) = worker_id else {
            return String::new();
        };

        let slots: HashMap<String, WorkerSlot> =
            store.get_typed(KEY_WORKER_SLOTS).await.unwrap_or_default();
        let Some(slot) = slots.get(worker_id) else {
            return String::new();
        };
        let Some(workspace_id) = slot.workspace_id.as_deref() else {
            return String::new();
        };

        let coder_url: Option<String> = store.get_typed("coder_url").await;
        let Some(coder_url) = coder_url else {
            return String::new();
        };

        format!(
            "{}/workspaces/{}",
            coder_url.trim_end_matches('/'),
            workspace_id
        )
    }

    async fn notify_awaiting_human(
        &self,
        store: &SharedStore,
        ticket_id: &str,
        worker_id: Option<&str>,
        reason: &str,
        github_link: Option<String>,
    ) {
        let service = NotificationService::from_env();
        let role = worker_id.map(Self::worker_role).unwrap_or("vessel");
        let workspace_link = self.workspace_link_for_worker(store, worker_id).await;
        let msg = NotificationMessage {
            ticket_id: ticket_id.to_string(),
            role: role.to_string(),
            reason: reason.to_string(),
            workspace_link,
            github_link: github_link.unwrap_or_default(),
        };
        service.notify(&msg).await;
    }

    async fn mark_ticket_awaiting_human(&self, store: &SharedStore, ticket_id: &str, reason: &str) {
        let mut tickets: Vec<Ticket> = store.get_typed(KEY_TICKETS).await.unwrap_or_default();
        let mut worker_id_for_notification: Option<String> = None;
        let mut github_link: Option<String> = None;

        for ticket in tickets.iter_mut() {
            if ticket.id == ticket_id {
                worker_id_for_notification = match &ticket.status {
                    TicketStatus::Assigned { worker_id }
                    | TicketStatus::InProgress { worker_id }
                    | TicketStatus::Merged { worker_id, .. }
                    | TicketStatus::Failed { worker_id, .. }
                    | TicketStatus::Completed { worker_id, .. }
                    | TicketStatus::Exhausted { worker_id, .. }
                    | TicketStatus::AwaitingHuman { worker_id, .. } => Some(worker_id.clone()),
                    TicketStatus::Open => None,
                };
                github_link = ticket.issue_url.clone();
                let attempts = ticket.attempts + 1;
                ticket.attempts = attempts;
                ticket.status = TicketStatus::AwaitingHuman {
                    worker_id: worker_id_for_notification
                        .clone()
                        .unwrap_or_else(|| String::from("vessel")),
                    reason: reason.to_string(),
                    attempts,
                };
                break;
            }
        }

        store.set(KEY_TICKETS, json!(tickets)).await;
        self.notify_awaiting_human(
            store,
            ticket_id,
            worker_id_for_notification.as_deref(),
            reason,
            github_link,
        )
        .await;
    }

    /// Remove PR from pending_prs list.
    async fn remove_from_pending_prs(&self, store: &SharedStore, pr_number: u64) {
        let mut pending: Vec<Value> = store.get_typed("pending_prs").await.unwrap_or_default();
        pending.retain(|pr| pr["number"].as_u64() != Some(pr_number));
        store.set("pending_prs", json!(pending)).await;
    }

    /// Increment the conflict_resolution_attempts counter for a PR in pending_prs.
    ///
    /// When the PR is removed from pending_prs during conflict routing and later
    /// re-added by the forge_pair node, the counter must be preserved. We store
    /// it in a separate key to survive the pending_prs removal/re-add cycle.
    async fn increment_conflict_resolution_attempts(&self, store: &SharedStore, pr_number: u64) {
        let key = format!("_conflict_attempts_{}", pr_number);
        let current: u32 = store.get_typed::<u32>(&key).await.unwrap_or(0);
        let next = current + 1;
        info!(
            pr_number,
            attempts = next,
            max = MAX_CONFLICT_RESOLUTION_ATTEMPTS,
            "Incremented conflict resolution attempt counter"
        );
        store.set(&key, json!(next)).await;
    }

    /// Get the current conflict resolution attempt count for a PR.
    async fn get_conflict_resolution_attempts(&self, store: &SharedStore, pr_number: u64) -> u32 {
        let key = format!("_conflict_attempts_{}", pr_number);
        store.get_typed::<u32>(&key).await.unwrap_or(0)
    }

    async fn increment_merge_blocked_attempts(&self, store: &SharedStore, pr_number: u64) {
        let key = format!("_merge_blocked_{}", pr_number);
        let current: u32 = store.get_typed::<u32>(&key).await.unwrap_or(0);
        let next = current + 1;
        info!(
            pr_number,
            attempts = next,
            "Incremented merge blocked attempt counter"
        );
        store.set(&key, json!(next)).await;
    }

    fn is_docs_pr(pr_info: &PrInfo) -> bool {
        pr_info.head_branch.starts_with("lore/") || pr_info.ticket_id.as_deref() == Some("T-DOCS")
    }

    /// Close a docs PR that has conflicts, allowing lore to regenerate.
    async fn close_docs_pr_with_conflicts(
        &self,
        owner: &str,
        repo: &str,
        pr_info: &PrInfo,
    ) -> Result<VesselOutcome> {
        let pr_number = pr_info.number;
        let comment = "This documentation PR has merge conflicts with the main branch. \
                       Closing to allow the lore agent to regenerate the documentation. \
                       Lore will create a fresh docs PR on the next deployment cycle.";

        match self
            .client
            .close_pull_request(owner, repo, pr_number, Some(comment))
            .await
        {
            Ok(()) => {
                info!(pr_number, "Closed conflicting docs PR");
                Ok(VesselOutcome::DocsPrClosed {
                    pr_number,
                    reason: "Merge conflicts on docs PR — closed for regeneration".to_string(),
                })
            }
            Err(e) => {
                warn!(pr_number, error = %e, "Failed to close docs PR with conflicts");
                Ok(VesselOutcome::Conflicts {
                    ticket_id: None,
                    pr_number,
                    conflicted_files: vec!["Docs PR could not be closed".to_string()],
                })
            }
        }
    }

    /// Recycle workers (both FORGE and paired SENTINEL) back to Idle after a PR is merged.
    async fn recycle_worker(&self, store: &SharedStore, pr: &Value) {
        let target_ticket = pr["ticket_id"].as_str();
        let target_worker = Self::resolve_worker_id_from_pr(pr);

        if target_worker.is_none() && target_ticket.is_none() {
            return;
        }

        // Fetch slots and recycle matching workers to Idle
        let mut slots: HashMap<String, WorkerSlot> =
            store.get_typed(KEY_WORKER_SLOTS).await.unwrap_or_default();
        let mut changed = false;

        for (worker_id, slot) in slots.iter_mut() {
            if slot.status.ticket_id().is_some() && slot.status.ticket_id() != target_ticket {
                continue;
            }
            let matches_worker = target_worker.as_deref() == Some(worker_id.as_str());
            let matches_ticket =
                target_ticket.is_some() && slot.status.ticket_id() == target_ticket;
            if matches_ticket || (matches_worker && slot.status.ticket_id().is_none()) {
                match &slot.status {
                    WorkerStatus::Done { .. }
                    | WorkerStatus::Assigned { .. }
                    | WorkerStatus::Working { .. } => {
                        info!(
                            worker_id = slot.id,
                            old_status = ?slot.status,
                            "Recycling worker to Idle after merge"
                        );
                        slot.status = WorkerStatus::Idle;
                        slot.workspace_id = None;
                        changed = true;
                    }
                    _ => {
                        if slot.workspace_id.is_some() {
                            slot.workspace_id = None;
                            changed = true;
                        }
                    }
                }
            }
        }

        if changed {
            store.set(KEY_WORKER_SLOTS, json!(slots)).await;
        }
    }

    fn derive_worker_id_from_branch(head_branch: &str) -> Option<String> {
        let parts: Vec<&str> = head_branch.splitn(2, '/').collect();
        if parts.len() == 2 {
            Some(parts[0].to_string())
        } else {
            None
        }
    }

    async fn find_idle_forge_worker(&self, store: &SharedStore) -> Option<String> {
        let slots: HashMap<String, WorkerSlot> =
            store.get_typed(KEY_WORKER_SLOTS).await.unwrap_or_default();

        let mut forge_slots: Vec<_> = slots
            .iter()
            .filter(|(id, _)| id.starts_with("forge-"))
            .collect();
        forge_slots.sort_by_key(|(id, _)| id.as_str());

        for (id, slot) in forge_slots {
            if matches!(slot.status, WorkerStatus::Idle | WorkerStatus::Done { .. }) {
                return Some(id.clone());
            }
        }
        None
    }

    async fn assign_worker_for_conflict_rework(
        &self,
        store: &SharedStore,
        worker_id: &str,
        ticket_id: &str,
    ) -> bool {
        let mut slots: HashMap<String, WorkerSlot> =
            store.get_typed(KEY_WORKER_SLOTS).await.unwrap_or_default();

        if let Some(slot) = slots.get_mut(worker_id) {
            let issue_url = match &slot.status {
                WorkerStatus::Done { ticket_id: tid, .. } => {
                    let tickets: Vec<Ticket> =
                        store.get_typed(KEY_TICKETS).await.unwrap_or_default();
                    tickets
                        .iter()
                        .find(|t| t.id == *tid)
                        .and_then(|t| t.issue_url.clone())
                }
                WorkerStatus::Idle => {
                    let tickets: Vec<Ticket> =
                        store.get_typed(KEY_TICKETS).await.unwrap_or_default();
                    tickets
                        .iter()
                        .find(|t| t.id == ticket_id)
                        .and_then(|t| t.issue_url.clone())
                }
                _ => None,
            };
            info!(
                worker_id,
                ticket_id,
                old_status = ?slot.status,
                "Re-assigning worker for conflict rework (→ Assigned)"
            );
            slot.status = WorkerStatus::Assigned {
                ticket_id: ticket_id.to_string(),
                issue_url,
            };
            store.set(KEY_WORKER_SLOTS, json!(slots)).await;
            true
        } else {
            warn!(
                worker_id,
                ticket_id, "Worker slot not found — cannot assign for conflict rework"
            );
            false
        }
    }

    async fn assign_worker_for_ci_fix(
        &self,
        store: &SharedStore,
        worker_id: &str,
        ticket_id: &str,
    ) -> bool {
        let mut slots: HashMap<String, WorkerSlot> =
            store.get_typed(KEY_WORKER_SLOTS).await.unwrap_or_default();

        if let Some(slot) = slots.get_mut(worker_id) {
            let issue_url = match &slot.status {
                WorkerStatus::Done { ticket_id: tid, .. } => {
                    let tickets: Vec<Ticket> =
                        store.get_typed(KEY_TICKETS).await.unwrap_or_default();
                    tickets
                        .iter()
                        .find(|t| t.id == *tid)
                        .and_then(|t| t.issue_url.clone())
                }
                WorkerStatus::Idle => {
                    let tickets: Vec<Ticket> =
                        store.get_typed(KEY_TICKETS).await.unwrap_or_default();
                    tickets
                        .iter()
                        .find(|t| t.id == ticket_id)
                        .and_then(|t| t.issue_url.clone())
                }
                _ => None,
            };
            info!(
                worker_id,
                ticket_id,
                old_status = ?slot.status,
                "Re-assigning worker for CI fix (→ Assigned)"
            );
            slot.status = WorkerStatus::Assigned {
                ticket_id: ticket_id.to_string(),
                issue_url,
            };
            store.set(KEY_WORKER_SLOTS, json!(slots)).await;

            let mut tickets: Vec<Ticket> = store.get_typed(KEY_TICKETS).await.unwrap_or_default();
            if let Some(ticket) = tickets.iter_mut().find(|t| t.id == ticket_id) {
                if !matches!(ticket.status, TicketStatus::InProgress { .. }) {
                    info!(
                        ticket_id,
                        old_status = ?ticket.status,
                        "Updating ticket status to InProgress for CI fix"
                    );
                    ticket.status = TicketStatus::InProgress {
                        worker_id: worker_id.to_string(),
                    };
                    store.set(KEY_TICKETS, json!(tickets)).await;
                }
            }
            true
        } else {
            warn!(
                worker_id,
                ticket_id, "Worker slot not found — cannot assign for CI fix"
            );
            false
        }
    }

    async fn increment_ci_fix_attempts(&self, store: &SharedStore, pr_number: u64) {
        let key = format!("_ci_fix_attempts_{}", pr_number);
        let current: u32 = store.get_typed::<u32>(&key).await.unwrap_or(0);
        let next = current + 1;
        info!(
            pr_number,
            attempts = next,
            max = MAX_CI_FIX_ATTEMPTS,
            "Incremented CI fix attempt counter"
        );
        store.set(&key, json!(next)).await;
    }

    async fn get_ci_fix_attempts(&self, store: &SharedStore, pr_number: u64) -> u32 {
        let key = format!("_ci_fix_attempts_{}", pr_number);
        store.get_typed::<u32>(&key).await.unwrap_or(0)
    }

    /// Increment the `/address_review` dispatch counter for a PR. Stored in a
    /// separate key so it survives pending_prs removal/re-add cycles.
    async fn increment_address_review_attempts(&self, store: &SharedStore, pr_number: u64) {
        let key = address_review_attempts_key(pr_number);
        let current: u32 = store.get_typed::<u32>(&key).await.unwrap_or(0);
        let next = current + 1;
        info!(
            pr_number,
            attempts = next,
            max = MAX_ADDRESS_REVIEW_ATTEMPTS,
            "Incremented /address_review attempt counter"
        );
        store.set(&key, json!(next)).await;
    }

    /// Get the current `/address_review` dispatch count for a PR.
    async fn get_address_review_attempts(&self, store: &SharedStore, pr_number: u64) -> u32 {
        let key = address_review_attempts_key(pr_number);
        store.get_typed::<u32>(&key).await.unwrap_or(0)
    }

    /// Reset the `/address_review` dispatch counter for a PR.
    async fn reset_address_review_attempts(&self, store: &SharedStore, pr_number: u64) {
        let key = address_review_attempts_key(pr_number);
        store.set(&key, json!(0)).await;
    }

    /// Clear the PR head SHA and dispatch timestamp for which `/address_review` was dispatched.
    async fn clear_address_review_dispatched(&self, store: &SharedStore, pr_number: u64) {
        let key = address_review_dispatched_key(pr_number);
        store.del(&key).await;
        let at_key = address_review_dispatched_at_key(pr_number);
        store.del(&at_key).await;
    }

    /// Return the PR head SHA that was last dispatched `/address_review` for,
    /// if any. Used to avoid re-dispatching while FORGE is still addressing the
    /// same head (i.e. it has not pushed new changes yet).
    async fn get_address_review_dispatched_sha(
        &self,
        store: &SharedStore,
        pr_number: u64,
    ) -> Option<String> {
        let key = address_review_dispatched_key(pr_number);
        store.get_typed::<String>(&key).await
    }

    /// Record the PR head SHA for which `/address_review` was dispatched.
    async fn set_address_review_dispatched_sha(
        &self,
        store: &SharedStore,
        pr_number: u64,
        head_sha: &str,
    ) {
        let key = address_review_dispatched_key(pr_number);
        store.set(&key, json!(head_sha)).await;
    }

    /// Return the timestamp when `/address_review` was last dispatched for a PR.
    async fn get_address_review_dispatched_at(
        store: &SharedStore,
        pr_number: u64,
    ) -> Option<String> {
        let key = address_review_dispatched_at_key(pr_number);
        store.get_typed::<String>(&key).await
    }

    /// Record the timestamp when `/address_review` was dispatched.
    async fn set_address_review_dispatched_at(
        &self,
        store: &SharedStore,
        pr_number: u64,
        timestamp: &str,
    ) {
        let key = address_review_dispatched_at_key(pr_number);
        store.set(&key, json!(timestamp)).await;
    }

    /// Retrieve rework evidence (last dispatched head SHA and dispatch timestamp) for a PR.
    async fn get_rework_evidence(store: &SharedStore, pr_number: u64) -> Option<ReworkEvidence> {
        let dispatched_sha = store
            .get_typed::<String>(&address_review_dispatched_key(pr_number))
            .await;
        let dispatched_at = Self::get_address_review_dispatched_at(store, pr_number).await;
        if dispatched_sha.is_some() || dispatched_at.is_some() {
            Some(ReworkEvidence {
                dispatched_sha,
                dispatched_at,
            })
        } else {
            None
        }
    }

    /// Resolve the current head SHA of a PR from the `pending_prs` entry.
    async fn pending_pr_head_sha(&self, store: &SharedStore, pr_number: u64) -> Option<String> {
        let pending_prs: Vec<Value> = store.get_typed(KEY_PENDING_PRS).await.unwrap_or_default();
        pending_prs
            .iter()
            .find(|p| p["number"].as_u64() == Some(pr_number))
            .and_then(|p| p["head_sha"].as_str().map(|s| s.to_string()))
    }

    /// Dispatch a `/address_review` directive into the responsible FORGE's
    /// existing Coder chat, keyed by role name (`ticket:{id}:chat:forge`).
    ///
    /// Returns `true` if the directive was sent. When `address_review_enabled`
    /// is false, or no forge chat / Coder client is resolvable, returns `false`
    /// so the caller can fall back to the file-based rework markers.
    async fn dispatch_address_review_for_pr(
        &self,
        store: &SharedStore,
        pr_number: u64,
        state: PrMonitorState,
        ticket_id: &str,
    ) -> bool {
        if !self.config.address_review_enabled {
            debug!(
                ticket_id,
                pr_number, "address_review dispatch disabled by config"
            );
            return false;
        }

        let repository: Option<String> = store.get_typed("repository").await;
        let (owner, repo) = parse_repository(repository.as_deref());
        let directive = collect_rework(&self.client, owner, repo, pr_number).await;
        let directive_text = build_directive(state, pr_number, directive.reason.as_deref());

        let forge_chat_key = full_ticket_key(ticket_id, KEY_TICKET_CHAT, "forge");
        let chat_id: Option<String> = store.get_typed(&forge_chat_key).await;
        let Some(client) = Self::coder_client_from_store(store).await else {
            warn!(
                ticket_id,
                pr_number, "No Coder client — cannot dispatch /address_review"
            );
            // Persist so NEXUS uses only this directive when it creates the forge chat.
            self.persist_rework_directive(store, ticket_id, "forge", &directive_text)
                .await;
            return false;
        };
        let Some(chat_id) = chat_id else {
            warn!(
                ticket_id,
                pr_number, "No forge chat binding — cannot dispatch /address_review"
            );
            // Persist so NEXUS uses only this directive when it creates the forge chat.
            self.persist_rework_directive(store, ticket_id, "forge", &directive_text)
                .await;
            return false;
        };

        match client
            .send_chat_message(
                &chat_id,
                vec![coder_client::types::ChatInputPart::text(directive_text)],
            )
            .await
        {
            Ok(_) => {
                info!(
                    ticket_id,
                    pr_number, chat_id, "Dispatched /address_review to forge chat"
                );
                true
            }
            Err(e) => {
                warn!(
                    ticket_id,
                    pr_number,
                    error = %e, "Failed to dispatch /address_review to forge chat"
                );
                false
            }
        }
    }

    /// Dispatch a `/ci_fix` directive into the responsible FORGE's **existing**
    /// Coder chat, keyed by role name (`ticket:{id}:chat:forge`).
    ///
    /// The goal is to reuse the existing FORGE workspace/chat for the issue so it
    /// checks out the already-existing branch and addresses the failing checks —
    /// not spawn a fresh workspace and start from scratch. Returns `true` when the
    /// directive was sent. When no forge chat or Coder client is resolvable,
    /// returns `false` so the caller falls back to the file-based `CI_FIX.md` +
    /// worker reassignment path (which lets NEXUS provision/reuse a forge).
    async fn dispatch_ci_fix_for_pr(
        &self,
        store: &SharedStore,
        pr_number: u64,
        ticket_id: &str,
        head_branch: &str,
        reason: &str,
        failure_detail: Option<&github::CiFailureDetail>,
    ) -> bool {
        let directive =
            build_ci_fix_directive(pr_number, ticket_id, head_branch, reason, failure_detail);

        let forge_chat_key = full_ticket_key(ticket_id, KEY_TICKET_CHAT, "forge");
        let chat_id: Option<String> = store.get_typed(&forge_chat_key).await;
        let Some(client) = Self::coder_client_from_store(store).await else {
            warn!(
                ticket_id,
                pr_number, "No Coder client — cannot dispatch /ci_fix"
            );
            // Persist so NEXUS uses only this directive when it creates the forge chat.
            self.persist_rework_directive(store, ticket_id, "forge", &directive)
                .await;
            return false;
        };
        let Some(chat_id) = chat_id else {
            warn!(
                ticket_id,
                pr_number, "No forge chat binding — cannot dispatch /ci_fix"
            );
            // Persist so NEXUS uses only this directive when it creates the forge chat.
            self.persist_rework_directive(store, ticket_id, "forge", &directive)
                .await;
            return false;
        };

        match client
            .send_chat_message(
                &chat_id,
                vec![coder_client::types::ChatInputPart::text(directive)],
            )
            .await
        {
            Ok(_) => {
                info!(
                    ticket_id,
                    pr_number, chat_id, "Dispatched /ci_fix to existing forge chat"
                );
                true
            }
            Err(e) => {
                warn!(
                    ticket_id,
                    pr_number,
                    error = %e, "Failed to dispatch /ci_fix to forge chat"
                );
                false
            }
        }
    }

    /// Persist a rework directive (`/ci_fix` / `/address_review`) that could not be
    /// delivered because no live forge chat existed. NEXUS reads it when it creates
    /// the forge chat so the new chat starts with only the targeted directive instead
    /// of the full ticket-assignment blast, then clears the key.
    async fn persist_rework_directive(
        &self,
        store: &SharedStore,
        ticket_id: &str,
        role: &str,
        directive: &str,
    ) {
        let key = full_ticket_key(ticket_id, KEY_TICKET_REWORK_DIRECTIVE, role);
        store.set(&key, json!(directive)).await;
        info!(
            ticket_id,
            role, "Persisted rework directive for NEXUS to use as forge chat initial prompt"
        );
    }

    /// Re-arm review PRs that FORGE signalled it addressed via `review_ready`.
    ///
    /// FORGE writes `_address_review_rearmed_{pr}` once it has re-armed the PR after
    /// a `/address_review`. We re-add those PRs to `pending_prs` (fetching current
    /// info from GitHub) so the next poll resumes processing them, then clear the
    /// marker. This makes re-notification event-driven instead of depending on
    /// NEXUS re-discovery.
    async fn rearm_review_prs(&self, store: &SharedStore, owner: &str, repo: &str) {
        let marker_keys = store.keys("_address_review_rearmed_*").await;
        if marker_keys.is_empty() {
            return;
        }

        for key in marker_keys {
            // Parse the pr_number from the trailing digits of the marker key.
            // `keys()` may return fully-qualified (namespaced) keys, so scan for the
            // marker substring and take whatever follows it.
            let pr_number = key
                .rsplit("_address_review_rearmed_")
                .next()
                .and_then(|suffix| suffix.trim().parse::<u64>().ok());
            let Some(pr_number) = pr_number else {
                continue;
            };

            info!(
                pr_number,
                "FORGE re-armed PR after /address_review — re-adding to pending_prs"
            );

            let mut pending_prs: Vec<Value> =
                store.get_typed(KEY_PENDING_PRS).await.unwrap_or_default();
            let already_tracked = pending_prs
                .iter()
                .any(|p| p["number"].as_u64() == Some(pr_number));
            if !already_tracked {
                if let Ok(pr_info) = self.client.get_pull_request(owner, repo, pr_number).await {
                    pending_prs.push(json!({
                        "number": pr_info.number,
                        "ticket_id": pr_info.ticket_id,
                        "head_sha": pr_info.head_sha,
                        "head_branch": pr_info.head_branch,
                        "base_branch": pr_info.base_branch,
                        "title": pr_info.title,
                        "mergeable": pr_info.mergeable,
                        "has_conflicts": pr_info.mergeable == Some(false),
                    }));
                    store.set(KEY_PENDING_PRS, json!(pending_prs)).await;
                } else {
                    warn!(
                        pr_number,
                        "Failed to fetch re-armed PR from GitHub — leaving out of pending_prs"
                    );
                }
            }

            // Clear the marker so we don't re-add it on every poll. Use the logical
            // key (store.del re-applies the namespace); `keys()` returns the fully
            // qualified form which must not be re-namespaced.
            store.del(&address_review_rearmed_key(pr_number)).await;
        }
    }

    async fn write_ci_fix_md(
        &self,
        pr_placeholder: &CiFixPrInfo,
        reason: &str,
        failure_detail: Option<&github::CiFailureDetail>,
    ) -> bool {
        let workspace_root = match config::AgentConfig::init_from_env()
            .ok()
            .and_then(|agent| agent.effective_workspace_root())
        {
            Some(root) => root,
            None => {
                warn!("AGENTFLOW_WORKSPACE_ROOT not set — cannot write CI_FIX.md");
                return false;
            }
        };

        // Extract pair_id from branch name (e.g., "forge-1/T-005" -> "forge-1")
        let branch = &pr_placeholder.head_branch;
        let parts: Vec<&str> = branch.splitn(2, '/').collect();
        if parts.len() != 2 {
            warn!(
                branch,
                "Cannot parse branch for pair_id — skipping CI_FIX.md"
            );
            return false;
        }
        let pair_id = parts[0];

        // Use ticket_id from PR info (extracted from title), not from branch name.
        // The branch name may be stale or mismatched with the actual ticket.
        // Fall back to branch-derived ticket_id if not available.
        let _ticket_id = pr_placeholder.ticket_id.as_deref().unwrap_or(parts[1]);

        let shared_dir = PathBuf::from(&workspace_root)
            .join("worktrees")
            .join(pair_id)
            .join(".pair-shared");

        // Ensure the shared directory exists before writing CI_FIX.md.
        // The directory may not exist yet if the pair hasn't been provisioned
        // for this ticket, or if the workspace was cleaned up.
        if !shared_dir.exists() {
            if let Err(e) = tokio::fs::create_dir_all(&shared_dir).await {
                warn!(
                    path = %shared_dir.display(),
                    error = %e,
                    "Failed to create shared directory for CI_FIX.md"
                );
                return false;
            }
            info!(path = %shared_dir.display(), "Created shared directory for CI_FIX.md");
        }

        let annotations_section = match failure_detail {
            Some(d) if !d.annotations.is_empty() => {
                let ann_lines = d
                    .annotations
                    .iter()
                    .map(|a| {
                        format!(
                            "- **{}** `{}:{}` {}",
                            a.check_name, a.path, a.start_line, a.message
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                format!(
                    "\n## Exact Errors (from CI annotations)\n\n\
                     These are the specific file:line errors from CI. Start by fixing these:\n\n\
                     {}\n",
                    ann_lines
                )
            }
            _ => String::new(),
        };

        let job_log_section = match failure_detail {
            Some(d) if !d.job_logs.is_empty() => {
                let logs = d
                    .job_logs
                    .iter()
                    .map(|(name, log)| format!("### Job: {}\n```\n{}\n```", name, log))
                    .collect::<Vec<_>>()
                    .join("\n\n");
                format!(
                    "\n## Job Log Output (for reference)\n\n\
                     {}\n\n\
                     **Do NOT try to fix errors from this log alone.** Read .github/workflows/ and run the actual steps locally.",
                    logs
                )
            }
            _ => String::new(),
        };

        let content = format!(
            "# CI Fix Required\n\n\
             VESSEL detected that CI checks failed for PR #{}.\n\n\
             ## Failed Checks\n\n{}\n\n\
             {}{}\
             ## How to Fix\n\n\
             The branch has been updated with the latest origin/main (merged in).\n\
             You now have the latest .github/workflows/ files — read them to find the failing jobs.\n\n\
             **After fixing, push your changes directly — the sandbox has network access for git push and GitHub API.**\n\n\
             1. Read .github/workflows/ — find the workflow(s) matching the failed check names above.\n\
             2. Match the check name to the job name in the workflow YAML.\n\
             3. Install any missing tools the workflow expects (pip, npm, ruff, etc.).\n\
             4. Install project deps as the workflow does (pip install -r requirements.txt, npm ci, etc.).\n\
             5. Run the failing job's exact `run:` steps locally from the workflow YAML.\n\
             6. Fix ALL errors — do not fix one error and stop, CI will just fail on the next.\n\
             7. Verify ALL checks pass locally.\n\
             8. Write a COMMIT_MSG.md file in the shared directory ({}) describing your changes (first line = subject, blank line, then body).\n\
             9. Run: `git add -A && git commit -m \"fix CI failures\" && git push`\n\
             10. Write STATUS.json with `\"status\": \"PR_OPENED\"` and your PR number\n\n\
             **Fallback:** If `git push` or `git commit` fails (e.g., in restricted environments), write STATUS.json with `\"status\": \"COMPLETE\"` instead — the harness will handle the commit and push for you.\n\n\
             If merge conflict markers are present in any files, resolve them BEFORE running CI checks.\n\n\
             ## WORKLOG Updates — CRITICAL\n\n\
             You MUST update WORKLOG.md in the shared directory as you work. The watchdog monitors\n\
             WORKLOG.md — if you don't update it, your pair will be killed after 20 minutes of silence.\n\n\
             ## Rules\n\n\
             - Do NOT change the PR description or title\n\
             - Try to push yourself first — the sandbox has git and network access\n\
             - If push fails, write `\"status\": \"COMPLETE\"` and the harness will push for you\n\
             - Fix ALL errors before writing STATUS.json — do not write COMPLETE until all local checks pass\n\
             - Read .github/workflows/ for the exact CI commands — do not guess\n\
             - After you push, VESSEL will re-monitor CI automatically",
            pr_placeholder.pr_number,
            reason,
            annotations_section,
            job_log_section,
            shared_dir.display(),
        );

        let path = shared_dir.join("CI_FIX.md");
        if let Err(e) = tokio::fs::write(&path, &content).await {
            warn!(path = %path.display(), error = %e, "Failed to write CI_FIX.md");
            false
        } else {
            info!(path = %path.display(), "Wrote CI_FIX.md for forge CI fix");
            true
        }
    }

    /// Reconcile startup: check for PRs that are already merged on GitHub.
    pub async fn reconcile(&self, store: &SharedStore) -> Result<()> {
        info!("Running VESSEL startup reconciliation");

        // Clean up / retry deletion for any retained workspaces from terminal tickets
        self.cleanup_terminal_ticket_workspaces(store).await;

        let repository: Option<String> = store.get_typed("repository").await;
        let pending_prs: Option<Vec<Value>> = store.get_typed("pending_prs").await;
        let (owner, repo) = parse_repository(repository.as_deref());

        let pending = pending_prs.unwrap_or_default();

        for pr in pending {
            let pr_number = pr["number"].as_u64().unwrap_or(0);
            if pr_number == 0 {
                continue;
            }

            if let Some(merge_sha) = self
                .client
                .confirmed_merge_sha(owner, repo, pr_number)
                .await?
            {
                warn!(pr_number, "Found already-merged PR during reconciliation");

                let ticket_id = pr["ticket_id"].as_str().map(String::from);
                let pr_info = self.client.get_pull_request(owner, repo, pr_number).await;

                if let Ok(info) = pr_info {
                    let tid = ticket_id
                        .or(info.ticket_id.clone())
                        .unwrap_or_else(|| format!("T-{}", pr_number));
                    VesselNotifier::emit_ticket_merged(
                        store,
                        &tid,
                        pr_number,
                        &info.head_sha,
                        &info.title,
                        None,
                    )
                    .await;
                    let state = store.lifecycle(&tid).await?;
                    if state.phase != config::lifecycle::Phase::Done {
                        store
                            .transition(
                                &tid,
                                state.version,
                                "vessel",
                                config::lifecycle::Event::ReconcileMerged {
                                    number: pr_number,
                                    sha: merge_sha,
                                },
                            )
                            .await?;
                    }

                    self.remove_from_pending_prs(store, pr_number).await;
                }
            }
        }

        Ok(())
    }
}

fn is_merge_conflict_message(msg: &str) -> bool {
    let lower = msg.to_lowercase();
    lower.contains("merge conflict")
        || lower.contains("merge_conflict")
        || (lower.contains("405") && lower.contains("method not allowed"))
}

/// Build the lightweight `/ci_fix` chat directive for FORGE.
///
/// This is a **pointer**, not a dump of the whole CI output: it names the PR, the
/// ticket, the branch, the failure reason, and the failed check names + `path:line`
/// annotations so FORGE can reproduce and fix in its existing workspace. FORGE is
/// instructed to reuse its existing workspace/branch (via the `/ci_fix` command and
/// `forge-ci-fix` skill) rather than start from scratch.
fn build_ci_fix_directive(
    pr_number: u64,
    ticket_id: &str,
    head_branch: &str,
    reason: &str,
    failure_detail: Option<&github::CiFailureDetail>,
) -> String {
    let mut out = String::from("/ci_fix\n");
    out.push_str("state: ci_failed\n");
    out.push_str(&format!("pr: {}\n", pr_number));
    out.push_str(&format!("ticket: {}\n", ticket_id));
    if !head_branch.is_empty() {
        out.push_str(&format!("branch: {}\n", head_branch));
    }
    if !reason.trim().is_empty() {
        out.push_str(&format!("reason: {}\n", reason.trim()));
    }

    if let Some(detail) = failure_detail {
        let check_names = detail.failed_check_names();
        if !check_names.is_empty() {
            out.push_str("checks:\n");
            for name in check_names {
                out.push_str(&format!("- {}\n", name));
            }
        }
        if !detail.annotations.is_empty() {
            out.push_str("annotations:\n");
            for a in &detail.annotations {
                out.push_str(&format!(
                    "- **{}** `{}:{}` {}\n",
                    a.check_name, a.path, a.start_line, a.message
                ));
            }
        }
    }

    out.push_str(
        "\nReuse your existing workspace and branch (you already have this PR checked \
         out). Match the failed checks to .github/workflows/, reproduce and fix ALL \
         errors locally, push, then re-run: openflows-harness status set testing",
    );
    out
}

fn parse_repository(repository: Option<&str>) -> (&str, &str) {
    match repository {
        Some(repo) => {
            let parts: Vec<&str> = repo.split('/').collect();
            if parts.len() == 2 {
                (parts[0], parts[1])
            } else {
                ("", "")
            }
        }
        None => ("", ""),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn merge_reservation_clears_only_for_definitive_rejection() {
        for (status, pending) in [
            (405, false),
            (409, false),
            (422, false),
            (408, true),
            (500, true),
            (0, true), // Transport failure after the merge request is accepted.
        ] {
            let mut server = mockito::Server::new_async().await;
            let _status = server
                .mock("GET", "/repos/org/repo/commits/head/status")
                .with_status(200)
                .with_body(r#"{"state":"success","total_count":1}"#)
                .create_async()
                .await;
            let _checks = server
                .mock(
                    "GET",
                    "/repos/org/repo/commits/head/check-suites?per_page=100&page=1",
                )
                .with_status(200)
                .with_body(r#"{"check_suites":[]}"#)
                .create_async()
                .await;
            let merge = server
                .mock("PUT", "/repos/org/repo/pulls/42/merge")
                .with_status(if status == 0 { 500 } else { status })
                .with_body(r#"{"message":"rejected"}"#)
                .expect(if status == 0 { 0 } else { 1 })
                .create_async()
                .await;
            let client = github::GithubRestClient::with_api_base("test", server.url());
            let mut node = VesselNode::new(VesselConfig::default());
            node.client = client.clone();
            let merge_client = if status == 0 {
                let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
                let endpoint = format!("http://{}", listener.local_addr().unwrap());
                tokio::spawn(async move {
                    use tokio::io::AsyncReadExt;
                    let (mut stream, _) = listener.accept().await.unwrap();
                    let mut request = [0; 4096];
                    let _ = stream.read(&mut request).await;
                    // Drop the connection without reporting whether the merge happened.
                });
                github::GithubRestClient::with_api_base("test", endpoint)
            } else {
                client
            };
            node.merger = PrMerger::new(merge_client, pocketflow_core::MergeMethod::Squash);
            let store = SharedStore::new_in_memory();
            let decision = json!({"round":0,"pr_number":42,"actor":"human","approved":true,"report":"approved","revision":1,"head":"head"});
            store.set("ticket:T-42:status", json!({"version":1,"phase":"submit","head":"head","pr_number":42,"pr_decision":decision,"pr_human":decision})).await;
            *node.lifecycle_store.lock().unwrap() = Some(store.clone());
            let pr = PrInfo {
                number: 42,
                head_sha: "head".into(),
                head_branch: "feature".into(),
                base_branch: "main".into(),
                ticket_id: Some("T-42".into()),
                title: "Feature".into(),
                body: None,
                state: pocketflow_core::PrState::Open,
                mergeable: Some(true),
            };
            let result = node.merge_reviewed("org", "repo", &pr).await;
            assert_eq!(
                store.lifecycle("T-42").await.unwrap().merge_pending,
                pending,
                "HTTP {status}: {result:?}"
            );
            assert_eq!(result.is_err(), pending);
            merge.assert_async().await;
        }
    }

    /// Build a VESSEL node whose GitHub reviews endpoint returns `status`/`body`,
    /// plus a store holding a `submit` lifecycle at head `head` where SENTINEL
    /// has approved but the human gate is absent and delivery is pending.
    async fn reconcile_fixture(
        status: usize,
        body: &str,
    ) -> (mockito::ServerGuard, VesselNode, SharedStore) {
        let mut server = mockito::Server::new_async().await;
        server
            .mock(
                "GET",
                "/repos/org/repo/pulls/42/reviews?per_page=100&page=1",
            )
            .with_status(status)
            .with_body(body)
            .create_async()
            .await;
        let mut node = VesselNode::new(VesselConfig::default());
        node.client = github::GithubRestClient::with_api_base("test", server.url());
        let store = SharedStore::new_in_memory();
        *node.lifecycle_store.lock().unwrap() = Some(store.clone());
        let sentinel = json!({"round":0,"pr_number":42,"actor":"sentinel","approved":true,"report":"ok","revision":1,"head":"head"});
        store
            .set(
                "ticket:T-42:status",
                json!({"version":1,"phase":"submit","head":"head","pr_number":42,"pr_decision":sentinel,"pr_delivery":sentinel}),
            )
            .await;
        (server, node, store)
    }

    async fn run_reconcile(node: &VesselNode, store: &SharedStore) -> bool {
        let state = store.lifecycle("T-42").await.unwrap();
        node.reconcile_github_approval(store, "T-42", &state, "org", "repo", 42, "head")
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn reconcile_github_approval_sets_pr_human_from_approved_review() {
        // An authorized collaborator (alice) approved the current head. SENTINEL
        // shares the PR author's identity and cannot self-approve, so this is
        // a genuine human sign-off.
        let (_server, node, store) = reconcile_fixture(
            200,
            r#"[{"state":"APPROVED","user":{"login":"alice"},"author_association":"COLLABORATOR","commit_id":"head","submitted_at":"2026-10-05T00:00:00Z","body":"LGTM"}]"#,
        )
        .await;
        store
            .set(
                KEY_TICKETS,
                json!([{
                    "id": "T-42",
                    "title": "Task",
                    "body": "Fix",
                    "priority": 1,
                    "branch": null,
                    "status": {
                        "type": "awaiting_human",
                        "worker_id": "vessel",
                        "reason": "review limit",
                        "attempts": 3
                    },
                    "attempts": 1
                }]),
            )
            .await;
        store.set("_address_review_attempts_42", json!(3)).await;
        store
            .set(&address_review_dispatched_key(42), json!("old_sha"))
            .await;

        assert!(!store.lifecycle("T-42").await.unwrap().merge_ready("head"));

        assert!(
            run_reconcile(&node, &store).await,
            "GitHub-approved PR should be reconciled"
        );

        let attempts: u32 = store
            .get_typed("_address_review_attempts_42")
            .await
            .unwrap_or(99);
        assert_eq!(attempts, 0, "review attempts must be reset upon approval");
        let dispatched = store.get(&address_review_dispatched_key(42)).await;
        assert!(dispatched.is_none(), "dispatch key must be cleared");

        let tickets: Vec<Ticket> = store.get_typed(KEY_TICKETS).await.unwrap();
        assert!(
            matches!(tickets[0].status, TicketStatus::InProgress { .. }),
            "AwaitingHuman ticket status must be cleared to InProgress"
        );

        let after = store.lifecycle("T-42").await.unwrap();
        let human = after.pr_human.as_ref().expect("pr_human must be recorded");
        assert!(human.approved);
        assert_eq!(
            human.actor, "human",
            "approval must be attributed to a human, not SENTINEL"
        );
        assert!(human.report.contains("alice"));
        assert!(
            after.pr_delivery.is_none(),
            "delivery handshake must complete"
        );
        assert!(
            after.merge_ready("head"),
            "reconciled PR must be merge-ready"
        );
    }

    #[tokio::test]
    async fn reconcile_github_approval_satisfies_missing_pr_decision() {
        // When Forge opens a PR after Sentinel testing approval, pr_decision has
        // not yet been written. Reconciling an authorized GitHub approval must
        // satisfy both pr_human and pr_decision so merge_ready passes without
        // waiting indefinitely for a separate sentinel review.
        let (_server, node, store) = reconcile_fixture(
            200,
            r#"[{"state":"APPROVED","user":{"login":"alice"},"author_association":"COLLABORATOR","commit_id":"head","submitted_at":"2026-10-05T00:00:00Z","body":"LGTM"}]"#,
        )
        .await;
        // Reset status to have NO pr_decision, simulating an opened PR in submit phase
        let test_decision = json!({"round":0,"actor":"sentinel","approved":true,"report":"ok","revision":1,"head":"head"});
        store
            .set(
                "ticket:T-42:status",
                json!({"version":1,"phase":"submit","head":"head","pr_number":42,"test_decision":test_decision}),
            )
            .await;
        let before = store.lifecycle("T-42").await.unwrap();
        assert!(before.pr_decision.is_none());
        assert!(!before.merge_ready("head"));

        assert!(
            run_reconcile(&node, &store).await,
            "GitHub-approved PR should be reconciled"
        );

        let after = store.lifecycle("T-42").await.unwrap();
        assert!(after.pr_human.as_ref().is_some_and(|h| h.approved));
        assert!(after.pr_decision.as_ref().is_some_and(|d| d.approved));
        assert!(after.pr_delivery.is_none());
        assert!(
            after.merge_ready("head"),
            "reconciled PR must be merge-ready when pr_decision was originally missing"
        );
    }

    #[tokio::test]
    async fn reconcile_github_approval_rejects_when_sentinel_test_gate_not_passed() {
        // If Sentinel has not verified the candidate in Testing phase,
        // reconciling a GitHub approval must NOT satisfy pr_decision.
        let (_server, node, store) = reconcile_fixture(
            200,
            r#"[{"state":"APPROVED","user":{"login":"alice"},"author_association":"COLLABORATOR","commit_id":"head","submitted_at":"2026-10-05T00:00:00Z","body":"LGTM"}]"#,
        )
        .await;
        // Submit phase status with NO test_decision (Sentinel never approved testing)
        store
            .set(
                "ticket:T-42:status",
                json!({"version":1,"phase":"submit","head":"head","pr_number":42}),
            )
            .await;

        assert!(
            !run_reconcile(&node, &store).await,
            "Reconciliation should be rejected when Sentinel test gate has not passed"
        );

        let after = store.lifecycle("T-42").await.unwrap();
        assert!(after.pr_human.is_none());
        assert!(after.pr_decision.is_none());
        assert!(!after.merge_ready("head"));
    }

    #[tokio::test]
    async fn reconcile_github_approval_skips_when_not_approved() {
        let (_server, node, store) = reconcile_fixture(
            200,
            r#"[{"state":"COMMENTED","user":{"login":"alice"},"author_association":"COLLABORATOR","commit_id":"head","submitted_at":"2026-10-05T00:00:00Z","body":"comments"}]"#,
        )
        .await;
        assert!(
            !run_reconcile(&node, &store).await,
            "not-approved PR must not be reconciled"
        );
        assert!(store.lifecycle("T-42").await.unwrap().pr_human.is_none());
    }

    #[tokio::test]
    async fn reconcile_github_approval_is_noop_when_human_already_approved() {
        let (_server, node, store) = reconcile_fixture(
            200,
            r#"[{"state":"APPROVED","user":{"login":"alice"},"author_association":"OWNER","commit_id":"head"}]"#,
        )
        .await;
        let sentinel = json!({"round":0,"pr_number":42,"actor":"sentinel","approved":true,"report":"ok","revision":1,"head":"head"});
        let human = json!({"round":0,"pr_number":42,"actor":"human","approved":true,"report":"ok","revision":1,"head":"head"});
        store
            .set(
                "ticket:T-42:status",
                json!({"version":1,"phase":"submit","head":"head","pr_number":42,"pr_decision":sentinel,"pr_human":human}),
            )
            .await;
        assert!(
            !run_reconcile(&node, &store).await,
            "already human-approved must be a no-op"
        );
    }

    #[tokio::test]
    async fn reconcile_github_approval_rejects_unauthorized_reviewer() {
        // A drive-by account without repository authority approves: not a
        // valid human sign-off.
        let (_server, node, store) = reconcile_fixture(
            200,
            r#"[{"state":"APPROVED","user":{"login":"mallory"},"author_association":"NONE","commit_id":"head","submitted_at":"2026-10-05T00:00:00Z"},
                {"state":"APPROVED","user":{"login":"eve"},"author_association":"CONTRIBUTOR","commit_id":"head","submitted_at":"2026-10-05T00:00:01Z"}]"#,
        )
        .await;
        assert!(!run_reconcile(&node, &store).await);
        assert!(store.lifecycle("T-42").await.unwrap().pr_human.is_none());
    }

    #[tokio::test]
    async fn reconcile_github_approval_rejects_stale_approval_of_older_commit() {
        // The owner approved an earlier commit; new code has since been pushed.
        let (_server, node, store) = reconcile_fixture(
            200,
            r#"[{"state":"APPROVED","user":{"login":"alice"},"author_association":"OWNER","commit_id":"old-head","submitted_at":"2026-10-05T00:00:00Z"}]"#,
        )
        .await;
        assert!(!run_reconcile(&node, &store).await);
        assert!(store.lifecycle("T-42").await.unwrap().pr_human.is_none());
    }

    #[tokio::test]
    async fn reconcile_github_approval_names_effective_approver() {
        // alice's only approval is stale (older commit); bob approved the
        // current head. The audit record must name bob, not alice.
        let (_server, node, store) = reconcile_fixture(
            200,
            r#"[{"state":"APPROVED","user":{"login":"alice"},"author_association":"MEMBER","commit_id":"old-head","submitted_at":"2026-10-04T00:00:00Z"},
                {"state":"APPROVED","user":{"login":"bob"},"author_association":"COLLABORATOR","commit_id":"head","submitted_at":"2026-10-05T00:00:00Z"}]"#,
        )
        .await;
        assert!(run_reconcile(&node, &store).await);
        let report = store
            .lifecycle("T-42")
            .await
            .unwrap()
            .pr_human
            .unwrap()
            .report;
        assert!(report.contains("bob"), "report: {report}");
        assert!(!report.contains("alice"), "report: {report}");
    }

    #[tokio::test]
    async fn reconcile_github_approval_vetoed_by_current_changes_requested() {
        let (_server, node, store) = reconcile_fixture(
            200,
            r#"[{"state":"APPROVED","user":{"login":"alice"},"author_association":"OWNER","commit_id":"head","submitted_at":"2026-10-05T00:00:00Z"},
                {"state":"CHANGES_REQUESTED","user":{"login":"bob"},"author_association":"MEMBER","commit_id":"head","submitted_at":"2026-10-05T00:00:01Z"}]"#,
        )
        .await;
        assert!(!run_reconcile(&node, &store).await);
        assert!(store.lifecycle("T-42").await.unwrap().pr_human.is_none());
    }

    #[tokio::test]
    async fn reconcile_github_approval_defers_on_review_fetch_error() {
        // A GitHub error must not propagate (which would abort the whole VESSEL
        // pass); the PR is simply deferred.
        let (_server, node, store) = reconcile_fixture(404, r#"{"message":"Not Found"}"#).await;
        assert!(!run_reconcile(&node, &store).await);
        assert!(store.lifecycle("T-42").await.unwrap().pr_human.is_none());
    }

    #[tokio::test]
    async fn reconcile_github_approval_preserves_approval_when_approver_later_comments() {
        // alice approved the PR, then later left a comment. The comment must not
        // hide or invalidate her approval.
        let (_server, node, store) = reconcile_fixture(
            200,
            r#"[{"state":"APPROVED","user":{"login":"alice"},"author_association":"OWNER","commit_id":"head","submitted_at":"2026-10-05T00:00:00Z"},
                {"state":"COMMENTED","user":{"login":"alice"},"author_association":"OWNER","commit_id":"head","submitted_at":"2026-10-05T01:00:00Z","body":"looks good!"}]"#,
        )
        .await;
        assert!(run_reconcile(&node, &store).await);
        let after = store.lifecycle("T-42").await.unwrap();
        assert!(after.pr_human.as_ref().is_some_and(|d| d.approved));
        assert!(after.pr_human.as_ref().unwrap().report.contains("alice"));
    }

    #[tokio::test]
    async fn ticketless_pr_is_reported_for_manual_handling_and_dequeued() {
        let mut server = mockito::Server::new_async().await;
        let _pr = server.mock("GET", "/repos/org/repo/pulls/42")
            .with_status(200).with_body(r#"{"number":42,"title":"Dependency update","body":null,"head":{"sha":"head","ref":"dependabot/update"},"base":{"ref":"main","sha":"base"},"state":"open","mergeable":true}"#)
            .create_async().await;
        let client = github::GithubRestClient::with_api_base("test", server.url());
        let mut node = VesselNode::new(VesselConfig::default());
        node.client = client;
        let store = SharedStore::new_in_memory();
        *node.lifecycle_store.lock().unwrap() = Some(store.clone());
        store.set(KEY_PENDING_PRS, json!([{"number":42}])).await;
        let result = node
            .exec(json!({"owner":"org","repo":"repo","pending_prs":[{"number":42}]}))
            .await
            .unwrap();
        assert_eq!(result["outcomes"][0]["type"], "unmanaged");
        let action = node.post(&store, result).await.unwrap();
        assert_eq!(action.as_str(), Action::AWAITING_HUMAN);
        assert!(store
            .get_typed::<Vec<Value>>(KEY_PENDING_PRS)
            .await
            .unwrap()
            .is_empty());
        assert!(store.get("pr:42:unmanaged").await.is_some());
    }

    #[test]
    fn test_parse_repository() {
        assert_eq!(parse_repository(Some("owner/repo")), ("owner", "repo"));
        assert_eq!(parse_repository(Some("single")), ("", ""));
        assert_eq!(parse_repository(None), ("", ""));
    }

    #[tokio::test]
    async fn test_prep_reads_pending_prs() {
        let store = SharedStore::new_in_memory();
        store.set("repository", json!("test-owner/test-repo")).await;
        store
            .set(
                "pending_prs",
                json!([
                    {"number": 1, "ticket_id": "T-1"},
                    {"number": 2, "ticket_id": "T-2"},
                ]),
            )
            .await;

        let config = VesselConfig::default();
        let node = VesselNode::new(config);

        let result = node.prep(&store).await.unwrap();

        assert_eq!(result["owner"], "test-owner");
        assert_eq!(result["repo"], "test-repo");
        assert_eq!(result["pending_prs"].as_array().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn test_prep_prefers_merge_ready_prs_handoff() {
        let store = SharedStore::new_in_memory();
        store.set("repository", json!("test-owner/test-repo")).await;
        store
            .set(
                KEY_PENDING_PRS,
                json!([
                    {"number": 1, "ticket_id": "T-1"},
                    {"number": 2, "ticket_id": "T-2"},
                ]),
            )
            .await;
        store
            .set(
                KEY_MERGE_READY_PRS,
                json!([{"number": 2, "ticket_id": "T-2"}]),
            )
            .await;

        let config = VesselConfig::default();
        let node = VesselNode::new(config);

        let result = node.prep(&store).await.unwrap();

        let pending = result["pending_prs"].as_array().unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0]["number"].as_u64(), Some(2));
    }

    #[tokio::test]
    async fn test_prep_refreshes_stale_empty_merge_ready_handoff() {
        let store = SharedStore::new_in_memory();
        store.set("repository", json!("test-owner/test-repo")).await;
        // NEXUS wrote an empty merge-ready snapshot, but a PR has since reached
        // submit via FORGE/SENTINEL without another NEXUS pass.
        store
            .set(KEY_PENDING_PRS, json!([{"number": 3, "ticket_id": "T-3"}]))
            .await;
        store.set(KEY_MERGE_READY_PRS, json!([])).await;
        store
            .set(
                &full_ticket_key_flat("T-3", config::state::KEY_TICKET_STATUS),
                json!(config::lifecycle::Lifecycle {
                    phase: config::lifecycle::Phase::Submit,
                    ..Default::default()
                }),
            )
            .await;

        let config = VesselConfig::default();
        let node = VesselNode::new(config);

        let result = node.prep(&store).await.unwrap();
        let pending = result["pending_prs"].as_array().unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0]["number"].as_u64(), Some(3));
    }

    #[tokio::test]
    async fn test_prep_empty_pending_prs() {
        let store = SharedStore::new_in_memory();

        let config = VesselConfig::default();
        let node = VesselNode::new(config);

        let result = node.prep(&store).await.unwrap();

        assert_eq!(result["pending_prs"].as_array().unwrap().len(), 0);
    }

    #[tokio::test]
    async fn test_post_handles_merged_outcome() {
        let store = SharedStore::new_in_memory();
        store
            .set("pending_prs", json!([{"number": 42, "ticket_id": "T-42"}]))
            .await;
        store
            .set(
                "tickets",
                json!([{"id": "T-42", "status": {"type": "in_progress"}}]),
            )
            .await;

        let config = VesselConfig::default();
        let node = VesselNode::new(config);

        let exec_result = json!({
            "outcomes": [VesselOutcome::Merged {
                ticket_id: "T-42".to_string(),
                pr_number: 42,
                sha: "abc123".to_string(),
                pr_title: "Add feature X".to_string(),
                pr_body: Some("Implementation details".to_string()),
            }],
            "has_work": true,
        });

        let action = node.post(&store, exec_result).await.unwrap();
        assert_eq!(action.as_str(), Action::DEPLOYED);

        let events = store.get_events_since(0).await;
        assert!(events.iter().any(|e| e.event_type == "ticket_merged"));

        let status = store.get("ticket:T-42:status").await;
        assert_eq!(status, None, "post cannot manufacture merge evidence");

        let pending: Vec<Value> = store.get_typed("pending_prs").await.unwrap_or_default();
        assert!(pending.is_empty());
    }

    #[tokio::test]
    async fn test_post_handles_ci_failed_outcome() {
        let store = SharedStore::new_in_memory();

        let config = VesselConfig::default();
        let node = VesselNode::new(config);

        let exec_result = json!({
            "outcomes": [VesselOutcome::CiFailed {
                ticket_id: Some("T-42".to_string()),
                pr_number: 42,
                reason: "Tests failed".to_string(),
                failure_detail: None,
            }],
            "has_work": true,
        });

        let action = node.post(&store, exec_result).await.unwrap();
        assert_eq!(action.as_str(), Action::DEPLOY_FAILED);

        let events = store.get_events_since(0).await;
        assert!(events.iter().any(|e| e.event_type == "ci_failed"));
    }

    #[tokio::test]
    async fn test_post_handles_merge_blocked_outcome() {
        let store = SharedStore::new_in_memory();

        let config = VesselConfig::default();
        let node = VesselNode::new(config);

        let exec_result = json!({
            "outcomes": [VesselOutcome::MergeBlocked {
                ticket_id: Some("T-42".to_string()),
                pr_number: 42,
                reason: "Merge conflict".to_string(),
            }],
            "has_work": true,
        });

        let action = node.post(&store, exec_result).await.unwrap();
        assert_eq!(action.as_str(), Action::DEPLOY_FAILED);

        let events = store.get_events_since(0).await;
        assert!(events.iter().any(|e| e.event_type == "merge_blocked"));
    }

    #[tokio::test]
    async fn deferred_lifecycle_prs_pause_without_dropping_or_merging() {
        for state in [
            json!({"phase":"planning"}),
            json!({"phase":"submit", "head":"head", "merge_pending":true}),
            json!({"phase":"submit", "head":"old-head"}),
            json!({"phase":"submit", "head":"head"}),
        ] {
            let mut server = mockito::Server::new_async().await;
            let pr = server.mock("GET", "/repos/org/repo/pulls/42")
                .with_status(200)
                .with_body(r#"{"number":42,"title":"Feature","body":null,"head":{"sha":"head","ref":"feature"},"base":{"ref":"main","sha":"base"},"state":"open","mergeable":true,"merged":false}"#)
                .expect(2).create_async().await;
            let merge = server
                .mock("PUT", "/repos/org/repo/pulls/42/merge")
                .expect(0)
                .create_async()
                .await;
            let mut node = VesselNode::new(VesselConfig::default());
            node.client = github::GithubRestClient::with_api_base("test", server.url());
            let store = SharedStore::new_in_memory();
            *node.lifecycle_store.lock().unwrap() = Some(store.clone());
            let pending = json!([{"number":42,"ticket_id":"T-42"}]);
            store.set(KEY_PENDING_PRS, pending.clone()).await;
            store.set("ticket:T-42:status", state.clone()).await;
            let result = node
                .exec(json!({"owner":"org","repo":"repo","pending_prs":pending}))
                .await
                .unwrap();
            let action = node.post(&store, result).await.unwrap();
            assert_eq!(action.as_str(), PAUSE_SIGNAL, "state: {state}");
            assert_eq!(store.get(KEY_PENDING_PRS).await.unwrap(), pending);
            pr.assert_async().await;
            merge.assert_async().await;
        }
    }

    #[tokio::test]
    async fn unapproved_pr_with_changes_requested_dispatches_rework_instead_of_deferring() {
        let mut server = mockito::Server::new_async().await;
        let pr = server
            .mock("GET", "/repos/org/repo/pulls/42")
            .with_status(200)
            .with_body(r#"{"number":42,"title":"Feature","body":null,"head":{"sha":"head","ref":"feature"},"base":{"ref":"main","sha":"base"},"state":"open","mergeable":true,"merged":false}"#)
            .expect_at_least(1)
            .create_async()
            .await;
        let reviews = server
            .mock("GET", "/repos/org/repo/pulls/42/reviews?per_page=100&page=1")
            .with_status(200)
            .with_body(r#"[{"id":1,"user":{"login":"alice","id":100},"body":"Please address this bug","state":"CHANGES_REQUESTED","submitted_at":"2026-10-08T12:00:00Z","commit_id":"head","author_association":"MEMBER"}]"#)
            .expect_at_least(1)
            .create_async()
            .await;
        let comments = server
            .mock("GET", "/repos/org/repo/pulls/42/comments?per_page=100")
            .with_status(200)
            .with_body("[]")
            .expect_at_least(1)
            .create_async()
            .await;
        let _status = server
            .mock("GET", "/repos/org/repo/commits/head/status")
            .with_status(200)
            .with_body(r#"{"state":"success","total_count":1}"#)
            .expect_at_least(1)
            .create_async()
            .await;
        let _checks = server
            .mock(
                "GET",
                "/repos/org/repo/commits/head/check-suites?per_page=100&page=1",
            )
            .with_status(200)
            .with_body(r#"{"check_suites":[]}"#)
            .expect_at_least(1)
            .create_async()
            .await;

        let client = github::GithubRestClient::with_api_base("test", server.url());
        let mut config = VesselConfig::default();
        config.ci_poll.interval_secs = 1;
        let mut node = VesselNode::new(config);
        node.client = client.clone();
        node.poller = CiPoller::new(node.config.ci_poll.clone(), client);

        let store = SharedStore::new_in_memory();
        *node.lifecycle_store.lock().unwrap() = Some(store.clone());
        let pending = json!([{"number":42,"ticket_id":"T-42"}]);
        store.set(KEY_PENDING_PRS, pending.clone()).await;
        // Ticket is in Submit phase but NOT merge_ready (no pr_decision or pr_human approval).
        store
            .set(
                "ticket:T-42:status",
                json!({"version":1,"phase":"submit","head":"head","pr_number":42}),
            )
            .await;

        let result = node
            .exec(json!({"owner":"org","repo":"repo","pending_prs":pending}))
            .await
            .unwrap();

        assert_eq!(result["has_work"], true);
        assert_eq!(result["outcomes"][0]["type"], "reviews");
        assert_eq!(result["outcomes"][0]["state"], "changes_requested");

        let action = node.post(&store, result).await.unwrap();
        assert!(
            action.as_str() == ACTION_ADDRESS_REVIEW_DISPATCHED
                || action.as_str() == ACTION_REWORK_PROVISION_NEEDED
        );
        let lifecycle = store.lifecycle("T-42").await.unwrap();
        assert_eq!(lifecycle.phase, config::lifecycle::Phase::Building);

        pr.assert_async().await;
        reviews.assert_async().await;
        comments.assert_async().await;
    }

    #[tokio::test]
    async fn unapproved_pr_with_inline_comments_dispatches_rework_instead_of_deferring() {
        let mut server = mockito::Server::new_async().await;
        let pr = server
            .mock("GET", "/repos/org/repo/pulls/42")
            .with_status(200)
            .with_body(r#"{"number":42,"title":"Feature","body":null,"head":{"sha":"head","ref":"feature"},"base":{"ref":"main","sha":"base"},"state":"open","mergeable":true,"merged":false}"#)
            .expect_at_least(1)
            .create_async()
            .await;
        let reviews = server
            .mock(
                "GET",
                "/repos/org/repo/pulls/42/reviews?per_page=100&page=1",
            )
            .with_status(200)
            .with_body("[]")
            .expect_at_least(1)
            .create_async()
            .await;
        let comments = server
            .mock("GET", "/repos/org/repo/pulls/42/comments?per_page=100")
            .with_status(200)
            .with_body(
                r#"[{"id":10,"body":"Please fix error handling","path":"src/main.rs","line":50}]"#,
            )
            .expect_at_least(1)
            .create_async()
            .await;
        let _status = server
            .mock("GET", "/repos/org/repo/commits/head/status")
            .with_status(200)
            .with_body(r#"{"state":"success","total_count":1}"#)
            .expect_at_least(1)
            .create_async()
            .await;
        let _checks = server
            .mock(
                "GET",
                "/repos/org/repo/commits/head/check-suites?per_page=100&page=1",
            )
            .with_status(200)
            .with_body(r#"{"check_suites":[]}"#)
            .expect_at_least(1)
            .create_async()
            .await;

        let client = github::GithubRestClient::with_api_base("test", server.url());
        let mut config = VesselConfig::default();
        config.ci_poll.interval_secs = 1;
        let mut node = VesselNode::new(config);
        node.client = client.clone();
        node.poller = CiPoller::new(node.config.ci_poll.clone(), client);

        let store = SharedStore::new_in_memory();
        *node.lifecycle_store.lock().unwrap() = Some(store.clone());
        let pending = json!([{"number":42,"ticket_id":"T-42"}]);
        store.set(KEY_PENDING_PRS, pending.clone()).await;
        // Ticket is in Submit phase but NOT merge_ready.
        store
            .set(
                "ticket:T-42:status",
                json!({"version":1,"phase":"submit","head":"head","pr_number":42}),
            )
            .await;

        let result = node
            .exec(json!({"owner":"org","repo":"repo","pending_prs":pending}))
            .await
            .unwrap();

        assert_eq!(result["has_work"], true);
        assert_eq!(result["outcomes"][0]["type"], "reviews");
        assert_eq!(result["outcomes"][0]["state"], "comments");

        let action = node.post(&store, result).await.unwrap();
        assert!(
            action.as_str() == ACTION_ADDRESS_REVIEW_DISPATCHED
                || action.as_str() == ACTION_REWORK_PROVISION_NEEDED
        );
        let lifecycle = store.lifecycle("T-42").await.unwrap();
        assert_eq!(lifecycle.phase, config::lifecycle::Phase::Building);

        pr.assert_async().await;
        reviews.assert_async().await;
        comments.assert_async().await;
    }

    #[tokio::test]
    async fn unapproved_pr_with_conflicts_and_successful_ci_dispatches_conflict_rework() {
        let mut server = mockito::Server::new_async().await;
        let pr = server
            .mock("GET", "/repos/org/repo/pulls/42")
            .with_status(200)
            .with_body(r#"{"number":42,"title":"Feature","body":null,"head":{"sha":"head","ref":"feature"},"base":{"ref":"main","sha":"base"},"state":"open","mergeable":false,"merged":false}"#)
            .expect_at_least(1)
            .create_async()
            .await;
        let reviews = server
            .mock(
                "GET",
                "/repos/org/repo/pulls/42/reviews?per_page=100&page=1",
            )
            .with_status(200)
            .with_body("[]")
            .expect_at_least(1)
            .create_async()
            .await;
        let comments = server
            .mock("GET", "/repos/org/repo/pulls/42/comments?per_page=100")
            .with_status(200)
            .with_body("[]")
            .expect_at_least(1)
            .create_async()
            .await;
        let _status = server
            .mock("GET", "/repos/org/repo/commits/head/status")
            .with_status(200)
            .with_body(r#"{"state":"success","total_count":1}"#)
            .expect_at_least(1)
            .create_async()
            .await;
        let _checks = server
            .mock(
                "GET",
                "/repos/org/repo/commits/head/check-suites?per_page=100&page=1",
            )
            .with_status(200)
            .with_body(r#"{"check_suites":[]}"#)
            .expect_at_least(1)
            .create_async()
            .await;
        let _files = server
            .mock("GET", "/repos/org/repo/pulls/42/files")
            .with_status(200)
            .with_body(r#"[{"filename":"src/lib.rs","status":"modified"}]"#)
            .expect_at_least(1)
            .create_async()
            .await;

        let client = github::GithubRestClient::with_api_base("test", server.url());
        let mut config = VesselConfig::default();
        config.ci_poll.interval_secs = 1;
        let mut node = VesselNode::new(config);
        node.client = client.clone();
        node.poller = CiPoller::new(node.config.ci_poll.clone(), client);

        let store = SharedStore::new_in_memory();
        *node.lifecycle_store.lock().unwrap() = Some(store.clone());
        let pending = json!([{"number":42,"ticket_id":"T-42"}]);
        store.set(KEY_PENDING_PRS, pending.clone()).await;
        // Ticket is in Submit phase with missing lifecycle approvals (not merge_ready).
        store
            .set(
                "ticket:T-42:status",
                json!({"version":1,"phase":"submit","head":"head","pr_number":42}),
            )
            .await;

        let result = node
            .exec(json!({"owner":"org","repo":"repo","pending_prs":pending}))
            .await
            .unwrap();

        assert_eq!(result["has_work"], true);
        assert_eq!(result["outcomes"][0]["type"], "conflicts");
        assert_eq!(result["outcomes"][0]["pr_number"], 42);

        let lifecycle = store.lifecycle("T-42").await.unwrap();
        assert_eq!(lifecycle.phase, config::lifecycle::Phase::Building);

        pr.assert_async().await;
        reviews.assert_async().await;
        comments.assert_async().await;
    }

    #[tokio::test]
    async fn test_post_handles_no_work() {
        let store = SharedStore::new_in_memory();

        let config = VesselConfig::default();
        let node = VesselNode::new(config);

        let exec_result = json!({
            "outcomes": [],
            "has_work": false,
        });

        let action = node.post(&store, exec_result).await.unwrap();
        assert_eq!(action.as_str(), "no_work");
    }

    #[tokio::test]
    async fn test_update_ticket_status() {
        let store = SharedStore::new_in_memory();
        store
            .set(
                "tickets",
                json!([
                    {"id": "T-1", "status": {"type": "open"}},
                    {"id": "T-42", "status": {"type": "in_progress"}},
                ]),
            )
            .await;

        let config = VesselConfig::default();
        let node = VesselNode::new(config);

        node.update_ticket_status(&store, "T-42", "merged").await;

        let tickets: Vec<Value> = store.get_typed("tickets").await.unwrap();
        let ticket = tickets.iter().find(|t| t["id"] == "T-42").unwrap();
        assert_eq!(ticket["status"]["type"], "merged");
        assert!(
            serde_json::from_value::<TicketStatus>(ticket["status"].clone()).is_ok(),
            "merged ticket projection must remain typed"
        );
    }

    #[tokio::test]
    async fn test_remove_from_pending_prs() {
        let store = SharedStore::new_in_memory();
        store
            .set(
                "pending_prs",
                json!([
                    {"number": 1, "ticket_id": "T-1"},
                    {"number": 42, "ticket_id": "T-42"},
                    {"number": 100, "ticket_id": "T-100"},
                ]),
            )
            .await;

        let config = VesselConfig::default();
        let node = VesselNode::new(config);

        node.remove_from_pending_prs(&store, 42).await;

        let pending: Vec<Value> = store.get_typed("pending_prs").await.unwrap();
        assert_eq!(pending.len(), 2);
        assert!(pending.iter().all(|pr| pr["number"] != 42));
    }

    #[tokio::test]
    async fn test_post_handles_address_review_outcome() {
        let store = SharedStore::new_in_memory();
        store
            .set(
                "pending_prs",
                json!([{"number": 42, "ticket_id": "T-42", "worker_id": "forge-1"}]),
            )
            .await;
        store
            .set(
                "tickets",
                json!([{"id": "T-42", "status": {"type": "in_progress"}}]),
            )
            .await;

        let config = VesselConfig::default();
        let node = VesselNode::new(config);

        let exec_result = json!({
            "outcomes": [VesselOutcome::Reviews {
                ticket_id: Some("T-42".to_string()),
                pr_number: 42,
                state: "changes_requested".to_string(),
                head_sha: None,
            }],
            "has_work": true,
        });

        // No FORGE chat binding exists in the store, so VESSEL cannot dispatch
        // `/address_review` directly — it must route to NEXUS to provision a
        // FORGE that delivers the persisted rework directive.
        let action = node.post(&store, exec_result).await.unwrap();
        assert_eq!(action.as_str(), ACTION_REWORK_PROVISION_NEEDED);

        // The PR is removed from pending_prs and the dispatch counter is bumped.
        let pending: Vec<Value> = store.get_typed("pending_prs").await.unwrap_or_default();
        assert!(pending.is_empty());
        let attempts: u32 = store
            .get_typed("_address_review_attempts_42")
            .await
            .unwrap_or(0);
        assert_eq!(attempts, 1);

        // The rework directive is persisted for NEXUS to deliver as the new
        // FORGE chat's initial prompt.
        let directive: Option<String> = store.get_typed("ticket:T-42:rework_directive:forge").await;
        assert!(directive.is_some());
    }

    #[tokio::test]
    async fn test_post_reviews_dedup_same_head_does_not_redispatch() {
        let store = SharedStore::new_in_memory();
        store
            .set(
                "pending_prs",
                json!([{
                    "number": 42,
                    "ticket_id": "T-42",
                    "worker_id": "forge-1",
                    "head_sha": "sha-abc",
                }]),
            )
            .await;
        store
            .set(
                "tickets",
                json!([{"id": "T-42", "status": {"type": "in_progress"}}]),
            )
            .await;
        // Already dispatched for this exact head SHA — FORGE is still working.
        store
            .set("_address_review_dispatched_42", json!("sha-abc"))
            .await;

        let config = VesselConfig::default();
        let node = VesselNode::new(config);

        let exec_result = json!({
            "outcomes": [VesselOutcome::Reviews {
                ticket_id: Some("T-42".to_string()),
                pr_number: 42,
                state: "changes_requested".to_string(),
                head_sha: Some("sha-abc".to_string()),
            }],
            "has_work": true,
        });

        let action = node.post(&store, exec_result).await.unwrap();
        // No re-dispatch — nothing to route on (all outcomes deduped).
        assert_eq!(action.as_str(), "no_work");

        // The dispatch counter is NOT incremented for the same head.
        let attempts: u32 = store
            .get_typed("_address_review_attempts_42")
            .await
            .unwrap_or(0);
        assert_eq!(attempts, 0);

        // PR still removed from pending_prs (FORGE is handling it).
        let pending: Vec<Value> = store.get_typed("pending_prs").await.unwrap_or_default();
        assert!(pending.is_empty());
    }

    #[tokio::test]
    async fn test_post_reviews_fresh_head_overrides_stale_pending_prs_and_redispatches() {
        let store = SharedStore::new_in_memory();
        store
            .set(
                "pending_prs",
                json!([{
                    "number": 42,
                    "ticket_id": "T-42",
                    "worker_id": "forge-1",
                    "head_sha": "sha-old",
                }]),
            )
            .await;
        store
            .set(
                "tickets",
                json!([{"id": "T-42", "status": {"type": "in_progress"}}]),
            )
            .await;
        store
            .set("_address_review_dispatched_42", json!("sha-old"))
            .await;

        let config = VesselConfig::default();
        let node = VesselNode::new(config);

        let exec_result = json!({
            "outcomes": [VesselOutcome::Reviews {
                ticket_id: Some("T-42".to_string()),
                pr_number: 42,
                state: "changes_requested".to_string(),
                head_sha: Some("sha-new".to_string()),
            }],
            "has_work": true,
        });

        let action = node.post(&store, exec_result).await.unwrap();
        assert_eq!(action.as_str(), ACTION_REWORK_PROVISION_NEEDED);

        let attempts: u32 = store
            .get_typed("_address_review_attempts_42")
            .await
            .unwrap_or(0);
        assert_eq!(attempts, 1);

        let dispatched_sha: Option<String> = store.get_typed("_address_review_dispatched_42").await;
        assert_eq!(dispatched_sha.as_deref(), Some("sha-new"));
    }

    #[test]
    fn test_post_reviews_resolves_real_forge_slot_for_nexus_reconciliation_ticket() {
        std::thread::Builder::new()
            .stack_size(8 * 1024 * 1024)
            .spawn(|| {
                tokio::runtime::Runtime::new().unwrap().block_on(async {
                    let mut server = mockito::Server::new_async().await;
                    let _chat_mock = server
                        .mock("POST", "/api/v2/chats/chat-42/messages")
                        .with_status(201)
                        .with_header("content-type", "application/json")
                        .with_body(
                            r#"{"id":"msg-1","chat_id":"chat-42","role":"user","content":[]}"#,
                        )
                        .create_async()
                        .await;
                    let _user_mock = server
                        .mock("GET", "/api/v2/users/me")
                        .with_status(200)
                        .with_header("content-type", "application/json")
                        .with_body(
                            r#"{"id":"user-1","username":"coder","email":"coder@example.com","roles":["member"]}"#,
                        )
                        .create_async()
                        .await;

                    let store = SharedStore::new_in_memory();
                    store.set("coder_url", json!(server.url())).await;
                    store.set("coder_api_token", json!("test-token")).await;

                    store
                        .set(
                            "pending_prs",
                            json!([{
                                "number": 42,
                                "ticket_id": "T-42",
                                "worker_id": "forge-1",
                                "head_branch": "forge-1/T-42",
                                "head_sha": "sha-new",
                            }]),
                        )
                        .await;
                    store
                        .set(
                            KEY_WORKER_SLOTS,
                            json!({
                                "forge-1": {
                                    "id": "forge-1",
                                    "status": { "type": "assigned", "ticket_id": "T-42" },
                                    "workspace_id": "ws-1"
                                }
                            }),
                        )
                        .await;
                    store
                        .set(
                            KEY_TICKETS,
                            json!([{
                                "id": "T-42",
                                "title": "Test",
                                "body": "",
                                "priority": 1,
                                "status": {
                                    "type": "completed",
                                    "outcome": "pr_opened",
                                    "worker_id": "nexus-reconciliation"
                                }
                            }]),
                        )
                        .await;
                    store.set("ticket:T-42:chat:forge", json!("chat-42")).await;

                    let config = VesselConfig::default();
                    let node = VesselNode::new(config);

                    let exec_result = json!({
                        "outcomes": [VesselOutcome::Reviews {
                            ticket_id: Some("T-42".to_string()),
                            pr_number: 42,
                            state: "changes_requested".to_string(),
                            head_sha: Some("sha-new".to_string()),
                        }],
                        "has_work": true,
                    });

                    let action = node.post(&store, exec_result).await.unwrap();
                    assert_eq!(action.as_str(), ACTION_ADDRESS_REVIEW_DISPATCHED);

                    let tickets: Vec<Ticket> = store.get_typed(KEY_TICKETS).await.unwrap();
                    let ticket = tickets.iter().find(|t| t.id == "T-42").unwrap();
                    assert_eq!(
                        ticket.status,
                        TicketStatus::InProgress {
                            worker_id: "forge-1".to_string()
                        }
                    );
                });
            })
            .unwrap()
            .join()
            .unwrap();
    }

    #[tokio::test]
    async fn test_rearm_review_prs_clears_marker_even_when_fetch_fails() {
        let store = SharedStore::new_in_memory();
        // FORGE re-armed a PR after addressing a /address_review.
        store.set("_address_review_rearmed_42", json!(true)).await;

        let config = VesselConfig::default();
        let node = VesselNode::new(config);

        // No repo configured → GitHub fetch fails → the marker must still be
        // cleared so VESSEL does not re-process it on every poll, and the call
        // must not panic.
        node.rearm_review_prs(&store, "", "").await;

        let marker_keys = store.keys("_address_review_rearmed_*").await;
        assert!(marker_keys.is_empty(), "re-arm marker should be cleared");
        let pending: Vec<Value> = store.get_typed("pending_prs").await.unwrap_or_default();
        assert!(pending.is_empty());
    }

    #[tokio::test]
    async fn test_rearm_review_prs_does_not_duplicate_existing_pr() {
        let store = SharedStore::new_in_memory();
        store
            .set("pending_prs", json!([{"number": 42, "ticket_id": "T-42"}]))
            .await;
        store.set("_address_review_rearmed_42", json!(true)).await;

        let config = VesselConfig::default();
        let node = VesselNode::new(config);

        node.rearm_review_prs(&store, "", "").await;

        let pending: Vec<Value> = store.get_typed("pending_prs").await.unwrap_or_default();
        // Already tracked → not duplicated.
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0]["number"], 42);
        // Marker still cleared.
        let marker_keys = store.keys("_address_review_rearmed_*").await;
        assert!(marker_keys.is_empty());
    }

    #[test]
    fn build_ci_fix_directive_is_minimal_pointer() {
        let detail = github::CiFailureDetail {
            failed_checks: vec![github::FailedCheck {
                name: "check-lint".to_string(),
                conclusion: "failure".to_string(),
            }],
            still_running: vec![],
            job_logs: vec![(
                "check-lint".to_string(),
                "##[error]lint failed\n".to_string(),
            )],
            annotations: vec![github::CheckAnnotationDetail {
                check_name: "check-lint".to_string(),
                path: "src/api.rs".to_string(),
                start_line: 78,
                message: "missing pagination".to_string(),
            }],
        };
        let d = build_ci_fix_directive(
            42,
            "T-034",
            "forge-1/T-034",
            "CI status: Failure",
            Some(&detail),
        );
        assert!(d.contains("/ci_fix"));
        assert!(d.contains("state: ci_failed"));
        assert!(d.contains("pr: 42"));
        assert!(d.contains("ticket: T-034"));
        assert!(d.contains("branch: forge-1/T-034"));
        assert!(d.contains("reason: CI status: Failure"));
        // Pointer, not a dump: full job-log text must NOT be embedded.
        assert!(!d.contains("##[error]lint failed"));
        assert!(d.contains("check-lint"));
        assert!(d.contains("src/api.rs"));
        // FORGE is told to reuse its existing workspace and re-arm the PR.
        assert!(d.contains("Reuse your existing workspace"));
        assert!(d.contains("openflows-harness status set testing"));
    }

    #[test]
    fn build_ci_fix_directive_omits_missing_parts() {
        let d = build_ci_fix_directive(7, "T-007", "", "CI status: Failure", None);
        assert!(d.contains("/ci_fix"));
        assert!(d.contains("pr: 7"));
        // No branch and no reason lines when absent.
        assert!(!d.contains("branch:"));
        assert!(!d.contains("checks:"));
        assert!(!d.contains("annotations:"));
    }

    #[tokio::test]
    async fn test_recycle_worker_recycles_forge_and_sentinel_pair_to_idle() {
        let store = SharedStore::new_in_memory();
        let node = VesselNode::new(VesselConfig::default());

        let mut slots: HashMap<String, WorkerSlot> = HashMap::new();
        slots.insert(
            "forge-1".to_string(),
            WorkerSlot {
                id: "forge-1".to_string(),
                status: WorkerStatus::Assigned {
                    ticket_id: "T-005".to_string(),
                    issue_url: None,
                },
                workspace_id: Some("ws-forge-1".to_string()),
            },
        );
        slots.insert(
            "sentinel".to_string(),
            WorkerSlot {
                id: "sentinel".to_string(),
                status: WorkerStatus::Assigned {
                    ticket_id: "T-005".to_string(),
                    issue_url: None,
                },
                workspace_id: None,
            },
        );
        store.set(KEY_WORKER_SLOTS, json!(slots)).await;

        let pr = json!({
            "number": 6,
            "ticket_id": "T-005",
            "head_branch": "forge-1/T-005",
        });

        node.recycle_worker(&store, &pr).await;

        let updated_slots: HashMap<String, WorkerSlot> =
            store.get_typed(KEY_WORKER_SLOTS).await.unwrap();

        let forge_slot = updated_slots.get("forge-1").unwrap();
        assert!(
            matches!(forge_slot.status, WorkerStatus::Idle),
            "Forge slot must be recycled to Idle"
        );
        assert_eq!(forge_slot.workspace_id, None);

        let sentinel_slot = updated_slots.get("sentinel").unwrap();
        assert!(
            matches!(sentinel_slot.status, WorkerStatus::Idle),
            "Sentinel slot must be recycled to Idle"
        );
        assert_eq!(sentinel_slot.workspace_id, None);
    }

    #[tokio::test]
    async fn test_recycle_worker_does_not_affect_reassigned_worker() {
        let store = SharedStore::new_in_memory();
        let node = VesselNode::new(VesselConfig::default());

        let mut slots: HashMap<String, WorkerSlot> = HashMap::new();
        // forge-1 was reassigned to T-006 with a new workspace
        slots.insert(
            "forge-1".to_string(),
            WorkerSlot {
                id: "forge-1".to_string(),
                status: WorkerStatus::Assigned {
                    ticket_id: "T-006".to_string(),
                    issue_url: None,
                },
                workspace_id: Some("ws-forge-1-new".to_string()),
            },
        );
        // sentinel is still on T-005
        slots.insert(
            "sentinel".to_string(),
            WorkerSlot {
                id: "sentinel".to_string(),
                status: WorkerStatus::Assigned {
                    ticket_id: "T-005".to_string(),
                    issue_url: None,
                },
                workspace_id: Some("ws-sentinel".to_string()),
            },
        );
        store.set(KEY_WORKER_SLOTS, json!(slots)).await;

        // T-005 has an old workspace recorded
        store
            .set(
                &full_ticket_key("T-005", KEY_TICKET_WORKSPACE, "forge"),
                json!("ws-forge-1-old"),
            )
            .await;

        let pr = json!({
            "number": 6,
            "ticket_id": "T-005",
            "head_branch": "forge-1/T-005",
        });

        node.recycle_worker(&store, &pr).await;

        let updated_slots: HashMap<String, WorkerSlot> =
            store.get_typed(KEY_WORKER_SLOTS).await.unwrap();

        // forge-1 MUST NOT be touched because it is working on T-006
        let forge_slot = updated_slots.get("forge-1").unwrap();
        assert!(
            matches!(&forge_slot.status, WorkerStatus::Assigned { ticket_id, .. } if ticket_id == "T-006"),
            "Forge slot working on T-006 must not be disturbed"
        );
        assert_eq!(
            forge_slot.workspace_id.as_deref(),
            Some("ws-forge-1-new"),
            "Forge slot workspace for new ticket must be preserved"
        );

        // sentinel was on T-005 so it is recycled
        let sentinel_slot = updated_slots.get("sentinel").unwrap();
        assert!(
            matches!(sentinel_slot.status, WorkerStatus::Idle),
            "Sentinel slot must be recycled to Idle"
        );
        assert_eq!(sentinel_slot.workspace_id, None);
    }

    #[tokio::test]
    async fn test_failed_workspace_destruction_preserves_references() {
        let store = SharedStore::new_in_memory();
        let node = VesselNode::new(VesselConfig::default());

        let mut slots: HashMap<String, WorkerSlot> = HashMap::new();
        slots.insert(
            "forge-1".to_string(),
            WorkerSlot {
                id: "forge-1".to_string(),
                status: WorkerStatus::Assigned {
                    ticket_id: "T-010".to_string(),
                    issue_url: None,
                },
                workspace_id: Some("ws-forge-10".to_string()),
            },
        );
        store.set(KEY_WORKER_SLOTS, json!(slots)).await;

        let ws_key = full_ticket_key("T-010", KEY_TICKET_WORKSPACE, "forge");
        assert!(
            store.get_typed::<String>(&ws_key).await.is_none(),
            "Workspace key should not be set yet"
        );

        let pending_prs = vec![json!({
            "number": 10,
            "ticket_id": "T-010",
            "head_branch": "forge-1/T-010",
        })];

        // Attempt destruction when Coder client is unavailable
        node.destroy_coder_workspace_for_pr(&store, &pending_prs, 10)
            .await;

        // The ticket-scoped workspace key MUST be recorded for recovery
        let stored_ws = store.get_typed::<String>(&ws_key).await;
        assert_eq!(
            stored_ws.as_deref(),
            Some("ws-forge-10"),
            "Ticket workspace key must be recorded when deletion fails"
        );

        // When recycled, the slot status becomes Idle and slot workspace_id is cleared
        // so a newly assigned ticket never inherits the old workspace.
        let pr = json!({
            "number": 10,
            "ticket_id": "T-010",
            "head_branch": "forge-1/T-010",
        });
        node.recycle_worker(&store, &pr).await;

        let updated_slots: HashMap<String, WorkerSlot> =
            store.get_typed(KEY_WORKER_SLOTS).await.unwrap();
        let forge_slot = updated_slots.get("forge-1").unwrap();
        assert!(
            matches!(forge_slot.status, WorkerStatus::Idle),
            "Forge slot is set to Idle"
        );
        assert_eq!(
            forge_slot.workspace_id, None,
            "Slot workspace_id must be cleared on Idle so it is not reused"
        );
    }

    #[tokio::test]
    async fn test_sentinel_workspace_found_via_ticket_key_preserves_sentinel_role() {
        let store = SharedStore::new_in_memory();
        let node = VesselNode::new(VesselConfig::default());

        let mut slots: HashMap<String, WorkerSlot> = HashMap::new();
        slots.insert(
            "forge-1".to_string(),
            WorkerSlot {
                id: "forge-1".to_string(),
                status: WorkerStatus::Assigned {
                    ticket_id: "T-020".to_string(),
                    issue_url: None,
                },
                workspace_id: Some("ws-forge-20".to_string()),
            },
        );
        slots.insert(
            "sentinel".to_string(),
            WorkerSlot {
                id: "sentinel".to_string(),
                status: WorkerStatus::Assigned {
                    ticket_id: "T-020".to_string(),
                    issue_url: None,
                },
                workspace_id: None,
            },
        );
        store.set(KEY_WORKER_SLOTS, json!(slots)).await;

        let forge_ws_key = full_ticket_key("T-020", KEY_TICKET_WORKSPACE, "forge");
        let sentinel_ws_key = full_ticket_key("T-020", KEY_TICKET_WORKSPACE, "sentinel");
        store.set(&forge_ws_key, json!("ws-forge-20")).await;
        store.set(&sentinel_ws_key, json!("ws-sentinel-20")).await;

        let pending_prs = vec![json!({
            "number": 20,
            "ticket_id": "T-020",
            "head_branch": "forge-1/T-020",
            "worker_id": "forge-1",
        })];

        // Destruction fails because Coder client is unavailable
        node.destroy_coder_workspace_for_pr(&store, &pending_prs, 20)
            .await;

        // Sentinel workspace MUST NOT overwrite forge workspace or be lost
        let forge_stored = store.get_typed::<String>(&forge_ws_key).await;
        let sentinel_stored = store.get_typed::<String>(&sentinel_ws_key).await;

        assert_eq!(
            forge_stored.as_deref(),
            Some("ws-forge-20"),
            "Forge ticket key must retain its forge workspace ID"
        );
        assert_eq!(
            sentinel_stored.as_deref(),
            Some("ws-sentinel-20"),
            "Sentinel ticket key must retain its sentinel workspace ID under sentinel role"
        );
    }

    #[tokio::test]
    async fn test_cleanup_terminal_ticket_workspaces_retries_and_cleans_up() {
        let mut server = mockito::Server::new_async().await;
        // Mock list_chats
        server
            .mock("GET", "/api/v2/chats")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body("[]")
            .create_async()
            .await;
        // Mock builds delete for forge workspace
        server
            .mock("POST", "/api/v2/workspaces/ws-forge-30/builds")
            .with_status(201)
            .with_header("content-type", "application/json")
            .with_body(r#"{"id":"b1","transition":"delete"}"#)
            .create_async()
            .await;
        // Mock get_workspace returning 404 (deleted)
        server
            .mock("GET", "/api/v2/workspaces/ws-forge-30")
            .with_status(404)
            .create_async()
            .await;
        // Mock builds delete for sentinel workspace
        server
            .mock("POST", "/api/v2/workspaces/ws-sentinel-30/builds")
            .with_status(201)
            .with_header("content-type", "application/json")
            .with_body(r#"{"id":"b2","transition":"delete"}"#)
            .create_async()
            .await;
        server
            .mock("GET", "/api/v2/workspaces/ws-sentinel-30")
            .with_status(404)
            .create_async()
            .await;

        let store = SharedStore::new_in_memory();
        store.set("coder_url", json!(server.url())).await;
        store.set("coder_api_token", json!("token-123")).await;

        let node = VesselNode::new(VesselConfig::default());

        store
            .set(
                KEY_TICKETS,
                json!([{
                    "id": "T-030",
                    "title": "Clean up test",
                    "body": "",
                    "priority": 1,
                    "status": {
                        "type": "merged",
                        "worker_id": "forge-1",
                        "pr_number": 30
                    }
                }]),
            )
            .await;

        let forge_ws_key = full_ticket_key("T-030", KEY_TICKET_WORKSPACE, "forge");
        let sentinel_ws_key = full_ticket_key("T-030", KEY_TICKET_WORKSPACE, "sentinel");
        store.set(&forge_ws_key, json!("ws-forge-30")).await;
        store.set(&sentinel_ws_key, json!("ws-sentinel-30")).await;

        let mut slots = HashMap::new();
        slots.insert(
            "forge-1".to_string(),
            WorkerSlot {
                id: "forge-1".to_string(),
                status: config::state::WorkerStatus::Idle,
                workspace_id: Some("ws-forge-30".to_string()),
            },
        );
        slots.insert(
            "sentinel".to_string(),
            WorkerSlot {
                id: "sentinel".to_string(),
                status: config::state::WorkerStatus::Idle,
                workspace_id: Some("ws-sentinel-30".to_string()),
            },
        );
        store.set(KEY_WORKER_SLOTS, json!(slots)).await;

        node.cleanup_terminal_ticket_workspaces(&store).await;

        assert!(
            store.get_typed::<String>(&forge_ws_key).await.is_none(),
            "Forge workspace key should be cleaned up after successful deletion"
        );
        assert!(
            store.get_typed::<String>(&sentinel_ws_key).await.is_none(),
            "Sentinel workspace key should be cleaned up after successful deletion"
        );

        let updated_slots: HashMap<String, WorkerSlot> =
            store.get_typed(KEY_WORKER_SLOTS).await.unwrap();
        assert_eq!(
            updated_slots["forge-1"].workspace_id, None,
            "forge-1 workspace_id should be cleared"
        );
        assert_eq!(
            updated_slots["sentinel"].workspace_id, None,
            "sentinel workspace_id should be cleared"
        );
    }

    #[tokio::test]
    async fn test_check_pr_needs_rework_stale_approval_does_not_hide_comments() {
        let mut server = mockito::Server::new_async().await;
        // Review approved on old_commit
        let _reviews = server
            .mock("GET", "/repos/org/repo/pulls/42/reviews?per_page=100&page=1")
            .with_status(200)
            .with_body(r#"[{"id":1,"user":{"login":"alice","id":100},"body":"LGTM","state":"APPROVED","submitted_at":"2026-10-08T12:00:00Z","commit_id":"old_commit","author_association":"MEMBER"}]"#)
            .expect_at_least(1)
            .create_async()
            .await;
        // CI success
        let _ci = server
            .mock("GET", "/repos/org/repo/commits/new_commit/status")
            .with_status(200)
            .with_body(r#"{"state":"success"}"#)
            .create_async()
            .await;
        let _runs = server
            .mock("GET", "/repos/org/repo/commits/new_commit/check-runs")
            .with_status(200)
            .with_body(r#"{"total_count":0,"check_runs":[]}"#)
            .create_async()
            .await;
        // Inline review comments present
        let _comments = server
            .mock("GET", "/repos/org/repo/pulls/42/comments?per_page=100")
            .with_status(200)
            .with_body(r#"[{"id":10,"path":"src/lib.rs","user":{"login":"bob","id":200},"body":"Fix this","commit_id":"new_commit","created_at":"2026-10-08T13:00:00Z"}]"#)
            .expect_at_least(1)
            .create_async()
            .await;

        let client = github::GithubRestClient::with_api_base("test", server.url());
        let config = VesselConfig {
            github_token: "test".to_string(),
            ..Default::default()
        };
        let node = VesselNode {
            lifecycle_store: std::sync::Mutex::new(None),
            poller: CiPoller::new(config.ci_poll.clone(), client.clone()),
            merger: PrMerger::new(client.clone(), config.merge_method),
            client,
            config,
            slots_lock: tokio::sync::Mutex::new(()),
        };

        let pr_info = PrInfo {
            number: 42,
            head_sha: "new_commit".to_string(),
            head_branch: "feat".to_string(),
            base_branch: "main".to_string(),
            title: "Feat".to_string(),
            body: None,
            state: pocketflow_core::PrState::Open,
            mergeable: Some(true),
            ticket_id: Some("T-42".to_string()),
        };

        let needs_rework = node.check_pr_needs_rework("org", "repo", &pr_info).await;
        assert!(
            needs_rework,
            "Stale approval on older commit must not hide review comments"
        );
    }

    #[tokio::test]
    async fn test_check_pr_needs_rework_unauthorized_approval_does_not_hide_comments() {
        let mut server = mockito::Server::new_async().await;
        // Review approved on new_commit by outside CONTRIBUTOR (not member/owner)
        let _reviews = server
            .mock("GET", "/repos/org/repo/pulls/42/reviews?per_page=100&page=1")
            .with_status(200)
            .with_body(r#"[{"id":1,"user":{"login":"driveby","id":100},"body":"LGTM","state":"APPROVED","submitted_at":"2026-10-08T12:00:00Z","commit_id":"new_commit","author_association":"CONTRIBUTOR"}]"#)
            .expect_at_least(1)
            .create_async()
            .await;
        // CI success
        let _ci = server
            .mock("GET", "/repos/org/repo/commits/new_commit/status")
            .with_status(200)
            .with_body(r#"{"state":"success"}"#)
            .create_async()
            .await;
        let _runs = server
            .mock("GET", "/repos/org/repo/commits/new_commit/check-runs")
            .with_status(200)
            .with_body(r#"{"total_count":0,"check_runs":[]}"#)
            .create_async()
            .await;
        // Inline review comments present
        let _comments = server
            .mock("GET", "/repos/org/repo/pulls/42/comments?per_page=100")
            .with_status(200)
            .with_body(r#"[{"id":10,"path":"src/lib.rs","user":{"login":"bob","id":200},"body":"Fix this","commit_id":"new_commit","created_at":"2026-10-08T13:00:00Z"}]"#)
            .expect_at_least(1)
            .create_async()
            .await;

        let client = github::GithubRestClient::with_api_base("test", server.url());
        let config = VesselConfig {
            github_token: "test".to_string(),
            ..Default::default()
        };
        let node = VesselNode {
            lifecycle_store: std::sync::Mutex::new(None),
            poller: CiPoller::new(config.ci_poll.clone(), client.clone()),
            merger: PrMerger::new(client.clone(), config.merge_method),
            client,
            config,
            slots_lock: tokio::sync::Mutex::new(()),
        };

        let pr_info = PrInfo {
            number: 42,
            head_sha: "new_commit".to_string(),
            head_branch: "feat".to_string(),
            base_branch: "main".to_string(),
            title: "Feat".to_string(),
            body: None,
            state: pocketflow_core::PrState::Open,
            mergeable: Some(true),
            ticket_id: Some("T-42".to_string()),
        };

        let needs_rework = node.check_pr_needs_rework("org", "repo", &pr_info).await;
        assert!(
            needs_rework,
            "Unauthorized drive-by approval must not hide review comments"
        );
    }
}

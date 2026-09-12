// crates/agent-nexus/src/hooks/bootstrap.rs
//! Slice A — `session_start` → `model_context` bootstrap.
//!
//! When a role's (empty) chat starts or resumes, the consumer returns injected
//! context so the agent realizes its environment and current state: persona,
//! the skill/command paths it may rely on, and exact resume guidance read from
//! Redis. Output is a single string capped at `model_context` size (16 KiB, D9).
//!
//! All reads are read-only against the SharedStore. Full personas are trimmed;
//! skill/command *paths* are preferred over embedding full file contents.

use super::context::{read_ticket_state, resolve_chat};
use pocketflow_core::SharedStore;
use serde_json::Value;
use std::path::PathBuf;

/// Maximum bytes for the assembled `model_context` (Coder caps at ~16 KiB).
pub const MODEL_CONTEXT_BYTE_LIMIT: usize = 16 * 1024;

/// Paths the Controller resolves at boot and hands to the consumer so it can
/// point an agent at its persona / skills / commands without filesystem
/// discovery of its own.
#[derive(Debug, Clone, Default)]
pub struct HookBootstrapContext {
    /// Absolute paths to each role's persona file, e.g. `forge.agent.md`.
    pub persona_by_role: std::collections::HashMap<String, PathBuf>,
    /// Absolute directory containing the skill `.md` files.
    pub skills_dir: Option<PathBuf>,
    /// Absolute directory containing the command `.md` files.
    pub commands_dir: Option<PathBuf>,
}

impl HookBootstrapContext {
    pub fn persona_path(&self, role: &str) -> Option<PathBuf> {
        self.persona_by_role.get(role).cloned()
    }

    pub fn role_skill_paths(&self, role: &str) -> Vec<PathBuf> {
        let dir = match self.skills_dir.as_ref() {
            Some(d) => d,
            None => return Vec::new(),
        };
        let read = match std::fs::read_dir(dir) {
            Ok(r) => r,
            Err(_) => return Vec::new(),
        };
        let prefix = format!("{role}-");
        read.filter_map(|e| e.ok())
            .filter(|e| {
                let name = e.file_name().to_string_lossy().to_string();
                name.starts_with(&prefix) || name.starts_with("shared-")
            })
            .map(|e| e.path())
            .collect()
    }

    pub fn all_command_paths(&self) -> Vec<PathBuf> {
        let dir = match &self.commands_dir {
            Some(d) => d,
            None => return Vec::new(),
        };
        let read = match std::fs::read_dir(dir) {
            Ok(r) => r,
            Err(_) => return Vec::new(),
        };
        read.filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().ends_with(".md"))
            .map(|e| e.path())
            .collect()
    }

    /// Read a persona file's first ~3KB as a trim; returns empty on any error.
    pub fn read_persona_trim(&self, role: &str) -> String {
        let path = match self.persona_path(role) {
            Some(p) => p,
            None => return String::new(),
        };
        read_trimmed(&path, 3000)
    }
}

/// Read a file, trimming to `limit` bytes and adding a truncation marker.
fn read_trimmed(path: &PathBuf, limit: usize) -> String {
    match std::fs::read_to_string(path) {
        Ok(s) => {
            let s = s.trim();
            if s.len() <= limit {
                s.to_string()
            } else {
                format!("{}…[trimmed]", &s[..limit])
            }
        }
        Err(_) => String::new(),
    }
}

/// Assemble the `model_context` string for a `session_start` dispatch.
///
/// Returns `None` (observation-only) when we cannot resolve role/ticket, so the
/// dispatch fails open.
pub async fn build_bootstrap_context(
    store: &SharedStore,
    chat_id: &str,
    dispatch: &Value,
    ctx: &HookBootstrapContext,
) -> Option<String> {
    let hc = resolve_chat(store, chat_id).await;
    let role = hc.role.clone()?;
    let ticket = hc.ticket_id.clone()?;
    let st = read_ticket_state(store, &ticket).await;

    let mut parts: Vec<String> = Vec::new();

    let persona = ctx.read_persona_trim(&role);
    if !persona.is_empty() {
        parts.push(format!("# Persona ({role})\n{persona}"));
    }

    // Skill paths
    let skills = ctx.role_skill_paths(&role);
    if !skills.is_empty() {
        let lines = skills
            .iter()
            .map(|p| format!("  - {}", p.display()))
            .collect::<Vec<_>>()
            .join("\n");
        parts.push(format!("# Skills ({role})\n{lines}"));
    }

    // Command paths
    let commands = ctx.all_command_paths();
    if !commands.is_empty() {
        let lines = commands
            .iter()
            .map(|p| format!("  - {}", p.display()))
            .collect::<Vec<_>>()
            .join("\n");
        parts.push(format!("# Harness commands (see these docs)\n{lines}"));
    }

    // Dispatch payload (title/body)
    if let Some(title) = dispatch.get("title").and_then(|v| v.as_str()) {
        parts.push(format!("# Assignment\nTicket: {ticket}\nTitle: {title}"));
    }

    // Resume state
    let phase = st.phase.clone().unwrap_or_else(|| "unset".to_string());
    let mut resume = format!("# Current state\nPhase: {phase}\n");
    if st.plan_exists {
        resume.push_str("Plan: present (already uploaded)\n");
    }
    if st.gate_approved {
        resume.push_str("Planning gate: approved\n");
    }
    if st.pr_recorded {
        resume.push_str("PR: recorded\n");
    }
    if st.handoff_exists {
        resume.push_str("Handoff: present\n");
    }
    resume.push_str(&format!(
        "You are resuming this ticket. Re-check `openflows-harness status get` and `dispatch read`."
    ));
    parts.push(resume);

    let joined = parts.join("\n\n---\n\n");
    Some(trim_to_limit(&joined))
}

/// Trim assembled output to the Coder `model_context` cap.
pub fn trim_to_limit(s: &str) -> String {
    if s.len() <= MODEL_CONTEXT_BYTE_LIMIT {
        s.to_string()
    } else {
        let cut = &s[..MODEL_CONTEXT_BYTE_LIMIT];
        format!("{cut}…[trimmed to model_context limit]")
    }
}

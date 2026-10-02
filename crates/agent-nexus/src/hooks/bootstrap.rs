// crates/agent-nexus/src/hooks/bootstrap.rs
//! Slice A — `session_start` → `model_context` bootstrap.
//!
//! When a role's (empty) chat starts or resumes, the consumer returns injected
//! context so the agent realizes its environment and current state. The persona,
//! skill and command excerpts are accompanied by workspace paths to the full
//! instructions. Excerpts are not a replacement for reading the relevant files.
//! Output is a single string capped at `model_context` size (16 KiB, D9).
//!
//! All reads are read-only against the SharedStore + local orchestration tree.

use super::context::{read_ticket_state, resolve_chat};
use pocketflow_core::SharedStore;
use serde_json::Value;
use std::path::{Path, PathBuf};

/// Maximum bytes for the assembled `model_context` (Coder caps at ~16 KiB).
pub const MODEL_CONTEXT_BYTE_LIMIT: usize = 16 * 1024;

/// Per-file byte budget for command/skill content embedded in the bootstrap.
const CONTENT_BYTE_BUDGET: usize = 1024;

/// Paths the Controller resolves at boot and hands to the consumer so it can
/// load the agent's persona / skills / commands without filesystem discovery.
#[derive(Debug, Clone, Default)]
pub struct HookBootstrapContext {
    /// Absolute paths to each role's persona file, e.g. `forge.agent.md`.
    pub persona_by_role: std::collections::HashMap<String, PathBuf>,
    /// Absolute directory containing role skill directories (`<role>-*/SKILL.md`).
    pub skills_dir: Option<PathBuf>,
    /// Exact registry-selected skills, shared with workspace provisioning.
    pub skills_by_role: std::collections::HashMap<String, Vec<String>>,
    /// Absolute directory containing command `.md` files.
    pub commands_dir: Option<PathBuf>,
}

/// A single command/skill available to the agent, with its resolved content.
#[derive(Debug, Clone)]
pub struct AgentDoc {
    pub name: String,
    /// Absolute path to the doc file (commands) or its SKILL.md (skills).
    pub path: PathBuf,
    /// Trimmed markdown content the agent will actually read.
    pub content: String,
}

impl HookBootstrapContext {
    pub fn persona_path(&self, role: &str) -> Option<PathBuf> {
        self.persona_by_role.get(role).cloned()
    }

    /// Resolve + read the content of the role's skill `SKILL.md` files.
    /// Only registry-selected skills are included, matching provisioning.
    pub fn role_skills(&self, role: &str) -> Vec<AgentDoc> {
        let dir = match self.skills_dir.as_ref() {
            Some(d) => d,
            None => return Vec::new(),
        };
        self.skills_by_role
            .get(role)
            .into_iter()
            .flatten()
            .filter_map(|name| {
                let path = dir.join(name).join("SKILL.md");
                skill_read(&path).map(|content| AgentDoc {
                    name: name.clone(),
                    path,
                    content,
                })
            })
            .collect()
    }

    /// Resolve + read the content of every orchestration command doc.
    pub fn all_commands(&self) -> Vec<AgentDoc> {
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
            .filter_map(|e| {
                let path = e.path();
                read_trimmed(&path, CONTENT_BYTE_BUDGET).map(|content| AgentDoc {
                    name: e
                        .file_name()
                        .to_string_lossy()
                        .trim_end_matches(".md")
                        .to_string(),
                    path,
                    content,
                })
            })
            .collect()
    }

    /// Read a persona file's first ~3KB as a trim; returns empty on any error.
    pub fn read_persona_trim(&self, role: &str) -> String {
        let path = match self.persona_path(role) {
            Some(p) => p,
            None => return String::new(),
        };
        read_trimmed(&path, 3000).unwrap_or_default()
    }
}

/// Read a `SKILL.md` at `dir/SKILL.md`, trimmed to the per-file budget.
fn skill_read(skill_md: &Path) -> Option<String> {
    read_trimmed(skill_md, CONTENT_BYTE_BUDGET)
}

/// Read a file, trimming to `limit` bytes and adding a truncation marker.
/// Truncation is UTF-8-safe: never splits a multi-byte character, which would
/// panic on `&s[..limit]` with non-ASCII command/skill/persona content.
fn read_trimmed(path: &Path, limit: usize) -> Option<String> {
    std::fs::read_to_string(path).ok().map(|s| {
        let s = s.trim();
        if s.len() <= limit {
            s.to_string()
        } else {
            format!("{}…[trimmed]", truncate_utf8(s, limit))
        }
    })
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

    let persona = ctx.read_persona_trim(&role);
    let commands = ctx.all_commands();
    let skills = ctx.role_skills(&role);

    // Sections are assembled in priority order and `trim_to_limit` keeps the
    // FRONT, so the essential assignment + current-state + next-step guidance
    // are placed first and survive even when the total overflows 16 KiB. The
    // verbose command/skill content is appended last and truncated away first.
    let mut parts: Vec<String> = Vec::new();

    // Assignment + current state first: the agent must know what ticket it is
    // on, what phase it is in, and what to do immediately.
    let phase = st.phase.clone().unwrap_or_else(|| "unset".to_string());
    let mut top = format!("# Assignment\nTicket: {ticket}\nCurrent phase: {phase}\n");
    if let Some(title) = dispatch.get("title").and_then(|v| v.as_str()) {
        top.push_str(&format!("Title: {title}\n"));
    }
    if st.plan_exists {
        top.push_str("Plan: present (already uploaded)\n");
    }
    if st.gate_approved {
        top.push_str("Planning gate: approved\n");
    }
    if st.pr_recorded {
        top.push_str("PR: recorded\n");
    }
    if st.handoff_exists {
        top.push_str("Handoff: present\n");
    }
    // Role-level first action: forge starts by planning, sentinel by reviewing.
    match role.as_str() {
        "forge" => {
            top.push_str("Your next action follows the current phase, not a fresh assignment. ");
            top.push_str(&super::guard::phase_guidance(&phase, st.plan_exists, &role));
            top.push('\n');
        }
        "sentinel" => {
            top.push_str(
                "Your first action: review what FORGE has submitted — read the plan/PR \
                 and record your verdict with `openflows-harness review submit` or the \
                 planning/testing gates with `gate decide --revision <N> --round <R> --report review.md` (see the /review and /plan commands).\n",
            );
        }
        _ => {}
    }
    top.push_str(
        "You are working this ticket. Re-check `openflows-harness status get` and \
         `openflows-harness dispatch read`.\n",
    );
    if role == "forge" {
        let plan_path = super::guard::chat_plan_path(chat_id);
        top.push_str(&format!("Current chat plan path: `{plan_path}`. During planning write the plan here, then run `openflows-harness plan write --file {plan_path}`. Do not write `/home/coder/PLAN.md` or guess a different chat ID.\n"));
    }
    parts.push(top);

    // Put complete workspace paths before optional excerpts so truncation never
    // hides how to retrieve the authoritative instructions.
    let mut index = format!("# Required instructions\nRead `/home/coder/workspace/AGENTS.md` and `/home/coder/workspace/{role}.agent.md`. File tools require absolute paths; resolve repository-relative paths against `/home/coder/workspace`. Startup excerpts are incomplete; read the full relevant files before acting.\n");
    if let Some(names) = ctx.skills_by_role.get(&role) {
        for name in names {
            index.push_str(&format!(
                "Skill: `/home/coder/workspace/.agents/skills/{name}/SKILL.md`\n"
            ));
        }
    }
    index.push_str("Full workflow commands: `/home/coder/workspace/.agents/commands/<name>.md`. These documents describe shell commands; execute the documented `openflows-harness` commands.\n");
    parts.push(index);

    if !persona.is_empty() {
        parts.push(format!("# Persona ({role})\n{persona}"));
    }

    if !commands.is_empty() {
        let mut block = String::from("# Harness commands (use these as-is)\n");
        for c in &commands {
            let entry = format!("\n## /{}\n{}\n", c.name, c.content);
            block.push_str(&entry);
        }
        parts.push(block);
    }

    if !skills.is_empty() {
        let mut block = String::from("# Skills\n");
        for s in &skills {
            block.push_str(&format!("\n## {}\n{}\n", s.name, s.content));
        }
        parts.push(block);
    }

    let joined = parts.join("\n\n---\n\n");
    Some(trim_to_limit(&joined))
}

/// Trim assembled output to the Coder `model_context` cap.
pub fn trim_to_limit(s: &str) -> String {
    if s.len() <= MODEL_CONTEXT_BYTE_LIMIT {
        s.to_string()
    } else {
        let marker = "...[trimmed to model_context limit]";
        let content_limit = MODEL_CONTEXT_BYTE_LIMIT.saturating_sub(marker.len());
        let cut = truncate_utf8(s, content_limit);
        format!("{cut}{marker}")
    }
}

/// Return a prefix of `s` holding at most `byte_limit` bytes without splitting a
/// multi-byte UTF-8 character in the middle. Rust's `&s[..n]` panics on such a
/// split, which is why the raw byte slice must never be used here.
fn truncate_utf8(s: &str, byte_limit: usize) -> &str {
    if s.len() <= byte_limit {
        return s;
    }
    let mut end = byte_limit;
    if !s.is_char_boundary(end) {
        // Back off to the previous char boundary (safe: ASCII at end is 1 byte).
        while end > 0 && !s.is_char_boundary(end) {
            end -= 1;
        }
    }
    &s[..end]
}

#[cfg(test)]
mod registry_skill_tests {
    use super::*;

    #[test]
    fn skills_follow_registry_not_role_prefix() {
        let root = std::env::temp_dir().join(format!("bootstrap-skills-{}", uuid::Uuid::new_v4()));
        for name in ["forge-unlisted", "custom-planning"] {
            std::fs::create_dir_all(root.join(name)).unwrap();
            std::fs::write(
                root.join(name).join("SKILL.md"),
                "# Skill\nRequired instructions",
            )
            .unwrap();
        }
        let ctx = HookBootstrapContext {
            skills_dir: Some(root.clone()),
            skills_by_role: [("forge".into(), vec!["custom-planning".into()])].into(),
            ..Default::default()
        };
        let skills = ctx.role_skills("forge");
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].name, "custom-planning");
        std::fs::remove_dir_all(root).unwrap();
    }
}

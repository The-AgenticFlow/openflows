// crates/agent-lore/src/types.rs
use anyhow::Result;
use config::Registry;
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone)]
pub struct LoreConfig {
    pub workspace_root: std::path::PathBuf,
    pub persona_path: std::path::PathBuf,
    pub docs_dir: std::path::PathBuf,
    pub adr_dir: std::path::PathBuf,
    pub github_token: String,
}

impl LoreConfig {
    pub fn new(
        workspace_root: impl Into<std::path::PathBuf>,
        persona_path: impl Into<std::path::PathBuf>,
    ) -> Self {
        let workspace_root = workspace_root.into();
        Self {
            docs_dir: workspace_root.join("docs"),
            adr_dir: workspace_root.join("docs").join("adr"),
            workspace_root,
            persona_path: persona_path.into(),
            github_token: String::new(),
        }
    }

    /// Create config with explicit paths and token resolved from registry.
    pub fn new_with_registry(
        workspace_root: impl Into<std::path::PathBuf>,
        persona_path: impl Into<std::path::PathBuf>,
        registry_path: impl AsRef<Path>,
    ) -> Result<Self> {
        let registry = Registry::load(registry_path)?;
        let github_token = registry.resolve_github_token("lore")?;
        tracing::info!("LORE resolved GitHub token from registry");

        let workspace_root = workspace_root.into();
        Ok(Self {
            docs_dir: workspace_root.join("docs"),
            adr_dir: workspace_root.join("docs").join("adr"),
            workspace_root,
            persona_path: persona_path.into(),
            github_token,
        })
    }

    /// Create config using per-agent token from registry (if configured).
    /// Falls back to GITHUB_TOKEN.
    pub fn from_registry(registry_path: impl AsRef<Path>) -> Result<Self> {
        let registry = Registry::load(registry_path)?;
        let github_token = registry.resolve_github_token("lore")?;

        let workspace_root = env_workspace_root();
        let persona_path = workspace_root
            .join("orchestration")
            .join("agent")
            .join("agents")
            .join("lore.agent.md");
        Ok(Self {
            docs_dir: workspace_root.join("docs"),
            adr_dir: workspace_root.join("docs").join("adr"),
            workspace_root,
            persona_path,
            github_token,
        })
    }

    pub fn from_env() -> Self {
        let env = config::EnvConfig::from_env().ok();
        let workspace_root = env_workspace_root();
        let persona_path = workspace_root
            .join("orchestration")
            .join("agent")
            .join("agents")
            .join("lore.agent.md");

        // Try workspace_root first, then current_dir as fallback
        let registry_path = if workspace_root
            .join("orchestration/agent/registry.json")
            .exists()
        {
            Some(workspace_root.join("orchestration/agent/registry.json"))
        } else {
            std::env::current_dir()
                .ok()
                .map(|p| p.join("orchestration").join("agent").join("registry.json"))
        };

        let github_token = match registry_path {
            Some(path) if path.exists() => {
                match Registry::load(&path).and_then(|reg| reg.resolve_github_token("lore")) {
                    Ok(token) => {
                        tracing::info!(registry = %path.display(), "LORE resolved GitHub token from registry");
                        token
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, registry = %path.display(), "LORE failed to resolve token from registry, falling back");
                        env.as_ref()
                            .and_then(|e| e.github.token.clone())
                            .unwrap_or_default()
                    }
                }
            }
            _ => {
                tracing::warn!("LORE: registry.json not found, falling back to GITHUB_TOKEN");
                env.as_ref()
                    .and_then(|e| e.github.token.clone())
                    .unwrap_or_default()
            }
        };

        Self {
            docs_dir: workspace_root.join("docs"),
            adr_dir: workspace_root.join("docs").join("adr"),
            workspace_root,
            persona_path,
            github_token,
        }
    }
}

/// Resolve the effective workspace root from the centralized config, falling
/// back to the current directory. Shared by the config constructors so the
/// resolution logic is defined once.
fn env_workspace_root() -> std::path::PathBuf {
    config::EnvConfig::from_env()
        .ok()
        .and_then(|e| e.agent.effective_workspace_root())
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_default())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum LoreTask {
    ChangelogUpdate {
        ticket_id: String,
        pr_number: u64,
        changes: Vec<String>,
        pr_title: Option<String>,
        pr_body: Option<String>,
    },
    AdrGeneration {
        decision: ArchitecturalDecision,
    },
    Retrospective {
        sprint_id: String,
    },
    DocSync {
        scope: DocScope,
    },
    ReadmeUpdate {
        ticket_id: String,
        feature_summary: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DocScope {
    Full,
    Api,
    UserGuide,
    Architecture,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum LoreOutcome {
    ChangelogUpdated { entry: String },
    AdrWritten { path: String, adr_id: String },
    RetrospectiveGenerated { path: String },
    DocsSynced { updated: Vec<String> },
    ReadmeUpdated { sections: Vec<String> },
    NoWork,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArchitecturalDecision {
    pub title: String,
    pub context: String,
    pub decision: String,
    pub consequences: String,
    pub ticket_id: String,
    pub date: String,
    pub pr_number: Option<u64>,
}

impl ArchitecturalDecision {
    pub fn new(
        title: impl Into<String>,
        context: impl Into<String>,
        decision: impl Into<String>,
        consequences: impl Into<String>,
        ticket_id: impl Into<String>,
        pr_number: Option<u64>,
    ) -> Self {
        Self {
            title: title.into(),
            context: context.into(),
            decision: decision.into(),
            consequences: consequences.into(),
            ticket_id: ticket_id.into(),
            date: chrono::Utc::now().format("%Y-%m-%d").to_string(),
            pr_number,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ChangeCategory {
    Added,
    Changed,
    Deprecated,
    Removed,
    Fixed,
    Security,
}

impl ChangeCategory {
    pub fn as_str(&self) -> &'static str {
        match self {
            ChangeCategory::Added => "Added",
            ChangeCategory::Changed => "Changed",
            ChangeCategory::Deprecated => "Deprecated",
            ChangeCategory::Removed => "Removed",
            ChangeCategory::Fixed => "Fixed",
            ChangeCategory::Security => "Security",
        }
    }

    pub fn from_pr_title(title: &str) -> Self {
        let lower = title.to_lowercase();
        if lower.contains("fix") || lower.contains("bug") {
            ChangeCategory::Fixed
        } else if lower.contains("add") || lower.contains("implement") || lower.contains("new") {
            ChangeCategory::Added
        } else if lower.contains("remove") || lower.contains("delete") {
            ChangeCategory::Removed
        } else if lower.contains("deprecate") {
            ChangeCategory::Deprecated
        } else if lower.contains("security") || lower.contains("vuln") {
            ChangeCategory::Security
        } else {
            ChangeCategory::Changed
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MergedTicketInfo {
    pub ticket_id: String,
    pub pr_number: u64,
    pub pr_title: String,
    pub pr_body: Option<String>,
    pub sha: String,
    pub merged_at: String,
    pub changes: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SprintMetrics {
    pub sprint_id: String,
    pub tickets_completed: u32,
    pub tickets_carried_over: u32,
    pub blockers: Vec<String>,
    pub highlights: Vec<String>,
    pub lessons_learned: Vec<String>,
}

//! Provisioning for worker workspaces (Coder-only redesign).
//!
//! Materializes `.agents/skills/`, `.mcp.json`, standards files, and the role
//! persona into a Coder workspace via `CoderTransport`, driven by `registry.json`
//! schema v2.

use anyhow::{Context, Result};
use serde_json::Value;
use std::path::{Path, PathBuf};
use tracing::{info, warn};

use crate::transport::WorkspaceTransport;

/// Provisions configuration files and skills into worker workspaces.
pub struct Provisioner {
    /// Orchestrator source directory (contains orchestration/).
    orchestrator_dir: PathBuf,
}

impl Provisioner {
    pub fn new(orchestrator_dir: impl Into<PathBuf>) -> Self {
        Self {
            orchestrator_dir: orchestrator_dir.into(),
        }
    }

    /// Materialize all provisioning artifacts into a workspace via the transport.
    ///
    /// Reads `registry.json` for the given role and provisions:
    /// 1. `.agents/skills/<name>/SKILL.md` for each listed skill
    /// 2. `.mcp.json` from the role's mcp config
    /// 3. Standards files (CODING.md, SECURITY.md, REVIEW.md)
    /// 4. Role persona file
    pub async fn provision_role(
        &self,
        transport: &dyn WorkspaceTransport,
        role: &str,
        registry: &config::Registry,
    ) -> Result<()> {
        let entry = registry
            .get(role)
            .with_context(|| format!("Role '{}' not found in registry", role))?;

        if !entry.enabled {
            info!(role, "Role is disabled — skipping provisioning");
            return Ok(());
        }

        // 1. Provision skills
        for skill_name in &entry.skills {
            let skill_dir = self
                .orchestrator_dir
                .join("orchestration")
                .join("plugin")
                .join("skills")
                .join(skill_name);

            let skill_md = skill_dir.join("SKILL.md");
            if skill_md.exists() {
                let target = format!(".agents/skills/{}/SKILL.md", skill_name);
                transport
                    .create_dir_all(&format!(".agents/skills/{}", skill_name))
                    .await?;
                transport.copy_file(&skill_md, &target).await.map_err(|e| {
                    anyhow::anyhow!("Failed to provision skill {}: {}", skill_name, e)
                })?;
                info!(skill = skill_name, role, "Provisioned skill");
            } else {
                warn!(skill = skill_name, "Skill directory not found — skipping");
            }
        }

        // 2. Provision .mcp.json
        if !entry.mcp.is_null() && !entry.mcp.as_object().map(|o| o.is_empty()).unwrap_or(true) {
            let mcp_json = serde_json::to_string_pretty(&entry.mcp)?;
            transport
                .write_file(".mcp.json", &mcp_json)
                .await
                .context("Failed to write .mcp.json")?;
            info!(role, "Provisioned .mcp.json");
        }

        // 3. Provision standards files
        let standards_dir = self
            .orchestrator_dir
            .join("orchestration")
            .join("agent")
            .join("standards");

        for standard in &["CODING.md", "SECURITY.md", "REVIEW.md"] {
            let path = standards_dir.join(standard);
            if path.exists() {
                transport
                    .copy_file(&path, standard)
                    .await
                    .map_err(|e| anyhow::anyhow!("Failed to provision {}: {}", standard, e))?;
                info!(standard, role, "Provisioned standard");
            }
        }

        // 4. Provision role persona
        let persona_path = self
            .orchestrator_dir
            .join("orchestration")
            .join("agent")
            .join("agents")
            .join(format!("{}.agent.md", role));

        if persona_path.exists() {
            transport
                .copy_file(&persona_path, &format!("{}.agent.md", role))
                .await
                .map_err(|e| anyhow::anyhow!("Failed to provision persona: {}", e))?;
            info!(role, "Provisioned persona");
        }

        Ok(())
    }
}

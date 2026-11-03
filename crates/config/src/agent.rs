// crates/config/src/agent.rs
//
// Parses orchestration/agent/{id}.agent.md files.
// Format: YAML frontmatter (between --- markers) + Markdown body.
// The `instances` field is deliberately absent — registry.json owns that.

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Parsed identity block from .agent.md YAML frontmatter.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentDef {
    pub id: String,
    pub role: String,
    pub cli: String,
    pub active: bool,
    pub github: String, // GitHub bot username
    pub slack: String,  // Slack handle e.g. "@forge"
    /// Capabilities / persona text (the Markdown body after the frontmatter).
    pub persona: String,
    pub permissions: AgentPermissions,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct AgentPermissions {
    pub allow: Vec<String>,
    pub deny: Vec<String>,
}

impl AgentDef {
    /// Load and parse a .agent.md file.
    pub fn load(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("Failed to read agent def at {}", path.display()))?;
        Self::parse(&content)
    }

    /// Parse agent definition from a string (frontmatter + body).
    pub fn parse(content: &str) -> Result<Self> {
        // Split on --- markers
        let parts: Vec<&str> = content.splitn(3, "---").collect();
        if parts.len() < 3 {
            bail!("Agent .md file missing YAML frontmatter (expected --- delimiters)");
        }
        let frontmatter = parts[1].trim();
        let body = parts[2].trim().to_string();

        // Parse the YAML frontmatter as a loose map first
        let raw: serde_json::Value =
            serde_saphyr::from_str(frontmatter).context("Failed to parse YAML frontmatter")?;

        let id = raw["id"].as_str().unwrap_or("").to_string();
        let role = raw["role"].as_str().unwrap_or("").to_string();
        let cli = raw["cli"].as_str().unwrap_or("claude").to_string();
        let active = raw["active"].as_bool().unwrap_or(true);
        let github = raw["github"].as_str().unwrap_or("").to_string();
        let slack = raw["slack"].as_str().unwrap_or("").to_string();

        // Parse permissions block from body (simple line scan)
        let permissions = parse_permissions(&body);

        Ok(AgentDef {
            id,
            role,
            cli,
            active,
            github,
            slack,
            persona: body,
            permissions,
        })
    }
}

/// Extract allow/deny lists from Markdown body lines like:
/// `allow: [Read, Write, Bash]`
/// `deny: [WebFetch, Slack]`
fn parse_permissions(body: &str) -> AgentPermissions {
    let mut allow = vec![];
    let mut deny = vec![];

    for line in body.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("allow:") {
            allow = parse_bracket_list(rest);
        } else if let Some(rest) = trimmed.strip_prefix("deny:") {
            deny = parse_bracket_list(rest);
        }
    }

    AgentPermissions { allow, deny }
}

fn parse_bracket_list(s: &str) -> Vec<String> {
    let s = s.trim().trim_start_matches('[').trim_end_matches(']');
    s.split(',')
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const FORGE_MD: &str = r#"---
id: forge
role: builder
cli: claude
active: true
github: forge-bot
slack: "@forge"
---

# Persona
You are FORGE, a pragmatic senior engineer.

# Permissions
allow: [Read, Write, Bash, Edit, GitPush]
deny: [WebFetch, Slack]
"#;

    #[test]
    fn test_parse_frontmatter() {
        let def = AgentDef::parse(FORGE_MD).unwrap();
        assert_eq!(def.id, "forge");
        assert_eq!(def.role, "builder");
        assert_eq!(def.cli, "claude");
        assert!(def.active);
        assert_eq!(def.github, "forge-bot");
        assert_eq!(def.slack, "@forge");
    }

    #[test]
    fn test_parse_permissions() {
        let def = AgentDef::parse(FORGE_MD).unwrap();
        assert!(def.permissions.allow.contains(&"Read".to_string()));
        assert!(def.permissions.allow.contains(&"GitPush".to_string()));
        assert!(def.permissions.deny.contains(&"Slack".to_string()));
        assert!(!def.permissions.deny.contains(&"Read".to_string()));
    }

    #[test]
    fn test_persona_body_included() {
        let def = AgentDef::parse(FORGE_MD).unwrap();
        assert!(def.persona.contains("pragmatic senior engineer"));
    }

    #[test]
    fn test_missing_frontmatter_errors() {
        let result = AgentDef::parse("no frontmatter here");
        assert!(result.is_err());
    }
}

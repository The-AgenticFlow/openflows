use anyhow::Context;
use std::path::PathBuf;
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
    /// 4. Role persona file (as `<role>.agent.md` and `AGENTS.md`)
    pub async fn provision_role(
        &self,
        transport: &dyn WorkspaceTransport,
        role: &str,
        registry: &config::Registry,
    ) -> anyhow::Result<()> {
        let entry = registry
            .team
            .iter()
            .find(|entry| entry.id == role)
            .ok_or_else(|| anyhow::anyhow!("Role '{}' not found in registry", role))?;

        if !entry.enabled {
            info!(role, "Role is disabled — skipping provisioning");
            return Ok(());
        }

        // Required skills must be available before an agent can start.
        transport
            .create_dir_all(".agents/skills")
            .await
            .context("Failed to create required skills directory")?;
        for skill_name in &entry.skills {
            let source = self
                .orchestrator_dir
                .join("orchestration/plugin/skills")
                .join(skill_name)
                .join("SKILL.md");
            transport
                .copy_file(&source, &format!(".agents/skills/{skill_name}/SKILL.md"))
                .await
                .with_context(|| format!("Failed to provision required skill {skill_name}"))?;
        }

        // Keep complete command documents accessible beyond startup excerpts.
        let commands = self.orchestrator_dir.join("orchestration/plugin/commands");
        if commands.is_dir() {
            for entry in std::fs::read_dir(&commands)? {
                let path = entry?.path();
                if path.extension().and_then(|s| s.to_str()) == Some("md") {
                    let name = path.file_name().unwrap().to_string_lossy();
                    transport
                        .copy_file(&path, &format!(".agents/commands/{name}"))
                        .await
                        .context("Failed to provision command instructions")?;
                }
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

        anyhow::ensure!(
            persona_path.is_file(),
            "Required persona missing: {}",
            persona_path.display()
        );
        {
            transport
                .copy_file(&persona_path, &format!("{}.agent.md", role))
                .await
                .map_err(|e| anyhow::anyhow!("Failed to provision persona: {}", e))?;
            info!(role, "Provisioned persona");

            // Also materialize the persona as the workspace's AGENTS.md. Coder's
            // Coder Agents reads AGENTS.md from the agent's working directory (and
            // ~/.coder/AGENTS.md) and injects it into the system prompt for every
            // conversation in this workspace, so the persona is delivered
            // server-side and persists across chats instead of being bundled into a
            // fragile first request.
            transport
                .copy_file(&persona_path, "AGENTS.md")
                .await
                .map_err(|e| anyhow::anyhow!("Failed to provision AGENTS.md persona: {}", e))?;
            info!(role, "Provisioned AGENTS.md persona");
        }

        // 5. Provision client-side hooks (Claude Code / Codex plugin style)
        //
        // Copies `orchestration/plugin/hooks/{role}/*.sh` into the workspace and
        // writes a `settings.json` mapping hook events to those scripts, exactly
        // the way Claude Code and Codex plugins are consumed. This keeps the
        // existing in-workspace policy hooks (pre_bash_guard, pre_write_check,
        // stop_require_artifact, ...) alive when a CLI agent is used, and mirrors
        // them in `.agents/` for agents that read that directory.
        //
        // NOTE: the default Coder Chats API agent does NOT execute these shell
        // hooks — Coder's own server-side `agent-lifecycle-hooks` webhook is the
        // equivalent there. Both seams are provisioned so either execution engine
        // has the policy available.
        let hooks_dir = self
            .orchestrator_dir
            .join("orchestration")
            .join("plugin")
            .join("hooks")
            .join(role);

        if hooks_dir.is_dir() {
            // Copy each role hook script into ~/.agents/hooks/{role}/ and
            // ~/.claude/hooks/{role}/ so both discovery conventions find them.
            let mut script_entries = Vec::new();
            for entry in std::fs::read_dir(&hooks_dir)? {
                let entry = entry?;
                if !entry.file_type()?.is_file() {
                    continue;
                }
                let name = entry.file_name().to_string_lossy().to_string();
                if !name.ends_with(".sh") {
                    continue;
                }
                script_entries.push((name.clone(), entry.path()));
                let rel = name.trim_end_matches(".sh");
                for base in [".agents/hooks", ".claude/hooks"] {
                    let target = format!("{base}/{role}/{name}");
                    match transport.copy_file(&entry.path(), &target).await {
                        Ok(_) => {
                            info!(role, script = %name, target = %target, "Provisioned hook script")
                        }
                        Err(e) => {
                            warn!(role, script = %name, error = %e, "Failed to copy hook script")
                        }
                    }
                    // Also drop an extensionless alias for backends that map
                    // event names without `.sh` (e.g. `.agents/hooks/{role}/SessionStart`).
                    let alias = format!("{base}/{role}/{rel}");
                    let _ = transport.symlink_or_copy(&entry.path(), &alias).await;
                }
            }

            // Write a Claude Code style settings.json wiring hook events to the
            // copied scripts (client-side execution).
            if !script_entries.is_empty() {
                let settings = build_hook_settings(role, &script_entries);
                let claude_settings = merge_hook_settings(
                    transport
                        .read_file(".claude/settings.json")
                        .await
                        .ok()
                        .as_deref(),
                    settings.clone(),
                );
                let agents_settings = merge_hook_settings(
                    transport
                        .read_file(".agents/settings.json")
                        .await
                        .ok()
                        .as_deref(),
                    settings,
                );
                if let (Ok(claude_json), Ok(agents_json)) = (
                    serde_json::to_string_pretty(&claude_settings),
                    serde_json::to_string_pretty(&agents_settings),
                ) {
                    let _ = transport
                        .write_file(".claude/settings.json", &claude_json)
                        .await;
                    let _ = transport
                        .write_file(".agents/settings.json", &agents_json)
                        .await;
                    info!(
                        role,
                        count = script_entries.len(),
                        "Provisioned client hook settings"
                    );
                }
            }
        } else {
            info!(role, dir = %hooks_dir.display(), "No role hook directory; skipped client hooks");
        }

        Ok(())
    }
}

/// Map OpenFlows role hook scripts to Claude Code / Codex hook event names and
/// build a `settings.json` that the client agent executes. Unknown event names
/// are preserved as custom hooks so nothing is silently dropped.
fn build_hook_settings(role: &str, scripts: &[(String, std::path::PathBuf)]) -> serde_json::Value {
    use serde_json::{json, Map, Value};

    // Canonical Claude Code event names.
    let mut events: Map<String, Value> = Map::new();

    for (name, _path) in scripts {
        let stem = name.trim_end_matches(".sh").to_string();
        let canonical = canonical_hook_event(&stem);
        let entry = json!({
            "hooks": [ { "type": "command", "command": format!(".agents/hooks/{role}/{name}") } ],
            "enabled": true
        });
        // If a role maps two scripts to one canonical event, expose both.
        match events.get_mut(&canonical) {
            Some(existing) => {
                if let Some(arr) = existing.get_mut("hooks").and_then(|h| h.as_array_mut()) {
                    arr.push(json!({ "type": "command", "command": format!(".agents/hooks/{role}/{name}") }));
                }
            }
            None => {
                events.insert(canonical, entry);
            }
        }
    }

    json!({
        "hooks": events,
        "openflows": { "role": role }
    })
}

fn merge_hook_settings(existing: Option<&str>, generated: serde_json::Value) -> serde_json::Value {
    use serde_json::Value;

    let mut out = existing
        .and_then(|raw| serde_json::from_str::<Value>(raw).ok())
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_default();

    let generated_obj = generated.as_object().cloned().unwrap_or_default();

    if let Some(generated_hooks) = generated_obj.get("hooks").and_then(|v| v.as_object()) {
        let mut hooks = out
            .remove("hooks")
            .and_then(|v| v.as_object().cloned())
            .unwrap_or_default();

        for (event, generated_event) in generated_hooks {
            match hooks.get_mut(event) {
                Some(existing_event) => {
                    merge_hook_event(existing_event, generated_event.clone());
                }
                None => {
                    hooks.insert(event.clone(), generated_event.clone());
                }
            }
        }
        out.insert("hooks".to_string(), Value::Object(hooks));
    }

    if let Some(generated_openflows) = generated_obj.get("openflows").and_then(|v| v.as_object()) {
        let mut openflows = out
            .remove("openflows")
            .and_then(|v| v.as_object().cloned())
            .unwrap_or_default();
        for (key, value) in generated_openflows {
            openflows.insert(key.clone(), value.clone());
        }
        out.insert("openflows".to_string(), Value::Object(openflows));
    }

    for (key, value) in generated_obj {
        out.entry(key).or_insert(value);
    }

    Value::Object(out)
}

fn merge_hook_event(existing: &mut serde_json::Value, generated: serde_json::Value) {
    use serde_json::Value;

    let Some(existing_obj) = existing.as_object_mut() else {
        *existing = generated;
        return;
    };
    let Some(generated_obj) = generated.as_object() else {
        return;
    };

    if let Some(generated_hooks) = generated_obj.get("hooks").and_then(|v| v.as_array()) {
        let existing_hooks = existing_obj
            .entry("hooks")
            .or_insert_with(|| Value::Array(Vec::new()));
        if let Some(existing_hooks) = existing_hooks.as_array_mut() {
            for hook in generated_hooks {
                if !existing_hooks.contains(hook) {
                    existing_hooks.push(hook.clone());
                }
            }
        }
    }

    for (key, value) in generated_obj {
        if key != "hooks" {
            existing_obj.entry(key.clone()).or_insert(value.clone());
        }
    }
}

/// Translate an OpenFlows hook stem to the canonical (Claude Code / Codex)
/// event name. Unknown stems pass through unchanged.
fn canonical_hook_event(stem: &str) -> String {
    match stem {
        "session_start" | "session-start" | "init-session" => "SessionStart".to_string(),
        "pre_bash_guard" | "pre_tool_use" => "PreToolUse".to_string(),
        "pre_write_check" | "pre_write" => "PreWrite".to_string(),
        "post_write_lint" | "post_tool_use" => "PostToolUse".to_string(),
        "pre_compact_handoff" | "pre_compact" => "PreCompact".to_string(),
        "post_compact" => "PostCompact".to_string(),
        "stop_require_artifact" | "stop_require_eval" | "stop" => "Stop".to_string(),
        "subagent_start" => "SubagentStart".to_string(),
        "subagent_stop" => "SubagentStop".to_string(),
        "log_decision" | "log-merge-status" | "log_merge_status" => "Stop".to_string(),
        "post_write_validate" => "PostToolUse".to_string(),
        other => {
            // Preserve custom names, capitalized to look like an event.
            let mut c = other.chars();
            match c.next() {
                Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
                None => other.to_string(),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use config::registry::{Registry, RegistryEntry};
    use std::collections::HashMap;
    use std::path::Path;

    /// In-memory transport that records all file writes for assertions.
    #[derive(Default)]
    struct MemTransport {
        files: std::sync::Mutex<HashMap<String, String>>,
    }

    impl MemTransport {
        fn written(&self, path: &str) -> Option<String> {
            self.files.lock().unwrap().get(path).cloned()
        }
    }

    #[async_trait]
    impl WorkspaceTransport for MemTransport {
        async fn read_file(&self, path: &str) -> anyhow::Result<String> {
            self.files
                .lock()
                .unwrap()
                .get(path)
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("file not found: {}", path))
        }
        async fn write_file(&self, path: &str, content: &str) -> anyhow::Result<()> {
            self.files
                .lock()
                .unwrap()
                .insert(path.to_string(), content.to_string());
            Ok(())
        }
        async fn execute(&self, _command: &str) -> anyhow::Result<crate::transport::CommandOutput> {
            Ok(crate::transport::CommandOutput {
                stdout: String::new(),
                stderr: String::new(),
                exit_code: 0,
            })
        }
        async fn list_directory(
            &self,
            _path: &str,
        ) -> anyhow::Result<Vec<crate::transport::DirEntry>> {
            Ok(vec![])
        }
        async fn symlink_or_copy(&self, _source: &Path, _target: &str) -> anyhow::Result<()> {
            Ok(())
        }
        async fn create_dir_all(&self, _path: &str) -> anyhow::Result<()> {
            Ok(())
        }
        async fn path_exists(&self, path: &str) -> bool {
            self.files.lock().unwrap().contains_key(path)
        }
        async fn remove_dir_all(&self, _path: &str) -> anyhow::Result<()> {
            Ok(())
        }
        async fn copy_file(&self, source_local: &Path, target: &str) -> anyhow::Result<()> {
            let content = std::fs::read_to_string(source_local)?;
            self.write_file(target, &content).await
        }
    }

    fn persona_dir() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let agents = dir.path().join("orchestration/agent/agents");
        std::fs::create_dir_all(&agents).unwrap();
        std::fs::write(
            agents.join("forge.agent.md"),
            "# Forge persona\nBuild awesome code.\n",
        )
        .unwrap();
        dir
    }

    fn registry() -> Registry {
        Registry {
            default_cli: "claude".to_string(),
            allowed_domains: vec![],
            team: vec![
                RegistryEntry {
                    id: "forge".to_string(),
                    enabled: true,
                    max_instances: 1,
                    skills: vec![],
                    mcp: serde_json::Value::Null,
                    cli: String::new(),
                    model_backend: None,
                    routing_key: None,
                    github_token_env: None,
                    allowed_domains: None,
                    coder_module: None,
                },
                RegistryEntry {
                    id: "lore".to_string(),
                    enabled: false,
                    max_instances: 1,
                    skills: vec![],
                    mcp: serde_json::Value::Null,
                    cli: String::new(),
                    model_backend: None,
                    routing_key: None,
                    github_token_env: None,
                    allowed_domains: None,
                    coder_module: None,
                },
            ],
        }
    }

    #[tokio::test]
    async fn required_instruction_files_cannot_be_missing() {
        let orch = tempfile::tempdir().unwrap();
        let transport = MemTransport::default();
        assert!(Provisioner::new(orch.path())
            .provision_role(&transport, "forge", &registry())
            .await
            .is_err());
        let orch = persona_dir();
        let mut reg = registry();
        reg.team[0].skills.push("missing-skill".into());
        assert!(Provisioner::new(orch.path())
            .provision_role(&transport, "forge", &reg)
            .await
            .is_err());
    }

    #[tokio::test]
    async fn provisions_agents_md_persona_for_enabled_role() {
        let orch = persona_dir();
        let commands = orch.path().join("orchestration/plugin/commands");
        std::fs::create_dir_all(&commands).unwrap();
        std::fs::write(commands.join("plan.md"), "Read the full planning skill").unwrap();
        let transport = MemTransport::default();
        let provisioner = Provisioner::new(orch.path());
        provisioner
            .provision_role(&transport, "forge", &registry())
            .await
            .unwrap();

        let agents_md = transport.written("AGENTS.md").expect("AGENTS.md written");
        assert!(agents_md.contains("Forge persona"));
        let persona = transport
            .written("forge.agent.md")
            .expect("forge.agent.md written");
        assert!(persona.contains("Forge persona"));
        assert_eq!(
            transport.written(".agents/commands/plan.md").as_deref(),
            Some("Read the full planning skill")
        );
    }

    #[tokio::test]
    async fn skips_agents_md_for_disabled_role() {
        let orch = persona_dir();
        let transport = MemTransport::default();
        let provisioner = Provisioner::new(orch.path());
        provisioner
            .provision_role(&transport, "lore", &registry())
            .await
            .unwrap();

        assert_eq!(transport.written("AGENTS.md"), None);
        assert_eq!(transport.written("lore.agent.md"), None);
    }

    #[test]
    fn canonicalizes_openflows_hook_stems() {
        assert_eq!(canonical_hook_event("session_start"), "SessionStart");
        assert_eq!(canonical_hook_event("pre_bash_guard"), "PreToolUse");
        assert_eq!(canonical_hook_event("pre_write_check"), "PreWrite");
        assert_eq!(canonical_hook_event("post_write_lint"), "PostToolUse");
        assert_eq!(canonical_hook_event("pre_compact_handoff"), "PreCompact");
        assert_eq!(canonical_hook_event("stop_require_artifact"), "Stop");
        assert_eq!(canonical_hook_event("subagent_start"), "SubagentStart");
        assert_eq!(canonical_hook_event("subagent_stop"), "SubagentStop");
        // Unknown stems pass through, capitalized.
        assert_eq!(canonical_hook_event("my_custom_thing"), "My_custom_thing");
    }

    #[test]
    fn builds_claude_settings_json_from_scripts() {
        let scripts = vec![
            ("pre_bash_guard.sh".to_string(), PathBuf::from("unused")),
            ("session_start.sh".to_string(), PathBuf::from("unused")),
            (
                "stop_require_artifact.sh".to_string(),
                PathBuf::from("unused"),
            ),
        ];
        let settings = build_hook_settings("forge", &scripts);
        let hooks = settings["hooks"].as_object().unwrap();
        assert!(hooks.contains_key("PreToolUse"));
        assert!(hooks.contains_key("SessionStart"));
        assert!(hooks.contains_key("Stop"));
        assert_eq!(settings["openflows"]["role"], "forge");
    }

    #[test]
    fn merges_hook_settings_without_dropping_existing_fields() {
        let existing = serde_json::json!({
            "permissions": { "allow": ["Bash(cargo test)"] },
            "hooks": {
                "PreToolUse": {
                    "hooks": [
                        { "type": "command", "command": "custom/pre.sh" }
                    ],
                    "enabled": true
                }
            },
            "openflows": { "tenant": "demo" }
        })
        .to_string();
        let generated = build_hook_settings(
            "forge",
            &[("pre_bash_guard.sh".to_string(), PathBuf::from("unused"))],
        );

        let merged = merge_hook_settings(Some(&existing), generated);
        assert_eq!(merged["permissions"]["allow"][0], "Bash(cargo test)");
        assert_eq!(merged["openflows"]["tenant"], "demo");
        assert_eq!(merged["openflows"]["role"], "forge");

        let hooks = merged["hooks"]["PreToolUse"]["hooks"].as_array().unwrap();
        assert_eq!(hooks.len(), 2);
        assert!(hooks
            .iter()
            .any(|h| h["command"] == ".agents/hooks/forge/pre_bash_guard.sh"));
        assert!(hooks.iter().any(|h| h["command"] == "custom/pre.sh"));
    }

    #[tokio::test]
    async fn provisions_client_hooks_into_workspace() {
        // Build a temp orchestration tree with a forge hooks dir.
        let dir = tempfile::tempdir().unwrap();
        let agents = dir.path().join("orchestration/agent/agents");
        std::fs::create_dir_all(&agents).unwrap();
        std::fs::write(
            agents.join("forge.agent.md"),
            "# Forge persona\nBuild awesome code.\n",
        )
        .unwrap();
        let hooks = dir.path().join("orchestration/plugin/hooks/forge");
        std::fs::create_dir_all(&hooks).unwrap();
        std::fs::write(hooks.join("pre_bash_guard.sh"), "#!/bin/bash\ncmd=$(cat)\n").unwrap();
        std::fs::write(hooks.join("session_start.sh"), "#!/bin/bash\necho hi\n").unwrap();

        let transport = MemTransport::default();
        let provisioner = Provisioner::new(dir.path());
        provisioner
            .provision_role(&transport, "forge", &registry())
            .await
            .unwrap();

        // Scripts copied into both discovery conventions.
        assert!(transport
            .written(".agents/hooks/forge/pre_bash_guard.sh")
            .is_some());
        assert!(transport
            .written(".claude/hooks/forge/pre_bash_guard.sh")
            .is_some());

        // settings.json written for the client agent.
        let settings = transport
            .written(".claude/settings.json")
            .expect("client settings.json written");
        assert!(settings.contains("PreToolUse"));
        assert!(settings.contains("SessionStart"));
    }
}

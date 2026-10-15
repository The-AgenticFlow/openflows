use anyhow::{Context, Result};
use config::Envconfig;
use tracing::{info, warn};

use crate::bundled::bundled_files;

const ORCHESTRATION_VERSION: &str = env!("CARGO_PKG_VERSION");

pub struct BundledFile {
    pub relative_path: &'static str,
    pub content: &'static str,
}

pub struct OrchestrationResolver {
    candidates: Vec<std::path::PathBuf>,
    orchestrator_dir: std::path::PathBuf,
}

impl OrchestrationResolver {
    pub fn new() -> Result<Self> {
        let tenant = config::TenantConfig::init_from_env()?;
        let openflows_home_path = Some(tenant.openflows_home());

        let mut candidates: Vec<std::path::PathBuf> = Vec::new();

        if let Some(ref home) = openflows_home_path {
            candidates.push(home.clone());
        }
        if let Ok(exe) = std::env::current_exe() {
            if let Some(dir) = exe.parent() {
                if !dir.as_os_str().is_empty() {
                    candidates.push(dir.to_path_buf());
                }
                if let Some(parent) = dir.parent() {
                    if !parent.as_os_str().is_empty() {
                        candidates.push(parent.to_path_buf());
                    }
                }
            }
        }
        if let Ok(cwd) = std::env::current_dir() {
            candidates.push(cwd);
        }

        let (orchestrator_dir, found_registry) = candidates
            .iter()
            .find_map(|dir| {
                let registry = dir.join("orchestration/agent/registry.json");
                if registry.exists() {
                    // Reject candidates that are inside an orchestration subdirectory
                    // to prevent doubled paths like /foo/orchestration/orchestration/agent/
                    let dir_str = dir.to_string_lossy();
                    if dir_str.ends_with("/orchestration/agent")
                        || dir_str.ends_with("/orchestration")
                        || dir_str.ends_with("/orchestration/agent/")
                        || dir_str.ends_with("/orchestration/")
                    {
                        warn!(dir = %dir.display(), "Skipping candidate inside orchestration subdirectory — would cause doubled paths");
                        return None;
                    }
                    Some((dir.clone(), true))
                } else {
                    None
                }
            })
            .unwrap_or_else(|| {
                let home = tenant.openflows_home();
                (home, false)
            });

        if found_registry {
            info!(dir = %orchestrator_dir.display(), "Found existing registry at");
        }

        Ok(Self {
            candidates,
            orchestrator_dir,
        })
    }

    fn write_version_file(&self, orch_dir: &std::path::Path) -> Result<()> {
        let version_path = orch_dir.join(".version");
        std::fs::write(&version_path, ORCHESTRATION_VERSION).with_context(|| {
            format!("Failed to write version file to {}", version_path.display())
        })?;
        Ok(())
    }

    fn read_disk_version(&self, orch_dir: &std::path::Path) -> Option<String> {
        let version_path = orch_dir.join(".version");
        std::fs::read_to_string(version_path)
            .ok()
            .map(|s| s.trim().to_string())
    }

    /// Add the skill required by current SENTINEL instructions without resetting
    /// user configuration or dropping fields unknown to the typed registry.
    fn ensure_sentinel_testing_skill(&self) -> Result<()> {
        use std::io::Write;
        let path = self.registry_path().canonicalize()?;
        let mut registry: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path)?)
                .context("Failed to read registry for SENTINEL skill upgrade")?;
        let team = registry
            .get_mut("team")
            .and_then(|v| v.as_array_mut())
            .context("Registry team must be an array")?;
        let mut changed = false;
        for role in team {
            if role.get("id").and_then(|v| v.as_str()) != Some("sentinel") {
                continue;
            }
            let role = role
                .as_object_mut()
                .context("SENTINEL registry entry must be an object")?;
            let skills = role
                .entry("skills")
                .or_insert_with(|| serde_json::json!([]))
                .as_array_mut()
                .context("SENTINEL skills must be an array")?;
            if !skills
                .iter()
                .any(|skill| skill.as_str() == Some("sentinel-testing"))
            {
                skills.push(serde_json::json!("sentinel-testing"));
                changed = true;
            }
        }
        if changed {
            let mut file = tempfile::NamedTempFile::new_in(path.parent().unwrap())?;
            file.as_file()
                .set_permissions(std::fs::metadata(&path)?.permissions())?;
            writeln!(file, "{}", serde_json::to_string_pretty(&registry)?)?;
            file.as_file().sync_all()?;
            file.persist(&path)
                .context("Failed to persist SENTINEL skill upgrade")?;
            info!("Added required sentinel-testing skill to existing registry; preserved other settings");
        }
        Ok(())
    }

    pub fn ensure_orchestration_dir(&self) -> Result<std::path::PathBuf> {
        let orch_dir = self.orchestrator_dir.join("orchestration");

        // Read the previously recorded version BEFORE writing anything,
        // so we can detect stale on-disk files.
        let disk_version = self.read_disk_version(&orch_dir);

        let mut written = 0usize;

        for file in bundled_files() {
            let target = orch_dir.join(file.relative_path);
            if target.exists() {
                continue;
            }
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent)
                    .with_context(|| format!("Failed to create directory {}", parent.display()))?;
            }
            std::fs::write(&target, file.content)
                .with_context(|| format!("Failed to write bundled file {}", target.display()))?;

            // Shell scripts need executable permissions so the
            // orchestration system can invoke them as hooks.
            if file.relative_path.ends_with(".sh") {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o755))
                        .with_context(|| {
                            format!("Failed to set permissions on {}", target.display())
                        })?;
                }
            }

            written += 1;
            info!(path = %target.display(), "Wrote bundled orchestration file");
        }

        self.ensure_sentinel_testing_skill()?;
        self.write_version_file(&orch_dir)?;

        if written > 0 {
            info!(
                dir = %orch_dir.display(),
                written,
                version = ORCHESTRATION_VERSION,
                "Materialized bundled orchestration files"
            );
        }

        if let Some(ref dv) = disk_version {
            if dv != ORCHESTRATION_VERSION {
                warn!(
                    disk_version = %dv,
                    bundled_version = ORCHESTRATION_VERSION,
                    dir = %orch_dir.display(),
                    "Orchestration files on disk are from an older version ({}). \
                     Run 'openflows --reset-orchestration' to update them.",
                    dv
                );
            }
        }

        Ok(orch_dir)
    }

    pub fn reset_orchestration_dir(&self) -> Result<std::path::PathBuf> {
        let orch_dir = self.orchestrator_dir.join("orchestration");
        let mut written = 0usize;

        for file in bundled_files() {
            let target = orch_dir.join(file.relative_path);

            // Preserve user-customized registry.json so reset doesn't
            // discard agent toggles, model backends, etc. configured via setup.
            // If registry.json is corrupted, users can delete it manually and
            // re-run --reset-orchestration to restore defaults.
            if file.relative_path == "agent/registry.json" && target.exists() {
                info!(
                    path = %target.display(),
                    "Preserving existing registry.json — manual edits will not be overwritten. \
                     To reset it, delete this file and re-run --reset-orchestration."
                );
                continue;
            }

            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent)
                    .with_context(|| format!("Failed to create directory {}", parent.display()))?;
            }
            std::fs::write(&target, file.content)
                .with_context(|| format!("Failed to write bundled file {}", target.display()))?;

            if file.relative_path.ends_with(".sh") {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o755))
                        .with_context(|| {
                            format!("Failed to set permissions on {}", target.display())
                        })?;
                }
            }

            written += 1;
        }

        self.ensure_sentinel_testing_skill()?;
        self.write_version_file(&orch_dir)?;

        info!(
            dir = %orch_dir.display(),
            written,
            version = ORCHESTRATION_VERSION,
            "Reset orchestration files to bundled defaults (preserved user registry)"
        );

        Ok(orch_dir)
    }

    pub fn orchestrator_dir(&self) -> &std::path::Path {
        &self.orchestrator_dir
    }

    pub fn registry_path(&self) -> std::path::PathBuf {
        self.orchestrator_dir
            .join("orchestration/agent/registry.json")
    }

    pub fn persona_path(&self, filename: &str) -> std::path::PathBuf {
        let relative = format!("orchestration/agent/agents/{}", filename);
        let direct = self.orchestrator_dir.join(&relative);
        if direct.exists() {
            return direct;
        }
        for candidate in &self.candidates {
            let path = candidate.join(&relative);
            if path.exists() {
                return path;
            }
        }
        direct
    }

    pub fn validate(&self) -> Result<()> {
        let orch_dir = self.orchestrator_dir.join("orchestration");
        let registry = orch_dir.join("agent/registry.json");
        if !registry.exists() {
            anyhow::bail!(
                "orchestration/agent/registry.json not found at {}\n\
                 Searched: {}\n\
                 Run 'openflows --reset-orchestration' to regenerate all files from defaults.",
                registry.display(),
                self.candidates
                    .iter()
                    .map(|c| c.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }

        let required_personas = [
            "nexus.agent.md",
            "forge.agent.md",
            "sentinel.agent.md",
            "vessel.agent.md",
        ];

        let agents_dir = orch_dir.join("agent/agents");
        let mut missing_personas = Vec::new();
        for name in &required_personas {
            if !agents_dir.join(name).exists() {
                missing_personas.push(*name);
            }
        }

        if !missing_personas.is_empty() {
            anyhow::bail!(
                "Missing required persona files in {}: {}\n\
                 The orchestrator_dir resolved to: {}\n\
                 Run 'openflows --reset-orchestration' to regenerate all files from defaults.",
                agents_dir.display(),
                missing_personas.join(", "),
                self.orchestrator_dir.display()
            );
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use provisioner::{CommandOutput, DirEntry, Provisioner, WorkspaceTransport};
    use std::path::{Path, PathBuf};

    // Exercise real provisioning reads/copies, with a local fixture replacing SSH.
    struct LocalWorkspace(PathBuf);

    #[async_trait]
    impl WorkspaceTransport for LocalWorkspace {
        async fn read_file(&self, path: &str) -> Result<String> {
            Ok(std::fs::read_to_string(self.0.join(path))?)
        }
        async fn write_file(&self, path: &str, content: &str) -> Result<()> {
            let target = self.0.join(path);
            std::fs::create_dir_all(target.parent().unwrap())?;
            Ok(std::fs::write(target, content)?)
        }
        async fn copy_file(&self, source: &Path, target: &str) -> Result<()> {
            self.write_file(target, &std::fs::read_to_string(source)?)
                .await
        }
        async fn create_dir_all(&self, path: &str) -> Result<()> {
            Ok(std::fs::create_dir_all(self.0.join(path))?)
        }
        async fn path_exists(&self, path: &str) -> bool {
            self.0.join(path).exists()
        }
        async fn execute(&self, command: &str) -> Result<CommandOutput> {
            let output = std::process::Command::new("bash")
                .args(["-c", command])
                .current_dir(&self.0)
                .output()?;
            Ok(CommandOutput {
                stdout: String::from_utf8(output.stdout)?,
                stderr: String::from_utf8(output.stderr)?,
                exit_code: output.status.code().unwrap_or(-1),
            })
        }
        async fn list_directory(&self, _path: &str) -> Result<Vec<DirEntry>> {
            anyhow::bail!("bundle provisioning should not list workspace directories")
        }
        async fn symlink_or_copy(&self, source: &Path, target: &str) -> Result<()> {
            self.copy_file(source, target).await
        }
        async fn remove_dir_all(&self, path: &str) -> Result<()> {
            Ok(std::fs::remove_dir_all(self.0.join(path))?)
        }
    }

    #[tokio::test]
    async fn materialized_bundle_provisions_all_required_sentinel_skills() {
        let installation = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        // No source checkout/candidate fallback: only the embedded bundle exists.
        let resolver = OrchestrationResolver {
            candidates: vec![],
            orchestrator_dir: installation.path().to_path_buf(),
        };
        let orchestration = resolver.ensure_orchestration_dir().unwrap();
        let registry = config::Registry::load(resolver.registry_path()).unwrap();
        let sentinel = registry.get("sentinel").unwrap();
        assert!(sentinel
            .skills
            .iter()
            .any(|name| name == "sentinel-testing"));
        // Catch future required-skill omissions for every enabled role as well.
        for role in registry.team.iter().filter(|role| role.enabled) {
            for skill in &role.skills {
                assert!(
                    orchestration
                        .join("plugin/skills")
                        .join(skill)
                        .join("SKILL.md")
                        .is_file(),
                    "{} requires unbundled skill {skill}",
                    role.id
                );
            }
        }
        let transport = LocalWorkspace(workspace.path().to_path_buf());
        assert_eq!(transport.execute("git init -q").await.unwrap().exit_code, 0);
        Provisioner::new(installation.path())
            .provision_role(&transport, "sentinel", &registry)
            .await
            .unwrap();
        for skill in &sentinel.skills {
            let bundled = std::fs::read_to_string(
                orchestration
                    .join("plugin/skills")
                    .join(skill)
                    .join("SKILL.md"),
            )
            .unwrap();
            let delivered = transport
                .read_file(&format!(".agents/skills/{skill}/SKILL.md"))
                .await
                .unwrap();
            assert_eq!(delivered, bundled, "complete skill must reach SENTINEL");
        }
        let status = transport.execute("git status --porcelain").await.unwrap();
        assert_eq!(status.exit_code, 0, "{}", status.stderr);
        assert!(
            status.stdout.is_empty(),
            "generated instructions must stay out of candidate commits"
        );
        assert!(transport
            .read_file(".git/info/exclude")
            .await
            .unwrap()
            .contains("/.agents/skills/sentinel-testing/SKILL.md"));
        assert_eq!(
            transport.read_file("AGENTS.md").await.unwrap(),
            std::fs::read_to_string(orchestration.join("agent/agents/sentinel.agent.md")).unwrap()
        );
    }
    #[tokio::test]
    async fn upgrade_preserves_custom_registry_and_delivers_required_skill() {
        for reset in [false, true] {
            let installation = tempfile::tempdir().unwrap();
            let workspace = tempfile::tempdir().unwrap();
            let resolver = OrchestrationResolver {
                candidates: vec![],
                orchestrator_dir: installation.path().to_path_buf(),
            };
            std::fs::create_dir_all(resolver.registry_path().parent().unwrap()).unwrap();
            let mut expected = serde_json::json!({
                "default_cli": "codex", "allowed_domains": ["example.test"],
                "team": [
                    {"id": "forge", "enabled": false, "max_instances": 7, "skills": ["custom-forge"]},
                    {"id": "sentinel", "enabled": true, "cli": "goose", "max_instances": 3,
                     "skills": ["sentinel-criteria", "sentinel-review"],
                     "mcp": {"custom": "retained"}, "allowed_domains": ["role.example.test"]}
                ]
            });
            std::fs::write(
                resolver.registry_path(),
                serde_json::to_string_pretty(&expected).unwrap(),
            )
            .unwrap();
            if reset {
                resolver.reset_orchestration_dir().unwrap();
            } else {
                resolver.ensure_orchestration_dir().unwrap();
            }
            expected["team"][1]["skills"]
                .as_array_mut()
                .unwrap()
                .push(serde_json::json!("sentinel-testing"));
            let upgraded = std::fs::read_to_string(resolver.registry_path()).unwrap();
            assert_eq!(
                serde_json::from_str::<serde_json::Value>(&upgraded).unwrap(),
                expected
            );
            // Repeated startup/reset adds no duplicates and does not rewrite the registry.
            resolver.ensure_orchestration_dir().unwrap();
            resolver.reset_orchestration_dir().unwrap();
            assert_eq!(
                std::fs::read_to_string(resolver.registry_path()).unwrap(),
                upgraded
            );
            let registry = config::Registry::load(resolver.registry_path()).unwrap();
            let transport = LocalWorkspace(workspace.path().to_path_buf());
            Provisioner::new(installation.path())
                .provision_role(&transport, "sentinel", &registry)
                .await
                .unwrap();
            assert_eq!(
                transport
                    .read_file(".agents/skills/sentinel-testing/SKILL.md")
                    .await
                    .unwrap(),
                include_str!("../../orchestration/plugin/skills/sentinel-testing/SKILL.md")
            );
        }
    }

    #[test]
    fn skill_upgrade_preserves_disabled_or_absent_roles_and_rejects_bad_registry() {
        for (input, succeeds, adds_skill) in [
            (
                r#"{"team":[{"id":"sentinel","enabled":false}]}"#,
                true,
                true,
            ),
            (r#"{"team":[]}"#, true, false),
            (
                r#"{"team":[{"id":"sentinel","enabled":false,"future_setting":42}],"future_root":true}"#,
                true,
                true,
            ),
            (
                r#"{"team":[{"id":"sentinel","skills":"invalid"}]}"#,
                false,
                false,
            ),
            ("invalid json", false, false),
        ] {
            let installation = tempfile::tempdir().unwrap();
            let resolver = OrchestrationResolver {
                candidates: vec![],
                orchestrator_dir: installation.path().to_path_buf(),
            };
            std::fs::create_dir_all(resolver.registry_path().parent().unwrap()).unwrap();
            std::fs::write(resolver.registry_path(), input).unwrap();
            assert_eq!(resolver.ensure_sentinel_testing_skill().is_ok(), succeeds);
            let result = std::fs::read_to_string(resolver.registry_path()).unwrap();
            if adds_skill {
                let role =
                    serde_json::from_str::<serde_json::Value>(&result).unwrap()["team"][0].clone();
                assert_eq!(role["enabled"], false);
                assert_eq!(role["skills"], serde_json::json!(["sentinel-testing"]));
                let mut original: serde_json::Value = serde_json::from_str(input).unwrap();
                original["team"][0]["skills"] = serde_json::json!(["sentinel-testing"]);
                assert_eq!(
                    serde_json::from_str::<serde_json::Value>(&result).unwrap(),
                    original
                );
            } else {
                assert_eq!(result, input);
            }
        }
    }
}

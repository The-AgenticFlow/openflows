use anyhow::{Context, Result};
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
        let openflows_home = std::env::var("OPENFLOWS_HOME")
            .or_else(|_| std::env::var("HOME").map(|h| format!("{}/.openflows", h)))
            .or_else(|_| std::env::var("USERPROFILE").map(|h| format!("{}/.openflows", h)))
            .ok();
        let openflows_home_path = openflows_home.map(std::path::PathBuf::from);

        let mut candidates: Vec<std::path::PathBuf> = Vec::new();

        if let Some(ref home) = openflows_home_path {
            candidates.push(home.clone());
        }
        if let Some(exe_path) = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|p| p.to_path_buf()))
        {
            if !exe_path.as_os_str().is_empty() {
                candidates.push(exe_path);
            }
        }
        if let Some(exe_parent) = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().and_then(|p| p.parent()).map(|p| p.to_path_buf()))
        {
            if !exe_parent.as_os_str().is_empty() {
                candidates.push(exe_parent);
            }
        }
        if let Ok(cwd) = std::env::current_dir() {
            candidates.push(cwd);
        }

        let (orchestrator_dir, found_registry) = candidates
            .iter()
            .find_map(|dir| {
                if dir.join("orchestration/agent/registry.json").exists() {
                    Some((dir.clone(), true))
                } else {
                    None
                }
            })
            .unwrap_or_else(|| {
                let home = openflows_home_path.unwrap_or_else(|| {
                    let home = std::env::var("OPENFLOWS_HOME")
                        .or_else(|_| std::env::var("HOME").map(|h| format!("{}/.openflows", h)))
                        .or_else(|_| {
                            std::env::var("USERPROFILE").map(|h| format!("{}/.openflows", h))
                        })
                        .unwrap_or_else(|_| ".openflows".to_string());
                    std::path::PathBuf::from(home)
                });
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

        self.write_version_file(&orch_dir)?;

        info!(
            dir = %orch_dir.display(),
            written,
            version = ORCHESTRATION_VERSION,
            "Reset all orchestration files to bundled defaults"
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

//! Reconcile local Docker resources after Coder has confirmed workspace deletion.
//! Coder's Terraform state can be lost or a delete build can succeed while a
//! container remains. Only deterministic, database-owned names are eligible.

use crate::error::ManagerError;
use serde_json::Value;
use std::time::Duration;
use tokio::process::Command;
use uuid::Uuid;

#[derive(Clone)]
pub struct LocalWorkspaceCleanup {
    binary: String,
}

impl LocalWorkspaceCleanup {
    pub fn from_env() -> Self {
        let binary = std::env::var("OPENFLOWS_DOCKER_BINARY").unwrap_or_else(|_| {
            if cfg!(target_os = "macos")
                && std::path::Path::new("/Applications/Docker.app/Contents/Resources/bin/docker")
                    .is_file()
            {
                "/Applications/Docker.app/Contents/Resources/bin/docker".into()
            } else {
                "docker".into()
            }
        });
        Self { binary }
    }

    async fn docker(&self, args: &[&str]) -> Result<std::process::Output, ManagerError> {
        tokio::time::timeout(
            Duration::from_secs(30),
            Command::new(&self.binary)
                .args(args)
                .kill_on_drop(true)
                .output(),
        )
        .await
        .map_err(|_| ManagerError::api("LOCAL_DOCKER_CLEANUP_FAILED", "Docker cleanup timed out"))?
        .map_err(|_| ManagerError::api("LOCAL_DOCKER_CLEANUP_FAILED", "Docker is unavailable"))
    }

    pub async fn remove_workspace(&self, role: &str, coder_id: &str) -> Result<(), ManagerError> {
        let (container, volume) = workspace_names(role, coder_id)?;
        let output = self.docker(&["container", "inspect", &container]).await?;
        if output.status.success() {
            let inspected: Vec<Value> = serde_json::from_slice(&output.stdout).map_err(|_| {
                ManagerError::api(
                    "LOCAL_DOCKER_CLEANUP_FAILED",
                    "invalid Docker inspect response",
                )
            })?;
            let owned = inspected
                .first()
                .is_some_and(|item| container_is_owned(item, &container, &volume));
            if !owned {
                return Err(ManagerError::api(
                    "LOCAL_DOCKER_CLEANUP_FAILED",
                    "workspace container ownership could not be verified",
                ));
            }
            self.require_success(&["container", "rm", "--force", &container])
                .await?;
        } else if !String::from_utf8_lossy(&output.stderr)
            .to_ascii_lowercase()
            .contains("no such container")
            && !String::from_utf8_lossy(&output.stderr)
                .to_ascii_lowercase()
                .contains("no such object")
        {
            return Err(ManagerError::api(
                "LOCAL_DOCKER_CLEANUP_FAILED",
                "cannot inspect workspace container",
            ));
        }
        self.remove_volume(&volume).await
    }

    pub async fn remove_artifacts(&self, tenant_id: Uuid) -> Result<(), ManagerError> {
        self.remove_volume(&format!("openflows-artifacts-{tenant_id}"))
            .await
    }

    async fn remove_volume(&self, name: &str) -> Result<(), ManagerError> {
        let output = self.docker(&["volume", "inspect", name]).await?;
        if !output.status.success() {
            if String::from_utf8_lossy(&output.stderr)
                .to_ascii_lowercase()
                .contains("no such volume")
            {
                return Ok(());
            }
            return Err(ManagerError::api(
                "LOCAL_DOCKER_CLEANUP_FAILED",
                "cannot inspect workspace volume",
            ));
        }
        let inspected: Vec<Value> = serde_json::from_slice(&output.stdout).map_err(|_| {
            ManagerError::api(
                "LOCAL_DOCKER_CLEANUP_FAILED",
                "invalid Docker volume response",
            )
        })?;
        if inspected
            .first()
            .and_then(|item| item.get("Name"))
            .and_then(Value::as_str)
            != Some(name)
        {
            return Err(ManagerError::api(
                "LOCAL_DOCKER_CLEANUP_FAILED",
                "workspace volume ownership could not be verified",
            ));
        }
        self.require_success(&["volume", "rm", name]).await
    }

    async fn require_success(&self, args: &[&str]) -> Result<(), ManagerError> {
        if !self.docker(args).await?.status.success() {
            return Err(ManagerError::api(
                "LOCAL_DOCKER_CLEANUP_FAILED",
                "Docker workspace cleanup failed; retry tenant deletion",
            ));
        }
        Ok(())
    }
}

fn workspace_names(role: &str, coder_id: &str) -> Result<(String, String), ManagerError> {
    if !matches!(role, "nexus" | "forge" | "sentinel" | "vessel" | "lore") {
        return Err(ManagerError::InvalidInput("invalid workspace role".into()));
    }
    let id = Uuid::parse_str(coder_id)
        .map_err(|_| ManagerError::InvalidInput("invalid Coder workspace id".into()))?;
    let name = format!("openflows-{role}-{id}");
    Ok((name.clone(), name))
}

fn container_is_owned(inspected: &Value, name: &str, volume: &str) -> bool {
    inspected.get("Name").and_then(Value::as_str) == Some(format!("/{name}").as_str())
        && inspected
            .get("Mounts")
            .and_then(Value::as_array)
            .is_some_and(|mounts| {
                mounts.iter().any(|mount| {
                    mount.get("Type").and_then(Value::as_str) == Some("volume")
                        && mount.get("Name").and_then(Value::as_str) == Some(volume)
                })
            })
}

#[cfg(test)]
mod tests {
    use super::{container_is_owned, workspace_names};

    #[test]
    fn cleanup_targets_only_known_role_and_uuid() {
        let id = "286e59c1-3e82-47d6-b92e-ffd906094407";
        assert_eq!(
            workspace_names("nexus", id).unwrap().0,
            format!("openflows-nexus-{id}")
        );
        assert!(workspace_names("unknown", id).is_err());
        assert!(workspace_names("nexus", "../../other").is_err());
    }

    #[test]
    fn container_cleanup_requires_matching_name_and_workspace_volume() {
        let inspected = serde_json::json!({
            "Name": "/openflows-nexus-286e59c1-3e82-47d6-b92e-ffd906094407",
            "Mounts": [{"Type": "volume", "Name": "openflows-nexus-286e59c1-3e82-47d6-b92e-ffd906094407"}]
        });
        let expected = "openflows-nexus-286e59c1-3e82-47d6-b92e-ffd906094407";
        assert!(container_is_owned(&inspected, expected, expected));
        assert!(!container_is_owned(
            &inspected,
            "openflows-forge-286e59c1-3e82-47d6-b92e-ffd906094407",
            expected
        ));
        assert!(!container_is_owned(
            &inspected,
            expected,
            "openflows-nexus-00000000-0000-0000-0000-000000000000"
        ));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn cleanup_removes_only_verified_workspace_resources() {
        use super::LocalWorkspaceCleanup;
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("docker-fixture");
        let log = dir.path().join("calls");
        let id = "286e59c1-3e82-47d6-b92e-ffd906094407";
        let name = format!("openflows-nexus-{id}");
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\ncase \"$1 $2\" in\n  'container inspect') printf '[{{\"Name\":\"/{name}\",\"Mounts\":[{{\"Type\":\"volume\",\"Name\":\"{name}\"}}]}}]' ;;\n  'volume inspect') printf '[{{\"Name\":\"%s\"}}]' \"$3\" ;;\nesac\n",
                log.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
        let cleanup = LocalWorkspaceCleanup {
            binary: script.to_string_lossy().into_owned(),
        };
        cleanup.remove_workspace("nexus", id).await.unwrap();
        let calls = std::fs::read_to_string(&log).unwrap();
        assert!(calls.contains(&format!("container rm --force {name}")));
        assert!(calls.contains(&format!("volume rm {name}")));
        assert!(!calls.contains("openflows-forge"));
    }
}

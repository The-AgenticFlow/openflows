//! Durable user credentials for connected local mode.
//!
//! Only the local Manager reads the rotating refresh token. One process owns
//! this store; the mutex serializes refresh so two agents cannot reuse a
//! consumed GitHub refresh token.

use crate::auth::local_github::{LocalGithubAuth, LocalUserToken};
use crate::error::ManagerError;
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::Mutex;

#[derive(Serialize, Deserialize)]
struct StoredCredentials {
    access_token: String,
    refresh_token: String,
    access_expires_at: u64,
    refresh_expires_at: u64,
}

pub struct LocalCredentials {
    path: PathBuf,
    github: Arc<LocalGithubAuth>,
    refresh_lock: Mutex<()>,
}

pub fn credential_path_from_env() -> Result<PathBuf, ManagerError> {
    if let Some(path) = std::env::var_os("OPENFLOWS_CONNECTED_LOCAL_CREDENTIAL_FILE") {
        return Ok(PathBuf::from(path));
    }
    let home = std::env::var_os("HOME")
        .ok_or_else(|| ManagerError::Config("HOME or local credential path is required".into()))?;
    Ok(PathBuf::from(home).join(".config/openflows/github-user.json"))
}

pub fn broker_key_path() -> Result<PathBuf, ManagerError> {
    let path = credential_path_from_env()?;
    Ok(path.with_file_name("github-broker-key"))
}

/// Created once during local sign-in. Only the path, never the value, is
/// supplied to Coder templates for read-only mounting into local workspaces.
pub fn ensure_broker_key() -> Result<(), ManagerError> {
    let path = broker_key_path()?;
    let parent = path
        .parent()
        .ok_or_else(|| ManagerError::Config("invalid broker key path".into()))?;
    ensure_private_directory(parent)?;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    match options.open(&path) {
        Ok(mut file) => {
            use std::io::Write;
            file.write_all(crate::auth::crypto::Secret::generate().encode().as_bytes())?;
            file.sync_all()?;
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            read_broker_key()?;
        }
        Err(error) => return Err(ManagerError::Io(error)),
    }
    Ok(())
}

pub fn read_broker_key() -> Result<String, ManagerError> {
    let path = broker_key_path()?;
    let metadata = std::fs::symlink_metadata(&path)?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(ManagerError::Config("invalid broker key file".into()));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o077 != 0 {
            return Err(ManagerError::Config(
                "broker key file is not private".into(),
            ));
        }
    }
    let key = std::fs::read_to_string(path)?;
    if key.trim().len() < 32 {
        return Err(ManagerError::Config("invalid broker key".into()));
    }
    Ok(key)
}

impl LocalCredentials {
    pub fn new(path: PathBuf, github: Arc<LocalGithubAuth>) -> Result<Self, ManagerError> {
        if !path.is_absolute() || path.file_name().is_none() {
            return Err(ManagerError::Config(
                "local GitHub credential path must be an absolute file path".into(),
            ));
        }
        Ok(Self {
            path,
            github,
            refresh_lock: Mutex::new(()),
        })
    }

    pub async fn save_authorization(&self, token: LocalUserToken) -> Result<(), ManagerError> {
        let _guard = self.refresh_lock.lock().await;
        let _file_lock = self.lock_file().await?;
        self.write(token)
    }

    pub async fn access_token(&self) -> Result<String, ManagerError> {
        let _guard = self.refresh_lock.lock().await;
        let _file_lock = self.lock_file().await?;
        let stored = self.read()?;
        let now = unix_now()?;
        if stored.access_expires_at > now.saturating_add(3600) {
            return Ok(stored.access_token);
        }
        if stored.refresh_expires_at <= now {
            return Err(reauthorize());
        }
        let renewed = self.github.refresh(&stored.refresh_token).await?;
        let access = renewed.access_token.clone();
        self.write(renewed)?;
        Ok(access)
    }

    pub async fn sign_out(&self) -> Result<(), ManagerError> {
        let _guard = self.refresh_lock.lock().await;
        let _file_lock = self.lock_file().await?;
        match std::fs::remove_file(&self.path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(ManagerError::Io(error)),
        }
    }

    /// The CLI and container Manager share this file. Keep a stable lock file
    /// while reading and rotating credentials so logout cannot race a refresh.
    async fn lock_file(&self) -> Result<std::fs::File, ManagerError> {
        let path = self.path.with_extension("lock");
        let parent = path
            .parent()
            .ok_or_else(|| ManagerError::Config("invalid local lock path".into()))?;
        ensure_private_directory(parent)?;
        let mut options = std::fs::OpenOptions::new();
        options.read(true).write(true).create(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let file = options.open(path)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if file.metadata()?.permissions().mode() & 0o077 != 0 {
                return Err(ManagerError::Config(
                    "local lock file is not private".into(),
                ));
            }
        }
        tokio::task::spawn_blocking(move || {
            file.lock_exclusive()?;
            Ok::<_, std::io::Error>(file)
        })
        .await
        .map_err(|_| ManagerError::Config("local lock task failed".into()))?
        .map_err(ManagerError::Io)
    }

    fn read(&self) -> Result<StoredCredentials, ManagerError> {
        let metadata = std::fs::symlink_metadata(&self.path).map_err(|_| reauthorize())?;
        if !metadata.is_file() || metadata.file_type().is_symlink() {
            return Err(ManagerError::Config("invalid local credential file".into()));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if metadata.permissions().mode() & 0o077 != 0 {
                return Err(ManagerError::Config(
                    "local credential file is not private".into(),
                ));
            }
        }
        let bytes = std::fs::read(&self.path)?;
        serde_json::from_slice(&bytes)
            .map_err(|_| ManagerError::Config("invalid local credential file".into()))
    }

    fn write(&self, token: LocalUserToken) -> Result<(), ManagerError> {
        let now = unix_now()?;
        let record = StoredCredentials {
            access_token: token.access_token,
            refresh_token: token.refresh_token,
            access_expires_at: now.saturating_add(token.expires_in),
            refresh_expires_at: now.saturating_add(token.refresh_token_expires_in),
        };
        let parent = self
            .path
            .parent()
            .ok_or_else(|| ManagerError::Config("local credential path has no parent".into()))?;
        ensure_private_directory(parent)?;
        let temp = parent.join(format!(".github-credentials-{}", uuid::Uuid::new_v4()));
        let bytes = serde_json::to_vec(&record)
            .map_err(|_| ManagerError::Config("cannot encode local credentials".into()))?;
        use std::io::Write;
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&temp)?;
        let write_result = file
            .write_all(&bytes)
            .and_then(|_| file.sync_all())
            .and_then(|_| std::fs::rename(&temp, &self.path));
        if write_result.is_err() {
            let _ = std::fs::remove_file(&temp);
        }
        write_result?;
        Ok(())
    }
}

fn ensure_private_directory(path: &Path) -> Result<(), ManagerError> {
    if !path.exists() {
        std::fs::create_dir_all(path)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
        }
    }
    let metadata = std::fs::symlink_metadata(path)?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(ManagerError::Config(
            "invalid local credential directory".into(),
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o077 != 0 {
            return Err(ManagerError::Config(
                "local credential directory is not private".into(),
            ));
        }
    }
    Ok(())
}

fn unix_now() -> Result<u64, ManagerError> {
    Ok(SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| ManagerError::Config("system clock is invalid".into()))?
        .as_secs())
}

fn reauthorize() -> ManagerError {
    ManagerError::api("GITHUB_REAUTH_REQUIRED", "GitHub authorization is required")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn credentials_survive_restart_and_sign_out_removes_them() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("private").join("github.json");
        let github = Arc::new(LocalGithubAuth::new("public-client-id".into()).unwrap());
        let first = LocalCredentials::new(path.clone(), github.clone()).unwrap();
        first
            .save_authorization(LocalUserToken {
                access_token: "user-access".into(),
                refresh_token: "user-refresh".into(),
                expires_in: 7200,
                refresh_token_expires_in: 86400,
            })
            .await
            .unwrap();
        let restarted = LocalCredentials::new(path.clone(), github).unwrap();
        assert_eq!(restarted.access_token().await.unwrap(), "user-access");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        restarted.sign_out().await.unwrap();
        assert!(restarted.access_token().await.is_err());
    }

    #[tokio::test]
    async fn sign_out_waits_for_another_process_lock() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("private").join("github.json");
        let github = Arc::new(LocalGithubAuth::new("public-client-id".into()).unwrap());
        let manager = LocalCredentials::new(path.clone(), github.clone()).unwrap();
        manager
            .save_authorization(LocalUserToken {
                access_token: "access".into(),
                refresh_token: "refresh".into(),
                expires_in: 7200,
                refresh_token_expires_in: 86400,
            })
            .await
            .unwrap();
        let other = LocalCredentials::new(path.clone(), github).unwrap();
        let lock = manager.lock_file().await.unwrap();
        let sign_out = tokio::spawn(async move { other.sign_out().await });
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(50), async {
                while !sign_out.is_finished() {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .is_err()
        );
        drop(lock);
        sign_out.await.unwrap().unwrap();
        assert!(!path.exists());
    }

    #[test]
    fn tokens_are_redacted_from_debug_output() {
        let token = LocalUserToken {
            access_token: "sensitive-access".into(),
            refresh_token: "sensitive-refresh".into(),
            expires_in: 1,
            refresh_token_expires_in: 2,
        };
        let output = format!("{token:?}");
        assert!(!output.contains("sensitive"));
    }
}

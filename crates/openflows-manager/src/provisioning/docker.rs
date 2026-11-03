//! Operator-controlled Docker provisioners. Credentials are encrypted in the
//! control-plane database and mounted as individual 0600 files, never supplied
//! in Docker arguments, environment configuration, or logs.
use crate::{auth::crypto::EnvelopeCipher, error::ManagerError};
use coder_client::{CoderClient, CoderOrganizationId};
use serde_json::Value;
use sqlx::PgPool;
use std::{path::PathBuf, time::Duration};
use tokio::process::Command;
use zeroize::Zeroizing;

#[derive(Clone)]
pub struct DockerProvisioner {
    pool: PgPool,
    cipher: EnvelopeCipher,
    binary: String,
    root: PathBuf,
    image: String,
    daemon_url: String,
    network: Option<String>,
    socket: String,
}

impl DockerProvisioner {
    pub async fn reconcile(&self, client: &CoderClient) -> Result<(), ManagerError> {
        let orgs: Vec<String> = sqlx::query_scalar("SELECT m.coder_organization_id::text FROM managed_provisioners m JOIN organizations o ON o.coder_organization_id = m.coder_organization_id::text WHERE m.coder_url=$1 AND o.status IN ('ready','provisioning')")
            .bind(client.base_url()).fetch_all(&self.pool).await?;
        for org in orgs {
            if let Err(error) = self
                .ensure(client, &CoderOrganizationId::new(org.clone()))
                .await
            {
                tracing::warn!(coder_org = %org, %error, "managed provisioner recovery failed; will retry");
            }
        }
        Ok(())
    }
    pub fn from_env(
        pool: PgPool,
        cipher: EnvelopeCipher,
        url: &str,
        version: &str,
    ) -> Result<Self, ManagerError> {
        let root = PathBuf::from(
            std::env::var("OPENFLOWS_PROVISIONER_SECRET_DIR")
                .unwrap_or_else(|_| ".local/provisioners".into()),
        );
        std::fs::create_dir_all(&root).map_err(io_error)?;
        let root = root.canonicalize().map_err(io_error)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700))
                .map_err(io_error)?;
        }
        let daemon_url = match std::env::var("OPENFLOWS_PROVISIONER_CODER_URL") {
            Ok(value) => value,
            Err(_) => container_url(url)?,
        };
        let parsed = url::Url::parse(&daemon_url)
            .map_err(|_| ManagerError::Config("invalid provisioner Coder URL".into()))?;
        if !matches!(parsed.scheme(), "http" | "https")
            || parsed.host_str().is_none()
            || !parsed.username().is_empty()
            || parsed.password().is_some()
        {
            return Err(ManagerError::Config(
                "provisioner Coder URL must be an HTTP(S) origin without credentials".into(),
            ));
        }
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
        Ok(Self {
            pool,
            cipher,
            binary,
            root,
            daemon_url,
            image: std::env::var("OPENFLOWS_PROVISIONER_IMAGE")
                .unwrap_or_else(|_| format!("ghcr.io/coder/coder:{}", version)),
            network: std::env::var("OPENFLOWS_PROVISIONER_NETWORK").ok(),
            socket: std::env::var("OPENFLOWS_PROVISIONER_DOCKER_SOCKET")
                .unwrap_or_else(|_| "/var/run/docker.sock".into()),
        })
    }

    pub async fn ensure(
        &self,
        client: &CoderClient,
        org: &CoderOrganizationId,
    ) -> Result<String, ManagerError> {
        let org_uuid = uuid::Uuid::parse_str(&org.0)
            .map_err(|_| ManagerError::Config("invalid Coder organization id".into()))?;
        // A committed intent survives cancellation of the subsequent API call.
        sqlx::query("INSERT INTO managed_provisioners (coder_url, coder_organization_id, key_name) VALUES ($1,$2,$3) ON CONFLICT DO NOTHING")
            .bind(client.base_url()).bind(org_uuid).bind(format!("of-provisioner-{}", uuid::Uuid::new_v4())).execute(&self.pool).await?;
        let mut tx = self.pool.begin().await?;
        let (key_name, sealed): (String, Option<Vec<u8>>) = sqlx::query_as("SELECT key_name, sealed_key FROM managed_provisioners WHERE coder_url=$1 AND coder_organization_id=$2 FOR UPDATE")
            .bind(client.base_url()).bind(org_uuid).fetch_one(&mut *tx).await?;
        let secret = if let Some(sealed) = sealed {
            Zeroizing::new(self.cipher.open(&sealed)?)
        } else {
            // Only our reserved, random key name can be removed. A crash after
            // upstream creation but before commit otherwise loses the raw key.
            client
                .delete_provisioner_key(org, &key_name)
                .await
                .map_err(ManagerError::from)?;
            let key = client.create_provisioner_key(org, &key_name).await.map_err(|_| ManagerError::api("PROVISIONER_KEY_FAILED", "Coder rejected provisioner key creation; check operator permissions and license"))?;
            let secret = Zeroizing::new(key.key.into_bytes());
            if secret.is_empty() {
                return Err(ManagerError::api(
                    "PROVISIONER_KEY_FAILED",
                    "Coder returned an empty provisioner key",
                ));
            }
            sqlx::query("UPDATE managed_provisioners SET sealed_key=$3 WHERE coder_url=$1 AND coder_organization_id=$2")
                .bind(client.base_url()).bind(org_uuid).bind(self.cipher.seal(&secret)?).execute(&mut *tx).await?;
            secret
        };
        tx.commit().await?;
        // The unique key identity also namespaces resources across deployments.
        let path = self.root.join(&key_name);
        write_secret(&path, &secret)?;
        self.ensure_container(&key_name, &org.0, &path).await?;
        Ok(key_name)
    }

    async fn docker(&self, args: &[String]) -> Result<std::process::Output, ManagerError> {
        let result = tokio::time::timeout(
            Duration::from_secs(180),
            Command::new(&self.binary)
                .args(args)
                .kill_on_drop(true)
                .output(),
        )
        .await;
        match result {
            Ok(Ok(output)) => Ok(output),
            _ => Err(ManagerError::api(
                "PROVISIONER_DOCKER_UNAVAILABLE",
                "cannot run Docker command; check Docker availability and OPENFLOWS_DOCKER_BINARY",
            )),
        }
    }

    async fn inspect(&self, name: &str) -> Result<Option<Value>, ManagerError> {
        // Listing distinguishes an absent container from a broken Docker daemon.
        let output = self
            .docker(&[
                "container".into(),
                "ls".into(),
                "-a".into(),
                "--filter".into(),
                format!("name=^/{name}$"),
                "--format".into(),
                "{{.ID}}".into(),
            ])
            .await?;
        if !output.status.success() {
            return Err(ManagerError::api(
                "PROVISIONER_DOCKER_UNAVAILABLE",
                "Docker daemon is unavailable",
            ));
        }
        if output.stdout.is_empty() {
            return Ok(None);
        }
        let output = self
            .docker(&["container".into(), "inspect".into(), name.into()])
            .await?;
        if !output.status.success() {
            return Err(ManagerError::api(
                "PROVISIONER_INSPECT_FAILED",
                "cannot inspect provisioner container",
            ));
        }
        let values: Vec<Value> = serde_json::from_slice(&output.stdout).map_err(|_| {
            ManagerError::api(
                "PROVISIONER_INSPECT_FAILED",
                "invalid Docker inspect response",
            )
        })?;
        Ok(values.into_iter().next())
    }

    async fn ensure_container(
        &self,
        name: &str,
        org: &str,
        path: &std::path::Path,
    ) -> Result<(), ManagerError> {
        if self.inspect(name).await?.is_none() {
            let mut args = vec![
                "create".into(),
                "--name".into(),
                name.into(),
                "--restart".into(),
                "unless-stopped".into(),
                "--user".into(),
                "0:0".into(),
                "--label".into(),
                format!("openflows.provisioner={name}"),
                "--label".into(),
                format!("openflows.coder-org={org}"),
                "--add-host".into(),
                "host.docker.internal:host-gateway".into(),
                "--mount".into(),
                format!(
                    "type=bind,source={},target=/run/openflows/provisioner-key,readonly",
                    path.display()
                ),
                "--mount".into(),
                format!(
                    "type=bind,source={},target=/var/run/docker.sock",
                    self.socket
                ),
                "--env".into(),
                format!("CODER_URL={}", self.daemon_url),
                "--env".into(),
                "DOCKER_HOST=unix:///var/run/docker.sock".into(),
            ];
            if let Some(network) = &self.network {
                args.extend(["--network".into(), network.clone()]);
            }
            args.extend(["--entrypoint".into(), "/bin/sh".into(), self.image.clone(), "-ec".into(), "export CODER_PROVISIONER_DAEMON_KEY=\"$(cat /run/openflows/provisioner-key)\"; exec /opt/coder provisionerd start".into()]);
            // Concurrent create is safe: inspect and verify the winner below.
            let created = self.docker(&args).await?;
            if !created.status.success() && self.inspect(name).await?.is_none() {
                return Err(ManagerError::api(
                    "PROVISIONER_CREATE_FAILED",
                    "Docker could not create the provisioner; check image, network and bind mounts",
                ));
            }
        }
        let container = self
            .inspect(name)
            .await?
            .ok_or_else(|| ManagerError::api("PROVISIONER_MISSING", "provisioner disappeared"))?;
        validate_container(&container, name, org, &self.image, path)?;
        if container["State"]["Running"] != true {
            let started = self.docker(&["start".into(), name.into()]).await?;
            if !started.status.success() {
                return Err(ManagerError::api(
                    "PROVISIONER_START_FAILED",
                    "Docker could not start the provisioner",
                ));
            }
        }
        Ok(())
    }
}

fn validate_container(
    value: &Value,
    name: &str,
    org: &str,
    image: &str,
    path: &std::path::Path,
) -> Result<(), ManagerError> {
    let mounted = value["Mounts"].as_array().is_some_and(|mounts| {
        mounts.iter().any(|m| {
            m["Destination"] == "/run/openflows/provisioner-key"
                && m["Source"].as_str() == path.to_str()
                && m["RW"] == false
        })
    });
    if value["Config"]["Labels"]["openflows.provisioner"] != name
        || value["Config"]["Labels"]["openflows.coder-org"] != org
        || value["Config"]["Image"] != image
        || !mounted
    {
        return Err(ManagerError::api("PROVISIONER_CONTAINER_CONFLICT", "existing container does not match the managed provisioner identity, image or secret mount"));
    }
    Ok(())
}

fn write_secret(path: &std::path::Path, secret: &[u8]) -> Result<(), ManagerError> {
    use std::io::Write;
    // Do not replace an inode already bind-mounted by a running container.
    match std::fs::symlink_metadata(path) {
        Ok(metadata) => {
            if !metadata.file_type().is_file() || std::fs::read(path).map_err(io_error)? != secret {
                return Err(ManagerError::api(
                    "PROVISIONER_SECRET_CONFLICT",
                    "existing provisioner secret file does not match its durable credential",
                ));
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
                    .map_err(io_error)?;
            }
            return Ok(());
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(io_error(e)),
    }
    // Atomic publication avoids a partial key file after interruption.
    let temporary = path.with_extension(uuid::Uuid::new_v4().to_string());
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&temporary).map_err(io_error)?;
    file.write_all(secret)
        .and_then(|_| file.sync_all())
        .map_err(io_error)?;
    // Hard-link publication never overwrites a concurrently mounted file.
    let result = std::fs::hard_link(&temporary, path);
    std::fs::remove_file(&temporary).map_err(io_error)?;
    match result {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => write_secret(path, secret),
        Err(e) => Err(io_error(e)),
    }
}

fn io_error(_: std::io::Error) -> ManagerError {
    ManagerError::api(
        "PROVISIONER_SECRET_IO",
        "cannot access provisioner credential directory",
    )
}

fn container_url(value: &str) -> Result<String, ManagerError> {
    let mut url =
        url::Url::parse(value).map_err(|_| ManagerError::Config("invalid Coder URL".into()))?;
    if matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "[::1]")) {
        url.set_host(Some("host.docker.internal"))
            .map_err(|_| ManagerError::Config("invalid Coder host".into()))?;
    }
    Ok(url.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn loopback_is_reachable_from_container() {
        assert_eq!(
            container_url("http://127.0.0.1:7080").unwrap(),
            "http://host.docker.internal:7080/"
        );
        assert_eq!(
            container_url("https://coder.example.com").unwrap(),
            "https://coder.example.com/"
        );
    }
    #[test]
    fn secret_publication_preserves_existing_mount_and_rejects_conflicts() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("key");
        write_secret(&path, b"secret").unwrap();
        write_secret(&path, b"secret").unwrap();
        assert!(write_secret(&path, b"different").is_err());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }
    #[test]
    fn foreign_container_is_rejected() {
        assert!(validate_container(
            &serde_json::json!({}),
            "expected",
            "org",
            "image",
            std::path::Path::new("/key")
        )
        .is_err());
    }
}

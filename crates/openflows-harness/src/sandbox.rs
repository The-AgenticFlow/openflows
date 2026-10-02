//! Disposable Docker verification environment. Never mounts the workspace or daemon socket.
use anyhow::{bail, Context, Result};
use std::path::Path;
use std::process::Command;

pub struct Sandbox {
    name: String,
    pub head: String,
    snapshot: tempfile::TempDir,
}

fn checked(command: &mut Command) -> Result<()> {
    use std::io::{Read, Seek, SeekFrom};
    use std::os::unix::process::CommandExt;
    use std::process::Stdio;
    use std::time::{Duration, Instant};
    let mut stderr = tempfile::tempfile()?;
    let mut child = command
        .stdout(Stdio::null())
        .stderr(stderr.try_clone()?)
        .process_group(0)
        .spawn()
        .context("Verification sandbox runtime unavailable")?;
    let deadline = Instant::now() + Duration::from_secs(60);
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = nix::sys::signal::killpg(
                nix::unistd::Pid::from_raw(child.id() as i32),
                nix::sys::signal::Signal::SIGKILL,
            );
            let _ = child.wait();
            bail!("Verification sandbox setup/cleanup exceeded 60 seconds; check the runtime endpoint");
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    if !status.success() {
        let length = stderr.metadata()?.len();
        stderr.seek(SeekFrom::Start(length.saturating_sub(16384)))?;
        let mut tail = String::new();
        stderr.read_to_string(&mut tail)?;
        bail!("Verification sandbox setup failed: {tail}");
    }
    Ok(())
}

impl Sandbox {
    pub fn create(repo: &Path, argv: &[String]) -> Result<Self> {
        a2a_protocol::validate_command_argv(argv)?;
        let image = std::env::var("OPENFLOWS_VERIFY_IMAGE")
            .context("Set OPENFLOWS_VERIFY_IMAGE to a prebuilt verification toolchain image; unsandboxed execution is disabled")?;
        Self::create_with_image(repo, argv, &image)
    }

    pub fn create_with_image(repo: &Path, argv: &[String], image: &str) -> Result<Self> {
        a2a_protocol::validate_command_argv(argv)?;
        anyhow::ensure!(
            !image.is_empty() && !image.starts_with('-'),
            "Verification image must be a nonempty image reference"
        );
        let status = Command::new("git")
            .current_dir(repo)
            .args(["status", "--porcelain"])
            .output()?;
        anyhow::ensure!(
            status.status.success() && status.stdout.is_empty(),
            "Verification requires a clean candidate checkout"
        );
        let output = Command::new("git")
            .current_dir(repo)
            .args(["rev-parse", "HEAD"])
            .output()?;
        anyhow::ensure!(output.status.success(), "Cannot read candidate HEAD");
        let head = String::from_utf8(output.stdout)?.trim().to_owned();
        let snapshot = tempfile::tempdir()?;
        let snapshot_repo = snapshot.path().join("repo");
        // A local clone does not execute repository hooks or share writable objects.
        checked(
            Command::new("git")
                .args(["clone", "--no-hardlinks", "--no-checkout", "--"])
                .arg(repo)
                .arg(&snapshot_repo),
        )?;
        checked(
            Command::new("git")
                .current_dir(&snapshot_repo)
                .args(["checkout", "--detach", &head]),
        )?;
        // No Git remote credentials/configuration are exposed to the test process.
        std::fs::remove_dir_all(snapshot_repo.join(".git"))?;
        make_writable(snapshot.path())?;
        let sandbox = Self {
            snapshot,
            name: format!("openflows-verify-{}", uuid::Uuid::new_v4()),
            head,
        };
        checked(
            Command::new("docker")
                .args([
                    "create",
                    "--pull=never",
                    "--name",
                    &sandbox.name,
                    "--network=none",
                    "--read-only",
                    "--cap-drop=ALL",
                    "--security-opt=no-new-privileges",
                    "--pids-limit=256",
                    "--memory=4g",
                    "--cpus=2",
                    "--user=65534:65534",
                    "--tmpfs",
                    "/tmp:rw,nosuid,nodev,size=512m,mode=1777",
                    "--volume",
                    "/workspace",
                    "--workdir=/workspace/repo",
                    "--entrypoint=/usr/bin/env",
                    image,
                    "-i",
                    "PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin",
                    "HOME=/tmp",
                    "CI=true",
                    "TMPDIR=/tmp",
                ])
                .args(argv),
        )?;
        // Copy into the container's private layer (works with remote Docker; no host binds).
        checked(
            Command::new("docker")
                .arg("cp")
                .arg(format!("{}/.", sandbox.snapshot.path().display()))
                .arg(format!("{}:/workspace", sandbox.name)),
        )?;
        Ok(sandbox)
    }

    pub fn source_unchanged(&self) -> Result<bool> {
        let after = tempfile::tempdir()?;
        checked(
            Command::new("docker")
                .arg("cp")
                .arg(format!("{}:/workspace/repo/.", self.name))
                .arg(after.path()),
        )?;
        same_sources(&self.snapshot.path().join("repo"), after.path())
    }

    pub fn artifacts(
        &self,
        paths: &[String],
    ) -> Result<Vec<(a2a_protocol::VerifyArtifact, String)>> {
        use sha2::{Digest, Sha256};
        let mut result = Vec::new();
        for path in paths {
            anyhow::ensure!(
                !path.is_empty()
                    && Path::new(path)
                        .components()
                        .all(|c| matches!(c, std::path::Component::Normal(_))),
                "Artifact must be a relative path without traversal"
            );
            let output = tempfile::tempdir()?;
            checked(
                Command::new("docker")
                    .arg("cp")
                    .arg(format!("{}:/workspace/repo/{path}", self.name))
                    .arg(output.path().join("artifact")),
            )?;
            let file = output.path().join("artifact");
            let meta = std::fs::symlink_metadata(&file)?;
            anyhow::ensure!(
                meta.is_file() && meta.len() <= 1_048_576,
                "Artifact must be a regular file of at most 1 MiB"
            );
            let bytes = std::fs::read(&file)?;
            let sha256 = format!("{:x}", Sha256::digest(&bytes));
            // JSON byte arrays preserve binary artifacts without lossy text conversion.
            result.push((
                a2a_protocol::VerifyArtifact {
                    path: path.clone(),
                    sha256,
                },
                serde_json::to_string(&bytes)?,
            ));
        }
        Ok(result)
    }

    pub fn command(&self) -> Command {
        let mut command = Command::new("docker");
        command.args(["start", "--attach", &self.name]);
        command
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        if let Err(error) =
            checked(Command::new("docker").args(["rm", "--force", "--volumes", &self.name]))
        {
            tracing::warn!(container = %self.name, %error, "Verification sandbox cleanup failed");
        }
    }
}

fn make_writable(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    for entry in std::fs::read_dir(path)? {
        let path = entry?.path();
        let meta = std::fs::symlink_metadata(&path)?;
        if meta.is_symlink() {
            continue;
        }
        if meta.is_dir() {
            make_writable(&path)?;
        }
        std::fs::set_permissions(
            &path,
            std::fs::Permissions::from_mode(meta.permissions().mode() | 0o666),
        )?;
    }
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o777))?;
    Ok(())
}

fn same_sources(before: &Path, after: &Path) -> Result<bool> {
    for entry in std::fs::read_dir(before)? {
        let before = entry?.path();
        let after = after.join(before.file_name().unwrap());
        let old = std::fs::symlink_metadata(&before)?;
        let Ok(new) = std::fs::symlink_metadata(&after) else {
            return Ok(false);
        };
        use std::os::unix::fs::PermissionsExt;
        if old.permissions().mode() & 0o111 != new.permissions().mode() & 0o111 {
            return Ok(false);
        }
        if old.file_type() != new.file_type() {
            return Ok(false);
        }
        if old.is_symlink() {
            if std::fs::read_link(&before)? != std::fs::read_link(&after)? {
                return Ok(false);
            }
        } else if old.is_dir() {
            if !same_sources(&before, &after)? {
                return Ok(false);
            }
        } else if std::fs::read(&before)? != std::fs::read(&after)? {
            return Ok(false);
        }
    }
    Ok(true)
}

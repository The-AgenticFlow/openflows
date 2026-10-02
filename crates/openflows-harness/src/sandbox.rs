//! Disposable verification checkout using FORGE's installed tools and environment.
//! This isolates ordinary checkout changes, not processes or credentials.
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Workspace-local handoff, never committed or sent through the relay.
#[derive(Serialize, Deserialize)]
pub struct PreparedEnvironment {
    pub head: String,
    pub lifecycle_version: u64,
    pub probe: Vec<String>,
    pub variables: BTreeMap<String, String>,
}

impl PreparedEnvironment {
    fn path(repo: &Path) -> Result<PathBuf> {
        let output = Command::new("git")
            .current_dir(repo)
            .args(["rev-parse", "--absolute-git-dir"])
            .output()?;
        anyhow::ensure!(
            output.status.success(),
            "Cannot locate workspace Git directory"
        );
        Ok(PathBuf::from(String::from_utf8(output.stdout)?.trim())
            .join("openflows-verification.json"))
    }

    pub fn load(repo: &Path, head: &str) -> Result<Self> {
        let data = std::fs::read(Self::path(repo)?)
            .context("Verification environment is not prepared; FORGE must run verify prepare from its configured project shell")?;
        let environment: Self = serde_json::from_slice(&data)?;
        anyhow::ensure!(
            environment.head == head,
            "Verification environment belongs to another HEAD; run verify prepare again"
        );
        Ok(environment)
    }

    /// Probe a fresh checkout before publishing the caller's environment atomically.
    pub fn prepare(repo: &Path, argv: &[String], lifecycle_version: u64) -> Result<()> {
        // Invalidate any previous readiness even if the new probe fails.
        let path = Self::path(repo)?;
        match std::fs::remove_file(&path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
        let variables = std::env::vars_os()
            .filter(|(key, _)| !key.to_string_lossy().starts_with("GIT_"))
            .map(|(key, value)| {
                Ok((
                    key.into_string()
                        .map_err(|_| anyhow::anyhow!("Non-UTF8 environment key"))?,
                    value
                        .into_string()
                        .map_err(|_| anyhow::anyhow!("Non-UTF8 environment value"))?,
                ))
            })
            .collect::<Result<BTreeMap<_, _>>>()?;
        let mut sandbox = Sandbox::create_unprepared(repo, argv)?;
        sandbox.environment = Some(variables.clone());
        checked(&mut sandbox.command())
            .context("Project readiness probe failed (60 second limit)")?;
        anyhow::ensure!(
            sandbox.source_unchanged()?,
            "Readiness probe modified committed source"
        );
        anyhow::ensure!(
            candidate_head(repo)? == sandbox.head,
            "Workspace HEAD changed during readiness preparation"
        );
        let environment = Self {
            head: sandbox.head.clone(),
            lifecycle_version,
            probe: argv.to_vec(),
            variables,
        };
        let mut file = tempfile::NamedTempFile::new_in(path.parent().unwrap())?;
        // NamedTempFile is owner-only (0600); environment values may include credentials.
        use std::io::Write;
        file.write_all(&serde_json::to_vec(&environment)?)?;
        file.persist(path)?;
        Ok(())
    }
}

pub struct Sandbox {
    pub head: String,
    root: tempfile::TempDir,
    argv: Vec<String>,
    environment: Option<BTreeMap<String, String>>,
}

fn candidate_head(repo: &Path) -> Result<String> {
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
    Ok(String::from_utf8(output.stdout)?.trim().to_owned())
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
        .context("Verification checkout setup unavailable")?;
    let deadline = Instant::now() + Duration::from_secs(60);
    let status = loop {
        if let Some(status) = child.try_wait()? {
            let _ = nix::sys::signal::killpg(
                nix::unistd::Pid::from_raw(child.id() as i32),
                nix::sys::signal::Signal::SIGKILL,
            );
            break status;
        }
        if Instant::now() >= deadline {
            let _ = nix::sys::signal::killpg(
                nix::unistd::Pid::from_raw(child.id() as i32),
                nix::sys::signal::Signal::SIGKILL,
            );
            let _ = child.wait();
            bail!("Verification checkout setup exceeded 60 seconds");
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    if !status.success() {
        let length = stderr.metadata()?.len();
        stderr.seek(SeekFrom::Start(length.saturating_sub(16384)))?;
        let mut tail = String::new();
        stderr.read_to_string(&mut tail)?;
        bail!("Verification checkout setup failed ({status}): {tail}");
    }
    Ok(())
}

impl Sandbox {
    pub fn create(repo: &Path, argv: &[String]) -> Result<Self> {
        let mut sandbox = Self::create_unprepared(repo, argv)?;
        sandbox.environment = Some(PreparedEnvironment::load(repo, &sandbox.head)?.variables);
        Ok(sandbox)
    }

    fn create_unprepared(repo: &Path, argv: &[String]) -> Result<Self> {
        a2a_protocol::validate_command_argv(argv)?;
        let repo = repo.canonicalize()?;
        let head = candidate_head(&repo)?;
        // Independent object databases and indexes prevent ordinary Git commands
        // in tests from modifying FORGE's branch or index.
        let root = tempfile::Builder::new()
            .prefix("openflows-verify-")
            .tempdir()?;
        let baseline = root.path().join("baseline");
        let checkout = root.path().join("repo");
        for destination in [&baseline, &checkout] {
            checked(
                Command::new("git")
                    .args(["clone", "--no-hardlinks", "--no-checkout", "--"])
                    .arg(&repo)
                    .arg(destination),
            )?;
            checked(Command::new("git").current_dir(destination).args([
                "-c",
                "core.hooksPath=/dev/null",
                "checkout",
                "--detach",
                &head,
            ]))?;
            checked(
                Command::new("git")
                    .current_dir(destination)
                    .args(["remote", "remove", "origin"]),
            )?;
        }
        // Compare committed files only; generated output may be collected as artifacts.
        std::fs::remove_dir_all(baseline.join(".git"))?;
        Ok(Self {
            head,
            root,
            argv: argv.to_vec(),
            environment: None,
        })
    }

    pub fn checkout_path(&self) -> PathBuf {
        self.root.path().join("repo")
    }

    pub fn source_unchanged(&self) -> Result<bool> {
        same_sources(&self.root.path().join("baseline"), &self.checkout_path())
    }

    pub fn artifacts(
        &self,
        paths: &[String],
    ) -> Result<Vec<(a2a_protocol::VerifyArtifact, String)>> {
        use sha2::{Digest, Sha256};
        let mut result = Vec::new();
        let checkout = self.checkout_path().canonicalize()?;
        for path in paths {
            anyhow::ensure!(
                !path.is_empty()
                    && Path::new(path)
                        .components()
                        .all(|c| matches!(c, std::path::Component::Normal(_))),
                "Artifact must be a relative path without traversal"
            );
            let file = checkout.join(path);
            anyhow::ensure!(
                file.canonicalize()?.starts_with(&checkout),
                "Artifact must stay inside the verification checkout"
            );
            let meta = std::fs::symlink_metadata(&file)?;
            anyhow::ensure!(
                meta.is_file() && meta.len() <= 1_048_576,
                "Artifact must be a regular file of at most 1 MiB"
            );
            let bytes = std::fs::read(&file)?;
            result.push((
                a2a_protocol::VerifyArtifact {
                    path: path.clone(),
                    sha256: format!("{:x}", Sha256::digest(&bytes)),
                },
                serde_json::to_string(&bytes)?,
            ));
        }
        Ok(result)
    }

    pub fn command(&self) -> Command {
        let mut command = Command::new(&self.argv[0]);
        if let Some(environment) = &self.environment {
            command.env_clear().envs(
                environment
                    .iter()
                    .filter(|(key, _)| !key.starts_with("GIT_")),
            );
        }
        command
            .args(&self.argv[1..])
            .current_dir(self.checkout_path())
            .env("PWD", self.checkout_path())
            .env("CI", "true");
        // Reuse PATH, HOME, caches and tools. Do not allow inherited Git overrides
        // to redirect commands back to the working checkout.
        for (key, _) in std::env::vars_os() {
            if key.to_string_lossy().starts_with("GIT_") {
                command.env_remove(key);
            }
        }
        command
    }
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
        if old.permissions().mode() & 0o111 != new.permissions().mode() & 0o111
            || old.file_type() != new.file_type()
        {
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

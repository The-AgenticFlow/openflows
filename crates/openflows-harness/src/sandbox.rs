//! Disposable verification checkout using FORGE's installed tools and environment.
//! This isolates ordinary checkout changes, not processes or credentials.
use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};
use std::process::Command;

pub struct Sandbox {
    pub head: String,
    root: tempfile::TempDir,
    argv: Vec<String>,
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
        bail!("Verification checkout setup failed: {tail}");
    }
    Ok(())
}

impl Sandbox {
    pub fn create(repo: &Path, argv: &[String]) -> Result<Self> {
        a2a_protocol::validate_command_argv(argv)?;
        let repo = repo.canonicalize()?;
        let status = Command::new("git")
            .current_dir(&repo)
            .args(["status", "--porcelain"])
            .output()?;
        anyhow::ensure!(
            status.status.success() && status.stdout.is_empty(),
            "Verification requires a clean candidate checkout"
        );
        let output = Command::new("git")
            .current_dir(&repo)
            .args(["rev-parse", "HEAD"])
            .output()?;
        anyhow::ensure!(output.status.success(), "Cannot read candidate HEAD");
        let head = String::from_utf8(output.stdout)?.trim().to_owned();
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
        // Load the workspace user's current login environment for every task.
        // The daemon may predate tools installed/configured while FORGE built.
        // Pass argv literally; never concatenate a requested command into shell code.
        let mut command = Command::new("bash");
        command
            .args([
                "-lc",
                r#"
for name in "${!GIT_@}"; do unset "$name"; done
cd -- "$1" || exit
shift
export CI=true
if ! type -P -- "$1" >/dev/null; then
    printf '[EXECUTOR_SETUP] Command not executable: %s\n' "$1" >&2
    exit 127
fi
exec -- "$@"
"#,
                "openflows-verify",
            ])
            .arg(self.checkout_path())
            .args(&self.argv)
            .current_dir(self.checkout_path())
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

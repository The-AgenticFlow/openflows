//! Workspace artifact bundle installation.
//!
//! A role workspace fetches the immutable persona, skill, command, standard and
//! hook bundle from the Manager instead of reading it from a shared volume
//! another workspace seeds. The Manager serves a deterministic gzipped tar and
//! reports its sha256 in the `x-artifact-digest` header, so the workspace can
//! verify exactly what it received before installing it.
//!
//! A workspace may pin a digest. A pinned workspace installs that bundle or
//! fails; it never silently runs a different release.

use anyhow::{bail, Context, Result};
use std::io::Read;
use std::path::{Component, Path};

/// The header carrying the digest of the served archive.
const DIGEST_HEADER: &str = "x-artifact-digest";

/// How many times a download is attempted before the workspace gives up.
const DOWNLOAD_ATTEMPTS: u32 = 3;

/// Result of a successful install.
#[derive(Debug, PartialEq, Eq)]
pub struct Installed {
    /// sha256 of the archive that was installed.
    pub digest: String,
    /// Number of files written.
    pub files: usize,
}

/// Download the bundle and install it into `dir`, returning what was installed.
///
/// With `pin`, the Manager must serve that exact digest. Without it, the
/// running release is installed and its digest returned so the caller can
/// record it.
pub async fn fetch(
    manager_url: &str,
    access_token: &str,
    pin: Option<&str>,
    dir: &Path,
) -> Result<Installed> {
    let base = manager_url.trim_end_matches('/');
    let url = match pin {
        Some(digest) => format!("{base}/api/v1/runtime/artifacts/{digest}"),
        None => format!("{base}/api/v1/runtime/artifacts"),
    };
    let token = access_token.to_string();
    let (archive, digest) = with_retries(DOWNLOAD_ATTEMPTS, move |_attempt| {
        let url = url.clone();
        let token = token.clone();
        async move { download(&url, &token).await }
    })
    .await?;
    install(&archive, &digest, pin, dir)
}

/// Run `action` up to `attempts` times, returning the first success. The last
/// error is the one reported, so a caller sees why the retries did not help.
async fn with_retries<T, F, Fut>(attempts: u32, mut action: F) -> Result<T>
where
    F: FnMut(u32) -> Fut,
    Fut: std::future::Future<Output = Result<T>>,
{
    let total = attempts.max(1);
    let mut last = None;
    for attempt in 1..=total {
        match action(attempt).await {
            Ok(value) => return Ok(value),
            Err(error) => {
                tracing::warn!(attempt, total, error = %error, "artifact download failed");
                last = Some(error);
                if attempt < total {
                    tokio::time::sleep(std::time::Duration::from_millis(200 * attempt as u64))
                        .await;
                }
            }
        }
    }
    Err(last.unwrap_or_else(|| anyhow::anyhow!("no download attempt ran")))
}

async fn download(url: &str, access_token: &str) -> Result<(Vec<u8>, String)> {
    let response = reqwest::Client::new()
        .get(url)
        .bearer_auth(access_token)
        .send()
        .await
        .with_context(|| format!("cannot reach the Manager for the artifact bundle at {url}"))?;
    let status = response.status();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        bail!(
            "the Manager refused the artifact bundle ({status}){}{}",
            if body.is_empty() { "" } else { ": " },
            body.trim()
        );
    }
    let digest = response
        .headers()
        .get(DIGEST_HEADER)
        .and_then(|value| value.to_str().ok())
        .context("the Manager response has no artifact digest header")?
        .to_string();
    let archive = response
        .bytes()
        .await
        .context("cannot read the artifact bundle body")?
        .to_vec();
    Ok((archive, digest))
}

/// Verify and unpack an archive into `dir`.
///
/// The digest is checked before anything is written, so a truncated or
/// corrupted download leaves the previous bundle untouched. Entries are
/// validated to stay inside `dir`: absolute paths, parent traversal, symlinks
/// and hard links are refused.
pub fn install(
    archive: &[u8],
    served_digest: &str,
    pin: Option<&str>,
    dir: &Path,
) -> Result<Installed> {
    let digest = workspace_assets::digest(archive);
    if digest != served_digest {
        bail!(
            "artifact bundle integrity check failed: the Manager reported {served_digest} \
             but the archive hashes to {digest}"
        );
    }
    if let Some(pin) = pin {
        if pin != digest {
            bail!(
                "artifact bundle release mismatch: this workspace pins {pin} \
                 but the Manager served {digest}. Rebuild the workspace from the \
                 pinned release or update the pin deliberately."
            );
        }
    }
    let files = unpack(archive, dir)?;
    Ok(Installed { digest, files })
}

fn unpack(archive: &[u8], dir: &Path) -> Result<usize> {
    let decoder = flate2::read::GzDecoder::new(std::io::Cursor::new(archive));
    let mut tar = tar::Archive::new(decoder);
    let mut files = 0usize;
    for entry in tar.entries().context("cannot read the artifact archive")? {
        let mut entry = entry.context("cannot read an artifact archive entry")?;
        let kind = entry.header().entry_type();
        let path = entry
            .path()
            .context("artifact entry has no path")?
            .into_owned();
        let relative = safe_relative(&path).with_context(|| {
            format!(
                "artifact archive entry {} escapes the install directory",
                path.display()
            )
        })?;
        if !(kind.is_file() || kind.is_dir()) {
            bail!(
                "artifact archive entry {} is a link, which the bundle never contains",
                relative.display()
            );
        }
        if kind.is_dir() {
            std::fs::create_dir_all(dir.join(&relative))?;
            continue;
        }
        let target = dir.join(&relative);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut content = Vec::new();
        entry
            .read_to_end(&mut content)
            .with_context(|| format!("cannot read artifact {}", relative.display()))?;
        std::fs::write(&target, &content)
            .with_context(|| format!("cannot write artifact {}", target.display()))?;
        // Hooks are executed directly; everything else is read as data.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = if relative.extension().is_some_and(|ext| ext == "sh") {
                0o755
            } else {
                0o644
            };
            std::fs::set_permissions(&target, std::fs::Permissions::from_mode(mode))
                .with_context(|| format!("cannot set permissions on {}", target.display()))?;
        }
        files += 1;
    }
    Ok(files)
}

/// Reject anything that is not a plain relative path inside the install root.
fn safe_relative(path: &Path) -> Result<std::path::PathBuf> {
    if path.as_os_str().is_empty() {
        bail!("empty path");
    }
    let mut cleaned = std::path::PathBuf::new();
    for component in path.components() {
        match component {
            Component::Normal(part) => cleaned.push(part),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                bail!("path is not relative to the install directory")
            }
        }
    }
    if cleaned.as_os_str().is_empty() {
        bail!("empty path");
    }
    Ok(cleaned)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_archive() -> Vec<u8> {
        workspace_assets::archive().unwrap()
    }

    #[test]
    fn installs_the_tree_under_the_given_directory() {
        let archive = sample_archive();
        let digest = workspace_assets::digest(&archive);
        let dir = tempfile::tempdir().unwrap();
        let installed = install(&archive, &digest, Some(&digest), dir.path()).unwrap();
        assert_eq!(installed.digest, digest);
        assert!(installed.files > 0);
        assert!(dir
            .path()
            .join("orchestration/agent/agents/forge.agent.md")
            .is_file());
        assert!(dir
            .path()
            .join("plugin/hooks/forge/session_start.sh")
            .is_file());
    }

    #[test]
    fn install_is_idempotent_so_a_retry_is_safe() {
        let archive = sample_archive();
        let digest = workspace_assets::digest(&archive);
        let dir = tempfile::tempdir().unwrap();
        let first = install(&archive, &digest, None, dir.path()).unwrap();
        let second = install(&archive, &digest, None, dir.path()).unwrap();
        assert_eq!(first, second);
    }

    #[test]
    fn hooks_are_executable_and_data_is_not() {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let archive = sample_archive();
            let digest = workspace_assets::digest(&archive);
            let dir = tempfile::tempdir().unwrap();
            install(&archive, &digest, None, dir.path()).unwrap();
            let hook = std::fs::metadata(dir.path().join("plugin/hooks/forge/session_start.sh"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            let persona =
                std::fs::metadata(dir.path().join("orchestration/agent/agents/forge.agent.md"))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777;
            assert_eq!(hook, 0o755);
            assert_eq!(persona, 0o644);
        }
    }

    #[test]
    fn rejects_a_pin_that_the_manager_did_not_serve() {
        let archive = sample_archive();
        let digest = workspace_assets::digest(&archive);
        let dir = tempfile::tempdir().unwrap();
        let error = install(&archive, &digest, Some("00"), dir.path()).unwrap_err();
        assert!(error.to_string().contains("release mismatch"));
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    }

    #[test]
    fn rejects_an_archive_that_does_not_match_the_reported_digest() {
        let archive = sample_archive();
        let dir = tempfile::tempdir().unwrap();
        let error = install(&archive, "not-the-digest", None, dir.path()).unwrap_err();
        assert!(error.to_string().contains("integrity check failed"));
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    }

    #[test]
    fn refuses_entries_that_escape_the_install_directory() {
        for path in ["../escape", "/etc/passwd", "a/../../escape"] {
            let error = safe_relative(Path::new(path)).unwrap_err();
            assert!(
                error.to_string().contains("relative") || error.to_string().contains("empty"),
                "unexpected error for {path}: {error}"
            );
        }
        assert!(safe_relative(Path::new("orchestration/agent/agents/forge.agent.md")).is_ok());
    }

    #[tokio::test]
    async fn retries_a_failing_download_before_reporting_it() {
        let mut attempts = 0;
        let result: Result<u32> = with_retries(3, |attempt| {
            attempts += 1;
            async move {
                if attempt < 3 {
                    bail!("transient {attempt}")
                }
                Ok(attempt)
            }
        })
        .await;
        assert_eq!(result.unwrap(), 3);
        assert_eq!(attempts, 3);
    }

    #[tokio::test]
    async fn reports_the_last_error_after_every_attempt_fails() {
        let result: Result<u32> = with_retries(2, |attempt| async move {
            Err::<u32, _>(anyhow::anyhow!("attempt {attempt} failed"))
        })
        .await;
        assert!(result.unwrap_err().to_string().contains("attempt 2 failed"));
    }
}

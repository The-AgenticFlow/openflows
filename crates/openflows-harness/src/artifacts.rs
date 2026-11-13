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

/// A failed download. Client errors are reported immediately; only a network
/// failure or a server error is worth another attempt.
#[derive(Debug)]
struct DownloadError {
    retryable: bool,
    error: anyhow::Error,
}

/// The digest of the artifact bundle this binary was built with.
///
/// The harness and the Manager are built from the same release, so this is the
/// release a workspace can run without mixing assets and code. It is computed
/// rather than configured, so a workspace cannot claim a release it does not
/// carry.
pub fn release_digest() -> Result<String> {
    let archive = workspace_assets::archive().context("cannot serialize the bundled artifacts")?;
    Ok(workspace_assets::digest(&archive))
}

/// Install the bundle of the release this binary was built with.
///
/// This is what a workspace should call at boot: a Manager still serving an
/// older or newer release is refused instead of quietly handing over assets
/// that do not match the harness running them. Pass an explicit pin through
/// [`fetch`] when the expected release is known some other way.
pub async fn fetch_own_release(
    manager_url: &str,
    access_token: &str,
    dir: &Path,
) -> Result<Installed> {
    let expected = release_digest()?;
    fetch(manager_url, access_token, Some(&expected), dir)
        .await
        .with_context(|| {
            format!(
                "cannot install the artifact bundle of this workspace release ({expected}); \
                 the Manager must serve that release, or pass --any-release to install \
                 whichever release it serves"
            )
        })
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

/// Run `action` up to `attempts` times, returning the first success. A failure
/// the caller marked as final is returned immediately, so a rejected request is
/// never retried. The last error is the one reported.
async fn with_retries<T, F, Fut>(attempts: u32, mut action: F) -> Result<T>
where
    F: FnMut(u32) -> Fut,
    Fut: std::future::Future<Output = std::result::Result<T, DownloadError>>,
{
    let total = attempts.max(1);
    let mut last = None;
    for attempt in 1..=total {
        match action(attempt).await {
            Ok(value) => return Ok(value),
            Err(failure) => {
                if !failure.retryable {
                    return Err(failure.error);
                }
                tracing::warn!(
                    attempt,
                    total,
                    error = %failure.error,
                    "artifact download failed"
                );
                last = Some(failure.error);
                if attempt < total {
                    tokio::time::sleep(std::time::Duration::from_millis(200 * attempt as u64))
                        .await;
                }
            }
        }
    }
    Err(last.unwrap_or_else(|| anyhow::anyhow!("no download attempt ran")))
}

async fn download(
    url: &str,
    access_token: &str,
) -> std::result::Result<(Vec<u8>, String), DownloadError> {
    let response = reqwest::Client::new()
        .get(url)
        .bearer_auth(access_token)
        .send()
        .await
        .map_err(|error| DownloadError {
            retryable: true,
            error: anyhow::Error::new(error).context(format!(
                "cannot reach the Manager for the artifact bundle at {url}"
            )),
        })?;
    let status = response.status();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        return Err(DownloadError {
            // A rejected request stays rejected; only a server fault is worth
            // another attempt.
            retryable: status.is_server_error(),
            error: anyhow::anyhow!(
                "the Manager refused the artifact bundle ({status}){}{}",
                if body.is_empty() { "" } else { ": " },
                body.trim()
            ),
        });
    }
    let digest = response
        .headers()
        .get(DIGEST_HEADER)
        .and_then(|value| value.to_str().ok())
        .ok_or_else(|| DownloadError {
            retryable: false,
            error: anyhow::anyhow!("the Manager response has no artifact digest header"),
        })?
        .to_string();
    let archive = response
        .bytes()
        .await
        .map_err(|error| DownloadError {
            retryable: true,
            error: anyhow::Error::new(error).context("cannot read the artifact bundle body"),
        })?
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
    // Extract beside the target and swap only once the whole archive is on
    // disk. A workspace volume survives restarts, so installing over the old
    // bundle would leave files a new release dropped still readable.
    let staging = sibling(dir, "staging");
    if staging.exists() {
        std::fs::remove_dir_all(&staging)
            .with_context(|| format!("cannot clear {}", staging.display()))?;
    }
    let files = match unpack(archive, &staging) {
        Ok(files) => files,
        Err(error) => {
            let _ = std::fs::remove_dir_all(&staging);
            return Err(error);
        }
    };
    swap_into_place(&staging, dir)?;
    Ok(Installed { digest, files })
}

/// A directory next to `dir`, so the swap is a rename on one filesystem.
fn sibling(dir: &Path, suffix: &str) -> std::path::PathBuf {
    let name = dir
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("bundle");
    dir.with_file_name(format!("{name}.{suffix}"))
}

/// Replace `target` with `staging`. The previous bundle is kept until the new
/// one is in place, so a failed swap leaves the workspace with the old bundle
/// rather than with nothing.
fn swap_into_place(staging: &Path, target: &Path) -> Result<()> {
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let previous = sibling(target, "previous");
    let had_previous = target.exists();
    if had_previous {
        if previous.exists() {
            std::fs::remove_dir_all(&previous)
                .with_context(|| format!("cannot clear {}", previous.display()))?;
        }
        std::fs::rename(target, &previous).with_context(|| {
            format!(
                "cannot move the previous bundle out of {}",
                target.display()
            )
        })?;
    }
    match std::fs::rename(staging, target) {
        Ok(()) => {
            if had_previous {
                let _ = std::fs::remove_dir_all(&previous);
            }
            Ok(())
        }
        Err(error) => {
            if had_previous {
                let _ = std::fs::rename(&previous, target);
            }
            Err(error)
                .with_context(|| format!("cannot install the bundle into {}", target.display()))
        }
    }
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
    fn reinstalling_drops_files_the_new_bundle_no_longer_ships() {
        let archive = sample_archive();
        let digest = workspace_assets::digest(&archive);
        let dir = tempfile::tempdir().unwrap();
        install(&archive, &digest, None, dir.path()).unwrap();

        // A hook or command removed by a later release must not survive.
        let stale = dir.path().join("orchestration/plugin/commands/removed.md");
        std::fs::write(&stale, "stale").unwrap();
        install(&archive, &digest, None, dir.path()).unwrap();
        assert!(!stale.exists(), "a stale file from the old bundle survived");

        assert!(
            !sibling(dir.path(), "staging").exists(),
            "staging left behind"
        );
        assert!(
            !sibling(dir.path(), "previous").exists(),
            "previous left behind"
        );
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
    async fn retries_a_transient_failure_before_reporting_it() {
        let mut attempts = 0;
        let result: Result<u32> = with_retries(3, |attempt| {
            attempts += 1;
            async move {
                if attempt < 3 {
                    Err(DownloadError {
                        retryable: true,
                        error: anyhow::anyhow!("transient {attempt}"),
                    })
                } else {
                    Ok(attempt)
                }
            }
        })
        .await;
        assert_eq!(result.unwrap(), 3);
        assert_eq!(attempts, 3);
    }

    #[tokio::test]
    async fn a_rejected_request_is_not_retried() {
        let mut attempts = 0;
        let result: Result<u32> = with_retries(3, |_attempt| {
            attempts += 1;
            async move {
                Err(DownloadError {
                    retryable: false,
                    error: anyhow::anyhow!("401 unauthorized"),
                })
            }
        })
        .await;
        assert!(result.unwrap_err().to_string().contains("401"));
        assert_eq!(attempts, 1, "a rejected request must not be retried");
    }

    #[tokio::test]
    async fn reports_the_last_error_after_every_attempt_fails() {
        let result: Result<u32> = with_retries(2, |attempt| async move {
            Err(DownloadError {
                retryable: true,
                error: anyhow::anyhow!("attempt {attempt} failed"),
            })
        })
        .await;
        assert!(result.unwrap_err().to_string().contains("attempt 2 failed"));
    }
}

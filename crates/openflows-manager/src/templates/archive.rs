//! Deterministic template archives.
//!
//! The same source directory must always produce the same archive bytes so the
//! manifest digest is a stable release identity. Every tar header field that
//! would otherwise vary between machines is normalized (mtime, uid/gid,
//! uname/gname, and the gzip header) and paths are sorted.

use crate::error::ManagerError;
use flate2::write::GzEncoder;
use flate2::Compression;
use std::path::{Path, PathBuf};
use tar::{Builder, EntryType, Header};

/// Normalized mtime for every archive entry (Unix epoch). Keeps packaging
/// reproducible across checkouts.
const NORMALIZED_MTIME: u64 = 0;

/// Build a deterministic `.tar.gz` archive from a source directory.
///
/// Paths are sorted; every entry gets a normalized mtime, uid/gid 0, empty
/// user/group names, and a stable mode. The gzip header carries no timestamp.
pub fn build_deterministic_archive(src_dir: &Path) -> Result<Vec<u8>, ManagerError> {
    let mut files = Vec::new();
    collect_files(src_dir, src_dir, &mut files)?;
    files.sort();

    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    {
        let mut builder = Builder::new(&mut encoder);
        builder.follow_symlinks(false);
        for rel in &files {
            let abs = src_dir.join(rel);
            let data = std::fs::read(&abs).map_err(|e| {
                ManagerError::Service(anyhow::anyhow!(
                    "failed to read template file '{}': {e}",
                    abs.display()
                ))
            })?;
            let mut header = Header::new_gnu();
            header.set_size(data.len() as u64);
            header.set_mode(0o644);
            header.set_mtime(NORMALIZED_MTIME);
            header.set_uid(0);
            header.set_gid(0);
            let _ = header.set_username("");
            let _ = header.set_groupname("");
            header.set_entry_type(EntryType::Regular);
            header.set_cksum();
            builder
                .append_data(&mut header, rel.as_os_str(), data.as_slice())
                .map_err(|e| {
                    ManagerError::Service(anyhow::anyhow!("failed to write tar entry: {e}"))
                })?;
        }
        builder
            .finish()
            .map_err(|e| ManagerError::Service(anyhow::anyhow!("failed to finish tar: {e}")))?;
    }
    let bytes = encoder
        .finish()
        .map_err(|e| ManagerError::Service(anyhow::anyhow!("failed to finish gzip: {e}")))?;
    Ok(bytes)
}

/// Files sorted for a reproducible archive, one entry per regular file.
///
/// Terraform's working directory and its lock file are skipped: they hold
/// downloaded provider binaries and are not template source.
fn collect_files(root: &Path, dir: &Path, out: &mut Vec<PathBuf>) -> Result<(), ManagerError> {
    let entries = std::fs::read_dir(dir).map_err(|e| {
        ManagerError::Service(anyhow::anyhow!(
            "failed to read template dir '{}': {e}",
            dir.display()
        ))
    })?;
    for entry in entries {
        let entry = entry.map_err(|e| {
            ManagerError::Service(anyhow::anyhow!("failed to read template entry: {e}"))
        })?;
        let path = entry.path();
        let rel = path.strip_prefix(root).map_err(|e| {
            ManagerError::Service(anyhow::anyhow!("failed to compute relative path: {e}"))
        })?;
        let ft = entry.file_type().map_err(ManagerError::from)?;
        if matches!(
            entry.file_name().to_str(),
            Some(".terraform" | ".terraform.lock.hcl")
        ) {
            continue;
        }
        if ft.is_dir() {
            collect_files(root, &path, out)?;
        } else if ft.is_file() {
            // Reject registry.json: it must never ship as a template parameter.
            if rel.components().any(|c| c.as_os_str() == "registry.json") {
                return Err(ManagerError::InvalidInput(
                    "registry.json must not be packaged into a template release".into(),
                ));
            }
            out.push(rel.to_path_buf());
        } else {
            return Err(ManagerError::InvalidInput(format!(
                "unsupported template source entry '{}'",
                rel.display()
            )));
        }
    }
    Ok(())
}

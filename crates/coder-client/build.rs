// Regenerate Coder template tar.gz archives from .tf sources before the
// crate is compiled, so that `include_bytes!("../templates/<name>.tar.gz")`
// in bootstrap.rs always embeds the current template.
//
// The archives are gitignored (see .gitignore: `*.tar`), so without this
// step a `cargo build` after editing a template would silently embed a
// stale archive — the exact bug that shipped workspace containers whose
// entrypoint ran the *startup script* instead of the *Coder agent init
// script*, leaving the agent "connecting" forever.
//
// We shell out to system `tar` (consistent with `rebuild_templates.sh`
// and `push_template` in lib.rs) rather than pulling in `tar`+`flate2`
// crates just for the build script.

use std::path::PathBuf;
use std::process::Command;

fn main() {
    // Templates live in <crate>/templates/<role>/main.tf and are packed into
    // <crate>/templates/<role>.tar.gz.  The crate manifest directory is the
    // build script's CARGO_MANIFEST_DIR.
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let templates_dir = manifest.join("templates");
    let roles = ["forge", "sentinel", "nexus", "vessel", "lore"];

    let tar = std::env::var("OPENFLOWS_TAR")
        .unwrap_or_else(|_| "tar".to_string());

    let mut any_rebuilt = false;
    for role in roles {
        let src_dir = templates_dir.join(format!("openflows-{role}"));
        let archive = templates_dir.join(format!("openflows-{role}.tar.gz"));

        // Skip roles whose source directory is absent (e.g. partial checkouts).
        if !src_dir.is_dir() {
            continue;
        }

        // Always tell Cargo to re-run this script only when a template
        // source changes.  Without this, Cargo's default heuristic re-runs
        // the script on every build, defeating the mtime check below.
        println!(
            "cargo:rerun-if-changed={}",
            src_dir.join("main.tf").display()
        );

        // Rebuild only when any source file (main.tf and friends) is newer
        // than the archive, or the archive is missing.  This keeps no-op
        // builds fast and avoids touching mtimes unnecessarily.
        let needs_rebuild = !archive.exists() || is_outdated(&src_dir, &archive);
        if !needs_rebuild {
            continue;
        }

        let result = Command::new(&tar)
            .arg("-czf")
            .arg(&archive)
            .arg("-C")
            .arg(&src_dir)
            .arg(".")
            .output();

        match result {
            Ok(out) if out.status.success() => {
                any_rebuilt = true;
            }
            Ok(out) => {
                let stderr = String::from_utf8_lossy(&out.stderr);
                panic!(
                    "cargo: failed to pack template 'openflows-{role}' via {tar}: {stderr}"
                );
            }
            Err(e) => {
                panic!(
                    "cargo: failed to invoke {tar} to pack template \
                     'openflows-{role}': {e}. Ensure GNU/tar is on PATH \
                     or set OPENFLOWS_TAR to an absolute tar binary."
                );
            }
        }
    }

    if any_rebuilt {
        println!("cargo:warning=Regenerated Coder template archives from .tf sources");
    }
}

/// True when any file inside `src_dir` is newer than `archive`.
/// Falls back to "rebuild" if walking the directory fails.
fn is_outdated(src_dir: &std::path::Path, archive: &std::path::Path) -> bool {
    let archive_mtime = match std::fs::metadata(archive) {
        Ok(m) => m
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs()),
        Err(_) => return true, // missing archive → rebuild
    };
    let archive_mtime = match archive_mtime {
        Some(t) => t,
        None => return true,
    };

    let check = |path: &std::path::Path| -> bool {
        std::fs::metadata(path)
            .ok()
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs() > archive_mtime)
            .unwrap_or(true)
    };

    // main.tf is the primary source; also catch any sibling files.
    if check(&src_dir.join("main.tf")) {
        return true;
    }
    if let Ok(entries) = std::fs::read_dir(src_dir) {
        for entry in entries.flatten() {
            if check(&entry.path()) {
                return true;
            }
        }
    }
    false
}
// Regenerate Coder template tar.gz archives from .tf sources before compile.
// Mtime checks miss same-second git pulls, so repack and byte-compare instead.
use std::path::{Path, PathBuf};
use std::process::Command;

/// Archive names the Manager expects in a release, per source directory.
/// `<role>` is the plain Docker variant; `kubernetes/<role>` is the hosted one.
const VARIANTS: [(&str, &str); 2] = [
    ("openflows-{role}", "openflows-{role}.tar.gz"),
    (
        "kubernetes/openflows-{role}",
        "openflows-kubernetes-{role}.tar.gz",
    ),
];

fn main() {
    // Templates live in <crate>/templates/<role>/main.tf and are packed into
    // <crate>/templates/<role>.tar.gz.  The crate manifest directory is the
    // build script's CARGO_MANIFEST_DIR.
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let templates_dir = manifest.join("templates");
    let roles = ["forge", "sentinel", "nexus", "vessel", "lore"];

    let tar = std::env::var("OPENFLOWS_TAR").unwrap_or_else(|_| "tar".to_string());
    println!("cargo:rerun-if-env-changed=OPENFLOWS_TAR");

    // Watch the Kubernetes variant tree, not only the sources that exist today: a
    // commit that adds a role variant must repackage it without a clean rebuild.
    // The packed archives are written beside this directory, not into it, so the
    // watch cannot see the script's own output and stay dirty for ever.
    println!(
        "cargo:rerun-if-changed={}",
        templates_dir.join("kubernetes").display()
    );

    let mut any_rebuilt = false;
    for (source, archive) in VARIANTS {
        for role in roles {
            let name = source.replace("{role}", role);
            let archive_name = archive.replace("{role}", role);
            if pack(&templates_dir, &name, &archive_name, &tar) {
                any_rebuilt = true;
            }
        }
    }

    if any_rebuilt {
        println!("cargo:warning=Regenerated Coder template archives from .tf sources");
    }
}

/// Pack one template source directory, rewriting the archive only when its bytes
/// change. Returns whether the archive was rewritten. A missing source is not an
/// error: a Kubernetes variant lands role by role, and partial checkouts exist.
fn pack(templates_dir: &Path, source: &str, archive_name: &str, tar: &str) -> bool {
    let src_dir = templates_dir.join(source);
    // A source that does not exist yet is covered by the tree watch above. Watching
    // its `main.tf` here would mark the script dirty on every build until it
    // appears, so the per-source watch only starts once the directory exists.
    if !src_dir.is_dir() {
        return false;
    }
    println!(
        "cargo:rerun-if-changed={}",
        src_dir.join("main.tf").display()
    );
    let archive = templates_dir.join(archive_name);

    // Repack to a temp archive and byte-compare; rewrite only when content differs.
    let mut tmp = archive.clone();
    tmp.set_file_name(format!(
        "{}.tmp",
        tmp.file_name().unwrap_or_default().to_string_lossy()
    ));
    // Terraform's local working directory holds provider binaries and must never
    // reach a template archive.
    let result = Command::new(tar)
        .arg("-czf")
        .arg(&tmp)
        .arg("-C")
        .arg(&src_dir)
        .arg("--exclude=.terraform")
        .arg("--exclude=.terraform.lock.hcl")
        .arg(".")
        .output();

    match result {
        Ok(out) if out.status.success() => {
            let fresh = match std::fs::read(&tmp) {
                Ok(bytes) => bytes,
                Err(e) => panic!(
                    "cargo: could not read packed template '{source}' at {}: {e}",
                    tmp.display()
                ),
            };
            let _ = std::fs::remove_file(&tmp);
            let current = std::fs::read(&archive).ok();
            if current.as_deref() != Some(fresh.as_slice()) {
                if let Err(e) = std::fs::write(&archive, &fresh) {
                    panic!(
                        "cargo: could not write template archive '{archive_name}' at {}: {e}",
                        archive.display()
                    );
                }
                return true;
            }
            false
        }
        Ok(out) => {
            let _ = std::fs::remove_file(&tmp);
            let stderr = String::from_utf8_lossy(&out.stderr);
            panic!("cargo: failed to pack template '{source}' via {tar}: {stderr}");
        }
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            panic!(
                "cargo: failed to invoke {tar} to pack template \
                 '{source}': {e}. Ensure GNU/tar is on PATH \
                 or set OPENFLOWS_TAR to an absolute tar binary."
            );
        }
    }
}

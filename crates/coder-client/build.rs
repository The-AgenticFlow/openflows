// Regenerate Coder template tar.gz archives from .tf sources before compile.
// Mtime checks miss same-second git pulls, so repack and byte-compare instead.
use std::path::PathBuf;
use std::process::Command;

fn main() {
    // Templates live in <crate>/templates/<role>/main.tf and are packed into
    // <crate>/templates/<role>.tar.gz.  The crate manifest directory is the
    // build script's CARGO_MANIFEST_DIR.
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let templates_dir = manifest.join("templates");
    let roles = ["forge", "sentinel", "nexus", "vessel", "lore"];

    let tar = std::env::var("OPENFLOWS_TAR").unwrap_or_else(|_| "tar".to_string());
    println!("cargo:rerun-if-env-changed=OPENFLOWS_TAR");

    let mut any_rebuilt = false;
    for role in roles {
        let src_dir = templates_dir.join(format!("openflows-{role}"));
        let archive = templates_dir.join(format!("openflows-{role}.tar.gz"));

        // Skip roles whose source directory is absent (e.g. partial checkouts).
        if !src_dir.is_dir() {
            continue;
        }

        // Re-run when the template source changes.
        println!(
            "cargo:rerun-if-changed={}",
            src_dir.join("main.tf").display()
        );

        // Repack to a temp archive and byte-compare; rewrite only when content differs.
        let mut tmp = archive.clone();
        tmp.set_file_name(format!(
            "{}.tmp",
            tmp.file_name().unwrap_or_default().to_string_lossy()
        ));
        let result = Command::new(&tar)
            .arg("-czf")
            .arg(&tmp)
            .arg("-C")
            .arg(&src_dir)
            .arg(".")
            .output();

        match result {
            Ok(out) if out.status.success() => {
                let fresh = match std::fs::read(&tmp) {
                    Ok(bytes) => bytes,
                    Err(e) => panic!(
                        "cargo: could not read packed template 'openflows-{role}' at {}: {e}",
                        tmp.display()
                    ),
                };
                let _ = std::fs::remove_file(&tmp);
                let current = std::fs::read(&archive).ok();
                if current.as_deref() != Some(fresh.as_slice()) {
                    if let Err(e) = std::fs::write(&archive, &fresh) {
                        panic!(
                            "cargo: could not write template archive 'openflows-{role}' at {}: {e}",
                            archive.display()
                        );
                    }
                    any_rebuilt = true;
                }
            }
            Ok(out) => {
                let _ = std::fs::remove_file(&tmp);
                let stderr = String::from_utf8_lossy(&out.stderr);
                panic!("cargo: failed to pack template 'openflows-{role}' via {tar}: {stderr}");
            }
            Err(e) => {
                let _ = std::fs::remove_file(&tmp);
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

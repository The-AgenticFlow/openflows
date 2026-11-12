use openflows_manager::provisioning::ReleaseProvider;
use openflows_manager::templates::packaging::sha256_hex;
use std::env;
use std::fs;
use std::path::PathBuf;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = env::args_os().skip(1);
    let source = args
        .next()
        .map(PathBuf::from)
        .ok_or("usage: build-template-release <source-dir> <release> <output-dir>")?;
    let release = args.next().ok_or("missing release version")?;
    let output = args
        .next()
        .map(PathBuf::from)
        .ok_or("missing output directory")?;
    if args.next().is_some() {
        return Err("usage: build-template-release <source-dir> <release> <output-dir>".into());
    }

    let artifacts =
        ReleaseProvider::new(source, None, None, release.to_string_lossy().into()).load()?;
    fs::create_dir_all(&output)?;
    let manifest = artifacts.manifest.to_canonical_json()?;
    fs::write(output.join("manifest.json"), &manifest)?;
    for ((backend, role), entry) in artifacts.manifest.all_variants() {
        let archive = artifacts
            .archive_for(backend, role)
            .ok_or_else(|| format!("missing archive for {} ({})", role.as_str(), backend))?;
        fs::write(output.join(&entry.archive), archive)?;
    }
    println!("manifest_sha256={}", sha256_hex(&manifest));
    Ok(())
}

use std::process::Command;

pub fn check_command(cmd: &str) -> Option<String> {
    let output = Command::new(cmd).arg("--version").output().ok()?;
    if output.status.success() {
        let version = String::from_utf8_lossy(&output.stdout);
        Some(version.lines().next()?.to_string())
    } else {
        None
    }
}

pub fn detect_os() -> (&'static str, &'static str) {
    let os = if cfg!(target_os = "linux") {
        "linux"
    } else if cfg!(target_os = "macos") {
        "macos"
    } else if cfg!(target_os = "windows") {
        "windows"
    } else {
        "unknown"
    };

    let arch = if cfg!(target_arch = "x86_64") {
        "x86_64"
    } else if cfg!(target_arch = "aarch64") {
        "aarch64"
    } else {
        "unknown"
    };

    (os, arch)
}

pub fn check_rustup() -> bool {
    check_command("rustup").is_some()
}

pub fn check_rustc() -> Option<String> {
    check_command("rustc")
}

pub fn check_git() -> Option<String> {
    check_command("git")
}

pub fn check_node() -> Option<String> {
    check_command("node")
}

pub fn check_claude() -> Option<String> {
    check_command("claude")
}

pub fn check_gh_cli() -> Option<String> {
    check_command("gh")
}

pub fn check_cargo() -> Option<String> {
    check_command("cargo")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_detect_os_returns_known_values() {
        let (os, arch) = detect_os();
        assert!(matches!(os, "linux" | "macos" | "windows" | "unknown"));
        assert!(matches!(arch, "x86_64" | "aarch64" | "unknown"));
    }
}

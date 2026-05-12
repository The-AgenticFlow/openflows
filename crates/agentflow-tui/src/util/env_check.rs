use std::collections::HashMap;
use std::fs;
use std::path::Path;
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

// ─────────────────────────────────────────────────────────────────────────────
// Token Validation
// ─────────────────────────────────────────────────────────────────────────────

/// Validate a GitHub Personal Access Token format.
/// Valid prefixes: ghp_, gho_, ghu_, ghs_, ghr_
/// Invalid prefixes: AIzaSy (Firebase/Google), sk- (OpenAI), etc.
pub fn validate_github_token(token: &str) -> Result<(), String> {
    let token = token.trim();
    
    if token.is_empty() {
        return Err("Token is empty".to_string());
    }
    
    // Valid GitHub token prefixes
    let valid_prefixes = ["ghp_", "gho_", "ghu_", "ghs_", "ghr_"];
    
    // Known invalid prefixes (other services)
    let invalid_prefixes = [
        ("AIzaSy", "Firebase/Google API key"),
        ("AIza", "Firebase/Google API key"),
        ("sk-", "OpenAI API key"),
        ("sk-ant-", "Anthropic API key"),
        ("fw_", "Fireworks API key"),
        ("ya29.", "Google OAuth token"),
        ("xoxb-", "Slack bot token"),
        ("xoxp-", "Slack app token"),
    ];
    
    // Check for invalid prefixes first
    for (prefix, service) in invalid_prefixes {
        if token.starts_with(prefix) {
            return Err(format!(
                "Token appears to be a {} (prefix: {}), not a GitHub PAT",
                service, prefix
            ));
        }
    }
    
    // Check for valid GitHub prefixes
    if valid_prefixes.iter().any(|p| token.starts_with(p)) {
        // Basic length check (GitHub PATs are typically 40+ chars after prefix)
        if token.len() < 36 {
            return Err("GitHub token appears truncated (too short)".to_string());
        }
        return Ok(());
    }
    
    // OAuth tokens (hex only, 40 chars)
    if token.len() == 40 && token.chars().all(|c| c.is_ascii_hexdigit()) {
        return Ok(());
    }
    
    // Unknown format
    Err(format!(
        "Token has unknown format (expected ghp_*, gho_*, ghu_*, ghs_*, ghr_*, or 40-char hex OAuth token)"
    ))
}

/// Check for duplicate keys in a .env file.
/// Returns a list of keys that appear more than once.
pub fn check_env_duplicates(env_path: &Path) -> Vec<(String, usize)> {
    if !env_path.exists() {
        return Vec::new();
    }
    
    let content = match fs::read_to_string(env_path) {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };
    
    let mut key_counts: HashMap<String, usize> = HashMap::new();
    
    for line in content.lines() {
        let line = line.trim();
        
        // Skip comments and empty lines
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        
        // Extract key (before =)
        if let Some(key) = line.split('=').next() {
            let key = key.trim().to_string();
            *key_counts.entry(key).or_insert(0) += 1;
        }
    }
    
    key_counts
        .into_iter()
        .filter(|(_, count)| *count > 1)
        .map(|(key, count)| (key, count))
        .collect()
}

/// Validate all agent GitHub tokens from environment.
/// Returns a list of (agent_id, error) for invalid tokens.
pub fn validate_agent_tokens() -> Vec<(String, String)> {
    let mut errors = Vec::new();
    
    // Standard GitHub PAT
    if let Ok(token) = std::env::var("GITHUB_PERSONAL_ACCESS_TOKEN") {
        if let Err(e) = validate_github_token(&token) {
            errors.push(("GITHUB_PERSONAL_ACCESS_TOKEN".to_string(), e));
        }
    }
    
    // Per-agent tokens (from registry pattern)
    let agent_ids = ["nexus", "forge", "sentinel", "vessel", "lore"];
    
    for agent_id in agent_ids {
        let env_var = format!("AGENT_{}_GITHUB_TOKEN", agent_id.to_uppercase());
        if let Ok(token) = std::env::var(&env_var) {
            if let Err(e) = validate_github_token(&token) {
                errors.push((env_var, e));
            }
        }
    }
    
    errors
}

/// Scan .env file for common issues:
/// - Duplicate keys
/// - Invalid token formats
/// - Missing required keys
pub fn scan_env_file(env_path: &Path) -> Vec<EnvIssue> {
    let mut issues = Vec::new();
    
    if !env_path.exists() {
        issues.push(EnvIssue::MissingFile);
        return issues;
    }
    
    // Check for duplicates
    let duplicates = check_env_duplicates(env_path);
    for (key, count) in duplicates {
        issues.push(EnvIssue::DuplicateKey { key, count });
    }
    
    // Validate tokens from already-loaded environment
    // (dotenvy is called by the binary before this)
    let token_errors = validate_agent_tokens();
    for (env_var, error) in token_errors {
        issues.push(EnvIssue::InvalidToken { env_var, error });
    }
    
    issues
}

#[derive(Debug, Clone)]
pub enum EnvIssue {
    MissingFile,
    DuplicateKey { key: String, count: usize },
    InvalidToken { env_var: String, error: String },
}

impl std::fmt::Display for EnvIssue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EnvIssue::MissingFile => write!(f, ".env file not found"),
            EnvIssue::DuplicateKey { key, count } => {
                write!(f, "Duplicate key '{}' appears {} times", key, count)
            }
            EnvIssue::InvalidToken { env_var, error } => {
                write!(f, "{}: {}", env_var, error)
            }
        }
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

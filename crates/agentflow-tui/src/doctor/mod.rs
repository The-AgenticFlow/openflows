use anyhow::Result;
use ratatui::backend::CrosstermBackend;
use ratatui::prelude::Widget;
use ratatui::Terminal;
use std::io;

use crate::app::App;
use crate::util::env_check::{self, EnvIssue};
use crate::util::theme::Theme;
use crate::widgets::check::{CheckList, CheckState};

pub async fn run_doctor(_app: &mut App) -> Result<()> {
    let terminal = crate::init_tui()?;
    let result = run_doctor_inner(terminal).await;
    crate::restore_tui();
    result
}

async fn run_doctor_inner(mut terminal: Terminal<CrosstermBackend<io::Stdout>>) -> Result<()> {
    let theme = Theme::default();
    let mut checks = Vec::new();

    checks.push(("── Environment ──".to_string(), CheckState::Pending));

    if let Some(version) = env_check::check_rustc() {
        checks.push((format!("Rust {}", version), CheckState::Pass));
    } else {
        checks.push(("Rust not found".to_string(), CheckState::Fail));
    }

    if let Some(version) = env_check::check_git() {
        checks.push((format!("Git {}", version), CheckState::Pass));
    } else {
        checks.push(("Git not found".to_string(), CheckState::Fail));
    }

    if let Some(version) = env_check::check_node() {
        checks.push((format!("Node.js {}", version), CheckState::Pass));
    } else {
        checks.push(("Node.js not found".to_string(), CheckState::Warn));
    }

    if let Some(version) = env_check::check_claude() {
        checks.push((format!("Claude CLI {}", version), CheckState::Pass));
    } else {
        checks.push(("Claude CLI not found".to_string(), CheckState::Warn));
    }

    // ── Pre-flight: .env File Validation ──────────────────────────────────────
    checks.push(("── Pre-flight Checks ──".to_string(), CheckState::Pending));

    let env_path = std::path::PathBuf::from(".env");
    let env_issues = env_check::scan_env_file(&env_path);
    
    if env_issues.is_empty() {
        checks.push((".env validation passed".to_string(), CheckState::Pass));
    } else {
        for issue in &env_issues {
            match issue {
                EnvIssue::DuplicateKey { key, count } => {
                    checks.push((
                        format!("⚠ DUPLICATE: '{}' defined {} times", key, count),
                        CheckState::Fail,
                    ));
                }
                EnvIssue::InvalidToken { env_var, error } => {
                    checks.push((
                        format!("⚠ INVALID TOKEN: {} - {}", env_var, error),
                        CheckState::Fail,
                    ));
                }
                EnvIssue::MissingFile => {
                    checks.push((".env file missing".to_string(), CheckState::Fail));
                }
            }
        }
    }

    // ── Configuration ──────────────────────────────────────────────────────────
    checks.push(("── Configuration ──".to_string(), CheckState::Pending));

    if std::path::Path::new(".env").exists() {
        checks.push((".env file exists".to_string(), CheckState::Pass));
    } else {
        checks.push((".env file missing".to_string(), CheckState::Fail));
    }

    // Check GitHub token format
    if let Ok(token) = std::env::var("GITHUB_PERSONAL_ACCESS_TOKEN") {
        match env_check::validate_github_token(&token) {
            Ok(()) => {
                checks.push((
                    format!("GITHUB_TOKEN valid format ({} chars)", token.len()),
                    CheckState::Pass,
                ));
            }
            Err(e) => {
                checks.push((
                    format!("GITHUB_TOKEN INVALID: {}", e),
                    CheckState::Fail,
                ));
            }
        }
    } else {
        checks.push((
            "GITHUB_PERSONAL_ACCESS_TOKEN not set".to_string(),
            CheckState::Fail,
        ));
    }

    if let Ok(repo) = std::env::var("GITHUB_REPOSITORY") {
        checks.push((format!("GITHUB_REPOSITORY = {}", repo), CheckState::Pass));
    } else {
        checks.push(("GITHUB_REPOSITORY not set".to_string(), CheckState::Fail));
    }

    let registry_path = std::env::current_dir()?
        .join("orchestration")
        .join("agent")
        .join("registry.json");

    if registry_path.exists() {
        match config::Registry::load(&registry_path) {
            Ok(registry) => {
                let agent_count = registry.active_agents().count();
                let slot_count = registry.all_worker_slots().len();
                checks.push((
                    format!(
                        "registry.json valid ({} agents, {} slots)",
                        agent_count, slot_count
                    ),
                    CheckState::Pass,
                ));
            }
            Err(e) => {
                checks.push((
                    format!("registry.json parse error: {}", e),
                    CheckState::Fail,
                ));
            }
        }
    } else {
        checks.push(("registry.json not found".to_string(), CheckState::Fail));
    }

    if std::env::var("PROXY_URL").is_ok() {
        checks.push(("PROXY_URL set".to_string(), CheckState::Pass));
    } else {
        checks.push((
            "PROXY_URL not set (using direct mode)".to_string(),
            CheckState::Warn,
        ));
    }

    // Check for mcp-proxy installation (Python tool from PyPI)
    if env_check::check_command("mcp-proxy").is_some() {
        checks.push(("mcp-proxy installed".to_string(), CheckState::Pass));
    } else {
        checks.push((
            "mcp-proxy not found (install: uv tool install mcp-proxy)".to_string(),
            CheckState::Fail,
        ));
    }

    // Check API configuration
    checks.push(("── API Configuration ──".to_string(), CheckState::Pending));

    // Check for any valid API key
    let has_api_key = std::env::var("FIREWORKS_API_KEY").is_ok()
        || std::env::var("ANTHROPIC_API_KEY").is_ok()
        || std::env::var("OPENAI_API_KEY").is_ok()
        || std::env::var("GEMINI_API_KEY").is_ok()
        || std::env::var("GATEWAY_API_KEY").is_ok()
        || std::env::var("PROXY_URL").is_ok();

    if has_api_key {
        if std::env::var("FIREWORKS_API_KEY").is_ok() {
            checks.push(("FIREWORKS_API_KEY set".to_string(), CheckState::Pass));
        }
        if std::env::var("ANTHROPIC_API_KEY").is_ok() {
            checks.push(("ANTHROPIC_API_KEY set".to_string(), CheckState::Pass));
        }
        if std::env::var("GATEWAY_API_KEY").is_ok() {
            checks.push(("GATEWAY_API_KEY set".to_string(), CheckState::Pass));
        }
    } else {
        checks.push((
            "No API key configured (need FIREWORKS_API_KEY, ANTHROPIC_API_KEY, etc.)".to_string(),
            CheckState::Fail,
        ));
    }

    checks.push(("── Connectivity ──".to_string(), CheckState::Pending));

    // Test GitHub API with the configured token
    if let Ok(token) = std::env::var("GITHUB_PERSONAL_ACCESS_TOKEN") {
        let client = reqwest::Client::new();
        match client
            .get("https://api.github.com/user")
            .header("Authorization", format!("Bearer {}", token))
            .header("User-Agent", "OpenFlows-Doctor")
            .timeout(std::time::Duration::from_secs(5))
            .send()
            .await
        {
            Ok(resp) if resp.status().is_success() => {
                checks.push(("GitHub API: token valid".to_string(), CheckState::Pass));
            }
            Ok(resp) if resp.status() == 401 => {
                checks.push((
                    "GitHub API: 401 Unauthorized (bad token)".to_string(),
                    CheckState::Fail,
                ));
            }
            Ok(resp) => {
                checks.push((
                    format!("GitHub API returned {}", resp.status()),
                    CheckState::Warn,
                ));
            }
            Err(e) => {
                checks.push((format!("GitHub API unreachable: {}", e), CheckState::Fail));
            }
        }
    } else {
        // Basic connectivity test without auth
        match reqwest::get("https://api.github.com").await {
            Ok(resp) if resp.status().is_success() => {
                checks.push(("GitHub API reachable (no token)".to_string(), CheckState::Warn));
            }
            Ok(resp) => {
                checks.push((
                    format!("GitHub API returned {}", resp.status()),
                    CheckState::Warn,
                ));
            }
            Err(e) => {
                checks.push((format!("GitHub API unreachable: {}", e), CheckState::Fail));
            }
        }
    }

    // Test Fireworks API if configured
    if let Ok(key) = std::env::var("FIREWORKS_API_KEY") {
        let client = reqwest::Client::new();
        match client
            .get("https://api.fireworks.ai/inference/v1/models")
            .header("Authorization", format!("Bearer {}", key))
            .timeout(std::time::Duration::from_secs(5))
            .send()
            .await
        {
            Ok(resp) if resp.status().is_success() => {
                checks.push(("Fireworks API: token valid".to_string(), CheckState::Pass));
            }
            Ok(resp) if resp.status() == 401 => {
                checks.push((
                    "Fireworks API: 401 Unauthorized".to_string(),
                    CheckState::Fail,
                ));
            }
            Ok(resp) => {
                checks.push((
                    format!("Fireworks API returned {}", resp.status()),
                    CheckState::Warn,
                ));
            }
            Err(e) => {
                checks.push((
                    format!("Fireworks API unreachable: {}", e),
                    CheckState::Warn,
                ));
            }
        }
    }

    // Test Anthropic API if configured
    if let Ok(key) = std::env::var("ANTHROPIC_API_KEY") {
        let client = reqwest::Client::new();
        match client
            .post("https://api.anthropic.com/v1/messages")
            .header("x-api-key", &key)
            .header("anthropic-version", "2023-06-01")
            .timeout(std::time::Duration::from_secs(5))
            .json(&serde_json::json!({
                "model": "claude-haiku-4-5-20251001",
                "max_tokens": 1,
                "messages": []
            }))
            .send()
            .await
        {
            Ok(resp) if resp.status().is_success() => {
                checks.push(("Anthropic API: token valid".to_string(), CheckState::Pass));
            }
            Ok(resp) if resp.status() == 401 => {
                checks.push((
                    "Anthropic API: 401 Unauthorized".to_string(),
                    CheckState::Fail,
                ));
            }
            Ok(resp) => {
                checks.push((
                    format!("Anthropic API returned {}", resp.status()),
                    CheckState::Warn,
                ));
            }
            Err(e) => {
                checks.push((
                    format!("Anthropic API unreachable: {}", e),
                    CheckState::Warn,
                ));
            }
        }
    }

    checks.push(("── Workspace ──".to_string(), CheckState::Pending));

    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .unwrap_or_else(|_| ".".to_string());

    let workspace_root = std::path::PathBuf::from(home)
        .join(".agentflow")
        .join("workspaces");
    if workspace_root.exists() {
        checks.push((
            format!("{} exists", workspace_root.display()),
            CheckState::Pass,
        ));
    } else {
        checks.push((
            format!("{} not found", workspace_root.display()),
            CheckState::Warn,
        ));
    }

    let issue_count = checks
        .iter()
        .filter(|(_, state)| state == &CheckState::Fail)
        .count();
    let warn_count = checks
        .iter()
        .filter(|(_, state)| state == &CheckState::Warn)
        .count();

    terminal.draw(|f| {
        let area = f.area();
        let check_list = CheckList::new(checks);
        check_list.render(area, f.buffer_mut());

        let summary = format!(
            "Summary: {} issues, {} warnings found",
            issue_count, warn_count
        );
        let summary_area = ratatui::layout::Rect {
            x: 2,
            y: area.height.saturating_sub(2),
            width: area.width.saturating_sub(4),
            height: 1,
        };
        let summary_style = if issue_count > 0 {
            theme.error_style()
        } else if warn_count > 0 {
            theme.warning_style()
        } else {
            theme.success_style()
        };
        let summary_widget = ratatui::widgets::Paragraph::new(summary).style(summary_style);
        summary_widget.render(summary_area, f.buffer_mut());
    })?;

    println!("\n[Fix API Key]  [View Details]  [Exit]");
    let mut input = String::new();
    std::io::stdin().read_line(&mut input)?;

    Ok(())
}

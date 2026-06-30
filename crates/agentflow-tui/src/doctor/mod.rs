use anyhow::Result;
use ratatui::backend::CrosstermBackend;
use ratatui::prelude::Widget;
use ratatui::Terminal;
use std::io;

use crate::app::App;
use crate::util::env_check;
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

    checks.push(("── Configuration ──".to_string(), CheckState::Pending));

    let home = std::env::var("OPENFLOWS_HOME").unwrap_or_else(|_| {
        format!(
            "{}/.openflows",
            std::env::var("HOME")
                .or_else(|_| std::env::var("USERPROFILE"))
                .unwrap_or_else(|_| ".".to_string())
        )
    });
    let env_path = std::path::Path::new(&home).join(".env");
    if env_path.exists() || std::path::Path::new(".env").exists() {
        checks.push((".env file exists".to_string(), CheckState::Pass));
    } else {
        checks.push((".env file missing".to_string(), CheckState::Fail));
    }

    if let Ok(key) = std::env::var("ANTHROPIC_API_KEY") {
        checks.push((
            format!("ANTHROPIC_API_KEY set (length: {})", key.len()),
            CheckState::Pass,
        ));
    } else {
        checks.push(("ANTHROPIC_API_KEY not set".to_string(), CheckState::Fail));
    }

    if let Ok(key) = std::env::var("GITHUB_PERSONAL_ACCESS_TOKEN") {
        checks.push((
            format!("GITHUB_PERSONAL_ACCESS_TOKEN set (length: {})", key.len()),
            CheckState::Pass,
        ));
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

    checks.push(("── Connectivity ──".to_string(), CheckState::Pending));

    match reqwest::get("https://api.github.com").await {
        Ok(resp) if resp.status().is_success() => {
            checks.push(("GitHub API reachable".to_string(), CheckState::Pass));
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

    if let Ok(key) = std::env::var("ANTHROPIC_API_KEY") {
        let client = reqwest::Client::new();
        match client
            .post("https://api.anthropic.com/v1/messages")
            .header("x-api-key", &key)
            .header("anthropic-version", "2023-06-01")
            .timeout(std::time::Duration::from_secs(5))
            .json(&serde_json::json!({
                "model": "claude-hhaiku-4-5-20251001",
                "max_tokens": 1,
                "messages": []
            }))
            .send()
            .await
        {
            Ok(resp) if resp.status().is_success() => {
                checks.push(("Anthropic API valid".to_string(), CheckState::Pass));
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
                    CheckState::Fail,
                ));
            }
        }
    } else {
        checks.push(("Anthropic API: key not set".to_string(), CheckState::Fail));
    }

    if std::env::var("PROXY_URL").is_ok() {
        checks.push(("LiteLLM proxy: configured".to_string(), CheckState::Pass));
    } else {
        checks.push((
            "LiteLLM proxy: not configured".to_string(),
            CheckState::Warn,
        ));
    }

    checks.push(("── Workspace ──".to_string(), CheckState::Pending));

    // Coder health checks
    checks.push(("── Coder ──".to_string(), CheckState::Pending));

    if let Ok(url) = std::env::var("CODER_URL") {
        match reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(5))
            .build()
        {
            Ok(client) => {
                match client
                    .get(format!("{}/api/v2/buildinfo", url.trim_end_matches('/')))
                    .send()
                    .await
                {
                    Ok(resp) if resp.status().is_success() => {
                        checks.push(("Coder server reachable".to_string(), CheckState::Pass));
                    }
                    Ok(resp) => {
                        checks.push((
                            format!("Coder server returned {}", resp.status()),
                            CheckState::Fail,
                        ));
                    }
                    Err(e) => {
                        checks.push((format!("Coder server unreachable: {}", e), CheckState::Fail));
                    }
                }

                // Check if workspace templates have been pushed
                if let Ok(token) = std::env::var("CODER_API_TOKEN") {
                    match client
                        .get(format!("{}/api/v2/templates", url.trim_end_matches('/')))
                        .header("Authorization", format!("Bearer {}", token))
                        .send()
                        .await
                    {
                        Ok(resp) if resp.status().is_success() => {
                            if let Ok(body) = resp.text().await {
                                let has_forge = body.contains("openflows-forge");
                                let has_sentinel = body.contains("openflows-sentinel");
                                if has_forge && has_sentinel {
                                    checks.push((
                                        "Coder templates pushed".to_string(),
                                        CheckState::Pass,
                                    ));
                                } else if has_forge || has_sentinel {
                                    checks.push((
                                        "Coder templates partially pushed".to_string(),
                                        CheckState::Warn,
                                    ));
                                } else {
                                    checks.push((
                                        "Coder templates not found".to_string(),
                                        CheckState::Warn,
                                    ));
                                }
                            } else {
                                checks.push((
                                    "Coder templates: could not read response".to_string(),
                                    CheckState::Warn,
                                ));
                            }
                        }
                        Ok(resp) => {
                            checks.push((
                                format!("Coder templates check failed (HTTP {})", resp.status()),
                                CheckState::Warn,
                            ));
                        }
                        Err(e) => {
                            checks.push((
                                format!("Coder templates check failed: {}", e),
                                CheckState::Warn,
                            ));
                        }
                    }
                } else {
                    checks.push((
                        "Coder templates: CODER_API_TOKEN not set".to_string(),
                        CheckState::Warn,
                    ));
                }
            }
            Err(_) => {
                checks.push(("Coder: HTTP client error".to_string(), CheckState::Fail));
            }
        }

        // Check Docker socket availability
        if std::path::Path::new("/var/run/docker.sock").exists() {
            checks.push(("Docker socket available".to_string(), CheckState::Pass));
        } else {
            checks.push((
                "Docker socket not found (workspace creation may fail)".to_string(),
                CheckState::Warn,
            ));
        }
    } else {
        checks.push((
            "Coder: not configured (local mode)".to_string(),
            CheckState::Warn,
        ));
    }

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

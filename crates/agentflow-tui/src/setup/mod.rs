use anyhow::Result;
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use std::io;

pub mod model_discovery;
pub mod step_agents;
pub mod step_api;
pub mod step_coder;
pub mod step_domains;
pub mod step_done;
pub mod step_env;
pub mod step_existing;
pub mod step_github;
pub mod step_mode;
pub mod step_provider;
pub mod step_proxy;
pub mod step_repo;
pub mod step_security;
pub mod step_welcome;

use step_agents::AgentsStep;
use step_api::ApiStep;
use step_coder::CoderStep;
use step_domains::DomainsStep;
use step_done::DoneStep;
use step_env::EnvStep;
use step_existing::{ConfigAction, ExistingConfigStep};
use step_github::GitHubStep;
use step_mode::{ModeStep, SetupMode};
use step_provider::ProviderStep;
use step_proxy::ProxyStep;
use step_repo::RepoStep;
use step_security::SecurityStep;
use step_welcome::WelcomeStep;

#[derive(Debug, Clone)]
pub struct AgentConfig {
    pub id: String,
    pub cli: String,
    pub active: bool,
    pub instances: u32,
    pub model_backend: Option<String>,
    pub routing_key: Option<String>,
    pub github_token_env: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DomainMode {
    Manual,
    All,
}

#[derive(Debug, Clone)]
pub struct SetupConfig {
    pub github_pat: String,
    pub anthropic_key: String,
    pub openai_key: Option<String>,
    pub fireworks_key: Option<String>,
    pub repo: String,
    pub workspace_dir: String,
    pub proxy_enabled: bool,
    pub proxy_url: Option<String>,
    pub proxy_api_key: Option<String>,
    pub gateway_url: Option<String>,
    pub gateway_api_key: Option<String>,
    pub selected_provider: Option<String>,
    pub selected_cli_backend: String,
    pub agent_tokens: Vec<(String, String)>,
    pub agents: Vec<AgentConfig>,
    pub domain_mode: DomainMode,
    pub allowed_domains: Vec<String>,
    pub workspace_provider: WorkspaceProvider,
    pub coder_url: Option<String>,
    pub coder_admin_password: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkspaceProvider {
    Local,
    Coder,
}

impl From<WorkspaceProvider> for config::state::WorkspaceProvider {
    fn from(value: WorkspaceProvider) -> Self {
        match value {
            WorkspaceProvider::Local => config::state::WorkspaceProvider::Local,
            WorkspaceProvider::Coder => config::state::WorkspaceProvider::Coder,
        }
    }
}

impl Default for SetupConfig {
    fn default() -> Self {
        let home = std::env::var("HOME")
            .or_else(|_| std::env::var("USERPROFILE"))
            .unwrap_or_else(|_| ".".to_string());

        Self {
            github_pat: String::new(),
            anthropic_key: String::new(),
            openai_key: None,
            fireworks_key: None,
            repo: "owner/repo".to_string(),
            workspace_dir: format!("{}/.agentflow/workspaces", home),
            proxy_enabled: false,
            proxy_url: Some("http://localhost:8080/v1".to_string()),
            proxy_api_key: None,
            gateway_url: Some("https://api.fireworks.ai/inference/v1/".to_string()),
            gateway_api_key: None,
            selected_provider: None,
            selected_cli_backend: "codex".to_string(),
            agent_tokens: Vec::new(),
            agents: Vec::new(),
            domain_mode: DomainMode::Manual,
            allowed_domains: vec!["api.github.com".to_string(), "*.github.com".to_string()],
            workspace_provider: WorkspaceProvider::Local,
            coder_url: Some("http://localhost:7080".to_string()),
            coder_admin_password: Some("openflows".to_string()),
        }
    }
}

pub async fn run_wizard(_app: &mut crate::app::App) -> Result<()> {
    let terminal = crate::init_tui()?;
    let result = run_wizard_inner(terminal).await;
    crate::restore_tui();
    result
}

async fn run_wizard_inner(mut terminal: Terminal<CrosstermBackend<io::Stdout>>) -> Result<()> {
    let mut config = SetupConfig::default();
    let theme = crate::util::theme::Theme::default();

    // Step 0: Welcome screen with logo
    let welcome_step = WelcomeStep::new();
    welcome_step.render(&mut terminal, &theme)?;

    // Step 1: Security disclaimer
    let mut security_step = SecurityStep::new();
    security_step.render(&mut terminal, &theme)?;

    if !security_step.is_confirmed() {
        return Err(anyhow::anyhow!("Security disclaimer not accepted"));
    }

    // Step 2: Setup mode selection
    let mut mode_step = ModeStep::new();
    mode_step.render(&mut terminal, &theme)?;

    let setup_mode = mode_step.selected_mode();

    // Step 3: Check for existing config
    let mut existing_step = ExistingConfigStep::new();
    existing_step.render(&mut terminal, &theme, &mut config)?;

    match existing_step.action() {
        ConfigAction::Cancel => {
            return Err(anyhow::anyhow!("Setup cancelled by user"));
        }
        ConfigAction::UseExisting => {
            // Config already populated from existing, skip to completion
            let done_step = DoneStep::new();
            done_step.render(&mut terminal, &theme, &config).await?;
            return Ok(());
        }
        ConfigAction::EditExisting => {
            // Config already populated from existing, continue with full setup
            // This allows user to edit existing values through the wizard
        }
        ConfigAction::Reconfigure => {
            // Continue with full setup (fresh config)
        }
    }

    // Step 4: Workspace mode (Local vs Coder) — primary architecture decision
    let mut coder_step = CoderStep::new();
    coder_step
        .render(&mut terminal, &theme, &mut config)
        .await?;

    // Step 5: Environment check
    let env_step = EnvStep::new();
    env_step.render(&mut terminal, &theme)?;

    // Step 6: Provider selection (must come before agent config)
    let mut provider_step = ProviderStep::new();
    provider_step
        .render(&mut terminal, &theme, &mut config)
        .await?;

    // Step 7: LLM API Key Input (based on selected provider)
    let api_step = ApiStep::new();
    api_step.render(&mut terminal, &theme, &mut config).await?;

    // Step 8: Agent Configuration (instances, model backend filtered by provider)
    let agents_step = AgentsStep::new();
    agents_step
        .render(&mut terminal, &theme, &mut config)
        .await?;

    // Step 9: GitHub Authentication (uses agent config to determine token fields)
    let github_step = GitHubStep::new();
    github_step
        .render(&mut terminal, &theme, &mut config)
        .await?;

    // Step 10: Repository Config
    let repo_step = RepoStep::new();
    repo_step.render(&mut terminal, &theme, &mut config).await?;

    // Step 11: Domain Configuration
    let domains_step = DomainsStep::new();
    domains_step
        .render(&mut terminal, &theme, &mut config)
        .await?;

    // Step 12: Proxy Config (advanced mode only)
    if setup_mode == SetupMode::Advanced {
        let proxy_step = ProxyStep::new();
        proxy_step
            .render(&mut terminal, &theme, &mut config)
            .await?;
    }

    // Step 13: Completion
    let done_step = DoneStep::new();
    done_step.render(&mut terminal, &theme, &config).await?;

    Ok(())
}

/// Detect the full path to a CLI binary, or return the binary name if not found in PATH
fn detect_cli_path(binary: &str) -> String {
    std::process::Command::new("which")
        .arg(binary)
        .output()
        .ok()
        .and_then(|output| {
            if output.status.success() {
                String::from_utf8(output.stdout)
                    .ok()
                    .map(|s| s.trim().to_string())
            } else {
                None
            }
        })
        .unwrap_or_else(|| binary.to_string())
}

pub fn write_env_file(config: &SetupConfig, project_dir: &std::path::Path) -> Result<()> {
    let mut content = String::new();
    content.push_str("# Generated by OpenFlows setup\n");

    // Write per-agent GitHub tokens
    for (env_key, token) in &config.agent_tokens {
        content.push_str(&format!("{}={}\n", env_key, token));
    }

    // Also write the general GitHub PAT if set
    if !config.github_pat.is_empty() {
        content.push_str(&format!(
            "GITHUB_PERSONAL_ACCESS_TOKEN={}\n",
            config.github_pat
        ));
    }

    content.push_str(&format!("GITHUB_REPOSITORY={}\n", config.repo));

    // Only write the API key for the selected provider
    match config.selected_provider.as_deref() {
        Some(p) if p.contains("Anthropic") => {
            if !config.anthropic_key.is_empty() {
                content.push_str(&format!("ANTHROPIC_API_KEY={}\n", config.anthropic_key));
                content.push_str("DEFAULT_CLI=claude\n");
                // Detect and set Claude CLI path
                let claude_path = detect_cli_path("claude");
                content.push_str(&format!("CLAUDE_PATH={}\n", claude_path));
            }
        }
        Some(p) if p.contains("OpenAI") || p.contains("Codex") => {
            if let Some(ref key) = config.openai_key {
                content.push_str(&format!("OPENAI_API_KEY={}\n", key));
                content.push_str("DEFAULT_CLI=codex\n");
                // Detect and set Codex CLI path
                let codex_path = detect_cli_path("codex");
                content.push_str(&format!("CODEX_PATH={}\n", codex_path));
            }
        }
        Some(p) if p.contains("Fireworks") => {
            if let Some(ref key) = config.fireworks_key {
                content.push_str(&format!("FIREWORKS_API_KEY={}\n", key));
                content.push_str("DEFAULT_CLI=codex\n");
                // Detect and set Codex CLI path
                let codex_path = detect_cli_path("codex");
                content.push_str(&format!("CODEX_PATH={}\n", codex_path));
                // For Fireworks, also set OPENAI_BASE_URL to Fireworks endpoint
                content.push_str("OPENAI_BASE_URL=https://api.fireworks.ai/inference/v1\n");
            }
        }
        _ => {}
    }

    if config.proxy_enabled {
        if let Some(ref url) = config.proxy_url {
            content.push_str(&format!("PROXY_URL={}\n", url));
        }
        if let Some(ref key) = config.proxy_api_key {
            content.push_str(&format!("PROXY_API_KEY={}\n", key));
        }
        if let Some(ref url) = config.gateway_url {
            content.push_str(&format!("GATEWAY_URL={}\n", url));
        }
        if let Some(ref key) = config.gateway_api_key {
            content.push_str(&format!("GATEWAY_API_KEY={}\n", key));
        }
    }

    content.push_str(&format!(
        "AGENTFLOW_WORKSPACE_ROOT={}\n",
        config.workspace_dir
    ));
    content.push_str("RUST_LOG=info,agent_team=debug,pocketflow_core=debug\n");

    // Coder workspace configuration
    match config.workspace_provider {
        WorkspaceProvider::Coder => {
            content.push_str("WORKSPACE_PROVIDER=coder\n");
            if let Some(ref url) = config.coder_url {
                content.push_str(&format!("CODER_URL={}\n", url));
            }
            if let Some(ref password) = config.coder_admin_password {
                content.push_str(&format!("CODER_ADMIN_PASSWORD={}\n", password));
            }
            content.push_str("CODER_ADMIN_USERNAME=admin\n");
            content.push_str("CODER_ADMIN_EMAIL=admin@openflows.dev\n");
            content.push_str("CODER_PG_PASSWORD=coder\n");
        }
        WorkspaceProvider::Local => {
            content.push_str("WORKSPACE_PROVIDER=local\n");
        }
    }

    // Write domain configuration
    if config.domain_mode == DomainMode::All {
        content.push_str("AGENTFLOW_DOMAIN_MODE=all\n");
    } else {
        content.push_str("AGENTFLOW_DOMAIN_MODE=manual\n");
    }
    if !config.allowed_domains.is_empty() {
        content.push_str(&format!(
            "AGENTFLOW_ALLOWED_DOMAINS={}\n",
            config.allowed_domains.join(",")
        ));
    }

    // GitHub MCP server command
    content.push_str("GITHUB_MCP_CMD=\"npx -y @modelcontextprotocol/server-github\"\n");

    std::fs::write(project_dir.join(".env"), content)?;
    Ok(())
}

pub fn write_registry_file(config: &SetupConfig, project_dir: &std::path::Path) -> Result<()> {
    let registry_dir = project_dir.join("orchestration").join("agent");
    std::fs::create_dir_all(&registry_dir)?;

    // Determine default CLI based on selected provider
    let default_cli = match config.selected_provider.as_deref() {
        Some(p) if p.contains("Anthropic") => "claude".to_string(),
        Some(p) if p.contains("OpenAI") || p.contains("Codex") || p.contains("Fireworks") => {
            "codex".to_string()
        }
        _ => config.selected_cli_backend.clone(),
    };

    let default_model = config
        .agents
        .first()
        .and_then(|a| a.model_backend.clone())
        .unwrap_or_else(|| match config.selected_provider.as_deref() {
            Some(p) if p.contains("Anthropic") => {
                "anthropic/claude-3-5-sonnet-20241022".to_string()
            }
            Some(p) if p.contains("Fireworks") => {
                "fireworks/accounts/fireworks/models/llama-v3p1-8b-instruct".to_string()
            }
            _ => "openai/gpt-4o".to_string(),
        });

    let registry = config::Registry {
        default_cli: default_cli.clone(),
        allowed_domains: vec![
            "api.github.com".to_string(),
            "*.github.com".to_string(),
            "pypi.org".to_string(),
            "registry.npmjs.org".to_string(),
            "crates.io".to_string(),
        ],
        team: if config.agents.is_empty() {
            // Default agents if none configured - use codex CLI for all supported providers
            vec![
                config::RegistryEntry {
                    id: "nexus".to_string(),
                    cli: default_cli.clone(),
                    active: true,
                    instances: 1,
                    model_backend: Some(default_model.clone()),
                    routing_key: Some("nexus-key".to_string()),
                    github_token_env: Some("AGENT_NEXUS_GITHUB_TOKEN".to_string()),
                    allowed_domains: None,
                    workspace_provider: Some(config::state::WorkspaceProvider::from(config.workspace_provider.clone())),
                },
                config::RegistryEntry {
                    id: "forge".to_string(),
                    cli: default_cli.clone(),
                    active: true,
                    instances: 2,
                    model_backend: Some(default_model.clone()),
                    routing_key: Some("forge-key".to_string()),
                    github_token_env: Some("AGENT_FORGE_GITHUB_TOKEN".to_string()),
                    allowed_domains: Some(vec![
                        "api.github.com".to_string(),
                        "*.github.com".to_string(),
                        "pypi.org".to_string(),
                        "registry.npmjs.org".to_string(),
                        "crates.io".to_string(),
                    ]),
                    workspace_provider: Some(config::state::WorkspaceProvider::from(config.workspace_provider.clone())),
                },
                config::RegistryEntry {
                    id: "sentinel".to_string(),
                    cli: default_cli.clone(),
                    active: true,
                    instances: 1,
                    model_backend: Some(default_model.clone()),
                    routing_key: Some("sentinel-key".to_string()),
                    github_token_env: Some("AGENT_SENTINEL_GITHUB_TOKEN".to_string()),
                    allowed_domains: Some(vec![
                        "api.github.com".to_string(),
                        "*.github.com".to_string(),
                    ]),
                    workspace_provider: Some(config::state::WorkspaceProvider::from(config.workspace_provider.clone())),
                },
                config::RegistryEntry {
                    id: "vessel".to_string(),
                    cli: default_cli.clone(),
                    active: true,
                    instances: 1,
                    model_backend: Some(default_model.clone()),
                    routing_key: Some("vessel-key".to_string()),
                    github_token_env: Some("AGENT_VESSEL_GITHUB_TOKEN".to_string()),
                    allowed_domains: None,
                    workspace_provider: Some(config::state::WorkspaceProvider::from(config.workspace_provider.clone())),
                },
                config::RegistryEntry {
                    id: "lore".to_string(),
                    cli: default_cli.clone(),
                    active: false,
                    instances: 1,
                    model_backend: Some(default_model.clone()),
                    routing_key: Some("lore-key".to_string()),
                    github_token_env: Some("AGENT_LORE_GITHUB_TOKEN".to_string()),
                    allowed_domains: None,
                    workspace_provider: Some(config::state::WorkspaceProvider::from(config.workspace_provider.clone())),
                },
            ]
        } else {
            config
                .agents
                .iter()
                .map(|agent| {
                    // Ensure github_token_env is set - use agent's value or generate default
                    let token_env = agent.github_token_env.clone().or_else(|| {
                        if agent.active {
                            Some(format!("AGENT_{}_GITHUB_TOKEN", agent.id.to_uppercase()))
                        } else {
                            None
                        }
                    });
                    config::RegistryEntry {
                        id: agent.id.clone(),
                        // Update CLI to match selected provider's CLI backend
                        cli: default_cli.clone(),
                        active: agent.active,
                        instances: agent.instances,
                        model_backend: agent.model_backend.clone(),
                        routing_key: agent.routing_key.clone(),
                        github_token_env: token_env,
                        allowed_domains: None,
                        workspace_provider: Some(config::state::WorkspaceProvider::from(config.workspace_provider.clone())),
                    }
                })
                .collect()
        },
    };

    let content = serde_json::to_string_pretty(&registry)?;
    std::fs::write(registry_dir.join("registry.json"), content)?;
    Ok(())
}

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
pub mod step_module;
pub mod step_notifications;
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
use step_module::ModuleStep;
use step_notifications::NotificationsStep;
use step_provider::ProviderStep;
use step_proxy::ProxyStep;
use step_repo::RepoStep;
use step_security::SecurityStep;
use step_welcome::WelcomeStep;

/// Resolve the Coder Registry module for a given CLI backend and role.
/// Used by step_module.rs and write_registry_file.
pub fn resolve_coder_module_for_cli(cli: &str, _role: &str) -> config::registry::CoderModule {
    config::registry::resolve_coder_module(cli, None)
}

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
    pub agent_tokens: Vec<(String, String)>,
    pub agents: Vec<AgentConfig>,
    pub domain_mode: DomainMode,
    pub allowed_domains: Vec<String>,
    pub workspace_provider: WorkspaceProvider,
    pub coder_url: Option<String>,
    pub coder_admin_password: Option<String>,
    pub enable_ai_gateway: bool,
    pub enable_slackme: bool,
    pub slack_webhook_url: Option<String>,
    pub discord_webhook_url: Option<String>,
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
            agent_tokens: Vec::new(),
            agents: Vec::new(),
            domain_mode: DomainMode::Manual,
            allowed_domains: vec!["api.github.com".to_string(), "*.github.com".to_string()],
            workspace_provider: WorkspaceProvider::Local,
            coder_url: Some("http://localhost:7080".to_string()),
            coder_admin_password: Some("openflows".to_string()),
            enable_ai_gateway: true,
            enable_slackme: false,
            slack_webhook_url: None,
            discord_webhook_url: None,
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

    // Step 8b: Agent Module Selection (Coder mode only)
    if config.workspace_provider == WorkspaceProvider::Coder {
        let mut module_step = ModuleStep::new();
        module_step
            .render(&mut terminal, &theme, &mut config)
            .await?;

        // Step 8c: Notification Configuration (Coder mode only)
        let mut notifications_step = NotificationsStep::new();
        notifications_step
            .render(&mut terminal, &theme, &mut config)
            .await?;
    }

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
            }
        }
        Some(p) if p.contains("OpenAI") || p.contains("Codex") => {
            if let Some(ref key) = config.openai_key {
                content.push_str(&format!("OPENAI_API_KEY={}\n", key));
            }
        }
        Some(p) if p.contains("Fireworks") => {
            if let Some(ref key) = config.fireworks_key {
                content.push_str(&format!("FIREWORKS_API_KEY={}\n", key));
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
            content.push_str(&format!(
                "USE_AI_GATEWAY={}\n",
                if config.enable_ai_gateway {
                    "true"
                } else {
                    "false"
                }
            ));
            content.push_str(&format!(
                "ENABLE_SLACKME={}\n",
                if config.enable_slackme {
                    "true"
                } else {
                    "false"
                }
            ));

            // Coder external auth for slackme (Slack DM notifications)
            if config.enable_slackme {
                content.push_str("CODER_EXTERNAL_AUTH_1_TYPE=slack\n");
                // Client ID and secret would come from Slack app configuration
                // These are placeholders — user must fill in their real values
                content.push_str("# CODER_EXTERNAL_AUTH_1_CLIENT_ID=<your-slack-client-id>\n");
                content
                    .push_str("# CODER_EXTERNAL_AUTH_1_CLIENT_SECRET=<your-slack-client-secret>\n");
            }
        }
        WorkspaceProvider::Local => {
            content.push_str("WORKSPACE_PROVIDER=local\n");
        }
    }

    // LiteLLM fallback proxy (available in both modes)
    content.push_str("LITELLM_PROXY_URL=http://proxy:4000\n");

    // Notification webhook URLs
    if let Some(ref url) = config.slack_webhook_url {
        content.push_str(&format!("SLACK_WEBHOOK_URL={}\n", url));
    }
    if let Some(ref url) = config.discord_webhook_url {
        content.push_str(&format!("DISCORD_WEBHOOK_URL={}\n", url));
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
        _ => "codex".to_string(),
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

    // Resolve coder_module for a given CLI when workspace_provider is Coder
    let is_coder = config.workspace_provider == WorkspaceProvider::Coder;
    let resolve_coder_module = |cli: &str, role: &str| -> Option<config::registry::CoderModule> {
        if !is_coder {
            return None;
        }
        let (source, version) = config::registry::DEFAULT_AGENT_MODULES
            .iter()
            .find(|(key, _, _)| *key == cli)
            .map(|(_, source, version)| (source.to_string(), version.to_string()))
            .unwrap_or_else(|| {
                (
                    "registry.coder.com/coder/claude-code/coder".to_string(),
                    "5.2.0".to_string(),
                )
            });
        let permission_mode = config::registry::default_permission_mode_for_role(role);
        let mut params = serde_json::Map::new();
        params.insert(
            "workdir".to_string(),
            serde_json::Value::String("/home/coder/workspace".to_string()),
        );
        params.insert(
            "permission_mode".to_string(),
            serde_json::Value::String(permission_mode.to_string()),
        );
        params.insert(
            "enable_ai_gateway".to_string(),
            serde_json::Value::Bool(config.enable_ai_gateway),
        );
        Some(config::registry::CoderModule::with_params(
            source,
            version,
            serde_json::Value::Object(params),
        ))
    };

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
                    workspace_provider: Some(config::state::WorkspaceProvider::from(
                        config.workspace_provider.clone(),
                    )),
                    coder_module: resolve_coder_module(&default_cli, "nexus"),
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
                    workspace_provider: Some(config::state::WorkspaceProvider::from(
                        config.workspace_provider.clone(),
                    )),
                    coder_module: resolve_coder_module(&default_cli, "forge"),
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
                    workspace_provider: Some(config::state::WorkspaceProvider::from(
                        config.workspace_provider.clone(),
                    )),
                    coder_module: resolve_coder_module(&default_cli, "sentinel"),
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
                    workspace_provider: Some(config::state::WorkspaceProvider::from(
                        config.workspace_provider.clone(),
                    )),
                    coder_module: resolve_coder_module(&default_cli, "vessel"),
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
                    workspace_provider: Some(config::state::WorkspaceProvider::from(
                        config.workspace_provider.clone(),
                    )),
                    coder_module: resolve_coder_module(&default_cli, "lore"),
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
                    let cli = if agent.cli.is_empty() {
                        default_cli.clone()
                    } else {
                        agent.cli.clone()
                    };
                    config::RegistryEntry {
                        id: agent.id.clone(),
                        cli: cli.clone(),
                        active: agent.active,
                        instances: agent.instances,
                        model_backend: agent.model_backend.clone(),
                        routing_key: agent.routing_key.clone(),
                        github_token_env: token_env,
                        allowed_domains: None,
                        workspace_provider: Some(config::state::WorkspaceProvider::from(
                            config.workspace_provider.clone(),
                        )),
                        coder_module: resolve_coder_module(&cli, &agent.id),
                    }
                })
                .collect()
        },
    };

    let content = serde_json::to_string_pretty(&registry)?;
    std::fs::write(registry_dir.join("registry.json"), content)?;
    Ok(())
}

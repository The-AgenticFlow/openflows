use anyhow::Result;
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use std::io;

pub mod step_agents;
pub mod step_api;
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

#[derive(Debug, Clone)]
pub struct SetupConfig {
    pub anthropic_key: String,
    pub github_pat: String,
    pub gemini_key: Option<String>,
    pub openai_key: Option<String>,
    pub fireworks_key: Option<String>,
    pub fireworks_api_format: String, // "anthropic" (default) or "openai"
    pub repo: String,
    pub workspace_dir: String,
    pub proxy_enabled: bool,
    pub proxy_url: Option<String>,
    pub proxy_api_key: Option<String>,
    pub proxy_target_model: Option<String>,
    pub gateway_url: Option<String>,
    pub gateway_api_key: Option<String>,
    pub selected_provider: Option<String>,
    pub agent_tokens: Vec<(String, String)>,
    pub agents: Vec<AgentConfig>,
}

impl Default for SetupConfig {
    fn default() -> Self {
        let home = std::env::var("HOME")
            .or_else(|_| std::env::var("USERPROFILE"))
            .unwrap_or_else(|_| ".".to_string());

        Self {
            anthropic_key: String::new(),
            github_pat: String::new(),
            gemini_key: None,
            openai_key: None,
            fireworks_key: None,
            fireworks_api_format: "anthropic".to_string(), // Default to Anthropic compatibility
            repo: "owner/repo".to_string(),
            workspace_dir: format!("{}/.agentflow/workspaces", home),
            proxy_enabled: false,
            proxy_url: Some("http://localhost:8765/v1".to_string()),
            proxy_api_key: None,
            proxy_target_model: None,
            gateway_url: Some("https://api.fireworks.ai/inference/v1/".to_string()),
            gateway_api_key: None,
            selected_provider: None,
            agent_tokens: Vec::new(),
            agents: Vec::new(),
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

    // Step 4: Environment check
    let env_step = EnvStep::new();
    env_step.render(&mut terminal, &theme)?;

    // Step 5: Provider selection (must come before agent config)
    let mut provider_step = ProviderStep::new();
    provider_step.render(&mut terminal, &theme, &mut config).await?;

    // Step 6: LLM API Key Input (based on selected provider)
    let mut api_step = ApiStep::new();
    api_step.render(&mut terminal, &theme, &mut config).await?;

    // Step 7: Agent Configuration (instances, model backend filtered by provider)
    let agents_step = AgentsStep::new();
    agents_step.render(&mut terminal, &theme, &mut config).await?;

    // Step 8: GitHub Authentication (uses agent config to determine token fields)
    let github_step = GitHubStep::new();
    github_step.render(&mut terminal, &theme, &mut config).await?;

    // Step 9: Repository Config
    let repo_step = RepoStep::new();
    repo_step.render(&mut terminal, &theme, &mut config).await?;

    // Step 10: Proxy Config (always shown for Fireworks, otherwise advanced mode only)
    let is_fireworks = config.selected_provider.as_deref() == Some("Fireworks AI");
    if is_fireworks || setup_mode == SetupMode::Advanced {
        let proxy_step = ProxyStep::new();
        proxy_step
            .render(&mut terminal, &theme, &mut config)
            .await?;
    }

    // Step 11: Completion
    let done_step = DoneStep::new();
    done_step.render(&mut terminal, &theme, &config).await?;

    Ok(())
}

pub fn write_env_file(config: &SetupConfig, project_dir: &std::path::Path) -> Result<()> {
    let mut content = String::new();
    content.push_str("# Generated by OpenFlow setup\n\n");
    
    // Check if Fireworks is selected
    let is_fireworks = config.selected_provider.as_deref() == Some("Fireworks AI");
    
    // Write the appropriate API key based on provider
    if is_fireworks {
        if let Some(ref key) = config.fireworks_key {
            content.push_str("# Fireworks AI Configuration\n");
            content.push_str(&format!("FIREWORKS_API_KEY={}\n", key));
            content.push_str("FIREWORKS_API_FORMAT=openai\n"); // Fireworks only supports OpenAI format
            content.push_str("\n# Proxy Configuration (required for Claude CLI with Fireworks)\n");
            content.push_str("PORT=8765\n");
            content.push_str("PROXY_URL=http://localhost:8765/v1\n");
            content.push_str(&format!("PROXY_API_KEY={}\n", key));
            content.push_str("\n# Fireworks Gateway\n");
            content.push_str("GATEWAY_URL=https://api.fireworks.ai/inference/v1/\n");
            content.push_str(&format!("GATEWAY_API_KEY={}\n", key));
            
            // Write PROXY_TARGET_MODEL for dynamic model mapping
            let target_model = config.proxy_target_model.as_deref()
                .unwrap_or("accounts/fireworks/models/glm-5");
            content.push_str("\n# Model Mapping: ALL Claude model names → target model\n");
            content.push_str("# The proxy strips ANSI codes and maps claude-*, opus, sonnet, haiku → target\n");
            content.push_str(&format!("PROXY_TARGET_MODEL={}\n", target_model));
            
            // Also write legacy MODEL_MAP for backward compatibility
            let fallback_model = target_model;
            content.push_str(&format!("MODEL_MAP=claude-haiku-4-5-20251001={fallback_model},claude-3-5-haiku-20241022={fallback_model}\n"));
        }
    } else if !config.anthropic_key.is_empty() {
        content.push_str("# Anthropic API Configuration\n");
        content.push_str(&format!("ANTHROPIC_API_KEY={}\n", config.anthropic_key));
    }
    
    if let Some(ref key) = config.gemini_key {
        if !is_fireworks {
            content.push_str(&format!("GEMINI_API_KEY={}\n", key));
        }
    }
    if let Some(ref key) = config.openai_key {
        if !is_fireworks {
            content.push_str(&format!("OPENAI_API_KEY={}\n", key));
        }
    }
    
    content.push_str("\n# GitHub Configuration\n");
    // Write per-agent GitHub tokens
    for (env_key, token) in &config.agent_tokens {
        content.push_str(&format!("{}={}\n", env_key, token));
    }
    
    // Also write the general GitHub PAT if set
    if !config.github_pat.is_empty() {
        content.push_str(&format!("GITHUB_PERSONAL_ACCESS_TOKEN={}\n", config.github_pat));
    }
    content.push_str(&format!("GITHUB_REPOSITORY={}\n", config.repo));
    
    // Legacy proxy config (only if explicitly enabled in advanced mode)
    if config.proxy_enabled && !is_fireworks {
        content.push_str("\n# Advanced Proxy Configuration\n");
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
    
    content.push_str("\n# Logging\n");
    content.push_str("RUST_LOG=info\n");
    content.push_str(&format!("\n# Workspace\n"));
    content.push_str(&format!("AGENTFLOW_WORKSPACE_ROOT={}\n", config.workspace_dir));
    
    std::fs::write(project_dir.join(".env"), content)?;
    Ok(())
}

pub fn write_registry_file(config: &SetupConfig, project_dir: &std::path::Path) -> Result<()> {
    let registry_dir = project_dir.join("orchestration").join("agent");
    std::fs::create_dir_all(&registry_dir)?;
    
    // Determine default model based on selected provider
    let is_fireworks = config.selected_provider.as_deref() == Some("Fireworks AI");
    let default_model = if is_fireworks {
        "accounts/fireworks/models/glm-5"
    } else {
        "anthropic/claude-sonnet-4-5"
    };

    let registry = config::Registry {
        team: if config.agents.is_empty() {
            // Default agents if none configured - use provider-appropriate models
            vec![
                config::RegistryEntry {
                    id: "nexus".to_string(),
                    cli: "claude".to_string(),
                    active: true,
                    instances: 1,
                    model_backend: Some(default_model.to_string()),
                    routing_key: Some("nexus-key".to_string()),
                    github_token_env: Some("AGENT_NEXUS_GITHUB_TOKEN".to_string()),
                },
                config::RegistryEntry {
                    id: "forge".to_string(),
                    cli: "claude".to_string(),
                    active: true,
                    instances: 2,
                    model_backend: Some(default_model.to_string()),
                    routing_key: Some("forge-key".to_string()),
                    github_token_env: Some("AGENT_FORGE_GITHUB_TOKEN".to_string()),
                },
                config::RegistryEntry {
                    id: "sentinel".to_string(),
                    cli: "claude".to_string(),
                    active: true,
                    instances: 1,
                    model_backend: Some(default_model.to_string()),
                    routing_key: Some("sentinel-key".to_string()),
                    github_token_env: Some("AGENT_SENTINEL_GITHUB_TOKEN".to_string()),
                },
                config::RegistryEntry {
                    id: "vessel".to_string(),
                    cli: "claude".to_string(),
                    active: true,
                    instances: 1,
                    model_backend: Some(default_model.to_string()),
                    routing_key: Some("vessel-key".to_string()),
                    github_token_env: Some("AGENT_VESSEL_GITHUB_TOKEN".to_string()),
                },
                config::RegistryEntry {
                    id: "lore".to_string(),
                    cli: "claude".to_string(),
                    active: false,
                    instances: 1,
                    model_backend: Some(default_model.to_string()),
                    routing_key: Some("lore-key".to_string()),
                    github_token_env: None, // No token needed for inactive agents
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
                        cli: agent.cli.clone(),
                        active: agent.active,
                        instances: agent.instances,
                        model_backend: agent.model_backend.clone(),
                        routing_key: agent.routing_key.clone(),
                        github_token_env: token_env,
                    }
                })
                .collect()
        },
    };

    let content = serde_json::to_string_pretty(&registry)?;
    std::fs::write(registry_dir.join("registry.json"), content)?;
    Ok(())
}

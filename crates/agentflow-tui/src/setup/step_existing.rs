use anyhow::Result;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::prelude::*;
use ratatui::widgets::Paragraph;
use ratatui::Terminal;
use std::io;
use std::path::Path;

use crate::setup::SetupConfig;
use crate::util::theme::Theme;
use crate::widgets::infobox::KeyValueBox;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ConfigAction {
    UseExisting,
    EditExisting,
    Reconfigure,
    Cancel,
}

pub struct ExistingConfigStep {
    action: ConfigAction,
    existing_config: Option<SetupConfig>,
}

impl Default for ExistingConfigStep {
    fn default() -> Self {
        Self {
            action: ConfigAction::UseExisting,
            existing_config: None,
        }
    }
}

impl ExistingConfigStep {
    pub fn new() -> Self {
        Self {
            action: ConfigAction::UseExisting,
            existing_config: None,
        }
    }

    pub fn detect_existing_config(project_dir: &Path) -> Option<SetupConfig> {
        let env_path = project_dir.join(".env");
        if !env_path.exists() {
            return None;
        }

        let content = std::fs::read_to_string(env_path).ok()?;
        let mut config = SetupConfig::default();

        for line in content.lines() {
            if line.starts_with('#') || line.is_empty() {
                continue;
            }
            if let Some((key, value)) = line.split_once('=') {
                match key.trim() {
                    "ANTHROPIC_API_KEY" => {
                        config.anthropic_key = value.to_string();
                        config.selected_provider = Some("Anthropic (Claude)".to_string());
                    }
                    "GITHUB_PERSONAL_ACCESS_TOKEN" => config.github_pat = value.to_string(),
                    "GITHUB_REPOSITORY" => config.repo = value.to_string(),
                    "GEMINI_API_KEY" => {
                        config.gemini_key = Some(value.to_string());
                        config.selected_provider = Some("Google Gemini".to_string());
                    }
                    "OPENAI_API_KEY" => {
                        config.openai_key = Some(value.to_string());
                        config.selected_provider = Some("OpenAI".to_string());
                    }
                    "FIREWORKS_API_KEY" => {
                        config.fireworks_key = Some(value.to_string());
                        config.selected_provider = Some("Fireworks AI".to_string());
                    }
                    "AGENTFLOW_WORKSPACE_ROOT" => config.workspace_dir = value.to_string(),
                    "PROXY_URL" => {
                        config.proxy_enabled = true;
                        config.proxy_url = Some(value.to_string());
                    }
                    "PROXY_API_KEY" => config.proxy_api_key = Some(value.to_string()),
                    "GATEWAY_URL" => config.gateway_url = Some(value.to_string()),
                    "GATEWAY_API_KEY" => config.gateway_api_key = Some(value.to_string()),
                    _ => {
                        // Capture per-agent tokens
                        if key.trim().starts_with("AGENT_") && key.trim().ends_with("_GITHUB_TOKEN")
                        {
                            config
                                .agent_tokens
                                .push((key.trim().to_string(), value.to_string()));
                        }
                    }
                }
            }
        }

        // Also load agents from registry.json
        let registry_path = project_dir
            .join("orchestration")
            .join("agent")
            .join("registry.json");

        if registry_path.exists() {
            if let Ok(registry) = config::Registry::load(&registry_path) {
                config.agents = registry
                    .team
                    .iter()
                    .map(|entry| crate::setup::AgentConfig {
                        id: entry.id.clone(),
                        cli: entry.cli.clone(),
                        active: entry.active,
                        instances: entry.instances,
                        model_backend: entry.model_backend.clone(),
                        routing_key: entry.routing_key.clone(),
                        github_token_env: entry.github_token_env.clone(),
                    })
                    .collect();
            }
        }

        Some(config)
    }

    pub fn action(&self) -> ConfigAction {
        self.action
    }

    pub fn existing_config(&self) -> Option<&SetupConfig> {
        self.existing_config.as_ref()
    }

    pub fn render(
        &mut self,
        terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
        theme: &Theme,
        config: &mut SetupConfig,
    ) -> Result<()> {
        let project_dir = std::env::current_dir()?;
        if let Some(existing) = Self::detect_existing_config(&project_dir) {
            self.existing_config = Some(existing.clone());
        } else {
            return Ok(());
        }

        let actions = [
            "Use existing values (skip setup)".to_string(),
            "Edit existing values".to_string(),
            "Reconfigure everything from scratch".to_string(),
            "Cancel setup".to_string(),
        ];
        let mut selected = 0;

        loop {
            terminal.draw(|f| {
                let area = f.area();

                let chunks = Layout::default()
                    .direction(Direction::Vertical)
                    .margin(2)
                    .constraints([
                        Constraint::Length(2),
                        Constraint::Length(1),
                        Constraint::Min(8),
                    ])
                    .split(area);

                let title_line = Line::styled(
                    "┌  OpenFlow Setup",
                    Style::default()
                        .fg(theme.accent())
                        .add_modifier(Modifier::BOLD),
                );
                let title_para = Paragraph::new(title_line);
                title_para.render(chunks[0], f.buffer_mut());

                let sep_line = Line::styled("│", Style::default().fg(theme.border()));
                let sep_para = Paragraph::new(sep_line);
                sep_para.render(chunks[1], f.buffer_mut());

                let prompt_line = Line::styled(
                    "◇  Existing config detected",
                    Style::default()
                        .fg(theme.accent())
                        .add_modifier(Modifier::BOLD),
                );
                let prompt_para = Paragraph::new(prompt_line);
                prompt_para.render(
                    Rect::new(chunks[2].x, chunks[2].y, chunks[2].width, 1),
                    f.buffer_mut(),
                );

                if let Some(ref existing) = self.existing_config {
                    let kv_box = KeyValueBox::new("Current configuration")
                        .item("workspace", &existing.workspace_dir)
                        .item("repo", &existing.repo);
                    kv_box.render(
                        Rect::new(chunks[2].x, chunks[2].y + 2, chunks[2].width, 5),
                        f.buffer_mut(),
                    );
                }

                let mut action_lines = Vec::new();
                for (i, action) in actions.iter().enumerate() {
                    let icon = if i == selected { "●" } else { "○" };
                    let style = if i == selected {
                        Style::default()
                            .fg(theme.accent())
                            .add_modifier(Modifier::BOLD)
                    } else {
                        theme.text_style()
                    };
                    action_lines.push(Line::from(vec![
                        Span::styled(format!("  {} ", icon), style),
                        Span::styled(action.as_str(), style),
                    ]));
                }
                let action_para = Paragraph::new(action_lines);
                action_para.render(
                    Rect::new(chunks[2].x, chunks[2].y + 8, chunks[2].width, 5),
                    f.buffer_mut(),
                );
            })?;

            if crossterm::event::poll(std::time::Duration::from_millis(100))? {
                if let crossterm::event::Event::Key(key) = crossterm::event::read()? {
                    use crossterm::event::KeyCode;
                    match key.code {
                        KeyCode::Up => {
                            selected = if selected == 0 {
                                actions.len() - 1
                            } else {
                                selected - 1
                            };
                        }
                        KeyCode::Down => {
                            selected = (selected + 1) % actions.len();
                        }
                        KeyCode::Enter => {
                            self.action = match selected {
                                0 => ConfigAction::UseExisting,
                                1 => ConfigAction::EditExisting,
                                2 => ConfigAction::Reconfigure,
                                3 => ConfigAction::Cancel,
                                _ => ConfigAction::UseExisting,
                            };
                            if matches!(
                                self.action,
                                ConfigAction::UseExisting | ConfigAction::EditExisting
                            ) {
                                if let Some(ref existing) = self.existing_config {
                                    *config = existing.clone();
                                }
                            }
                            break;
                        }
                        KeyCode::Esc => {
                            self.action = ConfigAction::Cancel;
                            break;
                        }
                        _ => {}
                    }
                }
            }
        }

        Ok(())
    }
}

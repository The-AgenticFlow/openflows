use anyhow::Result;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::prelude::*;
use ratatui::widgets::Paragraph;
use ratatui::Terminal;
use std::io;
use tui_input::backend::crossterm::EventHandler;
use tui_input::Input;

use crate::setup::SetupConfig;
use crate::util::theme::Theme;
use crate::widgets::input::InputWidget;

struct GitHubField {
    label: String,
    env_key: String,
    input: Input,
    required: bool,
}

pub struct GitHubStep;

impl GitHubStep {
    pub fn new() -> Self {
        Self
    }

    pub async fn render(
        &self,
        terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
        theme: &Theme,
        config: &mut SetupConfig,
    ) -> Result<()> {
        let mut fields: Vec<GitHubField> = Vec::new();

        // Use configured agents from SetupConfig if available, otherwise fall back to registry file
        if !config.agents.is_empty() {
            for agent in &config.agents {
                if agent.active {
                    let env_key = format!("AGENT_{}_GITHUB_TOKEN", agent.id.to_uppercase());
                    let existing = std::env::var(&env_key).unwrap_or_default();
                    fields.push(GitHubField {
                        label: format!("{} GitHub PAT", agent.id.to_uppercase()),
                        env_key,
                        input: Input::new(existing),
                        required: true,
                    });
                }
            }
        } else {
            let registry_path = std::env::current_dir()?
                .join("orchestration")
                .join("agent")
                .join("registry.json");

            if registry_path.exists() {
                if let Ok(registry) = config::Registry::load(&registry_path) {
                    for entry in registry.active_agents() {
                        let env_key = entry.github_token_env.clone()
                            .unwrap_or_else(|| "GITHUB_PERSONAL_ACCESS_TOKEN".to_string());
                        let existing = std::env::var(&env_key).unwrap_or_default();
                        fields.push(GitHubField {
                            label: format!("{} GitHub PAT", entry.id.to_uppercase()),
                            env_key,
                            input: Input::new(existing),
                            required: true,
                        });
                    }
                }
            }
        }

        if fields.is_empty() {
            fields.push(GitHubField {
                label: "GitHub PAT".to_string(),
                env_key: "GITHUB_PERSONAL_ACCESS_TOKEN".to_string(),
                input: Input::new(config.github_pat.clone()),
                required: true,
            });
        }

        let total_fields = fields.len();
        let mut focused_field: usize = 0;

        loop {
            terminal.draw(|f| {
                let area = f.area();

                let chunks = Layout::default()
                    .direction(Direction::Vertical)
                    .margin(3)
                    .constraints([
                        Constraint::Length(4),
                        Constraint::Min(1),
                        Constraint::Length(2),
                    ])
                    .split(area);

                let title_block = ratatui::widgets::Block::default()
                    .borders(ratatui::widgets::Borders::BOTTOM)
                    .border_style(Style::default().fg(theme.border()));

                let inner_title = title_block.inner(chunks[0]);
                title_block.render(chunks[0], f.buffer_mut());

                let title = Line::styled(
                    "◇ GITHUB AUTHENTICATION",
                    Style::default()
                        .fg(theme.accent())
                        .add_modifier(Modifier::BOLD),
                );
                let subtitle = Line::styled(
                    "  Configure tokens for agent operations",
                    Style::default().fg(theme.muted()),
                );
                let perm_hint = Line::styled(
                    "  Tokens need: FORGE (contents+PRs+issues rw), SENTINEL (PRs rw), VESSEL (contents+PRs+workflows rw), LORE (contents rw)",
                    Style::default().fg(theme.muted()),
                );
                let title_para = ratatui::widgets::Paragraph::new(vec![title, subtitle, perm_hint]);
                title_para.render(inner_title, f.buffer_mut());

                let input_area = Rect::new(chunks[1].x, chunks[1].y, chunks[1].width, chunks[1].height);

                let field_height = 3u16;
                let field_spacing = 0u16;
                let total_per_field = field_height + field_spacing;
                
                let visible_count = ((input_area.height) / total_per_field).max(1) as usize;
                let scroll_offset = focused_field.saturating_sub(visible_count.saturating_sub(1));

                let mut current_y = input_area.y;

                for (i, field) in fields.iter().enumerate() {
                    if i < scroll_offset || i >= scroll_offset + visible_count {
                        continue;
                    }
                    
                    if current_y + field_height > input_area.y + input_area.height {
                        break;
                    }

                    let label = if field.required {
                        field.label.clone()
                    } else {
                        field.label.clone()
                    };

                    let widget = InputWidget::new(&field.input, &label)
                        .masked(true)
                        .focused(focused_field == i)
                        .optional(!field.required);
                    widget.render(
                        Rect::new(input_area.x, current_y, input_area.width, field_height),
                        f.buffer_mut(),
                    );
                    current_y += total_per_field;
                }

                let help = if fields.len() > visible_count {
                    format!("  ◄ ► navigate  │  {} of {}  │  Enter: continue  │  Esc: cancel", focused_field + 1, fields.len())
                } else {
                    "  Tab/Arrows: navigate  │  Enter: continue  │  Esc: cancel".to_string()
                };
                let help_line = Line::styled(help, Style::default().fg(theme.muted()));
                let help_para = Paragraph::new(help_line);
                help_para.render(chunks[2], f.buffer_mut());
            })?;

            if crossterm::event::poll(std::time::Duration::from_millis(100))? {
                if let crossterm::event::Event::Key(key) = crossterm::event::read()? {
                    use crossterm::event::KeyCode;
                    match key.code {
                        KeyCode::Tab => {
                            focused_field = (focused_field + 1) % total_fields;
                        }
                        KeyCode::BackTab => {
                            focused_field = if focused_field == 0 {
                                total_fields - 1
                            } else {
                                focused_field - 1
                            };
                        }
                        KeyCode::Down => {
                            focused_field = (focused_field + 1) % total_fields;
                        }
                        KeyCode::Up => {
                            focused_field = if focused_field == 0 {
                                total_fields - 1
                            } else {
                                focused_field - 1
                            };
                        }
                        KeyCode::Enter => {
                            let all_required_filled = fields.iter()
                                .filter(|f| f.required)
                                .all(|f| !f.input.value().is_empty());

                            if all_required_filled {
                                for field in &fields {
                                    let value = field.input.value().to_string();
                                    match field.env_key.as_str() {
                                        "GITHUB_PERSONAL_ACCESS_TOKEN" => {
                                            config.github_pat = value.clone();
                                            // Also set this as the github_token_env for all active agents
                                            // when using the fallback single PAT
                                            for agent in config.agents.iter_mut().filter(|a| a.active) {
                                                agent.github_token_env = Some("GITHUB_PERSONAL_ACCESS_TOKEN".to_string());
                                            }
                                        }
                                        _ => {
                                            if field.env_key.starts_with("AGENT_") {
                                                config.agent_tokens.push((field.env_key.clone(), value));
                                                // Set the github_token_env on the corresponding agent
                                                let agent_id = field.env_key
                                                    .strip_prefix("AGENT_")
                                                    .and_then(|s| s.strip_suffix("_GITHUB_TOKEN"))
                                                    .map(|s| s.to_lowercase())
                                                    .unwrap_or_default();
                                                if let Some(agent) = config.agents.iter_mut().find(|a| a.id == agent_id) {
                                                    agent.github_token_env = Some(field.env_key.clone());
                                                }
                                            }
                                        }
                                    }
                                }
                                break;
                            }
                        }
                        KeyCode::Esc => {
                            return Err(anyhow::anyhow!("Setup cancelled"));
                        }
                        _ => {
                            let event = crossterm::event::Event::Key(key);
                            if focused_field < fields.len() {
                                fields[focused_field].input.handle_event(&event);
                            }
                        }
                    }
                }
            }
        }

        Ok(())
    }
}

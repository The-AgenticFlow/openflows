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

struct ApiField {
    label: String,
    env_key: String,
    input: Input,
    required: bool,
}

#[derive(Default)]
pub struct ApiStep;

impl ApiStep {
    pub fn new() -> Self {
        Self
    }

    pub async fn render(
        &self,
        terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
        theme: &Theme,
        config: &mut SetupConfig,
    ) -> Result<()> {
        let provider_name = config.selected_provider.clone().unwrap_or_default();

        let mut fields: Vec<ApiField> = Vec::new();

        let provider_field = match provider_name.as_str() {
            "Anthropic (Claude)" => Some(ApiField {
                label: "Anthropic API Key".to_string(),
                env_key: "ANTHROPIC_API_KEY".to_string(),
                input: Input::new(config.anthropic_key.clone()),
                required: true,
            }),
            "OpenAI" => Some(ApiField {
                label: "OpenAI API Key".to_string(),
                env_key: "OPENAI_API_KEY".to_string(),
                input: Input::new(config.openai_key.clone().unwrap_or_default()),
                required: true,
            }),
            "Google Gemini" => Some(ApiField {
                label: "Google Gemini API Key".to_string(),
                env_key: "GEMINI_API_KEY".to_string(),
                input: Input::new(config.gemini_key.clone().unwrap_or_default()),
                required: true,
            }),
            "Fireworks AI" => Some(ApiField {
                label: "Fireworks API Key".to_string(),
                env_key: "FIREWORKS_API_KEY".to_string(),
                input: Input::new(config.fireworks_key.clone().unwrap_or_default()),
                required: true,
            }),
            "LiteLLM Proxy" => {
                let proxy_fields = vec![
                    ApiField {
                        label: "LiteLLM Proxy URL".to_string(),
                        env_key: "LITELLM_URL".to_string(),
                        input: Input::new(std::env::var("LITELLM_URL").unwrap_or_default()),
                        required: true,
                    },
                    ApiField {
                        label: "LiteLLM API Key (optional)".to_string(),
                        env_key: "LITELLM_API_KEY".to_string(),
                        input: Input::new(std::env::var("LITELLM_API_KEY").unwrap_or_default()),
                        required: false,
                    },
                ];
                return self
                    .render_fields(terminal, theme, config, proxy_fields, &provider_name)
                    .await;
            }
            "Ollama (Local)" => Some(ApiField {
                label: "Ollama Host URL".to_string(),
                env_key: "OLLAMA_HOST".to_string(),
                input: Input::new(
                    std::env::var("OLLAMA_HOST")
                        .unwrap_or_else(|_| "http://localhost:11434".to_string()),
                ),
                required: true,
            }),
            "Skip for now" => return Ok(()),
            _ => return Ok(()),
        };

        if let Some(pf) = provider_field {
            fields.push(pf);
        }

        if fields.is_empty() {
            return Ok(());
        }

        self.render_fields(terminal, theme, config, fields, &provider_name)
            .await
    }

    async fn render_fields(
        &self,
        terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
        theme: &Theme,
        config: &mut SetupConfig,
        mut fields: Vec<ApiField>,
        provider_name: &str,
    ) -> Result<()> {
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
                    "◇ LLM PROVIDER",
                    Style::default()
                        .fg(theme.accent_alt())
                        .add_modifier(Modifier::BOLD),
                );
                let subtitle = Line::styled(
                    format!("  Configure {} API credentials", provider_name),
                    Style::default().fg(theme.muted()),
                );
                let title_para = ratatui::widgets::Paragraph::new(vec![title, subtitle]);
                title_para.render(inner_title, f.buffer_mut());

                let input_area =
                    Rect::new(chunks[1].x, chunks[1].y, chunks[1].width, chunks[1].height);

                let mut current_y = input_area.y;

                let field_height = 3u16;

                for (i, field) in fields.iter().enumerate() {
                    if current_y + field_height > input_area.y + input_area.height {
                        break;
                    }

                    let widget = InputWidget::new(&field.input, &field.label)
                        .masked(true)
                        .focused(focused_field == i)
                        .optional(!field.required);
                    widget.render(
                        Rect::new(input_area.x, current_y, input_area.width, field_height),
                        f.buffer_mut(),
                    );
                    current_y += field_height + 1;
                }

                let help = "  Tab/Arrows: navigate  │  Enter: continue  │  Esc: cancel";
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
                        KeyCode::Enter => {
                            let all_required_filled = fields
                                .iter()
                                .filter(|f| f.required)
                                .all(|f| !f.input.value().is_empty());

                            if all_required_filled {
                                for field in &fields {
                                    let value = field.input.value().to_string();
                                    match field.env_key.as_str() {
                                        "ANTHROPIC_API_KEY" => config.anthropic_key = value,
                                        "OPENAI_API_KEY" => {
                                            config.openai_key =
                                                if value.is_empty() { None } else { Some(value) };
                                        }
                                        "GEMINI_API_KEY" => {
                                            config.gemini_key =
                                                if value.is_empty() { None } else { Some(value) };
                                        }
                                        "FIREWORKS_API_KEY" => {
                                            config.fireworks_key =
                                                if value.is_empty() { None } else { Some(value) };
                                        }
                                        "LITELLM_URL" => {
                                            config.proxy_url =
                                                if value.is_empty() { None } else { Some(value) };
                                        }
                                        "LITELLM_API_KEY" => {
                                            config.proxy_api_key =
                                                if value.is_empty() { None } else { Some(value) };
                                        }
                                        "OLLAMA_HOST" => {
                                            config.gateway_url =
                                                if value.is_empty() { None } else { Some(value) };
                                        }
                                        _ => {}
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

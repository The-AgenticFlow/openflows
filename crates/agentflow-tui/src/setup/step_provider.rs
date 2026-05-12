use anyhow::Result;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::prelude::*;
use ratatui::Terminal;
use std::io;

use crate::setup::SetupConfig;
use crate::util::theme::Theme;
use crate::widgets::select::SelectableListState;

#[derive(Debug, Clone, PartialEq)]
pub struct Provider {
    pub name: String,
    pub env_key: String,
    pub requires_key: bool,
}

fn get_providers() -> Vec<Provider> {
    vec![
        Provider { name: "Anthropic (Claude)".into(), env_key: "ANTHROPIC_API_KEY".into(), requires_key: true },
        Provider { name: "OpenAI".into(), env_key: "OPENAI_API_KEY".into(), requires_key: true },
        Provider { name: "Google Gemini".into(), env_key: "GEMINI_API_KEY".into(), requires_key: true },
        Provider { name: "Fireworks AI".into(), env_key: "FIREWORKS_API_KEY".into(), requires_key: true },
        Provider { name: "LiteLLM Proxy".into(), env_key: "LITELLM_URL".into(), requires_key: false },
        Provider { name: "Ollama (Local)".into(), env_key: "OLLAMA_HOST".into(), requires_key: false },
        Provider { name: "Skip for now".into(), env_key: String::new(), requires_key: false },
    ]
}

pub struct ProviderStep {
    selected_provider: Option<Provider>,
}

impl ProviderStep {
    pub fn new() -> Self {
        Self {
            selected_provider: None,
        }
    }

    pub fn selected_provider(&self) -> Option<&Provider> {
        self.selected_provider.as_ref()
    }

    pub async fn render(
        &mut self,
        terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
        theme: &Theme,
        config: &mut SetupConfig,
    ) -> Result<()> {
        let providers = get_providers();
        let provider_names: Vec<String> = providers.iter().map(|p| p.name.clone()).collect();
        let mut list_state = SelectableListState::new(provider_names).with_search();

        loop {
            terminal.draw(|f| {
                let area = f.area();

                let chunks = Layout::default()
                    .direction(Direction::Vertical)
                    .margin(3)
                    .constraints([
                        Constraint::Length(4),
                        Constraint::Min(8),
                    ])
                    .split(area);

                let title_block = ratatui::widgets::Block::default()
                    .borders(ratatui::widgets::Borders::BOTTOM)
                    .border_style(Style::default().fg(theme.border()));

                let inner_title = title_block.inner(chunks[0]);
                title_block.render(chunks[0], f.buffer_mut());

                let title = Line::styled(
                    "◇ SELECT LLM PROVIDER",
                    Style::default()
                        .fg(theme.accent_alt())
                        .add_modifier(Modifier::BOLD),
                );
                let subtitle = Line::styled(
                    "  Choose your AI backend",
                    Style::default().fg(theme.muted()),
                );
                let title_para = ratatui::widgets::Paragraph::new(vec![title, subtitle]);
                title_para.render(inner_title, f.buffer_mut());

                let list_widget = crate::widgets::select::SelectableList::new(
                    &list_state.items,
                    list_state.selected,
                )
                .search_query(&list_state.search_input);

                list_widget.render(chunks[1], f.buffer_mut());
            })?;

            if crossterm::event::poll(std::time::Duration::from_millis(100))? {
                if let crossterm::event::Event::Key(key) = crossterm::event::read()? {
                    use crossterm::event::KeyCode;
                    match key.code {
                        KeyCode::Up => {
                            list_state.move_up();
                        }
                        KeyCode::Down => {
                            list_state.move_down();
                        }
                        KeyCode::Enter => {
                            if let Some(idx) = list_state.visible_items().iter().find(|(i, _)| *i == list_state.selected).map(|(i, _)| *i) {
                                self.selected_provider = Some(providers[idx].clone());
                                let provider = &providers[idx];

                                // Store selected provider name for ApiStep
                                config.selected_provider = Some(provider.name.clone());

                                match provider.name.as_str() {
                                    "Anthropic (Claude)" => {
                                        if config.anthropic_key.is_empty() {
                                            config.anthropic_key = std::env::var("ANTHROPIC_API_KEY").unwrap_or_default();
                                        }
                                    }
                                    "Google Gemini" => {
                                        if config.gemini_key.is_none() {
                                            config.gemini_key = Some(std::env::var("GEMINI_API_KEY").unwrap_or_default());
                                        }
                                    }
                                    "OpenAI" => {
                                        if config.openai_key.is_none() {
                                            config.openai_key = Some(std::env::var("OPENAI_API_KEY").unwrap_or_default());
                                        }
                                    }
                                    "Fireworks AI" => {
                                        if config.fireworks_key.is_none() {
                                            config.fireworks_key = Some(std::env::var("FIREWORKS_API_KEY").unwrap_or_default());
                                        }
                                    }
                                    _ => {}
                                }
                            }
                            break;
                        }
                        KeyCode::Esc => {
                            if !list_state.search_input.value().is_empty() {
                                list_state.search_input = tui_input::Input::default();
                            } else {
                                return Err(anyhow::anyhow!("Setup cancelled"));
                            }
                        }
                        _ => {
                            list_state.handle_key(key);
                        }
                    }
                }
            }
        }

        Ok(())
    }
}

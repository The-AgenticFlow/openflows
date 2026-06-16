use anyhow::Result;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::prelude::*;
use ratatui::Terminal;
use std::io;

use crate::setup::SetupConfig;
use crate::util::theme::Theme;
use crate::widgets::select::SelectableListState;

const ALL_CLI_BACKENDS: &[(&str, &str, &str)] = &[
    ("codex", "Codex CLI", "OpenAI's CLI agent"),
];

fn get_cli_backends(config: &SetupConfig) -> Vec<(&'static str, &'static str, &'static str)> {
    match config.selected_provider.as_deref() {
        Some(p) if p.contains("Codex") || p.contains("OpenAI") || p.contains("Fireworks") => {
            // Codex works with OpenAI directly and Fireworks (OpenAI-compatible)
            vec![("codex", "Codex CLI", "OpenAI-compatible CLI agent")]
        }
        _ => ALL_CLI_BACKENDS.to_vec(),
    }
}

#[derive(Default)]
pub struct CliBackendStep {
    selected: usize,
}

impl CliBackendStep {
    pub fn new() -> Self {
        Self { selected: 0 }
    }

    pub async fn render(
        &mut self,
        terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
        theme: &Theme,
        config: &mut SetupConfig,
    ) -> Result<()> {
        let backends = get_cli_backends(config);

        // If only one backend is compatible with the selected provider, auto-select it
        if backends.len() == 1 {
            config.selected_cli_backend = backends[0].0.to_string();
            return Ok(());
        }

        let items: Vec<String> = backends
            .iter()
            .map(|(_, name, _)| name.to_string())
            .collect();
        let mut list_state = SelectableListState::new(items);
        list_state.selected = self.selected;

        loop {
            terminal.draw(|f| {
                let area = f.area();

                let chunks = Layout::default()
                    .direction(Direction::Vertical)
                    .margin(3)
                    .constraints([
                        Constraint::Length(4),
                        Constraint::Min(4),
                        Constraint::Length(3),
                    ])
                    .split(area);

                let title_block = ratatui::widgets::Block::default()
                    .borders(ratatui::widgets::Borders::BOTTOM)
                    .border_style(Style::default().fg(theme.border()));

                let inner_title = title_block.inner(chunks[0]);
                title_block.render(chunks[0], f.buffer_mut());

                let title = Line::styled(
                    "◇ SELECT CLI BACKEND",
                    Style::default()
                        .fg(theme.accent_alt())
                        .add_modifier(Modifier::BOLD),
                );
                let subtitle = Line::styled(
                    "  Choose the CLI agent for code execution",
                    Style::default().fg(theme.muted()),
                );
                let title_para = ratatui::widgets::Paragraph::new(vec![title, subtitle]);
                title_para.render(inner_title, f.buffer_mut());

                let list_widget =
                    crate::widgets::select::SelectableList::new(&list_state.items, list_state.selected);
                list_widget.render(chunks[1], f.buffer_mut());

                let description = backends[list_state.selected].2;
                let help = Line::styled(
                    format!("  {}  |  ↑↓ navigate  |  Enter: select  |  Esc: cancel", description),
                    Style::default().fg(theme.muted()),
                );
                let help_para = ratatui::widgets::Paragraph::new(help);
                help_para.render(chunks[2], f.buffer_mut());
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
                            self.selected = list_state.selected;
                            config.selected_cli_backend = backends[self.selected].0.to_string();
                            return Ok(());
                        }
                        KeyCode::Esc => {
                            return Err(anyhow::anyhow!("Setup cancelled"));
                        }
                        _ => {}
                    }
                }
            }
        }
    }
}

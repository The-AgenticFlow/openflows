// crates/agentflow-tui/src/setup/step_module.rs
//! Agent Module Selection step in the setup wizard.
//!
//! Shown after Agent Config step, only when workspace_provider == Coder.
//! Lists each agent with its current CLI and resolved Coder Registry module.
//! Allows switching CLI (claude → codex → aider) which auto-resolves the module source.

use anyhow::Result;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::prelude::*;
use ratatui::widgets::Paragraph;
use ratatui::Terminal;
use std::io;

use super::resolve_coder_module_for_cli;
use super::{SetupConfig, WorkspaceProvider};
use crate::util::theme::Theme;

pub struct ModuleStep {
    selected: usize,
    cli_selections: Vec<String>,
}

impl ModuleStep {
    pub fn new() -> Self {
        Self {
            selected: 0,
            cli_selections: Vec::new(),
        }
    }

    pub async fn render(
        &mut self,
        terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
        theme: &Theme,
        config: &mut SetupConfig,
    ) -> Result<()> {
        // Only shown in Coder mode
        if config.workspace_provider != WorkspaceProvider::Coder {
            return Ok(());
        }

        // Initialize CLI selections from agent config, defaulting to "claude"
        if self.cli_selections.is_empty() {
            self.cli_selections = config
                .agents
                .iter()
                .map(|a| a.cli.to_string())
                .collect();
        }

        let available_clis = ["claude", "codex", "aider", "goose"];

        loop {
            terminal.draw(|f| {
                let area = f.area();
                let chunks = Layout::default()
                    .direction(Direction::Vertical)
                    .margin(2)
                    .constraints([
                        Constraint::Length(5),
                        Constraint::Min(3),
                        Constraint::Length(3),
                    ])
                    .split(area);

                let title_block = ratatui::widgets::Block::default()
                    .borders(ratatui::widgets::Borders::BOTTOM)
                    .border_style(Style::default().fg(theme.border()));
                let inner_title = title_block.inner(chunks[0]);
                title_block.render(chunks[0], f.buffer_mut());

                let title = Line::styled(
                    "◇ AGENT MODULE SELECTION",
                    Style::default()
                        .fg(theme.accent())
                        .add_modifier(Modifier::BOLD),
                );
                let subtitle = Line::styled(
                    "  Select CLI backend per agent — auto-resolves Coder Registry module",
                    Style::default().fg(theme.muted()),
                );
                let title_para =
                    ratatui::widgets::Paragraph::new(vec![title, subtitle]);
                title_para.render(inner_title, f.buffer_mut());

                // Rows
                let mut current_y = chunks[1].y;
                let row_height = 2u16;

                for (i, agent) in config.agents.iter().enumerate() {
                    if current_y + row_height > chunks[1].y + chunks[1].height {
                        break;
                    }

                    let is_selected = i == self.selected;
                    let row_style = if is_selected {
                        Style::default()
                            .fg(theme.accent())
                            .add_modifier(Modifier::BOLD)
                    } else {
                        Style::default().fg(theme.fg())
                    };

                    let prefix = if is_selected { "▶ " } else { "  " };

                    // Resolve module from CLI
                    let cli = self
                        .cli_selections
                        .get(i)
                        .map(|s| s.as_str())
                        .unwrap_or("claude");
                    let module = resolve_coder_module_for_cli(cli, &agent.id);
                    let perm_mode = config::registry::default_permission_mode_for_role(&agent.id);

                    let line1 = format!(
                        "{}{:<12} CLI: {:<8} Module: {}",
                        prefix,
                        agent.id,
                        cli,
                        module.source
                    );
                    let line2 = format!(
                        "               v{}  Permission: {}  AI Gateway: {}",
                        module.version,
                        perm_mode,
                        if config.enable_ai_gateway { "✓" } else { "✗" }
                    );

                    let para = ratatui::widgets::Paragraph::new(vec![
                        Line::styled(line1, row_style),
                        Line::styled(line2, row_style.clone().fg(theme.muted())),
                    ]);
                    para.render(
                        ratatui::layout::Rect::new(chunks[1].x, current_y, chunks[1].width, row_height),
                        f.buffer_mut(),
                    );
                    current_y += row_height;
                }

                let help_lines = vec![
                    Line::styled(
                        "  ↑↓ select agent  │  ←→: cycle CLI  │  a: toggle AI Gateway  │  Tab: finish",
                        Style::default().fg(theme.muted()),
                    ),
                    Line::styled(
                        "  Module auto-resolves from CLI. Permission set per role.",
                        Style::default().fg(theme.muted()),
                    ),
                ];
                let help_para = Paragraph::new(help_lines);
                help_para.render(chunks[2], f.buffer_mut());
            })?;

            if crossterm::event::poll(std::time::Duration::from_millis(100))? {
                if let crossterm::event::Event::Key(key) = crossterm::event::read()? {
                    use crossterm::event::KeyCode;
                    match key.code {
                        KeyCode::Up if self.selected > 0 => {
                            self.selected -= 1;
                        }
                        KeyCode::Down if self.selected + 1 < config.agents.len() => {
                            self.selected += 1;
                        }
                        KeyCode::Left if !config.agents.is_empty() => {
                            let idx = self.selected;
                            let current = self
                                .cli_selections
                                .get_mut(idx)
                                .expect("cli_selections out of sync");
                            let pos = available_clis
                                .iter()
                                .position(|c| c == &current.as_str())
                                .unwrap_or(0);
                            let new_pos = if pos > 0 { pos - 1 } else { available_clis.len() - 1 };
                            *current = available_clis[new_pos].to_string();
                        }
                        KeyCode::Right if !config.agents.is_empty() => {
                            let idx = self.selected;
                            let current = self
                                .cli_selections
                                .get_mut(idx)
                                .expect("cli_selections out of sync");
                            let pos = available_clis
                                .iter()
                                .position(|c| c == &current.as_str())
                                .unwrap_or(0);
                            let new_pos = (pos + 1) % available_clis.len();
                            *current = available_clis[new_pos].to_string();
                        }
                        KeyCode::Char('a') => {
                            config.enable_ai_gateway = !config.enable_ai_gateway;
                        }
                        KeyCode::Tab | KeyCode::BackTab => {
                            // Update agent CLIs from selections
                            for (i, agent) in config.agents.iter_mut().enumerate() {
                                if let Some(cli) = self.cli_selections.get(i) {
                                    agent.cli = cli.clone();
                                }
                            }
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

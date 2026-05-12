use anyhow::Result;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::prelude::*;
use ratatui::widgets::Paragraph;
use ratatui::Terminal;
use std::io;

use crate::util::env_check;
use crate::util::theme::Theme;
use crate::widgets::check::{CheckList, CheckState};

pub struct EnvStep {
    checks: Vec<(String, CheckState)>,
}

impl EnvStep {
    pub fn new() -> Self {
        let mut checks = Vec::new();

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
            checks.push(("Node.js not found (for GitHub MCP)".to_string(), CheckState::Warn));
        }

        if let Some(version) = env_check::check_claude() {
            checks.push((format!("Claude Code CLI {}", version), CheckState::Pass));
        } else {
            checks.push(("Claude Code CLI not found (required)".to_string(), CheckState::Fail));
        }

        Self { checks }
    }

    pub fn render(
        &self,
        terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
        theme: &Theme,
    ) -> Result<()> {
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
                        Constraint::Length(2),
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

                let sep_line = Line::styled(
                    "│",
                    Style::default().fg(theme.border()),
                );
                let sep_para = Paragraph::new(sep_line);
                sep_para.render(chunks[1], f.buffer_mut());

                let prompt_line = Line::styled(
                    "◆  Environment Check",
                    Style::default().fg(theme.accent()).add_modifier(Modifier::BOLD),
                );
                let prompt_para = Paragraph::new(prompt_line);
                prompt_para.render(chunks[2], f.buffer_mut());

                let check_list = CheckList::new(self.checks.clone());
                check_list.render(
                    Rect::new(chunks[2].x, chunks[2].y + 2, chunks[2].width, chunks[2].height - 2),
                    f.buffer_mut(),
                );

                let has_failures = self
                    .checks
                    .iter()
                    .any(|(_, state)| state == &CheckState::Fail);

                let help_text = if has_failures {
                    "Some required tools are missing. Press Enter to continue anyway, or Esc to cancel."
                } else {
                    "Press Enter to continue..."
                };

                let help_line = Line::styled(
                    help_text,
                    Style::default().fg(theme.muted()),
                );
                let help_para = Paragraph::new(help_line).alignment(Alignment::Center);
                help_para.render(chunks[3], f.buffer_mut());
            })?;

            if crossterm::event::poll(std::time::Duration::from_millis(100))? {
                if let crossterm::event::Event::Key(key) = crossterm::event::read()? {
                    if key.code == crossterm::event::KeyCode::Enter {
                        break;
                    }
                    if key.code == crossterm::event::KeyCode::Esc {
                        return Err(anyhow::anyhow!("Setup cancelled"));
                    }
                }
            }
        }

        Ok(())
    }
}

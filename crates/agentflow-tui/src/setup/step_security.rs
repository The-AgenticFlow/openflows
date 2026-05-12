use anyhow::Result;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::prelude::*;
use ratatui::widgets::Paragraph;
use ratatui::Terminal;
use std::io;

use crate::util::theme::Theme;
use crate::widgets::confirm::{ConfirmDialog, ConfirmDialogState, ConfirmResult};
use crate::widgets::infobox::InfoBox;

const DISCLAIMER_CONTENT: &[&str] = &[
    "OpenFlow is an autonomous development orchestration system",
    "",
    "What this means:",
    "- AI agents will read your codebase and execute git operations",
    "- Agents use Claude Code CLI to implement changes automatically",
    "- PRs are created, reviewed, and merged without manual intervention",
    "",
    "Security features built-in:",
    "- Secret pattern detection (API keys, tokens are auto-redacted)",
    "- Dangerous commands require approval gate",
    "- Whole-worktree scanning before any push",
    "",
    "Requirements:",
    "- Claude Code CLI must be installed and configured",
    "- GitHub Personal Access Token with repo scope",
    "- At least one LLM API key (Anthropic, OpenAI, Gemini, or Fireworks)",
    "",
    "Best practices:",
    "- Use a dedicated GitHub account for the bot",
    "- Start with a test repository to understand behavior",
    "- Review agent registry before running on production code",
];

pub struct SecurityStep {
    confirmed: bool,
}

impl SecurityStep {
    pub fn new() -> Self {
        Self { confirmed: false }
    }

    pub fn is_confirmed(&self) -> bool {
        self.confirmed
    }

    pub fn render(
        &mut self,
        terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
        theme: &Theme,
    ) -> Result<()> {
        let mut confirm_state = ConfirmDialogState::new();

        loop {
            terminal.draw(|f| {
                let area = f.area();

                let chunks = Layout::default()
                    .direction(Direction::Vertical)
                    .margin(2)
                    .constraints([
                        Constraint::Length(2),
                        Constraint::Min(10),
                        Constraint::Length(4),
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

                let box_content: Vec<String> = DISCLAIMER_CONTENT.iter().map(|s| s.to_string()).collect();
                let info_box = InfoBox::new("Getting Started", &box_content);
                info_box.render(chunks[1], f.buffer_mut());

                let confirm_prompt = "  Ready to set up OpenFlow?";
                let confirm_lines = vec![
                    Line::styled(confirm_prompt.to_string(), theme.text_style()),
                ];
                let confirm_para = Paragraph::new(confirm_lines);
                confirm_para.render(Rect::new(chunks[2].x, chunks[2].y, chunks[2].width, 1), f.buffer_mut());

                let dialog = ConfirmDialog::new("")
                    .selected_yes(confirm_state.selected_yes);
                let dialog_area = Rect::new(chunks[2].x, chunks[2].y + 2, chunks[2].width, 2);
                dialog.render(dialog_area, f.buffer_mut());
            })?;

            if crossterm::event::poll(std::time::Duration::from_millis(100))? {
                if let crossterm::event::Event::Key(key) = crossterm::event::read()? {
                    match confirm_state.handle_key(key) {
                        ConfirmResult::Yes => {
                            self.confirmed = true;
                            break;
                        }
                        ConfirmResult::No | ConfirmResult::Cancel => {
                            return Err(anyhow::anyhow!("Setup cancelled by user"));
                        }
                        ConfirmResult::Continue => {}
                    }
                }
            }
        }

        Ok(())
    }
}

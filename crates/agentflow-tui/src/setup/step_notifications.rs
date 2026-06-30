// crates/agentflow-tui/src/setup/step_notifications.rs
//! Notification Configuration step in the setup wizard.
//!
//! Shown after the module step when workspace_provider == Coder.
//! Configures Slack and Discord webhook URLs for awaiting_human escalation alerts.
//!
//! Note: The slackme module handles command-completion DMs via Coder external auth.
//! These webhooks handle event-oriented escalation alerts (e.g. when agents are stuck).

use anyhow::Result;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::prelude::*;
use ratatui::widgets::Paragraph;
use ratatui::Terminal;
use std::io;

use crate::setup::{SetupConfig, WorkspaceProvider};
use crate::util::theme::Theme;

pub struct NotificationsStep {
    focused_field: usize, // 0 = Slack, 1 = Discord, 2 = Done
    slack_url: String,
    discord_url: String,
    cursor_pos: usize,
}

impl NotificationsStep {
    pub fn new() -> Self {
        Self {
            focused_field: 0,
            slack_url: String::new(),
            discord_url: String::new(),
            cursor_pos: 0,
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

        // Pre-fill from existing config
        if self.slack_url.is_empty() {
            if let Some(ref url) = config.slack_webhook_url {
                self.slack_url = url.clone();
            }
        }
        if self.discord_url.is_empty() {
            if let Some(ref url) = config.discord_webhook_url {
                self.discord_url = url.clone();
            }
        }

        let mut input_mode = false;

        loop {
            terminal.draw(|f| {
                let area = f.area();
                let chunks = Layout::default()
                    .direction(Direction::Vertical)
                    .margin(2)
                    .constraints([
                        Constraint::Length(4),
                        Constraint::Length(1),
                        Constraint::Length(1),
                        Constraint::Length(3),
                        Constraint::Length(1),
                        Constraint::Length(1),
                        Constraint::Length(3),
                        Constraint::Min(1),
                    ])
                    .split(area);

                let title_block = ratatui::widgets::Block::default()
                    .borders(ratatui::widgets::Borders::BOTTOM)
                    .border_style(Style::default().fg(theme.border()));
                let inner_title = title_block.inner(chunks[0]);
                title_block.render(chunks[0], f.buffer_mut());

                let title = Line::styled(
                    "◇ NOTIFICATION CONFIGURATION",
                    Style::default()
                        .fg(theme.accent())
                        .add_modifier(Modifier::BOLD),
                );
                let subtitle = Line::styled(
                    "  Webhook URLs for awaiting_human escalation alerts",
                    Style::default().fg(theme.muted()),
                );
                let title_para =
                    ratatui::widgets::Paragraph::new(vec![title, subtitle]);
                title_para.render(inner_title, f.buffer_mut());

                let info = Line::styled(
                    "  slackme module: command-completion DMs (via Coder external auth)",
                    Style::default().fg(theme.muted()),
                );
                let info2 = Line::styled(
                    "  Webhooks below: awaiting_human escalation alerts (channel-based)",
                    Style::default().fg(theme.muted()),
                );
                let info_para = Paragraph::new(vec![info, info2]);
                info_para.render(chunks[1], f.buffer_mut());

                // Slack webhook URL field
                let slack_label = if self.focused_field == 0 {
                    Style::default()
                        .fg(theme.accent())
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(theme.fg())
                };
                let slack_prefix = if self.focused_field == 0 { "  ▶ " } else { "    " };
                let display_slack = if input_mode && self.focused_field == 0 {
                    format!("{}{}", &self.slack_url[..self.cursor_pos], "█")
                } else {
                    self.slack_url.clone()
                };
                let slack_line = Line::styled(
                    format!("{}Slack Webhook URL: {}", slack_prefix, display_slack),
                    slack_label,
                );
                let slack_para = Paragraph::new(slack_line);
                slack_para.render(chunks[2], f.buffer_mut());

                // Discord webhook URL field
                let discord_label = if self.focused_field == 1 {
                    Style::default()
                        .fg(theme.accent())
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(theme.fg())
                };
                let discord_prefix = if self.focused_field == 1 { "  ▶ " } else { "    " };
                let display_discord = if input_mode && self.focused_field == 1 {
                    format!("{}{}", &self.discord_url[..self.cursor_pos], "█")
                } else {
                    self.discord_url.clone()
                };
                let discord_line = Line::styled(
                    format!("{}Discord Webhook URL: {}", discord_prefix, display_discord),
                    discord_label,
                );
                let discord_para = Paragraph::new(discord_line);
                discord_para.render(chunks[5], f.buffer_mut());

                // Both are optional
                let opt_line = Line::styled(
                    "  (Both optional — leave empty to disable that channel)",
                    Style::default().fg(theme.muted()),
                );
                let opt_para = Paragraph::new(opt_line);
                opt_para.render(chunks[3], f.buffer_mut());

                let opt_line2 = Line::styled(
                    "  (Both optional — leave empty to disable that channel)",
                    Style::default().fg(theme.muted()),
                );
                let opt_para2 = Paragraph::new(opt_line2);
                opt_para2.render(chunks[6], f.buffer_mut());

                // Done option
                let done_label = if self.focused_field == 2 {
                    Style::default()
                        .fg(theme.accent())
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(theme.fg())
                };
                let done_prefix = if self.focused_field == 2 { "  ▶ " } else { "    " };
                let done_line = Line::styled(
                    format!("{}Done — Save and Continue", done_prefix),
                    done_label,
                );
                let done_para = Paragraph::new(done_line);
                done_para.render(chunks[4], f.buffer_mut());

                let help_lines = vec![
                    Line::styled(
                        "  ↑↓ navigate  │  Enter: edit field / save  │  Esc: cancel",
                        Style::default().fg(theme.muted()),
                    ),
                ];
                let help_para = Paragraph::new(help_lines);
                help_para.render(chunks[7], f.buffer_mut());
            })?;

            if input_mode {
                if crossterm::event::poll(std::time::Duration::from_millis(100))? {
                    if let crossterm::event::Event::Key(key) = crossterm::event::read()? {
                        use crossterm::event::KeyCode;
                        match key.code {
                            KeyCode::Enter | KeyCode::Esc | KeyCode::Tab => {
                                input_mode = false;
                            }
                            KeyCode::Backspace => {
                                if self.cursor_pos > 0 {
                                    let target = if self.focused_field == 0 {
                                        &mut self.slack_url
                                    } else {
                                        &mut self.discord_url
                                    };
                                    target.remove(self.cursor_pos - 1);
                                    self.cursor_pos -= 1;
                                }
                            }
                            KeyCode::Char(c) => {
                                let target = if self.focused_field == 0 {
                                    &mut self.slack_url
                                } else {
                                    &mut self.discord_url
                                };
                                target.insert(self.cursor_pos, c);
                                self.cursor_pos += 1;
                            }
                            _ => {}
                        }
                    }
                }
            } else {
                if crossterm::event::poll(std::time::Duration::from_millis(100))? {
                    if let crossterm::event::Event::Key(key) = crossterm::event::read()? {
                        use crossterm::event::KeyCode;
                        match key.code {
                            KeyCode::Up if self.focused_field > 0 => {
                                self.focused_field -= 1;
                            }
                            KeyCode::Down if self.focused_field < 2 => {
                                self.focused_field += 1;
                            }
                            KeyCode::Enter => {
                                if self.focused_field == 2 {
                                    // Save
                                    config.slack_webhook_url = if self.slack_url.is_empty() {
                                        None
                                    } else {
                                        Some(self.slack_url.clone())
                                    };
                                    config.discord_webhook_url = if self.discord_url.is_empty() {
                                        None
                                    } else {
                                        Some(self.discord_url.clone())
                                    };
                                    return Ok(());
                                } else {
                                    input_mode = true;
                                    self.cursor_pos = if self.focused_field == 0 {
                                        self.slack_url.len()
                                    } else {
                                        self.discord_url.len()
                                    };
                                }
                            }
                            KeyCode::Tab => {
                                self.focused_field = (self.focused_field + 1) % 3;
                            }
                            KeyCode::BackTab => {
                                if self.focused_field == 0 {
                                    self.focused_field = 2;
                                } else {
                                    self.focused_field -= 1;
                                }
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
}

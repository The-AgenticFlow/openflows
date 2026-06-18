use anyhow::Result;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::prelude::*;
use ratatui::widgets::Paragraph;
use ratatui::Terminal;
use std::io;
use tui_input::backend::crossterm::EventHandler;
use tui_input::Input;

use crate::setup::{DomainMode, SetupConfig};
use crate::util::theme::Theme;
use crate::widgets::input::InputWidget;
use crate::widgets::select::SelectableListState;

enum DomainsState {
    ModeSelect {
        list_state: SelectableListState,
    },
    ManualInput {
        domains: Vec<String>,
        current_input: Input,
        scroll_offset: usize,
    },
}

fn default_domains() -> Vec<String> {
    vec![
        "api.github.com".to_string(),
        "*.github.com".to_string(),
    ]
}

pub struct DomainsStep;

impl DomainsStep {
    pub fn new() -> Self {
        Self
    }

    pub async fn render(
        &self,
        terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
        _theme: &Theme,
        config: &mut SetupConfig,
    ) -> Result<()> {
        let theme = Theme::default();
        let mode_options = vec![
            "Manual — specify allowed domains".to_string(),
            "All — allow unrestricted internet access".to_string(),
        ];

        let mut state = DomainsState::ModeSelect {
            list_state: SelectableListState::new(mode_options),
        };

        loop {
            match &mut state {
                DomainsState::ModeSelect { list_state } => {
                    loop {
                        terminal.draw(|f| {
                            let area = f.area();
                            let chunks = Layout::default()
                                .direction(Direction::Vertical)
                                .margin(3)
                                .constraints([
                                    Constraint::Length(6),
                                    Constraint::Min(6),
                                    Constraint::Length(2),
                                ])
                                .split(area);

                            let title_block = ratatui::widgets::Block::default()
                                .borders(ratatui::widgets::Borders::BOTTOM)
                                .border_style(Style::default().fg(theme.border()));
                            let inner_title = title_block.inner(chunks[0]);
                            title_block.render(chunks[0], f.buffer_mut());

                            let title = Line::styled(
                                "◇ DOMAIN CONFIGURATION",
                                Style::default()
                                    .fg(theme.accent())
                                    .add_modifier(Modifier::BOLD),
                            );
                            let subtitle = Line::styled(
                                "  Restrict which network domains agents can reach during execution",
                                Style::default().fg(theme.muted()),
                            );
                            let desc1 = Line::styled(
                                "  Domains control outbound network access for sandboxed agents.",
                                Style::default().fg(theme.muted()),
                            );
                            let desc2 = Line::styled(
                                "  Choose 'Manual' to whitelist specific hosts, or 'All' to let agents",
                                Style::default().fg(theme.muted()),
                            );
                            let desc3 = Line::styled(
                                "  access any internet resource (package registries, APIs, etc.).",
                                Style::default().fg(theme.muted()),
                            );
                            let title_para = ratatui::widgets::Paragraph::new(vec![
                                title, subtitle, desc1, desc2, desc3,
                            ]);
                            title_para.render(inner_title, f.buffer_mut());

                            let list_widget = crate::widgets::select::SelectableList::new(
                                &list_state.items,
                                list_state.selected,
                            );
                            list_widget.render(chunks[1], f.buffer_mut());

                            let help = Line::styled(
                                "  ↑↓ navigate  │  Enter: select  │  Esc: cancel",
                                Style::default().fg(theme.muted()),
                            );
                            let help_para = Paragraph::new(help);
                            help_para.render(chunks[2], f.buffer_mut());
                        })?;

                        if crossterm::event::poll(std::time::Duration::from_millis(100))? {
                            if let crossterm::event::Event::Key(key) =
                                crossterm::event::read()?
                            {
                                use crossterm::event::KeyCode;
                                match key.code {
                                    KeyCode::Up => list_state.move_up(),
                                    KeyCode::Down => list_state.move_down(),
                                    KeyCode::Enter => {
                                        match list_state.selected {
                                            0 => {
                                                let initial_domains = if config
                                                    .allowed_domains
                                                    .is_empty()
                                                {
                                                    default_domains()
                                                } else {
                                                    config.allowed_domains.clone()
                                                };
                                                state = DomainsState::ManualInput {
                                                    domains: initial_domains,
                                                    current_input: Input::default(),
                                                    scroll_offset: 0,
                                                };
                                            }
                                            1 => {
                                                config.domain_mode = DomainMode::All;
                                                config.allowed_domains =
                                                    vec!["*".to_string()];
                                                return Ok(());
                                            }
                                            _ => {}
                                        }
                                        break;
                                    }
                                    KeyCode::Esc => {
                                        return Err(anyhow::anyhow!("Setup cancelled"));
                                    }
                                    _ => {
                                        list_state.handle_key(key);
                                    }
                                }
                            }
                        }
                    }
                }
                DomainsState::ManualInput {
                    domains,
                    current_input,
                    scroll_offset,
                } => {
                    loop {
                        terminal.draw(|f| {
                            let area = f.area();
                            let chunks = Layout::default()
                                .direction(Direction::Vertical)
                                .margin(2)
                                .constraints([
                                    Constraint::Length(5),
                                    Constraint::Length(3),
                                    Constraint::Min(3),
                                    Constraint::Length(2),
                                ])
                                .split(area);

                            // Title + description
                            let title_block = ratatui::widgets::Block::default()
                                .borders(ratatui::widgets::Borders::BOTTOM)
                                .border_style(Style::default().fg(theme.border()));
                            let inner_title = title_block.inner(chunks[0]);
                            title_block.render(chunks[0], f.buffer_mut());

                            let title = Line::styled(
                                "◇ ALLOWED DOMAINS",
                                Style::default()
                                    .fg(theme.accent())
                                    .add_modifier(Modifier::BOLD),
                            );
                            let subtitle = Line::styled(
                                "  Add domains agents need to reach (registries, APIs, internal hosts)",
                                Style::default().fg(theme.muted()),
                            );
                            let hint = Line::styled(
                                "  These domains are written to .env as AGENTFLOW_ALLOWED_DOMAINS",
                                Style::default().fg(theme.muted()),
                            );
                            let title_para = ratatui::widgets::Paragraph::new(vec![
                                title, subtitle, hint,
                            ]);
                            title_para.render(inner_title, f.buffer_mut());

                            // Input field
                            let input_widget = InputWidget::new(
                                current_input,
                                "Add domain (e.g. pypi.org, *.example.com)",
                            )
                            .focused(true);
                            input_widget.render(chunks[1], f.buffer_mut());

                            // Domain list with scrolling
                            let max_visible = (chunks[2].height as usize).saturating_sub(1);
                            let total_lines = domains.len() + 1; // +1 for header
                            if *scroll_offset > 0 && *scroll_offset + max_visible > total_lines {
                                *scroll_offset = total_lines.saturating_sub(max_visible);
                            }

                            let mut domain_lines: Vec<Line> = Vec::new();
                            domain_lines.push(Line::styled(
                                format!("  Domains ({}):", domains.len()),
                                Style::default()
                                    .fg(theme.accent())
                                    .add_modifier(Modifier::BOLD),
                            ));

                            let visible_domains: Vec<(usize, &String)> = domains
                                .iter()
                                .enumerate()
                                .skip(*scroll_offset)
                                .take(max_visible.saturating_sub(1))
                                .collect();

                            for (_i, domain) in &visible_domains {
                                domain_lines.push(Line::styled(
                                    format!("  ✓ {}", domain),
                                    Style::default().fg(theme.fg()),
                                ));
                            }

                            if total_lines > max_visible + *scroll_offset {
                                domain_lines.push(Line::styled(
                                    format!(
                                        "  ... and {} more (scroll with ↑↓)",
                                        total_lines - max_visible - *scroll_offset
                                    ),
                                    Style::default().fg(theme.muted()),
                                ));
                            }

                            let domains_para = Paragraph::new(domain_lines);
                            domains_para.render(chunks[2], f.buffer_mut());

                            // Help text
                            let help_lines = vec![
                                Line::styled(
                                    "  Enter: add/finish  │  Tab: finish  │  Backspace empty: remove last  │  ↑↓: scroll list  │  Esc: cancel",
                                    Style::default().fg(theme.muted()),
                                ),
                            ];
                            let help_para = Paragraph::new(help_lines);
                            help_para.render(chunks[3], f.buffer_mut());
                        })?;

                        if crossterm::event::poll(std::time::Duration::from_millis(100))? {
                            if let crossterm::event::Event::Key(key) =
                                crossterm::event::read()?
                            {
                                use crossterm::event::KeyCode;
                                use crossterm::event::KeyModifiers;
                                match key.code {
                                    KeyCode::Tab | KeyCode::BackTab => {
                                        config.domain_mode = DomainMode::Manual;
                                        config.allowed_domains = domains.clone();
                                        return Ok(());
                                    }
                                    KeyCode::Enter
                                        if key
                                            .modifiers
                                            .contains(KeyModifiers::SHIFT) =>
                                    {
                                        config.domain_mode = DomainMode::Manual;
                                        config.allowed_domains = domains.clone();
                                        return Ok(());
                                    }
                                    KeyCode::Enter => {
                                        let value = current_input.value().trim().to_string();
                                        if !value.is_empty() {
                                            if !domains.contains(&value) {
                                                domains.push(value);
                                            }
                                            *current_input = Input::default();
                                            *scroll_offset = domains.len().saturating_sub(5);
                                        } else {
                                            config.domain_mode = DomainMode::Manual;
                                            config.allowed_domains = domains.clone();
                                            return Ok(());
                                        }
                                    }
                                    KeyCode::Backspace => {
                                        if current_input.value().is_empty()
                                            && !domains.is_empty()
                                        {
                                            domains.pop();
                                        } else {
                                            let event =
                                                crossterm::event::Event::Key(key);
                                            current_input.handle_event(&event);
                                        }
                                    }
                                    KeyCode::Up if current_input.value().is_empty() => {
                                        if *scroll_offset > 0 {
                                            *scroll_offset -= 1;
                                        }
                                    }
                                    KeyCode::Down if current_input.value().is_empty() => {
                                        let max_visible = 5;
                                        if *scroll_offset + max_visible < domains.len() {
                                            *scroll_offset += 1;
                                        }
                                    }
                                    KeyCode::Esc => {
                                        return Err(anyhow::anyhow!("Setup cancelled"));
                                    }
                                    _ => {
                                        let event = crossterm::event::Event::Key(key);
                                        current_input.handle_event(&event);
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}
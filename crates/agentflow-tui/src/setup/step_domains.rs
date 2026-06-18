use anyhow::Result;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::prelude::*;
use ratatui::widgets::Paragraph;
use ratatui::Terminal;
use std::io;
use tui_input::backend::crossterm::EventHandler;
use tui_input::Input;

use crate::setup::{DomainMode, SetupConfig};
use crate::util::theme::Theme;
use crate::widgets::select::SelectableListState;

enum DomainsState {
    ModeSelect {
        list_state: SelectableListState,
    },
    ManualInput {
        domains: Vec<String>,
        current_input: Input,
        focused: usize,
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
                                    Constraint::Length(4),
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
                                "  Control which domains agents can access",
                                Style::default().fg(theme.muted()),
                            );
                            let hint = Line::styled(
                                "  'Manual' lets you pick specific domains · 'All' allows unrestricted internet",
                                Style::default().fg(theme.muted()),
                            );
                            let title_para =
                                ratatui::widgets::Paragraph::new(vec![title, subtitle, hint]);
                            title_para.render(inner_title, f.buffer_mut());

                            let list_widget = crate::widgets::select::SelectableList::new(
                                &list_state.items,
                                list_state.selected,
                            );
                            list_widget.render(chunks[1], f.buffer_mut());

                            let help = Line::styled(
                                "  ↑↓ navigate  │  Enter: select",
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
                                                    focused: 0,
                                                };
                                            }
                                            1 => {
                                                config.domain_mode = DomainMode::All;
                                                config.allowed_domains = vec![
                                                    "*".to_string(),
                                                ];
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
                    focused,
                } => {
                    loop {
                        terminal.draw(|f| {
                            let area = f.area();
                            let y_start = area.height / 2u16.saturating_sub(8).max(1);

                            let title = Line::styled(
                                "◇ ALLOWED DOMAINS",
                                Style::default()
                                    .fg(theme.accent())
                                    .add_modifier(Modifier::BOLD),
                            );
                            let subtitle = Line::styled(
                                "  Type domain patterns, Enter to add, Backspace to remove last",
                                Style::default().fg(theme.muted()),
                            );
                            let title_para = ratatui::widgets::Paragraph::new(vec![title, subtitle]);
                            title_para.render(
                                Rect::new(2, y_start, area.width - 4, 2),
                                f.buffer_mut(),
                            );

                            let input_area = Rect::new(2, y_start + 3, area.width - 4, 3);
                            let input_widget = crate::widgets::input::InputWidget::new(
                                current_input,
                                "Add domain (e.g. api.example.com, *.example.org)",
                            )
                            .focused(*focused == 0);
                            input_widget.render(input_area, f.buffer_mut());

                            let mut domain_lines: Vec<Line> = Vec::new();
                            domain_lines.push(Line::styled(
                                format!("  Domains ({}):", domains.len()),
                                Style::default()
                                    .fg(theme.accent())
                                    .add_modifier(Modifier::BOLD),
                            ));

                            for (i, domain) in domains.iter().enumerate() {
                                let check_icon = "✓";
                                domain_lines.push(Line::styled(
                                    format!("    {} {}", check_icon, domain),
                                    Style::default().fg(theme.fg()),
                                ));
                                if i > 12 {
                                    domain_lines.push(Line::styled(
                                        format!("    ... and {} more", domains.len() - 13),
                                        Style::default().fg(theme.muted()),
                                    ));
                                    break;
                                }
                            }

                            let domains_para = Paragraph::new(domain_lines);
                            let domains_area =
                                Rect::new(2, y_start + 7, area.width - 4, area.height / 2);
                            domains_para.render(domains_area, f.buffer_mut());

                            let mut help_lines = vec![
                                Line::styled(
                                    "  Enter: add domain  │  Shift+Enter or Tab: finish  │  Backspace on empty: remove last  │  Esc: cancel",
                                    Style::default().fg(theme.muted()),
                                ),
                            ];
                            help_lines.push(Line::styled(
                                "  Type domain specifics of your project (package registries, APIs, internal hosts)",
                                Style::default().fg(theme.muted()),
                            ));
                            let help_para = Paragraph::new(help_lines);
                            let help_area =
                                Rect::new(2, area.height - 3, area.width - 4, 2);
                            help_para.render(help_area, f.buffer_mut());
                        })?;

                        if crossterm::event::poll(std::time::Duration::from_millis(100))? {
                            if let crossterm::event::Event::Key(key) =
                                crossterm::event::read()?
                            {
                                use crossterm::event::KeyCode;
                                use crossterm::event::KeyModifiers;
                                match key.code {
                                    KeyCode::Tab => {
                                        config.domain_mode = DomainMode::Manual;
                                        config.allowed_domains = domains.clone();
                                        return Ok(());
                                    }
                                    KeyCode::BackTab => {
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
                                            let new_domain = if value.starts_with('*') {
                                                value
                                            } else {
                                                value
                                            };
                                            if !domains.contains(&new_domain) {
                                                domains.push(new_domain);
                                            }
                                            *current_input = Input::default();
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
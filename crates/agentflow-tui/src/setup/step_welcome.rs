use anyhow::Result;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Alignment, Constraint, Direction, Layout};
use ratatui::prelude::*;
use ratatui::widgets::Paragraph;
use ratatui::Terminal;
use std::io;

use crate::util::logo::{get_logo_lines, version_string};
use crate::util::theme::Theme;

pub struct WelcomeStep;

impl WelcomeStep {
    pub fn new() -> Self {
        Self
    }

    pub fn render(
        &self,
        terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
        _theme: &Theme,
    ) -> Result<()> {
        let theme = Theme::default();
        
        terminal.draw(|f| {
            let area = f.area();
            let logo_lines = get_logo_lines();

            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Percentage(38),
                    Constraint::Length(logo_lines.len() as u16),
                    Constraint::Length(2),
                    Constraint::Percentage(38),
                ])
                .split(area);

            let mut lines: Vec<Line> = Vec::new();

            for logo_line in &logo_lines {
                lines.push(Line::styled(
                    logo_line.clone(),
                    Style::default().fg(theme.accent()),
                ));
            }

            let paragraph = Paragraph::new(lines).alignment(Alignment::Center);
            paragraph.render(chunks[1], f.buffer_mut());

            let footer = Line::from(vec![
                Span::styled(version_string(), Style::default().fg(theme.muted())),
                Span::raw("  ·  "),
                Span::styled("Press Enter to begin", Style::default().fg(theme.fg())),
            ]);
            let footer_para = Paragraph::new(footer).alignment(Alignment::Center);
            footer_para.render(chunks[2], f.buffer_mut());
        })?;

        loop {
            if crossterm::event::poll(std::time::Duration::from_millis(100))? {
                if let crossterm::event::Event::Key(key) = crossterm::event::read()? {
                    if key.code == crossterm::event::KeyCode::Enter
                        || key.code == crossterm::event::KeyCode::Esc
                        || key.code == crossterm::event::KeyCode::Char(' ')
                    {
                        break;
                    }
                }
            }
        }

        Ok(())
    }
}

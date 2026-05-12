use anyhow::Result;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::prelude::*;
use ratatui::widgets::Paragraph;
use ratatui::Terminal;
use std::io;

use crate::util::theme::Theme;
use crate::widgets::select::SelectableListState;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SetupMode {
    QuickStart,
    Advanced,
}

pub struct ModeStep {
    selected_mode: SetupMode,
}

impl ModeStep {
    pub fn new() -> Self {
        Self {
            selected_mode: SetupMode::QuickStart,
        }
    }

    pub fn selected_mode(&self) -> SetupMode {
        self.selected_mode
    }

    pub fn render(
        &mut self,
        terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
        theme: &Theme,
    ) -> Result<()> {
        let modes = vec![
            "QuickStart".to_string(),
            "Advanced".to_string(),
        ];
        let mut list_state = SelectableListState::new(modes);

        loop {
            terminal.draw(|f| {
                let area = f.area();

                let chunks = Layout::default()
                    .direction(Direction::Vertical)
                    .margin(2)
                    .constraints([
                        Constraint::Length(2),
                        Constraint::Length(1),
                        Constraint::Length(1),
                        Constraint::Min(5),
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
                    "◇  Setup mode",
                    Style::default().fg(theme.accent()).add_modifier(Modifier::BOLD),
                );
                let prompt_para = Paragraph::new(prompt_line);
                prompt_para.render(chunks[2], f.buffer_mut());

                let mode_info = match list_state.selected {
                    0 => "QuickStart: Configure essentials only (recommended)",
                    1 => "Advanced: Full configuration including proxy settings",
                    _ => "",
                };

                let list_widget = crate::widgets::select::SelectableList::new(
                    &list_state.items,
                    list_state.selected,
                ).title("Select setup mode");
                list_widget.render(
                    Rect::new(chunks[3].x, chunks[3].y, chunks[3].width, chunks[3].height - 2),
                    f.buffer_mut(),
                );

                let info_line = Line::styled(mode_info.to_string(), theme.muted_style());
                let info_para = Paragraph::new(info_line).alignment(Alignment::Center);
                info_para.render(chunks[4], f.buffer_mut());
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
                            self.selected_mode = match list_state.selected {
                                0 => SetupMode::QuickStart,
                                1 => SetupMode::Advanced,
                                _ => SetupMode::QuickStart,
                            };
                            break;
                        }
                        KeyCode::Esc => {
                            return Err(anyhow::anyhow!("Setup cancelled"));
                        }
                        _ => {}
                    }
                }
            }
        }

        Ok(())
    }
}

use anyhow::Result;
use ratatui::backend::CrosstermBackend;
use ratatui::prelude::Widget;
use ratatui::Terminal;
use std::collections::VecDeque;
use std::io;
use std::time::Instant;

use crate::app::App;
use crate::util::theme::Theme;
use crate::widgets::table::WorkerTable;

mod events;
mod workers;

use events::LogEvent;
use workers::WorkerInfo;

pub struct DashboardState {
    workers: Vec<WorkerInfo>,
    events: VecDeque<LogEvent>,
    repo: String,
    status: String,
    last_refresh: Instant,
    selected_row: usize,
    show_log: bool,
}

impl Default for DashboardState {
    fn default() -> Self {
        Self::new()
    }
}

impl DashboardState {
    pub fn new() -> Self {
        Self {
            workers: Vec::new(),
            events: VecDeque::new(),
            repo: String::new(),
            status: "Unknown".to_string(),
            last_refresh: Instant::now(),
            selected_row: 0,
            show_log: false,
        }
    }

    pub async fn refresh(&mut self) -> Result<()> {
        self.last_refresh = Instant::now();
        Ok(())
    }
}

pub async fn run_dashboard(_app: &mut App) -> Result<()> {
    let terminal = crate::init_tui()?;
    let result = run_dashboard_inner(terminal).await;
    crate::restore_tui();
    result
}

async fn run_dashboard_inner(mut terminal: Terminal<CrosstermBackend<io::Stdout>>) -> Result<()> {
    let theme = Theme::default();
    let mut state = DashboardState::new();
    let refresh_interval = std::time::Duration::from_secs(2);

    state.refresh().await?;

    loop {
        terminal.draw(|f| {
            let area = f.area();

            let header = format!(
                "AgentFlow Dashboard  |  Repository: {}  |  Status: {}  |  [q:quit r:refresh]",
                state.repo, state.status
            );
            let header_widget = ratatui::widgets::Paragraph::new(header).style(theme.title_style());
            let header_area = ratatui::layout::Rect {
                x: 1,
                y: 0,
                width: area.width.saturating_sub(2),
                height: 1,
            };
            header_widget.render(header_area, f.buffer_mut());

            let worker_data: Vec<(String, String, String)> = state
                .workers
                .iter()
                .map(|w| (w.id.clone(), w.status.clone(), w.detail.clone()))
                .collect();

            let table_area = ratatui::layout::Rect {
                x: 1,
                y: 2,
                width: area.width.saturating_sub(2),
                height: area.height / 2,
            };
            let table = WorkerTable::new(worker_data).selected(state.selected_row);
            table.render(table_area, f.buffer_mut());

            if state.show_log {
                let events_area = ratatui::layout::Rect {
                    x: 1,
                    y: table_area.bottom(),
                    width: area.width.saturating_sub(2),
                    height: area.height.saturating_sub(table_area.bottom()) - 1,
                };
                let events_text: Vec<String> = state
                    .events
                    .iter()
                    .map(|e| format!("{} {} {}", e.timestamp, e.agent, e.message))
                    .collect();
                let events_widget = ratatui::widgets::Paragraph::new(events_text.join("\n"))
                    .style(theme.text_style());
                events_widget.render(events_area, f.buffer_mut());
            }
        })?;

        if crossterm::event::poll(std::time::Duration::from_millis(100))? {
            if let crossterm::event::Event::Key(key) = crossterm::event::read()? {
                use crossterm::event::KeyCode;
                match key.code {
                    KeyCode::Char('q') => break,
                    KeyCode::Char('r') => {
                        state.refresh().await?;
                    }
                    KeyCode::Char('l') => {
                        state.show_log = !state.show_log;
                    }
                    KeyCode::Up => {
                        if state.selected_row > 0 {
                            state.selected_row -= 1;
                        }
                    }
                    KeyCode::Down => {
                        if state.selected_row < state.workers.len().saturating_sub(1) {
                            state.selected_row += 1;
                        }
                    }
                    _ => {}
                }
            }
        }

        if state.last_refresh.elapsed() >= refresh_interval {
            state.refresh().await?;
        }
    }

    Ok(())
}

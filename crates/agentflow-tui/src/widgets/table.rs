use ratatui::layout::{Constraint, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::widgets::{Block, Borders, Row, Table, Widget};

use crate::dashboard::workers::WorkerInfo;

pub struct WorkerTable {
    workers: Vec<WorkerInfo>,
    selected_row: Option<usize>,
}

impl WorkerTable {
    pub fn new(workers: Vec<WorkerInfo>) -> Self {
        Self {
            workers,
            selected_row: None,
        }
    }

    pub fn selected(mut self, idx: usize) -> Self {
        self.selected_row = Some(idx);
        self
    }
}

impl Widget for WorkerTable {
    fn render(self, area: Rect, buf: &mut ratatui::buffer::Buffer) {
        let theme = crate::util::theme::Theme::default();
        let header_style = Style::default()
            .fg(theme.accent())
            .add_modifier(Modifier::BOLD);

        let has_workspace = self.workers.iter().any(|w| w.workspace_name.is_some());
        let rows: Vec<Row> = self
            .workers
            .iter()
            .map(|worker| {
                let status_color = match worker.status.as_str() {
                    "IDLE" | "Done" => theme.success(),
                    "WORKING" | "Assigned" | "Building" => theme.warning(),
                    "Suspended" | "Failed" => theme.error(),
                    _ => theme.fg(),
                };

                let mut cells = vec![worker.id.clone(), worker.status.clone(), worker.detail.clone()];
                if has_workspace {
                    cells.push(worker.workspace_name.clone().unwrap_or_else(|| "-".to_string()));
                }

                Row::new(cells).style(Style::default().fg(status_color))
            })
            .collect();

        let widths = if has_workspace {
            vec![
                Constraint::Length(15),
                Constraint::Length(15),
                Constraint::Length(40),
                Constraint::Length(30),
            ]
        } else {
            vec![
                Constraint::Length(15),
                Constraint::Length(15),
                Constraint::Length(40),
            ]
        };
        let headers = if has_workspace {
            vec!["Worker", "Status", "Detail", "Workspace"]
        } else {
            vec!["Worker", "Status", "Detail"]
        };
        let table = Table::new(rows, widths)
            .header(Row::new(headers).style(header_style))
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title("Workers")
                    .border_style(theme.border()),
            )
            .column_spacing(2);

        table.render(area, buf);
    }
}

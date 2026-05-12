use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::widgets::{Block, Borders, Row, Table, Widget};

pub struct WorkerTable {
    workers: Vec<(String, String, String)>,
    selected_row: Option<usize>,
}

impl WorkerTable {
    pub fn new(workers: Vec<(String, String, String)>) -> Self {
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

        let rows: Vec<Row> = self
            .workers
            .iter()
            .map(|(id, status, detail)| {
                let status_color = match status.as_str() {
                    "IDLE" | "Done" => theme.success(),
                    "WORKING" | "Assigned" | "Building" => theme.warning(),
                    "Suspended" | "Failed" => theme.error(),
                    _ => theme.fg(),
                };

                Row::new(vec![id.clone(), status.clone(), detail.clone()])
                    .style(Style::default().fg(status_color))
            })
            .collect();

        let widths = [15, 15, 40];
        let table = Table::new(rows, widths)
            .header(Row::new(vec!["Worker", "Status", "Detail"]).style(header_style))
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

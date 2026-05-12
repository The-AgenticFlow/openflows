use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Widget};

pub struct CheckList {
    items: Vec<(String, CheckState)>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum CheckState {
    Pass,
    Fail,
    Warn,
    Pending,
}

impl CheckList {
    pub fn new(items: Vec<(String, CheckState)>) -> Self {
        Self { items }
    }
}

impl Widget for CheckList {
    fn render(self, area: Rect, buf: &mut ratatui::buffer::Buffer) {
        let theme = crate::util::theme::Theme::default();
        let mut lines = Vec::new();
        for (label, state) in &self.items {
            let (icon, style) = match state {
                CheckState::Pass => ("✓", theme.success_style()),
                CheckState::Fail => ("✗", theme.error_style()),
                CheckState::Warn => ("⚠", theme.warning_style()),
                CheckState::Pending => ("○", theme.muted_style()),
            };
            lines.push(Line::from(vec![
                Span::styled(format!("{} ", icon), style),
                Span::styled(label, theme.text_style()),
            ]));
        }

        let paragraph = Paragraph::new(lines);
        paragraph.render(area, buf);
    }
}

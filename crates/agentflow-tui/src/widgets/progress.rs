use ratatui::layout::Rect;
use ratatui::widgets::Widget;

pub struct ProgressBar {
    current: usize,
    total: usize,
    width: u16,
}

impl ProgressBar {
    pub fn new(current: usize, total: usize) -> Self {
        Self {
            current,
            total,
            width: 40,
        }
    }

    pub fn width(mut self, width: u16) -> Self {
        self.width = width;
        self
    }
}

impl Widget for ProgressBar {
    fn render(self, area: Rect, buf: &mut ratatui::buffer::Buffer) {
        let theme = crate::util::theme::Theme::default();
        let filled = if self.total > 0 {
            (self.current as f64 / self.total as f64 * self.width as f64).ceil() as u16
        } else {
            0
        };

        let bar_str: String = (0..self.width)
            .map(|i| if i < filled { '█' } else { '░' })
            .collect();

        let content = format!("Step {}/{}: {}", self.current, self.total, bar_str);
        let paragraph = ratatui::widgets::Paragraph::new(content).style(theme.text_style());
        paragraph.render(area, buf);
    }
}

use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Widget};

pub struct InfoBox<'a> {
    title: &'a str,
    content: &'a [String],
    box_type: InfoBoxType,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum InfoBoxType {
    Info,
    Warning,
    Success,
    Error,
}

impl<'a> InfoBox<'a> {
    pub fn new(title: &'a str, content: &'a [String]) -> Self {
        Self {
            title,
            content,
            box_type: InfoBoxType::Info,
        }
    }

    pub fn box_type(mut self, box_type: InfoBoxType) -> Self {
        self.box_type = box_type;
        self
    }

    pub fn warning(mut self) -> Self {
        self.box_type = InfoBoxType::Warning;
        self
    }

    pub fn success(mut self) -> Self {
        self.box_type = InfoBoxType::Success;
        self
    }

    pub fn error(mut self) -> Self {
        self.box_type = InfoBoxType::Error;
        self
    }
}

impl Widget for InfoBox<'_> {
    fn render(self, area: Rect, buf: &mut ratatui::buffer::Buffer) {
        let theme = crate::util::theme::Theme::default();

        let (icon, accent_color) = match self.box_type {
            InfoBoxType::Info => ("ℹ", theme.accent()),
            InfoBoxType::Warning => ("⚠", theme.warning()),
            InfoBoxType::Success => ("✓", theme.success()),
            InfoBoxType::Error => ("✗", theme.error()),
        };

        let mut lines = Vec::new();

        for line in self.content {
            if line.is_empty() {
                lines.push(Line::raw(""));
            } else if line.starts_with("- ") || line.starts_with("• ") {
                lines.push(Line::from(vec![
                    Span::styled("  ", Style::default()),
                    Span::styled(line, theme.text_style()),
                ]));
            } else {
                lines.push(Line::styled(line.clone(), theme.text_style()));
            }
        }

        let title_with_icon = format!(" {} {}", icon, self.title);

        let block = Block::default()
            .borders(Borders::ALL)
            .title(title_with_icon)
            .border_style(Style::default().fg(accent_color))
            .title_style(Style::default().fg(accent_color).add_modifier(Modifier::BOLD));

        let inner = block.inner(area);
        block.render(area, buf);

        let paragraph = Paragraph::new(lines);
        paragraph.render(inner, buf);
    }
}

pub struct KeyValueBox<'a> {
    title: &'a str,
    items: Vec<(&'a str, &'a str)>,
}

impl<'a> KeyValueBox<'a> {
    pub fn new(title: &'a str) -> Self {
        Self {
            title,
            items: Vec::new(),
        }
    }

    pub fn item(mut self, key: &'a str, value: &'a str) -> Self {
        self.items.push((key, value));
        self
    }

    pub fn items(mut self, items: Vec<(&'a str, &'a str)>) -> Self {
        self.items = items;
        self
    }
}

impl Widget for KeyValueBox<'_> {
    fn render(self, area: Rect, buf: &mut ratatui::buffer::Buffer) {
        let theme = crate::util::theme::Theme::default();

        let mut lines = Vec::new();
        lines.push(Line::raw(""));

        for (key, value) in &self.items {
            lines.push(Line::from(vec![
                Span::styled("  ", Style::default()),
                Span::styled(format!("{}: ", key), theme.muted_style()),
                Span::styled(value.to_string(), theme.text_style()),
            ]));
        }
        lines.push(Line::raw(""));

        let block = Block::default()
            .borders(Borders::ALL)
            .title(format!(" {} ", self.title))
            .border_style(Style::default().fg(theme.border()))
            .title_style(theme.title_style());

        let inner = block.inner(area);
        block.render(area, buf);

        let paragraph = Paragraph::new(lines);
        paragraph.render(inner, buf);
    }
}

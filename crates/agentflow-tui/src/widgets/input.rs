use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::widgets::{Block, Borders, Widget};
use tui_input::Input;

pub struct InputWidget<'a> {
    input: &'a Input,
    label: &'a str,
    masked: bool,
    focused: bool,
    optional: bool,
}

impl<'a> InputWidget<'a> {
    pub fn new(input: &'a Input, label: &'a str) -> Self {
        Self {
            input,
            label,
            masked: false,
            focused: false,
            optional: false,
        }
    }

    pub fn masked(mut self, masked: bool) -> Self {
        self.masked = masked;
        self
    }

    pub fn focused(mut self, focused: bool) -> Self {
        self.focused = focused;
        self
    }

    pub fn optional(mut self, optional: bool) -> Self {
        self.optional = optional;
        self
    }
}

impl Widget for InputWidget<'_> {
    fn render(self, area: Rect, buf: &mut ratatui::buffer::Buffer) {
        let theme = crate::util::theme::Theme::default();
        let display_value = if self.masked {
            "•".repeat(self.input.value().chars().count())
        } else {
            self.input.value().to_string()
        };

        let label_suffix = if self.optional { "" } else { "" };
        let full_label = if self.label.ends_with(':') {
            format!(" {}{}", self.label.trim_end_matches(':'), label_suffix)
        } else {
            format!(" {}{}", self.label, label_suffix)
        };

        let border_color = if self.focused {
            theme.border_focus()
        } else {
            theme.border()
        };

        let block = Block::default()
            .borders(Borders::ALL)
            .title(full_label)
            .title_style(if self.focused {
                Style::default()
                    .fg(theme.border_focus())
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(theme.muted())
            })
            .border_style(Style::default().fg(border_color));

        let inner = block.inner(area);
        block.render(area, buf);

        let display_with_cursor = if self.focused {
            let cursor_pos = self.input.cursor();
            let chars: Vec<char> = display_value.chars().collect();
            let mut result = String::new();
            for (i, c) in chars.iter().enumerate() {
                if i == cursor_pos {
                    result.push_str(&format!("│{}", c));
                } else {
                    result.push(*c);
                }
            }
            if cursor_pos >= chars.len() {
                result.push('│');
            }
            result
        } else if display_value.is_empty() {
            "···".to_string()
        } else {
            display_value.clone()
        };

        let style = if !self.focused && display_value.is_empty() {
            Style::default().fg(theme.muted())
        } else if self.focused {
            Style::default().fg(theme.fg())
        } else {
            Style::default().fg(theme.muted())
        };

        let paragraph = ratatui::widgets::Paragraph::new(display_with_cursor).style(style);
        paragraph.render(inner, buf);
    }
}

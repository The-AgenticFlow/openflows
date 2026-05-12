use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Widget};

pub struct ConfirmDialog<'a> {
    prompt: &'a str,
    selected_yes: bool,
}

impl<'a> ConfirmDialog<'a> {
    pub fn new(prompt: &'a str) -> Self {
        Self {
            prompt,
            selected_yes: true,
        }
    }

    pub fn selected_yes(mut self, yes: bool) -> Self {
        self.selected_yes = yes;
        self
    }
}

impl Widget for ConfirmDialog<'_> {
    fn render(self, area: Rect, buf: &mut ratatui::buffer::Buffer) {
        let theme = crate::util::theme::Theme::default();

        let yes_style = if self.selected_yes {
            Style::default()
                .fg(theme.accent())
                .add_modifier(Modifier::BOLD)
        } else {
            theme.muted_style()
        };

        let no_style = if !self.selected_yes {
            Style::default()
                .fg(theme.accent())
                .add_modifier(Modifier::BOLD)
        } else {
            theme.muted_style()
        };

        let lines = vec![
            Line::raw(""),
            Line::styled(self.prompt.to_string(), theme.text_style()),
            Line::raw(""),
            Line::from(vec![
                Span::styled("  [Yes]  ", yes_style),
                Span::styled("   [No]  ", no_style),
            ]),
        ];

        let paragraph = Paragraph::new(lines);
        paragraph.render(area, buf);
    }
}

pub struct ConfirmDialogState {
    pub selected_yes: bool,
}

impl ConfirmDialogState {
    pub fn new() -> Self {
        Self { selected_yes: true }
    }

    pub fn toggle(&mut self) {
        self.selected_yes = !self.selected_yes;
    }

    pub fn move_left(&mut self) {
        self.selected_yes = true;
    }

    pub fn move_right(&mut self) {
        self.selected_yes = false;
    }

    pub fn handle_key(&mut self, key: crossterm::event::KeyEvent) -> ConfirmResult {
        use crossterm::event::KeyCode;
        match key.code {
            KeyCode::Left | KeyCode::Char('h') => {
                self.move_left();
                ConfirmResult::Continue
            }
            KeyCode::Right | KeyCode::Char('l') => {
                self.move_right();
                ConfirmResult::Continue
            }
            KeyCode::Tab => {
                self.toggle();
                ConfirmResult::Continue
            }
            KeyCode::Enter => {
                if self.selected_yes {
                    ConfirmResult::Yes
                } else {
                    ConfirmResult::No
                }
            }
            KeyCode::Char('y') | KeyCode::Char('Y') => {
                self.selected_yes = true;
                ConfirmResult::Yes
            }
            KeyCode::Char('n') | KeyCode::Char('N') => {
                self.selected_yes = false;
                ConfirmResult::No
            }
            KeyCode::Esc => ConfirmResult::Cancel,
            _ => ConfirmResult::Continue,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ConfirmResult {
    Yes,
    No,
    Cancel,
    Continue,
}

impl Default for ConfirmDialogState {
    fn default() -> Self {
        Self::new()
    }
}

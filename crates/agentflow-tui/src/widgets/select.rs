use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Widget};
use tui_input::Input;

pub struct SelectableList<'a> {
    items: &'a [String],
    selected: usize,
    title: Option<&'a str>,
    search_query: Option<&'a Input>,
    show_search: bool,
    filtered_indices: &'a [usize],
}

impl<'a> SelectableList<'a> {
    pub fn new(items: &'a [String], selected: usize) -> Self {
        Self {
            items,
            selected,
            title: None,
            search_query: None,
            show_search: false,
            filtered_indices: &[],
        }
    }

    pub fn title(mut self, title: &'a str) -> Self {
        self.title = Some(title);
        self
    }

    pub fn search_query(mut self, query: &'a Input) -> Self {
        self.search_query = Some(query);
        self.show_search = true;
        self
    }

    pub fn filtered_indices(mut self, indices: &'a [usize]) -> Self {
        self.filtered_indices = indices;
        self
    }

    fn visible_items(&self) -> Vec<(usize, &String)> {
        if self.filtered_indices.is_empty() && self.show_search {
            let query = self.search_query.map(|i| i.value().to_lowercase()).unwrap_or_default();
            if query.is_empty() {
                self.items.iter().enumerate().collect()
            } else {
                self.items
                    .iter()
                    .enumerate()
                    .filter(|(_, item)| item.to_lowercase().contains(&query))
                    .collect()
            }
        } else if !self.filtered_indices.is_empty() {
            self.filtered_indices
                .iter()
                .map(|&i| (i, &self.items[i]))
                .collect()
        } else {
            self.items.iter().enumerate().collect()
        }
    }
}

impl Widget for SelectableList<'_> {
    fn render(self, area: Rect, buf: &mut ratatui::buffer::Buffer) {
        let theme = crate::util::theme::Theme::default();
        let visible = self.visible_items();

        let content_height = if self.show_search { area.height.saturating_sub(3) } else { area.height.saturating_sub(2) };

        let scroll_offset = if self.selected >= content_height as usize {
            self.selected - content_height as usize + 1
        } else {
            0
        };

        let mut lines = Vec::new();

        if self.show_search {
            let search_val = self.search_query.map(|i| i.value()).unwrap_or("");
            let search_line = Line::from(vec![
                Span::styled("  ◄ ", Style::default().fg(theme.muted())),
                Span::styled(search_val, Style::default().fg(theme.fg())),
                Span::styled("│", Style::default().fg(theme.accent())),
            ]);
            lines.push(search_line);
            lines.push(Line::raw(""));
        }

        for (idx, item) in visible.iter().skip(scroll_offset).take(content_height as usize) {
            let is_selected = *idx == self.selected;
            let icon = if is_selected { "▸" } else { " " };
            let style = if is_selected {
                Style::default()
                    .fg(theme.accent())
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(theme.muted())
            };
            lines.push(Line::from(vec![
                Span::styled(format!("  {} ", icon), style),
                Span::styled(item.as_str(), style),
            ]));
        }

        while lines.len() < area.height as usize {
            lines.push(Line::raw(""));
        }

        let footer = Line::from(vec![
            Span::styled("  ↑/↓", Style::default().fg(theme.muted())),
            Span::styled(" navigate  ", Style::default().fg(theme.fg())),
            Span::styled("Enter", Style::default().fg(theme.muted())),
            Span::styled(" select  ", Style::default().fg(theme.fg())),
            Span::styled("Esc", Style::default().fg(theme.muted())),
            Span::styled(" back", Style::default().fg(theme.fg())),
        ]);

        let block = Block::default()
            .borders(Borders::NONE);

        let inner = block.inner(area);
        block.render(area, buf);

        let mut all_lines = lines.clone();
        all_lines.push(footer);

        let paragraph = Paragraph::new(all_lines);
        paragraph.render(inner, buf);
    }
}

pub struct SelectableListState {
    pub items: Vec<String>,
    pub selected: usize,
    pub search_input: Input,
    pub show_search: bool,
}

impl SelectableListState {
    pub fn new(items: Vec<String>) -> Self {
        Self {
            items,
            selected: 0,
            search_input: Input::default(),
            show_search: false,
        }
    }

    pub fn with_search(mut self) -> Self {
        self.show_search = true;
        self
    }

    pub fn filtered_indices(&self) -> Vec<usize> {
        let query = self.search_input.value().to_lowercase();
        if query.is_empty() {
            return vec![];
        }
        self.items
            .iter()
            .enumerate()
            .filter(|(_, item)| item.to_lowercase().contains(&query))
            .map(|(i, _)| i)
            .collect()
    }

    pub fn visible_items(&self) -> Vec<(usize, &String)> {
        let query = self.search_input.value().to_lowercase();
        if query.is_empty() {
            self.items.iter().enumerate().collect()
        } else {
            self.items
                .iter()
                .enumerate()
                .filter(|(_, item)| item.to_lowercase().contains(&query))
                .collect()
        }
    }

    pub fn move_up(&mut self) {
        let visible = self.visible_items();
        if visible.is_empty() {
            return;
        }
        let current_pos = visible.iter().position(|(i, _)| *i == self.selected).unwrap_or(0);
        if current_pos > 0 {
            self.selected = visible[current_pos - 1].0;
        } else {
            self.selected = visible.last().map(|(i, _)| *i).unwrap_or(0);
        }
    }

    pub fn move_down(&mut self) {
        let visible = self.visible_items();
        if visible.is_empty() {
            return;
        }
        let current_pos = visible.iter().position(|(i, _)| *i == self.selected).unwrap_or(0);
        if current_pos < visible.len() - 1 {
            self.selected = visible[current_pos + 1].0;
        } else {
            self.selected = visible.first().map(|(i, _)| *i).unwrap_or(0);
        }
    }

    pub fn selected_item(&self) -> Option<&String> {
        self.items.get(self.selected)
    }

    pub fn handle_key(&mut self, key: crossterm::event::KeyEvent) -> bool {
        use crossterm::event::KeyCode;
        match key.code {
            KeyCode::Up => {
                self.move_up();
                false
            }
            KeyCode::Down => {
                self.move_down();
                false
            }
            KeyCode::Enter => true,
            KeyCode::Esc => {
                if self.show_search && !self.search_input.value().is_empty() {
                    self.search_input = Input::default();
                }
                false
            }
            _ => {
                if self.show_search {
                    use tui_input::backend::crossterm::EventHandler;
                    let event = crossterm::event::Event::Key(key);
                    self.search_input.handle_event(&event);
                    let visible = self.visible_items();
                    if !visible.is_empty() && !visible.iter().any(|(i, _)| *i == self.selected) {
                        self.selected = visible[0].0;
                    }
                }
                false
            }
        }
    }
}

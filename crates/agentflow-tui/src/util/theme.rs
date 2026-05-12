use ratatui::style::{Color, Modifier, Style};

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Theme {
    Dark,
    Light,
}

impl Default for Theme {
    fn default() -> Self {
        Theme::Dark
    }
}

impl Theme {
    pub fn bg(&self) -> Color {
        Color::Rgb(10, 10, 15)
    }

    pub fn surface(&self) -> Color {
        Color::Rgb(18, 18, 26)
    }

    pub fn fg(&self) -> Color {
        Color::Rgb(230, 235, 245)
    }

    pub fn border(&self) -> Color {
        Color::Rgb(60, 70, 100)
    }

    pub fn border_focus(&self) -> Color {
        Color::Rgb(0, 255, 170)
    }

    pub fn success(&self) -> Color {
        Color::Rgb(0, 255, 170)
    }

    pub fn error(&self) -> Color {
        Color::Rgb(255, 85, 120)
    }

    pub fn warning(&self) -> Color {
        Color::Rgb(255, 200, 100)
    }

    pub fn accent(&self) -> Color {
        Color::Rgb(0, 200, 255)
    }

    pub fn accent_alt(&self) -> Color {
        Color::Rgb(180, 100, 255)
    }

    pub fn muted(&self) -> Color {
        Color::Rgb(80, 85, 110)
    }

    pub fn title_style(&self) -> Style {
        Style::default()
            .fg(self.accent())
            .add_modifier(Modifier::BOLD)
    }

    pub fn success_style(&self) -> Style {
        Style::default().fg(self.success())
    }

    pub fn error_style(&self) -> Style {
        Style::default().fg(self.error())
    }

    pub fn warning_style(&self) -> Style {
        Style::default().fg(self.warning())
    }

    pub fn text_style(&self) -> Style {
        Style::default().fg(self.fg())
    }

    pub fn muted_style(&self) -> Style {
        Style::default().fg(self.muted())
    }
}

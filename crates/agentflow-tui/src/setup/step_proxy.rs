use anyhow::Result;
use ratatui::backend::CrosstermBackend;
use ratatui::prelude::{Line, Modifier, Style, Widget};
use ratatui::Terminal;
use std::io;
use tui_input::backend::crossterm::EventHandler;
use tui_input::Input;

use crate::setup::SetupConfig;
use crate::util::theme::Theme;
use crate::widgets::input::InputWidget;

#[derive(Default)]
pub struct ProxyStep;

impl ProxyStep {
    pub fn new() -> Self {
        Self
    }

    pub async fn render(
        &self,
        terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
        _theme: &Theme,
        config: &mut SetupConfig,
    ) -> Result<()> {
        let theme = Theme::default();
        let mut proxy_url_input = Input::new(config.proxy_url.clone().unwrap_or_default());
        let mut proxy_key_input = Input::new(config.proxy_api_key.clone().unwrap_or_default());
        let mut gateway_url_input = Input::new(config.gateway_url.clone().unwrap_or_default());
        let mut gateway_key_input = Input::new(config.gateway_api_key.clone().unwrap_or_default());
        let mut focused_field = 0;

        loop {
            terminal.draw(|f| {
                let area = f.area();
                let y_start = area.height / 2 - 8;

                let title = Line::styled(
                    "◇ PROXY CONFIGURATION",
                    Style::default()
                        .fg(theme.accent())
                        .add_modifier(Modifier::BOLD),
                );
                let subtitle = Line::styled(
                    "  Advanced: configure LiteLLM proxy settings",
                    Style::default().fg(theme.muted()),
                );
                let title_para = ratatui::widgets::Paragraph::new(vec![title, subtitle]);
                title_para.render(
                    ratatui::layout::Rect {
                        x: 2,
                        y: y_start,
                        width: area.width - 4,
                        height: 2,
                    },
                    f.buffer_mut(),
                );

                let fields = [
                    (&proxy_url_input, "Proxy URL", true),
                    (&proxy_key_input, "Proxy API Key", true),
                    (&gateway_url_input, "Gateway URL", true),
                    (&gateway_key_input, "Gateway API Key", true),
                ];

                for (i, (input, label, optional)) in fields.iter().enumerate() {
                    let y = y_start + 3 + (i as u16) * 4;
                    let widget_area = ratatui::layout::Rect {
                        x: 2,
                        y,
                        width: area.width - 4,
                        height: 3,
                    };
                    let widget = InputWidget::new(input, label)
                        .masked(i == 1 || i == 3)
                        .focused(focused_field == i)
                        .optional(*optional);
                    widget.render(widget_area, f.buffer_mut());
                }
            })?;

            if crossterm::event::poll(std::time::Duration::from_millis(100))? {
                if let crossterm::event::Event::Key(key) = crossterm::event::read()? {
                    use crossterm::event::KeyCode;
                    match key.code {
                        KeyCode::Tab => {
                            focused_field = (focused_field + 1) % 4;
                        }
                        KeyCode::BackTab => {
                            focused_field = if focused_field == 0 {
                                3
                            } else {
                                focused_field - 1
                            };
                        }
                        KeyCode::Enter => {
                            config.proxy_enabled = !proxy_url_input.value().is_empty();
                            config.proxy_url = if proxy_url_input.value().is_empty() {
                                None
                            } else {
                                Some(proxy_url_input.value().to_string())
                            };
                            config.proxy_api_key = if proxy_key_input.value().is_empty() {
                                None
                            } else {
                                Some(proxy_key_input.value().to_string())
                            };
                            config.gateway_url = if gateway_url_input.value().is_empty() {
                                None
                            } else {
                                Some(gateway_url_input.value().to_string())
                            };
                            config.gateway_api_key = if gateway_key_input.value().is_empty() {
                                None
                            } else {
                                Some(gateway_key_input.value().to_string())
                            };
                            break;
                        }
                        KeyCode::Esc => {
                            config.proxy_enabled = false;
                            break;
                        }
                        _ => {
                            let event = crossterm::event::Event::Key(key);
                            let input = match focused_field {
                                0 => &mut proxy_url_input,
                                1 => &mut proxy_key_input,
                                2 => &mut gateway_url_input,
                                3 => &mut gateway_key_input,
                                _ => unreachable!(),
                            };
                            input.handle_event(&event);
                        }
                    }
                }
            }
        }

        Ok(())
    }
}

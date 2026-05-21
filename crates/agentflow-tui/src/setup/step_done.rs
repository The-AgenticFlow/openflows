use anyhow::Result;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Alignment, Constraint, Direction, Layout};
use ratatui::prelude::*;
use ratatui::widgets::Paragraph;
use ratatui::Terminal;
use std::io;
use std::path::Path;

use crate::setup::write_env_file;
use crate::setup::write_registry_file;
use crate::setup::SetupConfig;
use crate::util::theme::Theme;
use crate::widgets::check::{CheckList, CheckState};

#[derive(Default)]
pub struct DoneStep;

impl DoneStep {
    pub fn new() -> Self {
        Self
    }

    pub async fn render(
        &self,
        terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
        theme: &Theme,
        config: &SetupConfig,
    ) -> Result<()> {
        let current_dir = std::env::current_dir()?;
        write_env_file(config, &current_dir)?;
        write_registry_file(config, &current_dir)?;

        let registry_path = current_dir
            .join("orchestration")
            .join("agent")
            .join("registry.json");

        let mut checks = Vec::new();

        if Path::new(".env").exists() {
            checks.push((".env file written".to_string(), CheckState::Pass));
        } else {
            checks.push((".env file write failed".to_string(), CheckState::Fail));
        }

        if registry_path.exists() {
            match config::Registry::load(&registry_path) {
                Ok(registry) => {
                    let agent_count = registry.active_agents().count();
                    let slot_count = registry.all_worker_slots().len();
                    checks.push((
                        format!(
                            "Registry loaded ({} agents, {} slots)",
                            agent_count, slot_count
                        ),
                        CheckState::Pass,
                    ));
                }
                Err(e) => {
                    checks.push((format!("Registry parse error: {}", e), CheckState::Fail));
                }
            }
        } else {
            checks.push(("Registry file not found".to_string(), CheckState::Warn));
        }

        loop {
            terminal.draw(|f| {
                let area = f.area();

                let chunks = Layout::default()
                    .direction(Direction::Vertical)
                    .margin(2)
                    .constraints([
                        Constraint::Length(3),
                        Constraint::Length(6),
                        Constraint::Length(6),
                        Constraint::Min(1),
                    ])
                    .split(area);

                let title_chunks = Layout::default()
                    .direction(Direction::Vertical)
                    .constraints([Constraint::Length(1), Constraint::Length(1)])
                    .split(chunks[0]);

                let title_line = Line::styled(
                    "Setup complete!",
                    Style::default()
                        .fg(theme.success())
                        .add_modifier(Modifier::BOLD),
                );
                let title_para = Paragraph::new(title_line).alignment(Alignment::Left);
                title_para.render(title_chunks[0], f.buffer_mut());

                let sep_line = Line::styled(
                    "─────────────────────────────────────",
                    Style::default().fg(theme.border()),
                );
                let sep_para = Paragraph::new(sep_line);
                sep_para.render(title_chunks[1], f.buffer_mut());

                let check_list = CheckList::new(checks.clone());
                check_list.render(chunks[1], f.buffer_mut());

                let next_chunks = Layout::default()
                    .direction(Direction::Vertical)
                    .constraints([Constraint::Length(1), Constraint::Length(4)])
                    .split(chunks[2]);

                let next_title = Line::styled(
                    "Next steps:",
                    Style::default()
                        .fg(theme.accent())
                        .add_modifier(Modifier::BOLD),
                );
                let next_para = Paragraph::new(next_title);
                next_para.render(next_chunks[0], f.buffer_mut());

                let steps = vec![
                    Line::styled(
                        "  1. Review your .env and registry.json files",
                        theme.text_style(),
                    ),
                    Line::styled(
                        "  2. Run 'openflows-setup' to reconfigure",
                        theme.text_style(),
                    ),
                    Line::styled("  3. Run 'openflows' to start", theme.text_style()),
                ];
                let steps_para = Paragraph::new(steps);
                steps_para.render(next_chunks[1], f.buffer_mut());

                let help_line =
                    Line::styled("Press Enter to exit...", Style::default().fg(theme.muted()));
                let help_para = Paragraph::new(help_line).alignment(Alignment::Center);
                help_para.render(chunks[3], f.buffer_mut());
            })?;

            if crossterm::event::poll(std::time::Duration::from_millis(100))? {
                if let crossterm::event::Event::Key(key) = crossterm::event::read()? {
                    if key.code == crossterm::event::KeyCode::Enter {
                        break;
                    }
                }
            }
        }

        Ok(())
    }
}

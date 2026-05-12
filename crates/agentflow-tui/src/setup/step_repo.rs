use anyhow::Result;
use ratatui::backend::CrosstermBackend;
use ratatui::prelude::{Line, Modifier, Style, Widget};
use ratatui::Terminal;
use std::io;
use tui_input::backend::crossterm::EventHandler;
use tui_input::Input;

use crate::setup::SetupConfig;
use crate::util::theme::Theme;
use crate::widgets::check::{CheckList, CheckState};
use crate::widgets::input::InputWidget;

pub struct RepoStep;

impl RepoStep {
    pub fn new() -> Self {
        Self
    }

    /// Get the workspace directory path anchored to ~/.agentflow
    fn get_agentflow_workspace_dir(repo: &str) -> String {
        let home = std::env::var("HOME")
            .or_else(|_| std::env::var("USERPROFILE"))
            .unwrap_or_else(|_| ".".to_string());
        let dir_name = repo.replace('/', "-").replace('\\', "-");
        format!("{}/.agentflow/workspaces/{}", home, dir_name)
    }

    pub async fn render(
        &self,
        terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
        _theme: &Theme,
        config: &mut SetupConfig,
    ) -> Result<()> {
        let theme = Theme::default();
        let mut repo_input = Input::new(config.repo.clone());
        // Workspace is always anchored to ~/.agentflow/workspaces - derived from repo
        // This ensures artifact directories are always in the hidden ~/.agentflow dir
        let workspace_dir = Self::get_agentflow_workspace_dir(&config.repo);
        let mut workspace_input = Input::new(workspace_dir.clone());
        let mut focused_field = 0;

        let repo_regex = regex::Regex::new(r"^[a-zA-Z0-9_-]+/[a-zA-Z0-9_.-]+$").unwrap();

        loop {
            // Auto-update workspace when repo changes (workspace is derived, not editable)
            let current_repo = repo_input.value();
            let expected_workspace = Self::get_agentflow_workspace_dir(current_repo);
            if workspace_input.value() != expected_workspace {
                workspace_input = Input::new(expected_workspace);
            }

            let repo_valid = repo_regex.is_match(repo_input.value());
            let workspace_valid = !workspace_input.value().is_empty();

            terminal.draw(|f| {
                let area = f.area();
                let y_start = area.height / 2 - 6;

                let title = Line::styled(
                    "◇ REPOSITORY CONFIGURATION",
                    Style::default()
                        .fg(theme.accent())
                        .add_modifier(Modifier::BOLD),
                );
                let subtitle = Line::styled(
                    "  Set target repository and workspace",
                    Style::default().fg(theme.muted()),
                );
                let title_para = ratatui::widgets::Paragraph::new(vec![title, subtitle]);
                title_para.render(
                    ratatui::layout::Rect { x: 2, y: y_start, width: area.width - 4, height: 2 },
                    f.buffer_mut(),
                );

                let repo_widget_area = ratatui::layout::Rect {
                    x: 2,
                    y: y_start + 3,
                    width: area.width - 4,
                    height: 3,
                };
                let repo_widget = InputWidget::new(&repo_input, "GitHub Repository")
                    .focused(focused_field == 0);
                repo_widget.render(repo_widget_area, f.buffer_mut());

                let ws_widget_area = ratatui::layout::Rect {
                    x: 2,
                    y: y_start + 7,
                    width: area.width - 4,
                    height: 3,
                };
                let ws_widget = InputWidget::new(&workspace_input, "Workspace Directory (auto-derived)")
                    .focused(false); // Never focused - auto-derived from repo
                ws_widget.render(ws_widget_area, f.buffer_mut());

                let mut checks = Vec::new();
                if repo_valid {
                    checks.push(("Repository format valid".to_string(), CheckState::Pass));
                } else {
                    checks.push((
                        "Invalid repository format (owner/repo)".to_string(),
                        CheckState::Fail,
                    ));
                }
                // Workspace is always valid since it's auto-derived to ~/.agentflow/workspaces/
                checks.push(("Workspace directory (auto-derived to ~/.agentflow)".to_string(), CheckState::Pass));
                let check_area = ratatui::layout::Rect {
                    x: 2,
                    y: y_start + 11,
                    width: area.width - 4,
                    height: 4,
                };
                let check_list = CheckList::new(checks);
                check_list.render(check_area, f.buffer_mut());
            })?;

            if crossterm::event::poll(std::time::Duration::from_millis(100))? {
                if let crossterm::event::Event::Key(key) = crossterm::event::read()? {
                    use crossterm::event::KeyCode;
                    match key.code {
                        KeyCode::Tab => {
                            // Only one editable field (repo), workspace is auto-derived
                            focused_field = 0;
                        }
                        KeyCode::Enter => {
                            if repo_valid && workspace_valid {
                                config.repo = repo_input.value().to_string();
                                config.workspace_dir = workspace_input.value().to_string();
                                break;
                            }
                        }
                        KeyCode::Esc => {
                            return Err(anyhow::anyhow!("Setup cancelled"));
                        }
                        _ => {
                            let event = crossterm::event::Event::Key(key);
                            // Only handle events for repo field - workspace is auto-derived
                            if focused_field == 0 {
                                repo_input.handle_event(&event);
                            }
                        }
                    }
                }
            }
        }

        Ok(())
    }
}

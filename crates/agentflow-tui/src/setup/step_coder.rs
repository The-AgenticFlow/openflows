// crates/agentflow-tui/src/setup/step_coder.rs
//! Coder workspace configuration step in the setup wizard.
//!
//! This is a primary architecture decision — it determines whether
//! FORGE-SENTINEL pairs run in local git worktrees or in isolated
//! Coder workspaces. It should appear early in the wizard flow.

use anyhow::Result;
use ratatui::backend::CrosstermBackend;
use ratatui::prelude::Widget;
use ratatui::Terminal;
use std::io;
use std::time::Duration;

use super::{SetupConfig, WorkspaceProvider};
use crate::util::theme::Theme;

pub struct CoderStep {
    selected: usize,
    coder_url: String,
    admin_password: String,
}

impl CoderStep {
    pub fn new() -> Self {
        Self {
            selected: 0,
            coder_url: "http://localhost:7080".to_string(),
            admin_password: "openflows".to_string(),
        }
    }

    pub async fn render(
        &mut self,
        terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
        theme: &Theme,
        config: &mut SetupConfig,
    ) -> Result<()> {
        let options = [
            "Local mode (git worktrees)",
            "Coder mode (isolated workspaces)",
        ];

        loop {
            terminal.draw(|f| {
                let area = f.area();

                let mut lines = Vec::new();
                lines.push(String::new());
                lines.push("╔══════════════════════════════════════════════════╗".to_string());
                lines.push("║     Workspace Architecture                       ║".to_string());
                lines.push("╚══════════════════════════════════════════════════╝".to_string());
                lines.push(String::new());
                lines.push("How should FORGE-SENTINEL pairs run?".to_string());
                lines.push(String::new());
                lines.push("This choice determines whether agent pairs share".to_string());
                lines.push("a local git worktree (simpler) or run in isolated".to_string());
                lines.push("Coder workspaces (more secure, requires Docker).".to_string());
                lines.push(String::new());

                for (i, opt) in options.iter().enumerate() {
                    if i == self.selected {
                        lines.push(format!("  > {}", opt));
                    } else {
                        lines.push(format!("    {}", opt));
                    }
                }

                lines.push(String::new());
                if self.selected == 1 {
                    lines.push("── Coder Settings ──".to_string());
                    lines.push(format!("  URL:     {}", self.coder_url));
                    lines.push(format!("  Password: {}", "*".repeat(self.admin_password.len())));
                    lines.push(String::new());
                    lines.push("  Requires: Coder server running + Docker".to_string());
                    lines.push("  Openflows will bootstrap Coder on startup.".to_string());
                } else {
                    lines.push("── Local Mode ──".to_string());
                    lines.push("  Pairs run in git worktrees on this machine.".to_string());
                    lines.push("  No Docker or Coder required.".to_string());
                }

                lines.push(String::new());
                lines.push("[↑/↓] Select  [Enter] Confirm".to_string());

                let content = lines.join("\n");
                let paragraph = ratatui::widgets::Paragraph::new(content).style(theme.text_style());
                paragraph.render(area, f.buffer_mut());
            })?;

            if crossterm::event::poll(Duration::from_millis(100))? {
                if let crossterm::event::Event::Key(key) = crossterm::event::read()? {
                    match key.code {
                        crossterm::event::KeyCode::Up => {
                            if self.selected > 0 {
                                self.selected -= 1;
                            }
                        }
                        crossterm::event::KeyCode::Down => {
                            if self.selected < options.len() - 1 {
                                self.selected += 1;
                            }
                        }
                        crossterm::event::KeyCode::Enter => {
                            break;
                        }
                        crossterm::event::KeyCode::Char('q') => {
                            return Err(anyhow::anyhow!("Setup cancelled"));
                        }
                        _ => {}
                    }
                }
            }
        }

        match self.selected {
            0 => {
                config.workspace_provider = WorkspaceProvider::Local;
                config.coder_url = None;
                config.coder_admin_password = None;
            }
            1 => {
                config.workspace_provider = WorkspaceProvider::Coder;
                config.coder_url = Some(self.coder_url.clone());
                config.coder_admin_password = Some(self.admin_password.clone());
            }
            _ => {}
        }

        Ok(())
    }
}
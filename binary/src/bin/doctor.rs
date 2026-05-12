use agentflow_tui::app::run_app;
use agentflow_tui::app::{App, AppMode};
use agentflow_tui::restore_tui;
use anyhow::Result;

#[tokio::main]
async fn main() -> Result<()> {
    let mut app = App::new(AppMode::Doctor);
    let result = run_app(&mut app).await;
    restore_tui();
    result
}

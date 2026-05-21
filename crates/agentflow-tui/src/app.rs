use anyhow::Result;

#[derive(Debug, Clone, PartialEq)]
pub enum AppMode {
    Setup,
    Dashboard,
    Doctor,
}

pub struct App {
    pub mode: AppMode,
    pub running: bool,
    pub current_step: usize,
    pub total_steps: usize,
}

impl App {
    pub fn new(mode: AppMode) -> Self {
        Self {
            mode,
            running: true,
            current_step: 0,
            total_steps: 1,
        }
    }

    pub fn quit(&mut self) {
        self.running = false;
    }

    pub fn next_step(&mut self) {
        if self.current_step < self.total_steps.saturating_sub(1) {
            self.current_step += 1;
        }
    }

    pub fn prev_step(&mut self) {
        if self.current_step > 0 {
            self.current_step -= 1;
        }
    }
}

pub async fn run_app(app: &mut App) -> Result<()> {
    match app.mode {
        AppMode::Setup => crate::setup::run_wizard(app).await,
        AppMode::Dashboard => crate::dashboard::run_dashboard(app).await,
        AppMode::Doctor => crate::doctor::run_doctor(app).await,
    }
}

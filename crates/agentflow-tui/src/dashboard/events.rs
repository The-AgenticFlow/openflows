#[derive(Debug, Clone)]
pub struct LogEvent {
    pub timestamp: String,
    pub agent: String,
    pub message: String,
}

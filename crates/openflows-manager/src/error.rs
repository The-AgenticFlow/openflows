use thiserror::Error;

#[derive(Debug, Error)]
pub enum ManagerError {
    #[error("configuration error: {0}")]
    Config(String),

    #[error(transparent)]
    Io(#[from] std::io::Error),

    #[error(transparent)]
    Service(#[from] anyhow::Error),
}

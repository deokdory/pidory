use thiserror::Error;

#[derive(Debug, Error)]
pub enum PidoryError {
    #[error("Config error: {0}")]
    Config(String),

    #[error("Database error: {0}")]
    Db(#[from] sqlx::Error),

    #[error("Discord error: {0}")]
    Discord(Box<serenity::Error>),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("Subprocess error: {0}")]
    Subprocess(String),

    #[error("Not found: {0}")]
    NotFound(String),
}

impl From<serenity::Error> for PidoryError {
    fn from(e: serenity::Error) -> Self {
        Self::Discord(Box::new(e))
    }
}

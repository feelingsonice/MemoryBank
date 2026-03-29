use thiserror::Error;

#[derive(Error, Debug)]
pub enum AppError {
    #[error("HTTP client error: {0}")]
    HttpClient(String),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("JSON parse error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("Normalization error: {0}")]
    Normalize(String),

    #[error("Logging initialization error: {0}")]
    Logging(#[from] tracing_subscriber::util::TryInitError),
}

use thiserror::Error;

#[derive(Error, Debug)]
pub enum AppError {
    #[error("Database error: {0}")]
    Database(#[from] sqlx::Error),

    #[error("LLM error: {0}")]
    Llm(#[from] LlmError),

    #[error("Encoder error: {0}")]
    Encoder(#[from] EncoderError),

    #[error("Configuration error: {0}")]
    Config(String),

    #[error("Schema error: {0}")]
    Schema(String),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("HTTP server error: {0}")]
    HttpServer(String),

    #[error("Logging initialization error: {0}")]
    Logging(#[from] tracing_subscriber::util::TryInitError),
}

#[derive(Error, Debug, Clone)]
pub enum LlmError {
    #[error("API request failed: {0}")]
    Api(String),
    #[error("Client initialization failed: {0}")]
    Init(String),
}

#[derive(Error, Debug)]
pub enum EncoderError {
    #[error("Encoder initialization failed: {0}")]
    Init(String),

    #[error("Encoding failed: {0}")]
    Encode(String),
}

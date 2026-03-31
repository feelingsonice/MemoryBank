use inquire::InquireError;
use memory_bank_app::AppConfigError;
use std::io;
use std::time::Duration;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum AppError {
    #[error(transparent)]
    AppConfig(#[from] AppConfigError),
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error("prompt failed: {0}")]
    Prompt(#[from] InquireError),
    #[error("setup was canceled")]
    SetupCanceled,
    #[error("unsupported platform '{0}'")]
    UnsupportedPlatform(String),
    #[error("command `{0}` failed: {1}")]
    CommandFailed(String, String),
    #[error("command `{0}` timed out after {1:?}")]
    CommandTimedOut(String, Duration),
    #[error("required binary `{0}` was not found")]
    MissingBinary(String),
    #[error("missing required provider secret `{0}` in ~/.memory_bank/secrets.env")]
    MissingProviderSecret(&'static str),
    #[error("health check failed: {0}")]
    Health(String),
    #[error("invalid config key `{0}`")]
    InvalidConfigKey(String),
    #[error("invalid config value for `{0}`: {1}")]
    InvalidConfigValue(String, String),
    #[error("{0}")]
    Message(String),
}

use tracing_subscriber::EnvFilter;
use tracing_subscriber::prelude::*;

use crate::error::AppError;

pub fn initialize() -> Result<(), AppError> {
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::fmt::layer()
                .compact()
                .with_writer(std::io::stderr),
        )
        .with(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")))
        .try_init()?;

    Ok(())
}

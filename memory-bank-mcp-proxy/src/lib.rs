mod cli;
mod error;
mod proxy;

pub use cli::ProxyArgs;
pub use error::AppError;

use memory_bank_app::{AppPaths, AppSettings, default_server_url};
use proxy::{ProxyServer, UpstreamClient};
use tracing::info;

pub async fn run(args: ProxyArgs) -> Result<(), AppError> {
    initialize_logging();
    let server_url = resolve_server_url(args.server_url.as_deref())?;

    let upstream = UpstreamClient::connect(&server_url).await?;
    let proxy = ProxyServer::new(upstream);

    info!(
        server_url = %server_url,
        "Starting Memory Bank MCP stdio proxy",
    );

    proxy.run_stdio().await
}

fn initialize_logging() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .try_init();
}

fn resolve_server_url(cli_value: Option<&str>) -> Result<String, AppError> {
    if let Some(server_url) = cli_value {
        return Ok(server_url.to_string());
    }

    let paths = AppPaths::from_system().map_err(|error| {
        AppError::UpstreamConnect(
            "app-config".to_string(),
            format!("failed to load app paths: {error}"),
        )
    })?;
    let settings = AppSettings::load(&paths).map_err(|error| {
        AppError::UpstreamConnect(
            "app-config".to_string(),
            format!("failed to load app settings: {error}"),
        )
    })?;
    Ok(default_server_url(&settings))
}

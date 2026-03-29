mod cli;
mod error;
mod proxy;

pub use cli::ProxyArgs;
pub use error::AppError;

use proxy::{ProxyServer, UpstreamClient};
use tracing::info;

pub async fn run(args: ProxyArgs) -> Result<(), AppError> {
    initialize_logging();

    let upstream = UpstreamClient::connect(&args.server_url).await?;
    let proxy = ProxyServer::new(upstream);

    info!(
        server_url = %args.server_url,
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

mod cli;
mod client;
mod error;
mod logging;
mod normalize;

use std::io::Read;

use cli::HookArgs;
use memory_bank_app::{AppPaths, AppSettings, default_server_url};
use tracing::info;

fn main() -> Result<(), error::AppError> {
    logging::initialize()?;

    let config = HookArgs::parse();
    let server_url = resolve_server_url(config.server_url.as_deref())?;

    let mut raw = Vec::new();
    std::io::stdin().lock().read_to_end(&mut raw)?;

    info!(
        agent = %config.agent,
        event = %config.event,
        input_bytes = raw.len(),
        server_url = %server_url,
        "Parsing payload for Memory Bank hook",
    );

    let payload = normalize::EnvelopeParser::new(&config.agent, &config.event).parse(&raw)?;
    let Some(payload) = payload else {
        info!(
            agent = %config.agent,
            event = %config.event,
            input_bytes = raw.len(),
            "Hook payload did not produce a memory fragment; skipping delivery",
        );
        return Ok(());
    };

    info!(
        agent = %config.agent,
        event = %config.event,
        conversation_id = %payload.scope.conversation_id,
        fragment_id = %payload.scope.fragment_id,
        terminality = %payload.fragment.terminality.as_str(),
        server_url = %server_url,
        "Sending normalized hook fragment to Memory Bank server",
    );
    client::post_ingest(&server_url, &payload)?;

    info!(
        agent = %config.agent,
        event = %config.event,
        conversation_id = %payload.scope.conversation_id,
        fragment_id = %payload.scope.fragment_id,
        server_url = %server_url,
        "Delivered hook fragment to Memory Bank server",
    );

    Ok(())
}

fn resolve_server_url(cli_value: Option<&str>) -> Result<String, error::AppError> {
    if let Some(server_url) = cli_value {
        return Ok(server_url.to_string());
    }

    let paths =
        AppPaths::from_system().map_err(|error| error::AppError::HttpClient(error.to_string()))?;
    let settings = AppSettings::load(&paths)
        .map_err(|error| error::AppError::HttpClient(error.to_string()))?;
    Ok(default_server_url(&settings))
}

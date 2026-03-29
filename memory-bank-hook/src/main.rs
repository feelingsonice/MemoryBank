mod cli;
mod client;
mod error;
mod logging;
mod normalize;

use std::io::Read;

use cli::HookArgs;
use tracing::info;

fn main() -> Result<(), error::AppError> {
    logging::initialize()?;

    let config = HookArgs::parse();

    let mut raw = Vec::new();
    std::io::stdin().lock().read_to_end(&mut raw)?;

    info!(
        agent = %config.agent,
        event = %config.event,
        input_bytes = raw.len(),
        server_url = %config.server_url,
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
        server_url = %config.server_url,
        "Sending normalized hook fragment to Memory Bank server",
    );
    client::post_ingest(&config.server_url, &payload)?;

    info!(
        agent = %config.agent,
        event = %config.event,
        conversation_id = %payload.scope.conversation_id,
        fragment_id = %payload.scope.fragment_id,
        server_url = %config.server_url,
        "Delivered hook fragment to Memory Bank server",
    );

    Ok(())
}

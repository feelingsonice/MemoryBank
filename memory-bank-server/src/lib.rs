mod actor;
pub mod config;
mod db;
mod encoder;
pub mod error;
mod http_server;
mod ingest;
mod llm;
mod logging;
mod mcp_server;
mod memory_window;
#[cfg(test)]
mod ollama_eval;
#[cfg(test)]
mod retrieval_eval;
mod startup_state;

use crate::actor::MemoryActor;
use crate::config::ServeConfig;
use crate::http_server::{HealthResponse, HttpServer};
use crate::ingest::IngestService;
use crate::startup_state::StartupStateTracker;
use tracing::info;

pub async fn run(config: ServeConfig) -> Result<(), error::AppError> {
    let ServeConfig {
        port,
        namespace,
        llm,
        encoder,
        history_window_size,
        nearest_neighbor_count,
        dirs,
    } = config;

    let logging_state = logging::Logging::initialize()?;

    info!(
        port,
        namespace = %namespace,
        llm_provider = %llm,
        encoder_provider = %encoder,
        history_window_size,
        nearest_neighbor_count,
        data_dir = %dirs.data.display(),
        db_path = %dirs.db.display(),
        models_dir = %dirs.models.display(),
        "Starting Memory Bank server",
    );

    info!(
        encoder_provider = %encoder,
        models_dir = %dirs.models.display(),
        "Initializing encoder provider",
    );
    let encoder = encoder::initialize(encoder, &dirs.models)?;
    info!(encoder_model = %encoder.model_id, "Encoder provider ready");

    info!(llm_provider = %llm, "Initializing LLM provider");
    let llm = llm::initialize(llm)?;
    info!(llm_model = %llm.model_id, "LLM provider ready");

    info!(
        db_path = %dirs.db.display(),
        llm_model = %llm.model_id,
        encoder_model = %encoder.model_id,
        "Opening memory database",
    );
    let startup_state = StartupStateTracker::new(dirs.startup_state.clone(), namespace.to_string());
    let db = db::MemoryDb::open(
        &dirs.db,
        &llm.model_id,
        &encoder.model_id,
        &encoder.client,
        Some(&startup_state),
    )
    .await?;

    let llm_provider_name = llm.provider_name().to_string();
    let encoder_provider_name = encoder.provider_name().to_string();

    info!("Starting background memory actor");
    let (memory_handle, _memory_task) =
        MemoryActor::spawn(db, llm.client, encoder.client, nearest_neighbor_count);

    info!(db_path = %dirs.db.display(), "Opening durable ingest service");
    let ingest = IngestService::open(&dirs.db, memory_handle.clone(), history_window_size).await?;

    let health = HealthResponse {
        ok: true,
        namespace: namespace.to_string(),
        port,
        llm_provider: llm_provider_name,
        encoder_provider: encoder_provider_name,
        version: env!("CARGO_PKG_VERSION"),
    };

    let server =
        HttpServer::bind(port, health, memory_handle, ingest, logging_state.sender()).await?;
    server.run().await
}

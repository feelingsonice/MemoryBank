use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::{Executor, QueryBuilder, Sqlite, SqlitePool, Transaction};
use std::ffi::c_char;
use std::path::Path;
use std::str::FromStr;
use std::sync::Once;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tracing::{debug, error, info, warn};

use crate::encoder::{EmbeddingInput, EncoderClient, embedding_to_bytes, validate_embedding_count};
use crate::error::AppError;
use crate::startup_state::StartupStateTracker;

// --- Public helpers and row types ---

pub fn parse_json_vec(json: &str) -> Vec<String> {
    serde_json::from_str(json).unwrap_or_default()
}

/// Row returned by neighbor (KNN) queries — carries the id for graph operations.
#[derive(Debug, sqlx::FromRow)]
pub struct NeighborRow {
    pub id: i64,
    pub content: String,
    pub context: String,
    pub keywords: String,
    pub tags: String,
}

/// Row returned by retrieval queries — carries the timestamp for the MCP response.
#[derive(Debug, sqlx::FromRow)]
pub struct RetrievalRow {
    pub id: i64,
    pub content: String,
    pub timestamp: String,
    pub context: String,
    pub keywords: String,
    pub tags: String,
}

// --- Public database API ---

#[derive(Clone)]
pub(crate) struct SqliteRuntime {
    pool: SqlitePool,
    write_gate: std::sync::Arc<Semaphore>,
    #[cfg(test)]
    write_attempt_tx:
        std::sync::Arc<std::sync::Mutex<Option<tokio::sync::mpsc::UnboundedSender<()>>>>,
}

impl SqliteRuntime {
    pub(crate) async fn open(db_path: &Path) -> Result<Self, AppError> {
        register_sqlite_vec();

        let pool = create_pool(db_path, 2).await?;
        Ok(Self {
            pool,
            write_gate: std::sync::Arc::new(Semaphore::new(1)),
            #[cfg(test)]
            write_attempt_tx: std::sync::Arc::new(std::sync::Mutex::new(None)),
        })
    }

    pub(crate) fn pool(&self) -> &SqlitePool {
        &self.pool
    }

    pub(crate) async fn acquire_write_permit(&self) -> OwnedSemaphorePermit {
        #[cfg(test)]
        if let Some(write_attempt_tx) = self
            .write_attempt_tx
            .lock()
            .expect("lock write attempt notifier")
            .as_ref()
        {
            let _ = write_attempt_tx.send(());
        }
        self.write_gate
            .clone()
            .acquire_owned()
            .await
            .expect("SQLite write gate unexpectedly closed")
    }

    #[cfg(test)]
    pub(crate) async fn open_in_memory_for_tests(max_connections: u32) -> Result<Self, AppError> {
        register_sqlite_vec();

        let options =
            SqliteConnectOptions::from_str("sqlite::memory:")?.pragma("foreign_keys", "ON");
        let pool = SqlitePoolOptions::new()
            .max_connections(max_connections)
            .connect_with(options)
            .await?;

        Ok(Self {
            pool,
            write_gate: std::sync::Arc::new(Semaphore::new(1)),
            #[cfg(test)]
            write_attempt_tx: std::sync::Arc::new(std::sync::Mutex::new(None)),
        })
    }

    #[cfg(test)]
    pub(crate) async fn open_file_for_tests(
        db_path: &Path,
        max_connections: u32,
    ) -> Result<Self, AppError> {
        register_sqlite_vec();

        let pool = create_pool(db_path, max_connections).await?;
        Ok(Self {
            pool,
            write_gate: std::sync::Arc::new(Semaphore::new(1)),
            #[cfg(test)]
            write_attempt_tx: std::sync::Arc::new(std::sync::Mutex::new(None)),
        })
    }

    #[cfg(test)]
    pub(crate) fn install_write_attempt_notifier(
        &self,
    ) -> tokio::sync::mpsc::UnboundedReceiver<()> {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        *self
            .write_attempt_tx
            .lock()
            .expect("lock write attempt notifier") = Some(tx);
        rx
    }
}

pub struct MemoryDb {
    runtime: SqliteRuntime,
}

impl MemoryDb {
    #[allow(dead_code)]
    pub async fn open(
        db_path: &Path,
        llm_model_id: &str,
        encoder_model_id: &str,
        encoder: &EncoderClient,
        startup_state: Option<&StartupStateTracker>,
    ) -> Result<Self, AppError> {
        let runtime = SqliteRuntime::open(db_path).await?;
        Self::open_with_runtime(
            runtime,
            db_path,
            llm_model_id,
            encoder_model_id,
            encoder,
            startup_state,
        )
        .await
    }

    pub(crate) async fn open_with_runtime(
        runtime: SqliteRuntime,
        db_path: &Path,
        llm_model_id: &str,
        encoder_model_id: &str,
        encoder: &EncoderClient,
        startup_state: Option<&StartupStateTracker>,
    ) -> Result<Self, AppError> {
        let pool = runtime.pool();
        create_schema(pool).await?;
        ensure_vec_table(pool, llm_model_id, encoder_model_id, encoder, startup_state).await?;

        info!(
            db_path = %db_path.display(),
            llm_model = %llm_model_id,
            encoder_model = %encoder_model_id,
            "Memory database ready"
        );
        Ok(Self { runtime })
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub async fn begin(&self) -> Result<Transaction<'_, Sqlite>, sqlx::Error> {
        self.runtime.pool().begin().await
    }

    pub async fn begin_immediate(&self) -> Result<Transaction<'_, Sqlite>, sqlx::Error> {
        self.runtime.pool().begin_with("BEGIN IMMEDIATE").await
    }

    pub(crate) async fn acquire_write_permit(&self) -> OwnedSemaphorePermit {
        self.runtime.acquire_write_permit().await
    }

    // --- Vector search ---

    pub async fn find_nearest_neighbors(
        &self,
        embedding: &[u8],
        k: i32,
    ) -> Result<Vec<i64>, sqlx::Error> {
        sqlx::query_scalar(&nearest_neighbor_ids_query(k))
            .bind(embedding)
            .fetch_all(self.runtime.pool())
            .await
    }

    pub async fn find_nearest_memories(
        &self,
        embedding: &[u8],
        k: i32,
    ) -> Result<Vec<NeighborRow>, sqlx::Error> {
        sqlx::query_as::<_, NeighborRow>(&nearest_memories_query(k))
            .bind(embedding)
            .fetch_all(self.runtime.pool())
            .await
    }

    // --- Read operations ---

    pub async fn get_memories(&self, ids: &[i64]) -> Result<Vec<RetrievalRow>, sqlx::Error> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }

        let mut builder = QueryBuilder::<Sqlite>::new(
            "SELECT id, content, timestamp, context, keywords, tags
             FROM memories
             WHERE id IN (",
        );
        push_bound_ids(&mut builder, ids);
        builder.push(")");

        builder
            .build_query_as::<RetrievalRow>()
            .fetch_all(self.runtime.pool())
            .await
    }

    pub async fn get_links_for_ids(&self, ids: &[i64]) -> Result<Vec<(i64, i64)>, sqlx::Error> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }

        let mut builder = QueryBuilder::<Sqlite>::new(
            "SELECT source_id, target_id FROM memory_links WHERE source_id IN (",
        );
        push_bound_ids(&mut builder, ids);
        builder.push(") ORDER BY source_id, target_id");

        builder
            .build_query_as::<(i64, i64)>()
            .fetch_all(self.runtime.pool())
            .await
    }

    // --- Write operations ---

    pub async fn insert_memory<'e>(
        &self,
        executor: impl Executor<'e, Database = Sqlite>,
        content: &str,
        timestamp: &str,
        context: &str,
        keywords: &str,
        tags: &str,
    ) -> Result<i64, sqlx::Error> {
        let result = sqlx::query(INSERT_MEMORY_SQL)
            .bind(content)
            .bind(timestamp)
            .bind(context)
            .bind(keywords)
            .bind(tags)
            .execute(executor)
            .await?;

        Ok(result.last_insert_rowid())
    }

    pub async fn insert_embedding<'e>(
        &self,
        executor: impl Executor<'e, Database = Sqlite>,
        id: i64,
        embedding: &[u8],
    ) -> Result<(), sqlx::Error> {
        sqlx::query(INSERT_EMBEDDING_SQL)
            .bind(id)
            .bind(embedding)
            .execute(executor)
            .await?;
        Ok(())
    }

    pub async fn update_embedding<'e>(
        &self,
        executor: impl Executor<'e, Database = Sqlite>,
        id: i64,
        embedding: &[u8],
    ) -> Result<(), sqlx::Error> {
        let result = sqlx::query(UPDATE_EMBEDDING_SQL)
            .bind(embedding)
            .bind(id)
            .execute(executor)
            .await?;
        if result.rows_affected() != 1 {
            return Err(sqlx::Error::RowNotFound);
        }
        Ok(())
    }

    /// Insert both directions of a link. Requires a transaction reference
    /// since it executes two statements.
    pub async fn insert_bidirectional_links(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
        source_id: i64,
        target_id: i64,
    ) -> Result<(), sqlx::Error> {
        insert_link(&mut **tx, source_id, target_id).await?;
        insert_link(&mut **tx, target_id, source_id).await
    }

    pub async fn update_memory_metadata<'e>(
        &self,
        executor: impl Executor<'e, Database = Sqlite>,
        id: i64,
        context: &str,
        keywords: &str,
        tags: &str,
    ) -> Result<(), sqlx::Error> {
        let result = sqlx::query(UPDATE_MEMORY_METADATA_SQL)
            .bind(context)
            .bind(keywords)
            .bind(tags)
            .bind(id)
            .execute(executor)
            .await?;
        if result.rows_affected() != 1 {
            return Err(sqlx::Error::RowNotFound);
        }
        Ok(())
    }

    pub async fn mark_ingest_turn_stored<'e>(
        &self,
        executor: impl Executor<'e, Database = Sqlite>,
        turn_id: i64,
        stored_at: &str,
    ) -> Result<bool, sqlx::Error> {
        let result = sqlx::query(MARK_INGEST_TURN_STORED_SQL)
            .bind(stored_at)
            .bind(stored_at)
            .bind(turn_id)
            .execute(executor)
            .await?;
        Ok(result.rows_affected() == 1)
    }

    #[cfg(test)]
    pub(crate) async fn open_in_memory_for_tests(dimension: usize) -> Result<Self, AppError> {
        let runtime = SqliteRuntime::open_in_memory_for_tests(1).await?;
        Self::open_with_runtime_for_tests(runtime, dimension).await
    }

    #[cfg(test)]
    pub(crate) async fn open_with_runtime_for_tests(
        runtime: SqliteRuntime,
        dimension: usize,
    ) -> Result<Self, AppError> {
        create_schema(runtime.pool()).await?;
        create_vec_table(runtime.pool(), dimension).await?;
        Ok(Self { runtime })
    }

    #[cfg(test)]
    pub(crate) fn pool_for_tests(&self) -> &SqlitePool {
        self.runtime.pool()
    }
}

// --- Bootstrap flow used by `MemoryDb::open` ---

fn register_sqlite_vec() {
    static SQLITE_VEC_REGISTRATION: Once = Once::new();

    SQLITE_VEC_REGISTRATION.call_once(|| {
        info!("Registering sqlite-vec SQLite extension");

        // SAFETY: sqlite3_auto_extension expects a C-compatible initialization
        // function pointer. sqlite-vec exposes that symbol, but Rust cannot
        // express the exact type directly from the crate API, so we cast it once
        // here and register it for the lifetime of the process.
        let result = unsafe { libsqlite3_sys::sqlite3_auto_extension(Some(sqlite_vec_init_fn())) };

        if result != libsqlite3_sys::SQLITE_OK {
            error!(code = result, "Failed to register sqlite-vec extension");
        }
    });
}

fn sqlite_vec_init_fn() -> SqliteVecInitFn {
    // SAFETY: sqlite_vec::sqlite3_vec_init is the sqlite extension entrypoint
    // exported by sqlite-vec. The cast only narrows the raw symbol pointer to the
    // callback signature required by sqlite3_auto_extension.
    unsafe { std::mem::transmute(sqlite_vec::sqlite3_vec_init as *const ()) }
}

async fn create_pool(db_path: &Path, max_connections: u32) -> Result<SqlitePool, AppError> {
    let connection_string = format!("sqlite://{}?mode=rwc", db_path.to_string_lossy());
    debug!(
        db_path = %db_path.display(),
        connection_string = %connection_string,
        "Connecting to SQLite database"
    );

    let options = SqliteConnectOptions::from_str(&connection_string)?
        .create_if_missing(true)
        .pragma("journal_mode", "WAL")
        .pragma("synchronous", "NORMAL")
        .pragma("busy_timeout", SQLITE_BUSY_TIMEOUT_MS)
        .pragma("foreign_keys", "ON")
        .pragma("mmap_size", SQLITE_MMAP_SIZE_BYTES);

    Ok(SqlitePoolOptions::new()
        .max_connections(max_connections)
        .connect_with(options)
        .await?)
}

async fn create_schema(pool: &SqlitePool) -> Result<(), AppError> {
    info!("Ensuring core SQLite schema exists");

    for (table_name, statement) in [
        ("memories", CREATE_MEMORIES_TABLE_SQL),
        ("memory_links", CREATE_MEMORY_LINKS_TABLE_SQL),
        ("model_config", CREATE_MODEL_CONFIG_TABLE_SQL),
    ] {
        sqlx::query(statement).execute(pool).await?;
        debug!(table = table_name, "Verified SQLite table");
    }

    Ok(())
}

async fn ensure_vec_table(
    pool: &SqlitePool,
    llm_model_id: &str,
    encoder_model_id: &str,
    encoder: &EncoderClient,
    startup_state: Option<&StartupStateTracker>,
) -> Result<(), AppError> {
    let plan = VecTablePlan::build(pool, llm_model_id, encoder_model_id, encoder).await?;

    match plan.mode {
        VecTableMode::Rebuild => {
            rebuild_vec_table(
                pool,
                plan.vec_table_exists,
                plan.dimension,
                encoder,
                startup_state,
            )
            .await?;
        }
        VecTableMode::EnsureExists => {
            create_vec_table(pool, plan.dimension).await?;
            debug!(
                "Verified 'vec_memories' virtual table (dimension: {}).",
                plan.dimension
            );
        }
    }

    upsert_model_config(pool, llm_model_id, encoder_model_id).await?;

    Ok(())
}

// --- Vector table workflow ---

impl VecTablePlan {
    async fn build(
        pool: &SqlitePool,
        llm_model_id: &str,
        encoder_model_id: &str,
        encoder: &EncoderClient,
    ) -> Result<Self, AppError> {
        let previous_config = load_model_config(pool).await?;
        let vec_table_exists = vec_table_exists(pool).await?;
        let dimension = determine_encoder_dimension(encoder).await?;
        let mode = determine_vec_table_mode(
            previous_config.as_ref(),
            llm_model_id,
            encoder_model_id,
            vec_table_exists,
        );

        Ok(Self {
            mode,
            vec_table_exists,
            dimension,
        })
    }
}

fn determine_vec_table_mode(
    previous_config: Option<&ModelConfigRow>,
    llm_model_id: &str,
    encoder_model_id: &str,
    vec_table_exists: bool,
) -> VecTableMode {
    if !vec_table_exists {
        match previous_config {
            Some(previous) => warn!(
                encoder_model = %previous.encoder_model,
                "Vector index is missing; rebuilding embeddings to recover it"
            ),
            None => info!("Initializing vector index for a new memory store"),
        }
        return VecTableMode::Rebuild;
    }

    match previous_config {
        Some(previous) => {
            if previous.llm_model != llm_model_id {
                warn!(
                    previous_llm_model = %previous.llm_model,
                    llm_model = %llm_model_id,
                    "LLM model changed; future memory extraction may differ from existing memories"
                );
            }

            if previous.encoder_model != encoder_model_id {
                warn!(
                    previous_encoder_model = %previous.encoder_model,
                    encoder_model = %encoder_model_id,
                    "Encoder model changed; rebuilding vector index"
                );
                VecTableMode::Rebuild
            } else {
                VecTableMode::EnsureExists
            }
        }
        None => {
            info!("Initializing vector index for a new memory store");
            VecTableMode::Rebuild
        }
    }
}

async fn load_model_config(pool: &SqlitePool) -> Result<Option<ModelConfigRow>, AppError> {
    Ok(sqlx::query_as::<_, ModelConfigRow>(SELECT_MODEL_CONFIG_SQL)
        .fetch_optional(pool)
        .await?)
}

async fn vec_table_exists(pool: &SqlitePool) -> Result<bool, AppError> {
    let table = sqlx::query_scalar::<_, String>(SELECT_VEC_TABLE_SQL)
        .fetch_optional(pool)
        .await?;
    Ok(table.is_some())
}

async fn determine_encoder_dimension(encoder: &EncoderClient) -> Result<usize, AppError> {
    let embeddings = encoder
        .encode(vec![DIMENSION_PROBE_INPUT.to_owned()])
        .await
        .map_err(AppError::Encoder)?;
    let dimension = probe_encoder_dimension(&embeddings)?;
    debug!(dimension, "Detected encoder output dimension");

    Ok(dimension)
}

fn probe_encoder_dimension(embeddings: &[Vec<f32>]) -> Result<usize, AppError> {
    validate_embedding_count(embeddings.len(), 1, "dimension probe").map_err(AppError::Encoder)?;
    Ok(embeddings[0].len())
}

async fn rebuild_vec_table(
    pool: &SqlitePool,
    vec_table_exists: bool,
    dimension: usize,
    encoder: &EncoderClient,
    startup_state: Option<&StartupStateTracker>,
) -> Result<(), AppError> {
    if vec_table_exists {
        info!(dimension, "Dropping stale vector index before rebuild");
        sqlx::query(DROP_VEC_TABLE_SQL).execute(pool).await?;
    }

    create_vec_table(pool, dimension).await?;
    migrate_embeddings(pool, encoder, startup_state).await?;

    Ok(())
}

async fn create_vec_table(pool: &SqlitePool, dimension: usize) -> Result<(), AppError> {
    let sql = format!(
        "CREATE VIRTUAL TABLE IF NOT EXISTS vec_memories USING vec0(
            id INTEGER PRIMARY KEY,
            embedding float[{}]
        );",
        dimension
    );
    sqlx::query(&sql).execute(pool).await?;
    debug!(dimension, "Ensured vec_memories virtual table");
    Ok(())
}

async fn migrate_embeddings(
    pool: &SqlitePool,
    encoder: &EncoderClient,
    startup_state: Option<&StartupStateTracker>,
) -> Result<(), AppError> {
    let memories = load_memories_for_migration(pool).await?;
    if memories.is_empty() {
        info!("Vector index ready with no existing memories to migrate");
        return Ok(());
    }

    let _reindex_guard = startup_state
        .map(|state| state.begin_reindex(memories.len()))
        .transpose()?;

    info!(
        memory_count = memories.len(),
        chunk_size = MIGRATION_CHUNK_SIZE,
        "Re-encoding stored memories for vector index migration"
    );

    for chunk in memories.chunks(MIGRATION_CHUNK_SIZE) {
        let (keywords_vecs, tags_vecs) = parse_migration_metadata(chunk);
        let payloads: Vec<_> = chunk
            .iter()
            .enumerate()
            .map(|(index, row)| row.embedding_input(&keywords_vecs[index], &tags_vecs[index]))
            .collect();

        match encoder.encode_memories(&payloads).await {
            Ok(embeddings) => {
                validate_embedding_count(embeddings.len(), chunk.len(), "embedding migration")
                    .map_err(AppError::Encoder)?;
                persist_migrated_embeddings(pool, chunk, embeddings).await?;
            }
            Err(err) => return Err(AppError::Encoder(err)),
        }
    }

    info!(
        memory_count = memories.len(),
        "Completed vector index migration"
    );
    Ok(())
}

async fn load_memories_for_migration(
    pool: &SqlitePool,
) -> Result<Vec<MigrationMemoryRow>, AppError> {
    Ok(
        sqlx::query_as::<_, MigrationMemoryRow>(SELECT_MEMORIES_FOR_MIGRATION_SQL)
            .fetch_all(pool)
            .await?,
    )
}

fn parse_migration_metadata(chunk: &[MigrationMemoryRow]) -> (Vec<Vec<String>>, Vec<Vec<String>>) {
    let keywords = chunk
        .iter()
        .map(|row| parse_json_vec(&row.keywords))
        .collect();
    let tags = chunk.iter().map(|row| parse_json_vec(&row.tags)).collect();
    (keywords, tags)
}

async fn persist_migrated_embeddings(
    pool: &SqlitePool,
    chunk: &[MigrationMemoryRow],
    embeddings: Vec<Vec<f32>>,
) -> Result<(), AppError> {
    let mut tx = pool.begin().await?;

    for (row, embedding) in chunk.iter().zip(embeddings) {
        sqlx::query(INSERT_EMBEDDING_SQL)
            .bind(row.id)
            .bind(embedding_to_bytes(&embedding))
            .execute(&mut *tx)
            .await?;
    }

    tx.commit().await?;
    Ok(())
}

async fn upsert_model_config(
    pool: &SqlitePool,
    llm_model_id: &str,
    encoder_model_id: &str,
) -> Result<(), AppError> {
    sqlx::query(UPSERT_MODEL_CONFIG_SQL)
        .bind(llm_model_id)
        .bind(encoder_model_id)
        .execute(pool)
        .await?;
    Ok(())
}

// --- Low-level query helpers ---

fn nearest_neighbor_ids_query(k: i32) -> String {
    format!(
        "SELECT id FROM vec_memories WHERE embedding MATCH ? AND k = {} ORDER BY distance",
        k
    )
}

fn nearest_memories_query(k: i32) -> String {
    format!(
        "SELECT m.id, m.content, m.context, m.keywords, m.tags
         FROM vec_memories v
         JOIN memories m ON m.id = v.id
         WHERE v.embedding MATCH ? AND k = {}
         ORDER BY distance",
        k
    )
}

fn push_bound_ids(builder: &mut QueryBuilder<'_, Sqlite>, ids: &[i64]) {
    let mut separated = builder.separated(", ");
    for &id in ids {
        separated.push_bind(id);
    }
}

async fn insert_link<'e>(
    executor: impl Executor<'e, Database = Sqlite>,
    source_id: i64,
    target_id: i64,
) -> Result<(), sqlx::Error> {
    sqlx::query(INSERT_LINK_SQL)
        .bind(source_id)
        .bind(target_id)
        .execute(executor)
        .await?;
    Ok(())
}

// --- Private supporting types ---

type SqliteVecInitFn = unsafe extern "C" fn(
    *mut libsqlite3_sys::sqlite3,
    *mut *mut c_char,
    *const libsqlite3_sys::sqlite3_api_routines,
) -> i32;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum VecTableMode {
    EnsureExists,
    Rebuild,
}

#[derive(Debug)]
struct VecTablePlan {
    mode: VecTableMode,
    vec_table_exists: bool,
    dimension: usize,
}

#[derive(Debug, sqlx::FromRow)]
struct ModelConfigRow {
    llm_model: String,
    encoder_model: String,
}

#[derive(Debug, sqlx::FromRow)]
struct MigrationMemoryRow {
    id: i64,
    content: String,
    context: String,
    keywords: String,
    tags: String,
}

impl MigrationMemoryRow {
    fn embedding_input<'a>(
        &'a self,
        keywords: &'a [String],
        tags: &'a [String],
    ) -> EmbeddingInput<'a> {
        EmbeddingInput {
            content: self.content.as_str(),
            keywords,
            tags,
            context: self.context.as_str(),
        }
    }
}

// --- SQL and tuning constants ---

const MIGRATION_CHUNK_SIZE: usize = 50;
pub(crate) const SQLITE_BUSY_TIMEOUT_MS: &str = "5000";
pub(crate) const SQLITE_MMAP_SIZE_BYTES: &str = "268435456";
const DIMENSION_PROBE_INPUT: &str = "test";

const INSERT_MEMORY_SQL: &str =
    "INSERT INTO memories (content, timestamp, context, keywords, tags) VALUES (?, ?, ?, ?, ?)";
const INSERT_EMBEDDING_SQL: &str = "INSERT INTO vec_memories (id, embedding) VALUES (?, ?)";
const UPDATE_EMBEDDING_SQL: &str = "UPDATE vec_memories SET embedding = ? WHERE id = ?";
const INSERT_LINK_SQL: &str =
    "INSERT OR IGNORE INTO memory_links (source_id, target_id) VALUES (?, ?)";
const UPDATE_MEMORY_METADATA_SQL: &str =
    "UPDATE memories SET context = ?, keywords = ?, tags = ? WHERE id = ?";
const MARK_INGEST_TURN_STORED_SQL: &str = "UPDATE ingest_turns
    SET status = 'stored',
        last_error = NULL,
        next_attempt_at = NULL,
        processing_started_at = NULL,
        stored_at = ?,
        updated_at = ?
    WHERE id = ? AND status = 'processing'";

const CREATE_MEMORIES_TABLE_SQL: &str = "CREATE TABLE IF NOT EXISTS memories (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    content TEXT NOT NULL,
    timestamp TEXT NOT NULL,
    context TEXT NOT NULL,
    keywords TEXT NOT NULL,
    tags TEXT NOT NULL
);";
const CREATE_MEMORY_LINKS_TABLE_SQL: &str = "CREATE TABLE IF NOT EXISTS memory_links (
    source_id INTEGER NOT NULL,
    target_id INTEGER NOT NULL,
    PRIMARY KEY (source_id, target_id),
    FOREIGN KEY (source_id) REFERENCES memories(id) ON DELETE CASCADE,
    FOREIGN KEY (target_id) REFERENCES memories(id) ON DELETE CASCADE
);";
const CREATE_MODEL_CONFIG_TABLE_SQL: &str = "CREATE TABLE IF NOT EXISTS model_config (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    llm_model TEXT NOT NULL,
    encoder_model TEXT NOT NULL
);";

const SELECT_MODEL_CONFIG_SQL: &str =
    "SELECT llm_model, encoder_model FROM model_config WHERE id = 1";
const SELECT_VEC_TABLE_SQL: &str =
    "SELECT name FROM sqlite_master WHERE type='table' AND name='vec_memories'";
const SELECT_MEMORIES_FOR_MIGRATION_SQL: &str =
    "SELECT id, content, context, keywords, tags FROM memories";

const DROP_VEC_TABLE_SQL: &str = "DROP TABLE IF EXISTS vec_memories;";
const UPSERT_MODEL_CONFIG_SQL: &str = "INSERT INTO model_config (id, llm_model, encoder_model)
    VALUES (1, ?, ?)
    ON CONFLICT(id) DO UPDATE
    SET llm_model = excluded.llm_model, encoder_model = excluded.encoder_model";

#[cfg(test)]
mod tests {
    use super::{
        CREATE_MODEL_CONFIG_TABLE_SQL, ModelConfigRow, VecTableMode, determine_vec_table_mode,
        load_model_config, parse_json_vec, probe_encoder_dimension, upsert_model_config,
    };
    use crate::error::{AppError, EncoderError};
    use sqlx::sqlite::SqlitePoolOptions;

    #[test]
    fn parse_json_vec_returns_empty_on_invalid_json() {
        assert!(parse_json_vec("not json").is_empty());
    }

    #[test]
    fn vec_table_is_rebuilt_without_previous_config() {
        assert_eq!(
            determine_vec_table_mode(None, "llm-a", "enc-a", false),
            VecTableMode::Rebuild
        );
    }

    #[test]
    fn vec_table_is_rebuilt_when_encoder_changes() {
        let previous = ModelConfigRow {
            llm_model: "llm-a".to_owned(),
            encoder_model: "enc-a".to_owned(),
        };

        assert_eq!(
            determine_vec_table_mode(Some(&previous), "llm-a", "enc-b", true),
            VecTableMode::Rebuild
        );
    }

    #[test]
    fn vec_table_is_reused_when_only_llm_changes() {
        let previous = ModelConfigRow {
            llm_model: "llm-a".to_owned(),
            encoder_model: "enc-a".to_owned(),
        };

        assert_eq!(
            determine_vec_table_mode(Some(&previous), "llm-b", "enc-a", true),
            VecTableMode::EnsureExists
        );
    }

    #[test]
    fn vec_table_is_rebuilt_when_index_is_missing() {
        let previous = ModelConfigRow {
            llm_model: "llm-a".to_owned(),
            encoder_model: "enc-a".to_owned(),
        };

        assert_eq!(
            determine_vec_table_mode(Some(&previous), "llm-a", "enc-a", false),
            VecTableMode::Rebuild
        );
    }

    #[test]
    fn probe_encoder_dimension_rejects_empty_probe_response() {
        let error = probe_encoder_dimension(&[]).expect_err("empty probe should fail");

        assert!(matches!(
            error,
            AppError::Encoder(EncoderError::Encode(message))
                if message == "dimension probe returned 0 embedding(s) for 1 input(s)"
        ));
    }

    #[tokio::test]
    async fn upsert_model_config_refreshes_stored_llm_metadata() {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .expect("connect in-memory sqlite");
        sqlx::query(CREATE_MODEL_CONFIG_TABLE_SQL)
            .execute(&pool)
            .await
            .expect("create model config table");

        upsert_model_config(&pool, "Anthropic::claude-sonnet-4-6", "FastEmbed::default")
            .await
            .expect("insert initial config");
        upsert_model_config(&pool, "OpenAi::gpt-5-mini", "FastEmbed::default")
            .await
            .expect("refresh llm config");

        let stored = load_model_config(&pool)
            .await
            .expect("load updated model config")
            .expect("stored row");

        assert_eq!(stored.llm_model, "OpenAi::gpt-5-mini");
        assert_eq!(stored.encoder_model, "FastEmbed::default");
    }
}

use chrono::{DateTime, Utc};
use memory_bank_protocol::MemoryNote;
use rmcp::ErrorData as McpError;
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::time::Duration;
use thiserror::Error;
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use tracing::{debug, info, warn};

use crate::db::{MemoryDb, NeighborRow, RetrievalRow, parse_json_vec};
use crate::encoder::{
    EmbeddingInput, EncoderClient, embedding_to_bytes, take_single_embedding,
    validate_embedding_count,
};
use crate::error::{EncoderError, LlmError};
use crate::llm::{ExtractedMemoryAnalysis, LlmClient, MemoryEvolution};
use crate::memory_window::{
    ProjectedConversationWindow, render_projection_history, render_projection_markdown,
};

enum MemoryTask {
    Add {
        turn_id: i64,
        window: ProjectedConversationWindow,
        timestamp: DateTime<Utc>,
        responder: oneshot::Sender<Result<(), ProcessTurnError>>,
    },
    Retrieve {
        query: String,
        responder: oneshot::Sender<Result<Vec<MemoryNote>, McpError>>,
    },
}

const MEMORY_CHANNEL_CAPACITY: usize = 100;

#[derive(Clone)]
pub struct MemoryHandle {
    tx: mpsc::Sender<MemoryTask>,
}

#[derive(Debug, Error)]
pub enum ProcessTurnError {
    #[error(transparent)]
    Retryable(#[from] RetryableProcessTurnError),
    #[error(transparent)]
    Unrecoverable(#[from] UnrecoverableProcessTurnError),
}

#[derive(Debug, Error)]
pub enum RetryableProcessTurnError {
    #[error("memory actor is not running")]
    ActorUnavailable,
    #[error("background memory task dropped the response channel")]
    ResponseDropped,
    #[error("LLM memory analysis timed out after {0}s")]
    MemoryAnalysisTimeout(u64),
    #[error("LLM memory analysis failed: {0}")]
    MemoryAnalysis(String),
    #[error("LLM graph evolution timed out after {0}s")]
    GraphEvolutionTimeout(u64),
    #[error("LLM graph evolution failed: {0}")]
    GraphEvolution(String),
    #[error("encoder error: {0}")]
    Encoder(String),
    #[error("database error: {0}")]
    Database(#[from] sqlx::Error),
    #[error("serialization error: {0}")]
    Serialization(String),
}

#[derive(Debug, Error)]
pub enum UnrecoverableProcessTurnError {
    #[error("turn processing invariant violated: {0}")]
    Invariant(String),
}

impl From<sqlx::Error> for ProcessTurnError {
    fn from(error: sqlx::Error) -> Self {
        RetryableProcessTurnError::Database(error).into()
    }
}

impl MemoryHandle {
    pub async fn store_window(
        &self,
        turn_id: i64,
        window: ProjectedConversationWindow,
        timestamp: DateTime<Utc>,
    ) -> Result<(), ProcessTurnError> {
        let (tx, rx) = oneshot::channel();

        self.tx
            .send(MemoryTask::Add {
                turn_id,
                window,
                timestamp,
                responder: tx,
            })
            .await
            .map_err(|_| RetryableProcessTurnError::ActorUnavailable)?;

        rx.await
            .map_err(|_| ProcessTurnError::from(RetryableProcessTurnError::ResponseDropped))?
    }

    pub async fn retrieve(&self, query: String) -> Result<Vec<MemoryNote>, McpError> {
        let (tx, rx) = oneshot::channel();

        self.tx
            .send(MemoryTask::Retrieve {
                query,
                responder: tx,
            })
            .await
            .map_err(|_| {
                McpError::internal_error("Failed to enqueue retrieval request".to_string(), None)
            })?;

        match rx.await {
            Ok(result) => result,
            Err(_) => Err(McpError::internal_error(
                "Background task dropped the response channel".to_string(),
                None,
            )),
        }
    }

    #[cfg(test)]
    pub(crate) fn closed_for_tests() -> Self {
        let (tx, rx) = mpsc::channel(1);
        drop(rx);
        Self { tx }
    }

    #[cfg(test)]
    pub(crate) async fn channel_for_tests(
        capacity: usize,
    ) -> (Self, mpsc::Receiver<TestStoreTurnRequest>) {
        let (task_tx, mut task_rx) = mpsc::channel(capacity);
        let (test_tx, test_rx) = mpsc::channel(capacity);
        tokio::spawn(async move {
            while let Some(task) = task_rx.recv().await {
                match task {
                    MemoryTask::Add {
                        turn_id,
                        window,
                        timestamp,
                        responder,
                    } => {
                        if test_tx
                            .send(TestStoreTurnRequest {
                                turn_id,
                                window,
                                timestamp,
                                responder,
                            })
                            .await
                            .is_err()
                        {
                            break;
                        }
                    }
                    MemoryTask::Retrieve { responder, .. } => {
                        let _ = responder.send(Err(McpError::internal_error(
                            "test store channel does not handle retrieval".to_string(),
                            None,
                        )));
                    }
                }
            }
        });
        (Self { tx: task_tx }, test_rx)
    }
}

#[cfg(test)]
pub(crate) struct TestStoreTurnRequest {
    pub turn_id: i64,
    pub window: ProjectedConversationWindow,
    pub timestamp: DateTime<Utc>,
    pub responder: oneshot::Sender<Result<(), ProcessTurnError>>,
}

// --- Internal types ---

#[derive(Debug, Serialize)]
struct NeighborContext {
    id: i64,
    content: String,
    context: String,
    keywords: Vec<String>,
    tags: Vec<String>,
}

impl From<NeighborRow> for NeighborContext {
    fn from(row: NeighborRow) -> Self {
        Self {
            id: row.id,
            content: row.content,
            context: row.context,
            keywords: parse_json_vec(&row.keywords),
            tags: parse_json_vec(&row.tags),
        }
    }
}

#[derive(Debug)]
struct NeighborUpdate {
    id: i64,
    content: String,
    context: String,
    keywords: Vec<String>,
    tags: Vec<String>,
}

#[derive(Debug)]
struct PreparedMemory {
    content: String,
    conversation_context: String,
    keywords: Vec<String>,
    tags: Vec<String>,
}

#[derive(Debug)]
struct PreparedTurnWrite {
    content: String,
    memory_timestamp: String,
    conversation_context: String,
    keywords_json: String,
    tags_json: String,
    embedding_bytes: Vec<u8>,
    link_ids: Vec<i64>,
    neighbor_updates: Vec<PreparedNeighborUpdate>,
}

trait MemoryLlmClient {
    async fn analyze_memory_window(
        &self,
        previous_turns: &str,
        current_turn: &str,
        timestamp: DateTime<Utc>,
    ) -> Result<ExtractedMemoryAnalysis, LlmError>;

    async fn generate_memory_evolution(
        &self,
        context: &str,
        content: &str,
        keywords: &[String],
        neighbors_json: &str,
    ) -> Result<MemoryEvolution, LlmError>;
}

pub(crate) trait MemoryEncoderClient {
    async fn encode(&self, texts: Vec<String>) -> Result<Vec<Vec<f32>>, EncoderError>;

    async fn encode_memory(&self, payload: &EmbeddingInput<'_>) -> Result<Vec<f32>, EncoderError>;

    async fn encode_memories(
        &self,
        payloads: &[EmbeddingInput<'_>],
    ) -> Result<Vec<Vec<f32>>, EncoderError>;
}

impl MemoryLlmClient for LlmClient {
    async fn analyze_memory_window(
        &self,
        previous_turns: &str,
        current_turn: &str,
        timestamp: DateTime<Utc>,
    ) -> Result<ExtractedMemoryAnalysis, LlmError> {
        self.analyze_memory_window(previous_turns, current_turn, timestamp)
            .await
    }

    async fn generate_memory_evolution(
        &self,
        context: &str,
        content: &str,
        keywords: &[String],
        neighbors_json: &str,
    ) -> Result<MemoryEvolution, LlmError> {
        self.generate_memory_evolution(context, content, keywords, neighbors_json)
            .await
    }
}

impl MemoryEncoderClient for EncoderClient {
    async fn encode(&self, texts: Vec<String>) -> Result<Vec<Vec<f32>>, EncoderError> {
        self.encode(texts).await
    }

    async fn encode_memory(&self, payload: &EmbeddingInput<'_>) -> Result<Vec<f32>, EncoderError> {
        self.encode_memory(payload).await
    }

    async fn encode_memories(
        &self,
        payloads: &[EmbeddingInput<'_>],
    ) -> Result<Vec<Vec<f32>>, EncoderError> {
        self.encode_memories(payloads).await
    }
}

impl From<RetrievalRow> for MemoryNote {
    fn from(row: RetrievalRow) -> Self {
        let timestamp = DateTime::parse_from_rfc3339(&row.timestamp)
            .map(|timestamp| timestamp.with_timezone(&Utc))
            .unwrap_or_else(|_| Utc::now());
        Self {
            content: row.content,
            timestamp,
            context: row.context,
            keywords: parse_json_vec(&row.keywords),
            tags: parse_json_vec(&row.tags),
        }
    }
}

#[cfg_attr(not(test), allow(dead_code))]
#[derive(Debug)]
pub(crate) struct RetrievalOutcome {
    pub notes: Vec<MemoryNote>,
    pub knn_ids: Vec<i64>,
    pub knn_matches: usize,
    pub expanded_matches: usize,
}

/// Timeout for individual LLM API calls (memory analysis and graph evolution).
const LLM_TIMEOUT: Duration = Duration::from_secs(60);

// --- MemoryActor ---

pub struct MemoryActor {
    rx: mpsc::Receiver<MemoryTask>,
    db: MemoryDb,
    llm: LlmClient,
    encoder: EncoderClient,
    nearest_neighbor_count: i32,
}

impl MemoryActor {
    fn new(
        rx: mpsc::Receiver<MemoryTask>,
        db: MemoryDb,
        llm: LlmClient,
        encoder: EncoderClient,
        nearest_neighbor_count: i32,
    ) -> Self {
        Self {
            rx,
            db,
            llm,
            encoder,
            nearest_neighbor_count,
        }
    }

    pub fn spawn(
        db: MemoryDb,
        llm: LlmClient,
        encoder: EncoderClient,
        nearest_neighbor_count: i32,
    ) -> (MemoryHandle, JoinHandle<()>) {
        let (tx, rx) = mpsc::channel(MEMORY_CHANNEL_CAPACITY);
        let actor = Self::new(rx, db, llm, encoder, nearest_neighbor_count);
        let handle = MemoryHandle { tx };
        let task = tokio::spawn(async move {
            actor.run().await;
        });

        (handle, task)
    }

    pub async fn run(mut self) {
        info!(
            channel_capacity = MEMORY_CHANNEL_CAPACITY,
            nearest_neighbor_count = self.nearest_neighbor_count,
            "Memory actor started",
        );
        while let Some(task) = self.rx.recv().await {
            match task {
                MemoryTask::Add {
                    turn_id,
                    window,
                    timestamp,
                    responder,
                } => {
                    let result = self.handle_add(turn_id, window, timestamp).await;
                    if let Err(error) = &result {
                        warn!(
                            turn_id,
                            error = %error,
                            "Failed to store finalized turn in memory graph"
                        );
                    }
                    if responder.send(result).is_err() {
                        warn!(
                            turn_id,
                            "Finished processing turn, but the caller dropped the response channel"
                        );
                    }
                }
                MemoryTask::Retrieve { query, responder } => {
                    self.handle_retrieve(query, responder).await;
                }
            }
        }
        info!("Memory actor stopped because its work queue closed");
    }

    #[tracing::instrument(skip(self, window))]
    async fn handle_add(
        &self,
        turn_id: i64,
        window: ProjectedConversationWindow,
        timestamp: DateTime<Utc>,
    ) -> Result<(), ProcessTurnError> {
        process_turn_with_clients(
            &self.db,
            &self.llm,
            &self.encoder,
            self.nearest_neighbor_count,
            turn_id,
            window,
            timestamp,
        )
        .await
    }

    #[tracing::instrument(skip(self, responder), fields(query_chars = query.chars().count()))]
    async fn handle_retrieve(
        &self,
        query: String,
        responder: oneshot::Sender<Result<Vec<MemoryNote>, McpError>>,
    ) {
        let result = self.retrieve(query).await;
        if responder.send(result).is_err() {
            warn!("Retrieve request completed, but the caller dropped the response channel");
        } else {
            debug!("Retrieve results sent to caller");
        }
    }

    async fn retrieve(&self, query: String) -> Result<Vec<MemoryNote>, McpError> {
        retrieve_with_encoder(&self.db, &self.encoder, self.nearest_neighbor_count, &query)
            .await
            .map(|outcome| outcome.notes)
    }
}

pub(crate) async fn retrieve_with_encoder<E>(
    db: &MemoryDb,
    encoder: &E,
    nearest_neighbor_count: i32,
    query: &str,
) -> Result<RetrievalOutcome, McpError>
where
    E: MemoryEncoderClient + ?Sized,
{
    debug!("Encoding retrieve_memory query");

    // 1. Encode the query text
    let mut embeddings = encoder
        .encode(vec![query.to_string()])
        .await
        .map_err(|e| McpError::internal_error(format!("Encoder error: {}", e), None))?;

    let embedding = take_single_embedding(std::mem::take(&mut embeddings), "query encoding")
        .map_err(|e| McpError::internal_error(format!("Encoder error: {}", e), None))?;

    let embedding_bytes = embedding_to_bytes(&embedding);

    // 2. KNN search for top K matches
    let top_ids = db
        .find_nearest_neighbors(&embedding_bytes, nearest_neighbor_count)
        .await
        .map_err(|e| McpError::internal_error(format!("DB query error: {}", e), None))?;

    if top_ids.is_empty() {
        info!(
            knn_matches = 0,
            returned = 0,
            "Memory retrieval produced no matches"
        );
        return Ok(RetrievalOutcome {
            notes: Vec::new(),
            knn_ids: Vec::new(),
            knn_matches: 0,
            expanded_matches: 0,
        });
    }

    // 3. Expand to 1-hop subgraph via link traversal
    let knn_count = top_ids.len();
    let links = db.get_links_for_ids(&top_ids).await.unwrap_or_default();
    let ordered_ids = ordered_retrieval_ids(&top_ids, &links);
    let expanded_count = ordered_ids.len();

    // 4. Fetch and return the semantic content
    db.get_memories(&ordered_ids)
        .await
        .map(|mut rows| {
            sort_retrieval_rows(&mut rows, &ordered_ids);
            let notes: Vec<MemoryNote> = rows.into_iter().map(MemoryNote::from).collect();
            info!(
                knn_matches = knn_count,
                expanded_matches = expanded_count,
                returned = notes.len(),
                "Memory retrieval completed",
            );
            RetrievalOutcome {
                notes,
                knn_ids: top_ids,
                knn_matches: knn_count,
                expanded_matches: expanded_count,
            }
        })
        .map_err(|e| McpError::internal_error(format!("DB error fetching memories: {}", e), None))
}

fn estimate_token_count(text: &str) -> usize {
    let chars = text.chars().count();
    if chars == 0 { 0 } else { chars.div_ceil(4) }
}

fn serialize_json<T: Serialize>(value: &T, label: &str) -> Result<String, ProcessTurnError> {
    serde_json::to_string(value).map_err(|error| {
        RetryableProcessTurnError::Serialization(format!("Failed to serialize {label}: {error}"))
            .into()
    })
}

fn ordered_retrieval_ids(top_ids: &[i64], links: &[(i64, i64)]) -> Vec<i64> {
    let mut ordered_ids = Vec::with_capacity(top_ids.len() + links.len());
    let mut seen = HashSet::with_capacity(top_ids.len() + links.len());
    let mut links_by_source: HashMap<i64, Vec<i64>> = HashMap::with_capacity(top_ids.len());

    for &id in top_ids {
        if seen.insert(id) {
            ordered_ids.push(id);
        }
    }

    for &(source_id, target_id) in links {
        links_by_source
            .entry(source_id)
            .or_default()
            .push(target_id);
    }

    for source_id in top_ids {
        if let Some(target_ids) = links_by_source.get(source_id) {
            for &target_id in target_ids {
                if seen.insert(target_id) {
                    ordered_ids.push(target_id);
                }
            }
        }
    }

    ordered_ids
}

fn sort_retrieval_rows(rows: &mut [RetrievalRow], ordered_ids: &[i64]) {
    let mut positions = HashMap::with_capacity(ordered_ids.len());
    for (index, &id) in ordered_ids.iter().enumerate() {
        positions.insert(id, index);
    }

    rows.sort_by_key(|row| positions.get(&row.id).copied().unwrap_or(usize::MAX));
}

async fn prepare_memory_for_storage<A>(
    analysis: &A,
    window: &ProjectedConversationWindow,
    timestamp: DateTime<Utc>,
) -> Result<PreparedMemory, ProcessTurnError>
where
    A: MemoryLlmClient + ?Sized,
{
    let content = render_projection_markdown(&window.current_turn);
    let previous_turns = if window.previous_turns.is_empty() {
        String::new()
    } else {
        render_projection_history(&window.previous_turns)
    };
    let history_chars = previous_turns.chars().count();
    let current_turn_chars = content.chars().count();
    let total_prompt_chars = history_chars + current_turn_chars;
    let estimated_prompt_tokens =
        estimate_token_count(&previous_turns) + estimate_token_count(&content);

    info!(
        history_turns = window.previous_turns.len(),
        history_chars,
        current_turn_chars,
        total_prompt_chars,
        estimated_prompt_tokens,
        "Prepared memory analysis prompt",
    );
    let analysis_result = tokio::time::timeout(
        LLM_TIMEOUT,
        analysis.analyze_memory_window(&previous_turns, &content, timestamp),
    )
    .await
    .map_err(|_| RetryableProcessTurnError::MemoryAnalysisTimeout(LLM_TIMEOUT.as_secs()))?
    .map_err(|error| RetryableProcessTurnError::MemoryAnalysis(error.to_string()))?;

    Ok(PreparedMemory {
        content,
        conversation_context: analysis_result.context,
        keywords: analysis_result.keywords,
        tags: analysis_result.tags,
    })
}

#[derive(Debug)]
struct PreparedNeighborUpdate {
    id: i64,
    context: String,
    keywords_json: String,
    tags_json: String,
    embedding_bytes: Vec<u8>,
}

async fn process_turn_with_clients<L, E>(
    db: &MemoryDb,
    llm: &L,
    encoder: &E,
    nearest_neighbor_count: i32,
    turn_id: i64,
    window: ProjectedConversationWindow,
    timestamp: DateTime<Utc>,
) -> Result<(), ProcessTurnError>
where
    L: MemoryLlmClient + ?Sized,
    E: MemoryEncoderClient + ?Sized,
{
    debug!(
        previous_turns = window.previous_turns.len(),
        "Processing finalized turn into memory graph"
    );

    let prepared_memory = prepare_memory_for_storage(llm, &window, timestamp).await?;

    info!(
        keyword_count = prepared_memory.keywords.len(),
        tag_count = prepared_memory.tags.len(),
        context_chars = prepared_memory.conversation_context.chars().count(),
        "Extracted candidate memory from finalized turn",
    );
    debug!(
        keywords = ?prepared_memory.keywords,
        tags = ?prepared_memory.tags,
        conversation_context = %prepared_memory.conversation_context,
        "Memory analysis output"
    );

    let payload = EmbeddingInput {
        content: &prepared_memory.content,
        keywords: &prepared_memory.keywords,
        tags: &prepared_memory.tags,
        context: &prepared_memory.conversation_context,
    };
    let embedding = encoder
        .encode_memory(&payload)
        .await
        .map_err(|error| RetryableProcessTurnError::Encoder(error.to_string()))?;

    let mut embedding_bytes = embedding_to_bytes(&embedding);

    let neighbor_rows = db
        .find_nearest_memories(&embedding_bytes, nearest_neighbor_count)
        .await?;

    let mut neighbor_ids = HashSet::with_capacity(neighbor_rows.len());
    let mut neighbor_contexts: Vec<NeighborContext> = Vec::with_capacity(neighbor_rows.len());
    for row in neighbor_rows {
        neighbor_ids.insert(row.id);
        neighbor_contexts.push(row.into());
    }
    info!(
        neighbor_count = neighbor_contexts.len(),
        "Loaded nearest memory neighbors for graph evolution"
    );
    let neighbors_json = if neighbor_contexts.is_empty() {
        String::new()
    } else {
        serialize_json(&neighbor_contexts, "neighbor contexts")?
    };

    let mut link_ids = Vec::new();
    let mut neighbor_updates = Vec::new();
    let mut refined_tags = prepared_memory.tags.clone();

    if !neighbors_json.is_empty() {
        debug!(
            neighbor_count = neighbor_contexts.len(),
            "Running graph evolution against nearest neighbors"
        );
        let evolution_result = tokio::time::timeout(
            LLM_TIMEOUT,
            llm.generate_memory_evolution(
                &prepared_memory.conversation_context,
                &prepared_memory.content,
                &prepared_memory.keywords,
                &neighbors_json,
            ),
        )
        .await;
        let evolution = match evolution_result {
            Ok(Ok(evolution)) => evolution,
            Ok(Err(error)) => {
                return Err(RetryableProcessTurnError::GraphEvolution(error.to_string()).into());
            }
            Err(_) => {
                return Err(RetryableProcessTurnError::GraphEvolutionTimeout(
                    LLM_TIMEOUT.as_secs(),
                )
                .into());
            }
        };

        link_ids = evolution
            .suggested_connections
            .into_iter()
            .filter(|target_id| {
                let known = neighbor_ids.contains(target_id);
                if !known {
                    warn!(target_id, "Ignoring LLM-suggested link to unknown neighbor");
                }
                known
            })
            .collect();

        if !evolution.updated_new_memory_tags.is_empty() {
            refined_tags = evolution.updated_new_memory_tags;
        }

        for evolved in evolution.neighbor_updates {
            if let Some(orig) = neighbor_contexts
                .iter()
                .find(|neighbor| neighbor.id == evolved.id)
            {
                let next_context = evolved.context.unwrap_or_else(|| orig.context.clone());
                if orig.context == next_context && orig.tags == evolved.tags {
                    continue;
                }
                neighbor_updates.push(NeighborUpdate {
                    id: orig.id,
                    content: orig.content.clone(),
                    context: next_context,
                    keywords: orig.keywords.clone(),
                    tags: evolved.tags,
                });
            } else {
                warn!(
                    target_id = evolved.id,
                    "Ignoring LLM-suggested update for unknown neighbor"
                );
            }
        }

        info!(
            suggested_links = link_ids.len(),
            neighbor_updates = neighbor_updates.len(),
            tags_changed = (refined_tags != prepared_memory.tags),
            "Graph evolution completed",
        );
    }

    if refined_tags != prepared_memory.tags {
        let updated_payload = EmbeddingInput {
            content: &prepared_memory.content,
            keywords: &prepared_memory.keywords,
            tags: &refined_tags,
            context: &prepared_memory.conversation_context,
        };
        let refined_embedding = encoder
            .encode_memory(&updated_payload)
            .await
            .map_err(|error| RetryableProcessTurnError::Encoder(error.to_string()))?;
        embedding_bytes = embedding_to_bytes(&refined_embedding);
    }

    let prepared_turn = PreparedTurnWrite {
        content: prepared_memory.content,
        memory_timestamp: timestamp.to_rfc3339(),
        conversation_context: prepared_memory.conversation_context,
        keywords_json: serialize_json(&prepared_memory.keywords, "memory keywords")?,
        tags_json: serialize_json(&refined_tags, "memory tags")?,
        embedding_bytes,
        link_ids,
        neighbor_updates: prepare_neighbor_writes(encoder, &neighbor_updates, neighbor_ids.len())
            .await?,
    };

    persist_prepared_turn(db, turn_id, prepared_turn).await
}

#[cfg(test)]
pub(crate) async fn process_turn_for_real_clients(
    db: &MemoryDb,
    llm: &LlmClient,
    encoder: &EncoderClient,
    nearest_neighbor_count: i32,
    turn_id: i64,
    window: ProjectedConversationWindow,
    timestamp: DateTime<Utc>,
) -> Result<(), ProcessTurnError> {
    process_turn_with_clients(
        db,
        llm,
        encoder,
        nearest_neighbor_count,
        turn_id,
        window,
        timestamp,
    )
    .await
}

async fn prepare_neighbor_writes<E>(
    encoder: &E,
    neighbor_updates: &[NeighborUpdate],
    expected_neighbor_count: usize,
) -> Result<Vec<PreparedNeighborUpdate>, ProcessTurnError>
where
    E: MemoryEncoderClient + ?Sized,
{
    if neighbor_updates.is_empty() {
        return Ok(Vec::new());
    }

    let payloads: Vec<EmbeddingInput<'_>> = neighbor_updates
        .iter()
        .map(|update| EmbeddingInput {
            content: &update.content,
            keywords: &update.keywords,
            tags: &update.tags,
            context: &update.context,
        })
        .collect();

    let embeddings = encoder
        .encode_memories(&payloads)
        .await
        .map_err(|error| RetryableProcessTurnError::Encoder(error.to_string()))?;
    validate_embedding_count(
        embeddings.len(),
        neighbor_updates.len(),
        "neighbor re-encoding",
    )
    .map_err(|error| RetryableProcessTurnError::Encoder(error.to_string()))?;

    let mut prepared_updates = Vec::with_capacity(neighbor_updates.len());
    for (update, embedding) in neighbor_updates.iter().zip(embeddings) {
        prepared_updates.push(PreparedNeighborUpdate {
            id: update.id,
            context: update.context.clone(),
            keywords_json: serialize_json(&update.keywords, "neighbor keywords")?,
            tags_json: serialize_json(&update.tags, "neighbor tags")?,
            embedding_bytes: embedding_to_bytes(&embedding),
        });
    }

    if prepared_updates.len() > expected_neighbor_count {
        return Err(UnrecoverableProcessTurnError::Invariant(format!(
            "prepared {} neighbor updates from only {} known neighbors",
            prepared_updates.len(),
            expected_neighbor_count
        ))
        .into());
    }

    Ok(prepared_updates)
}

async fn persist_prepared_turn(
    db: &MemoryDb,
    turn_id: i64,
    prepared_turn: PreparedTurnWrite,
) -> Result<(), ProcessTurnError> {
    let _write_permit = db.acquire_write_permit().await;
    let mut tx = db.begin_immediate().await?;
    let link_count = prepared_turn.link_ids.len();
    let neighbor_update_count = prepared_turn.neighbor_updates.len();

    let new_id = db
        .insert_memory(
            &mut *tx,
            &prepared_turn.content,
            &prepared_turn.memory_timestamp,
            &prepared_turn.conversation_context,
            &prepared_turn.keywords_json,
            &prepared_turn.tags_json,
        )
        .await?;

    db.insert_embedding(&mut *tx, new_id, &prepared_turn.embedding_bytes)
        .await?;

    for target_id in prepared_turn.link_ids {
        db.insert_bidirectional_links(&mut tx, new_id, target_id)
            .await?;
    }

    for update in &prepared_turn.neighbor_updates {
        db.update_memory_metadata(
            &mut *tx,
            update.id,
            &update.context,
            &update.keywords_json,
            &update.tags_json,
        )
        .await?;
        db.update_embedding(&mut *tx, update.id, &update.embedding_bytes)
            .await?;
    }

    let stored_at = Utc::now().to_rfc3339();
    let marked_stored = db
        .mark_ingest_turn_stored(&mut *tx, turn_id, &stored_at)
        .await?;
    if !marked_stored {
        return Err(UnrecoverableProcessTurnError::Invariant(format!(
            "expected processing turn {} to transition to stored within the memory transaction",
            turn_id
        ))
        .into());
    }

    tx.commit().await?;
    info!(
        turn_id,
        memory_id = new_id,
        links_added = link_count,
        neighbor_updates = neighbor_update_count,
        "Stored memory and committed graph updates",
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{MemoryDb, SqliteRuntime};
    use crate::ingest::IngestService;
    use crate::memory_window::{MemoryProjection, MemoryStep, ProjectedConversationWindow};
    use chrono::Utc;
    use memory_bank_protocol::{
        ConversationFragment, ConversationScope, FragmentBody, INGEST_PROTOCOL_VERSION,
        IngestEnvelope, SourceMeta, Terminality,
    };
    use serde::Serializer;
    use serde_json::json;
    use sqlx::Row;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::{Arc, Mutex};
    use tokio::sync::Notify;

    static UNIQUE_COUNTER: AtomicU64 = AtomicU64::new(0);

    #[derive(Default, Clone)]
    struct FakeAnalyzerState {
        analysis_inputs: Arc<Mutex<Vec<(String, String)>>>,
    }

    struct FakeAnalyzer {
        state: FakeAnalyzerState,
        analysis_result: Result<ExtractedMemoryAnalysis, LlmError>,
        evolution_result: Result<MemoryEvolution, LlmError>,
    }

    impl FakeAnalyzer {
        fn succeeds(context: &str) -> (Self, FakeAnalyzerState) {
            let state = FakeAnalyzerState::default();
            (
                Self {
                    state: state.clone(),
                    analysis_result: Ok(ExtractedMemoryAnalysis {
                        context: context.to_string(),
                        keywords: vec!["kw".to_string()],
                        tags: vec!["tag".to_string()],
                    }),
                    evolution_result: Ok(MemoryEvolution::default()),
                },
                state,
            )
        }

        fn with_evolution(analysis: ExtractedMemoryAnalysis, evolution: MemoryEvolution) -> Self {
            Self {
                state: FakeAnalyzerState::default(),
                analysis_result: Ok(analysis),
                evolution_result: Ok(evolution),
            }
        }
    }

    struct BlockingAnalyzer {
        analysis_started: Arc<Notify>,
        release_analysis: Arc<Notify>,
    }

    impl MemoryLlmClient for BlockingAnalyzer {
        async fn analyze_memory_window(
            &self,
            _previous_turns: &str,
            _current_turn: &str,
            _timestamp: DateTime<Utc>,
        ) -> Result<ExtractedMemoryAnalysis, LlmError> {
            self.analysis_started.notify_one();
            self.release_analysis.notified().await;
            Ok(ExtractedMemoryAnalysis {
                context: "blocked context".to_string(),
                keywords: vec!["kw".to_string()],
                tags: vec!["tag".to_string()],
            })
        }

        async fn generate_memory_evolution(
            &self,
            _context: &str,
            _content: &str,
            _keywords: &[String],
            _neighbors_json: &str,
        ) -> Result<MemoryEvolution, LlmError> {
            Ok(MemoryEvolution::default())
        }
    }

    impl MemoryLlmClient for FakeAnalyzer {
        async fn analyze_memory_window(
            &self,
            previous_turns: &str,
            current_turn: &str,
            _timestamp: DateTime<Utc>,
        ) -> Result<ExtractedMemoryAnalysis, LlmError> {
            self.state
                .analysis_inputs
                .lock()
                .expect("lock")
                .push((previous_turns.to_string(), current_turn.to_string()));
            self.analysis_result.clone()
        }

        async fn generate_memory_evolution(
            &self,
            _context: &str,
            _content: &str,
            _keywords: &[String],
            _neighbors_json: &str,
        ) -> Result<MemoryEvolution, LlmError> {
            self.evolution_result.clone()
        }
    }

    #[derive(Clone, Default)]
    struct FakeEncoder {
        encode_results: Arc<Mutex<Vec<Result<Vec<Vec<f32>>, EncoderError>>>>,
        encode_memory_results: Arc<Mutex<Vec<Result<Vec<f32>, EncoderError>>>>,
        encode_memories_results: Arc<Mutex<Vec<Result<Vec<Vec<f32>>, EncoderError>>>>,
    }

    impl FakeEncoder {
        fn from_query_results(results: Vec<Result<Vec<Vec<f32>>, EncoderError>>) -> Self {
            Self {
                encode_results: Arc::new(Mutex::new(results)),
                encode_memory_results: Arc::new(Mutex::new(Vec::new())),
                encode_memories_results: Arc::new(Mutex::new(Vec::new())),
            }
        }

        fn from_memory_results(results: Vec<Result<Vec<f32>, EncoderError>>) -> Self {
            Self {
                encode_results: Arc::new(Mutex::new(Vec::new())),
                encode_memory_results: Arc::new(Mutex::new(results)),
                encode_memories_results: Arc::new(Mutex::new(Vec::new())),
            }
        }
    }

    impl MemoryEncoderClient for FakeEncoder {
        async fn encode(&self, _texts: Vec<String>) -> Result<Vec<Vec<f32>>, EncoderError> {
            self.encode_results.lock().expect("lock").remove(0)
        }

        async fn encode_memory(
            &self,
            _payload: &EmbeddingInput<'_>,
        ) -> Result<Vec<f32>, EncoderError> {
            self.encode_memory_results.lock().expect("lock").remove(0)
        }

        async fn encode_memories(
            &self,
            _payloads: &[EmbeddingInput<'_>],
        ) -> Result<Vec<Vec<f32>>, EncoderError> {
            self.encode_memories_results.lock().expect("lock").remove(0)
        }
    }

    fn current_projection() -> MemoryProjection {
        MemoryProjection {
            user_message: "What time is it?".to_string(),
            assistant_reply: "It is noon.".to_string(),
            steps: vec![MemoryStep::Thinking {
                text: "Checking timezone.".to_string(),
            }],
        }
    }

    struct FailingSerialize;

    impl Serialize for FailingSerialize {
        fn serialize<S>(&self, _serializer: S) -> Result<S::Ok, S::Error>
        where
            S: Serializer,
        {
            Err(serde::ser::Error::custom("boom"))
        }
    }

    #[tokio::test]
    async fn store_window_enqueues_add_task() {
        let (tx, mut rx) = mpsc::channel(1);
        let handle = MemoryHandle { tx };
        let window = ProjectedConversationWindow {
            previous_turns: Vec::new(),
            current_turn: current_projection(),
        };
        let timestamp = Utc::now();

        let store_task = tokio::spawn({
            let handle = handle.clone();
            let window = window.clone();
            async move { handle.store_window(41, window, timestamp).await }
        });

        let task = rx.recv().await.expect("task");
        match task {
            MemoryTask::Add {
                turn_id,
                window: queued_window,
                responder,
                ..
            } => {
                assert_eq!(turn_id, 41);
                assert_eq!(queued_window, window);
                responder.send(Ok(())).expect("ack");
            }
            MemoryTask::Retrieve { .. } => panic!("expected add task"),
        }

        store_task.await.expect("join").expect("store success");
    }

    #[tokio::test]
    async fn prepare_memory_passes_empty_history_to_single_pass_analysis() {
        let (analyzer, state) = FakeAnalyzer::succeeds("window context");
        let window = ProjectedConversationWindow {
            previous_turns: Vec::new(),
            current_turn: current_projection(),
        };

        let prepared = prepare_memory_for_storage(&analyzer, &window, Utc::now())
            .await
            .expect("prepare");

        assert_eq!(prepared.conversation_context, "window context");
        let analysis_inputs = state.analysis_inputs.lock().expect("lock");
        assert_eq!(analysis_inputs.len(), 1);
        assert_eq!(analysis_inputs[0].0, "");
        assert!(analysis_inputs[0].1.contains("## Assistant\nIt is noon."));
    }

    #[tokio::test]
    async fn prepare_memory_uses_rendered_window_for_single_pass_analysis() {
        let (analyzer, state) = FakeAnalyzer::succeeds("derived context");
        let window = ProjectedConversationWindow {
            previous_turns: vec![MemoryProjection {
                user_message: "Earlier".to_string(),
                assistant_reply: "Before".to_string(),
                steps: vec![MemoryStep::ToolCall {
                    name: "shell".to_string(),
                    input: "{\"command\":\"pwd\"}".to_string(),
                }],
            }],
            current_turn: current_projection(),
        };

        let prepared = prepare_memory_for_storage(&analyzer, &window, Utc::now())
            .await
            .expect("prepare");

        assert_eq!(prepared.conversation_context, "derived context");
        let analysis_inputs = state.analysis_inputs.lock().expect("lock");
        assert_eq!(analysis_inputs.len(), 1);
        assert!(analysis_inputs[0].0.contains("# Previous Turn 1"));
        assert!(analysis_inputs[0].1.contains("## Assistant\nIt is noon."));
    }

    #[tokio::test]
    async fn prepare_memory_fails_when_unified_analysis_fails() {
        let state = FakeAnalyzerState::default();
        let analyzer = FakeAnalyzer {
            state: state.clone(),
            analysis_result: Err(LlmError::Api("boom".to_string())),
            evolution_result: Ok(MemoryEvolution::default()),
        };
        let window = ProjectedConversationWindow {
            previous_turns: vec![MemoryProjection {
                user_message: "Earlier".to_string(),
                assistant_reply: "Before".to_string(),
                steps: Vec::new(),
            }],
            current_turn: current_projection(),
        };

        let error = prepare_memory_for_storage(&analyzer, &window, Utc::now())
            .await
            .expect_err("analysis failure");
        assert!(error.to_string().contains("LLM memory analysis failed"));
    }

    #[test]
    fn serialize_json_reports_contextual_errors() {
        let error = serialize_json(&FailingSerialize, "test payload").expect_err("json failure");

        assert!(
            error
                .to_string()
                .contains("Failed to serialize test payload")
        );
    }

    #[test]
    fn estimate_token_count_uses_simple_rounding_heuristic() {
        assert_eq!(estimate_token_count(""), 0);
        assert_eq!(estimate_token_count("abcd"), 1);
        assert_eq!(estimate_token_count("abcde"), 2);
    }

    #[test]
    fn ordered_retrieval_ids_keep_knn_matches_before_link_expansion() {
        let ordered = ordered_retrieval_ids(&[7, 3, 7], &[(3, 5), (7, 11), (7, 3), (3, 5)]);

        assert_eq!(ordered, vec![7, 3, 11, 5]);
    }

    #[test]
    fn sort_retrieval_rows_uses_relevance_order() {
        let ordered_ids = vec![11, 3, 5];
        let mut rows = vec![
            RetrievalRow {
                id: 5,
                content: "fifth".to_string(),
                timestamp: "2026-03-05T00:00:00Z".to_string(),
                context: "ctx".to_string(),
                keywords: "[]".to_string(),
                tags: "[]".to_string(),
            },
            RetrievalRow {
                id: 11,
                content: "eleventh".to_string(),
                timestamp: "2026-03-05T00:00:00Z".to_string(),
                context: "ctx".to_string(),
                keywords: "[]".to_string(),
                tags: "[]".to_string(),
            },
            RetrievalRow {
                id: 3,
                content: "third".to_string(),
                timestamp: "2026-03-05T00:00:00Z".to_string(),
                context: "ctx".to_string(),
                keywords: "[]".to_string(),
                tags: "[]".to_string(),
            },
        ];

        sort_retrieval_rows(&mut rows, &ordered_ids);

        assert_eq!(
            rows.iter().map(|row| row.id).collect::<Vec<_>>(),
            ordered_ids
        );
    }

    #[tokio::test]
    async fn retrieve_with_encoder_expands_linked_neighbors_after_knn_hits() {
        let db = open_memory_db().await;
        let anchor_id = insert_memory_with_embedding(
            &db,
            "Tokio mpsc backpressure keeps bounded queues stable.",
            "anchor",
            &["tokio", "mpsc", "backpressure"],
            &["rust", "async", "queue"],
            &[1.0, 0.0],
        )
        .await;
        let secondary_id = insert_memory_with_embedding(
            &db,
            "Bounded async queue pressure can stall producers until receivers catch up.",
            "secondary",
            &["bounded", "queue", "pressure"],
            &["rust", "async", "queue"],
            &[0.95, 0.05],
        )
        .await;
        let linked_id = insert_memory_with_embedding(
            &db,
            "Channel sizing guidance explains when to increase buffer capacity.",
            "linked",
            &["channel", "buffer", "capacity"],
            &["rust", "async", "tuning"],
            &[0.1, 0.9],
        )
        .await;

        let mut tx = db.begin().await.expect("begin");
        db.insert_bidirectional_links(&mut tx, anchor_id, linked_id)
            .await
            .expect("insert link");
        tx.commit().await.expect("commit");

        let encoder = FakeEncoder::from_query_results(vec![Ok(vec![vec![1.0, 0.0]])]);
        let outcome = retrieve_with_encoder(&db, &encoder, 2, "bounded rust queue backpressure")
            .await
            .expect("retrieve");

        assert_eq!(outcome.knn_matches, 2);
        assert_eq!(outcome.expanded_matches, 3);
        assert_eq!(
            outcome
                .notes
                .iter()
                .map(|note| note.content.as_str())
                .collect::<Vec<_>>(),
            vec![
                "Tokio mpsc backpressure keeps bounded queues stable.",
                "Bounded async queue pressure can stall producers until receivers catch up.",
                "Channel sizing guidance explains when to increase buffer capacity.",
            ]
        );

        let _ = secondary_id;
    }

    #[tokio::test]
    async fn refined_tag_reencoding_failure_aborts_before_persisting() {
        let db = open_memory_db().await;
        create_processing_turn_table(&db).await;
        insert_processing_turn(&db, 7).await;
        insert_memory_with_embedding(
            &db,
            "existing",
            "neighbor context",
            &["neighbor-kw"],
            &["neighbor-tag"],
            &[0.1, 0.2],
        )
        .await;

        let llm = FakeAnalyzer::with_evolution(
            ExtractedMemoryAnalysis {
                context: "conversation context".to_string(),
                keywords: vec!["kw".to_string()],
                tags: vec!["initial-tag".to_string()],
            },
            MemoryEvolution {
                suggested_connections: Vec::new(),
                updated_new_memory_tags: vec!["refined-tag".to_string()],
                neighbor_updates: Vec::new(),
            },
        );
        let encoder = FakeEncoder::from_memory_results(vec![
            Ok(vec![0.4, 0.5]),
            Err(EncoderError::Encode(
                "refined tag encoding failed".to_string(),
            )),
        ]);

        let error = process_turn_with_clients(
            &db,
            &llm,
            &encoder,
            10,
            7,
            ProjectedConversationWindow {
                previous_turns: Vec::new(),
                current_turn: current_projection(),
            },
            Utc::now(),
        )
        .await
        .expect_err("re-encoding should fail");

        assert!(error.to_string().contains("refined tag encoding failed"));
        assert_eq!(memory_count(&db).await, 1, "should not insert a new memory");
        assert_eq!(turn_status(&db, 7).await, "processing");
    }

    #[tokio::test]
    async fn neighbor_tag_only_updates_preserve_existing_context() {
        let db = open_memory_db().await;
        create_processing_turn_table(&db).await;
        insert_processing_turn(&db, 8).await;
        let neighbor_id = insert_memory_with_embedding(
            &db,
            "existing",
            "original context",
            &["neighbor-kw"],
            &["neighbor-tag"],
            &[0.1, 0.2],
        )
        .await;

        let llm = FakeAnalyzer::with_evolution(
            ExtractedMemoryAnalysis {
                context: "conversation context".to_string(),
                keywords: vec!["kw".to_string()],
                tags: vec!["initial-tag".to_string()],
            },
            MemoryEvolution {
                suggested_connections: Vec::new(),
                updated_new_memory_tags: Vec::new(),
                neighbor_updates: vec![crate::llm::EvolvedNeighbor {
                    id: neighbor_id,
                    context: None,
                    tags: vec!["updated-tag".to_string()],
                }],
            },
        );
        let encoder = FakeEncoder {
            encode_results: Arc::new(Mutex::new(Vec::new())),
            encode_memory_results: Arc::new(Mutex::new(vec![Ok(vec![0.4, 0.6])])),
            encode_memories_results: Arc::new(Mutex::new(vec![Ok(vec![vec![0.9, 0.1]])])),
        };

        process_turn_with_clients(
            &db,
            &llm,
            &encoder,
            10,
            8,
            ProjectedConversationWindow {
                previous_turns: Vec::new(),
                current_turn: current_projection(),
            },
            Utc::now(),
        )
        .await
        .expect("tag-only neighbor update should succeed");

        assert_eq!(memory_count(&db).await, 2, "new memory should be stored");
        assert_eq!(turn_status(&db, 8).await, "stored");

        let row = sqlx::query("SELECT context, tags FROM memories WHERE id = ?")
            .bind(neighbor_id)
            .fetch_one(db.pool_for_tests())
            .await
            .expect("neighbor row");
        assert_eq!(row.get::<String, _>("context"), "original context");
        assert_eq!(row.get::<String, _>("tags"), "[\"updated-tag\"]");
    }

    #[tokio::test]
    async fn link_insertion_failure_rolls_back_new_memory_and_turn_status() {
        let db = open_memory_db().await;
        create_processing_turn_table(&db).await;
        insert_processing_turn(&db, 11).await;

        let error = persist_prepared_turn(
            &db,
            11,
            PreparedTurnWrite {
                content: "new memory".to_string(),
                memory_timestamp: "2026-03-05T00:00:00Z".to_string(),
                conversation_context: "ctx".to_string(),
                keywords_json: "[\"kw\"]".to_string(),
                tags_json: "[\"tag\"]".to_string(),
                embedding_bytes: embedding_to_bytes(&[0.3, 0.7]),
                link_ids: vec![999],
                neighbor_updates: Vec::new(),
            },
        )
        .await
        .expect_err("missing link target should fail");

        assert!(error.to_string().contains("FOREIGN KEY"));
        assert_eq!(memory_count(&db).await, 0, "new memory should roll back");
        assert_eq!(turn_status(&db, 11).await, "processing");
    }

    #[tokio::test]
    async fn neighbor_update_failure_rolls_back_prior_neighbor_changes() {
        let db = open_memory_db().await;
        create_processing_turn_table(&db).await;
        insert_processing_turn(&db, 19).await;

        let original_embedding = embedding_to_bytes(&[0.2, 0.8]);
        let neighbor_id = insert_memory_with_embedding(
            &db,
            "existing memory",
            "original context",
            &["orig-kw"],
            &["orig-tag"],
            &[0.2, 0.8],
        )
        .await;

        let error = persist_prepared_turn(
            &db,
            19,
            PreparedTurnWrite {
                content: "new memory".to_string(),
                memory_timestamp: "2026-03-05T00:00:00Z".to_string(),
                conversation_context: "ctx".to_string(),
                keywords_json: "[\"kw\"]".to_string(),
                tags_json: "[\"tag\"]".to_string(),
                embedding_bytes: embedding_to_bytes(&[0.4, 0.6]),
                link_ids: Vec::new(),
                neighbor_updates: vec![
                    PreparedNeighborUpdate {
                        id: neighbor_id,
                        context: "updated context".to_string(),
                        keywords_json: "[\"updated-kw\"]".to_string(),
                        tags_json: "[\"updated-tag\"]".to_string(),
                        embedding_bytes: embedding_to_bytes(&[0.9, 0.1]),
                    },
                    PreparedNeighborUpdate {
                        id: 999,
                        context: "missing context".to_string(),
                        keywords_json: "[\"missing-kw\"]".to_string(),
                        tags_json: "[\"missing-tag\"]".to_string(),
                        embedding_bytes: embedding_to_bytes(&[0.5, 0.5]),
                    },
                ],
            },
        )
        .await
        .expect_err("missing neighbor should fail");

        assert!(matches!(
            error,
            ProcessTurnError::Retryable(RetryableProcessTurnError::Database(
                sqlx::Error::RowNotFound
            ))
        ));
        assert_eq!(memory_count(&db).await, 1, "new memory should roll back");
        assert_eq!(turn_status(&db, 19).await, "processing");

        let row = sqlx::query("SELECT context, keywords, tags FROM memories WHERE id = ?")
            .bind(neighbor_id)
            .fetch_one(db.pool_for_tests())
            .await
            .expect("neighbor row");
        assert_eq!(row.get::<String, _>("context"), "original context");
        assert_eq!(row.get::<String, _>("keywords"), "[\"orig-kw\"]");
        assert_eq!(row.get::<String, _>("tags"), "[\"orig-tag\"]");

        let stored_embedding =
            sqlx::query_scalar::<_, Vec<u8>>("SELECT embedding FROM vec_memories WHERE id = ?")
                .bind(neighbor_id)
                .fetch_one(db.pool_for_tests())
                .await
                .expect("neighbor embedding");
        assert_eq!(stored_embedding, original_embedding);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn memory_handle_retrieve_waits_behind_inflight_store_window() {
        let db = Arc::new(open_memory_db().await);
        create_processing_turn_table(db.as_ref()).await;
        insert_processing_turn(db.as_ref(), 41).await;
        insert_memory_with_embedding(
            db.as_ref(),
            "Existing memory",
            "context",
            &["kw"],
            &["tag"],
            &[1.0, 0.0],
        )
        .await;

        let analysis_started = Arc::new(Notify::new());
        let release_analysis = Arc::new(Notify::new());
        let analyzer = BlockingAnalyzer {
            analysis_started: analysis_started.clone(),
            release_analysis: release_analysis.clone(),
        };
        let encoder = FakeEncoder {
            encode_results: Arc::new(Mutex::new(vec![Ok(vec![vec![1.0, 0.0]])])),
            encode_memory_results: Arc::new(Mutex::new(vec![Ok(vec![0.4, 0.6])])),
            encode_memories_results: Arc::new(Mutex::new(Vec::new())),
        };
        let (handle, actor_task) = spawn_test_actor(db.clone(), analyzer, encoder, 1);
        let window = ProjectedConversationWindow {
            previous_turns: Vec::new(),
            current_turn: current_projection(),
        };

        let store_task = tokio::spawn({
            let handle = handle.clone();
            async move { handle.store_window(41, window, Utc::now()).await }
        });

        analysis_started.notified().await;

        let retrieve_task = tokio::spawn({
            let handle = handle.clone();
            async move { handle.retrieve("existing memory".to_string()).await }
        });
        tokio::task::yield_now().await;
        assert!(
            !retrieve_task.is_finished(),
            "retrieve should wait behind the in-flight add task"
        );

        release_analysis.notify_waiters();

        store_task
            .await
            .expect("join store task")
            .expect("store window");
        let notes = retrieve_task
            .await
            .expect("join retrieve task")
            .expect("retrieve notes");
        assert_eq!(turn_status(db.as_ref(), 41).await, "stored");
        assert!(!notes.is_empty());

        drop(handle);
        tokio::task::spawn_blocking(move || actor_task.join().expect("join actor thread"))
            .await
            .expect("join actor task");
    }

    #[tokio::test]
    async fn retrieve_with_encoder_reads_while_runtime_writer_is_active() {
        let db_path = test_db_path("retrieve_while_writer_active");
        let runtime = SqliteRuntime::open_file_for_tests(&db_path, 2)
            .await
            .expect("open sqlite runtime");
        let db = MemoryDb::open_with_runtime_for_tests(runtime.clone(), 2)
            .await
            .expect("open memory db");
        insert_memory_with_embedding(
            &db,
            "Existing memory",
            "context",
            &["kw"],
            &["tag"],
            &[1.0, 0.0],
        )
        .await;

        let write_permit = runtime.acquire_write_permit().await;
        let tx = runtime
            .pool()
            .begin_with("BEGIN IMMEDIATE")
            .await
            .expect("begin immediate");

        let encoder = FakeEncoder::from_query_results(vec![Ok(vec![vec![1.0, 0.0]])]);
        let outcome = retrieve_with_encoder(&db, &encoder, 1, "existing memory")
            .await
            .expect("retrieve while writer active");
        assert_eq!(outcome.notes.len(), 1);

        tx.commit().await.expect("commit writer transaction");
        drop(write_permit);
    }

    #[tokio::test]
    async fn ingest_and_memory_persistence_queue_behind_shared_writer_gate() {
        let db_path = test_db_path("shared_runtime");
        let runtime = SqliteRuntime::open_file_for_tests(&db_path, 2)
            .await
            .expect("open sqlite runtime");
        let db = Arc::new(
            MemoryDb::open_with_runtime_for_tests(runtime.clone(), 2)
                .await
                .expect("open memory db"),
        );
        let service = IngestService::open_with_runtime(
            runtime.clone(),
            &db_path,
            MemoryHandle::closed_for_tests(),
            0,
        )
        .await
        .expect("open ingest service");
        insert_real_processing_turn(db.as_ref(), 23).await;

        let write_permit = runtime.acquire_write_permit().await;
        let mut write_attempts = runtime.install_write_attempt_notifier();
        let tx = runtime
            .pool()
            .begin_with("BEGIN IMMEDIATE")
            .await
            .expect("begin immediate");

        let ingest_task = tokio::spawn({
            let service = service.clone();
            async move {
                service
                    .ingest(open_user_envelope("session-shared", "fragment-1"))
                    .await
            }
        });
        let persist_task = tokio::spawn({
            let db = db.clone();
            async move {
                persist_prepared_turn(
                    db.as_ref(),
                    23,
                    PreparedTurnWrite {
                        content: "new memory".to_string(),
                        memory_timestamp: "2026-03-05T00:00:00Z".to_string(),
                        conversation_context: "ctx".to_string(),
                        keywords_json: "[\"kw\"]".to_string(),
                        tags_json: "[\"tag\"]".to_string(),
                        embedding_bytes: embedding_to_bytes(&[0.4, 0.6]),
                        link_ids: Vec::new(),
                        neighbor_updates: Vec::new(),
                    },
                )
                .await
            }
        });
        wait_for_write_attempts(&mut write_attempts, 2).await;
        tokio::task::yield_now().await;
        assert!(!ingest_task.is_finished(), "ingest should still be blocked");
        assert!(
            !persist_task.is_finished(),
            "persist should still be blocked"
        );

        tx.commit().await.expect("commit transaction");
        drop(write_permit);

        let ingest = ingest_task
            .await
            .expect("join ingest task")
            .expect("ingest write");
        assert!(!ingest.finalized);
        persist_task
            .await
            .expect("join persist task")
            .expect("persist write");

        assert_eq!(memory_count(db.as_ref()).await, 1);
        assert_eq!(turn_status(db.as_ref(), 23).await, "stored");
    }

    async fn open_memory_db() -> MemoryDb {
        MemoryDb::open_in_memory_for_tests(2)
            .await
            .expect("test memory db")
    }

    async fn create_processing_turn_table(db: &MemoryDb) {
        sqlx::query(
            "CREATE TABLE ingest_turns (
                id INTEGER PRIMARY KEY,
                status TEXT NOT NULL,
                last_error TEXT,
                next_attempt_at TEXT,
                processing_started_at TEXT,
                stored_at TEXT,
                updated_at TEXT
            );",
        )
        .execute(db.pool_for_tests())
        .await
        .expect("create ingest_turns");
    }

    async fn insert_processing_turn(db: &MemoryDb, turn_id: i64) {
        sqlx::query(
            "INSERT INTO ingest_turns (
                id,
                status,
                next_attempt_at,
                processing_started_at,
                updated_at
            ) VALUES (?, 'processing', ?, ?, ?);",
        )
        .bind(turn_id)
        .bind("2026-03-05T00:00:00Z")
        .bind("2026-03-05T00:00:01Z")
        .bind("2026-03-05T00:00:01Z")
        .execute(db.pool_for_tests())
        .await
        .expect("insert processing turn");
    }

    async fn insert_memory_with_embedding(
        db: &MemoryDb,
        content: &str,
        context: &str,
        keywords: &[&str],
        tags: &[&str],
        embedding: &[f32],
    ) -> i64 {
        let mut tx = db.begin().await.expect("begin");
        let memory_id = db
            .insert_memory(
                &mut *tx,
                content,
                "2026-03-05T00:00:00Z",
                context,
                &serde_json::to_string(keywords).expect("keywords"),
                &serde_json::to_string(tags).expect("tags"),
            )
            .await
            .expect("insert memory");
        db.insert_embedding(&mut *tx, memory_id, &embedding_to_bytes(embedding))
            .await
            .expect("insert embedding");
        tx.commit().await.expect("commit");
        memory_id
    }

    async fn memory_count(db: &MemoryDb) -> i64 {
        sqlx::query_scalar("SELECT COUNT(*) FROM memories")
            .fetch_one(db.pool_for_tests())
            .await
            .expect("memory count")
    }

    async fn turn_status(db: &MemoryDb, turn_id: i64) -> String {
        sqlx::query_scalar("SELECT status FROM ingest_turns WHERE id = ?")
            .bind(turn_id)
            .fetch_one(db.pool_for_tests())
            .await
            .expect("turn status")
    }

    async fn wait_for_write_attempts(
        write_attempts: &mut tokio::sync::mpsc::UnboundedReceiver<()>,
        expected: usize,
    ) {
        for _ in 0..expected {
            write_attempts.recv().await.expect("write attempt");
        }
    }

    fn spawn_test_actor<L, E>(
        db: Arc<MemoryDb>,
        llm: L,
        encoder: E,
        nearest_neighbor_count: i32,
    ) -> (MemoryHandle, std::thread::JoinHandle<()>)
    where
        L: MemoryLlmClient + Send + Sync + 'static,
        E: MemoryEncoderClient + Send + Sync + 'static,
    {
        let (tx, mut rx) = mpsc::channel(MEMORY_CHANNEL_CAPACITY);
        let llm = Arc::new(llm);
        let encoder = Arc::new(encoder);
        let task = std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("build actor test runtime");
            runtime.block_on(async move {
                while let Some(task) = rx.recv().await {
                    match task {
                        MemoryTask::Add {
                            turn_id,
                            window,
                            timestamp,
                            responder,
                        } => {
                            let result = process_turn_with_clients(
                                db.as_ref(),
                                llm.as_ref(),
                                encoder.as_ref(),
                                nearest_neighbor_count,
                                turn_id,
                                window,
                                timestamp,
                            )
                            .await;
                            let _ = responder.send(result);
                        }
                        MemoryTask::Retrieve { query, responder } => {
                            let result = retrieve_with_encoder(
                                db.as_ref(),
                                encoder.as_ref(),
                                nearest_neighbor_count,
                                &query,
                            )
                            .await
                            .map(|outcome| outcome.notes);
                            let _ = responder.send(result);
                        }
                    }
                }
            });
        });
        (MemoryHandle { tx }, task)
    }

    async fn insert_real_processing_turn(db: &MemoryDb, turn_id: i64) {
        sqlx::query(
            "INSERT INTO ingest_turns (
                id,
                conversation_id,
                external_turn_id,
                turn_index,
                status,
                projection_json,
                terminality,
                created_at,
                updated_at,
                finalized_at,
                attempt_count,
                last_error,
                next_attempt_at,
                processing_started_at,
                stored_at
            ) VALUES (?, ?, NULL, ?, 'processing', ?, 'hard', ?, ?, ?, 0, NULL, ?, ?, NULL);",
        )
        .bind(turn_id)
        .bind("session-processing")
        .bind(1_i64)
        .bind(serde_json::to_string(&MemoryProjection::default()).expect("projection"))
        .bind("2026-03-05T00:00:00Z")
        .bind("2026-03-05T00:00:01Z")
        .bind("2026-03-05T00:00:00Z")
        .bind("2026-03-05T00:00:00Z")
        .bind("2026-03-05T00:00:01Z")
        .execute(db.pool_for_tests())
        .await
        .expect("insert processing turn");
    }

    fn test_db_path(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!("memory_bank_actor_{label}_{}.db", unique_suffix()))
    }

    fn unique_suffix() -> u128 {
        let time_component = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("time")
            .as_nanos();
        let counter_component = u128::from(UNIQUE_COUNTER.fetch_add(1, Ordering::Relaxed));
        (time_component << 16) | counter_component
    }

    fn open_user_envelope(conversation_id: &str, fragment_id: &str) -> IngestEnvelope {
        IngestEnvelope {
            protocol_version: INGEST_PROTOCOL_VERSION,
            source: SourceMeta {
                agent: "codex".to_string(),
                event: "UserPromptSubmit".to_string(),
            },
            scope: ConversationScope {
                conversation_id: conversation_id.to_string(),
                turn_id: None,
                fragment_id: fragment_id.to_string(),
                sequence_hint: Some(1),
                emitted_at_rfc3339: Some("2026-03-05T00:00:00Z".to_string()),
            },
            fragment: ConversationFragment {
                terminality: Terminality::None,
                body: FragmentBody::UserMessage {
                    text: "remember this".to_string(),
                },
            },
            raw: json!({"session_id": conversation_id}),
        }
    }
}

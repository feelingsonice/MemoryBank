use std::fmt;
use std::path::Path;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use libsqlite3_sys::{SQLITE_BUSY, SQLITE_LOCKED};
use memory_bank_protocol::{ConversationFragment, FragmentBody, IngestEnvelope, Terminality};
use sqlx::{SqlitePool, Transaction};
use thiserror::Error;
use tokio::sync::Notify;
use tokio::time::{Instant, sleep_until};
use tokio_retry::RetryIf;
use tokio_retry::strategy::ExponentialBackoff;
use tracing::{debug, info, warn};

use crate::actor::{MemoryHandle, ProcessTurnError, UnrecoverableProcessTurnError};
use crate::db::SqliteRuntime;
use crate::error::AppError;
use crate::memory_window::{MemoryProjection, MemoryStep, ProjectedConversationWindow};
const MAX_STAGE_RETRIES: usize = 3;
const STAGE_RETRY_BASE_DELAY_MS: u64 = 10;
const STAGE_RETRY_MAX_DELAY: Duration = Duration::from_millis(80);
const RETRY_BASE_DELAY: Duration = Duration::from_secs(5);
const RETRY_MAX_DELAY: Duration = Duration::from_secs(300);
const DISPATCHER_ERROR_RETRY_DELAY: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, Copy, PartialEq, Eq, sqlx::Type)]
#[sqlx(rename_all = "snake_case")]
enum IngestTurnStatus {
    Open,
    Finalized,
    Processing,
    Stored,
    Aborted,
    Failed,
}

impl IngestTurnStatus {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Finalized => "finalized",
            Self::Processing => "processing",
            Self::Stored => "stored",
            Self::Aborted => "aborted",
            Self::Failed => "failed",
        }
    }
}

impl fmt::Display for IngestTurnStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for IngestTurnStatus {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "open" => Ok(Self::Open),
            "finalized" => Ok(Self::Finalized),
            "processing" => Ok(Self::Processing),
            "stored" => Ok(Self::Stored),
            "aborted" => Ok(Self::Aborted),
            "failed" => Ok(Self::Failed),
            other => Err(format!("invalid ingest turn status '{other}'")),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, sqlx::Type)]
#[sqlx(rename_all = "snake_case")]
enum IngestFragmentKind {
    UserMessage,
    AssistantMessage,
    ToolCall,
    ToolResult,
    Thinking,
    Other,
}

impl IngestFragmentKind {
    const fn as_str(self) -> &'static str {
        match self {
            Self::UserMessage => "user_message",
            Self::AssistantMessage => "assistant_message",
            Self::ToolCall => "tool_call",
            Self::ToolResult => "tool_result",
            Self::Thinking => "thinking",
            Self::Other => "other",
        }
    }
}

impl fmt::Display for IngestFragmentKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for IngestFragmentKind {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "user_message" => Ok(Self::UserMessage),
            "assistant_message" => Ok(Self::AssistantMessage),
            "tool_call" => Ok(Self::ToolCall),
            "tool_result" => Ok(Self::ToolResult),
            "thinking" => Ok(Self::Thinking),
            "other" => Ok(Self::Other),
            other => Err(format!("invalid ingest fragment kind '{other}'")),
        }
    }
}

impl From<&FragmentBody> for IngestFragmentKind {
    fn from(body: &FragmentBody) -> Self {
        match body {
            FragmentBody::UserMessage { .. } => Self::UserMessage,
            FragmentBody::AssistantMessage { .. } => Self::AssistantMessage,
            FragmentBody::ToolCall { .. } => Self::ToolCall,
            FragmentBody::ToolResult { .. } => Self::ToolResult,
            FragmentBody::Thinking { .. } => Self::Thinking,
            FragmentBody::Other { .. } => Self::Other,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, sqlx::Type)]
#[sqlx(rename_all = "snake_case")]
enum StoredTerminality {
    None,
    Soft,
    Hard,
}

impl StoredTerminality {
    const fn into_terminality(self) -> Terminality {
        match self {
            Self::None => Terminality::None,
            Self::Soft => Terminality::Soft,
            Self::Hard => Terminality::Hard,
        }
    }
}

#[derive(Debug, Clone)]
pub struct IngestOutcome {
    pub agent: String,
    pub event: String,
    pub conversation_id: String,
    pub fragment_id: String,
    pub turn_index: i64,
    pub duplicate: bool,
    pub finalized: bool,
}

#[derive(Clone)]
pub struct IngestService {
    store: Arc<IngestStore>,
    dispatcher_notify: Arc<Notify>,
}

impl IngestService {
    #[cfg_attr(not(test), allow(dead_code))]
    pub async fn open(
        db_path: &Path,
        memory: MemoryHandle,
        history_window_size: u32,
    ) -> Result<Self, AppError> {
        let runtime = SqliteRuntime::open(db_path).await?;
        Self::open_with_runtime(runtime, db_path, memory, history_window_size).await
    }

    pub(crate) async fn open_with_runtime(
        runtime: SqliteRuntime,
        db_path: &Path,
        memory: MemoryHandle,
        history_window_size: u32,
    ) -> Result<Self, AppError> {
        let store = Arc::new(IngestStore::open_with_runtime(runtime, history_window_size).await?);
        let dispatcher_notify = Arc::new(Notify::new());
        let recovered_turns = store.recover_processing_turns().await?;
        if recovered_turns > 0 {
            warn!(
                recovered_turns,
                "Recovered interrupted ingest turns and re-queued them for processing"
            );
        }

        let dispatcher = IngestDispatcher::new(store.clone(), memory, dispatcher_notify.clone());
        tokio::spawn(async move {
            dispatcher.run().await;
        });

        info!(
            db_path = %db_path.display(),
            history_window_size,
            history_window_mode = if history_window_size == 0 {
                "unlimited"
            } else {
                "bounded"
            },
            "Durable ingest service ready"
        );

        Ok(Self {
            store,
            dispatcher_notify,
        })
    }

    pub async fn ingest(&self, envelope: IngestEnvelope) -> Result<IngestOutcome, IngestError> {
        envelope.validate().map_err(IngestError::Validation)?;
        validate_runtime_constraints(&envelope)?;
        let staged = self.store.stage_fragment(&envelope).await?;

        if staged.finalized {
            debug!(
                conversation_id = %envelope.scope.conversation_id,
                turn_index = staged.turn_index,
                duplicate = staged.duplicate,
                "Queued finalized turn for background memory extraction"
            );
            self.dispatcher_notify.notify_one();
        }

        let IngestEnvelope { source, scope, .. } = envelope;
        Ok(IngestOutcome {
            agent: source.agent,
            event: source.event,
            conversation_id: scope.conversation_id,
            fragment_id: scope.fragment_id,
            turn_index: staged.turn_index,
            duplicate: staged.duplicate,
            finalized: staged.finalized,
        })
    }
}

#[derive(Debug, Error)]
pub enum IngestError {
    #[error("invalid ingest payload: {0}")]
    Validation(String),
    #[error("database error: {0}")]
    Database(#[from] sqlx::Error),
    #[error("serialization error: {0}")]
    Serialization(#[from] serde_json::Error),
}

impl IngestError {
    pub(crate) fn is_sqlite_lock_contention(&self) -> bool {
        matches!(self, Self::Database(database_error) if is_sqlite_lock_contention(database_error))
    }
}

pub struct IngestStore {
    runtime: SqliteRuntime,
    history_window_size: u32,
}

impl IngestStore {
    #[cfg_attr(not(test), allow(dead_code))]
    async fn open(db_path: &Path, history_window_size: u32) -> Result<Self, AppError> {
        let runtime = SqliteRuntime::open(db_path).await?;
        Self::open_with_runtime(runtime, history_window_size).await
    }

    async fn open_with_runtime(
        runtime: SqliteRuntime,
        history_window_size: u32,
    ) -> Result<Self, AppError> {
        initialize_ingest_schema(runtime.pool()).await?;

        Ok(Self {
            runtime,
            history_window_size,
        })
    }

    fn pool(&self) -> &SqlitePool {
        self.runtime.pool()
    }

    async fn acquire_write_permit(&self) -> tokio::sync::OwnedSemaphorePermit {
        self.runtime.acquire_write_permit().await
    }

    async fn stage_fragment(
        &self,
        envelope: &IngestEnvelope,
    ) -> Result<StagedFragment, IngestError> {
        RetryIf::spawn(
            stage_retry_strategy(),
            || self.stage_fragment_once(envelope),
            should_retry_stage_error,
        )
        .await
    }

    async fn stage_fragment_once(
        &self,
        envelope: &IngestEnvelope,
    ) -> Result<StagedFragment, IngestError> {
        let _write_permit = self.acquire_write_permit().await;
        let mut tx = self.pool().begin_with("BEGIN IMMEDIATE").await?;
        let now = Utc::now().to_rfc3339();

        if let Some(existing) = self
            .load_existing_fragment_in_tx(&mut tx, &envelope.scope.fragment_id)
            .await?
        {
            tx.rollback().await?;
            return Ok(staged_fragment_from_existing(existing));
        }

        let turn = self.resolve_turn(&mut tx, envelope, &now).await?;
        let fragment_json = serde_json::to_string(&envelope.fragment)?;
        let raw_json = serde_json::to_string(&envelope.raw)?;
        let sequence_hint = envelope
            .scope
            .sequence_hint
            .map(i64::try_from)
            .transpose()
            .map_err(|_| {
                IngestError::Validation(
                    "scope.sequence_hint exceeds signed 64-bit range".to_string(),
                )
            })?;

        let insert_result = sqlx::query(INSERT_INGEST_FRAGMENT_SQL)
            .bind(&envelope.scope.fragment_id)
            .bind(&envelope.scope.conversation_id)
            .bind(turn.id)
            .bind(&envelope.source.agent)
            .bind(&envelope.source.event)
            .bind(IngestFragmentKind::from(&envelope.fragment.body))
            .bind(envelope.fragment.terminality.as_str())
            .bind(sequence_hint)
            .bind(&envelope.scope.emitted_at_rfc3339)
            .bind(fragment_json)
            .bind(raw_json)
            .bind(&now)
            .execute(&mut *tx)
            .await?;

        if insert_result.rows_affected() == 0 {
            tx.rollback().await?;
            let existing = self
                .load_existing_fragment(&envelope.scope.fragment_id)
                .await?;
            return Ok(staged_fragment_from_existing(existing));
        }

        let finalized = if envelope.fragment.terminality == Terminality::Hard {
            let reduced = self.rebuild_projection(&mut tx, turn.id).await?;
            self.update_turn_after_reduce(&mut tx, turn.id, reduced, &now)
                .await?
        } else {
            let terminality = merge_terminality(
                turn.terminality.into_terminality(),
                envelope.fragment.terminality,
            );
            self.update_open_turn_after_insert(&mut tx, turn.id, terminality, &now)
                .await?;
            false
        };

        tx.commit().await?;

        Ok(StagedFragment {
            turn_index: turn.turn_index,
            duplicate: false,
            finalized,
        })
    }

    async fn resolve_turn(
        &self,
        tx: &mut Transaction<'_, sqlx::Sqlite>,
        envelope: &IngestEnvelope,
        now: &str,
    ) -> Result<TurnRef, IngestError> {
        if let Some(external_turn_id) = &envelope.scope.turn_id {
            if let Some(turn) = self
                .load_turn_by_external_id(tx, &envelope.scope.conversation_id, external_turn_id)
                .await?
            {
                return match turn.status {
                    IngestTurnStatus::Open => Ok(turn.into_turn_ref()),
                    closed_status => Err(IngestError::Validation(format!(
                        "fragment references closed external turn '{}' in conversation '{}' with status '{}'",
                        external_turn_id, envelope.scope.conversation_id, closed_status
                    ))),
                };
            }
            return self
                .create_turn(
                    tx,
                    &envelope.scope.conversation_id,
                    Some(external_turn_id),
                    now,
                )
                .await;
        }

        if let Some(open_turn) = self
            .load_open_turn(tx, &envelope.scope.conversation_id)
            .await?
        {
            if matches!(envelope.fragment.body, FragmentBody::UserMessage { .. })
                && self.turn_has_fragments(tx, open_turn.id).await?
            {
                info!(
                    conversation_id = %envelope.scope.conversation_id,
                    previous_turn_id = open_turn.id,
                    previous_turn_index = open_turn.turn_index,
                    fragment_id = %envelope.scope.fragment_id,
                    "Aborting incomplete open turn because a new user message started a replacement turn"
                );
                self.abort_turn(tx, open_turn.id, now).await?;
                return self
                    .create_turn(tx, &envelope.scope.conversation_id, None, now)
                    .await;
            }

            return Ok(open_turn);
        }

        self.create_turn(tx, &envelope.scope.conversation_id, None, now)
            .await
    }

    async fn rebuild_projection(
        &self,
        tx: &mut Transaction<'_, sqlx::Sqlite>,
        turn_id: i64,
    ) -> Result<ReducedProjection, IngestError> {
        let rows = sqlx::query_as::<_, StoredFragmentRow>(SELECT_TURN_FRAGMENTS_SQL)
            .bind(turn_id)
            .fetch_all(&mut **tx)
            .await?;

        let mut projection = MemoryProjection::default();
        let mut terminality = Terminality::None;

        for row in rows {
            let fragment: ConversationFragment = serde_json::from_str(&row.fragment_json)?;
            apply_fragment_to_projection(&mut projection, &mut terminality, fragment);
        }

        Ok(ReducedProjection {
            projection,
            terminality,
        })
    }

    async fn update_open_turn_after_insert(
        &self,
        tx: &mut Transaction<'_, sqlx::Sqlite>,
        turn_id: i64,
        terminality: Terminality,
        now: &str,
    ) -> Result<(), IngestError> {
        let result = sqlx::query(UPDATE_OPEN_TURN_AFTER_INSERT_SQL)
            .bind(terminality.as_str())
            .bind(now)
            .bind(turn_id)
            .bind(IngestTurnStatus::Open)
            .execute(&mut **tx)
            .await?;
        if result.rows_affected() != 1 {
            return Err(sqlx::Error::RowNotFound.into());
        }
        Ok(())
    }

    async fn update_turn_after_reduce(
        &self,
        tx: &mut Transaction<'_, sqlx::Sqlite>,
        turn_id: i64,
        reduced: ReducedProjection,
        now: &str,
    ) -> Result<bool, IngestError> {
        let finalized = reduced.terminality == Terminality::Hard;
        let turn_status = if finalized {
            IngestTurnStatus::Finalized
        } else {
            IngestTurnStatus::Open
        };
        let result = sqlx::query(UPDATE_TURN_AFTER_REDUCE_SQL)
            .bind(serde_json::to_string(&reduced.projection)?)
            .bind(reduced.terminality.as_str())
            .bind(turn_status)
            .bind(if finalized { Some(now) } else { None })
            .bind(if finalized { Some(now) } else { None })
            .bind(now)
            .bind(turn_id)
            .bind(IngestTurnStatus::Open)
            .execute(&mut **tx)
            .await?;
        if result.rows_affected() != 1 {
            return Err(sqlx::Error::RowNotFound.into());
        }
        Ok(finalized)
    }

    async fn claim_next_due_turn(&self, now: &str) -> Result<Option<ClaimedTurnRow>, sqlx::Error> {
        let _write_permit = self.acquire_write_permit().await;
        sqlx::query_as::<_, ClaimedTurnRow>(CLAIM_NEXT_DUE_TURN_SQL)
            .bind(IngestTurnStatus::Processing)
            .bind(now)
            .bind(now)
            .bind(IngestTurnStatus::Finalized)
            .bind(now)
            .bind(IngestTurnStatus::Open)
            .bind(IngestTurnStatus::Finalized)
            .bind(IngestTurnStatus::Processing)
            .bind(IngestTurnStatus::Failed)
            .fetch_optional(self.pool())
            .await
    }

    async fn next_due_deadline(&self) -> Result<Option<Instant>, DispatcherError> {
        let due_at = sqlx::query_scalar::<_, String>(SELECT_NEXT_DUE_TURN_AT_SQL)
            .bind(IngestTurnStatus::Finalized)
            .bind(IngestTurnStatus::Open)
            .bind(IngestTurnStatus::Finalized)
            .bind(IngestTurnStatus::Processing)
            .bind(IngestTurnStatus::Failed)
            .fetch_optional(self.pool())
            .await?;
        due_at.as_deref().map(parse_retry_deadline).transpose()
    }

    async fn build_window(
        &self,
        turn: &ClaimedTurnRow,
    ) -> Result<ProjectedConversationWindow, ProcessTurnError> {
        let previous_turn_rows = if self.history_window_size == 0 {
            sqlx::query_scalar::<_, String>(SELECT_ALL_PREVIOUS_TURNS_SQL)
                .bind(&turn.conversation_id)
                .bind(IngestTurnStatus::Stored)
                .bind(turn.turn_index)
                .fetch_all(self.pool())
                .await?
        } else {
            sqlx::query_scalar::<_, String>(SELECT_PREVIOUS_TURNS_SQL)
                .bind(&turn.conversation_id)
                .bind(IngestTurnStatus::Stored)
                .bind(turn.turn_index)
                .bind(i64::from(self.history_window_size))
                .fetch_all(self.pool())
                .await?
        };

        let mut previous_turns = previous_turn_rows
            .into_iter()
            .map(|projection_json| {
                serde_json::from_str::<MemoryProjection>(&projection_json).map_err(|error| {
                    UnrecoverableProcessTurnError::Invariant(format!(
                        "stored prior projection for conversation '{}' is invalid JSON: {}",
                        turn.conversation_id, error
                    ))
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        previous_turns.reverse();

        let current_turn = serde_json::from_str::<MemoryProjection>(&turn.projection_json)
            .map_err(|error| {
                UnrecoverableProcessTurnError::Invariant(format!(
                    "claimed turn {} has invalid projection JSON: {}",
                    turn.id, error
                ))
            })?;
        Ok(ProjectedConversationWindow {
            previous_turns,
            current_turn,
        })
    }

    async fn mark_retryable_failure(
        &self,
        turn_id: i64,
        error: &str,
        attempt_count: i64,
        next_attempt_at: &str,
        now: &str,
    ) -> Result<(), DispatcherError> {
        let _write_permit = self.acquire_write_permit().await;
        let result = sqlx::query(MARK_TURN_RETRYABLE_SQL)
            .bind(IngestTurnStatus::Finalized)
            .bind(attempt_count)
            .bind(error)
            .bind(next_attempt_at)
            .bind(now)
            .bind(turn_id)
            .bind(IngestTurnStatus::Processing)
            .execute(self.pool())
            .await?;
        if result.rows_affected() != 1 {
            return Err(DispatcherError::Invariant(format!(
                "expected to reschedule processing turn {turn_id}, but no row was updated"
            )));
        }
        Ok(())
    }

    async fn mark_failed(
        &self,
        turn_id: i64,
        error: &str,
        now: &str,
    ) -> Result<(), DispatcherError> {
        let _write_permit = self.acquire_write_permit().await;
        let result = sqlx::query(MARK_TURN_FAILED_SQL)
            .bind(IngestTurnStatus::Failed)
            .bind(error)
            .bind(now)
            .bind(turn_id)
            .bind(IngestTurnStatus::Processing)
            .execute(self.pool())
            .await?;
        if result.rows_affected() != 1 {
            return Err(DispatcherError::Invariant(format!(
                "expected to fail processing turn {turn_id}, but no row was updated"
            )));
        }
        Ok(())
    }

    async fn recover_processing_turns(&self) -> Result<u64, sqlx::Error> {
        let _write_permit = self.acquire_write_permit().await;
        let now = Utc::now().to_rfc3339();
        let result = sqlx::query(RECOVER_PROCESSING_TURNS_SQL)
            .bind(IngestTurnStatus::Finalized)
            .bind("processing interrupted before completion")
            .bind(&now)
            .bind(&now)
            .bind(IngestTurnStatus::Processing)
            .execute(self.pool())
            .await?;
        Ok(result.rows_affected())
    }

    async fn load_turn_by_external_id(
        &self,
        tx: &mut Transaction<'_, sqlx::Sqlite>,
        conversation_id: &str,
        external_turn_id: &str,
    ) -> Result<Option<ExternalTurnRef>, IngestError> {
        Ok(
            sqlx::query_as::<_, ExternalTurnRef>(SELECT_TURN_BY_EXTERNAL_ID_SQL)
                .bind(conversation_id)
                .bind(external_turn_id)
                .fetch_optional(&mut **tx)
                .await?,
        )
    }

    async fn load_open_turn(
        &self,
        tx: &mut Transaction<'_, sqlx::Sqlite>,
        conversation_id: &str,
    ) -> Result<Option<TurnRef>, IngestError> {
        Ok(sqlx::query_as::<_, TurnRef>(SELECT_OPEN_TURN_SQL)
            .bind(conversation_id)
            .bind(IngestTurnStatus::Open)
            .fetch_optional(&mut **tx)
            .await?)
    }

    async fn load_existing_fragment(
        &self,
        fragment_id: &str,
    ) -> Result<ExistingFragmentRow, IngestError> {
        Ok(
            sqlx::query_as::<_, ExistingFragmentRow>(SELECT_EXISTING_FRAGMENT_SQL)
                .bind(fragment_id)
                .fetch_one(self.pool())
                .await?,
        )
    }

    async fn load_existing_fragment_in_tx(
        &self,
        tx: &mut Transaction<'_, sqlx::Sqlite>,
        fragment_id: &str,
    ) -> Result<Option<ExistingFragmentRow>, IngestError> {
        Ok(
            sqlx::query_as::<_, ExistingFragmentRow>(SELECT_EXISTING_FRAGMENT_SQL)
                .bind(fragment_id)
                .fetch_optional(&mut **tx)
                .await?,
        )
    }

    async fn create_turn(
        &self,
        tx: &mut Transaction<'_, sqlx::Sqlite>,
        conversation_id: &str,
        external_turn_id: Option<&str>,
        now: &str,
    ) -> Result<TurnRef, IngestError> {
        let turn_index: i64 = sqlx::query_scalar(SELECT_NEXT_TURN_INDEX_SQL)
            .bind(conversation_id)
            .fetch_one(&mut **tx)
            .await?;
        let projection_json = serde_json::to_string(&MemoryProjection::default())?;
        let result = sqlx::query(INSERT_INGEST_TURN_SQL)
            .bind(conversation_id)
            .bind(external_turn_id)
            .bind(turn_index)
            .bind(IngestTurnStatus::Open)
            .bind(projection_json)
            .bind(Terminality::None.as_str())
            .bind(now)
            .bind(now)
            .execute(&mut **tx)
            .await?;

        let turn_id = result.last_insert_rowid();
        debug!(
            turn_id,
            conversation_id = %conversation_id,
            turn_index,
            external_turn_id = external_turn_id.unwrap_or(""),
            "Created ingest turn"
        );

        Ok(TurnRef {
            id: turn_id,
            turn_index,
            terminality: StoredTerminality::None,
        })
    }

    async fn turn_has_fragments(
        &self,
        tx: &mut Transaction<'_, sqlx::Sqlite>,
        turn_id: i64,
    ) -> Result<bool, IngestError> {
        let count: i64 = sqlx::query_scalar(COUNT_TURN_FRAGMENTS_SQL)
            .bind(turn_id)
            .fetch_one(&mut **tx)
            .await?;
        Ok(count > 0)
    }

    async fn abort_turn(
        &self,
        tx: &mut Transaction<'_, sqlx::Sqlite>,
        turn_id: i64,
        now: &str,
    ) -> Result<(), IngestError> {
        sqlx::query(ABORT_TURN_SQL)
            .bind(IngestTurnStatus::Aborted)
            .bind(now)
            .bind(turn_id)
            .execute(&mut **tx)
            .await?;
        Ok(())
    }
}

struct IngestDispatcher {
    store: Arc<IngestStore>,
    memory: MemoryHandle,
    notify: Arc<Notify>,
}

impl IngestDispatcher {
    fn new(store: Arc<IngestStore>, memory: MemoryHandle, notify: Arc<Notify>) -> Self {
        Self {
            store,
            memory,
            notify,
        }
    }

    async fn run(self) {
        info!("Durable ingest dispatcher started");
        loop {
            match self.run_once().await {
                Ok(DispatchLoopState::ProcessedTurn) => continue,
                Ok(DispatchLoopState::WaitUntil(deadline)) => {
                    self.wait_for_work(deadline).await;
                }
                Err(error) => {
                    warn!(
                        error = %error,
                        "Ingest dispatcher hit an unexpected error; retrying after backoff"
                    );
                    if let Err(recovery_error) = self.store.recover_processing_turns().await {
                        warn!(
                            error = %recovery_error,
                            "Failed to recover in-flight turns after dispatcher error"
                        );
                    }
                    self.wait_for_work(Some(Instant::now() + DISPATCHER_ERROR_RETRY_DELAY))
                        .await;
                }
            }
        }
    }

    async fn run_once(&self) -> Result<DispatchLoopState, DispatcherError> {
        let now = Utc::now().to_rfc3339();
        let Some(turn) = self.store.claim_next_due_turn(&now).await? else {
            return Ok(DispatchLoopState::WaitUntil(
                self.store.next_due_deadline().await?,
            ));
        };

        self.process_claimed_turn(turn).await?;
        Ok(DispatchLoopState::ProcessedTurn)
    }

    async fn process_claimed_turn(&self, turn: ClaimedTurnRow) -> Result<(), DispatcherError> {
        let finalized_at = match parse_turn_timestamp(turn.finalized_at.as_deref()) {
            Ok(timestamp) => timestamp,
            Err(error) => {
                warn!(
                    turn_id = turn.id,
                    conversation_id = %turn.conversation_id,
                    turn_index = turn.turn_index,
                    error = %error,
                    "Claimed turn has invalid finalized timestamp; marking it failed"
                );
                self.store
                    .mark_failed(turn.id, &error.to_string(), &Utc::now().to_rfc3339())
                    .await?;
                return Ok(());
            }
        };
        let window = match self.store.build_window(&turn).await {
            Ok(window) => window,
            Err(error) => {
                warn!(
                    turn_id = turn.id,
                    conversation_id = %turn.conversation_id,
                    turn_index = turn.turn_index,
                    error = %error,
                    "Failed to rebuild conversation window for claimed turn; marking it failed"
                );
                self.store
                    .mark_failed(turn.id, &error.to_string(), &Utc::now().to_rfc3339())
                    .await?;
                return Ok(());
            }
        };

        info!(
            turn_id = turn.id,
            conversation_id = %turn.conversation_id,
            turn_index = turn.turn_index,
            attempt = turn.attempt_count + 1,
            history_turns = window.previous_turns.len(),
            "Dispatching finalized turn for memory extraction"
        );

        match self
            .memory
            .store_window(turn.id, window, finalized_at)
            .await
        {
            Ok(()) => Ok(()),
            Err(ProcessTurnError::Retryable(error)) => {
                let attempt_count = turn.attempt_count + 1;
                let next_attempt_at = compute_next_attempt_at(attempt_count);
                warn!(
                    turn_id = turn.id,
                    conversation_id = %turn.conversation_id,
                    turn_index = turn.turn_index,
                    attempt_count,
                    next_attempt_at = %next_attempt_at,
                    error = %error,
                    "Memory extraction failed; rescheduling turn"
                );
                self.store
                    .mark_retryable_failure(
                        turn.id,
                        &error.to_string(),
                        attempt_count,
                        &next_attempt_at,
                        &Utc::now().to_rfc3339(),
                    )
                    .await?;
                Ok(())
            }
            Err(ProcessTurnError::Unrecoverable(error)) => {
                warn!(
                    turn_id = turn.id,
                    conversation_id = %turn.conversation_id,
                    turn_index = turn.turn_index,
                    error = %error,
                    "Memory extraction failed permanently; marking turn failed"
                );
                self.store
                    .mark_failed(turn.id, &error.to_string(), &Utc::now().to_rfc3339())
                    .await?;
                Ok(())
            }
        }
    }

    async fn wait_for_work(&self, deadline: Option<Instant>) {
        match deadline {
            Some(deadline) => {
                let sleep = sleep_until(deadline);
                tokio::pin!(sleep);
                tokio::select! {
                    _ = self.notify.notified() => {}
                    _ = &mut sleep => {}
                }
            }
            None => {
                self.notify.notified().await;
            }
        }
    }
}

enum DispatchLoopState {
    ProcessedTurn,
    WaitUntil(Option<Instant>),
}

#[derive(Debug, Error)]
enum DispatcherError {
    #[error("database error: {0}")]
    Database(#[from] sqlx::Error),
    #[error("invalid scheduled retry timestamp '{0}'")]
    InvalidRetryTimestamp(String),
    #[error("dispatcher invariant violated: {0}")]
    Invariant(String),
}

struct SchemaConstraintRequirement {
    description: &'static str,
    sql_fragment: &'static str,
}

async fn initialize_ingest_schema(pool: &SqlitePool) -> Result<(), AppError> {
    for statement in [
        CREATE_INGEST_TURNS_TABLE_SQL,
        CREATE_INGEST_FRAGMENTS_TABLE_SQL,
    ] {
        sqlx::query(statement).execute(pool).await?;
    }

    validate_ingest_schema_constraints(pool).await?;
    validate_ingest_enum_values(pool).await?;

    for statement in [
        CREATE_INGEST_TURNS_INDEX_SQL,
        CREATE_INGEST_TURNS_EXTERNAL_INDEX_SQL,
        CREATE_INGEST_TURNS_OPEN_INDEX_SQL,
        CREATE_INGEST_TURNS_SCHEDULE_INDEX_SQL,
        CREATE_INGEST_FRAGMENTS_TURN_INDEX_SQL,
        CREATE_INGEST_FRAGMENTS_CONVERSATION_INDEX_SQL,
    ] {
        sqlx::query(statement).execute(pool).await?;
    }

    Ok(())
}

async fn validate_ingest_schema_constraints(pool: &SqlitePool) -> Result<(), AppError> {
    validate_table_schema_constraints(pool, "ingest_turns", INGEST_TURNS_SCHEMA_REQUIREMENTS)
        .await?;
    validate_table_schema_constraints(
        pool,
        "ingest_fragments",
        INGEST_FRAGMENTS_SCHEMA_REQUIREMENTS,
    )
    .await
}

async fn validate_table_schema_constraints(
    pool: &SqlitePool,
    table_name: &str,
    requirements: &[SchemaConstraintRequirement],
) -> Result<(), AppError> {
    let schema_sql = sqlx::query_scalar::<_, String>(SELECT_TABLE_SCHEMA_SQL)
        .bind(table_name)
        .fetch_optional(pool)
        .await?
        .ok_or_else(|| {
            AppError::Schema(format!(
                "Existing ingest schema is unsupported: missing required table '{table_name}'. Recreate the database or migrate externally."
            ))
        })?;
    let compact_schema = compact_sql(&schema_sql);

    for requirement in requirements {
        if !compact_schema.contains(requirement.sql_fragment) {
            return Err(AppError::Schema(format!(
                "Existing ingest schema is unsupported: table '{table_name}' is missing the {}. Recreate the database or migrate externally.",
                requirement.description
            )));
        }
    }

    Ok(())
}

async fn validate_ingest_enum_values(pool: &SqlitePool) -> Result<(), AppError> {
    validate_invalid_turn_status(pool).await?;
    validate_invalid_turn_terminality(pool).await?;
    validate_invalid_fragment_kind(pool).await?;
    validate_invalid_fragment_terminality(pool).await
}

async fn validate_invalid_turn_status(pool: &SqlitePool) -> Result<(), AppError> {
    let invalid = sqlx::query_scalar::<_, String>(SELECT_INVALID_TURN_STATUS_SQL)
        .bind(IngestTurnStatus::Open)
        .bind(IngestTurnStatus::Finalized)
        .bind(IngestTurnStatus::Processing)
        .bind(IngestTurnStatus::Stored)
        .bind(IngestTurnStatus::Aborted)
        .bind(IngestTurnStatus::Failed)
        .fetch_optional(pool)
        .await?;

    validate_invalid_value("ingest_turns.status", invalid)
}

async fn validate_invalid_turn_terminality(pool: &SqlitePool) -> Result<(), AppError> {
    let invalid = sqlx::query_scalar::<_, String>(SELECT_INVALID_TURN_TERMINALITY_SQL)
        .bind(Terminality::None.as_str())
        .bind(Terminality::Soft.as_str())
        .bind(Terminality::Hard.as_str())
        .fetch_optional(pool)
        .await?;

    validate_invalid_value("ingest_turns.terminality", invalid)
}

async fn validate_invalid_fragment_kind(pool: &SqlitePool) -> Result<(), AppError> {
    let invalid = sqlx::query_scalar::<_, String>(SELECT_INVALID_FRAGMENT_KIND_SQL)
        .bind(IngestFragmentKind::UserMessage)
        .bind(IngestFragmentKind::AssistantMessage)
        .bind(IngestFragmentKind::ToolCall)
        .bind(IngestFragmentKind::ToolResult)
        .bind(IngestFragmentKind::Thinking)
        .bind(IngestFragmentKind::Other)
        .fetch_optional(pool)
        .await?;

    validate_invalid_value("ingest_fragments.kind", invalid)
}

async fn validate_invalid_fragment_terminality(pool: &SqlitePool) -> Result<(), AppError> {
    let invalid = sqlx::query_scalar::<_, String>(SELECT_INVALID_FRAGMENT_TERMINALITY_SQL)
        .bind(Terminality::None.as_str())
        .bind(Terminality::Soft.as_str())
        .bind(Terminality::Hard.as_str())
        .fetch_optional(pool)
        .await?;

    validate_invalid_value("ingest_fragments.terminality", invalid)
}

fn validate_invalid_value(column_name: &str, invalid: Option<String>) -> Result<(), AppError> {
    if let Some(value) = invalid {
        return Err(AppError::Schema(format!(
            "Existing ingest schema contains unsupported value '{value}' in {column_name}. Recreate the database or migrate externally."
        )));
    }

    Ok(())
}

fn compact_sql(sql: &str) -> String {
    sql.chars()
        .filter(|ch| !ch.is_whitespace())
        .flat_map(char::to_lowercase)
        .collect()
}

#[derive(Debug)]
struct StagedFragment {
    turn_index: i64,
    duplicate: bool,
    finalized: bool,
}

#[derive(Debug, sqlx::FromRow)]
struct TurnRef {
    id: i64,
    turn_index: i64,
    terminality: StoredTerminality,
}

#[derive(Debug, sqlx::FromRow)]
struct ExternalTurnRef {
    id: i64,
    turn_index: i64,
    status: IngestTurnStatus,
    terminality: StoredTerminality,
}

impl ExternalTurnRef {
    fn into_turn_ref(self) -> TurnRef {
        TurnRef {
            id: self.id,
            turn_index: self.turn_index,
            terminality: self.terminality,
        }
    }
}

#[derive(Debug, sqlx::FromRow)]
struct StoredFragmentRow {
    fragment_json: String,
}

#[derive(Debug, sqlx::FromRow)]
struct ClaimedTurnRow {
    id: i64,
    conversation_id: String,
    turn_index: i64,
    projection_json: String,
    finalized_at: Option<String>,
    attempt_count: i64,
}

#[derive(Debug, sqlx::FromRow)]
struct ExistingFragmentRow {
    turn_index: i64,
    status: IngestTurnStatus,
}

fn staged_fragment_from_existing(existing: ExistingFragmentRow) -> StagedFragment {
    StagedFragment {
        turn_index: existing.turn_index,
        duplicate: true,
        finalized: matches!(
            existing.status,
            IngestTurnStatus::Finalized
                | IngestTurnStatus::Processing
                | IngestTurnStatus::Stored
                | IngestTurnStatus::Failed
        ),
    }
}

#[derive(Debug)]
struct ReducedProjection {
    projection: MemoryProjection,
    terminality: Terminality,
}

fn apply_fragment_to_projection(
    projection: &mut MemoryProjection,
    terminality: &mut Terminality,
    fragment: ConversationFragment,
) {
    if fragment.terminality.rank() > terminality.rank() {
        *terminality = fragment.terminality;
    }

    match fragment.body {
        FragmentBody::UserMessage { text } => {
            append_message_fragment(&mut projection.user_message, text);
        }
        FragmentBody::AssistantMessage { text } => {
            append_message_fragment(&mut projection.assistant_reply, text);
        }
        FragmentBody::ToolCall {
            name, input_json, ..
        } => {
            projection.steps.push(MemoryStep::ToolCall {
                name,
                input: input_json,
            });
        }
        FragmentBody::ToolResult {
            name, output_json, ..
        } => {
            projection.steps.push(MemoryStep::ToolResult {
                name,
                output: output_json,
            });
        }
        FragmentBody::Thinking { text } => {
            projection.steps.push(MemoryStep::Thinking { text });
        }
        FragmentBody::Other { .. } => {}
    }
}

fn append_message_fragment(target: &mut String, fragment: String) {
    if fragment.is_empty() {
        return;
    }

    if target.is_empty() {
        *target = fragment;
        return;
    }

    target.push_str("\n\n");
    target.push_str(&fragment);
}

const fn merge_terminality(current: Terminality, next: Terminality) -> Terminality {
    if next.rank() > current.rank() {
        next
    } else {
        current
    }
}

fn compute_next_attempt_at(attempt_count: i64) -> String {
    let delay = retry_delay_for_attempt(attempt_count, RETRY_BASE_DELAY, RETRY_MAX_DELAY);
    (Utc::now() + chrono::Duration::from_std(delay).unwrap_or(chrono::Duration::MAX)).to_rfc3339()
}

fn parse_turn_timestamp(value: Option<&str>) -> Result<DateTime<Utc>, ProcessTurnError> {
    let value = value.ok_or_else(|| {
        UnrecoverableProcessTurnError::Invariant(
            "claimed processing turn is missing finalized_at".to_string(),
        )
    })?;
    DateTime::parse_from_rfc3339(value)
        .map(|timestamp| timestamp.with_timezone(&Utc))
        .map_err(|error| {
            UnrecoverableProcessTurnError::Invariant(format!(
                "turn finalized_at '{value}' is invalid: {error}"
            ))
            .into()
        })
}

fn stage_retry_strategy() -> impl Iterator<Item = Duration> {
    exponential_backoff_strategy(
        Duration::from_millis(STAGE_RETRY_BASE_DELAY_MS),
        STAGE_RETRY_MAX_DELAY,
    )
    .take(MAX_STAGE_RETRIES)
}

fn retry_delay_for_attempt(
    attempt_number: i64,
    base_delay: Duration,
    max_delay: Duration,
) -> Duration {
    let attempt_index = usize::try_from(attempt_number.saturating_sub(1)).unwrap_or(usize::MAX);
    exponential_backoff_strategy(base_delay, max_delay)
        .nth(attempt_index)
        .unwrap_or(max_delay)
}

fn exponential_backoff_strategy(base_delay: Duration, max_delay: Duration) -> ExponentialBackoff {
    let factor = std::cmp::max(
        1,
        u64::try_from(base_delay.as_millis().saturating_div(2)).unwrap_or(u64::MAX),
    );
    ExponentialBackoff::from_millis(2)
        .factor(factor)
        .max_delay(max_delay)
}

fn parse_retry_deadline(value: &str) -> Result<Instant, DispatcherError> {
    let due_at = DateTime::parse_from_rfc3339(value)
        .map(|timestamp| timestamp.with_timezone(&Utc))
        .map_err(|_| DispatcherError::InvalidRetryTimestamp(value.to_string()))?;
    let now = Utc::now();
    let wait = (due_at - now)
        .to_std()
        .unwrap_or_else(|_| Duration::from_secs(0));
    Ok(Instant::now() + wait)
}

fn validate_runtime_constraints(_envelope: &IngestEnvelope) -> Result<(), IngestError> {
    Ok(())
}

fn should_retry_stage_error(error: &IngestError) -> bool {
    let IngestError::Database(database_error) = error else {
        return false;
    };

    is_unique_violation(database_error) || is_sqlite_lock_contention(database_error)
}

fn sqlite_error_code(error: &sqlx::Error) -> Option<i32> {
    let sqlx::Error::Database(db_error) = error else {
        return None;
    };

    db_error.code()?.parse().ok()
}

fn sqlite_base_error_code(code: i32) -> i32 {
    code & 0xff
}

fn is_unique_violation(error: &sqlx::Error) -> bool {
    matches!(sqlite_error_code(error), Some(1555 | 2067))
}

fn is_sqlite_lock_contention(error: &sqlx::Error) -> bool {
    sqlite_error_code(error)
        .map(sqlite_base_error_code)
        .is_some_and(|code| matches!(code, SQLITE_BUSY | SQLITE_LOCKED))
}

const CREATE_INGEST_TURNS_TABLE_SQL: &str = "CREATE TABLE IF NOT EXISTS ingest_turns (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    conversation_id TEXT NOT NULL,
    external_turn_id TEXT,
    turn_index INTEGER NOT NULL,
    status TEXT NOT NULL CHECK(status IN ('open','finalized','processing','stored','aborted','failed')),
    projection_json TEXT NOT NULL,
    terminality TEXT NOT NULL DEFAULT 'none' CHECK(terminality IN ('none','soft','hard')),
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    finalized_at TEXT,
    attempt_count INTEGER NOT NULL DEFAULT 0 CHECK(attempt_count >= 0),
    last_error TEXT,
    next_attempt_at TEXT,
    processing_started_at TEXT,
    stored_at TEXT,
    CHECK(
        (status = 'open' AND finalized_at IS NULL AND next_attempt_at IS NULL AND processing_started_at IS NULL AND stored_at IS NULL) OR
        (status = 'finalized' AND finalized_at IS NOT NULL AND next_attempt_at IS NOT NULL AND processing_started_at IS NULL AND stored_at IS NULL) OR
        (status = 'processing' AND finalized_at IS NOT NULL AND next_attempt_at IS NOT NULL AND processing_started_at IS NOT NULL AND stored_at IS NULL) OR
        (status = 'stored' AND finalized_at IS NOT NULL AND next_attempt_at IS NULL AND processing_started_at IS NULL AND stored_at IS NOT NULL) OR
        (status = 'aborted' AND finalized_at IS NULL AND next_attempt_at IS NULL AND processing_started_at IS NULL AND stored_at IS NULL) OR
        (status = 'failed' AND finalized_at IS NOT NULL AND next_attempt_at IS NULL AND processing_started_at IS NULL AND stored_at IS NULL)
    )
);";

const CREATE_INGEST_TURNS_INDEX_SQL: &str =
    "CREATE UNIQUE INDEX IF NOT EXISTS idx_ingest_turns_conversation_turn
    ON ingest_turns (conversation_id, turn_index);";
const CREATE_INGEST_TURNS_EXTERNAL_INDEX_SQL: &str =
    "CREATE UNIQUE INDEX IF NOT EXISTS idx_ingest_turns_external
    ON ingest_turns (conversation_id, external_turn_id)
    WHERE external_turn_id IS NOT NULL;";
const CREATE_INGEST_TURNS_OPEN_INDEX_SQL: &str =
    "CREATE UNIQUE INDEX IF NOT EXISTS idx_ingest_turns_single_open
    ON ingest_turns (conversation_id)
    WHERE status = 'open' AND external_turn_id IS NULL;";
const CREATE_INGEST_TURNS_SCHEDULE_INDEX_SQL: &str =
    "CREATE INDEX IF NOT EXISTS idx_ingest_turns_dispatch_schedule
    ON ingest_turns (status, next_attempt_at, id);";

const CREATE_INGEST_FRAGMENTS_TABLE_SQL: &str = "CREATE TABLE IF NOT EXISTS ingest_fragments (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    fragment_id TEXT NOT NULL UNIQUE,
    conversation_id TEXT NOT NULL,
    turn_id INTEGER NOT NULL,
    agent TEXT NOT NULL,
    event TEXT NOT NULL,
    kind TEXT NOT NULL CHECK(kind IN ('user_message','assistant_message','tool_call','tool_result','thinking','other')),
    terminality TEXT NOT NULL CHECK(terminality IN ('none','soft','hard')),
    sequence_hint INTEGER,
    emitted_at TEXT,
    fragment_json TEXT NOT NULL,
    raw_json TEXT NOT NULL,
    received_at TEXT NOT NULL,
    FOREIGN KEY (turn_id) REFERENCES ingest_turns(id) ON DELETE CASCADE
);";
const CREATE_INGEST_FRAGMENTS_TURN_INDEX_SQL: &str =
    "CREATE INDEX IF NOT EXISTS idx_ingest_fragments_turn ON ingest_fragments (turn_id, id);";
const CREATE_INGEST_FRAGMENTS_CONVERSATION_INDEX_SQL: &str =
    "CREATE INDEX IF NOT EXISTS idx_ingest_fragments_conversation
    ON ingest_fragments (conversation_id, turn_id, id);";

const SELECT_TURN_BY_EXTERNAL_ID_SQL: &str = "SELECT id, turn_index, status, terminality
    FROM ingest_turns
    WHERE conversation_id = ? AND external_turn_id = ?
    LIMIT 1;";
const SELECT_OPEN_TURN_SQL: &str = "SELECT id, turn_index, terminality
    FROM ingest_turns
    WHERE conversation_id = ? AND status = ? AND external_turn_id IS NULL
    LIMIT 1;";
const SELECT_EXISTING_FRAGMENT_SQL: &str = "SELECT t.turn_index, t.status
    FROM ingest_fragments f
    JOIN ingest_turns t ON t.id = f.turn_id
    WHERE f.fragment_id = ?
    LIMIT 1;";
const SELECT_NEXT_TURN_INDEX_SQL: &str =
    "SELECT COALESCE(MAX(turn_index), 0) + 1 FROM ingest_turns WHERE conversation_id = ?;";
const INSERT_INGEST_TURN_SQL: &str = "INSERT INTO ingest_turns (
        conversation_id,
        external_turn_id,
        turn_index,
        status,
        projection_json,
        terminality,
        created_at,
        updated_at
    ) VALUES (?, ?, ?, ?, ?, ?, ?, ?);";
const COUNT_TURN_FRAGMENTS_SQL: &str = "SELECT COUNT(*) FROM ingest_fragments WHERE turn_id = ?;";
const ABORT_TURN_SQL: &str = "UPDATE ingest_turns
    SET status = ?,
        updated_at = ?,
        next_attempt_at = NULL,
        processing_started_at = NULL,
        stored_at = NULL
    WHERE id = ?;";

const INSERT_INGEST_FRAGMENT_SQL: &str = "INSERT INTO ingest_fragments (
        fragment_id,
        conversation_id,
        turn_id,
        agent,
        event,
        kind,
        terminality,
        sequence_hint,
        emitted_at,
        fragment_json,
        raw_json,
        received_at
    ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
    ON CONFLICT(fragment_id) DO NOTHING;";

const SELECT_TURN_FRAGMENTS_SQL: &str = "SELECT fragment_json
    FROM ingest_fragments
    WHERE turn_id = ?
    ORDER BY CASE WHEN sequence_hint IS NULL THEN 1 ELSE 0 END, sequence_hint, id;";

const UPDATE_OPEN_TURN_AFTER_INSERT_SQL: &str = "UPDATE ingest_turns
    SET terminality = ?,
        updated_at = ?
    WHERE id = ? AND status = ?;";

const UPDATE_TURN_AFTER_REDUCE_SQL: &str = "UPDATE ingest_turns
    SET projection_json = ?,
        terminality = ?,
        status = ?,
        finalized_at = ?,
        next_attempt_at = ?,
        last_error = NULL,
        processing_started_at = NULL,
        stored_at = NULL,
        updated_at = ?
    WHERE id = ? AND status = ?;";

const CLAIM_NEXT_DUE_TURN_SQL: &str = "UPDATE ingest_turns
    SET status = ?,
        processing_started_at = ?,
        updated_at = ?
    WHERE id = (
        SELECT candidate.id
        FROM ingest_turns candidate
        WHERE candidate.status = ?
          AND candidate.next_attempt_at <= ?
          AND NOT EXISTS (
              SELECT 1
              FROM ingest_turns blocker
              WHERE blocker.conversation_id = candidate.conversation_id
                AND blocker.turn_index < candidate.turn_index
                AND blocker.status IN (?, ?, ?, ?)
          )
        ORDER BY candidate.next_attempt_at ASC, candidate.id ASC
        LIMIT 1
    )
    RETURNING id, conversation_id, turn_index, projection_json, finalized_at, attempt_count;";

const SELECT_NEXT_DUE_TURN_AT_SQL: &str = "SELECT candidate.next_attempt_at
    FROM ingest_turns candidate
    WHERE candidate.status = ?
      AND NOT EXISTS (
          SELECT 1
          FROM ingest_turns blocker
          WHERE blocker.conversation_id = candidate.conversation_id
            AND blocker.turn_index < candidate.turn_index
            AND blocker.status IN (?, ?, ?, ?)
      )
    ORDER BY candidate.next_attempt_at ASC, candidate.id ASC
    LIMIT 1;";

const SELECT_ALL_PREVIOUS_TURNS_SQL: &str = "SELECT projection_json
    FROM ingest_turns
    WHERE conversation_id = ?
      AND status = ?
      AND turn_index < ?
    ORDER BY turn_index DESC;";

const SELECT_PREVIOUS_TURNS_SQL: &str = "SELECT projection_json
    FROM ingest_turns
    WHERE conversation_id = ?
      AND status = ?
      AND turn_index < ?
    ORDER BY turn_index DESC
    LIMIT ?;";

const MARK_TURN_RETRYABLE_SQL: &str = "UPDATE ingest_turns
    SET status = ?,
        attempt_count = ?,
        last_error = ?,
        next_attempt_at = ?,
        processing_started_at = NULL,
        stored_at = NULL,
        updated_at = ?
    WHERE id = ? AND status = ?;";

const MARK_TURN_FAILED_SQL: &str = "UPDATE ingest_turns
    SET status = ?,
        last_error = ?,
        next_attempt_at = NULL,
        processing_started_at = NULL,
        stored_at = NULL,
        updated_at = ?
    WHERE id = ? AND status = ?;";

const RECOVER_PROCESSING_TURNS_SQL: &str = "UPDATE ingest_turns
    SET status = ?,
        attempt_count = attempt_count + 1,
        last_error = COALESCE(last_error, ?),
        next_attempt_at = ?,
        processing_started_at = NULL,
        stored_at = NULL,
        updated_at = ?
    WHERE status = ?;";

const SELECT_TABLE_SCHEMA_SQL: &str =
    "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = ? LIMIT 1;";
const SELECT_INVALID_TURN_STATUS_SQL: &str =
    "SELECT status FROM ingest_turns WHERE status NOT IN (?, ?, ?, ?, ?, ?) LIMIT 1;";
const SELECT_INVALID_TURN_TERMINALITY_SQL: &str =
    "SELECT terminality FROM ingest_turns WHERE terminality NOT IN (?, ?, ?) LIMIT 1;";
const SELECT_INVALID_FRAGMENT_KIND_SQL: &str =
    "SELECT kind FROM ingest_fragments WHERE kind NOT IN (?, ?, ?, ?, ?, ?) LIMIT 1;";
const SELECT_INVALID_FRAGMENT_TERMINALITY_SQL: &str =
    "SELECT terminality FROM ingest_fragments WHERE terminality NOT IN (?, ?, ?) LIMIT 1;";

const INGEST_TURNS_SCHEMA_REQUIREMENTS: &[SchemaConstraintRequirement] = &[
    SchemaConstraintRequirement {
        description: "status CHECK constraint",
        sql_fragment: "check(statusin('open','finalized','processing','stored','aborted','failed'))",
    },
    SchemaConstraintRequirement {
        description: "terminality CHECK constraint",
        sql_fragment: "check(terminalityin('none','soft','hard'))",
    },
    SchemaConstraintRequirement {
        description: "attempt_count column",
        sql_fragment: "attempt_countintegernotnulldefault0check(attempt_count>=0)",
    },
    SchemaConstraintRequirement {
        description: "last_error column",
        sql_fragment: "last_errortext",
    },
    SchemaConstraintRequirement {
        description: "next_attempt_at column",
        sql_fragment: "next_attempt_attext",
    },
    SchemaConstraintRequirement {
        description: "processing_started_at column",
        sql_fragment: "processing_started_attext",
    },
    SchemaConstraintRequirement {
        description: "stored_at column",
        sql_fragment: "stored_attext",
    },
];

const INGEST_FRAGMENTS_SCHEMA_REQUIREMENTS: &[SchemaConstraintRequirement] = &[
    SchemaConstraintRequirement {
        description: "kind CHECK constraint",
        sql_fragment: "check(kindin('user_message','assistant_message','tool_call','tool_result','thinking','other'))",
    },
    SchemaConstraintRequirement {
        description: "terminality CHECK constraint",
        sql_fragment: "check(terminalityin('none','soft','hard'))",
    },
];

#[cfg(test)]
mod tests {
    use super::{
        ClaimedTurnRow, INGEST_FRAGMENTS_SCHEMA_REQUIREMENTS, INGEST_TURNS_SCHEMA_REQUIREMENTS,
        IngestDispatcher, IngestFragmentKind, IngestService, IngestStore, IngestTurnStatus,
        SELECT_TABLE_SCHEMA_SQL, StoredTerminality, Terminality, apply_fragment_to_projection,
        compact_sql, should_retry_stage_error, validate_runtime_constraints,
    };
    use crate::actor::{
        MemoryHandle, ProcessTurnError, RetryableProcessTurnError, TestStoreTurnRequest,
    };
    use crate::db::{MemoryDb, SQLITE_BUSY_TIMEOUT_MS, SQLITE_MMAP_SIZE_BYTES, SqliteRuntime};
    use crate::error::AppError;
    use crate::memory_window::{MemoryProjection, MemoryStep};
    use memory_bank_protocol::{
        ConversationFragment, ConversationScope, FragmentBody, INGEST_PROTOCOL_VERSION,
        IngestEnvelope, SourceMeta,
    };
    use serde_json::json;
    use sqlx::SqlitePool;
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
    use std::path::{Path, PathBuf};
    use std::str::FromStr;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};
    use tokio::sync::Notify;
    use tokio::time::{Duration, timeout};

    const LEGACY_INGEST_TURNS_TABLE_SQL: &str = "CREATE TABLE ingest_turns (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        conversation_id TEXT NOT NULL,
        external_turn_id TEXT,
        turn_index INTEGER NOT NULL,
        status TEXT NOT NULL,
        projection_json TEXT NOT NULL,
        terminality TEXT NOT NULL DEFAULT 'none',
        created_at TEXT NOT NULL,
        updated_at TEXT NOT NULL,
        finalized_at TEXT
    );";

    const LEGACY_INGEST_FRAGMENTS_TABLE_SQL: &str = "CREATE TABLE ingest_fragments (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        fragment_id TEXT NOT NULL UNIQUE,
        conversation_id TEXT NOT NULL,
        turn_id INTEGER NOT NULL,
        agent TEXT NOT NULL,
        event TEXT NOT NULL,
        kind TEXT NOT NULL,
        terminality TEXT NOT NULL,
        sequence_hint INTEGER,
        emitted_at TEXT,
        fragment_json TEXT NOT NULL,
        raw_json TEXT NOT NULL,
        received_at TEXT NOT NULL,
        FOREIGN KEY (turn_id) REFERENCES ingest_turns(id) ON DELETE CASCADE
    );";

    static UNIQUE_COUNTER: AtomicU64 = AtomicU64::new(0);
    const TEST_HISTORY_WINDOW_SIZE: u32 = 0;

    #[derive(Debug, sqlx::FromRow)]
    struct TurnStatusRow {
        status: IngestTurnStatus,
    }

    #[derive(Debug, sqlx::FromRow)]
    struct FragmentKindRow {
        kind: IngestFragmentKind,
    }

    #[derive(Debug, sqlx::FromRow)]
    struct TurnStateRow {
        status: IngestTurnStatus,
        attempt_count: i64,
        last_error: Option<String>,
        next_attempt_at: Option<String>,
        processing_started_at: Option<String>,
        stored_at: Option<String>,
    }

    #[derive(Debug, sqlx::FromRow)]
    struct TurnProjectionRow {
        id: i64,
        status: IngestTurnStatus,
        terminality: StoredTerminality,
        projection_json: String,
    }

    #[test]
    fn ingest_turn_status_round_trips_strings() {
        for status in [
            IngestTurnStatus::Open,
            IngestTurnStatus::Finalized,
            IngestTurnStatus::Processing,
            IngestTurnStatus::Stored,
            IngestTurnStatus::Aborted,
            IngestTurnStatus::Failed,
        ] {
            assert_eq!(status.to_string(), status.as_str());
            assert_eq!(status.as_str().parse::<IngestTurnStatus>().unwrap(), status);
        }
    }

    #[test]
    fn ingest_turn_status_rejects_invalid_strings() {
        assert!("done".parse::<IngestTurnStatus>().is_err());
    }

    #[test]
    fn ingest_fragment_kind_round_trips_strings() {
        for kind in [
            IngestFragmentKind::UserMessage,
            IngestFragmentKind::AssistantMessage,
            IngestFragmentKind::ToolCall,
            IngestFragmentKind::ToolResult,
            IngestFragmentKind::Thinking,
            IngestFragmentKind::Other,
        ] {
            assert_eq!(kind.to_string(), kind.as_str());
            assert_eq!(kind.as_str().parse::<IngestFragmentKind>().unwrap(), kind);
        }
    }

    #[test]
    fn ingest_fragment_kind_rejects_invalid_strings() {
        assert!("message".parse::<IngestFragmentKind>().is_err());
    }

    #[test]
    fn retry_delay_uses_tokio_retry_exponential_backoff() {
        assert_eq!(
            super::retry_delay_for_attempt(1, Duration::from_millis(10), Duration::from_millis(80)),
            Duration::from_millis(10)
        );
        assert_eq!(
            super::retry_delay_for_attempt(2, Duration::from_millis(10), Duration::from_millis(80)),
            Duration::from_millis(20)
        );
        assert_eq!(
            super::retry_delay_for_attempt(3, Duration::from_millis(10), Duration::from_millis(80)),
            Duration::from_millis(40)
        );
        assert_eq!(
            super::retry_delay_for_attempt(4, Duration::from_millis(10), Duration::from_millis(80)),
            Duration::from_millis(80)
        );
        assert_eq!(
            super::retry_delay_for_attempt(5, Duration::from_millis(10), Duration::from_millis(80)),
            Duration::from_millis(80)
        );
    }

    #[tokio::test]
    async fn sqlx_decodes_typed_turn_status_rows() {
        let pool = open_memory_pool().await;

        let row = sqlx::query_as::<_, TurnStatusRow>("SELECT 'finalized' AS status")
            .fetch_one(&pool)
            .await
            .expect("row");

        assert_eq!(row.status, IngestTurnStatus::Finalized);
    }

    #[tokio::test]
    async fn sqlx_rejects_invalid_turn_status_rows() {
        let pool = open_memory_pool().await;

        assert!(
            sqlx::query_as::<_, TurnStatusRow>("SELECT 'invalid' AS status")
                .fetch_one(&pool)
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn sqlx_decodes_typed_fragment_kind_rows() {
        let pool = open_memory_pool().await;

        let row = sqlx::query_as::<_, FragmentKindRow>("SELECT 'tool_result' AS kind")
            .fetch_one(&pool)
            .await
            .expect("row");

        assert_eq!(row.kind, IngestFragmentKind::ToolResult);
    }

    #[tokio::test]
    async fn sqlx_rejects_invalid_fragment_kind_rows() {
        let pool = open_memory_pool().await;

        assert!(
            sqlx::query_as::<_, FragmentKindRow>("SELECT 'invalid' AS kind")
                .fetch_one(&pool)
                .await
                .is_err()
        );
    }

    #[test]
    fn repeated_message_fragments_are_appended_in_projection_order() {
        let mut projection = MemoryProjection::default();
        let mut terminality = Terminality::None;

        for fragment in [
            ConversationFragment {
                terminality: Terminality::None,
                body: FragmentBody::UserMessage {
                    text: "first question".to_string(),
                },
            },
            ConversationFragment {
                terminality: Terminality::Soft,
                body: FragmentBody::UserMessage {
                    text: "follow-up detail".to_string(),
                },
            },
            ConversationFragment {
                terminality: Terminality::None,
                body: FragmentBody::AssistantMessage {
                    text: "first answer".to_string(),
                },
            },
            ConversationFragment {
                terminality: Terminality::Hard,
                body: FragmentBody::AssistantMessage {
                    text: "final answer".to_string(),
                },
            },
        ] {
            apply_fragment_to_projection(&mut projection, &mut terminality, fragment);
        }

        assert_eq!(
            projection.user_message,
            "first question\n\nfollow-up detail"
        );
        assert_eq!(projection.assistant_reply, "first answer\n\nfinal answer");
        assert_eq!(terminality, Terminality::Hard);
    }

    #[test]
    fn runtime_constraints_allow_turn_ids() {
        let envelope = IngestEnvelope {
            protocol_version: INGEST_PROTOCOL_VERSION,
            source: SourceMeta {
                agent: "claude-code".to_string(),
                event: "Stop".to_string(),
            },
            scope: ConversationScope {
                conversation_id: "session-1".to_string(),
                turn_id: Some("turn-1".to_string()),
                fragment_id: "fragment-1".to_string(),
                sequence_hint: None,
                emitted_at_rfc3339: None,
            },
            fragment: ConversationFragment {
                terminality: Terminality::Hard,
                body: FragmentBody::AssistantMessage {
                    text: "Done.".to_string(),
                },
            },
            raw: json!({"session_id": "session-1"}),
        };

        validate_runtime_constraints(&envelope).expect("turn_id should be accepted");
    }

    #[tokio::test]
    async fn open_creates_constrained_schema_for_fresh_database() {
        let db_path = test_db_path("fresh");
        let store = IngestStore::open(&db_path, TEST_HISTORY_WINDOW_SIZE)
            .await
            .expect("open store");

        let turns_sql = load_table_sql(store.pool(), "ingest_turns").await;
        let fragments_sql = load_table_sql(store.pool(), "ingest_fragments").await;
        assert_schema_requirements(&turns_sql, INGEST_TURNS_SCHEMA_REQUIREMENTS);
        assert_schema_requirements(&fragments_sql, INGEST_FRAGMENTS_SCHEMA_REQUIREMENTS);
    }

    #[tokio::test]
    async fn writer_gate_blocks_writes_but_still_allows_reads() {
        let db_path = test_db_path("writer_gate");
        let runtime = SqliteRuntime::open_file_for_tests(&db_path, 2)
            .await
            .expect("open sqlite runtime");
        let store = Arc::new(
            IngestStore::open_with_runtime(runtime.clone(), TEST_HISTORY_WINDOW_SIZE)
                .await
                .expect("open store"),
        );
        let db = MemoryDb::open_with_runtime_for_tests(runtime.clone(), 2)
            .await
            .expect("memory db");
        let memory_id = insert_memory_note(&db, "seed memory").await;

        let write_permit = runtime.acquire_write_permit().await;
        let mut write_attempts = runtime.install_write_attempt_notifier();
        let tx = runtime
            .pool()
            .begin_with("BEGIN IMMEDIATE")
            .await
            .expect("begin immediate");

        let store_for_write = store.clone();
        let blocked_write =
            tokio::spawn(async move { store_for_write.recover_processing_turns().await });
        wait_for_write_attempts(&mut write_attempts, 1).await;

        let memories = db.get_memories(&[memory_id]).await.expect("read memories");
        assert_eq!(memories.len(), 1, "read should use the second connection");
        tokio::task::yield_now().await;
        assert!(
            !blocked_write.is_finished(),
            "write should wait for the shared gate"
        );

        tx.commit().await.expect("commit transaction");
        drop(write_permit);

        let recovered = blocked_write
            .await
            .expect("join blocked write")
            .expect("recover processing turns");
        assert_eq!(recovered, 0);
    }

    #[tokio::test]
    async fn concurrent_external_fragments_preserve_a_single_coherent_projection() {
        let db_path = test_db_path("concurrent_external_fragments");
        let runtime = SqliteRuntime::open_file_for_tests(&db_path, 2)
            .await
            .expect("open sqlite runtime");
        let store = Arc::new(
            IngestStore::open_with_runtime(runtime.clone(), TEST_HISTORY_WINDOW_SIZE)
                .await
                .expect("open store"),
        );
        let write_permit = runtime.acquire_write_permit().await;
        let mut write_attempts = runtime.install_write_attempt_notifier();

        let user_store = store.clone();
        let user_task = tokio::spawn(async move {
            let envelope =
                external_user_envelope("session-concurrent", "turn-1", "fragment-1", "hello");
            user_store.stage_fragment(&envelope).await
        });
        wait_for_write_attempts(&mut write_attempts, 1).await;

        let assistant_store = store.clone();
        let assistant_task = tokio::spawn(async move {
            let envelope = external_tool_call_envelope(
                "session-concurrent",
                "turn-1",
                "fragment-2",
                "tool-1",
                "Bash",
                json!({"command": "pwd"}),
            );
            assistant_store.stage_fragment(&envelope).await
        });
        wait_for_write_attempts(&mut write_attempts, 1).await;
        drop(write_permit);

        let user = user_task
            .await
            .expect("join user task")
            .expect("stage user");
        let assistant = assistant_task
            .await
            .expect("join assistant task")
            .expect("stage assistant");

        assert_eq!(user.turn_index, 1);
        assert_eq!(assistant.turn_index, 1);

        let turn_count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM ingest_turns WHERE conversation_id = ?")
                .bind("session-concurrent")
                .fetch_one(store.pool())
                .await
                .expect("turn count");
        assert_eq!(turn_count, 1);

        let turn = load_turn_projection_row(store.pool(), "session-concurrent", "turn-1").await;
        assert_eq!(turn.status, IngestTurnStatus::Open);
        assert_eq!(turn.terminality.into_terminality(), Terminality::None);

        let fragment_count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM ingest_fragments WHERE turn_id = ?")
                .bind(turn.id)
                .fetch_one(store.pool())
                .await
                .expect("fragment count");
        assert_eq!(fragment_count, 2);

        let mut tx = store
            .pool()
            .begin()
            .await
            .expect("begin replay transaction");
        let reduced = store
            .rebuild_projection(&mut tx, turn.id)
            .await
            .expect("rebuild projection");
        tx.rollback().await.expect("rollback replay transaction");

        assert_eq!(reduced.terminality, Terminality::None);
        assert_eq!(reduced.projection.user_message, "hello");
        assert!(reduced.projection.assistant_reply.is_empty());
        assert_eq!(
            reduced.projection.steps,
            vec![MemoryStep::ToolCall {
                name: "Bash".to_string(),
                input: "{\"command\":\"pwd\"}".to_string(),
            }]
        );
    }

    #[tokio::test]
    async fn sqlite_lock_errors_are_classified_as_retryable_stage_errors() {
        let db_path = test_db_path("busy_retry");
        let holder = open_file_pool_with_busy_timeout(&db_path, "0").await;
        let blocked = open_file_pool_with_busy_timeout(&db_path, "0").await;
        sqlx::query("CREATE TABLE write_lock (id INTEGER PRIMARY KEY, value TEXT NOT NULL);")
            .execute(&holder)
            .await
            .expect("create write_lock table");

        let mut tx = holder
            .begin_with("BEGIN IMMEDIATE")
            .await
            .expect("begin immediate");
        sqlx::query("INSERT INTO write_lock (id, value) VALUES (1, 'locked');")
            .execute(&mut *tx)
            .await
            .expect("insert row");

        let error = blocked
            .begin_with("BEGIN IMMEDIATE")
            .await
            .expect_err("busy lock error");
        assert!(super::is_sqlite_lock_contention(&error));
        assert!(should_retry_stage_error(&super::IngestError::Database(
            error
        )));

        tx.rollback().await.expect("rollback");
    }

    #[tokio::test]
    async fn unique_constraint_errors_remain_retryable() {
        let pool = open_memory_pool().await;
        sqlx::query("CREATE TABLE dedupe (id INTEGER PRIMARY KEY, value TEXT NOT NULL UNIQUE);")
            .execute(&pool)
            .await
            .expect("create dedupe table");
        sqlx::query("INSERT INTO dedupe (id, value) VALUES (1, 'once');")
            .execute(&pool)
            .await
            .expect("insert first row");

        let error = sqlx::query("INSERT INTO dedupe (id, value) VALUES (2, 'once');")
            .execute(&pool)
            .await
            .expect_err("unique violation");
        assert!(!super::is_sqlite_lock_contention(&error));
        assert!(should_retry_stage_error(&super::IngestError::Database(
            error
        )));
    }

    #[tokio::test]
    async fn open_rejects_legacy_turn_schema_without_constraints() {
        let db_path = test_db_path("legacy_turns");
        let pool = open_file_pool(&db_path).await;
        sqlx::query(LEGACY_INGEST_TURNS_TABLE_SQL)
            .execute(&pool)
            .await
            .expect("create legacy turns");
        sqlx::query(super::CREATE_INGEST_FRAGMENTS_TABLE_SQL)
            .execute(&pool)
            .await
            .expect("create constrained fragments");
        drop(pool);

        let error = IngestStore::open(&db_path, TEST_HISTORY_WINDOW_SIZE)
            .await
            .err()
            .expect("legacy turns schema should fail");

        assert!(matches!(
            error,
            AppError::Schema(message)
                if message.contains("ingest_turns")
                    && message.contains("status CHECK constraint")
        ));
    }

    #[tokio::test]
    async fn open_rejects_legacy_fragment_schema_without_constraints() {
        let db_path = test_db_path("legacy_fragments");
        let pool = open_file_pool(&db_path).await;
        sqlx::query(super::CREATE_INGEST_TURNS_TABLE_SQL)
            .execute(&pool)
            .await
            .expect("create constrained turns");
        sqlx::query(LEGACY_INGEST_FRAGMENTS_TABLE_SQL)
            .execute(&pool)
            .await
            .expect("create legacy fragments");
        drop(pool);

        let error = IngestStore::open(&db_path, TEST_HISTORY_WINDOW_SIZE)
            .await
            .err()
            .expect("legacy fragments schema should fail");

        assert!(matches!(
            error,
            AppError::Schema(message)
                if message.contains("ingest_fragments")
                    && message.contains("kind CHECK constraint")
        ));
    }

    #[tokio::test]
    async fn open_rejects_existing_invalid_enum_values() {
        let db_path = test_db_path("invalid_values");
        let pool = open_file_pool(&db_path).await;
        sqlx::query(super::CREATE_INGEST_TURNS_TABLE_SQL)
            .execute(&pool)
            .await
            .expect("create turns");
        sqlx::query(super::CREATE_INGEST_FRAGMENTS_TABLE_SQL)
            .execute(&pool)
            .await
            .expect("create fragments");
        sqlx::query("PRAGMA ignore_check_constraints = ON;")
            .execute(&pool)
            .await
            .expect("disable check enforcement");
        sqlx::query(
            "INSERT INTO ingest_turns (
                conversation_id,
                turn_index,
                status,
                projection_json,
                terminality,
                created_at,
                updated_at
            ) VALUES (?, ?, ?, ?, ?, ?, ?);",
        )
        .bind("session-1")
        .bind(1_i64)
        .bind("unexpected")
        .bind(serde_json::to_string(&MemoryProjection::default()).expect("projection json"))
        .bind(Terminality::None.as_str())
        .bind("2026-03-05T00:00:00Z")
        .bind("2026-03-05T00:00:00Z")
        .execute(&pool)
        .await
        .expect("insert invalid row");
        drop(pool);

        let error = IngestStore::open(&db_path, TEST_HISTORY_WINDOW_SIZE)
            .await
            .err()
            .expect("invalid enum values should fail");

        assert!(matches!(
            error,
            AppError::Schema(message)
                if message.contains("ingest_turns.status")
                    && message.contains("unexpected")
        ));
    }

    #[tokio::test]
    async fn duplicate_fragments_do_not_create_extra_turns() {
        let db_path = test_db_path("duplicate_fragment");
        let store = IngestStore::open(&db_path, TEST_HISTORY_WINDOW_SIZE)
            .await
            .expect("open store");
        let envelope = IngestEnvelope {
            protocol_version: INGEST_PROTOCOL_VERSION,
            source: SourceMeta {
                agent: "claude-code".to_string(),
                event: "Stop".to_string(),
            },
            scope: ConversationScope {
                conversation_id: "session-1".to_string(),
                turn_id: None,
                fragment_id: "fragment-1".to_string(),
                sequence_hint: Some(1),
                emitted_at_rfc3339: Some("2026-03-05T00:00:00Z".to_string()),
            },
            fragment: ConversationFragment {
                terminality: Terminality::Hard,
                body: FragmentBody::AssistantMessage {
                    text: "Done.".to_string(),
                },
            },
            raw: json!({"session_id": "session-1"}),
        };

        let first = store.stage_fragment(&envelope).await.expect("first stage");
        let duplicate = store
            .stage_fragment(&envelope)
            .await
            .expect("duplicate stage");

        assert!(first.finalized);
        assert!(duplicate.duplicate);
        assert!(duplicate.finalized);
        assert_eq!(duplicate.turn_index, first.turn_index);

        let turn_count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM ingest_turns WHERE conversation_id = ?")
                .bind("session-1")
                .fetch_one(store.pool())
                .await
                .expect("turn count");
        let open_turns: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM ingest_turns WHERE conversation_id = ? AND status = 'open'",
        )
        .bind("session-1")
        .fetch_one(store.pool())
        .await
        .expect("open turn count");

        assert_eq!(turn_count, 1);
        assert_eq!(open_turns, 0);
    }

    #[tokio::test]
    async fn external_turn_id_coalesces_fragments_into_one_turn() {
        let db_path = test_db_path("external_turn_coalesce");
        let store = IngestStore::open(&db_path, TEST_HISTORY_WINDOW_SIZE)
            .await
            .expect("open store");

        let first = store
            .stage_fragment(&external_user_envelope(
                "session-external",
                "turn-1",
                "fragment-1",
                "hello",
            ))
            .await
            .expect("stage first fragment");
        let second = store
            .stage_fragment(&external_hard_assistant_envelope(
                "session-external",
                "turn-1",
                "fragment-2",
                "done",
            ))
            .await
            .expect("stage second fragment");

        assert_eq!(first.turn_index, 1);
        assert_eq!(second.turn_index, 1);
        assert!(!first.finalized);
        assert!(second.finalized);

        let turn_count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM ingest_turns WHERE conversation_id = ?")
                .bind("session-external")
                .fetch_one(store.pool())
                .await
                .expect("turn count");
        assert_eq!(turn_count, 1);

        let turn = load_turn_projection_row(store.pool(), "session-external", "turn-1").await;
        assert_eq!(turn.status, IngestTurnStatus::Finalized);
        assert_eq!(turn.terminality.into_terminality(), Terminality::Hard);

        let projection: MemoryProjection =
            serde_json::from_str(&turn.projection_json).expect("finalized projection");
        assert_eq!(projection.user_message, "hello");
        assert_eq!(projection.assistant_reply, "done");
        assert!(projection.steps.is_empty());
    }

    #[tokio::test]
    async fn different_external_turn_ids_create_new_turns() {
        let db_path = test_db_path("external_turn_split");
        let store = IngestStore::open(&db_path, TEST_HISTORY_WINDOW_SIZE)
            .await
            .expect("open store");

        let first = store
            .stage_fragment(&external_hard_assistant_envelope(
                "session-external",
                "turn-1",
                "fragment-1",
                "first",
            ))
            .await
            .expect("stage first turn");
        let second = store
            .stage_fragment(&external_hard_assistant_envelope(
                "session-external",
                "turn-2",
                "fragment-2",
                "second",
            ))
            .await
            .expect("stage second turn");

        assert_eq!(first.turn_index, 1);
        assert_eq!(second.turn_index, 2);
    }

    #[tokio::test]
    async fn soft_terminality_keeps_turn_open_and_updates_turn_metadata() {
        let db_path = test_db_path("soft_terminality_metadata");
        let store = IngestStore::open(&db_path, TEST_HISTORY_WINDOW_SIZE)
            .await
            .expect("open store");

        let mut envelope = external_user_envelope("session-soft", "turn-1", "fragment-1", "hello");
        envelope.fragment.terminality = Terminality::Soft;

        let staged = store
            .stage_fragment(&envelope)
            .await
            .expect("stage soft-terminal fragment");

        assert!(!staged.finalized);

        let turn = load_turn_projection_row(store.pool(), "session-soft", "turn-1").await;
        assert_eq!(turn.status, IngestTurnStatus::Open);
        assert_eq!(turn.terminality.into_terminality(), Terminality::Soft);
    }

    #[tokio::test]
    async fn duplicate_fragment_for_closed_external_turn_returns_duplicate_success() {
        let db_path = test_db_path("external_turn_duplicate");
        let store = IngestStore::open(&db_path, TEST_HISTORY_WINDOW_SIZE)
            .await
            .expect("open store");
        let envelope =
            external_hard_assistant_envelope("session-external", "turn-1", "fragment-1", "done");

        let first = store.stage_fragment(&envelope).await.expect("first stage");
        let duplicate = store
            .stage_fragment(&envelope)
            .await
            .expect("duplicate stage");

        assert!(first.finalized);
        assert!(duplicate.duplicate);
        assert!(duplicate.finalized);
        assert_eq!(duplicate.turn_index, first.turn_index);
    }

    #[tokio::test]
    async fn late_fragments_for_closed_external_turns_are_rejected() {
        let db_path = test_db_path("external_turn_closed");
        let store = IngestStore::open(&db_path, TEST_HISTORY_WINDOW_SIZE)
            .await
            .expect("open store");

        for (index, status) in [
            IngestTurnStatus::Finalized,
            IngestTurnStatus::Processing,
            IngestTurnStatus::Stored,
            IngestTurnStatus::Failed,
            IngestTurnStatus::Aborted,
        ]
        .into_iter()
        .enumerate()
        {
            let conversation_id = format!("session-closed-{index}");
            let turn_id = format!("turn-{index}");
            let (next_attempt_at, stored_at, finalized_at, processing_started_at) = match status {
                IngestTurnStatus::Finalized => (
                    Some("2026-03-05T00:00:00Z"),
                    None,
                    Some("2026-03-05T00:00:00Z"),
                    None,
                ),
                IngestTurnStatus::Processing => (
                    Some("2026-03-05T00:00:00Z"),
                    None,
                    Some("2026-03-05T00:00:00Z"),
                    Some("2026-03-05T00:00:00Z"),
                ),
                IngestTurnStatus::Stored => (
                    None,
                    Some("2026-03-05T00:00:00Z"),
                    Some("2026-03-05T00:00:00Z"),
                    None,
                ),
                IngestTurnStatus::Failed => (None, None, Some("2026-03-05T00:00:00Z"), None),
                IngestTurnStatus::Aborted => (None, None, None, None),
                IngestTurnStatus::Open => unreachable!("test only covers closed turns"),
            };
            insert_turn_row_with_external_id(
                store.pool(),
                &conversation_id,
                Some(&turn_id),
                1,
                status,
                &projection_json("user", "assistant"),
                next_attempt_at,
                stored_at,
                finalized_at,
                processing_started_at,
                0,
            )
            .await;

            let error = store
                .stage_fragment(&external_user_envelope(
                    &conversation_id,
                    &turn_id,
                    &format!("fragment-late-{index}"),
                    "late fragment",
                ))
                .await
                .expect_err("closed external turn should reject late fragment");

            assert!(matches!(
                error,
                super::IngestError::Validation(message)
                    if message.contains("fragment references closed external turn")
                        && message.contains(&turn_id)
                        && message.contains(status.as_str())
            ));
        }
    }

    #[tokio::test]
    async fn service_duplicate_replay_of_closed_external_fragment_reports_duplicate() {
        let db_path = test_db_path("service_external_duplicate");
        let service = IngestService::open(&db_path, MemoryHandle::closed_for_tests(), 0)
            .await
            .expect("open service");
        let envelope =
            external_hard_assistant_envelope("session-service", "turn-1", "fragment-1", "done");

        let first = service
            .ingest(envelope.clone())
            .await
            .expect("first ingest");
        let duplicate = service.ingest(envelope).await.expect("duplicate ingest");

        assert_eq!(first.turn_index, 1);
        assert!(!first.duplicate);
        assert!(first.finalized);
        assert_eq!(duplicate.turn_index, 1);
        assert!(duplicate.duplicate);
        assert!(duplicate.finalized);
    }

    #[tokio::test]
    async fn service_scopes_external_turn_ids_by_conversation() {
        let db_path = test_db_path("service_external_scope");
        let service = IngestService::open(&db_path, MemoryHandle::closed_for_tests(), 0)
            .await
            .expect("open service");

        let first = service
            .ingest(external_hard_assistant_envelope(
                "conversation-a",
                "turn-1",
                "fragment-1",
                "first",
            ))
            .await
            .expect("first ingest");
        let second = service
            .ingest(external_hard_assistant_envelope(
                "conversation-b",
                "turn-1",
                "fragment-2",
                "second",
            ))
            .await
            .expect("second ingest");

        assert_eq!(first.turn_index, 1);
        assert_eq!(second.turn_index, 1);

        let pool = open_file_pool(&db_path).await;
        let first_count = turn_count_for_conversation(&pool, "conversation-a").await;
        let second_count = turn_count_for_conversation(&pool, "conversation-b").await;
        assert_eq!(first_count, 1);
        assert_eq!(second_count, 1);
    }

    #[tokio::test]
    async fn service_external_turn_window_preserves_steps_and_messages() {
        let db_path = test_db_path("service_external_window");
        let (memory, mut requests) = MemoryHandle::channel_for_tests(1).await;
        let service = IngestService::open(&db_path, memory, TEST_HISTORY_WINDOW_SIZE)
            .await
            .expect("open service");

        service
            .ingest(external_user_envelope(
                "session-window",
                "turn-1",
                "fragment-1",
                "remember this",
            ))
            .await
            .expect("user fragment");
        service
            .ingest(external_tool_call_envelope(
                "session-window",
                "turn-1",
                "fragment-2",
                "tool-1",
                "Bash",
                json!({"command": "pwd"}),
            ))
            .await
            .expect("tool call fragment");
        service
            .ingest(external_tool_result_envelope(
                "session-window",
                "turn-1",
                "fragment-3",
                "tool-1",
                "Bash",
                json!({"stdout": "/tmp"}),
            ))
            .await
            .expect("tool result fragment");
        let outcome = service
            .ingest(external_hard_assistant_envelope(
                "session-window",
                "turn-1",
                "fragment-4",
                "done",
            ))
            .await
            .expect("assistant fragment");

        assert!(outcome.finalized);

        let request = recv_store_request(&mut requests).await;
        assert_eq!(request.window.previous_turns.len(), 0);
        assert_eq!(request.window.current_turn.user_message, "remember this");
        assert_eq!(request.window.current_turn.assistant_reply, "done");
        assert_eq!(
            request.window.current_turn.steps,
            vec![
                MemoryStep::ToolCall {
                    name: "Bash".to_string(),
                    input: "{\"command\":\"pwd\"}".to_string(),
                },
                MemoryStep::ToolResult {
                    name: "Bash".to_string(),
                    output: "{\"stdout\":\"/tmp\"}".to_string(),
                },
            ]
        );

        let pool = open_file_pool(&db_path).await;
        mark_processing_turn_stored(&pool, request.turn_id).await;
        request.responder.send(Ok(())).expect("ack success");
        let stored = wait_for_turn_status(&pool, request.turn_id, IngestTurnStatus::Stored).await;
        assert_eq!(stored.status, IngestTurnStatus::Stored);
    }

    #[tokio::test]
    async fn service_rejects_late_external_fragments_while_processing() {
        let db_path = test_db_path("service_external_processing_reject");
        let (memory, mut requests) = MemoryHandle::channel_for_tests(1).await;
        let service = IngestService::open(&db_path, memory, TEST_HISTORY_WINDOW_SIZE)
            .await
            .expect("open service");

        service
            .ingest(external_hard_assistant_envelope(
                "session-processing",
                "turn-1",
                "fragment-1",
                "done",
            ))
            .await
            .expect("finalizing ingest");

        let request = recv_store_request(&mut requests).await;
        let error = service
            .ingest(external_user_envelope(
                "session-processing",
                "turn-1",
                "fragment-2",
                "late user",
            ))
            .await
            .expect_err("processing turn should reject late fragment");
        assert!(matches!(
            error,
            super::IngestError::Validation(message)
                if message.contains("closed external turn 'turn-1'")
                    && message.contains("processing")
        ));

        let pool = open_file_pool(&db_path).await;
        mark_processing_turn_stored(&pool, request.turn_id).await;
        request.responder.send(Ok(())).expect("ack success");
        wait_for_turn_status(&pool, request.turn_id, IngestTurnStatus::Stored).await;
    }

    #[tokio::test]
    async fn service_rejects_late_external_fragments_after_stored() {
        let db_path = test_db_path("service_external_stored_reject");
        let (memory, mut requests) = MemoryHandle::channel_for_tests(1).await;
        let service = IngestService::open(&db_path, memory, TEST_HISTORY_WINDOW_SIZE)
            .await
            .expect("open service");

        service
            .ingest(external_hard_assistant_envelope(
                "session-stored",
                "turn-1",
                "fragment-1",
                "done",
            ))
            .await
            .expect("finalizing ingest");

        let request = recv_store_request(&mut requests).await;
        let pool = open_file_pool(&db_path).await;
        mark_processing_turn_stored(&pool, request.turn_id).await;
        request.responder.send(Ok(())).expect("ack success");
        wait_for_turn_status(&pool, request.turn_id, IngestTurnStatus::Stored).await;

        let error = service
            .ingest(external_user_envelope(
                "session-stored",
                "turn-1",
                "fragment-2",
                "late user",
            ))
            .await
            .expect_err("stored turn should reject late fragment");
        assert!(matches!(
            error,
            super::IngestError::Validation(message)
                if message.contains("closed external turn 'turn-1'")
                    && message.contains("stored")
        ));
    }

    #[tokio::test]
    async fn claim_next_due_turn_moves_finalized_turn_to_processing() {
        let db_path = test_db_path("claim_processing");
        let store = IngestStore::open(&db_path, TEST_HISTORY_WINDOW_SIZE)
            .await
            .expect("open store");

        let staged = store
            .stage_fragment(&hard_assistant_envelope(
                "session-claim",
                "fragment-1",
                1,
                "Done.",
            ))
            .await
            .expect("stage finalized turn");
        assert!(staged.finalized);

        let claimed = store
            .claim_next_due_turn(&chrono::Utc::now().to_rfc3339())
            .await
            .expect("claim turn")
            .expect("due turn");

        assert_eq!(claimed.turn_index, 1);
        assert_eq!(claimed.attempt_count, 0);

        let row = load_turn_state(store.pool(), claimed.id).await;
        assert_eq!(row.status, IngestTurnStatus::Processing);
        assert!(row.processing_started_at.is_some());
        assert!(row.next_attempt_at.is_some());
    }

    #[tokio::test]
    async fn claim_next_due_turn_blocks_later_turns_until_earlier_turn_is_stored() {
        let db_path = test_db_path("claim_ordering");
        let store = IngestStore::open(&db_path, TEST_HISTORY_WINDOW_SIZE)
            .await
            .expect("open store");

        store
            .stage_fragment(&hard_assistant_envelope(
                "session-order",
                "fragment-1",
                1,
                "First",
            ))
            .await
            .expect("stage first turn");
        store
            .stage_fragment(&user_envelope(
                "session-order",
                "fragment-2",
                2,
                "Second question",
            ))
            .await
            .expect("stage second turn user fragment");
        store
            .stage_fragment(&hard_assistant_envelope(
                "session-order",
                "fragment-3",
                3,
                "Second",
            ))
            .await
            .expect("stage second turn completion");

        let first = store
            .claim_next_due_turn(&chrono::Utc::now().to_rfc3339())
            .await
            .expect("claim first")
            .expect("first due turn");
        assert_eq!(first.turn_index, 1);

        let blocked = store
            .claim_next_due_turn(&chrono::Utc::now().to_rfc3339())
            .await
            .expect("claim blocked");
        assert!(blocked.is_none(), "later turn should remain blocked");

        sqlx::query(
            "UPDATE ingest_turns
             SET status = 'stored',
                 next_attempt_at = NULL,
                 processing_started_at = NULL,
                 stored_at = ?,
                 updated_at = ?
             WHERE id = ?",
        )
        .bind("2026-03-05T00:00:02Z")
        .bind("2026-03-05T00:00:02Z")
        .bind(first.id)
        .execute(store.pool())
        .await
        .expect("mark first stored");

        let second = store
            .claim_next_due_turn(&chrono::Utc::now().to_rfc3339())
            .await
            .expect("claim second")
            .expect("second due turn");
        assert_eq!(second.turn_index, 2);
    }

    #[tokio::test]
    async fn build_window_uses_all_stored_previous_turns_when_history_is_unlimited() {
        let db_path = test_db_path("history_window");
        let store = IngestStore::open(&db_path, TEST_HISTORY_WINDOW_SIZE)
            .await
            .expect("open store");

        let first_stored_projection = projection_json("stored question 1", "stored answer 1");
        let second_stored_projection = projection_json("stored question 2", "stored answer 2");
        let ignored_projection = projection_json("ignored question", "ignored answer");
        let current_projection = projection_json("current question", "current answer");
        insert_turn_row(
            store.pool(),
            "session-history",
            1,
            IngestTurnStatus::Stored,
            &first_stored_projection,
            None,
            Some("2026-03-05T00:00:01Z"),
            Some("2026-03-05T00:00:00Z"),
            None,
            0,
        )
        .await;
        insert_turn_row(
            store.pool(),
            "session-history",
            2,
            IngestTurnStatus::Stored,
            &second_stored_projection,
            None,
            Some("2026-03-05T00:00:30Z"),
            Some("2026-03-05T00:00:20Z"),
            None,
            0,
        )
        .await;
        insert_turn_row(
            store.pool(),
            "session-history",
            3,
            IngestTurnStatus::Finalized,
            &ignored_projection,
            Some("2026-03-05T00:01:00Z"),
            None,
            Some("2026-03-05T00:01:00Z"),
            None,
            0,
        )
        .await;
        let current_id = insert_turn_row(
            store.pool(),
            "session-history",
            4,
            IngestTurnStatus::Processing,
            &current_projection,
            Some("2026-03-05T00:02:00Z"),
            None,
            Some("2026-03-05T00:02:00Z"),
            Some("2026-03-05T00:02:01Z"),
            0,
        )
        .await;

        let window = store
            .build_window(&ClaimedTurnRow {
                id: current_id,
                conversation_id: "session-history".to_string(),
                turn_index: 4,
                projection_json: current_projection.clone(),
                finalized_at: Some("2026-03-05T00:02:00Z".to_string()),
                attempt_count: 0,
            })
            .await
            .expect("build window");

        assert_eq!(window.previous_turns.len(), 2);
        assert_eq!(window.previous_turns[0].user_message, "stored question 1");
        assert_eq!(window.previous_turns[1].user_message, "stored question 2");
        assert_eq!(window.current_turn.assistant_reply, "current answer");
    }

    #[tokio::test]
    async fn build_window_respects_configured_history_limit() {
        let db_path = test_db_path("history_window_limited");
        let store = IngestStore::open(&db_path, 1).await.expect("open store");

        let first_stored_projection = projection_json("stored question 1", "stored answer 1");
        let second_stored_projection = projection_json("stored question 2", "stored answer 2");
        let current_projection = projection_json("current question", "current answer");
        insert_turn_row(
            store.pool(),
            "session-history-limited",
            1,
            IngestTurnStatus::Stored,
            &first_stored_projection,
            None,
            Some("2026-03-05T00:00:01Z"),
            Some("2026-03-05T00:00:00Z"),
            None,
            0,
        )
        .await;
        insert_turn_row(
            store.pool(),
            "session-history-limited",
            2,
            IngestTurnStatus::Stored,
            &second_stored_projection,
            None,
            Some("2026-03-05T00:00:30Z"),
            Some("2026-03-05T00:00:20Z"),
            None,
            0,
        )
        .await;
        let current_id = insert_turn_row(
            store.pool(),
            "session-history-limited",
            3,
            IngestTurnStatus::Processing,
            &current_projection,
            Some("2026-03-05T00:02:00Z"),
            None,
            Some("2026-03-05T00:02:00Z"),
            Some("2026-03-05T00:02:01Z"),
            0,
        )
        .await;

        let window = store
            .build_window(&ClaimedTurnRow {
                id: current_id,
                conversation_id: "session-history-limited".to_string(),
                turn_index: 3,
                projection_json: current_projection.clone(),
                finalized_at: Some("2026-03-05T00:02:00Z".to_string()),
                attempt_count: 0,
            })
            .await
            .expect("build window");

        assert_eq!(window.previous_turns.len(), 1);
        assert_eq!(window.previous_turns[0].user_message, "stored question 2");
        assert_eq!(window.current_turn.assistant_reply, "current answer");
    }

    #[tokio::test]
    async fn dispatcher_reschedules_retryable_failures() {
        let db_path = test_db_path("dispatcher_retry");
        let store = Arc::new(
            IngestStore::open(&db_path, TEST_HISTORY_WINDOW_SIZE)
                .await
                .expect("open store"),
        );
        store
            .stage_fragment(&hard_assistant_envelope(
                "session-retry",
                "fragment-1",
                1,
                "Done.",
            ))
            .await
            .expect("stage finalized turn");

        let (memory, mut requests) = MemoryHandle::channel_for_tests(1).await;
        let dispatcher = IngestDispatcher::new(store.clone(), memory, Arc::new(Notify::new()));

        let run = tokio::spawn(async move { dispatcher.run_once().await });
        let request = recv_store_request(&mut requests).await;
        assert_eq!(request.window.current_turn.assistant_reply, "Done.");
        assert!(request.timestamp <= chrono::Utc::now());
        request
            .responder
            .send(Err(ProcessTurnError::Retryable(
                RetryableProcessTurnError::Encoder("temporary encoder failure".to_string()),
            )))
            .expect("send retryable failure");
        run.await.expect("join").expect("dispatcher result");

        let row = load_turn_state(store.pool(), request.turn_id).await;
        assert_eq!(row.status, IngestTurnStatus::Finalized);
        assert_eq!(row.attempt_count, 1);
        assert_eq!(
            row.last_error.as_deref(),
            Some("encoder error: temporary encoder failure")
        );
        assert!(row.next_attempt_at.is_some());
        assert!(row.processing_started_at.is_none());
    }

    #[tokio::test]
    async fn service_recovers_processing_turns_and_dispatches_on_startup() {
        let db_path = test_db_path("startup_recovery");
        let store = IngestStore::open(&db_path, TEST_HISTORY_WINDOW_SIZE)
            .await
            .expect("open store");
        store
            .stage_fragment(&hard_assistant_envelope(
                "session-startup",
                "fragment-1",
                1,
                "Recovered turn",
            ))
            .await
            .expect("stage finalized turn");
        let claimed = store
            .claim_next_due_turn(&chrono::Utc::now().to_rfc3339())
            .await
            .expect("claim turn")
            .expect("processing turn");
        drop(store);

        let (memory, mut requests) = MemoryHandle::channel_for_tests(1).await;
        let service = IngestService::open(&db_path, memory, TEST_HISTORY_WINDOW_SIZE)
            .await
            .expect("open service");

        let request = recv_store_request(&mut requests).await;
        assert_eq!(request.turn_id, claimed.id);
        assert_eq!(
            request.window.current_turn.assistant_reply,
            "Recovered turn"
        );

        let pool = open_file_pool(&db_path).await;
        sqlx::query(
            "UPDATE ingest_turns
             SET status = 'stored',
                 last_error = NULL,
                 next_attempt_at = NULL,
                 processing_started_at = NULL,
                 stored_at = ?,
                 updated_at = ?
             WHERE id = ? AND status = 'processing'",
        )
        .bind("2026-03-05T00:00:05Z")
        .bind("2026-03-05T00:00:05Z")
        .bind(request.turn_id)
        .execute(&pool)
        .await
        .expect("mark recovered turn stored");
        request.responder.send(Ok(())).expect("ack success");

        let stored = wait_for_turn_status(&pool, request.turn_id, IngestTurnStatus::Stored).await;
        assert_eq!(stored.status, IngestTurnStatus::Stored);
        assert_eq!(
            stored.attempt_count, 1,
            "recovery should increment attempts"
        );
        assert!(stored.stored_at.is_some());

        drop(service);
    }

    #[tokio::test]
    async fn ingest_returns_before_background_persistence_finishes() {
        let db_path = test_db_path("async_ingest");
        let (memory, mut requests) = MemoryHandle::channel_for_tests(1).await;
        let service = IngestService::open(&db_path, memory, TEST_HISTORY_WINDOW_SIZE)
            .await
            .expect("open service");

        let outcome = service
            .ingest(hard_assistant_envelope(
                "session-async",
                "fragment-1",
                1,
                "Background write",
            ))
            .await
            .expect("ingest");

        assert!(outcome.finalized);

        let request = recv_store_request(&mut requests).await;
        assert_eq!(
            request.window.current_turn.assistant_reply,
            "Background write"
        );
        let pool = open_file_pool(&db_path).await;
        sqlx::query(
            "UPDATE ingest_turns
             SET status = 'stored',
                 last_error = NULL,
                 next_attempt_at = NULL,
                 processing_started_at = NULL,
                 stored_at = ?,
                 updated_at = ?
             WHERE id = ? AND status = 'processing'",
        )
        .bind("2026-03-05T00:00:05Z")
        .bind("2026-03-05T00:00:05Z")
        .bind(request.turn_id)
        .execute(&pool)
        .await
        .expect("mark turn stored");
        request.responder.send(Ok(())).expect("ack success");

        let stored = wait_for_turn_status(&pool, request.turn_id, IngestTurnStatus::Stored).await;
        assert_eq!(stored.status, IngestTurnStatus::Stored);
    }

    async fn open_memory_pool() -> SqlitePool {
        SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .expect("memory pool")
    }

    async fn open_file_pool(db_path: &Path) -> SqlitePool {
        open_file_pool_with_busy_timeout(db_path, SQLITE_BUSY_TIMEOUT_MS).await
    }

    async fn open_file_pool_with_busy_timeout(db_path: &Path, busy_timeout_ms: &str) -> SqlitePool {
        let connection_string = format!("sqlite://{}?mode=rwc", db_path.to_string_lossy());
        let options = SqliteConnectOptions::from_str(&connection_string)
            .expect("sqlite options")
            .create_if_missing(true)
            .pragma("journal_mode", "WAL")
            .pragma("synchronous", "NORMAL")
            .pragma("busy_timeout", busy_timeout_ms.to_string())
            .pragma("foreign_keys", "ON")
            .pragma("mmap_size", SQLITE_MMAP_SIZE_BYTES);

        SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .expect("file pool")
    }

    async fn wait_for_write_attempts(
        write_attempts: &mut tokio::sync::mpsc::UnboundedReceiver<()>,
        expected: usize,
    ) {
        for _ in 0..expected {
            timeout(Duration::from_secs(1), write_attempts.recv())
                .await
                .expect("write attempt timeout")
                .expect("write attempt");
        }
    }

    async fn load_table_sql(pool: &SqlitePool, table_name: &str) -> String {
        sqlx::query_scalar::<_, String>(SELECT_TABLE_SCHEMA_SQL)
            .bind(table_name)
            .fetch_one(pool)
            .await
            .expect("table schema")
    }

    fn assert_schema_requirements(
        schema_sql: &str,
        requirements: &[super::SchemaConstraintRequirement],
    ) {
        let compact_schema = compact_sql(schema_sql);

        for requirement in requirements {
            assert!(
                compact_schema.contains(requirement.sql_fragment),
                "missing schema fragment {} in {}",
                requirement.sql_fragment,
                schema_sql
            );
        }
    }

    fn test_db_path(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!("memory_bank_ingest_{label}_{}.db", unique_suffix()))
    }

    fn unique_suffix() -> u128 {
        let time_component = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("time")
            .as_nanos();
        let counter_component = u128::from(UNIQUE_COUNTER.fetch_add(1, Ordering::Relaxed));

        (time_component << 16) | counter_component
    }

    fn hard_assistant_envelope(
        conversation_id: &str,
        fragment_id: &str,
        sequence_hint: u64,
        text: &str,
    ) -> IngestEnvelope {
        IngestEnvelope {
            protocol_version: INGEST_PROTOCOL_VERSION,
            source: SourceMeta {
                agent: "claude-code".to_string(),
                event: "Stop".to_string(),
            },
            scope: ConversationScope {
                conversation_id: conversation_id.to_string(),
                turn_id: None,
                fragment_id: fragment_id.to_string(),
                sequence_hint: Some(sequence_hint),
                emitted_at_rfc3339: Some("2026-03-05T00:00:00Z".to_string()),
            },
            fragment: ConversationFragment {
                terminality: Terminality::Hard,
                body: FragmentBody::AssistantMessage {
                    text: text.to_string(),
                },
            },
            raw: json!({"session_id": conversation_id}),
        }
    }

    fn external_hard_assistant_envelope(
        conversation_id: &str,
        turn_id: &str,
        fragment_id: &str,
        text: &str,
    ) -> IngestEnvelope {
        IngestEnvelope {
            protocol_version: INGEST_PROTOCOL_VERSION,
            source: SourceMeta {
                agent: "codex".to_string(),
                event: "Stop".to_string(),
            },
            scope: ConversationScope {
                conversation_id: conversation_id.to_string(),
                turn_id: Some(turn_id.to_string()),
                fragment_id: fragment_id.to_string(),
                sequence_hint: None,
                emitted_at_rfc3339: None,
            },
            fragment: ConversationFragment {
                terminality: Terminality::Hard,
                body: FragmentBody::AssistantMessage {
                    text: text.to_string(),
                },
            },
            raw: json!({"session_id": conversation_id, "turn_id": turn_id}),
        }
    }

    fn user_envelope(
        conversation_id: &str,
        fragment_id: &str,
        sequence_hint: u64,
        text: &str,
    ) -> IngestEnvelope {
        IngestEnvelope {
            protocol_version: INGEST_PROTOCOL_VERSION,
            source: SourceMeta {
                agent: "claude-code".to_string(),
                event: "UserMessage".to_string(),
            },
            scope: ConversationScope {
                conversation_id: conversation_id.to_string(),
                turn_id: None,
                fragment_id: fragment_id.to_string(),
                sequence_hint: Some(sequence_hint),
                emitted_at_rfc3339: Some("2026-03-05T00:00:00Z".to_string()),
            },
            fragment: ConversationFragment {
                terminality: Terminality::None,
                body: FragmentBody::UserMessage {
                    text: text.to_string(),
                },
            },
            raw: json!({"session_id": conversation_id}),
        }
    }

    fn external_user_envelope(
        conversation_id: &str,
        turn_id: &str,
        fragment_id: &str,
        text: &str,
    ) -> IngestEnvelope {
        IngestEnvelope {
            protocol_version: INGEST_PROTOCOL_VERSION,
            source: SourceMeta {
                agent: "codex".to_string(),
                event: "UserPromptSubmit".to_string(),
            },
            scope: ConversationScope {
                conversation_id: conversation_id.to_string(),
                turn_id: Some(turn_id.to_string()),
                fragment_id: fragment_id.to_string(),
                sequence_hint: None,
                emitted_at_rfc3339: None,
            },
            fragment: ConversationFragment {
                terminality: Terminality::None,
                body: FragmentBody::UserMessage {
                    text: text.to_string(),
                },
            },
            raw: json!({"session_id": conversation_id, "turn_id": turn_id}),
        }
    }

    fn external_tool_call_envelope(
        conversation_id: &str,
        turn_id: &str,
        fragment_id: &str,
        tool_use_id: &str,
        tool_name: &str,
        input: serde_json::Value,
    ) -> IngestEnvelope {
        IngestEnvelope {
            protocol_version: INGEST_PROTOCOL_VERSION,
            source: SourceMeta {
                agent: "codex".to_string(),
                event: "PreToolUse".to_string(),
            },
            scope: ConversationScope {
                conversation_id: conversation_id.to_string(),
                turn_id: Some(turn_id.to_string()),
                fragment_id: fragment_id.to_string(),
                sequence_hint: None,
                emitted_at_rfc3339: None,
            },
            fragment: ConversationFragment {
                terminality: Terminality::None,
                body: FragmentBody::ToolCall {
                    name: tool_name.to_string(),
                    input_json: serde_json::to_string(&input).expect("tool input json"),
                    tool_use_id: Some(tool_use_id.to_string()),
                },
            },
            raw: json!({
                "session_id": conversation_id,
                "turn_id": turn_id,
                "tool_use_id": tool_use_id,
                "tool_name": tool_name,
                "tool_input": input
            }),
        }
    }

    fn external_tool_result_envelope(
        conversation_id: &str,
        turn_id: &str,
        fragment_id: &str,
        tool_use_id: &str,
        tool_name: &str,
        output: serde_json::Value,
    ) -> IngestEnvelope {
        IngestEnvelope {
            protocol_version: INGEST_PROTOCOL_VERSION,
            source: SourceMeta {
                agent: "codex".to_string(),
                event: "PostToolUse".to_string(),
            },
            scope: ConversationScope {
                conversation_id: conversation_id.to_string(),
                turn_id: Some(turn_id.to_string()),
                fragment_id: fragment_id.to_string(),
                sequence_hint: None,
                emitted_at_rfc3339: None,
            },
            fragment: ConversationFragment {
                terminality: Terminality::None,
                body: FragmentBody::ToolResult {
                    name: tool_name.to_string(),
                    output_json: serde_json::to_string(&output).expect("tool output json"),
                    tool_use_id: Some(tool_use_id.to_string()),
                },
            },
            raw: json!({
                "session_id": conversation_id,
                "turn_id": turn_id,
                "tool_use_id": tool_use_id,
                "tool_name": tool_name,
                "tool_response": output
            }),
        }
    }

    fn projection_json(user_message: &str, assistant_reply: &str) -> String {
        serde_json::to_string(&MemoryProjection {
            user_message: user_message.to_string(),
            assistant_reply: assistant_reply.to_string(),
            steps: Vec::new(),
        })
        .expect("projection json")
    }

    async fn insert_turn_row(
        pool: &SqlitePool,
        conversation_id: &str,
        turn_index: i64,
        status: IngestTurnStatus,
        projection_json: &str,
        next_attempt_at: Option<&str>,
        stored_at: Option<&str>,
        finalized_at: Option<&str>,
        processing_started_at: Option<&str>,
        attempt_count: i64,
    ) -> i64 {
        insert_turn_row_with_external_id(
            pool,
            conversation_id,
            None,
            turn_index,
            status,
            projection_json,
            next_attempt_at,
            stored_at,
            finalized_at,
            processing_started_at,
            attempt_count,
        )
        .await
    }

    async fn insert_memory_note(db: &MemoryDb, content: &str) -> i64 {
        let mut tx = db.begin().await.expect("begin memory transaction");
        let memory_id = db
            .insert_memory(
                &mut *tx,
                content,
                "2026-03-05T00:00:00Z",
                "context",
                "[]",
                "[]",
            )
            .await
            .expect("insert memory note");
        tx.commit().await.expect("commit memory note");
        memory_id
    }

    async fn insert_turn_row_with_external_id(
        pool: &SqlitePool,
        conversation_id: &str,
        external_turn_id: Option<&str>,
        turn_index: i64,
        status: IngestTurnStatus,
        projection_json: &str,
        next_attempt_at: Option<&str>,
        stored_at: Option<&str>,
        finalized_at: Option<&str>,
        processing_started_at: Option<&str>,
        attempt_count: i64,
    ) -> i64 {
        let result = sqlx::query(
            "INSERT INTO ingest_turns (
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
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(conversation_id)
        .bind(external_turn_id)
        .bind(turn_index)
        .bind(status)
        .bind(projection_json)
        .bind(Terminality::Hard.as_str())
        .bind("2026-03-05T00:00:00Z")
        .bind("2026-03-05T00:00:00Z")
        .bind(finalized_at)
        .bind(attempt_count)
        .bind::<Option<String>>(None)
        .bind(next_attempt_at)
        .bind(processing_started_at)
        .bind(stored_at)
        .execute(pool)
        .await
        .expect("insert turn row");
        result.last_insert_rowid()
    }

    async fn load_turn_state(pool: &SqlitePool, turn_id: i64) -> TurnStateRow {
        sqlx::query_as::<_, TurnStateRow>(
            "SELECT status, attempt_count, last_error, next_attempt_at, processing_started_at, stored_at
             FROM ingest_turns
             WHERE id = ?",
        )
        .bind(turn_id)
        .fetch_one(pool)
        .await
        .expect("turn state")
    }

    async fn load_turn_projection_row(
        pool: &SqlitePool,
        conversation_id: &str,
        external_turn_id: &str,
    ) -> TurnProjectionRow {
        sqlx::query_as::<_, TurnProjectionRow>(
            "SELECT id, status, terminality, projection_json
             FROM ingest_turns
             WHERE conversation_id = ? AND external_turn_id = ?
             LIMIT 1",
        )
        .bind(conversation_id)
        .bind(external_turn_id)
        .fetch_one(pool)
        .await
        .expect("turn projection row")
    }

    async fn turn_count_for_conversation(pool: &SqlitePool, conversation_id: &str) -> i64 {
        sqlx::query_scalar("SELECT COUNT(*) FROM ingest_turns WHERE conversation_id = ?")
            .bind(conversation_id)
            .fetch_one(pool)
            .await
            .expect("turn count")
    }

    async fn mark_processing_turn_stored(pool: &SqlitePool, turn_id: i64) {
        sqlx::query(
            "UPDATE ingest_turns
             SET status = 'stored',
                 last_error = NULL,
                 next_attempt_at = NULL,
                 processing_started_at = NULL,
                 stored_at = ?,
                 updated_at = ?
             WHERE id = ? AND status = 'processing'",
        )
        .bind("2026-03-05T00:00:05Z")
        .bind("2026-03-05T00:00:05Z")
        .bind(turn_id)
        .execute(pool)
        .await
        .expect("mark turn stored");
    }

    async fn recv_store_request(
        requests: &mut tokio::sync::mpsc::Receiver<TestStoreTurnRequest>,
    ) -> TestStoreTurnRequest {
        timeout(Duration::from_secs(5), requests.recv())
            .await
            .expect("receive request before timeout")
            .expect("store request")
    }

    async fn wait_for_turn_status(
        pool: &SqlitePool,
        turn_id: i64,
        expected: IngestTurnStatus,
    ) -> TurnStateRow {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        loop {
            let row = load_turn_state(pool, turn_id).await;
            if row.status == expected {
                return row;
            }
            if tokio::time::Instant::now() >= deadline {
                panic!(
                    "timed out waiting for turn {} to reach status {}",
                    turn_id, expected
                );
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }
}

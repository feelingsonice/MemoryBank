use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;

use axum::extract::rejection::JsonRejection;
use axum::extract::{DefaultBodyLimit, Json, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use memory_bank_protocol::IngestEnvelope;
use rmcp::model::LoggingMessageNotificationParam;
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::streamable_http_server::tower::{
    StreamableHttpServerConfig, StreamableHttpService,
};
use serde::Serialize;
use tokio::net::TcpListener;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use crate::actor::MemoryHandle;
use crate::error::AppError;
use crate::ingest::{IngestError, IngestService};
use crate::mcp_server::McpServer;

pub struct HttpServer {
    listener: TcpListener,
    app: axum::Router,
    bind_addr: SocketAddr,
    shutdown: CancellationToken,
}

impl HttpServer {
    pub async fn bind(
        port: u16,
        health: HealthResponse,
        memory: MemoryHandle,
        ingest: IngestService,
        log_tx: broadcast::Sender<LoggingMessageNotificationParam>,
    ) -> Result<Self, AppError> {
        let requested_bind_addr = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
        let shutdown = CancellationToken::new();
        let app = build_app(health, memory, ingest, log_tx, &shutdown);
        let listener = TcpListener::bind(requested_bind_addr).await.map_err(|e| {
            AppError::HttpServer(format!("Failed to bind to {}: {}", requested_bind_addr, e))
        })?;
        let bind_addr = listener.local_addr().map_err(|e| {
            AppError::HttpServer(format!(
                "Failed to read bound address for {}: {}",
                requested_bind_addr, e
            ))
        })?;

        Ok(Self {
            listener,
            app,
            bind_addr,
            shutdown,
        })
    }

    pub async fn run(self) -> Result<(), AppError> {
        self.log_endpoints();

        let Self {
            listener,
            app,
            bind_addr,
            shutdown,
        } = self;

        axum::serve(listener, app)
            .with_graceful_shutdown(shutdown_signal(shutdown))
            .await
            .map_err(|e| AppError::HttpServer(format!("Server error: {}", e)))?;

        info!(bind_addr = %bind_addr, "HTTP server stopped");
        Ok(())
    }

    fn log_endpoints(&self) {
        info!(
            bind_addr = %self.bind_addr,
            mcp_endpoint = %format!("http://{}/mcp", self.bind_addr),
            ingest_endpoint = %format!("http://{}/ingest", self.bind_addr),
            "HTTP server listening",
        );
    }
}

fn build_app(
    health: HealthResponse,
    memory: MemoryHandle,
    ingest: IngestService,
    log_tx: broadcast::Sender<LoggingMessageNotificationParam>,
    shutdown: &CancellationToken,
) -> axum::Router {
    let state = HttpState { health, ingest };
    let mcp_service = StreamableHttpService::new(
        move || Ok(McpServer::new(memory.clone(), log_tx.clone())),
        Arc::new(LocalSessionManager::default()),
        StreamableHttpServerConfig {
            cancellation_token: shutdown.child_token(),
            ..Default::default()
        },
    );
    let ingest_routes = axum::Router::new()
        .route("/ingest", post(handle_ingest))
        .route("/healthz", get(handle_healthz))
        .with_state(state)
        .layer(DefaultBodyLimit::disable());

    axum::Router::new()
        .nest_service("/mcp", mcp_service)
        .merge(ingest_routes)
        .fallback(|| async { StatusCode::NOT_FOUND })
}

#[derive(Clone)]
struct HttpState {
    health: HealthResponse,
    ingest: IngestService,
}

async fn handle_ingest(
    State(state): State<HttpState>,
    payload: Result<Json<IngestEnvelope>, JsonRejection>,
) -> StatusCode {
    let Json(request) = match payload {
        Ok(payload) => payload,
        Err(err) => {
            warn!(error = %err, "Rejected /ingest request with invalid JSON");
            return StatusCode::BAD_REQUEST;
        }
    };

    match state.ingest.ingest(request).await {
        Ok(outcome) => {
            debug!(
                agent = %outcome.agent,
                event = %outcome.event,
                conversation_id = %outcome.conversation_id,
                fragment_id = %outcome.fragment_id,
                turn_index = outcome.turn_index,
                duplicate = outcome.duplicate,
                finalized = outcome.finalized,
                "Staged ingest fragment",
            );
            StatusCode::ACCEPTED
        }
        Err(IngestError::Validation(error)) => {
            warn!(error = %error, "Rejected ingest fragment because the payload was invalid");
            StatusCode::UNPROCESSABLE_ENTITY
        }
        Err(error) if error.is_sqlite_lock_contention() => {
            warn!(
                error = %error,
                "Failed to stage ingest fragment because the namespace database stayed locked; another process may be writing to the same namespace"
            );
            StatusCode::SERVICE_UNAVAILABLE
        }
        Err(error) => {
            warn!(error = %error, "Failed to stage ingest fragment in the durable ingest queue");
            StatusCode::SERVICE_UNAVAILABLE
        }
    }
}

async fn handle_healthz(State(state): State<HttpState>) -> Json<HealthResponse> {
    Json(state.health)
}

#[derive(Clone, Debug, Serialize)]
pub struct HealthResponse {
    pub ok: bool,
    pub namespace: String,
    pub port: u16,
    pub llm_provider: String,
    pub encoder_provider: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub llm_model_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub encoder_model_id: Option<String>,
    pub version: &'static str,
}

async fn shutdown_signal(shutdown: CancellationToken) {
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {
            info!("Received shutdown signal; waiting for in-flight HTTP requests to finish");
            shutdown.cancel();
        }
        _ = shutdown.cancelled() => {
            info!("HTTP shutdown requested; waiting for in-flight HTTP requests to finish");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{HealthResponse, HttpServer, build_app};
    use crate::actor::{MemoryHandle, TestStoreTurnRequest};
    use crate::db::SqliteRuntime;
    use crate::ingest::IngestService;
    use axum::body::{Body, to_bytes};
    use axum::http::{Request, StatusCode};
    use memory_bank_protocol::{
        ConversationFragment, ConversationScope, FragmentBody, INGEST_PROTOCOL_VERSION,
        IngestEnvelope, SourceMeta, Terminality,
    };
    use rmcp::model::LoggingMessageNotificationParam;
    use serde_json::json;
    use sqlx::SqlitePool;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};
    use tokio::sync::{broadcast, mpsc};
    use tokio::task::JoinHandle;
    use tokio::time::{Duration, timeout};
    use tokio_util::sync::CancellationToken;
    use tower::ServiceExt;

    static UNIQUE_COUNTER: AtomicU64 = AtomicU64::new(0);

    #[tokio::test]
    async fn post_ingest_accepts_valid_json() {
        let app = app().await;
        let payload = serde_json::to_vec(&sample_payload()).expect("json");

        let response = app
            .oneshot(
                Request::post("/ingest")
                    .header("content-type", "application/json")
                    .body(Body::from(payload))
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::ACCEPTED);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn real_http_concurrent_external_turn_fragments_share_one_turn_and_one_dispatch() {
        let db_path = test_db_path();
        let runtime = SqliteRuntime::open_file_for_tests(&db_path, 2)
            .await
            .expect("sqlite runtime");
        let (memory, mut requests) = MemoryHandle::channel_for_tests(2).await;
        let ingest = IngestService::open_with_runtime(runtime.clone(), &db_path, memory.clone(), 0)
            .await
            .expect("ingest service");
        let server = spawn_http_server(memory, ingest).await;

        let write_permit = runtime.acquire_write_permit().await;
        let mut write_attempts = runtime.install_write_attempt_notifier();

        let user_task = tokio::spawn(post_ingest_status(
            server.base_url.clone(),
            external_user_payload("session-http-external", "turn-1", "fragment-user", "hello"),
        ));
        wait_for_write_attempts(&mut write_attempts, 1).await;

        let tool_call_task = tokio::spawn(post_ingest_status(
            server.base_url.clone(),
            external_tool_call_payload(
                "session-http-external",
                "turn-1",
                "fragment-tool-call",
                "tool-1",
                "Bash",
                json!({"command": "pwd"}),
            ),
        ));
        wait_for_write_attempts(&mut write_attempts, 1).await;

        drop(write_permit);

        assert_eq!(user_task.await.expect("join user request"), 202);
        assert_eq!(tool_call_task.await.expect("join tool call request"), 202);
        assert_eq!(
            post_ingest_status(
                server.base_url.clone(),
                external_assistant_stop_payload(
                    "session-http-external",
                    "turn-1",
                    "fragment-stop",
                    "done",
                ),
            )
            .await,
            202
        );

        let turn_count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM ingest_turns WHERE conversation_id = ?")
                .bind("session-http-external")
                .fetch_one(runtime.pool())
                .await
                .expect("turn count");
        assert_eq!(turn_count, 1);

        let projection_json: String = sqlx::query_scalar(
            "SELECT projection_json
             FROM ingest_turns
             WHERE conversation_id = ? AND external_turn_id = ?
             LIMIT 1",
        )
        .bind("session-http-external")
        .bind("turn-1")
        .fetch_one(runtime.pool())
        .await
        .expect("projection json");
        assert!(projection_json.contains("hello"));
        assert!(projection_json.contains("Bash"));
        assert!(projection_json.contains("done"));

        let dispatched_turn_id =
            receive_single_dispatch_and_ack(&mut requests, runtime.pool()).await;
        assert!(dispatched_turn_id > 0);

        server.stop().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn real_http_duplicate_replay_race_accepts_all_requests_without_extra_rows() {
        let db_path = test_db_path();
        let runtime = SqliteRuntime::open_file_for_tests(&db_path, 2)
            .await
            .expect("sqlite runtime");
        let memory = MemoryHandle::closed_for_tests();
        let ingest = IngestService::open_with_runtime(runtime.clone(), &db_path, memory.clone(), 0)
            .await
            .expect("ingest service");
        let server = spawn_http_server(memory, ingest).await;

        let write_permit = runtime.acquire_write_permit().await;
        let mut write_attempts = runtime.install_write_attempt_notifier();
        let payload = fixed_user_payload("session-http-duplicate", "fragment-fixed", "hello");
        let mut tasks = Vec::new();
        for _ in 0..3 {
            tasks.push(tokio::spawn(post_ingest_status(
                server.base_url.clone(),
                payload.clone(),
            )));
        }
        wait_for_write_attempts(&mut write_attempts, 3).await;
        drop(write_permit);

        for task in tasks {
            assert_eq!(task.await.expect("join duplicate request"), 202);
        }

        let turn_count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM ingest_turns WHERE conversation_id = ?")
                .bind("session-http-duplicate")
                .fetch_one(runtime.pool())
                .await
                .expect("turn count");
        let fragment_count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM ingest_fragments WHERE conversation_id = ?")
                .bind("session-http-duplicate")
                .fetch_one(runtime.pool())
                .await
                .expect("fragment count");
        assert_eq!(turn_count, 1);
        assert_eq!(fragment_count, 1);

        server.stop().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn real_http_concurrent_conversations_keep_turn_scopes_independent() {
        let db_path = test_db_path();
        let runtime = SqliteRuntime::open_file_for_tests(&db_path, 2)
            .await
            .expect("sqlite runtime");
        let memory = MemoryHandle::closed_for_tests();
        let ingest = IngestService::open_with_runtime(runtime.clone(), &db_path, memory.clone(), 0)
            .await
            .expect("ingest service");
        let server = spawn_http_server(memory, ingest).await;

        let write_permit = runtime.acquire_write_permit().await;
        let mut write_attempts = runtime.install_write_attempt_notifier();
        let first_task = tokio::spawn(post_ingest_status(
            server.base_url.clone(),
            fixed_user_payload("conversation-a", "fragment-a", "hello a"),
        ));
        let second_task = tokio::spawn(post_ingest_status(
            server.base_url.clone(),
            fixed_user_payload("conversation-b", "fragment-b", "hello b"),
        ));
        wait_for_write_attempts(&mut write_attempts, 2).await;
        drop(write_permit);

        assert_eq!(first_task.await.expect("join first request"), 202);
        assert_eq!(second_task.await.expect("join second request"), 202);

        let rows: Vec<(String, i64)> = sqlx::query_as(
            "SELECT conversation_id, turn_index
             FROM ingest_turns
             WHERE conversation_id IN (?, ?)
             ORDER BY conversation_id",
        )
        .bind("conversation-a")
        .bind("conversation-b")
        .fetch_all(runtime.pool())
        .await
        .expect("turn rows");
        assert_eq!(
            rows,
            vec![
                ("conversation-a".to_string(), 1),
                ("conversation-b".to_string(), 1),
            ]
        );

        server.stop().await;
    }

    #[tokio::test]
    async fn post_ingest_accepts_turn_ids() {
        let app = app().await;
        let mut payload = sample_payload();
        payload.scope.turn_id = Some("turn-1".to_string());
        let payload = serde_json::to_vec(&payload).expect("json");

        let response = app
            .oneshot(
                Request::post("/ingest")
                    .header("content-type", "application/json")
                    .body(Body::from(payload))
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::ACCEPTED);
    }

    #[tokio::test]
    async fn post_ingest_rejects_late_fragments_for_closed_external_turns() {
        let app = app().await;
        let mut first = sample_payload();
        first.source.agent = "codex".to_string();
        first.source.event = "Stop".to_string();
        first.scope.turn_id = Some("turn-1".to_string());
        first.fragment.terminality = Terminality::Hard;
        first.fragment.body = FragmentBody::AssistantMessage {
            text: "done".to_string(),
        };

        let first_response = app
            .clone()
            .oneshot(
                Request::post("/ingest")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&first).expect("json")))
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(first_response.status(), StatusCode::ACCEPTED);

        let mut late = sample_payload();
        late.source.agent = "codex".to_string();
        late.source.event = "UserPromptSubmit".to_string();
        late.scope.turn_id = Some("turn-1".to_string());
        late.scope.fragment_id = format!("fragment-{}", unique_suffix());

        let response = app
            .oneshot(
                Request::post("/ingest")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&late).expect("json")))
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    }

    #[tokio::test]
    async fn post_ingest_accepts_duplicate_replay_for_closed_external_turn() {
        let app = app().await;
        let mut payload = sample_payload();
        payload.source.agent = "codex".to_string();
        payload.source.event = "Stop".to_string();
        payload.scope.turn_id = Some("turn-1".to_string());
        payload.fragment.terminality = Terminality::Hard;
        payload.fragment.body = FragmentBody::AssistantMessage {
            text: "done".to_string(),
        };

        let first_response = app
            .clone()
            .oneshot(
                Request::post("/ingest")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&payload).expect("json")))
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(first_response.status(), StatusCode::ACCEPTED);

        let duplicate_response = app
            .oneshot(
                Request::post("/ingest")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&payload).expect("json")))
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(duplicate_response.status(), StatusCode::ACCEPTED);
    }

    #[tokio::test]
    async fn post_ingest_scopes_identical_turn_ids_by_conversation() {
        let app = app().await;
        let mut first = sample_payload();
        first.source.agent = "codex".to_string();
        first.source.event = "Stop".to_string();
        first.scope.turn_id = Some("turn-1".to_string());
        first.scope.conversation_id = "conversation-a".to_string();
        first.raw = json!({"session_id": "conversation-a", "turn_id": "turn-1"});
        first.fragment.terminality = Terminality::Hard;
        first.fragment.body = FragmentBody::AssistantMessage {
            text: "done a".to_string(),
        };

        let mut second = sample_payload();
        second.source.agent = "codex".to_string();
        second.source.event = "Stop".to_string();
        second.scope.turn_id = Some("turn-1".to_string());
        second.scope.conversation_id = "conversation-b".to_string();
        second.scope.fragment_id = format!("fragment-{}", unique_suffix());
        second.raw = json!({"session_id": "conversation-b", "turn_id": "turn-1"});
        second.fragment.terminality = Terminality::Hard;
        second.fragment.body = FragmentBody::AssistantMessage {
            text: "done b".to_string(),
        };

        let first_response = app
            .clone()
            .oneshot(
                Request::post("/ingest")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&first).expect("json")))
                    .expect("request"),
            )
            .await
            .expect("response");
        let second_response = app
            .oneshot(
                Request::post("/ingest")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&second).expect("json")))
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(first_response.status(), StatusCode::ACCEPTED);
        assert_eq!(second_response.status(), StatusCode::ACCEPTED);
    }

    #[tokio::test]
    async fn get_ingest_is_not_found_or_method_not_allowed() {
        let app = app().await;

        let response = app
            .oneshot(
                Request::get("/ingest")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        assert!(matches!(
            response.status(),
            StatusCode::NOT_FOUND | StatusCode::METHOD_NOT_ALLOWED
        ));
    }

    #[tokio::test]
    async fn root_returns_not_found() {
        let app = app().await;

        let response = app
            .oneshot(Request::get("/").body(Body::empty()).expect("request"))
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn mcp_route_is_mounted() {
        let app = app().await;

        let response = app
            .oneshot(Request::post("/mcp").body(Body::empty()).expect("request"))
            .await
            .expect("response");

        assert_ne!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn healthz_route_returns_ok_payload() {
        let app = app().await;

        let response = app
            .oneshot(
                Request::get("/healthz")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("health body");
        let json: serde_json::Value = serde_json::from_slice(&body).expect("health json");
        assert_eq!(json["llm_model_id"], "Anthropic::claude-sonnet-4-6");
        assert_eq!(json["encoder_model_id"], "FastEmbed::default");
    }

    async fn app() -> axum::Router {
        let (log_tx, _) = broadcast::channel::<LoggingMessageNotificationParam>(8);
        let shutdown = CancellationToken::new();
        let memory = MemoryHandle::closed_for_tests();
        let ingest = IngestService::open(&test_db_path(), memory.clone(), 0)
            .await
            .expect("ingest service");
        let health = HealthResponse {
            ok: true,
            namespace: "default".to_string(),
            port: 3737,
            llm_provider: "anthropic".to_string(),
            encoder_provider: "fast-embed".to_string(),
            llm_model_id: Some("Anthropic::claude-sonnet-4-6".to_string()),
            encoder_model_id: Some("FastEmbed::default".to_string()),
            version: "test",
        };
        build_app(health, memory, ingest, log_tx, &shutdown)
    }

    struct TestHttpServer {
        base_url: String,
        shutdown: CancellationToken,
        task: JoinHandle<Result<(), crate::error::AppError>>,
    }

    impl TestHttpServer {
        async fn stop(self) {
            self.shutdown.cancel();
            self.task
                .await
                .expect("join http server task")
                .expect("http server run");
        }
    }

    async fn spawn_http_server(memory: MemoryHandle, ingest: IngestService) -> TestHttpServer {
        let (log_tx, _) = broadcast::channel::<LoggingMessageNotificationParam>(8);
        let server = HttpServer::bind(0, test_health(), memory, ingest, log_tx)
            .await
            .expect("bind http server");
        let base_url = format!("http://{}", server.bind_addr);
        let shutdown = server.shutdown.clone();
        let task = tokio::spawn(async move { server.run().await });
        TestHttpServer {
            base_url,
            shutdown,
            task,
        }
    }

    async fn post_ingest_status(base_url: String, payload: IngestEnvelope) -> u16 {
        tokio::task::spawn_blocking(move || {
            let response = ureq::post(&format!("{base_url}/ingest"))
                .send_json(serde_json::to_value(payload).expect("payload json"));
            match response {
                Ok(response) => response.status(),
                Err(ureq::Error::Status(code, _response)) => code,
                Err(error) => panic!("http request failed: {error}"),
            }
        })
        .await
        .expect("join blocking request")
    }

    async fn wait_for_write_attempts(
        write_attempts: &mut mpsc::UnboundedReceiver<()>,
        expected: usize,
    ) {
        for _ in 0..expected {
            timeout(Duration::from_secs(1), write_attempts.recv())
                .await
                .expect("write attempt timeout")
                .expect("write attempt");
        }
    }

    async fn receive_single_dispatch_and_ack(
        requests: &mut mpsc::Receiver<TestStoreTurnRequest>,
        pool: &SqlitePool,
    ) -> i64 {
        let request = timeout(Duration::from_secs(1), requests.recv())
            .await
            .expect("dispatch timeout")
            .expect("dispatch request");
        mark_processing_turn_stored(pool, request.turn_id).await;
        request.responder.send(Ok(())).expect("ack success");
        tokio::task::yield_now().await;
        assert!(requests.try_recv().is_err(), "expected a single dispatch");
        request.turn_id
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

    fn fixed_user_payload(conversation_id: &str, fragment_id: &str, text: &str) -> IngestEnvelope {
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
                    text: text.to_string(),
                },
            },
            raw: json!({"session_id": conversation_id}),
        }
    }

    fn external_user_payload(
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

    fn external_assistant_stop_payload(
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

    fn external_tool_call_payload(
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

    fn test_health() -> HealthResponse {
        HealthResponse {
            ok: true,
            namespace: "default".to_string(),
            port: 3737,
            llm_provider: "anthropic".to_string(),
            encoder_provider: "fast-embed".to_string(),
            llm_model_id: Some("Anthropic::claude-sonnet-4-6".to_string()),
            encoder_model_id: Some("FastEmbed::default".to_string()),
            version: "test",
        }
    }

    fn sample_payload() -> IngestEnvelope {
        IngestEnvelope {
            protocol_version: INGEST_PROTOCOL_VERSION,
            source: SourceMeta {
                agent: "claude-code".to_string(),
                event: "UserPromptSubmit".to_string(),
            },
            scope: ConversationScope {
                conversation_id: "session-http-test".to_string(),
                turn_id: None,
                fragment_id: format!("fragment-{}", unique_suffix()),
                sequence_hint: Some(1),
                emitted_at_rfc3339: Some("2026-03-05T00:00:00Z".to_string()),
            },
            fragment: ConversationFragment {
                terminality: Terminality::None,
                body: FragmentBody::UserMessage {
                    text: "hello".to_string(),
                },
            },
            raw: json!({"session_id": "session-http-test"}),
        }
    }

    fn test_db_path() -> PathBuf {
        std::env::temp_dir().join(format!(
            "memory_bank_http_server_test_{}.db",
            unique_suffix()
        ))
    }

    fn unique_suffix() -> u128 {
        let time_component = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("time")
            .as_nanos();
        let counter_component = u128::from(UNIQUE_COUNTER.fetch_add(1, Ordering::Relaxed));

        (time_component << 16) | counter_component
    }
}

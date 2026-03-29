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
use tracing::{info, warn};

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
        let bind_addr = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
        let shutdown = CancellationToken::new();
        let app = build_app(health, memory, ingest, log_tx, &shutdown);
        let listener = TcpListener::bind(bind_addr)
            .await
            .map_err(|e| AppError::HttpServer(format!("Failed to bind to {}: {}", bind_addr, e)))?;

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
            warn!(error = %err, "Rejected ingest request with invalid JSON body");
            return StatusCode::BAD_REQUEST;
        }
    };

    match state.ingest.ingest(request).await {
        Ok(outcome) => {
            info!(
                agent = %outcome.agent,
                event = %outcome.event,
                conversation_id = %outcome.conversation_id,
                fragment_id = %outcome.fragment_id,
                turn_index = outcome.turn_index,
                duplicate = outcome.duplicate,
                finalized = outcome.finalized,
                "Accepted ingest fragment",
            );
            StatusCode::ACCEPTED
        }
        Err(IngestError::Validation(error)) => {
            warn!(error = %error, "Rejected ingest fragment after validation");
            StatusCode::UNPROCESSABLE_ENTITY
        }
        Err(error) => {
            warn!(error = %error, "Failed to stage ingest fragment");
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
    pub version: &'static str,
}

async fn shutdown_signal(shutdown: CancellationToken) {
    tokio::signal::ctrl_c().await.ok();
    info!("Shutdown signal received; waiting for in-flight requests to finish");
    shutdown.cancel();
}

#[cfg(test)]
mod tests {
    use super::{HealthResponse, build_app};
    use crate::actor::MemoryHandle;
    use crate::ingest::IngestService;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use memory_bank_protocol::{
        ConversationFragment, ConversationScope, FragmentBody, INGEST_PROTOCOL_VERSION,
        IngestEnvelope, SourceMeta, Terminality,
    };
    use rmcp::model::LoggingMessageNotificationParam;
    use serde_json::json;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};
    use tokio::sync::broadcast;
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

    #[tokio::test]
    async fn post_ingest_rejects_turn_ids_for_now() {
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

        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
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
            version: "test",
        };
        build_app(health, memory, ingest, log_tx, &shutdown)
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

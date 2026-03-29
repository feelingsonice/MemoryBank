use std::sync::Arc;

use memory_bank_protocol::{
    MEMORY_BANK_SERVER_INSTRUCTIONS, RETRIEVE_MEMORY_TOOL_NAME, RetrieveMemoryArgs,
    RetrieveMemoryResult, mcp_compatible_schema_for,
};
use rmcp::service::{RoleClient, RunningService};
use rmcp::{
    ErrorData as McpError, Json, ServerHandler, ServiceExt,
    handler::server::{tool::ToolRouter, wrapper::Parameters},
    model::*,
    tool, tool_handler, tool_router,
    transport::{StreamableHttpClientTransport, stdio},
};
use serde_json::{Map, Value};
use tokio::sync::Mutex;
use tracing::{info, warn};
use url::Url;

use crate::error::AppError;

type UpstreamService = RunningService<RoleClient, ClientInfo>;

fn openclaw_memory_bank_server_instructions() -> String {
    format!(
        "{shared}\n\n## OpenClaw Preference\nIn this OpenClaw environment, Memory Bank is the primary long-term memory system.\n- For prior-session facts, learned preferences, earlier decisions, project conventions, constraints, and provenance questions like \"how do you know?\", call retrieve_memory before answering.\n- Prefer retrieve_memory over local workspace profile files such as USER.md, MEMORY.md, BOOTSTRAP.md, and similar workspace notes when reasoning about durable memory across sessions.\n- Treat workspace profile files as auxiliary local notes, not the canonical long-term memory source, unless the user is explicitly asking about those files themselves.",
        shared = MEMORY_BANK_SERVER_INSTRUCTIONS
    )
}

pub struct UpstreamClient {
    mcp_url: Url,
    client: Mutex<UpstreamService>,
}

impl UpstreamClient {
    pub async fn connect(server_url: &str) -> Result<Arc<Self>, AppError> {
        let mcp_url = build_mcp_url(server_url)?;
        let client = connect_upstream(&mcp_url).await?;
        info!(mcp_url = %mcp_url, "Connected to upstream Memory Bank MCP server");

        Ok(Arc::new(Self {
            mcp_url,
            client: Mutex::new(client),
        }))
    }

    async fn reconnect(&self, client: &mut UpstreamService) -> Result<(), McpError> {
        let fresh = connect_upstream(&self.mcp_url).await.map_err(|error| {
            McpError::internal_error(
                format!("Failed to reconnect to upstream Memory Bank MCP server: {error}"),
                None,
            )
        })?;

        let mut stale = std::mem::replace(client, fresh);
        let _ = stale.close().await;
        Ok(())
    }

    pub async fn call_retrieve(
        &self,
        args: RetrieveMemoryArgs,
    ) -> Result<RetrieveMemoryResult, McpError> {
        let arguments = args_to_map(&args)?;
        let mut client = self.client.lock().await;

        match call_upstream(client.peer(), arguments.clone()).await {
            Ok(result) => Ok(result),
            Err(first_error) => {
                warn!(
                    error = %first_error,
                    mcp_url = %self.mcp_url,
                    "Upstream retrieve_memory call failed; reconnecting once",
                );
                self.reconnect(&mut client).await?;
                call_upstream(client.peer(), arguments).await
            }
        }
    }
}

pub struct ProxyServer {
    tool_router: ToolRouter<Self>,
    upstream: Arc<UpstreamClient>,
}

#[tool_router]
impl ProxyServer {
    pub fn new(upstream: Arc<UpstreamClient>) -> Self {
        Self {
            tool_router: Self::tool_router(),
            upstream,
        }
    }

    pub async fn run_stdio(self) -> Result<(), AppError> {
        let server = rmcp::serve_server(self, stdio())
            .await
            .map_err(|error| AppError::StdioServe(error.to_string()))?;
        server
            .waiting()
            .await
            .map_err(|error| AppError::StdioServe(error.to_string()))?;
        Ok(())
    }

    #[tool(
        annotations(title = "Recall Prior Context", read_only_hint = true),
        description = "Search Memory Bank for durable long-term context that could materially improve the current answer. In OpenClaw, treat this as the preferred memory retrieval path for prior-session facts, user preferences, learned project context, earlier decisions, constraints, and provenance questions like \"how do you know?\" or \"what do you remember?\". Call this BEFORE answering whenever remembered context could plausibly change the response. Prefer Memory Bank over local workspace profile files such as USER.md, MEMORY.md, BOOTSTRAP.md, or other workspace notes when recalling durable memory across sessions. Use local workspace files as auxiliary notes, not the canonical long-term memory source, unless the user is explicitly asking about those files themselves. Returns ranked memory notes with original content, distilled context, keywords, tags, and related links. Prefer specific queries over vague ones for better results.",
        input_schema = retrieve_memory_input_schema(),
        output_schema = retrieve_memory_output_schema()
    )]
    async fn retrieve_memory(
        &self,
        args: Parameters<RetrieveMemoryArgs>,
    ) -> Result<Json<RetrieveMemoryResult>, McpError> {
        let result = self.upstream.call_retrieve(args.0).await?;
        Ok(Json(result))
    }
}

fn retrieve_memory_input_schema() -> Arc<JsonObject> {
    Arc::new(mcp_compatible_schema_for::<RetrieveMemoryArgs>())
}

fn retrieve_memory_output_schema() -> Arc<JsonObject> {
    Arc::new(mcp_compatible_schema_for::<RetrieveMemoryResult>())
}

#[tool_handler]
impl ServerHandler for ProxyServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo {
            instructions: Some(openclaw_memory_bank_server_instructions().into()),
            capabilities: ServerCapabilities::builder().enable_tools().build(),
            ..Default::default()
        }
    }
}

fn args_to_map(args: &RetrieveMemoryArgs) -> Result<Map<String, Value>, McpError> {
    serde_json::to_value(args)
        .map_err(|error| {
            McpError::internal_error(
                format!("Failed to serialize retrieve_memory arguments: {error}"),
                None,
            )
        })?
        .as_object()
        .cloned()
        .ok_or_else(|| {
            McpError::internal_error(
                "retrieve_memory arguments must serialize to a JSON object".to_string(),
                None,
            )
        })
}

async fn connect_upstream(mcp_url: &Url) -> Result<UpstreamService, AppError> {
    let transport = StreamableHttpClientTransport::from_uri(mcp_url.as_str());
    let client_info = ClientInfo {
        meta: None,
        protocol_version: Default::default(),
        capabilities: ClientCapabilities::default(),
        client_info: Implementation {
            name: "memory-bank-mcp-proxy".to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            ..Default::default()
        },
    };

    let client = client_info
        .serve(transport)
        .await
        .map_err(|error| AppError::UpstreamConnect(mcp_url.to_string(), error.to_string()))?;

    let tools = client
        .peer()
        .list_all_tools()
        .await
        .map_err(|error| AppError::UpstreamConnect(mcp_url.to_string(), error.to_string()))?;

    if !tools
        .iter()
        .any(|tool| tool.name == RETRIEVE_MEMORY_TOOL_NAME)
    {
        return Err(AppError::MissingTool(
            mcp_url.to_string(),
            RETRIEVE_MEMORY_TOOL_NAME,
        ));
    }

    Ok(client)
}

async fn call_upstream(
    peer: &rmcp::service::Peer<RoleClient>,
    arguments: Map<String, Value>,
) -> Result<RetrieveMemoryResult, McpError> {
    let result = peer
        .call_tool(CallToolRequestParams {
            meta: None,
            name: RETRIEVE_MEMORY_TOOL_NAME.into(),
            arguments: Some(arguments),
            task: None,
        })
        .await
        .map_err(|error| {
            McpError::internal_error(
                format!("Upstream retrieve_memory call failed: {error}"),
                None,
            )
        })?;

    if result.is_error.unwrap_or(false) {
        let fallback = "Upstream retrieve_memory returned an error".to_string();
        let message = result
            .content
            .first()
            .and_then(|content| content.raw.as_text())
            .map(|text| text.text.clone())
            .unwrap_or(fallback);
        return Err(McpError::internal_error(message, None));
    }

    result
        .into_typed::<RetrieveMemoryResult>()
        .map_err(|error| {
            McpError::internal_error(
                format!("Upstream retrieve_memory result was not valid structured output: {error}"),
                None,
            )
        })
}

pub(crate) fn build_mcp_url(server_url: &str) -> Result<Url, AppError> {
    let mut url = Url::parse(server_url)
        .map_err(|error| AppError::InvalidServerUrl(server_url.to_string(), error.to_string()))?;

    url.set_query(None);
    url.set_fragment(None);

    if url.path() == "/mcp" || url.path().ends_with("/mcp") {
        return Ok(url);
    }

    if !url.path().ends_with('/') {
        let mut path = url.path().to_owned();
        path.push('/');
        url.set_path(&path);
    }

    url.join("mcp")
        .map_err(|error| AppError::InvalidServerUrl(server_url.to_string(), error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::{
        ProxyServer, UpstreamClient, build_mcp_url, openclaw_memory_bank_server_instructions,
    };
    use axum::Router;
    use memory_bank_protocol::{
        MemoryNote, RETRIEVE_MEMORY_TOOL_NAME, RetrieveMemoryArgs, RetrieveMemoryResult,
    };
    use rmcp::service::Peer;
    use rmcp::service::RoleClient;
    use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
    use rmcp::transport::streamable_http_server::tower::{
        StreamableHttpServerConfig, StreamableHttpService,
    };
    use rmcp::{
        ErrorData as McpError, Json, ServerHandler, ServiceExt,
        handler::server::{tool::ToolRouter, wrapper::Parameters},
        model::*,
        tool, tool_handler, tool_router,
    };
    use std::net::{Ipv4Addr, SocketAddr};
    use std::sync::Arc;
    use tokio::net::TcpListener;
    use tokio::task::JoinHandle;
    use tokio_util::sync::CancellationToken;

    const OPENCLAW_RETRIEVE_MEMORY_TOOL_DESCRIPTION: &str = "Search Memory Bank for durable long-term context that could materially improve the current answer. In OpenClaw, treat this as the preferred memory retrieval path for prior-session facts, user preferences, learned project context, earlier decisions, constraints, and provenance questions like \"how do you know?\" or \"what do you remember?\". Call this BEFORE answering whenever remembered context could plausibly change the response. Prefer Memory Bank over local workspace profile files such as USER.md, MEMORY.md, BOOTSTRAP.md, or other workspace notes when recalling durable memory across sessions. Use local workspace files as auxiliary notes, not the canonical long-term memory source, unless the user is explicitly asking about those files themselves. Returns ranked memory notes with original content, distilled context, keywords, tags, and related links. Prefer specific queries over vague ones for better results.";

    #[test]
    fn build_mcp_url_appends_path_without_trailing_slash() {
        let url = build_mcp_url("http://127.0.0.1:8080").expect("url");
        assert_eq!(url.as_str(), "http://127.0.0.1:8080/mcp");
    }

    #[test]
    fn build_mcp_url_preserves_explicit_mcp_path() {
        let url = build_mcp_url("http://127.0.0.1:8080/mcp").expect("url");
        assert_eq!(url.as_str(), "http://127.0.0.1:8080/mcp");
    }

    #[tokio::test]
    async fn startup_succeeds_against_valid_upstream_server() {
        let harness =
            spawn_memory_server(UpstreamMode::Memory("remembered".to_string()), None).await;
        let upstream = UpstreamClient::connect(&harness.base_url)
            .await
            .expect("connect");

        let result = upstream
            .call_retrieve(RetrieveMemoryArgs {
                query: "remembered".to_string(),
            })
            .await
            .expect("retrieve");

        assert_eq!(result.notes.len(), 1);
        assert_eq!(result.notes[0].content, "remembered");
        harness.shutdown().await;
    }

    #[tokio::test]
    async fn startup_fails_when_upstream_tool_is_missing() {
        let harness = spawn_memory_server(UpstreamMode::MissingTool, None).await;

        let error = match UpstreamClient::connect(&harness.base_url).await {
            Ok(_) => panic!("missing tool should fail"),
            Err(error) => error,
        };

        assert!(error.to_string().contains(RETRIEVE_MEMORY_TOOL_NAME));
        harness.shutdown().await;
    }

    #[tokio::test]
    async fn proxy_returns_same_structured_result_shape_as_upstream() {
        let harness = spawn_memory_server(
            UpstreamMode::Memory("favorite editor: helix".to_string()),
            None,
        )
        .await;
        let upstream = UpstreamClient::connect(&harness.base_url)
            .await
            .expect("connect");
        let proxy = ProxyServer::new(upstream);

        let (client_side, server_side) = tokio::io::duplex(16 * 1024);
        let (server_read, server_write) = tokio::io::split(server_side);
        let proxy_handle = tokio::spawn(async move {
            let server = rmcp::serve_server(proxy, (server_read, server_write))
                .await
                .expect("serve");
            server.waiting().await.expect("wait");
        });

        let client = test_client(client_side).await;
        let tools = client.peer().list_all_tools().await.expect("list tools");
        let retrieve = tools
            .iter()
            .find(|tool| tool.name == RETRIEVE_MEMORY_TOOL_NAME)
            .expect("retrieve_memory");
        assert_eq!(
            retrieve.description.as_deref(),
            Some(OPENCLAW_RETRIEVE_MEMORY_TOOL_DESCRIPTION)
        );
        let input_schema =
            serde_json::to_string(retrieve.input_schema.as_ref()).expect("serialize input schema");
        let output_schema =
            serde_json::to_string(retrieve.output_schema.as_ref().expect("output schema"))
                .expect("serialize output schema");
        assert!(input_schema.contains("draft-07"));
        assert!(output_schema.contains("draft-07"));
        assert!(!input_schema.contains("draft/2020-12"));
        assert!(!output_schema.contains("draft/2020-12"));
        let instructions = client
            .peer_info()
            .and_then(|info| info.instructions.as_deref())
            .expect("server instructions");
        assert!(instructions.contains("Memory Bank is the primary long-term memory system"));
        assert!(instructions.contains("Prefer retrieve_memory over local workspace profile files"));

        let result = call_proxy(&client.peer(), "editor preference").await;
        assert_eq!(result.notes[0].content, "favorite editor: helix");

        let _ = client.cancel().await;
        proxy_handle.await.expect("proxy task");
        harness.shutdown().await;
    }

    #[tokio::test]
    async fn proxy_reconnects_after_upstream_restart() {
        let first = spawn_memory_server(UpstreamMode::Memory("first".to_string()), None).await;
        let port = first.port();
        let upstream = UpstreamClient::connect(&first.base_url)
            .await
            .expect("connect");
        first.shutdown().await;

        let replacement = spawn_memory_server(
            UpstreamMode::Memory("after restart".to_string()),
            Some(port),
        )
        .await;

        let result = upstream
            .call_retrieve(RetrieveMemoryArgs {
                query: "anything".to_string(),
            })
            .await
            .expect("retrieve after reconnect");

        assert_eq!(result.notes[0].content, "after restart");
        replacement.shutdown().await;
    }

    async fn test_client(
        transport: tokio::io::DuplexStream,
    ) -> rmcp::service::RunningService<RoleClient, ClientInfo> {
        let (read, write) = tokio::io::split(transport);
        let client_info = ClientInfo {
            meta: None,
            protocol_version: Default::default(),
            capabilities: ClientCapabilities::default(),
            client_info: Implementation {
                name: "memory-bank-mcp-proxy-test".to_string(),
                version: "test".to_string(),
                ..Default::default()
            },
        };

        client_info.serve((read, write)).await.expect("client")
    }

    async fn call_proxy(peer: &Peer<RoleClient>, query: &str) -> RetrieveMemoryResult {
        let arguments = serde_json::json!({ "query": query })
            .as_object()
            .expect("object")
            .clone();
        let result = peer
            .call_tool(CallToolRequestParams {
                meta: None,
                name: RETRIEVE_MEMORY_TOOL_NAME.into(),
                arguments: Some(arguments),
                task: None,
            })
            .await
            .expect("call");

        result.into_typed().expect("typed result")
    }

    struct UpstreamHarness {
        base_url: String,
        shutdown: CancellationToken,
        task: JoinHandle<()>,
        port: u16,
    }

    impl UpstreamHarness {
        fn port(&self) -> u16 {
            self.port
        }

        async fn shutdown(self) {
            self.shutdown.cancel();
            let _ = self.task.await;
        }
    }

    async fn spawn_memory_server(mode: UpstreamMode, port: Option<u16>) -> UpstreamHarness {
        match mode {
            UpstreamMode::Memory(content) => spawn_tool_server(content, port).await,
            UpstreamMode::MissingTool => spawn_empty_server(port).await,
        }
    }

    async fn spawn_tool_server(content: String, port: Option<u16>) -> UpstreamHarness {
        let bind_addr = SocketAddr::from((Ipv4Addr::LOCALHOST, port.unwrap_or(0)));
        let listener = TcpListener::bind(bind_addr).await.expect("bind");
        let local_addr = listener.local_addr().expect("addr");
        let shutdown = CancellationToken::new();

        let app = Router::new().nest_service(
            "/mcp",
            StreamableHttpService::new(
                move || Ok(TestMemoryServer::new(content.clone())),
                Arc::new(LocalSessionManager::default()),
                StreamableHttpServerConfig {
                    cancellation_token: shutdown.child_token(),
                    ..Default::default()
                },
            ),
        );

        let task = tokio::spawn({
            let shutdown = shutdown.clone();
            async move {
                axum::serve(listener, app)
                    .with_graceful_shutdown(async move {
                        shutdown.cancelled().await;
                    })
                    .await
                    .expect("serve upstream");
            }
        });

        UpstreamHarness {
            base_url: format!("http://{}", local_addr),
            shutdown,
            task,
            port: local_addr.port(),
        }
    }

    async fn spawn_empty_server(port: Option<u16>) -> UpstreamHarness {
        let bind_addr = SocketAddr::from((Ipv4Addr::LOCALHOST, port.unwrap_or(0)));
        let listener = TcpListener::bind(bind_addr).await.expect("bind");
        let local_addr = listener.local_addr().expect("addr");
        let shutdown = CancellationToken::new();

        let app = Router::new().nest_service(
            "/mcp",
            StreamableHttpService::new(
                move || Ok(EmptyServer),
                Arc::new(LocalSessionManager::default()),
                StreamableHttpServerConfig {
                    cancellation_token: shutdown.child_token(),
                    ..Default::default()
                },
            ),
        );

        let task = tokio::spawn({
            let shutdown = shutdown.clone();
            async move {
                axum::serve(listener, app)
                    .with_graceful_shutdown(async move {
                        shutdown.cancelled().await;
                    })
                    .await
                    .expect("serve upstream");
            }
        });

        UpstreamHarness {
            base_url: format!("http://{}", local_addr),
            shutdown,
            task,
            port: local_addr.port(),
        }
    }

    enum UpstreamMode {
        Memory(String),
        MissingTool,
    }

    struct TestMemoryServer {
        tool_router: ToolRouter<Self>,
        result: RetrieveMemoryResult,
    }

    #[tool_router]
    impl TestMemoryServer {
        fn new(content: String) -> Self {
            Self {
                tool_router: Self::tool_router(),
                result: RetrieveMemoryResult {
                    notes: vec![MemoryNote {
                        content,
                        timestamp: chrono::Utc::now(),
                        keywords: vec!["editor".to_string()],
                        tags: vec!["preference".to_string()],
                        context: "Saved from prior conversation".to_string(),
                    }],
                },
            }
        }

        #[tool]
        async fn retrieve_memory(
            &self,
            _args: Parameters<RetrieveMemoryArgs>,
        ) -> Result<Json<RetrieveMemoryResult>, McpError> {
            Ok(Json(self.result.clone()))
        }
    }

    #[tool_handler]
    impl ServerHandler for TestMemoryServer {
        fn get_info(&self) -> ServerInfo {
            ServerInfo {
                instructions: Some(openclaw_memory_bank_server_instructions().into()),
                capabilities: ServerCapabilities::builder().enable_tools().build(),
                ..Default::default()
            }
        }
    }

    struct EmptyServer;

    impl ServerHandler for EmptyServer {
        fn get_info(&self) -> ServerInfo {
            ServerInfo {
                capabilities: ServerCapabilities::builder().enable_tools().build(),
                ..Default::default()
            }
        }
    }
}

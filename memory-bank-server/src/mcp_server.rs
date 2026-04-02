use std::sync::Arc;

use memory_bank_protocol::{
    MEMORY_BANK_SERVER_INSTRUCTIONS, RetrieveMemoryArgs, RetrieveMemoryResult,
    mcp_compatible_schema_for,
};
use rmcp::{
    ErrorData as McpError, Json, ServerHandler,
    handler::server::{tool::ToolRouter, wrapper::Parameters},
    model::*,
    service::ServiceError,
    tool, tool_handler, tool_router,
};
use tokio::sync::broadcast;
use tracing::{debug, warn};

use crate::actor::MemoryHandle;
use crate::logging::INTERNAL_LOG_STREAM_TARGET;

pub struct McpServer {
    tool_router: ToolRouter<Self>,
    memory: MemoryHandle,
    log_rx: broadcast::Receiver<LoggingMessageNotificationParam>,
}

#[tool_router]
impl McpServer {
    pub fn new(
        memory: MemoryHandle,
        log_tx: broadcast::Sender<LoggingMessageNotificationParam>,
    ) -> Self {
        Self {
            tool_router: Self::tool_router(),
            memory,
            log_rx: log_tx.subscribe(),
        }
    }

    #[tool(
        annotations(title = "Recall Prior Context", read_only_hint = true),
        description = "Search long-term memory for any previously learned context that could materially improve the current answer. Call this BEFORE answering whenever prior conversations, user or project context, earlier decisions, constraints, or learned facts might help you answer more accurately, consistently, or personally. Use it even when the user does not explicitly ask you to recall something and even when the request is indirect, transformed, or requires you to apply or synthesize prior context rather than repeat it verbatim. If the answer could plausibly change after checking memory, retrieve first. Returns a ranked list of memory notes, each containing the original content, conversation context distilled from the captured conversation window, keywords, tags, and links to related memories. Prefer specific queries over vague ones for better results.",
        input_schema = retrieve_memory_input_schema(),
        output_schema = retrieve_memory_output_schema()
    )]
    async fn retrieve_memory(
        &self,
        args: Parameters<RetrieveMemoryArgs>,
    ) -> Result<Json<RetrieveMemoryResult>, McpError> {
        let RetrieveMemoryArgs { query } = args.0;
        debug!(
            query_chars = query.chars().count(),
            "Running retrieve_memory request"
        );

        let notes = self.memory.retrieve(query).await?;
        Ok(Json(RetrieveMemoryResult { notes }))
    }
}

fn retrieve_memory_input_schema() -> Arc<JsonObject> {
    Arc::new(mcp_compatible_schema_for::<RetrieveMemoryArgs>())
}

fn retrieve_memory_output_schema() -> Arc<JsonObject> {
    Arc::new(mcp_compatible_schema_for::<RetrieveMemoryResult>())
}

#[tool_handler]
impl ServerHandler for McpServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo {
            instructions: Some(MEMORY_BANK_SERVER_INSTRUCTIONS.into()),
            capabilities: ServerCapabilities::builder()
                .enable_tools()
                .enable_logging()
                .build(),
            ..Default::default()
        }
    }

    async fn on_initialized(&self, context: rmcp::service::NotificationContext<rmcp::RoleServer>) {
        let peer = context.peer.clone();
        let mut log_rx = self.log_rx.resubscribe();

        tokio::spawn(async move {
            loop {
                match log_rx.recv().await {
                    Ok(param) => {
                        if let Err(error) = peer.notify_logging_message(param).await {
                            if is_expected_log_stream_disconnect(&error) {
                                debug!(
                                    target: INTERNAL_LOG_STREAM_TARGET,
                                    error = %error,
                                    "MCP client disconnected; stopping log stream"
                                );
                            } else {
                                warn!(
                                    target: INTERNAL_LOG_STREAM_TARGET,
                                    error = %error,
                                    "Stopping MCP log stream because sending a log notification failed"
                                );
                            }
                            break;
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        warn!(
                            target: INTERNAL_LOG_STREAM_TARGET,
                            dropped_messages = n,
                            "MCP log stream fell behind and dropped buffered messages"
                        );
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        });
    }
}

fn is_expected_log_stream_disconnect(error: &ServiceError) -> bool {
    match error {
        ServiceError::TransportClosed => true,
        ServiceError::TransportSend(error) => error.error.to_string() == "Transport closed",
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::{McpServer, is_expected_log_stream_disconnect};
    use crate::actor::MemoryHandle;
    use memory_bank_protocol::{
        MEMORY_BANK_SERVER_INSTRUCTIONS, RETRIEVE_MEMORY_TOOL_NAME, RETRIEVE_MEMORY_TOOL_TITLE,
    };
    use rmcp::{ServerHandler, service::ServiceError, transport::DynamicTransportError};
    use std::{any::TypeId, borrow::Cow, io};
    use tokio::sync::broadcast;

    const SERVER_RETRIEVE_MEMORY_TOOL_DESCRIPTION: &str = "Search long-term memory for any previously learned context that could materially improve the current answer. Call this BEFORE answering whenever prior conversations, user or project context, earlier decisions, constraints, or learned facts might help you answer more accurately, consistently, or personally. Use it even when the user does not explicitly ask you to recall something and even when the request is indirect, transformed, or requires you to apply or synthesize prior context rather than repeat it verbatim. If the answer could plausibly change after checking memory, retrieve first. Returns a ranked list of memory notes, each containing the original content, conversation context distilled from the captured conversation window, keywords, tags, and links to related memories. Prefer specific queries over vague ones for better results.";

    #[test]
    fn retrieve_memory_tool_exposes_output_schema() {
        let (log_tx, _) = broadcast::channel(8);
        let server = McpServer::new(MemoryHandle::closed_for_tests(), log_tx);
        let tools = server.tool_router.list_all();

        let retrieve_memory = tools
            .iter()
            .find(|tool| tool.name == RETRIEVE_MEMORY_TOOL_NAME)
            .expect("retrieve_memory tool");

        assert!(retrieve_memory.output_schema.is_some());
        let annotations = retrieve_memory
            .annotations
            .as_ref()
            .expect("tool annotations");
        assert_eq!(
            annotations.title.as_deref(),
            Some(RETRIEVE_MEMORY_TOOL_TITLE)
        );
        assert_eq!(annotations.read_only_hint, Some(true));
        assert_eq!(
            retrieve_memory.description.as_deref(),
            Some(SERVER_RETRIEVE_MEMORY_TOOL_DESCRIPTION)
        );
    }

    #[test]
    fn retrieve_memory_output_schema_includes_descriptions() {
        let (log_tx, _) = broadcast::channel(8);
        let server = McpServer::new(MemoryHandle::closed_for_tests(), log_tx);
        let tools = server.tool_router.list_all();

        let retrieve_memory = tools
            .iter()
            .find(|tool| tool.name == RETRIEVE_MEMORY_TOOL_NAME)
            .expect("retrieve_memory tool");

        let schema = serde_json::to_string(
            retrieve_memory
                .output_schema
                .as_ref()
                .expect("output schema"),
        )
        .expect("serialize schema");

        assert!(schema.contains("Structured response returned by the retrieve_memory tool."));
        assert!(schema.contains(
            "Memory notes relevant to the query, including their content and retrieval metadata."
        ));
        assert!(schema.contains("A stored long-term memory note returned by memory retrieval."));
        assert!(
            schema
                .contains("Rendered memory content captured from the original conversation turn.")
        );
        assert!(schema.contains("draft-07"));
        assert!(!schema.contains("draft/2020-12"));
    }

    #[test]
    fn server_instructions_emphasize_material_improvement_rule() {
        let (log_tx, _) = broadcast::channel(8);
        let server = McpServer::new(MemoryHandle::closed_for_tests(), log_tx);
        let info = server.get_info();
        let instructions = info.instructions.expect("server instructions");

        assert!(instructions.contains(
            "Before answering, retrieve whenever prior context could materially improve the answer."
        ));
        assert!(
            instructions
                .contains("If prior context could plausibly change the answer, retrieve first.")
        );
        assert!(instructions.contains(
            "The current request asks you to apply, interpret, or synthesize what was learned earlier"
        ));
        assert_eq!(instructions, MEMORY_BANK_SERVER_INSTRUCTIONS);
    }

    #[test]
    fn transport_closed_errors_are_treated_as_expected_disconnects() {
        assert!(is_expected_log_stream_disconnect(
            &ServiceError::TransportClosed
        ));

        let error = ServiceError::TransportSend(DynamicTransportError {
            transport_name: Cow::Borrowed("test"),
            transport_type_id: TypeId::of::<()>(),
            error: Box::new(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "Transport closed",
            )),
        });
        assert!(is_expected_log_stream_disconnect(&error));
    }

    #[test]
    fn unexpected_log_stream_errors_remain_actionable() {
        assert!(!is_expected_log_stream_disconnect(
            &ServiceError::UnexpectedResponse
        ));
    }
}

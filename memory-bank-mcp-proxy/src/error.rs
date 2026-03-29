use thiserror::Error;

#[derive(Debug, Error)]
pub enum AppError {
    #[error("invalid Memory Bank server URL '{0}': {1}")]
    InvalidServerUrl(String, String),
    #[error("failed to connect to upstream Memory Bank MCP server at {0}: {1}")]
    UpstreamConnect(String, String),
    #[error("upstream Memory Bank MCP server at {0} does not expose '{1}'")]
    MissingTool(String, &'static str),
    #[error("failed to serve stdio MCP proxy: {0}")]
    StdioServe(String),
}

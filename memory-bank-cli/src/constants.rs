use std::time::Duration;

pub(crate) const MB_BINARY_NAME: &str = "mb";
pub(crate) const SERVER_BINARY_NAME: &str = "memory-bank-server";
pub(crate) const HOOK_BINARY_NAME: &str = "memory-bank-hook";
pub(crate) const MCP_PROXY_BINARY_NAME: &str = "memory-bank-mcp-proxy";
pub(crate) const LAUNCHD_LABEL: &str = "com.memory-bank.mb";
pub(crate) const SYSTEMD_UNIT_NAME: &str = "memory-bank.service";
pub(crate) const REMOTE_MODEL_CATALOG_URL: &str = "https://raw.githubusercontent.com/feelingsonice/MemoryBank/main/config/setup-model-catalog.json";
pub(crate) const HEALTH_STARTUP_TIMEOUT: Duration = Duration::from_secs(10);
pub(crate) const HEALTH_POLL_INTERVAL: Duration = Duration::from_secs(2);
pub(crate) const SERVICE_TRANSITION_TIMEOUT: Duration = Duration::from_secs(2);
pub(crate) const SERVICE_TRANSITION_POLL_INTERVAL: Duration = Duration::from_millis(100);
pub(crate) const DEFAULT_HISTORY_WINDOW_SIZE: u32 = memory_bank_app::DEFAULT_HISTORY_WINDOW_SIZE;
pub(crate) const OLLAMA_HISTORY_WINDOW_SIZE: u32 = memory_bank_app::OLLAMA_HISTORY_WINDOW_SIZE;
pub(crate) const DEFAULT_NEAREST_NEIGHBOR_COUNT: i32 = 10;
pub(crate) const LOG_TAIL_LINE_COUNT: &str = "200";
pub(crate) const EMBEDDED_OPENCODE_PLUGIN: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../.opencode/plugins/memory-bank.js"
));
pub(crate) const EMBEDDED_OPENCLAW_INDEX: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../.openclaw/extensions/memory-bank/index.js"
));
pub(crate) const EMBEDDED_OPENCLAW_MANIFEST: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../.openclaw/extensions/memory-bank/openclaw.plugin.json"
));
pub(crate) const EMBEDDED_OPENCLAW_PACKAGE: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../.openclaw/extensions/memory-bank/package.json"
));
pub(crate) const EMBEDDED_MODEL_CATALOG: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../config/setup-model-catalog.json"
));

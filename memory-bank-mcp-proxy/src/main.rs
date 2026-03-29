use memory_bank_mcp_proxy::{ProxyArgs, run};

#[tokio::main]
async fn main() -> Result<(), memory_bank_mcp_proxy::AppError> {
    run(ProxyArgs::parse()).await
}

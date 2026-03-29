use memory_bank_server::{config::ServeArgs, run};

#[tokio::main]
async fn main() -> Result<(), memory_bank_server::error::AppError> {
    let args = ServeArgs::parse();
    run(args.try_into()?).await
}

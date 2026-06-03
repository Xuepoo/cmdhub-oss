use anyhow::Result;

#[tokio::main]
async fn main() -> Result<()> {
    cmdhub_cli::run().await
}

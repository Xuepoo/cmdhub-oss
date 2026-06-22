use anyhow::Result;

#[tokio::main]
async fn main() -> Result<()> {
    // Restore default SIGPIPE so `cmdh search … | head`/`| jq` exits quietly instead
    // of panicking on a broken pipe (EPIPE). See cmdhub_cli::reset_sigpipe.
    cmdhub_cli::reset_sigpipe();
    cmdhub_cli::run().await
}

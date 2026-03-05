mod cli;
mod runtime;
mod vcl_loader;

use tracing::info;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Initialize logging
    tracing_subscriber::fmt::init();

    info!("varaha-cache 0.1.0 starting");

    // Parse CLI arguments
    let mut args = cli::parse_args();

    // Initialize runtime
    let runtime = runtime::ServerRuntime::new(&mut args)?;

    // Run server
    runtime.run(&args).await?;

    Ok(())
}

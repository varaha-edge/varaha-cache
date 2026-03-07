mod cli;
mod runtime;
mod telemetry;
mod vcl_loader;

use tracing::info;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let otel_endpoint = std::env::var("OTEL_EXPORTER_OTLP_ENDPOINT")
        .unwrap_or_else(|_| "http://otel.intra.varaha.io:4317".to_string());

    let provider = telemetry::init(&otel_endpoint);

    info!("varaha-cache 0.1.0 starting");

    // Parse CLI arguments
    let mut args = cli::parse_args();

    // Initialize runtime
    let runtime = runtime::ServerRuntime::new(&mut args)?;

    // Run server
    runtime.run(&args).await?;

    // Flush pending spans on shutdown
    provider.shutdown().ok();

    Ok(())
}

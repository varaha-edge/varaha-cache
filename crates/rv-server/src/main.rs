mod cli;
mod fleet;
mod runtime;
mod telemetry;
mod vcl_loader;

use tracing::info;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let otel_endpoint = std::env::var("OTEL_EXPORTER_OTLP_ENDPOINT")
        .unwrap_or_else(|_| "https://otel.intra.varaha.io".to_string());

    let providers = telemetry::init(&otel_endpoint);

    info!("varaha-cache 0.1.0 starting");

    // Parse CLI arguments
    let mut args = cli::parse_args();

    // Initialize runtime
    let runtime = runtime::ServerRuntime::new(&mut args)?;

    // Register cache metrics now that the engine exists
    telemetry::register_cache_metrics(&providers.meter, runtime.cache.stats_ref().clone());

    // Run server
    runtime.run(&args).await?;

    // Flush all pending telemetry on shutdown
    providers.shutdown();

    Ok(())
}

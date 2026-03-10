use anyhow::Result;
use tracing_subscriber::{EnvFilter, layer::SubscriberExt, util::SubscriberInitExt};

use rv_agent::agent::Agent;
use rv_agent::config::AgentConfig;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::registry()
        .with(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")))
        .with(tracing_subscriber::fmt::layer())
        .init();

    let config = AgentConfig::from_env()?;
    tracing::info!(
        control_plane = %config.control_plane_url,
        nats = %config.nats_url,
        "starting rv-agent"
    );

    let agent = Agent::new(config).await?;
    agent.run().await
}

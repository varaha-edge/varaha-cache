use anyhow::{Context, Result};
use std::env;
use std::path::PathBuf;
use std::time::Duration;

/// Configuration for the rv-agent edge agent.
#[derive(Debug, Clone)]
pub struct AgentConfig {
    /// Control plane API base URL (e.g., "http://cp-api:8080")
    pub control_plane_url: String,
    /// NATS server URL (e.g., "nats://nats:4222")
    pub nats_url: String,
    /// API token for control plane authentication
    pub api_token: String,
    /// Node hostname (defaults to system hostname)
    pub hostname: String,
    /// Region this node belongs to
    pub region: String,
    /// POP (point of presence) this node belongs to
    pub pop: String,
    /// Heartbeat interval
    pub heartbeat_interval: Duration,
    /// Local state directory
    pub state_dir: PathBuf,
    /// Control plane request timeout
    pub request_timeout: Duration,
}

impl AgentConfig {
    pub fn from_env() -> Result<Self> {
        let hostname = env::var("RV_HOSTNAME")
            .or_else(|_| hostname::get().map(|h| h.to_string_lossy().to_string()))
            .unwrap_or_else(|_| "unknown".to_string());

        Ok(Self {
            control_plane_url: env::var("CONTROL_PLANE_URL")
                .context("CONTROL_PLANE_URL must be set")?,
            nats_url: env::var("NATS_URL").context("NATS_URL must be set")?,
            api_token: env::var("VARAHA_API_TOKEN").unwrap_or_default(),
            hostname,
            region: env::var("RV_REGION").unwrap_or_else(|_| "default".to_string()),
            pop: env::var("RV_POP").unwrap_or_else(|_| "default".to_string()),
            heartbeat_interval: Duration::from_secs(
                env::var("RV_HEARTBEAT_INTERVAL_SECS")
                    .unwrap_or_else(|_| "30".to_string())
                    .parse()
                    .unwrap_or(30),
            ),
            state_dir: PathBuf::from(
                env::var("RV_STATE_DIR").unwrap_or_else(|_| "/var/lib/varaha-cache".to_string()),
            ),
            request_timeout: Duration::from_secs(10),
        })
    }
}

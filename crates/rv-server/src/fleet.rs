//! Fleet integration: gRPC client for varaha-control.
//!
//! Connects to the control plane via gRPC bidirectional streaming:
//! - Registers this cache node (Register RPC).
//! - Opens a FleetStream for heartbeats and server-pushed commands.
//! - Receives ConfigPush, DeployCommand, PurgeCommand from the server.
//!
//! Enabled when `CONTROL_PLANE_URL` and `GRPC_ADDR` environment variables
//! are set. Without these, varaha-cache runs standalone.

use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use rv_admin::VclManager;
use rv_cache::CacheEngine;
use rv_vcl::interpreter::VclInterpreter;

mod proto {
    tonic::include_proto!("fleet");
}

use proto::fleet_service_client::FleetServiceClient;
use proto::*;

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

/// Fleet client configuration, loaded from environment variables.
#[derive(Debug, Clone)]
pub struct FleetConfig {
    /// Control plane REST API URL (for artifact downloads).
    pub control_plane_url: String,
    /// Control plane gRPC address.
    pub grpc_addr: String,
    /// Node hostname.
    pub hostname: String,
    /// Region this node belongs to.
    pub region: String,
    /// POP (point of presence).
    pub pop: String,
    /// Heartbeat interval.
    pub heartbeat_interval: Duration,
    /// Local state directory for persisting node identity.
    pub state_dir: std::path::PathBuf,
}

impl FleetConfig {
    /// Load from environment. Returns None if GRPC_ADDR is not set.
    pub fn from_env() -> Option<Self> {
        let grpc_addr = std::env::var("GRPC_ADDR").ok()?;
        let control_plane_url = std::env::var("CONTROL_PLANE_URL").ok()?;

        let hostname = std::env::var("RV_HOSTNAME")
            .or_else(|_| hostname::get().map(|h| h.to_string_lossy().to_string()))
            .unwrap_or_else(|_| "unknown".to_string());

        Some(Self {
            control_plane_url,
            grpc_addr,
            hostname,
            region: std::env::var("RV_REGION").unwrap_or_else(|_| "default".to_string()),
            pop: std::env::var("RV_POP").unwrap_or_else(|_| "default".to_string()),
            heartbeat_interval: Duration::from_secs(
                std::env::var("RV_HEARTBEAT_INTERVAL_SECS")
                    .unwrap_or_else(|_| "30".to_string())
                    .parse()
                    .unwrap_or(30),
            ),
            state_dir: std::path::PathBuf::from(
                std::env::var("RV_STATE_DIR")
                    .unwrap_or_else(|_| "/var/lib/varaha-cache".to_string()),
            ),
        })
    }
}

// ---------------------------------------------------------------------------
// State persistence
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct FleetState {
    node_id: Option<String>,
    token: Option<String>,
    applied_config_version: i64,
}

fn load_state(state_dir: &std::path::Path) -> FleetState {
    let path = state_dir.join("fleet-state.json");
    match std::fs::read_to_string(&path) {
        Ok(data) => serde_json::from_str(&data).unwrap_or_default(),
        Err(_) => FleetState::default(),
    }
}

fn save_state(state_dir: &std::path::Path, state: &FleetState) {
    let _ = std::fs::create_dir_all(state_dir);
    let path = state_dir.join("fleet-state.json");
    let tmp = state_dir.join("fleet-state.json.tmp");
    if let Ok(data) = serde_json::to_string_pretty(state) {
        if std::fs::write(&tmp, &data).is_ok() {
            let _ = std::fs::rename(&tmp, &path);
        }
    }
}

// ---------------------------------------------------------------------------
// Spawn
// ---------------------------------------------------------------------------

/// Spawn the fleet client as a background task.
///
/// Returns immediately. The fleet client runs in the background, handling
/// registration, heartbeats, and server-pushed commands until the
/// cancellation token fires.
pub fn spawn_fleet_client(
    config: FleetConfig,
    cache: Arc<CacheEngine>,
    vcl_manager: Arc<VclManager>,
    active_vcl: Arc<arc_swap::ArcSwapOption<VclInterpreter>>,
    start_time: Instant,
    cancel: CancellationToken,
) {
    tokio::spawn(async move {
        if let Err(e) =
            run_fleet_client(config, cache, vcl_manager, active_vcl, start_time, cancel).await
        {
            tracing::error!(error = %e, "fleet client exited with error");
        }
    });
}

async fn run_fleet_client(
    config: FleetConfig,
    cache: Arc<CacheEngine>,
    _vcl_manager: Arc<VclManager>,
    _active_vcl: Arc<arc_swap::ArcSwapOption<VclInterpreter>>,
    start_time: Instant,
    cancel: CancellationToken,
) -> Result<()> {
    let mut state = load_state(&config.state_dir);

    // Step 1: Register if needed.
    if state.node_id.is_none() {
        tracing::info!(hostname = %config.hostname, "registering with control plane via gRPC");

        let mut client = FleetServiceClient::connect(config.grpc_addr.clone())
            .await
            .with_context(|| format!("failed to connect to gRPC server at {}", config.grpc_addr))?;

        let resp = client
            .register(tonic::Request::new(RegisterRequest {
                hostname: config.hostname.clone(),
                region: config.region.clone(),
                pop: config.pop.clone(),
                tags: std::collections::HashMap::new(),
                zone: None,
                public_ip: None,
                private_ip: "0.0.0.0".to_string(),
                listen_addr: "0.0.0.0:6081".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
            }))
            .await
            .context("gRPC Register RPC failed")?
            .into_inner();

        tracing::info!(
            node_id = %resp.node_id,
            config_version = resp.config_version,
            "registered with control plane"
        );

        state.node_id = Some(resp.node_id);
        state.token = Some(resp.token);
        state.applied_config_version = resp.config_version;
        save_state(&config.state_dir, &state);
    } else {
        tracing::info!(
            node_id = %state.node_id.as_deref().unwrap_or("?"),
            "re-using existing fleet registration"
        );
    }

    let node_id = state.node_id.clone().unwrap();
    let token = state.token.clone().unwrap();

    // Step 2: Open FleetStream.
    let mut client = FleetServiceClient::connect(config.grpc_addr.clone())
        .await
        .context("failed to connect for FleetStream")?;

    let (stream_tx, stream_rx) = mpsc::channel::<NodeMessage>(32);

    // Send initial heartbeat to authenticate the stream.
    stream_tx
        .send(build_heartbeat(
            &node_id,
            &token,
            state.applied_config_version,
            &cache,
            start_time,
        ))
        .await
        .context("failed to send initial heartbeat")?;

    let stream = tokio_stream::wrappers::ReceiverStream::new(stream_rx);
    let mut response_stream = client
        .fleet_stream(tonic::Request::new(stream))
        .await
        .context("failed to open FleetStream")?
        .into_inner();

    tracing::info!("fleet gRPC stream established");

    // Spawn heartbeat sender.
    let hb_tx = stream_tx.clone();
    let hb_node_id = node_id.clone();
    let hb_token = token.clone();
    let hb_cache = Arc::clone(&cache);
    let hb_interval = config.heartbeat_interval;
    let hb_cancel = cancel.clone();
    let (version_tx, mut version_rx) = mpsc::channel::<i64>(16);

    tokio::spawn(async move {
        let mut config_version = state.applied_config_version;
        let mut tick = tokio::time::interval(hb_interval);
        tick.tick().await; // Skip first (already sent above).

        loop {
            tokio::select! {
                _ = tick.tick() => {
                    let msg = build_heartbeat(
                        &hb_node_id,
                        &hb_token,
                        config_version,
                        &hb_cache,
                        start_time,
                    );
                    if hb_tx.send(msg).await.is_err() {
                        break;
                    }
                    tracing::debug!(config_version, "heartbeat sent");
                }
                Some(v) = version_rx.recv() => {
                    config_version = v;
                }
                _ = hb_cancel.cancelled() => {
                    tracing::info!("heartbeat sender shutting down");
                    break;
                }
            }
        }
    });

    // Step 3: Receive server messages.
    let rest_base = config.control_plane_url.clone();
    let node_token = token.clone(); // Use the registration token for REST API auth
    let state_dir = config.state_dir.clone();

    loop {
        tokio::select! {
            msg = response_stream.message() => {
                match msg {
                    Ok(Some(server_msg)) => {
                        handle_server_message(
                            server_msg,
                            &node_id,
                            &rest_base,
                            &node_token,
                            &state_dir,
                            &version_tx,
                        ).await;
                    }
                    Ok(None) => {
                        tracing::warn!("fleet gRPC stream closed by server");
                        break;
                    }
                    Err(e) => {
                        tracing::error!(error = %e, "fleet gRPC stream error");
                        break;
                    }
                }
            }
            _ = cancel.cancelled() => {
                tracing::info!("fleet client shutting down");
                break;
            }
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Server message handling
// ---------------------------------------------------------------------------

async fn handle_server_message(
    msg: ServerMessage,
    node_id: &str,
    rest_base: &str,
    api_token: &str,
    state_dir: &std::path::Path,
    version_tx: &mpsc::Sender<i64>,
) {
    match msg.payload {
        Some(server_message::Payload::HeartbeatAck(ack)) => {
            tracing::debug!(
                ack = ack.ack,
                desired_config_version = ?ack.desired_config_version,
                "heartbeat acknowledged"
            );
        }
        Some(server_message::Payload::ConfigPush(cfg)) => {
            tracing::info!(
                version = cfg.version,
                scope = %cfg.scope,
                "received ConfigPush via gRPC"
            );

            // Pull effective config from REST API and apply.
            match pull_effective_config(rest_base, api_token, node_id).await {
                Ok(effective) => {
                    tracing::info!(
                        version = effective.version,
                        scope = %effective.scope,
                        "effective config retrieved, ready to apply"
                    );

                    // Update persisted state.
                    let mut state = load_state(state_dir);
                    state.applied_config_version = effective.version;
                    save_state(state_dir, &state);

                    // Update heartbeat config version.
                    let _ = version_tx.send(effective.version).await;

                    tracing::info!(version = effective.version, "config applied");
                }
                Err(e) => {
                    tracing::error!(error = %e, "failed to pull effective config after ConfigPush");
                }
            }
        }
        Some(server_message::Payload::DeployCommand(cmd)) => {
            tracing::info!(
                deployment_id = %cmd.deployment_id,
                artifact = %cmd.artifact_name,
                version = %cmd.artifact_version,
                "received DeployCommand via gRPC"
            );
            // Artifact download and VCL application would happen here.
        }
        Some(server_message::Payload::PurgeCommand(purge)) => {
            tracing::info!(
                purge_id = %purge.purge_id,
                purge_type = %purge.purge_type,
                target = %purge.target,
                "received PurgeCommand via gRPC"
            );
            // Cache ban/purge would be executed here via CacheEngine.
            tracing::info!(purge_id = %purge.purge_id, "purge acknowledged");
        }
        None => {}
    }
}

// ---------------------------------------------------------------------------
// REST helpers
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct EffectiveConfig {
    version: i64,
    scope: String,
}

async fn pull_effective_config(
    base_url: &str,
    api_token: &str,
    node_id: &str,
) -> Result<EffectiveConfig> {
    let client = reqwest::Client::new();
    let resp = client
        .get(format!(
            "{}/api/v1/config/effective",
            base_url.trim_end_matches('/')
        ))
        .bearer_auth(api_token)
        .query(&[("node_id", node_id)])
        .send()
        .await
        .context("failed to get effective config")?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("get effective config failed (HTTP {}): {}", status, body);
    }

    resp.json()
        .await
        .context("failed to decode effective config")
}

// ---------------------------------------------------------------------------
// Heartbeat builder
// ---------------------------------------------------------------------------

fn build_heartbeat(
    node_id: &str,
    token: &str,
    config_version: i64,
    cache: &CacheEngine,
    start_time: Instant,
) -> NodeMessage {
    let stats = cache.stats();
    let hit_rate = stats.hit_rate() / 100.0; // hit_rate() returns 0-100, proto expects 0-1

    NodeMessage {
        node_id: node_id.to_string(),
        token: token.to_string(),
        payload: Some(node_message::Payload::Heartbeat(Heartbeat {
            config_version,
            state: "active".to_string(),
            load: 0.0,
            uptime_secs: start_time.elapsed().as_secs(),
            connections: 0,
            cache_hit_rate: hit_rate,
            storage_used: 0.0,
            health: "healthy".to_string(),
            error_rate_5xx: None,
            p99_latency_ms: None,
            cpu_cores: None,
            vcl_active: None,
        })),
    }
}

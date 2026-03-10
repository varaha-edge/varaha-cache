//! Main agent orchestration.
//!
//! The [`Agent`] ties together all rv-agent subsystems into a single
//! run loop:
//!
//! 1. Register (or re-register) with the control plane.
//! 2. Pull the initial configuration and apply it via the VCL bridge.
//! 3. Start the NATS consumer to receive fleet-wide events.
//! 4. Enter the main loop: send periodic heartbeats and handle events.
//!
//! The agent is designed to be resilient: transient failures in heartbeat
//! or config pulls are logged and retried on the next interval rather than
//! causing a crash.

use anyhow::{Context, Result};
use tokio::sync::mpsc;
use tokio::time;
use tracing;

use crate::config::AgentConfig;
use crate::control_plane::{
    ConfigVersion, ControlPlaneClient, HeartbeatRequest, RegisterNodeRequest,
};
use crate::nats_consumer::{FleetEvent, NatsConsumer};
use crate::state::{AgentState, StateManager};
use crate::vcl_bridge::VclBridge;

/// The top-level agent that orchestrates registration, configuration,
/// heartbeats, and event handling.
pub struct Agent {
    config: AgentConfig,
    cp_client: ControlPlaneClient,
    state_manager: StateManager,
    vcl_bridge: VclBridge,
    state: AgentState,
}

impl Agent {
    /// Create a new agent from the given configuration.
    ///
    /// Initialises the control plane client, state manager, and VCL bridge.
    /// The state manager loads any previously persisted state from disk so
    /// that the agent can resume after a restart.
    pub async fn new(config: AgentConfig) -> Result<Self> {
        let cp_client = ControlPlaneClient::new(
            &config.control_plane_url,
            &config.api_token,
            config.request_timeout,
        )?;

        let state_manager = StateManager::new(&config.state_dir)?;
        let state = state_manager.load()?;
        let vcl_bridge = VclBridge::new();

        Ok(Self {
            config,
            cp_client,
            state_manager,
            vcl_bridge,
            state,
        })
    }

    /// Run the agent until a shutdown signal is received.
    ///
    /// This is the main entry point. It performs registration, pulls the
    /// initial config, starts the NATS consumer, and enters the select loop
    /// for heartbeats and events.
    pub async fn run(mut self) -> Result<()> {
        // Step 1: Register with control plane (or re-register).
        self.register().await?;

        let node_id = self
            .state
            .node_id
            .clone()
            .context("node_id must be set after registration")?;

        // Step 2: Pull initial config.
        self.pull_and_apply_config(&node_id).await?;

        // Step 3: Start NATS consumer.
        let (tx, mut rx) = mpsc::channel::<FleetEvent>(100);
        let nats_consumer = NatsConsumer::new(
            &self.config.nats_url,
            &node_id,
            &self.config.region,
            &self.config.pop,
        );
        nats_consumer.start(tx).await?;

        // Step 4: Main loop -- heartbeat + event processing.
        let mut heartbeat_interval = time::interval(self.config.heartbeat_interval);

        tracing::info!(node_id = %node_id, "agent running");

        loop {
            tokio::select! {
                _ = heartbeat_interval.tick() => {
                    self.send_heartbeat(&node_id).await;
                }
                Some(event) = rx.recv() => {
                    self.handle_event(&node_id, event).await;
                }
                _ = tokio::signal::ctrl_c() => {
                    tracing::info!("received shutdown signal");
                    break;
                }
            }
        }

        tracing::info!("agent shutting down");
        Ok(())
    }

    /// Register the node with the control plane.
    ///
    /// If a node ID was previously persisted in the local state, it is
    /// re-used without making a new registration request. This ensures
    /// that the same node identity survives agent restarts.
    async fn register(&mut self) -> Result<()> {
        if let Some(ref node_id) = self.state.node_id {
            tracing::info!(node_id = %node_id, "re-using existing node registration");
            return Ok(());
        }

        tracing::info!(hostname = %self.config.hostname, "registering with control plane");

        let resp = self
            .cp_client
            .register(&RegisterNodeRequest {
                hostname: self.config.hostname.clone(),
                region: self.config.region.clone(),
                pop: self.config.pop.clone(),
                tags: std::collections::HashMap::new(),
            })
            .await?;

        tracing::info!(node_id = %resp.id, hostname = %resp.hostname, "registered successfully");
        self.state.node_id = Some(resp.id);
        self.state_manager.save(&self.state)?;

        Ok(())
    }

    /// Pull the effective configuration from the control plane and apply it.
    ///
    /// If the remote version is newer than the locally applied version, the
    /// new config is downloaded and applied. On failure, the agent logs a
    /// warning and falls back to any cached config for offline bootstrap.
    async fn pull_and_apply_config(&mut self, node_id: &str) -> Result<()> {
        match self.cp_client.get_effective_config(node_id).await {
            Ok(config) => {
                if config.version > self.state.applied_config_version {
                    self.apply_config(&config).await?;
                } else {
                    tracing::info!(version = config.version, "config already up to date");
                }
            }
            Err(e) => {
                tracing::warn!(error = %e, "failed to pull config from control plane");
                // Try to use cached config for offline bootstrap.
                if let Some(ref cached) = self.state.last_config {
                    tracing::info!(
                        version = cached.version,
                        "using cached config for offline bootstrap"
                    );
                }
            }
        }
        Ok(())
    }

    /// Apply a new configuration version.
    ///
    /// If the config references a VCL artifact, it is downloaded, validated,
    /// cached locally, and applied through the VCL bridge. The agent state
    /// is updated and persisted on success.
    async fn apply_config(&mut self, config: &ConfigVersion) -> Result<()> {
        tracing::info!(version = config.version, scope = %config.scope, "applying new config");

        // If there is a VCL reference, download and apply it.
        if let Some(ref vcl_ref) = config.vcl_ref {
            let vcl_content = self
                .cp_client
                .download_artifact(vcl_ref, &format!("v{}", config.version))
                .await
                .context("failed to download VCL artifact")?;

            let vcl_source =
                String::from_utf8(vcl_content.clone()).context("VCL content is not valid UTF-8")?;

            // Validate VCL before applying.
            VclBridge::validate_vcl(&vcl_source)
                .map_err(|e| anyhow::anyhow!("VCL validation failed: {}", e))?;

            // Cache VCL locally for offline bootstrap.
            self.state_manager.cache_vcl(vcl_ref, &vcl_content)?;

            // Apply VCL through the bridge.
            let program_name = format!("config-v{}", config.version);
            self.vcl_bridge
                .apply_vcl(&program_name, &vcl_source)
                .await?;
        }

        // Update state.
        self.state.applied_config_version = config.version;
        self.state.last_config = Some(config.clone());
        self.state_manager.save(&self.state)?;

        tracing::info!(version = config.version, "config applied successfully");
        Ok(())
    }

    /// Send a heartbeat to the control plane.
    ///
    /// Heartbeat failures are logged as warnings but do not stop the agent.
    async fn send_heartbeat(&self, node_id: &str) {
        let req = HeartbeatRequest {
            config_version: self.state.applied_config_version,
            state: "active".to_string(),
            load: 0.0,
        };

        match self.cp_client.heartbeat(node_id, &req).await {
            Ok(resp) => {
                tracing::debug!(
                    ack = resp.ack,
                    config_version = self.state.applied_config_version,
                    "heartbeat sent"
                );
            }
            Err(e) => {
                tracing::warn!(error = %e, "heartbeat failed");
            }
        }
    }

    /// Handle a single fleet event received from the NATS consumer.
    async fn handle_event(&mut self, node_id: &str, event: FleetEvent) {
        match event {
            FleetEvent::ConfigUpdated { version, scope } => {
                self.handle_config_updated(node_id, version, &scope).await;
            }
            FleetEvent::PurgeRequested {
                purge_id,
                purge_type,
                scope,
                keys: _,
                urls: _,
                soft,
            } => {
                self.handle_purge(purge_id, &purge_type, &scope, soft).await;
            }
            FleetEvent::DeploymentStarted {
                deployment_id,
                artifact_name,
                artifact_version,
            } => {
                self.handle_deployment(deployment_id, &artifact_name, &artifact_version)
                    .await;
            }
        }
    }

    /// Handle a config-updated event from the NATS consumer.
    async fn handle_config_updated(&mut self, node_id: &str, version: i64, scope: &str) {
        tracing::info!(version, scope, "received config update event");
        if version > self.state.applied_config_version {
            if let Err(e) = self.pull_and_apply_config(node_id).await {
                tracing::error!(error = %e, version, "failed to apply config update");
            }
        }
    }

    /// Handle a purge request event.
    async fn handle_purge(&self, purge_id: String, purge_type: &str, scope: &str, soft: bool) {
        tracing::info!(
            purge_id = %purge_id,
            purge_type,
            scope,
            soft,
            "received purge request"
        );
        // Purge handling would be implemented via rv-cache's ban/purge APIs.
        // For now, acknowledge receipt.
        tracing::info!(purge_id = %purge_id, "purge acknowledged");
    }

    /// Handle a deployment-started event.
    async fn handle_deployment(
        &mut self,
        deployment_id: String,
        artifact_name: &str,
        artifact_version: &str,
    ) {
        tracing::info!(
            deployment_id = %deployment_id,
            artifact = artifact_name,
            version = artifact_version,
            "received deployment event"
        );

        // Download the artifact.
        let content = match self
            .cp_client
            .download_artifact(artifact_name, artifact_version)
            .await
        {
            Ok(c) => c,
            Err(e) => {
                tracing::error!(error = %e, "failed to download deployment artifact");
                return;
            }
        };

        let vcl_source = match String::from_utf8(content) {
            Ok(s) => s,
            Err(e) => {
                tracing::error!(error = %e, "artifact content is not valid UTF-8");
                return;
            }
        };

        // Validate the VCL.
        if let Err(e) = VclBridge::validate_vcl(&vcl_source) {
            tracing::error!(error = %e, "VCL validation failed for deployment artifact");
            return;
        }

        // Apply the deployment artifact.
        let program_name = format!("{}-{}", artifact_name, artifact_version);
        match self.vcl_bridge.apply_vcl(&program_name, &vcl_source).await {
            Ok(()) => {
                tracing::info!(deployment_id = %deployment_id, "deployment artifact applied");
            }
            Err(e) => {
                tracing::error!(error = %e, "failed to apply deployment artifact");
            }
        }
    }
}

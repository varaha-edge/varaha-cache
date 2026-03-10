use anyhow::{Context, Result};
use async_nats::jetstream;
use serde::Deserialize;
use tokio::sync::mpsc;
use tracing;

/// Events received from the control plane via NATS.
#[derive(Debug, Clone)]
pub enum FleetEvent {
    /// New config version published
    ConfigUpdated { version: i64, scope: String },
    /// Purge request
    PurgeRequested {
        purge_id: String,
        purge_type: String,
        scope: String,
        keys: Vec<String>,
        urls: Vec<String>,
        soft: bool,
    },
    /// Deployment started targeting this node
    DeploymentStarted {
        deployment_id: String,
        artifact_name: String,
        artifact_version: String,
    },
}

/// NATS message payloads
#[derive(Debug, Deserialize)]
struct ConfigUpdatedPayload {
    version: i64,
    scope: String,
}

#[derive(Debug, Deserialize)]
struct PurgePayload {
    id: String,
    #[serde(rename = "type")]
    purge_type: String,
    scope: String,
    #[serde(default)]
    keys: Vec<String>,
    #[serde(default)]
    urls: Vec<String>,
    #[serde(default)]
    soft: bool,
}

#[derive(Debug, Deserialize)]
struct DeploymentPayload {
    deployment_id: String,
    artifact_name: String,
    artifact_version: String,
}

/// NATS JetStream consumer for fleet events.
pub struct NatsConsumer {
    nats_url: String,
    node_id: String,
    region: String,
    pop: String,
}

impl NatsConsumer {
    pub fn new(nats_url: &str, node_id: &str, region: &str, pop: &str) -> Self {
        Self {
            nats_url: nats_url.to_string(),
            node_id: node_id.to_string(),
            region: region.to_string(),
            pop: pop.to_string(),
        }
    }

    /// Connect to NATS and subscribe to fleet event subjects.
    /// Returns a receiver channel that emits FleetEvents.
    /// The consumer runs in a background task.
    pub async fn start(&self, tx: mpsc::Sender<FleetEvent>) -> Result<()> {
        let client = async_nats::connect(&self.nats_url)
            .await
            .context("failed to connect to NATS")?;

        let jetstream = jetstream::new(client);

        // Get or create the fleet stream
        let stream = jetstream
            .get_or_create_stream(jetstream::stream::Config {
                name: "FLEET".to_string(),
                subjects: vec![
                    "fleet.config.>".to_string(),
                    "fleet.purge.>".to_string(),
                    "fleet.deploy.>".to_string(),
                ],
                ..Default::default()
            })
            .await
            .context("failed to get/create FLEET stream")?;

        // Create a durable consumer for this node
        let consumer_name = format!("rv-agent-{}", self.node_id);
        let consumer = stream
            .get_or_create_consumer(
                &consumer_name,
                jetstream::consumer::pull::Config {
                    durable_name: Some(consumer_name.clone()),
                    filter_subjects: vec![
                        "fleet.config.updated".to_string(),
                        format!("fleet.config.region.{}", self.region),
                        format!("fleet.config.pop.{}", self.pop),
                        format!("fleet.config.node.{}", self.node_id),
                        "fleet.purge.global".to_string(),
                        format!("fleet.purge.region.{}", self.region),
                        format!("fleet.purge.pop.{}", self.pop),
                        format!("fleet.purge.node.{}", self.node_id),
                        format!("fleet.deploy.node.{}", self.node_id),
                    ],
                    ..Default::default()
                },
            )
            .await
            .context("failed to create NATS consumer")?;

        let tx_clone = tx.clone();
        let node_id = self.node_id.clone();

        tokio::spawn(async move {
            tracing::info!(consumer = %consumer_name, "NATS consumer started");

            let messages = match consumer.messages().await {
                Ok(m) => m,
                Err(e) => {
                    tracing::error!(error = %e, "failed to start NATS message stream");
                    return;
                }
            };

            use futures::StreamExt;
            let mut messages = messages;

            while let Some(msg_result) = messages.next().await {
                let msg = match msg_result {
                    Ok(m) => m,
                    Err(e) => {
                        tracing::warn!(error = %e, "error receiving NATS message");
                        continue;
                    }
                };

                let subject = msg.subject.as_str();
                let payload = msg.payload.as_ref();

                let event = if subject.starts_with("fleet.config.") {
                    match serde_json::from_slice::<ConfigUpdatedPayload>(payload) {
                        Ok(p) => Some(FleetEvent::ConfigUpdated {
                            version: p.version,
                            scope: p.scope,
                        }),
                        Err(e) => {
                            tracing::warn!(error = %e, subject, "failed to parse config event");
                            None
                        }
                    }
                } else if subject.starts_with("fleet.purge.") {
                    match serde_json::from_slice::<PurgePayload>(payload) {
                        Ok(p) => Some(FleetEvent::PurgeRequested {
                            purge_id: p.id,
                            purge_type: p.purge_type,
                            scope: p.scope,
                            keys: p.keys,
                            urls: p.urls,
                            soft: p.soft,
                        }),
                        Err(e) => {
                            tracing::warn!(error = %e, subject, "failed to parse purge event");
                            None
                        }
                    }
                } else if subject.starts_with("fleet.deploy.") {
                    match serde_json::from_slice::<DeploymentPayload>(payload) {
                        Ok(p) => Some(FleetEvent::DeploymentStarted {
                            deployment_id: p.deployment_id,
                            artifact_name: p.artifact_name,
                            artifact_version: p.artifact_version,
                        }),
                        Err(e) => {
                            tracing::warn!(error = %e, subject, "failed to parse deploy event");
                            None
                        }
                    }
                } else {
                    tracing::debug!(subject, "ignoring unknown subject");
                    None
                };

                if let Some(event) = event {
                    if let Err(e) = tx_clone.send(event).await {
                        tracing::error!(error = %e, "failed to send event to agent");
                        break;
                    }
                }

                // Acknowledge the message
                if let Err(e) = msg.ack().await {
                    tracing::warn!(
                        error = %e,
                        subject,
                        node_id = %node_id,
                        "failed to ack NATS message"
                    );
                }
            }

            tracing::warn!("NATS message stream ended");
        });

        Ok(())
    }
}

use anyhow::{Context, Result};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::time::Duration;

/// Registration request sent when the agent starts up.
#[derive(Debug, Serialize)]
pub struct RegisterNodeRequest {
    pub hostname: String,
    pub region: String,
    pub pop: String,
    pub tags: std::collections::HashMap<String, String>,
}

/// Registration response with assigned node ID.
#[derive(Debug, Deserialize)]
pub struct RegisterNodeResponse {
    pub id: String,
    pub hostname: String,
    pub region: String,
    pub pop: String,
}

/// Heartbeat request sent periodically.
#[derive(Debug, Serialize)]
pub struct HeartbeatRequest {
    pub config_version: i64,
    pub state: String,
    pub load: f64,
}

/// Heartbeat response from control plane.
#[derive(Debug, Deserialize)]
pub struct HeartbeatResponse {
    pub ack: bool,
    pub desired_config_version: Option<i64>,
}

/// Configuration version from the control plane.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ConfigVersion {
    pub version: i64,
    pub scope: String,
    pub parameters: String,
    pub backends: Option<String>,
    pub vcl_ref: Option<String>,
    pub wasm_modules: Option<String>,
    pub tls_config: Option<String>,
    pub storage_config: Option<String>,
    pub created_at: String,
}

/// Client for the varaha control plane REST API.
pub struct ControlPlaneClient {
    client: Client,
    base_url: String,
    api_token: String,
}

impl ControlPlaneClient {
    pub fn new(base_url: &str, api_token: &str, timeout: Duration) -> Result<Self> {
        let client = Client::builder()
            .timeout(timeout)
            .pool_max_idle_per_host(10)
            .build()
            .context("failed to build HTTP client")?;

        Ok(Self {
            client,
            base_url: base_url.trim_end_matches('/').to_string(),
            api_token: api_token.to_string(),
        })
    }

    /// Register this node with the control plane.
    /// POST /api/v1/nodes
    pub async fn register(&self, req: &RegisterNodeRequest) -> Result<RegisterNodeResponse> {
        let resp = self
            .client
            .post(format!("{}/api/v1/nodes", self.base_url))
            .bearer_auth(&self.api_token)
            .json(req)
            .send()
            .await
            .context("failed to register node")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("registration failed (HTTP {}): {}", status, body);
        }

        resp.json()
            .await
            .context("failed to decode registration response")
    }

    /// Send heartbeat to control plane.
    /// POST /api/v1/nodes/{id}/heartbeat
    pub async fn heartbeat(
        &self,
        node_id: &str,
        req: &HeartbeatRequest,
    ) -> Result<HeartbeatResponse> {
        let resp = self
            .client
            .post(format!(
                "{}/api/v1/nodes/{}/heartbeat",
                self.base_url, node_id
            ))
            .bearer_auth(&self.api_token)
            .json(req)
            .send()
            .await
            .context("failed to send heartbeat")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("heartbeat failed (HTTP {}): {}", status, body);
        }

        resp.json()
            .await
            .context("failed to decode heartbeat response")
    }

    /// Pull effective configuration for this node.
    /// GET /api/v1/config/effective?node_id={id}
    pub async fn get_effective_config(&self, node_id: &str) -> Result<ConfigVersion> {
        let resp = self
            .client
            .get(format!("{}/api/v1/config/effective", self.base_url))
            .bearer_auth(&self.api_token)
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
            .context("failed to decode config response")
    }

    /// Get a specific config version.
    /// GET /api/v1/config/{version}
    pub async fn get_config(&self, version: i64) -> Result<ConfigVersion> {
        let resp = self
            .client
            .get(format!("{}/api/v1/config/{}", self.base_url, version))
            .bearer_auth(&self.api_token)
            .send()
            .await
            .context("failed to get config")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("get config failed (HTTP {}): {}", status, body);
        }

        resp.json()
            .await
            .context("failed to decode config response")
    }

    /// Download artifact content.
    /// GET /api/v1/artifacts/{name}/{version}/content
    pub async fn download_artifact(&self, name: &str, version: &str) -> Result<Vec<u8>> {
        let resp = self
            .client
            .get(format!(
                "{}/api/v1/artifacts/{}/{}/content",
                self.base_url, name, version
            ))
            .bearer_auth(&self.api_token)
            .send()
            .await
            .context("failed to download artifact")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("download artifact failed (HTTP {}): {}", status, body);
        }

        resp.bytes()
            .await
            .map(|b| b.to_vec())
            .context("failed to read artifact bytes")
    }
}

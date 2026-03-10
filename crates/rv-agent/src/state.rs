use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

use crate::control_plane::ConfigVersion;

/// Persistent local state for the rv-agent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentState {
    /// Assigned node ID from control plane registration
    pub node_id: Option<String>,
    /// Currently applied config version number
    pub applied_config_version: i64,
    /// Last successfully applied configuration (for offline bootstrap)
    pub last_config: Option<ConfigVersion>,
}

impl Default for AgentState {
    fn default() -> Self {
        Self {
            node_id: None,
            applied_config_version: 0,
            last_config: None,
        }
    }
}

/// Manages persistent state on disk.
pub struct StateManager {
    state_dir: PathBuf,
    state_file: PathBuf,
}

impl StateManager {
    pub fn new(state_dir: &Path) -> Result<Self> {
        fs::create_dir_all(state_dir).with_context(|| {
            format!("failed to create state directory: {}", state_dir.display())
        })?;

        Ok(Self {
            state_dir: state_dir.to_path_buf(),
            state_file: state_dir.join("agent-state.json"),
        })
    }

    /// Load state from disk, returning default state if file doesn't exist.
    pub fn load(&self) -> Result<AgentState> {
        if !self.state_file.exists() {
            tracing::info!(path = %self.state_file.display(), "no existing state file, using defaults");
            return Ok(AgentState::default());
        }

        let data = fs::read_to_string(&self.state_file)
            .with_context(|| format!("failed to read state file: {}", self.state_file.display()))?;

        serde_json::from_str(&data)
            .with_context(|| format!("failed to parse state file: {}", self.state_file.display()))
    }

    /// Save state to disk atomically (write to temp file then rename).
    pub fn save(&self, state: &AgentState) -> Result<()> {
        let data =
            serde_json::to_string_pretty(state).context("failed to serialize agent state")?;

        let tmp_file = self.state_dir.join("agent-state.json.tmp");
        fs::write(&tmp_file, &data)
            .with_context(|| format!("failed to write temp state file: {}", tmp_file.display()))?;

        fs::rename(&tmp_file, &self.state_file).with_context(|| {
            format!(
                "failed to rename state file to: {}",
                self.state_file.display()
            )
        })?;

        tracing::debug!(
            version = state.applied_config_version,
            "state saved to disk"
        );

        Ok(())
    }

    /// Get the path where VCL files are cached locally.
    pub fn vcl_cache_dir(&self) -> PathBuf {
        self.state_dir.join("vcl")
    }

    /// Ensure the VCL cache directory exists and write VCL content.
    pub fn cache_vcl(&self, name: &str, content: &[u8]) -> Result<PathBuf> {
        let vcl_dir = self.vcl_cache_dir();
        fs::create_dir_all(&vcl_dir)
            .with_context(|| format!("failed to create VCL cache dir: {}", vcl_dir.display()))?;

        let vcl_path = vcl_dir.join(format!("{}.vcl", name));
        fs::write(&vcl_path, content)
            .with_context(|| format!("failed to cache VCL file: {}", vcl_path.display()))?;

        Ok(vcl_path)
    }
}

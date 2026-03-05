use std::path::{Path, PathBuf};

use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use tokio::sync::broadcast;
use tracing::{error, info};

use crate::config::CacheConfig;
use crate::loader::load_config;

/// Events emitted by the configuration file watcher.
///
/// Consumers can subscribe to these events to react to runtime
/// configuration changes without restarting the process.
#[derive(Debug, Clone)]
pub enum ConfigEvent {
    /// The main configuration file was re-read and parsed successfully.
    ConfigReloaded(CacheConfig),

    /// A VCL source file was reloaded. The payload is the new VCL source.
    VclReloaded(String),

    /// A backend definition changed (added, removed, or modified).
    BackendChanged,

    /// A single named parameter was changed.
    /// The first element is the parameter name, the second is the new value.
    ParameterChanged(String, String),
}

/// Watches configuration files for changes and emits events.
pub struct ConfigWatcher {
    config_path: PathBuf,
    tx: broadcast::Sender<ConfigEvent>,
    _watcher: RecommendedWatcher,
}

impl ConfigWatcher {
    /// Creates a new ConfigWatcher that monitors the given config file.
    /// Returns the watcher and a receiver for config events.
    pub fn new(config_path: impl AsRef<Path>) -> Result<(Self, broadcast::Receiver<ConfigEvent>), ConfigWatcherError> {
        let config_path = config_path.as_ref().to_path_buf();
        let (tx, rx) = broadcast::channel(16);

        let tx_clone = tx.clone();
        let path_clone = config_path.clone();

        let mut watcher = notify::recommended_watcher(move |res: Result<Event, notify::Error>| {
            match res {
                Ok(event) => {
                    if matches!(event.kind, EventKind::Modify(_) | EventKind::Create(_)) {
                        match load_config(&path_clone) {
                            Ok(config) => {
                                info!(path = %path_clone.display(), "configuration reloaded");
                                let _ = tx_clone.send(ConfigEvent::ConfigReloaded(config));
                            }
                            Err(e) => {
                                error!(path = %path_clone.display(), error = %e, "failed to reload config");
                            }
                        }
                    }
                }
                Err(e) => {
                    error!(error = %e, "file watcher error");
                }
            }
        }).map_err(ConfigWatcherError::Notify)?;

        // Watch the parent directory so we catch file replacements (atomic writes)
        let watch_path = config_path.parent().unwrap_or(Path::new("."));
        watcher
            .watch(watch_path, RecursiveMode::NonRecursive)
            .map_err(ConfigWatcherError::Notify)?;

        Ok((
            Self {
                config_path,
                tx,
                _watcher: watcher,
            },
            rx,
        ))
    }

    /// Returns the path being watched.
    pub fn config_path(&self) -> &Path {
        &self.config_path
    }

    /// Returns a new receiver for config events.
    pub fn subscribe(&self) -> broadcast::Receiver<ConfigEvent> {
        self.tx.subscribe()
    }
}

/// Errors from the config watcher.
#[derive(Debug, thiserror::Error)]
pub enum ConfigWatcherError {
    #[error("file watcher error: {0}")]
    Notify(#[from] notify::Error),
}

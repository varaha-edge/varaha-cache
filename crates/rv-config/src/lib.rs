//! Multi-format configuration loading for varaha-cache.
//!
//! This crate provides a unified [`CacheConfig`] struct that can be
//! deserialized from TOML, JSON, or YAML files.  It also exposes
//! helpers for parsing human-readable duration and size strings
//! (e.g. `"120s"`, `"256m"`), and a [`ConfigEvent`] enum for
//! future hot-reload support.

pub mod config;
pub mod loader;
pub mod parse;
pub mod watcher;

// Re-export the most-used types at crate root for convenience.
pub use config::{
    AclConfig, AclEntry, AdminConfig, BackendConfig, CacheConfig, FeatureFlags, HashConfig,
    ListenConfig, LogConfig, ProbeConfig, StorageConfig, ThreadPoolConfig, TimeoutConfig,
    TtlConfig, VclConfig,
};
pub use loader::{ConfigFormat, LoadError, load_config, load_from_str};
pub use parse::{ParseError, parse_duration, parse_size};
pub use watcher::{ConfigEvent, ConfigWatcher, ConfigWatcherError};

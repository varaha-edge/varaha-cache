use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Top-level cache configuration.
///
/// Mirrors the major configuration sections found in Varnish,
/// but expressed as a single, strongly-typed Rust struct that
/// can be loaded from TOML, JSON, or YAML.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct CacheConfig {
    /// Listening addresses and protocols.
    pub listen: Vec<ListenConfig>,

    /// Named storage backends (malloc, file, persistent).
    pub storage: HashMap<String, StorageConfig>,

    /// Hash algorithm selection.
    pub hash: HashConfig,

    /// Default TTL / grace / keep values.
    pub ttl: TtlConfig,

    /// Thread pool sizing.
    pub threads: ThreadPoolConfig,

    /// Various network timeouts.
    pub timeouts: TimeoutConfig,

    /// Origin / backend server definitions.
    pub backends: Vec<BackendConfig>,

    /// Named health-check probe definitions.
    pub probes: HashMap<String, ProbeConfig>,

    /// Named access-control lists.
    pub acls: HashMap<String, AclConfig>,

    /// Optional VCL program source.
    pub vcl: Option<VclConfig>,

    /// Admin / CLI listener.
    pub admin: AdminConfig,

    /// Shared-memory log configuration.
    pub logging: LogConfig,

    /// Runtime feature flags.
    pub features: FeatureFlags,
}

impl Default for CacheConfig {
    fn default() -> Self {
        Self {
            listen: vec![ListenConfig::default()],
            storage: {
                let mut m = HashMap::new();
                m.insert("default".to_string(), StorageConfig::default());
                m
            },
            hash: HashConfig::default(),
            ttl: TtlConfig::default(),
            threads: ThreadPoolConfig::default(),
            timeouts: TimeoutConfig::default(),
            backends: Vec::new(),
            probes: HashMap::new(),
            acls: HashMap::new(),
            vcl: None,
            admin: AdminConfig::default(),
            logging: LogConfig::default(),
            features: FeatureFlags::default(),
        }
    }
}

// ---------------------------------------------------------------------------
// Listen
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ListenConfig {
    /// Socket address, e.g. "0.0.0.0:8080".
    pub address: String,

    /// Protocol: "http" or "https".
    pub protocol: String,

    /// Path to PEM-encoded TLS certificate chain (required when protocol = "https").
    pub tls_cert: Option<String>,

    /// Path to PEM-encoded TLS private key (required when protocol = "https").
    pub tls_key: Option<String>,
}

impl Default for ListenConfig {
    fn default() -> Self {
        Self {
            address: "0.0.0.0:8080".to_string(),
            protocol: "http".to_string(),
            tls_cert: None,
            tls_key: None,
        }
    }
}

// ---------------------------------------------------------------------------
// Storage
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct StorageConfig {
    /// Storage type: "malloc", "file", "persistent".
    #[serde(rename = "type")]
    pub type_: String,

    /// Human-readable size string, e.g. "256m", "1g".
    pub size: String,

    /// Optional path for file-backed storage.
    pub path: Option<String>,
}

impl Default for StorageConfig {
    fn default() -> Self {
        Self {
            type_: "malloc".to_string(),
            size: "256m".to_string(),
            path: None,
        }
    }
}

// ---------------------------------------------------------------------------
// Hash
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct HashConfig {
    /// Hash implementation: "critbit", "simple", "classic".
    #[serde(rename = "type")]
    pub type_: String,
}

impl Default for HashConfig {
    fn default() -> Self {
        Self {
            type_: "critbit".to_string(),
        }
    }
}

// ---------------------------------------------------------------------------
// TTL
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct TtlConfig {
    /// Default object TTL, e.g. "120s".
    pub default_ttl: String,

    /// Default grace period, e.g. "10s".
    pub default_grace: String,

    /// Default keep window, e.g. "0s".
    pub default_keep: String,
}

impl Default for TtlConfig {
    fn default() -> Self {
        Self {
            default_ttl: "120s".to_string(),
            default_grace: "10s".to_string(),
            default_keep: "0s".to_string(),
        }
    }
}

// ---------------------------------------------------------------------------
// Thread Pool
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ThreadPoolConfig {
    /// Number of thread pools.
    pub pool_count: u32,

    /// Minimum threads per pool.
    pub thread_pool_min: u32,

    /// Maximum threads per pool.
    pub thread_pool_max: u32,
}

impl Default for ThreadPoolConfig {
    fn default() -> Self {
        Self {
            pool_count: 2,
            thread_pool_min: 100,
            thread_pool_max: 5000,
        }
    }
}

// ---------------------------------------------------------------------------
// Timeouts
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct TimeoutConfig {
    /// Backend connect timeout.
    pub connect_timeout: String,

    /// Time to wait for the first byte from the backend.
    pub first_byte_timeout: String,

    /// Maximum idle time between bytes from the backend.
    pub between_bytes_timeout: String,

    /// Client send timeout.
    pub send_timeout: String,

    /// Pipe mode timeout.
    pub pipe_timeout: String,

    /// Idle backend connection timeout.
    pub backend_idle_timeout: String,
}

impl Default for TimeoutConfig {
    fn default() -> Self {
        Self {
            connect_timeout: "3.5s".to_string(),
            first_byte_timeout: "60s".to_string(),
            between_bytes_timeout: "60s".to_string(),
            send_timeout: "60s".to_string(),
            pipe_timeout: "60s".to_string(),
            backend_idle_timeout: "60s".to_string(),
        }
    }
}

// ---------------------------------------------------------------------------
// Backend
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackendConfig {
    /// Logical name of the backend.
    pub name: String,

    /// Hostname or IP address.
    pub host: String,

    /// TCP port.
    pub port: u16,

    /// Name of a probe definition to use for health checking.
    pub probe: Option<String>,

    /// Maximum number of concurrent connections.
    pub max_connections: Option<u32>,

    /// Per-backend connect timeout override.
    pub connect_timeout: Option<String>,

    /// Per-backend first-byte timeout override.
    pub first_byte_timeout: Option<String>,

    /// Per-backend between-bytes timeout override.
    pub between_bytes_timeout: Option<String>,
}

// ---------------------------------------------------------------------------
// Probe
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ProbeConfig {
    /// URL path to probe.
    pub url: String,

    /// Interval between probes.
    pub interval: String,

    /// Probe request timeout.
    pub timeout: String,

    /// Number of good probes in the window required to mark healthy.
    pub threshold: u32,

    /// Sliding window size.
    pub window: u32,

    /// Initial assumed-good probes at startup.
    pub initial: Option<u32>,
}

impl Default for ProbeConfig {
    fn default() -> Self {
        Self {
            url: "/health".to_string(),
            interval: "5s".to_string(),
            timeout: "1s".to_string(),
            threshold: 3,
            window: 5,
            initial: None,
        }
    }
}

// ---------------------------------------------------------------------------
// ACL
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct AclConfig {
    /// Ordered list of ACL entries.
    pub entries: Vec<AclEntry>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct AclEntry {
    /// IP address or CIDR prefix.
    pub addr: String,

    /// Optional prefix length (e.g. 24 for /24).
    pub mask: Option<u8>,

    /// If true, this entry negates the match.
    pub negate: bool,
}

// ---------------------------------------------------------------------------
// VCL
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct VclConfig {
    /// Path to a VCL file on disk.
    pub file: Option<String>,

    /// Inline VCL source code.
    pub inline: Option<String>,
}

// ---------------------------------------------------------------------------
// Admin
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AdminConfig {
    /// Admin CLI listen address.
    pub listen: String,

    /// Path to the shared-secret file used for CLI authentication.
    pub secret_file: Option<String>,
}

impl Default for AdminConfig {
    fn default() -> Self {
        Self {
            listen: "127.0.0.1:6082".to_string(),
            secret_file: None,
        }
    }
}

// ---------------------------------------------------------------------------
// Logging
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct LogConfig {
    /// Size of the shared-memory log ring buffer.
    pub size: String,

    /// Log format: "binary" or "text".
    pub format: String,
}

impl Default for LogConfig {
    fn default() -> Self {
        Self {
            size: "80m".to_string(),
            format: "binary".to_string(),
        }
    }
}

// ---------------------------------------------------------------------------
// Feature Flags
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct FeatureFlags {
    /// Skip XML validity checks during ESI processing.
    pub esi_disable_xml_check: bool,

    /// Treat HTTPS URLs as HTTP during ESI processing.
    pub esi_ignore_https: bool,

    /// Ignore unknown ESI elements instead of failing.
    pub esi_ignore_other_elements: bool,

    /// Shorten panic messages (omit back-trace).
    pub short_panic: bool,

    /// Wait for silo to be fully loaded before accepting traffic.
    pub wait_silo: bool,

    /// Disable core dumps.
    pub no_coredump: bool,

    /// Enable HTTP/2 support.
    pub http2: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_is_valid() {
        let cfg = CacheConfig::default();
        assert_eq!(cfg.listen.len(), 1);
        assert_eq!(cfg.listen[0].address, "0.0.0.0:8080");
        assert_eq!(cfg.listen[0].protocol, "http");
        assert!(cfg.storage.contains_key("default"));
        assert_eq!(cfg.hash.type_, "critbit");
        assert_eq!(cfg.ttl.default_ttl, "120s");
        assert_eq!(cfg.threads.pool_count, 2);
        assert_eq!(cfg.threads.thread_pool_min, 100);
        assert_eq!(cfg.threads.thread_pool_max, 5000);
        assert_eq!(cfg.timeouts.connect_timeout, "3.5s");
        assert!(cfg.backends.is_empty());
        assert!(cfg.probes.is_empty());
        assert!(cfg.acls.is_empty());
        assert!(cfg.vcl.is_none());
        assert_eq!(cfg.admin.listen, "127.0.0.1:6082");
        assert_eq!(cfg.logging.size, "80m");
        assert_eq!(cfg.logging.format, "binary");
        assert!(!cfg.features.http2);
    }

    #[test]
    fn default_config_roundtrips_through_toml() {
        let cfg = CacheConfig::default();
        let toml_str = toml::to_string_pretty(&cfg).expect("serialize to TOML");
        let decoded: CacheConfig = toml::from_str(&toml_str).expect("deserialize from TOML");
        assert_eq!(decoded.listen.len(), cfg.listen.len());
        assert_eq!(decoded.hash.type_, cfg.hash.type_);
    }

    #[test]
    fn default_config_roundtrips_through_json() {
        let cfg = CacheConfig::default();
        let json_str = serde_json::to_string_pretty(&cfg).expect("serialize to JSON");
        let decoded: CacheConfig = serde_json::from_str(&json_str).expect("deserialize from JSON");
        assert_eq!(decoded.ttl.default_ttl, cfg.ttl.default_ttl);
    }
}

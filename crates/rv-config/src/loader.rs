use std::path::Path;
use thiserror::Error;

use crate::config::CacheConfig;

/// Supported configuration file formats.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigFormat {
    Toml,
    Json,
    Yaml,
}

#[derive(Debug, Error)]
pub enum LoadError {
    #[error("I/O error reading config: {0}")]
    Io(#[from] std::io::Error),

    #[error("unsupported config file extension: {0}")]
    UnsupportedExtension(String),

    #[error("TOML parse error: {0}")]
    Toml(#[from] toml::de::Error),

    #[error("JSON parse error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("YAML parse error: {0}")]
    Yaml(#[from] serde_yaml::Error),
}

/// Load a [`CacheConfig`] from a file, auto-detecting the format from
/// the file extension.
///
/// Recognised extensions: `.toml`, `.json`, `.yaml`, `.yml`.
pub fn load_config(path: &Path) -> Result<CacheConfig, LoadError> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();

    let format = match ext.as_str() {
        "toml" => ConfigFormat::Toml,
        "json" => ConfigFormat::Json,
        "yaml" | "yml" => ConfigFormat::Yaml,
        other => return Err(LoadError::UnsupportedExtension(other.to_string())),
    };

    let content = std::fs::read_to_string(path)?;
    load_from_str(&content, format)
}

/// Parse a [`CacheConfig`] from a string in the specified format.
pub fn load_from_str(content: &str, format: ConfigFormat) -> Result<CacheConfig, LoadError> {
    match format {
        ConfigFormat::Toml => {
            let cfg: CacheConfig = toml::from_str(content)?;
            Ok(cfg)
        }
        ConfigFormat::Json => {
            let cfg: CacheConfig = serde_json::from_str(content)?;
            Ok(cfg)
        }
        ConfigFormat::Yaml => {
            let cfg: CacheConfig = serde_yaml::from_str(content)?;
            Ok(cfg)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn load_minimal_toml() {
        let toml_str = r#"
[[listen]]
address = "0.0.0.0:8080"
protocol = "http"

[hash]
type = "critbit"

[ttl]
default_ttl = "120s"

[threads]
pool_count = 2

[timeouts]
connect_timeout = "3.5s"

[admin]
listen = "127.0.0.1:6082"

[logging]
size = "80m"

[features]
http2 = false
"#;
        let cfg = load_from_str(toml_str, ConfigFormat::Toml).expect("parse TOML");
        assert_eq!(cfg.listen[0].address, "0.0.0.0:8080");
        assert_eq!(cfg.hash.type_, "critbit");
    }

    #[test]
    fn load_minimal_json() {
        let json_str = r#"{
            "listen": [{"address": "127.0.0.1:80", "protocol": "http"}],
            "hash": {"type": "simple"},
            "ttl": {"default_ttl": "60s"},
            "admin": {"listen": "127.0.0.1:6082"},
            "logging": {"size": "40m"}
        }"#;
        let cfg = load_from_str(json_str, ConfigFormat::Json).expect("parse JSON");
        assert_eq!(cfg.listen[0].address, "127.0.0.1:80");
        assert_eq!(cfg.hash.type_, "simple");
    }

    #[test]
    fn load_minimal_yaml() {
        let yaml_str = r#"
listen:
  - address: "0.0.0.0:443"
    protocol: "https"
hash:
  type: "critbit"
ttl:
  default_ttl: "300s"
admin:
  listen: "127.0.0.1:6082"
logging:
  size: "80m"
"#;
        let cfg = load_from_str(yaml_str, ConfigFormat::Yaml).expect("parse YAML");
        assert_eq!(cfg.listen[0].protocol, "https");
        assert_eq!(cfg.ttl.default_ttl, "300s");
    }

    #[test]
    fn empty_string_gives_defaults() {
        // TOML: an empty document should produce all defaults.
        let cfg = load_from_str("", ConfigFormat::Toml).expect("parse empty TOML");
        assert_eq!(cfg.listen.len(), 1);
        assert_eq!(cfg.hash.type_, "critbit");
    }

    #[test]
    fn unsupported_extension() {
        let path = Path::new("/tmp/config.xml");
        let err = load_config(path);
        assert!(err.is_err());
    }

    #[test]
    fn load_with_backends() {
        let toml_str = r#"
[[backends]]
name = "origin"
host = "10.0.0.1"
port = 8080

[[backends]]
name = "fallback"
host = "10.0.0.2"
port = 8080
max_connections = 100
connect_timeout = "1s"
"#;
        let cfg = load_from_str(toml_str, ConfigFormat::Toml).expect("parse backends");
        assert_eq!(cfg.backends.len(), 2);
        assert_eq!(cfg.backends[0].name, "origin");
        assert_eq!(cfg.backends[1].max_connections, Some(100));
    }

    #[test]
    fn load_with_probes_and_acls() {
        let toml_str = r#"
[probes.health]
url = "/ping"
interval = "10s"
timeout = "2s"
threshold = 4
window = 8

[acls.internal]
entries = [
    { addr = "10.0.0.0", mask = 8 },
    { addr = "192.168.0.0", mask = 16 },
]
"#;
        let cfg = load_from_str(toml_str, ConfigFormat::Toml).expect("parse probes/acls");
        assert_eq!(cfg.probes["health"].url, "/ping");
        assert_eq!(cfg.probes["health"].window, 8);
        assert_eq!(cfg.acls["internal"].entries.len(), 2);
        assert_eq!(cfg.acls["internal"].entries[0].mask, Some(8));
    }

    #[test]
    fn load_with_vcl() {
        let toml_str = r#"
[vcl]
file = "/etc/varnish/default.vcl"
"#;
        let cfg = load_from_str(toml_str, ConfigFormat::Toml).expect("parse vcl");
        let vcl = cfg.vcl.expect("vcl should be present");
        assert_eq!(vcl.file.as_deref(), Some("/etc/varnish/default.vcl"));
    }

    #[test]
    fn load_with_feature_flags() {
        let json_str = r#"{
            "features": {
                "http2": true,
                "esi_disable_xml_check": true
            }
        }"#;
        let cfg = load_from_str(json_str, ConfigFormat::Json).expect("parse features");
        assert!(cfg.features.http2);
        assert!(cfg.features.esi_disable_xml_check);
        assert!(!cfg.features.no_coredump);
    }
}

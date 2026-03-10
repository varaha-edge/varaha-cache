//! VCL bridge: connects the control plane configuration to the local cache's
//! VCL manager.
//!
//! The [`VclBridge`] validates VCL source using `rv-vcl` directly, then applies
//! it to the running cache instance. It supports two modes of operation:
//!
//! 1. **Direct mode** -- when the agent runs in-process with the cache and has
//!    access to a shared [`VclManager`] via `Arc`, VCL programs are loaded and
//!    activated through the manager's library API.
//!
//! 2. **Admin CLI mode** -- when the agent communicates with a separate cache
//!    process, commands are sent over the admin TCP socket using the wire
//!    protocol defined in `rv-admin`.
//!
//! The bridge always validates VCL locally (using `rv-vcl`) before attempting
//! to load it, providing fast failure without a network round-trip.

use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tracing;

use rv_admin::VclManager;
use rv_admin::protocol::{CliResponse, CliStatus, decode_response};
use rv_vcl::{Lexer, Parser};

/// Operational mode for the VCL bridge.
enum BridgeMode {
    /// In-process access to the VCL manager (shared via Arc).
    Direct { vcl_manager: Arc<VclManager> },
    /// Out-of-process access via the admin CLI TCP socket.
    AdminCli {
        addr: SocketAddr,
        secret: Option<String>,
    },
}

/// Bridges control plane VCL configuration to the local cache instance.
///
/// The bridge is responsible for:
/// - Validating VCL source using the `rv-vcl` lexer and parser.
/// - Loading validated VCL programs into the cache.
/// - Activating new programs (making them the live configuration).
/// - Rolling back to the previous program if activation fails.
pub struct VclBridge {
    mode: BridgeMode,
    /// Name of the last successfully activated program (used for rollback).
    previous_active: Option<String>,
}

impl VclBridge {
    /// Create a new VCL bridge in direct mode, sharing the given VCL manager.
    ///
    /// This is the preferred mode when the agent runs inside the same process
    /// as the cache engine.
    pub fn new_direct(vcl_manager: Arc<VclManager>) -> Self {
        Self {
            mode: BridgeMode::Direct { vcl_manager },
            previous_active: None,
        }
    }

    /// Create a new VCL bridge in admin CLI mode, connecting to the cache's
    /// admin TCP socket.
    ///
    /// The optional `secret` is used for challenge-response authentication
    /// as described in the `rv-admin::auth` module.
    pub fn new_admin_cli(addr: SocketAddr, secret: Option<String>) -> Self {
        Self {
            mode: BridgeMode::AdminCli { addr, secret },
            previous_active: None,
        }
    }

    /// Create a VCL bridge using direct mode with a fresh, empty VCL manager.
    ///
    /// Useful for standalone operation or testing.
    pub fn new() -> Self {
        Self::new_direct(Arc::new(VclManager::new()))
    }

    /// Validate VCL source without loading it.
    ///
    /// Runs the `rv-vcl` lexer and parser to verify that the source is
    /// syntactically correct. Returns `Ok(())` on success or an error message
    /// describing the first problem found.
    pub fn validate_vcl(vcl_source: &str) -> Result<(), String> {
        if vcl_source.trim().is_empty() {
            return Err("VCL source is empty".to_string());
        }

        let tokens = Lexer::tokenize(vcl_source).map_err(|e| format!("VCL lexer error: {e}"))?;

        Parser::parse(&tokens).map_err(|e| format!("VCL parse error: {e}"))?;

        Ok(())
    }

    /// Apply a new VCL configuration.
    ///
    /// The full sequence is: validate -> load -> activate. If activation fails,
    /// the bridge attempts to roll back to the previously active program.
    pub async fn apply_vcl(&mut self, name: &str, vcl_source: &str) -> Result<()> {
        // Always validate locally first for fast failure.
        Self::validate_vcl(vcl_source)
            .map_err(|e| anyhow::anyhow!("VCL validation failed: {e}"))?;

        match &self.mode {
            BridgeMode::Direct { vcl_manager } => {
                self.apply_vcl_direct(vcl_manager.clone(), name, vcl_source)
                    .await
            }
            BridgeMode::AdminCli { addr, secret } => {
                let addr = *addr;
                let secret = secret.clone();
                self.apply_vcl_admin_cli(addr, secret.as_deref(), name, vcl_source)
                    .await
            }
        }
    }

    /// Return the name of the currently active VCL program, if any.
    pub fn active_program(&self) -> Option<String> {
        match &self.mode {
            BridgeMode::Direct { vcl_manager } => vcl_manager.active_name(),
            BridgeMode::AdminCli { .. } => self.previous_active.clone(),
        }
    }

    /// Apply VCL using the in-process VCL manager.
    async fn apply_vcl_direct(
        &mut self,
        vcl_manager: Arc<VclManager>,
        name: &str,
        vcl_source: &str,
    ) -> Result<()> {
        // Remember the current active program for rollback.
        let previous = vcl_manager.active_name();

        // Load the new program.
        vcl_manager
            .load(name, vcl_source)
            .map_err(|e| anyhow::anyhow!("failed to load VCL program '{}': {}", name, e))?;

        tracing::info!(program = name, "VCL program loaded");

        // Activate the new program.
        match vcl_manager.use_program(name) {
            Ok(_interpreter) => {
                tracing::info!(program = name, "VCL program activated");
                self.previous_active = previous;
                Ok(())
            }
            Err(e) => {
                tracing::error!(program = name, error = %e, "failed to activate VCL program");

                // Attempt rollback: discard the failed program and re-activate
                // the previous one if there was one.
                let _ = vcl_manager.discard(name);

                if let Some(ref prev_name) = previous {
                    if let Err(rollback_err) = vcl_manager.use_program(prev_name) {
                        tracing::error!(
                            program = prev_name,
                            error = %rollback_err,
                            "rollback also failed"
                        );
                    } else {
                        tracing::info!(program = prev_name, "rolled back to previous VCL program");
                    }
                }

                bail!("failed to activate VCL program '{}': {}", name, e);
            }
        }
    }

    /// Apply VCL via the admin CLI TCP socket.
    async fn apply_vcl_admin_cli(
        &mut self,
        addr: SocketAddr,
        secret: Option<&str>,
        name: &str,
        vcl_source: &str,
    ) -> Result<()> {
        let mut client = AdminClient::connect(addr, secret)
            .await
            .context("failed to connect to admin CLI")?;

        // Load the VCL program.
        let load_cmd = format!(
            "vcl.load {} \"{}\"",
            name,
            vcl_source.replace('\\', "\\\\").replace('"', "\\\""),
        );
        let load_resp = client
            .send_command(&load_cmd)
            .await
            .context("failed to send vcl.load command")?;

        if load_resp.status != CliStatus::Ok {
            bail!("vcl.load failed: {}", load_resp.body);
        }

        tracing::info!(program = name, "VCL program loaded via admin CLI");

        // Activate the VCL program.
        let use_cmd = format!("vcl.use {}", name);
        let use_resp = client
            .send_command(&use_cmd)
            .await
            .context("failed to send vcl.use command")?;

        if use_resp.status != CliStatus::Ok {
            tracing::error!(program = name, error = %use_resp.body, "vcl.use failed via admin CLI");

            // Attempt to discard the failed program.
            let discard_cmd = format!("vcl.discard {}", name);
            let _ = client.send_command(&discard_cmd).await;

            // Attempt rollback to previous program.
            if let Some(ref prev_name) = self.previous_active {
                let rollback_cmd = format!("vcl.use {}", prev_name);
                match client.send_command(&rollback_cmd).await {
                    Ok(resp) if resp.status == CliStatus::Ok => {
                        tracing::info!(program = prev_name, "rolled back to previous VCL program");
                    }
                    _ => {
                        tracing::error!(program = prev_name, "rollback also failed");
                    }
                }
            }

            bail!("vcl.use failed: {}", use_resp.body);
        }

        self.previous_active = Some(name.to_string());
        tracing::info!(program = name, "VCL program activated via admin CLI");
        Ok(())
    }
}

impl Default for VclBridge {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Admin CLI TCP client
// ---------------------------------------------------------------------------

/// A minimal TCP client for the rv-admin CLI wire protocol.
///
/// Connects to the admin server, optionally authenticates, and provides
/// a method to send commands and receive responses. The wire format is
/// the same length-prefixed protocol defined in `rv_admin::protocol`.
struct AdminClient {
    reader: BufReader<tokio::net::tcp::OwnedReadHalf>,
    writer: tokio::net::tcp::OwnedWriteHalf,
}

impl AdminClient {
    /// Connect to the admin server and optionally authenticate.
    async fn connect(addr: SocketAddr, secret: Option<&str>) -> Result<Self> {
        let stream = TcpStream::connect(addr)
            .await
            .context("failed to connect to admin TCP socket")?;

        let (read_half, write_half) = stream.into_split();
        let mut client = Self {
            reader: BufReader::new(read_half),
            writer: write_half,
        };

        // Read the initial banner or auth challenge.
        let banner = client
            .read_response()
            .await
            .context("failed to read admin server banner")?;

        if let Some(secret) = secret {
            // If the server sent a challenge (status 107), authenticate.
            if banner.starts_with("107 ") {
                client
                    .authenticate(secret, &banner)
                    .await
                    .context("admin CLI authentication failed")?;
            }
        }

        Ok(client)
    }

    /// Perform challenge-response authentication.
    async fn authenticate(&mut self, secret: &str, challenge_header: &str) -> Result<()> {
        // Parse the challenge from the header line.
        // Format: "107 <length>\n<challenge_hex>\n"
        // We already have the full response data, need to extract the challenge hex.
        let parts: Vec<&str> = challenge_header.splitn(2, '\n').collect();
        let challenge_hex = if parts.len() > 1 {
            parts[1].trim()
        } else {
            // Read the challenge body from the stream.
            let mut line = String::new();
            self.reader.read_line(&mut line).await?;
            // This is a bit awkward since we need to own the string;
            // we'll handle it inline.
            return self.authenticate_with_hex(secret, line.trim()).await;
        };

        self.authenticate_with_hex(secret, challenge_hex).await
    }

    /// Compute and send the auth response for the given hex-encoded challenge.
    async fn authenticate_with_hex(&mut self, secret: &str, challenge_hex: &str) -> Result<()> {
        let challenge_bytes = hex_decode(challenge_hex);
        let response = rv_admin::compute_auth_response(&challenge_bytes, secret.as_bytes());

        self.writer
            .write_all(format!("{response}\n").as_bytes())
            .await?;

        // Read the auth result.
        let result = self.read_response().await?;
        if result.contains("200 ") {
            Ok(())
        } else {
            bail!("authentication failed: {}", result);
        }
    }

    /// Send a command and return the parsed response.
    async fn send_command(&mut self, command: &str) -> Result<CliResponse> {
        self.writer
            .write_all(format!("{command}\n").as_bytes())
            .await?;

        let raw = self.read_response().await?;
        let response = decode_response(raw.as_bytes())
            .map_err(|e| anyhow::anyhow!("failed to decode admin response: {e}"))?;

        Ok(response)
    }

    /// Read a full response from the admin server.
    ///
    /// Reads the header line to determine the body length, then reads
    /// exactly that many bytes plus the trailing newline.
    async fn read_response(&mut self) -> Result<String> {
        let mut header = String::new();
        let n = self.reader.read_line(&mut header).await?;
        if n == 0 {
            bail!("admin server closed connection");
        }

        // Parse body length from the header: "<status> <length>\n"
        let header_trimmed = header.trim();
        let parts: Vec<&str> = header_trimmed.splitn(2, ' ').collect();
        if parts.len() < 2 {
            // Not a standard response line, return as-is.
            return Ok(header);
        }

        let body_len: usize = parts[1].parse().unwrap_or(0);
        if body_len == 0 {
            // Read the trailing newline.
            let mut trailing = String::new();
            let _ = self.reader.read_line(&mut trailing).await;
            return Ok(header);
        }

        // Read the body bytes.
        let mut body_buf = vec![0u8; body_len];
        tokio::io::AsyncReadExt::read_exact(&mut self.reader, &mut body_buf).await?;

        // Read the trailing newline.
        let mut trailing = String::new();
        let _ = self.reader.read_line(&mut trailing).await;

        let mut full = header;
        full.push_str(&String::from_utf8_lossy(&body_buf));
        full.push('\n');

        Ok(full)
    }
}

/// Decode a hex string into bytes.
fn hex_decode(hex: &str) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(hex.len() / 2);
    let mut chars = hex.chars();
    while let Some(hi) = chars.next() {
        if let Some(lo) = chars.next() {
            let byte = u8::from_str_radix(&format!("{hi}{lo}"), 16).unwrap_or(0);
            bytes.push(byte);
        }
    }
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;

    const VALID_VCL: &str = r#"
vcl 4.0;
backend default {
    .host = "127.0.0.1";
    .port = "8080";
}
"#;

    const INVALID_VCL: &str = "this is not valid VCL at all {{{{";

    #[test]
    fn validate_valid_vcl_succeeds() {
        assert!(VclBridge::validate_vcl(VALID_VCL).is_ok());
    }

    #[test]
    fn validate_invalid_vcl_returns_error() {
        let result = VclBridge::validate_vcl(INVALID_VCL);
        assert!(result.is_err());
    }

    #[test]
    fn validate_empty_vcl_returns_error() {
        let result = VclBridge::validate_vcl("");
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn direct_mode_apply_and_activate() {
        let mut bridge = VclBridge::new();
        bridge.apply_vcl("test-v1", VALID_VCL).await.unwrap();
        assert_eq!(bridge.active_program(), Some("test-v1".to_string()));
    }

    #[tokio::test]
    async fn direct_mode_invalid_vcl_rejected() {
        let mut bridge = VclBridge::new();
        let result = bridge.apply_vcl("bad", INVALID_VCL).await;
        assert!(result.is_err());
        assert!(bridge.active_program().is_none());
    }

    #[tokio::test]
    async fn direct_mode_successive_activations() {
        let mut bridge = VclBridge::new();

        bridge.apply_vcl("v1", VALID_VCL).await.unwrap();
        assert_eq!(bridge.active_program(), Some("v1".to_string()));

        let vcl_v2 = r#"
vcl 4.0;
backend api {
    .host = "10.0.0.1";
    .port = "9090";
}
"#;
        bridge.apply_vcl("v2", vcl_v2).await.unwrap();
        assert_eq!(bridge.active_program(), Some("v2".to_string()));
    }

    #[test]
    fn hex_decode_roundtrip() {
        let input = vec![0xde, 0xad, 0xbe, 0xef];
        let hex = "deadbeef";
        assert_eq!(hex_decode(hex), input);
    }

    #[test]
    fn hex_decode_empty() {
        assert!(hex_decode("").is_empty());
    }

    #[test]
    fn default_bridge_has_no_active_program() {
        let bridge = VclBridge::default();
        assert!(bridge.active_program().is_none());
    }
}

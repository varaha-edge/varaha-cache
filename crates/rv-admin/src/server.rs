//! Async TCP server for the admin CLI.
//!
//! The [`AdminServer`] listens on a configured TCP address and spawns a
//! Tokio task for each incoming connection. Each connection task reads
//! command lines using a buffered reader, parses them, executes them via
//! the handler, and writes back encoded responses. The server runs until
//! the listening future is cancelled.
//!
//! When a shared secret is configured, the server performs a
//! challenge-response authentication handshake before accepting any
//! commands. See the [`auth`](crate::auth) module for protocol details.

use std::net::SocketAddr;
use std::sync::Arc;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;
use tracing::{error, info, warn};

use crate::auth;
use crate::commands::parse_command;
use crate::handler::{handle_command, AdminContext};
use crate::protocol::{encode_response, CliResponse, CliStatus};

/// An asynchronous TCP server that accepts admin CLI connections.
///
/// Each connection is handled in its own spawned Tokio task. Commands are
/// read line-by-line, parsed, executed, and the encoded response is written
/// back to the client.
///
/// When `secret` is `Some`, each new connection must complete a
/// challenge-response authentication handshake before any commands are
/// accepted. When `secret` is `None`, authentication is skipped.
pub struct AdminServer {
    addr: SocketAddr,
    context: Arc<AdminContext>,
    secret: Option<String>,
}

impl AdminServer {
    /// Create a new admin server that will listen on `addr` and use
    /// the given `context` for command execution.
    ///
    /// If `secret` is `Some`, every incoming connection must authenticate
    /// using the challenge-response scheme before commands are accepted.
    pub fn new(addr: SocketAddr, context: Arc<AdminContext>, secret: Option<String>) -> Self {
        Self {
            addr,
            context,
            secret,
        }
    }

    /// Start accepting connections and processing commands.
    ///
    /// This function runs indefinitely until the Tokio runtime shuts down
    /// or the future is dropped. Each accepted connection is handled in a
    /// separate spawned task so that slow clients do not block others.
    pub async fn serve(&self) -> Result<(), crate::error::AdminError> {
        let listener = TcpListener::bind(self.addr).await?;
        info!("Admin CLI server listening on {}", self.addr);

        if self.secret.is_some() {
            info!("Admin CLI authentication is enabled");
        } else {
            warn!("Admin CLI authentication is disabled (no secret configured)");
        }

        loop {
            match listener.accept().await {
                Ok((stream, peer)) => {
                    info!("Admin connection from {peer}");
                    let ctx = Arc::clone(&self.context);
                    let secret = self.secret.clone();
                    tokio::spawn(async move {
                        if let Err(e) = handle_connection(stream, ctx, secret).await {
                            warn!("Admin connection from {peer} ended with error: {e}");
                        } else {
                            info!("Admin connection from {peer} closed");
                        }
                    });
                }
                Err(e) => {
                    error!("Failed to accept admin connection: {e}");
                }
            }
        }
    }
}

/// Encode a challenge as a hex string for transmission to the client.
///
/// The challenge bytes are encoded as lowercase hex (64 characters for 32 bytes).
fn hex_encode_challenge(challenge: &[u8; 32]) -> String {
    let mut s = String::with_capacity(64);
    for &b in challenge {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

/// Perform the authentication handshake on a new connection.
///
/// Sends a challenge line to the client and reads back the expected
/// SHA-256 response. Returns `Ok(())` if authentication succeeds,
/// or an error if it fails.
async fn authenticate(
    buf_reader: &mut BufReader<tokio::net::tcp::OwnedReadHalf>,
    writer: &mut tokio::net::tcp::OwnedWriteHalf,
    secret: &str,
) -> Result<(), crate::error::AdminError> {
    let challenge = auth::generate_challenge();
    let challenge_hex = hex_encode_challenge(&challenge);

    // Send the challenge as an "auth challenge" response line.
    // The status code 107 is used by Varnish to indicate that authentication
    // is required. We use a similar approach: the body is the hex challenge.
    let challenge_msg = format!("107 {}\n{}\n", challenge_hex.len(), challenge_hex);
    writer.write_all(challenge_msg.as_bytes()).await?;

    // Read the client's response line.
    let mut response_line = String::new();
    let bytes_read = buf_reader.read_line(&mut response_line).await?;

    if bytes_read == 0 {
        return Err(crate::error::AdminError::AuthError(
            "client disconnected during authentication".to_string(),
        ));
    }

    let response = response_line.trim();

    if auth::verify_auth(&challenge, secret.as_bytes(), response) {
        // Send success banner.
        let banner = encode_response(&CliResponse::ok("Authentication successful."));
        writer.write_all(&banner).await?;
        info!("Admin client authenticated successfully");
        Ok(())
    } else {
        // Send auth failure and close.
        let fail =
            encode_response(&CliResponse::new(CliStatus::Close, "Authentication failed."));
        writer.write_all(&fail).await?;
        warn!("Admin client authentication failed");
        Err(crate::error::AdminError::AuthError(
            "authentication failed".to_string(),
        ))
    }
}

/// Handle a single admin TCP connection.
///
/// If a secret is configured, performs the authentication handshake first.
/// Then reads lines from the client, parses each as a command, executes it,
/// and writes the encoded response back. The connection is closed when
/// the client disconnects or sends a command that results in `CliStatus::Close`.
async fn handle_connection(
    stream: tokio::net::TcpStream,
    ctx: Arc<AdminContext>,
    secret: Option<String>,
) -> Result<(), crate::error::AdminError> {
    let (reader, mut writer) = stream.into_split();
    let mut buf_reader = BufReader::new(reader);

    // If a secret is configured, require authentication before accepting commands.
    if let Some(secret) = secret {
        authenticate(&mut buf_reader, &mut writer, &secret).await?;
    } else {
        // No authentication required; send the welcome banner directly.
        let banner = encode_response(&CliResponse::ok("varaha-cache admin CLI ready."));
        writer.write_all(&banner).await?;
    }

    let mut line = String::new();

    loop {
        line.clear();
        let bytes_read = buf_reader.read_line(&mut line).await?;
        if bytes_read == 0 {
            // Client disconnected.
            break;
        }

        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        // Handle the special "quit" / "close" commands directly.
        if trimmed == "quit" || trimmed == "close" {
            let resp =
                encode_response(&CliResponse::new(CliStatus::Close, "Closing connection."));
            writer.write_all(&resp).await?;
            break;
        }

        let response = match parse_command(trimmed) {
            Ok(cmd) => handle_command(&ctx, cmd),
            Err(e) => CliResponse::new(CliStatus::UnknownCommand, e.to_string()),
        };

        let encoded = encode_response(&response);
        writer.write_all(&encoded).await?;

        if response.status == CliStatus::Close {
            break;
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth;

    #[test]
    fn hex_encode_challenge_length() {
        let challenge = [0xabu8; 32];
        let hex = hex_encode_challenge(&challenge);
        assert_eq!(hex.len(), 64);
        assert!(hex.chars().all(|c| c == 'a' || c == 'b'));
    }

    #[test]
    fn hex_encode_challenge_known_value() {
        let mut challenge = [0u8; 32];
        challenge[0] = 0xde;
        challenge[1] = 0xad;
        challenge[31] = 0xff;
        let hex = hex_encode_challenge(&challenge);
        assert!(hex.starts_with("dead"));
        assert!(hex.ends_with("ff"));
    }

    #[tokio::test]
    async fn auth_handshake_success() {
        // Set up a TCP pair using a local listener.
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let secret = "test_secret_42".to_string();

        let client_secret = secret.clone();
        let client_handle = tokio::spawn(async move {
            let stream = tokio::net::TcpStream::connect(addr).await.unwrap();
            let (reader, mut writer) = stream.into_split();
            let mut buf_reader = BufReader::new(reader);

            // Read the challenge line (status + length).
            let mut header_line = String::new();
            buf_reader.read_line(&mut header_line).await.unwrap();

            // Read the challenge body.
            let mut challenge_line = String::new();
            buf_reader.read_line(&mut challenge_line).await.unwrap();
            let challenge_hex = challenge_line.trim();

            // Decode the hex challenge back to bytes for computing response.
            let challenge_bytes = hex_decode(challenge_hex);

            // Compute and send the auth response.
            let response = auth::compute_auth_response(&challenge_bytes, client_secret.as_bytes());
            writer
                .write_all(format!("{response}\n").as_bytes())
                .await
                .unwrap();

            // Read the success banner.
            let mut banner = String::new();
            buf_reader.read_line(&mut banner).await.unwrap();

            // The banner should contain the 200 status.
            assert!(banner.contains("200"), "Expected success banner, got: {banner}");
            true
        });

        let (stream, _peer) = listener.accept().await.unwrap();
        let (reader, mut writer) = stream.into_split();
        let mut buf_reader = BufReader::new(reader);

        let result = authenticate(&mut buf_reader, &mut writer, &secret).await;
        assert!(result.is_ok(), "Auth should succeed: {result:?}");

        let client_ok = client_handle.await.unwrap();
        assert!(client_ok);
    }

    #[tokio::test]
    async fn auth_handshake_failure() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let secret = "correct_secret".to_string();

        let client_handle = tokio::spawn(async move {
            let stream = tokio::net::TcpStream::connect(addr).await.unwrap();
            let (reader, mut writer) = stream.into_split();
            let mut buf_reader = BufReader::new(reader);

            // Read the challenge header and body.
            let mut header_line = String::new();
            buf_reader.read_line(&mut header_line).await.unwrap();
            let mut _challenge_line = String::new();
            buf_reader.read_line(&mut _challenge_line).await.unwrap();

            // Send a wrong response.
            writer
                .write_all(b"0000000000000000000000000000000000000000000000000000000000000000\n")
                .await
                .unwrap();

            // Read the failure response.
            let mut fail_resp = String::new();
            buf_reader.read_line(&mut fail_resp).await.unwrap();
            assert!(
                fail_resp.contains("999"),
                "Expected close status, got: {fail_resp}"
            );
        });

        let (stream, _peer) = listener.accept().await.unwrap();
        let (reader, mut writer) = stream.into_split();
        let mut buf_reader = BufReader::new(reader);

        let result = authenticate(&mut buf_reader, &mut writer, &secret).await;
        assert!(result.is_err(), "Auth should fail with wrong secret");

        client_handle.await.unwrap();
    }

    /// Decode a hex string into bytes (test helper).
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
}

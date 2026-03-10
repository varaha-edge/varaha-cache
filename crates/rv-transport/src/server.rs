use std::future::Future;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;

use tokio::io::{AsyncWriteExt, BufReader};
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info};

use rv_http::message::HttpMessage;

use crate::error::TransportError;
use crate::h2_server;
use crate::http1_server;
use crate::traits::{ConnectionInfo, DetectedVersion};

/// The HTTP/2 connection preface that clients send at the start of every
/// HTTP/2 connection. It is exactly 24 bytes:
/// "PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n"
const H2_PREFACE: &[u8; 24] = b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n";

/// Configuration for the transport server.
#[derive(Debug, Clone)]
pub struct TransportConfig {
    /// Address to listen on.
    pub listen_addr: SocketAddr,
    /// Maximum request body size in bytes.
    pub max_body_size: usize,
    /// Whether to accept PROXY protocol.
    pub accept_proxy_protocol: bool,
}

impl Default for TransportConfig {
    fn default() -> Self {
        Self {
            listen_addr: "127.0.0.1:6081".parse().unwrap(),
            max_body_size: 64 * 1024 * 1024,
            accept_proxy_protocol: false,
        }
    }
}

/// Callback for handling incoming requests.
/// Returns (response, optional response body).
pub type RequestHandler = Arc<
    dyn Fn(
            HttpMessage,
            Option<Vec<u8>>,
            ConnectionInfo,
        ) -> Pin<Box<dyn Future<Output = (HttpMessage, Option<Vec<u8>>)> + Send>>
        + Send
        + Sync,
>;

/// The transport server that accepts incoming HTTP connections.
pub struct TransportServer {
    config: TransportConfig,
}

impl TransportServer {
    pub fn new(config: TransportConfig) -> Self {
        Self { config }
    }

    /// Start listening and serving requests.
    ///
    /// The server will accept connections until the provided `cancel` token
    /// is cancelled, at which point it stops accepting new connections and
    /// returns `Ok(())`.
    pub async fn serve(
        &self,
        handler: RequestHandler,
        cancel: CancellationToken,
    ) -> Result<(), TransportError> {
        let listener = TcpListener::bind(self.config.listen_addr)
            .await
            .map_err(TransportError::Io)?;

        info!(addr = %self.config.listen_addr, "transport server listening");

        loop {
            tokio::select! {
                _ = cancel.cancelled() => {
                    info!("transport server shutting down, no longer accepting connections");
                    break;
                }
                result = listener.accept() => {
                    let (stream, peer_addr) = result.map_err(TransportError::Io)?;

                    // Disable Nagle's algorithm for lower latency on small responses.
                    let _ = stream.set_nodelay(true);

                    let local_addr = stream
                        .local_addr()
                        .unwrap_or(self.config.listen_addr);

                    let handler = Arc::clone(&handler);
                    let max_body_size = self.config.max_body_size;

                    tokio::spawn(async move {
                        let conn_info = ConnectionInfo {
                            client_addr: peer_addr,
                            local_addr,
                            real_client_addr: None,
                            is_tls: false,
                            http_version: DetectedVersion::Http11,
                        };

                        if let Err(e) =
                            handle_connection(stream, conn_info, handler, max_body_size).await
                        {
                            match &e {
                                TransportError::ConnectionClosed => {}
                                _ => error!(error = %e, peer = %peer_addr, "connection error"),
                            }
                        }
                    });
                }
            }
        }

        Ok(())
    }
}

/// Handle a connection with automatic HTTP/1.1 vs HTTP/2 protocol detection.
///
/// Peeks at the first 24 bytes of the connection to check for the HTTP/2
/// connection preface. If detected, the connection is handed off to the
/// h2 server handler. Otherwise it proceeds as HTTP/1.1.
async fn handle_connection(
    stream: tokio::net::TcpStream,
    conn_info: ConnectionInfo,
    handler: RequestHandler,
    max_body_size: usize,
) -> Result<(), TransportError> {
    // Peek at the first 24 bytes to detect HTTP/2 connection preface.
    let mut peek_buf = [0u8; 24];
    let peeked = stream
        .peek(&mut peek_buf)
        .await
        .map_err(TransportError::Io)?;

    if peeked >= 24 && peek_buf == *H2_PREFACE {
        debug!(peer = %conn_info.client_addr, "detected HTTP/2 connection preface");

        let mut h2_conn_info = conn_info;
        h2_conn_info.http_version = DetectedVersion::Http2;

        return h2_server::handle_h2_connection(stream, h2_conn_info, handler, max_body_size).await;
    }

    // Fall through to HTTP/1.1 handling.
    let mut buf_stream = BufReader::new(stream);

    loop {
        // Read request
        let request = match http1_server::read_request(&mut buf_stream).await {
            Ok(req) => req,
            Err(TransportError::ConnectionClosed) => return Ok(()),
            Err(e) => return Err(e),
        };

        let keepalive = http1_server::should_keepalive(&request);

        // Handle Expect: 100-continue -- send interim response before reading body
        let mut request = request;
        if request
            .get_header("Expect")
            .is_some_and(|v| v.eq_ignore_ascii_case("100-continue"))
        {
            let stream = buf_stream.get_mut();
            stream
                .write_all(b"HTTP/1.1 100 Continue\r\n\r\n")
                .await
                .map_err(TransportError::Io)?;
            stream.flush().await.map_err(TransportError::Io)?;
            request.unset_header("Expect");
        }

        // Read body if present
        let body = http1_server::read_body(&mut buf_stream, &request, max_body_size).await?;

        // Call the handler
        let (response, resp_body) = handler(request, body, conn_info.clone()).await;

        // Build response headers into a pre-sized buffer. The body is written
        // separately to avoid copying it into the header buffer.
        let mut hdr_buf = Vec::with_capacity(512);

        hdr_buf.extend_from_slice(
            format!(
                "{} {} {}\r\n",
                response.protocol,
                response.status.code(),
                response.reason
            )
            .as_bytes(),
        );

        for h in response.headers.iter() {
            hdr_buf.extend_from_slice(format!("{}: {}\r\n", h.name, h.value).as_bytes());
        }

        if let Some(ref body) = resp_body {
            if response.get_header("Content-Length").is_none() {
                hdr_buf
                    .extend_from_slice(format!("Content-Length: {}\r\n", body.len()).as_bytes());
            }
        }

        if !keepalive {
            hdr_buf.extend_from_slice(b"Connection: close\r\n");
        }

        hdr_buf.extend_from_slice(b"\r\n");

        // Write headers and body as separate writes to avoid copying the body.
        let stream = buf_stream.get_mut();
        stream
            .write_all(&hdr_buf)
            .await
            .map_err(TransportError::Io)?;

        if let Some(ref body) = resp_body {
            stream.write_all(body).await.map_err(TransportError::Io)?;
        }

        stream.flush().await.map_err(TransportError::Io)?;

        if !keepalive {
            return Ok(());
        }
    }
}

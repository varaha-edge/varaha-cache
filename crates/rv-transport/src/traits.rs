use std::net::SocketAddr;

use rv_http::message::HttpMessage;

use crate::error::TransportError;

/// Information about an accepted connection.
#[derive(Debug, Clone)]
pub struct ConnectionInfo {
    /// The remote (client) address.
    pub client_addr: SocketAddr,
    /// The local (server) address.
    pub local_addr: SocketAddr,
    /// The real client address (may differ if PROXY protocol was used).
    pub real_client_addr: Option<SocketAddr>,
    /// Whether the connection uses TLS.
    pub is_tls: bool,
    /// Detected HTTP version.
    pub http_version: DetectedVersion,
}

/// Detected HTTP version on a new connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DetectedVersion {
    Http11,
    Http2,
    Unknown,
}

/// An incoming request from a client.
pub struct IncomingRequest {
    /// The parsed HTTP request.
    pub request: HttpMessage,
    /// Connection metadata.
    pub conn_info: ConnectionInfo,
    /// A handle to send the response back.
    pub responder: Box<dyn Responder>,
}

/// Trait for sending a response back to the client.
pub trait Responder: Send {
    /// Send an HTTP response.
    fn send_response(&mut self, response: HttpMessage, body: Option<Vec<u8>>) -> Result<(), TransportError>;
}

/// Trait for sending an HTTP request to a backend.
pub trait ClientTransport: Send + Sync {
    /// Send a request to a backend and receive the response.
    fn send_request(
        &self,
        addr: SocketAddr,
        request: HttpMessage,
        body: Option<Vec<u8>>,
    ) -> Result<(HttpMessage, Option<Vec<u8>>), TransportError>;
}

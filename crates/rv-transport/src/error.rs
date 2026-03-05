use thiserror::Error;

/// Errors that can occur in the transport layer.
#[derive(Debug, Error)]
pub enum TransportError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("HTTP parse error: {0}")]
    HttpParse(String),

    #[error("connection closed")]
    ConnectionClosed,

    #[error("request too large")]
    RequestTooLarge,

    #[error("invalid HTTP version")]
    InvalidVersion,

    #[error("timeout")]
    Timeout,

    #[error("PROXY protocol error: {0}")]
    ProxyProtocol(String),

    #[error("HTTP/2 error: {0}")]
    Http2(String),

    #[error("TLS error: {0}")]
    Tls(String),
}

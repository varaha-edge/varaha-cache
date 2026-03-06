use rv_types::{BodyStatus, HttpMethod, HttpStatus};

use crate::header::HeaderMap;

/// HTTP protocol version.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HttpVersion {
    Http10,
    Http11,
    Http2,
}

impl std::str::FromStr for HttpVersion {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "HTTP/1.0" => Ok(Self::Http10),
            "HTTP/1.1" => Ok(Self::Http11),
            "HTTP/2" | "HTTP/2.0" => Ok(Self::Http2),
            _ => Err(()),
        }
    }
}

impl HttpVersion {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Http10 => "HTTP/1.0",
            Self::Http11 => "HTTP/1.1",
            Self::Http2 => "HTTP/2",
        }
    }

    pub fn minor(&self) -> u8 {
        match self {
            Self::Http10 => 0,
            Self::Http11 => 1,
            Self::Http2 => 0,
        }
    }

    pub fn supports_keepalive(&self) -> bool {
        !matches!(self, Self::Http10)
    }
}

impl std::fmt::Display for HttpVersion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// An HTTP message (request or response).
/// Mirrors struct http from cache.h
#[derive(Debug, Clone)]
pub struct HttpMessage {
    /// HTTP headers
    pub headers: HeaderMap,
    /// HTTP status code (for responses)
    pub status: HttpStatus,
    /// Protocol version
    pub protocol: HttpVersion,
    /// HTTP method (for requests)
    pub method: HttpMethod,
    /// Request URL (for requests)
    pub url: String,
    /// Reason phrase (for responses)
    pub reason: String,
    /// Body status
    pub body_status: BodyStatus,
}

impl HttpMessage {
    /// Create a new request message.
    pub fn new_request(method: HttpMethod, url: impl Into<String>, version: HttpVersion) -> Self {
        Self {
            headers: HeaderMap::new(),
            status: HttpStatus::OK,
            protocol: version,
            method,
            url: url.into(),
            reason: String::new(),
            body_status: BodyStatus::None,
        }
    }

    /// Create a new response message.
    pub fn new_response(status: HttpStatus, version: HttpVersion) -> Self {
        Self {
            headers: HeaderMap::new(),
            status,
            protocol: version,
            method: HttpMethod::Get,
            url: String::new(),
            reason: status.reason().to_string(),
            body_status: BodyStatus::None,
        }
    }

    /// Set a header value.
    pub fn set_header(&mut self, name: impl Into<String>, value: impl Into<String>) {
        self.headers.set(name, value);
    }

    /// Get a header value.
    pub fn get_header(&self, name: &str) -> Option<&str> {
        self.headers.get(name)
    }

    /// Unset (remove) a header.
    pub fn unset_header(&mut self, name: &str) {
        self.headers.unset(name);
    }

    /// Copy this message into a new HttpMessage with a deep clone.
    /// Equivalent to HTTP_Clone in cache_http.c
    pub fn clone_message(&self) -> Self {
        self.clone()
    }

    /// Reset the message to a clean state.
    pub fn reset(&mut self) {
        self.headers.clear();
        self.status = HttpStatus::OK;
        self.method = HttpMethod::Get;
        self.url.clear();
        self.reason.clear();
        self.body_status = BodyStatus::None;
    }

    /// Check if the response allows a body.
    /// Per RFC 2616, 1xx, 204, and 304 responses must not include a body.
    pub fn response_has_body(&self) -> bool {
        !self.status.is_informational()
            && self.status != HttpStatus::NO_CONTENT
            && self.status != HttpStatus::NOT_MODIFIED
    }

    /// Serialize the request line.
    pub fn request_line(&self) -> String {
        format!("{} {} {}", self.method, self.url, self.protocol)
    }

    /// Serialize the status line.
    pub fn status_line(&self) -> String {
        format!("{} {} {}", self.protocol, self.status.code(), self.reason)
    }
}

impl Default for HttpMessage {
    fn default() -> Self {
        Self::new_request(HttpMethod::Get, "/", HttpVersion::Http11)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new_request() {
        let msg = HttpMessage::new_request(HttpMethod::Get, "/index.html", HttpVersion::Http11);
        assert_eq!(msg.method, HttpMethod::Get);
        assert_eq!(msg.url, "/index.html");
        assert_eq!(msg.protocol, HttpVersion::Http11);
        assert_eq!(msg.request_line(), "GET /index.html HTTP/1.1");
    }

    #[test]
    fn test_new_response() {
        let msg = HttpMessage::new_response(HttpStatus::OK, HttpVersion::Http11);
        assert_eq!(msg.status, HttpStatus::OK);
        assert_eq!(msg.status_line(), "HTTP/1.1 200 OK");
    }

    #[test]
    fn test_response_has_body() {
        let msg_200 = HttpMessage::new_response(HttpStatus::OK, HttpVersion::Http11);
        assert!(msg_200.response_has_body());

        let msg_204 = HttpMessage::new_response(HttpStatus::NO_CONTENT, HttpVersion::Http11);
        assert!(!msg_204.response_has_body());

        let msg_304 = HttpMessage::new_response(HttpStatus::NOT_MODIFIED, HttpVersion::Http11);
        assert!(!msg_304.response_has_body());

        let msg_100 = HttpMessage::new_response(HttpStatus::CONTINUE, HttpVersion::Http11);
        assert!(!msg_100.response_has_body());
    }
}

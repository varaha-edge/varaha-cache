//! CLI wire protocol encoding and decoding.
//!
//! The wire format mirrors the Varnish CLI protocol:
//!
//! ```text
//! <status> <length>\n
//! <response_body>\n
//! ```
//!
//! Where `<status>` is a numeric status code, `<length>` is the byte length
//! of `<response_body>`, and both lines are terminated by `\n`.

use crate::error::AdminError;

/// CLI response status codes, modelled after the Varnish CLI status codes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u16)]
pub enum CliStatus {
    /// Command executed successfully.
    Ok = 200,
    /// Syntax error in the command.
    SyntaxError = 100,
    /// The command is not recognized.
    UnknownCommand = 101,
    /// The command is recognized but not yet implemented.
    Unimplemented = 102,
    /// Too few parameters were supplied.
    TooFewParams = 104,
    /// The response body was truncated.
    Truncated = 300,
    /// The requested resource is not available.
    NotAvailable = 400,
    /// A server-side error occurred while executing the command.
    Error = 500,
    /// The connection should be closed.
    Close = 999,
}

impl CliStatus {
    /// Return the numeric value of this status code.
    pub fn code(self) -> u16 {
        self as u16
    }

    /// Parse a numeric value into a `CliStatus`, returning `None` for
    /// unrecognized codes.
    pub fn from_code(code: u16) -> Option<Self> {
        match code {
            200 => Some(Self::Ok),
            100 => Some(Self::SyntaxError),
            101 => Some(Self::UnknownCommand),
            102 => Some(Self::Unimplemented),
            104 => Some(Self::TooFewParams),
            300 => Some(Self::Truncated),
            400 => Some(Self::NotAvailable),
            500 => Some(Self::Error),
            999 => Some(Self::Close),
            _ => None,
        }
    }
}

/// A structured CLI response consisting of a status code and a body string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CliResponse {
    pub status: CliStatus,
    pub body: String,
}

impl CliResponse {
    /// Create a new `CliResponse`.
    pub fn new(status: CliStatus, body: impl Into<String>) -> Self {
        Self {
            status,
            body: body.into(),
        }
    }

    /// Shorthand for a successful response.
    pub fn ok(body: impl Into<String>) -> Self {
        Self::new(CliStatus::Ok, body)
    }

    /// Shorthand for an error response.
    pub fn error(body: impl Into<String>) -> Self {
        Self::new(CliStatus::Error, body)
    }
}

/// Encode a `CliResponse` into its wire-format byte representation.
///
/// The format is:
/// ```text
/// <status> <body_length>\n
/// <body>\n
/// ```
pub fn encode_response(response: &CliResponse) -> Vec<u8> {
    let body_bytes = response.body.as_bytes();
    let header = format!("{} {}\n", response.status.code(), body_bytes.len());
    let mut buf = Vec::with_capacity(header.len() + body_bytes.len() + 1);
    buf.extend_from_slice(header.as_bytes());
    buf.extend_from_slice(body_bytes);
    buf.push(b'\n');
    buf
}

/// Decode a wire-format byte slice into a `CliResponse`.
///
/// Expects the format produced by [`encode_response`].
pub fn decode_response(data: &[u8]) -> Result<CliResponse, AdminError> {
    let text = std::str::from_utf8(data)
        .map_err(|e| AdminError::InvalidCommand(format!("invalid UTF-8: {e}")))?;

    // Find the header line (first line).
    let newline_pos = text
        .find('\n')
        .ok_or_else(|| AdminError::InvalidCommand("missing header newline".to_string()))?;

    let header = &text[..newline_pos];

    // Parse "status length" from the header.
    let mut parts = header.splitn(2, ' ');

    let status_str = parts
        .next()
        .ok_or_else(|| AdminError::InvalidCommand("missing status code".to_string()))?;

    let length_str = parts
        .next()
        .ok_or_else(|| AdminError::InvalidCommand("missing length".to_string()))?;

    let status_code: u16 = status_str
        .parse()
        .map_err(|e| AdminError::InvalidCommand(format!("invalid status code: {e}")))?;

    let body_len: usize = length_str
        .parse()
        .map_err(|e| AdminError::InvalidCommand(format!("invalid body length: {e}")))?;

    let status = CliStatus::from_code(status_code)
        .ok_or_else(|| AdminError::InvalidCommand(format!("unknown status code: {status_code}")))?;

    // The body starts right after the header newline.
    let body_start = newline_pos + 1;
    let body_end = body_start + body_len;

    if body_end > text.len() {
        return Err(AdminError::InvalidCommand(format!(
            "body length {body_len} exceeds available data ({})",
            text.len() - body_start
        )));
    }

    let body = text[body_start..body_end].to_string();

    Ok(CliResponse { status, body })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_ok_response() {
        let resp = CliResponse::ok("PONG 1234567890");
        let encoded = encode_response(&resp);
        let text = String::from_utf8(encoded).unwrap();
        assert!(text.starts_with("200 15\n"));
        assert!(text.contains("PONG 1234567890"));
    }

    #[test]
    fn encode_error_response() {
        let resp = CliResponse::error("something went wrong");
        let encoded = encode_response(&resp);
        let text = String::from_utf8(encoded).unwrap();
        assert!(text.starts_with("500 20\n"));
    }

    #[test]
    fn encode_empty_body() {
        let resp = CliResponse::ok("");
        let encoded = encode_response(&resp);
        let text = String::from_utf8(encoded).unwrap();
        assert_eq!(text, "200 0\n\n");
    }

    #[test]
    fn decode_ok_response() {
        let data = b"200 5\nhello\n";
        let resp = decode_response(data).unwrap();
        assert_eq!(resp.status, CliStatus::Ok);
        assert_eq!(resp.body, "hello");
    }

    #[test]
    fn decode_error_response() {
        let data = b"500 10\nbad things\n";
        let resp = decode_response(data).unwrap();
        assert_eq!(resp.status, CliStatus::Error);
        assert_eq!(resp.body, "bad things");
    }

    #[test]
    fn roundtrip_ok() {
        let original = CliResponse::ok("cache hit ratio: 95.3%");
        let encoded = encode_response(&original);
        let decoded = decode_response(&encoded).unwrap();
        assert_eq!(original, decoded);
    }

    #[test]
    fn roundtrip_error() {
        let original = CliResponse::error("ban expression parse failed");
        let encoded = encode_response(&original);
        let decoded = decode_response(&encoded).unwrap();
        assert_eq!(original, decoded);
    }

    #[test]
    fn roundtrip_unimplemented() {
        let original = CliResponse::new(CliStatus::Unimplemented, "vcl.load not yet available");
        let encoded = encode_response(&original);
        let decoded = decode_response(&encoded).unwrap();
        assert_eq!(original, decoded);
    }

    #[test]
    fn roundtrip_empty_body() {
        let original = CliResponse::ok("");
        let encoded = encode_response(&original);
        let decoded = decode_response(&encoded).unwrap();
        assert_eq!(original, decoded);
    }

    #[test]
    fn roundtrip_multiline_body() {
        let body = "line one\nline two\nline three";
        let original = CliResponse::ok(body);
        let encoded = encode_response(&original);
        let decoded = decode_response(&encoded).unwrap();
        assert_eq!(original, decoded);
    }

    #[test]
    fn decode_missing_newline() {
        let data = b"200 5";
        let result = decode_response(data);
        assert!(result.is_err());
    }

    #[test]
    fn decode_unknown_status() {
        let data = b"999 2\nok\n";
        let resp = decode_response(data).unwrap();
        assert_eq!(resp.status, CliStatus::Close);
    }

    #[test]
    fn decode_invalid_status_code() {
        let data = b"777 2\nok\n";
        let result = decode_response(data);
        assert!(result.is_err());
    }

    #[test]
    fn decode_truncated_body() {
        let data = b"200 100\nshort\n";
        let result = decode_response(data);
        assert!(result.is_err());
    }

    #[test]
    fn cli_status_code_values() {
        assert_eq!(CliStatus::Ok.code(), 200);
        assert_eq!(CliStatus::SyntaxError.code(), 100);
        assert_eq!(CliStatus::UnknownCommand.code(), 101);
        assert_eq!(CliStatus::Unimplemented.code(), 102);
        assert_eq!(CliStatus::TooFewParams.code(), 104);
        assert_eq!(CliStatus::Truncated.code(), 300);
        assert_eq!(CliStatus::NotAvailable.code(), 400);
        assert_eq!(CliStatus::Error.code(), 500);
        assert_eq!(CliStatus::Close.code(), 999);
    }

    #[test]
    fn cli_status_from_code_roundtrip() {
        for &status in &[
            CliStatus::Ok,
            CliStatus::SyntaxError,
            CliStatus::UnknownCommand,
            CliStatus::Unimplemented,
            CliStatus::TooFewParams,
            CliStatus::Truncated,
            CliStatus::NotAvailable,
            CliStatus::Error,
            CliStatus::Close,
        ] {
            assert_eq!(CliStatus::from_code(status.code()), Some(status));
        }
    }
}

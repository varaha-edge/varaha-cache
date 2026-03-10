use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;

use rv_http::message::{HttpMessage, HttpVersion};
use rv_types::{BodyStatus, HttpMethod};

use crate::error::TransportError;

/// Maximum number of headers to parse per request.
const MAX_HEADERS: usize = 64;

/// Initial capacity for the header accumulation buffer.
const INITIAL_BUF_SIZE: usize = 4096;

/// Maximum allowed header size (64 KiB) to prevent memory exhaustion.
const MAX_HEADER_SIZE: usize = 64 * 1024;

/// Parse an HTTP/1.1 request from a TCP stream using `httparse` for
/// zero-allocation header parsing.
///
/// Headers are read line-by-line via `read_until(b'\n')` into a single
/// pre-allocated buffer, then parsed in one pass by `httparse`. Body bytes
/// remain untouched in the `BufReader`'s internal buffer for subsequent
/// `read_body()` calls.
pub async fn read_request(
    stream: &mut BufReader<TcpStream>,
) -> Result<HttpMessage, TransportError> {
    // Accumulate raw header bytes until we see the blank line (\r\n\r\n).
    let mut buf = Vec::with_capacity(INITIAL_BUF_SIZE);

    loop {
        let before = buf.len();
        let byte_count = stream
            .read_until(b'\n', &mut buf)
            .await
            .map_err(TransportError::Io)?;

        if byte_count == 0 {
            if buf.is_empty() {
                return Err(TransportError::ConnectionClosed);
            }
            return Err(TransportError::HttpParse(
                "incomplete request headers".to_string(),
            ));
        }

        // Check for end-of-headers: \r\n\r\n or \n\n
        let len = buf.len();
        if len >= 4 && &buf[len - 4..] == b"\r\n\r\n" {
            break;
        }
        if len >= 2 && &buf[len - 2..] == b"\n\n" {
            break;
        }
        // Also detect a lone \r\n or \n right after the first line break
        // (handles the case where the line we just read is the blank line
        // but a previous line ended with bare \n).
        let segment = &buf[before..];
        if segment == b"\r\n" || segment == b"\n" {
            // This was the blank terminator line.
            break;
        }

        if buf.len() > MAX_HEADER_SIZE {
            return Err(TransportError::RequestTooLarge);
        }
    }

    // Parse the accumulated header bytes with httparse.
    let mut headers = [httparse::EMPTY_HEADER; MAX_HEADERS];
    let mut req = httparse::Request::new(&mut headers);

    match req.parse(&buf) {
        Ok(httparse::Status::Complete(_header_len)) => {
            let method = HttpMethod::parse_method(req.method.unwrap_or("GET"));
            let path = req.path.unwrap_or("/").to_string();
            let version = match req.version {
                Some(0) => HttpVersion::Http10,
                _ => HttpVersion::Http11,
            };

            let mut msg = HttpMessage::new_request(method, path, version);

            for header in req.headers.iter() {
                let value = std::str::from_utf8(header.value).unwrap_or("");
                msg.set_header(header.name, value);
            }

            // Determine body status from headers.
            if let Some(cl) = msg.get_header("Content-Length") {
                if cl.parse::<usize>().unwrap_or(0) > 0 {
                    msg.body_status = BodyStatus::Length;
                }
            } else if msg
                .get_header("Transfer-Encoding")
                .is_some_and(|te| te.eq_ignore_ascii_case("chunked"))
            {
                msg.body_status = BodyStatus::Chunked;
            }

            Ok(msg)
        }
        Ok(httparse::Status::Partial) => Err(TransportError::HttpParse(
            "incomplete HTTP request".to_string(),
        )),
        Err(e) => Err(TransportError::HttpParse(format!(
            "HTTP parse error: {e}"
        ))),
    }
}

/// Read the request body based on Content-Length or chunked encoding.
pub async fn read_body(
    stream: &mut BufReader<TcpStream>,
    msg: &HttpMessage,
    max_size: usize,
) -> Result<Option<Vec<u8>>, TransportError> {
    match msg.body_status {
        BodyStatus::Length => {
            let length: usize = msg
                .get_header("Content-Length")
                .and_then(|v| v.parse().ok())
                .unwrap_or(0);

            if length == 0 {
                return Ok(None);
            }
            if length > max_size {
                return Err(TransportError::RequestTooLarge);
            }

            let mut body = vec![0u8; length];
            stream
                .read_exact(&mut body)
                .await
                .map_err(TransportError::Io)?;
            Ok(Some(body))
        }
        BodyStatus::Chunked => {
            let mut body = Vec::new();
            loop {
                let mut size_line = String::new();
                stream
                    .read_line(&mut size_line)
                    .await
                    .map_err(TransportError::Io)?;

                let chunk_size = usize::from_str_radix(size_line.trim(), 16)
                    .map_err(|e| TransportError::HttpParse(format!("invalid chunk size: {e}")))?;

                if chunk_size == 0 {
                    // Read trailing CRLF
                    let mut trailer = String::new();
                    stream
                        .read_line(&mut trailer)
                        .await
                        .map_err(TransportError::Io)?;
                    break;
                }

                if body.len() + chunk_size > max_size {
                    return Err(TransportError::RequestTooLarge);
                }

                let mut chunk = vec![0u8; chunk_size];
                stream
                    .read_exact(&mut chunk)
                    .await
                    .map_err(TransportError::Io)?;
                body.extend_from_slice(&chunk);

                // Read chunk-ending CRLF
                let mut crlf = [0u8; 2];
                stream
                    .read_exact(&mut crlf)
                    .await
                    .map_err(TransportError::Io)?;
            }
            if body.is_empty() {
                Ok(None)
            } else {
                Ok(Some(body))
            }
        }
        _ => Ok(None),
    }
}

/// Write an HTTP/1.1 response to a TCP stream.
pub async fn write_response(
    stream: &mut TcpStream,
    response: &HttpMessage,
    body: Option<&[u8]>,
) -> Result<(), TransportError> {
    // Status line
    let status_line = format!(
        "{} {} {}\r\n",
        response.protocol,
        response.status.code(),
        response.reason
    );
    stream
        .write_all(status_line.as_bytes())
        .await
        .map_err(TransportError::Io)?;

    // Headers
    for h in response.headers.iter() {
        let header_line = format!("{}: {}\r\n", h.name, h.value);
        stream
            .write_all(header_line.as_bytes())
            .await
            .map_err(TransportError::Io)?;
    }

    // Add Content-Length if body is present and header not already set
    if let Some(body) = body {
        if response.get_header("Content-Length").is_none()
            && response.get_header("Transfer-Encoding").is_none()
        {
            let cl = format!("Content-Length: {}\r\n", body.len());
            stream
                .write_all(cl.as_bytes())
                .await
                .map_err(TransportError::Io)?;
        }
    }

    // End of headers
    stream
        .write_all(b"\r\n")
        .await
        .map_err(TransportError::Io)?;

    // Body
    if let Some(body) = body {
        stream.write_all(body).await.map_err(TransportError::Io)?;
    }

    stream.flush().await.map_err(TransportError::Io)?;

    Ok(())
}

/// Check if the connection should be kept alive.
pub fn should_keepalive(msg: &HttpMessage) -> bool {
    if msg.protocol == HttpVersion::Http10 {
        // HTTP/1.0: keepalive only if explicitly requested
        msg.get_header("Connection")
            .is_some_and(|v| v.eq_ignore_ascii_case("keep-alive"))
    } else {
        // HTTP/1.1: keepalive by default unless "Connection: close"
        !msg.get_header("Connection")
            .is_some_and(|v| v.eq_ignore_ascii_case("close"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_should_keepalive_http11() {
        let msg = HttpMessage::new_request(HttpMethod::Get, "/", HttpVersion::Http11);
        assert!(should_keepalive(&msg));
    }

    #[test]
    fn test_should_not_keepalive_http11_close() {
        let mut msg = HttpMessage::new_request(HttpMethod::Get, "/", HttpVersion::Http11);
        msg.set_header("Connection", "close");
        assert!(!should_keepalive(&msg));
    }

    #[test]
    fn test_should_not_keepalive_http10() {
        let msg = HttpMessage::new_request(HttpMethod::Get, "/", HttpVersion::Http10);
        assert!(!should_keepalive(&msg));
    }

    #[test]
    fn test_should_keepalive_http10_explicit() {
        let mut msg = HttpMessage::new_request(HttpMethod::Get, "/", HttpVersion::Http10);
        msg.set_header("Connection", "keep-alive");
        assert!(should_keepalive(&msg));
    }
}

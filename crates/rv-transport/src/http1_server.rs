use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;

use rv_http::message::{HttpMessage, HttpVersion};
use rv_types::{BodyStatus, HttpMethod};

use crate::error::TransportError;

/// Parse an HTTP/1.1 request from a TCP stream.
pub async fn read_request(stream: &mut BufReader<TcpStream>) -> Result<HttpMessage, TransportError> {
    // Read request line
    let mut request_line = String::new();
    let n = stream
        .read_line(&mut request_line)
        .await
        .map_err(TransportError::Io)?;
    if n == 0 {
        return Err(TransportError::ConnectionClosed);
    }

    let request_line = request_line.trim_end();
    let parts: Vec<&str> = request_line.splitn(3, ' ').collect();
    if parts.len() != 3 {
        return Err(TransportError::HttpParse(format!(
            "invalid request line: {request_line}"
        )));
    }

    let method = HttpMethod::from_str(parts[0]);
    let url = parts[1].to_string();
    let version = HttpVersion::from_str(parts[2]).ok_or_else(|| {
        TransportError::InvalidVersion
    })?;

    let mut msg = HttpMessage::new_request(method, url, version);

    // Read headers
    loop {
        let mut line = String::new();
        let n = stream
            .read_line(&mut line)
            .await
            .map_err(TransportError::Io)?;
        if n == 0 {
            return Err(TransportError::ConnectionClosed);
        }

        let line = line.trim_end_matches("\r\n").trim_end_matches('\n');
        if line.is_empty() {
            break;
        }

        if let Some((name, value)) = line.split_once(':') {
            msg.set_header(name.trim(), value.trim());
        }
    }

    // Determine body status from headers
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
        stream
            .write_all(body)
            .await
            .map_err(TransportError::Io)?;
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
        !msg
            .get_header("Connection")
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

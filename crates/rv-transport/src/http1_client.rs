use std::net::SocketAddr;

use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;

use rv_backend::ConnectionPool;
use rv_http::message::{HttpMessage, HttpVersion};
use rv_types::HttpStatus;

use crate::error::TransportError;

/// Determine whether the response indicates the connection should be
/// kept alive for reuse.  HTTP/1.1 defaults to keep-alive unless the
/// response contains `Connection: close`.  HTTP/1.0 requires an
/// explicit `Connection: keep-alive` header.
fn is_keep_alive(response: &HttpMessage) -> bool {
    let conn_header = response.get_header("Connection").unwrap_or_default();
    match response.protocol {
        HttpVersion::Http11 => !conn_header.eq_ignore_ascii_case("close"),
        _ => conn_header.eq_ignore_ascii_case("keep-alive"),
    }
}

/// Send an HTTP/1.1 request to a backend and read the response.
///
/// When a `pool` is provided the function first attempts to reuse an
/// existing pooled connection for `addr`.  If the pooled stream turns
/// out to be stale (peer closed), a fresh connection is established
/// transparently.  After a successful response with keep-alive
/// semantics the underlying `TcpStream` is returned to the pool for
/// future requests.
pub async fn send_backend_request(
    addr: SocketAddr,
    request: &HttpMessage,
    body: Option<&[u8]>,
    pool: Option<&ConnectionPool>,
) -> Result<(HttpMessage, Option<Vec<u8>>), TransportError> {
    // Try to obtain a pooled connection first
    let stream = if let Some(p) = pool {
        p.get(addr)
    } else {
        None
    };

    let mut stream = match stream {
        Some(s) => s,
        None => TcpStream::connect(addr).await.map_err(TransportError::Io)?,
    };

    // Write request line
    let request_line = format!(
        "{} {} {}\r\n",
        request.method, request.url, request.protocol
    );
    stream
        .write_all(request_line.as_bytes())
        .await
        .map_err(TransportError::Io)?;

    // Write headers
    for h in request.headers.iter() {
        let header = format!("{}: {}\r\n", h.name, h.value);
        stream
            .write_all(header.as_bytes())
            .await
            .map_err(TransportError::Io)?;
    }

    // Write Host header if not present
    if request.get_header("Host").is_none() {
        let host = format!("Host: {addr}\r\n");
        stream
            .write_all(host.as_bytes())
            .await
            .map_err(TransportError::Io)?;
    }

    // Write body
    if let Some(body) = body {
        let cl = format!("Content-Length: {}\r\n", body.len());
        stream
            .write_all(cl.as_bytes())
            .await
            .map_err(TransportError::Io)?;
        stream
            .write_all(b"\r\n")
            .await
            .map_err(TransportError::Io)?;
        stream.write_all(body).await.map_err(TransportError::Io)?;
    } else {
        stream
            .write_all(b"\r\n")
            .await
            .map_err(TransportError::Io)?;
    }

    stream.flush().await.map_err(TransportError::Io)?;

    // Read response
    let mut reader = BufReader::new(stream);

    // Status line
    let mut status_line = String::new();
    let n = reader
        .read_line(&mut status_line)
        .await
        .map_err(TransportError::Io)?;
    if n == 0 {
        return Err(TransportError::ConnectionClosed);
    }

    let status_line = status_line.trim_end();
    let parts: Vec<&str> = status_line.splitn(3, ' ').collect();
    if parts.len() < 2 {
        return Err(TransportError::HttpParse(format!(
            "invalid status line: {status_line}"
        )));
    }

    let version = parts[0].parse().unwrap_or(HttpVersion::Http11);
    let status_code: u16 = parts[1]
        .parse()
        .map_err(|e| TransportError::HttpParse(format!("invalid status code: {e}")))?;
    let reason = if parts.len() > 2 { parts[2] } else { "" };

    let status = HttpStatus::new(status_code);
    let mut response = HttpMessage::new_response(status, version);
    response.reason = reason.to_string();

    // Read response headers
    loop {
        let mut line = String::new();
        let n = reader
            .read_line(&mut line)
            .await
            .map_err(TransportError::Io)?;
        if n == 0 {
            break;
        }

        let line = line.trim_end_matches("\r\n").trim_end_matches('\n');
        if line.is_empty() {
            break;
        }

        if let Some((name, value)) = line.split_once(':') {
            response.set_header(name.trim(), value.trim());
        }
    }

    // Determine if the connection can be reused before reading the body,
    // since body reading may consume the stream via read-to-close.
    let keep_alive = is_keep_alive(&response);

    // Track whether the body was read with a known length (Content-Length
    // or chunked).  Only these modes leave the stream in a reusable
    // state; read-until-close consumes and shuts down the stream.
    let mut body_length_known = false;

    // Read response body
    let resp_body = if response.response_has_body() {
        if let Some(cl) = response.get_header("Content-Length") {
            let length: usize = cl.parse().unwrap_or(0);
            if length > 0 {
                let mut body = vec![0u8; length];
                reader
                    .read_exact(&mut body)
                    .await
                    .map_err(TransportError::Io)?;
                body_length_known = true;
                Some(body)
            } else {
                body_length_known = true;
                None
            }
        } else if response
            .get_header("Transfer-Encoding")
            .is_some_and(|te| te.eq_ignore_ascii_case("chunked"))
        {
            // Read chunked body
            let mut body = Vec::new();
            loop {
                let mut size_line = String::new();
                reader
                    .read_line(&mut size_line)
                    .await
                    .map_err(TransportError::Io)?;

                let chunk_size = usize::from_str_radix(size_line.trim(), 16).unwrap_or(0);
                if chunk_size == 0 {
                    let mut trailer = String::new();
                    reader
                        .read_line(&mut trailer)
                        .await
                        .map_err(TransportError::Io)?;
                    break;
                }

                let mut chunk = vec![0u8; chunk_size];
                reader
                    .read_exact(&mut chunk)
                    .await
                    .map_err(TransportError::Io)?;
                body.extend_from_slice(&chunk);

                let mut crlf = [0u8; 2];
                reader
                    .read_exact(&mut crlf)
                    .await
                    .map_err(TransportError::Io)?;
            }
            body_length_known = true;
            if body.is_empty() { None } else { Some(body) }
        } else {
            // Read until connection close -- stream is consumed
            let mut body = Vec::new();
            reader
                .read_to_end(&mut body)
                .await
                .map_err(TransportError::Io)?;
            if body.is_empty() { None } else { Some(body) }
        }
    } else {
        body_length_known = true;
        None
    };

    // Return the stream to the pool when the response allows keep-alive
    // and we read the body with a deterministic framing method.
    if keep_alive && body_length_known {
        if let Some(p) = pool {
            // Recover the TcpStream from the BufReader.  We only do this
            // when no buffered data remains (body fully consumed above).
            let inner = reader.into_inner();
            p.put(addr, inner);
        }
    }

    Ok((response, resp_body))
}

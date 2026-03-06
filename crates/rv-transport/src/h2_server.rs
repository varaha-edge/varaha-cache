use std::sync::Arc;

use bytes::Bytes;
use h2::RecvStream;
use h2::server;
use tracing::{debug, error};

use rv_http::message::{HttpMessage, HttpVersion};
use rv_types::{BodyStatus, HttpMethod};

use crate::error::TransportError;
use crate::server::RequestHandler;
use crate::traits::ConnectionInfo;

/// Handle an HTTP/2 connection.
///
/// Performs the h2 server handshake on the provided TCP stream, then accepts
/// and processes multiplexed streams. Each stream is handled concurrently
/// by spawning a task that reads the request, converts it to an HttpMessage,
/// invokes the handler, and sends the response back over HTTP/2.
pub async fn handle_h2_connection(
    stream: tokio::net::TcpStream,
    conn_info: ConnectionInfo,
    handler: RequestHandler,
    max_body_size: usize,
) -> Result<(), TransportError> {
    let mut connection = server::handshake(stream)
        .await
        .map_err(|e| TransportError::Http2(format!("h2 handshake failed: {e}")))?;

    debug!(
        peer = %conn_info.client_addr,
        "HTTP/2 connection established"
    );

    while let Some(result) = connection.accept().await {
        let (request, send_response) =
            result.map_err(|e| TransportError::Http2(format!("h2 accept error: {e}")))?;

        let handler = Arc::clone(&handler);
        let conn_info = conn_info.clone();

        tokio::spawn(async move {
            if let Err(e) =
                handle_h2_stream(request, send_response, handler, conn_info, max_body_size).await
            {
                error!(error = %e, "h2 stream error");
            }
        });
    }

    debug!(
        peer = %conn_info.client_addr,
        "HTTP/2 connection closed"
    );

    Ok(())
}

/// Handle a single HTTP/2 stream: read the request, call the handler,
/// and send the response.
async fn handle_h2_stream(
    request: http::Request<RecvStream>,
    mut send_response: h2::server::SendResponse<Bytes>,
    handler: RequestHandler,
    conn_info: ConnectionInfo,
    max_body_size: usize,
) -> Result<(), TransportError> {
    let (parts, mut recv_body) = request.into_parts();

    // Convert the h2 request into an HttpMessage.
    let msg = h2_request_to_message(&parts)?;

    // Read the request body if present.
    let body = read_h2_body(&mut recv_body, max_body_size).await?;

    // Invoke the application handler.
    let (response, resp_body) = handler(msg, body, conn_info).await;

    // Convert the HttpMessage response to an http::Response for h2.
    send_h2_response(&mut send_response, &response, resp_body.as_deref())?;

    Ok(())
}

/// Convert HTTP/2 request parts into an HttpMessage.
fn h2_request_to_message(parts: &http::request::Parts) -> Result<HttpMessage, TransportError> {
    let method = HttpMethod::parse_method(parts.method.as_str());

    let url = parts
        .uri
        .path_and_query()
        .map(|pq| pq.as_str().to_string())
        .unwrap_or_else(|| parts.uri.path().to_string());

    let mut msg = HttpMessage::new_request(method, url, HttpVersion::Http2);

    // Copy headers from the h2 request into the HttpMessage.
    for (name, value) in parts.headers.iter() {
        // Skip HTTP/2 pseudo-headers (they start with ':').
        if name.as_str().starts_with(':') {
            continue;
        }
        let val = value.to_str().unwrap_or("");
        msg.set_header(name.as_str(), val);
    }

    // Determine body status from headers.
    if let Some(cl) = msg.get_header("Content-Length") {
        if cl.parse::<usize>().unwrap_or(0) > 0 {
            msg.body_status = BodyStatus::Length;
        }
    }
    // HTTP/2 does not use Transfer-Encoding: chunked. Body presence is
    // determined by DATA frames, but for the HttpMessage model we leave
    // body_status as None if no Content-Length was set. The actual body
    // is read separately and its presence is indicated by Option<Vec<u8>>.

    Ok(msg)
}

/// Read the full body from an h2 RecvStream, respecting max_body_size.
async fn read_h2_body(
    recv: &mut RecvStream,
    max_body_size: usize,
) -> Result<Option<Vec<u8>>, TransportError> {
    let mut body = Vec::new();

    while let Some(chunk) = recv.data().await {
        let chunk = chunk.map_err(|e| TransportError::Http2(format!("h2 body read error: {e}")))?;

        if body.len() + chunk.len() > max_body_size {
            return Err(TransportError::RequestTooLarge);
        }

        body.extend_from_slice(&chunk);

        // Release flow control capacity back to the sender so it can
        // continue transmitting. Without this call, the sender would stall
        // once the initial window is exhausted.
        let _ = recv.flow_control().release_capacity(chunk.len());
    }

    if body.is_empty() {
        Ok(None)
    } else {
        Ok(Some(body))
    }
}

/// Build and send an HTTP/2 response from an HttpMessage.
fn send_h2_response(
    send_response: &mut h2::server::SendResponse<Bytes>,
    response: &HttpMessage,
    body: Option<&[u8]>,
) -> Result<(), TransportError> {
    let status = http::StatusCode::from_u16(response.status.code())
        .unwrap_or(http::StatusCode::INTERNAL_SERVER_ERROR);

    let mut builder = http::Response::builder().status(status);

    for h in response.headers.iter() {
        // Skip hop-by-hop headers that are not valid in HTTP/2.
        let lower = h.name.to_ascii_lowercase();
        if matches!(
            lower.as_str(),
            "connection" | "keep-alive" | "transfer-encoding" | "upgrade"
        ) {
            continue;
        }

        let header_name = http::header::HeaderName::from_bytes(h.name.as_bytes())
            .map_err(|e| TransportError::Http2(format!("invalid header name '{}': {e}", h.name)))?;
        let header_value = http::header::HeaderValue::from_str(&h.value).map_err(|e| {
            TransportError::Http2(format!("invalid header value '{}': {e}", h.value))
        })?;
        builder = builder.header(header_name, header_value);
    }

    let has_body = body.is_some_and(|b| !b.is_empty());

    // Send the response headers. The end_of_stream flag indicates whether
    // there are DATA frames to follow.
    let h2_response = builder
        .body(())
        .map_err(|e| TransportError::Http2(format!("failed to build h2 response: {e}")))?;

    let mut send_stream = send_response
        .send_response(h2_response, !has_body)
        .map_err(|e| TransportError::Http2(format!("h2 send_response error: {e}")))?;

    // Send the body if present.
    if let Some(data) = body {
        if !data.is_empty() {
            send_stream
                .send_data(Bytes::copy_from_slice(data), true)
                .map_err(|e| TransportError::Http2(format!("h2 send_data error: {e}")))?;
        }
    }

    Ok(())
}

#[cfg(test)]
#[allow(clippy::type_complexity)]
mod tests {
    use super::*;

    #[test]
    fn test_h2_request_to_message_basic() {
        let request = http::Request::builder()
            .method("GET")
            .uri("https://example.com/hello?q=1")
            .header("host", "example.com")
            .header("accept", "text/html")
            .body(())
            .unwrap();

        let (parts, _body) = request.into_parts();
        let msg = h2_request_to_message(&parts).unwrap();

        assert_eq!(msg.method, HttpMethod::Get);
        assert_eq!(msg.url, "/hello?q=1");
        assert_eq!(msg.protocol, HttpVersion::Http2);
        assert_eq!(msg.get_header("host"), Some("example.com"));
        assert_eq!(msg.get_header("accept"), Some("text/html"));
    }

    #[test]
    fn test_h2_request_to_message_post_with_content_length() {
        let request = http::Request::builder()
            .method("POST")
            .uri("/api/data")
            .header("content-type", "application/json")
            .header("content-length", "42")
            .body(())
            .unwrap();

        let (parts, _body) = request.into_parts();
        let msg = h2_request_to_message(&parts).unwrap();

        assert_eq!(msg.method, HttpMethod::Post);
        assert_eq!(msg.url, "/api/data");
        assert_eq!(msg.body_status, BodyStatus::Length);
        assert_eq!(msg.get_header("content-type"), Some("application/json"));
    }

    #[test]
    fn test_h2_request_to_message_no_query() {
        let request = http::Request::builder()
            .method("HEAD")
            .uri("/status")
            .body(())
            .unwrap();

        let (parts, _body) = request.into_parts();
        let msg = h2_request_to_message(&parts).unwrap();

        assert_eq!(msg.method, HttpMethod::Head);
        assert_eq!(msg.url, "/status");
    }

    #[test]
    fn test_h2_request_to_message_filters_pseudo_headers() {
        // HTTP/2 pseudo-headers like :method, :path, :scheme, :authority
        // are part of the URI/method in the http crate, but if any leak
        // into the header map they should be filtered out.
        let request = http::Request::builder()
            .method("GET")
            .uri("/test")
            .header("x-custom", "value")
            .body(())
            .unwrap();

        let (parts, _body) = request.into_parts();
        let msg = h2_request_to_message(&parts).unwrap();

        assert_eq!(msg.get_header("x-custom"), Some("value"));
        // Pseudo-headers should not appear
        assert!(msg.get_header(":method").is_none());
        assert!(msg.get_header(":path").is_none());
    }

    /// Test h2 server/client roundtrip using the raw h2 crate APIs.
    /// The server runs handle_h2_connection; the client sends a GET and verifies the response.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn test_h2_roundtrip_get() {
        use crate::traits::DetectedVersion;
        use rv_types::HttpStatus;
        use std::future::Future;
        use std::pin::Pin;
        use tokio::net::{TcpListener, TcpStream};

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        // Handler that echoes the request URL back in the response body.
        let handler: RequestHandler = Arc::new(
            |req: HttpMessage,
             _body: Option<Vec<u8>>,
             _conn: ConnectionInfo|
             -> Pin<Box<dyn Future<Output = (HttpMessage, Option<Vec<u8>>)> + Send>> {
                Box::pin(async move {
                    let body = req.url.as_bytes().to_vec();
                    let mut resp = HttpMessage::new_response(HttpStatus::OK, HttpVersion::Http2);
                    resp.set_header("content-type", "text/plain");
                    (resp, Some(body))
                })
            },
        );

        let handler_clone = Arc::clone(&handler);
        let server_task = tokio::spawn(async move {
            let (stream, peer) = listener.accept().await.unwrap();
            let local_addr = stream.local_addr().unwrap();

            let conn_info = ConnectionInfo {
                client_addr: peer,
                local_addr,
                real_client_addr: None,
                is_tls: false,
                http_version: DetectedVersion::Http2,
            };

            let _ = handle_h2_connection(stream, conn_info, handler_clone, 64 * 1024).await;
        });

        let stream = TcpStream::connect(addr).await.unwrap();
        let (mut client, h2_conn) = h2::client::handshake(stream).await.unwrap();

        tokio::spawn(async move {
            let _ = h2_conn.await;
        });

        let request = http::Request::builder()
            .method("GET")
            .uri("https://example.com/echo-test")
            .body(())
            .unwrap();

        let (response_future, _send) = client.send_request(request, true).unwrap();
        let response = response_future.await.unwrap();

        assert_eq!(response.status(), http::StatusCode::OK);

        let mut resp_body = response.into_body();
        let mut body_data = Vec::new();
        while let Some(chunk) = resp_body.data().await {
            let chunk = chunk.unwrap();
            let _ = resp_body.flow_control().release_capacity(chunk.len());
            body_data.extend_from_slice(&chunk);
        }
        assert_eq!(body_data, b"/echo-test");

        // Abort the server task -- we have verified the response.
        // The h2 connection shutdown is asynchronous and can take a while;
        // we do not need to wait for it in tests.
        server_task.abort();
    }

    /// Test h2 server/client roundtrip with a POST body.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn test_h2_roundtrip_post_with_body() {
        use crate::traits::DetectedVersion;
        use rv_types::HttpStatus;
        use std::future::Future;
        use std::pin::Pin;
        use tokio::net::{TcpListener, TcpStream};

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let handler: RequestHandler = Arc::new(
            |_req: HttpMessage,
             body: Option<Vec<u8>>,
             _conn: ConnectionInfo|
             -> Pin<Box<dyn Future<Output = (HttpMessage, Option<Vec<u8>>)> + Send>> {
                Box::pin(async move {
                    let resp_body = body.map(|mut b| {
                        b.reverse();
                        b
                    });
                    let resp = HttpMessage::new_response(HttpStatus::OK, HttpVersion::Http2);
                    (resp, resp_body)
                })
            },
        );

        let handler_clone = Arc::clone(&handler);
        let server_task = tokio::spawn(async move {
            let (stream, peer) = listener.accept().await.unwrap();
            let local_addr = stream.local_addr().unwrap();

            let conn_info = ConnectionInfo {
                client_addr: peer,
                local_addr,
                real_client_addr: None,
                is_tls: false,
                http_version: DetectedVersion::Http2,
            };

            let _ = handle_h2_connection(stream, conn_info, handler_clone, 64 * 1024).await;
        });

        let stream = TcpStream::connect(addr).await.unwrap();
        let (mut client, h2_conn) = h2::client::handshake(stream).await.unwrap();

        tokio::spawn(async move {
            let _ = h2_conn.await;
        });

        let request = http::Request::builder()
            .method("POST")
            .uri("https://example.com/data")
            .body(())
            .unwrap();

        let (response_future, mut send_stream) = client.send_request(request, false).unwrap();
        send_stream
            .send_data(Bytes::from_static(b"hello"), true)
            .unwrap();

        let response = response_future.await.unwrap();
        assert_eq!(response.status(), http::StatusCode::OK);

        let mut resp_body = response.into_body();
        let mut body_data = Vec::new();
        while let Some(chunk) = resp_body.data().await {
            let chunk = chunk.unwrap();
            let _ = resp_body.flow_control().release_capacity(chunk.len());
            body_data.extend_from_slice(&chunk);
        }
        // "hello" reversed is "olleh"
        assert_eq!(body_data, b"olleh");

        server_task.abort();
    }

    /// Test h2 server/client with 204 No Content (empty response body).
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn test_h2_roundtrip_no_content() {
        use crate::traits::DetectedVersion;
        use rv_types::HttpStatus;
        use std::future::Future;
        use std::pin::Pin;
        use tokio::net::{TcpListener, TcpStream};

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let handler: RequestHandler = Arc::new(
            |_req: HttpMessage,
             _body: Option<Vec<u8>>,
             _conn: ConnectionInfo|
             -> Pin<Box<dyn Future<Output = (HttpMessage, Option<Vec<u8>>)> + Send>> {
                Box::pin(async move {
                    let resp =
                        HttpMessage::new_response(HttpStatus::NO_CONTENT, HttpVersion::Http2);
                    (resp, None)
                })
            },
        );

        let handler_clone = Arc::clone(&handler);
        let server_task = tokio::spawn(async move {
            let (stream, peer) = listener.accept().await.unwrap();
            let local_addr = stream.local_addr().unwrap();

            let conn_info = ConnectionInfo {
                client_addr: peer,
                local_addr,
                real_client_addr: None,
                is_tls: false,
                http_version: DetectedVersion::Http2,
            };

            let _ = handle_h2_connection(stream, conn_info, handler_clone, 64 * 1024).await;
        });

        let stream = TcpStream::connect(addr).await.unwrap();
        let (mut client, h2_conn) = h2::client::handshake(stream).await.unwrap();

        tokio::spawn(async move {
            let _ = h2_conn.await;
        });

        let request = http::Request::builder()
            .method("GET")
            .uri("https://example.com/empty")
            .body(())
            .unwrap();

        let (response_future, _send) = client.send_request(request, true).unwrap();
        let response = response_future.await.unwrap();

        assert_eq!(response.status(), http::StatusCode::NO_CONTENT);

        server_task.abort();
    }
}

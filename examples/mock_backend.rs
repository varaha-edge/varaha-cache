/// Mock backend HTTP server for testing varaha-cache with full_features.vcl.
///
/// Listens on 127.0.0.1:18080 and returns different responses based on URL.
///
/// Run with:
///   rustc examples/mock_backend.rs -o target/mock_backend && target/mock_backend
///
/// Or compile and run in one step:
///   cargo build -p rv-server && rustc examples/mock_backend.rs -o target/mock_backend
///   target/mock_backend &
///   cargo run -p rv-server -- -f examples/full_features.vcl
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;

fn main() {
    let listener = TcpListener::bind("127.0.0.1:18080").expect("failed to bind 18080");
    eprintln!("[mock-backend] listening on 127.0.0.1:18080");

    for stream in listener.incoming() {
        let mut stream = match stream {
            Ok(s) => s,
            Err(e) => {
                eprintln!("[mock-backend] accept error: {e}");
                continue;
            }
        };

        let reader = BufReader::new(stream.try_clone().unwrap());
        let mut lines = reader.lines();

        // Read request line
        let request_line = match lines.next() {
            Some(Ok(line)) => line,
            _ => continue,
        };

        // Read headers until blank line
        let mut headers = Vec::new();
        for line in lines.by_ref() {
            match line {
                Ok(l) if l.trim().is_empty() => break,
                Ok(l) => headers.push(l),
                Err(_) => break,
            }
        }

        // Parse method and URL
        let parts: Vec<&str> = request_line.split_whitespace().collect();
        let method = parts.first().copied().unwrap_or("GET");
        let url = parts.get(1).copied().unwrap_or("/");

        eprintln!("[mock-backend] {method} {url}");

        let (status, content_type, extra_headers, body) = route(url, method, &headers);

        let response = format!(
            "HTTP/1.1 {status}\r\n\
             Content-Type: {content_type}\r\n\
             Content-Length: {}\r\n\
             {extra_headers}\
             Connection: close\r\n\
             \r\n\
             {body}",
            body.len(),
        );

        let _ = stream.write_all(response.as_bytes());
    }
}

fn route<'a>(url: &'a str, method: &'a str, headers: &[String]) -> (&'a str, &'a str, String, String) {
    // Echo X-Backend-Echo if present
    let mut extra = String::new();
    for h in headers {
        if h.to_lowercase().starts_with("x-backend-echo:") {
            let val = h.splitn(2, ':').nth(1).unwrap_or("").trim();
            extra.push_str(&format!("X-Backend-Echoed: {val}\r\n"));
        }
        if h.to_lowercase().starts_with("x-api-request:") {
            extra.push_str("X-Api-Confirmed: true\r\n");
        }
    }

    match url {
        // ---- JSON API endpoint ----
        "/api/data" | "/api/v2/data" => (
            "200 OK",
            "application/json",
            format!(
                "ETag: \"v1-abc123\"\r\n\
                 Last-Modified: Mon, 01 Jan 2024 00:00:00 GMT\r\n\
                 Vary: Accept-Encoding\r\n\
                 Cache-Control: max-age=60\r\n\
                 {extra}"
            ),
            r#"{"status":"ok","data":{"users":42,"active":true},"timestamp":"2024-01-01T00:00:00Z"}"#
                .to_string(),
        ),

        // ---- Private/uncacheable endpoint ----
        "/api/private" => (
            "200 OK",
            "text/plain",
            format!("Cache-Control: no-store, private\r\n{extra}"),
            "This is private data that should never be cached.".to_string(),
        ),

        // ---- User profile (also private) ----
        p if p.starts_with("/user/profile") => (
            "200 OK",
            "application/json",
            format!("Cache-Control: private\r\n{extra}"),
            r#"{"username":"testuser","email":"test@example.com"}"#.to_string(),
        ),

        // ---- Static assets ----
        p if p.ends_with(".css") => (
            "200 OK",
            "text/css",
            format!("Cache-Control: max-age=604800\r\n{extra}"),
            "body { margin: 0; font-family: sans-serif; }".to_string(),
        ),
        p if p.ends_with(".js") => (
            "200 OK",
            "application/javascript",
            format!("Cache-Control: max-age=604800\r\n{extra}"),
            "console.log('varaha-cache test');".to_string(),
        ),
        p if p.ends_with(".png") || p.ends_with(".jpg") => (
            "200 OK",
            "image/png",
            format!("Cache-Control: max-age=604800\r\n{extra}"),
            "FAKE-IMAGE-DATA-0123456789".to_string(),
        ),

        // ---- HTML pages ----
        "/" | "/index.html" => (
            "200 OK",
            "text/html",
            format!("Cache-Control: max-age=300\r\n{extra}"),
            "<html><body><h1>Welcome to varaha-cache</h1><p>This is the homepage.</p></body></html>"
                .to_string(),
        ),

        // ---- Legacy redirect target ----
        "/legacy/home" => (
            "200 OK",
            "text/html",
            format!("{extra}"),
            "<html><body>Legacy home (should be rewritten by VCL)</body></html>".to_string(),
        ),

        // ---- Old API path (tests URL rewriting) ----
        p if p.starts_with("/old-api/") => (
            "200 OK",
            "application/json",
            format!("{extra}"),
            format!(r#"{{"old_api":"should have been rewritten","url":"{p}"}}"#),
        ),

        // ---- POST handler ----
        _ if method == "POST" => (
            "200 OK",
            "application/json",
            format!("{extra}"),
            format!(r#"{{"method":"POST","url":"{url}","accepted":true}}"#),
        ),

        // ---- 404 for unknown paths ----
        _ => (
            "200 OK",
            "text/plain",
            format!("{extra}"),
            format!("Backend response for {url}"),
        ),
    }
}

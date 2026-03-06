use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::time::Duration;

use rv_admin::protocol::{CliResponse, CliStatus, decode_response};

/// Statistics viewer tool - displays cache performance counters.
/// Equivalent to varnishstat in the C codebase.
///
/// A client that connects to the admin port and retrieves status information
/// using the CLI wire protocol.
pub struct StatClient {
    stream: TcpStream,
}

impl StatClient {
    /// Connect to the admin server at the given address.
    ///
    /// The connection timeout is 5 seconds. After connecting, the client
    /// reads and discards the initial banner (or auth challenge) sent by
    /// the server.
    pub fn connect(addr: SocketAddr) -> Result<Self, anyhow::Error> {
        let stream = TcpStream::connect_timeout(&addr, Duration::from_secs(5))?;
        stream.set_read_timeout(Some(Duration::from_secs(5)))?;
        stream.set_write_timeout(Some(Duration::from_secs(5)))?;

        let mut client = Self { stream };

        // Read and discard the initial banner from the server.
        let _banner = client.read_response()?;

        Ok(client)
    }

    /// Connect to the admin server and perform authentication using the
    /// challenge-response scheme.
    ///
    /// The server sends a challenge; the client computes the SHA-256
    /// response using the shared secret and sends it back.
    pub fn connect_with_auth(addr: SocketAddr, secret: &str) -> Result<Self, anyhow::Error> {
        let stream = TcpStream::connect_timeout(&addr, Duration::from_secs(5))?;
        stream.set_read_timeout(Some(Duration::from_secs(5)))?;
        stream.set_write_timeout(Some(Duration::from_secs(5)))?;

        let mut client = Self { stream };

        // Read the auth challenge from the server.
        let challenge_resp = client.read_response()?;

        // The challenge body is the hex-encoded challenge bytes.
        let challenge_hex = challenge_resp.body.trim();
        let challenge_bytes = hex_decode(challenge_hex);

        // Compute the auth response.
        let auth_response =
            rv_admin::auth::compute_auth_response(&challenge_bytes, secret.as_bytes());

        // Send the auth response.
        let msg = format!("{auth_response}\n");
        client.stream.write_all(msg.as_bytes())?;

        // Read the auth result.
        let result = client.read_response()?;
        if result.status != CliStatus::Ok {
            return Err(anyhow::anyhow!("authentication failed: {}", result.body));
        }

        Ok(client)
    }

    /// Send a command string to the admin server and read the response.
    pub fn send_command(&mut self, cmd: &str) -> Result<CliResponse, anyhow::Error> {
        let msg = format!("{cmd}\n");
        self.stream.write_all(msg.as_bytes())?;
        self.read_response()
    }

    /// Send the `status` command and return the response body as a string.
    pub fn get_status(&mut self) -> Result<String, anyhow::Error> {
        let resp = self.send_command("status")?;
        if resp.status == CliStatus::Ok {
            Ok(resp.body)
        } else {
            Err(anyhow::anyhow!(
                "status command failed ({}): {}",
                resp.status.code(),
                resp.body
            ))
        }
    }

    /// Read a single wire-protocol response from the server.
    ///
    /// The protocol format is:
    /// ```text
    /// <status> <length>\n
    /// <body>\n
    /// ```
    fn read_response(&mut self) -> Result<CliResponse, anyhow::Error> {
        // Read the header line to determine body length.
        let header = self.read_line()?;
        let mut parts = header.splitn(2, ' ');

        let status_str = parts
            .next()
            .ok_or_else(|| anyhow::anyhow!("missing status code in response header"))?;
        let length_str = parts
            .next()
            .ok_or_else(|| anyhow::anyhow!("missing length in response header"))?
            .trim();

        let _status_code: u16 = status_str.parse()?;
        let body_len: usize = length_str.parse()?;

        // Read the body + trailing newline.
        let mut body_buf = vec![0u8; body_len + 1]; // +1 for trailing \n
        self.stream.read_exact(&mut body_buf)?;

        // Reconstruct the full wire message for decoding.
        let mut full_msg = Vec::with_capacity(header.len() + 1 + body_buf.len());
        full_msg.extend_from_slice(header.as_bytes());
        full_msg.push(b'\n');
        full_msg.extend_from_slice(&body_buf);

        let resp = decode_response(&full_msg)?;
        Ok(resp)
    }

    /// Read a single line (terminated by `\n`) from the TCP stream.
    fn read_line(&mut self) -> Result<String, anyhow::Error> {
        let mut line = Vec::new();
        let mut byte = [0u8; 1];
        loop {
            self.stream.read_exact(&mut byte)?;
            if byte[0] == b'\n' {
                break;
            }
            line.push(byte[0]);
        }
        Ok(String::from_utf8(line)?)
    }
}

/// Parse the status response body into key-value pairs suitable for JSON output.
///
/// Expects lines like "Uptime: 0h 00m 05s", "Objects: 0", "Hit rate: 0.0%", etc.
fn parse_status_fields(body: &str) -> serde_json::Value {
    let mut map = serde_json::Map::new();

    for line in body.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Some((key, value)) = line.split_once(':') {
            let key = key.trim().to_lowercase().replace(' ', "_");
            let value = value.trim().to_string();
            map.insert(key, serde_json::Value::String(value));
        }
    }

    serde_json::Value::Object(map)
}

/// Decode a hex string into bytes.
fn hex_decode(hex: &str) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(hex.len() / 2);
    let mut chars = hex.chars();
    while let Some(hi) = chars.next() {
        if let Some(lo) = chars.next() {
            let byte = u8::from_str_radix(&format!("{hi}{lo}"), 16).unwrap_or(0);
            bytes.push(byte);
        }
    }
    bytes
}

fn print_usage() {
    eprintln!("Usage: rv-stat [OPTIONS]");
    eprintln!();
    eprintln!("Options:");
    eprintln!("  -j           JSON output mode");
    eprintln!("  -1           One-shot mode (print and exit)");
    eprintln!("  -n INTERVAL  Refresh interval in seconds (default: 1)");
    eprintln!("  -T HOST:PORT Connect to admin server for stats");
    eprintln!("  -S SECRET    Shared secret for authentication");
    eprintln!("  -h           Show this help");
}

fn main() {
    let args: Vec<String> = std::env::args().collect();

    let mut json_mode = false;
    let mut one_shot = false;
    let mut interval_secs: u64 = 1;
    let mut admin_addr: Option<SocketAddr> = None;
    let mut secret: Option<String> = None;

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "-j" => json_mode = true,
            "-1" => one_shot = true,
            "-n" => {
                i += 1;
                if i < args.len() {
                    interval_secs = args[i].parse().unwrap_or(1);
                }
            }
            "-T" => {
                i += 1;
                if i < args.len() {
                    admin_addr = args[i].parse().ok();
                }
            }
            "-S" => {
                i += 1;
                if i < args.len() {
                    secret = Some(args[i].clone());
                }
            }
            "-h" | "--help" => {
                print_usage();
                return;
            }
            _ => {
                eprintln!("Unknown option: {}", args[i]);
                print_usage();
                std::process::exit(1);
            }
        }
        i += 1;
    }

    if let Some(addr) = admin_addr {
        // Connect to the admin server and fetch live stats.
        run_with_admin(addr, secret, json_mode, one_shot, interval_secs);
    } else if one_shot || json_mode {
        print_stats(json_mode);
    } else {
        // Continuous mode with placeholder stats.
        loop {
            // Clear screen.
            print!("\x1B[2J\x1B[H");
            std::io::stdout().flush().ok();

            println!("varaha-cache statistics - press Ctrl+C to quit");
            println!("==============================================");
            println!();
            print_stats(false);

            std::thread::sleep(std::time::Duration::from_secs(interval_secs));
        }
    }
}

/// Connect to the admin server and display live stats.
fn run_with_admin(
    addr: SocketAddr,
    secret: Option<String>,
    json_mode: bool,
    one_shot: bool,
    interval_secs: u64,
) {
    let connect_result = match secret {
        Some(s) => StatClient::connect_with_auth(addr, &s),
        None => StatClient::connect(addr),
    };

    let mut client = match connect_result {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Could not connect to {addr}: {e}");
            eprintln!("Is the server running with -T {addr}?");
            std::process::exit(1);
        }
    };

    if one_shot || json_mode {
        match client.get_status() {
            Ok(status) => {
                if json_mode {
                    let json = parse_status_fields(&status);
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&json).unwrap_or_default()
                    );
                } else {
                    println!("{status}");
                }
            }
            Err(e) => {
                eprintln!("Failed to get status: {e}");
                std::process::exit(1);
            }
        }
    } else {
        // Continuous mode.
        loop {
            print!("\x1B[2J\x1B[H");
            std::io::stdout().flush().ok();

            println!("varaha-cache statistics - press Ctrl+C to quit");
            println!("==============================================");
            println!();

            match client.get_status() {
                Ok(status) => println!("{status}"),
                Err(e) => {
                    eprintln!("Failed to get status: {e}");
                    std::process::exit(1);
                }
            }

            std::thread::sleep(std::time::Duration::from_secs(interval_secs));
        }
    }
}

fn print_stats(json_mode: bool) {
    // In production, these would come from shared memory or admin connection.
    // For now, show placeholder output that demonstrates the format.
    if json_mode {
        let obj = serde_json::json!({
            "timestamp": chrono_now(),
            "counters": {
                "cache_hit": 0,
                "cache_miss": 0,
                "cache_pass": 0,
                "backend_fetches": 0,
                "n_objects": 0,
                "n_expired": 0,
                "bytes_stored": 0,
                "hit_rate": 0.0
            }
        });
        println!("{}", serde_json::to_string_pretty(&obj).unwrap_or_default());
    } else {
        println!("{:<30} {:>12} {:>8}", "COUNTER", "VALUE", "RATE");
        println!("{:-<54}", "");
        println!("{:<30} {:>12} {:>8}", "cache_hit", "0", "0/s");
        println!("{:<30} {:>12} {:>8}", "cache_miss", "0", "0/s");
        println!("{:<30} {:>12} {:>8}", "cache_pass", "0", "0/s");
        println!("{:<30} {:>12} {:>8}", "cache_hit_for_pass", "0", "0/s");
        println!("{:<30} {:>12} {:>8}", "cache_hit_grace", "0", "0/s");
        println!("{:<30} {:>12} {:>8}", "backend_fetches", "0", "0/s");
        println!("{:<30} {:>12} {:>8}", "evictions", "0", "0/s");
        println!("{:<30} {:>12} {:>8}", "n_objects", "0", ".");
        println!("{:<30} {:>12} {:>8}", "n_expired", "0", ".");
        println!("{:<30} {:>12} {:>8}", "bytes_stored", "0", ".");
        println!();
        println!("Hit rate: 0.00%");
    }
}

fn chrono_now() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    format!("{secs}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_status_fields_basic() {
        let body = "\
Child (worker) process is running.
Uptime: 0h 00m 05s
Objects: 42
Cache hits: 100
Cache misses: 10
Hit rate: 90.9%";

        let json = parse_status_fields(body);
        let obj = json.as_object().unwrap();

        assert!(obj.contains_key("uptime"));
        assert_eq!(obj["objects"].as_str().unwrap(), "42");
        assert_eq!(obj["cache_hits"].as_str().unwrap(), "100");
        assert_eq!(obj["cache_misses"].as_str().unwrap(), "10");
        assert_eq!(obj["hit_rate"].as_str().unwrap(), "90.9%");
    }

    #[test]
    fn parse_status_fields_empty() {
        let json = parse_status_fields("");
        assert!(json.as_object().unwrap().is_empty());
    }

    #[test]
    fn parse_status_fields_no_colon() {
        let body = "Child (worker) process is running.";
        let json = parse_status_fields(body);
        // "Child (worker) process is running." has no colon, so it is skipped.
        assert!(json.as_object().unwrap().is_empty());
    }

    #[test]
    fn hex_decode_known_values() {
        assert_eq!(hex_decode("deadbeef"), vec![0xde, 0xad, 0xbe, 0xef]);
        assert_eq!(hex_decode("00ff"), vec![0x00, 0xff]);
        assert_eq!(hex_decode(""), Vec::<u8>::new());
    }

    #[test]
    fn json_output_is_valid() {
        let obj = serde_json::json!({
            "timestamp": "1234567890",
            "counters": {
                "cache_hit": 0,
                "cache_miss": 0,
            }
        });
        let s = serde_json::to_string_pretty(&obj).unwrap();
        assert!(s.contains("cache_hit"));
        assert!(s.contains("timestamp"));
    }
}

use std::io::{self, BufRead, Write};
use std::net::SocketAddr;

/// Admin CLI tool - connects to the management port.
/// Equivalent to varnishadm in the C codebase.

fn print_usage() {
    eprintln!("Usage: rv-admin-cli [OPTIONS] [COMMAND...]");
    eprintln!();
    eprintln!("Options:");
    eprintln!("  -T HOST:PORT   Connect to management address (default: 127.0.0.1:6082)");
    eprintln!("  -h             Show this help");
    eprintln!();
    eprintln!("Commands:");
    eprintln!("  ping                  Ping the server");
    eprintln!("  status                Show server status");
    eprintln!("  vcl.list              List loaded VCL programs");
    eprintln!("  vcl.load NAME FILE    Load a VCL program");
    eprintln!("  vcl.use NAME          Activate a VCL program");
    eprintln!("  vcl.discard NAME      Discard a VCL program");
    eprintln!("  ban EXPR              Add a ban expression");
    eprintln!("  ban.list              List active bans");
    eprintln!("  param.show [PARAM]    Show parameters");
    eprintln!("  param.set PARAM VAL   Set a parameter");
    eprintln!("  backend.list          List backends");
    eprintln!("  help                  Show available commands");
}

fn main() {
    let args: Vec<String> = std::env::args().collect();

    let mut addr: SocketAddr = "127.0.0.1:6082".parse().unwrap();
    let mut command_args: Vec<String> = Vec::new();

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "-T" => {
                i += 1;
                if i < args.len() {
                    addr = args[i].parse().unwrap_or(addr);
                }
            }
            "-h" | "--help" => {
                print_usage();
                return;
            }
            _ => {
                // Everything else is the command
                command_args = args[i..].to_vec();
                break;
            }
        }
        i += 1;
    }

    if command_args.is_empty() {
        // Interactive mode
        println!("rv-admin-cli - connecting to {}", addr);
        println!("Type 'help' for available commands, 'quit' to exit.");
        println!();

        let stdin = io::stdin();
        let mut stdout = io::stdout();

        loop {
            print!("varnish> ");
            stdout.flush().ok();

            let mut line = String::new();
            match stdin.lock().read_line(&mut line) {
                Ok(0) => break, // EOF
                Ok(_) => {
                    let line = line.trim();
                    if line.is_empty() {
                        continue;
                    }
                    if line == "quit" || line == "exit" {
                        break;
                    }
                    send_command(addr, line);
                }
                Err(e) => {
                    eprintln!("Error reading input: {}", e);
                    break;
                }
            }
        }
    } else {
        // One-shot mode
        let command = command_args.join(" ");
        send_command(addr, &command);
    }
}

fn send_command(addr: SocketAddr, command: &str) {
    use std::io::Read;
    use std::net::TcpStream;
    use std::time::Duration;

    match TcpStream::connect_timeout(&addr.into(), Duration::from_secs(5)) {
        Ok(mut stream) => {
            stream.set_read_timeout(Some(Duration::from_secs(5))).ok();
            stream.set_write_timeout(Some(Duration::from_secs(5))).ok();

            // Send command
            let cmd = format!("{}\n", command);
            if let Err(e) = stream.write_all(cmd.as_bytes()) {
                eprintln!("Error sending command: {}", e);
                return;
            }

            // Read response
            let mut response = String::new();
            match stream.read_to_string(&mut response) {
                Ok(_) => {
                    if response.is_empty() {
                        println!("(no response)");
                    } else {
                        print!("{}", response);
                    }
                }
                Err(e) => {
                    // Timeout is expected after receiving data
                    if !response.is_empty() {
                        print!("{}", response);
                    } else {
                        eprintln!("Error reading response: {}", e);
                    }
                }
            }
        }
        Err(e) => {
            eprintln!("Could not connect to {}: {}", addr, e);
            eprintln!("Is the server running with -T {}?", addr);
            std::process::exit(1);
        }
    }
}

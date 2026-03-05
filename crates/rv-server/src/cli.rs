use std::net::SocketAddr;
use std::path::PathBuf;

/// Command-line arguments for the varaha-cache server.
pub struct CliArgs {
    /// Listen address for client connections (default: 127.0.0.1:6081)
    pub listen_addr: SocketAddr,
    /// Backend address (default: 127.0.0.1:8080)
    pub backend_addr: Option<SocketAddr>,
    /// Path to VCL file
    pub vcl_file: Option<PathBuf>,
    /// Path to configuration file
    pub config_file: Option<PathBuf>,
    /// Storage specification (default: malloc,256m)
    pub storage_spec: String,
    /// Admin listen address (default: 127.0.0.1:6082)
    pub admin_addr: SocketAddr,
    /// Working directory
    pub workdir: Option<PathBuf>,
    /// Hash algorithm (default: critbit)
    pub hash_type: String,
}

impl Default for CliArgs {
    fn default() -> Self {
        Self {
            listen_addr: "127.0.0.1:6081".parse().unwrap(),
            backend_addr: None,
            vcl_file: None,
            config_file: None,
            storage_spec: "malloc,256m".to_string(),
            admin_addr: "127.0.0.1:6082".parse().unwrap(),
            workdir: None,
            hash_type: "critbit".to_string(),
        }
    }
}

/// Parse command-line arguments.
pub fn parse_args() -> CliArgs {
    let args: Vec<String> = std::env::args().collect();
    let mut cli = CliArgs::default();

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "-a" => {
                i += 1;
                if i < args.len() {
                    if let Ok(addr) = args[i].parse() {
                        cli.listen_addr = addr;
                    }
                }
            }
            "-b" => {
                i += 1;
                if i < args.len() {
                    cli.backend_addr = args[i].parse().ok();
                }
            }
            "-f" => {
                i += 1;
                if i < args.len() {
                    cli.vcl_file = Some(PathBuf::from(&args[i]));
                }
            }
            "-C" => {
                i += 1;
                if i < args.len() {
                    cli.config_file = Some(PathBuf::from(&args[i]));
                }
            }
            "-s" => {
                i += 1;
                if i < args.len() {
                    cli.storage_spec = args[i].clone();
                }
            }
            "-T" => {
                i += 1;
                if i < args.len() {
                    if let Ok(addr) = args[i].parse() {
                        cli.admin_addr = addr;
                    }
                }
            }
            "-n" => {
                i += 1;
                if i < args.len() {
                    cli.workdir = Some(PathBuf::from(&args[i]));
                }
            }
            "-h" | "--help" => {
                print_usage();
                std::process::exit(0);
            }
            "-V" | "--version" => {
                println!("varaha-cache 0.1.0");
                std::process::exit(0);
            }
            arg => {
                eprintln!("Unknown argument: {}", arg);
                print_usage();
                std::process::exit(1);
            }
        }
        i += 1;
    }

    cli
}

fn print_usage() {
    eprintln!("Usage: varaha-cache [OPTIONS]");
    eprintln!();
    eprintln!("Options:");
    eprintln!("  -a ADDR       Listen address (default: 127.0.0.1:6081)");
    eprintln!("  -b ADDR       Backend address (e.g., 127.0.0.1:8080)");
    eprintln!("  -f FILE       VCL file path");
    eprintln!("  -C FILE       Configuration file path");
    eprintln!("  -s SPEC       Storage specification (default: malloc,256m)");
    eprintln!("  -T ADDR       Admin listen address (default: 127.0.0.1:6082)");
    eprintln!("  -n DIR        Working directory");
    eprintln!("  -h, --help    Show this help");
    eprintln!("  -V, --version Show version");
}

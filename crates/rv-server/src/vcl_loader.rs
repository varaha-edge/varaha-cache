use std::net::SocketAddr;
use std::path::Path;

use anyhow::{Context, Result};
use tracing::info;

use rv_vcl::{Expr, Lexer, Parser, VclProgram};

/// Load and parse a VCL file from disk.
pub fn load_vcl(path: &Path) -> Result<VclProgram> {
    let source = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read VCL file: {}", path.display()))?;

    let tokens = Lexer::tokenize(&source).map_err(|e| anyhow::anyhow!("VCL lexer error: {e}"))?;

    let program = Parser::parse(&tokens).map_err(|e| anyhow::anyhow!("VCL parser error: {e}"))?;

    info!(
        path = %path.display(),
        backends = program.backends.len(),
        subs = program.subs.len(),
        "VCL loaded"
    );

    Ok(program)
}

/// Extract backend name and address from parsed VCL backend declarations.
pub fn extract_backends(program: &VclProgram) -> Vec<(String, SocketAddr)> {
    let mut backends = Vec::new();

    for decl in &program.backends {
        let host = eval_string_property(&decl.properties, "host");
        let port =
            eval_string_property(&decl.properties, "port").unwrap_or_else(|| "80".to_string());

        if let Some(host) = host {
            let addr_str = format!("{host}:{port}");
            if let Ok(addr) = addr_str.parse::<SocketAddr>() {
                info!(name = %decl.name, addr = %addr, "found backend in VCL");
                backends.push((decl.name.clone(), addr));
            } else {
                tracing::warn!(
                    name = %decl.name,
                    addr = %addr_str,
                    "could not parse backend address"
                );
            }
        }
    }

    backends
}

/// Extract a string value from a list of VCL backend properties.
fn eval_string_property(properties: &[(String, Expr)], key: &str) -> Option<String> {
    for (name, expr) in properties {
        if name == key {
            return match expr {
                Expr::StringLit(s) => Some(s.clone()),
                Expr::IntLit(i) => Some(i.to_string()),
                _ => None,
            };
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_backends() {
        let vcl_src = r#"
vcl 4.0;

backend default {
    .host = "127.0.0.1";
    .port = "8080";
}

backend api {
    .host = "10.0.0.1";
    .port = "9090";
}
"#;
        let tokens = Lexer::tokenize(vcl_src).unwrap();
        let program = Parser::parse(&tokens).unwrap();
        let backends = extract_backends(&program);

        assert_eq!(backends.len(), 2);
        assert_eq!(backends[0].0, "default");
        assert_eq!(
            backends[0].1,
            "127.0.0.1:8080".parse::<SocketAddr>().unwrap()
        );
        assert_eq!(backends[1].0, "api");
        assert_eq!(
            backends[1].1,
            "10.0.0.1:9090".parse::<SocketAddr>().unwrap()
        );
    }

    #[test]
    fn test_extract_backend_default_port() {
        let vcl_src = r#"
vcl 4.0;

backend web {
    .host = "192.168.1.1";
}
"#;
        let tokens = Lexer::tokenize(vcl_src).unwrap();
        let program = Parser::parse(&tokens).unwrap();
        let backends = extract_backends(&program);

        assert_eq!(backends.len(), 1);
        assert_eq!(
            backends[0].1,
            "192.168.1.1:80".parse::<SocketAddr>().unwrap()
        );
    }
}

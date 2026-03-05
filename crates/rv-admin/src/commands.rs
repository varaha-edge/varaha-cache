//! CLI command parsing.
//!
//! Parses a text command line (as received over the admin TCP socket) into a
//! strongly-typed [`CliCommand`] enum variant. The command syntax follows the
//! Varnish CLI conventions: a command name optionally followed by positional
//! arguments separated by whitespace.

use crate::error::AdminError;

/// A parsed CLI command ready for execution by the handler.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CliCommand {
    /// Load a new VCL program with the given name from the given source.
    VclLoad { name: String, source: String },

    /// Switch the active VCL to the named program.
    VclUse { name: String },

    /// List all loaded VCL programs.
    VclList,

    /// Discard the named VCL program.
    VclDiscard { name: String },

    /// Add a ban expression.
    Ban { expression: String },

    /// List all active bans.
    BanList,

    /// Show one or all runtime parameters.
    ParamShow { param: Option<String> },

    /// Set a runtime parameter to a new value.
    ParamSet { param: String, value: String },

    /// List all configured backends.
    BackendList,

    /// Set the health state of a backend.
    BackendSetHealth { backend: String, health: String },

    /// Show the current child process status and uptime.
    Status,

    /// Stop accepting new connections (child stop).
    Stop,

    /// Start accepting connections (child start).
    Start,

    /// Ping the admin server. Expects a "PONG" response with a timestamp.
    Ping,

    /// List available commands.
    Help,

    /// Stream recent log entries with optional tag filtering.
    LogStream { tags: Option<String> },
}

impl CliCommand {
    /// Return the command name for audit logging.
    pub fn name(&self) -> &'static str {
        match self {
            Self::VclLoad { .. } => "vcl.load",
            Self::VclUse { .. } => "vcl.use",
            Self::VclList => "vcl.list",
            Self::VclDiscard { .. } => "vcl.discard",
            Self::Ban { .. } => "ban",
            Self::BanList => "ban.list",
            Self::ParamShow { .. } => "param.show",
            Self::ParamSet { .. } => "param.set",
            Self::BackendList => "backend.list",
            Self::BackendSetHealth { .. } => "backend.set_health",
            Self::Status => "status",
            Self::Stop => "stop",
            Self::Start => "start",
            Self::Ping => "ping",
            Self::Help => "help",
            Self::LogStream { .. } => "log.stream",
        }
    }
}

/// Parse a single command line into a [`CliCommand`].
///
/// The input is a trimmed line of text. Leading and trailing whitespace is
/// stripped before parsing. Empty lines return an error.
pub fn parse_command(line: &str) -> Result<CliCommand, AdminError> {
    let line = line.trim();
    if line.is_empty() {
        return Err(AdminError::InvalidCommand("empty command".to_string()));
    }

    // Split into the command word(s) and the rest. We need to handle
    // dotted command names like "vcl.load" and "param.show".
    let mut tokens = Tokenizer::new(line);
    let cmd = tokens
        .next()
        .ok_or_else(|| AdminError::InvalidCommand("empty command".to_string()))?;

    match cmd.as_str() {
        "vcl.load" => {
            let name = tokens.next().ok_or_else(|| {
                AdminError::InvalidCommand("vcl.load requires <name> <source>".to_string())
            })?;
            let source = tokens.rest().ok_or_else(|| {
                AdminError::InvalidCommand("vcl.load requires <name> <source>".to_string())
            })?;
            Ok(CliCommand::VclLoad { name, source })
        }
        "vcl.use" => {
            let name = tokens.next().ok_or_else(|| {
                AdminError::InvalidCommand("vcl.use requires <name>".to_string())
            })?;
            Ok(CliCommand::VclUse { name })
        }
        "vcl.list" => Ok(CliCommand::VclList),
        "vcl.discard" => {
            let name = tokens.next().ok_or_else(|| {
                AdminError::InvalidCommand("vcl.discard requires <name>".to_string())
            })?;
            Ok(CliCommand::VclDiscard { name })
        }
        "ban" => {
            let expression = tokens.rest().ok_or_else(|| {
                AdminError::InvalidCommand("ban requires <expression>".to_string())
            })?;
            Ok(CliCommand::Ban { expression })
        }
        "ban.list" => Ok(CliCommand::BanList),
        "param.show" => {
            let param = tokens.next();
            Ok(CliCommand::ParamShow { param })
        }
        "param.set" => {
            let param = tokens.next().ok_or_else(|| {
                AdminError::InvalidCommand("param.set requires <param> <value>".to_string())
            })?;
            let value = tokens.rest().ok_or_else(|| {
                AdminError::InvalidCommand("param.set requires <param> <value>".to_string())
            })?;
            Ok(CliCommand::ParamSet { param, value })
        }
        "backend.list" => Ok(CliCommand::BackendList),
        "backend.set_health" => {
            let backend = tokens.next().ok_or_else(|| {
                AdminError::InvalidCommand(
                    "backend.set_health requires <backend> <health>".to_string(),
                )
            })?;
            let health = tokens.next().ok_or_else(|| {
                AdminError::InvalidCommand(
                    "backend.set_health requires <backend> <health>".to_string(),
                )
            })?;
            Ok(CliCommand::BackendSetHealth { backend, health })
        }
        "status" => Ok(CliCommand::Status),
        "stop" => Ok(CliCommand::Stop),
        "start" => Ok(CliCommand::Start),
        "ping" => Ok(CliCommand::Ping),
        "help" => Ok(CliCommand::Help),
        "log.stream" => {
            let tags = tokens.rest();
            Ok(CliCommand::LogStream { tags })
        }
        other => Err(AdminError::InvalidCommand(format!(
            "unknown command: {other}"
        ))),
    }
}

/// A simple whitespace-aware tokenizer that handles quoted strings.
///
/// Tokens are either bare words separated by whitespace, or quoted strings
/// delimited by double quotes. Inside a quoted string, a backslash escapes
/// the next character.
struct Tokenizer<'a> {
    remaining: &'a str,
}

impl<'a> Tokenizer<'a> {
    fn new(input: &'a str) -> Self {
        Self {
            remaining: input.trim(),
        }
    }

    /// Extract the next token, consuming it from the remaining input.
    fn next(&mut self) -> Option<String> {
        self.remaining = self.remaining.trim_start();
        if self.remaining.is_empty() {
            return None;
        }

        if self.remaining.starts_with('"') {
            // Quoted string.
            self.parse_quoted()
        } else {
            // Bare word.
            self.parse_bare()
        }
    }

    /// Return all remaining text (trimmed), consuming the tokenizer.
    fn rest(&mut self) -> Option<String> {
        self.remaining = self.remaining.trim();
        if self.remaining.is_empty() {
            None
        } else {
            let result = self.remaining.to_string();
            self.remaining = "";
            Some(result)
        }
    }

    fn parse_bare(&mut self) -> Option<String> {
        let end = self
            .remaining
            .find(char::is_whitespace)
            .unwrap_or(self.remaining.len());
        let token = self.remaining[..end].to_string();
        self.remaining = &self.remaining[end..];
        Some(token)
    }

    fn parse_quoted(&mut self) -> Option<String> {
        // Skip opening quote.
        let input = &self.remaining[1..];
        let mut result = String::new();
        let mut chars = input.char_indices();
        let mut end_pos = input.len();

        while let Some((i, ch)) = chars.next() {
            match ch {
                '\\' => {
                    // Escaped character - take the next one literally.
                    if let Some((_, escaped)) = chars.next() {
                        result.push(escaped);
                    }
                }
                '"' => {
                    end_pos = i + 1; // past the closing quote
                    break;
                }
                _ => {
                    result.push(ch);
                }
            }
        }

        // +1 for the opening quote we already skipped.
        self.remaining = &self.remaining[(1 + end_pos)..];
        Some(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_ping() {
        assert_eq!(parse_command("ping").unwrap(), CliCommand::Ping);
    }

    #[test]
    fn parse_help() {
        assert_eq!(parse_command("help").unwrap(), CliCommand::Help);
    }

    #[test]
    fn parse_status() {
        assert_eq!(parse_command("status").unwrap(), CliCommand::Status);
    }

    #[test]
    fn parse_start() {
        assert_eq!(parse_command("start").unwrap(), CliCommand::Start);
    }

    #[test]
    fn parse_stop() {
        assert_eq!(parse_command("stop").unwrap(), CliCommand::Stop);
    }

    #[test]
    fn parse_vcl_list() {
        assert_eq!(parse_command("vcl.list").unwrap(), CliCommand::VclList);
    }

    #[test]
    fn parse_vcl_load() {
        let cmd = parse_command("vcl.load myvcl /etc/varnish/default.vcl").unwrap();
        assert_eq!(
            cmd,
            CliCommand::VclLoad {
                name: "myvcl".to_string(),
                source: "/etc/varnish/default.vcl".to_string(),
            }
        );
    }

    #[test]
    fn parse_vcl_load_inline() {
        let cmd =
            parse_command(r#"vcl.load test "vcl 4.0; backend default { .host = \"localhost\"; }""#)
                .unwrap();
        match cmd {
            CliCommand::VclLoad { name, source } => {
                assert_eq!(name, "test");
                assert!(source.contains("vcl 4.0"));
            }
            _ => panic!("expected VclLoad"),
        }
    }

    #[test]
    fn parse_vcl_use() {
        let cmd = parse_command("vcl.use production").unwrap();
        assert_eq!(
            cmd,
            CliCommand::VclUse {
                name: "production".to_string(),
            }
        );
    }

    #[test]
    fn parse_vcl_discard() {
        let cmd = parse_command("vcl.discard old_config").unwrap();
        assert_eq!(
            cmd,
            CliCommand::VclDiscard {
                name: "old_config".to_string(),
            }
        );
    }

    #[test]
    fn parse_ban() {
        let cmd = parse_command("ban req.url ~ ^/images/").unwrap();
        assert_eq!(
            cmd,
            CliCommand::Ban {
                expression: "req.url ~ ^/images/".to_string(),
            }
        );
    }

    #[test]
    fn parse_ban_compound() {
        let cmd = parse_command("ban req.url ~ ^/api/ && obj.http.X-Cache == old").unwrap();
        assert_eq!(
            cmd,
            CliCommand::Ban {
                expression: "req.url ~ ^/api/ && obj.http.X-Cache == old".to_string(),
            }
        );
    }

    #[test]
    fn parse_ban_list() {
        assert_eq!(parse_command("ban.list").unwrap(), CliCommand::BanList);
    }

    #[test]
    fn parse_param_show_all() {
        let cmd = parse_command("param.show").unwrap();
        assert_eq!(cmd, CliCommand::ParamShow { param: None });
    }

    #[test]
    fn parse_param_show_specific() {
        let cmd = parse_command("param.show default_ttl").unwrap();
        assert_eq!(
            cmd,
            CliCommand::ParamShow {
                param: Some("default_ttl".to_string()),
            }
        );
    }

    #[test]
    fn parse_param_set() {
        let cmd = parse_command("param.set default_ttl 300s").unwrap();
        assert_eq!(
            cmd,
            CliCommand::ParamSet {
                param: "default_ttl".to_string(),
                value: "300s".to_string(),
            }
        );
    }

    #[test]
    fn parse_backend_list() {
        assert_eq!(
            parse_command("backend.list").unwrap(),
            CliCommand::BackendList
        );
    }

    #[test]
    fn parse_backend_set_health() {
        let cmd = parse_command("backend.set_health web01 healthy").unwrap();
        assert_eq!(
            cmd,
            CliCommand::BackendSetHealth {
                backend: "web01".to_string(),
                health: "healthy".to_string(),
            }
        );
    }

    #[test]
    fn parse_empty_line() {
        assert!(parse_command("").is_err());
        assert!(parse_command("   ").is_err());
    }

    #[test]
    fn parse_unknown_command() {
        let err = parse_command("frobnicate").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("unknown command: frobnicate"));
    }

    #[test]
    fn parse_vcl_load_missing_args() {
        assert!(parse_command("vcl.load").is_err());
        assert!(parse_command("vcl.load myvcl").is_err());
    }

    #[test]
    fn parse_vcl_use_missing_args() {
        assert!(parse_command("vcl.use").is_err());
    }

    #[test]
    fn parse_ban_missing_expression() {
        assert!(parse_command("ban").is_err());
    }

    #[test]
    fn parse_param_set_missing_args() {
        assert!(parse_command("param.set").is_err());
        assert!(parse_command("param.set default_ttl").is_err());
    }

    #[test]
    fn parse_backend_set_health_missing_args() {
        assert!(parse_command("backend.set_health").is_err());
        assert!(parse_command("backend.set_health web01").is_err());
    }

    #[test]
    fn parse_with_leading_trailing_whitespace() {
        assert_eq!(parse_command("  ping  ").unwrap(), CliCommand::Ping);
        assert_eq!(parse_command("\tstatus\n").unwrap(), CliCommand::Status);
    }
}

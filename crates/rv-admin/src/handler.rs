//! Command execution against the cache engine and configuration.
//!
//! The [`AdminContext`] holds shared references to the subsystems that CLI
//! commands operate on. The [`handle_command`] function dispatches a parsed
//! [`CliCommand`] to the appropriate subsystem and produces a [`CliResponse`].

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use arc_swap::ArcSwapOption;
use rv_cache::CacheEngine;
use rv_config::CacheConfig;
use rv_log::{LogReader, LogWriter};
use rv_types::LogTag;
use rv_types::vsl::Vxid;
use rv_vcl::interpreter::VclInterpreter;

use crate::commands::CliCommand;
use crate::protocol::{CliResponse, CliStatus};
use crate::vcl_manager::VclManager;

/// Shared context available to all command handlers.
///
/// All fields are wrapped in `Arc` so that the context can be cheaply cloned
/// across spawned connection tasks. The `start_time` records when the cache
/// process started, used to compute uptime in the `status` command.
///
/// When `active_vcl` is set, `vcl.use` will atomically swap the interpreter
/// so that new requests pick up the updated VCL while in-flight requests
/// continue using whatever `Arc<VclInterpreter>` they already loaded.
pub struct AdminContext {
    pub cache: Arc<CacheEngine>,
    pub log: Arc<LogWriter>,
    pub config: Arc<Mutex<CacheConfig>>,
    pub start_time: Instant,
    pub vcl_manager: Arc<VclManager>,
    pub running: Arc<AtomicBool>,
    /// Atomically-swappable active VCL interpreter shared with the
    /// transport request handler. When set, `vcl.use` will atomically
    /// swap the interpreter so new requests pick up the updated VCL
    /// while in-flight requests retain the previous `Arc`.
    pub active_vcl: Option<Arc<ArcSwapOption<VclInterpreter>>>,
}

impl AdminContext {
    /// Create a new admin context.
    ///
    /// The `vcl_manager` and `running` flag are optional for backward
    /// compatibility. When not supplied, sensible defaults are used: an empty
    /// VCL manager and a running flag initialised to `true`.
    pub fn new(cache: Arc<CacheEngine>, log: Arc<LogWriter>, config: Arc<CacheConfig>) -> Self {
        Self {
            cache,
            log,
            config: Arc::new(Mutex::new((*config).clone())),
            start_time: Instant::now(),
            vcl_manager: Arc::new(VclManager::new()),
            running: Arc::new(AtomicBool::new(true)),
            active_vcl: None,
        }
    }

    /// Create an admin context with explicit VCL manager and running flag.
    pub fn with_vcl_manager(
        cache: Arc<CacheEngine>,
        log: Arc<LogWriter>,
        config: Arc<CacheConfig>,
        vcl_manager: Arc<VclManager>,
        running: Arc<AtomicBool>,
    ) -> Self {
        Self {
            cache,
            log,
            config: Arc::new(Mutex::new((*config).clone())),
            start_time: Instant::now(),
            vcl_manager,
            running,
            active_vcl: None,
        }
    }

    /// Create an admin context with an atomically-swappable active VCL.
    ///
    /// The `active_vcl` is shared with the transport request handler so
    /// that `vcl.use` can swap the interpreter without blocking in-flight
    /// requests.
    pub fn with_active_vcl(
        cache: Arc<CacheEngine>,
        log: Arc<LogWriter>,
        config: Arc<CacheConfig>,
        vcl_manager: Arc<VclManager>,
        running: Arc<AtomicBool>,
        active_vcl: Arc<ArcSwapOption<VclInterpreter>>,
    ) -> Self {
        Self {
            cache,
            log,
            config: Arc::new(Mutex::new((*config).clone())),
            start_time: Instant::now(),
            vcl_manager,
            running,
            active_vcl: Some(active_vcl),
        }
    }
}

/// Execute a parsed CLI command and return the CLI response.
pub fn handle_command(ctx: &AdminContext, cmd: CliCommand) -> CliResponse {
    // Audit log: record admin command execution
    rv_log::rv_log!(ctx.log, LogTag::Debug, Vxid(0), "admin: {}", cmd.name());

    match cmd {
        CliCommand::Ping => handle_ping(),
        CliCommand::Status => handle_status(ctx),
        CliCommand::Help => handle_help(),
        CliCommand::BanList => handle_ban_list(ctx),
        CliCommand::Ban { expression } => handle_ban(ctx, &expression),
        CliCommand::ParamShow { param } => handle_param_show(ctx, param.as_deref()),
        CliCommand::ParamSet { param, value } => handle_param_set(ctx, &param, &value),
        CliCommand::BackendList => handle_backend_list(),
        CliCommand::BackendSetHealth { backend, .. } => handle_backend_set_health(&backend),
        CliCommand::VclLoad { name, source } => handle_vcl_load(ctx, &name, &source),
        CliCommand::VclUse { name } => handle_vcl_use(ctx, &name),
        CliCommand::VclList => handle_vcl_list(ctx),
        CliCommand::VclDiscard { name } => handle_vcl_discard(ctx, &name),
        CliCommand::Stop => handle_stop(ctx),
        CliCommand::Start => handle_start(ctx),
        CliCommand::LogStream { tags } => handle_log_stream(ctx, tags.as_deref()),
    }
}

/// Handle the `ping` command.
fn handle_ping() -> CliResponse {
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    CliResponse::ok(format!("PONG {timestamp}"))
}

/// Handle the `status` command.
fn handle_status(ctx: &AdminContext) -> CliResponse {
    let uptime = ctx.start_time.elapsed();
    let secs = uptime.as_secs();
    let hours = secs / 3600;
    let minutes = (secs % 3600) / 60;
    let remaining_secs = secs % 60;

    let stats = ctx.cache.stats();
    let state = if ctx.running.load(Ordering::Relaxed) {
        "running"
    } else {
        "stopped"
    };

    let body = format!(
        "Child (worker) process is {state}.\n\
         Uptime: {}h {:02}m {:02}s\n\
         Objects: {}\n\
         Cache hits: {}\n\
         Cache misses: {}\n\
         Hit rate: {:.1}%",
        hours,
        minutes,
        remaining_secs,
        stats.n_objects,
        stats.cache_hit,
        stats.cache_miss,
        stats.hit_rate(),
    );
    CliResponse::ok(body)
}

/// Handle the `help` command.
fn handle_help() -> CliResponse {
    let body = "\
Available commands:\n\
  help                              Show this help message\n\
  ping                              Keep-alive check\n\
  status                            Show child process status and uptime\n\
  start                             Start the child process\n\
  stop                              Stop the child process\n\
  vcl.load <name> <source>          Load a VCL program\n\
  vcl.use <name>                    Switch to a loaded VCL program\n\
  vcl.list                          List loaded VCL programs\n\
  vcl.discard <name>                Discard a loaded VCL program\n\
  ban <expression>                  Add a ban expression\n\
  ban.list                          List active bans\n\
  param.show [<param>]              Show runtime parameters\n\
  param.set <param> <value>         Set a runtime parameter\n\
  backend.list                      List backends\n\
  backend.set_health <be> <health>  Set backend health state\n\
  log.stream [-t <tags>]            Stream recent log entries";

    CliResponse::ok(body)
}

/// Handle the `ban.list` command.
fn handle_ban_list(ctx: &AdminContext) -> CliResponse {
    let bans = ctx.cache.ban_list().list();

    if bans.is_empty() {
        return CliResponse::ok("No active bans.");
    }

    let mut lines = Vec::with_capacity(bans.len() + 1);
    lines.push(format!("{} active ban(s):", bans.len()));

    for (i, ban) in bans.iter().enumerate() {
        let status = if ban.completed { "completed" } else { "active" };
        let tests: Vec<String> = ban
            .tests
            .iter()
            .map(|t| match t {
                rv_cache::BanTest::UrlMatch(re) => format!("req.url ~ {}", re.as_str()),
                rv_cache::BanTest::ObjHeaderMatch { header, pattern } => {
                    format!("obj.http.{header} ~ {}", pattern.as_str())
                }
                rv_cache::BanTest::ObjHeaderEq { header, value } => {
                    format!("obj.http.{header} == {value}")
                }
            })
            .collect();

        lines.push(format!(
            "  {:>3}  [{status}]  {}",
            i + 1,
            tests.join(" && "),
        ));
    }

    CliResponse::ok(lines.join("\n"))
}

/// Handle the `ban` command.
fn handle_ban(ctx: &AdminContext, expression: &str) -> CliResponse {
    match ctx.cache.ban(expression) {
        Ok(()) => CliResponse::ok(format!("Ban added: {expression}")),
        Err(e) => CliResponse::new(CliStatus::Error, format!("Failed to add ban: {e}")),
    }
}

/// Handle the `param.show` command.
fn handle_param_show(ctx: &AdminContext, param: Option<&str>) -> CliResponse {
    let config = ctx.config.lock().unwrap();
    match param {
        Some(name) => {
            let value = get_param_value(&config, name);
            match value {
                Some(v) => CliResponse::ok(format!("{name}: {v}")),
                None => CliResponse::new(CliStatus::Error, format!("Unknown parameter: {name}")),
            }
        }
        None => {
            // Show all known parameters.
            let params = list_all_params(&config);
            CliResponse::ok(params)
        }
    }
}

/// Handle the `param.set` command.
fn handle_param_set(ctx: &AdminContext, param: &str, value: &str) -> CliResponse {
    let mut config = ctx.config.lock().unwrap();
    match set_param_value(&mut config, param, value) {
        Ok(()) => CliResponse::ok(format!("{param} set to {value}")),
        Err(e) => CliResponse::new(CliStatus::Error, e),
    }
}

/// Handle the `backend.list` command.
fn handle_backend_list() -> CliResponse {
    CliResponse::ok("No backends configured.")
}

/// Handle the `backend.set_health` command.
fn handle_backend_set_health(backend: &str) -> CliResponse {
    CliResponse::new(CliStatus::Error, format!("Backend '{backend}' not found."))
}

/// Handle the `vcl.load` command.
fn handle_vcl_load(ctx: &AdminContext, name: &str, source: &str) -> CliResponse {
    match ctx.vcl_manager.load(name, source) {
        Ok(()) => CliResponse::ok(format!("VCL '{name}' compiled and loaded.")),
        Err(e) => CliResponse::new(CliStatus::Error, format!("Failed to load VCL: {e}")),
    }
}

/// Handle the `vcl.use` command.
///
/// When the admin context has an `active_vcl` ArcSwap, the newly activated
/// interpreter is atomically swapped in so that subsequent requests pick it
/// up. In-flight requests that already loaded the previous `Arc` continue
/// to use it until they complete.
fn handle_vcl_use(ctx: &AdminContext, name: &str) -> CliResponse {
    match ctx.vcl_manager.use_program(name) {
        Ok(interpreter) => {
            // If we have an ArcSwap, atomically publish the new interpreter.
            if let Some(active_vcl) = &ctx.active_vcl {
                active_vcl.store(Some(interpreter));
                tracing::info!(vcl = name, "hot-swapped active VCL interpreter");
            }
            CliResponse::ok(format!("VCL '{name}' now active."))
        }
        Err(e) => CliResponse::new(CliStatus::Error, e),
    }
}

/// Handle the `vcl.list` command.
fn handle_vcl_list(ctx: &AdminContext) -> CliResponse {
    let entries = ctx.vcl_manager.list();
    if entries.is_empty() {
        return CliResponse::ok("No VCL programs loaded.");
    }

    let mut lines = Vec::with_capacity(entries.len() + 1);
    lines.push(format!("{:<20} {:<12} {}", "Name", "State", "Since"));

    for (name, state, loaded_at) in &entries {
        let age = loaded_at.elapsed();
        let secs = age.as_secs();
        let age_str = if secs < 60 {
            format!("{secs}s ago")
        } else if secs < 3600 {
            format!("{}m ago", secs / 60)
        } else {
            format!("{}h ago", secs / 3600)
        };
        lines.push(format!("{name:<20} {state:<12} {age_str}"));
    }

    CliResponse::ok(lines.join("\n"))
}

/// Handle the `vcl.discard` command.
fn handle_vcl_discard(ctx: &AdminContext, name: &str) -> CliResponse {
    match ctx.vcl_manager.discard(name) {
        Ok(()) => CliResponse::ok(format!("VCL '{name}' discarded.")),
        Err(e) => CliResponse::new(CliStatus::Error, e),
    }
}

/// Handle the `stop` command.
fn handle_stop(ctx: &AdminContext) -> CliResponse {
    if !ctx.running.load(Ordering::Relaxed) {
        return CliResponse::new(CliStatus::Error, "Child process is already stopped.");
    }
    ctx.running.store(false, Ordering::Relaxed);
    CliResponse::ok("Child process stopped.")
}

/// Handle the `start` command.
fn handle_start(ctx: &AdminContext) -> CliResponse {
    if ctx.running.load(Ordering::Relaxed) {
        return CliResponse::new(CliStatus::Error, "Child process is already running.");
    }
    ctx.running.store(true, Ordering::Relaxed);
    CliResponse::ok("Child process started.")
}

/// Handle the `log.stream` command.
///
/// Dumps recent log entries from the ring buffer. An optional comma-separated
/// list of tag names can be provided to filter entries (e.g. `-t ReqStart,RespStatus`).
fn handle_log_stream(ctx: &AdminContext, tags_filter: Option<&str>) -> CliResponse {
    let reader = LogReader::from_start(ctx.log.buffer().clone());
    let records = reader.tail(100);

    if records.is_empty() {
        return CliResponse::ok("No log entries.");
    }

    // Parse tag filter
    let tag_names: Option<Vec<&str>> = tags_filter.map(|t| {
        let stripped = t
            .strip_prefix("-t ")
            .or_else(|| t.strip_prefix("-t"))
            .unwrap_or(t);
        stripped.split(',').map(|s| s.trim()).collect()
    });

    let mut lines = Vec::new();
    for rec in &records {
        if let Some(ref names) = tag_names {
            if !names.iter().any(|n| *n == rec.tag.name()) {
                continue;
            }
        }
        lines.push(format!("{rec}"));
    }

    if lines.is_empty() {
        CliResponse::ok("No matching log entries.")
    } else {
        CliResponse::ok(lines.join("\n"))
    }
}

/// Look up a single named parameter from the configuration.
fn get_param_value(config: &CacheConfig, name: &str) -> Option<String> {
    match name {
        "default_ttl" => Some(config.ttl.default_ttl.clone()),
        "default_grace" => Some(config.ttl.default_grace.clone()),
        "default_keep" => Some(config.ttl.default_keep.clone()),
        "thread_pool_min" => Some(config.threads.thread_pool_min.to_string()),
        "thread_pool_max" => Some(config.threads.thread_pool_max.to_string()),
        "thread_pools" => Some(config.threads.pool_count.to_string()),
        "connect_timeout" => Some(config.timeouts.connect_timeout.clone()),
        "first_byte_timeout" => Some(config.timeouts.first_byte_timeout.clone()),
        "between_bytes_timeout" => Some(config.timeouts.between_bytes_timeout.clone()),
        "send_timeout" => Some(config.timeouts.send_timeout.clone()),
        "pipe_timeout" => Some(config.timeouts.pipe_timeout.clone()),
        "backend_idle_timeout" => Some(config.timeouts.backend_idle_timeout.clone()),
        "http2" => Some(config.features.http2.to_string()),
        "esi_disable_xml_check" => Some(config.features.esi_disable_xml_check.to_string()),
        "esi_ignore_https" => Some(config.features.esi_ignore_https.to_string()),
        "esi_ignore_other_elements" => Some(config.features.esi_ignore_other_elements.to_string()),
        "short_panic" => Some(config.features.short_panic.to_string()),
        "no_coredump" => Some(config.features.no_coredump.to_string()),
        "admin_listen" => Some(config.admin.listen.clone()),
        "log_size" => Some(config.logging.size.clone()),
        "log_format" => Some(config.logging.format.clone()),
        "hash_type" => Some(config.hash.type_.clone()),
        _ => None,
    }
}

/// Set a named parameter to a new value.
fn set_param_value(config: &mut CacheConfig, name: &str, value: &str) -> Result<(), String> {
    match name {
        "default_ttl" => {
            config.ttl.default_ttl = value.to_string();
            Ok(())
        }
        "default_grace" => {
            config.ttl.default_grace = value.to_string();
            Ok(())
        }
        "default_keep" => {
            config.ttl.default_keep = value.to_string();
            Ok(())
        }
        "thread_pool_min" => {
            let v: u32 = value
                .parse()
                .map_err(|_| format!("invalid value for {name}: {value}"))?;
            config.threads.thread_pool_min = v;
            Ok(())
        }
        "thread_pool_max" => {
            let v: u32 = value
                .parse()
                .map_err(|_| format!("invalid value for {name}: {value}"))?;
            config.threads.thread_pool_max = v;
            Ok(())
        }
        "thread_pools" => {
            let v: u32 = value
                .parse()
                .map_err(|_| format!("invalid value for {name}: {value}"))?;
            config.threads.pool_count = v;
            Ok(())
        }
        "connect_timeout" => {
            config.timeouts.connect_timeout = value.to_string();
            Ok(())
        }
        "first_byte_timeout" => {
            config.timeouts.first_byte_timeout = value.to_string();
            Ok(())
        }
        "between_bytes_timeout" => {
            config.timeouts.between_bytes_timeout = value.to_string();
            Ok(())
        }
        "send_timeout" => {
            config.timeouts.send_timeout = value.to_string();
            Ok(())
        }
        "pipe_timeout" => {
            config.timeouts.pipe_timeout = value.to_string();
            Ok(())
        }
        "backend_idle_timeout" => {
            config.timeouts.backend_idle_timeout = value.to_string();
            Ok(())
        }
        "http2" => {
            let v: bool = value
                .parse()
                .map_err(|_| format!("invalid boolean for {name}: {value}"))?;
            config.features.http2 = v;
            Ok(())
        }
        "esi_disable_xml_check" => {
            let v: bool = value
                .parse()
                .map_err(|_| format!("invalid boolean for {name}: {value}"))?;
            config.features.esi_disable_xml_check = v;
            Ok(())
        }
        "esi_ignore_https" => {
            let v: bool = value
                .parse()
                .map_err(|_| format!("invalid boolean for {name}: {value}"))?;
            config.features.esi_ignore_https = v;
            Ok(())
        }
        "esi_ignore_other_elements" => {
            let v: bool = value
                .parse()
                .map_err(|_| format!("invalid boolean for {name}: {value}"))?;
            config.features.esi_ignore_other_elements = v;
            Ok(())
        }
        "short_panic" => {
            let v: bool = value
                .parse()
                .map_err(|_| format!("invalid boolean for {name}: {value}"))?;
            config.features.short_panic = v;
            Ok(())
        }
        "no_coredump" => {
            let v: bool = value
                .parse()
                .map_err(|_| format!("invalid boolean for {name}: {value}"))?;
            config.features.no_coredump = v;
            Ok(())
        }
        "admin_listen" => {
            config.admin.listen = value.to_string();
            Ok(())
        }
        "log_size" => {
            config.logging.size = value.to_string();
            Ok(())
        }
        "log_format" => {
            config.logging.format = value.to_string();
            Ok(())
        }
        "hash_type" => {
            config.hash.type_ = value.to_string();
            Ok(())
        }
        _ => Err(format!("Unknown parameter: {name}")),
    }
}

/// List all known parameters and their current values.
fn list_all_params(config: &CacheConfig) -> String {
    let params = [
        ("default_ttl", config.ttl.default_ttl.as_str()),
        ("default_grace", config.ttl.default_grace.as_str()),
        ("default_keep", config.ttl.default_keep.as_str()),
        ("connect_timeout", config.timeouts.connect_timeout.as_str()),
        (
            "first_byte_timeout",
            config.timeouts.first_byte_timeout.as_str(),
        ),
        (
            "between_bytes_timeout",
            config.timeouts.between_bytes_timeout.as_str(),
        ),
        ("send_timeout", config.timeouts.send_timeout.as_str()),
        ("pipe_timeout", config.timeouts.pipe_timeout.as_str()),
        (
            "backend_idle_timeout",
            config.timeouts.backend_idle_timeout.as_str(),
        ),
        ("admin_listen", config.admin.listen.as_str()),
        ("log_size", config.logging.size.as_str()),
        ("log_format", config.logging.format.as_str()),
        ("hash_type", config.hash.type_.as_str()),
    ];

    let owned_params = [
        ("thread_pools", config.threads.pool_count.to_string()),
        (
            "thread_pool_min",
            config.threads.thread_pool_min.to_string(),
        ),
        (
            "thread_pool_max",
            config.threads.thread_pool_max.to_string(),
        ),
        ("http2", config.features.http2.to_string()),
        (
            "esi_disable_xml_check",
            config.features.esi_disable_xml_check.to_string(),
        ),
        (
            "esi_ignore_https",
            config.features.esi_ignore_https.to_string(),
        ),
        (
            "esi_ignore_other_elements",
            config.features.esi_ignore_other_elements.to_string(),
        ),
        ("short_panic", config.features.short_panic.to_string()),
        ("no_coredump", config.features.no_coredump.to_string()),
    ];

    let mut lines: Vec<String> = params
        .iter()
        .map(|(name, val)| format!("  {name:<30} {val}"))
        .collect();

    for (name, val) in &owned_params {
        lines.push(format!("  {name:<30} {val}"));
    }

    lines.sort();
    lines.insert(0, "Runtime parameters:".to_string());
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use rv_cache::CacheEngine;
    use rv_config::CacheConfig;
    use rv_hash::HashSlinger;
    use rv_hash::simple::SimpleListHash;
    use rv_log::{LogWriter, RingBuffer};
    use rv_storage::Stevedore;
    use rv_storage::malloc::MallocStevedore;

    fn make_context() -> AdminContext {
        let config = CacheConfig::default();
        let hash: Arc<dyn HashSlinger> = Arc::new(SimpleListHash::new());
        let storage: Arc<dyn Stevedore> = Arc::new(MallocStevedore::new("test", 256 * 1024 * 1024));
        let ringbuf = Arc::new(RingBuffer::new(1024));
        let log = Arc::new(LogWriter::new(ringbuf));
        let cache = Arc::new(CacheEngine::new(
            config.clone(),
            hash,
            storage,
            Arc::clone(&log),
        ));
        AdminContext::new(cache, log, Arc::new(config))
    }

    const VALID_VCL: &str = r#"vcl 4.0; backend default { .host = "127.0.0.1"; .port = "8080"; }"#;

    #[test]
    fn handle_ping_returns_pong() {
        let ctx = make_context();
        let resp = handle_command(&ctx, CliCommand::Ping);
        assert_eq!(resp.status, CliStatus::Ok);
        assert!(resp.body.starts_with("PONG "));
    }

    #[test]
    fn handle_status_returns_uptime() {
        let ctx = make_context();
        let resp = handle_command(&ctx, CliCommand::Status);
        assert_eq!(resp.status, CliStatus::Ok);
        assert!(resp.body.contains("Uptime:"));
        assert!(resp.body.contains("Objects:"));
    }

    #[test]
    fn handle_help_lists_commands() {
        let ctx = make_context();
        let resp = handle_command(&ctx, CliCommand::Help);
        assert_eq!(resp.status, CliStatus::Ok);
        assert!(resp.body.contains("ping"));
        assert!(resp.body.contains("status"));
        assert!(resp.body.contains("vcl.load"));
        assert!(resp.body.contains("ban"));
        assert!(resp.body.contains("param.show"));
        assert!(resp.body.contains("backend.list"));
    }

    #[test]
    fn handle_ban_adds_and_lists() {
        let ctx = make_context();

        // Initially no bans.
        let resp = handle_command(&ctx, CliCommand::BanList);
        assert_eq!(resp.status, CliStatus::Ok);
        assert!(resp.body.contains("No active bans"));

        // Add a ban.
        let resp = handle_command(
            &ctx,
            CliCommand::Ban {
                expression: "req.url ~ ^/images/".to_string(),
            },
        );
        assert_eq!(resp.status, CliStatus::Ok);
        assert!(resp.body.contains("Ban added"));

        // Now list should show it.
        let resp = handle_command(&ctx, CliCommand::BanList);
        assert_eq!(resp.status, CliStatus::Ok);
        assert!(resp.body.contains("1 active ban(s)"));
        assert!(resp.body.contains("req.url ~ ^/images/"));
    }

    #[test]
    fn handle_ban_invalid_expression() {
        let ctx = make_context();
        let resp = handle_command(
            &ctx,
            CliCommand::Ban {
                expression: "invalid expression".to_string(),
            },
        );
        assert_eq!(resp.status, CliStatus::Error);
        assert!(resp.body.contains("Failed to add ban"));
    }

    #[test]
    fn handle_param_show_all() {
        let ctx = make_context();
        let resp = handle_command(&ctx, CliCommand::ParamShow { param: None });
        assert_eq!(resp.status, CliStatus::Ok);
        assert!(resp.body.contains("default_ttl"));
        assert!(resp.body.contains("120s"));
        assert!(resp.body.contains("thread_pool_min"));
    }

    #[test]
    fn handle_param_show_specific() {
        let ctx = make_context();
        let resp = handle_command(
            &ctx,
            CliCommand::ParamShow {
                param: Some("default_ttl".to_string()),
            },
        );
        assert_eq!(resp.status, CliStatus::Ok);
        assert!(resp.body.contains("default_ttl: 120s"));
    }

    #[test]
    fn handle_param_show_unknown() {
        let ctx = make_context();
        let resp = handle_command(
            &ctx,
            CliCommand::ParamShow {
                param: Some("nonexistent_param".to_string()),
            },
        );
        assert_eq!(resp.status, CliStatus::Error);
        assert!(resp.body.contains("Unknown parameter"));
    }

    // -------------------------------------------------------------------
    // VCL management tests
    // -------------------------------------------------------------------

    #[test]
    fn vcl_load_valid_source() {
        let ctx = make_context();
        let resp = handle_command(
            &ctx,
            CliCommand::VclLoad {
                name: "test1".to_string(),
                source: VALID_VCL.to_string(),
            },
        );
        assert_eq!(resp.status, CliStatus::Ok);
        assert!(resp.body.contains("compiled and loaded"));
    }

    #[test]
    fn vcl_load_invalid_source() {
        let ctx = make_context();
        let resp = handle_command(
            &ctx,
            CliCommand::VclLoad {
                name: "bad".to_string(),
                source: "not valid VCL {{{{".to_string(),
            },
        );
        assert_eq!(resp.status, CliStatus::Error);
        assert!(resp.body.contains("Failed to load VCL"));
    }

    #[test]
    fn vcl_use_activates_program() {
        let ctx = make_context();

        // Load first
        handle_command(
            &ctx,
            CliCommand::VclLoad {
                name: "v1".to_string(),
                source: VALID_VCL.to_string(),
            },
        );

        let resp = handle_command(
            &ctx,
            CliCommand::VclUse {
                name: "v1".to_string(),
            },
        );
        assert_eq!(resp.status, CliStatus::Ok);
        assert!(resp.body.contains("now active"));
    }

    #[test]
    fn vcl_use_nonexistent() {
        let ctx = make_context();
        let resp = handle_command(
            &ctx,
            CliCommand::VclUse {
                name: "nope".to_string(),
            },
        );
        assert_eq!(resp.status, CliStatus::Error);
        assert!(resp.body.contains("not found"));
    }

    #[test]
    fn vcl_list_empty() {
        let ctx = make_context();
        let resp = handle_command(&ctx, CliCommand::VclList);
        assert_eq!(resp.status, CliStatus::Ok);
        assert!(resp.body.contains("No VCL programs loaded"));
    }

    #[test]
    fn vcl_list_with_entries() {
        let ctx = make_context();
        handle_command(
            &ctx,
            CliCommand::VclLoad {
                name: "v1".to_string(),
                source: VALID_VCL.to_string(),
            },
        );
        handle_command(
            &ctx,
            CliCommand::VclUse {
                name: "v1".to_string(),
            },
        );

        let resp = handle_command(&ctx, CliCommand::VclList);
        assert_eq!(resp.status, CliStatus::Ok);
        assert!(resp.body.contains("v1"));
        assert!(resp.body.contains("active"));
    }

    #[test]
    fn vcl_discard_available() {
        let ctx = make_context();
        handle_command(
            &ctx,
            CliCommand::VclLoad {
                name: "v1".to_string(),
                source: VALID_VCL.to_string(),
            },
        );

        let resp = handle_command(
            &ctx,
            CliCommand::VclDiscard {
                name: "v1".to_string(),
            },
        );
        assert_eq!(resp.status, CliStatus::Ok);
        assert!(resp.body.contains("discarded"));
    }

    #[test]
    fn vcl_discard_active_fails() {
        let ctx = make_context();
        handle_command(
            &ctx,
            CliCommand::VclLoad {
                name: "v1".to_string(),
                source: VALID_VCL.to_string(),
            },
        );
        handle_command(
            &ctx,
            CliCommand::VclUse {
                name: "v1".to_string(),
            },
        );

        let resp = handle_command(
            &ctx,
            CliCommand::VclDiscard {
                name: "v1".to_string(),
            },
        );
        assert_eq!(resp.status, CliStatus::Error);
        assert!(resp.body.contains("active"));
    }

    // -------------------------------------------------------------------
    // param.set tests
    // -------------------------------------------------------------------

    #[test]
    fn param_set_valid_string_param() {
        let ctx = make_context();
        let resp = handle_command(
            &ctx,
            CliCommand::ParamSet {
                param: "default_ttl".to_string(),
                value: "300s".to_string(),
            },
        );
        assert_eq!(resp.status, CliStatus::Ok);
        assert!(resp.body.contains("default_ttl set to 300s"));

        // Verify it was updated
        let resp = handle_command(
            &ctx,
            CliCommand::ParamShow {
                param: Some("default_ttl".to_string()),
            },
        );
        assert!(resp.body.contains("300s"));
    }

    #[test]
    fn param_set_valid_numeric_param() {
        let ctx = make_context();
        let resp = handle_command(
            &ctx,
            CliCommand::ParamSet {
                param: "thread_pool_min".to_string(),
                value: "200".to_string(),
            },
        );
        assert_eq!(resp.status, CliStatus::Ok);

        let resp = handle_command(
            &ctx,
            CliCommand::ParamShow {
                param: Some("thread_pool_min".to_string()),
            },
        );
        assert!(resp.body.contains("200"));
    }

    #[test]
    fn param_set_invalid_numeric() {
        let ctx = make_context();
        let resp = handle_command(
            &ctx,
            CliCommand::ParamSet {
                param: "thread_pool_min".to_string(),
                value: "not_a_number".to_string(),
            },
        );
        assert_eq!(resp.status, CliStatus::Error);
    }

    #[test]
    fn param_set_unknown_param() {
        let ctx = make_context();
        let resp = handle_command(
            &ctx,
            CliCommand::ParamSet {
                param: "nonexistent".to_string(),
                value: "foo".to_string(),
            },
        );
        assert_eq!(resp.status, CliStatus::Error);
        assert!(resp.body.contains("Unknown parameter"));
    }

    #[test]
    fn param_set_boolean_param() {
        let ctx = make_context();
        let resp = handle_command(
            &ctx,
            CliCommand::ParamSet {
                param: "http2".to_string(),
                value: "true".to_string(),
            },
        );
        assert_eq!(resp.status, CliStatus::Ok);

        let resp = handle_command(
            &ctx,
            CliCommand::ParamShow {
                param: Some("http2".to_string()),
            },
        );
        assert!(resp.body.contains("true"));
    }

    // -------------------------------------------------------------------
    // backend.list / backend.set_health tests
    // -------------------------------------------------------------------

    #[test]
    fn backend_list_returns_no_backends() {
        let ctx = make_context();
        let resp = handle_command(&ctx, CliCommand::BackendList);
        assert_eq!(resp.status, CliStatus::Ok);
        assert!(resp.body.contains("No backends configured"));
    }

    #[test]
    fn backend_set_health_returns_not_found() {
        let ctx = make_context();
        let resp = handle_command(
            &ctx,
            CliCommand::BackendSetHealth {
                backend: "web01".to_string(),
                health: "healthy".to_string(),
            },
        );
        assert_eq!(resp.status, CliStatus::Error);
        assert!(resp.body.contains("not found"));
    }

    // -------------------------------------------------------------------
    // start / stop tests
    // -------------------------------------------------------------------

    #[test]
    fn stop_sets_running_false() {
        let ctx = make_context();
        assert!(ctx.running.load(Ordering::Relaxed));

        let resp = handle_command(&ctx, CliCommand::Stop);
        assert_eq!(resp.status, CliStatus::Ok);
        assert!(resp.body.contains("stopped"));
        assert!(!ctx.running.load(Ordering::Relaxed));
    }

    #[test]
    fn stop_when_already_stopped() {
        let ctx = make_context();
        ctx.running.store(false, Ordering::Relaxed);

        let resp = handle_command(&ctx, CliCommand::Stop);
        assert_eq!(resp.status, CliStatus::Error);
        assert!(resp.body.contains("already stopped"));
    }

    #[test]
    fn start_sets_running_true() {
        let ctx = make_context();
        ctx.running.store(false, Ordering::Relaxed);

        let resp = handle_command(&ctx, CliCommand::Start);
        assert_eq!(resp.status, CliStatus::Ok);
        assert!(resp.body.contains("started"));
        assert!(ctx.running.load(Ordering::Relaxed));
    }

    #[test]
    fn start_when_already_running() {
        let ctx = make_context();
        let resp = handle_command(&ctx, CliCommand::Start);
        assert_eq!(resp.status, CliStatus::Error);
        assert!(resp.body.contains("already running"));
    }

    #[test]
    fn status_shows_running_state() {
        let ctx = make_context();
        let resp = handle_command(&ctx, CliCommand::Status);
        assert!(resp.body.contains("running"));

        ctx.running.store(false, Ordering::Relaxed);
        let resp = handle_command(&ctx, CliCommand::Status);
        assert!(resp.body.contains("stopped"));
    }
}

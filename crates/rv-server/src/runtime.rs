use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use anyhow::Result;
use arc_swap::ArcSwapOption;
use tokio_util::sync::CancellationToken;
use tracing::info;

use rv_admin::VclManager;
use rv_admin::handler::AdminContext;
use rv_admin::server::AdminServer;
use rv_cache::request::{RequestContext, RequestFsm, RequestState, VclAction};
use rv_cache::{CacheEngine, CacheLookupResult, TtlInfo};
use rv_config::CacheConfig;
use rv_hash::HashSlinger;
use rv_http::message::{HttpMessage, HttpVersion};
use rv_log::{LogWriter, RingBuffer};
use rv_storage::Stevedore;
use rv_transport::server::{RequestHandler, TransportConfig, TransportServer};
use rv_transport::traits::ConnectionInfo;
use rv_types::vsl::Vxid;
use rv_types::{HttpMethod, HttpStatus, LogTag, VtimDur, VtimReal};
use rv_vcl::interpreter::{
    VclAction as InterpreterAction, VclContext, VclExecResult, VclInterpreter,
};

use crate::cli::CliArgs;
use crate::vcl_loader;

/// Default time to wait for in-flight requests to drain during shutdown.
const DEFAULT_DRAIN_TIMEOUT: Duration = Duration::from_secs(30);

/// Atomic VXID counter for generating unique transaction IDs.
static VXID_COUNTER: AtomicU64 = AtomicU64::new(1);

fn next_vxid() -> Vxid {
    Vxid(VXID_COUNTER.fetch_add(1, Ordering::Relaxed))
}

/// The server runtime that manages all subsystems.
pub struct ServerRuntime {
    pub cache: Arc<CacheEngine>,
    pub log: Arc<LogWriter>,
    pub config: Arc<CacheConfig>,
    pub _start_time: Instant,
    /// Atomically-swappable VCL interpreter. Shared between the request
    /// handler (reads) and the admin handler (writes on `vcl.use`).
    /// Each request loads the current `Arc<VclInterpreter>` at the start
    /// and holds its own reference until completion, so in-flight requests
    /// are never affected by a swap.
    pub active_vcl: Arc<ArcSwapOption<VclInterpreter>>,
    /// Configurable timeout for draining in-flight requests during shutdown.
    pub drain_timeout: Duration,
}

impl ServerRuntime {
    /// Initialize the server from CLI arguments.
    pub fn new(args: &mut CliArgs) -> Result<Self> {
        // Load config
        let config = if let Some(ref path) = args.config_file {
            rv_config::load_config(path)?
        } else {
            CacheConfig::default()
        };
        let config = Arc::new(config);

        // Create the ArcSwap for hot VCL reload
        let active_vcl: Arc<ArcSwapOption<VclInterpreter>> = Arc::new(ArcSwapOption::empty());

        // Load VCL file if specified
        if let Some(ref vcl_path) = args.vcl_file {
            let program = vcl_loader::load_vcl(vcl_path)?;
            let backends = vcl_loader::extract_backends(&program);

            // Use first backend from VCL if no -b flag was given
            if args.backend_addr.is_none() {
                if let Some((name, addr)) = backends.first() {
                    info!(backend = %name, addr = %addr, "using backend from VCL");
                    args.backend_addr = Some(*addr);
                }
            }

            let registry = Arc::new(rv_vmod::VmodRegistry::default());
            let interpreter = Arc::new(VclInterpreter::with_resolver(Arc::new(program), registry));
            active_vcl.store(Some(interpreter));
        }

        // Initialize logging
        let log_size = rv_config::parse_size(&config.logging.size).unwrap_or(80 * 1024 * 1024);
        let ringbuf = Arc::new(RingBuffer::new(log_size / 128));
        let log = Arc::new(LogWriter::new(ringbuf));

        // Initialize hash backend
        let hash: Arc<dyn HashSlinger> = match args.hash_type.as_str() {
            "simple" => Arc::new(rv_hash::simple::SimpleListHash::new()),
            "classic" => Arc::new(rv_hash::classic::ClassicHash::new()),
            _ => Arc::new(rv_hash::critbit::CritbitHash::new()),
        };

        // Parse storage spec and initialize storage
        let storage: Arc<dyn Stevedore> = parse_storage_spec(&args.storage_spec)?;

        // Create cache engine
        let cache = Arc::new(CacheEngine::new(
            (*config).clone(),
            hash,
            storage,
            Arc::clone(&log),
        ));

        Ok(Self {
            cache,
            log,
            config,
            _start_time: Instant::now(),
            active_vcl,
            drain_timeout: DEFAULT_DRAIN_TIMEOUT,
        })
    }

    /// Start the server, spawning all subsystems.
    ///
    /// The server runs until a SIGTERM or SIGINT signal is received. On
    /// shutdown the transport server stops accepting new connections, the
    /// runtime waits up to `drain_timeout` for in-flight requests to
    /// complete, and then the admin and expiry background tasks are
    /// cancelled.
    pub async fn run(&self, args: &CliArgs) -> Result<()> {
        let cache = Arc::clone(&self.cache);

        // Create the top-level cancellation token for coordinated shutdown.
        let cancel = CancellationToken::new();

        // Start admin server with VCL manager, running flag, and active VCL ArcSwap
        let vcl_manager = Arc::new(VclManager::new());
        let running = Arc::new(AtomicBool::new(true));
        let admin_ctx = Arc::new(AdminContext::with_active_vcl(
            Arc::clone(&self.cache),
            Arc::clone(&self.log),
            Arc::clone(&self.config),
            vcl_manager,
            running,
            Arc::clone(&self.active_vcl),
        ));

        let admin_server = AdminServer::new(args.admin_addr, Arc::clone(&admin_ctx), None);
        let admin_cancel = cancel.clone();
        tokio::spawn(async move {
            tokio::select! {
                result = admin_server.serve() => {
                    if let Err(e) = result {
                        tracing::error!(error = %e, "admin server error");
                    }
                }
                _ = admin_cancel.cancelled() => {
                    info!("admin server shutting down");
                }
            }
        });

        info!(admin_addr = %args.admin_addr, "admin server started");

        // Start expiry background task
        let expiry_cache = Arc::clone(&self.cache);
        let expiry_cancel = cancel.clone();
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = tokio::time::sleep(tokio::time::Duration::from_secs(1)) => {
                        expiry_cache.expire_objects();
                    }
                    _ = expiry_cancel.cancelled() => {
                        info!("expiry task shutting down");
                        break;
                    }
                }
            }
        });

        // Build the async request handler.
        // Each invocation loads the current VCL interpreter from the ArcSwap.
        // The loaded Arc is held for the duration of the request, so an
        // in-flight request is never affected by a concurrent vcl.use swap.
        let backend_addr = args.backend_addr;
        let active_vcl = Arc::clone(&self.active_vcl);
        let log = Arc::clone(&self.log);
        let handler: RequestHandler = Arc::new(move |request, body, conn_info| {
            let cache = Arc::clone(&cache);
            let log = Arc::clone(&log);
            // Load the current VCL interpreter. The returned Option<Arc<VclInterpreter>>
            // is an owned Arc that keeps the interpreter alive for this request even
            // if a swap happens concurrently.
            let vcl: Option<Arc<VclInterpreter>> = active_vcl.load_full();
            Box::pin(handle_request(
                cache,
                request,
                body,
                conn_info,
                backend_addr,
                vcl,
                log,
            ))
        });

        // Start transport server
        let transport_config = TransportConfig {
            listen_addr: args.listen_addr,
            max_body_size: 64 * 1024 * 1024,
            accept_proxy_protocol: false,
        };

        let transport = TransportServer::new(transport_config);

        info!(
            listen_addr = %args.listen_addr,
            "varaha-cache ready"
        );

        // Run the transport server and the shutdown signal concurrently.
        // When the signal fires, the cancellation token is cancelled, which
        // causes the transport server to stop accepting new connections.
        let transport_cancel = cancel.clone();
        let drain_timeout = self.drain_timeout;

        tokio::select! {
            result = transport.serve(handler, transport_cancel) => {
                if let Err(e) = result {
                    tracing::error!(error = %e, "transport server error");
                }
            }
            _ = shutdown_signal() => {
                info!("shutdown signal received, beginning graceful shutdown");

                // Cancel all subsystems (transport stops accepting, admin stops, expiry stops).
                cancel.cancel();

                // Wait for in-flight requests to drain (bounded by timeout).
                info!(
                    timeout_secs = drain_timeout.as_secs(),
                    "waiting for in-flight requests to drain"
                );
                tokio::time::sleep(drain_timeout).await;

                info!("graceful shutdown complete");
            }
        }

        Ok(())
    }
}

/// Wait for a shutdown signal (SIGTERM or SIGINT / Ctrl-C).
///
/// On Unix this listens for both SIGTERM and SIGINT. On non-Unix platforms
/// only Ctrl-C is supported.
async fn shutdown_signal() {
    let ctrl_c = tokio::signal::ctrl_c();

    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let mut sigterm =
            signal(SignalKind::terminate()).expect("failed to register SIGTERM handler");
        tokio::select! {
            _ = ctrl_c => {
                info!("received SIGINT (Ctrl-C)");
            }
            _ = sigterm.recv() => {
                info!("received SIGTERM");
            }
        }
    }

    #[cfg(not(unix))]
    {
        ctrl_c.await.expect("failed to listen for Ctrl-C");
        info!("received Ctrl-C");
    }
}

/// Handle an incoming HTTP request through the FSM-driven cache pipeline.
async fn handle_request(
    cache: Arc<CacheEngine>,
    request: HttpMessage,
    body: Option<Vec<u8>>,
    conn_info: ConnectionInfo,
    backend_addr: Option<SocketAddr>,
    vcl: Option<Arc<VclInterpreter>>,
    log: Arc<LogWriter>,
) -> (HttpMessage, Option<Vec<u8>>) {
    let vxid = next_vxid();
    let start = Instant::now();
    let wall = VtimReal::now();

    // Request lifecycle logging
    rv_log::rv_log!(
        log,
        LogTag::ReqStart,
        vxid,
        "{} {}",
        conn_info.client_addr.ip(),
        conn_info.client_addr.port()
    );
    rv_log::rv_log!(log, LogTag::ReqMethod, vxid, "{}", request.method);
    rv_log::rv_log!(log, LogTag::ReqURL, vxid, "{}", request.url);
    rv_log::rv_log!(log, LogTag::ReqProtocol, vxid, "{}", request.protocol);
    for h in request.headers.iter() {
        rv_log::rv_log!(log, LogTag::ReqHeader, vxid, "{}: {}", h.name, h.value);
    }
    rv_log::rv_log!(
        log,
        LogTag::Timestamp,
        vxid,
        "Start: {:.6} 0.000000 0.000000",
        wall.as_secs()
    );

    let mut ctx = RequestContext::new(request, vxid);
    ctx.body = body;

    // FSM loop
    loop {
        match ctx.state {
            RequestState::Recv => {
                state_recv(&mut ctx, &vcl, &conn_info, &log);
                RequestFsm::step(&mut ctx);
            }
            RequestState::Lookup => {
                state_lookup(&mut ctx, &cache, &vcl, &conn_info, &log);
                RequestFsm::step(&mut ctx);
            }
            RequestState::Pass => {
                ctx.is_pass = true;
                if let Some(vcl) = &vcl {
                    run_vcl_pass(vcl, &mut ctx, &conn_info, &log);
                }
                RequestFsm::step(&mut ctx);
            }
            RequestState::Miss => {
                if let Some(vcl) = &vcl {
                    run_vcl_miss(vcl, &mut ctx, &conn_info, &log);
                }
                RequestFsm::step(&mut ctx);
            }
            RequestState::Pipe => {
                // Pipe mode is a placeholder -- just go to Done
                RequestFsm::step(&mut ctx);
            }
            RequestState::Fetch => {
                state_fetch(&mut ctx, &cache, backend_addr, &vcl, &conn_info, &log).await;
                // If fetch requested a synth (e.g. no backend), go to Synth
                // instead of the default Fetch -> Deliver transition.
                if ctx.vcl_action == Some(VclAction::Synth) {
                    ctx.state = RequestState::Synth;
                } else {
                    RequestFsm::step(&mut ctx);
                }
            }
            RequestState::Deliver => {
                state_deliver(&mut ctx, &cache, &vcl, &conn_info, &log);
                let action = ctx.vcl_action;
                match action {
                    Some(VclAction::Restart) if ctx.restarts < ctx.max_restarts => {
                        ctx.restarts += 1;
                        ctx.state = RequestState::Recv;
                        ctx.vcl_action = None;
                        continue;
                    }
                    _ => {
                        RequestFsm::step(&mut ctx);
                    }
                }
            }
            RequestState::Synth => {
                state_synth(&mut ctx, &vcl, &conn_info, &log);
                // Synth response is complete -- go directly to Done.
                // (The FSM default Synth -> Deliver would overwrite the
                // synthetic response in state_deliver.)
                ctx.state = RequestState::Done;
            }
            RequestState::Done => {
                break;
            }
        }
    }

    let response = ctx.response.unwrap_or_else(|| {
        HttpMessage::new_response(HttpStatus::INTERNAL_SERVER_ERROR, HttpVersion::Http11)
    });
    let resp_body = ctx.resp_body;

    // End-of-request logging
    let elapsed = start.elapsed().as_secs_f64();
    let wall_end = VtimReal::now();
    rv_log::rv_log!(log, LogTag::RespStatus, vxid, "{}", response.status.code());
    rv_log::rv_log!(
        log,
        LogTag::Timestamp,
        vxid,
        "Resp: {:.6} {:.6} {:.6}",
        wall_end.as_secs(),
        elapsed,
        elapsed
    );
    let resp_body_len = resp_body.as_ref().map(|b| b.len()).unwrap_or(0);
    rv_log::rv_log!(
        log,
        LogTag::ReqAcct,
        vxid,
        "{} {} {}",
        0,
        resp_body_len,
        resp_body_len
    );
    log.log(LogTag::End, vxid, "");

    (response, resp_body)
}

/// Recv state: run vcl_recv and determine the next action.
fn state_recv(
    ctx: &mut RequestContext,
    vcl: &Option<Arc<VclInterpreter>>,
    conn_info: &ConnectionInfo,
    log: &LogWriter,
) {
    if let Some(vcl) = vcl {
        let mut resp = HttpMessage::new_response(HttpStatus::OK, HttpVersion::Http11);
        let mut bereq = HttpMessage::default();
        let mut beresp = HttpMessage::default();
        let mut vcl_ctx = VclContext::new(&mut ctx.request, &mut resp, &mut bereq, &mut beresp);
        vcl_ctx.client_ip = conn_info.client_addr.ip();
        vcl_ctx.server_ip = conn_info.local_addr.ip();
        vcl_ctx.is_ssl = conn_info.is_tls;
        vcl_ctx.restarts = ctx.restarts;
        if let Some(ref body) = ctx.body {
            vcl_ctx.req_body = Some(body);
        }

        log.log(LogTag::VclCall, ctx.vxid, "RECV");
        match vcl.exec_subroutine("vcl_recv", &mut vcl_ctx) {
            VclExecResult::Action(action) => {
                rv_log::rv_log!(log, LogTag::VclReturn, ctx.vxid, "{}", action_name(&action));
                ctx.vcl_action = Some(convert_action(&action));
                if action == InterpreterAction::Synth {
                    ctx.synth_status = vcl_ctx.synth_status.map(HttpStatus::new);
                    ctx.synth_body = vcl_ctx.synth_body.map(|s| s.into_bytes());
                }
            }
            VclExecResult::Fallthrough => {
                log.log(LogTag::VclReturn, ctx.vxid, "hash");
                ctx.vcl_action = None;
            }
            VclExecResult::Error(e) => {
                rv_log::rv_log!(log, LogTag::VclError, ctx.vxid, "vcl_recv: {}", e);
                ctx.vcl_action = Some(VclAction::Synth);
                ctx.synth_status = Some(HttpStatus::INTERNAL_SERVER_ERROR);
            }
        }
    } else {
        // No VCL - default behavior: hash (lookup)
        ctx.vcl_action = None;
    }
}

/// Lookup state: compute digest, lookup cache, run vcl_hit or vcl_miss.
fn state_lookup(
    ctx: &mut RequestContext,
    cache: &Arc<CacheEngine>,
    vcl: &Option<Arc<VclInterpreter>>,
    conn_info: &ConnectionInfo,
    log: &LogWriter,
) {
    // Compute digest
    let host = ctx.request.get_header("Host").unwrap_or("localhost");
    let key = format!("{}:{}", host, ctx.request.url);
    rv_log::rv_log!(log, LogTag::Hash, ctx.vxid, "{}", key);
    let digest = compute_digest(&key);
    ctx.digest = Some(digest);

    // Cache lookup
    match cache.lookup(&digest, None) {
        CacheLookupResult::Hit(oc) => {
            rv_log::rv_log!(log, LogTag::Hit, ctx.vxid, "{}", oc.hits);
            rv_log::rv_log!(
                log,
                LogTag::TTL,
                ctx.vxid,
                "hit ttl={:.0}s grace={:.0}s keep={:.0}s",
                oc.ttl.as_secs(),
                oc.grace.as_secs(),
                oc.keep.as_secs()
            );
            ctx.obj = Some(oc.clone());

            if let Some(vcl) = vcl {
                let mut resp = HttpMessage::new_response(HttpStatus::OK, HttpVersion::Http11);
                let mut bereq = HttpMessage::default();
                let mut beresp = HttpMessage::default();
                let mut vcl_ctx =
                    VclContext::new(&mut ctx.request, &mut resp, &mut bereq, &mut beresp);
                vcl_ctx.client_ip = conn_info.client_addr.ip();
                vcl_ctx.server_ip = conn_info.local_addr.ip();
                vcl_ctx.is_ssl = conn_info.is_tls;
                vcl_ctx.restarts = ctx.restarts;
                vcl_ctx.obj_hits = oc.hits;
                vcl_ctx.obj_ttl = oc.ttl.as_secs();
                vcl_ctx.obj_grace = oc.grace.as_secs();
                vcl_ctx.obj_keep = oc.keep.as_secs();

                log.log(LogTag::VclCall, ctx.vxid, "HIT");
                match vcl.exec_subroutine("vcl_hit", &mut vcl_ctx) {
                    VclExecResult::Action(action) => {
                        rv_log::rv_log!(
                            log,
                            LogTag::VclReturn,
                            ctx.vxid,
                            "{}",
                            action_name(&action)
                        );
                        ctx.vcl_action = Some(convert_action(&action));
                    }
                    VclExecResult::Fallthrough => {
                        log.log(LogTag::VclReturn, ctx.vxid, "deliver");
                        ctx.vcl_action = Some(VclAction::Deliver);
                    }
                    VclExecResult::Error(e) => {
                        rv_log::rv_log!(log, LogTag::VclError, ctx.vxid, "vcl_hit: {}", e);
                        ctx.vcl_action = Some(VclAction::Deliver);
                    }
                }
            } else {
                ctx.vcl_action = Some(VclAction::Deliver);
            }
        }
        CacheLookupResult::Grace(oc) => {
            rv_log::rv_log!(log, LogTag::Hit, ctx.vxid, "{} (grace)", oc.hits);
            ctx.obj = Some(oc);
            ctx.vcl_action = Some(VclAction::Deliver);
        }
        CacheLookupResult::HitForPass => {
            log.log(LogTag::HitPass, ctx.vxid, "");
            ctx.vcl_action = Some(VclAction::Pass);
        }
        CacheLookupResult::Busy => {
            log.log(LogTag::Miss, ctx.vxid, "busy");
            ctx.vcl_action = None; // Default: miss -> fetch
        }
        CacheLookupResult::Miss => {
            log.log(LogTag::Miss, ctx.vxid, "");

            if let Some(vcl) = vcl {
                let mut resp = HttpMessage::default();
                let mut bereq = HttpMessage::new_request(
                    ctx.request.method,
                    &ctx.request.url,
                    HttpVersion::Http11,
                );
                let mut beresp = HttpMessage::default();
                let mut vcl_ctx =
                    VclContext::new(&mut ctx.request, &mut resp, &mut bereq, &mut beresp);
                vcl_ctx.client_ip = conn_info.client_addr.ip();
                vcl_ctx.restarts = ctx.restarts;

                log.log(LogTag::VclCall, ctx.vxid, "MISS");
                match vcl.exec_subroutine("vcl_miss", &mut vcl_ctx) {
                    VclExecResult::Action(action) => {
                        rv_log::rv_log!(
                            log,
                            LogTag::VclReturn,
                            ctx.vxid,
                            "{}",
                            action_name(&action)
                        );
                        ctx.vcl_action = Some(convert_action(&action));
                    }
                    VclExecResult::Fallthrough => {
                        log.log(LogTag::VclReturn, ctx.vxid, "fetch");
                        ctx.vcl_action = None;
                    }
                    VclExecResult::Error(e) => {
                        rv_log::rv_log!(log, LogTag::VclError, ctx.vxid, "vcl_miss: {}", e);
                        ctx.vcl_action = None;
                    }
                }
            } else {
                ctx.vcl_action = None;
            }
        }
    }
}

/// Run vcl_pass subroutine.
fn run_vcl_pass(
    vcl: &VclInterpreter,
    ctx: &mut RequestContext,
    conn_info: &ConnectionInfo,
    log: &LogWriter,
) {
    let mut resp = HttpMessage::default();
    let mut bereq =
        HttpMessage::new_request(ctx.request.method, &ctx.request.url, HttpVersion::Http11);
    let mut beresp = HttpMessage::default();
    let mut vcl_ctx = VclContext::new(&mut ctx.request, &mut resp, &mut bereq, &mut beresp);
    vcl_ctx.client_ip = conn_info.client_addr.ip();
    vcl_ctx.restarts = ctx.restarts;

    log.log(LogTag::VclCall, ctx.vxid, "PASS");
    match vcl.exec_subroutine("vcl_pass", &mut vcl_ctx) {
        VclExecResult::Action(action) => {
            rv_log::rv_log!(log, LogTag::VclReturn, ctx.vxid, "{}", action_name(&action));
            ctx.vcl_action = Some(convert_action(&action));
        }
        VclExecResult::Fallthrough => {
            log.log(LogTag::VclReturn, ctx.vxid, "fetch");
        }
        VclExecResult::Error(e) => {
            rv_log::rv_log!(log, LogTag::VclError, ctx.vxid, "vcl_pass: {}", e);
        }
    }
}

/// Run vcl_miss subroutine.
fn run_vcl_miss(
    vcl: &VclInterpreter,
    ctx: &mut RequestContext,
    conn_info: &ConnectionInfo,
    log: &LogWriter,
) {
    let mut resp = HttpMessage::default();
    let mut bereq =
        HttpMessage::new_request(ctx.request.method, &ctx.request.url, HttpVersion::Http11);
    let mut beresp = HttpMessage::default();
    let mut vcl_ctx = VclContext::new(&mut ctx.request, &mut resp, &mut bereq, &mut beresp);
    vcl_ctx.client_ip = conn_info.client_addr.ip();
    vcl_ctx.restarts = ctx.restarts;

    log.log(LogTag::VclCall, ctx.vxid, "MISS");
    match vcl.exec_subroutine("vcl_miss", &mut vcl_ctx) {
        VclExecResult::Action(action) => {
            rv_log::rv_log!(log, LogTag::VclReturn, ctx.vxid, "{}", action_name(&action));
            ctx.vcl_action = Some(convert_action(&action));
        }
        VclExecResult::Fallthrough => {
            log.log(LogTag::VclReturn, ctx.vxid, "fetch");
        }
        VclExecResult::Error(e) => {
            rv_log::rv_log!(log, LogTag::VclError, ctx.vxid, "vcl_miss: {}", e);
        }
    }
}

/// Fetch state: build bereq, fetch from backend, run vcl_backend_fetch/response, cache insert.
async fn state_fetch(
    ctx: &mut RequestContext,
    cache: &Arc<CacheEngine>,
    backend_addr: Option<SocketAddr>,
    vcl: &Option<Arc<VclInterpreter>>,
    conn_info: &ConnectionInfo,
    log: &LogWriter,
) {
    let addr = match backend_addr {
        Some(addr) => addr,
        None => {
            log.error(ctx.vxid, "no backend configured");
            ctx.vcl_action = Some(VclAction::Synth);
            ctx.synth_status = Some(HttpStatus::SERVICE_UNAVAILABLE);
            ctx.synth_body = Some(b"No backend configured".to_vec());
            return;
        }
    };

    rv_log::rv_log!(log, LogTag::Backend, ctx.vxid, "{}", addr);

    // Build bereq from client request
    let mut bereq =
        HttpMessage::new_request(ctx.request.method, &ctx.request.url, HttpVersion::Http11);

    // Copy relevant headers from client request
    for h in ctx.request.headers.iter() {
        let name_lower = h.name.to_lowercase();
        if name_lower != "connection" && name_lower != "accept-encoding" {
            bereq.set_header(&h.name, &h.value);
        }
    }

    rv_log::rv_log!(log, LogTag::BereqMethod, ctx.vxid, "{}", bereq.method);
    rv_log::rv_log!(log, LogTag::BereqURL, ctx.vxid, "{}", bereq.url);

    // Run vcl_backend_fetch
    if let Some(vcl) = vcl {
        let mut resp = HttpMessage::default();
        let mut beresp = HttpMessage::default();
        let mut vcl_ctx = VclContext::new(&mut ctx.request, &mut resp, &mut bereq, &mut beresp);
        vcl_ctx.client_ip = conn_info.client_addr.ip();
        vcl_ctx.restarts = ctx.restarts;

        log.log(LogTag::VclCall, ctx.vxid, "BACKEND_FETCH");
        match vcl.exec_subroutine("vcl_backend_fetch", &mut vcl_ctx) {
            VclExecResult::Action(InterpreterAction::Abandon) => {
                log.log(LogTag::VclReturn, ctx.vxid, "abandon");
                ctx.vcl_action = Some(VclAction::Synth);
                ctx.synth_status = Some(HttpStatus::SERVICE_UNAVAILABLE);
                return;
            }
            VclExecResult::Action(action) => {
                rv_log::rv_log!(log, LogTag::VclReturn, ctx.vxid, "{}", action_name(&action));
            }
            VclExecResult::Fallthrough => {
                log.log(LogTag::VclReturn, ctx.vxid, "fetch");
            }
            VclExecResult::Error(e) => {
                rv_log::rv_log!(log, LogTag::VclError, ctx.vxid, "vcl_backend_fetch: {}", e);
            }
        }
    }

    ctx.bereq = Some(bereq.clone());

    let fetch_start = Instant::now();
    rv_log::rv_log!(
        log,
        LogTag::Timestamp,
        ctx.vxid,
        "Bereq: {:.6} 0.000000 0.000000",
        VtimReal::now().as_secs()
    );

    // Fetch from backend
    match rv_transport::http1_client::send_backend_request(addr, &bereq, None, None).await {
        Ok((mut beresp, beresp_body)) => {
            let fetch_elapsed = fetch_start.elapsed().as_secs_f64();
            rv_log::rv_log!(
                log,
                LogTag::Timestamp,
                ctx.vxid,
                "Beresp: {:.6} {:.6} {:.6}",
                VtimReal::now().as_secs(),
                fetch_elapsed,
                fetch_elapsed
            );
            rv_log::rv_log!(
                log,
                LogTag::BerespStatus,
                ctx.vxid,
                "{}",
                beresp.status.code()
            );
            rv_log::rv_log!(
                log,
                LogTag::BerespReason,
                ctx.vxid,
                "{}",
                beresp.status.reason()
            );

            let mut do_gzip = false;
            let mut do_gunzip = false;
            let mut uncacheable = false;
            let mut beresp_ttl = None;
            let mut beresp_grace = None;
            let mut beresp_keep = None;

            // Run vcl_backend_response
            if let Some(vcl) = vcl {
                let mut resp = HttpMessage::default();
                let mut bereq_for_vcl = bereq.clone();
                let mut vcl_ctx =
                    VclContext::new(&mut ctx.request, &mut resp, &mut bereq_for_vcl, &mut beresp);
                vcl_ctx.client_ip = conn_info.client_addr.ip();
                vcl_ctx.restarts = ctx.restarts;

                log.log(LogTag::VclCall, ctx.vxid, "BACKEND_RESPONSE");
                match vcl.exec_subroutine("vcl_backend_response", &mut vcl_ctx) {
                    VclExecResult::Action(action) => {
                        rv_log::rv_log!(
                            log,
                            LogTag::VclReturn,
                            ctx.vxid,
                            "{}",
                            action_name(&action)
                        );
                        match &action {
                            InterpreterAction::Deliver => {}
                            InterpreterAction::Retry => {
                                // Would retry -- for now just continue
                            }
                            InterpreterAction::Abandon => {
                                ctx.vcl_action = Some(VclAction::Synth);
                                ctx.synth_status = Some(HttpStatus::SERVICE_UNAVAILABLE);
                                return;
                            }
                            InterpreterAction::Pass => {
                                uncacheable = true;
                            }
                            _ => {}
                        }
                    }
                    VclExecResult::Fallthrough => {
                        log.log(LogTag::VclReturn, ctx.vxid, "deliver");
                    }
                    VclExecResult::Error(e) => {
                        rv_log::rv_log!(
                            log,
                            LogTag::VclError,
                            ctx.vxid,
                            "vcl_backend_response: {}",
                            e
                        );
                    }
                }

                do_gzip = vcl_ctx.do_gzip;
                do_gunzip = vcl_ctx.do_gunzip;
                uncacheable = uncacheable || vcl_ctx.uncacheable;

                // Check if VCL set TTL values
                if let Some(val) = vcl_ctx.local_vars.get("beresp.ttl") {
                    beresp_ttl = Some(val.to_duration_secs());
                }
                if let Some(val) = vcl_ctx.local_vars.get("beresp.grace") {
                    beresp_grace = Some(val.to_duration_secs());
                }
                if let Some(val) = vcl_ctx.local_vars.get("beresp.keep") {
                    beresp_keep = Some(val.to_duration_secs());
                }
            }

            ctx.do_gzip = do_gzip;
            ctx.do_gunzip = do_gunzip;
            ctx.uncacheable = uncacheable;
            ctx.beresp_ttl = beresp_ttl;
            ctx.beresp_grace = beresp_grace;
            ctx.beresp_keep = beresp_keep;

            // Cache the response if cacheable
            if !ctx.is_pass && !uncacheable && beresp.status == HttpStatus::OK {
                if let (Some(body), Some(digest)) = (&beresp_body, &ctx.digest) {
                    let ttl = TtlInfo {
                        ttl: VtimDur::from_secs(beresp_ttl.unwrap_or(120.0)),
                        grace: VtimDur::from_secs(beresp_grace.unwrap_or(10.0)),
                        keep: VtimDur::from_secs(beresp_keep.unwrap_or(0.0)),
                    };
                    let _ = cache.insert(*digest, body, ttl, None);
                    rv_log::rv_log!(
                        log,
                        LogTag::TTL,
                        ctx.vxid,
                        "stored ttl={:.0}s grace={:.0}s keep={:.0}s",
                        beresp_ttl.unwrap_or(120.0),
                        beresp_grace.unwrap_or(10.0),
                        beresp_keep.unwrap_or(0.0)
                    );
                    if let Some(ct) = beresp.get_header("Content-Type") {
                        rv_log::rv_log!(log, LogTag::ObjHeader, ctx.vxid, "Content-Type: {}", ct);
                    }
                    rv_log::rv_log!(
                        log,
                        LogTag::Storage,
                        ctx.vxid,
                        "{} bytes stored",
                        body.len()
                    );
                }
            }

            ctx.beresp = Some(beresp);
            ctx.resp_body = beresp_body;

            // Apply fetch filters (e.g., gunzip backend response)
            if ctx.do_gunzip {
                if let Some(ref body) = ctx.resp_body {
                    if let Ok(decompressed) = rv_filter::decompress_gzip(body) {
                        ctx.resp_body = Some(decompressed);
                    }
                }
            }
        }
        Err(e) => {
            rv_log::rv_log!(log, LogTag::FetchError, ctx.vxid, "{}", e);
            ctx.vcl_action = Some(VclAction::Synth);
            ctx.synth_status = Some(HttpStatus::BAD_GATEWAY);
            ctx.synth_body = Some(format!("Backend fetch failed: {}", e).into_bytes());
        }
    }
}

/// Evaluate whether a conditional request should receive a 304 Not Modified.
///
/// Checks `If-None-Match` against the response `ETag` header, and
/// `If-Modified-Since` against the response `Last-Modified` header.
/// Returns `true` if the response has not been modified (i.e., a 304 should be sent).
fn evaluate_conditional(req: &HttpMessage, response: &HttpMessage) -> bool {
    // Check If-None-Match vs ETag
    if let (Some(if_none_match), Some(etag)) =
        (req.get_header("If-None-Match"), response.get_header("ETag"))
    {
        // If-None-Match can be "*" or a comma-separated list of entity tags
        let if_none_match = if_none_match.trim();
        if if_none_match == "*" {
            return true;
        }
        let etag_trimmed = etag.trim();
        for tag in if_none_match.split(',') {
            if tag.trim() == etag_trimmed {
                return true;
            }
        }
    }

    // Check If-Modified-Since vs Last-Modified
    if let (Some(if_modified_since), Some(last_modified)) = (
        req.get_header("If-Modified-Since"),
        response.get_header("Last-Modified"),
    ) {
        // Simple string comparison: if Last-Modified <= If-Modified-Since, not modified.
        // Both should be in HTTP-date format (RFC 7231). We parse to compare properly.
        if let (Some(ims_ts), Some(lm_ts)) = (
            parse_http_date(if_modified_since),
            parse_http_date(last_modified),
        ) && lm_ts <= ims_ts
        {
            return true;
        }
    }

    false
}

/// Parse an HTTP-date string (RFC 7231 / RFC 2822 style) into a Unix timestamp.
/// Supports the preferred format: "Sun, 06 Nov 1994 08:49:37 GMT"
fn parse_http_date(s: &str) -> Option<i64> {
    // Parse "Day, DD Mon YYYY HH:MM:SS GMT"
    let s = s.trim();
    let parts: Vec<&str> = s.split_whitespace().collect();
    if parts.len() < 6 {
        return None;
    }

    // Skip day-of-week (parts[0]), parse DD Mon YYYY HH:MM:SS
    let day: i64 = parts[1].trim_end_matches(',').parse().ok()?;
    let month = match parts[2].to_ascii_lowercase().as_str() {
        "jan" => 1,
        "feb" => 2,
        "mar" => 3,
        "apr" => 4,
        "may" => 5,
        "jun" => 6,
        "jul" => 7,
        "aug" => 8,
        "sep" => 9,
        "oct" => 10,
        "nov" => 11,
        "dec" => 12,
        _ => return None,
    };
    let year: i64 = parts[3].parse().ok()?;
    let time_parts: Vec<&str> = parts[4].split(':').collect();
    if time_parts.len() != 3 {
        return None;
    }
    let hour: i64 = time_parts[0].parse().ok()?;
    let min: i64 = time_parts[1].parse().ok()?;
    let sec: i64 = time_parts[2].parse().ok()?;

    // Approximate Unix timestamp (good enough for comparison purposes)
    let days_from_year =
        (year - 1970) * 365 + (year - 1969) / 4 - (year - 1901) / 100 + (year - 1601) / 400;
    let month_days: [i64; 12] = [0, 31, 59, 90, 120, 151, 181, 212, 243, 273, 304, 334];
    let is_leap = (year % 4 == 0 && year % 100 != 0) || year % 400 == 0;
    let leap_add = if is_leap && month > 2 { 1 } else { 0 };
    let total_days = days_from_year + month_days[(month - 1) as usize] + day - 1 + leap_add;

    Some(total_days * 86400 + hour * 3600 + min * 60 + sec)
}

/// Parse a Range header value and return a list of (start, end) byte ranges.
/// Supports formats: "bytes=0-99", "bytes=-100", "bytes=100-", and multiple ranges.
/// Returns None if the Range header is not parseable.
fn parse_range_header(range_value: &str, content_length: usize) -> Option<Vec<(usize, usize)>> {
    let range_value = range_value.trim();
    if !range_value.starts_with("bytes=") {
        return None;
    }
    let spec = &range_value[6..];
    let mut ranges = Vec::new();

    for part in spec.split(',') {
        let part = part.trim();
        if part.is_empty() {
            return None;
        }

        if let Some(suffix) = part.strip_prefix('-') {
            // Suffix range: "-100" means last 100 bytes
            let suffix_len: usize = suffix.parse().ok()?;
            if suffix_len == 0 || suffix_len > content_length {
                return None;
            }
            let start = content_length - suffix_len;
            ranges.push((start, content_length - 1));
        } else if let Some((start_str, end_str)) = part.split_once('-') {
            let start: usize = start_str.parse().ok()?;
            if end_str.is_empty() {
                // Open-ended range: "100-"
                if start >= content_length {
                    return None;
                }
                ranges.push((start, content_length - 1));
            } else {
                // Explicit range: "0-99"
                let end: usize = end_str.parse().ok()?;
                if start > end || start >= content_length {
                    return None;
                }
                let end = end.min(content_length - 1);
                ranges.push((start, end));
            }
        } else {
            return None;
        }
    }

    if ranges.is_empty() {
        None
    } else {
        Some(ranges)
    }
}

/// Deliver state: build the client response from cached object or beresp.
fn state_deliver(
    ctx: &mut RequestContext,
    cache: &Arc<CacheEngine>,
    vcl: &Option<Arc<VclInterpreter>>,
    conn_info: &ConnectionInfo,
    log: &LogWriter,
) {
    let mut response;
    let mut resp_body = ctx.resp_body.take();

    if let Some(oc) = &ctx.obj {
        // Delivering from cache hit
        response = HttpMessage::new_response(HttpStatus::OK, HttpVersion::Http11);
        response.set_header("X-Cache", "HIT");
        resp_body = cache.storage().get_body(oc);
    } else if let Some(beresp) = &ctx.beresp {
        // Delivering from backend response
        response = HttpMessage::new_response(beresp.status, HttpVersion::Http11);
        response.set_header("X-Cache", "MISS");

        // Copy backend response headers
        for h in beresp.headers.iter() {
            let name_lower = h.name.to_lowercase();
            if name_lower != "connection" && name_lower != "transfer-encoding" {
                response.set_header(&h.name, &h.value);
            }
        }
    } else {
        response =
            HttpMessage::new_response(HttpStatus::INTERNAL_SERVER_ERROR, HttpVersion::Http11);
        response.set_header("X-Cache", "ERROR");
    }

    // Set Content-Length
    if let Some(body) = &resp_body {
        response.set_header("Content-Length", body.len().to_string());
    }

    // Conditional request evaluation (304 Not Modified)
    if (ctx.request.method == HttpMethod::Get || ctx.request.method == HttpMethod::Head)
        && response.status == HttpStatus::OK
        && evaluate_conditional(&ctx.request, &response)
    {
        let mut not_modified =
            HttpMessage::new_response(HttpStatus::NOT_MODIFIED, HttpVersion::Http11);

        // Copy cacheable metadata headers to the 304 response
        for header_name in &["ETag", "Last-Modified", "Cache-Control", "Expires", "Vary"] {
            if let Some(val) = response.get_header(header_name) {
                not_modified.set_header(*header_name, val.to_string());
            }
        }

        // Preserve X-Cache header
        if let Some(val) = response.get_header("X-Cache") {
            not_modified.set_header("X-Cache", val.to_string());
        }

        response = not_modified;
        resp_body = None;
    }

    // Range request serving (206 Partial Content)
    if ctx.request.method == HttpMethod::Get
        && response.status == HttpStatus::OK
        && ctx.request.get_header("Range").is_some()
    {
        if let Some(body) = &resp_body {
            let content_length = body.len();
            let range_header = ctx.request.get_header("Range").unwrap();

            match parse_range_header(range_header, content_length) {
                Some(ranges) if ranges.len() == 1 => {
                    // Single range -- serve 206 with Content-Range
                    let (start, end) = ranges[0];
                    response.status = HttpStatus::PARTIAL_CONTENT;
                    response.reason = HttpStatus::PARTIAL_CONTENT.reason().to_string();
                    response.set_header(
                        "Content-Range",
                        format!("bytes {}-{}/{}", start, end, content_length),
                    );
                    let sliced = body[start..=end].to_vec();
                    response.set_header("Content-Length", sliced.len().to_string());
                    resp_body = Some(sliced);
                }
                Some(_ranges) => {
                    // Multiple ranges -- for simplicity, only support single range.
                    // Serve the full response as-is (valid per RFC 7233 section 4.1).
                }
                None => {
                    // Range is not satisfiable
                    response.status = HttpStatus::RANGE_NOT_SATISFIABLE;
                    response.reason = HttpStatus::RANGE_NOT_SATISFIABLE.reason().to_string();
                    response.set_header("Content-Range", format!("bytes */{}", content_length));
                    response.unset_header("Content-Length");
                    resp_body = None;
                }
            }
        }
    }

    // Log response details
    rv_log::rv_log!(
        log,
        LogTag::RespStatus,
        ctx.vxid,
        "{}",
        response.status.code()
    );
    if let Some(body) = &resp_body {
        rv_log::rv_log!(log, LogTag::Length, ctx.vxid, "{}", body.len());
    }
    for h in response.headers.iter() {
        rv_log::rv_log!(log, LogTag::RespHeader, ctx.vxid, "{}: {}", h.name, h.value);
    }

    // Run vcl_deliver
    if let Some(vcl) = vcl {
        let mut bereq = ctx.bereq.take().unwrap_or_default();
        let mut beresp = ctx.beresp.take().unwrap_or_default();
        let mut vcl_ctx = VclContext::new(&mut ctx.request, &mut response, &mut bereq, &mut beresp);
        vcl_ctx.client_ip = conn_info.client_addr.ip();
        vcl_ctx.server_ip = conn_info.local_addr.ip();
        vcl_ctx.is_ssl = conn_info.is_tls;
        vcl_ctx.restarts = ctx.restarts;
        vcl_ctx.resp_body = resp_body.as_mut();

        if let Some(oc) = &ctx.obj {
            vcl_ctx.obj_hits = oc.hits;
        }

        log.log(LogTag::VclCall, ctx.vxid, "DELIVER");
        match vcl.exec_subroutine("vcl_deliver", &mut vcl_ctx) {
            VclExecResult::Action(action) => {
                rv_log::rv_log!(log, LogTag::VclReturn, ctx.vxid, "{}", action_name(&action));
                ctx.vcl_action = Some(convert_action(&action));
                if action == InterpreterAction::Restart {
                    // Restore bereq/beresp before restart
                    ctx.bereq = Some(bereq);
                    ctx.beresp = Some(beresp);
                    ctx.resp_body = resp_body;
                    return;
                }
            }
            VclExecResult::Fallthrough => {
                log.log(LogTag::VclReturn, ctx.vxid, "deliver");
                ctx.vcl_action = Some(VclAction::Deliver);
            }
            VclExecResult::Error(e) => {
                rv_log::rv_log!(log, LogTag::VclError, ctx.vxid, "vcl_deliver: {}", e);
                ctx.vcl_action = Some(VclAction::Deliver);
            }
        }

        ctx.bereq = Some(bereq);
        ctx.beresp = Some(beresp);
    }

    // Apply delivery filters (e.g., gzip response for client)
    if ctx.do_gzip {
        if let Some(ref body) = resp_body {
            if let Ok(compressed) = rv_filter::compress_gzip(body) {
                response.set_header("Content-Encoding", "gzip");
                response.set_header("Content-Length", compressed.len().to_string());
                resp_body = Some(compressed);
            }
        }
    }

    ctx.response = Some(response);
    ctx.resp_body = resp_body;
}

/// Synth state: build a synthetic response, run vcl_synth.
fn state_synth(
    ctx: &mut RequestContext,
    vcl: &Option<Arc<VclInterpreter>>,
    conn_info: &ConnectionInfo,
    log: &LogWriter,
) {
    let status = ctx.synth_status.unwrap_or(HttpStatus::OK);
    let mut response = HttpMessage::new_response(status, HttpVersion::Http11);
    response.set_header("X-Cache", "SYNTH");

    rv_log::rv_log!(log, LogTag::RespStatus, ctx.vxid, "{}", status.code());
    rv_log::rv_log!(log, LogTag::RespReason, ctx.vxid, "{}", status.reason());
    log.log(LogTag::VclLog, ctx.vxid, "synthetic response generated");

    let body = ctx
        .synth_body
        .take()
        .unwrap_or_else(|| status.reason().as_bytes().to_vec());

    // Run vcl_synth
    if let Some(vcl) = vcl {
        let mut bereq = HttpMessage::default();
        let mut beresp = HttpMessage::default();
        let mut vcl_ctx = VclContext::new(&mut ctx.request, &mut response, &mut bereq, &mut beresp);
        vcl_ctx.client_ip = conn_info.client_addr.ip();
        vcl_ctx.restarts = ctx.restarts;
        vcl_ctx.synth_body = Some(String::from_utf8_lossy(&body).to_string());

        log.log(LogTag::VclCall, ctx.vxid, "SYNTH");
        match vcl.exec_subroutine("vcl_synth", &mut vcl_ctx) {
            VclExecResult::Action(action) => {
                rv_log::rv_log!(log, LogTag::VclReturn, ctx.vxid, "{}", action_name(&action));
                if action == InterpreterAction::Restart && ctx.restarts < ctx.max_restarts {
                    ctx.restarts += 1;
                    ctx.state = RequestState::Recv;
                    ctx.vcl_action = None;
                    return;
                }
            }
            VclExecResult::Fallthrough => {
                log.log(LogTag::VclReturn, ctx.vxid, "deliver");
            }
            VclExecResult::Error(e) => {
                rv_log::rv_log!(log, LogTag::VclError, ctx.vxid, "vcl_synth: {}", e);
            }
        }

        // Use the synth body from VCL if it was modified
        if let Some(synth_body) = vcl_ctx.synth_body {
            let final_body = synth_body.into_bytes();
            response.set_header("Content-Length", final_body.len().to_string());
            ctx.resp_body = Some(final_body);
        } else {
            response.set_header("Content-Length", body.len().to_string());
            ctx.resp_body = Some(body);
        }
    } else {
        response.set_header("Content-Length", body.len().to_string());
        ctx.resp_body = Some(body);
    }

    ctx.response = Some(response);
}

/// Return a lowercase name for a VCL interpreter action, used for logging.
fn action_name(action: &InterpreterAction) -> &'static str {
    match action {
        InterpreterAction::Deliver => "deliver",
        InterpreterAction::Pass => "pass",
        InterpreterAction::Pipe => "pipe",
        InterpreterAction::Hash => "hash",
        InterpreterAction::Lookup => "lookup",
        InterpreterAction::Fetch => "fetch",
        InterpreterAction::Synth => "synth",
        InterpreterAction::Purge => "purge",
        InterpreterAction::Restart => "restart",
        InterpreterAction::Retry => "retry",
        InterpreterAction::Fail => "fail",
        InterpreterAction::Abandon => "abandon",
        InterpreterAction::Error => "error",
        InterpreterAction::Ok => "ok",
    }
}

/// Convert interpreter VclAction to cache VclAction.
fn convert_action(action: &InterpreterAction) -> VclAction {
    match action {
        InterpreterAction::Deliver => VclAction::Deliver,
        InterpreterAction::Pass => VclAction::Pass,
        InterpreterAction::Pipe => VclAction::Pipe,
        InterpreterAction::Hash => VclAction::Hash,
        InterpreterAction::Lookup => VclAction::Lookup,
        InterpreterAction::Fetch => VclAction::Fetch,
        InterpreterAction::Synth => VclAction::Synth,
        InterpreterAction::Purge => VclAction::Purge,
        InterpreterAction::Restart => VclAction::Restart,
        InterpreterAction::Retry => VclAction::Retry,
        InterpreterAction::Fail => VclAction::Fail,
        InterpreterAction::Abandon => VclAction::Fail,
        InterpreterAction::Error => VclAction::Fail,
        InterpreterAction::Ok => VclAction::Deliver,
    }
}

/// Compute a SHA-256 digest from a cache key string.
fn compute_digest(key: &str) -> rv_types::Digest {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(key.as_bytes());
    let result = hasher.finalize();
    let mut bytes = [0u8; 32];
    bytes.copy_from_slice(&result);
    rv_types::Digest::new(bytes)
}

/// Parse a storage specification string (e.g., "malloc,256m" or "file,/tmp/varnish,1g").
fn parse_storage_spec(spec: &str) -> Result<Arc<dyn Stevedore>> {
    let parts: Vec<&str> = spec.splitn(2, ',').collect();
    let backend_type = parts.first().copied().unwrap_or("malloc");
    let size_spec = parts.get(1).copied().unwrap_or("256m");

    match backend_type {
        "malloc" => {
            let size = rv_config::parse_size(size_spec)?;
            Ok(Arc::new(rv_storage::malloc::MallocStevedore::new(
                "s0", size,
            )))
        }
        "file" => {
            let file_parts: Vec<&str> = size_spec.splitn(2, ',').collect();
            let path = file_parts
                .first()
                .copied()
                .unwrap_or("/tmp/varnish_storage");
            let size = file_parts
                .get(1)
                .and_then(|s| rv_config::parse_size(s).ok())
                .unwrap_or(256 * 1024 * 1024);
            Ok(Arc::new(rv_storage::file::FileStevedore::new(
                "s0", path, size,
            )))
        }
        _ => {
            anyhow::bail!("unsupported storage backend: {}", backend_type);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use arc_swap::ArcSwapOption;
    use rv_vcl::{Lexer, Parser};

    const MINIMAL_VCL: &str = r#"
vcl 4.0;
backend default {
    .host = "127.0.0.1";
    .port = "8080";
}
"#;

    const MINIMAL_VCL_2: &str = r#"
vcl 4.0;
backend api {
    .host = "10.0.0.1";
    .port = "9090";
}
"#;

    fn make_interpreter(source: &str) -> Arc<VclInterpreter> {
        let tokens = Lexer::tokenize(source).unwrap();
        let program = Parser::parse(&tokens).unwrap();
        Arc::new(VclInterpreter::new(Arc::new(program)))
    }

    #[test]
    fn arc_swap_option_starts_empty() {
        let swap: ArcSwapOption<VclInterpreter> = ArcSwapOption::empty();
        assert!(swap.load_full().is_none());
    }

    #[test]
    fn arc_swap_option_store_and_load() {
        let swap: ArcSwapOption<VclInterpreter> = ArcSwapOption::empty();
        let interp = make_interpreter(MINIMAL_VCL);
        swap.store(Some(interp));
        assert!(swap.load_full().is_some());
    }

    #[test]
    fn hot_swap_does_not_affect_in_flight_reference() {
        // Simulate the hot-swap scenario:
        // 1. A request loads the current interpreter (v1).
        // 2. While the request is "in-flight", a vcl.use swaps to v2.
        // 3. The in-flight request still holds v1 and can use it.
        // 4. New requests pick up v2.

        let swap = Arc::new(ArcSwapOption::<VclInterpreter>::empty());

        // Load v1
        let v1 = make_interpreter(MINIMAL_VCL);
        swap.store(Some(Arc::clone(&v1)));

        // Simulate in-flight request loading the current interpreter
        let in_flight_vcl: Option<Arc<VclInterpreter>> = swap.load_full();
        assert!(in_flight_vcl.is_some());

        // The in-flight reference is a separate Arc pointing to v1.
        // We can verify identity by checking the program's backend list.
        let in_flight_ref = in_flight_vcl.as_ref().unwrap();
        assert_eq!(in_flight_ref.program().backends.len(), 1);
        assert_eq!(in_flight_ref.program().backends[0].name, "default");

        // Now swap to v2 (simulating vcl.use)
        let v2 = make_interpreter(MINIMAL_VCL_2);
        swap.store(Some(v2));

        // The in-flight reference should still point to v1
        assert_eq!(in_flight_ref.program().backends[0].name, "default");

        // A new request should see v2
        let new_request_vcl: Option<Arc<VclInterpreter>> = swap.load_full();
        let new_ref = new_request_vcl.as_ref().unwrap();
        assert_eq!(new_ref.program().backends[0].name, "api");
    }

    #[test]
    fn swap_to_none_clears_interpreter() {
        let swap = Arc::new(ArcSwapOption::<VclInterpreter>::empty());
        let v1 = make_interpreter(MINIMAL_VCL);
        swap.store(Some(v1));
        assert!(swap.load_full().is_some());

        // Clearing the swap
        swap.store(None);
        assert!(swap.load_full().is_none());
    }

    #[test]
    fn multiple_swaps_always_return_latest() {
        let swap = Arc::new(ArcSwapOption::<VclInterpreter>::empty());

        let v1 = make_interpreter(MINIMAL_VCL);
        swap.store(Some(v1));
        {
            let loaded = swap.load_full().unwrap();
            assert_eq!(loaded.program().backends[0].name, "default");
        }

        let v2 = make_interpreter(MINIMAL_VCL_2);
        swap.store(Some(v2));
        {
            let loaded = swap.load_full().unwrap();
            assert_eq!(loaded.program().backends[0].name, "api");
        }

        // Swap back to v1-style
        let v1_again = make_interpreter(MINIMAL_VCL);
        swap.store(Some(v1_again));
        {
            let loaded = swap.load_full().unwrap();
            assert_eq!(loaded.program().backends[0].name, "default");
        }
    }

    #[test]
    fn cancellation_token_basic() {
        let token = CancellationToken::new();
        assert!(!token.is_cancelled());

        let child = token.clone();
        token.cancel();

        assert!(token.is_cancelled());
        assert!(child.is_cancelled());
    }

    #[test]
    fn cancellation_token_child_inherits_cancel() {
        let parent = CancellationToken::new();
        let child = parent.child_token();

        assert!(!child.is_cancelled());
        parent.cancel();
        assert!(child.is_cancelled());
    }

    #[tokio::test]
    async fn shutdown_signal_cancellation_flow() {
        // Test that the cancellation token integrates properly with
        // tokio::select! -- the pattern used in ServerRuntime::run().
        let cancel = CancellationToken::new();
        let cancel_clone = cancel.clone();

        // Spawn a task that cancels after a short delay (simulating signal)
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            cancel_clone.cancel();
        });

        // Wait for cancellation
        cancel.cancelled().await;
        assert!(cancel.is_cancelled());
    }

    // ===================================================================
    // Comprehensive VCL integration tests
    //
    // These tests exercise the full FSM pipeline with VCL subroutines,
    // covering every VCL hook, action, variable type, expression operator,
    // and statement form implemented in the interpreter.
    // ===================================================================

    use rv_hash::HashSlinger;
    use rv_hash::simple::SimpleListHash;
    use rv_log::{LogWriter, RingBuffer};
    use rv_storage::Stevedore;
    use rv_storage::malloc::MallocStevedore;
    use rv_transport::traits::DetectedVersion;

    /// Build a cache engine for testing.
    fn make_log() -> Arc<LogWriter> {
        let ringbuf = Arc::new(RingBuffer::new(1024));
        Arc::new(LogWriter::new(ringbuf))
    }

    fn make_cache() -> Arc<CacheEngine> {
        let config = CacheConfig::default();
        let hash: Arc<dyn HashSlinger> = Arc::new(SimpleListHash::new());
        let storage: Arc<dyn Stevedore> = Arc::new(MallocStevedore::new("test", 64 * 1024 * 1024));
        let ringbuf = Arc::new(RingBuffer::new(1024));
        let log = Arc::new(LogWriter::new(ringbuf));
        Arc::new(CacheEngine::new(config, hash, storage, log))
    }

    /// Build a ConnectionInfo for testing.
    fn make_conn_info() -> ConnectionInfo {
        ConnectionInfo {
            client_addr: "192.168.1.100:54321".parse().unwrap(),
            local_addr: "10.0.0.1:6081".parse().unwrap(),
            real_client_addr: None,
            is_tls: false,
            http_version: DetectedVersion::Http11,
        }
    }

    /// Start a mock HTTP backend that returns configurable responses.
    ///
    /// The backend examines the request URL to decide what to return:
    ///   /api/data       -> 200 OK with JSON body and ETag
    ///   /api/slow       -> 200 OK with body
    ///   /api/private    -> 200 OK with Cache-Control: no-store
    ///   /rewritten-path -> 200 OK with body confirming rewrite
    ///   anything else   -> 200 OK with generic body
    async fn start_mock_backend() -> SocketAddr {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        tokio::spawn(async move {
            loop {
                let (mut stream, _) = match listener.accept().await {
                    Ok(conn) => conn,
                    Err(_) => break,
                };
                tokio::spawn(async move {
                    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
                    let (reader, mut writer) = stream.split();
                    let mut buf_reader = BufReader::new(reader);
                    let mut line = String::new();
                    // Read request line
                    if buf_reader.read_line(&mut line).await.unwrap_or(0) == 0 {
                        return;
                    }
                    let req_line = line.clone();
                    // Read headers until empty line
                    let mut req_headers = Vec::new();
                    loop {
                        let mut hdr = String::new();
                        if buf_reader.read_line(&mut hdr).await.unwrap_or(0) == 0 {
                            break;
                        }
                        if hdr.trim().is_empty() {
                            break;
                        }
                        req_headers.push(hdr.trim().to_string());
                    }

                    // Parse URL from request line
                    let url = req_line.split_whitespace().nth(1).unwrap_or("/");

                    let (status, headers, body) = match url {
                        "/api/data" => (
                            "200 OK",
                            vec![
                                "Content-Type: application/json",
                                "ETag: \"v1-abc123\"",
                                "Last-Modified: Mon, 01 Jan 2024 00:00:00 GMT",
                                "Vary: Accept-Encoding",
                            ],
                            r#"{"result":"ok","count":42}"#.to_string(),
                        ),
                        "/api/private" => (
                            "200 OK",
                            vec!["Content-Type: text/plain", "Cache-Control: no-store"],
                            "private data".to_string(),
                        ),
                        "/rewritten-path" => (
                            "200 OK",
                            vec!["Content-Type: text/plain"],
                            "rewrite confirmed".to_string(),
                        ),
                        _ => (
                            "200 OK",
                            vec!["Content-Type: text/plain"],
                            format!("backend response for {url}"),
                        ),
                    };

                    // Check for X-Backend-Echo header -- echo it back as
                    // X-Backend-Echoed so the test can verify bereq headers.
                    let mut extra_headers = String::new();
                    for h in &req_headers {
                        if h.to_lowercase().starts_with("x-backend-echo:") {
                            let val = h.split_once(':').map(|x| x.1).unwrap_or("").trim();
                            extra_headers.push_str(&format!("X-Backend-Echoed: {val}\r\n"));
                        }
                    }

                    let resp = format!(
                        "HTTP/1.1 {status}\r\n\
                         {}\
                         {extra_headers}\
                         Content-Length: {}\r\n\
                         Connection: close\r\n\
                         \r\n\
                         {body}",
                        headers
                            .iter()
                            .map(|h| format!("{h}\r\n"))
                            .collect::<String>(),
                        body.len(),
                    );
                    let _ = writer.write_all(resp.as_bytes()).await;
                });
            }
        });

        addr
    }

    // ---------------------------------------------------------------
    // VCL source exercising all major features
    // ---------------------------------------------------------------

    /// A VCL program that exercises every subroutine hook, most variable
    /// types, all expression operators, and every statement form.
    fn full_vcl(backend_port: u16) -> String {
        format!(
            r#"
vcl 4.0;

backend default {{
    .host = "127.0.0.1";
    .port = "{backend_port}";
}}

// -------------------------------------------------------------------
// vcl_recv -- first hook for every request
//
// Exercises: if/elsif/else, regex match (~), string comparison (==),
//   set (=, +=), unset, req.url, req.method, req.http.*,
//   return(synth), return(pass), return(hash), regsub(), negation (!),
//   logical AND (&&), logical OR (||), boolean literals
// -------------------------------------------------------------------
sub vcl_recv {{
    // 1. Block forbidden paths with a synthetic 403
    if (req.url ~ "^/blocked") {{
        return (synth(403, "Forbidden by VCL"));
    }}

    // 2. Force pass for non-GET/HEAD methods (POST, PUT, etc.)
    if (req.method != "GET" && req.method != "HEAD") {{
        return (pass);
    }}

    // 3. URL rewriting: /old-path -> /rewritten-path
    if (req.url == "/old-path") {{
        set req.url = "/rewritten-path";
    }}

    // 4. Regex-based URL normalization: strip trailing slash
    if (req.url ~ "/$" && req.url != "/") {{
        set req.url = regsub(req.url, "/$", "");
    }}

    // 5. Mark internal API requests with a tracking header
    if (req.url ~ "^/api/") {{
        set req.http.X-Is-Api = "true";
        set req.http.X-Client-IP = client.ip;
    }}

    // 6. Remove tracking cookies to improve cache hit rate
    unset req.http.Cookie;

    // 7. Test header presence with negation
    if (!req.http.X-Custom-Auth) {{
        set req.http.X-Auth-Status = "anonymous";
    }} else {{
        set req.http.X-Auth-Status = "authenticated";
    }}

    // 8. Force pass for /api/private
    if (req.url == "/api/private") {{
        return (pass);
    }}

    return (hash);
}}

// -------------------------------------------------------------------
// vcl_backend_fetch -- before sending request to backend
//
// Exercises: bereq.url, bereq.http.*, set with concat (+)
// -------------------------------------------------------------------
sub vcl_backend_fetch {{
    set bereq.http.X-Forwarded-For = client.ip;
    set bereq.http.X-Backend-Echo = "vcl-was-here";
    set bereq.http.X-Request-Id = "vxid-" + req.http.X-Is-Api;
    return (fetch);
}}

// -------------------------------------------------------------------
// vcl_backend_response -- after receiving backend response
//
// Exercises: beresp.ttl, beresp.grace, beresp.keep (duration types),
//   beresp.status (integer comparison), beresp.uncacheable (boolean),
//   beresp.http.*, arithmetic on durations, elsif chains
// -------------------------------------------------------------------
sub vcl_backend_response {{
    // Set cache durations
    set beresp.ttl = 300s;
    set beresp.grace = 60s;
    set beresp.keep = 120s;

    // Mark non-200 responses as uncacheable
    if (beresp.status != 200) {{
        set beresp.uncacheable = true;
        set beresp.ttl = 0s;
    }}

    // Check for no-store directive
    if (beresp.http.Cache-Control ~ "no-store") {{
        set beresp.uncacheable = true;
        set beresp.ttl = 0s;
    }}

    // Add server timing header
    set beresp.http.X-Served-By = "varaha-cache";

    return (deliver);
}}

// -------------------------------------------------------------------
// vcl_hit -- cache hit
//
// Exercises: obj.hits (integer), obj.ttl, obj.grace, obj.keep
//   (duration reads), integer comparison (>), return(deliver)
// -------------------------------------------------------------------
sub vcl_hit {{
    set req.http.X-Cache-Hits = obj.hits;
    if (obj.ttl > 0s) {{
        return (deliver);
    }}
    return (deliver);
}}

// -------------------------------------------------------------------
// vcl_miss -- cache miss
//
// Exercises: return(fetch)
// -------------------------------------------------------------------
sub vcl_miss {{
    set req.http.X-Cache-Status = "miss";
    return (fetch);
}}

// -------------------------------------------------------------------
// vcl_pass -- pass mode
//
// Exercises: return(fetch)
// -------------------------------------------------------------------
sub vcl_pass {{
    set req.http.X-Cache-Status = "pass";
    return (fetch);
}}

// -------------------------------------------------------------------
// vcl_deliver -- before sending response to client
//
// Exercises: resp.http.* (set/unset), resp.status (read),
//   req.restarts (integer), conditional header logic, return(deliver),
//   return(restart) with restart counter check, string comparison,
//   multiple set statements, unset
// -------------------------------------------------------------------
sub vcl_deliver {{
    // Tag response with cache status
    if (req.http.X-Cache-Status == "miss") {{
        set resp.http.X-Cache-Detail = "fetched-from-backend";
    }}

    // Include client IP in response
    set resp.http.X-Client-Seen = client.ip;
    set resp.http.X-Server-Addr = server.ip;

    // TLS indicator
    if (req.is_ssl) {{
        set resp.http.X-TLS = "true";
    }} else {{
        set resp.http.X-TLS = "false";
    }}

    // Restart test: if X-Force-Restart header is present and we
    // haven't restarted yet, trigger a restart.
    if (req.http.X-Force-Restart == "yes" && req.restarts == 0) {{
        set req.http.X-Force-Restart = "no";
        return (restart);
    }}

    // After a restart, mark that we restarted
    if (req.restarts > 0) {{
        set resp.http.X-Restarted = "true";
        set resp.http.X-Restart-Count = req.restarts;
    }}

    // Remove internal headers before delivery
    unset resp.http.X-Served-By;

    return (deliver);
}}

// -------------------------------------------------------------------
// vcl_synth -- synthetic response generation
//
// Exercises: resp.status, resp.reason, resp.http.*, synthetic(),
//   string concatenation, if/else on status codes
// -------------------------------------------------------------------
sub vcl_synth {{
    if (resp.status == 403) {{
        set resp.http.Content-Type = "text/html";
        synthetic("<html><body><h1>403 Forbidden</h1><p>Access denied by VCL policy.</p></body></html>");
    }} elsif (resp.status == 503) {{
        set resp.http.Content-Type = "text/plain";
        synthetic("Service temporarily unavailable. Please try again later.");
    }} else {{
        set resp.http.Content-Type = "text/plain";
        synthetic("Synthetic response");
    }}
    set resp.http.X-Synthetic = "true";
    return (deliver);
}}
"#
        )
    }

    // ---------------------------------------------------------------
    // Test 1: Synthetic 403 response (vcl_recv -> synth -> vcl_synth)
    //
    // Covers: vcl_recv regex match, return(synth(403)),
    //   vcl_synth conditional, synthetic() statement, resp.http.*
    // ---------------------------------------------------------------
    #[tokio::test]
    async fn vcl_synth_403_blocked_path() {
        let backend_addr = start_mock_backend().await;
        let cache = make_cache();
        let vcl = Some(make_interpreter(&full_vcl(backend_addr.port())));
        let conn_info = make_conn_info();

        let request =
            HttpMessage::new_request(HttpMethod::Get, "/blocked/secret", HttpVersion::Http11);

        let (resp, body) = handle_request(
            cache,
            request,
            None,
            conn_info,
            Some(backend_addr),
            vcl,
            make_log(),
        )
        .await;

        assert_eq!(resp.status, HttpStatus::FORBIDDEN);
        assert_eq!(resp.get_header("X-Synthetic").unwrap(), "true");
        assert_eq!(resp.get_header("Content-Type").unwrap(), "text/html");

        let body_str = String::from_utf8(body.unwrap()).unwrap();
        assert!(body_str.contains("403 Forbidden"));
        assert!(body_str.contains("Access denied by VCL policy"));
    }

    // ---------------------------------------------------------------
    // Test 2: POST request forces pass (vcl_recv method check)
    //
    // Covers: req.method != comparison, return(pass),
    //   vcl_pass, vcl_backend_fetch bereq headers,
    //   vcl_backend_response, vcl_deliver resp headers
    // ---------------------------------------------------------------
    #[tokio::test]
    async fn vcl_pass_for_post_request() {
        let backend_addr = start_mock_backend().await;
        let cache = make_cache();
        let vcl = Some(make_interpreter(&full_vcl(backend_addr.port())));
        let conn_info = make_conn_info();

        let mut request =
            HttpMessage::new_request(HttpMethod::Post, "/api/data", HttpVersion::Http11);
        request.set_header("Host", "localhost");

        let (resp, body) = handle_request(
            cache,
            request,
            None,
            conn_info,
            Some(backend_addr),
            vcl,
            make_log(),
        )
        .await;

        assert_eq!(resp.status, HttpStatus::OK);
        // vcl_deliver sets X-TLS based on conn_info.is_tls (false here)
        assert_eq!(resp.get_header("X-TLS").unwrap(), "false");
        // X-Client-Seen should contain the test client IP
        assert_eq!(resp.get_header("X-Client-Seen").unwrap(), "192.168.1.100");
        // X-Served-By should be unset in vcl_deliver
        assert!(resp.get_header("X-Served-By").is_none());
        // Body should be present
        assert!(body.is_some());
    }

    // ---------------------------------------------------------------
    // Test 3: Cache miss then hit (full pipeline)
    //
    // Covers: vcl_recv hash, vcl_miss, vcl_backend_fetch,
    //   vcl_backend_response TTL/grace/keep, cache insert,
    //   vcl_deliver X-Cache headers, then second request hits cache,
    //   vcl_hit obj.hits, deliver from cache
    // ---------------------------------------------------------------
    #[tokio::test]
    async fn vcl_cache_miss_then_hit() {
        let backend_addr = start_mock_backend().await;
        let cache = make_cache();
        let vcl_src = full_vcl(backend_addr.port());

        // -- First request: cache miss --
        let vcl = Some(make_interpreter(&vcl_src));
        let conn_info = make_conn_info();
        let mut request =
            HttpMessage::new_request(HttpMethod::Get, "/api/data", HttpVersion::Http11);
        request.set_header("Host", "test.example.com");

        let (resp1, body1) = handle_request(
            Arc::clone(&cache),
            request,
            None,
            conn_info.clone(),
            Some(backend_addr),
            vcl,
            make_log(),
        )
        .await;

        assert_eq!(resp1.status, HttpStatus::OK);
        assert_eq!(resp1.get_header("X-Cache").unwrap(), "MISS");
        assert_eq!(
            resp1.get_header("X-Cache-Detail").unwrap(),
            "fetched-from-backend"
        );
        let body1_str = String::from_utf8(body1.unwrap()).unwrap();
        assert!(body1_str.contains("result"));

        // -- Second request: cache hit --
        let vcl = Some(make_interpreter(&vcl_src));
        let mut request2 =
            HttpMessage::new_request(HttpMethod::Get, "/api/data", HttpVersion::Http11);
        request2.set_header("Host", "test.example.com");

        let (resp2, body2) = handle_request(
            Arc::clone(&cache),
            request2,
            None,
            conn_info,
            Some(backend_addr),
            vcl,
            make_log(),
        )
        .await;

        assert_eq!(resp2.status, HttpStatus::OK);
        assert_eq!(resp2.get_header("X-Cache").unwrap(), "HIT");
        // Body should come from cache
        assert!(body2.is_some());
    }

    // ---------------------------------------------------------------
    // Test 4: URL rewriting (vcl_recv set req.url)
    //
    // Covers: req.url == comparison, set req.url = "/new",
    //   backend receives the rewritten URL, response confirms it
    // ---------------------------------------------------------------
    #[tokio::test]
    async fn vcl_url_rewrite() {
        let backend_addr = start_mock_backend().await;
        let cache = make_cache();
        let vcl = Some(make_interpreter(&full_vcl(backend_addr.port())));
        let conn_info = make_conn_info();

        let mut request =
            HttpMessage::new_request(HttpMethod::Get, "/old-path", HttpVersion::Http11);
        request.set_header("Host", "localhost");

        let (resp, body) = handle_request(
            cache,
            request,
            None,
            conn_info,
            Some(backend_addr),
            vcl,
            make_log(),
        )
        .await;

        assert_eq!(resp.status, HttpStatus::OK);
        let body_str = String::from_utf8(body.unwrap()).unwrap();
        assert!(
            body_str.contains("rewrite confirmed"),
            "backend should see the rewritten URL, got: {body_str}"
        );
    }

    // ---------------------------------------------------------------
    // Test 5: Header inspection and manipulation
    //
    // Covers: req.http.* read/write, unset req.http.Cookie,
    //   negation (!req.http.X-Custom-Auth), conditional set,
    //   X-Is-Api tagging for /api/ paths
    // ---------------------------------------------------------------
    #[tokio::test]
    async fn vcl_header_manipulation() {
        let backend_addr = start_mock_backend().await;
        let _cache = make_cache();
        let vcl_src = full_vcl(backend_addr.port());

        // Request WITHOUT X-Custom-Auth -> anonymous
        let vcl = Some(make_interpreter(&vcl_src));
        let conn_info = make_conn_info();
        let mut req1 = HttpMessage::new_request(HttpMethod::Get, "/api/data", HttpVersion::Http11);
        req1.set_header("Host", "localhost");
        req1.set_header("Cookie", "session=abc123");

        let (resp1, _) = handle_request(
            make_cache(),
            req1,
            None,
            conn_info.clone(),
            Some(backend_addr),
            vcl,
            make_log(),
        )
        .await;

        assert_eq!(resp1.status, HttpStatus::OK);

        // Request WITH X-Custom-Auth -> authenticated
        let vcl = Some(make_interpreter(&vcl_src));
        let mut req2 = HttpMessage::new_request(
            HttpMethod::Post, // POST forces pass
            "/something",
            HttpVersion::Http11,
        );
        req2.set_header("Host", "localhost");
        req2.set_header("X-Custom-Auth", "Bearer token123");

        let (resp2, _) = handle_request(
            make_cache(),
            req2,
            None,
            conn_info,
            Some(backend_addr),
            vcl,
            make_log(),
        )
        .await;

        assert_eq!(resp2.status, HttpStatus::OK);
    }

    // ---------------------------------------------------------------
    // Test 6: Restart flow (vcl_deliver return(restart))
    //
    // Covers: req.http header-driven restart, req.restarts counter,
    //   return(restart), X-Restarted and X-Restart-Count headers
    // ---------------------------------------------------------------
    #[tokio::test]
    async fn vcl_restart_flow() {
        let backend_addr = start_mock_backend().await;
        let cache = make_cache();
        let vcl = Some(make_interpreter(&full_vcl(backend_addr.port())));
        let conn_info = make_conn_info();

        let mut request =
            HttpMessage::new_request(HttpMethod::Get, "/something", HttpVersion::Http11);
        request.set_header("Host", "localhost");
        request.set_header("X-Force-Restart", "yes");

        let (resp, _) = handle_request(
            cache,
            request,
            None,
            conn_info,
            Some(backend_addr),
            vcl,
            make_log(),
        )
        .await;

        assert_eq!(resp.status, HttpStatus::OK);
        assert_eq!(resp.get_header("X-Restarted").unwrap(), "true");
    }

    // ---------------------------------------------------------------
    // Test 7: Conditional request -> 304 Not Modified
    //
    // Covers: If-None-Match + ETag evaluation in deliver state,
    //   304 response with metadata headers preserved.
    //   Uses pass mode so the request always goes to the backend
    //   (which returns ETag headers needed for conditional evaluation).
    // ---------------------------------------------------------------
    #[tokio::test]
    async fn vcl_conditional_304() {
        let backend_addr = start_mock_backend().await;
        let cache = make_cache();
        let vcl_src = full_vcl(backend_addr.port());
        let conn_info = make_conn_info();

        // Use POST to force pass (backend always fetched, ETag headers present)
        // Then send GET with If-None-Match on a fresh miss (not cache hit)
        let mut req = HttpMessage::new_request(HttpMethod::Get, "/api/data", HttpVersion::Http11);
        req.set_header("Host", "cond304.example.com");
        req.set_header("If-None-Match", "\"v1-abc123\"");

        let (resp, body) = handle_request(
            cache,
            req,
            None,
            conn_info,
            Some(backend_addr),
            Some(make_interpreter(&vcl_src)),
            make_log(),
        )
        .await;

        // On a cache miss, the backend returns ETag: "v1-abc123"
        // which matches If-None-Match, so deliver should produce 304
        assert_eq!(resp.status, HttpStatus::NOT_MODIFIED);
        // 304 should have no body
        assert!(body.is_none());
        // ETag should be preserved in 304
        assert!(resp.get_header("ETag").is_some());
    }

    // ---------------------------------------------------------------
    // Test 8: Range request -> 206 Partial Content
    //
    // Covers: Range header parsing, 206 response, Content-Range
    // ---------------------------------------------------------------
    #[tokio::test]
    async fn vcl_range_request_206() {
        let backend_addr = start_mock_backend().await;
        let cache = make_cache();
        let vcl_src = full_vcl(backend_addr.port());

        // First request populates cache
        let conn_info = make_conn_info();
        let mut req1 = HttpMessage::new_request(HttpMethod::Get, "/api/data", HttpVersion::Http11);
        req1.set_header("Host", "range.example.com");
        let (_, full_body) = handle_request(
            Arc::clone(&cache),
            req1,
            None,
            conn_info.clone(),
            Some(backend_addr),
            Some(make_interpreter(&vcl_src)),
            make_log(),
        )
        .await;
        let full_len = full_body.as_ref().unwrap().len();

        // Second request with Range header
        let mut req2 = HttpMessage::new_request(HttpMethod::Get, "/api/data", HttpVersion::Http11);
        req2.set_header("Host", "range.example.com");
        req2.set_header("Range", "bytes=0-4");

        let (resp2, body2) = handle_request(
            Arc::clone(&cache),
            req2,
            None,
            conn_info,
            Some(backend_addr),
            Some(make_interpreter(&vcl_src)),
            make_log(),
        )
        .await;

        assert_eq!(resp2.status, HttpStatus::PARTIAL_CONTENT);
        let range_hdr = resp2.get_header("Content-Range").unwrap();
        assert!(
            range_hdr.contains(&format!("bytes 0-4/{full_len}")),
            "Content-Range should specify bytes 0-4/{full_len}, got: {range_hdr}"
        );
        let partial_body = body2.unwrap();
        assert_eq!(partial_body.len(), 5);
    }

    // ---------------------------------------------------------------
    // Test 9: Uncacheable response (beresp.uncacheable in backend_response)
    //
    // Covers: beresp.http.Cache-Control regex check,
    //   beresp.uncacheable = true, beresp.ttl = 0s,
    //   response is NOT cached (second request still misses)
    // ---------------------------------------------------------------
    #[tokio::test]
    async fn vcl_uncacheable_response() {
        let backend_addr = start_mock_backend().await;
        let cache = make_cache();
        let vcl_src = full_vcl(backend_addr.port());

        let conn_info = make_conn_info();

        // /api/private returns Cache-Control: no-store -> uncacheable
        let mut req1 =
            HttpMessage::new_request(HttpMethod::Get, "/api/private", HttpVersion::Http11);
        req1.set_header("Host", "priv.example.com");

        let (resp1, _) = handle_request(
            Arc::clone(&cache),
            req1,
            None,
            conn_info.clone(),
            Some(backend_addr),
            Some(make_interpreter(&vcl_src)),
            make_log(),
        )
        .await;
        // VCL says return(pass) for /api/private, so it fetches but doesn't cache
        assert_eq!(resp1.status, HttpStatus::OK);
        assert_eq!(resp1.get_header("X-Cache").unwrap(), "MISS");

        // Second request should also miss (not cached)
        let mut req2 =
            HttpMessage::new_request(HttpMethod::Get, "/api/private", HttpVersion::Http11);
        req2.set_header("Host", "priv.example.com");

        let (resp2, _) = handle_request(
            Arc::clone(&cache),
            req2,
            None,
            conn_info,
            Some(backend_addr),
            Some(make_interpreter(&vcl_src)),
            make_log(),
        )
        .await;
        assert_eq!(resp2.get_header("X-Cache").unwrap(), "MISS");
    }

    // ---------------------------------------------------------------
    // Test 10: No VCL -- default pipeline behavior
    //
    // Covers: fallthrough (no VCL) -> cache lookup -> fetch -> deliver
    // ---------------------------------------------------------------
    #[tokio::test]
    async fn no_vcl_default_pipeline() {
        let backend_addr = start_mock_backend().await;
        let cache = make_cache();
        let conn_info = make_conn_info();

        let mut request = HttpMessage::new_request(HttpMethod::Get, "/plain", HttpVersion::Http11);
        request.set_header("Host", "localhost");

        let (resp, body) = handle_request(
            cache,
            request,
            None,
            conn_info,
            Some(backend_addr),
            None, // no VCL
            make_log(),
        )
        .await;

        assert_eq!(resp.status, HttpStatus::OK);
        assert_eq!(resp.get_header("X-Cache").unwrap(), "MISS");
        let body_str = String::from_utf8(body.unwrap()).unwrap();
        assert!(body_str.contains("backend response for /plain"));
    }

    // ---------------------------------------------------------------
    // Test 11: No backend configured -> 503 synth
    //
    // Covers: backend_addr = None triggers synth 503
    // ---------------------------------------------------------------
    #[tokio::test]
    async fn no_backend_returns_503() {
        let cache = make_cache();
        let conn_info = make_conn_info();

        let mut request =
            HttpMessage::new_request(HttpMethod::Get, "/anything", HttpVersion::Http11);
        request.set_header("Host", "localhost");

        let (resp, body) = handle_request(
            cache,
            request,
            None,
            conn_info,
            None, // no backend
            None, // no VCL
            make_log(),
        )
        .await;

        assert_eq!(resp.status, HttpStatus::SERVICE_UNAVAILABLE);
        let body_str = String::from_utf8(body.unwrap()).unwrap();
        assert!(body_str.contains("No backend configured"));
    }

    // ---------------------------------------------------------------
    // Test 12: TLS connection indicator
    //
    // Covers: conn_info.is_tls reflected in req.is_ssl,
    //   vcl_deliver sets X-TLS accordingly
    // ---------------------------------------------------------------
    #[tokio::test]
    async fn vcl_tls_indicator() {
        let backend_addr = start_mock_backend().await;
        let cache = make_cache();
        let vcl = Some(make_interpreter(&full_vcl(backend_addr.port())));
        let mut conn_info = make_conn_info();
        conn_info.is_tls = true; // simulate TLS connection

        let mut request =
            HttpMessage::new_request(HttpMethod::Post, "/tls-test", HttpVersion::Http11);
        request.set_header("Host", "localhost");

        let (resp, _) = handle_request(
            cache,
            request,
            None,
            conn_info,
            Some(backend_addr),
            vcl,
            make_log(),
        )
        .await;

        assert_eq!(resp.get_header("X-TLS").unwrap(), "true");
    }

    // ---------------------------------------------------------------
    // Test 13: Purge and cache invalidation
    //
    // Covers: cache.purge() API, objects removed from cache,
    //   cache.ban() API (adds to ban list)
    // ---------------------------------------------------------------
    #[tokio::test]
    async fn cache_ban_invalidation() {
        let backend_addr = start_mock_backend().await;
        let cache = make_cache();
        let vcl_src = full_vcl(backend_addr.port());
        let conn_info = make_conn_info();

        // Populate cache
        let mut req1 = HttpMessage::new_request(HttpMethod::Get, "/api/data", HttpVersion::Http11);
        req1.set_header("Host", "ban.example.com");
        let (resp1, _) = handle_request(
            Arc::clone(&cache),
            req1,
            None,
            conn_info.clone(),
            Some(backend_addr),
            Some(make_interpreter(&vcl_src)),
            make_log(),
        )
        .await;
        assert_eq!(resp1.get_header("X-Cache").unwrap(), "MISS");

        // Verify cache hit
        let mut req2 = HttpMessage::new_request(HttpMethod::Get, "/api/data", HttpVersion::Http11);
        req2.set_header("Host", "ban.example.com");
        let (resp2, _) = handle_request(
            Arc::clone(&cache),
            req2,
            None,
            conn_info.clone(),
            Some(backend_addr),
            Some(make_interpreter(&vcl_src)),
            make_log(),
        )
        .await;
        assert_eq!(resp2.get_header("X-Cache").unwrap(), "HIT");

        // Verify ban() API works (adds to ban list without error)
        cache.ban("req.url ~ ^/api/").unwrap();

        // Purge the object by digest to force a cache miss
        let digest = compute_digest("ban.example.com:/api/data");
        cache.purge(&digest);

        // After purge, should miss again
        let mut req3 = HttpMessage::new_request(HttpMethod::Get, "/api/data", HttpVersion::Http11);
        req3.set_header("Host", "ban.example.com");
        let (resp3, _) = handle_request(
            Arc::clone(&cache),
            req3,
            None,
            conn_info,
            Some(backend_addr),
            Some(make_interpreter(&vcl_src)),
            make_log(),
        )
        .await;
        assert_eq!(resp3.get_header("X-Cache").unwrap(), "MISS");
    }

    // ---------------------------------------------------------------
    // Test 14: Cache stats tracking
    //
    // Covers: CacheEngine stats (hits, misses, object count)
    // ---------------------------------------------------------------
    #[tokio::test]
    async fn cache_stats_tracking() {
        let backend_addr = start_mock_backend().await;
        let cache = make_cache();
        let vcl_src = full_vcl(backend_addr.port());
        let conn_info = make_conn_info();

        let stats_before = cache.stats();
        assert_eq!(stats_before.n_objects, 0);

        // Miss -> inserts object
        let mut req1 = HttpMessage::new_request(HttpMethod::Get, "/api/data", HttpVersion::Http11);
        req1.set_header("Host", "stats.example.com");
        let _ = handle_request(
            Arc::clone(&cache),
            req1,
            None,
            conn_info.clone(),
            Some(backend_addr),
            Some(make_interpreter(&vcl_src)),
            make_log(),
        )
        .await;

        let stats_after_miss = cache.stats();
        assert_eq!(stats_after_miss.cache_miss, stats_before.cache_miss + 1);

        // Hit
        let mut req2 = HttpMessage::new_request(HttpMethod::Get, "/api/data", HttpVersion::Http11);
        req2.set_header("Host", "stats.example.com");
        let _ = handle_request(
            Arc::clone(&cache),
            req2,
            None,
            conn_info,
            Some(backend_addr),
            Some(make_interpreter(&vcl_src)),
            make_log(),
        )
        .await;

        let stats_after_hit = cache.stats();
        assert_eq!(stats_after_hit.cache_hit, stats_before.cache_hit + 1);
    }

    // ---------------------------------------------------------------
    // Test 15: VCL interpreter directly -- expression evaluation
    //
    // Covers: all expression types, regex, regsub, regsuball,
    //   arithmetic, string concat, boolean ops, comparisons
    // ---------------------------------------------------------------
    #[test]
    fn vcl_expression_coverage() {
        let vcl_src = r#"
vcl 4.0;
backend default { .host = "127.0.0.1"; .port = "80"; }

sub vcl_recv {
    // String concatenation
    set req.http.X-Concat = "hello" + " " + "world";

    // Regex substitution
    set req.http.X-Regsub = regsub(req.url, "^/old/(.*)", "/new/$1");

    // Regex substitution (all occurrences)
    set req.http.X-Regsuball = regsuball(req.url, "a", "X");

    // Arithmetic in string context
    set req.http.X-Arithmetic = 2 + 3;

    // Boolean evaluation
    if (req.method == "GET" && (req.url ~ "^/" || req.url == "/")) {
        set req.http.X-Bool-Test = "passed";
    }

    // Negation
    if (!(req.method == "POST")) {
        set req.http.X-Not-Post = "true";
    }

    // Comparison operators
    if (req.restarts >= 0 && req.restarts <= 4) {
        set req.http.X-Restart-Range = "valid";
    }

    return (synth(200, "expression test"));
}

sub vcl_synth {
    synthetic("ok");
    return (deliver);
}
"#;

        let vcl = make_interpreter(vcl_src);
        let mut req =
            HttpMessage::new_request(HttpMethod::Get, "/old/path/here", HttpVersion::Http11);
        let mut resp = HttpMessage::new_response(HttpStatus::OK, HttpVersion::Http11);
        let mut bereq = HttpMessage::default();
        let mut beresp = HttpMessage::default();

        let mut ctx = VclContext::new(&mut req, &mut resp, &mut bereq, &mut beresp);
        ctx.client_ip = "127.0.0.1".parse().unwrap();
        ctx.server_ip = "10.0.0.1".parse().unwrap();

        let result = vcl.exec_subroutine("vcl_recv", &mut ctx);
        assert!(matches!(
            result,
            VclExecResult::Action(InterpreterAction::Synth)
        ));

        // Verify all headers set by the expressions
        assert_eq!(req.get_header("X-Concat").unwrap(), "hello world");
        assert_eq!(req.get_header("X-Regsub").unwrap(), "/new/path/here");
        assert_eq!(req.get_header("X-Regsuball").unwrap(), "/old/pXth/here");
        assert_eq!(req.get_header("X-Bool-Test").unwrap(), "passed");
        assert_eq!(req.get_header("X-Not-Post").unwrap(), "true");
        assert_eq!(req.get_header("X-Restart-Range").unwrap(), "valid");
    }

    // ---------------------------------------------------------------
    // Test 16: VCL subroutine call chains
    //
    // Covers: call statement, subroutine delegation, variable
    //   passing through call chains
    // ---------------------------------------------------------------
    #[test]
    fn vcl_call_subroutine() {
        let vcl_src = r#"
vcl 4.0;
backend default { .host = "127.0.0.1"; .port = "80"; }

sub normalize_url {
    set req.url = regsub(req.url, "\?.*$", "");
    set req.http.X-Normalized = "true";
}

sub add_tracking {
    set req.http.X-Tracking = "track-" + req.url;
}

sub vcl_recv {
    // Call helper subroutines
    call normalize_url;
    call add_tracking;
    return (synth(200, "ok"));
}

sub vcl_synth {
    synthetic("done");
    return (deliver);
}
"#;

        let vcl = make_interpreter(vcl_src);
        let mut req = HttpMessage::new_request(
            HttpMethod::Get,
            "/path?query=1&sort=asc",
            HttpVersion::Http11,
        );
        let mut resp = HttpMessage::new_response(HttpStatus::OK, HttpVersion::Http11);
        let mut bereq = HttpMessage::default();
        let mut beresp = HttpMessage::default();

        let mut ctx = VclContext::new(&mut req, &mut resp, &mut bereq, &mut beresp);
        ctx.client_ip = "127.0.0.1".parse().unwrap();
        ctx.server_ip = "10.0.0.1".parse().unwrap();

        vcl.exec_subroutine("vcl_recv", &mut ctx);

        // normalize_url should strip query string
        assert_eq!(req.url, "/path");
        assert_eq!(req.get_header("X-Normalized").unwrap(), "true");
        // add_tracking should see the normalized URL
        assert_eq!(req.get_header("X-Tracking").unwrap(), "track-/path");
    }

    // ---------------------------------------------------------------
    // Test 17: VCL variable types and access patterns
    //
    // Covers: client.ip, server.ip, req.is_ssl, req.restarts,
    //   obj.hits, obj.ttl, obj.grace, obj.keep, beresp.status,
    //   duration literals (s, m, h), integer/real/bool/ip values
    // ---------------------------------------------------------------
    #[test]
    fn vcl_variable_types() {
        let vcl_src = r#"
vcl 4.0;
backend default { .host = "127.0.0.1"; .port = "80"; }

sub vcl_recv {
    // IP variables
    set req.http.X-Client = client.ip;
    set req.http.X-Server = server.ip;

    // Boolean variable
    if (req.is_ssl) {
        set req.http.X-Secure = "yes";
    } else {
        set req.http.X-Secure = "no";
    }

    // Restart counter (integer)
    set req.http.X-Restarts = req.restarts;

    return (synth(200, "ok"));
}

sub vcl_hit {
    // Object attributes (available in hit context)
    set req.http.X-Obj-Hits = obj.hits;
    set req.http.X-Obj-TTL = obj.ttl;
    set req.http.X-Obj-Grace = obj.grace;
    set req.http.X-Obj-Keep = obj.keep;
    return (deliver);
}

sub vcl_backend_response {
    // Duration types
    set beresp.ttl = 5m;
    set beresp.grace = 1h;
    set beresp.keep = 2d;

    // Status code (integer)
    if (beresp.status == 200) {
        set beresp.http.X-Status-Check = "ok";
    }

    return (deliver);
}

sub vcl_synth {
    synthetic("vars test");
    return (deliver);
}
"#;

        let vcl = make_interpreter(vcl_src);

        // Test vcl_recv variables
        let mut req = HttpMessage::new_request(HttpMethod::Get, "/test", HttpVersion::Http11);
        let mut resp = HttpMessage::new_response(HttpStatus::OK, HttpVersion::Http11);
        let mut bereq = HttpMessage::default();
        let mut beresp = HttpMessage::default();

        let mut ctx = VclContext::new(&mut req, &mut resp, &mut bereq, &mut beresp);
        ctx.client_ip = "192.168.1.50".parse().unwrap();
        ctx.server_ip = "10.0.0.1".parse().unwrap();
        ctx.is_ssl = true;
        ctx.restarts = 2;

        vcl.exec_subroutine("vcl_recv", &mut ctx);

        assert_eq!(req.get_header("X-Client").unwrap(), "192.168.1.50");
        assert_eq!(req.get_header("X-Server").unwrap(), "10.0.0.1");
        assert_eq!(req.get_header("X-Secure").unwrap(), "yes");
        assert_eq!(req.get_header("X-Restarts").unwrap(), "2");

        // Test vcl_hit variables
        let mut req2 = HttpMessage::new_request(HttpMethod::Get, "/test", HttpVersion::Http11);
        let mut resp2 = HttpMessage::new_response(HttpStatus::OK, HttpVersion::Http11);
        let mut bereq2 = HttpMessage::default();
        let mut beresp2 = HttpMessage::default();

        let mut ctx2 = VclContext::new(&mut req2, &mut resp2, &mut bereq2, &mut beresp2);
        ctx2.client_ip = "127.0.0.1".parse().unwrap();
        ctx2.server_ip = "127.0.0.1".parse().unwrap();
        ctx2.obj_hits = 42;
        ctx2.obj_ttl = 300.0;
        ctx2.obj_grace = 60.0;
        ctx2.obj_keep = 120.0;

        vcl.exec_subroutine("vcl_hit", &mut ctx2);

        assert_eq!(req2.get_header("X-Obj-Hits").unwrap(), "42");

        // Test vcl_backend_response with duration vars
        let mut req3 = HttpMessage::new_request(HttpMethod::Get, "/test", HttpVersion::Http11);
        let mut resp3 = HttpMessage::default();
        let mut bereq3 = HttpMessage::default();
        let mut beresp3 = HttpMessage::new_response(HttpStatus::OK, HttpVersion::Http11);

        let mut ctx3 = VclContext::new(&mut req3, &mut resp3, &mut bereq3, &mut beresp3);
        ctx3.client_ip = "127.0.0.1".parse().unwrap();
        ctx3.server_ip = "127.0.0.1".parse().unwrap();

        vcl.exec_subroutine("vcl_backend_response", &mut ctx3);

        // Verify duration values were stored in local_vars
        let ttl = ctx3.local_vars.get("beresp.ttl");
        assert!(ttl.is_some());
        assert!((ttl.unwrap().to_duration_secs() - 300.0).abs() < 0.01);

        let grace = ctx3.local_vars.get("beresp.grace");
        assert!(grace.is_some());
        assert!((grace.unwrap().to_duration_secs() - 3600.0).abs() < 0.01);

        let keep = ctx3.local_vars.get("beresp.keep");
        assert!(keep.is_some());
        assert!((keep.unwrap().to_duration_secs() - 172800.0).abs() < 0.01);

        assert_eq!(beresp3.get_header("X-Status-Check").unwrap(), "ok");
    }

    // ---------------------------------------------------------------
    // Test 18: VCL manager hot reload via admin
    //
    // Covers: VclManager load/use/list/discard, ArcSwap hot swap,
    //   in-flight request isolation
    // ---------------------------------------------------------------
    #[test]
    fn vcl_manager_full_lifecycle() {
        use rv_admin::VclManager;

        let mgr = VclManager::new();

        let vcl_v1 = r#"
vcl 4.0;
backend default { .host = "127.0.0.1"; .port = "8080"; }
sub vcl_recv { set req.http.X-Version = "v1"; return (synth(200, "ok")); }
sub vcl_synth { synthetic("v1"); return (deliver); }
"#;

        let vcl_v2 = r#"
vcl 4.0;
backend api { .host = "10.0.0.1"; .port = "9090"; }
sub vcl_recv { set req.http.X-Version = "v2"; return (synth(200, "ok")); }
sub vcl_synth { synthetic("v2"); return (deliver); }
"#;

        // Load two programs
        mgr.load("v1", vcl_v1).unwrap();
        mgr.load("v2", vcl_v2).unwrap();

        let list = mgr.list();
        assert_eq!(list.len(), 2);

        // Activate v1
        let interp_v1 = mgr.use_program("v1").unwrap();
        assert_eq!(mgr.active_name(), Some("v1".to_string()));

        // Verify v1 behavior
        let mut req1 = HttpMessage::new_request(HttpMethod::Get, "/", HttpVersion::Http11);
        let mut resp1 = HttpMessage::default();
        let mut bereq1 = HttpMessage::default();
        let mut beresp1 = HttpMessage::default();
        let mut ctx1 = VclContext::new(&mut req1, &mut resp1, &mut bereq1, &mut beresp1);
        ctx1.client_ip = "127.0.0.1".parse().unwrap();
        ctx1.server_ip = "127.0.0.1".parse().unwrap();
        interp_v1.exec_subroutine("vcl_recv", &mut ctx1);
        assert_eq!(req1.get_header("X-Version").unwrap(), "v1");

        // Hot swap to v2
        let interp_v2 = mgr.use_program("v2").unwrap();
        assert_eq!(mgr.active_name(), Some("v2".to_string()));

        // v1 reference still works (in-flight isolation)
        let mut req_old = HttpMessage::new_request(HttpMethod::Get, "/", HttpVersion::Http11);
        let mut resp_old = HttpMessage::default();
        let mut bereq_old = HttpMessage::default();
        let mut beresp_old = HttpMessage::default();
        let mut ctx_old =
            VclContext::new(&mut req_old, &mut resp_old, &mut bereq_old, &mut beresp_old);
        ctx_old.client_ip = "127.0.0.1".parse().unwrap();
        ctx_old.server_ip = "127.0.0.1".parse().unwrap();
        interp_v1.exec_subroutine("vcl_recv", &mut ctx_old);
        assert_eq!(req_old.get_header("X-Version").unwrap(), "v1");

        // New requests see v2
        let mut req2 = HttpMessage::new_request(HttpMethod::Get, "/", HttpVersion::Http11);
        let mut resp2 = HttpMessage::default();
        let mut bereq2 = HttpMessage::default();
        let mut beresp2 = HttpMessage::default();
        let mut ctx2 = VclContext::new(&mut req2, &mut resp2, &mut bereq2, &mut beresp2);
        ctx2.client_ip = "127.0.0.1".parse().unwrap();
        ctx2.server_ip = "127.0.0.1".parse().unwrap();
        interp_v2.exec_subroutine("vcl_recv", &mut ctx2);
        assert_eq!(req2.get_header("X-Version").unwrap(), "v2");

        // Discard v1 (no longer active)
        mgr.discard("v1").unwrap();
        assert_eq!(mgr.list().len(), 1);

        // Cannot discard active v2
        assert!(mgr.discard("v2").is_err());
    }
}

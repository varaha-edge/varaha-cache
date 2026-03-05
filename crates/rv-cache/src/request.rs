use std::sync::Arc;

use rv_types::vsl::Vxid;
use rv_types::{Digest, HttpStatus, VtimReal};

use rv_http::message::HttpMessage;
use rv_storage::ObjCore;

/// States in the request processing state machine.
/// Maps to CNT_Request() states in cache_req_fsm.c.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestState {
    /// Waiting to receive the request.
    Recv,
    /// Performing cache lookup.
    Lookup,
    /// VCL determined this request should pass (no caching).
    Pass,
    /// VCL determined this request should pipe (tunnel mode).
    Pipe,
    /// Cache miss - need to fetch from backend.
    Miss,
    /// Fetching from backend.
    Fetch,
    /// Delivering response to client.
    Deliver,
    /// Generating a synthetic response.
    Synth,
    /// Request processing complete.
    Done,
}

/// The action returned by VCL subroutine execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VclAction {
    Lookup,
    Pass,
    Pipe,
    Hash,
    Purge,
    Synth,
    Restart,
    Deliver,
    Fetch,
    Retry,
    Fail,
}

/// Context for processing a single HTTP request through the cache.
pub struct RequestContext {
    /// Current state machine state.
    pub state: RequestState,
    /// Unique transaction ID.
    pub vxid: Vxid,
    /// The client request.
    pub request: HttpMessage,
    /// The response to send to client (built during processing).
    pub response: Option<HttpMessage>,
    /// VCL action from the last subroutine call.
    pub vcl_action: Option<VclAction>,
    /// Whether this request should be restarted.
    pub restarts: u32,
    /// Maximum allowed restarts.
    pub max_restarts: u32,
    /// Timestamp when the request was received.
    pub t_req: VtimReal,
    /// Synthetic response status (for synth state).
    pub synth_status: Option<HttpStatus>,
    /// Synthetic response body.
    pub synth_body: Option<Vec<u8>>,
    /// The backend request.
    pub bereq: Option<HttpMessage>,
    /// The backend response.
    pub beresp: Option<HttpMessage>,
    /// The cached object (on hit).
    pub obj: Option<Arc<ObjCore>>,
    /// Cache digest (hash of Host + URL).
    pub digest: Option<Digest>,
    /// Whether this request bypasses the cache (pass mode).
    pub is_pass: bool,
    /// The request body from the client.
    pub body: Option<Vec<u8>>,
    /// The response body to send to the client.
    pub resp_body: Option<Vec<u8>>,
    /// Whether beresp.do_gzip was set by VCL.
    pub do_gzip: bool,
    /// Whether beresp.do_gunzip was set by VCL.
    pub do_gunzip: bool,
    /// Whether the response is uncacheable.
    pub uncacheable: bool,
    /// beresp.ttl set by VCL (seconds).
    pub beresp_ttl: Option<f64>,
    /// beresp.grace set by VCL (seconds).
    pub beresp_grace: Option<f64>,
    /// beresp.keep set by VCL (seconds).
    pub beresp_keep: Option<f64>,
}

impl RequestContext {
    pub fn new(request: HttpMessage, vxid: Vxid) -> Self {
        Self {
            state: RequestState::Recv,
            vxid,
            request,
            response: None,
            vcl_action: None,
            restarts: 0,
            max_restarts: 4,
            t_req: VtimReal::now(),
            synth_status: None,
            synth_body: None,
            bereq: None,
            beresp: None,
            obj: None,
            digest: None,
            is_pass: false,
            body: None,
            resp_body: None,
            do_gzip: false,
            do_gunzip: false,
            uncacheable: false,
            beresp_ttl: None,
            beresp_grace: None,
            beresp_keep: None,
        }
    }
}

/// The request FSM drives a request through its states.
pub struct RequestFsm;

impl RequestFsm {
    /// Advance the request to its next state based on current state and VCL action.
    pub fn step(ctx: &mut RequestContext) -> RequestState {
        let next = match ctx.state {
            RequestState::Recv => Self::handle_recv(ctx),
            RequestState::Lookup => Self::handle_lookup(ctx),
            RequestState::Pass => RequestState::Fetch,
            RequestState::Pipe => RequestState::Done,
            RequestState::Miss => RequestState::Fetch,
            RequestState::Fetch => RequestState::Deliver,
            RequestState::Deliver => RequestState::Done,
            RequestState::Synth => RequestState::Deliver,
            RequestState::Done => RequestState::Done,
        };
        ctx.state = next;
        next
    }

    fn handle_recv(ctx: &RequestContext) -> RequestState {
        match ctx.vcl_action {
            Some(VclAction::Lookup) | Some(VclAction::Hash) => RequestState::Lookup,
            Some(VclAction::Pass) => RequestState::Pass,
            Some(VclAction::Pipe) => RequestState::Pipe,
            Some(VclAction::Synth) => RequestState::Synth,
            Some(VclAction::Purge) => RequestState::Lookup,
            _ => RequestState::Lookup, // Default: try cache
        }
    }

    fn handle_lookup(ctx: &RequestContext) -> RequestState {
        match ctx.vcl_action {
            Some(VclAction::Deliver) => RequestState::Deliver,
            Some(VclAction::Pass) => RequestState::Pass,
            Some(VclAction::Fetch) => RequestState::Miss,
            Some(VclAction::Synth) => RequestState::Synth,
            Some(VclAction::Restart) => RequestState::Recv,
            _ => RequestState::Miss, // Default on miss
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rv_http::message::HttpVersion;
    use rv_types::HttpMethod;

    fn make_request() -> RequestContext {
        let msg = HttpMessage::new_request(HttpMethod::Get, "/test", HttpVersion::Http11);
        RequestContext::new(msg, Vxid(1))
    }

    #[test]
    fn test_fsm_default_flow() {
        let mut ctx = make_request();
        assert_eq!(ctx.state, RequestState::Recv);

        // Recv -> Lookup (default when no VCL action)
        let next = RequestFsm::step(&mut ctx);
        assert_eq!(next, RequestState::Lookup);

        // Lookup -> Miss (default when no VCL action, cache miss)
        let next = RequestFsm::step(&mut ctx);
        assert_eq!(next, RequestState::Miss);

        // Miss -> Fetch
        let next = RequestFsm::step(&mut ctx);
        assert_eq!(next, RequestState::Fetch);

        // Fetch -> Deliver
        let next = RequestFsm::step(&mut ctx);
        assert_eq!(next, RequestState::Deliver);

        // Deliver -> Done
        let next = RequestFsm::step(&mut ctx);
        assert_eq!(next, RequestState::Done);
    }

    #[test]
    fn test_fsm_pass_flow() {
        let mut ctx = make_request();
        ctx.vcl_action = Some(VclAction::Pass);

        // Recv -> Pass
        let next = RequestFsm::step(&mut ctx);
        assert_eq!(next, RequestState::Pass);

        // Pass -> Fetch
        let next = RequestFsm::step(&mut ctx);
        assert_eq!(next, RequestState::Fetch);
    }

    #[test]
    fn test_fsm_synth_flow() {
        let mut ctx = make_request();
        ctx.vcl_action = Some(VclAction::Synth);

        // Recv -> Synth
        let next = RequestFsm::step(&mut ctx);
        assert_eq!(next, RequestState::Synth);

        // Synth -> Deliver
        let next = RequestFsm::step(&mut ctx);
        assert_eq!(next, RequestState::Deliver);
    }

    #[test]
    fn test_fsm_pipe_flow() {
        let mut ctx = make_request();
        ctx.vcl_action = Some(VclAction::Pipe);

        // Recv -> Pipe
        let next = RequestFsm::step(&mut ctx);
        assert_eq!(next, RequestState::Pipe);

        // Pipe -> Done
        let next = RequestFsm::step(&mut ctx);
        assert_eq!(next, RequestState::Done);
    }

    #[test]
    fn test_fsm_hit_deliver() {
        let mut ctx = make_request();

        // Recv -> Lookup
        RequestFsm::step(&mut ctx);
        assert_eq!(ctx.state, RequestState::Lookup);

        // VCL says deliver (cache hit)
        ctx.vcl_action = Some(VclAction::Deliver);
        let next = RequestFsm::step(&mut ctx);
        assert_eq!(next, RequestState::Deliver);
    }
}

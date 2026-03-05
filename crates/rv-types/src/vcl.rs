use serde::{Deserialize, Serialize};

/// VCL subroutine identifiers.
/// Maps to the VCL subroutines that the cache engine calls at various stages.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum VclSubroutine {
    VclRecv,
    VclHash,
    VclHit,
    VclMiss,
    VclPass,
    VclDeliver,
    VclSynth,
    VclPurge,
    VclPipe,
    VclBackendFetch,
    VclBackendResponse,
    VclBackendError,
    VclInit,
    VclFini,
}

impl VclSubroutine {
    pub fn name(&self) -> &'static str {
        match self {
            Self::VclRecv => "vcl_recv",
            Self::VclHash => "vcl_hash",
            Self::VclHit => "vcl_hit",
            Self::VclMiss => "vcl_miss",
            Self::VclPass => "vcl_pass",
            Self::VclDeliver => "vcl_deliver",
            Self::VclSynth => "vcl_synth",
            Self::VclPurge => "vcl_purge",
            Self::VclPipe => "vcl_pipe",
            Self::VclBackendFetch => "vcl_backend_fetch",
            Self::VclBackendResponse => "vcl_backend_response",
            Self::VclBackendError => "vcl_backend_error",
            Self::VclInit => "vcl_init",
            Self::VclFini => "vcl_fini",
        }
    }

    pub fn valid_returns(&self) -> &'static [VclAction] {
        match self {
            Self::VclRecv => &[
                VclAction::Pass,
                VclAction::Pipe,
                VclAction::Hash,
                VclAction::Synth,
                VclAction::Purge,
                VclAction::Fail,
            ],
            Self::VclHash => &[VclAction::Lookup],
            Self::VclHit => &[
                VclAction::Deliver,
                VclAction::Pass,
                VclAction::Restart,
                VclAction::Synth,
                VclAction::Fail,
            ],
            Self::VclMiss => &[
                VclAction::Fetch,
                VclAction::Pass,
                VclAction::Restart,
                VclAction::Synth,
                VclAction::Fail,
            ],
            Self::VclPass => &[
                VclAction::Fetch,
                VclAction::Restart,
                VclAction::Synth,
                VclAction::Fail,
            ],
            Self::VclDeliver => &[VclAction::Deliver, VclAction::Restart, VclAction::Synth],
            Self::VclSynth => &[VclAction::Deliver, VclAction::Restart],
            Self::VclPurge => &[VclAction::Synth, VclAction::Restart],
            Self::VclPipe => &[VclAction::Pipe],
            Self::VclBackendFetch => &[
                VclAction::Fetch,
                VclAction::Abandon,
                VclAction::Fail,
                VclAction::Error,
            ],
            Self::VclBackendResponse => &[
                VclAction::Deliver,
                VclAction::Retry,
                VclAction::Abandon,
                VclAction::Fail,
                VclAction::Error,
                VclAction::Pass,
            ],
            Self::VclBackendError => &[
                VclAction::Deliver,
                VclAction::Retry,
                VclAction::Abandon,
                VclAction::Fail,
            ],
            Self::VclInit => &[VclAction::Ok, VclAction::Fail],
            Self::VclFini => &[VclAction::Ok],
        }
    }
}

impl std::fmt::Display for VclSubroutine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
    }
}

/// VCL return actions.
/// The set of valid return values from VCL subroutines.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum VclAction {
    Deliver,
    Pass,
    Pipe,
    Hash,
    Lookup,
    Fetch,
    Synth,
    Purge,
    Restart,
    Retry,
    Abandon,
    Fail,
    Error,
    Ok,
}

impl VclAction {
    pub fn name(&self) -> &'static str {
        match self {
            Self::Deliver => "deliver",
            Self::Pass => "pass",
            Self::Pipe => "pipe",
            Self::Hash => "hash",
            Self::Lookup => "lookup",
            Self::Fetch => "fetch",
            Self::Synth => "synth",
            Self::Purge => "purge",
            Self::Restart => "restart",
            Self::Retry => "retry",
            Self::Abandon => "abandon",
            Self::Fail => "fail",
            Self::Error => "error",
            Self::Ok => "ok",
        }
    }
}

impl std::fmt::Display for VclAction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
    }
}

/// VCL lifecycle events.
/// Mapped from vcl_event_e
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum VclEvent {
    Load,
    Warm,
    Cold,
    Discard,
    Use,
}

impl VclEvent {
    pub fn name(&self) -> &'static str {
        match self {
            Self::Load => "load",
            Self::Warm => "warm",
            Self::Cold => "cold",
            Self::Discard => "discard",
            Self::Use => "use",
        }
    }
}

use serde::{Deserialize, Serialize};

/// Busy Object Core state.
/// Mapped from include/tbl/boc_state.h
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[repr(u8)]
pub enum BocState {
    /// Don't touch yet
    Invalid = 0,
    /// bereq.* can be examined
    ReqDone = 1,
    /// beresp.* can be examined, streaming in progress
    Stream = 2,
    /// Object is complete
    Finished = 3,
    /// Something went wrong
    Failed = 4,
}

impl BocState {
    pub fn name(&self) -> &'static str {
        match self {
            Self::Invalid => "invalid",
            Self::ReqDone => "req_done",
            Self::Stream => "stream",
            Self::Finished => "finished",
            Self::Failed => "failed",
        }
    }

    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Finished | Self::Failed)
    }
}

impl std::fmt::Display for BocState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
    }
}

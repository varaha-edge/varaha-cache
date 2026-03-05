use serde::{Deserialize, Serialize};

/// Task priority/queue type.
/// Mapped from cache.h enum task_prio
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[repr(u8)]
pub enum TaskPriority {
    /// Backend operation
    BackendOp = 0,
    /// Request returning from waiting list
    Rush = 1,
    /// New request (H1/H2)
    Request = 2,
    /// New streaming request
    Stream = 3,
    /// VCA tasks
    Vca = 4,
    /// Background tasks
    Background = 5,
}

impl TaskPriority {
    pub fn name(&self) -> &'static str {
        match self {
            Self::BackendOp => "BO",
            Self::Rush => "RUSH",
            Self::Request => "REQ",
            Self::Stream => "STR",
            Self::Vca => "VCA",
            Self::Background => "BG",
        }
    }
}

impl std::fmt::Display for TaskPriority {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
    }
}

use serde::{Deserialize, Serialize};

/// Body status - describes how the body is being transferred.
/// Mapped from include/tbl/body_status.h
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[repr(u8)]
pub enum BodyStatus {
    None = 0,
    Error = 1,
    Chunked = 2,
    Length = 3,
    Eof = 4,
    Taken = 5,
    Cached = 6,
}

impl BodyStatus {
    pub fn name(&self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Error => "error",
            Self::Chunked => "chunked",
            Self::Length => "length",
            Self::Eof => "eof",
            Self::Taken => "taken",
            Self::Cached => "cached",
        }
    }

    /// Whether the body is available for reading.
    /// -1 = error, 0 = not available, 1 = available, 2 = cached
    pub fn available(&self) -> i8 {
        match self {
            Self::None => 0,
            Self::Error => -1,
            Self::Chunked => 1,
            Self::Length => 1,
            Self::Eof => 1,
            Self::Taken => 0,
            Self::Cached => 2,
        }
    }

    /// Whether the total length is known in advance.
    pub fn length_known(&self) -> bool {
        matches!(self, Self::None | Self::Length | Self::Cached)
    }
}

impl std::fmt::Display for BodyStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
    }
}

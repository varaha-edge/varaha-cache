use serde::{Deserialize, Serialize};

/// Well-known HTTP methods.
/// Mapped from include/tbl/well_known_methods.h
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[repr(u8)]
pub enum HttpMethod {
    Get = 0,
    Head = 1,
    Post = 2,
    Put = 3,
    Delete = 4,
    Options = 5,
    Trace = 6,
    Patch = 7,
    Connect = 8,
    Unknown = 31,
}

impl HttpMethod {
    pub fn name(&self) -> &'static str {
        match self {
            Self::Get => "GET",
            Self::Head => "HEAD",
            Self::Post => "POST",
            Self::Put => "PUT",
            Self::Delete => "DELETE",
            Self::Options => "OPTIONS",
            Self::Trace => "TRACE",
            Self::Patch => "PATCH",
            Self::Connect => "CONNECT",
            Self::Unknown => "UNKNOWN",
        }
    }

    pub fn parse_method(s: &str) -> Self {
        s.parse().unwrap_or(Self::Unknown)
    }

    /// Whether this method is considered safe (no side effects).
    pub fn is_safe(&self) -> bool {
        matches!(self, Self::Get | Self::Head | Self::Options | Self::Trace)
    }

    /// Whether this method is idempotent.
    pub fn is_idempotent(&self) -> bool {
        matches!(
            self,
            Self::Get | Self::Head | Self::Put | Self::Delete | Self::Options | Self::Trace
        )
    }

    /// Whether this method may have a request body.
    pub fn may_have_body(&self) -> bool {
        matches!(self, Self::Post | Self::Put | Self::Patch)
    }
}

impl std::str::FromStr for HttpMethod {
    type Err = std::convert::Infallible;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(match s {
            "GET" => Self::Get,
            "HEAD" => Self::Head,
            "POST" => Self::Post,
            "PUT" => Self::Put,
            "DELETE" => Self::Delete,
            "OPTIONS" => Self::Options,
            "TRACE" => Self::Trace,
            "PATCH" => Self::Patch,
            "CONNECT" => Self::Connect,
            _ => Self::Unknown,
        })
    }
}

impl std::fmt::Display for HttpMethod {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
    }
}

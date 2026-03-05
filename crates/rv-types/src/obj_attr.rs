use serde::{Deserialize, Serialize};

/// Object attributes stored per cache object.
/// Mapped from include/tbl/obj_attr.h
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ObjAttr {
    /// Object length (u64, fixed 8 bytes)
    Len,
    /// VXID (u64, fixed 8 bytes)
    Vxid,
    /// Object flags (u8, fixed 1 byte)
    Flags,
    /// Gzip bits (fixed 32 bytes)
    GzipBits,
    /// Last-Modified timestamp (f64, fixed 8 bytes)
    LastModified,
    /// Vary matching data (variable length)
    Vary,
    /// Stored HTTP headers (variable length)
    Headers,
    /// ESI data (auxiliary, variable length)
    EsiData,
}

impl ObjAttr {
    /// Fixed size for fixed-size attributes, None for variable/auxiliary.
    pub fn fixed_size(&self) -> Option<usize> {
        match self {
            Self::Len => Some(8),          // sizeof(uint64_t)
            Self::Vxid => Some(8),         // sizeof(uint64_t)
            Self::Flags => Some(1),        // 1 byte
            Self::GzipBits => Some(32),    // 32 bytes
            Self::LastModified => Some(8), // sizeof(double)
            Self::Vary => None,
            Self::Headers => None,
            Self::EsiData => None,
        }
    }

    /// Whether this is a fixed-size attribute.
    pub fn is_fixed(&self) -> bool {
        self.fixed_size().is_some()
    }

    /// Whether this is a variable-size attribute.
    pub fn is_variable(&self) -> bool {
        matches!(self, Self::Vary | Self::Headers)
    }

    /// Whether this is an auxiliary attribute.
    pub fn is_auxiliary(&self) -> bool {
        matches!(self, Self::EsiData)
    }

    pub fn name(&self) -> &'static str {
        match self {
            Self::Len => "len",
            Self::Vxid => "vxid",
            Self::Flags => "flags",
            Self::GzipBits => "gzipbits",
            Self::LastModified => "lastmodified",
            Self::Vary => "vary",
            Self::Headers => "headers",
            Self::EsiData => "esidata",
        }
    }
}

impl std::fmt::Display for ObjAttr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
    }
}

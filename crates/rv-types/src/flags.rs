use serde::{Deserialize, Serialize};

/// Object flags stored in OA_FLAGS attribute.
/// Mapped from include/tbl/obj_attr.h OBJ_FLAG entries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
pub struct ObjFlags(u8);

impl ObjFlags {
    pub const GZIPED: Self = Self(1 << 1);
    pub const CHGCE: Self = Self(1 << 2);
    pub const IMSCAND: Self = Self(1 << 3);
    pub const ESIPROC: Self = Self(1 << 4);

    pub fn empty() -> Self {
        Self(0)
    }

    pub fn contains(self, other: Self) -> bool {
        (self.0 & other.0) == other.0
    }

    pub fn insert(&mut self, other: Self) {
        self.0 |= other.0;
    }

    pub fn remove(&mut self, other: Self) {
        self.0 &= !other.0;
    }

    pub fn bits(self) -> u8 {
        self.0
    }

    pub fn from_bits(bits: u8) -> Self {
        Self(bits)
    }
}

impl std::ops::BitOr for ObjFlags {
    type Output = Self;
    fn bitor(self, rhs: Self) -> Self {
        Self(self.0 | rhs.0)
    }
}

impl std::ops::BitAnd for ObjFlags {
    type Output = Self;
    fn bitand(self, rhs: Self) -> Self {
        Self(self.0 & rhs.0)
    }
}

/// Object Core flags.
/// Mapped from include/tbl/oc_flags.h
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
pub struct ObjCoreFlags(u8);

impl ObjCoreFlags {
    pub const WITHDRAWN: Self = Self(1 << 0);
    pub const BUSY: Self = Self(1 << 1);
    pub const HFM: Self = Self(1 << 2);
    pub const HFP: Self = Self(1 << 3);
    pub const CANCEL: Self = Self(1 << 4);
    pub const PRIVATE: Self = Self(1 << 5);
    pub const FAILED: Self = Self(1 << 6);
    pub const DYING: Self = Self(1 << 7);

    pub fn empty() -> Self {
        Self(0)
    }

    pub fn contains(self, other: Self) -> bool {
        (self.0 & other.0) == other.0
    }

    pub fn insert(&mut self, other: Self) {
        self.0 |= other.0;
    }

    pub fn remove(&mut self, other: Self) {
        self.0 &= !other.0;
    }

    /// OC_F_TRANSIENT = OC_F_PRIVATE | OC_F_HFM | OC_F_HFP
    pub fn is_transient(self) -> bool {
        self.contains(Self::PRIVATE) || self.contains(Self::HFM) || self.contains(Self::HFP)
    }

    pub fn bits(self) -> u8 {
        self.0
    }

    pub fn from_bits(bits: u8) -> Self {
        Self(bits)
    }
}

impl std::ops::BitOr for ObjCoreFlags {
    type Output = Self;
    fn bitor(self, rhs: Self) -> Self {
        Self(self.0 | rhs.0)
    }
}

impl std::ops::BitAnd for ObjCoreFlags {
    type Output = Self;
    fn bitand(self, rhs: Self) -> Self {
        Self(self.0 & rhs.0)
    }
}

/// Object Core expiry flags.
/// Mapped from include/tbl/oc_exp_flags.h
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
pub struct ObjExpFlags(u8);

impl ObjExpFlags {
    pub const POSTED: Self = Self(1 << 1);
    pub const REFD: Self = Self(1 << 2);
    pub const MOVE: Self = Self(1 << 3);
    pub const INSERT: Self = Self(1 << 4);
    pub const REMOVE: Self = Self(1 << 5);
    pub const NEW: Self = Self(1 << 6);

    pub fn empty() -> Self {
        Self(0)
    }

    pub fn contains(self, other: Self) -> bool {
        (self.0 & other.0) == other.0
    }

    pub fn insert(&mut self, other: Self) {
        self.0 |= other.0;
    }

    pub fn remove(&mut self, other: Self) {
        self.0 &= !other.0;
    }

    pub fn bits(self) -> u8 {
        self.0
    }

    pub fn from_bits(bits: u8) -> Self {
        Self(bits)
    }
}

impl std::ops::BitOr for ObjExpFlags {
    type Output = Self;
    fn bitor(self, rhs: Self) -> Self {
        Self(self.0 | rhs.0)
    }
}

/// Request flags.
/// Mapped from include/tbl/req_flags.h
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
pub struct ReqFlags {
    pub disable_esi: bool,
    pub hash_ignore_busy: bool,
    pub hash_ignore_vary: bool,
    pub hash_always_miss: bool,
    pub is_hit: bool,
    pub want100cont: bool,
    pub late100cont: bool,
    pub req_reset: bool,
    pub res_esi: bool,
    pub res_pipe: bool,
}

/// Shared request/backend-request flags.
/// Mapped from include/tbl/req_bereq_flags.h
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
pub struct ReqBereqFlags {
    pub is_hitmiss: bool,
    pub is_hitpass: bool,
    pub trace: bool,
}

/// Backend request flags.
/// Mapped from include/tbl/bereq_flags.h
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
pub struct BereqFlags {
    pub uncacheable: bool,
    pub is_bgfetch: bool,
}

/// Backend response flags.
/// Mapped from include/tbl/beresp_flags.h
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
pub struct BerespFlags {
    pub do_esi: bool,
    pub do_gzip: bool,
    pub do_gunzip: bool,
    pub do_stream: bool,
    pub was_304: bool,
}

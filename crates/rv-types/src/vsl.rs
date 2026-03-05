use serde::{Deserialize, Serialize};

/// VSL (Varnish Shared Log) tags.
/// Mapped from include/tbl/vsl_tags.h - common tags used for logging.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[repr(u16)]
pub enum LogTag {
    // Session/Connection
    SessOpen = 1,
    SessClose = 2,
    // Request lifecycle
    ReqStart = 3,
    ReqMethod = 4,
    ReqURL = 5,
    ReqProtocol = 6,
    ReqStatus = 7,
    ReqAcct = 8,
    ReqEnd = 9,
    ReqHeader = 15,
    ReqUnset = 16,
    // Response
    RespStatus = 10,
    RespReason = 11,
    RespProtocol = 12,
    RespHeader = 13,
    RespUnset = 14,
    // Backend request
    BereqMethod = 20,
    BereqURL = 21,
    BereqProtocol = 22,
    BereqHeader = 23,
    BereqUnset = 24,
    // Backend response
    BerespStatus = 30,
    BerespReason = 31,
    BerespProtocol = 32,
    BerespHeader = 33,
    BerespUnset = 34,
    // Object lifecycle
    ObjStatus = 40,
    ObjReason = 41,
    ObjProtocol = 42,
    ObjHeader = 43,
    ObjUnset = 44,
    // VCL
    VclCall = 50,
    VclReturn = 51,
    VclLog = 52,
    VclError = 53,
    // Cache
    Hit = 60,
    HitMiss = 61,
    HitPass = 62,
    Miss = 63,
    Pass = 64,
    Pipe = 65,
    // Backend
    Backend = 70,
    BackendOpen = 71,
    BackendClose = 72,
    BackendReuse = 73,
    // Timing
    Timestamp = 80,
    Length = 81,
    // Hash
    Hash = 90,
    // Bans
    BanLurker = 100,
    // Debug/Internal
    Debug = 110,
    Error = 111,
    // Fetch
    Fetch = 120,
    FetchError = 121,
    // Gzip
    Gzip = 130,
    // ESI
    Esi = 140,
    // Filters
    VfpAcct = 150,
    VdpAcct = 151,
    // H2
    H2RxHdr = 160,
    H2RxBody = 161,
    H2TxHdr = 162,
    H2TxBody = 163,
    // Proxy protocol
    Proxy = 170,
    // Link
    Link = 180,
    Begin = 181,
    End = 182,
    // Storage
    Storage = 190,
    // TTL
    TTL = 200,
    // Expiry
    ExpBan = 210,
    ExpKill = 211,
}

impl LogTag {
    pub fn name(&self) -> &'static str {
        match self {
            Self::SessOpen => "SessOpen",
            Self::SessClose => "SessClose",
            Self::ReqStart => "ReqStart",
            Self::ReqMethod => "ReqMethod",
            Self::ReqURL => "ReqURL",
            Self::ReqProtocol => "ReqProtocol",
            Self::ReqStatus => "ReqStatus",
            Self::ReqAcct => "ReqAcct",
            Self::ReqEnd => "ReqEnd",
            Self::ReqHeader => "ReqHeader",
            Self::ReqUnset => "ReqUnset",
            Self::RespStatus => "RespStatus",
            Self::RespReason => "RespReason",
            Self::RespProtocol => "RespProtocol",
            Self::RespHeader => "RespHeader",
            Self::RespUnset => "RespUnset",
            Self::BereqMethod => "BereqMethod",
            Self::BereqURL => "BereqURL",
            Self::BereqProtocol => "BereqProtocol",
            Self::BereqHeader => "BereqHeader",
            Self::BereqUnset => "BereqUnset",
            Self::BerespStatus => "BerespStatus",
            Self::BerespReason => "BerespReason",
            Self::BerespProtocol => "BerespProtocol",
            Self::BerespHeader => "BerespHeader",
            Self::BerespUnset => "BerespUnset",
            Self::ObjStatus => "ObjStatus",
            Self::ObjReason => "ObjReason",
            Self::ObjProtocol => "ObjProtocol",
            Self::ObjHeader => "ObjHeader",
            Self::ObjUnset => "ObjUnset",
            Self::VclCall => "VCL_call",
            Self::VclReturn => "VCL_return",
            Self::VclLog => "VCL_Log",
            Self::VclError => "VCL_Error",
            Self::Hit => "Hit",
            Self::HitMiss => "HitMiss",
            Self::HitPass => "HitPass",
            Self::Miss => "Miss",
            Self::Pass => "Pass",
            Self::Pipe => "Pipe",
            Self::Backend => "Backend",
            Self::BackendOpen => "BackendOpen",
            Self::BackendClose => "BackendClose",
            Self::BackendReuse => "BackendReuse",
            Self::Timestamp => "Timestamp",
            Self::Length => "Length",
            Self::Hash => "Hash",
            Self::BanLurker => "BanLurker",
            Self::Debug => "Debug",
            Self::Error => "Error",
            Self::Fetch => "Fetch",
            Self::FetchError => "FetchError",
            Self::Gzip => "Gzip",
            Self::Esi => "ESI",
            Self::VfpAcct => "VfpAcct",
            Self::VdpAcct => "VdpAcct",
            Self::H2RxHdr => "H2RxHdr",
            Self::H2RxBody => "H2RxBody",
            Self::H2TxHdr => "H2TxHdr",
            Self::H2TxBody => "H2TxBody",
            Self::Proxy => "Proxy",
            Self::Link => "Link",
            Self::Begin => "Begin",
            Self::End => "End",
            Self::Storage => "Storage",
            Self::TTL => "TTL",
            Self::ExpBan => "ExpBan",
            Self::ExpKill => "ExpKill",
        }
    }
}

impl std::fmt::Display for LogTag {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
    }
}

/// VXID - Varnish transaction ID.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
pub struct Vxid(pub u64);

impl Vxid {
    pub fn new(id: u64) -> Self {
        Self(id)
    }

    pub fn is_zero(&self) -> bool {
        self.0 == 0
    }
}

impl std::fmt::Display for Vxid {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

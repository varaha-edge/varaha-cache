use serde::{Deserialize, Serialize};
use std::net::SocketAddr;

/// Session attributes.
/// Mapped from include/tbl/sess_attr.h
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SessAttr {
    Transport,
    RemoteAddr,
    LocalAddr,
    ClientAddr,
    ServerAddr,
    ClientIp,
    ClientPort,
    ProxyTlv,
    ProtoPriv,
}

impl SessAttr {
    pub fn name(&self) -> &'static str {
        match self {
            Self::Transport => "transport",
            Self::RemoteAddr => "remote_addr",
            Self::LocalAddr => "local_addr",
            Self::ClientAddr => "client_addr",
            Self::ServerAddr => "server_addr",
            Self::ClientIp => "client_ip",
            Self::ClientPort => "client_port",
            Self::ProxyTlv => "proxy_tlv",
            Self::ProtoPriv => "proto_priv",
        }
    }
}

/// Session address information.
#[derive(Debug, Clone)]
pub struct SessionAddrs {
    pub remote_addr: Option<SocketAddr>,
    pub local_addr: Option<SocketAddr>,
    pub client_addr: Option<SocketAddr>,
    pub server_addr: Option<SocketAddr>,
}

/// Request accounting fields.
/// Mapped from include/tbl/acct_fields_req.h
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct SessionAccounting {
    pub req_hdrbytes: u64,
    pub req_bodybytes: u64,
    pub resp_hdrbytes: u64,
    pub resp_bodybytes: u64,
}

/// Backend request accounting fields.
/// Mapped from include/tbl/acct_fields_bereq.h
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct BackendAccounting {
    pub bereq_hdrbytes: u64,
    pub bereq_bodybytes: u64,
    pub beresp_hdrbytes: u64,
    pub beresp_bodybytes: u64,
}

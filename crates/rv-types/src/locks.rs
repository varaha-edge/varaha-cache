use serde::{Deserialize, Serialize};

/// Lock kinds used throughout Varnish for mutex identification.
/// Mapped from include/tbl/locks.h
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum LockKind {
    Ban,
    BusyObj,
    Cli,
    Director,
    Exp,
    Hcb,
    Lru,
    Mempool,
    ObjHdr,
    PerPool,
    PipeStat,
    Probe,
    Sess,
    ConnPool,
    DeadPool,
    Vbe,
    VcaPace,
    VcaShut,
    Vcl,
    Vxid,
    Waiter,
    Wq,
    Wstat,
}

impl LockKind {
    pub fn name(&self) -> &'static str {
        match self {
            Self::Ban => "ban",
            Self::BusyObj => "busyobj",
            Self::Cli => "cli",
            Self::Director => "director",
            Self::Exp => "exp",
            Self::Hcb => "hcb",
            Self::Lru => "lru",
            Self::Mempool => "mempool",
            Self::ObjHdr => "objhdr",
            Self::PerPool => "perpool",
            Self::PipeStat => "pipestat",
            Self::Probe => "probe",
            Self::Sess => "sess",
            Self::ConnPool => "conn_pool",
            Self::DeadPool => "dead_pool",
            Self::Vbe => "vbe",
            Self::VcaPace => "vcapace",
            Self::VcaShut => "vcashut",
            Self::Vcl => "vcl",
            Self::Vxid => "vxid",
            Self::Waiter => "waiter",
            Self::Wq => "wq",
            Self::Wstat => "wstat",
        }
    }
}

impl std::fmt::Display for LockKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
    }
}

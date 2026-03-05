use std::net::SocketAddr;
use std::sync::Arc;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum BackendError {
    #[error("backend connection failed: {0}")]
    ConnectionFailed(String),
    #[error("backend timeout")]
    Timeout,
    #[error("no healthy backend available")]
    NoHealthyBackend,
    #[error("backend I/O error: {0}")]
    Io(#[from] std::io::Error),
}

/// Administrative health status for a backend.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdminHealth {
    /// Probes determine health
    Probe,
    /// Administratively forced healthy
    Healthy,
    /// Administratively forced sick
    Sick,
    /// Backend was deleted
    Deleted,
}

impl AdminHealth {
    pub fn name(&self) -> &'static str {
        match self {
            Self::Probe => "probe",
            Self::Healthy => "healthy",
            Self::Sick => "sick",
            Self::Deleted => "deleted",
        }
    }
}

impl std::fmt::Display for AdminHealth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
    }
}

/// Entry in a director listing.
#[derive(Debug, Clone)]
pub struct DirectorListEntry {
    pub name: String,
    pub admin_health: AdminHealth,
    pub is_healthy: bool,
    pub description: String,
}

/// A backend server that can handle HTTP requests.
pub trait Backend: Send + Sync {
    fn name(&self) -> &str;
    fn addr(&self) -> SocketAddr;
    fn is_healthy(&self) -> bool;
    fn admin_health(&self) -> AdminHealth;
    fn set_admin_health(&self, health: AdminHealth);
}

/// A director selects which backend to use for a request.
/// Based on vdi_methods from vrt.h.
pub trait Director: Send + Sync {
    fn name(&self) -> &str;

    /// Resolve this director to a concrete backend.
    fn resolve(&self) -> Option<Arc<dyn Backend>>;

    /// Check if any backend in this director is healthy.
    fn healthy(&self) -> bool;

    /// Get the administrative health override.
    fn admin_health(&self) -> AdminHealth;

    /// List all backends in this director.
    fn list(&self) -> Vec<DirectorListEntry>;
}

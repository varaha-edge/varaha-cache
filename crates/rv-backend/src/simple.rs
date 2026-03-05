use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use crate::health::HealthProbe;
use crate::traits::{AdminHealth, Backend};

/// A simple backend server with a fixed address.
pub struct SimpleBackend {
    name: String,
    addr: SocketAddr,
    admin_health: Mutex<AdminHealth>,
    probe: Option<Arc<HealthProbe>>,
}

impl SimpleBackend {
    pub fn new(name: impl Into<String>, addr: SocketAddr) -> Self {
        Self {
            name: name.into(),
            addr,
            admin_health: Mutex::new(AdminHealth::Probe),
            probe: None,
        }
    }

    /// Create a backend with an attached health probe.
    pub fn with_probe(name: impl Into<String>, addr: SocketAddr, probe: Arc<HealthProbe>) -> Self {
        Self {
            name: name.into(),
            addr,
            admin_health: Mutex::new(AdminHealth::Probe),
            probe: Some(probe),
        }
    }

    /// Get the health probe, if configured.
    pub fn probe(&self) -> Option<&Arc<HealthProbe>> {
        self.probe.as_ref()
    }
}

impl Backend for SimpleBackend {
    fn name(&self) -> &str {
        &self.name
    }

    fn addr(&self) -> SocketAddr {
        self.addr
    }

    fn is_healthy(&self) -> bool {
        let ah = *self.admin_health.lock().unwrap();
        match ah {
            AdminHealth::Healthy => true,
            AdminHealth::Sick | AdminHealth::Deleted => false,
            AdminHealth::Probe => {
                // Check actual probe if configured
                match &self.probe {
                    Some(probe) => probe.is_healthy(),
                    None => true, // no probe configured, assume healthy
                }
            }
        }
    }

    fn admin_health(&self) -> AdminHealth {
        *self.admin_health.lock().unwrap()
    }

    fn set_admin_health(&self, health: AdminHealth) {
        *self.admin_health.lock().unwrap() = health;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_simple_backend() {
        let addr: SocketAddr = "127.0.0.1:8080".parse().unwrap();
        let be = SimpleBackend::new("default", addr);
        assert_eq!(be.name(), "default");
        assert_eq!(be.addr(), addr);
        assert!(be.is_healthy());

        be.set_admin_health(AdminHealth::Sick);
        assert!(!be.is_healthy());

        be.set_admin_health(AdminHealth::Healthy);
        assert!(be.is_healthy());
    }
}

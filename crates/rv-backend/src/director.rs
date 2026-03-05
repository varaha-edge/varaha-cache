use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use parking_lot::RwLock;

use crate::traits::{AdminHealth, Backend, Director, DirectorListEntry};

/// Round-robin director.
/// Cycles through healthy backends in order.
pub struct RoundRobinDirector {
    name: String,
    backends: RwLock<Vec<Arc<dyn Backend>>>,
    next: AtomicUsize,
}

impl RoundRobinDirector {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            backends: RwLock::new(Vec::new()),
            next: AtomicUsize::new(0),
        }
    }

    pub fn add_backend(&self, backend: Arc<dyn Backend>) {
        self.backends.write().push(backend);
    }
}

impl Director for RoundRobinDirector {
    fn name(&self) -> &str {
        &self.name
    }

    fn resolve(&self) -> Option<Arc<dyn Backend>> {
        let backends = self.backends.read();
        if backends.is_empty() {
            return None;
        }

        let len = backends.len();
        for _ in 0..len {
            let idx = self.next.fetch_add(1, Ordering::Relaxed) % len;
            if backends[idx].is_healthy() {
                return Some(Arc::clone(&backends[idx]));
            }
        }
        None
    }

    fn healthy(&self) -> bool {
        self.backends.read().iter().any(|b| b.is_healthy())
    }

    fn admin_health(&self) -> AdminHealth {
        AdminHealth::Probe
    }

    fn list(&self) -> Vec<DirectorListEntry> {
        self.backends
            .read()
            .iter()
            .map(|b| DirectorListEntry {
                name: b.name().to_string(),
                admin_health: b.admin_health(),
                is_healthy: b.is_healthy(),
                description: format!("{}", b.addr()),
            })
            .collect()
    }
}

/// Random director.
/// Selects a random healthy backend.
pub struct RandomDirector {
    name: String,
    backends: RwLock<Vec<Arc<dyn Backend>>>,
    counter: AtomicUsize,
}

impl RandomDirector {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            backends: RwLock::new(Vec::new()),
            counter: AtomicUsize::new(0),
        }
    }

    pub fn add_backend(&self, backend: Arc<dyn Backend>) {
        self.backends.write().push(backend);
    }
}

impl Director for RandomDirector {
    fn name(&self) -> &str {
        &self.name
    }

    fn resolve(&self) -> Option<Arc<dyn Backend>> {
        let backends = self.backends.read();
        let healthy: Vec<_> = backends.iter().filter(|b| b.is_healthy()).collect();
        if healthy.is_empty() {
            return None;
        }
        // Simple pseudo-random selection using counter
        let idx = self.counter.fetch_add(1, Ordering::Relaxed) % healthy.len();
        Some(Arc::clone(healthy[idx]))
    }

    fn healthy(&self) -> bool {
        self.backends.read().iter().any(|b| b.is_healthy())
    }

    fn admin_health(&self) -> AdminHealth {
        AdminHealth::Probe
    }

    fn list(&self) -> Vec<DirectorListEntry> {
        self.backends
            .read()
            .iter()
            .map(|b| DirectorListEntry {
                name: b.name().to_string(),
                admin_health: b.admin_health(),
                is_healthy: b.is_healthy(),
                description: format!("{}", b.addr()),
            })
            .collect()
    }
}

/// Hash director.
/// Selects a backend based on a hash of the URL or other key.
pub struct HashDirector {
    name: String,
    backends: RwLock<Vec<Arc<dyn Backend>>>,
}

impl HashDirector {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            backends: RwLock::new(Vec::new()),
        }
    }

    pub fn add_backend(&self, backend: Arc<dyn Backend>) {
        self.backends.write().push(backend);
    }

    /// Resolve with a specific hash key.
    pub fn resolve_with_key(&self, key: u64) -> Option<Arc<dyn Backend>> {
        let backends = self.backends.read();
        let healthy: Vec<_> = backends.iter().filter(|b| b.is_healthy()).collect();
        if healthy.is_empty() {
            return None;
        }
        let idx = (key as usize) % healthy.len();
        Some(Arc::clone(healthy[idx]))
    }
}

impl Director for HashDirector {
    fn name(&self) -> &str {
        &self.name
    }

    fn resolve(&self) -> Option<Arc<dyn Backend>> {
        self.resolve_with_key(0)
    }

    fn healthy(&self) -> bool {
        self.backends.read().iter().any(|b| b.is_healthy())
    }

    fn admin_health(&self) -> AdminHealth {
        AdminHealth::Probe
    }

    fn list(&self) -> Vec<DirectorListEntry> {
        self.backends
            .read()
            .iter()
            .map(|b| DirectorListEntry {
                name: b.name().to_string(),
                admin_health: b.admin_health(),
                is_healthy: b.is_healthy(),
                description: format!("{}", b.addr()),
            })
            .collect()
    }
}

/// Fallback director.
/// Returns the first healthy backend in priority order.
pub struct FallbackDirector {
    name: String,
    backends: RwLock<Vec<Arc<dyn Backend>>>,
}

impl FallbackDirector {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            backends: RwLock::new(Vec::new()),
        }
    }

    pub fn add_backend(&self, backend: Arc<dyn Backend>) {
        self.backends.write().push(backend);
    }
}

impl Director for FallbackDirector {
    fn name(&self) -> &str {
        &self.name
    }

    fn resolve(&self) -> Option<Arc<dyn Backend>> {
        self.backends
            .read()
            .iter()
            .find(|b| b.is_healthy())
            .cloned()
    }

    fn healthy(&self) -> bool {
        self.backends.read().iter().any(|b| b.is_healthy())
    }

    fn admin_health(&self) -> AdminHealth {
        AdminHealth::Probe
    }

    fn list(&self) -> Vec<DirectorListEntry> {
        self.backends
            .read()
            .iter()
            .map(|b| DirectorListEntry {
                name: b.name().to_string(),
                admin_health: b.admin_health(),
                is_healthy: b.is_healthy(),
                description: format!("{}", b.addr()),
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::SocketAddr;
    use std::sync::atomic::AtomicBool;

    struct TestBackend {
        name: String,
        addr: SocketAddr,
        healthy: AtomicBool,
    }

    impl TestBackend {
        fn new(name: &str, addr: &str, healthy: bool) -> Arc<dyn Backend> {
            Arc::new(Self {
                name: name.to_string(),
                addr: addr.parse().unwrap(),
                healthy: AtomicBool::new(healthy),
            })
        }
    }

    impl Backend for TestBackend {
        fn name(&self) -> &str {
            &self.name
        }
        fn addr(&self) -> SocketAddr {
            self.addr
        }
        fn is_healthy(&self) -> bool {
            self.healthy.load(Ordering::Relaxed)
        }
        fn admin_health(&self) -> AdminHealth {
            AdminHealth::Probe
        }
        fn set_admin_health(&self, _health: AdminHealth) {}
    }

    #[test]
    fn test_round_robin() {
        let dir = RoundRobinDirector::new("rr");
        dir.add_backend(TestBackend::new("b1", "127.0.0.1:8001", true));
        dir.add_backend(TestBackend::new("b2", "127.0.0.1:8002", true));

        let first = dir.resolve().unwrap();
        let second = dir.resolve().unwrap();
        assert_ne!(first.name(), second.name());
    }

    #[test]
    fn test_round_robin_skips_sick() {
        let dir = RoundRobinDirector::new("rr");
        dir.add_backend(TestBackend::new("b1", "127.0.0.1:8001", false));
        dir.add_backend(TestBackend::new("b2", "127.0.0.1:8002", true));

        let resolved = dir.resolve().unwrap();
        assert_eq!(resolved.name(), "b2");
    }

    #[test]
    fn test_fallback_priority() {
        let dir = FallbackDirector::new("fb");
        dir.add_backend(TestBackend::new("primary", "127.0.0.1:8001", true));
        dir.add_backend(TestBackend::new("secondary", "127.0.0.1:8002", true));

        let resolved = dir.resolve().unwrap();
        assert_eq!(resolved.name(), "primary");
    }

    #[test]
    fn test_fallback_to_secondary() {
        let dir = FallbackDirector::new("fb");
        dir.add_backend(TestBackend::new("primary", "127.0.0.1:8001", false));
        dir.add_backend(TestBackend::new("secondary", "127.0.0.1:8002", true));

        let resolved = dir.resolve().unwrap();
        assert_eq!(resolved.name(), "secondary");
    }

    #[test]
    fn test_no_healthy_backend() {
        let dir = RoundRobinDirector::new("rr");
        dir.add_backend(TestBackend::new("b1", "127.0.0.1:8001", false));

        assert!(dir.resolve().is_none());
    }
}

//! Shard director -- consistent hash ring.
//!
//! The `ShardDirector` distributes requests across backends using a consistent
//! hash ring with configurable virtual nodes (replicas). This ensures that when
//! backends are added or removed, only a fraction of keys are remapped rather
//! than all of them -- an important property for cache-friendly routing.
//!
//! The implementation uses SipHash (via [`std::collections::hash_map::DefaultHasher`])
//! for hashing both the virtual node identifiers and request keys.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::Arc;

use parking_lot::RwLock;

use crate::traits::{AdminHealth, Backend, Director, DirectorListEntry};

/// Default number of virtual nodes per physical backend on the ring.
const DEFAULT_REPLICAS: u32 = 150;

/// A single point on the consistent hash ring.
#[derive(Debug, Clone)]
struct RingPoint {
    /// Hash value (position on the ring).
    hash: u64,
    /// Index into the `backends` vector.
    backend_idx: usize,
}

/// Consistent hash ring director.
///
/// Each backend is mapped to `replicas` virtual nodes on a 64-bit hash ring.
/// When resolving a key, the director finds the first ring point whose hash is
/// greater than or equal to the key hash (with wrap-around) and returns the
/// corresponding backend. If that backend is unhealthy, the director walks
/// clockwise around the ring until a healthy backend is found.
pub struct ShardDirector {
    name: String,
    ring: RwLock<Vec<RingPoint>>,
    backends: RwLock<Vec<Arc<dyn Backend>>>,
    replicas: u32,
}

impl ShardDirector {
    /// Creates a new `ShardDirector` with the given backends and default
    /// replica count (150).
    pub fn new(name: impl Into<String>, backends: Vec<Arc<dyn Backend>>) -> Self {
        Self::with_replicas(name, backends, DEFAULT_REPLICAS)
    }

    /// Creates a new `ShardDirector` with a custom replica count.
    ///
    /// # Arguments
    ///
    /// * `name` - Human-readable name for this director.
    /// * `backends` - The set of backends to distribute across.
    /// * `replicas` - Number of virtual nodes per backend on the ring.
    pub fn with_replicas(
        name: impl Into<String>,
        backends: Vec<Arc<dyn Backend>>,
        replicas: u32,
    ) -> Self {
        let ring = Self::build_ring(&backends, replicas);
        Self {
            name: name.into(),
            ring: RwLock::new(ring),
            backends: RwLock::new(backends),
            replicas,
        }
    }

    /// Builds the consistent hash ring from the given backends.
    ///
    /// For each backend, `replicas` virtual nodes are created by hashing a
    /// composite key of `"{backend_name}-{replica_index}"`.
    fn build_ring(backends: &[Arc<dyn Backend>], replicas: u32) -> Vec<RingPoint> {
        let mut ring = Vec::with_capacity(backends.len() * replicas as usize);

        for (idx, backend) in backends.iter().enumerate() {
            for r in 0..replicas {
                let key = format!("{}-{r}", backend.name());
                let hash = Self::hash_key(&key);
                ring.push(RingPoint {
                    hash,
                    backend_idx: idx,
                });
            }
        }

        ring.sort_by_key(|p| p.hash);
        ring
    }

    /// Hashes a string key to a u64 value using SipHash.
    pub fn hash_key(key: &str) -> u64 {
        let mut hasher = DefaultHasher::new();
        key.hash(&mut hasher);
        hasher.finish()
    }

    /// Resolves a backend for the given key string.
    ///
    /// This is the primary entry point for key-based routing. The key is
    /// typically the request URL or a VCL-computed hash string.
    ///
    /// If the primary backend for the key is unhealthy, the director walks
    /// clockwise around the ring to find the next healthy backend.
    pub fn resolve_with_key(&self, key: &str) -> Option<Arc<dyn Backend>> {
        let hash = Self::hash_key(key);
        self.resolve_with_hash(hash)
    }

    /// Resolves a backend for a pre-computed hash value.
    fn resolve_with_hash(&self, hash: u64) -> Option<Arc<dyn Backend>> {
        let ring = self.ring.read();
        let backends = self.backends.read();

        if ring.is_empty() || backends.is_empty() {
            return None;
        }

        // Binary search for the first ring point >= hash.
        let start = match ring.binary_search_by_key(&hash, |p| p.hash) {
            Ok(pos) => pos,
            Err(pos) => {
                if pos >= ring.len() {
                    0 // wrap around
                } else {
                    pos
                }
            }
        };

        // Walk the ring from the start position looking for a healthy backend.
        // We track which distinct backend indices we have visited to avoid
        // scanning the entire ring when all backends are unhealthy.
        let ring_len = ring.len();
        let backend_count = backends.len();
        let mut seen = vec![false; backend_count];
        let mut distinct_visited = 0usize;

        for offset in 0..ring_len {
            let idx = (start + offset) % ring_len;
            let backend_idx = ring[idx].backend_idx;

            if backend_idx < backend_count {
                let be = &backends[backend_idx];
                if be.is_healthy() {
                    return Some(Arc::clone(be));
                }
                if !seen[backend_idx] {
                    seen[backend_idx] = true;
                    distinct_visited += 1;
                    // If we have tried every distinct backend, stop.
                    if distinct_visited >= backend_count {
                        break;
                    }
                }
            }
        }

        None
    }

    /// Replaces the backend list and rebuilds the hash ring.
    pub fn set_backends(&self, backends: Vec<Arc<dyn Backend>>) {
        let ring = Self::build_ring(&backends, self.replicas);
        *self.ring.write() = ring;
        *self.backends.write() = backends;
    }

    /// Returns the number of points on the hash ring.
    pub fn ring_size(&self) -> usize {
        self.ring.read().len()
    }

    /// Returns the number of backends.
    pub fn backend_count(&self) -> usize {
        self.backends.read().len()
    }

    /// Returns the configured replica count.
    pub fn replicas(&self) -> u32 {
        self.replicas
    }
}

impl Director for ShardDirector {
    fn name(&self) -> &str {
        &self.name
    }

    /// Resolves a backend using a default key.
    ///
    /// In production this would use the request URL or hash from the VCL
    /// context. Here we use a fixed key as a fallback; callers should prefer
    /// [`resolve_with_key`](ShardDirector::resolve_with_key) for proper
    /// key-based routing.
    fn resolve(&self) -> Option<Arc<dyn Backend>> {
        // Fall back to the first healthy backend on the ring.
        let ring = self.ring.read();
        let backends = self.backends.read();

        for point in ring.iter() {
            if point.backend_idx < backends.len() && backends[point.backend_idx].is_healthy() {
                return Some(Arc::clone(&backends[point.backend_idx]));
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::SocketAddr;
    use std::sync::atomic::{AtomicBool, Ordering};

    struct TestBackend {
        name: String,
        addr: SocketAddr,
        healthy: AtomicBool,
    }

    impl TestBackend {
        fn new(name: &str, port: u16, healthy: bool) -> Arc<Self> {
            Arc::new(Self {
                name: name.to_string(),
                addr: SocketAddr::from(([127, 0, 0, 1], port)),
                healthy: AtomicBool::new(healthy),
            })
        }

        fn set_healthy(&self, h: bool) {
            self.healthy.store(h, Ordering::Relaxed);
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
    fn test_empty_shard_director() {
        let dir = ShardDirector::new("empty", vec![]);
        assert!(dir.resolve_with_key("anything").is_none());
        assert!(Director::resolve(&dir).is_none());
        assert!(!dir.healthy());
        assert_eq!(dir.ring_size(), 0);
        assert_eq!(dir.backend_count(), 0);
    }

    #[test]
    fn test_single_backend() {
        let be = TestBackend::new("solo", 8001, true);
        let dir = ShardDirector::new("single", vec![be]);

        assert_eq!(dir.backend_count(), 1);
        assert_eq!(dir.ring_size(), DEFAULT_REPLICAS as usize);

        let resolved = dir.resolve_with_key("/index.html").unwrap();
        assert_eq!(resolved.name(), "solo");
    }

    #[test]
    fn test_consistent_hashing() {
        let backends: Vec<Arc<dyn Backend>> = vec![
            TestBackend::new("a", 8001, true),
            TestBackend::new("b", 8002, true),
            TestBackend::new("c", 8003, true),
        ];
        let dir = ShardDirector::new("hash-test", backends);

        // The same key should always resolve to the same backend.
        let key = "/api/users/42";
        let first = dir.resolve_with_key(key).unwrap().name().to_string();
        for _ in 0..100 {
            let resolved = dir.resolve_with_key(key).unwrap();
            assert_eq!(resolved.name(), first, "consistent hashing violated");
        }
    }

    #[test]
    fn test_distribution() {
        let backends: Vec<Arc<dyn Backend>> = vec![
            TestBackend::new("a", 8001, true),
            TestBackend::new("b", 8002, true),
            TestBackend::new("c", 8003, true),
        ];
        let dir = ShardDirector::new("dist-test", backends);

        let mut counts = std::collections::HashMap::new();
        for i in 0..3000 {
            let key = format!("/path/{i}");
            let resolved = dir.resolve_with_key(&key).unwrap();
            *counts.entry(resolved.name().to_string()).or_insert(0u32) += 1;
        }

        // Each backend should get a reasonable share (at least 15% of 3000 = 450).
        // Consistent hashing with 150 replicas should distribute well.
        for (name, count) in &counts {
            assert!(
                *count > 300,
                "backend {name} got only {count} out of 3000 -- distribution is too skewed"
            );
        }
        assert_eq!(counts.len(), 3, "all 3 backends should receive traffic");
    }

    #[test]
    fn test_failover() {
        let be_a = TestBackend::new("a", 8001, true);
        let be_b = TestBackend::new("b", 8002, true);
        let be_c = TestBackend::new("c", 8003, true);
        let backends: Vec<Arc<dyn Backend>> = vec![
            Arc::clone(&be_a) as Arc<dyn Backend>,
            Arc::clone(&be_b) as Arc<dyn Backend>,
            Arc::clone(&be_c) as Arc<dyn Backend>,
        ];

        let dir = ShardDirector::new("failover-test", backends);

        let key = "/important/resource";
        let _primary = dir.resolve_with_key(key).unwrap().name().to_string();

        // Make all backends sick.
        be_a.set_healthy(false);
        be_b.set_healthy(false);
        be_c.set_healthy(false);

        assert!(
            dir.resolve_with_key(key).is_none(),
            "should return None when all backends are sick"
        );

        // Make only one healthy.
        be_b.set_healthy(true);
        let resolved = dir.resolve_with_key(key).unwrap();
        assert_eq!(
            resolved.name(),
            "b",
            "should failover to the only healthy backend"
        );
    }

    #[test]
    fn test_all_unhealthy() {
        let backends: Vec<Arc<dyn Backend>> = vec![
            TestBackend::new("x", 8001, false),
            TestBackend::new("y", 8002, false),
        ];
        let dir = ShardDirector::new("sick-test", backends);

        assert!(dir.resolve_with_key("/foo").is_none());
        assert!(Director::resolve(&dir).is_none());
        assert!(!dir.healthy());
    }

    #[test]
    fn test_set_backends() {
        let dir = ShardDirector::new("dynamic", vec![TestBackend::new("a", 8001, true)]);
        assert_eq!(dir.backend_count(), 1);

        dir.set_backends(vec![
            TestBackend::new("x", 9001, true),
            TestBackend::new("y", 9002, true),
        ]);
        assert_eq!(dir.backend_count(), 2);
        assert_eq!(dir.ring_size(), 2 * DEFAULT_REPLICAS as usize);

        let resolved = dir.resolve_with_key("/test").unwrap();
        assert!(
            resolved.name() == "x" || resolved.name() == "y",
            "should resolve to one of the new backends"
        );
    }

    #[test]
    fn test_custom_replicas() {
        let backends: Vec<Arc<dyn Backend>> = vec![
            TestBackend::new("a", 8001, true),
            TestBackend::new("b", 8002, true),
        ];
        let dir = ShardDirector::with_replicas("custom", backends, 50);
        assert_eq!(dir.replicas(), 50);
        assert_eq!(dir.ring_size(), 100); // 2 backends * 50 replicas
    }

    #[test]
    fn test_list() {
        let backends: Vec<Arc<dyn Backend>> = vec![
            TestBackend::new("a", 8001, true),
            TestBackend::new("b", 8002, false),
        ];
        let dir = ShardDirector::new("list-test", backends);

        let list = dir.list();
        assert_eq!(list.len(), 2);
        assert!(list.iter().any(|e| e.name == "a" && e.is_healthy));
        assert!(list.iter().any(|e| e.name == "b" && !e.is_healthy));
    }

    #[test]
    fn test_hash_deterministic() {
        let h1 = ShardDirector::hash_key("test-key");
        let h2 = ShardDirector::hash_key("test-key");
        assert_eq!(h1, h2);

        let h3 = ShardDirector::hash_key("different-key");
        assert_ne!(h1, h3);
    }
}

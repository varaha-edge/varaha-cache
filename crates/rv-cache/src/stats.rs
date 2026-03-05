use std::sync::atomic::{AtomicU64, Ordering};

/// Cache statistics with atomic counters for lock-free updates.
pub struct CacheStats {
    pub cache_hit: AtomicU64,
    pub cache_miss: AtomicU64,
    pub cache_pass: AtomicU64,
    pub cache_hit_for_pass: AtomicU64,
    pub cache_hit_grace: AtomicU64,
    pub backend_fetches: AtomicU64,
    pub evictions: AtomicU64,
    pub bans_added: AtomicU64,
    pub bans_checked: AtomicU64,
    pub n_objects: AtomicU64,
    pub n_expired: AtomicU64,
    pub n_purged: AtomicU64,
    pub bytes_stored: AtomicU64,
}

impl CacheStats {
    pub fn new() -> Self {
        Self {
            cache_hit: AtomicU64::new(0),
            cache_miss: AtomicU64::new(0),
            cache_pass: AtomicU64::new(0),
            cache_hit_for_pass: AtomicU64::new(0),
            cache_hit_grace: AtomicU64::new(0),
            backend_fetches: AtomicU64::new(0),
            evictions: AtomicU64::new(0),
            bans_added: AtomicU64::new(0),
            bans_checked: AtomicU64::new(0),
            n_objects: AtomicU64::new(0),
            n_expired: AtomicU64::new(0),
            n_purged: AtomicU64::new(0),
            bytes_stored: AtomicU64::new(0),
        }
    }

    pub fn snapshot(&self) -> CacheStatsSnapshot {
        CacheStatsSnapshot {
            cache_hit: self.cache_hit.load(Ordering::Relaxed),
            cache_miss: self.cache_miss.load(Ordering::Relaxed),
            cache_pass: self.cache_pass.load(Ordering::Relaxed),
            cache_hit_for_pass: self.cache_hit_for_pass.load(Ordering::Relaxed),
            cache_hit_grace: self.cache_hit_grace.load(Ordering::Relaxed),
            backend_fetches: self.backend_fetches.load(Ordering::Relaxed),
            evictions: self.evictions.load(Ordering::Relaxed),
            bans_added: self.bans_added.load(Ordering::Relaxed),
            bans_checked: self.bans_checked.load(Ordering::Relaxed),
            n_objects: self.n_objects.load(Ordering::Relaxed),
            n_expired: self.n_expired.load(Ordering::Relaxed),
            n_purged: self.n_purged.load(Ordering::Relaxed),
            bytes_stored: self.bytes_stored.load(Ordering::Relaxed),
        }
    }
}

impl Default for CacheStats {
    fn default() -> Self {
        Self::new()
    }
}

/// A point-in-time snapshot of cache stats (all plain u64).
#[derive(Debug, Clone)]
pub struct CacheStatsSnapshot {
    pub cache_hit: u64,
    pub cache_miss: u64,
    pub cache_pass: u64,
    pub cache_hit_for_pass: u64,
    pub cache_hit_grace: u64,
    pub backend_fetches: u64,
    pub evictions: u64,
    pub bans_added: u64,
    pub bans_checked: u64,
    pub n_objects: u64,
    pub n_expired: u64,
    pub n_purged: u64,
    pub bytes_stored: u64,
}

impl CacheStatsSnapshot {
    /// Cache hit ratio as a percentage (0.0 - 100.0).
    pub fn hit_rate(&self) -> f64 {
        let total = self.cache_hit + self.cache_miss + self.cache_pass;
        if total == 0 {
            0.0
        } else {
            (self.cache_hit as f64 / total as f64) * 100.0
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_stats_increment_and_snapshot() {
        let stats = CacheStats::new();
        stats.cache_hit.fetch_add(10, Ordering::Relaxed);
        stats.cache_miss.fetch_add(5, Ordering::Relaxed);
        stats.cache_pass.fetch_add(5, Ordering::Relaxed);

        let snap = stats.snapshot();
        assert_eq!(snap.cache_hit, 10);
        assert_eq!(snap.cache_miss, 5);
        assert!((snap.hit_rate() - 50.0).abs() < 0.01);
    }

    #[test]
    fn test_hit_rate_zero_total() {
        let stats = CacheStats::new();
        let snap = stats.snapshot();
        assert_eq!(snap.hit_rate(), 0.0);
    }
}

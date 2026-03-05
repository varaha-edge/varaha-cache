use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

/// Health probe status.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProbeStatus {
    Healthy,
    Sick,
    Unknown,
}

/// Result of a single probe attempt.
#[derive(Debug, Clone)]
pub struct ProbeResult {
    pub status: ProbeStatus,
    pub response_time: Duration,
    pub http_status: Option<u16>,
}

/// Health probe configuration and state.
/// Based on cache_backend_probe.c
pub struct HealthProbe {
    pub url: String,
    pub interval: Duration,
    pub timeout: Duration,
    pub threshold: u32,
    pub window: u32,
    pub initial: u32,
    target: SocketAddr,
    is_healthy: AtomicBool,
    good_count: AtomicU64,
    total_count: AtomicU64,
    /// Bitmap of recent probe results (1 = healthy, 0 = sick)
    bitmap: parking_lot::Mutex<Vec<bool>>,
}

impl HealthProbe {
    pub fn new(
        target: SocketAddr,
        url: impl Into<String>,
        interval: Duration,
        timeout: Duration,
        threshold: u32,
        window: u32,
    ) -> Self {
        let initial_bitmap = vec![false; window as usize];
        Self {
            url: url.into(),
            interval,
            timeout,
            threshold,
            window,
            initial: 0,
            target,
            is_healthy: AtomicBool::new(false),
            good_count: AtomicU64::new(0),
            total_count: AtomicU64::new(0),
            bitmap: parking_lot::Mutex::new(initial_bitmap),
        }
    }

    pub fn target(&self) -> SocketAddr {
        self.target
    }

    pub fn is_healthy(&self) -> bool {
        self.is_healthy.load(Ordering::Relaxed)
    }

    /// Record a probe result and update health status.
    pub fn record_result(&self, result: &ProbeResult) {
        let is_good = result.status == ProbeStatus::Healthy;
        self.total_count.fetch_add(1, Ordering::Relaxed);
        if is_good {
            self.good_count.fetch_add(1, Ordering::Relaxed);
        }

        let mut bitmap = self.bitmap.lock();
        let idx = (self.total_count.load(Ordering::Relaxed) as usize - 1) % self.window as usize;
        bitmap[idx] = is_good;

        // Count healthy probes in the window
        let healthy_in_window = bitmap.iter().filter(|&&b| b).count() as u32;
        self.is_healthy
            .store(healthy_in_window >= self.threshold, Ordering::Relaxed);
    }

    pub fn good_count(&self) -> u64 {
        self.good_count.load(Ordering::Relaxed)
    }

    pub fn total_count(&self) -> u64 {
        self.total_count.load(Ordering::Relaxed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_probe() -> HealthProbe {
        HealthProbe::new(
            "127.0.0.1:8080".parse().unwrap(),
            "/health",
            Duration::from_secs(5),
            Duration::from_secs(1),
            3, // threshold
            5, // window
        )
    }

    #[test]
    fn test_initially_sick() {
        let probe = make_probe();
        assert!(!probe.is_healthy());
    }

    #[test]
    fn test_becomes_healthy() {
        let probe = make_probe();
        let good = ProbeResult {
            status: ProbeStatus::Healthy,
            response_time: Duration::from_millis(10),
            http_status: Some(200),
        };

        // Need 3 out of 5 to be healthy
        probe.record_result(&good);
        probe.record_result(&good);
        assert!(!probe.is_healthy()); // 2/5, need 3

        probe.record_result(&good);
        assert!(probe.is_healthy()); // 3/5
    }

    #[test]
    fn test_becomes_sick() {
        let probe = make_probe();
        let good = ProbeResult {
            status: ProbeStatus::Healthy,
            response_time: Duration::from_millis(10),
            http_status: Some(200),
        };
        let bad = ProbeResult {
            status: ProbeStatus::Sick,
            response_time: Duration::from_millis(1000),
            http_status: None,
        };

        // Fill window with good results
        for _ in 0..5 {
            probe.record_result(&good);
        }
        assert!(probe.is_healthy());

        // Now add bad results to push good ones out
        for _ in 0..3 {
            probe.record_result(&bad);
        }
        assert!(!probe.is_healthy()); // Only 2 good left in window
    }
}

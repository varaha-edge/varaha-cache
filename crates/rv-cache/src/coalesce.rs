use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use dashmap::DashMap;
use rv_types::Digest;
use tokio::sync::broadcast;

/// Channel capacity for broadcast notifications.
/// Each in-flight digest only needs a single message (the result),
/// but we use capacity 16 to handle edge cases where multiple
/// complete/cancel calls might race.
const CHANNEL_CAPACITY: usize = 16;

/// Default timeout for coalesced waiters.
const DEFAULT_COALESCE_TIMEOUT: Duration = Duration::from_secs(5);

/// Default maximum number of waiters per digest before new requests fetch independently.
const DEFAULT_MAX_WAITERS: usize = 1000;

/// Result communicated from the fetching request to all waiting requests.
#[derive(Debug, Clone)]
pub enum CoalesceResult {
    /// The object has been fetched and is now available in the cache.
    Ready,
    /// The fetch failed with the given error description.
    Failed(String),
}

/// Decision returned by `CoalesceManager::check` to the caller.
pub enum CoalesceDecision {
    /// You are the first request for this digest -- go fetch from the backend.
    Fetch,
    /// Another request is already fetching this digest -- wait for the result.
    Wait(broadcast::Receiver<CoalesceResult>),
}

/// Statistics for coalescing activity.
pub struct CoalesceStats {
    /// Total number of requests that were coalesced (waited instead of fetching).
    pub coalesced: u64,
    /// Total number of coalesced waits that timed out.
    pub timeouts: u64,
    /// Current number of in-flight fetches.
    pub inflight: usize,
}

/// Internal entry tracking broadcast sender and waiter count for a digest.
struct CoalesceEntry {
    tx: broadcast::Sender<CoalesceResult>,
    waiter_count: usize,
}

/// Manages request coalescing (also known as "request collapsing").
///
/// When multiple requests arrive for the same cache object simultaneously,
/// only the first request performs the backend fetch. All subsequent requests
/// for the same digest subscribe to a broadcast channel and wait for the
/// first request to complete. This prevents the "thundering herd" problem
/// where N cache misses for the same object would generate N backend fetches.
///
/// Thread-safe: uses `DashMap` for concurrent access without a global lock.
pub struct CoalesceManager {
    /// Map from digest to the in-flight entry.
    inflight: DashMap<Digest, CoalesceEntry>,
    /// Maximum time a coalesced waiter should wait before timing out.
    coalesce_timeout: Duration,
    /// Maximum number of waiters per digest. When exceeded, new requests
    /// receive `Fetch` so they fetch independently instead of piling up.
    max_waiters: usize,
    /// Counter of requests that were coalesced (received `Wait`).
    coalesced_count: AtomicU64,
    /// Counter of coalesced waits that timed out (caller responsibility to record).
    timeout_count: AtomicU64,
}

impl CoalesceManager {
    /// Create a new coalesce manager with default settings.
    pub fn new() -> Self {
        Self {
            inflight: DashMap::new(),
            coalesce_timeout: DEFAULT_COALESCE_TIMEOUT,
            max_waiters: DEFAULT_MAX_WAITERS,
            coalesced_count: AtomicU64::new(0),
            timeout_count: AtomicU64::new(0),
        }
    }

    /// Create a new coalesce manager with custom timeout and max waiters.
    pub fn with_config(coalesce_timeout: Duration, max_waiters: usize) -> Self {
        Self {
            inflight: DashMap::new(),
            coalesce_timeout,
            max_waiters,
            coalesced_count: AtomicU64::new(0),
            timeout_count: AtomicU64::new(0),
        }
    }

    /// Returns the configured coalesce timeout.
    pub fn coalesce_timeout(&self) -> Duration {
        self.coalesce_timeout
    }

    /// Check whether a fetch is already in progress for the given digest.
    ///
    /// Returns `CoalesceDecision::Fetch` if this is the first request (the
    /// caller should proceed to fetch from the backend and then call
    /// `complete` or `cancel`).
    ///
    /// Returns `CoalesceDecision::Wait(receiver)` if another request is
    /// already fetching this digest (the caller should await the receiver).
    ///
    /// If the number of waiters for this digest has reached `max_waiters`,
    /// returns `CoalesceDecision::Fetch` to let the request fetch independently
    /// rather than piling up behind a potentially slow fetch.
    pub fn check(&self, digest: &Digest) -> CoalesceDecision {
        // Fast path: if the digest is already in-flight, subscribe.
        if let Some(mut entry) = self.inflight.get_mut(digest) {
            if entry.waiter_count >= self.max_waiters {
                return CoalesceDecision::Fetch;
            }
            entry.waiter_count += 1;
            self.coalesced_count.fetch_add(1, Ordering::Relaxed);
            return CoalesceDecision::Wait(entry.tx.subscribe());
        }

        // Slow path: try to insert. Use the entry API to avoid TOCTOU races.
        match self.inflight.entry(*digest) {
            dashmap::Entry::Occupied(mut entry) => {
                let e = entry.get_mut();
                if e.waiter_count >= self.max_waiters {
                    return CoalesceDecision::Fetch;
                }
                e.waiter_count += 1;
                self.coalesced_count.fetch_add(1, Ordering::Relaxed);
                CoalesceDecision::Wait(e.tx.subscribe())
            }
            dashmap::Entry::Vacant(entry) => {
                let (tx, _rx) = broadcast::channel(CHANNEL_CAPACITY);
                entry.insert(CoalesceEntry {
                    tx,
                    waiter_count: 0,
                });
                CoalesceDecision::Fetch
            }
        }
    }

    /// Signal all waiting requests that the fetch has completed.
    ///
    /// Sends `result` to every subscriber, then removes the entry from the
    /// in-flight map. It is safe to call this even if there are no waiters.
    pub fn complete(&self, digest: &Digest, result: CoalesceResult) {
        if let Some((_, entry)) = self.inflight.remove(digest) {
            // send() returns Err only when there are no active receivers,
            // which is fine -- it means nobody was waiting.
            let _ = entry.tx.send(result);
        }
    }

    /// Cancel an in-flight fetch without sending a result.
    ///
    /// Removes the entry so the next request for this digest will get
    /// `CoalesceDecision::Fetch` again. Any existing waiters will see
    /// their receiver return `RecvError` (channel closed).
    pub fn cancel(&self, digest: &Digest) {
        self.inflight.remove(digest);
    }

    /// Returns the number of in-flight fetches currently being coalesced.
    pub fn inflight_count(&self) -> usize {
        self.inflight.len()
    }

    /// Record that a coalesced wait timed out. This should be called by the
    /// caller when a `Wait` receiver times out.
    pub fn record_timeout(&self) {
        self.timeout_count.fetch_add(1, Ordering::Relaxed);
    }

    /// Returns a snapshot of coalescing statistics.
    pub fn stats(&self) -> CoalesceStats {
        CoalesceStats {
            coalesced: self.coalesced_count.load(Ordering::Relaxed),
            timeouts: self.timeout_count.load(Ordering::Relaxed),
            inflight: self.inflight.len(),
        }
    }
}

impl Default for CoalesceManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_digest(val: u8) -> Digest {
        let mut bytes = [0u8; 32];
        bytes[0] = val;
        Digest::new(bytes)
    }

    #[test]
    fn first_request_gets_fetch() {
        let mgr = CoalesceManager::new();
        let digest = test_digest(1);

        match mgr.check(&digest) {
            CoalesceDecision::Fetch => {} // expected
            CoalesceDecision::Wait(_) => panic!("expected Fetch for first request"),
        }

        assert_eq!(mgr.inflight_count(), 1);
    }

    #[test]
    fn second_request_gets_wait() {
        let mgr = CoalesceManager::new();
        let digest = test_digest(1);

        // First request claims the fetch.
        match mgr.check(&digest) {
            CoalesceDecision::Fetch => {}
            CoalesceDecision::Wait(_) => panic!("expected Fetch for first request"),
        }

        // Second request should wait.
        match mgr.check(&digest) {
            CoalesceDecision::Wait(_) => {} // expected
            CoalesceDecision::Fetch => panic!("expected Wait for second request"),
        }

        assert_eq!(mgr.inflight_count(), 1);
    }

    #[test]
    fn different_digests_both_get_fetch() {
        let mgr = CoalesceManager::new();

        match mgr.check(&test_digest(1)) {
            CoalesceDecision::Fetch => {}
            CoalesceDecision::Wait(_) => panic!("expected Fetch for digest 1"),
        }

        match mgr.check(&test_digest(2)) {
            CoalesceDecision::Fetch => {}
            CoalesceDecision::Wait(_) => panic!("expected Fetch for digest 2"),
        }

        assert_eq!(mgr.inflight_count(), 2);
    }

    #[tokio::test]
    async fn complete_notifies_waiters() {
        let mgr = CoalesceManager::new();
        let digest = test_digest(1);

        // First request starts the fetch.
        match mgr.check(&digest) {
            CoalesceDecision::Fetch => {}
            CoalesceDecision::Wait(_) => panic!("expected Fetch"),
        }

        // Two waiting requests subscribe.
        let mut rx1 = match mgr.check(&digest) {
            CoalesceDecision::Wait(rx) => rx,
            CoalesceDecision::Fetch => panic!("expected Wait"),
        };
        let mut rx2 = match mgr.check(&digest) {
            CoalesceDecision::Wait(rx) => rx,
            CoalesceDecision::Fetch => panic!("expected Wait"),
        };

        // Complete the fetch.
        mgr.complete(&digest, CoalesceResult::Ready);

        // Both waiters should receive Ready.
        match rx1.recv().await {
            Ok(CoalesceResult::Ready) => {}
            other => panic!("expected Ready, got {:?}", other),
        }
        match rx2.recv().await {
            Ok(CoalesceResult::Ready) => {}
            other => panic!("expected Ready, got {:?}", other),
        }

        // Entry should be removed.
        assert_eq!(mgr.inflight_count(), 0);
    }

    #[tokio::test]
    async fn complete_with_failure_notifies_waiters() {
        let mgr = CoalesceManager::new();
        let digest = test_digest(1);

        match mgr.check(&digest) {
            CoalesceDecision::Fetch => {}
            CoalesceDecision::Wait(_) => panic!("expected Fetch"),
        }

        let mut rx = match mgr.check(&digest) {
            CoalesceDecision::Wait(rx) => rx,
            CoalesceDecision::Fetch => panic!("expected Wait"),
        };

        mgr.complete(
            &digest,
            CoalesceResult::Failed("backend timeout".to_string()),
        );

        match rx.recv().await {
            Ok(CoalesceResult::Failed(msg)) => {
                assert_eq!(msg, "backend timeout");
            }
            other => panic!("expected Failed, got {:?}", other),
        }

        assert_eq!(mgr.inflight_count(), 0);
    }

    #[test]
    fn cancel_removes_entry() {
        let mgr = CoalesceManager::new();
        let digest = test_digest(1);

        match mgr.check(&digest) {
            CoalesceDecision::Fetch => {}
            CoalesceDecision::Wait(_) => panic!("expected Fetch"),
        }

        assert_eq!(mgr.inflight_count(), 1);

        mgr.cancel(&digest);

        assert_eq!(mgr.inflight_count(), 0);

        // Next request for the same digest should get Fetch again.
        match mgr.check(&digest) {
            CoalesceDecision::Fetch => {}
            CoalesceDecision::Wait(_) => panic!("expected Fetch after cancel"),
        }

        assert_eq!(mgr.inflight_count(), 1);
    }

    #[tokio::test]
    async fn cancel_closes_waiter_channel() {
        let mgr = CoalesceManager::new();
        let digest = test_digest(1);

        match mgr.check(&digest) {
            CoalesceDecision::Fetch => {}
            CoalesceDecision::Wait(_) => panic!("expected Fetch"),
        }

        let mut rx = match mgr.check(&digest) {
            CoalesceDecision::Wait(rx) => rx,
            CoalesceDecision::Fetch => panic!("expected Wait"),
        };

        // Cancel drops the sender, which closes the channel.
        mgr.cancel(&digest);

        // The receiver should get a closed error.
        match rx.recv().await {
            Err(broadcast::error::RecvError::Closed) => {} // expected
            other => panic!("expected Closed error, got {:?}", other),
        }
    }

    #[test]
    fn complete_on_nonexistent_digest_is_noop() {
        let mgr = CoalesceManager::new();
        // Should not panic.
        mgr.complete(&test_digest(99), CoalesceResult::Ready);
        assert_eq!(mgr.inflight_count(), 0);
    }

    #[test]
    fn cancel_on_nonexistent_digest_is_noop() {
        let mgr = CoalesceManager::new();
        // Should not panic.
        mgr.cancel(&test_digest(99));
        assert_eq!(mgr.inflight_count(), 0);
    }

    #[test]
    fn max_waiters_causes_independent_fetch() {
        let mgr = CoalesceManager::with_config(Duration::from_secs(5), 3);
        let digest = test_digest(1);

        // First request: Fetch
        match mgr.check(&digest) {
            CoalesceDecision::Fetch => {}
            CoalesceDecision::Wait(_) => panic!("expected Fetch for first request"),
        }

        // Next 3 requests: Wait (waiter_count goes 1, 2, 3)
        for i in 0..3 {
            match mgr.check(&digest) {
                CoalesceDecision::Wait(_) => {}
                CoalesceDecision::Fetch => panic!("expected Wait for request {}", i + 2),
            }
        }

        // 5th request: should get Fetch because max_waiters (3) reached
        match mgr.check(&digest) {
            CoalesceDecision::Fetch => {}
            CoalesceDecision::Wait(_) => panic!("expected Fetch when max_waiters exceeded"),
        }
    }

    #[test]
    fn stats_tracks_coalesced_count() {
        let mgr = CoalesceManager::new();
        let digest = test_digest(1);

        // First request: Fetch (not coalesced)
        match mgr.check(&digest) {
            CoalesceDecision::Fetch => {}
            CoalesceDecision::Wait(_) => panic!("expected Fetch"),
        }

        // Two coalesced requests
        match mgr.check(&digest) {
            CoalesceDecision::Wait(_) => {}
            CoalesceDecision::Fetch => panic!("expected Wait"),
        }
        match mgr.check(&digest) {
            CoalesceDecision::Wait(_) => {}
            CoalesceDecision::Fetch => panic!("expected Wait"),
        }

        let stats = mgr.stats();
        assert_eq!(stats.coalesced, 2);
        assert_eq!(stats.timeouts, 0);
        assert_eq!(stats.inflight, 1);
    }

    #[test]
    fn record_timeout_increments_counter() {
        let mgr = CoalesceManager::new();
        mgr.record_timeout();
        mgr.record_timeout();
        mgr.record_timeout();

        let stats = mgr.stats();
        assert_eq!(stats.timeouts, 3);
    }

    #[test]
    fn with_config_sets_timeout_and_max_waiters() {
        let mgr = CoalesceManager::with_config(Duration::from_secs(10), 500);
        assert_eq!(mgr.coalesce_timeout(), Duration::from_secs(10));
        assert_eq!(mgr.max_waiters, 500);
    }
}

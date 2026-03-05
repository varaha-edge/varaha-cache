use dashmap::DashMap;
use rv_types::Digest;
use tokio::sync::broadcast;

/// Channel capacity for broadcast notifications.
/// Each in-flight digest only needs a single message (the result),
/// but we use capacity 16 to handle edge cases where multiple
/// complete/cancel calls might race.
const CHANNEL_CAPACITY: usize = 16;

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
    /// Map from digest to the broadcast sender for in-flight fetches.
    inflight: DashMap<Digest, broadcast::Sender<CoalesceResult>>,
}

impl CoalesceManager {
    /// Create a new, empty coalesce manager.
    pub fn new() -> Self {
        Self {
            inflight: DashMap::new(),
        }
    }

    /// Check whether a fetch is already in progress for the given digest.
    ///
    /// Returns `CoalesceDecision::Fetch` if this is the first request (the
    /// caller should proceed to fetch from the backend and then call
    /// `complete` or `cancel`).
    ///
    /// Returns `CoalesceDecision::Wait(receiver)` if another request is
    /// already fetching this digest (the caller should await the receiver).
    pub fn check(&self, digest: &Digest) -> CoalesceDecision {
        // Fast path: if the digest is already in-flight, subscribe.
        if let Some(entry) = self.inflight.get(digest) {
            return CoalesceDecision::Wait(entry.value().subscribe());
        }

        // Slow path: try to insert. Use the entry API to avoid TOCTOU races.
        match self.inflight.entry(*digest) {
            dashmap::Entry::Occupied(entry) => CoalesceDecision::Wait(entry.get().subscribe()),
            dashmap::Entry::Vacant(entry) => {
                let (tx, _rx) = broadcast::channel(CHANNEL_CAPACITY);
                entry.insert(tx);
                CoalesceDecision::Fetch
            }
        }
    }

    /// Signal all waiting requests that the fetch has completed.
    ///
    /// Sends `result` to every subscriber, then removes the entry from the
    /// in-flight map. It is safe to call this even if there are no waiters.
    pub fn complete(&self, digest: &Digest, result: CoalesceResult) {
        if let Some((_, tx)) = self.inflight.remove(digest) {
            // send() returns Err only when there are no active receivers,
            // which is fine -- it means nobody was waiting.
            let _ = tx.send(result);
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
}

use std::cmp::Ordering;
use std::collections::BinaryHeap;
use std::sync::Arc;

use parking_lot::Mutex;
use rv_types::{VtimDur, VtimReal};

use rv_storage::ObjCore;

/// An entry in the expiry heap. Ordered by timer_when (earliest first).
struct ExpiryEntry {
    when: VtimReal,
    objcore: Arc<ObjCore>,
}

impl PartialEq for ExpiryEntry {
    fn eq(&self, other: &Self) -> bool {
        self.when.0 == other.when.0
    }
}

impl Eq for ExpiryEntry {}

impl PartialOrd for ExpiryEntry {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for ExpiryEntry {
    fn cmp(&self, other: &Self) -> Ordering {
        // Reverse order for min-heap (earliest deadline first)
        other
            .when
            .0
            .partial_cmp(&self.when.0)
            .unwrap_or(Ordering::Equal)
    }
}

/// Manages object expiry using a binary heap priority queue.
pub struct ExpiryManager {
    heap: Mutex<BinaryHeap<ExpiryEntry>>,
}

impl ExpiryManager {
    pub fn new() -> Self {
        Self {
            heap: Mutex::new(BinaryHeap::new()),
        }
    }

    /// Insert an object into the expiry queue.
    /// The object will be scheduled for expiry at `t_origin + ttl + grace + keep`.
    pub fn insert(&self, oc: Arc<ObjCore>) {
        let when = oc.t_origin + oc.ttl + oc.grace + oc.keep;
        let mut heap = self.heap.lock();
        heap.push(ExpiryEntry { when, objcore: oc });
    }

    /// Insert with an explicit deadline.
    pub fn insert_at(&self, oc: Arc<ObjCore>, when: VtimReal) {
        let mut heap = self.heap.lock();
        heap.push(ExpiryEntry { when, objcore: oc });
    }

    /// Check the earliest deadline without removing.
    pub fn peek_when(&self) -> Option<VtimReal> {
        let heap = self.heap.lock();
        heap.peek().map(|e| e.when)
    }

    /// Expire objects whose deadline has passed.
    /// Returns the expired ObjCore references so the caller can clean them up.
    pub fn expire(&self, now: VtimReal) -> Vec<Arc<ObjCore>> {
        let mut expired = Vec::new();
        let mut heap = self.heap.lock();

        while let Some(entry) = heap.peek() {
            if entry.when.0 > now.0 {
                break;
            }
            if let Some(entry) = heap.pop() {
                expired.push(entry.objcore);
            }
        }

        expired
    }

    /// Number of objects in the expiry queue.
    pub fn len(&self) -> usize {
        self.heap.lock().len()
    }

    pub fn is_empty(&self) -> bool {
        self.heap.lock().is_empty()
    }

    /// Time until the next expiry, or None if the queue is empty.
    pub fn time_to_next(&self, now: VtimReal) -> Option<VtimDur> {
        self.peek_when().map(|when| {
            let diff = when.0 - now.0;
            if diff > 0.0 {
                VtimDur::from_secs(diff)
            } else {
                VtimDur::ZERO
            }
        })
    }
}

impl Default for ExpiryManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rv_types::Digest;

    fn test_digest(val: u8) -> Digest {
        let mut bytes = [0u8; 32];
        bytes[0] = val;
        Digest::new(bytes)
    }

    fn make_oc(val: u8, origin: f64, ttl: f64, grace: f64, keep: f64) -> Arc<ObjCore> {
        let mut oc = ObjCore::new(test_digest(val));
        oc.t_origin = VtimReal::from_secs(origin);
        oc.ttl = VtimDur::from_secs(ttl);
        oc.grace = VtimDur::from_secs(grace);
        oc.keep = VtimDur::from_secs(keep);
        Arc::new(oc)
    }

    #[test]
    fn test_insert_and_expire() {
        let mgr = ExpiryManager::new();

        // Object expires at t=1000 + 60 + 10 + 0 = 1070
        let oc1 = make_oc(1, 1000.0, 60.0, 10.0, 0.0);
        // Object expires at t=1000 + 30 + 5 + 0 = 1035
        let oc2 = make_oc(2, 1000.0, 30.0, 5.0, 0.0);

        mgr.insert(oc1);
        mgr.insert(oc2);

        assert_eq!(mgr.len(), 2);

        // At t=1040, only oc2 should be expired
        let expired = mgr.expire(VtimReal::from_secs(1040.0));
        assert_eq!(expired.len(), 1);
        assert_eq!(expired[0].digest, test_digest(2));

        // At t=1080, oc1 should also be expired
        let expired = mgr.expire(VtimReal::from_secs(1080.0));
        assert_eq!(expired.len(), 1);
        assert_eq!(expired[0].digest, test_digest(1));

        assert!(mgr.is_empty());
    }

    #[test]
    fn test_time_to_next() {
        let mgr = ExpiryManager::new();
        assert!(mgr.time_to_next(VtimReal::from_secs(0.0)).is_none());

        let oc = make_oc(1, 1000.0, 60.0, 0.0, 0.0);
        mgr.insert(oc); // expires at 1060

        let ttl = mgr.time_to_next(VtimReal::from_secs(1050.0)).unwrap();
        assert!((ttl.as_secs() - 10.0).abs() < 0.01);
    }
}

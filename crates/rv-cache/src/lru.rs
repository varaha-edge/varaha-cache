use std::collections::HashMap;
use std::ptr;
use std::sync::atomic::{AtomicU64, Ordering};

use parking_lot::Mutex;
use rv_types::Digest;

const N_SHARDS: usize = 64;

/// An intrusive doubly-linked list node for LRU ordering.
struct LruNode {
    digest: Digest,
    /// Monotonic timestamp assigned on insert or touch, used to compare
    /// ages across shards during eviction.
    timestamp: u64,
    prev: *mut LruNode,
    next: *mut LruNode,
}

/// A single shard of the sharded LRU, containing a hash map for O(1) lookup
/// and an intrusive doubly-linked list for O(1) insert/remove/reorder.
///
/// The list is ordered from oldest (head) to newest (tail).
struct LruShard {
    map: HashMap<Digest, *mut LruNode>,
    head: *mut LruNode,
    tail: *mut LruNode,
}

impl LruShard {
    fn new() -> Self {
        Self {
            map: HashMap::new(),
            head: ptr::null_mut(),
            tail: ptr::null_mut(),
        }
    }

    /// Unlink a node from the doubly-linked list without deallocating it.
    ///
    /// # Safety
    /// `node` must be a valid, non-null pointer to a node that is currently
    /// linked in this shard's list.
    unsafe fn unlink(&mut self, node: *mut LruNode) {
        unsafe {
            let prev = (*node).prev;
            let next = (*node).next;

            if prev.is_null() {
                self.head = next;
            } else {
                (*prev).next = next;
            }

            if next.is_null() {
                self.tail = prev;
            } else {
                (*next).prev = prev;
            }

            (*node).prev = ptr::null_mut();
            (*node).next = ptr::null_mut();
        }
    }

    /// Push a node to the tail (most-recently-used position).
    ///
    /// # Safety
    /// `node` must be a valid, non-null pointer that is not currently linked
    /// in any list.
    unsafe fn push_tail(&mut self, node: *mut LruNode) {
        unsafe {
            (*node).prev = self.tail;
            (*node).next = ptr::null_mut();

            if self.tail.is_null() {
                self.head = node;
            } else {
                (*self.tail).next = node;
            }
            self.tail = node;
        }
    }

    /// Return the timestamp of the head (oldest) node, or None if empty.
    fn head_timestamp(&self) -> Option<u64> {
        if self.head.is_null() {
            None
        } else {
            // SAFETY: head is non-null and was allocated via Box::into_raw.
            Some(unsafe { (*self.head).timestamp })
        }
    }

    /// Pop the head node (least-recently-used) and return its digest.
    /// Also removes it from the hash map.
    fn pop_head(&mut self) -> Option<Digest> {
        if self.head.is_null() {
            return None;
        }

        let node = self.head;
        // SAFETY: head is non-null and was allocated via Box::into_raw.
        unsafe {
            let digest = (*node).digest;
            self.unlink(node);
            self.map.remove(&digest);
            drop(Box::from_raw(node));
            Some(digest)
        }
    }

    /// Insert a digest with the given timestamp. If it already exists,
    /// move it to the tail and update its timestamp. Otherwise, allocate
    /// a new node and push it to the tail.
    fn insert(&mut self, digest: Digest, timestamp: u64) {
        if let Some(&node) = self.map.get(&digest) {
            // SAFETY: node is valid because it is in our map and was allocated
            // via Box::into_raw.
            unsafe {
                (*node).timestamp = timestamp;
                self.unlink(node);
                self.push_tail(node);
            }
            return;
        }

        let node = Box::into_raw(Box::new(LruNode {
            digest,
            timestamp,
            prev: ptr::null_mut(),
            next: ptr::null_mut(),
        }));

        // SAFETY: node was just allocated above and is not linked.
        unsafe {
            self.push_tail(node);
        }
        self.map.insert(digest, node);
    }

    /// Touch a digest, moving it to the tail (most-recently-used) and
    /// updating its timestamp. Returns true if the digest was found.
    fn touch(&mut self, digest: &Digest, timestamp: u64) -> bool {
        if let Some(&node) = self.map.get(digest) {
            // SAFETY: node is valid because it is in our map.
            unsafe {
                (*node).timestamp = timestamp;
                self.unlink(node);
                self.push_tail(node);
            }
            true
        } else {
            false
        }
    }

    /// Remove a digest from this shard. Returns true if it was found.
    fn remove(&mut self, digest: &Digest) -> bool {
        if let Some(node) = self.map.remove(digest) {
            // SAFETY: node is valid because it was in our map.
            unsafe {
                self.unlink(node);
                drop(Box::from_raw(node));
            }
            true
        } else {
            false
        }
    }

    fn len(&self) -> usize {
        self.map.len()
    }
}

impl Drop for LruShard {
    fn drop(&mut self) {
        let mut current = self.head;
        while !current.is_null() {
            // SAFETY: current is non-null and was allocated via Box::into_raw.
            unsafe {
                let next = (*current).next;
                drop(Box::from_raw(current));
                current = next;
            }
        }
    }
}

/// O(1) sharded LRU eviction tracker.
///
/// Uses 64 shards, each with its own `parking_lot::Mutex`, to reduce
/// lock contention under high concurrency. Each shard contains a
/// `HashMap<Digest, *mut LruNode>` for O(1) lookup and an intrusive
/// doubly-linked list for O(1) insert, remove, and touch operations.
///
/// A global atomic counter assigns monotonically increasing timestamps
/// to nodes on insert and touch, allowing `evict_oldest()` to compare
/// head nodes across all shards and evict the globally oldest entry.
///
/// All operations are O(1) (with eviction being O(N_SHARDS) = O(64) which
/// is constant). This replaces the previous `VecDeque`-based tracker which
/// required O(n) linear scans for `touch()` and `remove()`.
pub struct ShardedLru {
    shards: Vec<Mutex<LruShard>>,
    capacity: usize,
    /// Global monotonic counter for assigning timestamps to nodes.
    clock: AtomicU64,
}

// SAFETY: The raw pointers in LruNode are only ever accessed while holding
// the per-shard Mutex, so ShardedLru is safe to share across threads.
unsafe impl Send for ShardedLru {}
unsafe impl Sync for ShardedLru {}

impl ShardedLru {
    /// Create a new sharded LRU tracker with the given maximum capacity.
    pub fn new(capacity: usize) -> Self {
        let mut shards = Vec::with_capacity(N_SHARDS);
        for _ in 0..N_SHARDS {
            shards.push(Mutex::new(LruShard::new()));
        }
        Self {
            shards,
            capacity,
            clock: AtomicU64::new(0),
        }
    }

    /// Allocate the next monotonic timestamp.
    #[inline]
    fn next_timestamp(&self) -> u64 {
        self.clock.fetch_add(1, Ordering::Relaxed)
    }

    /// Determine which shard a digest belongs to.
    #[inline]
    fn shard_index(digest: &Digest) -> usize {
        digest.bytes[0] as usize % N_SHARDS
    }

    /// Record a new digest insertion. If the digest already exists, it is
    /// moved to the most-recently-used position. Otherwise it is inserted
    /// at the tail.
    pub fn insert(&self, digest: Digest) {
        let ts = self.next_timestamp();
        let idx = Self::shard_index(&digest);
        self.shards[idx].lock().insert(digest, ts);
    }

    /// Touch a digest on access (cache hit), moving it to the
    /// most-recently-used position.
    pub fn touch(&self, digest: &Digest) {
        let ts = self.next_timestamp();
        let idx = Self::shard_index(digest);
        self.shards[idx].lock().touch(digest, ts);
    }

    /// Remove a specific digest (e.g. on purge or explicit expiry).
    pub fn remove(&self, digest: &Digest) {
        let idx = Self::shard_index(digest);
        self.shards[idx].lock().remove(digest);
    }

    /// Evict the globally least-recently-used digest by comparing the
    /// head timestamps across all shards and popping from the shard
    /// with the smallest (oldest) timestamp.
    ///
    /// Returns `None` if all shards are empty.
    pub fn evict_oldest(&self) -> Option<Digest> {
        // First pass: find the shard with the oldest head timestamp.
        // We lock each shard briefly to read the head timestamp, then
        // drop the lock before acquiring the winner.
        let mut oldest_shard: Option<usize> = None;
        let mut oldest_ts = u64::MAX;

        for (i, shard) in self.shards.iter().enumerate() {
            let guard = shard.lock();
            if let Some(ts) = guard.head_timestamp() {
                if ts < oldest_ts {
                    oldest_ts = ts;
                    oldest_shard = Some(i);
                }
            }
        }

        // Second pass: lock the winner shard and pop its head.
        if let Some(idx) = oldest_shard {
            self.shards[idx].lock().pop_head()
        } else {
            None
        }
    }

    /// The maximum number of objects this tracker allows before eviction
    /// should be triggered.
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// Current number of tracked digests across all shards.
    pub fn len(&self) -> usize {
        self.shards.iter().map(|s| s.lock().len()).sum()
    }

    /// Returns true if no digests are tracked.
    pub fn is_empty(&self) -> bool {
        self.shards.iter().all(|s| s.lock().len() == 0)
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
    fn test_new_is_empty() {
        let lru = ShardedLru::new(100);
        assert!(lru.is_empty());
        assert_eq!(lru.len(), 0);
        assert_eq!(lru.capacity(), 100);
    }

    #[test]
    fn test_insert_and_len() {
        let lru = ShardedLru::new(100);
        lru.insert(test_digest(1));
        lru.insert(test_digest(2));
        assert_eq!(lru.len(), 2);
        assert!(!lru.is_empty());
    }

    #[test]
    fn test_insert_deduplicates() {
        let lru = ShardedLru::new(100);
        lru.insert(test_digest(1));
        lru.insert(test_digest(1));
        assert_eq!(lru.len(), 1);
    }

    #[test]
    fn test_evict_oldest_fifo_order() {
        let lru = ShardedLru::new(100);
        let d1 = test_digest(1);
        let d2 = test_digest(2);

        lru.insert(d1);
        lru.insert(d2);

        // d1 was inserted first (lower timestamp) so it is evicted first,
        // regardless of which shard each digest belongs to.
        assert_eq!(lru.evict_oldest(), Some(d1));
        assert_eq!(lru.evict_oldest(), Some(d2));
        assert_eq!(lru.evict_oldest(), None);
    }

    #[test]
    fn test_evict_oldest_same_shard() {
        let lru = ShardedLru::new(100);
        // 0 and 64 both map to shard 0
        let d0 = test_digest(0);
        let d64 = test_digest(64);

        lru.insert(d0);
        lru.insert(d64);

        assert_eq!(lru.evict_oldest(), Some(d0));
        assert_eq!(lru.evict_oldest(), Some(d64));
        assert_eq!(lru.evict_oldest(), None);
    }

    #[test]
    fn test_touch_moves_to_back_same_shard() {
        let lru = ShardedLru::new(100);
        // Use same-shard digests so ordering within the list is visible
        let d0 = test_digest(0);
        let d64 = test_digest(64);
        let d128 = test_digest(128);

        lru.insert(d0);
        lru.insert(d64);
        lru.insert(d128);

        // Touch d0 -- moves it to the back within shard 0
        lru.touch(&d0);

        // Evict order within shard 0 should now be d64, d128, d0
        assert_eq!(lru.evict_oldest(), Some(d64));
        assert_eq!(lru.evict_oldest(), Some(d128));
        assert_eq!(lru.evict_oldest(), Some(d0));
    }

    #[test]
    fn test_touch_moves_to_back_cross_shard() {
        let lru = ShardedLru::new(100);
        let d1 = test_digest(1);
        let d2 = test_digest(2);
        let d3 = test_digest(3);

        lru.insert(d1);
        lru.insert(d2);
        lru.insert(d3);

        // Touch d1 -- gives it a newer timestamp
        lru.touch(&d1);

        // d2 now has the oldest timestamp globally, then d3, then d1
        assert_eq!(lru.evict_oldest(), Some(d2));
        assert_eq!(lru.evict_oldest(), Some(d3));
        assert_eq!(lru.evict_oldest(), Some(d1));
    }

    #[test]
    fn test_remove() {
        let lru = ShardedLru::new(100);
        let d1 = test_digest(1);
        let d2 = test_digest(2);

        lru.insert(d1);
        lru.insert(d2);

        lru.remove(&d1);
        assert_eq!(lru.len(), 1);

        let evicted = lru.evict_oldest().unwrap();
        assert_eq!(evicted, d2);
        assert!(lru.is_empty());
    }

    #[test]
    fn test_remove_nonexistent_is_noop() {
        let lru = ShardedLru::new(100);
        lru.remove(&test_digest(99));
        assert!(lru.is_empty());
    }

    #[test]
    fn test_touch_nonexistent_is_noop() {
        let lru = ShardedLru::new(100);
        lru.touch(&test_digest(99));
        assert!(lru.is_empty());
    }

    #[test]
    fn test_insert_remove_insert() {
        let lru = ShardedLru::new(100);
        let d1 = test_digest(1);

        lru.insert(d1);
        lru.remove(&d1);
        assert!(lru.is_empty());

        lru.insert(d1);
        assert_eq!(lru.len(), 1);
        assert_eq!(lru.evict_oldest(), Some(d1));
    }

    #[test]
    fn test_many_inserts_and_evictions() {
        let lru = ShardedLru::new(1000);

        for i in 0u8..200 {
            lru.insert(test_digest(i));
        }
        assert_eq!(lru.len(), 200);

        // Evict all -- should return them in insertion order (by timestamp)
        let mut evicted = Vec::new();
        while let Some(d) = lru.evict_oldest() {
            evicted.push(d);
        }
        assert_eq!(evicted.len(), 200);
        assert!(lru.is_empty());

        // Verify they came out in insertion order
        for (i, d) in evicted.iter().enumerate() {
            assert_eq!(d.bytes[0], i as u8);
        }
    }

    #[test]
    fn test_single_element_operations() {
        let lru = ShardedLru::new(10);
        let d = test_digest(42);

        lru.insert(d);
        assert_eq!(lru.len(), 1);

        lru.touch(&d);
        assert_eq!(lru.len(), 1);

        assert_eq!(lru.evict_oldest(), Some(d));
        assert!(lru.is_empty());
    }

    #[test]
    fn test_drop_frees_all_nodes() {
        // This test primarily checks that Drop does not leak or double-free.
        // Run under miri or valgrind for full verification.
        let lru = ShardedLru::new(100);
        for i in 0u8..50 {
            lru.insert(test_digest(i));
        }
        drop(lru);
    }
}

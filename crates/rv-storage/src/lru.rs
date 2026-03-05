//! Index-based doubly-linked LRU eviction list.
//!
//! The list is backed by a `Vec<LruEntry>` where each entry stores forward and
//! backward indices rather than raw pointers. This avoids the unsafety of
//! pointer-based linked lists while still providing O(1) insert, remove, and
//! touch operations.
//!
//! A key of type `u64` is associated with each entry so the caller can map
//! evicted entries back to the corresponding `ObjCore`.

use parking_lot::Mutex;

/// A single node in the doubly-linked list.
#[derive(Debug)]
struct LruEntry {
    /// Caller-supplied identifier (e.g., a hash of the digest).
    key: u64,
    /// Index of the previous (more recently used) entry, or `None` if head.
    prev: Option<usize>,
    /// Index of the next (less recently used) entry, or `None` if tail.
    next: Option<usize>,
    /// Whether this slot is occupied.
    active: bool,
}

/// An index-based doubly-linked LRU list.
///
/// The most recently used entry is at the head; the least recently used is at
/// the tail. Eviction removes from the tail.
pub struct Lru {
    entries: Vec<LruEntry>,
    head: Option<usize>,
    tail: Option<usize>,
    len: usize,
    /// Indices of slots that have been removed and can be reused.
    free_list: Vec<usize>,
}

impl Lru {
    /// Creates a new empty LRU list.
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
            head: None,
            tail: None,
            len: 0,
            free_list: Vec::new(),
        }
    }

    /// Creates a new empty LRU list with pre-allocated capacity.
    pub fn with_capacity(cap: usize) -> Self {
        Self {
            entries: Vec::with_capacity(cap),
            head: None,
            tail: None,
            len: 0,
            free_list: Vec::new(),
        }
    }

    /// Returns the number of active entries.
    pub fn len(&self) -> usize {
        self.len
    }

    /// Returns `true` if there are no active entries.
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Inserts a new entry at the head (most recently used position).
    ///
    /// Returns the index of the newly inserted entry which should be stored
    /// alongside the object for future `touch` and `remove` calls.
    pub fn insert(&mut self, key: u64) -> usize {
        let idx = if let Some(free_idx) = self.free_list.pop() {
            // Reuse a previously freed slot.
            self.entries[free_idx] = LruEntry {
                key,
                prev: None,
                next: self.head,
                active: true,
            };
            free_idx
        } else {
            // Append a new slot.
            let idx = self.entries.len();
            self.entries.push(LruEntry {
                key,
                prev: None,
                next: self.head,
                active: true,
            });
            idx
        };

        // Link the old head's prev to the new entry.
        if let Some(old_head) = self.head {
            self.entries[old_head].prev = Some(idx);
        }

        self.head = Some(idx);

        // If this is the first entry, it is also the tail.
        if self.tail.is_none() {
            self.tail = Some(idx);
        }

        self.len += 1;
        idx
    }

    /// Moves an existing entry to the head (most recently used position).
    ///
    /// This is a no-op if the entry is already at the head or the index is
    /// invalid.
    pub fn touch(&mut self, idx: usize) {
        if idx >= self.entries.len() || !self.entries[idx].active {
            return;
        }

        // Already at the head -- nothing to do.
        if self.head == Some(idx) {
            return;
        }

        // Unlink from current position.
        self.unlink(idx);

        // Re-insert at head.
        self.entries[idx].prev = None;
        self.entries[idx].next = self.head;

        if let Some(old_head) = self.head {
            self.entries[old_head].prev = Some(idx);
        }

        self.head = Some(idx);

        if self.tail.is_none() {
            self.tail = Some(idx);
        }
    }

    /// Removes the entry at `idx` from the list.
    ///
    /// The slot is added to the free list for reuse. Returns the key of the
    /// removed entry, or `None` if the index was invalid.
    pub fn remove(&mut self, idx: usize) -> Option<u64> {
        if idx >= self.entries.len() || !self.entries[idx].active {
            return None;
        }

        self.unlink(idx);
        self.entries[idx].active = false;
        self.len -= 1;
        self.free_list.push(idx);
        Some(self.entries[idx].key)
    }

    /// Evicts and returns the key of the least recently used (tail) entry.
    ///
    /// Returns `None` if the list is empty.
    pub fn evict_oldest(&mut self) -> Option<u64> {
        let tail_idx = self.tail?;
        self.remove(tail_idx)
    }

    /// Returns the key at the head (most recently used), without removing it.
    pub fn peek_newest(&self) -> Option<u64> {
        self.head.map(|idx| self.entries[idx].key)
    }

    /// Returns the key at the tail (least recently used), without removing it.
    pub fn peek_oldest(&self) -> Option<u64> {
        self.tail.map(|idx| self.entries[idx].key)
    }

    // ---------------------------------------------------------------
    // Internal helpers
    // ---------------------------------------------------------------

    /// Unlinks an entry from its neighbors without marking it inactive.
    fn unlink(&mut self, idx: usize) {
        let prev = self.entries[idx].prev;
        let next = self.entries[idx].next;

        if let Some(p) = prev {
            self.entries[p].next = next;
        } else {
            // This entry was the head.
            self.head = next;
        }

        if let Some(n) = next {
            self.entries[n].prev = prev;
        } else {
            // This entry was the tail.
            self.tail = prev;
        }

        self.entries[idx].prev = None;
        self.entries[idx].next = None;
    }
}

impl Default for Lru {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for Lru {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Lru")
            .field("len", &self.len)
            .field("head", &self.head)
            .field("tail", &self.tail)
            .finish()
    }
}

/// A thread-safe wrapper around [`Lru`].
///
/// All operations acquire the internal mutex. This is suitable for moderate
/// contention; under very high concurrency a sharded design may be preferable.
pub struct SyncLru {
    inner: Mutex<Lru>,
}

impl SyncLru {
    /// Creates a new thread-safe LRU list.
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(Lru::new()),
        }
    }

    /// Creates a new thread-safe LRU list with pre-allocated capacity.
    pub fn with_capacity(cap: usize) -> Self {
        Self {
            inner: Mutex::new(Lru::with_capacity(cap)),
        }
    }

    pub fn insert(&self, key: u64) -> usize {
        self.inner.lock().insert(key)
    }

    pub fn touch(&self, idx: usize) {
        self.inner.lock().touch(idx);
    }

    pub fn remove(&self, idx: usize) -> Option<u64> {
        self.inner.lock().remove(idx)
    }

    pub fn evict_oldest(&self) -> Option<u64> {
        self.inner.lock().evict_oldest()
    }

    pub fn len(&self) -> usize {
        self.inner.lock().len()
    }

    pub fn is_empty(&self) -> bool {
        self.inner.lock().is_empty()
    }
}

impl Default for SyncLru {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_insert_and_len() {
        let mut lru = Lru::new();
        assert!(lru.is_empty());
        lru.insert(10);
        lru.insert(20);
        lru.insert(30);
        assert_eq!(lru.len(), 3);
    }

    #[test]
    fn test_evict_oldest_order() {
        let mut lru = Lru::new();
        lru.insert(10);
        lru.insert(20);
        lru.insert(30);

        // Oldest inserted first => 10 is the tail.
        assert_eq!(lru.evict_oldest(), Some(10));
        assert_eq!(lru.evict_oldest(), Some(20));
        assert_eq!(lru.evict_oldest(), Some(30));
        assert_eq!(lru.evict_oldest(), None);
        assert!(lru.is_empty());
    }

    #[test]
    fn test_touch_moves_to_head() {
        let mut lru = Lru::new();
        let a = lru.insert(10);
        let _b = lru.insert(20);
        let _c = lru.insert(30);

        // Order is now head=[30, 20, 10]=tail
        // Touch 10 => head=[10, 30, 20]=tail
        lru.touch(a);

        assert_eq!(lru.peek_newest(), Some(10));
        assert_eq!(lru.peek_oldest(), Some(20));

        assert_eq!(lru.evict_oldest(), Some(20));
        assert_eq!(lru.evict_oldest(), Some(30));
        assert_eq!(lru.evict_oldest(), Some(10));
    }

    #[test]
    fn test_remove_middle() {
        let mut lru = Lru::new();
        let _a = lru.insert(10);
        let b = lru.insert(20);
        let _c = lru.insert(30);

        assert_eq!(lru.remove(b), Some(20));
        assert_eq!(lru.len(), 2);

        assert_eq!(lru.evict_oldest(), Some(10));
        assert_eq!(lru.evict_oldest(), Some(30));
    }

    #[test]
    fn test_remove_head() {
        let mut lru = Lru::new();
        let _a = lru.insert(10);
        let _b = lru.insert(20);
        let c = lru.insert(30);

        // 30 is at the head.
        assert_eq!(lru.remove(c), Some(30));
        assert_eq!(lru.peek_newest(), Some(20));
        assert_eq!(lru.len(), 2);
    }

    #[test]
    fn test_remove_tail() {
        let mut lru = Lru::new();
        let a = lru.insert(10);
        let _b = lru.insert(20);
        let _c = lru.insert(30);

        // 10 is at the tail.
        assert_eq!(lru.remove(a), Some(10));
        assert_eq!(lru.peek_oldest(), Some(20));
        assert_eq!(lru.len(), 2);
    }

    #[test]
    fn test_free_list_reuse() {
        let mut lru = Lru::new();
        let a = lru.insert(10);
        lru.remove(a);
        // The slot should be reused.
        let b = lru.insert(20);
        assert_eq!(a, b);
        assert_eq!(lru.evict_oldest(), Some(20));
    }

    #[test]
    fn test_single_element() {
        let mut lru = Lru::new();
        let a = lru.insert(42);
        assert_eq!(lru.peek_newest(), Some(42));
        assert_eq!(lru.peek_oldest(), Some(42));

        // Touch on single element is a no-op but should not break anything.
        lru.touch(a);
        assert_eq!(lru.peek_newest(), Some(42));

        assert_eq!(lru.evict_oldest(), Some(42));
        assert!(lru.is_empty());
    }

    #[test]
    fn test_touch_invalid_index() {
        let mut lru = Lru::new();
        lru.insert(10);
        // Should not panic.
        lru.touch(999);
    }

    #[test]
    fn test_remove_invalid_index() {
        let mut lru = Lru::new();
        assert_eq!(lru.remove(0), None);
        assert_eq!(lru.remove(999), None);
    }

    #[test]
    fn test_sync_lru_basic() {
        let lru = SyncLru::new();
        let a = lru.insert(1);
        let _b = lru.insert(2);
        let _c = lru.insert(3);

        lru.touch(a);
        assert_eq!(lru.len(), 3);
        assert_eq!(lru.evict_oldest(), Some(2));
    }
}

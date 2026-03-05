use std::sync::atomic::{AtomicU32, Ordering};

use parking_lot::Mutex;
use rv_types::Digest;

/// ObjHead represents a hash bucket -- the top-level entry in the cache
/// object lookup chain. Each unique Digest maps to exactly one ObjHead.
///
/// In the full implementation, an ObjHead would contain a list of ObjCore
/// entries (variants of the same cached object, e.g. different Vary matches).
/// For now we track the object count as a placeholder.
///
/// The refcount tracks how many active references exist. When it drops to
/// zero, the ObjHead can be removed from the hash table.
pub struct ObjHead {
    /// The digest (hash key) that this ObjHead corresponds to.
    pub digest: Digest,

    /// Reference count. Protected by atomic operations.
    /// Starts at 1 when inserted into the hash table (the hash table itself
    /// holds a reference). Additional references are taken by worker threads
    /// performing lookups.
    pub refcnt: AtomicU32,

    /// Per-ObjHead mutex used to serialize operations on this bucket
    /// (e.g. object insertion, busy-wait, vary matching).
    pub mtx: Mutex<()>,

    /// Number of ObjCore entries attached to this ObjHead.
    /// Placeholder for the full object list.
    pub obj_count: AtomicU32,
}

impl ObjHead {
    /// Create a new ObjHead for the given digest.
    /// Initial refcount is 0 -- the hash implementation will increment it
    /// when inserting into the table.
    pub fn new(digest: Digest) -> Self {
        Self {
            digest,
            refcnt: AtomicU32::new(0),
            mtx: Mutex::new(()),
            obj_count: AtomicU32::new(0),
        }
    }

    /// Returns the current reference count.
    pub fn ref_count(&self) -> u32 {
        self.refcnt.load(Ordering::Acquire)
    }

    /// Atomically increment the reference count. Returns the new value.
    pub fn inc_ref(&self) -> u32 {
        let prev = self.refcnt.fetch_add(1, Ordering::AcqRel);
        prev + 1
    }

    /// Atomically decrement the reference count. Returns the new value.
    ///
    /// # Panics
    /// Debug-asserts that the refcount does not underflow.
    pub fn dec_ref(&self) -> u32 {
        let prev = self.refcnt.fetch_sub(1, Ordering::AcqRel);
        debug_assert!(prev > 0, "ObjHead refcount underflow");
        prev - 1
    }
}

impl std::fmt::Debug for ObjHead {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ObjHead")
            .field("digest", &self.digest)
            .field("refcnt", &self.refcnt.load(Ordering::Relaxed))
            .field("obj_count", &self.obj_count.load(Ordering::Relaxed))
            .finish()
    }
}

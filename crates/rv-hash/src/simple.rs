use std::sync::Arc;

use parking_lot::RwLock;
use rv_types::Digest;

use crate::objhead::ObjHead;
use crate::traits::HashSlinger;

/// SimpleListHash is a trivial hash implementation using a single list.
///
/// All ObjHead entries are stored in a Vec protected by a single RwLock.
/// Lookup is O(n) linear scan. This implementation is intended for testing
/// and as a reference baseline -- it is not suitable for production use
/// with large numbers of cached objects.
///
/// This mirrors Varnish's hash_simple implementation.
pub struct SimpleListHash {
    entries: RwLock<Vec<Arc<ObjHead>>>,
}

impl SimpleListHash {
    pub fn new() -> Self {
        Self {
            entries: RwLock::new(Vec::new()),
        }
    }
}

impl Default for SimpleListHash {
    fn default() -> Self {
        Self::new()
    }
}

impl HashSlinger for SimpleListHash {
    fn name(&self) -> &str {
        "simple_list"
    }

    fn start(&self) {
        // No initialization needed for simple list.
    }

    fn lookup(&self, digest: &Digest, new_oh: Arc<ObjHead>) -> (Arc<ObjHead>, Option<Arc<ObjHead>>) {
        // Fast path: check with a read lock first.
        {
            let entries = self.entries.read();
            for entry in entries.iter() {
                if entry.digest == *digest {
                    entry.inc_ref();
                    return (Arc::clone(entry), Some(new_oh));
                }
            }
        }

        // Slow path: acquire write lock and re-check before inserting.
        let mut entries = self.entries.write();

        // Double-check: another thread may have inserted while we waited for the write lock.
        for entry in entries.iter() {
            if entry.digest == *digest {
                entry.inc_ref();
                return (Arc::clone(entry), Some(new_oh));
            }
        }

        // Not found -- insert the new ObjHead.
        new_oh.inc_ref();
        entries.push(Arc::clone(&new_oh));
        (new_oh, None)
    }

    fn deref(&self, oh: &Arc<ObjHead>) -> bool {
        let new_count = oh.dec_ref();
        if new_count == 0 {
            let mut entries = self.entries.write();
            // Find and remove by pointer identity.
            if let Some(pos) = entries.iter().position(|e| Arc::ptr_eq(e, oh)) {
                entries.swap_remove(pos);
            }
            true
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rv_types::Digest;

    fn make_digest(val: u8) -> Digest {
        let mut bytes = [0u8; 32];
        bytes[0] = val;
        Digest::new(bytes)
    }

    #[test]
    fn test_insert_and_lookup() {
        let hash = SimpleListHash::new();
        hash.start();

        let d1 = make_digest(1);
        let oh1 = Arc::new(ObjHead::new(d1));
        let (found, unused) = hash.lookup(&d1, oh1);
        assert!(unused.is_none(), "new_oh should be consumed on first insert");
        assert_eq!(found.ref_count(), 1);
        assert_eq!(found.digest, d1);
    }

    #[test]
    fn test_lookup_existing_returns_same() {
        let hash = SimpleListHash::new();
        hash.start();

        let d1 = make_digest(1);
        let oh1 = Arc::new(ObjHead::new(d1));
        let (first, _) = hash.lookup(&d1, oh1);
        assert_eq!(first.ref_count(), 1);

        // Second lookup for the same digest should return the same ObjHead.
        let oh2 = Arc::new(ObjHead::new(d1));
        let (second, unused) = hash.lookup(&d1, oh2.clone());
        assert!(unused.is_some(), "new_oh should be returned unused");
        assert!(Arc::ptr_eq(&first, &second));
        assert_eq!(second.ref_count(), 2);

        // The unused ObjHead should be the one we passed in.
        assert!(Arc::ptr_eq(&unused.unwrap(), &oh2));
    }

    #[test]
    fn test_deref_removes_at_zero() {
        let hash = SimpleListHash::new();
        hash.start();

        let d1 = make_digest(1);
        let oh1 = Arc::new(ObjHead::new(d1));
        let (found, _) = hash.lookup(&d1, oh1);
        assert_eq!(found.ref_count(), 1);

        let removed = hash.deref(&found);
        assert!(removed, "should be removed when refcount drops to 0");
        assert_eq!(found.ref_count(), 0);

        // A new lookup should not find the old entry.
        let oh_new = Arc::new(ObjHead::new(d1));
        let (fresh, unused) = hash.lookup(&d1, oh_new);
        assert!(unused.is_none(), "should insert anew after removal");
        assert!(!Arc::ptr_eq(&found, &fresh));
    }

    #[test]
    fn test_deref_does_not_remove_with_remaining_refs() {
        let hash = SimpleListHash::new();
        hash.start();

        let d1 = make_digest(1);
        let oh1 = Arc::new(ObjHead::new(d1));
        let (found, _) = hash.lookup(&d1, oh1);

        // Take a second reference.
        let oh2 = Arc::new(ObjHead::new(d1));
        let (same, _) = hash.lookup(&d1, oh2);
        assert_eq!(same.ref_count(), 2);

        let removed = hash.deref(&found);
        assert!(!removed, "should not remove with remaining refs");
        assert_eq!(found.ref_count(), 1);
    }

    #[test]
    fn test_multiple_distinct_digests() {
        let hash = SimpleListHash::new();
        hash.start();

        let d1 = make_digest(1);
        let d2 = make_digest(2);
        let d3 = make_digest(3);

        let (oh1, _) = hash.lookup(&d1, Arc::new(ObjHead::new(d1)));
        let (oh2, _) = hash.lookup(&d2, Arc::new(ObjHead::new(d2)));
        let (oh3, _) = hash.lookup(&d3, Arc::new(ObjHead::new(d3)));

        assert!(!Arc::ptr_eq(&oh1, &oh2));
        assert!(!Arc::ptr_eq(&oh2, &oh3));
        assert_eq!(oh1.digest, d1);
        assert_eq!(oh2.digest, d2);
        assert_eq!(oh3.digest, d3);
    }

    #[test]
    fn test_concurrent_inserts() {
        use std::thread;

        let hash = Arc::new(SimpleListHash::new());
        hash.start();

        let digest = make_digest(42);
        let mut handles = Vec::new();

        for _ in 0..8 {
            let h = Arc::clone(&hash);
            let d = digest;
            handles.push(thread::spawn(move || {
                let oh = Arc::new(ObjHead::new(d));
                h.lookup(&d, oh)
            }));
        }

        let results: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();

        // All threads should find or insert the same ObjHead.
        let first = &results[0].0;
        for (oh, _) in &results {
            assert!(Arc::ptr_eq(first, oh));
        }

        // Exactly one thread should have consumed its new_oh (the inserter).
        let insert_count = results.iter().filter(|(_, unused)| unused.is_none()).count();
        assert_eq!(insert_count, 1, "exactly one thread should insert");

        assert_eq!(first.ref_count(), 8);
    }
}

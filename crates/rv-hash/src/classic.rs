use std::sync::Arc;

use parking_lot::Mutex;
use rv_types::Digest;

use crate::objhead::ObjHead;
use crate::traits::HashSlinger;

/// Default number of buckets. This is the same prime used by Varnish's
/// classic hash implementation (hash_classic.c). A prime bucket count
/// provides better distribution for the modulo-based bucket selection.
const DEFAULT_BUCKETS: usize = 16381;

/// ClassicHash is a fixed-size hash table with separate chaining.
///
/// Each bucket is a Vec<Arc<ObjHead>> protected by its own Mutex, so
/// operations on different buckets proceed in parallel with zero contention.
/// Bucket selection uses the first bytes of the digest interpreted as a
/// little-endian integer, reduced modulo the bucket count.
///
/// Lookup within a bucket is O(n/buckets) amortized linear scan.
///
/// This mirrors Varnish's hash_classic implementation.
pub struct ClassicHash {
    buckets: Vec<Mutex<Vec<Arc<ObjHead>>>>,
    n_buckets: usize,
}

impl ClassicHash {
    /// Create a ClassicHash with the default number of buckets (16381).
    pub fn new() -> Self {
        Self::with_buckets(DEFAULT_BUCKETS)
    }

    /// Create a ClassicHash with a specified number of buckets.
    pub fn with_buckets(n_buckets: usize) -> Self {
        assert!(n_buckets > 0, "bucket count must be positive");
        let mut buckets = Vec::with_capacity(n_buckets);
        for _ in 0..n_buckets {
            buckets.push(Mutex::new(Vec::new()));
        }
        Self { buckets, n_buckets }
    }

    /// Compute the bucket index for a given digest.
    /// Uses the first 8 bytes of the digest as a little-endian u64,
    /// then reduces modulo the bucket count.
    fn bucket_index(&self, digest: &Digest) -> usize {
        let val = u64::from_le_bytes([
            digest.bytes[0],
            digest.bytes[1],
            digest.bytes[2],
            digest.bytes[3],
            digest.bytes[4],
            digest.bytes[5],
            digest.bytes[6],
            digest.bytes[7],
        ]);
        (val % self.n_buckets as u64) as usize
    }
}

impl Default for ClassicHash {
    fn default() -> Self {
        Self::new()
    }
}

impl HashSlinger for ClassicHash {
    fn name(&self) -> &str {
        "classic"
    }

    fn start(&self) {
        // No additional initialization needed.
    }

    fn lookup(&self, digest: &Digest, new_oh: Arc<ObjHead>) -> (Arc<ObjHead>, Option<Arc<ObjHead>>) {
        let idx = self.bucket_index(digest);
        let mut bucket = self.buckets[idx].lock();

        // Search the bucket for an existing entry.
        for entry in bucket.iter() {
            if entry.digest == *digest {
                entry.inc_ref();
                return (Arc::clone(entry), Some(new_oh));
            }
        }

        // Not found -- insert the new ObjHead.
        new_oh.inc_ref();
        bucket.push(Arc::clone(&new_oh));
        (new_oh, None)
    }

    fn deref(&self, oh: &Arc<ObjHead>) -> bool {
        let new_count = oh.dec_ref();
        if new_count == 0 {
            let idx = self.bucket_index(&oh.digest);
            let mut bucket = self.buckets[idx].lock();
            if let Some(pos) = bucket.iter().position(|e| Arc::ptr_eq(e, oh)) {
                bucket.swap_remove(pos);
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

    fn make_digest_u32(val: u32) -> Digest {
        let mut bytes = [0u8; 32];
        bytes[..4].copy_from_slice(&val.to_le_bytes());
        Digest::new(bytes)
    }

    #[test]
    fn test_insert_and_lookup() {
        let hash = ClassicHash::new();
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
        let hash = ClassicHash::new();
        hash.start();

        let d1 = make_digest(1);
        let oh1 = Arc::new(ObjHead::new(d1));
        let (first, _) = hash.lookup(&d1, oh1);

        let oh2 = Arc::new(ObjHead::new(d1));
        let (second, unused) = hash.lookup(&d1, oh2.clone());
        assert!(unused.is_some());
        assert!(Arc::ptr_eq(&first, &second));
        assert_eq!(second.ref_count(), 2);
        assert!(Arc::ptr_eq(&unused.unwrap(), &oh2));
    }

    #[test]
    fn test_deref_removes_at_zero() {
        let hash = ClassicHash::new();
        hash.start();

        let d1 = make_digest(1);
        let oh1 = Arc::new(ObjHead::new(d1));
        let (found, _) = hash.lookup(&d1, oh1);

        let removed = hash.deref(&found);
        assert!(removed);

        // Re-insert should create a fresh entry.
        let oh_new = Arc::new(ObjHead::new(d1));
        let (fresh, unused) = hash.lookup(&d1, oh_new);
        assert!(unused.is_none());
        assert!(!Arc::ptr_eq(&found, &fresh));
    }

    #[test]
    fn test_deref_does_not_remove_with_remaining_refs() {
        let hash = ClassicHash::new();
        hash.start();

        let d1 = make_digest(1);
        let oh1 = Arc::new(ObjHead::new(d1));
        let (found, _) = hash.lookup(&d1, oh1);

        let oh2 = Arc::new(ObjHead::new(d1));
        hash.lookup(&d1, oh2);
        assert_eq!(found.ref_count(), 2);

        let removed = hash.deref(&found);
        assert!(!removed);
        assert_eq!(found.ref_count(), 1);
    }

    #[test]
    fn test_bucket_distribution() {
        // Use a small number of buckets to verify different digests land in different buckets.
        let hash = ClassicHash::with_buckets(4);
        hash.start();

        // Insert many distinct digests.
        let mut heads = Vec::new();
        for i in 0u32..20 {
            let d = make_digest_u32(i);
            let oh = Arc::new(ObjHead::new(d));
            let (found, _) = hash.lookup(&d, oh);
            heads.push(found);
        }

        // Verify all are distinct and retrievable.
        for (i, oh) in heads.iter().enumerate() {
            let d = make_digest_u32(i as u32);
            assert_eq!(oh.digest, d);
        }
    }

    #[test]
    fn test_concurrent_inserts() {
        use std::thread;

        let hash = Arc::new(ClassicHash::new());
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

        let first = &results[0].0;
        for (oh, _) in &results {
            assert!(Arc::ptr_eq(first, oh));
        }

        let insert_count = results.iter().filter(|(_, unused)| unused.is_none()).count();
        assert_eq!(insert_count, 1);

        assert_eq!(first.ref_count(), 8);
    }

    #[test]
    fn test_concurrent_different_digests() {
        use std::thread;

        let hash = Arc::new(ClassicHash::new());
        hash.start();

        let mut handles = Vec::new();

        for i in 0u8..16 {
            let h = Arc::clone(&hash);
            handles.push(thread::spawn(move || {
                let d = make_digest(i);
                let oh = Arc::new(ObjHead::new(d));
                let (found, unused) = h.lookup(&d, oh);
                assert!(unused.is_none());
                assert_eq!(found.digest, d);
                found
            }));
        }

        let results: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();

        // All should be distinct ObjHead instances.
        for i in 0..results.len() {
            for j in (i + 1)..results.len() {
                assert!(!Arc::ptr_eq(&results[i], &results[j]));
            }
        }
    }
}

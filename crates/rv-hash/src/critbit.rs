use std::sync::Arc;

use parking_lot::RwLock;
use rv_types::Digest;
use rv_types::digest::DIGEST_LEN;

use crate::objhead::ObjHead;
use crate::traits::HashSlinger;

/// A node in the crit-bit tree.
///
/// Internal nodes store the critical bit position where two keys diverge,
/// along with left and right children. Leaf nodes store an Arc<ObjHead>.
///
/// The crit-bit tree provides O(key_length) lookup, insert, and delete --
/// in practice O(256) for 32-byte digests, which is effectively O(1) and
/// much better than the O(n) or O(n/buckets) of the list-based approaches
/// for large working sets.
enum CritbitNode {
    /// Internal node. `bit_pos` is the index of the first differing bit.
    /// `left` is followed when the bit at `bit_pos` in the lookup key is 0.
    /// `right` is followed when that bit is 1.
    Internal {
        bit_pos: usize,
        left: Box<CritbitNode>,
        right: Box<CritbitNode>,
    },
    /// Leaf node containing an ObjHead.
    Leaf(Arc<ObjHead>),
}

/// CritbitHash is a crit-bit tree (binary trie) over digest bytes.
///
/// The tree is protected by a single RwLock. Reads take a shared lock
/// and writes take an exclusive lock. For workloads that are read-heavy
/// (which cache lookups typically are), this provides good concurrency.
///
/// This mirrors the concept from Varnish's hash_critbit implementation,
/// adapted for Rust's ownership model with Arc-based ObjHead references.
pub struct CritbitHash {
    root: RwLock<Option<CritbitNode>>,
}

impl CritbitHash {
    pub fn new() -> Self {
        Self {
            root: RwLock::new(None),
        }
    }
}

impl Default for CritbitHash {
    fn default() -> Self {
        Self::new()
    }
}

/// Extract bit at position `bit_pos` from a digest's bytes.
/// Bit 0 is the most significant bit of byte 0, bit 7 is the least
/// significant bit of byte 0, bit 8 is the MSB of byte 1, etc.
#[inline]
fn get_bit(digest: &Digest, bit_pos: usize) -> u8 {
    let byte_idx = bit_pos / 8;
    let bit_idx = 7 - (bit_pos % 8);
    (digest.bytes[byte_idx] >> bit_idx) & 1
}

/// Find the first bit position where two digests differ.
/// Returns None if the digests are identical.
fn find_critical_bit(a: &Digest, b: &Digest) -> Option<usize> {
    for i in 0..DIGEST_LEN {
        let diff = a.bytes[i] ^ b.bytes[i];
        if diff != 0 {
            // Find the highest set bit in diff (first differing bit).
            let bit_in_byte = 7 - (diff.leading_zeros() as usize);
            let bit_pos = i * 8 + (7 - bit_in_byte);
            return Some(bit_pos);
        }
    }
    None
}

/// Walk the tree to find the leaf that the given digest would map to.
/// Returns a reference to the leaf's ObjHead if found.
fn walk_to_leaf<'a>(node: &'a CritbitNode, digest: &Digest) -> &'a Arc<ObjHead> {
    match node {
        CritbitNode::Leaf(oh) => oh,
        CritbitNode::Internal {
            bit_pos,
            left,
            right,
        } => {
            if get_bit(digest, *bit_pos) == 0 {
                walk_to_leaf(left, digest)
            } else {
                walk_to_leaf(right, digest)
            }
        }
    }
}

/// Insert a new leaf into the tree, returning the new root.
/// The `crit_bit` is the bit position where the new digest differs from
/// the existing leaf that it would collide with.
fn insert_node(
    node: CritbitNode,
    new_leaf: CritbitNode,
    crit_bit: usize,
    new_digest: &Digest,
) -> CritbitNode {
    match node {
        CritbitNode::Internal {
            bit_pos,
            left,
            right,
        } => {
            // If the new critical bit is higher up (lower position number)
            // than this internal node, the new node becomes a parent of this subtree.
            if crit_bit < bit_pos {
                if get_bit(new_digest, crit_bit) == 0 {
                    CritbitNode::Internal {
                        bit_pos: crit_bit,
                        left: Box::new(new_leaf),
                        right: Box::new(CritbitNode::Internal {
                            bit_pos,
                            left,
                            right,
                        }),
                    }
                } else {
                    CritbitNode::Internal {
                        bit_pos: crit_bit,
                        left: Box::new(CritbitNode::Internal {
                            bit_pos,
                            left,
                            right,
                        }),
                        right: Box::new(new_leaf),
                    }
                }
            } else {
                // Recurse into the appropriate subtree.
                if get_bit(new_digest, bit_pos) == 0 {
                    CritbitNode::Internal {
                        bit_pos,
                        left: Box::new(insert_node(*left, new_leaf, crit_bit, new_digest)),
                        right,
                    }
                } else {
                    CritbitNode::Internal {
                        bit_pos,
                        left,
                        right: Box::new(insert_node(*right, new_leaf, crit_bit, new_digest)),
                    }
                }
            }
        }
        CritbitNode::Leaf(_) => {
            // We have reached a leaf. Create an internal node that splits
            // at the critical bit, with the existing leaf and new leaf as children.
            if get_bit(new_digest, crit_bit) == 0 {
                CritbitNode::Internal {
                    bit_pos: crit_bit,
                    left: Box::new(new_leaf),
                    right: Box::new(node),
                }
            } else {
                CritbitNode::Internal {
                    bit_pos: crit_bit,
                    left: Box::new(node),
                    right: Box::new(new_leaf),
                }
            }
        }
    }
}

/// Remove a digest from the tree. Returns the new tree and the removed ObjHead (if any).
fn remove_node(node: CritbitNode, digest: &Digest) -> (Option<CritbitNode>, Option<Arc<ObjHead>>) {
    match node {
        CritbitNode::Leaf(ref oh) => {
            if oh.digest == *digest {
                // Destructure to get ownership of the Arc.
                if let CritbitNode::Leaf(oh) = node {
                    (None, Some(oh))
                } else {
                    unreachable!()
                }
            } else {
                (Some(node), None)
            }
        }
        CritbitNode::Internal {
            bit_pos,
            left,
            right,
        } => {
            if get_bit(digest, bit_pos) == 0 {
                let (new_left, removed) = remove_node(*left, digest);
                if removed.is_some() {
                    match new_left {
                        Some(l) => (
                            Some(CritbitNode::Internal {
                                bit_pos,
                                left: Box::new(l),
                                right,
                            }),
                            removed,
                        ),
                        None => {
                            // Left child was removed, promote right child.
                            (Some(*right), removed)
                        }
                    }
                } else {
                    (
                        Some(CritbitNode::Internal {
                            bit_pos,
                            left: Box::new(new_left.unwrap()),
                            right,
                        }),
                        None,
                    )
                }
            } else {
                let (new_right, removed) = remove_node(*right, digest);
                if removed.is_some() {
                    match new_right {
                        Some(r) => (
                            Some(CritbitNode::Internal {
                                bit_pos,
                                left,
                                right: Box::new(r),
                            }),
                            removed,
                        ),
                        None => {
                            // Right child was removed, promote left child.
                            (Some(*left), removed)
                        }
                    }
                } else {
                    (
                        Some(CritbitNode::Internal {
                            bit_pos,
                            left,
                            right: Box::new(new_right.unwrap()),
                        }),
                        None,
                    )
                }
            }
        }
    }
}

impl HashSlinger for CritbitHash {
    fn name(&self) -> &str {
        "critbit"
    }

    fn start(&self) {
        // No initialization needed.
    }

    fn lookup(
        &self,
        digest: &Digest,
        new_oh: Arc<ObjHead>,
    ) -> (Arc<ObjHead>, Option<Arc<ObjHead>>) {
        // Fast path: read-only search.
        {
            let root = self.root.read();
            if let Some(ref tree) = *root {
                let leaf = walk_to_leaf(tree, digest);
                if leaf.digest == *digest {
                    leaf.inc_ref();
                    return (Arc::clone(leaf), Some(new_oh));
                }
            }
        }

        // Slow path: acquire write lock and insert.
        let mut root = self.root.write();

        match *root {
            None => {
                // Empty tree -- insert the first leaf.
                new_oh.inc_ref();
                let oh = Arc::clone(&new_oh);
                *root = Some(CritbitNode::Leaf(new_oh));
                (oh, None)
            }
            Some(ref tree) => {
                // Re-check under write lock (another thread may have inserted).
                let leaf = walk_to_leaf(tree, digest);
                if leaf.digest == *digest {
                    leaf.inc_ref();
                    return (Arc::clone(leaf), Some(new_oh));
                }

                // Find the critical bit between the new digest and the closest existing leaf.
                let crit_bit = find_critical_bit(digest, &leaf.digest)
                    .expect("digests must differ if we reached here");

                // Insert the new leaf.
                new_oh.inc_ref();
                let oh = Arc::clone(&new_oh);
                let new_leaf = CritbitNode::Leaf(new_oh);
                let old_root = root.take().unwrap();
                *root = Some(insert_node(old_root, new_leaf, crit_bit, digest));
                (oh, None)
            }
        }
    }

    fn deref(&self, oh: &Arc<ObjHead>) -> bool {
        let new_count = oh.dec_ref();
        if new_count == 0 {
            let mut root = self.root.write();
            if let Some(tree) = root.take() {
                let (new_tree, _removed) = remove_node(tree, &oh.digest);
                *root = new_tree;
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

    fn make_digest_bytes(bytes: [u8; 32]) -> Digest {
        Digest::new(bytes)
    }

    #[test]
    fn test_get_bit() {
        let mut bytes = [0u8; 32];
        bytes[0] = 0b10110010;
        let d = Digest::new(bytes);

        assert_eq!(get_bit(&d, 0), 1); // MSB of byte 0
        assert_eq!(get_bit(&d, 1), 0);
        assert_eq!(get_bit(&d, 2), 1);
        assert_eq!(get_bit(&d, 3), 1);
        assert_eq!(get_bit(&d, 4), 0);
        assert_eq!(get_bit(&d, 5), 0);
        assert_eq!(get_bit(&d, 6), 1);
        assert_eq!(get_bit(&d, 7), 0); // LSB of byte 0
    }

    #[test]
    fn test_find_critical_bit() {
        let a = make_digest(0b10000000);
        let b = make_digest(0b11000000);
        // Bit 0: a=1, b=1 (same)
        // Bit 1: a=0, b=1 (differ)
        assert_eq!(find_critical_bit(&a, &b), Some(1));

        let c = make_digest(0b10000000);
        let d = make_digest(0b00000000);
        // Bit 0: c=1, d=0 (differ)
        assert_eq!(find_critical_bit(&c, &d), Some(0));

        // Identical digests.
        assert_eq!(find_critical_bit(&a, &a), None);
    }

    #[test]
    fn test_insert_and_lookup() {
        let hash = CritbitHash::new();
        hash.start();

        let d1 = make_digest(1);
        let oh1 = Arc::new(ObjHead::new(d1));
        let (found, unused) = hash.lookup(&d1, oh1);
        assert!(unused.is_none());
        assert_eq!(found.ref_count(), 1);
        assert_eq!(found.digest, d1);
    }

    #[test]
    fn test_lookup_existing_returns_same() {
        let hash = CritbitHash::new();
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
        let hash = CritbitHash::new();
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
        let hash = CritbitHash::new();
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
    fn test_many_inserts_and_lookups() {
        let hash = CritbitHash::new();
        hash.start();

        let mut heads = Vec::new();
        for i in 0u8..64 {
            let d = make_digest(i);
            let oh = Arc::new(ObjHead::new(d));
            let (found, unused) = hash.lookup(&d, oh);
            assert!(unused.is_none());
            heads.push(found);
        }

        // Verify all are retrievable.
        for i in 0u8..64 {
            let d = make_digest(i);
            let oh = Arc::new(ObjHead::new(d));
            let (found, unused) = hash.lookup(&d, oh);
            assert!(unused.is_some());
            assert!(Arc::ptr_eq(&found, &heads[i as usize]));
        }
    }

    #[test]
    fn test_remove_middle_entry() {
        let hash = CritbitHash::new();
        hash.start();

        let d1 = make_digest(1);
        let d2 = make_digest(2);
        let d3 = make_digest(3);

        let (oh1, _) = hash.lookup(&d1, Arc::new(ObjHead::new(d1)));
        let (oh2, _) = hash.lookup(&d2, Arc::new(ObjHead::new(d2)));
        let (oh3, _) = hash.lookup(&d3, Arc::new(ObjHead::new(d3)));

        // Remove the middle one.
        let removed = hash.deref(&oh2);
        assert!(removed);

        // The other two should still be findable.
        let (found1, unused1) = hash.lookup(&d1, Arc::new(ObjHead::new(d1)));
        assert!(unused1.is_some());
        assert!(Arc::ptr_eq(&found1, &oh1));

        let (found3, unused3) = hash.lookup(&d3, Arc::new(ObjHead::new(d3)));
        assert!(unused3.is_some());
        assert!(Arc::ptr_eq(&found3, &oh3));

        // And d2 should be re-insertable.
        let (fresh2, unused2) = hash.lookup(&d2, Arc::new(ObjHead::new(d2)));
        assert!(unused2.is_none());
        assert!(!Arc::ptr_eq(&fresh2, &oh2));
    }

    #[test]
    fn test_bit_diverse_digests() {
        // Test with digests that differ in various byte positions,
        // exercising the crit-bit selection logic more thoroughly.
        let hash = CritbitHash::new();
        hash.start();

        let mut digests = Vec::new();
        for i in 0u8..16 {
            let mut bytes = [0u8; 32];
            // Spread differences across multiple bytes.
            bytes[0] = i;
            bytes[1] = i.wrapping_mul(37);
            bytes[15] = i.wrapping_mul(113);
            bytes[31] = i.wrapping_mul(7);
            let d = make_digest_bytes(bytes);
            digests.push(d);

            let oh = Arc::new(ObjHead::new(d));
            let (_, unused) = hash.lookup(&d, oh);
            assert!(unused.is_none());
        }

        // All should be retrievable.
        for d in &digests {
            let oh = Arc::new(ObjHead::new(*d));
            let (_, unused) = hash.lookup(d, oh);
            assert!(unused.is_some());
        }
    }

    #[test]
    fn test_concurrent_inserts() {
        use std::thread;

        let hash = Arc::new(CritbitHash::new());
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

        let insert_count = results
            .iter()
            .filter(|(_, unused)| unused.is_none())
            .count();
        assert_eq!(insert_count, 1);
        assert_eq!(first.ref_count(), 8);
    }

    #[test]
    fn test_concurrent_different_digests() {
        use std::thread;

        let hash = Arc::new(CritbitHash::new());
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

        for i in 0..results.len() {
            for j in (i + 1)..results.len() {
                assert!(!Arc::ptr_eq(&results[i], &results[j]));
            }
        }
    }

    #[test]
    fn test_insert_remove_reinsert_cycle() {
        let hash = CritbitHash::new();
        hash.start();

        for _cycle in 0..3 {
            let d = make_digest(99);
            let oh = Arc::new(ObjHead::new(d));
            let (found, unused) = hash.lookup(&d, oh);
            assert!(unused.is_none());
            assert_eq!(found.ref_count(), 1);

            let removed = hash.deref(&found);
            assert!(removed);
        }
    }
}

//! Object core -- the central metadata structure for every cached object.
//!
//! `ObjCore` holds the digest (cache key), reference count, expiry parameters,
//! flags, and the inner body/attribute storage behind a `Mutex`. The struct is
//! designed for concurrent access: the atomic fields can be read without
//! holding the lock, while body and attribute mutations go through the
//! `parking_lot::Mutex`-protected inner state.

use std::collections::HashMap;
use std::sync::atomic::{AtomicI32, AtomicU8, Ordering};

use parking_lot::Mutex;
use rv_types::{Digest, ObjAttr, ObjCoreFlags, ObjExpFlags, ObjFlags, VtimDur, VtimReal};

/// Inner mutable state protected by a mutex.
struct ObjCoreInner {
    /// The object body bytes.
    body: Vec<u8>,
    /// Attribute key-value storage.
    attrs: HashMap<ObjAttr, Vec<u8>>,
    /// Per-object flags (gzipped, ESI-processed, etc.).
    obj_flags: ObjFlags,
}

/// Core cached object metadata.
///
/// Every object in the cache is represented by an `ObjCore`. It is referenced
/// from the hash lookup table, the LRU list, and the expiry heap. The design
/// mirrors `struct objcore` in Varnish.
pub struct ObjCore {
    /// Reference count (atomic for lock-free reads).
    pub refcnt: AtomicI32,
    /// The SHA-256 digest used as the cache key.
    pub digest: Digest,
    /// Object core flags (busy, dying, etc.) stored atomically.
    pub flags: AtomicU8,
    /// Expiry-related flags stored atomically.
    pub exp_flags: AtomicU8,
    /// Absolute timestamp of when the backend produced this response.
    pub t_origin: VtimReal,
    /// Time-to-live duration.
    pub ttl: VtimDur,
    /// Grace period duration.
    pub grace: VtimDur,
    /// Keep period duration.
    pub keep: VtimDur,
    /// Number of cache hits served from this object.
    pub hits: i64,
    /// Absolute timestamp at which the expiry timer should fire.
    pub timer_when: VtimReal,
    /// Timestamp of the last LRU list access.
    pub last_lru: VtimReal,
    /// Index into the expiry timer binary heap.
    pub timer_idx: u32,
    /// Mutex-protected inner state (body, attrs, obj_flags).
    inner: Mutex<ObjCoreInner>,
}

impl ObjCore {
    /// Creates a new `ObjCore` with the given digest and default values.
    pub fn new(digest: Digest) -> Self {
        Self {
            refcnt: AtomicI32::new(1),
            digest,
            flags: AtomicU8::new(0),
            exp_flags: AtomicU8::new(0),
            t_origin: VtimReal::default(),
            ttl: VtimDur::ZERO,
            grace: VtimDur::ZERO,
            keep: VtimDur::ZERO,
            hits: 0,
            timer_when: VtimReal::default(),
            last_lru: VtimReal::default(),
            timer_idx: 0,
            inner: Mutex::new(ObjCoreInner {
                body: Vec::new(),
                attrs: HashMap::new(),
                obj_flags: ObjFlags::empty(),
            }),
        }
    }

    // ---------------------------------------------------------------
    // Flag helpers
    // ---------------------------------------------------------------

    /// Returns `true` if the object is currently marked busy.
    pub fn is_busy(&self) -> bool {
        let raw = ObjCoreFlags::from_bits(self.flags.load(Ordering::Acquire));
        raw.contains(ObjCoreFlags::BUSY)
    }

    /// Returns `true` if the object has expired as of `now`.
    ///
    /// An object is expired when its origin time plus TTL is in the past.
    pub fn is_expired(&self, now: VtimReal) -> bool {
        let deadline = self.t_origin + self.ttl;
        now.0 > deadline.0
    }

    /// Returns the remaining TTL relative to `now`, or zero if already expired.
    pub fn remaining_ttl(&self, now: VtimReal) -> VtimDur {
        let deadline = self.t_origin + self.ttl;
        let remaining = deadline.0 - now.0;
        if remaining > 0.0 {
            VtimDur::from_secs(remaining)
        } else {
            VtimDur::ZERO
        }
    }

    /// Returns the remaining grace period relative to `now`, or zero if gone.
    pub fn remaining_grace(&self, now: VtimReal) -> VtimDur {
        let deadline = self.t_origin + self.ttl + self.grace;
        let remaining = deadline.0 - now.0;
        if remaining > 0.0 {
            VtimDur::from_secs(remaining)
        } else {
            VtimDur::ZERO
        }
    }

    // ---------------------------------------------------------------
    // Reference counting
    // ---------------------------------------------------------------

    /// Returns the current reference count.
    pub fn ref_count(&self) -> i32 {
        self.refcnt.load(Ordering::Acquire)
    }

    /// Increments the reference count by one.
    pub fn inc_ref(&self) {
        self.refcnt.fetch_add(1, Ordering::AcqRel);
    }

    /// Decrements the reference count by one, returning the new value.
    pub fn dec_ref(&self) -> i32 {
        self.refcnt.fetch_sub(1, Ordering::AcqRel) - 1
    }

    // ---------------------------------------------------------------
    // Body access
    // ---------------------------------------------------------------

    /// Stores (replaces) the object body.
    pub fn store_body(&self, data: &[u8]) {
        let mut inner = self.inner.lock();
        inner.body = data.to_vec();
    }

    /// Appends data to the existing body.
    pub fn append_body(&self, data: &[u8]) {
        let mut inner = self.inner.lock();
        inner.body.extend_from_slice(data);
    }

    /// Returns a clone of the object body, or `None` if empty.
    pub fn get_body(&self) -> Option<Vec<u8>> {
        let inner = self.inner.lock();
        if inner.body.is_empty() {
            None
        } else {
            Some(inner.body.clone())
        }
    }

    /// Returns the current body length in bytes.
    pub fn body_len(&self) -> usize {
        let inner = self.inner.lock();
        inner.body.len()
    }

    /// Clears the body, releasing memory.
    pub fn clear_body(&self) {
        let mut inner = self.inner.lock();
        inner.body = Vec::new();
    }

    // ---------------------------------------------------------------
    // Attribute access
    // ---------------------------------------------------------------

    /// Sets an attribute value, replacing any previous value.
    pub fn set_attr(&self, attr: ObjAttr, data: &[u8]) {
        let mut inner = self.inner.lock();
        inner.attrs.insert(attr, data.to_vec());
    }

    /// Returns a clone of the attribute value, or `None` if not set.
    pub fn get_attr(&self, attr: ObjAttr) -> Option<Vec<u8>> {
        let inner = self.inner.lock();
        inner.attrs.get(&attr).cloned()
    }

    /// Removes an attribute, returning its previous value if it existed.
    pub fn remove_attr(&self, attr: ObjAttr) -> Option<Vec<u8>> {
        let mut inner = self.inner.lock();
        inner.attrs.remove(&attr)
    }

    // ---------------------------------------------------------------
    // ObjFlags access
    // ---------------------------------------------------------------

    /// Returns the current `ObjFlags`.
    pub fn obj_flags(&self) -> ObjFlags {
        let inner = self.inner.lock();
        inner.obj_flags
    }

    /// Sets the `ObjFlags` to the given value.
    pub fn set_obj_flags(&self, flags: ObjFlags) {
        let mut inner = self.inner.lock();
        inner.obj_flags = flags;
    }

    // ---------------------------------------------------------------
    // ObjCoreFlags helpers
    // ---------------------------------------------------------------

    /// Returns the current `ObjCoreFlags`.
    pub fn oc_flags(&self) -> ObjCoreFlags {
        ObjCoreFlags::from_bits(self.flags.load(Ordering::Acquire))
    }

    /// Atomically sets a flag bit in the object core flags.
    pub fn set_oc_flag(&self, flag: ObjCoreFlags) {
        self.flags.fetch_or(flag.bits(), Ordering::AcqRel);
    }

    /// Atomically clears a flag bit in the object core flags.
    pub fn clear_oc_flag(&self, flag: ObjCoreFlags) {
        self.flags.fetch_and(!flag.bits(), Ordering::AcqRel);
    }

    // ---------------------------------------------------------------
    // ObjExpFlags helpers
    // ---------------------------------------------------------------

    /// Returns the current `ObjExpFlags`.
    pub fn oe_flags(&self) -> ObjExpFlags {
        ObjExpFlags::from_bits(self.exp_flags.load(Ordering::Acquire))
    }

    /// Atomically sets a flag bit in the expiry flags.
    pub fn set_oe_flag(&self, flag: ObjExpFlags) {
        self.exp_flags.fetch_or(flag.bits(), Ordering::AcqRel);
    }

    /// Atomically clears a flag bit in the expiry flags.
    pub fn clear_oe_flag(&self, flag: ObjExpFlags) {
        self.exp_flags.fetch_and(!flag.bits(), Ordering::AcqRel);
    }
}

impl std::fmt::Debug for ObjCore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ObjCore")
            .field("digest", &self.digest)
            .field("refcnt", &self.refcnt.load(Ordering::Relaxed))
            .field("flags", &self.flags.load(Ordering::Relaxed))
            .field("ttl", &self.ttl)
            .field("grace", &self.grace)
            .field("hits", &self.hits)
            .finish()
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
    fn test_new_objcore_defaults() {
        let oc = ObjCore::new(test_digest(1));
        assert_eq!(oc.ref_count(), 1);
        assert!(!oc.is_busy());
        assert!(oc.get_body().is_none());
        assert_eq!(oc.body_len(), 0);
    }

    #[test]
    fn test_ref_counting() {
        let oc = ObjCore::new(test_digest(2));
        assert_eq!(oc.ref_count(), 1);
        oc.inc_ref();
        assert_eq!(oc.ref_count(), 2);
        let new_val = oc.dec_ref();
        assert_eq!(new_val, 1);
        assert_eq!(oc.ref_count(), 1);
    }

    #[test]
    fn test_body_store_and_retrieve() {
        let oc = ObjCore::new(test_digest(3));
        oc.store_body(b"hello world");
        assert_eq!(oc.body_len(), 11);
        assert_eq!(oc.get_body().unwrap(), b"hello world");
    }

    #[test]
    fn test_body_append() {
        let oc = ObjCore::new(test_digest(4));
        oc.store_body(b"hello ");
        oc.append_body(b"world");
        assert_eq!(oc.get_body().unwrap(), b"hello world");
    }

    #[test]
    fn test_attrs() {
        let oc = ObjCore::new(test_digest(5));
        assert!(oc.get_attr(ObjAttr::Len).is_none());
        oc.set_attr(ObjAttr::Len, &42u64.to_le_bytes());
        let val = oc.get_attr(ObjAttr::Len).unwrap();
        assert_eq!(u64::from_le_bytes(val.try_into().unwrap()), 42);
    }

    #[test]
    fn test_expiry() {
        let mut oc = ObjCore::new(test_digest(6));
        oc.t_origin = VtimReal::from_secs(1000.0);
        oc.ttl = VtimDur::from_secs(60.0);
        oc.grace = VtimDur::from_secs(30.0);

        let before = VtimReal::from_secs(1050.0);
        assert!(!oc.is_expired(before));
        assert!(oc.remaining_ttl(before).as_secs() > 9.0);

        let after = VtimReal::from_secs(1070.0);
        assert!(oc.is_expired(after));
        assert_eq!(oc.remaining_ttl(after), VtimDur::ZERO);

        // Grace still active at 1070 (deadline = 1000 + 60 + 30 = 1090)
        assert!(oc.remaining_grace(after).as_secs() > 0.0);

        let past_grace = VtimReal::from_secs(1100.0);
        assert_eq!(oc.remaining_grace(past_grace), VtimDur::ZERO);
    }

    #[test]
    fn test_oc_flags() {
        let oc = ObjCore::new(test_digest(7));
        assert!(!oc.is_busy());
        oc.set_oc_flag(ObjCoreFlags::BUSY);
        assert!(oc.is_busy());
        oc.clear_oc_flag(ObjCoreFlags::BUSY);
        assert!(!oc.is_busy());
    }
}

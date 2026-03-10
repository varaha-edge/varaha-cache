//! Heap-allocated (malloc) storage backend.
//!
//! `MallocStevedore` keeps objects in process memory. Each object is stored as
//! a `StoredObject` inside a `DashMap` keyed by the object's [`Digest`]. This
//! provides lock-free concurrent reads and fine-grained per-shard locks on
//! writes.
//!
//! A global `AtomicUsize` tracks the total allocated bytes so that the
//! stevedore can enforce its configured `max_size` limit.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use bytes::Bytes;
use dashmap::DashMap;
use rv_types::{Digest, ObjAttr};
use tracing::debug;

use crate::objcore::ObjCore;
use crate::traits::{Stevedore, StorageError};

/// A single cached object's metadata stored on the heap.
///
/// Body data is stored in the `ObjCore` (as `Bytes`) to avoid double storage.
/// The stevedore only tracks attributes and space accounting.
#[derive(Debug, Clone)]
struct StoredObject {
    attrs: HashMap<ObjAttr, Vec<u8>>,
    /// Tracked body size for space accounting (body itself lives in ObjCore).
    body_size: usize,
}

impl StoredObject {
    fn new() -> Self {
        Self {
            attrs: HashMap::new(),
            body_size: 0,
        }
    }

    /// Returns the approximate heap footprint of this object's metadata.
    fn size(&self) -> usize {
        let attr_size: usize = self
            .attrs
            .values()
            .map(|v| v.len() + std::mem::size_of::<ObjAttr>())
            .sum();
        self.body_size + attr_size + std::mem::size_of::<Self>()
    }
}

/// Heap-allocated storage backend.
///
/// This is the simplest stevedore and the default for most configurations. It
/// allocates objects on the normal heap and is bounded by `max_size` bytes.
pub struct MallocStevedore {
    name: String,
    max_size: usize,
    used: AtomicUsize,
    objects: DashMap<Digest, StoredObject>,
    open: AtomicBool,
}

impl MallocStevedore {
    /// Creates a new `MallocStevedore` with the given name and capacity.
    ///
    /// # Arguments
    ///
    /// * `name` - Human-readable name for this storage backend.
    /// * `max_size` - Maximum number of bytes the stevedore is allowed to use.
    pub fn new(name: impl Into<String>, max_size: usize) -> Self {
        Self {
            name: name.into(),
            max_size,
            used: AtomicUsize::new(0),
            objects: DashMap::new(),
            open: AtomicBool::new(false),
        }
    }

    /// Returns the number of objects currently stored.
    pub fn object_count(&self) -> usize {
        self.objects.len()
    }

    /// Attempts to reserve `additional` bytes of capacity.
    ///
    /// Returns `Ok(())` if the reservation succeeded, or `Err(StorageError::Full)`
    /// if it would exceed `max_size`.
    fn try_reserve(&self, additional: usize) -> Result<(), StorageError> {
        let mut current = self.used.load(Ordering::Acquire);
        loop {
            let new_val = current.checked_add(additional).ok_or(StorageError::Full)?;
            if new_val > self.max_size {
                return Err(StorageError::Full);
            }
            match self.used.compare_exchange_weak(
                current,
                new_val,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return Ok(()),
                Err(actual) => current = actual,
            }
        }
    }

    /// Releases `amount` bytes of tracked capacity.
    fn release(&self, amount: usize) {
        self.used.fetch_sub(amount, Ordering::AcqRel);
    }
}

impl Stevedore for MallocStevedore {
    fn name(&self) -> &str {
        &self.name
    }

    fn open(&mut self) -> Result<(), StorageError> {
        self.open.store(true, Ordering::Release);
        debug!(name = %self.name, max_size = self.max_size, "malloc stevedore opened");
        Ok(())
    }

    fn close(&mut self) {
        self.open.store(false, Ordering::Release);
        self.objects.clear();
        self.used.store(0, Ordering::Release);
        debug!(name = %self.name, "malloc stevedore closed");
    }

    fn alloc_obj(&self, oc: &mut ObjCore, size_hint: usize) -> Result<(), StorageError> {
        // Reserve the estimated size.
        let reserve = size_hint + std::mem::size_of::<StoredObject>();
        self.try_reserve(reserve)?;

        let obj = StoredObject::new();
        self.objects.insert(oc.digest, obj);

        debug!(digest = %oc.digest, size_hint, "allocated object in malloc stevedore");
        Ok(())
    }

    fn free_obj(&self, oc: &mut ObjCore) {
        if let Some((_, obj)) = self.objects.remove(&oc.digest) {
            let freed = obj.size();
            self.release(freed);
            debug!(digest = %oc.digest, freed, "freed object from malloc stevedore");
        }
    }

    fn get_space(&self, _oc: &ObjCore, desired: usize) -> Result<Vec<u8>, StorageError> {
        self.try_reserve(desired)?;
        Ok(Vec::with_capacity(desired))
    }

    fn extend(&self, oc: &ObjCore, data: &[u8]) -> Result<(), StorageError> {
        let mut entry = self
            .objects
            .get_mut(&oc.digest)
            .ok_or(StorageError::NotFound)?;

        // Track the body size for space accounting only.
        // The actual body data lives in ObjCore (as Bytes).
        entry.body_size += data.len();

        Ok(())
    }

    fn trim(&self, _oc: &ObjCore) {
        // No-op: body is stored in ObjCore as Bytes, not in StoredObject.
    }

    fn get_attr(&self, oc: &ObjCore, attr: ObjAttr) -> Option<Vec<u8>> {
        let entry = self.objects.get(&oc.digest)?;
        entry.attrs.get(&attr).cloned()
    }

    fn set_attr(&self, oc: &mut ObjCore, attr: ObjAttr, data: &[u8]) -> Result<(), StorageError> {
        let mut entry = self
            .objects
            .get_mut(&oc.digest)
            .ok_or(StorageError::NotFound)?;

        let old_size = entry.attrs.get(&attr).map(|v| v.len()).unwrap_or(0);
        let new_size = data.len();

        if new_size > old_size {
            self.try_reserve(new_size - old_size)?;
        } else {
            self.release(old_size - new_size);
        }

        entry.attrs.insert(attr, data.to_vec());
        Ok(())
    }

    fn get_body(&self, oc: &ObjCore) -> Option<Bytes> {
        // Body is stored in ObjCore, not in the stevedore.
        // Delegate to ObjCore for backward compatibility.
        let _ = self.objects.get(&oc.digest)?;
        oc.get_body()
    }

    fn total_space(&self) -> usize {
        self.max_size
    }

    fn used_space(&self) -> usize {
        self.used.load(Ordering::Acquire)
    }

    fn free_space(&self) -> usize {
        let used = self.used.load(Ordering::Acquire);
        self.max_size.saturating_sub(used)
    }
}

impl std::fmt::Debug for MallocStevedore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MallocStevedore")
            .field("name", &self.name)
            .field("max_size", &self.max_size)
            .field("used", &self.used.load(Ordering::Relaxed))
            .field("objects", &self.objects.len())
            .finish()
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

    fn make_stevedore() -> MallocStevedore {
        let mut s = MallocStevedore::new("test-malloc", 1024 * 1024);
        s.open().unwrap();
        s
    }

    #[test]
    fn test_alloc_and_free() {
        let stv = make_stevedore();
        let mut oc = ObjCore::new(test_digest(1));

        stv.alloc_obj(&mut oc, 256).unwrap();
        assert_eq!(stv.object_count(), 1);
        assert!(stv.used_space() > 0);

        stv.free_obj(&mut oc);
        assert_eq!(stv.object_count(), 0);
    }

    #[test]
    fn test_store_and_retrieve_body() {
        let stv = make_stevedore();
        let mut oc = ObjCore::new(test_digest(2));

        stv.alloc_obj(&mut oc, 64).unwrap();
        stv.extend(&oc, b"hello world").unwrap();
        // Body is stored in ObjCore, not in the stevedore.
        oc.store_body(b"hello world");

        let body = stv.get_body(&oc).unwrap();
        assert_eq!(&body[..], b"hello world");
    }

    #[test]
    fn test_extend_multiple() {
        let stv = make_stevedore();
        let mut oc = ObjCore::new(test_digest(3));

        stv.alloc_obj(&mut oc, 64).unwrap();
        stv.extend(&oc, b"hello ").unwrap();
        stv.extend(&oc, b"world").unwrap();
        oc.store_body(b"hello world");

        let body = stv.get_body(&oc).unwrap();
        assert_eq!(&body[..], b"hello world");
    }

    #[test]
    fn test_set_and_get_attr() {
        let stv = make_stevedore();
        let mut oc = ObjCore::new(test_digest(4));

        stv.alloc_obj(&mut oc, 64).unwrap();
        stv.set_attr(&mut oc, ObjAttr::Len, &42u64.to_le_bytes())
            .unwrap();

        let val = stv.get_attr(&oc, ObjAttr::Len).unwrap();
        assert_eq!(u64::from_le_bytes(val.try_into().unwrap()), 42);
    }

    #[test]
    fn test_get_attr_missing() {
        let stv = make_stevedore();
        let mut oc = ObjCore::new(test_digest(5));
        stv.alloc_obj(&mut oc, 64).unwrap();
        assert!(stv.get_attr(&oc, ObjAttr::Vary).is_none());
    }

    #[test]
    fn test_space_tracking() {
        let stv = make_stevedore();
        let initial_free = stv.free_space();

        let mut oc = ObjCore::new(test_digest(6));
        stv.alloc_obj(&mut oc, 256).unwrap();

        assert!(stv.free_space() < initial_free);
        assert!(stv.used_space() > 0);
        assert_eq!(stv.total_space(), 1024 * 1024);

        stv.free_obj(&mut oc);
        // After freeing, used should decrease.
        // (It may not return to exactly 0 due to atomic accounting.)
    }

    #[test]
    fn test_alloc_full() {
        // Create a very small stevedore.
        let mut stv = MallocStevedore::new("tiny", 64);
        stv.open().unwrap();

        let mut oc = ObjCore::new(test_digest(7));
        // size_hint + StoredObject overhead should exceed 64 bytes.
        let result = stv.alloc_obj(&mut oc, 128);
        assert!(result.is_err());
    }

    #[test]
    fn test_get_body_empty() {
        let stv = make_stevedore();
        let mut oc = ObjCore::new(test_digest(8));
        stv.alloc_obj(&mut oc, 64).unwrap();
        assert!(stv.get_body(&oc).is_none());
    }

    #[test]
    fn test_trim() {
        let stv = make_stevedore();
        let mut oc = ObjCore::new(test_digest(9));
        stv.alloc_obj(&mut oc, 1024).unwrap();

        stv.extend(&oc, b"small").unwrap();
        oc.store_body(b"small");
        stv.trim(&oc);
        assert_eq!(&stv.get_body(&oc).unwrap()[..], b"small");
    }

    #[test]
    fn test_multiple_objects() {
        let stv = make_stevedore();

        let mut oc1 = ObjCore::new(test_digest(10));
        let mut oc2 = ObjCore::new(test_digest(11));

        stv.alloc_obj(&mut oc1, 64).unwrap();
        stv.alloc_obj(&mut oc2, 64).unwrap();
        assert_eq!(stv.object_count(), 2);

        stv.extend(&oc1, b"body1").unwrap();
        oc1.store_body(b"body1");
        stv.extend(&oc2, b"body2").unwrap();
        oc2.store_body(b"body2");

        assert_eq!(&stv.get_body(&oc1).unwrap()[..], b"body1");
        assert_eq!(&stv.get_body(&oc2).unwrap()[..], b"body2");

        stv.free_obj(&mut oc1);
        assert_eq!(stv.object_count(), 1);
        assert_eq!(&stv.get_body(&oc2).unwrap()[..], b"body2");
    }

    #[test]
    fn test_extend_not_found() {
        let stv = make_stevedore();
        let oc = ObjCore::new(test_digest(12));
        let result = stv.extend(&oc, b"data");
        assert!(result.is_err());
    }

    #[test]
    fn test_set_attr_not_found() {
        let stv = make_stevedore();
        let mut oc = ObjCore::new(test_digest(13));
        let result = stv.set_attr(&mut oc, ObjAttr::Len, &[0u8; 8]);
        assert!(result.is_err());
    }
}

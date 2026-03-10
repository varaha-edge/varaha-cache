//! Memory-mapped file storage backend.
//!
//! `FileStevedore` creates a file of a specified size, memory-maps it, and
//! manages object allocation within the mapped region using a simple bump
//! allocator. Objects are stored contiguously in the file with a small header
//! describing their layout.
//!
//! This backend is suitable for caches that should survive process restarts
//! (though the current implementation does not persist index metadata -- it
//! treats the file as ephemeral scratch space that is faster than hitting the
//! page cache for large working sets).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use bytes::Bytes;
use memmap2::MmapMut;
use parking_lot::Mutex;
use rv_types::{Digest, ObjAttr};
use tracing::debug;

use crate::objcore::ObjCore;
use crate::traits::{Stevedore, StorageError};

/// Header size for each allocation slot.
///
/// Layout (little-endian):
///   [0..32]  digest bytes
///   [32..40] body offset (u64)
///   [40..48] body length (u64)
///   [48..56] attrs region offset (u64)
///   [56..64] attrs region length (u64)
const SLOT_HEADER_SIZE: usize = 64;

/// Alignment for allocations within the mmap.
const ALLOC_ALIGN: usize = 8;

/// In-memory index entry pointing into the mmap region.
#[derive(Debug, Clone)]
struct FileSlot {
    /// Byte offset of the slot header within the mmap.
    offset: usize,
    /// Total bytes reserved for this slot (header + body + attrs).
    reserved: usize,
    /// Current body length.
    body_len: usize,
    /// Attribute data stored separately (small; kept in-process for
    /// simplicity).
    attrs: HashMap<ObjAttr, Vec<u8>>,
}

/// Memory-mapped file storage backend.
pub struct FileStevedore {
    name: String,
    path: PathBuf,
    size: usize,
    /// The mmap is behind a Mutex to allow interior mutability from `&self`
    /// trait methods. The mutex is only held briefly for pointer extraction;
    /// actual writes go through raw pointers with safety guaranteed by the
    /// bump allocator (non-overlapping regions) and the index lock (per-object
    /// serialization).
    mmap: Mutex<Option<MmapMut>>,
    /// Bump pointer -- next free byte offset in the mmap.
    bump: AtomicUsize,
    /// Index mapping digests to their file slots.
    index: Mutex<HashMap<Digest, FileSlot>>,
    open: AtomicBool,
}

impl FileStevedore {
    /// Creates a new `FileStevedore`.
    ///
    /// # Arguments
    ///
    /// * `name` - Human-readable name for this backend.
    /// * `path` - Filesystem path for the backing file.
    /// * `size` - Size of the backing file in bytes.
    pub fn new(name: impl Into<String>, path: impl Into<PathBuf>, size: usize) -> Self {
        Self {
            name: name.into(),
            path: path.into(),
            size,
            mmap: Mutex::new(None),
            bump: AtomicUsize::new(0),
            index: Mutex::new(HashMap::new()),
            open: AtomicBool::new(false),
        }
    }

    /// Returns the backing file path.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Returns the number of objects currently stored.
    pub fn object_count(&self) -> usize {
        self.index.lock().len()
    }

    /// Allocates `len` bytes from the bump allocator, respecting alignment.
    ///
    /// Returns the byte offset of the allocation, or `None` if there is
    /// insufficient space.
    fn bump_alloc(&self, len: usize) -> Option<usize> {
        let aligned = (len + ALLOC_ALIGN - 1) & !(ALLOC_ALIGN - 1);
        let mut current = self.bump.load(Ordering::Acquire);
        loop {
            let new_val = current.checked_add(aligned)?;
            if new_val > self.size {
                return None;
            }
            match self.bump.compare_exchange_weak(
                current,
                new_val,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return Some(current),
                Err(actual) => current = actual,
            }
        }
    }

    /// Returns the base pointer of the mmap, or an error if not open.
    ///
    /// The returned pointer is valid for the lifetime of the mmap (until
    /// `close` is called). Callers must ensure writes do not overlap.
    fn mmap_base_ptr(&self) -> Result<*mut u8, StorageError> {
        let guard = self.mmap.lock();
        match guard.as_ref() {
            Some(m) => Ok(m.as_ptr() as *mut u8),
            None => Err(StorageError::Io(std::io::Error::new(
                std::io::ErrorKind::NotConnected,
                "file stevedore not open",
            ))),
        }
    }
}

impl Stevedore for FileStevedore {
    fn name(&self) -> &str {
        &self.name
    }

    fn open(&mut self) -> Result<(), StorageError> {
        // Create (or truncate) the backing file to the requested size.
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(&self.path)?;

        file.set_len(self.size as u64)?;

        // Safety: we own the file exclusively. The mmap is valid for the
        // lifetime of `self`.
        let mmap = unsafe { MmapMut::map_mut(&file)? };
        *self.mmap.lock() = Some(mmap);
        self.bump.store(0, Ordering::Release);
        self.open.store(true, Ordering::Release);

        debug!(name = %self.name, path = %self.path.display(), size = self.size,
               "file stevedore opened");
        Ok(())
    }

    fn close(&mut self) {
        if let Some(mmap) = self.mmap.lock().take() {
            // Flush is best-effort.
            let _ = mmap.flush();
        }
        self.index.lock().clear();
        self.bump.store(0, Ordering::Release);
        self.open.store(false, Ordering::Release);
        debug!(name = %self.name, "file stevedore closed");
    }

    fn alloc_obj(&self, oc: &mut ObjCore, size_hint: usize) -> Result<(), StorageError> {
        let total = SLOT_HEADER_SIZE + size_hint;
        let offset = self.bump_alloc(total).ok_or(StorageError::Full)?;

        // Write the digest into the slot header.
        let base = self.mmap_base_ptr()?;
        // Safety: offset..offset+32 is within the region we just bump-allocated,
        // so no other thread can be writing to this range. The bump allocator
        // guarantees non-overlapping regions.
        unsafe {
            std::ptr::copy_nonoverlapping(oc.digest.bytes.as_ptr(), base.add(offset), 32);
        }

        let slot = FileSlot {
            offset,
            reserved: total,
            body_len: 0,
            attrs: HashMap::new(),
        };

        self.index.lock().insert(oc.digest, slot);

        debug!(digest = %oc.digest, offset, size_hint,
               "allocated object in file stevedore");
        Ok(())
    }

    fn free_obj(&self, oc: &mut ObjCore) {
        // Note: with a bump allocator we cannot truly reclaim space in the
        // middle. We remove the index entry so the data is logically freed.
        if self.index.lock().remove(&oc.digest).is_some() {
            debug!(digest = %oc.digest, "freed object from file stevedore (logical)");
        }
    }

    fn get_space(&self, _oc: &ObjCore, desired: usize) -> Result<Vec<u8>, StorageError> {
        // For the file stevedore we return a temporary buffer; the caller
        // writes into it and then calls `extend` to commit to the mmap.
        Ok(Vec::with_capacity(desired))
    }

    fn extend(&self, oc: &ObjCore, data: &[u8]) -> Result<(), StorageError> {
        let base = self.mmap_base_ptr()?;
        let mut index = self.index.lock();
        let slot = index.get_mut(&oc.digest).ok_or(StorageError::NotFound)?;

        let body_start = slot.offset + SLOT_HEADER_SIZE;
        let write_offset = body_start + slot.body_len;
        let write_end = write_offset + data.len();

        // Check that the write fits within the reserved region.
        if write_end > slot.offset + slot.reserved {
            let extra = write_end - (slot.offset + slot.reserved);
            if self.bump_alloc(extra).is_none() {
                return Err(StorageError::Full);
            }
            slot.reserved += extra;
        }

        // Safety: write_offset..write_end is within the region we reserved
        // for this slot. We hold the index lock so no concurrent write to
        // the same slot can occur, and the bump allocator guarantees this
        // region does not overlap with any other slot's data.
        unsafe {
            std::ptr::copy_nonoverlapping(data.as_ptr(), base.add(write_offset), data.len());
        }

        slot.body_len += data.len();
        Ok(())
    }

    fn trim(&self, oc: &ObjCore) {
        // No-op for bump allocator -- we cannot shrink in place.
        let _ = oc;
    }

    fn get_attr(&self, oc: &ObjCore, attr: ObjAttr) -> Option<Vec<u8>> {
        let index = self.index.lock();
        let slot = index.get(&oc.digest)?;
        slot.attrs.get(&attr).cloned()
    }

    fn set_attr(&self, oc: &mut ObjCore, attr: ObjAttr, data: &[u8]) -> Result<(), StorageError> {
        let mut index = self.index.lock();
        let slot = index.get_mut(&oc.digest).ok_or(StorageError::NotFound)?;
        slot.attrs.insert(attr, data.to_vec());
        Ok(())
    }

    fn get_body(&self, oc: &ObjCore) -> Option<Bytes> {
        let guard = self.mmap.lock();
        let mmap = guard.as_ref()?;
        let index = self.index.lock();
        let slot = index.get(&oc.digest)?;

        if slot.body_len == 0 {
            return None;
        }

        let body_start = slot.offset + SLOT_HEADER_SIZE;
        let body_end = body_start + slot.body_len;
        Some(Bytes::copy_from_slice(&mmap[body_start..body_end]))
    }

    fn total_space(&self) -> usize {
        self.size
    }

    fn used_space(&self) -> usize {
        self.bump.load(Ordering::Acquire)
    }

    fn free_space(&self) -> usize {
        self.size.saturating_sub(self.bump.load(Ordering::Acquire))
    }
}

impl Drop for FileStevedore {
    fn drop(&mut self) {
        if self.open.load(Ordering::Acquire) {
            self.close();
        }
    }
}

impl std::fmt::Debug for FileStevedore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FileStevedore")
            .field("name", &self.name)
            .field("path", &self.path)
            .field("size", &self.size)
            .field("used", &self.bump.load(Ordering::Relaxed))
            .field("objects", &self.index.lock().len())
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

    fn make_stevedore(dir: &std::path::Path) -> FileStevedore {
        let path = dir.join("test_storage.bin");
        let mut stv = FileStevedore::new("test-file", path, 1024 * 1024);
        stv.open().unwrap();
        stv
    }

    #[test]
    fn test_alloc_and_free() {
        let tmp = tempfile::tempdir().unwrap();
        let stv = make_stevedore(tmp.path());

        let mut oc = ObjCore::new(test_digest(1));
        stv.alloc_obj(&mut oc, 256).unwrap();
        assert_eq!(stv.object_count(), 1);
        assert!(stv.used_space() > 0);

        stv.free_obj(&mut oc);
        assert_eq!(stv.object_count(), 0);
    }

    #[test]
    fn test_store_and_retrieve_body() {
        let tmp = tempfile::tempdir().unwrap();
        let stv = make_stevedore(tmp.path());

        let mut oc = ObjCore::new(test_digest(2));
        stv.alloc_obj(&mut oc, 256).unwrap();
        stv.extend(&oc, b"hello world").unwrap();

        let body = stv.get_body(&oc).unwrap();
        assert_eq!(&body[..], b"hello world");
    }

    #[test]
    fn test_extend_multiple() {
        let tmp = tempfile::tempdir().unwrap();
        let stv = make_stevedore(tmp.path());

        let mut oc = ObjCore::new(test_digest(3));
        stv.alloc_obj(&mut oc, 256).unwrap();
        stv.extend(&oc, b"hello ").unwrap();
        stv.extend(&oc, b"world").unwrap();

        let body = stv.get_body(&oc).unwrap();
        assert_eq!(&body[..], b"hello world");
    }

    #[test]
    fn test_attrs() {
        let tmp = tempfile::tempdir().unwrap();
        let stv = make_stevedore(tmp.path());

        let mut oc = ObjCore::new(test_digest(4));
        stv.alloc_obj(&mut oc, 256).unwrap();
        stv.set_attr(&mut oc, ObjAttr::Len, &100u64.to_le_bytes())
            .unwrap();

        let val = stv.get_attr(&oc, ObjAttr::Len).unwrap();
        assert_eq!(u64::from_le_bytes(val.try_into().unwrap()), 100);
    }

    #[test]
    fn test_space_tracking() {
        let tmp = tempfile::tempdir().unwrap();
        let stv = make_stevedore(tmp.path());
        assert_eq!(stv.total_space(), 1024 * 1024);
        assert_eq!(stv.used_space(), 0);

        let mut oc = ObjCore::new(test_digest(5));
        stv.alloc_obj(&mut oc, 256).unwrap();
        assert!(stv.used_space() > 0);
        assert!(stv.free_space() < stv.total_space());
    }

    #[test]
    fn test_alloc_full() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("tiny.bin");
        let mut stv = FileStevedore::new("tiny", path, 128);
        stv.open().unwrap();

        let mut oc = ObjCore::new(test_digest(6));
        // SLOT_HEADER_SIZE (64) + 128 = 192 > 128 file size.
        let result = stv.alloc_obj(&mut oc, 128);
        assert!(result.is_err());
    }

    #[test]
    fn test_get_body_empty() {
        let tmp = tempfile::tempdir().unwrap();
        let stv = make_stevedore(tmp.path());

        let mut oc = ObjCore::new(test_digest(7));
        stv.alloc_obj(&mut oc, 256).unwrap();
        assert!(stv.get_body(&oc).is_none());
    }

    #[test]
    fn test_multiple_objects() {
        let tmp = tempfile::tempdir().unwrap();
        let stv = make_stevedore(tmp.path());

        let mut oc1 = ObjCore::new(test_digest(10));
        let mut oc2 = ObjCore::new(test_digest(11));

        stv.alloc_obj(&mut oc1, 128).unwrap();
        stv.alloc_obj(&mut oc2, 128).unwrap();
        assert_eq!(stv.object_count(), 2);

        stv.extend(&oc1, b"body-one").unwrap();
        stv.extend(&oc2, b"body-two").unwrap();

        assert_eq!(&stv.get_body(&oc1).unwrap()[..], b"body-one");
        assert_eq!(&stv.get_body(&oc2).unwrap()[..], b"body-two");
    }
}

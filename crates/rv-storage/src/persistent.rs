//! Persistent storage backend.
//!
//! `PersistentStevedore` maps a file into memory and manages object allocation
//! within it using a slab-based allocator. Unlike [`FileStevedore`], this
//! backend is designed to survive process restarts: a header region at the
//! start of the file records a magic number, version, and a slab table that
//! can be scanned on startup to recover objects that are still within their
//! TTL + grace window.
//!
//! The on-disk layout is:
//!
//! ```text
//!   [FILE_HEADER (128 bytes)]
//!   [SLAB_TABLE  (slab_count * SLAB_ENTRY_SIZE bytes)]
//!   [DATA_REGION ...]
//! ```

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use memmap2::MmapMut;
use parking_lot::Mutex;
use rv_types::{Digest, ObjAttr};
use tracing::debug;

use crate::objcore::ObjCore;
use crate::traits::{Stevedore, StorageError};

// ---------------------------------------------------------------------------
// On-disk constants
// ---------------------------------------------------------------------------

/// Magic bytes written at the start of the file to identify it as a
/// persistent stevedore file.
const MAGIC: &[u8; 8] = b"RVSLAB01";

/// Current format version.
const VERSION: u32 = 1;

/// Size of the file header region.
const FILE_HEADER_SIZE: usize = 128;

/// Size of each slab entry in the on-disk slab table.
///
/// Layout (all little-endian):
///   [0..32]   digest bytes
///   [32..40]  data offset in file (u64)
///   [40..48]  slab size (u64)
///   [48..49]  in_use flag (u8: 1 = in use, 0 = free)
///   [49..57]  body_len (u64)
///   [57..64]  reserved / padding
const SLAB_ENTRY_SIZE: usize = 64;

/// Maximum number of slabs we support in the table. This limits the slab
/// table to `MAX_SLABS * SLAB_ENTRY_SIZE` bytes.
const MAX_SLABS: usize = 65536;

/// The data region starts after the file header and the slab table.
const DATA_REGION_OFFSET: usize = FILE_HEADER_SIZE + MAX_SLABS * SLAB_ENTRY_SIZE;

/// Alignment for data allocations.
const DATA_ALIGN: usize = 8;

// ---------------------------------------------------------------------------
// In-memory types
// ---------------------------------------------------------------------------

/// Tracks a single slab allocation within the persistent file.
#[derive(Debug, Clone)]
struct SlabEntry {
    /// Index in the slab table (0-based).
    index: usize,
    /// Byte offset of the data region for this slab within the file.
    offset: usize,
    /// Total size of the slab data region (bytes).
    size: usize,
    /// Whether this slab is currently occupied.
    in_use: bool,
    /// Digest of the object stored in this slab (meaningful only when in_use).
    digest: Digest,
    /// Current body length written into the slab.
    body_len: usize,
    /// Attribute data (kept in-memory for simplicity; not persisted to the
    /// mmap in this implementation).
    attrs: HashMap<ObjAttr, Vec<u8>>,
}

/// Persistent memory-mapped file storage backend.
///
/// Objects are stored in the data region of the file. A slab table at the
/// beginning of the file records which slabs are in use and their digests, so
/// that on restart the stevedore can reload objects that are still valid.
pub struct PersistentStevedore {
    name: String,
    path: PathBuf,
    total_size: usize,
    used_size: AtomicUsize,
    mmap: Mutex<Option<MmapMut>>,
    allocations: Mutex<Vec<SlabEntry>>,
    /// Index from digest to slab table index for fast lookup.
    digest_index: Mutex<HashMap<Digest, usize>>,
    open: AtomicBool,
}

impl PersistentStevedore {
    /// Creates a new `PersistentStevedore`.
    ///
    /// # Arguments
    ///
    /// * `name` - Human-readable name for this storage backend.
    /// * `path` - Filesystem path for the backing file.
    /// * `size` - Total size of the backing file in bytes. Must be large enough
    ///   to hold the file header and slab table plus a meaningful data region.
    pub fn new(name: impl Into<String>, path: impl Into<PathBuf>, size: usize) -> Self {
        assert!(
            size > DATA_REGION_OFFSET,
            "file size must be larger than the header + slab table ({DATA_REGION_OFFSET} bytes)"
        );
        Self {
            name: name.into(),
            path: path.into(),
            total_size: size,
            used_size: AtomicUsize::new(0),
            mmap: Mutex::new(None),
            allocations: Mutex::new(Vec::new()),
            digest_index: Mutex::new(HashMap::new()),
            open: AtomicBool::new(false),
        }
    }

    /// Returns the backing file path.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Returns the number of currently allocated (in-use) slabs.
    pub fn slab_count(&self) -> usize {
        self.allocations.lock().iter().filter(|s| s.in_use).count()
    }

    /// Returns the usable data capacity (total size minus header/table overhead).
    pub fn data_capacity(&self) -> usize {
        self.total_size.saturating_sub(DATA_REGION_OFFSET)
    }

    /// Loads existing slab entries from an already-mapped file and returns
    /// `ObjCore` instances for slabs that are still marked in-use.
    ///
    /// This is called during `open()` when the file already exists and
    /// contains a valid header.
    pub fn load_existing(&self) -> Vec<ObjCore> {
        let mmap_guard = self.mmap.lock();
        let mmap = match mmap_guard.as_ref() {
            Some(m) => m,
            None => return Vec::new(),
        };

        let mut allocs = self.allocations.lock();
        let mut digest_idx = self.digest_index.lock();
        let mut recovered = Vec::new();
        let mut total_used = 0usize;

        for i in 0..MAX_SLABS {
            let entry_offset = FILE_HEADER_SIZE + i * SLAB_ENTRY_SIZE;
            if entry_offset + SLAB_ENTRY_SIZE > self.total_size {
                break;
            }

            let entry_bytes = &mmap[entry_offset..entry_offset + SLAB_ENTRY_SIZE];

            // Read the in_use flag.
            let in_use = entry_bytes[48] != 0;
            if !in_use {
                // Check if digest is all zeros -- means we have reached an
                // unused slot that was never written, so we can stop scanning.
                if entry_bytes[0..32].iter().all(|&b| b == 0) {
                    break;
                }
                // This was a previously freed slab; record it as free.
                let mut digest_bytes = [0u8; 32];
                digest_bytes.copy_from_slice(&entry_bytes[0..32]);
                let data_offset =
                    u64::from_le_bytes(entry_bytes[32..40].try_into().unwrap()) as usize;
                let slab_size =
                    u64::from_le_bytes(entry_bytes[40..48].try_into().unwrap()) as usize;

                allocs.push(SlabEntry {
                    index: i,
                    offset: data_offset,
                    size: slab_size,
                    in_use: false,
                    digest: Digest::new(digest_bytes),
                    body_len: 0,
                    attrs: HashMap::new(),
                });
                continue;
            }

            // Read digest.
            let mut digest_bytes = [0u8; 32];
            digest_bytes.copy_from_slice(&entry_bytes[0..32]);
            let digest = Digest::new(digest_bytes);

            // Read offset and size.
            let data_offset = u64::from_le_bytes(entry_bytes[32..40].try_into().unwrap()) as usize;
            let slab_size = u64::from_le_bytes(entry_bytes[40..48].try_into().unwrap()) as usize;
            let body_len = u64::from_le_bytes(entry_bytes[49..57].try_into().unwrap()) as usize;

            let slab = SlabEntry {
                index: i,
                offset: data_offset,
                size: slab_size,
                in_use: true,
                digest,
                body_len,
                attrs: HashMap::new(),
            };

            total_used += slab_size;
            digest_idx.insert(digest, i);

            let oc = ObjCore::new(digest);
            recovered.push(oc);

            allocs.push(slab);
        }

        self.used_size.store(total_used, Ordering::Release);

        debug!(
            name = %self.name,
            recovered = recovered.len(),
            total_used,
            "loaded existing slabs from persistent storage"
        );

        recovered
    }

    /// Writes the slab entry metadata to the on-disk slab table.
    fn write_slab_entry(&self, slab: &SlabEntry) {
        let mmap_guard = self.mmap.lock();
        let mmap = match mmap_guard.as_ref() {
            Some(m) => m,
            None => return,
        };

        let entry_offset = FILE_HEADER_SIZE + slab.index * SLAB_ENTRY_SIZE;
        let base = mmap.as_ptr() as *mut u8;

        // Safety: entry_offset is within the slab table region which is
        // entirely within the mmap. We hold the mmap lock so no concurrent
        // writes to the same entry can occur.
        unsafe {
            let dst = base.add(entry_offset);

            // [0..32] digest
            std::ptr::copy_nonoverlapping(slab.digest.bytes.as_ptr(), dst, 32);

            // [32..40] data offset
            let offset_bytes = (slab.offset as u64).to_le_bytes();
            std::ptr::copy_nonoverlapping(offset_bytes.as_ptr(), dst.add(32), 8);

            // [40..48] slab size
            let size_bytes = (slab.size as u64).to_le_bytes();
            std::ptr::copy_nonoverlapping(size_bytes.as_ptr(), dst.add(40), 8);

            // [48] in_use flag
            *dst.add(48) = if slab.in_use { 1 } else { 0 };

            // [49..57] body_len
            let body_len_bytes = (slab.body_len as u64).to_le_bytes();
            std::ptr::copy_nonoverlapping(body_len_bytes.as_ptr(), dst.add(49), 8);
        }
    }

    /// Writes the file header (magic, version).
    fn write_header(&self) {
        let mmap_guard = self.mmap.lock();
        let mmap = match mmap_guard.as_ref() {
            Some(m) => m,
            None => return,
        };

        let base = mmap.as_ptr() as *mut u8;
        // Safety: FILE_HEADER_SIZE is 128 which is smaller than any valid
        // total_size (DATA_REGION_OFFSET is much larger). We hold the lock.
        unsafe {
            // [0..8] magic
            std::ptr::copy_nonoverlapping(MAGIC.as_ptr(), base, 8);

            // [8..12] version
            let ver = VERSION.to_le_bytes();
            std::ptr::copy_nonoverlapping(ver.as_ptr(), base.add(8), 4);

            // [12..20] total file size
            let sz = (self.total_size as u64).to_le_bytes();
            std::ptr::copy_nonoverlapping(sz.as_ptr(), base.add(12), 8);
        }
    }

    /// Checks whether the file header contains a valid magic and version.
    fn validate_header(&self) -> bool {
        let mmap_guard = self.mmap.lock();
        let mmap = match mmap_guard.as_ref() {
            Some(m) => m,
            None => return false,
        };

        if mmap.len() < FILE_HEADER_SIZE {
            return false;
        }

        &mmap[0..8] == MAGIC && u32::from_le_bytes(mmap[8..12].try_into().unwrap()) == VERSION
    }

    /// Finds a free slab that can fit `size` bytes, or returns `None`.
    fn find_free_slab(&self, size: usize) -> Option<usize> {
        let allocs = self.allocations.lock();
        for (i, slab) in allocs.iter().enumerate() {
            if !slab.in_use && slab.size >= size {
                return Some(i);
            }
        }
        None
    }

    /// Allocates a new slab at the end of the data region.
    fn alloc_new_slab(&self, size: usize) -> Result<usize, StorageError> {
        let aligned_size = (size + DATA_ALIGN - 1) & !(DATA_ALIGN - 1);
        let mut allocs = self.allocations.lock();

        // Determine where the next free data region starts.
        let next_offset = if allocs.is_empty() {
            DATA_REGION_OFFSET
        } else {
            allocs
                .iter()
                .map(|s| s.offset + s.size)
                .max()
                .unwrap_or(DATA_REGION_OFFSET)
        };

        let aligned_offset = (next_offset + DATA_ALIGN - 1) & !(DATA_ALIGN - 1);

        if aligned_offset + aligned_size > self.total_size {
            return Err(StorageError::Full);
        }

        if allocs.len() >= MAX_SLABS {
            return Err(StorageError::Full);
        }

        let index = allocs.len();
        let slab = SlabEntry {
            index,
            offset: aligned_offset,
            size: aligned_size,
            in_use: false, // Will be set to true by the caller.
            digest: Digest::ZERO,
            body_len: 0,
            attrs: HashMap::new(),
        };

        allocs.push(slab);
        Ok(index)
    }

    /// Optional compaction: defragments by moving in-use slabs to the front
    /// of the data region and reclaiming free space at the end.
    ///
    /// This is an expensive operation and should be called during low-traffic
    /// periods.
    pub fn compact(&self) {
        let mmap_guard = self.mmap.lock();
        let mmap = match mmap_guard.as_ref() {
            Some(m) => m,
            None => return,
        };

        let mut allocs = self.allocations.lock();
        let mut digest_idx = self.digest_index.lock();

        // Collect in-use slabs sorted by their current offset.
        let mut in_use: Vec<usize> = allocs
            .iter()
            .enumerate()
            .filter(|(_, s)| s.in_use)
            .map(|(i, _)| i)
            .collect();
        in_use.sort_by_key(|&i| allocs[i].offset);

        let base = mmap.as_ptr() as *mut u8;
        let mut next_offset = DATA_REGION_OFFSET;

        for &idx in &in_use {
            let src_offset = allocs[idx].offset;
            let size = allocs[idx].body_len; // Only move the used portion.
            let aligned_size = (size + DATA_ALIGN - 1) & !(DATA_ALIGN - 1);

            if next_offset != src_offset && size > 0 {
                // Safety: both source and dest are within the mmap. We hold
                // both locks so no concurrent access.
                unsafe {
                    std::ptr::copy(base.add(src_offset), base.add(next_offset), size);
                }
            }

            allocs[idx].offset = next_offset;
            allocs[idx].size = aligned_size.max(allocs[idx].size);
            next_offset += allocs[idx].size;

            // Update the on-disk slab entry.
            // (We cannot call write_slab_entry here because we hold the mmap
            // lock. Instead we write directly.)
            let entry_offset = FILE_HEADER_SIZE + allocs[idx].index * SLAB_ENTRY_SIZE;
            unsafe {
                let dst = base.add(entry_offset);
                let offset_bytes = (allocs[idx].offset as u64).to_le_bytes();
                std::ptr::copy_nonoverlapping(offset_bytes.as_ptr(), dst.add(32), 8);
                let size_bytes = (allocs[idx].size as u64).to_le_bytes();
                std::ptr::copy_nonoverlapping(size_bytes.as_ptr(), dst.add(40), 8);
            }
        }

        // Remove free slabs from the allocations list and rebuild digest index.
        let active: Vec<SlabEntry> = allocs.iter().filter(|s| s.in_use).cloned().collect();

        digest_idx.clear();
        for (new_idx, slab) in active.iter().enumerate() {
            digest_idx.insert(slab.digest, new_idx);
        }
        *allocs = active;

        // Re-index the slab entries.
        for (i, slab) in allocs.iter_mut().enumerate() {
            slab.index = i;
        }

        let total_used: usize = allocs.iter().map(|s| s.size).sum();
        self.used_size.store(total_used, Ordering::Release);

        debug!(
            name = %self.name,
            slabs = allocs.len(),
            used = total_used,
            "compaction complete"
        );
    }
}

impl Stevedore for PersistentStevedore {
    fn name(&self) -> &str {
        &self.name
    }

    fn open(&mut self) -> Result<(), StorageError> {
        let exists = self.path.exists();

        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(!exists) // Only truncate if the file does not already exist.
            .open(&self.path)?;

        file.set_len(self.total_size as u64)?;

        // Safety: we own the file exclusively.
        let mmap = unsafe { MmapMut::map_mut(&file)? };
        *self.mmap.lock() = Some(mmap);

        if exists && self.validate_header() {
            debug!(
                name = %self.name,
                path = %self.path.display(),
                "opening existing persistent storage, scanning for recoverable objects"
            );
            let recovered = self.load_existing();
            debug!(
                name = %self.name,
                recovered = recovered.len(),
                "recovery complete"
            );
        } else {
            self.write_header();
            debug!(
                name = %self.name,
                path = %self.path.display(),
                size = self.total_size,
                "created new persistent storage file"
            );
        }

        self.open.store(true, Ordering::Release);
        Ok(())
    }

    fn close(&mut self) {
        if let Some(mmap) = self.mmap.lock().take() {
            let _ = mmap.flush();
        }
        self.allocations.lock().clear();
        self.digest_index.lock().clear();
        self.used_size.store(0, Ordering::Release);
        self.open.store(false, Ordering::Release);
        debug!(name = %self.name, "persistent stevedore closed");
    }

    fn alloc_obj(&self, oc: &mut ObjCore, size_hint: usize) -> Result<(), StorageError> {
        let required = size_hint.max(64); // Minimum slab size.

        // Try to reuse a free slab first.
        let slab_idx = if let Some(idx) = self.find_free_slab(required) {
            idx
        } else {
            self.alloc_new_slab(required)?
        };

        {
            let mut allocs = self.allocations.lock();
            let slab = &mut allocs[slab_idx];
            slab.in_use = true;
            slab.digest = oc.digest;
            slab.body_len = 0;
            slab.attrs.clear();

            self.used_size.fetch_add(slab.size, Ordering::AcqRel);

            // Write the slab entry to disk. We need to drop allocs first to
            // avoid a deadlock with write_slab_entry which takes the mmap lock.
            let slab_copy = slab.clone();
            drop(allocs);
            self.write_slab_entry(&slab_copy);
        }

        self.digest_index.lock().insert(oc.digest, slab_idx);

        debug!(
            digest = %oc.digest,
            slab_idx,
            size_hint,
            "allocated object in persistent stevedore"
        );
        Ok(())
    }

    fn free_obj(&self, oc: &mut ObjCore) {
        let slab_idx = { self.digest_index.lock().remove(&oc.digest) };

        if let Some(idx) = slab_idx {
            let mut allocs = self.allocations.lock();
            if idx < allocs.len() {
                let slab = &mut allocs[idx];
                let freed = slab.size;
                slab.in_use = false;
                slab.body_len = 0;
                slab.attrs.clear();

                self.used_size.fetch_sub(freed, Ordering::AcqRel);

                let slab_copy = slab.clone();
                drop(allocs);
                self.write_slab_entry(&slab_copy);

                debug!(
                    digest = %oc.digest,
                    freed,
                    "freed object from persistent stevedore"
                );
            }
        }
    }

    fn get_space(&self, _oc: &ObjCore, desired: usize) -> Result<Vec<u8>, StorageError> {
        // Return a temporary buffer; the caller fills it and calls extend().
        Ok(Vec::with_capacity(desired))
    }

    fn extend(&self, oc: &ObjCore, data: &[u8]) -> Result<(), StorageError> {
        let slab_idx = *self
            .digest_index
            .lock()
            .get(&oc.digest)
            .ok_or(StorageError::NotFound)?;

        let mmap_guard = self.mmap.lock();
        let mmap = mmap_guard.as_ref().ok_or_else(|| {
            StorageError::Io(std::io::Error::new(
                std::io::ErrorKind::NotConnected,
                "persistent stevedore not open",
            ))
        })?;

        let mut allocs = self.allocations.lock();
        let slab = allocs.get_mut(slab_idx).ok_or(StorageError::NotFound)?;

        let write_offset = slab.offset + slab.body_len;
        let write_end = write_offset + data.len();

        if write_end > slab.offset + slab.size {
            return Err(StorageError::Full);
        }

        // Safety: write_offset..write_end is within the slab's data region
        // which is within the mmap. We hold both locks.
        let base = mmap.as_ptr() as *mut u8;
        unsafe {
            std::ptr::copy_nonoverlapping(data.as_ptr(), base.add(write_offset), data.len());
        }

        slab.body_len += data.len();
        Ok(())
    }

    fn trim(&self, _oc: &ObjCore) {
        // No-op for persistent storage -- slab sizes are fixed.
    }

    fn get_attr(&self, oc: &ObjCore, attr: ObjAttr) -> Option<Vec<u8>> {
        let slab_idx = *self.digest_index.lock().get(&oc.digest)?;
        let allocs = self.allocations.lock();
        let slab = allocs.get(slab_idx)?;
        slab.attrs.get(&attr).cloned()
    }

    fn set_attr(&self, oc: &mut ObjCore, attr: ObjAttr, data: &[u8]) -> Result<(), StorageError> {
        let slab_idx = *self
            .digest_index
            .lock()
            .get(&oc.digest)
            .ok_or(StorageError::NotFound)?;

        let mut allocs = self.allocations.lock();
        let slab = allocs.get_mut(slab_idx).ok_or(StorageError::NotFound)?;
        slab.attrs.insert(attr, data.to_vec());
        Ok(())
    }

    fn get_body(&self, oc: &ObjCore) -> Option<Vec<u8>> {
        let slab_idx = *self.digest_index.lock().get(&oc.digest)?;

        let mmap_guard = self.mmap.lock();
        let mmap = mmap_guard.as_ref()?;
        let allocs = self.allocations.lock();
        let slab = allocs.get(slab_idx)?;

        if slab.body_len == 0 {
            return None;
        }

        let start = slab.offset;
        let end = start + slab.body_len;
        Some(mmap[start..end].to_vec())
    }

    fn total_space(&self) -> usize {
        self.total_size
    }

    fn used_space(&self) -> usize {
        self.used_size.load(Ordering::Acquire)
    }

    fn free_space(&self) -> usize {
        self.total_size
            .saturating_sub(self.used_size.load(Ordering::Acquire))
    }
}

impl Drop for PersistentStevedore {
    fn drop(&mut self) {
        if self.open.load(Ordering::Acquire) {
            self.close();
        }
    }
}

impl std::fmt::Debug for PersistentStevedore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PersistentStevedore")
            .field("name", &self.name)
            .field("path", &self.path)
            .field("total_size", &self.total_size)
            .field("used_size", &self.used_size.load(Ordering::Relaxed))
            .field("slabs", &self.allocations.lock().len())
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

    fn make_stevedore(dir: &Path) -> PersistentStevedore {
        let path = dir.join("test_persistent.bin");
        // 16 MB -- plenty for tests.
        let size = 16 * 1024 * 1024;
        let mut stv = PersistentStevedore::new("test-persistent", path, size);
        stv.open().unwrap();
        stv
    }

    #[test]
    fn test_alloc_and_free() {
        let tmp = tempfile::tempdir().unwrap();
        let stv = make_stevedore(tmp.path());

        let mut oc = ObjCore::new(test_digest(1));
        stv.alloc_obj(&mut oc, 256).unwrap();
        assert_eq!(stv.slab_count(), 1);
        assert!(stv.used_space() > 0);

        stv.free_obj(&mut oc);
        assert_eq!(stv.slab_count(), 0);
    }

    #[test]
    fn test_store_and_retrieve_body() {
        let tmp = tempfile::tempdir().unwrap();
        let stv = make_stevedore(tmp.path());

        let mut oc = ObjCore::new(test_digest(2));
        stv.alloc_obj(&mut oc, 256).unwrap();
        stv.extend(&oc, b"hello world").unwrap();

        let body = stv.get_body(&oc).unwrap();
        assert_eq!(body, b"hello world");
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
        assert_eq!(body, b"hello world");
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
    fn test_get_attr_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let stv = make_stevedore(tmp.path());

        let mut oc = ObjCore::new(test_digest(5));
        stv.alloc_obj(&mut oc, 256).unwrap();
        assert!(stv.get_attr(&oc, ObjAttr::Vary).is_none());
    }

    #[test]
    fn test_space_tracking() {
        let tmp = tempfile::tempdir().unwrap();
        let stv = make_stevedore(tmp.path());

        let size = 16 * 1024 * 1024;
        assert_eq!(stv.total_space(), size);
        let initial_free = stv.free_space();

        let mut oc = ObjCore::new(test_digest(6));
        stv.alloc_obj(&mut oc, 256).unwrap();

        assert!(stv.free_space() < initial_free);
        assert!(stv.used_space() > 0);

        stv.free_obj(&mut oc);
    }

    #[test]
    fn test_multiple_objects() {
        let tmp = tempfile::tempdir().unwrap();
        let stv = make_stevedore(tmp.path());

        let mut oc1 = ObjCore::new(test_digest(10));
        let mut oc2 = ObjCore::new(test_digest(11));

        stv.alloc_obj(&mut oc1, 128).unwrap();
        stv.alloc_obj(&mut oc2, 128).unwrap();
        assert_eq!(stv.slab_count(), 2);

        stv.extend(&oc1, b"body-one").unwrap();
        stv.extend(&oc2, b"body-two").unwrap();

        assert_eq!(stv.get_body(&oc1).unwrap(), b"body-one");
        assert_eq!(stv.get_body(&oc2).unwrap(), b"body-two");

        stv.free_obj(&mut oc1);
        assert_eq!(stv.slab_count(), 1);
        assert_eq!(stv.get_body(&oc2).unwrap(), b"body-two");
    }

    #[test]
    fn test_slab_reuse() {
        let tmp = tempfile::tempdir().unwrap();
        let stv = make_stevedore(tmp.path());

        // Allocate and free a slab.
        let mut oc1 = ObjCore::new(test_digest(20));
        stv.alloc_obj(&mut oc1, 256).unwrap();
        stv.free_obj(&mut oc1);
        assert_eq!(stv.slab_count(), 0);

        // Allocate again -- should reuse the freed slab.
        let mut oc2 = ObjCore::new(test_digest(21));
        stv.alloc_obj(&mut oc2, 128).unwrap();
        assert_eq!(stv.slab_count(), 1);

        stv.extend(&oc2, b"reused").unwrap();
        assert_eq!(stv.get_body(&oc2).unwrap(), b"reused");
    }

    #[test]
    fn test_get_body_empty() {
        let tmp = tempfile::tempdir().unwrap();
        let stv = make_stevedore(tmp.path());

        let mut oc = ObjCore::new(test_digest(30));
        stv.alloc_obj(&mut oc, 256).unwrap();
        assert!(stv.get_body(&oc).is_none());
    }

    #[test]
    fn test_extend_not_found() {
        let tmp = tempfile::tempdir().unwrap();
        let stv = make_stevedore(tmp.path());

        let oc = ObjCore::new(test_digest(40));
        let result = stv.extend(&oc, b"data");
        assert!(result.is_err());
    }

    #[test]
    fn test_set_attr_not_found() {
        let tmp = tempfile::tempdir().unwrap();
        let stv = make_stevedore(tmp.path());

        let mut oc = ObjCore::new(test_digest(50));
        let result = stv.set_attr(&mut oc, ObjAttr::Len, &[0u8; 8]);
        assert!(result.is_err());
    }

    #[test]
    fn test_data_capacity() {
        let tmp = tempfile::tempdir().unwrap();
        let stv = make_stevedore(tmp.path());
        let expected = 16 * 1024 * 1024 - DATA_REGION_OFFSET;
        assert_eq!(stv.data_capacity(), expected);
    }

    #[test]
    fn test_compact() {
        let tmp = tempfile::tempdir().unwrap();
        let stv = make_stevedore(tmp.path());

        // Allocate three objects, free the middle one, then compact.
        let mut oc1 = ObjCore::new(test_digest(60));
        let mut oc2 = ObjCore::new(test_digest(61));
        let mut oc3 = ObjCore::new(test_digest(62));

        stv.alloc_obj(&mut oc1, 128).unwrap();
        stv.alloc_obj(&mut oc2, 128).unwrap();
        stv.alloc_obj(&mut oc3, 128).unwrap();

        stv.extend(&oc1, b"first").unwrap();
        stv.extend(&oc2, b"second").unwrap();
        stv.extend(&oc3, b"third").unwrap();

        stv.free_obj(&mut oc2);
        assert_eq!(stv.slab_count(), 2);

        stv.compact();
        assert_eq!(stv.slab_count(), 2);

        // Remaining objects should still be readable.
        assert_eq!(stv.get_body(&oc1).unwrap(), b"first");
        assert_eq!(stv.get_body(&oc3).unwrap(), b"third");
    }
}

//! Storage backend traits.
//!
//! The [`Stevedore`] trait defines the interface that all storage backends must
//! implement. This mirrors the stevedore abstraction in Varnish, providing
//! pluggable object storage with allocation, attribute management, and capacity
//! reporting.

use rv_types::{ObjAttr, VtimReal};

use crate::objcore::ObjCore;

/// Errors that can occur during storage operations.
#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    /// The storage backend could not allocate the requested space.
    #[error("storage allocation failed")]
    AllocFailed,

    /// The storage backend has reached its configured capacity.
    #[error("storage full")]
    Full,

    /// The requested object was not found in the storage backend.
    #[error("object not found")]
    NotFound,

    /// An underlying I/O error occurred.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}

/// Information about a ban operation on an object.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BanInfo {
    /// Object was newly added to the ban list.
    New,
    /// Object was removed from the ban list.
    Drop,
}

/// The core storage backend trait.
///
/// Each stevedore implementation manages a region of storage (heap, file,
/// persistent, etc.) and provides methods to allocate, free, read, and write
/// cache objects within that region.
///
/// Implementations must be `Send + Sync` to allow concurrent access from
/// multiple worker threads via the Tokio runtime.
pub trait Stevedore: Send + Sync {
    /// Returns the human-readable name of this storage backend.
    fn name(&self) -> &str;

    /// Opens the storage backend, performing any required initialization.
    fn open(&mut self) -> Result<(), StorageError>;

    /// Closes the storage backend, releasing resources.
    fn close(&mut self);

    /// Allocates storage for an object.
    ///
    /// The `size_hint` provides an estimated size in bytes. The stevedore may
    /// allocate more or less depending on its internal strategy. On success,
    /// the `ObjCore` is associated with this stevedore.
    fn alloc_obj(&self, oc: &mut ObjCore, size_hint: usize) -> Result<(), StorageError>;

    /// Frees all storage associated with the given object.
    fn free_obj(&self, oc: &mut ObjCore);

    /// Requests a buffer of the desired size for writing body data.
    ///
    /// Returns a `Vec<u8>` with capacity of at least `desired` bytes.
    fn get_space(&self, oc: &ObjCore, desired: usize) -> Result<Vec<u8>, StorageError>;

    /// Appends `data` to the object's stored body.
    fn extend(&self, oc: &ObjCore, data: &[u8]) -> Result<(), StorageError>;

    /// Trims any over-allocated storage for the object down to its actual size.
    fn trim(&self, oc: &ObjCore);

    /// Retrieves the value of the given attribute for an object.
    fn get_attr(&self, oc: &ObjCore, attr: ObjAttr) -> Option<Vec<u8>>;

    /// Sets the value of the given attribute for an object.
    fn set_attr(
        &self,
        oc: &mut ObjCore,
        attr: ObjAttr,
        data: &[u8],
    ) -> Result<(), StorageError>;

    /// Retrieves the full body of the object.
    fn get_body(&self, oc: &ObjCore) -> Option<Vec<u8>>;

    /// Returns the total storage capacity in bytes.
    fn total_space(&self) -> usize;

    /// Returns the number of bytes currently in use.
    fn used_space(&self) -> usize;

    /// Returns the number of bytes available for allocation.
    fn free_space(&self) -> usize;
}

/// Provides object method dispatch for a stevedore.
///
/// This trait allows different stevedore implementations to customize how
/// objects are accessed and manipulated beyond the base `Stevedore` trait.
pub trait ObjMethodProvider: Send + Sync {
    /// Returns the expiry timestamp used for timer scheduling.
    fn get_timer_when(&self, oc: &ObjCore) -> VtimReal;

    /// Sets the expiry timestamp used for timer scheduling.
    fn set_timer_when(&self, oc: &mut ObjCore, when: VtimReal);
}

//! Pluggable storage backends for cached objects.
//!
//! This crate provides the [`Stevedore`] trait and two concrete
//! implementations:
//!
//! - [`MallocStevedore`] -- heap-allocated storage (the default).
//! - [`FileStevedore`] -- memory-mapped file storage for large working sets.
//!
//! It also provides [`ObjCore`], the central metadata structure for every
//! cached object, and [`Lru`]/[`SyncLru`] for LRU eviction.

pub mod file;
pub mod lru;
pub mod malloc;
pub mod objcore;
pub mod persistent;
pub mod traits;

// Re-export the primary public types at crate root.
pub use file::FileStevedore;
pub use lru::{Lru, SyncLru};
pub use malloc::MallocStevedore;
pub use objcore::ObjCore;
pub use persistent::PersistentStevedore;
pub use traits::{BanInfo, ObjMethodProvider, Stevedore, StorageError};

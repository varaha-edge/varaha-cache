//! rv-hash: Pluggable hash backends for cache object lookup.
//!
//! This crate provides the `HashSlinger` trait and three implementations:
//!
//! - `SimpleListHash` -- O(n) linear scan, useful for testing and reference.
//! - `ClassicHash` -- Fixed-size hash table with per-bucket locking, O(n/buckets) amortized.
//! - `CritbitHash` -- Crit-bit tree (binary trie), O(key_length) lookup.
//!
//! Each implementation maps a `Digest` (32-byte SHA-256 hash) to an `ObjHead`,
//! which serves as the hash bucket / top-level entry in the cache object chain.

pub mod classic;
pub mod critbit;
pub mod objhead;
pub mod simple;
pub mod traits;

pub use classic::ClassicHash;
pub use critbit::CritbitHash;
pub use objhead::ObjHead;
pub use simple::SimpleListHash;
pub use traits::{HashError, HashSlinger};

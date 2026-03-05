use std::sync::Arc;

use rv_types::Digest;

use crate::objhead::ObjHead;

/// Errors that can occur during hash operations.
#[derive(Debug, thiserror::Error)]
pub enum HashError {
    #[error("hash initialization failed: {0}")]
    InitError(String),
}

/// The HashSlinger trait defines the interface for pluggable hash backends.
///
/// This mirrors Varnish's hash_slinger concept: the hash subsystem is
/// responsible for mapping Digest values to ObjHead entries, which serve
/// as the top-level buckets in the cache object lookup chain.
///
/// Implementations must be thread-safe (Send + Sync) as the hash table
/// is accessed concurrently from multiple worker threads.
pub trait HashSlinger: Send + Sync {
    /// Returns the name of this hash implementation (e.g. "simple_list", "classic", "critbit").
    fn name(&self) -> &str;

    /// Called once at startup to initialize the hash subsystem.
    fn start(&self);

    /// Look up or insert an ObjHead for the given digest.
    ///
    /// If an ObjHead with a matching digest already exists, returns it and
    /// gives back the unused `new_oh` in the Option so the caller can recycle it.
    ///
    /// If no match exists, inserts `new_oh` into the hash and returns it.
    /// The Option will be None in this case, indicating the new ObjHead was consumed.
    ///
    /// The returned ObjHead will have its refcount incremented.
    fn lookup(&self, digest: &Digest, new_oh: Arc<ObjHead>)
    -> (Arc<ObjHead>, Option<Arc<ObjHead>>);

    /// Dereference an ObjHead, decrementing its refcount.
    ///
    /// Returns true if the ObjHead was removed from the hash (refcount dropped to 0).
    fn deref(&self, oh: &Arc<ObjHead>) -> bool;
}

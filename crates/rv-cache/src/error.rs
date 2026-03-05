use thiserror::Error;

/// Errors that can occur in the cache engine.
#[derive(Debug, Error)]
pub enum CacheError {
    #[error("cache lookup failed: {0}")]
    LookupFailed(String),

    #[error("object insertion failed: {0}")]
    InsertFailed(String),

    #[error("storage error: {0}")]
    Storage(#[from] rv_storage::StorageError),

    #[error("backend fetch failed: {0}")]
    FetchFailed(String),

    #[error("ban expression error: {0}")]
    BanError(String),

    #[error("object expired")]
    Expired,

    #[error("object not found")]
    NotFound,

    #[error("request processing error: {0}")]
    RequestError(String),
}

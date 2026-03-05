use thiserror::Error;

#[derive(Debug, Error)]
pub enum FilterError {
    #[error("filter error: {0}")]
    Error(String),
    #[error("filter I/O error: {0}")]
    Io(#[from] std::io::Error),
}

/// Fetch processor status, mirroring enum vfp_status from cache_filter.h.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VfpStatus {
    /// Error occurred
    Error,
    /// Data available, more to come
    Ok,
    /// Data available, this is the last chunk
    End,
    /// No data (filter skipped)
    Null,
}

/// Delivery processor action, mirroring enum vdp_action from cache_filter.h.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VdpAction {
    /// Input buffer remains valid after call
    Null,
    /// Input buffer will be invalidated (flush)
    Flush,
    /// Last buffer, implies flush
    End,
}

/// Fetch processor trait (VFP).
/// Filters in the fetch pipeline transform data as it arrives from the backend.
/// Based on struct vfp from cache_filter.h.
pub trait FetchProcessor: Send + Sync {
    fn name(&self) -> &str;

    /// Initialize the filter. Called once when the filter chain is set up.
    fn init(&mut self) -> VfpStatus;

    /// Pull data through the filter. Reads from upstream, transforms, and
    /// writes to the provided buffer. Returns (status, bytes_written).
    fn pull(&mut self, buf: &mut [u8]) -> (VfpStatus, usize);

    /// Finalize the filter. Called when the chain is torn down.
    fn fini(&mut self);
}

/// Delivery processor trait (VDP).
/// Filters in the delivery pipeline transform data as it goes to the client.
/// Based on struct vdp from cache_filter.h.
pub trait DeliveryProcessor: Send + Sync {
    fn name(&self) -> &str;

    /// Initialize the filter.
    fn init(&mut self) -> Result<(), i32>;

    /// Process a chunk of data. Returns the transformed output bytes on success.
    fn bytes(&mut self, action: VdpAction, data: &[u8]) -> Result<Vec<u8>, i32>;

    /// Finalize the filter.
    fn fini(&mut self);
}

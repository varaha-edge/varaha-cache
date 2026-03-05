use crate::traits::{DeliveryProcessor, FetchProcessor, VdpAction, VfpStatus};

/// A chain of fetch processors.
/// Data flows from the last filter (closest to the backend) through
/// each preceding filter to the first (closest to storage).
///
/// Each filter's `pull` reads from the previous filter in the chain,
/// forming a pipeline. The last filter is the data source (backend).
pub struct FetchFilterChain {
    filters: Vec<Box<dyn FetchProcessor>>,
}

impl FetchFilterChain {
    pub fn new() -> Self {
        Self {
            filters: Vec::new(),
        }
    }

    /// Add a filter to the end of the chain (closest to backend/upstream).
    pub fn push(&mut self, filter: Box<dyn FetchProcessor>) {
        self.filters.push(filter);
    }

    /// Initialize all filters in the chain.
    pub fn init(&mut self) -> VfpStatus {
        for filter in &mut self.filters {
            let status = filter.init();
            if status == VfpStatus::Error {
                return VfpStatus::Error;
            }
        }
        VfpStatus::Ok
    }

    /// Pull data through the entire chain.
    ///
    /// Pulls from the last filter (source), then passes data through each
    /// preceding filter in reverse order, forming a pipeline where each
    /// filter transforms the output of the next.
    pub fn pull(&mut self, buf: &mut [u8]) -> (VfpStatus, usize) {
        if self.filters.is_empty() {
            return (VfpStatus::End, 0);
        }
        if self.filters.len() == 1 {
            return self.filters[0].pull(buf);
        }

        // Pull from the last filter (data source)
        let last = self.filters.len() - 1;
        let (mut status, mut len) = self.filters[last].pull(buf);

        // Pass through each preceding filter in reverse order
        for i in (0..last).rev() {
            if status == VfpStatus::Error {
                break;
            }
            let (new_status, new_len) = self.filters[i].pull(&mut buf[..len]);
            status = match (status, new_status) {
                (_, VfpStatus::Error) => VfpStatus::Error,
                (VfpStatus::End, _) => VfpStatus::End,
                (_, s) => s,
            };
            len = new_len;
        }

        (status, len)
    }

    /// Finalize all filters.
    pub fn fini(&mut self) {
        for filter in &mut self.filters {
            filter.fini();
        }
    }

    pub fn len(&self) -> usize {
        self.filters.len()
    }

    pub fn is_empty(&self) -> bool {
        self.filters.is_empty()
    }
}

impl Default for FetchFilterChain {
    fn default() -> Self {
        Self::new()
    }
}

/// A chain of delivery processors.
/// Data flows from the first filter (closest to cache) through
/// each subsequent filter to the last (closest to client).
///
/// Each filter's output is fed as input to the next filter.
pub struct DeliveryFilterChain {
    filters: Vec<Box<dyn DeliveryProcessor>>,
}

impl DeliveryFilterChain {
    pub fn new() -> Self {
        Self {
            filters: Vec::new(),
        }
    }

    /// Add a filter to the end of the chain (closest to client).
    pub fn push(&mut self, filter: Box<dyn DeliveryProcessor>) {
        self.filters.push(filter);
    }

    /// Initialize all filters in the chain.
    pub fn init(&mut self) -> Result<(), i32> {
        for filter in &mut self.filters {
            filter.init()?;
        }
        Ok(())
    }

    /// Push data through the entire chain.
    /// Each filter's output is fed as input to the next filter, forming
    /// a proper transformation pipeline.
    pub fn bytes(&mut self, action: VdpAction, data: &[u8]) -> Result<Vec<u8>, i32> {
        if self.filters.is_empty() {
            return Ok(data.to_vec());
        }

        let mut current_data = data.to_vec();
        for filter in &mut self.filters {
            current_data = filter.bytes(action, &current_data)?;
        }
        Ok(current_data)
    }

    /// Finalize all filters.
    pub fn fini(&mut self) {
        for filter in &mut self.filters {
            filter.fini();
        }
    }

    pub fn len(&self) -> usize {
        self.filters.len()
    }

    pub fn is_empty(&self) -> bool {
        self.filters.is_empty()
    }
}

impl Default for DeliveryFilterChain {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct PassthroughVfp;
    impl FetchProcessor for PassthroughVfp {
        fn name(&self) -> &str { "passthrough" }
        fn init(&mut self) -> VfpStatus { VfpStatus::Ok }
        fn pull(&mut self, buf: &mut [u8]) -> (VfpStatus, usize) {
            let data = b"hello";
            let len = data.len().min(buf.len());
            buf[..len].copy_from_slice(&data[..len]);
            (VfpStatus::End, len)
        }
        fn fini(&mut self) {}
    }

    /// A filter that uppercases data passing through it.
    struct UppercaseVfp;
    impl FetchProcessor for UppercaseVfp {
        fn name(&self) -> &str { "uppercase" }
        fn init(&mut self) -> VfpStatus { VfpStatus::Ok }
        fn pull(&mut self, buf: &mut [u8]) -> (VfpStatus, usize) {
            let len = buf.len();
            for b in buf[..len].iter_mut() {
                *b = b.to_ascii_uppercase();
            }
            (VfpStatus::Ok, len)
        }
        fn fini(&mut self) {}
    }

    struct PassthroughVdp;
    impl DeliveryProcessor for PassthroughVdp {
        fn name(&self) -> &str { "passthrough" }
        fn init(&mut self) -> Result<(), i32> { Ok(()) }
        fn bytes(&mut self, _action: VdpAction, data: &[u8]) -> Result<Vec<u8>, i32> {
            Ok(data.to_vec())
        }
        fn fini(&mut self) {}
    }

    #[test]
    fn test_fetch_chain_single() {
        let mut chain = FetchFilterChain::new();
        chain.push(Box::new(PassthroughVfp));
        assert_eq!(chain.init(), VfpStatus::Ok);

        let mut buf = [0u8; 1024];
        let (status, len) = chain.pull(&mut buf);
        assert_eq!(status, VfpStatus::End);
        assert_eq!(&buf[..len], b"hello");
        chain.fini();
    }

    #[test]
    fn test_fetch_chain_pipeline() {
        let mut chain = FetchFilterChain::new();
        // UppercaseVfp is first (closest to storage), PassthroughVfp is last (source)
        chain.push(Box::new(UppercaseVfp));
        chain.push(Box::new(PassthroughVfp));
        assert_eq!(chain.init(), VfpStatus::Ok);

        let mut buf = [0u8; 1024];
        let (status, len) = chain.pull(&mut buf);
        assert_eq!(status, VfpStatus::End);
        assert_eq!(&buf[..len], b"HELLO");
        chain.fini();
    }

    #[test]
    fn test_delivery_chain() {
        let mut chain = DeliveryFilterChain::new();
        chain.push(Box::new(PassthroughVdp));
        assert!(chain.init().is_ok());
        let output = chain.bytes(VdpAction::End, b"world").unwrap();
        assert_eq!(&output, b"world");
        chain.fini();
    }

    #[test]
    fn test_empty_chains() {
        let mut fetch = FetchFilterChain::new();
        let mut buf = [0u8; 64];
        let (status, len) = fetch.pull(&mut buf);
        assert_eq!(status, VfpStatus::End);
        assert_eq!(len, 0);

        let mut delivery = DeliveryFilterChain::new();
        let output = delivery.bytes(VdpAction::End, b"data").unwrap();
        assert_eq!(&output, b"data");
    }
}

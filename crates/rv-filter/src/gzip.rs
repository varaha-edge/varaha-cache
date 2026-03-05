use std::io::{Read, Write};

use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use flate2::Compression;

use crate::traits::{DeliveryProcessor, FetchProcessor, VdpAction, VfpStatus};

/// Fetch processor that decompresses gzip data from the backend.
pub struct GzipDecompressVfp {
    buffer: Vec<u8>,
    upstream_data: Option<Vec<u8>>,
}

impl GzipDecompressVfp {
    pub fn new() -> Self {
        Self {
            buffer: Vec::new(),
            upstream_data: None,
        }
    }

    /// Feed data into the decompressor for processing.
    pub fn set_input(&mut self, data: Vec<u8>) {
        self.upstream_data = Some(data);
    }
}

impl Default for GzipDecompressVfp {
    fn default() -> Self {
        Self::new()
    }
}

impl FetchProcessor for GzipDecompressVfp {
    fn name(&self) -> &str {
        "gzip_decompress"
    }

    fn init(&mut self) -> VfpStatus {
        VfpStatus::Ok
    }

    fn pull(&mut self, buf: &mut [u8]) -> (VfpStatus, usize) {
        // If upstream_data has been set, consume it into buffer first
        if let Some(data) = self.upstream_data.take() {
            self.buffer = data;
        }

        if self.buffer.is_empty() {
            return (VfpStatus::End, 0);
        }

        let mut decoder = GzDecoder::new(&self.buffer[..]);
        match decoder.read(buf) {
            Ok(0) => (VfpStatus::End, 0),
            Ok(n) => (VfpStatus::Ok, n),
            Err(_) => (VfpStatus::Error, 0),
        }
    }

    fn fini(&mut self) {
        self.buffer.clear();
        self.upstream_data = None;
    }
}

/// Delivery processor that compresses data with gzip before sending to client.
pub struct GzipCompressVdp {
    compression_level: Compression,
}

impl GzipCompressVdp {
    pub fn new(level: u32) -> Self {
        Self {
            compression_level: Compression::new(level),
        }
    }
}

impl Default for GzipCompressVdp {
    fn default() -> Self {
        Self::new(6)
    }
}

impl DeliveryProcessor for GzipCompressVdp {
    fn name(&self) -> &str {
        "gzip_compress"
    }

    fn init(&mut self) -> Result<(), i32> {
        Ok(())
    }

    fn bytes(&mut self, _action: VdpAction, data: &[u8]) -> Result<Vec<u8>, i32> {
        let mut encoder = GzEncoder::new(Vec::new(), self.compression_level);
        encoder.write_all(data).map_err(|_| -1)?;
        let compressed = encoder.finish().map_err(|_| -1)?;
        Ok(compressed)
    }

    fn fini(&mut self) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_gzip_compress_vdp() {
        let mut vdp = GzipCompressVdp::default();
        assert!(vdp.init().is_ok());
        let compressed = vdp.bytes(VdpAction::End, b"hello world").unwrap();
        assert!(!compressed.is_empty());
        vdp.fini();
    }

    #[test]
    fn test_gzip_compress_returns_valid_data() {
        let mut vdp = GzipCompressVdp::default();
        vdp.init().unwrap();
        let compressed = vdp.bytes(VdpAction::End, b"test data").unwrap();

        // Verify compressed data is valid gzip by decompressing it
        let mut decoder = GzDecoder::new(&compressed[..]);
        let mut decompressed = Vec::new();
        decoder.read_to_end(&mut decompressed).unwrap();
        assert_eq!(&decompressed, b"test data");
        vdp.fini();
    }

    #[test]
    fn test_gzip_roundtrip() {
        let original = b"hello world, this is a test of gzip compression";

        // Compress via VDP
        let mut vdp = GzipCompressVdp::default();
        vdp.init().unwrap();
        let compressed = vdp.bytes(VdpAction::End, original).unwrap();
        vdp.fini();

        // Decompress via VFP using set_input
        let mut vfp = GzipDecompressVfp::new();
        vfp.set_input(compressed);
        assert_eq!(vfp.init(), VfpStatus::Ok);

        let mut buf = [0u8; 1024];
        let (status, len) = vfp.pull(&mut buf);
        assert_eq!(status, VfpStatus::Ok);
        assert_eq!(&buf[..len], original);
        vfp.fini();
    }

    #[test]
    fn test_gzip_decompress_with_upstream_data() {
        let original = b"upstream data test";

        // Create compressed data
        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(original).unwrap();
        let compressed = encoder.finish().unwrap();

        // Use set_input to feed data
        let mut vfp = GzipDecompressVfp::new();
        vfp.set_input(compressed);
        assert_eq!(vfp.init(), VfpStatus::Ok);

        let mut buf = [0u8; 1024];
        let (status, len) = vfp.pull(&mut buf);
        assert_eq!(status, VfpStatus::Ok);
        assert_eq!(&buf[..len], original);
        vfp.fini();
    }

    #[test]
    fn test_gzip_decompress_empty_input() {
        let mut vfp = GzipDecompressVfp::new();
        assert_eq!(vfp.init(), VfpStatus::Ok);

        let mut buf = [0u8; 1024];
        let (status, len) = vfp.pull(&mut buf);
        assert_eq!(status, VfpStatus::End);
        assert_eq!(len, 0);
        vfp.fini();
    }
}

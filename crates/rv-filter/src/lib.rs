pub mod chain;
pub mod esi;
pub mod gzip;
pub mod traits;

pub use chain::{DeliveryFilterChain, FetchFilterChain};
pub use esi::{EsiFragment, EsiProcessor};
pub use gzip::{GzipCompressVdp, GzipDecompressVfp};
pub use traits::{DeliveryProcessor, FetchProcessor, FilterError, VdpAction, VfpStatus};

use std::io::{Read, Write};

use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use flate2::Compression;

/// Compress data with gzip using default compression level (6).
///
/// Returns the compressed bytes on success, or -1 on failure.
pub fn compress_gzip(data: &[u8]) -> Result<Vec<u8>, i32> {
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(data).map_err(|_| -1)?;
    encoder.finish().map_err(|_| -1)
}

/// Decompress gzip data.
///
/// Returns the decompressed bytes on success, or a descriptive error string on failure.
pub fn decompress_gzip(data: &[u8]) -> Result<Vec<u8>, String> {
    let mut decoder = GzDecoder::new(data);
    let mut decompressed = Vec::new();
    decoder
        .read_to_end(&mut decompressed)
        .map_err(|e| format!("gzip decompression failed: {e}"))?;
    Ok(decompressed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_compress_gzip_basic() {
        let original = b"hello world, this is a test of gzip compression";
        let compressed = compress_gzip(original).unwrap();
        assert!(!compressed.is_empty());
        // Compressed data should generally be different from original
        assert_ne!(&compressed[..], &original[..]);
    }

    #[test]
    fn test_decompress_gzip_basic() {
        let original = b"hello world, this is a test of gzip decompression";
        let compressed = compress_gzip(original).unwrap();
        let decompressed = decompress_gzip(&compressed).unwrap();
        assert_eq!(&decompressed, original);
    }

    #[test]
    fn test_compress_decompress_roundtrip() {
        let original = b"roundtrip test data with special chars: !@#$%^&*()";
        let compressed = compress_gzip(original).unwrap();
        let decompressed = decompress_gzip(&compressed).unwrap();
        assert_eq!(&decompressed, original);
    }

    #[test]
    fn test_compress_empty_data() {
        let compressed = compress_gzip(b"").unwrap();
        let decompressed = decompress_gzip(&compressed).unwrap();
        assert!(decompressed.is_empty());
    }

    #[test]
    fn test_decompress_invalid_data() {
        let result = decompress_gzip(b"not valid gzip data");
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.contains("gzip decompression failed"));
    }

    #[test]
    fn test_compress_large_data() {
        let original: Vec<u8> = (0..10_000).map(|i| (i % 256) as u8).collect();
        let compressed = compress_gzip(&original).unwrap();
        let decompressed = decompress_gzip(&compressed).unwrap();
        assert_eq!(decompressed, original);
    }
}

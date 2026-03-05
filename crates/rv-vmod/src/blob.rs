//! vmod_blob -- binary large object encoding and decoding.
//!
//! Provides functions for encoding binary data to various text
//! representations and decoding them back.

use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64_STANDARD};

use crate::error::VmodError;
use crate::registry::VmodFunction;
use crate::types::VclValue;

/// Supported blob encodings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlobEncoding {
    Base64,
    Hex,
    Url,
}

impl BlobEncoding {
    /// Parse an encoding name (case-insensitive).
    fn from_str(s: &str) -> Result<Self, VmodError> {
        match s.to_uppercase().as_str() {
            "BASE64" => Ok(BlobEncoding::Base64),
            "HEX" => Ok(BlobEncoding::Hex),
            "URL" => Ok(BlobEncoding::Url),
            _ => Err(VmodError::InvalidArgument(format!(
                "unsupported encoding: {s}"
            ))),
        }
    }
}

/// Encode binary data to a text representation.
fn encode_data(encoding: BlobEncoding, data: &[u8]) -> String {
    match encoding {
        BlobEncoding::Base64 => BASE64_STANDARD.encode(data),
        BlobEncoding::Hex => {
            let mut hex = String::with_capacity(data.len() * 2);
            for byte in data {
                hex.push_str(&format!("{byte:02x}"));
            }
            hex
        }
        BlobEncoding::Url => {
            let mut result = String::with_capacity(data.len() * 3);
            for &byte in data {
                if byte.is_ascii_alphanumeric()
                    || byte == b'-'
                    || byte == b'_'
                    || byte == b'.'
                    || byte == b'~'
                {
                    result.push(byte as char);
                } else {
                    result.push_str(&format!("%{byte:02X}"));
                }
            }
            result
        }
    }
}

/// Decode a text representation back to binary data.
fn decode_data(encoding: BlobEncoding, s: &str) -> Result<Vec<u8>, VmodError> {
    match encoding {
        BlobEncoding::Base64 => BASE64_STANDARD
            .decode(s)
            .map_err(|e| VmodError::InvalidArgument(format!("invalid base64: {e}"))),
        BlobEncoding::Hex => decode_hex(s),
        BlobEncoding::Url => decode_url(s),
    }
}

/// Decode a hex string to bytes.
fn decode_hex(s: &str) -> Result<Vec<u8>, VmodError> {
    let s = s.trim();
    if s.len() % 2 != 0 {
        return Err(VmodError::InvalidArgument(
            "hex string must have even length".to_string(),
        ));
    }

    let mut bytes = Vec::with_capacity(s.len() / 2);
    let mut chars = s.chars();
    while let (Some(hi), Some(lo)) = (chars.next(), chars.next()) {
        let hi = hi
            .to_digit(16)
            .ok_or_else(|| VmodError::InvalidArgument(format!("invalid hex char: {hi}")))?
            as u8;
        let lo = lo
            .to_digit(16)
            .ok_or_else(|| VmodError::InvalidArgument(format!("invalid hex char: {lo}")))?
            as u8;
        bytes.push((hi << 4) | lo);
    }
    Ok(bytes)
}

/// Decode a percent-encoded (URL) string to bytes.
fn decode_url(s: &str) -> Result<Vec<u8>, VmodError> {
    let mut bytes = Vec::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '%' {
            let hi = chars
                .next()
                .ok_or_else(|| {
                    VmodError::InvalidArgument("truncated percent encoding".to_string())
                })?
                .to_digit(16)
                .ok_or_else(|| VmodError::InvalidArgument("invalid percent encoding".to_string()))?
                as u8;
            let lo = chars
                .next()
                .ok_or_else(|| {
                    VmodError::InvalidArgument("truncated percent encoding".to_string())
                })?
                .to_digit(16)
                .ok_or_else(|| VmodError::InvalidArgument("invalid percent encoding".to_string()))?
                as u8;
            bytes.push((hi << 4) | lo);
        } else {
            // For non-ASCII characters, encode as UTF-8 bytes
            let mut buf = [0u8; 4];
            let encoded = c.encode_utf8(&mut buf);
            bytes.extend_from_slice(encoded.as_bytes());
        }
    }
    Ok(bytes)
}

/// blob.encode(encoding, data) - Encode binary data to a text format.
pub struct BlobEncode;

impl VmodFunction for BlobEncode {
    fn name(&self) -> &str {
        "encode"
    }

    fn call(&self, args: &[VclValue]) -> Result<VclValue, VmodError> {
        if args.len() != 2 {
            return Err(VmodError::InvalidArgument(
                "encode(encoding, data) requires exactly 2 arguments".to_string(),
            ));
        }

        let encoding_name = args[0].to_string_value();
        let encoding = BlobEncoding::from_str(&encoding_name)?;

        let data = match &args[1] {
            VclValue::Blob(bytes) => bytes.clone(),
            VclValue::String(s) => s.as_bytes().to_vec(),
            other => {
                return Err(VmodError::TypeMismatch(format!(
                    "encode expects blob or string data, got {other}"
                )));
            }
        };

        let encoded = encode_data(encoding, &data);
        Ok(VclValue::String(encoded))
    }
}

/// blob.decode(encoding, string) - Decode a text representation to binary data.
pub struct BlobDecode;

impl VmodFunction for BlobDecode {
    fn name(&self) -> &str {
        "decode"
    }

    fn call(&self, args: &[VclValue]) -> Result<VclValue, VmodError> {
        if args.len() != 2 {
            return Err(VmodError::InvalidArgument(
                "decode(encoding, string) requires exactly 2 arguments".to_string(),
            ));
        }

        let encoding_name = args[0].to_string_value();
        let encoding = BlobEncoding::from_str(&encoding_name)?;
        let input = args[1].to_string_value();

        let decoded = decode_data(encoding, &input)?;
        Ok(VclValue::Blob(decoded))
    }
}

/// Build the full set of blob module functions.
pub fn blob_module() -> Vec<Box<dyn VmodFunction>> {
    vec![Box::new(BlobEncode), Box::new(BlobDecode)]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_base64_roundtrip() {
        let original = b"Hello, World!";
        let encoded = encode_data(BlobEncoding::Base64, original);
        assert_eq!(encoded, "SGVsbG8sIFdvcmxkIQ==");
        let decoded = decode_data(BlobEncoding::Base64, &encoded).unwrap();
        assert_eq!(decoded, original);
    }

    #[test]
    fn test_hex_roundtrip() {
        let original = vec![0xde, 0xad, 0xbe, 0xef];
        let encoded = encode_data(BlobEncoding::Hex, &original);
        assert_eq!(encoded, "deadbeef");
        let decoded = decode_data(BlobEncoding::Hex, &encoded).unwrap();
        assert_eq!(decoded, original);
    }

    #[test]
    fn test_url_roundtrip() {
        let original = b"hello world/foo?bar=baz";
        let encoded = encode_data(BlobEncoding::Url, original);
        assert!(encoded.contains("%20")); // space encoded
        assert!(encoded.contains("%2F")); // slash encoded
        let decoded = decode_data(BlobEncoding::Url, &encoded).unwrap();
        assert_eq!(decoded, original);
    }

    #[test]
    fn test_url_unreserved_chars() {
        let original = b"hello-world_test.file~";
        let encoded = encode_data(BlobEncoding::Url, original);
        // Unreserved chars should not be percent-encoded
        assert_eq!(encoded, "hello-world_test.file~");
    }

    #[test]
    fn test_hex_odd_length_error() {
        let result = decode_data(BlobEncoding::Hex, "abc");
        assert!(result.is_err());
    }

    #[test]
    fn test_hex_invalid_char_error() {
        let result = decode_data(BlobEncoding::Hex, "zz");
        assert!(result.is_err());
    }

    #[test]
    fn test_invalid_encoding() {
        let result = BlobEncoding::from_str("ROT13");
        assert!(result.is_err());
    }

    #[test]
    fn test_encode_function() {
        let f = BlobEncode;
        let result = f
            .call(&[
                VclValue::String("HEX".to_string()),
                VclValue::Blob(vec![0xca, 0xfe]),
            ])
            .unwrap();
        assert_eq!(result.to_string_value(), "cafe");
    }

    #[test]
    fn test_decode_function() {
        let f = BlobDecode;
        let result = f
            .call(&[
                VclValue::String("HEX".to_string()),
                VclValue::String("cafe".to_string()),
            ])
            .unwrap();
        if let VclValue::Blob(data) = result {
            assert_eq!(data, vec![0xca, 0xfe]);
        } else {
            panic!("expected VBlob");
        }
    }

    #[test]
    fn test_encode_string_input() {
        let f = BlobEncode;
        let result = f
            .call(&[
                VclValue::String("BASE64".to_string()),
                VclValue::String("test".to_string()),
            ])
            .unwrap();
        assert_eq!(result.to_string_value(), "dGVzdA==");
    }

    #[test]
    fn test_encode_decode_roundtrip_via_functions() {
        let original_data = vec![1u8, 2, 3, 4, 5, 255, 0, 128];

        for encoding in &["BASE64", "HEX", "URL"] {
            let encoded = BlobEncode
                .call(&[
                    VclValue::String(encoding.to_string()),
                    VclValue::Blob(original_data.clone()),
                ])
                .unwrap();

            let decoded = BlobDecode
                .call(&[VclValue::String(encoding.to_string()), encoded])
                .unwrap();

            if let VclValue::Blob(data) = decoded {
                assert_eq!(data, original_data, "roundtrip failed for {encoding}");
            } else {
                panic!("expected VBlob for {encoding}");
            }
        }
    }

    #[test]
    fn test_blob_module_factory() {
        let funcs = blob_module();
        let names: Vec<&str> = funcs.iter().map(|f| f.name()).collect();
        assert!(names.contains(&"encode"));
        assert!(names.contains(&"decode"));
    }
}

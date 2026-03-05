use serde::{Deserialize, Serialize};

/// SHA-256 digest used for cache hash lookup.
/// The digest length matches the Varnish DIGEST_LEN (32 bytes for SHA-256).
pub const DIGEST_LEN: usize = 32;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Digest {
    pub bytes: [u8; DIGEST_LEN],
}

impl Digest {
    pub const ZERO: Self = Self {
        bytes: [0u8; DIGEST_LEN],
    };

    pub fn new(bytes: [u8; DIGEST_LEN]) -> Self {
        Self { bytes }
    }

    pub fn from_slice(data: &[u8]) -> Option<Self> {
        if data.len() != DIGEST_LEN {
            return None;
        }
        let mut bytes = [0u8; DIGEST_LEN];
        bytes.copy_from_slice(data);
        Some(Self { bytes })
    }
}

impl std::fmt::Debug for Digest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Digest(")?;
        for b in &self.bytes[..8] {
            write!(f, "{b:02x}")?;
        }
        write!(f, "...)")
    }
}

impl std::fmt::Display for Digest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for b in &self.bytes {
            write!(f, "{b:02x}")?;
        }
        Ok(())
    }
}

//! Admin CLI authentication using challenge-response with SHA-256.
//!
//! When a shared secret is configured, the admin server authenticates each
//! incoming connection before accepting any commands. The scheme works as
//! follows:
//!
//! 1. The server generates a random 32-byte challenge and sends it to the
//!    client as a hex-encoded string.
//! 2. The client computes `SHA256(challenge + secret + challenge)` and sends
//!    the result back as a hex-encoded string.
//! 3. The server verifies the response by computing the same hash locally.
//!
//! This mirrors the authentication scheme used by the Varnish CLI protocol,
//! ensuring that the shared secret is never transmitted over the wire.

use sha2::{Digest, Sha256};

/// Generate a random 32-byte challenge for the authentication handshake.
///
/// Uses the `getrandom` crate which delegates to the OS-provided CSPRNG
/// (e.g., `/dev/urandom` on Unix, `BCryptGenRandom` on Windows).
pub fn generate_challenge() -> [u8; 32] {
    let mut buf = [0u8; 32];
    getrandom::getrandom(&mut buf).expect("failed to generate random challenge bytes");
    buf
}

/// Compute the authentication response for a given challenge and secret.
///
/// The response is `SHA256(challenge_bytes + secret_bytes + challenge_bytes)`
/// encoded as a lowercase hex string (64 characters).
pub fn compute_auth_response(challenge: &[u8], secret: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(challenge);
    hasher.update(secret);
    hasher.update(challenge);
    let result = hasher.finalize();
    hex_encode(&result)
}

/// Verify that a client's authentication response matches the expected value.
///
/// Returns `true` if the `response` string (hex-encoded SHA-256 hash) matches
/// the hash computed from the challenge and secret.
pub fn verify_auth(challenge: &[u8], secret: &[u8], response: &str) -> bool {
    let expected = compute_auth_response(challenge, secret);
    // Use constant-time comparison to prevent timing side-channels.
    constant_time_eq(expected.as_bytes(), response.trim().as_bytes())
}

/// Encode a byte slice as a lowercase hex string.
fn hex_encode(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

/// Constant-time byte comparison to prevent timing attacks.
///
/// Returns `true` only if both slices have the same length and identical
/// contents. The comparison always examines every byte regardless of
/// where a mismatch occurs.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generate_challenge_returns_32_bytes() {
        let challenge = generate_challenge();
        assert_eq!(challenge.len(), 32);
    }

    #[test]
    fn generate_challenge_is_not_all_zeros() {
        let challenge = generate_challenge();
        // It is astronomically unlikely that 32 random bytes are all zero.
        assert!(challenge.iter().any(|&b| b != 0));
    }

    #[test]
    fn generate_challenge_produces_different_values() {
        let c1 = generate_challenge();
        let c2 = generate_challenge();
        assert_ne!(c1, c2);
    }

    #[test]
    fn compute_auth_response_is_hex() {
        let challenge = b"0123456789abcdef0123456789abcdef";
        let secret = b"my_secret";
        let response = compute_auth_response(challenge, secret);

        // SHA-256 produces 32 bytes = 64 hex characters.
        assert_eq!(response.len(), 64);
        assert!(response.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn compute_auth_response_is_deterministic() {
        let challenge = b"test_challenge__________________";
        let secret = b"test_secret";
        let r1 = compute_auth_response(challenge, secret);
        let r2 = compute_auth_response(challenge, secret);
        assert_eq!(r1, r2);
    }

    #[test]
    fn compute_auth_response_varies_with_challenge() {
        let secret = b"same_secret";
        let r1 = compute_auth_response(b"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", secret);
        let r2 = compute_auth_response(b"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb", secret);
        assert_ne!(r1, r2);
    }

    #[test]
    fn compute_auth_response_varies_with_secret() {
        let challenge = b"same_challenge__________________";
        let r1 = compute_auth_response(challenge, b"secret_one");
        let r2 = compute_auth_response(challenge, b"secret_two");
        assert_ne!(r1, r2);
    }

    #[test]
    fn verify_auth_correct_response() {
        let challenge = generate_challenge();
        let secret = b"hunter2";
        let response = compute_auth_response(&challenge, secret);
        assert!(verify_auth(&challenge, secret, &response));
    }

    #[test]
    fn verify_auth_wrong_response() {
        let challenge = generate_challenge();
        let secret = b"correct_secret";
        let wrong = compute_auth_response(&challenge, b"wrong_secret");
        assert!(!verify_auth(&challenge, secret, &wrong));
    }

    #[test]
    fn verify_auth_empty_response() {
        let challenge = generate_challenge();
        let secret = b"some_secret";
        assert!(!verify_auth(&challenge, secret, ""));
    }

    #[test]
    fn verify_auth_garbage_response() {
        let challenge = generate_challenge();
        let secret = b"some_secret";
        assert!(!verify_auth(&challenge, secret, "not_a_valid_hex_hash_at_all"));
    }

    #[test]
    fn verify_auth_trims_whitespace() {
        let challenge = generate_challenge();
        let secret = b"my_secret";
        let response = compute_auth_response(&challenge, secret);
        let padded = format!("  {response}  \n");
        assert!(verify_auth(&challenge, secret, &padded));
    }

    #[test]
    fn hex_encode_correctness() {
        assert_eq!(hex_encode(&[0x00]), "00");
        assert_eq!(hex_encode(&[0xff]), "ff");
        assert_eq!(hex_encode(&[0xde, 0xad, 0xbe, 0xef]), "deadbeef");
        assert_eq!(hex_encode(&[]), "");
    }

    #[test]
    fn constant_time_eq_equal_slices() {
        assert!(constant_time_eq(b"hello", b"hello"));
        assert!(constant_time_eq(b"", b""));
    }

    #[test]
    fn constant_time_eq_different_slices() {
        assert!(!constant_time_eq(b"hello", b"world"));
        assert!(!constant_time_eq(b"hello", b"hell"));
        assert!(!constant_time_eq(b"he", b"hello"));
    }
}

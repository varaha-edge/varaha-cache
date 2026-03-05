use std::fs::File;
use std::io::BufReader;
use std::path::PathBuf;
use std::sync::Arc;

use rustls::ServerConfig;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use tokio_rustls::TlsAcceptor;

use crate::error::TransportError;

/// TLS configuration pointing to PEM-encoded certificate and key files.
#[derive(Debug, Clone)]
pub struct TlsConfig {
    /// Path to the PEM-encoded certificate chain file.
    pub cert_path: PathBuf,
    /// Path to the PEM-encoded private key file.
    pub key_path: PathBuf,
}

/// Load PEM-encoded certificates from a file.
fn load_certs(path: &PathBuf) -> Result<Vec<CertificateDer<'static>>, TransportError> {
    let file = File::open(path).map_err(|e| {
        TransportError::Tls(format!(
            "failed to open cert file {}: {}",
            path.display(),
            e
        ))
    })?;
    let mut reader = BufReader::new(file);
    let certs: Vec<CertificateDer<'static>> = rustls_pemfile::certs(&mut reader)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| {
            TransportError::Tls(format!(
                "failed to parse certs from {}: {}",
                path.display(),
                e
            ))
        })?;
    if certs.is_empty() {
        return Err(TransportError::Tls(format!(
            "no certificates found in {}",
            path.display()
        )));
    }
    Ok(certs)
}

/// Load a PEM-encoded private key from a file.
fn load_private_key(path: &PathBuf) -> Result<PrivateKeyDer<'static>, TransportError> {
    let file = File::open(path).map_err(|e| {
        TransportError::Tls(format!("failed to open key file {}: {}", path.display(), e))
    })?;
    let mut reader = BufReader::new(file);
    let key = rustls_pemfile::private_key(&mut reader)
        .map_err(|e| {
            TransportError::Tls(format!(
                "failed to parse private key from {}: {}",
                path.display(),
                e
            ))
        })?
        .ok_or_else(|| {
            TransportError::Tls(format!("no private key found in {}", path.display()))
        })?;
    Ok(key)
}

/// Build a `TlsAcceptor` from the given TLS configuration.
///
/// The acceptor is configured with ALPN protocols `h2` and `http/1.1`
/// so that clients can negotiate HTTP/2 over TLS.
pub fn build_tls_acceptor(config: &TlsConfig) -> Result<TlsAcceptor, TransportError> {
    let certs = load_certs(&config.cert_path)?;
    let key = load_private_key(&config.key_path)?;

    let mut server_config = ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .map_err(|e| TransportError::Tls(format!("failed to build ServerConfig: {}", e)))?;

    server_config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];

    Ok(TlsAcceptor::from(Arc::new(server_config)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ring::signature::KeyPair;

    #[test]
    fn missing_cert_file_returns_error() {
        let config = TlsConfig {
            cert_path: PathBuf::from("/nonexistent/cert.pem"),
            key_path: PathBuf::from("/nonexistent/key.pem"),
        };
        let result = build_tls_acceptor(&config);
        match result {
            Err(e) => {
                let err = e.to_string();
                assert!(
                    err.contains("failed to open cert file"),
                    "unexpected error: {}",
                    err
                );
            }
            Ok(_) => panic!("expected error, got Ok"),
        }
    }

    #[test]
    fn missing_key_file_returns_error() {
        let dir = std::env::temp_dir().join("rv-transport-test-missing-key");
        let _ = std::fs::create_dir_all(&dir);
        let cert_path = dir.join("cert.pem");

        // Write non-PEM content so load_certs returns "no certificates found".
        std::fs::write(&cert_path, b"not a pem file").unwrap();

        let config = TlsConfig {
            cert_path,
            key_path: PathBuf::from("/nonexistent/key.pem"),
        };
        let result = build_tls_acceptor(&config);
        match result {
            Err(e) => {
                let err = e.to_string();
                // Either "no certificates found" or "failed to open key file" depending on parse
                assert!(
                    err.contains("no certificates found")
                        || err.contains("failed to open key file"),
                    "unexpected error: {}",
                    err
                );
            }
            Ok(_) => panic!("expected error, got Ok"),
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn empty_cert_file_returns_error() {
        let dir = std::env::temp_dir().join("rv-transport-test-empty-cert");
        let _ = std::fs::create_dir_all(&dir);
        let cert_path = dir.join("cert.pem");
        let key_path = dir.join("key.pem");

        // Write empty files.
        std::fs::write(&cert_path, b"").unwrap();
        std::fs::write(&key_path, b"").unwrap();

        let config = TlsConfig {
            cert_path,
            key_path,
        };
        match build_tls_acceptor(&config) {
            Err(e) => {
                let err = e.to_string();
                assert!(
                    err.contains("no certificates found"),
                    "unexpected error: {}",
                    err
                );
            }
            Ok(_) => panic!("expected error, got Ok"),
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn no_private_key_in_file_returns_error() {
        let dir = std::env::temp_dir().join("rv-transport-test-no-key");
        let _ = std::fs::create_dir_all(&dir);
        let key_path = dir.join("key.pem");

        // Write a file that contains no PEM private key block.
        std::fs::write(&key_path, b"not a PEM key").unwrap();

        let result = load_private_key(&key_path);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("no private key found"),
            "unexpected error: {}",
            err
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn tls_config_debug_display() {
        let config = TlsConfig {
            cert_path: PathBuf::from("/etc/ssl/cert.pem"),
            key_path: PathBuf::from("/etc/ssl/key.pem"),
        };
        let debug = format!("{:?}", config);
        assert!(debug.contains("cert.pem"));
        assert!(debug.contains("key.pem"));
    }

    #[test]
    fn valid_cert_and_key_builds_acceptor() {
        let dir = std::env::temp_dir().join("rv-transport-test-valid-tls");
        let _ = std::fs::create_dir_all(&dir);
        let cert_path = dir.join("cert.pem");
        let key_path = dir.join("key.pem");

        // Generate ECDSA P-256 key pair via ring.
        let rng = ring::rand::SystemRandom::new();
        let pkcs8_doc = ring::signature::EcdsaKeyPair::generate_pkcs8(
            &ring::signature::ECDSA_P256_SHA256_ASN1_SIGNING,
            &rng,
        )
        .expect("generate pkcs8");

        // Write key as PEM.
        let key_pem = pem_encode("PRIVATE KEY", pkcs8_doc.as_ref());
        std::fs::write(&key_path, key_pem.as_bytes()).unwrap();

        // Build a minimal self-signed X.509 certificate DER.
        let key_pair = ring::signature::EcdsaKeyPair::from_pkcs8(
            &ring::signature::ECDSA_P256_SHA256_ASN1_SIGNING,
            pkcs8_doc.as_ref(),
            &rng,
        )
        .expect("parse key pair");

        let cert_der = build_self_signed_cert_der(&key_pair, &rng);
        let cert_pem = pem_encode("CERTIFICATE", &cert_der);
        std::fs::write(&cert_path, cert_pem.as_bytes()).unwrap();

        let config = TlsConfig {
            cert_path,
            key_path,
        };
        let result = build_tls_acceptor(&config);
        assert!(result.is_ok(), "expected Ok, got: {:?}", result.err());

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// PEM-encode a DER blob with the given label.
    fn pem_encode(label: &str, der: &[u8]) -> String {
        use base64::Engine;
        let b64 = base64::engine::general_purpose::STANDARD.encode(der);
        let mut pem = format!("-----BEGIN {}-----\n", label);
        for chunk in b64.as_bytes().chunks(64) {
            pem.push_str(std::str::from_utf8(chunk).unwrap());
            pem.push('\n');
        }
        pem.push_str(&format!("-----END {}-----\n", label));
        pem
    }

    /// Build a minimal self-signed X.509v3 certificate in DER encoding.
    ///
    /// This constructs raw ASN.1 DER by hand to avoid needing the rcgen
    /// dependency just for tests. The certificate is valid for "localhost"
    /// with a 2-year validity window.
    fn build_self_signed_cert_der(
        key_pair: &ring::signature::EcdsaKeyPair,
        rng: &ring::rand::SystemRandom,
    ) -> Vec<u8> {
        fn der_len(len: usize) -> Vec<u8> {
            if len < 0x80 {
                vec![len as u8]
            } else if len < 0x100 {
                vec![0x81, len as u8]
            } else {
                vec![0x82, (len >> 8) as u8, (len & 0xff) as u8]
            }
        }

        fn der_seq(contents: &[u8]) -> Vec<u8> {
            let mut v = vec![0x30];
            v.extend(der_len(contents.len()));
            v.extend(contents);
            v
        }

        fn der_set(contents: &[u8]) -> Vec<u8> {
            let mut v = vec![0x31];
            v.extend(der_len(contents.len()));
            v.extend(contents);
            v
        }

        fn der_int(val: &[u8]) -> Vec<u8> {
            let mut v = vec![0x02];
            if !val.is_empty() && val[0] & 0x80 != 0 {
                v.extend(der_len(val.len() + 1));
                v.push(0x00);
            } else {
                v.extend(der_len(val.len()));
            }
            v.extend(val);
            v
        }

        fn der_oid(oid_bytes: &[u8]) -> Vec<u8> {
            let mut v = vec![0x06];
            v.extend(der_len(oid_bytes.len()));
            v.extend(oid_bytes);
            v
        }

        fn der_utf8(s: &str) -> Vec<u8> {
            let mut v = vec![0x0c];
            v.extend(der_len(s.len()));
            v.extend(s.as_bytes());
            v
        }

        fn der_utctime(s: &str) -> Vec<u8> {
            let mut v = vec![0x17];
            v.extend(der_len(s.len()));
            v.extend(s.as_bytes());
            v
        }

        fn der_bitstring(bits: &[u8]) -> Vec<u8> {
            let mut v = vec![0x03];
            v.extend(der_len(bits.len() + 1));
            v.push(0x00); // no unused bits
            v.extend(bits);
            v
        }

        fn der_explicit_tag(tag: u8, contents: &[u8]) -> Vec<u8> {
            let mut v = vec![0xa0 | tag];
            v.extend(der_len(contents.len()));
            v.extend(contents);
            v
        }

        // OID for ecdsaWithSHA256: 1.2.840.10045.4.3.2
        let oid_ecdsa_sha256: &[u8] = &[0x2a, 0x86, 0x48, 0xce, 0x3d, 0x04, 0x03, 0x02];
        // OID for id-ecPublicKey: 1.2.840.10045.2.1
        let oid_ec_public_key: &[u8] = &[0x2a, 0x86, 0x48, 0xce, 0x3d, 0x02, 0x01];
        // OID for prime256v1 (P-256): 1.2.840.10045.3.1.7
        let oid_prime256v1: &[u8] = &[0x2a, 0x86, 0x48, 0xce, 0x3d, 0x03, 0x01, 0x07];
        // OID for commonName: 2.5.4.3
        let oid_cn: &[u8] = &[0x55, 0x04, 0x03];

        // Signature algorithm: SEQUENCE { OID ecdsaWithSHA256 }
        let sig_alg = der_seq(&der_oid(oid_ecdsa_sha256));

        // Issuer = Subject: CN=localhost
        let cn_attr = der_seq(&[der_oid(oid_cn), der_utf8("localhost")].concat());
        let rdn = der_set(&cn_attr);
        let name = der_seq(&rdn);

        // Serial number
        let serial = der_int(&[0x01]);

        // Validity: 2025-01-01 to 2027-01-01
        let validity =
            der_seq(&[der_utctime("250101000000Z"), der_utctime("270101000000Z")].concat());

        // Subject Public Key Info
        let pub_key_bytes = key_pair.public_key().as_ref();
        let spki_algo = der_seq(&[der_oid(oid_ec_public_key), der_oid(oid_prime256v1)].concat());
        let spki = der_seq(&[spki_algo, der_bitstring(pub_key_bytes)].concat());

        // Version: v3 (integer 2), explicit tag [0]
        let version = der_explicit_tag(0, &der_int(&[0x02]));

        // TBSCertificate
        let tbs_contents = [
            version,
            serial,
            sig_alg.clone(),
            name.clone(), // issuer
            validity,
            name, // subject (self-signed)
            spki,
        ]
        .concat();
        let tbs = der_seq(&tbs_contents);

        // Sign the TBSCertificate
        let signature = key_pair.sign(rng, &tbs).expect("sign TBS");
        let sig_bits = der_bitstring(signature.as_ref());

        // Certificate = SEQUENCE { tbs, sigAlg, signature }
        der_seq(&[tbs, sig_alg, sig_bits].concat())
    }
}

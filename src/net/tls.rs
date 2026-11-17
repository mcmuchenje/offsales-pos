//! TLS for the LAN link.
//!
//! The server makes its own certificate on first start. There is no public
//! authority on a shop LAN, so instead of checking names a till "pins" the
//! server's certificate fingerprint when it pairs, and afterwards talks only
//! to a server presenting exactly that certificate.

use std::sync::{Arc, Mutex};

use anyhow::{anyhow, Context, Result};
use hmac::{Hmac, Mac};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{ring, CryptoProvider};
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName, UnixTime};
use rustls::{DigitallySignedStruct, SignatureScheme};
use sha2::{Digest, Sha256};

pub fn provider() -> Arc<CryptoProvider> {
    Arc::new(ring::default_provider())
}

/// A new self-signed certificate and key, both PEM.
pub fn generate_identity() -> Result<(String, String)> {
    let ck = rcgen::generate_simple_self_signed(vec!["offsales-pos.local".to_string()]).context("creating the server certificate")?;
    Ok((ck.cert.pem(), ck.key_pair.serialize_pem()))
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
}

pub fn fingerprint_of_pem(cert_pem: &str) -> Result<String> {
    let der = CertificateDer::from_pem_slice(cert_pem.as_bytes()).map_err(|e| anyhow!("bad certificate: {e}"))?;
    Ok(sha256_hex(der.as_ref()))
}

/// First 16 hex digits, grouped for reading aloud: "3F9A-01BC-77D2-E410".
pub fn short_fingerprint(fp: &str) -> String {
    fp.chars().take(16).collect::<Vec<_>>().chunks(4).map(|c| c.iter().collect::<String>().to_uppercase()).collect::<Vec<_>>().join("-")
}

pub fn server_config(cert_pem: &str, key_pem: &str) -> Result<rustls::ServerConfig> {
    let cert = CertificateDer::from_pem_slice(cert_pem.as_bytes()).map_err(|e| anyhow!("bad certificate: {e}"))?;
    let key = PrivateKeyDer::from_pem_slice(key_pem.as_bytes()).map_err(|e| anyhow!("bad key: {e}"))?;
    let mut cfg = rustls::ServerConfig::builder_with_provider(provider())
        .with_safe_default_protocol_versions()?
        .with_no_client_auth()
        .with_single_cert(vec![cert], key)?;
    cfg.alpn_protocols = vec![b"http/1.1".to_vec()];
    Ok(cfg)
}

/// Accepts only the pinned certificate. With no pin (pairing), accepts any
/// certificate but records its fingerprint so pairing can prove it.
#[derive(Debug)]
pub struct PinnedVerifier {
    pinned: Option<String>,
    seen: Mutex<Option<String>>,
    provider: Arc<CryptoProvider>,
}

impl PinnedVerifier {
    pub fn new(pinned: Option<String>) -> Arc<Self> {
        Arc::new(PinnedVerifier { pinned, seen: Mutex::new(None), provider: provider() })
    }
    pub fn seen(&self) -> Option<String> {
        self.seen.lock().unwrap().clone()
    }
}

impl ServerCertVerifier for PinnedVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        let fp = sha256_hex(end_entity.as_ref());
        *self.seen.lock().unwrap() = Some(fp.clone());
        match &self.pinned {
            Some(p) if *p != fp => Err(rustls::Error::General(
                "This isn't the server this till was paired with. Pair the till again if the server was replaced.".into(),
            )),
            _ => Ok(ServerCertVerified::assertion()),
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(message, cert, dss, &self.provider.signature_verification_algorithms)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(message, cert, dss, &self.provider.signature_verification_algorithms)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider.signature_verification_algorithms.supported_schemes()
    }
}

pub fn client_config(verifier: Arc<PinnedVerifier>) -> Result<rustls::ClientConfig> {
    let mut cfg = rustls::ClientConfig::builder_with_provider(provider())
        .with_safe_default_protocol_versions()?
        .dangerous()
        .with_custom_certificate_verifier(verifier)
        .with_no_client_auth();
    cfg.alpn_protocols = vec![b"http/1.1".to_vec()];
    Ok(cfg)
}

/// HMAC-SHA256 over the parts, keyed by the pairing code. Hex.
pub fn proof(code: &str, parts: &[&str]) -> String {
    let mut mac = Hmac::<Sha256>::new_from_slice(code.as_bytes()).expect("any key length");
    for p in parts {
        mac.update(p.as_bytes());
        mac.update(b"|");
    }
    mac.finalize().into_bytes().iter().map(|b| format!("{b:02x}")).collect()
}

/// Constant-time comparison for proofs.
pub fn same(a: &str, b: &str) -> bool {
    a.len() == b.len() && a.bytes().zip(b.bytes()).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

pub fn random_hex(bytes: usize) -> String {
    use rand_core::RngCore;
    let mut buf = vec![0u8; bytes];
    rand_core::OsRng.fill_bytes(&mut buf);
    buf.iter().map(|b| format!("{b:02x}")).collect()
}

/// 8-digit pairing code.
pub fn pairing_code() -> String {
    use rand_core::RngCore;
    format!("{:08}", rand_core::OsRng.next_u32() % 100_000_000)
}

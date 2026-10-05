use std::sync::Arc;

use quinn::crypto::rustls::{NoInitialCipherSuite, QuicClientConfig};
use rcgen::{CertificateParams, KeyPair, PKCS_ED25519};
use rustls::{
    ClientConfig, DigitallySignedStruct, SignatureScheme,
    client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier},
    crypto::{
        CryptoProvider, ring::default_provider, verify_tls12_signature,
        verify_tls13_signature,
    },
    pki_types::{CertificateDer, PrivateKeyDer, ServerName, UnixTime},
    version::TLS13,
};
use thiserror::Error;

pub const TPU_ALPN: &[u8] = b"solana-tpu";

#[derive(Debug, Error)]
pub enum TlsError {
    #[error("failed to build the client certificate: {0}")]
    Certificate(#[from] rcgen::Error),
    #[error("failed to build the tls client configuration: {0}")]
    Rustls(#[from] rustls::Error),
    #[error("tls client configuration is unusable for quic: {0}")]
    Quic(#[from] NoInitialCipherSuite),
}

pub fn client_config() -> Result<quinn::ClientConfig, TlsError> {
    let provider = Arc::new(default_provider());
    let (certificate, key) = self_signed_ed25519()?;
    let mut tls = ClientConfig::builder_with_provider(Arc::clone(&provider))
        .with_protocol_versions(&[&TLS13])?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(AnyServerCertificate(provider)))
        .with_client_auth_cert(vec![certificate], key)?;
    tls.alpn_protocols = vec![TPU_ALPN.to_vec()];
    Ok(quinn::ClientConfig::new(Arc::new(
        QuicClientConfig::try_from(tls)?,
    )))
}

fn self_signed_ed25519()
-> Result<(CertificateDer<'static>, PrivateKeyDer<'static>), TlsError> {
    let key_pair = KeyPair::generate_for(&PKCS_ED25519)?;
    let certificate = CertificateParams::default().self_signed(&key_pair)?;
    Ok((certificate.der().clone(), PrivateKeyDer::from(key_pair)))
}

#[derive(Debug)]
struct AnyServerCertificate(Arc<CryptoProvider>);

impl ServerCertVerifier for AnyServerCertificate {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        certificate: &CertificateDer<'_>,
        signature: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls12_signature(
            message,
            certificate,
            signature,
            &self.0.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        certificate: &CertificateDer<'_>,
        signature: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls13_signature(
            message,
            certificate,
            signature,
            &self.0.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ED25519_ALGORITHM_IDENTIFIER: [u8; 7] =
        [0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70];

    #[test]
    fn client_certificate_is_self_signed_ed25519() {
        let (certificate, _) = self_signed_ed25519().unwrap();

        let occurrences = certificate
            .as_ref()
            .windows(ED25519_ALGORITHM_IDENTIFIER.len())
            .filter(|window| *window == ED25519_ALGORITHM_IDENTIFIER)
            .count();

        assert_eq!(occurrences, 3);
    }

    #[test]
    fn client_key_is_an_ed25519_key_the_ring_provider_can_sign_with() {
        let (_, key) = self_signed_ed25519().unwrap();

        let signing_key = default_provider()
            .key_provider
            .load_private_key(key)
            .unwrap();

        assert_eq!(signing_key.algorithm(), rustls::SignatureAlgorithm::ED25519);
    }

    #[test]
    fn quic_client_configuration_builds_with_the_ring_provider() {
        assert!(client_config().is_ok());
    }

    #[test]
    fn verifier_accepts_a_certificate_it_cannot_chain() {
        let verifier = AnyServerCertificate(Arc::new(default_provider()));
        let (certificate, _) = self_signed_ed25519().unwrap();
        let name = ServerName::try_from("192.0.2.1").unwrap();

        let verdict =
            verifier.verify_server_cert(&certificate, &[], &name, &[], UnixTime::now());

        assert!(verdict.is_ok());
    }

    #[test]
    fn verifier_offers_ed25519() {
        let verifier = AnyServerCertificate(Arc::new(default_provider()));

        assert!(
            verifier
                .supported_verify_schemes()
                .contains(&SignatureScheme::ED25519)
        );
    }
}

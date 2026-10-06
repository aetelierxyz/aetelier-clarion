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

const DER_SEQUENCE: u8 = 0x30;
const DER_EXPLICIT_VERSION: u8 = 0xa0;
const TBS_FIELDS_BEFORE_SUBJECT_PUBLIC_KEY_INFO: usize = 5;
const ED25519_SPKI_HEADER: [u8; 10] =
    [0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x03, 0x21, 0x00];
const ED25519_KEY_LEN: usize = 32;

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

pub fn peer_pubkey(connection: &quinn::Connection) -> Option<String> {
    let chain = connection
        .peer_identity()?
        .downcast::<Vec<CertificateDer<'static>>>()
        .ok()?;
    end_entity_pubkey(&chain)
}

pub fn end_entity_pubkey(chain: &[CertificateDer<'_>]) -> Option<String> {
    let end_entity = chain.first()?;
    ed25519_public_key(end_entity.as_ref()).map(|key| bs58::encode(key).into_string())
}

pub fn ed25519_public_key(certificate: &[u8]) -> Option<[u8; ED25519_KEY_LEN]> {
    let (certificate, _) = der_sequence(certificate)?;
    let (tbs, _) = der_sequence(certificate)?;
    let (tag, _, after_version) = der_element(tbs)?;
    let mut fields = if tag == DER_EXPLICIT_VERSION {
        after_version
    } else {
        tbs
    };
    for _ in 0..TBS_FIELDS_BEFORE_SUBJECT_PUBLIC_KEY_INFO {
        let (_, _, rest) = der_element(fields)?;
        fields = rest;
    }
    let (spki, _) = der_sequence(fields)?;
    spki.strip_prefix(ED25519_SPKI_HEADER.as_slice())?
        .try_into()
        .ok()
}

fn der_sequence(input: &[u8]) -> Option<(&[u8], &[u8])> {
    let (tag, content, rest) = der_element(input)?;
    (tag == DER_SEQUENCE).then_some((content, rest))
}

fn der_element(input: &[u8]) -> Option<(u8, &[u8], &[u8])> {
    let (&tag, rest) = input.split_first()?;
    let (&first, rest) = rest.split_first()?;
    let (length, rest) = if first < 0x80 {
        (usize::from(first), rest)
    } else {
        let count = usize::from(first & 0x7f);
        if count == 0 || count > 4 {
            return None;
        }
        let (bytes, rest) = rest.split_at_checked(count)?;
        let length = bytes.iter().try_fold(0_usize, |length, byte| {
            length.checked_mul(256)?.checked_add(usize::from(*byte))
        })?;
        (length, rest)
    };
    let (content, rest) = rest.split_at_checked(length)?;
    Some((tag, content, rest))
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

    #[test]
    fn ed25519_key_is_read_from_the_end_entity_certificate() {
        let key_pair = KeyPair::generate_for(&PKCS_ED25519).unwrap();
        let certificate = CertificateParams::default().self_signed(&key_pair).unwrap();

        let key = ed25519_public_key(certificate.der()).unwrap();

        assert_eq!(key.as_slice(), key_pair.public_key_raw());
    }

    #[test]
    fn proven_key_comes_from_the_end_entity_ahead_of_its_issuer() {
        let leaf_key = KeyPair::generate_for(&PKCS_ED25519).unwrap();
        let issuer_key = KeyPair::generate_for(&PKCS_ED25519).unwrap();
        let leaf = CertificateParams::default().self_signed(&leaf_key).unwrap();
        let issuer = CertificateParams::default()
            .self_signed(&issuer_key)
            .unwrap();

        let key = end_entity_pubkey(&[leaf.der().clone(), issuer.der().clone()]);

        assert_eq!(
            key,
            Some(bs58::encode(leaf_key.public_key_raw()).into_string())
        );
    }

    fn der_length(length: usize) -> Vec<u8> {
        if length < 0x80 {
            return vec![u8::try_from(length).unwrap()];
        }
        let bytes: Vec<u8> = length
            .to_be_bytes()
            .into_iter()
            .skip_while(|byte| *byte == 0)
            .collect();
        [vec![0x80 | u8::try_from(bytes.len()).unwrap()], bytes].concat()
    }

    fn der_sequence_of(content: &[u8]) -> Vec<u8> {
        [
            vec![DER_SEQUENCE],
            der_length(content.len()),
            content.to_vec(),
        ]
        .concat()
    }

    #[test]
    fn ed25519_key_is_read_from_a_version_one_certificate() {
        let key_pair = KeyPair::generate_for(&PKCS_ED25519).unwrap();
        let certificate = CertificateParams::default().self_signed(&key_pair).unwrap();
        let (outer, _) = der_sequence(certificate.der()).unwrap();
        let (tbs, after_tbs) = der_sequence(outer).unwrap();
        let version_one_tbs = tbs
            .strip_prefix([DER_EXPLICIT_VERSION, 0x03, 0x02, 0x01, 0x02].as_slice())
            .unwrap();

        let version_one = der_sequence_of(
            &[der_sequence_of(version_one_tbs), after_tbs.to_vec()].concat(),
        );

        assert!(der_sequence(&version_one).unwrap().1.is_empty());
        assert_eq!(
            ed25519_public_key(&version_one).map(|key| key.to_vec()),
            Some(key_pair.public_key_raw().to_vec())
        );
    }

    #[test]
    fn certificate_with_an_ecdsa_key_yields_no_ed25519_key() {
        let key_pair = KeyPair::generate_for(&rcgen::PKCS_ECDSA_P256_SHA256).unwrap();
        let certificate = CertificateParams::default().self_signed(&key_pair).unwrap();

        assert_eq!(ed25519_public_key(certificate.der()), None);
    }

    #[test]
    fn malformed_certificates_yield_no_key() {
        let (certificate, _) = self_signed_ed25519().unwrap();
        let bytes = certificate.as_ref();

        assert_eq!(ed25519_public_key(&[]), None);
        assert_eq!(
            ed25519_public_key(&[0x30, 0x84, 0xff, 0xff, 0xff, 0xff]),
            None
        );
        assert_eq!(ed25519_public_key(&[0x30, 0x80]), None);
        assert_eq!(ed25519_public_key(&bytes[..bytes.len() / 2]), None);
        for cut in 1..bytes.len() {
            assert_eq!(ed25519_public_key(&bytes[..cut]), None, "{cut}");
        }
    }

    #[test]
    fn long_form_lengths_are_decoded() {
        let mut long = vec![0x04, 0x82, 0x01, 0x00];
        long.extend([9_u8; 256]);
        long.push(0xff);

        let (tag, content, rest) = der_element(&long).unwrap();

        assert_eq!(tag, 0x04);
        assert_eq!(content.len(), 256);
        assert_eq!(rest, [0xff]);
    }
}

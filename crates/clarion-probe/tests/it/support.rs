use std::{
    io,
    net::{Ipv4Addr, Ipv6Addr, SocketAddr, UdpSocket},
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use clarion_probe::{config::ProbeConfig, targets::Target};
use quinn::{Endpoint, crypto::rustls::QuicServerConfig};
use rcgen::{CertificateParams, KeyPair, PKCS_ED25519};
use rustls::{
    DigitallySignedStruct, DistinguishedName, ServerConfig, SignatureScheme,
    client::danger::HandshakeSignatureValid,
    crypto::{
        CryptoProvider, ring::default_provider, verify_tls12_signature,
        verify_tls13_signature,
    },
    pki_types::{CertificateDer, PrivateKeyDer, UnixTime},
    server::danger::{ClientCertVerified, ClientCertVerifier},
    version::TLS13,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::mpsc,
};

pub const VANTAGE: &str = "test-vantage";
pub const STAMP: &str = "20260921T141320Z";
pub const HEADER: &str = "sampled_ts_us,cluster,vantage,pubkey,tpu_quic,gossip_ip,attempt,ok,\
                          handshake_us,rtt_us,err\n";

pub fn fixture(name: &str) -> Vec<u8> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name);
    std::fs::read(path).unwrap()
}

pub fn scratch_dir(name: &str) -> PathBuf {
    let directory = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join("clarion-probe")
        .join(name);
    let _ = std::fs::remove_dir_all(&directory);
    directory
}

pub fn probe_config(
    rounds: u8,
    concurrency: u16,
    connect_timeout: Duration,
) -> ProbeConfig {
    ProbeConfig {
        rpc_url: String::new(),
        vantage: VANTAGE.to_owned(),
        rounds,
        round_spacing: Duration::from_millis(10),
        concurrency,
        connect_timeout,
        output_dir: PathBuf::new(),
    }
}

pub fn target(pubkey: &str, tpu_quic: SocketAddr) -> Target {
    Target {
        pubkey: pubkey.to_owned(),
        tpu_quic,
        gossip_ip: Some(tpu_quic.ip()),
    }
}

pub fn loopback() -> SocketAddr {
    SocketAddr::from((Ipv4Addr::LOCALHOST, 0))
}

pub fn loopback_v6() -> SocketAddr {
    SocketAddr::from((Ipv6Addr::LOCALHOST, 0))
}

pub struct SilentPort {
    pub address: SocketAddr,
    _socket: UdpSocket,
}

impl SilentPort {
    pub fn bind() -> Self {
        let socket = UdpSocket::bind(loopback()).unwrap();
        Self {
            address: socket.local_addr().unwrap(),
            _socket: socket,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub enum Admission {
    Accept,
    Refuse,
}

pub struct TpuServer {
    pub address: SocketAddr,
    pub client_certificate_counts: mpsc::UnboundedReceiver<usize>,
}

impl TpuServer {
    pub fn spawn(alpn: &[u8], admission: Admission) -> Self {
        Self::spawn_at(loopback(), alpn, admission, || {}).unwrap()
    }

    pub fn spawn_at(
        address: SocketAddr,
        alpn: &[u8],
        admission: Admission,
        mut on_incoming: impl FnMut() + Send + 'static,
    ) -> io::Result<Self> {
        let endpoint = Endpoint::server(server_config(alpn), address)?;
        let address = endpoint.local_addr()?;
        let (counts, client_certificate_counts) = mpsc::unbounded_channel();
        tokio::spawn(async move {
            while let Some(incoming) = endpoint.accept().await {
                on_incoming();
                match admission {
                    Admission::Refuse => incoming.refuse(),
                    Admission::Accept => {
                        let counts = counts.clone();
                        tokio::spawn(async move {
                            if let Ok(connection) = incoming.await {
                                let presented = connection
                                    .peer_identity()
                                    .and_then(|identity| {
                                        identity
                                            .downcast::<Vec<CertificateDer<'static>>>()
                                            .ok()
                                    })
                                    .map_or(0, |certificates| certificates.len());
                                let _ = counts.send(presented);
                                connection.closed().await;
                            }
                        });
                    }
                }
            }
        });
        Ok(Self {
            address,
            client_certificate_counts,
        })
    }
}

fn server_config(alpn: &[u8]) -> quinn::ServerConfig {
    let provider = Arc::new(default_provider());
    let key_pair = KeyPair::generate_for(&PKCS_ED25519).unwrap();
    let certificate = CertificateParams::default().self_signed(&key_pair).unwrap();
    let mut tls = ServerConfig::builder_with_provider(Arc::clone(&provider))
        .with_protocol_versions(&[&TLS13])
        .unwrap()
        .with_client_cert_verifier(Arc::new(AnyClientCertificate(provider)))
        .with_single_cert(
            vec![certificate.der().clone()],
            PrivateKeyDer::from(key_pair),
        )
        .unwrap();
    tls.alpn_protocols = vec![alpn.to_vec()];
    quinn::ServerConfig::with_crypto(Arc::new(QuicServerConfig::try_from(tls).unwrap()))
}

#[derive(Debug)]
struct AnyClientCertificate(Arc<CryptoProvider>);

impl ClientCertVerifier for AnyClientCertificate {
    fn root_hint_subjects(&self) -> &[DistinguishedName] {
        &[]
    }

    fn verify_client_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _now: UnixTime,
    ) -> Result<ClientCertVerified, rustls::Error> {
        Ok(ClientCertVerified::assertion())
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

pub struct RpcReply {
    pub status: u16,
    pub body: String,
}

impl RpcReply {
    pub fn ok(body: impl Into<String>) -> Self {
        Self {
            status: 200,
            body: body.into(),
        }
    }
}

pub struct CannedRpc {
    pub url: String,
    pub requests: mpsc::UnboundedReceiver<serde_json::Value>,
}

impl CannedRpc {
    pub async fn spawn(reply_to: fn(&str) -> RpcReply) -> Self {
        let listener = TcpListener::bind(loopback()).await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let (seen, requests) = mpsc::unbounded_channel();
        tokio::spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                let request: serde_json::Value =
                    serde_json::from_slice(&read_body(&mut stream).await).unwrap();
                let reply = reply_to(request["method"].as_str().unwrap());
                let _ = seen.send(request);
                let response = format!(
                    "HTTP/1.1 {} Canned\r\ncontent-type: application/json\r\n\
                     content-length: {}\r\nconnection: close\r\n\r\n{}",
                    reply.status,
                    reply.body.len(),
                    reply.body
                );
                stream.write_all(response.as_bytes()).await.unwrap();
                stream.shutdown().await.unwrap();
            }
        });
        Self { url, requests }
    }
}

async fn read_body(stream: &mut TcpStream) -> Vec<u8> {
    let mut request = Vec::new();
    loop {
        let mut chunk = [0u8; 4096];
        let read = stream.read(&mut chunk).await.unwrap();
        assert!(read > 0, "request ended before its body was complete");
        request.extend_from_slice(&chunk[..read]);
        if let Some(body) = complete_body(&request) {
            return body;
        }
    }
}

fn complete_body(request: &[u8]) -> Option<Vec<u8>> {
    let body_start = request
        .windows(4)
        .position(|window| window == b"\r\n\r\n")?
        + 4;
    let headers = std::str::from_utf8(&request[..body_start]).unwrap();
    let length: usize = headers.lines().find_map(|line| {
        let (name, value) = line.split_once(':')?;
        name.eq_ignore_ascii_case("content-length")
            .then(|| value.trim().parse().unwrap())
    })?;
    let body = &request[body_start..];
    (body.len() >= length).then(|| body[..length].to_vec())
}

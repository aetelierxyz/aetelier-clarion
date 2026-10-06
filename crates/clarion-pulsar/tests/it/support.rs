use std::{
    collections::VecDeque,
    io,
    net::{Ipv4Addr, Ipv6Addr, SocketAddr, UdpSocket},
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use clarion_pulsar::{
    config::PulsarConfig,
    output::CsvFile,
    rpc::{CallPolicy, RpcClient},
    targets::Target,
};
use quinn::{Endpoint, crypto::rustls::QuicServerConfig};
use rcgen::{
    CertificateParams, KeyPair, PKCS_ECDSA_P256_SHA256, PKCS_ED25519, SignatureAlgorithm,
};
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
                          handshake_us,peer_pubkey,err\n";
pub const DEVNET_HASH: &str = "EtWTRABZaYq6iMfeYKouRu166VU2xqa1wcaWoxPkrZBG";

pub fn fixture(name: &str) -> Vec<u8> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name);
    std::fs::read(path).unwrap()
}

pub fn fixture_text(name: &str) -> String {
    String::from_utf8(fixture(name)).unwrap()
}

pub fn scratch_dir(name: &str) -> PathBuf {
    let directory = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join("clarion-pulsar")
        .join(name);
    let _ = std::fs::remove_dir_all(&directory);
    directory
}

pub fn pulsar_config(
    rounds: u8,
    concurrency: u16,
    connect_timeout: Duration,
) -> PulsarConfig {
    PulsarConfig {
        rpc_url: String::new(),
        vantage: VANTAGE.to_owned(),
        rounds,
        round_spacing: Duration::from_millis(10),
        concurrency,
        connect_timeout,
        max_starts_per_second: 100,
        output_dir: PathBuf::new(),
    }
}

pub fn handshake_file(name: &str) -> CsvFile {
    CsvFile::create(&scratch_dir(name), STAMP).unwrap()
}

pub fn fast_policy() -> CallPolicy {
    CallPolicy {
        attempts: 5,
        first_backoff: Duration::from_millis(10),
        retry_after_cap: Duration::from_secs(30),
        request_timeout: Duration::from_secs(5),
        max_body_bytes: clarion_pulsar::rpc::MAX_BODY_BYTES,
    }
}

pub fn client(url: &str, policy: CallPolicy) -> RpcClient {
    RpcClient::with_policy(url, policy, fastrand::Rng::with_seed(5)).unwrap()
}

pub fn target(pubkey: &str, tpu_quic: SocketAddr) -> Target {
    Target {
        pubkey: pubkey.to_owned(),
        tpu_quic,
        gossip_ip: Some(tpu_quic.ip()),
    }
}

pub fn synthetic_key(seed: u8) -> String {
    bs58::encode([seed; 32]).into_string()
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
        Self::bind_at(loopback()).unwrap()
    }

    pub fn bind_at(address: SocketAddr) -> io::Result<Self> {
        let socket = UdpSocket::bind(address)?;
        Ok(Self {
            address: socket.local_addr()?,
            _socket: socket,
        })
    }
}

#[derive(Debug, Clone, Copy)]
pub enum Admission {
    Accept,
    Refuse,
}

#[derive(Debug, Clone, Copy)]
pub enum ServerKey {
    Ed25519,
    EcdsaP256,
}

pub struct TpuServer {
    pub address: SocketAddr,
    pub public_key: String,
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
        on_incoming: impl FnMut() + Send + 'static,
    ) -> io::Result<Self> {
        Self::spawn_with(address, alpn, admission, ServerKey::Ed25519, on_incoming)
    }

    pub fn spawn_with(
        address: SocketAddr,
        alpn: &[u8],
        admission: Admission,
        key: ServerKey,
        mut on_incoming: impl FnMut() + Send + 'static,
    ) -> io::Result<Self> {
        let (config, public_key) = server_config(alpn, key);
        let endpoint = Endpoint::server(config, address)?;
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
            public_key,
            client_certificate_counts,
        })
    }
}

fn server_config(alpn: &[u8], key: ServerKey) -> (quinn::ServerConfig, String) {
    let algorithm: &SignatureAlgorithm = match key {
        ServerKey::Ed25519 => &PKCS_ED25519,
        ServerKey::EcdsaP256 => &PKCS_ECDSA_P256_SHA256,
    };
    let provider = Arc::new(default_provider());
    let key_pair = KeyPair::generate_for(algorithm).unwrap();
    let public_key = bs58::encode(key_pair.public_key_raw()).into_string();
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
    let config = quinn::ServerConfig::with_crypto(Arc::new(
        QuicServerConfig::try_from(tls).unwrap(),
    ));
    (config, public_key)
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Framing {
    Sized,
    Announced(u64),
    UntilClose,
}

#[derive(Debug, Clone)]
pub struct RpcReply {
    pub status: u16,
    pub body: String,
    pub headers: Vec<(String, String)>,
    pub framing: Framing,
}

impl RpcReply {
    pub fn ok(body: impl Into<String>) -> Self {
        Self::status(200, body)
    }

    pub fn status(status: u16, body: impl Into<String>) -> Self {
        Self {
            status,
            body: body.into(),
            headers: Vec::new(),
            framing: Framing::Sized,
        }
    }

    pub fn result(result: &serde_json::Value) -> Self {
        Self::ok(
            serde_json::json!({ "jsonrpc": "2.0", "result": result, "id": 1 })
                .to_string(),
        )
    }

    pub fn remote_error(code: i64, message: &str) -> Self {
        Self::ok(
            serde_json::json!({
                "jsonrpc": "2.0",
                "error": { "code": code, "message": message },
                "id": 1
            })
            .to_string(),
        )
    }

    pub fn header(mut self, name: &str, value: &str) -> Self {
        self.headers.push((name.to_owned(), value.to_owned()));
        self
    }

    pub fn framed(mut self, framing: Framing) -> Self {
        self.framing = framing;
        self
    }
}

#[derive(Debug, Clone)]
pub struct CannedRequest {
    pub user_agent: Option<String>,
    pub body: serde_json::Value,
}

impl CannedRequest {
    pub fn method(&self) -> &str {
        self.body["method"].as_str().unwrap()
    }
}

pub struct CannedRpc {
    pub url: String,
    pub requests: mpsc::UnboundedReceiver<CannedRequest>,
}

impl CannedRpc {
    pub async fn spawn(
        mut reply_to: impl FnMut(&CannedRequest) -> RpcReply + Send + 'static,
    ) -> Self {
        let listener = TcpListener::bind(loopback()).await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let (seen, requests) = mpsc::unbounded_channel();
        tokio::spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                let (headers, body) = read_request(&mut stream).await;
                let request = CannedRequest {
                    user_agent: header_value(&headers, "user-agent"),
                    body: serde_json::from_slice(&body).unwrap(),
                };
                let reply = reply_to(&request);
                let _ = seen.send(request);
                write_reply(&mut stream, &reply).await;
            }
        });
        Self { url, requests }
    }

    pub async fn by_method(reply_to: fn(&str) -> RpcReply) -> Self {
        Self::spawn(move |request| reply_to(request.method())).await
    }

    pub async fn scripted(replies: Vec<RpcReply>) -> Self {
        let mut replies = VecDeque::from(replies);
        Self::spawn(move |_| {
            replies
                .pop_front()
                .unwrap_or_else(|| RpcReply::status(500, "script exhausted"))
        })
        .await
    }

    pub fn methods(&mut self) -> Vec<String> {
        let mut methods = Vec::new();
        while let Ok(request) = self.requests.try_recv() {
            methods.push(request.method().to_owned());
        }
        methods
    }
}

async fn write_reply(stream: &mut TcpStream, reply: &RpcReply) {
    let mut head = format!(
        "HTTP/1.1 {} Canned\r\ncontent-type: application/json\r\n",
        reply.status
    );
    match reply.framing {
        Framing::Sized => {
            head.push_str(&format!("content-length: {}\r\n", reply.body.len()))
        }
        Framing::Announced(length) => {
            head.push_str(&format!("content-length: {length}\r\n"))
        }
        Framing::UntilClose => {}
    }
    for (name, value) in &reply.headers {
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    head.push_str("connection: close\r\n\r\n");
    let _ = stream.write_all(head.as_bytes()).await;
    let _ = stream.write_all(reply.body.as_bytes()).await;
    let _ = stream.shutdown().await;
}

pub struct HangingHttp {
    pub url: String,
}

impl HangingHttp {
    pub async fn spawn() -> Self {
        let listener = TcpListener::bind(loopback()).await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move {
            let mut held = Vec::new();
            while let Ok((stream, _)) = listener.accept().await {
                held.push(stream);
            }
        });
        Self { url }
    }
}

pub async fn closed_port() -> SocketAddr {
    let listener = TcpListener::bind(loopback()).await.unwrap();
    listener.local_addr().unwrap()
}

async fn read_request(stream: &mut TcpStream) -> (String, Vec<u8>) {
    let mut request = Vec::new();
    loop {
        let mut chunk = [0u8; 4096];
        let read = stream.read(&mut chunk).await.unwrap();
        assert!(read > 0, "request ended before its body was complete");
        request.extend_from_slice(&chunk[..read]);
        if let Some(complete) = complete_request(&request) {
            return complete;
        }
    }
}

fn complete_request(request: &[u8]) -> Option<(String, Vec<u8>)> {
    let body_start = request
        .windows(4)
        .position(|window| window == b"\r\n\r\n")?
        + 4;
    let headers = std::str::from_utf8(&request[..body_start])
        .unwrap()
        .to_owned();
    let length: usize = header_value(&headers, "content-length")?.parse().unwrap();
    let body = &request[body_start..];
    (body.len() >= length).then(|| (headers, body[..length].to_vec()))
}

fn header_value(headers: &str, wanted: &str) -> Option<String> {
    headers.lines().find_map(|line| {
        let (name, value) = line.split_once(':')?;
        name.eq_ignore_ascii_case(wanted)
            .then(|| value.trim().to_owned())
    })
}

pub struct Cluster {
    pub nodes: serde_json::Value,
    pub vote_accounts: serde_json::Value,
    pub block_production: serde_json::Value,
    pub leader_schedule: serde_json::Value,
    pub epoch: u64,
}

impl Cluster {
    pub fn with_tpu(addresses: &[(String, SocketAddr)]) -> Self {
        let nodes = addresses
            .iter()
            .map(|(pubkey, tpu_quic)| {
                serde_json::json!({
                    "pubkey": pubkey,
                    "gossip": "192.0.2.10:8001",
                    "tpuQuic": tpu_quic.to_string(),
                    "version": "3.1.13"
                })
            })
            .collect();
        let first = addresses
            .first()
            .map(|(pubkey, _)| pubkey.clone())
            .unwrap_or_default();
        Self {
            nodes: serde_json::Value::Array(nodes),
            vote_accounts: serde_json::json!({
                "current": [{
                    "votePubkey": synthetic_key(200),
                    "nodePubkey": first,
                    "activatedStake": 42,
                    "epochVoteAccount": true,
                    "commission": 5,
                    "lastVote": 1000,
                    "rootSlot": 968,
                    "epochCredits": [[7, 20, 10]]
                }],
                "delinquent": []
            }),
            block_production: serde_json::json!({
                "context": { "slot": 1000 },
                "value": { "byIdentity": {}, "range": { "firstSlot": 800, "lastSlot": 1000 } }
            }),
            leader_schedule: serde_json::json!({ first: [0, 1, 2] }),
            epoch: 7,
        }
    }

    pub fn reply(&self, method: &str) -> RpcReply {
        match method {
            "getGenesisHash" => RpcReply::result(&serde_json::json!(DEVNET_HASH)),
            "getEpochInfo" => RpcReply::result(&serde_json::json!({
                "absoluteSlot": 1000,
                "blockHeight": 990,
                "epoch": self.epoch,
                "slotIndex": 200,
                "slotsInEpoch": 432000,
                "transactionCount": 1
            })),
            "getClusterNodes" => RpcReply::result(&self.nodes),
            "getVoteAccounts" => RpcReply::result(&self.vote_accounts),
            "getBlockProduction" => RpcReply::result(&self.block_production),
            "getLeaderSchedule" => RpcReply::result(&self.leader_schedule),
            _ => RpcReply::remote_error(-32601, "Method not found"),
        }
    }

    pub async fn serve(self) -> CannedRpc {
        CannedRpc::spawn(move |request| self.reply(request.method())).await
    }
}

use std::{
    collections::HashMap,
    io,
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use quinn::{ConnectionError, Endpoint, TransportErrorCode, VarInt};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::{
    task::{Id, JoinError, JoinSet},
    time,
};

use crate::{
    config::PulsarConfig,
    output::{CsvFile, OutputError},
    schedule::{Next, Scheduler, round_order, start_interval},
    targets::{Target, distinct_addresses},
    tls::{self, TlsError},
};

const CRYPTO_ERROR_CODES: std::ops::Range<u64> = 0x100..0x200;
const IPV4_ANY: SocketAddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 0);
const IPV6_ANY: SocketAddr = SocketAddr::new(IpAddr::V6(Ipv6Addr::UNSPECIFIED), 0);

#[derive(Debug, Error)]
pub enum PulsarError {
    #[error(transparent)]
    Tls(#[from] TlsError),
    #[error("failed to bind the quic endpoint: {0}")]
    Bind(#[from] io::Error),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HandshakeFailure {
    Timeout,
    Refused,
    Tls,
    ClosedByPeer,
    Other,
}

impl From<&ConnectionError> for HandshakeFailure {
    fn from(error: &ConnectionError) -> Self {
        match error {
            ConnectionError::TimedOut => Self::Timeout,
            ConnectionError::ConnectionClosed(close) => {
                Self::from_transport_code(close.error_code, Self::ClosedByPeer)
            }
            ConnectionError::TransportError(error) => {
                Self::from_transport_code(error.code, Self::Other)
            }
            ConnectionError::ApplicationClosed(_) | ConnectionError::Reset => {
                Self::ClosedByPeer
            }
            ConnectionError::VersionMismatch
            | ConnectionError::LocallyClosed
            | ConnectionError::CidsExhausted => Self::Other,
        }
    }
}

impl HandshakeFailure {
    fn from_transport_code(code: TransportErrorCode, otherwise: Self) -> Self {
        if code == TransportErrorCode::CONNECTION_REFUSED {
            Self::Refused
        } else if CRYPTO_ERROR_CODES.contains(&u64::from(code)) {
            Self::Tls
        } else {
            otherwise
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PulsarSample {
    pub sampled_ts_us: u64,
    pub cluster: String,
    pub vantage: String,
    pub pubkey: String,
    pub tpu_quic: SocketAddr,
    pub gossip_ip: Option<IpAddr>,
    pub attempt: u8,
    pub ok: bool,
    pub handshake_us: u64,
    pub peer_pubkey: Option<String>,
    pub err: Option<HandshakeFailure>,
}

#[derive(Debug, Clone)]
struct Handshake {
    handshake_us: u64,
    peer_pubkey: Option<String>,
}

#[derive(Debug, Clone)]
struct Measurement {
    sampled_ts_us: u64,
    outcome: Result<Handshake, HandshakeFailure>,
}

impl Measurement {
    fn failed(failure: HandshakeFailure) -> Self {
        Self {
            sampled_ts_us: unix_micros(SystemTime::now()),
            outcome: Err(failure),
        }
    }
}

#[derive(Debug)]
pub struct Pulsar {
    endpoint: Endpoint,
    endpoint_v6: Option<Endpoint>,
    vantage: String,
    rounds: u8,
    round_spacing: Duration,
    concurrency: u16,
    start_interval: Duration,
    connect_timeout: Duration,
    seed: u64,
}

impl Pulsar {
    pub fn new(config: &PulsarConfig) -> Result<Self, PulsarError> {
        Self::bound(config, IPV4_ANY, IPV6_ANY)
    }

    pub fn with_seed(self, seed: u64) -> Self {
        Self { seed, ..self }
    }

    fn bound(
        config: &PulsarConfig,
        ipv4: SocketAddr,
        ipv6: SocketAddr,
    ) -> Result<Self, PulsarError> {
        let mut endpoint = Endpoint::client(ipv4)?;
        let client_config = tls::client_config()?;
        endpoint.set_default_client_config(client_config.clone());
        let endpoint_v6 = Endpoint::client(ipv6).ok().map(|mut endpoint| {
            endpoint.set_default_client_config(client_config);
            endpoint
        });
        Ok(Self {
            endpoint,
            endpoint_v6,
            vantage: config.vantage.clone(),
            rounds: config.rounds,
            round_spacing: config.round_spacing,
            concurrency: config.concurrency,
            start_interval: start_interval(config.max_starts_per_second),
            connect_timeout: config.connect_timeout,
            seed: fastrand::u64(..),
        })
    }

    pub async fn run(
        &self,
        targets: &[Target],
        cluster: &str,
        output: &mut CsvFile,
    ) -> Result<Vec<PulsarSample>, OutputError> {
        let mut rng = fastrand::Rng::with_seed(self.seed);
        let mut samples = Vec::new();
        for attempt in 0..self.rounds {
            if attempt > 0 {
                time::sleep(self.round_spacing).await;
            }
            let round = self.round(targets, cluster, attempt, &mut rng).await;
            output.append(&round)?;
            samples.extend(round);
        }
        Ok(samples)
    }

    pub async fn drain(&self) {
        self.endpoint.wait_idle().await;
        if let Some(endpoint) = &self.endpoint_v6 {
            endpoint.wait_idle().await;
        }
    }

    fn endpoint_for(&self, address: SocketAddr) -> Option<Endpoint> {
        match address {
            SocketAddr::V4(_) => Some(self.endpoint.clone()),
            SocketAddr::V6(_) => self.endpoint_v6.clone(),
        }
    }

    async fn round(
        &self,
        targets: &[Target],
        cluster: &str,
        attempt: u8,
        rng: &mut fastrand::Rng,
    ) -> Vec<PulsarSample> {
        let order = round_order(&distinct_addresses(targets), rng);
        let measurements = self.sweep(order).await;
        targets
            .iter()
            .map(|target| {
                let measurement = measurements
                    .get(&target.tpu_quic)
                    .cloned()
                    .unwrap_or_else(|| Measurement::failed(HandshakeFailure::Other));
                self.sample(target, cluster, attempt, measurement)
            })
            .collect()
    }

    async fn sweep(&self, order: Vec<SocketAddr>) -> HashMap<SocketAddr, Measurement> {
        let mut sweep = Sweep {
            scheduler: Scheduler::new(order, self.concurrency, self.start_interval),
            running: HashMap::new(),
            measurements: HashMap::new(),
        };
        let mut tasks = JoinSet::new();
        loop {
            match sweep.scheduler.next(Instant::now()) {
                Next::Start(address) => {
                    let task = tasks.spawn(measure(
                        self.endpoint_for(address),
                        address,
                        self.connect_timeout,
                    ));
                    sweep.running.insert(task.id(), address);
                }
                Next::WaitUntil(at) => {
                    tokio::select! {
                        biased;
                        Some(joined) = tasks.join_next_with_id() => sweep.settle(joined),
                        () = time::sleep_until(at.into()) => {}
                    }
                }
                Next::WaitForCompletion => match tasks.join_next_with_id().await {
                    Some(joined) => sweep.settle(joined),
                    None => break,
                },
                Next::Finished => break,
            }
        }
        sweep.measurements
    }

    fn sample(
        &self,
        target: &Target,
        cluster: &str,
        attempt: u8,
        measurement: Measurement,
    ) -> PulsarSample {
        let (handshake_us, peer_pubkey, err) = match measurement.outcome {
            Ok(handshake) => (handshake.handshake_us, handshake.peer_pubkey, None),
            Err(failure) => (0, None, Some(failure)),
        };
        PulsarSample {
            sampled_ts_us: measurement.sampled_ts_us,
            cluster: cluster.to_owned(),
            vantage: self.vantage.clone(),
            pubkey: target.pubkey.clone(),
            tpu_quic: target.tpu_quic,
            gossip_ip: target.gossip_ip,
            attempt,
            ok: err.is_none(),
            handshake_us,
            peer_pubkey,
            err,
        }
    }
}

struct Sweep {
    scheduler: Scheduler,
    running: HashMap<Id, SocketAddr>,
    measurements: HashMap<SocketAddr, Measurement>,
}

impl Sweep {
    fn settle(&mut self, joined: Result<(Id, Measurement), JoinError>) {
        let (id, measurement) = match joined {
            Ok(finished) => finished,
            Err(error) => (error.id(), Measurement::failed(HandshakeFailure::Other)),
        };
        if let Some(address) = self.running.remove(&id) {
            self.scheduler.complete(address);
            self.measurements.insert(address, measurement);
        }
    }
}

async fn measure(
    endpoint: Option<Endpoint>,
    address: SocketAddr,
    connect_timeout: Duration,
) -> Measurement {
    let Some(endpoint) = endpoint else {
        return Measurement::failed(HandshakeFailure::Other);
    };
    let sampled_ts_us = unix_micros(SystemTime::now());
    let started = Instant::now();
    let outcome = match endpoint.connect(address, &address.ip().to_string()) {
        Err(_) => Err(HandshakeFailure::Other),
        Ok(connecting) => match time::timeout(connect_timeout, connecting).await {
            Err(_) => Err(HandshakeFailure::Timeout),
            Ok(Err(error)) => Err(HandshakeFailure::from(&error)),
            Ok(Ok(connection)) => {
                let handshake_us = micros(started.elapsed());
                let peer_pubkey = tls::peer_pubkey(&connection);
                connection.close(VarInt::from_u32(0), &[]);
                Ok(Handshake {
                    handshake_us,
                    peer_pubkey,
                })
            }
        },
    };
    Measurement {
        sampled_ts_us,
        outcome,
    }
}

fn micros(duration: Duration) -> u64 {
    u64::try_from(duration.as_micros()).unwrap_or(u64::MAX)
}

fn unix_micros(at: SystemTime) -> u64 {
    at.duration_since(UNIX_EPOCH).map_or(0, micros)
}

#[cfg(test)]
mod tests {
    use quinn::{ApplicationClose, ConnectionClose};

    use super::*;

    const IPV4_TARGET: &str = "192.0.2.1:8009";
    const IPV6_TARGET: &str = "[2001:db8::1]:8009";

    fn config() -> PulsarConfig {
        PulsarConfig {
            rpc_url: String::new(),
            vantage: "unit".to_owned(),
            rounds: 1,
            round_spacing: Duration::ZERO,
            concurrency: 1,
            connect_timeout: Duration::from_secs(1),
            max_starts_per_second: 100,
            output_dir: std::path::PathBuf::new(),
        }
    }

    fn closed_by_peer(code: TransportErrorCode) -> ConnectionError {
        ConnectionError::ConnectionClosed(ConnectionClose {
            error_code: code,
            frame_type: None,
            reason: Default::default(),
        })
    }

    #[test]
    fn idle_timeout_is_classified_as_timeout() {
        assert_eq!(
            HandshakeFailure::from(&ConnectionError::TimedOut),
            HandshakeFailure::Timeout
        );
    }

    #[test]
    fn connection_refused_close_is_classified_as_refused() {
        let error = closed_by_peer(TransportErrorCode::CONNECTION_REFUSED);

        assert_eq!(HandshakeFailure::from(&error), HandshakeFailure::Refused);
    }

    #[test]
    fn peer_tls_alert_is_classified_as_tls() {
        let no_application_protocol = closed_by_peer(TransportErrorCode::crypto(120));

        assert_eq!(
            HandshakeFailure::from(&no_application_protocol),
            HandshakeFailure::Tls
        );
    }

    #[test]
    fn local_tls_failure_is_classified_as_tls() {
        let bad_certificate =
            ConnectionError::TransportError(TransportErrorCode::crypto(42).into());

        assert_eq!(
            HandshakeFailure::from(&bad_certificate),
            HandshakeFailure::Tls
        );
    }

    #[test]
    fn both_ends_of_the_crypto_code_range_are_classified_as_tls() {
        for alert in [u8::MIN, u8::MAX] {
            let error = closed_by_peer(TransportErrorCode::crypto(alert));

            assert_eq!(HandshakeFailure::from(&error), HandshakeFailure::Tls);
        }
    }

    #[test]
    fn other_peer_closes_are_classified_as_closed_by_peer() {
        let transport = closed_by_peer(TransportErrorCode::PROTOCOL_VIOLATION);
        let application = ConnectionError::ApplicationClosed(ApplicationClose {
            error_code: VarInt::from_u32(2),
            reason: Default::default(),
        });

        assert_eq!(
            HandshakeFailure::from(&transport),
            HandshakeFailure::ClosedByPeer
        );
        assert_eq!(
            HandshakeFailure::from(&application),
            HandshakeFailure::ClosedByPeer
        );
        assert_eq!(
            HandshakeFailure::from(&ConnectionError::Reset),
            HandshakeFailure::ClosedByPeer
        );
    }

    #[test]
    fn local_and_protocol_level_failures_are_classified_as_other() {
        let local_violation = ConnectionError::TransportError(
            TransportErrorCode::PROTOCOL_VIOLATION.into(),
        );

        for error in [
            local_violation,
            ConnectionError::VersionMismatch,
            ConnectionError::LocallyClosed,
            ConnectionError::CidsExhausted,
        ] {
            assert_eq!(HandshakeFailure::from(&error), HandshakeFailure::Other);
        }
    }

    #[tokio::test]
    async fn targets_are_dialled_from_the_endpoint_of_their_own_family() {
        let pulsar = Pulsar::new(&config()).unwrap();

        let ipv4 = pulsar.endpoint_for(IPV4_TARGET.parse().unwrap()).unwrap();
        let ipv6 = pulsar.endpoint_for(IPV6_TARGET.parse().unwrap());

        assert!(ipv4.local_addr().unwrap().is_ipv4());
        assert!(ipv6.is_none_or(|endpoint| endpoint.local_addr().unwrap().is_ipv6()));
    }

    #[tokio::test]
    async fn ipv6_bind_failure_leaves_ipv4_sampling_and_reports_ipv6_targets_as_other() {
        let taken = std::net::UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let unbindable = taken.local_addr().unwrap();
        let target = Target {
            pubkey: "ipv6".to_owned(),
            tpu_quic: IPV6_TARGET.parse().unwrap(),
            gossip_ip: None,
        };

        let pulsar = Pulsar::bound(&config(), IPV4_ANY, unbindable).unwrap();
        let samples = pulsar
            .round(&[target], "localnet", 0, &mut fastrand::Rng::with_seed(1))
            .await;

        assert!(pulsar.endpoint_for(IPV4_TARGET.parse().unwrap()).is_some());
        assert!(pulsar.endpoint_for(IPV6_TARGET.parse().unwrap()).is_none());
        assert_eq!(samples.len(), 1);
        assert!(!samples[0].ok);
        assert_eq!(samples[0].err, Some(HandshakeFailure::Other));
        assert_eq!(samples[0].peer_pubkey, None);
    }

    #[tokio::test]
    async fn start_rate_becomes_the_interval_between_starts() {
        let pulsar = Pulsar::new(&PulsarConfig {
            max_starts_per_second: 25,
            ..config()
        })
        .unwrap();

        assert_eq!(pulsar.start_interval, Duration::from_millis(40));
    }

    #[test]
    fn durations_convert_to_whole_microseconds() {
        assert_eq!(micros(Duration::from_nanos(1_999)), 1);
        assert_eq!(micros(Duration::from_millis(250)), 250_000);
    }

    #[test]
    fn durations_beyond_u64_microseconds_saturate() {
        assert_eq!(micros(Duration::MAX), u64::MAX);
    }

    #[test]
    fn wall_clock_converts_to_unix_microseconds() {
        let at = UNIX_EPOCH + Duration::from_micros(1_790_000_000_123_456);

        assert_eq!(unix_micros(at), 1_790_000_000_123_456);
    }

    #[test]
    fn wall_clock_before_the_epoch_reads_zero() {
        assert_eq!(unix_micros(UNIX_EPOCH - Duration::from_secs(1)), 0);
    }
}

use std::time::{Duration, Instant};

use clarion_probe::{
    output::{CsvFile, OutputError},
    probe::{HandshakeFailure, ProbeSample, Prober},
    tls::TPU_ALPN,
};
use tokio::sync::mpsc;

use crate::support::{
    Admission, HEADER, STAMP, SilentPort, TpuServer, VANTAGE, loopback, loopback_v6,
    probe_config, scratch_dir, target,
};

const GENEROUS_TIMEOUT: Duration = Duration::from_secs(10);
const SHORT_TIMEOUT: Duration = Duration::from_millis(200);

#[tokio::test]
async fn completed_handshake_yields_an_ok_sample() {
    let server = TpuServer::spawn(TPU_ALPN, Admission::Accept);
    let prober = Prober::new(&probe_config(1, 4, GENEROUS_TIMEOUT)).unwrap();

    let (_, samples) = prober
        .run(
            &[target("live", server.address)],
            "localnet",
            &scratch_dir("ok-sample"),
            STAMP,
        )
        .await
        .unwrap();

    assert_eq!(samples.len(), 1);
    let sample = &samples[0];
    assert!(sample.ok);
    assert!(sample.handshake_us > 0);
    assert!(sample.rtt_us > 0);
    assert_eq!(sample.err, None);
    assert_eq!(sample.attempt, 0);
    assert_eq!(sample.cluster, "localnet");
    assert_eq!(sample.vantage, VANTAGE);
    assert_eq!(sample.pubkey, "live");
    assert_eq!(sample.tpu_quic, server.address);
    assert_eq!(sample.gossip_ip, Some(server.address.ip()));
    assert!(sample.sampled_ts_us > 1_600_000_000_000_000);
}

#[tokio::test]
async fn server_that_demands_a_client_certificate_receives_exactly_one() {
    let mut server = TpuServer::spawn(TPU_ALPN, Admission::Accept);
    let prober = Prober::new(&probe_config(1, 4, GENEROUS_TIMEOUT)).unwrap();

    let (_, samples) = prober
        .run(
            &[target("live", server.address)],
            "localnet",
            &scratch_dir("client-certificate"),
            STAMP,
        )
        .await
        .unwrap();

    assert!(samples[0].ok);
    assert_eq!(server.client_certificate_counts.recv().await, Some(1));
}

#[tokio::test]
async fn silent_port_yields_a_timeout_sample() {
    let silent = SilentPort::bind();
    let prober = Prober::new(&probe_config(1, 4, SHORT_TIMEOUT)).unwrap();

    let (_, samples) = prober
        .run(
            &[target("dead", silent.address)],
            "localnet",
            &scratch_dir("silent-port"),
            STAMP,
        )
        .await
        .unwrap();

    assert_eq!(samples.len(), 1);
    let sample = &samples[0];
    assert!(!sample.ok);
    assert_eq!(sample.err, Some(HandshakeFailure::Timeout));
    assert_eq!(sample.handshake_us, 0);
    assert_eq!(sample.rtt_us, 0);
}

#[tokio::test]
async fn failed_target_does_not_abort_the_run_and_every_round_samples_every_target() {
    let server = TpuServer::spawn(TPU_ALPN, Admission::Accept);
    let silent = SilentPort::bind();
    let targets = [
        target("dead", silent.address),
        target("live", server.address),
    ];
    let prober = Prober::new(&probe_config(2, 4, Duration::from_secs(1))).unwrap();

    let (_, samples) = prober
        .run(&targets, "localnet", &scratch_dir("failed-target"), STAMP)
        .await
        .unwrap();

    let observed: Vec<(u8, &str, bool)> = samples
        .iter()
        .map(|sample| (sample.attempt, sample.pubkey.as_str(), sample.ok))
        .collect();
    assert_eq!(
        observed,
        [
            (0, "dead", false),
            (0, "live", true),
            (1, "dead", false),
            (1, "live", true)
        ]
    );
}

#[tokio::test]
async fn alpn_other_than_solana_tpu_is_reported_as_tls() {
    let server = TpuServer::spawn(b"h3", Admission::Accept);
    let prober = Prober::new(&probe_config(1, 4, GENEROUS_TIMEOUT)).unwrap();

    let (_, samples) = prober
        .run(
            &[target("other-alpn", server.address)],
            "localnet",
            &scratch_dir("other-alpn"),
            STAMP,
        )
        .await
        .unwrap();

    assert!(!samples[0].ok);
    assert_eq!(samples[0].err, Some(HandshakeFailure::Tls));
}

#[tokio::test]
async fn refusing_server_is_reported_as_refused() {
    let server = TpuServer::spawn(TPU_ALPN, Admission::Refuse);
    let prober = Prober::new(&probe_config(1, 4, GENEROUS_TIMEOUT)).unwrap();

    let (_, samples) = prober
        .run(
            &[target("refusing", server.address)],
            "localnet",
            &scratch_dir("refusing"),
            STAMP,
        )
        .await
        .unwrap();

    assert!(!samples[0].ok);
    assert_eq!(samples[0].err, Some(HandshakeFailure::Refused));
}

#[tokio::test]
async fn concurrency_of_one_runs_handshakes_one_after_another() {
    let ports = [SilentPort::bind(), SilentPort::bind(), SilentPort::bind()];
    let targets = ports.each_ref().map(|port| target("dead", port.address));
    let prober = Prober::new(&probe_config(1, 1, SHORT_TIMEOUT)).unwrap();

    let started = Instant::now();
    let (_, samples) = prober
        .run(&targets, "localnet", &scratch_dir("one-by-one"), STAMP)
        .await
        .unwrap();

    assert_eq!(samples.len(), 3);
    assert!(started.elapsed() >= SHORT_TIMEOUT * 3);
}

#[tokio::test]
async fn drain_returns_once_sampled_connections_are_closed() {
    let server = TpuServer::spawn(TPU_ALPN, Admission::Accept);
    let prober = Prober::new(&probe_config(1, 4, GENEROUS_TIMEOUT)).unwrap();
    prober
        .run(
            &[target("live", server.address)],
            "localnet",
            &scratch_dir("drain"),
            STAMP,
        )
        .await
        .unwrap();

    let drained = tokio::time::timeout(GENEROUS_TIMEOUT, prober.drain()).await;

    assert!(drained.is_ok());
}

fn rows(csv_text: &str) -> Vec<ProbeSample> {
    csv::Reader::from_reader(csv_text.as_bytes())
        .deserialize()
        .collect::<Result<_, _>>()
        .unwrap()
}

#[tokio::test]
async fn existing_file_fails_the_run_before_any_handshake() {
    let directory = scratch_dir("collision");
    let taken = CsvFile::create(&directory, STAMP).unwrap();
    let before = std::fs::read(taken.path()).unwrap();
    let (seen, mut arrivals) = mpsc::unbounded_channel();
    let server =
        TpuServer::spawn_at(loopback(), TPU_ALPN, Admission::Accept, move || {
            let _ = seen.send(());
        })
        .unwrap();
    let prober = Prober::new(&probe_config(1, 4, GENEROUS_TIMEOUT)).unwrap();

    let error = prober
        .run(
            &[target("live", server.address)],
            "localnet",
            &directory,
            STAMP,
        )
        .await
        .unwrap_err();

    assert!(matches!(
        error,
        OutputError::Create { path: reported, .. } if reported == taken.path()
    ));
    assert_eq!(arrivals.try_recv(), Err(mpsc::error::TryRecvError::Empty));
    assert_eq!(std::fs::read(taken.path()).unwrap(), before);
}

#[tokio::test]
async fn rows_of_a_finished_round_are_on_disk_before_the_next_round_starts() {
    let directory = scratch_dir("round-rows");
    let expected_path = directory.join("probe-20260921T141320Z.csv");
    let watched = expected_path.clone();
    let (seen, mut snapshots) = mpsc::unbounded_channel();
    let server =
        TpuServer::spawn_at(loopback(), TPU_ALPN, Admission::Accept, move || {
            let _ = seen.send(std::fs::read_to_string(&watched).ok());
        })
        .unwrap();
    let prober = Prober::new(&probe_config(3, 4, GENEROUS_TIMEOUT)).unwrap();

    let (path, samples) = prober
        .run(
            &[target("live", server.address)],
            "localnet",
            &directory,
            STAMP,
        )
        .await
        .unwrap();

    let mut on_disk_at_round_start = Vec::new();
    while let Ok(snapshot) = snapshots.try_recv() {
        on_disk_at_round_start.push(snapshot);
    }
    assert_eq!(path, expected_path);
    assert_eq!(samples.len(), 3);
    assert_eq!(on_disk_at_round_start.len(), 3);
    assert_eq!(on_disk_at_round_start[0].as_deref(), Some(HEADER));
    assert_eq!(
        on_disk_at_round_start[1].as_deref().map(rows),
        Some(samples[..1].to_vec())
    );
    assert_eq!(
        on_disk_at_round_start[2].as_deref().map(rows),
        Some(samples[..2].to_vec())
    );
    assert_eq!(rows(&std::fs::read_to_string(path).unwrap()), samples);
}

#[tokio::test]
async fn ipv4_and_ipv6_targets_complete_handshakes_in_one_run() {
    let Ok(server_v6) =
        TpuServer::spawn_at(loopback_v6(), TPU_ALPN, Admission::Accept, || {})
    else {
        return;
    };
    let server_v4 = TpuServer::spawn(TPU_ALPN, Admission::Accept);
    let targets = [
        target("live-v6", server_v6.address),
        target("live-v4", server_v4.address),
    ];
    let prober = Prober::new(&probe_config(1, 4, GENEROUS_TIMEOUT)).unwrap();

    let (_, samples) = prober
        .run(&targets, "localnet", &scratch_dir("both-families"), STAMP)
        .await
        .unwrap();

    assert!(server_v6.address.is_ipv6());
    assert!(server_v4.address.is_ipv4());
    let observed: Vec<_> = samples
        .iter()
        .map(|sample| (sample.tpu_quic, sample.ok, sample.err))
        .collect();
    assert_eq!(
        observed,
        [
            (server_v6.address, true, None),
            (server_v4.address, true, None)
        ]
    );
}

use std::{
    collections::HashSet,
    time::{Duration, Instant},
};

use clarion_pulsar::{
    config::PulsarConfig,
    pulsar::{HandshakeFailure, Pulsar, PulsarSample},
    tls::TPU_ALPN,
};
use tokio::sync::mpsc;

use crate::support::{
    Admission, HEADER, STAMP, ServerKey, SilentPort, TpuServer, VANTAGE, handshake_file,
    loopback, loopback_v6, pulsar_config, scratch_dir, target,
};

const GENEROUS_TIMEOUT: Duration = Duration::from_secs(10);
const SHORT_TIMEOUT: Duration = Duration::from_millis(300);

fn arrivals(
    address: std::net::SocketAddr,
    label: usize,
    seen: &mpsc::UnboundedSender<(usize, Instant)>,
) -> Option<TpuServer> {
    let seen = seen.clone();
    TpuServer::spawn_at(address, TPU_ALPN, Admission::Accept, move || {
        let _ = seen.send((label, Instant::now()));
    })
    .ok()
}

fn drain_arrivals(
    receiver: &mut mpsc::UnboundedReceiver<(usize, Instant)>,
) -> Vec<(usize, Instant)> {
    let mut seen = Vec::new();
    while let Ok(arrival) = receiver.try_recv() {
        seen.push(arrival);
    }
    seen
}

#[tokio::test]
async fn completed_handshake_yields_an_ok_sample_with_the_proven_key() {
    let server = TpuServer::spawn(TPU_ALPN, Admission::Accept);
    let pulsar = Pulsar::new(&pulsar_config(1, 4, GENEROUS_TIMEOUT)).unwrap();

    let samples = pulsar
        .run(
            &[target("live", server.address)],
            "localnet",
            &mut handshake_file("ok-sample"),
        )
        .await
        .unwrap();

    assert_eq!(samples.len(), 1);
    let sample = &samples[0];
    assert!(sample.ok);
    assert!(sample.handshake_us > 0);
    assert_eq!(
        sample.peer_pubkey.as_deref(),
        Some(server.public_key.as_str())
    );
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
async fn proven_key_differs_from_the_claimed_identity_when_the_server_holds_another_key()
{
    let server = TpuServer::spawn(TPU_ALPN, Admission::Accept);
    let claimed = crate::support::synthetic_key(9);
    let pulsar = Pulsar::new(&pulsar_config(1, 4, GENEROUS_TIMEOUT)).unwrap();

    let samples = pulsar
        .run(
            &[target(&claimed, server.address)],
            "localnet",
            &mut handshake_file("other-key"),
        )
        .await
        .unwrap();

    assert_eq!(samples[0].pubkey, claimed);
    assert_eq!(
        samples[0].peer_pubkey.as_deref(),
        Some(server.public_key.as_str())
    );
    assert_ne!(samples[0].peer_pubkey.as_deref(), Some(claimed.as_str()));
}

#[tokio::test]
async fn server_with_a_non_ed25519_key_completes_with_an_empty_peer_pubkey() {
    let server = TpuServer::spawn_with(
        loopback(),
        TPU_ALPN,
        Admission::Accept,
        ServerKey::EcdsaP256,
        || {},
    )
    .unwrap();
    let pulsar = Pulsar::new(&pulsar_config(1, 4, GENEROUS_TIMEOUT)).unwrap();

    let samples = pulsar
        .run(
            &[target("ecdsa", server.address)],
            "localnet",
            &mut handshake_file("ecdsa-key"),
        )
        .await
        .unwrap();

    assert!(samples[0].ok);
    assert_eq!(samples[0].peer_pubkey, None);
}

#[tokio::test]
async fn server_that_demands_a_client_certificate_receives_exactly_one() {
    let mut server = TpuServer::spawn(TPU_ALPN, Admission::Accept);
    let pulsar = Pulsar::new(&pulsar_config(1, 4, GENEROUS_TIMEOUT)).unwrap();

    let samples = pulsar
        .run(
            &[target("live", server.address)],
            "localnet",
            &mut handshake_file("client-certificate"),
        )
        .await
        .unwrap();

    assert!(samples[0].ok);
    assert_eq!(server.client_certificate_counts.recv().await, Some(1));
}

#[tokio::test]
async fn silent_port_yields_a_timeout_sample() {
    let silent = SilentPort::bind();
    let pulsar = Pulsar::new(&pulsar_config(1, 4, SHORT_TIMEOUT)).unwrap();

    let samples = pulsar
        .run(
            &[target("dead", silent.address)],
            "localnet",
            &mut handshake_file("silent-port"),
        )
        .await
        .unwrap();

    assert_eq!(samples.len(), 1);
    let sample = &samples[0];
    assert!(!sample.ok);
    assert_eq!(sample.err, Some(HandshakeFailure::Timeout));
    assert_eq!(sample.handshake_us, 0);
    assert_eq!(sample.peer_pubkey, None);
}

#[tokio::test]
async fn failed_target_does_not_abort_the_run_and_every_round_samples_every_target() {
    let server = TpuServer::spawn(TPU_ALPN, Admission::Accept);
    let silent = SilentPort::bind();
    let targets = [
        target("dead", silent.address),
        target("live", server.address),
    ];
    let pulsar = Pulsar::new(&pulsar_config(2, 4, SHORT_TIMEOUT)).unwrap();

    let samples = pulsar
        .run(&targets, "localnet", &mut handshake_file("failed-target"))
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
    let pulsar = Pulsar::new(&pulsar_config(1, 4, GENEROUS_TIMEOUT)).unwrap();

    let samples = pulsar
        .run(
            &[target("other-alpn", server.address)],
            "localnet",
            &mut handshake_file("other-alpn"),
        )
        .await
        .unwrap();

    assert!(!samples[0].ok);
    assert_eq!(samples[0].err, Some(HandshakeFailure::Tls));
    assert_eq!(samples[0].peer_pubkey, None);
}

#[tokio::test]
async fn refusing_server_is_reported_as_refused() {
    let server = TpuServer::spawn(TPU_ALPN, Admission::Refuse);
    let pulsar = Pulsar::new(&pulsar_config(1, 4, GENEROUS_TIMEOUT)).unwrap();

    let samples = pulsar
        .run(
            &[target("refusing", server.address)],
            "localnet",
            &mut handshake_file("refusing"),
        )
        .await
        .unwrap();

    assert!(!samples[0].ok);
    assert_eq!(samples[0].err, Some(HandshakeFailure::Refused));
}

#[tokio::test]
async fn identities_sharing_an_address_share_one_handshake_per_round() {
    let (seen, mut incoming) = mpsc::unbounded_channel();
    let server = arrivals(loopback(), 0, &seen).unwrap();
    let targets = [
        target("first", server.address),
        target("second", server.address),
        target("third", server.address),
    ];
    let pulsar = Pulsar::new(&pulsar_config(2, 4, GENEROUS_TIMEOUT)).unwrap();

    let samples = pulsar
        .run(&targets, "localnet", &mut handshake_file("shared-address"))
        .await
        .unwrap();

    assert_eq!(drain_arrivals(&mut incoming).len(), 2);
    let rows: Vec<(u8, &str)> = samples
        .iter()
        .map(|sample| (sample.attempt, sample.pubkey.as_str()))
        .collect();
    assert_eq!(
        rows,
        [
            (0, "first"),
            (0, "second"),
            (0, "third"),
            (1, "first"),
            (1, "second"),
            (1, "third")
        ]
    );
    for round in samples.chunks(3) {
        assert!(round.iter().all(|sample| sample.ok));
        assert!(
            round
                .iter()
                .all(|sample| sample.sampled_ts_us == round[0].sampled_ts_us
                    && sample.handshake_us == round[0].handshake_us
                    && sample.peer_pubkey == round[0].peer_pubkey)
        );
    }
    assert_ne!(samples[0].sampled_ts_us, samples[3].sampled_ts_us);
}

#[tokio::test]
async fn distinct_addresses_are_dialled_in_a_new_shuffled_order_every_round() {
    let (seen, mut incoming) = mpsc::unbounded_channel();
    let servers: Vec<TpuServer> = (0..6)
        .map(|label| arrivals(loopback(), label, &seen).unwrap())
        .collect();
    let targets: Vec<_> = servers
        .iter()
        .enumerate()
        .map(|(label, server)| target(&format!("node-{label}"), server.address))
        .collect();
    let pulsar = Pulsar::new(&pulsar_config(3, 64, GENEROUS_TIMEOUT))
        .unwrap()
        .with_seed(7);

    let samples = pulsar
        .run(&targets, "localnet", &mut handshake_file("shuffled"))
        .await
        .unwrap();

    let order: Vec<usize> = drain_arrivals(&mut incoming)
        .into_iter()
        .map(|(label, _)| label)
        .collect();
    let rounds: Vec<&[usize]> = order.chunks(6).collect();
    assert_eq!(samples.len(), 18);
    assert!(samples.iter().all(|sample| sample.ok));
    assert_eq!(rounds.len(), 3);
    for round in &rounds {
        let mut labels = round.to_vec();
        labels.sort_unstable();
        assert_eq!(labels, [0, 1, 2, 3, 4, 5]);
    }
    let distinct: HashSet<&[usize]> = rounds.iter().copied().collect();
    assert!(distinct.len() > 1, "{rounds:?}");
    assert!(
        rounds.iter().any(|round| *round != [0, 1, 2, 3, 4, 5]),
        "{rounds:?}"
    );
}

#[tokio::test]
async fn handshake_starts_are_spaced_by_the_start_rate() {
    let (seen, mut incoming) = mpsc::unbounded_channel();
    let Some(server_v6) = arrivals(loopback_v6(), 6, &seen) else {
        return;
    };
    let server_v4 = arrivals(loopback(), 4, &seen).unwrap();
    let targets = [
        target("v4", server_v4.address),
        target("v6", server_v6.address),
    ];
    let pulsar = Pulsar::new(&PulsarConfig {
        max_starts_per_second: 2,
        ..pulsar_config(1, 64, GENEROUS_TIMEOUT)
    })
    .unwrap();

    pulsar
        .run(&targets, "localnet", &mut handshake_file("start-rate"))
        .await
        .unwrap();

    let arrived = drain_arrivals(&mut incoming);
    assert_eq!(arrived.len(), 2);
    let gap = arrived[1].1 - arrived[0].1;
    assert!(gap >= Duration::from_millis(450), "{gap:?}");
}

#[tokio::test]
async fn one_handshake_at_a_time_per_destination_ip() {
    let ports = [SilentPort::bind(), SilentPort::bind()];
    let targets = ports.each_ref().map(|port| target("dead", port.address));
    let pulsar = Pulsar::new(&pulsar_config(1, 64, SHORT_TIMEOUT)).unwrap();

    let started = Instant::now();
    let samples = pulsar
        .run(&targets, "localnet", &mut handshake_file("one-per-ip"))
        .await
        .unwrap();

    assert_eq!(samples.len(), 2);
    assert!(started.elapsed() >= SHORT_TIMEOUT * 2);
}

#[tokio::test]
async fn different_ips_are_dialled_in_parallel() {
    let Ok(port_v6) = SilentPort::bind_at(loopback_v6()) else {
        return;
    };
    let port_v4 = SilentPort::bind();
    let targets = [
        target("dead-v4", port_v4.address),
        target("dead-v6", port_v6.address),
    ];
    let pulsar = Pulsar::new(&pulsar_config(1, 64, SHORT_TIMEOUT)).unwrap();

    let started = Instant::now();
    let samples = pulsar
        .run(&targets, "localnet", &mut handshake_file("parallel-ips"))
        .await
        .unwrap();

    assert_eq!(samples.len(), 2);
    assert!(started.elapsed() < SHORT_TIMEOUT * 2);
}

#[tokio::test]
async fn concurrency_of_one_runs_handshakes_one_after_another() {
    let Ok(port_v6) = SilentPort::bind_at(loopback_v6()) else {
        return;
    };
    let port_v4 = SilentPort::bind();
    let targets = [
        target("dead-v4", port_v4.address),
        target("dead-v6", port_v6.address),
    ];
    let pulsar = Pulsar::new(&pulsar_config(1, 1, SHORT_TIMEOUT)).unwrap();

    let started = Instant::now();
    let samples = pulsar
        .run(&targets, "localnet", &mut handshake_file("one-by-one"))
        .await
        .unwrap();

    assert_eq!(samples.len(), 2);
    assert!(started.elapsed() >= SHORT_TIMEOUT * 2);
}

#[tokio::test]
async fn next_round_starts_no_sooner_than_the_round_spacing_after_the_last_ended() {
    let (seen, mut incoming) = mpsc::unbounded_channel();
    let server = arrivals(loopback(), 0, &seen).unwrap();
    let spacing = Duration::from_millis(400);
    let pulsar = Pulsar::new(&PulsarConfig {
        round_spacing: spacing,
        ..pulsar_config(2, 4, GENEROUS_TIMEOUT)
    })
    .unwrap();

    pulsar
        .run(
            &[target("live", server.address)],
            "localnet",
            &mut handshake_file("round-gap"),
        )
        .await
        .unwrap();

    let arrived = drain_arrivals(&mut incoming);
    assert_eq!(arrived.len(), 2);
    assert!(arrived[1].1 - arrived[0].1 >= spacing);
}

#[tokio::test]
async fn drain_returns_once_sampled_connections_are_closed() {
    let server = TpuServer::spawn(TPU_ALPN, Admission::Accept);
    let pulsar = Pulsar::new(&pulsar_config(1, 4, GENEROUS_TIMEOUT)).unwrap();
    pulsar
        .run(
            &[target("live", server.address)],
            "localnet",
            &mut handshake_file("drain"),
        )
        .await
        .unwrap();

    let drained = tokio::time::timeout(GENEROUS_TIMEOUT, pulsar.drain()).await;

    assert!(drained.is_ok());
}

fn rows(csv_text: &str) -> Vec<PulsarSample> {
    csv::Reader::from_reader(csv_text.as_bytes())
        .deserialize()
        .collect::<Result<_, _>>()
        .unwrap()
}

#[tokio::test]
async fn rows_of_a_finished_round_are_on_disk_before_the_next_round_starts() {
    let directory = scratch_dir("round-rows");
    let expected_path = directory.join("clarion-pulsar-20260921T141320Z.csv");
    let watched = expected_path.clone();
    let (seen, mut snapshots) = mpsc::unbounded_channel();
    let server =
        TpuServer::spawn_at(loopback(), TPU_ALPN, Admission::Accept, move || {
            let _ = seen.send(std::fs::read_to_string(&watched).ok());
        })
        .unwrap();
    let pulsar = Pulsar::new(&pulsar_config(3, 4, GENEROUS_TIMEOUT)).unwrap();
    let mut file = clarion_pulsar::output::CsvFile::create(&directory, STAMP).unwrap();

    let samples = pulsar
        .run(&[target("live", server.address)], "localnet", &mut file)
        .await
        .unwrap();

    let mut on_disk_at_round_start = Vec::new();
    while let Ok(snapshot) = snapshots.try_recv() {
        on_disk_at_round_start.push(snapshot);
    }
    assert_eq!(file.path(), expected_path);
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
    assert_eq!(
        rows(&std::fs::read_to_string(expected_path).unwrap()),
        samples
    );
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
    let pulsar = Pulsar::new(&pulsar_config(1, 4, GENEROUS_TIMEOUT)).unwrap();

    let samples = pulsar
        .run(&targets, "localnet", &mut handshake_file("both-families"))
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

use std::{net::SocketAddr, time::Duration};

use clarion_pulsar::{
    output::CsvFile,
    pulsar::Pulsar,
    rpc::TESTNET_GENESIS_HASH,
    run::{Dropped, RunError, ScheduleSource, execute},
    validators::ValidatorRow,
};
use tokio::sync::mpsc;

use crate::support::{
    Admission, CannedRpc, Cluster, RpcReply, STAMP, TpuServer, client, fast_policy,
    loopback, pulsar_config, scratch_dir, synthetic_key,
};

const SECOND_STAMP: &str = "20260921T141420Z";
const ALL_METHODS: [&str; 6] = [
    "getGenesisHash",
    "getEpochInfo",
    "getClusterNodes",
    "getVoteAccounts",
    "getBlockProduction",
    "getLeaderSchedule",
];

fn pulsar() -> Pulsar {
    Pulsar::new(&pulsar_config(1, 4, Duration::from_secs(10))).unwrap()
}

fn watched_server(
    watched: Vec<std::path::PathBuf>,
) -> (TpuServer, mpsc::UnboundedReceiver<Vec<Option<String>>>) {
    let (seen, snapshots) = mpsc::unbounded_channel();
    let server = TpuServer::spawn_at(
        loopback(),
        clarion_pulsar::tls::TPU_ALPN,
        Admission::Accept,
        move || {
            let _ = seen.send(
                watched
                    .iter()
                    .map(|path| std::fs::read_to_string(path).ok())
                    .collect(),
            );
        },
    )
    .unwrap();
    (server, snapshots)
}

fn validator_rows(text: &str) -> Vec<ValidatorRow> {
    csv::Reader::from_reader(text.as_bytes())
        .deserialize()
        .collect::<Result<_, _>>()
        .unwrap()
}

fn shared(addresses: &[SocketAddr]) -> Vec<(String, SocketAddr)> {
    addresses
        .iter()
        .enumerate()
        .map(|(index, address)| {
            (synthetic_key(u8::try_from(index + 1).unwrap()), *address)
        })
        .collect()
}

#[tokio::test]
async fn validators_file_is_complete_on_disk_before_the_first_handshake() {
    let directory = scratch_dir("run-order");
    let validators_path =
        directory.join("clarion-pulsar-validators-20260921T141320Z.csv");
    let handshake_path = directory.join("clarion-pulsar-20260921T141320Z.csv");
    let (server, mut snapshots) =
        watched_server(vec![validators_path.clone(), handshake_path.clone()]);
    let mut rpc = Cluster::with_tpu(&shared(&[server.address, server.address]))
        .serve()
        .await;

    let report = execute(
        &client(&rpc.url, fast_policy()),
        &pulsar(),
        &directory,
        STAMP,
    )
    .await
    .unwrap();

    assert_eq!(rpc.methods(), ALL_METHODS);
    let at_first_handshake = snapshots.recv().await.unwrap();
    let final_validators = std::fs::read_to_string(&validators_path).unwrap();
    assert_eq!(
        at_first_handshake[0].as_deref(),
        Some(final_validators.as_str())
    );
    assert_eq!(validator_rows(&final_validators).len(), 2);
    assert_eq!(
        at_first_handshake[1].as_deref(),
        Some(crate::support::HEADER)
    );
    assert_eq!(report.validators_path, validators_path);
    assert_eq!(report.handshake_path, handshake_path);
    assert_eq!(report.validator_rows, 2);
    assert_eq!(report.targets, 2);
    assert_eq!(report.addresses, 1);
    assert_eq!(report.rows, 2);
    assert_eq!(report.ok_rows, 2);
    assert_eq!(report.cluster, "devnet");
    assert_eq!(report.epoch, 7);
    assert_eq!(report.schedule_source, ScheduleSource::Rpc);
}

#[tokio::test]
async fn second_run_in_the_same_directory_reads_the_leader_slot_cache() {
    let directory = scratch_dir("run-cache");
    let server = TpuServer::spawn(clarion_pulsar::tls::TPU_ALPN, Admission::Accept);
    let mut rpc = Cluster::with_tpu(&shared(&[server.address])).serve().await;
    let rpc_client = client(&rpc.url, fast_policy());

    let first = execute(&rpc_client, &pulsar(), &directory, STAMP)
        .await
        .unwrap();
    let first_methods = rpc.methods();
    let second = execute(&rpc_client, &pulsar(), &directory, SECOND_STAMP)
        .await
        .unwrap();
    let second_methods = rpc.methods();

    assert_eq!(first_methods, ALL_METHODS);
    assert_eq!(second_methods, ALL_METHODS[..5]);
    assert_eq!(first.schedule_source, ScheduleSource::Rpc);
    assert_eq!(second.schedule_source, ScheduleSource::Cache);
    assert_eq!(
        second.leader_slots_path,
        directory.join("clarion-pulsar-leader-slots-devnet-7.csv")
    );
    let rows = validator_rows(&std::fs::read_to_string(&second.validators_path).unwrap());
    assert_eq!(rows[0].scheduled_leader_slots, 3);
}

#[tokio::test]
async fn leader_schedule_is_requested_for_the_first_slot_of_the_epoch() {
    let directory = scratch_dir("run-schedule-slot");
    let server = TpuServer::spawn(clarion_pulsar::tls::TPU_ALPN, Admission::Accept);
    let mut rpc = Cluster::with_tpu(&shared(&[server.address])).serve().await;

    execute(
        &client(&rpc.url, fast_policy()),
        &pulsar(),
        &directory,
        STAMP,
    )
    .await
    .unwrap();

    let mut schedule_params = Vec::new();
    while let Ok(request) = rpc.requests.try_recv() {
        if request.method() == "getLeaderSchedule" {
            schedule_params.push(request.body["params"].clone());
        }
    }
    assert_eq!(schedule_params, [serde_json::json!([800])]);
}

#[tokio::test]
async fn cache_of_another_cluster_is_not_read() {
    let directory = scratch_dir("run-cache-cluster");
    std::fs::create_dir_all(&directory).unwrap();
    std::fs::write(
        directory.join("clarion-pulsar-leader-slots-devnet-7.csv"),
        "identity,leader_slots\n",
    )
    .unwrap();
    let server = TpuServer::spawn(clarion_pulsar::tls::TPU_ALPN, Admission::Accept);
    let cluster = Cluster::with_tpu(&shared(&[server.address]));
    let rpc = CannedRpc::spawn(move |request| match request.method() {
        "getGenesisHash" => RpcReply::result(&serde_json::json!(TESTNET_GENESIS_HASH)),
        method => cluster.reply(method),
    })
    .await;

    let report = execute(
        &client(&rpc.url, fast_policy()),
        &pulsar(),
        &directory,
        STAMP,
    )
    .await
    .unwrap();

    assert_eq!(report.cluster, "testnet");
    assert_eq!(report.schedule_source, ScheduleSource::Rpc);
    assert!(
        report
            .leader_slots_path
            .ends_with("clarion-pulsar-leader-slots-testnet-7.csv")
    );
}

#[tokio::test]
async fn unreadable_cache_is_kept_unchanged_and_reported_as_not_cached() {
    let directory = scratch_dir("run-cache-unreadable");
    std::fs::create_dir_all(&directory).unwrap();
    let cache = directory.join("clarion-pulsar-leader-slots-devnet-7.csv");
    std::fs::write(&cache, "identity,leader_slots\ngarbage,1\n").unwrap();
    let server = TpuServer::spawn(clarion_pulsar::tls::TPU_ALPN, Admission::Accept);
    let mut rpc = Cluster::with_tpu(&shared(&[server.address])).serve().await;

    let report = execute(
        &client(&rpc.url, fast_policy()),
        &pulsar(),
        &directory,
        STAMP,
    )
    .await
    .unwrap();

    assert_eq!(rpc.methods(), ALL_METHODS);
    assert_eq!(report.schedule_source, ScheduleSource::RpcNotCached);
    assert_eq!(report.leader_slots_path, cache);
    assert_eq!(
        std::fs::read_to_string(&cache).unwrap(),
        "identity,leader_slots\ngarbage,1\n"
    );
}

#[tokio::test]
async fn null_leader_schedule_leaves_counts_at_zero_and_writes_no_cache() {
    let directory = scratch_dir("run-null-schedule");
    let server = TpuServer::spawn(clarion_pulsar::tls::TPU_ALPN, Admission::Accept);
    let mut cluster = Cluster::with_tpu(&shared(&[server.address]));
    cluster.leader_schedule = serde_json::Value::Null;
    let rpc = cluster.serve().await;

    let report = execute(
        &client(&rpc.url, fast_policy()),
        &pulsar(),
        &directory,
        STAMP,
    )
    .await
    .unwrap();

    assert_eq!(report.schedule_source, ScheduleSource::Null);
    assert!(!report.leader_slots_path.exists());
    let rows = validator_rows(&std::fs::read_to_string(&report.validators_path).unwrap());
    assert!(rows.iter().all(|row| row.scheduled_leader_slots == 0));
}

#[tokio::test]
async fn invalid_entries_are_dropped_and_counted_in_the_report() {
    let directory = scratch_dir("run-dropped");
    let server = TpuServer::spawn(clarion_pulsar::tls::TPU_ALPN, Admission::Accept);
    let mut cluster = Cluster::with_tpu(&shared(&[server.address]));
    cluster
        .nodes
        .as_array_mut()
        .unwrap()
        .push(serde_json::json!({
            "pubkey": "\u{1b}[31mnot-a-key", "tpuQuic": server.address.to_string()
        }));
    cluster.vote_accounts["current"]
        .as_array_mut()
        .unwrap()
        .push(serde_json::json!({
            "votePubkey": "not-a-vote-account",
            "nodePubkey": synthetic_key(1),
            "activatedStake": 1,
            "commission": 5,
            "lastVote": 1000,
            "rootSlot": 968,
            "epochCredits": []
        }));
    cluster.block_production = serde_json::json!({
        "context": { "slot": 1000 },
        "value": { "byIdentity": { "short": [1, 1] }, "range": { "firstSlot": 800, "lastSlot": 1000 } }
    });
    cluster.leader_schedule = serde_json::json!({ "short": [0], "also short": [1] });
    let rpc = cluster.serve().await;

    let report = execute(
        &client(&rpc.url, fast_policy()),
        &pulsar(),
        &directory,
        STAMP,
    )
    .await
    .unwrap();

    assert_eq!(
        report.dropped,
        Dropped {
            nodes: 1,
            vote_accounts: 1,
            block_production: 1,
            leader_schedule: 2,
        }
    );
    assert_eq!(report.targets, 1);
    let text = std::fs::read_to_string(&report.handshake_path).unwrap();
    assert!(!text.contains('\u{1b}'));
}

#[tokio::test]
async fn rpc_failure_starts_no_handshake_and_creates_no_file() {
    let directory = scratch_dir("run-rpc-failure");
    let (seen, mut arrivals) = mpsc::unbounded_channel();
    let server = TpuServer::spawn_at(
        loopback(),
        clarion_pulsar::tls::TPU_ALPN,
        Admission::Accept,
        move || {
            let _ = seen.send(());
        },
    )
    .unwrap();
    let cluster = Cluster::with_tpu(&shared(&[server.address]));
    let rpc = CannedRpc::spawn(move |request| match request.method() {
        "getVoteAccounts" => RpcReply::status(403, "Forbidden"),
        method => cluster.reply(method),
    })
    .await;

    let error = execute(
        &client(&rpc.url, fast_policy()),
        &pulsar(),
        &directory,
        STAMP,
    )
    .await
    .unwrap_err();

    assert!(matches!(error, RunError::Rpc(_)));
    assert_eq!(
        error.to_string(),
        "getVoteAccounts failed after 1 of 5 attempts: rpc returned http status 403"
    );
    assert_eq!(arrivals.try_recv(), Err(mpsc::error::TryRecvError::Empty));
    assert!(!directory.exists());
}

#[tokio::test]
async fn existing_handshake_file_fails_the_run_before_any_handshake() {
    let directory = scratch_dir("run-handshake-collision");
    let taken = CsvFile::create(&directory, STAMP).unwrap();
    let before = std::fs::read(taken.path()).unwrap();
    let (seen, mut arrivals) = mpsc::unbounded_channel();
    let server = TpuServer::spawn_at(
        loopback(),
        clarion_pulsar::tls::TPU_ALPN,
        Admission::Accept,
        move || {
            let _ = seen.send(());
        },
    )
    .unwrap();
    let rpc = Cluster::with_tpu(&shared(&[server.address])).serve().await;

    let error = execute(
        &client(&rpc.url, fast_policy()),
        &pulsar(),
        &directory,
        STAMP,
    )
    .await
    .unwrap_err();

    assert!(matches!(error, RunError::Output(_)));
    assert_eq!(arrivals.try_recv(), Err(mpsc::error::TryRecvError::Empty));
    assert_eq!(std::fs::read(taken.path()).unwrap(), before);
    assert!(
        !directory
            .join("clarion-pulsar-validators-20260921T141320Z.csv")
            .exists()
    );
}

#[tokio::test]
async fn existing_validators_file_fails_the_run_before_any_handshake() {
    let directory = scratch_dir("run-validators-collision");
    let taken = CsvFile::create_validators(&directory, STAMP).unwrap();
    let (seen, mut arrivals) = mpsc::unbounded_channel();
    let server = TpuServer::spawn_at(
        loopback(),
        clarion_pulsar::tls::TPU_ALPN,
        Admission::Accept,
        move || {
            let _ = seen.send(());
        },
    )
    .unwrap();
    let rpc = Cluster::with_tpu(&shared(&[server.address])).serve().await;

    let error = execute(
        &client(&rpc.url, fast_policy()),
        &pulsar(),
        &directory,
        STAMP,
    )
    .await
    .unwrap_err();

    assert!(matches!(error, RunError::Output(_)));
    assert_eq!(arrivals.try_recv(), Err(mpsc::error::TryRecvError::Empty));
    assert_eq!(
        std::fs::read_to_string(taken.path())
            .unwrap()
            .lines()
            .count(),
        1
    );
}

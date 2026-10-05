use std::{
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use thiserror::Error;

use crate::{
    leader_slots::{self, CacheError},
    output::{CsvFile, OutputError},
    pulsar::Pulsar,
    rpc::{LeaderSlots, RpcClient, RpcError, cluster_label},
    targets::{distinct_addresses, select_targets},
    validators::{Snapshot, join},
};

#[derive(Debug, Error)]
pub enum RunError {
    #[error(transparent)]
    Rpc(#[from] RpcError),
    #[error(transparent)]
    Cache(#[from] CacheError),
    #[error(transparent)]
    Output(#[from] OutputError),
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Dropped {
    pub nodes: u64,
    pub vote_accounts: u64,
    pub block_production: u64,
    pub leader_schedule: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScheduleSource {
    Cache,
    Rpc,
    RpcNotCached,
    Null,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    pub cluster: String,
    pub epoch: u64,
    pub handshake_path: PathBuf,
    pub validators_path: PathBuf,
    pub leader_slots_path: PathBuf,
    pub schedule_source: ScheduleSource,
    pub nodes: usize,
    pub targets: usize,
    pub addresses: usize,
    pub validator_rows: usize,
    pub rows: usize,
    pub ok_rows: usize,
    pub dropped: Dropped,
}

pub async fn execute(
    rpc: &RpcClient,
    pulsar: &Pulsar,
    directory: &Path,
    stamp: &str,
) -> Result<Report, RunError> {
    let genesis_hash = rpc.genesis_hash().await?;
    let cluster = cluster_label(&genesis_hash).to_owned();
    let epoch_info = rpc.epoch_info().await?;
    let nodes = rpc.cluster_nodes().await?;
    let vote_accounts = rpc.vote_accounts().await?;
    let block_production = rpc.block_production().await?;
    let mut dropped = Dropped {
        nodes: nodes.dropped,
        vote_accounts: vote_accounts.dropped,
        block_production: block_production.dropped,
        leader_schedule: 0,
    };
    let (leader_slots, schedule_source) =
        match leader_slots::read(directory, &cluster, epoch_info.epoch) {
            Some(cached) => (cached, ScheduleSource::Cache),
            None => {
                let first_slot =
                    epoch_info.first_slot().unwrap_or(epoch_info.absolute_slot);
                match rpc.leader_schedule(first_slot).await? {
                    Some(schedule) => {
                        dropped.leader_schedule = schedule.dropped;
                        let created = leader_slots::write(
                            directory,
                            &cluster,
                            epoch_info.epoch,
                            &schedule.kept,
                        )?;
                        let source = if created {
                            ScheduleSource::Rpc
                        } else {
                            ScheduleSource::RpcNotCached
                        };
                        (schedule.kept, source)
                    }
                    None => (LeaderSlots::new(), ScheduleSource::Null),
                }
            }
        };
    let rows = join(&Snapshot {
        sampled_ts_us: unix_micros(SystemTime::now()),
        cluster: &cluster,
        epoch: epoch_info.epoch,
        nodes: &nodes.kept,
        vote_accounts: &vote_accounts.kept,
        block_production: &block_production.kept,
        leader_slots: &leader_slots,
    });
    let targets = select_targets(&nodes.kept);
    let mut handshakes = CsvFile::create(directory, stamp)?;
    let mut validators = CsvFile::create_validators(directory, stamp)?;
    validators.append(&rows)?;
    let samples = pulsar.run(&targets, &cluster, &mut handshakes).await?;
    pulsar.drain().await;
    Ok(Report {
        epoch: epoch_info.epoch,
        handshake_path: handshakes.path().to_path_buf(),
        validators_path: validators.path().to_path_buf(),
        leader_slots_path: leader_slots::path(directory, &cluster, epoch_info.epoch),
        schedule_source,
        nodes: nodes.kept.len(),
        targets: targets.len(),
        addresses: distinct_addresses(&targets).len(),
        validator_rows: rows.len(),
        rows: samples.len(),
        ok_rows: samples.iter().filter(|sample| sample.ok).count(),
        dropped,
        cluster,
    })
}

fn unix_micros(at: SystemTime) -> u64 {
    at.duration_since(UNIX_EPOCH).map_or(0, |since| {
        u64::try_from(since.as_micros()).unwrap_or(u64::MAX)
    })
}

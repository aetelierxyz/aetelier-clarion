use std::{
    fs::{self, File},
    io::{self, Write},
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::rpc::{LeaderSlots, is_pubkey};

pub const COLUMNS: [&str; 2] = ["identity", "leader_slots"];

#[derive(Debug, Error)]
pub enum CacheError {
    #[error("failed to write the leader slot cache {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("failed to write the leader slot cache {path}: {source}")]
    Csv {
        path: PathBuf,
        #[source]
        source: csv::Error,
    },
}

#[derive(Debug, Serialize, Deserialize)]
struct Entry {
    identity: String,
    leader_slots: u64,
}

pub fn file_name(cluster: &str, epoch: u64) -> String {
    format!("clarion-pulsar-leader-slots-{cluster}-{epoch}.csv")
}

pub fn path(directory: &Path, cluster: &str, epoch: u64) -> PathBuf {
    directory.join(file_name(cluster, epoch))
}

pub fn read(directory: &Path, cluster: &str, epoch: u64) -> Option<LeaderSlots> {
    let file = File::open(path(directory, cluster, epoch)).ok()?;
    parse(file)
}

fn parse(source: impl io::Read) -> Option<LeaderSlots> {
    let mut reader = csv::Reader::from_reader(source);
    if reader.headers().ok()? != COLUMNS.as_slice() {
        return None;
    }
    let mut slots = LeaderSlots::new();
    for entry in reader.deserialize::<Entry>() {
        let entry = entry.ok()?;
        if !is_pubkey(&entry.identity) || slots.contains_key(&entry.identity) {
            return None;
        }
        slots.insert(entry.identity, entry.leader_slots);
    }
    Some(slots)
}

pub fn write(
    directory: &Path,
    cluster: &str,
    epoch: u64,
    slots: &LeaderSlots,
) -> Result<bool, CacheError> {
    let target = path(directory, cluster, epoch);
    let io_error = |source| CacheError::Io {
        path: target.clone(),
        source,
    };
    fs::create_dir_all(directory).map_err(io_error)?;
    let temporary = directory.join(format!(
        "{}.{:016x}.tmp",
        file_name(cluster, epoch),
        fastrand::u64(..)
    ));
    let written = write_temporary(&temporary, slots, &target);
    let published = written.and_then(|()| match fs::hard_link(&temporary, &target) {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => Ok(false),
        Err(error) => Err(io_error(error)),
    });
    let removed = fs::remove_file(&temporary);
    let created = published?;
    removed.map_err(io_error)?;
    Ok(created)
}

fn write_temporary(
    temporary: &Path,
    slots: &LeaderSlots,
    target: &Path,
) -> Result<(), CacheError> {
    let io_error = |source| CacheError::Io {
        path: target.to_path_buf(),
        source,
    };
    let csv_error = |source| CacheError::Csv {
        path: target.to_path_buf(),
        source,
    };
    let file = File::options()
        .write(true)
        .create_new(true)
        .open(temporary)
        .map_err(io_error)?;
    let mut writer = csv::Writer::from_writer(file);
    for (identity, leader_slots) in slots {
        writer
            .serialize(Entry {
                identity: identity.clone(),
                leader_slots: *leader_slots,
            })
            .map_err(csv_error)?;
    }
    if slots.is_empty() {
        writer.write_record(COLUMNS).map_err(csv_error)?;
    }
    let mut file = writer
        .into_inner()
        .map_err(|error| io_error(error.into_error()))?;
    file.flush().map_err(io_error)?;
    file.sync_all().map_err(io_error)
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY_A: &str = "F2K3Fm5Vm26tz7ENRmu1RetmC2K91Lciz1bVZG3b3J2V";
    const KEY_B: &str = "6WK8ze98CueYEekV3SopfDeUUeKcoB7AjVknciENUBZC";

    #[test]
    fn file_name_carries_cluster_and_epoch() {
        assert_eq!(
            file_name("devnet", 7),
            "clarion-pulsar-leader-slots-devnet-7.csv"
        );
    }

    #[test]
    fn identity_counts_parse() {
        let text = format!("identity,leader_slots\n{KEY_A},4\n{KEY_B},0\n");

        assert_eq!(
            parse(text.as_bytes()),
            Some(LeaderSlots::from([
                (KEY_A.to_owned(), 4),
                (KEY_B.to_owned(), 0)
            ]))
        );
    }

    #[test]
    fn header_only_parses_as_an_empty_schedule() {
        assert_eq!(
            parse("identity,leader_slots\n".as_bytes()),
            Some(LeaderSlots::new())
        );
    }

    #[test]
    fn malformed_content_does_not_parse() {
        for text in [
            String::new(),
            "pubkey,slots\n".to_owned(),
            format!("identity,leader_slots\n{KEY_A},four\n"),
            format!("identity,leader_slots\n{KEY_A},-1\n"),
            "identity,leader_slots\nnot-a-key,4\n".to_owned(),
            format!("identity,leader_slots\n{KEY_A},4\n{KEY_A},5\n"),
            format!("identity,leader_slots\n{KEY_A}\n"),
        ] {
            assert_eq!(parse(text.as_bytes()), None, "{text}");
        }
    }
}

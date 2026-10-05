use clarion_pulsar::{leader_slots, rpc::LeaderSlots};

use crate::support::scratch_dir;

const KEY_A: &str = "F2K3Fm5Vm26tz7ENRmu1RetmC2K91Lciz1bVZG3b3J2V";
const KEY_B: &str = "6WK8ze98CueYEekV3SopfDeUUeKcoB7AjVknciENUBZC";

fn slots() -> LeaderSlots {
    LeaderSlots::from([(KEY_A.to_owned(), 4), (KEY_B.to_owned(), 0)])
}

fn entries(directory: &std::path::Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(directory)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    names
}

#[test]
fn written_counts_read_back_and_no_temporary_file_remains() {
    let directory = scratch_dir("cache-round-trip").join("nested");

    let created = leader_slots::write(&directory, "devnet", 7, &slots()).unwrap();

    let path = leader_slots::path(&directory, "devnet", 7);
    assert!(created);
    assert_eq!(
        path,
        directory.join("clarion-pulsar-leader-slots-devnet-7.csv")
    );
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        format!("identity,leader_slots\n{KEY_B},0\n{KEY_A},4\n")
    );
    assert_eq!(leader_slots::read(&directory, "devnet", 7), Some(slots()));
    assert_eq!(
        entries(&directory),
        ["clarion-pulsar-leader-slots-devnet-7.csv"]
    );
}

#[test]
fn present_cache_file_is_never_overwritten() {
    let directory = scratch_dir("cache-kept");
    std::fs::create_dir_all(&directory).unwrap();
    let path = directory.join("clarion-pulsar-leader-slots-devnet-7.csv");
    std::fs::write(&path, b"identity,leader_slots\n").unwrap();

    let created = leader_slots::write(&directory, "devnet", 7, &slots()).unwrap();

    assert!(!created);
    assert_eq!(std::fs::read(&path).unwrap(), b"identity,leader_slots\n");
    assert_eq!(
        entries(&directory),
        ["clarion-pulsar-leader-slots-devnet-7.csv"]
    );
}

#[test]
fn cache_is_keyed_by_cluster_and_epoch() {
    let directory = scratch_dir("cache-keys");

    leader_slots::write(&directory, "devnet", 7, &slots()).unwrap();

    assert_eq!(leader_slots::read(&directory, "devnet", 8), None);
    assert_eq!(leader_slots::read(&directory, "testnet", 7), None);
    assert_eq!(leader_slots::read(&directory, "devnet", 7), Some(slots()));
}

#[test]
fn unreadable_or_absent_cache_reads_as_none() {
    let directory = scratch_dir("cache-absent");
    std::fs::create_dir_all(&directory).unwrap();
    std::fs::write(
        directory.join("clarion-pulsar-leader-slots-devnet-7.csv"),
        b"identity,leader_slots\nnot-a-key,3\n",
    )
    .unwrap();

    assert_eq!(leader_slots::read(&directory, "devnet", 7), None);
    assert_eq!(leader_slots::read(&directory, "devnet", 9), None);
}

#[test]
fn empty_schedule_is_cached_as_a_header() {
    let directory = scratch_dir("cache-empty");

    let created =
        leader_slots::write(&directory, "devnet", 7, &LeaderSlots::new()).unwrap();

    assert!(created);
    assert_eq!(
        std::fs::read_to_string(leader_slots::path(&directory, "devnet", 7)).unwrap(),
        "identity,leader_slots\n"
    );
    assert_eq!(
        leader_slots::read(&directory, "devnet", 7),
        Some(LeaderSlots::new())
    );
}

use clarion_pulsar::{
    output::{CsvFile, OutputError},
    pulsar::{HandshakeFailure, PulsarSample},
};

use crate::support::{HEADER, STAMP, scratch_dir};

const COMPLETED_ROW: &str = "1790000000123456,devnet,fra-1,\
                             F2K3Fm5Vm26tz7ENRmu1RetmC2K91Lciz1bVZG3b3J2V,192.0.2.10:8009,\
                             192.0.2.10,0,true,48210,\
                             F2K3Fm5Vm26tz7ENRmu1RetmC2K91Lciz1bVZG3b3J2V,\n";
const TIMED_OUT_ROW: &str = "1790000000123456,devnet,fra-1,\
                             F2K3Fm5Vm26tz7ENRmu1RetmC2K91Lciz1bVZG3b3J2V,192.0.2.10:8009,\
                             192.0.2.10,1,false,0,,timeout\n";

fn samples() -> Vec<PulsarSample> {
    let completed = PulsarSample {
        sampled_ts_us: 1_790_000_000_123_456,
        cluster: "devnet".to_owned(),
        vantage: "fra-1".to_owned(),
        pubkey: "F2K3Fm5Vm26tz7ENRmu1RetmC2K91Lciz1bVZG3b3J2V".to_owned(),
        tpu_quic: "192.0.2.10:8009".parse().unwrap(),
        gossip_ip: Some("192.0.2.10".parse().unwrap()),
        attempt: 0,
        ok: true,
        handshake_us: 48_210,
        peer_pubkey: Some("F2K3Fm5Vm26tz7ENRmu1RetmC2K91Lciz1bVZG3b3J2V".to_owned()),
        err: None,
    };
    let timed_out = PulsarSample {
        attempt: 1,
        ok: false,
        handshake_us: 0,
        peer_pubkey: None,
        err: Some(HandshakeFailure::Timeout),
        ..completed.clone()
    };
    vec![completed, timed_out]
}

#[test]
fn csv_is_written_under_a_created_directory_with_the_stamped_name() {
    let directory = scratch_dir("stamped-name").join("datasets/clarion-pulsar");

    let mut file = CsvFile::create(&directory, "20260921T141320Z").unwrap();
    file.append(&samples()).unwrap();

    assert_eq!(
        file.path(),
        directory.join("clarion-pulsar-20260921T141320Z.csv")
    );
    assert_eq!(
        std::fs::read_to_string(file.path()).unwrap(),
        "sampled_ts_us,cluster,vantage,pubkey,tpu_quic,gossip_ip,attempt,ok,handshake_us,peer_pubkey,err\n\
         1790000000123456,devnet,fra-1,F2K3Fm5Vm26tz7ENRmu1RetmC2K91Lciz1bVZG3b3J2V,192.0.2.10:8009,192.0.2.10,0,true,48210,F2K3Fm5Vm26tz7ENRmu1RetmC2K91Lciz1bVZG3b3J2V,\n\
         1790000000123456,devnet,fra-1,F2K3Fm5Vm26tz7ENRmu1RetmC2K91Lciz1bVZG3b3J2V,192.0.2.10:8009,192.0.2.10,1,false,0,,timeout\n"
    );
}

#[test]
fn run_without_targets_still_writes_the_header() {
    let directory = scratch_dir("header-only");

    let mut file = CsvFile::create(&directory, "20260921T141320Z").unwrap();
    file.append::<PulsarSample>(&[]).unwrap();

    assert_eq!(
        std::fs::read_to_string(file.path())
            .unwrap()
            .lines()
            .count(),
        1
    );
}

#[test]
fn existing_file_is_never_overwritten() {
    let directory = scratch_dir("no-overwrite");
    let mut file = CsvFile::create(&directory, "20260921T141320Z").unwrap();
    file.append(&samples()).unwrap();
    let before = std::fs::read(file.path()).unwrap();

    let error = CsvFile::create(&directory, "20260921T141320Z").unwrap_err();

    assert!(matches!(
        error,
        OutputError::Create { path: reported, .. } if reported == file.path()
    ));
    assert_eq!(std::fs::read(file.path()).unwrap(), before);
}

#[test]
fn header_is_on_disk_as_soon_as_the_file_is_created() {
    let file = CsvFile::create(&scratch_dir("header-first"), STAMP).unwrap();

    assert_eq!(std::fs::read_to_string(file.path()).unwrap(), HEADER);
}

#[test]
fn appended_rows_are_on_disk_when_each_append_returns() {
    let samples = samples();
    let mut file = CsvFile::create(&scratch_dir("append-flush"), STAMP).unwrap();

    file.append(&samples[..1]).unwrap();
    let after_first = std::fs::read_to_string(file.path()).unwrap();
    file.append(&samples[1..]).unwrap();
    let after_second = std::fs::read_to_string(file.path()).unwrap();

    assert_eq!(after_first, [HEADER, COMPLETED_ROW].concat());
    assert_eq!(
        after_second,
        [HEADER, COMPLETED_ROW, TIMED_OUT_ROW].concat()
    );
}

#[test]
fn directory_that_cannot_be_created_is_reported_at_creation() {
    let occupied = scratch_dir("blocked-directory");
    std::fs::create_dir_all(&occupied).unwrap();
    let blocker = occupied.join("not-a-directory");
    std::fs::write(&blocker, b"").unwrap();
    let directory = blocker.join("clarion-pulsar");

    let error = CsvFile::create(&directory, STAMP).unwrap_err();

    assert!(matches!(
        error,
        OutputError::Create { path: reported, .. }
            if reported == directory.join("clarion-pulsar-20260921T141320Z.csv")
    ));
    assert!(!directory.exists());
}

#[test]
fn validators_file_is_created_beside_the_handshake_file_with_the_same_stamp() {
    let directory = scratch_dir("validators-file");

    let handshakes = CsvFile::create(&directory, STAMP).unwrap();
    let validators = CsvFile::create_validators(&directory, STAMP).unwrap();

    assert_eq!(validators.path().parent(), handshakes.path().parent());
    assert_eq!(
        validators.path(),
        directory.join("clarion-pulsar-validators-20260921T141320Z.csv")
    );
    assert_eq!(
        std::fs::read_to_string(validators.path()).unwrap(),
        "sampled_ts_us,cluster,epoch,identity,vote_account,gossip,tpu_quic,version,\
         activated_stake_lamports,commission,delinquent,last_vote,root_slot,epoch_credits,\
         leader_slots,blocks_produced,scheduled_leader_slots\n"
    );
}

#[test]
fn existing_validators_file_is_never_overwritten() {
    let directory = scratch_dir("validators-no-overwrite");
    let first = CsvFile::create_validators(&directory, STAMP).unwrap();
    std::fs::write(first.path(), b"kept").unwrap();

    let error = CsvFile::create_validators(&directory, STAMP).unwrap_err();

    assert!(matches!(
        error,
        OutputError::Create { path: reported, .. } if reported == first.path()
    ));
    assert_eq!(std::fs::read(first.path()).unwrap(), b"kept");
}

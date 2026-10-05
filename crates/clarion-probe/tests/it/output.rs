use clarion_probe::{
    output::{CsvFile, OutputError},
    probe::{HandshakeFailure, ProbeSample},
};

use crate::support::{HEADER, STAMP, scratch_dir};

const COMPLETED_ROW: &str = "1790000000123456,devnet,fra-1,\
                             F2K3Fm5Vm26tz7ENRmu1RetmC2K91Lciz1bVZG3b3J2V,192.0.2.10:8009,\
                             192.0.2.10,0,true,48210,23950,\n";
const TIMED_OUT_ROW: &str = "1790000000123456,devnet,fra-1,\
                             F2K3Fm5Vm26tz7ENRmu1RetmC2K91Lciz1bVZG3b3J2V,192.0.2.10:8009,\
                             192.0.2.10,1,false,0,0,timeout\n";

fn samples() -> Vec<ProbeSample> {
    let completed = ProbeSample {
        sampled_ts_us: 1_790_000_000_123_456,
        cluster: "devnet".to_owned(),
        vantage: "fra-1".to_owned(),
        pubkey: "F2K3Fm5Vm26tz7ENRmu1RetmC2K91Lciz1bVZG3b3J2V".to_owned(),
        tpu_quic: "192.0.2.10:8009".parse().unwrap(),
        gossip_ip: Some("192.0.2.10".parse().unwrap()),
        attempt: 0,
        ok: true,
        handshake_us: 48_210,
        rtt_us: 23_950,
        err: None,
    };
    let timed_out = ProbeSample {
        attempt: 1,
        ok: false,
        handshake_us: 0,
        rtt_us: 0,
        err: Some(HandshakeFailure::Timeout),
        ..completed.clone()
    };
    vec![completed, timed_out]
}

#[test]
fn csv_is_written_under_a_created_directory_with_the_stamped_name() {
    let directory = scratch_dir("stamped-name").join("datasets/probe");

    let mut file = CsvFile::create(&directory, "20260921T141320Z").unwrap();
    file.append(&samples()).unwrap();

    assert_eq!(file.path(), directory.join("probe-20260921T141320Z.csv"));
    assert_eq!(
        std::fs::read_to_string(file.path()).unwrap(),
        "sampled_ts_us,cluster,vantage,pubkey,tpu_quic,gossip_ip,attempt,ok,handshake_us,rtt_us,err\n\
         1790000000123456,devnet,fra-1,F2K3Fm5Vm26tz7ENRmu1RetmC2K91Lciz1bVZG3b3J2V,192.0.2.10:8009,192.0.2.10,0,true,48210,23950,\n\
         1790000000123456,devnet,fra-1,F2K3Fm5Vm26tz7ENRmu1RetmC2K91Lciz1bVZG3b3J2V,192.0.2.10:8009,192.0.2.10,1,false,0,0,timeout\n"
    );
}

#[test]
fn run_without_targets_still_writes_the_header() {
    let directory = scratch_dir("header-only");

    let mut file = CsvFile::create(&directory, "20260921T141320Z").unwrap();
    file.append(&[]).unwrap();

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
    let directory = blocker.join("probe");

    let error = CsvFile::create(&directory, STAMP).unwrap_err();

    assert!(matches!(
        error,
        OutputError::Create { path: reported, .. }
            if reported == directory.join("probe-20260921T141320Z.csv")
    ));
    assert!(!directory.exists());
}

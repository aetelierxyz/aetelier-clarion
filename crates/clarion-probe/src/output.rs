use std::{
    fs::{self, File},
    io,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use thiserror::Error;

use crate::probe::ProbeSample;

pub const COLUMNS: [&str; 11] = [
    "sampled_ts_us",
    "cluster",
    "vantage",
    "pubkey",
    "tpu_quic",
    "gossip_ip",
    "attempt",
    "ok",
    "handshake_us",
    "rtt_us",
    "err",
];

const SECONDS_PER_DAY: u64 = 86_400;
const DAYS_PER_ERA: u64 = 146_097;
const DAYS_FROM_ERA_START_TO_UNIX_EPOCH: u64 = 719_468;

#[derive(Debug, Error)]
pub enum OutputError {
    #[error("system clock is outside the supported utc range")]
    Clock,
    #[error("failed to create {path}: {source}")]
    Create {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("failed to write {path}: {source}")]
    Write {
        path: PathBuf,
        #[source]
        source: csv::Error,
    },
}

pub fn utc_stamp(at: SystemTime) -> Result<String, OutputError> {
    let unix_seconds = at
        .duration_since(UNIX_EPOCH)
        .map_err(|_| OutputError::Clock)?
        .as_secs();
    let (year, month, day) =
        civil_from_days(unix_seconds / SECONDS_PER_DAY).ok_or(OutputError::Clock)?;
    let second_of_day = unix_seconds % SECONDS_PER_DAY;
    let (hour, minute, second) = (
        second_of_day / 3_600,
        second_of_day % 3_600 / 60,
        second_of_day % 60,
    );
    Ok(format!(
        "{year:04}{month:02}{day:02}T{hour:02}{minute:02}{second:02}Z"
    ))
}

fn civil_from_days(unix_days: u64) -> Option<(u64, u64, u64)> {
    let days = unix_days.checked_add(DAYS_FROM_ERA_START_TO_UNIX_EPOCH)?;
    let era = days / DAYS_PER_ERA;
    let day_of_era = days % DAYS_PER_ERA;
    let year_of_era = day_of_era
        .checked_sub(day_of_era / 1_460)?
        .checked_add(day_of_era / 36_524)?
        .checked_sub(day_of_era / 146_096)?
        / 365;
    let days_before_year = year_of_era
        .checked_mul(365)?
        .checked_add(year_of_era / 4)?
        .checked_sub(year_of_era / 100)?;
    let day_of_year = day_of_era.checked_sub(days_before_year)?;
    let month_from_march = day_of_year.checked_mul(5)?.checked_add(2)? / 153;
    let days_before_month = month_from_march.checked_mul(153)?.checked_add(2)? / 5;
    let day = day_of_year.checked_sub(days_before_month)?.checked_add(1)?;
    let (month, year_carry) = if month_from_march < 10 {
        (month_from_march.checked_add(3)?, 0)
    } else {
        (month_from_march.checked_sub(9)?, 1)
    };
    let year = era
        .checked_mul(400)?
        .checked_add(year_of_era)?
        .checked_add(year_carry)?;
    Some((year, month, day))
}

pub fn file_name(stamp: &str) -> String {
    format!("probe-{stamp}.csv")
}

fn write_header<W: io::Write>(sink: W) -> Result<csv::Writer<W>, csv::Error> {
    let mut writer = csv::WriterBuilder::new()
        .has_headers(false)
        .from_writer(sink);
    writer.write_record(COLUMNS)?;
    writer.flush()?;
    Ok(writer)
}

fn write_rows<W: io::Write>(
    writer: &mut csv::Writer<W>,
    samples: &[ProbeSample],
) -> Result<(), csv::Error> {
    for sample in samples {
        writer.serialize(sample)?;
    }
    writer.flush()?;
    Ok(())
}

#[derive(Debug)]
pub struct CsvFile {
    path: PathBuf,
    writer: csv::Writer<File>,
}

impl CsvFile {
    pub fn create(directory: &Path, stamp: &str) -> Result<Self, OutputError> {
        let path = directory.join(file_name(stamp));
        let create = |source| OutputError::Create {
            path: path.clone(),
            source,
        };
        fs::create_dir_all(directory).map_err(create)?;
        let file = File::options()
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(create)?;
        let writer = write_header(file).map_err(|source| OutputError::Write {
            path: path.clone(),
            source,
        })?;
        Ok(Self { path, writer })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn append(&mut self, samples: &[ProbeSample]) -> Result<(), OutputError> {
        write_rows(&mut self.writer, samples).map_err(|source| OutputError::Write {
            path: self.path.clone(),
            source,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::probe::HandshakeFailure;

    fn stamp_at(unix_seconds: u64) -> String {
        utc_stamp(UNIX_EPOCH + Duration::from_secs(unix_seconds)).unwrap()
    }

    fn ok_sample() -> ProbeSample {
        ProbeSample {
            sampled_ts_us: 1_790_000_000_123_456,
            cluster: "devnet".to_owned(),
            vantage: "fra-1".to_owned(),
            pubkey: "F2K3Fm5Vm26tz7ENRmu1RetmC2K91Lciz1bVZG3b3J2V".to_owned(),
            tpu_quic: "198.51.100.7:8009".parse().unwrap(),
            gossip_ip: Some("192.0.2.1".parse().unwrap()),
            attempt: 2,
            ok: true,
            handshake_us: 48_210,
            rtt_us: 23_950,
            err: None,
        }
    }

    fn failed_sample(failure: HandshakeFailure) -> ProbeSample {
        ProbeSample {
            gossip_ip: None,
            attempt: 0,
            ok: false,
            handshake_us: 0,
            rtt_us: 0,
            err: Some(failure),
            ..ok_sample()
        }
    }

    fn encode(samples: &[ProbeSample]) -> String {
        let mut bytes = Vec::new();
        let mut writer = write_header(&mut bytes).unwrap();
        write_rows(&mut writer, samples).unwrap();
        drop(writer);
        String::from_utf8(bytes).unwrap()
    }

    #[test]
    fn epoch_formats_as_compact_utc() {
        assert_eq!(stamp_at(0), "19700101T000000Z");
    }

    #[test]
    fn stamp_carries_time_of_day() {
        assert_eq!(stamp_at(1_700_000_000), "20231114T221320Z");
        assert_eq!(stamp_at(1_790_000_000), "20260921T141320Z");
    }

    #[test]
    fn last_second_of_a_day_does_not_roll_over() {
        assert_eq!(stamp_at(86_399), "19700101T235959Z");
        assert_eq!(stamp_at(86_400), "19700102T000000Z");
    }

    #[test]
    fn leap_day_and_the_day_after_are_distinct() {
        assert_eq!(stamp_at(1_709_251_199), "20240229T235959Z");
        assert_eq!(stamp_at(1_709_251_200), "20240301T000000Z");
    }

    #[test]
    fn century_leap_rules_hold() {
        assert_eq!(stamp_at(951_782_400), "20000229T000000Z");
        assert_eq!(stamp_at(4_107_456_000), "21000228T000000Z");
        assert_eq!(stamp_at(4_107_542_400), "21000301T000000Z");
    }

    #[test]
    fn year_boundaries_hold() {
        assert_eq!(stamp_at(1_767_225_599), "20251231T235959Z");
        assert_eq!(stamp_at(1_767_225_600), "20260101T000000Z");
    }

    #[test]
    fn subsecond_part_is_truncated() {
        let at = UNIX_EPOCH + Duration::from_millis(1_700_000_000_999);

        assert_eq!(utc_stamp(at).unwrap(), "20231114T221320Z");
    }

    #[test]
    fn clock_before_the_epoch_is_rejected() {
        let error = utc_stamp(UNIX_EPOCH - Duration::from_secs(1)).unwrap_err();

        assert!(matches!(error, OutputError::Clock));
    }

    #[test]
    fn largest_day_count_stays_in_range() {
        assert!(civil_from_days(u64::MAX / SECONDS_PER_DAY).is_some());
    }

    #[test]
    fn day_count_overflow_is_reported_not_wrapped() {
        assert_eq!(civil_from_days(u64::MAX), None);
    }

    #[test]
    fn file_name_embeds_the_stamp() {
        assert_eq!(file_name("20260921T141320Z"), "probe-20260921T141320Z.csv");
    }

    #[test]
    fn header_is_the_eleven_columns_in_order() {
        let text = encode(&[]);

        assert_eq!(
            text,
            "sampled_ts_us,cluster,vantage,pubkey,tpu_quic,gossip_ip,attempt,ok,\
             handshake_us,rtt_us,err\n"
        );
    }

    #[test]
    fn ok_row_leaves_err_empty() {
        let text = encode(&[ok_sample()]);

        assert_eq!(
            text.lines().nth(1),
            Some(
                "1790000000123456,devnet,fra-1,F2K3Fm5Vm26tz7ENRmu1RetmC2K91Lciz1bVZG3b3J2V,\
                 198.51.100.7:8009,192.0.2.1,2,true,48210,23950,"
            )
        );
    }

    #[test]
    fn failed_row_zeroes_durations_and_names_the_failure() {
        let text = encode(&[failed_sample(HandshakeFailure::ClosedByPeer)]);

        assert_eq!(
            text.lines().nth(1),
            Some(
                "1790000000123456,devnet,fra-1,F2K3Fm5Vm26tz7ENRmu1RetmC2K91Lciz1bVZG3b3J2V,\
                 198.51.100.7:8009,,0,false,0,0,closed_by_peer"
            )
        );
    }

    #[test]
    fn every_failure_serialises_to_its_snake_case_name() {
        let names: Vec<String> = [
            HandshakeFailure::Timeout,
            HandshakeFailure::Refused,
            HandshakeFailure::Tls,
            HandshakeFailure::ClosedByPeer,
            HandshakeFailure::Other,
        ]
        .into_iter()
        .map(|failure| {
            let text = encode(&[failed_sample(failure)]);
            text.trim_end().rsplit(',').next().unwrap().to_owned()
        })
        .collect();

        assert_eq!(
            names,
            ["timeout", "refused", "tls", "closed_by_peer", "other"]
        );
    }

    #[test]
    fn rows_round_trip_through_the_header() {
        let samples = vec![
            ok_sample(),
            failed_sample(HandshakeFailure::Timeout),
            ProbeSample {
                vantage: "rack 4, row \"b\"".to_owned(),
                ..ok_sample()
            },
        ];

        let text = encode(&samples);
        let decoded: Vec<ProbeSample> = csv::Reader::from_reader(text.as_bytes())
            .deserialize()
            .collect::<Result<_, _>>()
            .unwrap();

        assert_eq!(decoded, samples);
    }
}

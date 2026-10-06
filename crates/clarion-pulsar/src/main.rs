use std::{error::Error, path::PathBuf, process::ExitCode, time::SystemTime};

use clap::Parser;
use clarion_pulsar::{
    config::{FileConfig, Overrides, PulsarConfig},
    output::utc_stamp,
    pulsar::Pulsar,
    rpc::{RpcClient, escape_controls},
    run::{Report, ScheduleSource, execute},
};

const LONG_ABOUT: &str = "Solana TPU QUIC handshake latency sampler.

A run reads the cluster over JSON-RPC (getGenesisHash, getEpochInfo, getClusterNodes, \
getVoteAccounts, getBlockProduction, getLeaderSchedule), writes one validators file with \
stake, vote and leader data for every identity, then runs paced sweeps that open one QUIC \
handshake (ALPN solana-tpu) to every distinct tpuQuic address and write one row per \
identity and sweep. Each completed handshake records the Ed25519 key the server proved.";

const AFTER_LONG_HELP: &str = "\
Config file keys (TOML, all optional except vantage):
  rpc_url                JSON-RPC endpoint; default https://api.devnet.solana.com
  vantage                label written to every handshake row; required
  rounds                 sweeps per run, 1 to 255; default 3
  round_spacing_ms       pause from the end of one sweep to the next, at least 30000; default 30000
  concurrency            handshakes in flight, 1 to 65535; default 64
  connect_timeout_ms     handshake timeout, at least 1; default 2000
  max_starts_per_second  handshake starts per second, 1 to 100; default 100
  output_dir             directory for every output file; default datasets/clarion-pulsar

Output files in output_dir (<ts> is the UTC start, YYYYMMDDTHHMMSSZ):
  clarion-pulsar-<ts>.csv
      sampled_ts_us, cluster, vantage, pubkey, tpu_quic, gossip_ip, attempt, ok,
      handshake_us, peer_pubkey, err
  clarion-pulsar-validators-<ts>.csv
      sampled_ts_us, cluster, epoch, identity, vote_account, gossip, tpu_quic, version,
      activated_stake_lamports, commission, delinquent, last_vote, root_slot,
      epoch_credits, leader_slots, blocks_produced, scheduled_leader_slots
  clarion-pulsar-leader-slots-<cluster>-<epoch>.csv
      identity, leader_slots; written once per cluster and epoch and read instead of
      calling getLeaderSchedule while present and readable

err values: timeout, refused, tls, closed_by_peer, other

Pacing:
  one handshake per distinct tpu_quic address per sweep, copied to every identity on it
  addresses shuffled at the start of every sweep
  at most max_starts_per_second starts per second and concurrency handshakes in flight
  at most one handshake in flight per destination IP address
  at least round_spacing_ms from the end of one sweep to the start of the next

RPC:
  up to 5 attempts per call on HTTP 429, HTTP 5xx, timeouts, connection errors and
  JSON-RPC error -32005; Retry-After is honoured up to 30 s, otherwise the waits are
  1, 2, 4 and 8 s plus up to 25% jitter; bodies above 64 MiB are rejected

Exit codes:
  0  run completed, or help or version printed
  1  configuration, RPC or file failure, reported on one stderr line
  2  invalid command line";

#[derive(Debug, Parser)]
#[command(
    name = "clarion-pulsar",
    version,
    about = "Solana TPU QUIC handshake latency sampler",
    long_about = LONG_ABOUT,
    after_long_help = AFTER_LONG_HELP
)]
struct Cli {
    #[arg(
        long,
        value_name = "PATH",
        help = "TOML config file; without it every key takes its default"
    )]
    config: Option<PathBuf>,
    #[arg(
        long,
        value_name = "URL",
        help = "JSON-RPC endpoint, overrides rpc_url; default https://api.devnet.solana.com; \
                a keyed provider URL belongs in the config file instead"
    )]
    rpc_url: Option<String>,
    #[arg(
        long,
        value_name = "LABEL",
        help = "Vantage label, overrides vantage; no default, required here or in the file"
    )]
    vantage: Option<String>,
}

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() -> ExitCode {
    match run(Cli::parse()).await {
        Ok(report) => {
            print!("{}", summary(&report));
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("clarion-pulsar: {}", escape_controls(&error.to_string()));
            ExitCode::FAILURE
        }
    }
}

async fn run(cli: Cli) -> Result<Report, Box<dyn Error>> {
    let overrides = Overrides {
        rpc_url: cli.rpc_url,
        vantage: cli.vantage,
    };
    let config =
        PulsarConfig::resolve(FileConfig::load(cli.config.as_deref())?, overrides)?;
    let stamp = utc_stamp(SystemTime::now())?;
    let rpc = RpcClient::new(config.rpc_url.as_str())?;
    let pulsar = Pulsar::new(&config)?;
    Ok(execute(&rpc, &pulsar, &config.output_dir, &stamp).await?)
}

fn summary(report: &Report) -> String {
    let schedule = match report.schedule_source {
        ScheduleSource::Cache => "read from",
        ScheduleSource::Rpc => "fetched into",
        ScheduleSource::RpcNotCached => "fetched; existing unreadable file kept at",
        ScheduleSource::Null => "null, nothing cached at",
    };
    let dropped = report.dropped;
    format!(
        "clarion-pulsar: cluster {}, epoch {}\n\
         handshakes: {}: {} of {} rows ok, {} identities on {} addresses of {} nodes\n\
         validators: {}: {} rows\n\
         leader schedule: {schedule} {}\n\
         dropped invalid entries: nodes {}, vote accounts {}, block production {}, \
         leader schedule {}\n",
        report.cluster,
        report.epoch,
        report.handshake_path.display(),
        report.ok_rows,
        report.rows,
        report.targets,
        report.addresses,
        report.nodes,
        report.validators_path.display(),
        report.validator_rows,
        report.leader_slots_path.display(),
        dropped.nodes,
        dropped.vote_accounts,
        dropped.block_production,
        dropped.leader_schedule,
    )
}

#[cfg(test)]
mod tests {
    use clap::CommandFactory;
    use clarion_pulsar::{
        config::FILE_KEYS,
        output::{COLUMNS, VALIDATOR_COLUMNS},
        run::Dropped,
    };

    use super::*;

    fn long_help() -> String {
        Cli::command().render_long_help().to_string()
    }

    #[test]
    fn command_definition_is_consistent() {
        Cli::command().debug_assert();
    }

    #[test]
    fn every_flag_is_optional() {
        let cli = Cli::try_parse_from(["clarion-pulsar"]).unwrap();

        assert_eq!(cli.config, None);
        assert_eq!(cli.rpc_url, None);
        assert_eq!(cli.vantage, None);
    }

    #[test]
    fn flags_are_parsed_by_their_long_names() {
        let cli = Cli::try_parse_from([
            "clarion-pulsar",
            "--config",
            "clarion-pulsar.toml",
            "--rpc-url",
            "http://127.0.0.1:8899",
            "--vantage",
            "fra-1",
        ])
        .unwrap();

        assert_eq!(cli.config, Some(PathBuf::from("clarion-pulsar.toml")));
        assert_eq!(cli.rpc_url.as_deref(), Some("http://127.0.0.1:8899"));
        assert_eq!(cli.vantage.as_deref(), Some("fra-1"));
    }

    #[test]
    fn unknown_flag_is_rejected() {
        assert!(Cli::try_parse_from(["clarion-pulsar", "--cluster", "devnet"]).is_err());
    }

    #[test]
    fn short_help_states_what_the_tool_is() {
        let help = Cli::command().render_help().to_string();

        assert!(help.starts_with("Solana TPU QUIC handshake latency sampler"));
        assert!(!help.contains("Exit codes"));
    }

    #[test]
    fn long_help_lists_every_config_key_and_every_column() {
        let help = long_help();

        for key in FILE_KEYS {
            assert!(help.contains(&format!("\n  {key} ")), "{key}");
        }
        for column in COLUMNS.iter().chain(VALIDATOR_COLUMNS.iter()) {
            assert!(help.contains(column), "{column}");
        }
    }

    #[test]
    fn long_help_states_the_defaults_and_limits_the_code_applies() {
        let help = long_help();
        let defaults = FileConfig::default();

        for fact in [
            format!("default {}", defaults.rpc_url),
            format!("default {}", defaults.rounds),
            format!("at least 30000; default {}", defaults.round_spacing_ms),
            format!("default {}", defaults.concurrency),
            format!("default {}", defaults.connect_timeout_ms),
            format!("1 to 100; default {}", defaults.max_starts_per_second),
            format!("default {}", defaults.output_dir.display()),
            "clarion-pulsar-<ts>.csv".to_owned(),
            "clarion-pulsar-validators-<ts>.csv".to_owned(),
            "clarion-pulsar-leader-slots-<cluster>-<epoch>.csv".to_owned(),
            "timeout, refused, tls, closed_by_peer, other".to_owned(),
            "Exit codes".to_owned(),
        ] {
            assert!(help.contains(&fact), "{fact}");
        }
    }

    #[test]
    fn every_flag_help_states_its_default() {
        let command = Cli::command();

        for argument in command.get_arguments() {
            let id = argument.get_id().as_str();
            if id == "help" || id == "version" {
                continue;
            }
            let help = argument
                .get_help()
                .map(ToString::to_string)
                .unwrap_or_default();
            assert!(help.contains("default"), "{id}: {help}");
        }
    }

    fn report() -> Report {
        Report {
            cluster: "devnet".to_owned(),
            epoch: 7,
            handshake_path: PathBuf::from("out/clarion-pulsar-20260921T141320Z.csv"),
            validators_path: PathBuf::from(
                "out/clarion-pulsar-validators-20260921T141320Z.csv",
            ),
            leader_slots_path: PathBuf::from(
                "out/clarion-pulsar-leader-slots-devnet-7.csv",
            ),
            schedule_source: ScheduleSource::Cache,
            nodes: 6,
            targets: 4,
            addresses: 3,
            validator_rows: 7,
            rows: 12,
            ok_rows: 9,
            dropped: Dropped {
                nodes: 1,
                vote_accounts: 2,
                block_production: 3,
                leader_schedule: 4,
            },
        }
    }

    #[test]
    fn summary_names_both_files_and_the_dropped_counts() {
        let text = summary(&report());

        assert_eq!(
            text,
            "clarion-pulsar: cluster devnet, epoch 7\n\
             handshakes: out/clarion-pulsar-20260921T141320Z.csv: 9 of 12 rows ok, \
             4 identities on 3 addresses of 6 nodes\n\
             validators: out/clarion-pulsar-validators-20260921T141320Z.csv: 7 rows\n\
             leader schedule: read from out/clarion-pulsar-leader-slots-devnet-7.csv\n\
             dropped invalid entries: nodes 1, vote accounts 2, block production 3, \
             leader schedule 4\n"
        );
    }

    #[test]
    fn summary_says_an_unreadable_cache_was_kept_and_not_written() {
        let text = summary(&Report {
            schedule_source: ScheduleSource::RpcNotCached,
            ..report()
        });

        assert!(text.contains(
            "leader schedule: fetched; existing unreadable file kept at \
             out/clarion-pulsar-leader-slots-devnet-7.csv\n"
        ));
    }

    #[test]
    fn long_help_says_the_cache_is_read_only_while_present_and_readable() {
        assert!(long_help().contains("getLeaderSchedule while present and readable"));
    }
}

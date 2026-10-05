use std::{error::Error, path::PathBuf, process::ExitCode, time::SystemTime};

use clap::Parser;
use clarion_probe::{
    config::{FileConfig, Overrides, ProbeConfig},
    output::utc_stamp,
    probe::Prober,
    rpc::{RpcClient, cluster_label},
    targets::select_targets,
};

#[derive(Debug, Parser)]
#[command(
    name = "clarion-probe",
    version,
    about = "TPU QUIC handshake RTT prober"
)]
struct Cli {
    #[arg(long, value_name = "PATH", help = "TOML configuration file")]
    config: Option<PathBuf>,
    #[arg(
        long,
        value_name = "URL",
        help = "JSON-RPC endpoint, overrides the file"
    )]
    rpc_url: Option<String>,
    #[arg(long, value_name = "LABEL", help = "Vantage label, overrides the file")]
    vantage: Option<String>,
}

#[tokio::main]
async fn main() -> ExitCode {
    match run(Cli::parse()).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("clarion-probe: {error}");
            ExitCode::FAILURE
        }
    }
}

async fn run(cli: Cli) -> Result<(), Box<dyn Error>> {
    let overrides = Overrides {
        rpc_url: cli.rpc_url,
        vantage: cli.vantage,
    };
    let config =
        ProbeConfig::resolve(FileConfig::load(cli.config.as_deref())?, overrides)?;
    let stamp = utc_stamp(SystemTime::now())?;
    let rpc = RpcClient::new(config.rpc_url.as_str())?;
    let genesis_hash = rpc.genesis_hash().await?;
    let cluster = cluster_label(&genesis_hash);
    let nodes = rpc.cluster_nodes().await?;
    let targets = select_targets(&nodes);
    let prober = Prober::new(&config)?;
    let (path, samples) = prober
        .run(&targets, cluster, &config.output_dir, &stamp)
        .await?;
    prober.drain().await;
    let completed = samples.iter().filter(|sample| sample.ok).count();
    println!(
        "{}: cluster {cluster}, {} of {} nodes targeted, {completed} of {} handshakes ok",
        path.display(),
        targets.len(),
        nodes.len(),
        samples.len(),
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use clap::CommandFactory;

    use super::*;

    #[test]
    fn command_definition_is_consistent() {
        Cli::command().debug_assert();
    }

    #[test]
    fn every_flag_is_optional() {
        let cli = Cli::try_parse_from(["clarion-probe"]).unwrap();

        assert_eq!(cli.config, None);
        assert_eq!(cli.rpc_url, None);
        assert_eq!(cli.vantage, None);
    }

    #[test]
    fn flags_are_parsed_by_their_long_names() {
        let cli = Cli::try_parse_from([
            "clarion-probe",
            "--config",
            "probe.toml",
            "--rpc-url",
            "https://api.mainnet-beta.solana.com",
            "--vantage",
            "fra-1",
        ])
        .unwrap();

        assert_eq!(cli.config, Some(PathBuf::from("probe.toml")));
        assert_eq!(
            cli.rpc_url.as_deref(),
            Some("https://api.mainnet-beta.solana.com")
        );
        assert_eq!(cli.vantage.as_deref(), Some("fra-1"));
    }

    #[test]
    fn unknown_flag_is_rejected() {
        assert!(Cli::try_parse_from(["clarion-probe", "--cluster", "devnet"]).is_err());
    }
}

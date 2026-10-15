use std::{
    error::Error,
    path::{Path, PathBuf},
    process::ExitCode,
    time::Duration,
};

use clap::{Parser, Subcommand, error::ErrorKind};
use clarion_program_client::{
    buffer_upload::{PassReport, WriteOptions, read_status, write_buffer},
    paced_rpc::{HttpTransport, MAX_REQUESTS_PER_SECOND, PacedRpc, SystemClock},
};
use solana_keypair::{Keypair, read_keypair_file};
use solana_program::pubkey::Pubkey;

const USAGE_EXIT_CODE: u8 = 2;

#[derive(Debug, Parser)]
#[command(
    name = "clarion-buffer",
    version,
    about = "Paced status and Write of an upgradeable-loader buffer over public JSON-RPC",
    disable_help_subcommand = true
)]
struct Cli {
    #[arg(long, value_name = "URL", help = "JSON-RPC endpoint")]
    rpc_url: String,
    #[arg(
        long,
        value_name = "RATE",
        default_value_t = 2.0,
        value_parser = parse_rate,
        help = "Ceiling on JSON-RPC requests per second, reads and sends alike, \
                above 0 and at most 3.5"
    )]
    max_requests_per_second: f64,
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    #[command(
        about = "Compare the buffer with the program file in one getAccountInfo and print \
                 JSON; chunks are sized for a separate fee payer"
    )]
    Status {
        #[arg(long, value_name = "PUBKEY", help = "Buffer account, base58")]
        buffer: Pubkey,
        #[arg(long, value_name = "PATH", help = "Program .so file")]
        program_so: PathBuf,
    },
    #[command(
        about = "Send one loader Write per differing chunk, re-read after each pass, and \
                 stop once the buffer holds the program file"
    )]
    Write {
        #[arg(long, value_name = "PUBKEY", help = "Buffer account, base58")]
        buffer: Pubkey,
        #[arg(long, value_name = "PATH", help = "Program .so file")]
        program_so: PathBuf,
        #[arg(long, value_name = "PATH", help = "Buffer authority keypair file")]
        authority: PathBuf,
        #[arg(
            long,
            value_name = "PATH",
            help = "Fee payer keypair file, may be the authority"
        )]
        fee_payer: PathBuf,
        #[arg(
            long,
            value_name = "SECONDS",
            default_value_t = 20,
            help = "Wait after each pass before re-reading the buffer"
        )]
        settle_seconds: u64,
        #[arg(
            long,
            value_name = "COUNT",
            default_value_t = 10,
            value_parser = clap::value_parser!(u32).range(1..),
            help = "Write passes before giving up"
        )]
        max_passes: u32,
    },
}

fn parse_rate(text: &str) -> Result<f64, String> {
    let rate: f64 = text
        .parse()
        .map_err(|_| format!("{text} is not a number"))?;
    if rate.is_finite() && rate > 0.0 && rate <= MAX_REQUESTS_PER_SECOND {
        Ok(rate)
    } else {
        Err(format!(
            "{text} is outside (0, {MAX_REQUESTS_PER_SECOND}] requests per second"
        ))
    }
}

fn main() -> ExitCode {
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(error) => return report_parse_outcome(&error),
    };
    match run(&cli) {
        Ok(Outcome::Complete(output)) => {
            println!("{output}");
            ExitCode::SUCCESS
        }
        Ok(Outcome::Incomplete { output, reason }) => {
            println!("{output}");
            eprintln!("clarion-buffer: {reason}");
            ExitCode::FAILURE
        }
        Err(error) => {
            eprintln!("clarion-buffer: {}", one_line(&error_chain(error.as_ref())));
            ExitCode::FAILURE
        }
    }
}

enum Outcome {
    Complete(String),
    Incomplete { output: String, reason: String },
}

fn report_parse_outcome(error: &clap::Error) -> ExitCode {
    if error.use_stderr() {
        eprintln!("clarion-buffer: {}", usage_line(error));
        return ExitCode::from(USAGE_EXIT_CODE);
    }
    match error.print() {
        Ok(()) => ExitCode::SUCCESS,
        Err(_) => ExitCode::FAILURE,
    }
}

fn usage_line(error: &clap::Error) -> String {
    let rendered = error.render().to_string();
    let mut paragraphs = rendered.split("\n\n");
    let cause = paragraphs.next().unwrap_or_default();
    let usage = paragraphs
        .find(|paragraph| paragraph.starts_with("Usage: "))
        .filter(|_| error.kind() == ErrorKind::UnknownArgument)
        .unwrap_or_default();
    one_line(&format!("{} {usage}", cause.trim_start_matches("error: ")))
}

fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn error_chain(error: &dyn Error) -> String {
    let mut rendered = error.to_string();
    let mut source = error.source();
    while let Some(cause) = source {
        let text = cause.to_string();
        if !rendered.ends_with(&text) {
            rendered.push_str(": ");
            rendered.push_str(&text);
        }
        source = cause.source();
    }
    rendered
}

fn load_keypair(role: &str, path: &Path) -> Result<Keypair, Box<dyn Error>> {
    read_keypair_file(path).map_err(|error| {
        format!("failed to read {role} keypair {}: {error}", path.display()).into()
    })
}

fn load_program(path: &Path) -> Result<Vec<u8>, Box<dyn Error>> {
    std::fs::read(path).map_err(|error| {
        format!("failed to read program {}: {error}", path.display()).into()
    })
}

fn progress_line(report: &PassReport) -> String {
    format!(
        "pass {}/{}: {} of {} chunks differed, {} writes sent, {} failed",
        report.pass,
        report.max_passes,
        report.chunks_differing,
        report.chunks_total,
        report.transactions_sent,
        report.send_errors
    )
}

fn run(cli: &Cli) -> Result<Outcome, Box<dyn Error>> {
    match &cli.command {
        Command::Status { buffer, program_so } => {
            let program = load_program(program_so)?;
            let mut rpc = paced_rpc(cli)?;
            let status = read_status(&mut rpc, buffer, &program)?;
            Ok(Outcome::Complete(serde_json::to_string_pretty(&status)?))
        }
        Command::Write {
            buffer,
            program_so,
            authority,
            fee_payer,
            settle_seconds,
            max_passes,
        } => {
            let program = load_program(program_so)?;
            let authority = load_keypair("authority", authority)?;
            let fee_payer = load_keypair("fee payer", fee_payer)?;
            let mut rpc = paced_rpc(cli)?;
            let options = WriteOptions {
                max_passes: *max_passes,
                settle: Duration::from_secs(*settle_seconds),
            };
            let summary = write_buffer(
                &mut rpc,
                buffer,
                &program,
                &authority,
                &fee_payer,
                options,
                |report| eprintln!("clarion-buffer: {}", progress_line(report)),
            )?;
            let output = serde_json::to_string_pretty(&summary)?;
            if summary.complete {
                Ok(Outcome::Complete(output))
            } else {
                Ok(Outcome::Incomplete {
                    output,
                    reason: format!(
                        "buffer {buffer} still differs from {} in {} of {} chunks after \
                         {} passes",
                        program_so.display(),
                        summary.chunks_missing,
                        summary.chunks_total,
                        summary.passes
                    ),
                })
            }
        }
    }
}

fn paced_rpc(cli: &Cli) -> Result<PacedRpc<HttpTransport, SystemClock>, Box<dyn Error>> {
    Ok(PacedRpc::new(
        HttpTransport::new(&cli.rpc_url)?,
        SystemClock,
        cli.max_requests_per_second,
    ))
}

#[cfg(test)]
mod tests {
    use clap::CommandFactory;

    use super::*;

    const BUFFER: &str = "zon8JcJQDxGTTusNvGdtZR1G9yoBQRzpSFDY4zVRC6G";

    fn parse(arguments: &[&str]) -> Result<Cli, clap::Error> {
        let globals = ["clarion-buffer", "--rpc-url", "http://127.0.0.1:8899"];
        Cli::try_parse_from(globals.iter().chain(arguments))
    }

    fn write_arguments() -> Vec<&'static str> {
        vec![
            "write",
            "--buffer",
            BUFFER,
            "--program-so",
            "clarion.so",
            "--authority",
            "authority.json",
            "--fee-payer",
            "payer.json",
        ]
    }

    #[test]
    fn command_definition_is_consistent() {
        Cli::command().debug_assert();
    }

    #[test]
    fn subcommands_are_status_and_write() {
        let command = Cli::command();
        let names: Vec<&str> = command
            .get_subcommands()
            .map(|subcommand| subcommand.get_name())
            .collect();

        assert_eq!(names, ["status", "write"]);
    }

    #[test]
    fn status_takes_buffer_and_program_with_the_default_rate() {
        let cli =
            parse(&["status", "--buffer", BUFFER, "--program-so", "clarion.so"]).unwrap();

        assert_eq!(cli.max_requests_per_second, 2.0);
        assert!(matches!(
            cli.command,
            Command::Status { buffer, program_so }
                if buffer.to_string() == BUFFER && program_so == Path::new("clarion.so")
        ));
    }

    #[test]
    fn write_takes_both_keypairs_with_settle_and_pass_defaults() {
        let cli = parse(&write_arguments()).unwrap();

        assert!(matches!(
            cli.command,
            Command::Write {
                settle_seconds: 20,
                max_passes: 10,
                ref authority,
                ref fee_payer,
                ..
            } if authority == Path::new("authority.json")
                && fee_payer == Path::new("payer.json")
        ));
    }

    #[test]
    fn write_requires_the_authority_and_the_fee_payer() {
        let error = parse(&["write", "--buffer", BUFFER, "--program-so", "clarion.so"])
            .unwrap_err();

        assert_eq!(error.kind(), ErrorKind::MissingRequiredArgument);
        assert_eq!(
            usage_line(&error),
            "the following required arguments were not provided: --authority <PATH> \
             --fee-payer <PATH>"
        );
    }

    #[test]
    fn rate_accepts_up_to_three_and_a_half() {
        for (rate, expected) in [("0.5", 0.5), ("3.5", 3.5), ("0.01", 0.01)] {
            let cli = parse(&[
                "--max-requests-per-second",
                rate,
                "status",
                "--buffer",
                BUFFER,
                "--program-so",
                "clarion.so",
            ])
            .unwrap();

            assert_eq!(cli.max_requests_per_second, expected);
        }
    }

    #[test]
    fn rate_outside_the_range_is_a_usage_failure() {
        for rate in ["0", "-1", "3.51", "4", "8", "NaN", "inf", "fast"] {
            let option = format!("--max-requests-per-second={rate}");
            let error = parse(&[
                &option,
                "status",
                "--buffer",
                BUFFER,
                "--program-so",
                "clarion.so",
            ])
            .unwrap_err();

            assert_eq!(error.kind(), ErrorKind::ValueValidation, "{rate}");
        }
    }

    #[test]
    fn max_passes_must_be_at_least_one() {
        let error = parse(&[&write_arguments()[..], &["--max-passes", "0"]].concat())
            .unwrap_err();

        assert_eq!(error.kind(), ErrorKind::ValueValidation);
    }

    #[test]
    fn buffer_must_be_base58_of_a_pubkey() {
        let error = parse(&[
            "status",
            "--buffer",
            "not-a-pubkey",
            "--program-so",
            "clarion.so",
        ])
        .unwrap_err();

        assert_eq!(error.kind(), ErrorKind::ValueValidation);
    }

    #[test]
    fn rpc_url_is_required() {
        let error = Cli::try_parse_from([
            "clarion-buffer",
            "status",
            "--buffer",
            BUFFER,
            "--program-so",
            "clarion.so",
        ])
        .unwrap_err();

        assert_eq!(
            usage_line(&error),
            "the following required arguments were not provided: --rpc-url <URL>"
        );
    }

    #[test]
    fn there_is_no_subcommand_beyond_status_and_write() {
        for subcommand in ["upgrade", "set-authority", "close", "deploy"] {
            let error = parse(&[subcommand]).unwrap_err();

            assert_eq!(error.kind(), ErrorKind::InvalidSubcommand, "{subcommand}");
        }
    }

    #[test]
    fn help_and_version_are_not_failures() {
        for arguments in [
            vec!["clarion-buffer", "--help"],
            vec!["clarion-buffer", "--version"],
            vec!["clarion-buffer", "status", "--help"],
            vec!["clarion-buffer", "write", "--help"],
        ] {
            let error = Cli::try_parse_from(arguments).unwrap_err();

            assert!(!error.use_stderr(), "{error}");
        }
    }

    #[test]
    fn progress_line_reports_the_pass_counts() {
        let line = progress_line(&PassReport {
            pass: 2,
            max_passes: 10,
            chunks_differing: 5,
            chunks_total: 120,
            transactions_sent: 5,
            send_errors: 1,
        });

        assert_eq!(
            line,
            "pass 2/10: 5 of 120 chunks differed, 5 writes sent, 1 failed"
        );
    }
}

use std::{error::Error, fs, path::PathBuf, process::ExitCode};

use clap::{Parser, Subcommand, error::ErrorKind};
use clarion_formulae::{
    Binding, Declaration, Inputs, Meta, bind, build, formulae_path, load_formulae,
    load_record, load_window, parse_input, verify, write_record,
};
use solana_keypair::Signature;
use solana_program::pubkey::Pubkey;

const USAGE_EXIT_CODE: u8 = 2;

#[derive(Debug, Parser)]
#[command(
    name = "clarion-formulae",
    version,
    about = "Formulae ids, attestation records, chain binding and offline verification"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    #[command(about = "Print the id of a formulae document")]
    Id {
        #[arg(value_name = "FILE", help = "Formulae document")]
        formulae: PathBuf,
    },
    #[command(about = "Build a record from the artifacts and the declared inputs")]
    Build {
        #[arg(long, value_name = "FILE", help = "Formulae document")]
        formulae: PathBuf,
        #[arg(long, value_name = "DIR", help = "Directory holding the artifacts")]
        artifacts: PathBuf,
        #[arg(
            long,
            value_name = "GROUP.FIELD=VALUE",
            help = "Value of a field declared with source input; one per declared input"
        )]
        input: Vec<String>,
        #[arg(long, value_name = "FILE", help = "Record to write")]
        out: PathBuf,
        #[arg(long, value_name = "URL", help = "Unattested meta.source_repo")]
        source_repo: Option<String>,
    },
    #[command(about = "Bind a built record to its revealed window")]
    Bind {
        #[arg(long, value_name = "FILE", help = "Record to bind, rewritten in place")]
        attestation: PathBuf,
        #[arg(long, value_name = "FILE", help = "clarion-admin show output")]
        window: PathBuf,
        #[arg(
            long,
            value_name = "NAME",
            help = "Cluster name, devnet or mainnet-beta"
        )]
        cluster: String,
        #[arg(long, value_name = "PUBKEY", help = "Clarion program id, base58")]
        program_id: Pubkey,
        #[arg(long, value_name = "SIGNATURE", help = "Commit transaction signature")]
        commit_signature: Signature,
        #[arg(long, value_name = "SIGNATURE", help = "Reveal transaction signature")]
        reveal_signature: Signature,
    },
    #[command(about = "Verify a record: layers 1 and 2 always, layer 3 with --artifacts")]
    Verify {
        #[arg(long, value_name = "FILE", help = "Record to verify")]
        attestation: PathBuf,
        #[arg(long, value_name = "FILE", help = "clarion-admin show output")]
        window: PathBuf,
        #[arg(
            long,
            value_name = "FILE",
            help = "Formulae document; default <formulae-dir>/<record formulae name>.json"
        )]
        formulae: Option<PathBuf>,
        #[arg(
            long,
            value_name = "DIR",
            default_value = "formulae",
            help = "Formulae directory"
        )]
        formulae_dir: PathBuf,
        #[arg(
            long,
            value_name = "DIR",
            help = "Artifacts for layer 3; files its withheld.json declares are not re-derived"
        )]
        artifacts: Option<PathBuf>,
        #[arg(
            long,
            requires = "artifacts",
            help = "Ignore withheld.json and re-derive every field from the artifacts"
        )]
        ignore_withheld: bool,
    },
}

struct Outcome {
    stdout: String,
    success: bool,
}

fn main() -> ExitCode {
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(error) => return report_parse_outcome(&error),
    };
    match run(cli) {
        Ok(outcome) => {
            print!("{}", outcome.stdout);
            if outcome.success {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            }
        }
        Err(error) => {
            eprintln!(
                "clarion-formulae: {}",
                one_line(&error_chain(error.as_ref()))
            );
            ExitCode::FAILURE
        }
    }
}

fn run(cli: Cli) -> Result<Outcome, Box<dyn Error>> {
    match cli.command {
        Command::Id { formulae } => {
            let (_, id) = load_formulae(&formulae)?;
            Ok(Outcome {
                stdout: format!("{id} {}\n", id.hex()),
                success: true,
            })
        }
        Command::Build {
            formulae,
            artifacts,
            input,
            out,
            source_repo,
        } => {
            let (formulae, id) = load_formulae(&formulae)?;
            let mut inputs = Inputs::new();
            for text in &input {
                let (key, value) = parse_input(text)?;
                inputs.insert(key, value);
            }
            match build(&formulae, id, &artifacts, &inputs, Meta::now(source_repo)) {
                Ok(record) => {
                    write_record(&out, &record)?;
                    Ok(Outcome {
                        stdout: record.pretty()?,
                        success: true,
                    })
                }
                Err(report) => Ok(Outcome {
                    stdout: report.pretty()?,
                    success: false,
                }),
            }
        }
        Command::Bind {
            attestation,
            window,
            cluster,
            program_id,
            commit_signature,
            reveal_signature,
        } => {
            let record = load_record(&attestation)?;
            let window = load_window(&window)?;
            let binding = Binding {
                cluster,
                program_id,
                commit_signature,
                reveal_signature,
            };
            let bound = bind(record, &window, &binding)?;
            write_record(&attestation, &bound)?;
            Ok(Outcome {
                stdout: bound.pretty()?,
                success: true,
            })
        }
        Command::Verify {
            attestation,
            window,
            formulae,
            formulae_dir,
            artifacts,
            ignore_withheld,
        } => {
            let record = load_record(&attestation)?;
            let window = load_window(&window)?;
            let path = formulae
                .unwrap_or_else(|| formulae_path(&formulae_dir, &record.formulae.name));
            let text = fs::read_to_string(&path)
                .map_err(|error| format!("formulae file {}: {error}", path.display()));
            let verification = verify(
                &record,
                text.as_deref().map_err(Clone::clone),
                &window,
                artifacts.as_deref(),
                if ignore_withheld {
                    Declaration::Ignore
                } else {
                    Declaration::Read
                },
            );
            Ok(Outcome {
                stdout: verification.to_string(),
                success: verification.passed(),
            })
        }
    }
}

fn report_parse_outcome(error: &clap::Error) -> ExitCode {
    if error.use_stderr() {
        eprintln!("clarion-formulae: {}", usage_line(error));
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

use std::{
    error::Error,
    path::{Path, PathBuf},
    process::ExitCode,
};

use clap::{
    Parser, Subcommand,
    error::{ContextKind, ContextValue, ErrorKind},
};
use clarion_program_client::{
    ARM_COUNT, ArmAggregates, ClarionError, State, authority_transaction, commit,
    decode_state, decode_window, init, parse_aggregates, parse_merkle_root,
    render_window, reveal, state_address, window_address, window_slots,
};
use solana_commitment_config::CommitmentConfig;
use solana_keypair::{Keypair, Signature, Signer, read_keypair_file};
use solana_program::{
    instruction::{Instruction, InstructionError},
    pubkey::Pubkey,
};
use solana_rpc_client::{
    api::client_error::{Error as ClientError, TransactionError},
    rpc_client::{RpcClient, SerializableTransaction},
};

const USAGE_EXIT_CODE: u8 = 2;

#[derive(Debug, Parser)]
#[command(
    name = "clarion-admin",
    version,
    about = "Init, Commit, Reveal and show for the clarion program",
    arg_required_else_help = false,
    disable_help_subcommand = true
)]
struct Cli {
    #[arg(long, value_name = "URL", help = "JSON-RPC endpoint")]
    rpc_url: String,
    #[arg(long, value_name = "PUBKEY", help = "Clarion program id, base58")]
    program_id: Pubkey,
    #[arg(
        long,
        value_name = "PATH",
        help = "Authority keypair file, required by init, commit and reveal"
    )]
    authority: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    #[command(flatten)]
    Submit(Submission),
    #[command(about = "Print a window account as JSON")]
    Show {
        #[arg(long, value_name = "ID", help = "Window to print")]
        window_id: u64,
    },
}

#[derive(Debug, Subcommand)]
enum Submission {
    #[command(
        about = "Create the state account, signed by the authority and the program keypair"
    )]
    Init {
        #[arg(long, value_name = "SLOT", help = "First slot of window 0")]
        genesis_slot: u64,
        #[arg(long, value_name = "SLOTS", help = "Slots per window")]
        window_len: u64,
        #[arg(
            long,
            value_name = "SLOTS",
            help = "Minimum slots between Commit and Reveal"
        )]
        lag: u64,
        #[arg(
            long,
            value_name = "PATH",
            help = "Program keypair file, its pubkey must equal --program-id"
        )]
        program_keypair: PathBuf,
    },
    #[command(about = "Commit a merkle root on the slot bounds of the on-chain grid")]
    Commit {
        #[arg(long, value_name = "ID", help = "Window to commit")]
        window_id: u64,
        #[arg(
            long,
            value_name = "HEX",
            value_parser = parse_merkle_root,
            help = "Merkle root, 64 hex characters"
        )]
        root: [u8; 32],
    },
    #[command(about = "Reveal the four arm aggregates of a committed window")]
    Reveal {
        #[arg(long, value_name = "ID", help = "Window to reveal")]
        window_id: u64,
        #[arg(
            long,
            value_name = "JSON",
            value_parser = parse_aggregates,
            help = "Array of four objects with tx_submitted, tx_landed, stl_p50_slots, \
                    stl_p90_slots, cu_price_p50_micro, cu_price_p90_micro, \
                    fee_total_lamports, tip_total_lamports"
        )]
        aggregates: [ArmAggregates; ARM_COUNT],
    },
}

#[derive(Debug, thiserror::Error)]
enum SubmitError {
    #[error("transaction {signature}: {rejection:?} ({}): {rejection}", .rejection.code())]
    Rejected {
        signature: Signature,
        rejection: ClarionError,
    },
    #[error("transaction {signature}: {source}")]
    Failed {
        signature: Signature,
        #[source]
        source: ClientError,
    },
}

impl SubmitError {
    fn new(signature: Signature, source: ClientError) -> Self {
        match rejection(&source) {
            Some(rejection) => Self::Rejected {
                signature,
                rejection,
            },
            None => Self::Failed { signature, source },
        }
    }
}

fn main() -> ExitCode {
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(error) => return report_parse_outcome(&error),
    };
    let rpc = RpcClient::new_with_commitment(
        cli.rpc_url.clone(),
        CommitmentConfig::confirmed(),
    );
    match run(&cli, &rpc, load_keypair) {
        Ok(output) => {
            println!("{output}");
            ExitCode::SUCCESS
        }
        Err(error) => match error.downcast_ref::<clap::Error>() {
            Some(usage) => report_parse_outcome(usage),
            None => {
                eprintln!("clarion-admin: {}", one_line(&error_chain(error.as_ref())));
                ExitCode::FAILURE
            }
        },
    }
}

fn report_parse_outcome(error: &clap::Error) -> ExitCode {
    if error.use_stderr() {
        eprintln!("clarion-admin: {}", usage_line(error));
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

fn missing_authority() -> clap::Error {
    let mut error = clap::Error::new(ErrorKind::MissingRequiredArgument);
    error.insert(
        ContextKind::InvalidArg,
        ContextValue::Strings(vec!["--authority <PATH>".to_string()]),
    );
    error
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

fn run(
    cli: &Cli,
    rpc: &RpcClient,
    read_keypair: impl Fn(&str, &Path) -> Result<Keypair, Box<dyn Error>>,
) -> Result<String, Box<dyn Error>> {
    let program_id = &cli.program_id;
    match &cli.command {
        Command::Submit(submission) => {
            let authority_path =
                cli.authority.as_deref().ok_or_else(missing_authority)?;
            let authority = read_keypair("authority", authority_path)?;
            if authority.pubkey() == *program_id {
                return Err(format!(
                    "authority keypair {} holds the program id {program_id}, the \
                     authority must be a separate key",
                    authority_path.display()
                )
                .into());
            }
            let cosigners =
                cosigners(submission, program_id, |path| read_keypair("program", path))?;
            let instruction = instruction_for(
                submission,
                program_id,
                &authority.pubkey(),
                || read_state(rpc, program_id),
                |window_id| read_window_data(rpc, program_id, window_id),
            )?;
            submit(rpc, instruction, &authority, &cosigners)
        }
        Command::Show { window_id } => show(rpc, program_id, *window_id),
    }
}

fn instruction_for(
    submission: &Submission,
    program_id: &Pubkey,
    authority: &Pubkey,
    read_state: impl FnOnce() -> Result<State, Box<dyn Error>>,
    read_window_data: impl FnOnce(u64) -> Result<(Pubkey, Vec<u8>), Box<dyn Error>>,
) -> Result<Instruction, Box<dyn Error>> {
    Ok(match submission {
        Submission::Init {
            genesis_slot,
            window_len,
            lag,
            ..
        } => init(program_id, authority, *genesis_slot, *window_len, *lag),
        Submission::Commit { window_id, root } => {
            let (slot_start, slot_end) = window_slots(&read_state()?, *window_id)?;
            commit(
                program_id, authority, *window_id, slot_start, slot_end, *root,
            )
        }
        Submission::Reveal {
            window_id,
            aggregates,
        } => {
            read_state()?;
            read_window_data(*window_id)?;
            reveal(program_id, authority, *window_id, *aggregates)
        }
    })
}

fn load_keypair(role: &str, path: &Path) -> Result<Keypair, Box<dyn Error>> {
    read_keypair_file(path).map_err(|error| {
        format!("failed to read {role} keypair {}: {error}", path.display()).into()
    })
}

fn cosigners(
    submission: &Submission,
    program_id: &Pubkey,
    read_keypair: impl FnOnce(&Path) -> Result<Keypair, Box<dyn Error>>,
) -> Result<Vec<Keypair>, Box<dyn Error>> {
    match submission {
        Submission::Init {
            program_keypair, ..
        } => {
            let keypair = read_keypair(program_keypair)?;
            if keypair.pubkey() == *program_id {
                Ok(vec![keypair])
            } else {
                Err(format!(
                    "program keypair {} holds {}, expected program id {program_id}",
                    program_keypair.display(),
                    keypair.pubkey()
                )
                .into())
            }
        }
        Submission::Commit { .. } | Submission::Reveal { .. } => Ok(Vec::new()),
    }
}

fn account_data(rpc: &RpcClient, key: &Pubkey) -> Result<Option<Vec<u8>>, ClientError> {
    let accounts = rpc.get_multiple_accounts(std::slice::from_ref(key))?;
    Ok(accounts
        .into_iter()
        .next()
        .flatten()
        .map(|account| account.data))
}

fn read_state(rpc: &RpcClient, program_id: &Pubkey) -> Result<State, Box<dyn Error>> {
    let (state_key, _) = state_address(program_id);
    let data = account_data(rpc, &state_key)?
        .ok_or_else(|| format!("state account {state_key} does not exist"))?;
    Ok(decode_state(&data)?)
}

fn read_window_data(
    rpc: &RpcClient,
    program_id: &Pubkey,
    window_id: u64,
) -> Result<(Pubkey, Vec<u8>), Box<dyn Error>> {
    let (window_key, _) = window_address(program_id, window_id);
    let data = account_data(rpc, &window_key)?.ok_or_else(|| {
        format!("window {window_id} account {window_key} does not exist")
    })?;
    Ok((window_key, data))
}

fn show(
    rpc: &RpcClient,
    program_id: &Pubkey,
    window_id: u64,
) -> Result<String, Box<dyn Error>> {
    let (window_key, data) = read_window_data(rpc, program_id, window_id)?;
    Ok(render_window(&window_key, &decode_window(&data)?)?)
}

fn submit(
    rpc: &RpcClient,
    instruction: Instruction,
    authority: &Keypair,
    cosigners: &[Keypair],
) -> Result<String, Box<dyn Error>> {
    let cosigners: Vec<&Keypair> = cosigners.iter().collect();
    let transaction = authority_transaction(
        instruction,
        authority,
        &cosigners,
        rpc.get_latest_blockhash()?,
    )?;
    let signature = *transaction.get_signature();
    match rpc.send_and_confirm_transaction(&transaction) {
        Ok(confirmed) => Ok(confirmed.to_string()),
        Err(error) => Err(SubmitError::new(signature, error).into()),
    }
}

fn rejection(error: &ClientError) -> Option<ClarionError> {
    match error.get_transaction_error()? {
        TransactionError::InstructionError(_, InstructionError::Custom(code)) => {
            ClarionError::from_code(code)
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use clap::CommandFactory;
    use clarion_program_client::{ClarionInstruction, STATE_DISCRIMINATOR};
    use serde_json::{Value, json};
    use solana_rpc_client::{
        api::request::{RpcError, RpcRequest, RpcResponseErrorData},
        mock_sender::{Mocks, MocksMap, PUBKEY},
    };

    use super::*;

    const PROGRAM_ID: &str = "3qbR1eZRqXUWroWKKYhbDmR3FfqTHfqSU8zZSxtANzYh";
    const AUTHORITY: Pubkey = Pubkey::new_from_array([9; 32]);
    const STATE_BASE64: &str = "AQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJ6AMAAAAAAACWAAAA\
                                AAAAAJcAAAAAAAAABwAAAAAAAAD+";
    const INIT: [&str; 7] = [
        "init",
        "--genesis-slot",
        "1000",
        "--window-len",
        "150",
        "--lag",
        "151",
    ];
    const PROGRAM_KEYPAIR_OPTION: [&str; 2] = ["--program-keypair", "program.json"];
    const AUTHORITY_OPTION: [&str; 2] = ["--authority", "authority.json"];
    const ROOT: &str = "0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a";
    const ARM: &str = r#"{"tx_submitted": 10, "tx_landed": 9, "stl_p50_slots": 1,
        "stl_p90_slots": 4, "cu_price_p50_micro": 1000, "cu_price_p90_micro": 9000,
        "fee_total_lamports": 50000, "tip_total_lamports": 25000}"#;

    fn parse(arguments: &[&str]) -> Result<Cli, clap::Error> {
        let globals = [
            "clarion-admin",
            "--rpc-url",
            "http://127.0.0.1:8899",
            "--program-id",
            PROGRAM_ID,
        ];
        Cli::try_parse_from(globals.iter().chain(arguments))
    }

    fn with_authority(arguments: &[&str]) -> Cli {
        parse(&[&AUTHORITY_OPTION[..], arguments].concat()).unwrap()
    }

    fn init_arguments() -> Vec<&'static str> {
        [&INIT[..], &PROGRAM_KEYPAIR_OPTION].concat()
    }

    fn arms(count: usize) -> String {
        format!("[{}]", vec![ARM; count].join(","))
    }

    fn program_id() -> Pubkey {
        Pubkey::new_from_array([42; 32])
    }

    fn submission(arguments: &[&str]) -> Submission {
        match parse(arguments).unwrap().command {
            Command::Submit(submission) => submission,
            Command::Show { window_id } => panic!("show of window {window_id}"),
        }
    }

    fn state() -> State {
        State {
            discriminator: STATE_DISCRIMINATOR,
            authority: AUTHORITY,
            genesis_slot: 1_000,
            window_len: 150,
            min_reveal_lag_slots: 151,
            next_window_id: 7,
            bump: 254,
        }
    }

    fn unread_state() -> Result<State, Box<dyn Error>> {
        Err("state is not read".into())
    }

    fn unread_window(window_id: u64) -> Result<(Pubkey, Vec<u8>), Box<dyn Error>> {
        Err(format!("window {window_id} is not read").into())
    }

    fn present(data: &str, space: usize) -> Value {
        response(json!([{
            "lamports": 1_350_240,
            "data": [data, "base64"],
            "owner": PROGRAM_ID,
            "executable": false,
            "rentEpoch": 0,
            "space": space,
        }]))
    }

    fn landed(code: u32) -> ClientError {
        ClientError::from(TransactionError::InstructionError(
            0,
            InstructionError::Custom(code),
        ))
    }

    fn preflight(code: u32) -> ClientError {
        let simulation = serde_json::from_value(json!({
            "err": {"InstructionError": [0, {"Custom": code}]},
            "logs": ["Program log: first", "Program log: second"],
        }))
        .unwrap();
        ClientError::from(RpcError::RpcResponseError {
            code: -32002,
            message: "Transaction simulation failed".to_string(),
            data: RpcResponseErrorData::SendTransactionPreflightFailure(simulation),
        })
    }

    fn response(value: Value) -> Value {
        json!({"context": {"slot": 1}, "value": value})
    }

    fn signer() -> Keypair {
        Keypair::new_from_array([1; 32])
    }

    fn grid_commit(authority: &Keypair) -> Instruction {
        commit(
            &program_id(),
            &authority.pubkey(),
            7,
            2_050,
            2_199,
            [0x0a; 32],
        )
    }

    fn signature_on_the_mock_blockhash(
        instruction: Instruction,
        authority: &Keypair,
        cosigners: &[&Keypair],
    ) -> Signature {
        let transaction = authority_transaction(
            instruction,
            authority,
            cosigners,
            PUBKEY.parse().unwrap(),
        )
        .unwrap();
        *transaction.get_signature()
    }

    #[test]
    fn command_definition_is_consistent() {
        Cli::command().debug_assert();
    }

    #[test]
    fn subcommands_are_exactly_init_commit_reveal_show() {
        let command = Cli::command();
        let names: Vec<&str> = command
            .get_subcommands()
            .map(|subcommand| subcommand.get_name())
            .collect();

        assert_eq!(names, ["init", "commit", "reveal", "show"]);
    }

    #[test]
    fn authority_has_no_default() {
        let cli = parse(&["show", "--window-id", "0"]).unwrap();

        assert_eq!(cli.authority, None);
        assert_eq!(cli.rpc_url, "http://127.0.0.1:8899");
        assert_eq!(cli.program_id, Pubkey::new_from_array([42; 32]));
    }

    #[test]
    fn authority_takes_a_path() {
        let cli = Cli::try_parse_from([
            "clarion-admin",
            "--rpc-url",
            "http://127.0.0.1:8899",
            "--program-id",
            PROGRAM_ID,
            "--authority",
            "authority.json",
            "show",
            "--window-id",
            "0",
        ])
        .unwrap();

        assert_eq!(cli.authority, Some(PathBuf::from("authority.json")));
    }

    #[test]
    fn rpc_url_and_program_id_are_required() {
        let without_both =
            Cli::try_parse_from(["clarion-admin", "show", "--window-id", "0"]);
        let without_program_id = Cli::try_parse_from([
            "clarion-admin",
            "--rpc-url",
            "http://127.0.0.1:8899",
            "show",
            "--window-id",
            "0",
        ]);

        for outcome in [without_both, without_program_id] {
            assert_eq!(
                outcome.unwrap_err().kind(),
                ErrorKind::MissingRequiredArgument
            );
        }
    }

    #[test]
    fn program_id_must_be_base58_of_a_pubkey() {
        let outcome = Cli::try_parse_from([
            "clarion-admin",
            "--rpc-url",
            "http://127.0.0.1:8899",
            "--program-id",
            "not-a-pubkey",
            "show",
            "--window-id",
            "0",
        ]);

        assert_eq!(outcome.unwrap_err().kind(), ErrorKind::ValueValidation);
    }

    #[test]
    fn init_takes_genesis_slot_window_len_lag_and_program_keypair() {
        let cli = parse(&[
            "init",
            "--genesis-slot",
            "1000",
            "--window-len",
            "150",
            "--lag",
            "151",
            "--program-keypair",
            "program.json",
        ])
        .unwrap();

        assert!(matches!(
            cli.command,
            Command::Submit(Submission::Init {
                genesis_slot: 1_000,
                window_len: 150,
                lag: 151,
                program_keypair,
            }) if program_keypair == Path::new("program.json")
        ));
    }

    #[test]
    fn init_requires_the_program_keypair() {
        let error = parse(&INIT).unwrap_err();

        assert_eq!(error.kind(), ErrorKind::MissingRequiredArgument);
        assert_eq!(
            usage_line(&error),
            "the following required arguments were not provided: --program-keypair <PATH>"
        );
    }

    #[test]
    fn program_keypair_belongs_to_init_alone() {
        for subcommand in ["commit", "reveal", "show"] {
            let outcome = parse(&[subcommand, "--program-keypair", "program.json"]);

            assert_eq!(outcome.unwrap_err().kind(), ErrorKind::UnknownArgument);
        }
    }

    #[test]
    fn commit_takes_window_id_and_a_hex_root() {
        let cli = parse(&["commit", "--window-id", "7", "--root", ROOT]).unwrap();

        assert!(matches!(
            cli.command,
            Command::Submit(Submission::Commit { window_id: 7, root })
                if root == [0x0a; 32]
        ));
    }

    #[test]
    fn commit_takes_no_slot_bounds() {
        for flag in ["--slot-start", "--slot-end"] {
            let outcome =
                parse(&["commit", "--window-id", "7", "--root", ROOT, flag, "1"]);

            assert_eq!(outcome.unwrap_err().kind(), ErrorKind::UnknownArgument);
        }
    }

    #[test]
    fn commit_rejects_a_root_that_is_not_64_hex_characters() {
        for root in ["0a0a", "zz".repeat(32).as_str()] {
            let error =
                parse(&["commit", "--window-id", "7", "--root", root]).unwrap_err();

            assert_eq!(error.kind(), ErrorKind::ValueValidation);
            assert!(error.to_string().contains("merkle root"), "{error}");
        }
    }

    #[test]
    fn reveal_takes_window_id_and_four_arms_of_json() {
        let cli =
            parse(&["reveal", "--window-id", "3", "--aggregates", &arms(4)]).unwrap();
        let expected = ArmAggregates {
            tx_submitted: 10,
            tx_landed: 9,
            stl_p50_slots: 1,
            stl_p90_slots: 4,
            cu_price_p50_micro: 1_000,
            cu_price_p90_micro: 9_000,
            fee_total_lamports: 50_000,
            tip_total_lamports: 25_000,
        };

        assert!(matches!(
            cli.command,
            Command::Submit(Submission::Reveal { window_id: 3, aggregates })
                if aggregates == [expected; ARM_COUNT]
        ));
    }

    #[test]
    fn reveal_rejects_another_arm_count() {
        for count in [3, 5] {
            let error =
                parse(&["reveal", "--window-id", "3", "--aggregates", &arms(count)])
                    .unwrap_err();

            assert_eq!(error.kind(), ErrorKind::ValueValidation);
            assert!(error.to_string().contains("expected 4"), "{error}");
        }
    }

    #[test]
    fn show_takes_window_id() {
        let cli = parse(&["show", "--window-id", "12"]).unwrap();

        assert!(matches!(cli.command, Command::Show { window_id: 12 }));
    }

    #[test]
    fn window_id_must_be_a_u64() {
        for value in ["-1", "18446744073709551616", "first"] {
            let error = parse(&["show", "--window-id", value]).unwrap_err();

            assert!(error.use_stderr(), "{error}");
        }
    }

    #[test]
    fn usage_failures_collapse_to_their_cause_on_one_line() {
        let missing = Cli::try_parse_from(["clarion-admin", "show", "--window-id", "0"])
            .unwrap_err();
        let invalid =
            parse(&["commit", "--window-id", "7", "--root", "0a0a"]).unwrap_err();

        let missing_line = usage_line(&missing);
        let invalid_line = usage_line(&invalid);

        assert_eq!(
            missing_line,
            "the following required arguments were not provided: --rpc-url <URL> \
             --program-id <PUBKEY>"
        );
        assert_eq!(
            invalid_line,
            "invalid value '0a0a' for '--root <HEX>': merkle root holds 4 characters, \
             expected 64 hex"
        );
    }

    #[test]
    fn unknown_argument_failure_alone_carries_the_usage_on_its_line() {
        let misplaced = parse(&[
            "show",
            "--window-id",
            "0",
            "--rpc-url",
            "http://127.0.0.1:8899",
        ])
        .unwrap_err();
        let missing = Cli::try_parse_from(["clarion-admin", "show", "--window-id", "0"])
            .unwrap_err();

        assert_eq!(misplaced.kind(), ErrorKind::UnknownArgument);
        assert_eq!(
            usage_line(&misplaced),
            "unexpected argument '--rpc-url' found Usage: clarion-admin --rpc-url <URL> \
             --program-id <PUBKEY> show --window-id <ID>"
        );
        assert_eq!(missing.kind(), ErrorKind::MissingRequiredArgument);
        assert!(missing.render().to_string().contains("Usage: "));
        assert!(!usage_line(&missing).contains("Usage: "));
    }

    #[test]
    fn invocation_without_a_subcommand_is_a_usage_failure_listing_the_subcommands() {
        for outcome in [Cli::try_parse_from(["clarion-admin"]), parse(&[])] {
            let error = outcome.unwrap_err();

            assert_eq!(error.kind(), ErrorKind::MissingSubcommand);
            assert!(error.use_stderr());
            assert_eq!(
                usage_line(&error),
                "'clarion-admin' requires a subcommand but one was not provided \
                 [subcommands: init, commit, reveal, show]"
            );
        }
    }

    #[test]
    fn help_and_version_are_not_failures() {
        for arguments in [
            vec!["clarion-admin", "--help"],
            vec!["clarion-admin", "--version"],
            vec!["clarion-admin", "init", "--help"],
            vec!["clarion-admin", "commit", "--help"],
            vec!["clarion-admin", "reveal", "--help"],
            vec!["clarion-admin", "show", "--help"],
        ] {
            let error = Cli::try_parse_from(arguments).unwrap_err();

            assert!(!error.use_stderr(), "{error}");
        }
    }

    #[test]
    fn one_line_joins_lines_and_squeezes_whitespace() {
        assert_eq!(
            one_line("simulation failed\n  log one\n  log two\n"),
            "simulation failed log one log two"
        );
        assert_eq!(one_line("already one line"), "already one line");
        assert_eq!(one_line(""), "");
    }

    #[test]
    fn missing_authority_file_is_reported_with_its_path() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("absent/authority.json");

        let error = load_keypair("authority", &path).unwrap_err().to_string();

        assert!(
            error.starts_with("failed to read authority keypair "),
            "{error}"
        );
        assert!(error.contains("absent/authority.json"), "{error}");
    }

    #[test]
    fn init_arguments_reach_the_instruction_in_program_order() {
        let instruction = instruction_for(
            &submission(&init_arguments()),
            &program_id(),
            &AUTHORITY,
            unread_state,
            unread_window,
        )
        .unwrap();

        assert_eq!(
            ClarionInstruction::unpack(&instruction.data),
            Ok(ClarionInstruction::Init {
                genesis_slot: 1_000,
                window_len: 150,
                min_reveal_lag_slots: 151,
            })
        );
        assert_eq!(
            instruction,
            init(&program_id(), &AUTHORITY, 1_000, 150, 151)
        );
    }

    #[test]
    fn commit_takes_its_bounds_from_the_grid_of_the_read_state() {
        let submission = submission(&["commit", "--window-id", "7", "--root", ROOT]);

        let instruction = instruction_for(
            &submission,
            &program_id(),
            &AUTHORITY,
            || Ok(state()),
            unread_window,
        )
        .unwrap();

        assert_eq!(
            ClarionInstruction::unpack(&instruction.data),
            Ok(ClarionInstruction::Commit {
                window_id: 7,
                slot_start: 2_050,
                slot_end: 2_199,
                merkle_root: [0x0a; 32],
            })
        );
        assert_eq!(
            instruction,
            commit(&program_id(), &AUTHORITY, 7, 2_050, 2_199, [0x0a; 32])
        );
    }

    #[test]
    fn commit_reports_a_state_that_cannot_be_read() {
        let submission = submission(&["commit", "--window-id", "7", "--root", ROOT]);

        let error = instruction_for(
            &submission,
            &program_id(),
            &AUTHORITY,
            unread_state,
            unread_window,
        )
        .unwrap_err();

        assert_eq!(error.to_string(), "state is not read");
    }

    #[test]
    fn commit_reports_a_window_beyond_the_u64_grid() {
        let submission = submission(&[
            "commit",
            "--window-id",
            "18446744073709551615",
            "--root",
            ROOT,
        ]);

        let error = instruction_for(
            &submission,
            &program_id(),
            &AUTHORITY,
            || Ok(state()),
            unread_window,
        )
        .unwrap_err();

        assert_eq!(
            error.to_string(),
            "window 18446744073709551615 has no u64 bounds on the grid of genesis_slot \
             1000 and window_len 150"
        );
    }

    #[test]
    fn reveal_arguments_reach_the_instruction_after_reading_state_then_window() {
        let text = format!(
            "[{}]",
            ["10", "20", "30", "40"]
                .map(|submitted| ARM.replace(
                    "\"tx_submitted\": 10",
                    &format!("\"tx_submitted\": {submitted}")
                ))
                .join(",")
        );
        let aggregates = [10, 20, 30, 40].map(|tx_submitted| ArmAggregates {
            tx_submitted,
            tx_landed: 9,
            stl_p50_slots: 1,
            stl_p90_slots: 4,
            cu_price_p50_micro: 1_000,
            cu_price_p90_micro: 9_000,
            fee_total_lamports: 50_000,
            tip_total_lamports: 25_000,
        });
        let submission =
            submission(&["reveal", "--window-id", "3", "--aggregates", &text]);
        let reads = RefCell::new(Vec::new());

        let instruction = instruction_for(
            &submission,
            &program_id(),
            &AUTHORITY,
            || {
                reads.borrow_mut().push("state".to_string());
                Ok(state())
            },
            |window_id| {
                reads.borrow_mut().push(format!("window {window_id}"));
                Ok((window_address(&program_id(), window_id).0, Vec::new()))
            },
        )
        .unwrap();

        assert_eq!(*reads.borrow(), ["state", "window 3"]);
        assert_eq!(
            ClarionInstruction::unpack(&instruction.data),
            Ok(ClarionInstruction::Reveal {
                window_id: 3,
                aggregates,
            })
        );
        assert_eq!(
            instruction,
            reveal(&program_id(), &AUTHORITY, 3, aggregates)
        );
    }

    #[test]
    fn commit_and_reveal_read_no_program_keypair() {
        for submission in [
            submission(&["commit", "--window-id", "7", "--root", ROOT]),
            submission(&["reveal", "--window-id", "3", "--aggregates", &arms(4)]),
        ] {
            let cosigners =
                cosigners(&submission, &program_id(), |_| Err("unread".into())).unwrap();

            assert!(cosigners.is_empty());
        }
    }

    #[test]
    fn init_is_cosigned_by_the_keypair_of_the_program_id() {
        let program_id = Keypair::new_from_array([42; 32]).pubkey();

        let cosigners = cosigners(&submission(&init_arguments()), &program_id, |path| {
            assert_eq!(path, Path::new("program.json"));
            Ok(Keypair::new_from_array([42; 32]))
        })
        .unwrap();

        assert_eq!(
            cosigners.iter().map(Keypair::pubkey).collect::<Vec<_>>(),
            [program_id]
        );
    }

    #[test]
    fn init_refuses_the_keypair_of_another_program_id() {
        let held = Keypair::new_from_array([42; 32]);

        let error = cosigners(&submission(&init_arguments()), &program_id(), |_| {
            Ok(held.insecure_clone())
        })
        .unwrap_err();

        assert_eq!(
            error.to_string(),
            format!(
                "program keypair program.json holds {}, expected program id {PROGRAM_ID}",
                held.pubkey()
            )
        );
    }

    #[test]
    fn init_reports_a_missing_program_keypair_file_with_its_path() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("absent/program.json");
        let path = path.to_str().unwrap();
        let submission = submission(&[&INIT[..], &["--program-keypair", path]].concat());

        let error = cosigners(&submission, &program_id(), |path| {
            load_keypair("program", path)
        })
        .unwrap_err()
        .to_string();

        assert!(
            error.starts_with("failed to read program keypair "),
            "{error}"
        );
        assert!(error.contains("absent/program.json"), "{error}");
    }

    #[test]
    fn program_rejections_map_to_their_error_before_and_after_landing() {
        for (code, expected) in [
            (100, ClarionError::InstructionDataMalformed),
            (105, ClarionError::StateAlreadyInitialized),
            (117, ClarionError::WindowIdNotSequential),
            (126, ClarionError::ProgramSignatureMissing),
        ] {
            assert_eq!(rejection(&landed(code)), Some(expected));
            assert_eq!(rejection(&preflight(code)), Some(expected));
        }
    }

    #[test]
    fn failures_outside_the_program_codes_are_not_rejections() {
        for error in [
            landed(99),
            landed(122),
            preflight(122),
            preflight(127),
            ClientError::from(TransactionError::AccountInUse),
            ClientError::from(TransactionError::InstructionError(
                0,
                InstructionError::InvalidArgument,
            )),
            ClientError::from(RpcError::ForUser("unable to confirm".to_string())),
        ] {
            assert_eq!(rejection(&error), None, "{error}");
        }
    }

    #[test]
    fn rejection_line_holds_signature_variant_code_and_text_without_logs() {
        let signature = Signature::from([7; 64]);

        for source in [preflight(117), landed(117)] {
            let error = SubmitError::new(signature, source);

            assert_eq!(
                one_line(&error_chain(&error)),
                format!(
                    "transaction {signature}: WindowIdNotSequential (117): window_id is \
                     not next_window_id"
                )
            );
        }
    }

    #[test]
    fn failure_outside_the_program_codes_keeps_its_text_behind_the_signature() {
        let signature = Signature::from([7; 64]);

        let error = SubmitError::new(signature, preflight(122));

        assert_eq!(
            one_line(&error_chain(&error)),
            format!(
                "transaction {signature}: RPC response error -32002: Transaction \
                 simulation failed; 2 log messages: Program log: first Program log: \
                 second"
            )
        );
    }

    #[test]
    fn error_chain_appends_each_cause_once() {
        #[derive(Debug, thiserror::Error)]
        #[error("request failed")]
        struct Request(#[source] Connect);

        #[derive(Debug, thiserror::Error)]
        #[error("connect failed: {0}")]
        struct Connect(#[source] Refused);

        #[derive(Debug, thiserror::Error)]
        #[error("connection refused")]
        struct Refused;

        assert_eq!(
            error_chain(&Request(Connect(Refused))),
            "request failed: connect failed: connection refused"
        );
        assert_eq!(
            error_chain(&Connect(Refused)),
            "connect failed: connection refused"
        );
        assert_eq!(error_chain(&Refused), "connection refused");
    }

    #[test]
    fn absent_state_is_named_with_its_address() {
        let rpc = RpcClient::new_mock("succeeds");

        let error = read_state(&rpc, &program_id()).unwrap_err();

        assert_eq!(
            error.to_string(),
            format!(
                "state account {} does not exist",
                state_address(&program_id()).0
            )
        );
    }

    #[test]
    fn absent_window_is_named_with_its_id_and_address() {
        let rpc = RpcClient::new_mock("succeeds");

        let error = show(&rpc, &program_id(), 9).unwrap_err();

        assert_eq!(
            error.to_string(),
            format!(
                "window 9 account {} does not exist",
                window_address(&program_id(), 9).0
            )
        );
    }

    #[test]
    fn failed_account_read_is_not_reported_as_an_absent_account() {
        let rpc = RpcClient::new_mock("fails");

        for error in [
            read_state(&rpc, &program_id()).unwrap_err(),
            show(&rpc, &program_id(), 9).unwrap_err(),
        ] {
            let line = error_chain(error.as_ref());

            assert!(error.downcast_ref::<ClientError>().is_some(), "{line}");
            assert!(!line.contains("does not exist"), "{line}");
            assert!(!line.contains("AccountNotFound"), "{line}");
        }
    }

    #[test]
    fn present_state_is_decoded_from_the_account_data() {
        let rpc = RpcClient::new_mock_with_mocks(
            "succeeds",
            Mocks::from([(RpcRequest::GetMultipleAccounts, present(STATE_BASE64, 66))]),
        );

        assert_eq!(read_state(&rpc, &program_id()).unwrap(), state());
    }

    #[test]
    fn confirmed_submission_prints_the_transaction_signature() {
        let authority = signer();
        let rpc = RpcClient::new_mock("succeeds");

        let output = submit(&rpc, grid_commit(&authority), &authority, &[]).unwrap();

        assert_eq!(
            output,
            signature_on_the_mock_blockhash(grid_commit(&authority), &authority, &[])
                .to_string()
        );
    }

    #[test]
    fn init_submission_is_cosigned_by_the_program_keypair() {
        let authority = signer();
        let program = Keypair::new_from_array([42; 32]);
        let instruction = init(&program.pubkey(), &authority.pubkey(), 1_000, 150, 151);
        let signature =
            signature_on_the_mock_blockhash(instruction.clone(), &authority, &[&program]);
        let rpc = RpcClient::new_mock("succeeds");

        let unsigned = submit(&rpc, instruction.clone(), &authority, &[]).unwrap_err();
        let output = submit(&rpc, instruction, &authority, &[program]).unwrap();

        assert_eq!(unsigned.to_string(), "not enough signers");
        assert_eq!(output, signature.to_string());
    }

    #[test]
    fn confirmation_failure_names_the_transaction_signature() {
        let authority = signer();
        let signature =
            signature_on_the_mock_blockhash(grid_commit(&authority), &authority, &[]);
        let rpc = RpcClient::new_mock_with_mocks(
            "sig_not_found",
            Mocks::from([(RpcRequest::IsBlockhashValid, response(json!(false)))]),
        );

        let error = submit(&rpc, grid_commit(&authority), &authority, &[]).unwrap_err();

        let line = one_line(&error_chain(error.as_ref()));
        assert!(
            line.starts_with(&format!(
                "transaction {signature}: unable to confirm transaction"
            )),
            "{line}"
        );
    }

    #[test]
    fn rejection_after_landing_prints_the_program_error() {
        let authority = signer();
        let signature =
            signature_on_the_mock_blockhash(grid_commit(&authority), &authority, &[]);
        let failure = json!({"InstructionError": [0, {"Custom": 121}]});
        let status = json!({
            "slot": 1,
            "confirmations": null,
            "status": {"Err": failure},
            "err": failure,
            "confirmationStatus": "finalized",
        });
        let rpc = RpcClient::new_mock_with_mocks(
            "succeeds",
            Mocks::from([(RpcRequest::GetSignatureStatuses, response(json!([status])))]),
        );

        let error = submit(&rpc, grid_commit(&authority), &authority, &[]).unwrap_err();

        assert_eq!(
            one_line(&error_chain(error.as_ref())),
            format!(
                "transaction {signature}: WindowNotClosed (121): slot_end has not passed"
            )
        );
    }

    #[test]
    fn init_run_submits_the_instruction_signed_by_authority_and_program_keypair() {
        let authority = signer();
        let program = Keypair::new_from_array([42; 32]);
        let program_id = program.pubkey().to_string();
        let cli = Cli::try_parse_from(
            [
                &["clarion-admin", "--rpc-url", "mock", "--program-id"][..],
                &[program_id.as_str()],
                &AUTHORITY_OPTION,
                &init_arguments(),
            ]
            .concat(),
        )
        .unwrap();
        let rpc = RpcClient::new_mock("succeeds");

        let output = run(&cli, &rpc, |role, path| match (role, path.to_str()) {
            ("authority", Some("authority.json")) => Ok(signer()),
            ("program", Some("program.json")) => Ok(program.insecure_clone()),
            _ => Err(format!("{role} keypair {}", path.display()).into()),
        })
        .unwrap();

        assert_eq!(
            output,
            signature_on_the_mock_blockhash(
                init(&program.pubkey(), &authority.pubkey(), 1_000, 150, 151),
                &authority,
                &[&program],
            )
            .to_string()
        );
    }

    #[test]
    fn commit_run_submits_the_bounds_of_the_fetched_state_signed_by_the_authority() {
        let authority = signer();
        let cli = with_authority(&["commit", "--window-id", "7", "--root", ROOT]);
        let rpc = RpcClient::new_mock_with_mocks(
            "succeeds",
            Mocks::from([(RpcRequest::GetMultipleAccounts, present(STATE_BASE64, 66))]),
        );

        let output = run(&cli, &rpc, |role, path| match role {
            "authority" => Ok(signer()),
            _ => Err(format!("{role} keypair {}", path.display()).into()),
        })
        .unwrap();

        assert_eq!(
            output,
            signature_on_the_mock_blockhash(grid_commit(&authority), &authority, &[])
                .to_string()
        );
    }

    #[test]
    fn reveal_run_submits_the_aggregates_signed_by_the_authority() {
        let authority = signer();
        let cli =
            with_authority(&["reveal", "--window-id", "3", "--aggregates", &arms(4)]);
        let aggregates = parse_aggregates(&arms(4)).unwrap();
        let rpc = RpcClient::new_mock_with_mocks_map(
            "succeeds",
            MocksMap::from_iter([
                (RpcRequest::GetMultipleAccounts, present(STATE_BASE64, 66)),
                (RpcRequest::GetMultipleAccounts, present("", 0)),
            ]),
        );

        let output = run(&cli, &rpc, |role, path| match role {
            "authority" => Ok(signer()),
            _ => Err(format!("{role} keypair {}", path.display()).into()),
        })
        .unwrap();

        assert_eq!(
            output,
            signature_on_the_mock_blockhash(
                reveal(&program_id(), &authority.pubkey(), 3, aggregates),
                &authority,
                &[],
            )
            .to_string()
        );
    }

    #[test]
    fn reveal_run_stops_at_an_absent_state_account() {
        let cli =
            with_authority(&["reveal", "--window-id", "3", "--aggregates", &arms(4)]);
        let rpc = RpcClient::new_mock("succeeds");

        let error = run(&cli, &rpc, |role, path| match role {
            "authority" => Ok(signer()),
            _ => Err(format!("{role} keypair {}", path.display()).into()),
        })
        .unwrap_err();

        assert_eq!(
            error.to_string(),
            format!(
                "state account {} does not exist",
                state_address(&program_id()).0
            )
        );
    }

    #[test]
    fn reveal_run_stops_at_an_absent_window_account() {
        let cli =
            with_authority(&["reveal", "--window-id", "3", "--aggregates", &arms(4)]);
        let rpc = RpcClient::new_mock_with_mocks(
            "succeeds",
            Mocks::from([(RpcRequest::GetMultipleAccounts, present(STATE_BASE64, 66))]),
        );

        let error = run(&cli, &rpc, |role, path| match role {
            "authority" => Ok(signer()),
            _ => Err(format!("{role} keypair {}", path.display()).into()),
        })
        .unwrap_err();

        assert_eq!(
            error.to_string(),
            format!(
                "window 3 account {} does not exist",
                window_address(&program_id(), 3).0
            )
        );
    }

    #[test]
    fn show_run_reads_no_keypair_with_or_without_an_authority() {
        let rpc = RpcClient::new_mock("succeeds");

        for cli in [
            parse(&["show", "--window-id", "9"]).unwrap(),
            with_authority(&["show", "--window-id", "9"]),
        ] {
            let error = run(&cli, &rpc, |role, path| {
                Err(format!("{role} keypair {}", path.display()).into())
            })
            .unwrap_err();

            assert_eq!(
                error.to_string(),
                format!(
                    "window 9 account {} does not exist",
                    window_address(&program_id(), 9).0
                )
            );
        }
    }

    #[test]
    fn submission_run_without_an_authority_is_a_usage_failure_before_any_read() {
        let rpc = RpcClient::new_mock("fails");
        let reads = RefCell::new(Vec::new());

        for arguments in [
            init_arguments(),
            vec!["commit", "--window-id", "7", "--root", ROOT],
            vec!["reveal", "--window-id", "3", "--aggregates", &arms(4)],
        ] {
            let error = run(&parse(&arguments).unwrap(), &rpc, |role, path| {
                reads
                    .borrow_mut()
                    .push(format!("{role} keypair {}", path.display()));
                Ok(signer())
            })
            .unwrap_err();

            let usage = error.downcast_ref::<clap::Error>().unwrap();
            assert_eq!(usage.kind(), ErrorKind::MissingRequiredArgument);
            assert!(usage.use_stderr());
            assert_eq!(
                usage_line(usage),
                "the following required arguments were not provided: --authority <PATH>"
            );
        }
        assert_eq!(*reads.borrow(), Vec::<String>::new());
    }

    #[test]
    fn submission_run_stops_at_an_unreadable_authority_keypair() {
        let rpc = RpcClient::new_mock("succeeds");

        for arguments in [
            init_arguments(),
            vec!["commit", "--window-id", "7", "--root", ROOT],
        ] {
            let error = run(&with_authority(&arguments), &rpc, |role, path| {
                Err(format!("{role} keypair {}", path.display()).into())
            })
            .unwrap_err();

            assert_eq!(error.to_string(), "authority keypair authority.json");
        }
    }

    #[test]
    fn submission_run_refuses_an_authority_keypair_holding_the_program_id() {
        let program = Keypair::new_from_array([42; 32]);
        let program_id = program.pubkey().to_string();
        let globals = ["clarion-admin", "--rpc-url", "mock", "--program-id"];
        let rpc = RpcClient::new_mock("fails");

        for arguments in [
            init_arguments(),
            vec!["commit", "--window-id", "7", "--root", ROOT],
            vec!["reveal", "--window-id", "3", "--aggregates", &arms(4)],
        ] {
            let cli = Cli::try_parse_from(
                [
                    &globals[..],
                    &[program_id.as_str()],
                    &AUTHORITY_OPTION,
                    &arguments,
                ]
                .concat(),
            )
            .unwrap();

            let error = run(&cli, &rpc, |_, _| Ok(program.insecure_clone())).unwrap_err();

            assert_eq!(
                error.to_string(),
                format!(
                    "authority keypair authority.json holds the program id \
                     {program_id}, the authority must be a separate key"
                )
            );
        }
    }
}

use std::{
    io::{BufRead, BufReader, Read, Write},
    net::{TcpListener, TcpStream},
    path::PathBuf,
    process::{Command, Output},
    sync::{Arc, Mutex},
    thread,
    time::{Duration, Instant},
};

use base64::{Engine, engine::general_purpose::STANDARD};
use clarion_program_client::loader_buffer::{
    BUFFER_METADATA_SIZE, SignerLayout, max_chunk_bytes, sha256_hex,
};
use serde_json::{Value, json};
use solana_keypair::{Keypair, Signer, write_keypair_file};
use solana_loader_v3_interface::{
    instruction::UpgradeableLoaderInstruction, state::UpgradeableLoaderState,
};
use solana_program::pubkey::Pubkey;
use solana_sdk_ids::bpf_loader_upgradeable;
use solana_transaction::{Hash, Transaction};

const CLOSED_RPC_URL: &str = "http://127.0.0.1:1";

struct FakeLedger {
    data: Vec<u8>,
    owner: Pubkey,
    rate_limit_first: bool,
    drop_write: fn(usize) -> bool,
    writes_seen: usize,
    dropped: usize,
    methods: Vec<String>,
    received_at: Vec<Instant>,
    instructions: Vec<(Pubkey, UpgradeableLoaderInstruction)>,
}

fn buffer_key() -> Pubkey {
    Pubkey::new_from_array([7; 32])
}

fn program(len: usize) -> Vec<u8> {
    (0..len).map(|index| (index % 239) as u8 + 1).collect()
}

fn buffer_data(authority: Option<Pubkey>, stored: &[u8]) -> Vec<u8> {
    let mut data = bincode::serialize(&UpgradeableLoaderState::Buffer {
        authority_address: authority,
    })
    .unwrap();
    data.resize(BUFFER_METADATA_SIZE, 0);
    data.extend_from_slice(stored);
    data
}

fn test_dir(test: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join("clarion-buffer")
        .join(test);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn written_key(test: &str, file: &str, keypair: &Keypair) -> PathBuf {
    let path = test_dir(test).join(file);
    write_keypair_file(keypair, &path).unwrap();
    path
}

fn written_program(test: &str, program: &[u8]) -> PathBuf {
    let path = test_dir(test).join("program.so");
    std::fs::write(&path, program).unwrap();
    path
}

fn buffer_cli(arguments: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_clarion-buffer"))
        .args(arguments)
        .output()
        .unwrap()
}

fn read_body(stream: &TcpStream) -> Value {
    let mut reader = BufReader::new(stream);
    let mut content_length = 0;
    loop {
        let mut header = String::new();
        reader.read_line(&mut header).unwrap();
        let header = header.trim_end().to_ascii_lowercase();
        if header.is_empty() {
            break;
        }
        if let Some(length) = header.strip_prefix("content-length:") {
            content_length = length.trim().parse().unwrap();
        }
    }
    let mut body = vec![0; content_length];
    reader.read_exact(&mut body).unwrap();
    serde_json::from_slice(&body).unwrap()
}

fn apply_send(ledger: &mut FakeLedger, request: &Value) -> Value {
    let wire = STANDARD
        .decode(request["params"][0].as_str().unwrap())
        .unwrap();
    let transaction: Transaction = bincode::deserialize(&wire).unwrap();
    assert!(transaction.verify().is_ok());
    assert!(wire.len() <= 1232, "{}", wire.len());
    assert_eq!(request["params"][1]["skipPreflight"], true);
    let keys = &transaction.message.account_keys;
    for instruction in &transaction.message.instructions {
        let program_id = keys[usize::from(instruction.program_id_index)];
        let decoded =
            bincode::deserialize::<UpgradeableLoaderInstruction>(&instruction.data);
        ledger.instructions.push((
            program_id,
            decoded.unwrap_or(UpgradeableLoaderInstruction::Close),
        ));
        if let Some(UpgradeableLoaderInstruction::Write { offset, bytes }) = ledger
            .instructions
            .last()
            .map(|(_, decoded)| decoded.clone())
        {
            let index = ledger.writes_seen;
            ledger.writes_seen += 1;
            if (ledger.drop_write)(index) {
                ledger.dropped += 1;
                continue;
            }
            let start = BUFFER_METADATA_SIZE + offset as usize;
            ledger.data[start..start + bytes.len()].copy_from_slice(&bytes);
        }
    }
    json!(transaction.signatures[0].to_string())
}

fn respond(stream: &mut TcpStream, status: &str, extra: &str, body: &str) {
    write!(
        stream,
        "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\
         {extra}connection: close\r\n\r\n{body}",
        body.len()
    )
    .unwrap();
}

fn serve(ledger: FakeLedger) -> (String, Arc<Mutex<FakeLedger>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let ledger = Arc::new(Mutex::new(ledger));
    let shared = ledger.clone();
    thread::spawn(move || {
        for stream in listener.incoming() {
            let mut stream = stream.unwrap();
            let request = read_body(&stream);
            let mut ledger = shared.lock().unwrap();
            let method = request["method"].as_str().unwrap().to_string();
            ledger.methods.push(method.clone());
            ledger.received_at.push(Instant::now());
            if ledger.rate_limit_first {
                ledger.rate_limit_first = false;
                respond(
                    &mut stream,
                    "429 Too Many Requests",
                    "retry-after: 0\r\n",
                    "",
                );
                continue;
            }
            let result = match method.as_str() {
                "getAccountInfo" => json!({"context": {"slot": 1}, "value": {
                    "lamports": 1,
                    "owner": ledger.owner.to_string(),
                    "executable": false,
                    "rentEpoch": 0,
                    "space": ledger.data.len(),
                    "data": [STANDARD.encode(&ledger.data), "base64"],
                }}),
                "getLatestBlockhash" => json!({"context": {"slot": 1}, "value": {
                    "blockhash": Hash::new_from_array([4; 32]).to_string(),
                    "lastValidBlockHeight": 1,
                }}),
                "sendTransaction" => apply_send(&mut ledger, &request),
                other => panic!("unexpected method {other}"),
            };
            let reply = json!({"jsonrpc": "2.0", "id": request["id"], "result": result});
            respond(&mut stream, "200 OK", "", &reply.to_string());
        }
    });
    (url, ledger)
}

fn ledger(data: Vec<u8>, rate_limit_first: bool) -> FakeLedger {
    FakeLedger {
        data,
        owner: bpf_loader_upgradeable::ID,
        rate_limit_first,
        drop_write: |index| index % 3 == 1,
        writes_seen: 0,
        dropped: 0,
        methods: Vec::new(),
        received_at: Vec::new(),
        instructions: Vec::new(),
    }
}

fn assert_failure_line(output: &Output, code: i32) -> String {
    let stderr = String::from_utf8(output.stderr.clone()).unwrap();
    assert_eq!(output.status.code(), Some(code), "{stderr}");
    assert!(output.stdout.is_empty());
    assert_eq!(stderr.matches('\n').count(), 1, "{stderr}");
    stderr
        .strip_prefix("clarion-buffer: ")
        .and_then(|line| line.strip_suffix('\n'))
        .unwrap()
        .to_string()
}

#[test]
fn buffer_help_exits_zero_with_the_usage_on_stdout() {
    let output = buffer_cli(&["--help"]);

    let stdout = String::from_utf8(output.stdout).unwrap();
    assert_eq!(output.status.code(), Some(0));
    assert!(output.stderr.is_empty());
    assert!(
        stdout.contains("Usage: clarion-buffer [OPTIONS] --rpc-url <URL> <COMMAND>"),
        "{stdout}"
    );
}

#[test]
fn buffer_write_help_lists_both_keypairs_as_required() {
    let output = buffer_cli(&["write", "--help"]);

    let stdout = String::from_utf8(output.stdout).unwrap();
    assert_eq!(output.status.code(), Some(0));
    assert!(
        stdout.contains(
            "Usage: clarion-buffer --rpc-url <URL> write [OPTIONS] --buffer <PUBKEY> \
             --program-so <PATH> --authority <PATH> --fee-payer <PATH>"
        ),
        "{stdout}"
    );
}

#[test]
fn buffer_usage_failure_exits_two_with_one_line_on_stderr() {
    let buffer = buffer_key().to_string();

    let output = buffer_cli(&["status", "--buffer", &buffer, "--program-so", "x.so"]);

    assert_eq!(
        assert_failure_line(&output, 2),
        "the following required arguments were not provided: --rpc-url <URL>"
    );
}

#[test]
fn buffer_rate_above_three_and_a_half_exits_two() {
    let buffer = buffer_key().to_string();

    let output = buffer_cli(&[
        "--rpc-url",
        CLOSED_RPC_URL,
        "--max-requests-per-second",
        "4",
        "status",
        "--buffer",
        &buffer,
        "--program-so",
        "x.so",
    ]);

    let line = assert_failure_line(&output, 2);
    assert!(line.contains("outside (0, 3.5]"), "{line}");
}

#[test]
fn buffer_write_with_a_missing_authority_file_exits_one_before_any_request() {
    let test = "missing_authority";
    let program = written_program(test, &program(10));
    let authority = test_dir(test).join("absent.json");
    let buffer = buffer_key().to_string();

    let output = buffer_cli(&[
        "--rpc-url",
        CLOSED_RPC_URL,
        "write",
        "--buffer",
        &buffer,
        "--program-so",
        program.to_str().unwrap(),
        "--authority",
        authority.to_str().unwrap(),
        "--fee-payer",
        authority.to_str().unwrap(),
    ]);

    let line = assert_failure_line(&output, 1);
    assert!(
        line.starts_with(&format!(
            "failed to read authority keypair {}: ",
            authority.display()
        )),
        "{line}"
    );
}

#[test]
fn buffer_status_reads_the_account_once_and_prints_the_chunk_counts() {
    let chunk = max_chunk_bytes(SignerLayout::SeparateFeePayer);
    let program = program(chunk * 2 + 5);
    let mut stored = program.clone();
    stored[chunk] = 0;
    let authority = Pubkey::new_from_array([1; 32]);
    let (url, ledger) = serve(ledger(buffer_data(Some(authority), &stored), false));
    let path = written_program("status", &program);
    let buffer = buffer_key().to_string();

    let output = buffer_cli(&[
        "--rpc-url",
        &url,
        "status",
        "--buffer",
        &buffer,
        "--program-so",
        path.to_str().unwrap(),
    ]);

    let stderr = String::from_utf8(output.stderr).unwrap();
    assert_eq!(output.status.code(), Some(0), "{stderr}");
    let printed: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        printed,
        json!({
            "buffer": buffer,
            "authority": authority.to_string(),
            "data_len": BUFFER_METADATA_SIZE + program.len(),
            "program_len": program.len(),
            "chunk_bytes": chunk,
            "chunks_total": 3,
            "chunks_matching": 2,
            "chunks_missing": 1,
            "complete": false,
            "program_sha256": sha256_hex(&program),
            "buffer_program_sha256": sha256_hex(&stored),
        })
    );
    assert_eq!(ledger.lock().unwrap().methods, ["getAccountInfo"]);
}

#[test]
fn buffer_write_of_another_authority_exits_one_after_the_single_read() {
    let test = "another_authority";
    let held = Keypair::new_from_array([1; 32]);
    let stored = Pubkey::new_from_array([2; 32]);
    let program = program(10);
    let (url, ledger) = serve(ledger(buffer_data(Some(stored), &[0; 10]), false));
    let authority = written_key(test, "authority.json", &held);
    let path = written_program(test, &program);
    let buffer = buffer_key().to_string();

    let output = buffer_cli(&[
        "--rpc-url",
        &url,
        "write",
        "--buffer",
        &buffer,
        "--program-so",
        path.to_str().unwrap(),
        "--authority",
        authority.to_str().unwrap(),
        "--fee-payer",
        authority.to_str().unwrap(),
    ]);

    assert_eq!(
        assert_failure_line(&output, 1),
        format!(
            "buffer {buffer} authority is {stored}, the authority keypair holds {}",
            held.pubkey()
        )
    );
    assert_eq!(ledger.lock().unwrap().methods, ["getAccountInfo"]);
}

#[test]
fn buffer_write_converges_to_an_exact_copy_through_dropped_writes_and_a_429() {
    let test = "converges";
    let authority = Keypair::new_from_array([1; 32]);
    let fee_payer = Keypair::new_from_array([2; 32]);
    let chunk = max_chunk_bytes(SignerLayout::SeparateFeePayer);
    let program = program(chunk * 3 + 100);
    let mut stored = vec![0; program.len()];
    stored[..chunk].copy_from_slice(&program[..chunk]);
    let initial = buffer_data(Some(authority.pubkey()), &stored);
    let (url, ledger) = serve(ledger(initial, true));
    let authority_path = written_key(test, "authority.json", &authority);
    let fee_payer_path = written_key(test, "fee-payer.json", &fee_payer);
    let path = written_program(test, &program);
    let buffer = buffer_key().to_string();

    let output = buffer_cli(&[
        "--rpc-url",
        &url,
        "--max-requests-per-second",
        "3.5",
        "write",
        "--buffer",
        &buffer,
        "--program-so",
        path.to_str().unwrap(),
        "--authority",
        authority_path.to_str().unwrap(),
        "--fee-payer",
        fee_payer_path.to_str().unwrap(),
        "--settle-seconds",
        "0",
    ]);

    let stderr = String::from_utf8(output.stderr).unwrap();
    assert_eq!(output.status.code(), Some(0), "{stderr}");
    let ledger = ledger.lock().unwrap();
    assert_eq!(ledger.data, buffer_data(Some(authority.pubkey()), &program));
    assert!(ledger.dropped > 0);
    assert!(!ledger.instructions.is_empty());
    for (program_id, instruction) in &ledger.instructions {
        assert_eq!(*program_id, bpf_loader_upgradeable::ID);
        assert!(
            matches!(instruction, UpgradeableLoaderInstruction::Write { .. }),
            "{instruction:?}"
        );
    }
    assert_eq!(ledger.methods[..2], ["getAccountInfo", "getAccountInfo"]);
    let sends = ledger
        .methods
        .iter()
        .filter(|method| *method == "sendTransaction")
        .count();
    let printed: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        printed,
        json!({
            "passes": 2,
            "transactions_sent": sends,
            "chunks_total": 4,
            "complete": true,
            "program_sha256": sha256_hex(&program),
        })
    );
    assert_eq!(sends, 4);
    assert_eq!(stderr.lines().count(), 2, "{stderr}");
    assert!(
        stderr.starts_with(
            "clarion-buffer: pass 1/10: 3 of 4 chunks differed, 3 writes sent"
        ),
        "{stderr}"
    );
}

#[test]
fn buffer_write_that_runs_out_of_passes_exits_one_with_the_summary_on_stdout() {
    let test = "exhausted";
    let authority = Keypair::new_from_array([1; 32]);
    let fee_payer = Keypair::new_from_array([2; 32]);
    let chunk = max_chunk_bytes(SignerLayout::SeparateFeePayer);
    let program = program(chunk * 2 + 5);
    let initial = buffer_data(Some(authority.pubkey()), &vec![0; program.len()]);
    let mut never_lands = ledger(initial.clone(), false);
    never_lands.drop_write = |_| true;
    let (url, ledger) = serve(never_lands);
    let authority_path = written_key(test, "authority.json", &authority);
    let fee_payer_path = written_key(test, "fee-payer.json", &fee_payer);
    let path = written_program(test, &program);
    let buffer = buffer_key().to_string();

    let output = buffer_cli(&[
        "--rpc-url",
        &url,
        "--max-requests-per-second",
        "3.5",
        "write",
        "--buffer",
        &buffer,
        "--program-so",
        path.to_str().unwrap(),
        "--authority",
        authority_path.to_str().unwrap(),
        "--fee-payer",
        fee_payer_path.to_str().unwrap(),
        "--max-passes",
        "2",
        "--settle-seconds",
        "1",
    ]);

    let stderr = String::from_utf8(output.stderr).unwrap();
    assert_eq!(output.status.code(), Some(1), "{stderr}");
    let ledger = ledger.lock().unwrap();
    assert_eq!(ledger.data, initial);
    let sends = ledger
        .methods
        .iter()
        .filter(|method| *method == "sendTransaction")
        .count();
    assert_eq!(sends, 6);
    let printed: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        printed,
        json!({
            "passes": 2,
            "transactions_sent": sends,
            "chunks_total": 3,
            "complete": false,
            "program_sha256": sha256_hex(&program),
        })
    );
    assert_eq!(
        stderr.lines().collect::<Vec<_>>(),
        [
            "clarion-buffer: pass 1/2: 3 of 3 chunks differed, 3 writes sent, 0 failed"
                .to_string(),
            "clarion-buffer: pass 2/2: 3 of 3 chunks differed, 3 writes sent, 0 failed"
                .to_string(),
            format!(
                "clarion-buffer: buffer {buffer} still differs from {} in 3 of 3 chunks \
                 after 2 passes",
                path.display()
            ),
        ]
    );
    let reads: Vec<usize> = ledger
        .methods
        .iter()
        .enumerate()
        .filter(|(_, method)| *method == "getAccountInfo")
        .map(|(position, _)| position)
        .collect();
    assert_eq!(reads.len(), 3);
    for read in &reads[1..] {
        let settle = ledger.received_at[*read] - ledger.received_at[read - 1];
        assert!(
            settle >= Duration::from_secs(1) && settle < Duration::from_secs(5),
            "{settle:?}"
        );
    }
}

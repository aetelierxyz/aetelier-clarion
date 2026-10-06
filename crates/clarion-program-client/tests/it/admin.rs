use std::{
    io::{BufRead, BufReader, Read, Write},
    net::TcpListener,
    path::PathBuf,
    process::{Command, Output},
    sync::mpsc::{self, Receiver, TryRecvError},
    thread,
};

use serde_json::{Value, json};
use solana_keypair::{Keypair, Signer, write_keypair_file};

use crate::support::{
    AGGREGATES_JSON, MERKLE_ROOT_HEX, PROGRAM_ID, state_key, window_key,
};

const CLOSED_RPC_URL: &str = "http://127.0.0.1:1";
const WINDOW_BASE64: &str = "AgMAAAAAAAAAggAAAAAAAACLAAAAAAAAAAABAgMEBQYHCAkKCwwNDg8Q\
                             ERITFBUWFxgZGhscHR4fjAAAAAAAAACRAAAAAAAAAGQAAABbAAAAAQAC\
                             AOgDAAAAAAAAKCMAAAAAAAAgoQcAAAAAAAAAAAAAAAAAyAAAAMAAAAAC\
                             AAQA0AcAAAAAAABQRgAAAAAAAEBCDwAAAAAAkNADAAAAAAAsAQAAJQEA\
                             AAMABgAAAAAAAAAAAAAAAAAAAAAAYOMWAAAAAAAAAAAAAAAAAJABAACQ\
                             AQAABAAIAKAPAAAAAAAAoIwAAAAAAACAhB4AAAAAAAAAAAAAAAAA/A==";
const INIT: [&str; 7] = [
    "init",
    "--genesis-slot",
    "1000",
    "--window-len",
    "150",
    "--lag",
    "151",
];

fn admin(arguments: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_clarion-admin"))
        .args(arguments)
        .output()
        .unwrap()
}

fn key_path(test: &str, file: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join("clarion-admin")
        .join(test)
        .join(file)
}

fn written_key(test: &str, file: &str, keypair: &Keypair) -> PathBuf {
    let path = key_path(test, file);
    write_keypair_file(keypair, &path).unwrap();
    path
}

fn window_account(owner: &str) -> Value {
    json!([{
        "lamports": 2_630_880,
        "data": [WINDOW_BASE64, "base64"],
        "owner": owner,
        "executable": false,
        "rentEpoch": 0,
        "space": 250,
    }])
}

fn serve_one_response(value: Value) -> (String, Receiver<Value>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let (requests, received) = mpsc::channel();
    thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
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
        let request: Value = serde_json::from_slice(&body).unwrap();
        let reply = json!({
            "jsonrpc": "2.0",
            "id": request["id"],
            "result": {"context": {"slot": 1}, "value": value},
        })
        .to_string();
        requests.send(request).unwrap();
        write!(
            reader.get_mut(),
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\
             connection: close\r\n\r\n{reply}",
            reply.len()
        )
        .unwrap();
    });
    (url, received)
}

fn assert_failure_line(output: &Output, code: i32) -> String {
    let stderr = String::from_utf8(output.stderr.clone()).unwrap();
    assert_eq!(output.status.code(), Some(code), "{stderr}");
    assert!(output.stdout.is_empty());
    assert_eq!(stderr.matches('\n').count(), 1, "{stderr}");
    stderr
        .strip_prefix("clarion-admin: ")
        .and_then(|line| line.strip_suffix('\n'))
        .unwrap()
        .to_string()
}

#[test]
fn help_exits_zero_with_the_usage_on_stdout() {
    let output = admin(&["--help"]);

    let stdout = String::from_utf8(output.stdout).unwrap();
    assert_eq!(output.status.code(), Some(0));
    assert!(output.stderr.is_empty());
    assert!(
        stdout.contains(
            "Usage: clarion-admin [OPTIONS] --rpc-url <URL> --program-id <PUBKEY> <COMMAND>"
        ),
        "{stdout}"
    );
    assert!(!stdout.contains("[default: "), "{stdout}");
}

#[test]
fn init_help_lists_the_program_keypair_as_required_without_a_default() {
    let output = admin(&["init", "--help"]);

    let stdout = String::from_utf8(output.stdout).unwrap();
    assert_eq!(output.status.code(), Some(0));
    assert!(output.stderr.is_empty());
    assert!(
        stdout.contains(
            "Usage: clarion-admin --rpc-url <URL> --program-id <PUBKEY> init \
             --genesis-slot <SLOT> --window-len <SLOTS> --lag <SLOTS> --program-keypair \
             <PATH>"
        ),
        "{stdout}"
    );
    assert!(!stdout.contains("[default: "), "{stdout}");
}

#[test]
fn usage_failure_exits_two_with_one_line_on_stderr() {
    let output = admin(&["show", "--window-id", "0"]);

    assert_eq!(
        assert_failure_line(&output, 2),
        "the following required arguments were not provided: --rpc-url <URL> \
         --program-id <PUBKEY>"
    );
}

#[test]
fn init_without_the_program_keypair_exits_two_with_one_line_on_stderr() {
    let authority = key_path("init_without_program_keypair", "authority.json");
    let program_id = PROGRAM_ID.to_string();
    let globals = [
        "--rpc-url",
        CLOSED_RPC_URL,
        "--program-id",
        &program_id,
        "--authority",
        authority.to_str().unwrap(),
    ];

    let output = admin(&[&globals[..], &INIT].concat());

    assert_eq!(
        assert_failure_line(&output, 2),
        "the following required arguments were not provided: --program-keypair <PATH>"
    );
}

#[test]
fn submission_without_the_authority_exits_two_before_any_request_or_key_file_read() {
    let program = key_path("submission_without_authority", "program.json");
    let program_id = PROGRAM_ID.to_string();
    let keypair = ["--program-keypair", program.to_str().unwrap()];

    for arguments in [
        [&INIT[..], &keypair].concat(),
        vec!["commit", "--window-id", "0", "--root", MERKLE_ROOT_HEX],
        vec![
            "reveal",
            "--window-id",
            "0",
            "--aggregates",
            AGGREGATES_JSON,
        ],
    ] {
        let (url, requests) = serve_one_response(json!([null]));
        let globals = ["--rpc-url", &url, "--program-id", &program_id];

        let output = admin(&[&globals[..], &arguments].concat());

        assert_eq!(
            assert_failure_line(&output, 2),
            "the following required arguments were not provided: --authority <PATH>"
        );
        assert_eq!(requests.try_recv(), Err(TryRecvError::Empty));
    }
}

#[test]
fn missing_authority_key_file_exits_one_with_one_line_on_stderr() {
    let authority = key_path("missing_authority", "authority.json");
    let program_id = PROGRAM_ID.to_string();

    let output = admin(&[
        "--rpc-url",
        CLOSED_RPC_URL,
        "--program-id",
        &program_id,
        "--authority",
        authority.to_str().unwrap(),
        "commit",
        "--window-id",
        "0",
        "--root",
        MERKLE_ROOT_HEX,
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
fn init_with_a_missing_program_key_file_exits_one_with_one_line_on_stderr() {
    let test = "missing_program_key";
    let authority =
        written_key(test, "authority.json", &Keypair::new_from_array([1; 32]));
    let program = key_path(test, "program.json");
    let program_id = PROGRAM_ID.to_string();
    let globals = [
        "--rpc-url",
        CLOSED_RPC_URL,
        "--program-id",
        &program_id,
        "--authority",
        authority.to_str().unwrap(),
    ];
    let keypair = ["--program-keypair", program.to_str().unwrap()];

    let output = admin(&[&globals[..], &INIT, &keypair].concat());

    let line = assert_failure_line(&output, 1);
    assert!(
        line.starts_with(&format!(
            "failed to read program keypair {}: ",
            program.display()
        )),
        "{line}"
    );
}

#[test]
fn init_with_the_keypair_of_another_program_id_exits_one_before_any_request() {
    let test = "another_program_id";
    let authority =
        written_key(test, "authority.json", &Keypair::new_from_array([1; 32]));
    let held = Keypair::new_from_array([7; 32]);
    let program = written_key(test, "program.json", &held);
    let program_id = PROGRAM_ID.to_string();
    let globals = [
        "--rpc-url",
        CLOSED_RPC_URL,
        "--program-id",
        &program_id,
        "--authority",
        authority.to_str().unwrap(),
    ];
    let keypair = ["--program-keypair", program.to_str().unwrap()];

    let output = admin(&[&globals[..], &INIT, &keypair].concat());

    assert_eq!(
        assert_failure_line(&output, 1),
        format!(
            "program keypair {} holds {}, expected program id {program_id}",
            program.display(),
            held.pubkey()
        )
    );
}

#[test]
fn malformed_rpc_url_reports_its_cause_on_the_same_line() {
    let program_id = PROGRAM_ID.to_string();

    let output = admin(&[
        "--rpc-url",
        "not-a-url",
        "--program-id",
        &program_id,
        "show",
        "--window-id",
        "0",
    ]);

    let line = assert_failure_line(&output, 1);
    assert!(line.ends_with(": relative URL without a base"), "{line}");
}

#[test]
fn show_requests_the_window_address_alone_and_prints_the_window_under_it() {
    let program_id = PROGRAM_ID.to_string();
    let window_key = window_key(3).to_string();
    let (url, requests) = serve_one_response(window_account(&program_id));

    let output = admin(&[
        "--rpc-url",
        &url,
        "--program-id",
        &program_id,
        "show",
        "--window-id",
        "3",
    ]);

    let request = requests.try_recv().unwrap();
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert_eq!(request["method"], "getMultipleAccounts");
    assert_eq!(request["params"][0], json!([window_key]));
    assert_eq!(output.status.code(), Some(0), "{stderr}");
    assert!(stderr.is_empty(), "{stderr}");
    let printed: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(printed["address"], window_key);
    assert_eq!(printed["window_id"], 3);
    assert_eq!(printed["merkle_root"], MERKLE_ROOT_HEX);
    assert_eq!(
        printed["aggregates"],
        serde_json::from_str::<Value>(AGGREGATES_JSON).unwrap()
    );
}

#[test]
fn show_prints_the_window_without_the_authority_and_with_one_whose_file_is_missing() {
    let missing = key_path("show_ignores_authority", "authority.json");
    let program_id = PROGRAM_ID.to_string();

    for authority in [vec![], vec!["--authority", missing.to_str().unwrap()]] {
        let (url, requests) = serve_one_response(window_account(&program_id));
        let globals = ["--rpc-url", &url, "--program-id", &program_id];
        let show = ["show", "--window-id", "3"];

        let output = admin(&[&globals[..], &authority, &show].concat());

        let stderr = String::from_utf8(output.stderr).unwrap();
        assert_eq!(output.status.code(), Some(0), "{stderr}");
        assert!(stderr.is_empty(), "{stderr}");
        assert_eq!(
            requests.try_recv().unwrap()["method"],
            "getMultipleAccounts"
        );
        let printed: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(printed["window_id"], 3);
    }
}

#[test]
fn commit_requests_the_state_address_alone_and_reports_its_absence() {
    let authority = written_key(
        "commit_absent_state",
        "authority.json",
        &Keypair::new_from_array([1; 32]),
    );
    let program_id = PROGRAM_ID.to_string();
    let state_key = state_key().to_string();
    let (url, requests) = serve_one_response(json!([null]));

    let output = admin(&[
        "--rpc-url",
        &url,
        "--program-id",
        &program_id,
        "--authority",
        authority.to_str().unwrap(),
        "commit",
        "--window-id",
        "0",
        "--root",
        MERKLE_ROOT_HEX,
    ]);

    let request = requests.try_recv().unwrap();
    assert_eq!(request["method"], "getMultipleAccounts");
    assert_eq!(request["params"][0], json!([state_key]));
    assert_eq!(
        assert_failure_line(&output, 1),
        format!("state account {state_key} does not exist")
    );
}

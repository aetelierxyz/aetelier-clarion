use std::{path::Path, process::Output};

use clarion_pulsar::{config::FILE_KEYS, tls::TPU_ALPN};
use tokio::process::Command;

use crate::support::{
    Admission, CannedRpc, Cluster, RpcReply, TpuServer, scratch_dir, synthetic_key,
};

const SYNTHETIC_PROVIDER_KEY: &str = "k3y-0000-synthetic-provider-token";

async fn binary(arguments: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_clarion-pulsar"))
        .args(arguments)
        .kill_on_drop(true)
        .output()
        .await
        .unwrap()
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8(bytes.to_vec()).unwrap()
}

fn write_config(directory: &Path, rpc_url: &str) -> String {
    std::fs::create_dir_all(directory).unwrap();
    let path = directory.join("clarion-pulsar-test.toml");
    std::fs::write(
        &path,
        format!(
            "rpc_url = \"{rpc_url}\"\nvantage = \"test-vantage\"\nrounds = 1\n\
             output_dir = \"{}\"\n",
            directory.join("out").display()
        ),
    )
    .unwrap();
    path.display().to_string()
}

#[tokio::test]
async fn long_help_exits_zero_and_documents_keys_files_and_exit_codes() {
    let output = binary(&["--help"]).await;

    let stdout = text(&output.stdout);
    assert_eq!(output.status.code(), Some(0));
    assert!(stdout.starts_with("Solana TPU QUIC handshake latency sampler"));
    for key in FILE_KEYS {
        assert!(stdout.contains(key), "{key}");
    }
    for fact in [
        "--config <PATH>",
        "--rpc-url <URL>",
        "--vantage <LABEL>",
        "clarion-pulsar-validators-<ts>.csv",
        "clarion-pulsar-leader-slots-<cluster>-<epoch>.csv",
        "Exit codes",
        "one handshake in flight per destination IP",
    ] {
        assert!(stdout.contains(fact), "{fact}");
    }
}

#[tokio::test]
async fn short_help_exits_zero() {
    let output = binary(&["-h"]).await;

    assert_eq!(output.status.code(), Some(0));
    assert!(
        text(&output.stdout).starts_with("Solana TPU QUIC handshake latency sampler")
    );
}

#[tokio::test]
async fn invalid_command_line_exits_two() {
    let output = binary(&["--cluster", "devnet"]).await;

    assert_eq!(output.status.code(), Some(2));
}

#[tokio::test]
async fn configuration_failure_exits_one_with_one_stderr_line() {
    let directory = scratch_dir("cli-config-failure");
    std::fs::create_dir_all(&directory).unwrap();
    let config = directory.join("clarion-pulsar-test.toml");
    std::fs::write(&config, "vantage = \"v\"\nround_spacing_ms = 200\n").unwrap();

    let output = binary(&["--config", &config.display().to_string()]).await;

    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        text(&output.stderr),
        "clarion-pulsar: round_spacing_ms must be at least 30000\n"
    );
    assert!(output.stdout.is_empty());
}

#[tokio::test]
async fn malformed_config_exits_one_with_one_line_that_never_shows_the_keyed_url() {
    let directory = scratch_dir("cli-config-parse-failure");
    std::fs::create_dir_all(&directory).unwrap();
    let keyed = format!("https://rpc.example.invalid/v1/{SYNTHETIC_PROVIDER_KEY}");
    let cases = [
        (
            "clarion-pulsar-key-typo.toml",
            format!("rpc-url = \"{keyed}\"\nvantage = \"v\"\n"),
            "at line 1, column 1: unknown field `rpc-url`",
        ),
        (
            "clarion-pulsar-unterminated.toml",
            format!("vantage = \"v\"\nrpc_url = \"{keyed}\n"),
            "at line 2, column ",
        ),
    ];

    for (name, document, position) in cases {
        let config = directory.join(name);
        std::fs::write(&config, document).unwrap();

        let output = binary(&["--config", &config.display().to_string()]).await;

        let stderr = text(&output.stderr);
        assert_eq!(output.status.code(), Some(1), "{name}");
        assert_eq!(stderr.lines().count(), 1, "{stderr}");
        assert!(!stderr.contains(SYNTHETIC_PROVIDER_KEY), "{stderr}");
        assert!(
            stderr.starts_with(&format!(
                "clarion-pulsar: failed to parse {} {position}",
                config.display()
            )),
            "{stderr}"
        );
        assert!(output.stdout.is_empty());
    }
}

#[tokio::test]
async fn rpc_failure_exits_one_with_one_line_that_never_shows_the_url() {
    let rpc = CannedRpc::scripted(vec![RpcReply::status(403, "Forbidden")]).await;
    let keyed = format!(
        "{}/{SYNTHETIC_PROVIDER_KEY}?api-key={SYNTHETIC_PROVIDER_KEY}",
        rpc.url
    );
    let directory = scratch_dir("cli-rpc-failure");
    let config = write_config(&directory, &keyed);

    let output = binary(&["--config", &config]).await;

    let stderr = text(&output.stderr);
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(stderr.lines().count(), 1, "{stderr}");
    assert!(stderr.ends_with('\n'));
    assert!(!stderr.contains(SYNTHETIC_PROVIDER_KEY), "{stderr}");
    assert!(!stderr.contains(&rpc.url), "{stderr}");
    assert_eq!(
        stderr,
        "clarion-pulsar: getGenesisHash failed after 1 of 5 attempts: rpc returned http status 403\n"
    );
    assert!(!directory.join("out").exists());
}

#[tokio::test]
async fn completed_run_exits_zero_and_names_both_files_and_dropped_counts() {
    let server = TpuServer::spawn(TPU_ALPN, Admission::Accept);
    let rpc = Cluster::with_tpu(&[(synthetic_key(1), server.address)])
        .serve()
        .await;
    let directory = scratch_dir("cli-run");
    let config = write_config(&directory, &rpc.url);

    let output = binary(&["--config", &config]).await;

    let stdout = text(&output.stdout);
    let out = directory.join("out");
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr));
    assert!(output.stderr.is_empty());
    assert!(stdout.starts_with("clarion-pulsar: cluster devnet, epoch 7\n"));
    assert!(stdout.contains(&format!("handshakes: {}/clarion-pulsar-", out.display())));
    assert!(stdout.contains(&format!(
        "validators: {}/clarion-pulsar-validators-",
        out.display()
    )));
    assert!(stdout.contains("1 of 1 rows ok"));
    assert!(stdout.contains(
        "dropped invalid entries: nodes 0, vote accounts 0, block production 0, \
         leader schedule 0"
    ));
    assert!(
        out.join("clarion-pulsar-leader-slots-devnet-7.csv")
            .exists()
    );
    let names: Vec<String> = std::fs::read_dir(&out)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    assert_eq!(names.len(), 3);
    assert!(names.iter().all(|name| name.starts_with("clarion-pulsar-")));
}

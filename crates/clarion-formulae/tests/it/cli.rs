use std::fs;

use clarion_program_client::{Arm, ArmAggregates};
use serde_json::Value;

use crate::support::{
    CORRECTED_LEDGER_SHA256, FORMULAE_ID_HEX, LEAVES, REVEAL_SLOT, ROOT_HEX,
    SOURCE_COMMIT, WINDOW_ID, bound, built, code, copy_fixtures, fixtures, formulae,
    formulae_dir, formulae_file, formulae_in, program_id, repo_root, revealed_window,
    signature, stderr, stdout, tmp, window_json, write, zero_arms,
};

fn record_json(path: &std::path::Path) -> Value {
    serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap()
}

fn bind_args<'a>(record: &'a str, window: &'a str, program: &'a str) -> Vec<&'a str> {
    vec![
        "bind",
        "--attestation",
        record,
        "--window",
        window,
        "--cluster",
        "devnet",
        "--program-id",
        program,
        "--commit-signature",
        "4tondpTQwrpu2S9inx1RGTJuhYqg2AmXLRURXAWxqg7H5KZTXj5Du6vH5PTHbpkDgo4VVurUZT3FFDfSzHXR5zjT",
        "--reveal-signature",
        "4tondpTQwrpu2S9inx1RGTJuhYqg2AmXLRURXAWxqg7H5KZTXj5Du6vH5PTHbpkDgo4VVurUZT3FFDfSzHXR5zjT",
    ]
}

#[test]
fn id_prints_the_display_form_and_the_hex() {
    let output = formulae(&["id", formulae_file().to_str().unwrap()]);

    assert_eq!(code(&output), 0);
    assert_eq!(
        stdout(&output),
        format!("formulae-d283858a6801 {FORMULAE_ID_HEX}\n")
    );
}

#[test]
fn id_fails_on_an_invalid_document_with_one_stderr_line() {
    let dir = tmp("id-invalid");
    let path = write(
        &dir.join("x.v1.json"),
        &fs::read_to_string(formulae_file())
            .unwrap()
            .replace("\"formulae.v1\"", "\"formulae.v9\""),
    );

    let output = formulae(&["id", &path]);

    assert_eq!(code(&output), 1);
    assert_eq!(stdout(&output), "");
    assert_eq!(
        stderr(&output),
        "clarion-formulae: formulae schema is \"formulae.v9\", expected \"formulae.v1\"\n"
    );
}

#[test]
fn build_writes_the_record_with_the_reference_leaves_and_root() {
    let (_, record) = built("build-reference");

    let value = record_json(&record);

    assert_eq!(value["schema"], "formulae.attestation.v1");
    assert_eq!(value["formulae"]["name"], "trace-completed.v1");
    assert_eq!(value["formulae"]["id"], FORMULAE_ID_HEX);
    assert_eq!(value["leaves"], serde_json::json!(LEAVES));
    assert_eq!(value["root"], ROOT_HEX);
    assert!(value.get("chain").is_none());
    assert_eq!(value["groups"][0]["group"], "system");
    assert_eq!(value["groups"][0]["fields"]["engine_version"], "0.1.0");
    assert_eq!(value["groups"][0]["fields"]["params_version"], 1);
    assert_eq!(value["groups"][0]["fields"]["source_commit"], SOURCE_COMMIT);
    assert_eq!(value["groups"][1]["fields"]["subject_id"], "replay");
    assert_eq!(value["groups"][1]["fields"]["paper_n"], 14_433);
    assert_eq!(value["groups"][1]["fields"]["corrected_n_kept"], 11_959);
    assert_eq!(
        value["groups"][1]["fields"]["corrected_total_usd"],
        -6_205_706_422_i64
    );
    assert_eq!(
        value["groups"][2]["fields"]["files"][1]["filename"],
        "corrected_ledger.json"
    );
    assert_eq!(
        value["groups"][2]["fields"]["files"][1]["sha256"],
        CORRECTED_LEDGER_SHA256
    );
    assert_eq!(
        value["groups"][2]["fields"]["input_sha256"]["fills/xyz_TSLA.csv"],
        "38de9398fa8eb163c8a593272fb70f67e03cd125a53c87eec568634ed7b57208"
    );
    assert_eq!(value["meta"]["tool_version"], env!("CARGO_PKG_VERSION"));
    assert!(value["meta"]["built_at_us"].as_u64().unwrap() > 1_700_000_000_000_000);
    assert!(
        fs::read_to_string(&record)
            .unwrap()
            .starts_with("{\n  \"formulae\": {\n")
    );
}

#[test]
fn build_prints_the_record_it_writes() {
    let dir = tmp("build-stdout");
    let record = dir.join("attestation.json");

    let output = formulae(&[
        "build",
        "--formulae",
        formulae_file().to_str().unwrap(),
        "--artifacts",
        fixtures().to_str().unwrap(),
        "--input",
        &format!("system.source_commit={SOURCE_COMMIT}"),
        "--input",
        "results.subject_id=replay",
        "--out",
        record.to_str().unwrap(),
        "--source-repo",
        "https://github.com/aetelierxyz/aetelier-tracer",
    ]);

    assert_eq!(code(&output), 0, "{}", stderr(&output));
    assert_eq!(stdout(&output), fs::read_to_string(&record).unwrap());
    assert_eq!(
        record_json(&record)["meta"]["source_repo"],
        "https://github.com/aetelierxyz/aetelier-tracer"
    );
}

#[test]
fn build_reports_every_problem_at_once_and_writes_nothing() {
    let dir = tmp("build-report");
    let empty = dir.join("empty");
    fs::create_dir_all(&empty).unwrap();
    let record = dir.join("attestation.json");

    let output = formulae(&[
        "build",
        "--formulae",
        formulae_file().to_str().unwrap(),
        "--artifacts",
        empty.to_str().unwrap(),
        "--input",
        "results.subject_id=replay",
        "--input",
        "extra.key=1",
        "--out",
        record.to_str().unwrap(),
    ]);

    assert_eq!(code(&output), 1);
    assert_eq!(stderr(&output), "");
    assert!(!record.exists());
    let report: Value = serde_json::from_str(&stdout(&output)).unwrap();
    assert_eq!(report["schema"], "formulae.report.v1");
    let fields: Vec<&str> = report["problems"]
        .as_array()
        .unwrap()
        .iter()
        .map(|problem| problem["field"].as_str().unwrap())
        .collect();
    assert_eq!(
        fields,
        [
            "extra.key",
            "system.engine_version",
            "system.params_version",
            "system.source_commit",
            "results.paper_n",
            "results.corrected_n_kept",
            "results.corrected_total_usd",
            "artifacts.files",
            "artifacts.input_sha256",
        ]
    );
    assert_eq!(report["problems"][0]["reason"], "is not a declared input");
    assert_eq!(report["problems"][3]["reason"], "input is missing");
    assert!(
        report["problems"][1]["reason"]
            .as_str()
            .unwrap()
            .starts_with("artifact ")
    );
}

#[test]
fn build_rejects_a_malformed_input_and_reserved_formulae_values() {
    let dir = tmp("build-rejects");
    let record = dir.join("attestation.json");
    let output = formulae(&[
        "build",
        "--formulae",
        formulae_file().to_str().unwrap(),
        "--artifacts",
        fixtures().to_str().unwrap(),
        "--input",
        "subject_id",
        "--out",
        record.to_str().unwrap(),
    ]);
    assert_eq!(code(&output), 1);
    assert_eq!(
        stderr(&output),
        "clarion-formulae: input \"subject_id\" is not group.field=value\n"
    );

    let reserved = write(
        &dir.join("reserved.v1.json"),
        &fs::read_to_string(formulae_file())
            .unwrap()
            .replace("\"preimages\"", "\"hashes_only\"")
            .replace("\"per_group\"", "\"per_element\""),
    );
    let output = formulae(&[
        "build",
        "--formulae",
        &reserved,
        "--artifacts",
        fixtures().to_str().unwrap(),
        "--input",
        &format!("system.source_commit={SOURCE_COMMIT}"),
        "--input",
        "results.subject_id=replay",
        "--out",
        record.to_str().unwrap(),
    ]);
    assert_eq!(code(&output), 1);
    let report: Value = serde_json::from_str(&stdout(&output)).unwrap();
    assert_eq!(report["problems"][0]["field"], "publication");
    assert_eq!(report["problems"][1]["field"], "leaf_mode");
    assert_eq!(report["problems"].as_array().unwrap().len(), 2);
    assert!(!record.exists());
}

#[test]
fn build_reports_a_value_of_another_type_than_declared() {
    let dir = tmp("build-type");
    let artifacts = copy_fixtures(&dir);
    let ledger = artifacts.join("corrected_ledger.json");
    let edited = fs::read_to_string(&ledger)
        .unwrap()
        .replace("\"n\": 14433", "\"n\": \"14433\"");
    fs::write(&ledger, edited).unwrap();

    let output = formulae(&[
        "build",
        "--formulae",
        formulae_file().to_str().unwrap(),
        "--artifacts",
        artifacts.to_str().unwrap(),
        "--input",
        &format!("system.source_commit={SOURCE_COMMIT}"),
        "--input",
        "results.subject_id=replay",
        "--out",
        dir.join("attestation.json").to_str().unwrap(),
    ]);

    assert_eq!(code(&output), 1);
    let report: Value = serde_json::from_str(&stdout(&output)).unwrap();
    assert_eq!(
        report["problems"],
        serde_json::json!([{
            "field": "results.paper_n",
            "reason": "value is not an unsigned integer",
        }])
    );
}

#[test]
fn bind_adds_the_chain_and_refuses_a_second_binding() {
    let (_, record, window) = bound("bind-chain");

    let value = record_json(&record);
    assert_eq!(
        value["chain"],
        serde_json::json!({
            "cluster": "devnet",
            "program_id": program_id().to_string(),
            "window_id": WINDOW_ID,
            "commit_signature": signature(1),
            "commit_slot": 1_500,
            "reveal_signature": signature(2),
            "reveal_slot": REVEAL_SLOT,
        })
    );
    assert_eq!(value["root"], ROOT_HEX);

    let output = formulae(&bind_args(
        record.to_str().unwrap(),
        window.to_str().unwrap(),
        &program_id().to_string(),
    ));

    assert_eq!(code(&output), 1);
    assert_eq!(
        stderr(&output),
        format!(
            "clarion-formulae: record is already bound to window {WINDOW_ID} of {} on devnet\n",
            program_id()
        )
    );
}

#[test]
fn bind_refuses_a_window_that_does_not_match_the_record() {
    let (dir, record) = built("bind-mismatch");
    let other_root = format!("{}{}", "00", &ROOT_HEX[2..]);
    let mut landed = zero_arms();
    landed[2].tx_submitted = 1;
    let mut reordered = zero_arms();
    reordered.swap(0, 1);
    let cases: [(&str, String, &str); 5] = [
        (
            "root",
            window_json(
                &program_id(),
                WINDOW_ID,
                &other_root,
                REVEAL_SLOT,
                zero_arms(),
            ),
            "window merkle root 00",
        ),
        (
            "unrevealed",
            window_json(&program_id(), WINDOW_ID, ROOT_HEX, 0, zero_arms()),
            "window 3 is not revealed",
        ),
        (
            "landed",
            window_json(&program_id(), WINDOW_ID, ROOT_HEX, REVEAL_SLOT, landed),
            "aggregates of arm rpc_default are not zero",
        ),
        (
            "order",
            window_json(&program_id(), WINDOW_ID, ROOT_HEX, REVEAL_SLOT, reordered),
            "are not in arm order",
        ),
        (
            "program",
            window_json(
                &solana_program::pubkey::Pubkey::new_from_array([8; 32]),
                WINDOW_ID,
                ROOT_HEX,
                REVEAL_SLOT,
                zero_arms(),
            ),
            "window address ",
        ),
    ];

    for (name, window, expected) in cases {
        let path = write(&dir.join(format!("{name}.json")), &window);

        let output = formulae(&bind_args(
            record.to_str().unwrap(),
            &path,
            &program_id().to_string(),
        ));

        assert_eq!(code(&output), 1, "{name}");
        let text = stderr(&output);
        assert!(
            text.starts_with("clarion-formulae: window does not match the record: "),
            "{name}: {text}"
        );
        assert!(text.contains(expected), "{name}: {text}");
        assert!(record_json(&record).get("chain").is_none(), "{name}");
    }
}

#[test]
fn bind_rejects_a_cluster_name_with_other_characters() {
    let (dir, record) = built("bind-cluster");
    let window = write(&dir.join("show.json"), &revealed_window());
    let program = program_id().to_string();
    let mut args = bind_args(record.to_str().unwrap(), &window, &program);
    args[6] = "Devnet";

    let output = formulae(&args);

    assert_eq!(code(&output), 1);
    assert_eq!(
        stderr(&output),
        "clarion-formulae: cluster \"Devnet\" is not lowercase letters, digits and hyphens\n"
    );
}

#[test]
fn verify_passes_all_three_layers_on_a_bound_record() {
    let (_, record, window) = bound("verify-pass");

    let output = formulae(&[
        "verify",
        "--attestation",
        record.to_str().unwrap(),
        "--window",
        window.to_str().unwrap(),
        "--formulae",
        formulae_file().to_str().unwrap(),
        "--artifacts",
        fixtures().to_str().unwrap(),
    ]);

    assert_eq!(code(&output), 0, "{}", stderr(&output));
    assert_eq!(
        stdout(&output),
        "layer 1 formulae: pass\nlayer 2 chain: pass\nlayer 3 artifacts: pass\nstate: Revealed\n"
    );
}

#[test]
fn verify_resolves_the_formulae_by_name_and_skips_layer_three_without_artifacts() {
    let (dir, record, window) = bound("verify-resolve");

    let output = formulae_in(
        &repo_root(),
        &[
            "verify",
            "--attestation",
            record.to_str().unwrap(),
            "--window",
            window.to_str().unwrap(),
        ],
    );
    assert_eq!(code(&output), 0, "{}", stderr(&output));
    assert_eq!(
        stdout(&output),
        "layer 1 formulae: pass\nlayer 2 chain: pass\nlayer 3 artifacts: skipped\nstate: Revealed\n"
    );

    let output = formulae(&[
        "verify",
        "--attestation",
        record.to_str().unwrap(),
        "--window",
        window.to_str().unwrap(),
        "--formulae-dir",
        dir.to_str().unwrap(),
    ]);
    assert_eq!(code(&output), 1);
    let text = stdout(&output);
    assert!(
        text.starts_with(&format!(
            "layer 1 formulae: fail: formulae file {}: ",
            dir.join("trace-completed.v1.json").display()
        )),
        "{text}"
    );
    assert!(
        text.ends_with(
            "layer 2 chain: pass\nlayer 3 artifacts: skipped\nstate: Revealed\n"
        )
    );
}

#[test]
fn verify_fails_layer_one_on_an_edited_formulae() {
    let (dir, record, window) = bound("verify-layer-one");
    let edited = write(
        &dir.join("trace-completed.v1.json"),
        &fs::read_to_string(formulae_file())
            .unwrap()
            .replace("\"/paper/n\"", "\"/corrected/n_kept\""),
    );

    let output = formulae(&[
        "verify",
        "--attestation",
        record.to_str().unwrap(),
        "--window",
        window.to_str().unwrap(),
        "--formulae",
        &edited,
    ]);

    assert_eq!(code(&output), 1);
    let text = stdout(&output);
    assert!(
        text.starts_with("layer 1 formulae: fail: formulae id "),
        "{text}"
    );
    assert!(
        text.contains(&format!("differs from the record id {FORMULAE_ID_HEX}")),
        "{text}"
    );
}

#[test]
fn verify_reports_built_and_committed_states_for_an_unbound_record() {
    let (dir, record) = built("verify-unbound");
    let committed = write(
        &dir.join("committed.json"),
        &window_json(&program_id(), WINDOW_ID, ROOT_HEX, 0, zero_arms()),
    );
    let foreign = write(
        &dir.join("foreign.json"),
        &window_json(&program_id(), WINDOW_ID, &"ab".repeat(32), 0, zero_arms()),
    );

    for (window, state) in [(committed, "Committed"), (foreign, "Built")] {
        let output = formulae(&[
            "verify",
            "--attestation",
            record.to_str().unwrap(),
            "--window",
            &window,
            "--formulae",
            formulae_file().to_str().unwrap(),
        ]);

        assert_eq!(code(&output), 1, "{state}");
        assert_eq!(
            stdout(&output),
            format!(
                "layer 1 formulae: pass\nlayer 2 chain: fail: record is not bound\nlayer 3 artifacts: skipped\nstate: {state}\n"
            )
        );
    }
}

#[test]
fn verify_fails_layer_two_when_a_recorded_field_or_the_window_changes() {
    let (dir, record, window) = bound("verify-layer-two");
    let edited = write(
        &dir.join("edited.json"),
        &fs::read_to_string(&record)
            .unwrap()
            .replace("\"paper_n\": 14433", "\"paper_n\": 14434"),
    );
    let other_window = write(
        &dir.join("other-window.json"),
        &window_json(
            &program_id(),
            WINDOW_ID + 1,
            ROOT_HEX,
            REVEAL_SLOT,
            zero_arms(),
        ),
    );
    let other_program = write(
        &dir.join("other-program.json"),
        &window_json(
            &solana_program::pubkey::Pubkey::new_from_array([9; 32]),
            WINDOW_ID,
            ROOT_HEX,
            REVEAL_SLOT,
            zero_arms(),
        ),
    );
    let cases = [
        (
            edited,
            window.to_str().unwrap().to_owned(),
            "record leaves differ from the leaves of its groups",
        ),
        (
            record.to_str().unwrap().to_owned(),
            other_window,
            "record window id 3 differs from the window id 4",
        ),
        (
            record.to_str().unwrap().to_owned(),
            other_program,
            "window address ",
        ),
    ];

    for (attestation, window, expected) in cases {
        let output = formulae(&[
            "verify",
            "--attestation",
            &attestation,
            "--window",
            &window,
            "--formulae",
            formulae_file().to_str().unwrap(),
            "--artifacts",
            fixtures().to_str().unwrap(),
        ]);

        assert_eq!(code(&output), 1, "{expected}");
        let text = stdout(&output);
        assert!(
            text.contains(&format!("layer 2 chain: fail: {expected}")),
            "{text}"
        );
        assert!(text.ends_with("state: Built\n"), "{text}");
    }
}

#[test]
fn verify_fails_layer_three_on_a_tampered_artifact() {
    let (dir, record, window) = bound("verify-layer-three");
    let artifacts = copy_fixtures(&dir);
    fs::write(artifacts.join("fillrates.json"), "{\"symbols\": 6}\n").unwrap();

    let output = formulae(&[
        "verify",
        "--attestation",
        record.to_str().unwrap(),
        "--window",
        window.to_str().unwrap(),
        "--formulae",
        formulae_file().to_str().unwrap(),
        "--artifacts",
        artifacts.to_str().unwrap(),
    ]);

    assert_eq!(code(&output), 1);
    let text = stdout(&output);
    assert!(
        text.contains("layer 3 artifacts: fail: field artifacts.files re-derives as "),
        "{text}"
    );
    assert!(
        text.starts_with("layer 1 formulae: pass\nlayer 2 chain: pass\n"),
        "{text}"
    );
    assert!(text.ends_with("state: Revealed\n"), "{text}");
}

#[test]
fn verify_fails_layer_three_when_a_pointer_value_changes() {
    let (dir, record, window) = bound("verify-pointer");
    let artifacts = copy_fixtures(&dir);
    let ledger = artifacts.join("corrected_ledger.json");
    let edited = fs::read_to_string(&ledger)
        .unwrap()
        .replace("\"n_kept\": 11959", "\"n_kept\": 11958");
    fs::write(&ledger, edited).unwrap();

    let output = formulae(&[
        "verify",
        "--attestation",
        record.to_str().unwrap(),
        "--window",
        window.to_str().unwrap(),
        "--formulae",
        formulae_file().to_str().unwrap(),
        "--artifacts",
        artifacts.to_str().unwrap(),
    ]);

    assert_eq!(code(&output), 1);
    assert!(
        stdout(&output).contains(
            "layer 3 artifacts: fail: field results.corrected_n_kept re-derives as 11958 against the recorded 11959"
        ),
        "{}",
        stdout(&output)
    );
}

#[test]
fn window_with_nonzero_or_reordered_arms_fails_verification() {
    let (dir, record, _) = bound("verify-arms");
    let mut landed = zero_arms();
    landed[3] = ArmAggregates {
        tx_landed: 1,
        ..ArmAggregates::empty(Arm::FixedPriorityFee)
    };
    let landed = write(
        &dir.join("landed.json"),
        &window_json(&program_id(), WINDOW_ID, ROOT_HEX, REVEAL_SLOT, landed),
    );

    let output = formulae(&[
        "verify",
        "--attestation",
        record.to_str().unwrap(),
        "--window",
        &landed,
        "--formulae",
        formulae_file().to_str().unwrap(),
    ]);

    assert_eq!(code(&output), 1);
    assert!(
        stdout(&output).contains(
            "layer 2 chain: fail: aggregates of arm fixed_priority_fee are not zero"
        ),
        "{}",
        stdout(&output)
    );
}

#[test]
fn usage_errors_exit_with_two_and_help_lists_the_subcommands() {
    let none = formulae(&[]);
    let unknown = formulae(&["verify", "--attestation", "x", "--window", "y", "--chain"]);
    let help = formulae(&["--help"]);

    assert_eq!(code(&none), 2);
    assert!(
        stderr(&none).starts_with("clarion-formulae: "),
        "{}",
        stderr(&none)
    );
    assert_eq!(code(&unknown), 2);
    assert!(stderr(&unknown).contains("--chain"), "{}", stderr(&unknown));
    assert!(stderr(&unknown).contains("Usage: "), "{}", stderr(&unknown));
    assert_eq!(code(&help), 0);
    for subcommand in ["id", "build", "bind", "verify"] {
        assert!(
            stdout(&help).contains(&format!("\n  {subcommand} ")),
            "{subcommand}"
        );
    }
}

#[test]
fn the_formulae_directory_holds_the_shipped_document() {
    assert!(formulae_dir().join("trace-completed.v1.json").is_file());
    assert!(formulae_dir().join("ids.txt").is_file());
}

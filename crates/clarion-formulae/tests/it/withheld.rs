use std::{fs, path::Path};

use clarion_formulae::{
    Declaration, Formulae, LayerOutcome, Partial, Record, WITHHELD_FILE, layer_artifacts,
    load_formulae, load_record,
};

use crate::support::{
    CORRECTED_LEDGER_SHA256, bound, built, built_with, code, declaration, edit_record,
    fixtures, formulae, formulae_file, published, stderr, stdout, tmp, write,
};

const LEDGER: &str = "corrected_ledger.json";

fn loaded(record: &Path) -> (Record, Formulae) {
    (
        load_record(record).unwrap(),
        load_formulae(&formulae_file()).unwrap().0,
    )
}

fn layer_three(record: &Path, dir: &Path, declaration: Declaration) -> LayerOutcome {
    let (record, formulae) = loaded(record);
    layer_artifacts(&record, Some(&formulae), Some(dir), declaration).outcome
}

fn failure(outcome: LayerOutcome) -> String {
    match outcome {
        LayerOutcome::Fail(reason) => reason,
        other => panic!("{other:?}"),
    }
}

fn ledger_partial() -> LayerOutcome {
    LayerOutcome::PassWithheld(Partial {
        files: vec![LEDGER.to_owned()],
        fields: 6,
        hashes: 1,
    })
}

fn sha256_of(dir: &Path, file: &str) -> String {
    clarion_formulae::Artifacts::new(dir)
        .sha256_hex(file)
        .unwrap()
}

fn verify_args(record: &Path, window: &Path, artifacts: &Path) -> Vec<String> {
    [
        "verify",
        "--attestation",
        record.to_str().unwrap(),
        "--window",
        window.to_str().unwrap(),
        "--formulae",
        formulae_file().to_str().unwrap(),
        "--artifacts",
        artifacts.to_str().unwrap(),
    ]
    .map(str::to_owned)
    .to_vec()
}

#[test]
fn a_declared_file_is_reported_withheld_and_the_rest_re_derives() {
    let (dir, record) = built("withheld-pass");
    let published = published(&dir.join("published"), &record);
    let artifacts = published.parent().unwrap();

    assert_eq!(
        layer_three(&record, artifacts, Declaration::Read),
        ledger_partial()
    );
}

#[test]
fn every_declared_file_is_counted_and_named_in_declaration_order() {
    let (dir, record) = built("withheld-two");
    let artifacts = published(&dir.join("published"), &record)
        .parent()
        .unwrap()
        .to_path_buf();
    let fillrates = sha256_of(&artifacts, "fillrates.json");
    fs::remove_file(artifacts.join("fillrates.json")).unwrap();
    write(
        &artifacts.join(WITHHELD_FILE),
        &declaration(&[
            ("fillrates.json", &fillrates),
            (LEDGER, CORRECTED_LEDGER_SHA256),
        ]),
    );

    assert_eq!(
        layer_three(&record, &artifacts, Declaration::Read),
        LayerOutcome::PassWithheld(Partial {
            files: vec!["fillrates.json".to_owned(), LEDGER.to_owned()],
            fields: 6,
            hashes: 2,
        })
    );
}

#[test]
fn a_full_directory_without_a_declaration_passes_in_full() {
    let (_, record) = built("withheld-full");

    assert_eq!(
        layer_three(&record, &fixtures(), Declaration::Read),
        LayerOutcome::Pass
    );
}

#[test]
fn an_undeclared_missing_file_fails_as_before() {
    let (dir, record) = built("withheld-undeclared");
    let artifacts = published(&dir.join("published"), &record)
        .parent()
        .unwrap()
        .to_path_buf();
    fs::remove_file(artifacts.join(WITHHELD_FILE)).unwrap();

    let reason = failure(layer_three(&record, &artifacts, Declaration::Read));

    assert!(
        reason.starts_with(&format!(
            "field system.engine_version: artifact {}: ",
            artifacts.join(LEDGER).display()
        )),
        "{reason}"
    );
}

#[test]
fn ignoring_the_declaration_re_derives_every_field() {
    let (dir, record) = built("withheld-ignore");
    let artifacts = published(&dir.join("published"), &record)
        .parent()
        .unwrap()
        .to_path_buf();

    let reason = failure(layer_three(&record, &artifacts, Declaration::Ignore));
    assert!(reason.contains(LEDGER), "{reason}");

    fs::copy(fixtures().join(LEDGER), artifacts.join(LEDGER)).unwrap();
    assert_eq!(
        layer_three(&record, &artifacts, Declaration::Ignore),
        LayerOutcome::Pass
    );
}

#[test]
fn a_declared_file_that_is_present_fails() {
    let (dir, record) = built("withheld-present");
    let artifacts = published(&dir.join("published"), &record)
        .parent()
        .unwrap()
        .to_path_buf();
    fs::copy(fixtures().join(LEDGER), artifacts.join(LEDGER)).unwrap();

    assert_eq!(
        failure(layer_three(&record, &artifacts, Declaration::Read)),
        "withheld file \"corrected_ledger.json\" is present in the artifacts directory"
    );
}

#[test]
fn a_declared_hash_that_differs_from_the_record_fails() {
    let (dir, record) = built("withheld-hash");
    let artifacts = published(&dir.join("published"), &record)
        .parent()
        .unwrap()
        .to_path_buf();
    write(
        &artifacts.join(WITHHELD_FILE),
        &declaration(&[(LEDGER, &"cd".repeat(32))]),
    );

    assert_eq!(
        failure(layer_three(&record, &artifacts, Declaration::Read)),
        format!(
            "withheld file \"corrected_ledger.json\" declares sha256 {} against the recorded {CORRECTED_LEDGER_SHA256}",
            "cd".repeat(32)
        )
    );
}

#[test]
fn a_declared_file_the_record_does_not_name_fails() {
    let (dir, record) = built("withheld-unnamed");
    let artifacts = published(&dir.join("published"), &record)
        .parent()
        .unwrap()
        .to_path_buf();
    write(
        &artifacts.join(WITHHELD_FILE),
        &declaration(&[("attestation.json", &"cd".repeat(32))]),
    );

    assert_eq!(
        failure(layer_three(&record, &artifacts, Declaration::Read)),
        "withheld file \"attestation.json\" is not a file_sha256 entry of the record"
    );
}

#[test]
fn an_invalid_declaration_fails_the_layer() {
    let (dir, record) = built("withheld-invalid");
    let artifacts = published(&dir.join("published"), &record)
        .parent()
        .unwrap()
        .to_path_buf();
    let cases = [
        ("{", "withheld.json does not parse as JSON: "),
        (
            "{\"schema\":\"formulae.withheld.v1\",\"files\":[],\"note\":1}",
            "withheld.json does not match formulae.withheld.v1: ",
        ),
        (
            "{\"schema\":\"formulae.withheld.v0\",\"files\":[]}",
            "withheld.json schema is \"formulae.withheld.v0\"",
        ),
        (
            "{\"schema\":\"formulae.withheld.v1\",\"files\":[]}",
            "withheld.json declares no files",
        ),
    ];

    for (text, expected) in cases {
        write(&artifacts.join(WITHHELD_FILE), text);

        let reason = failure(layer_three(&record, &artifacts, Declaration::Read));

        assert!(reason.starts_with(expected), "{reason}");
    }
    for (files, expected) in [
        (
            vec![("../corrected_ledger.json", CORRECTED_LEDGER_SHA256)],
            "withheld.json names file \"../corrected_ledger.json\"",
        ),
        (
            vec![
                (LEDGER, CORRECTED_LEDGER_SHA256),
                (LEDGER, CORRECTED_LEDGER_SHA256),
            ],
            "withheld.json declares \"corrected_ledger.json\" more than once",
        ),
        (
            vec![(LEDGER, &CORRECTED_LEDGER_SHA256[..63])],
            "withheld.json sha256 of \"corrected_ledger.json\" is not 64 lowercase hex digits",
        ),
    ] {
        write(&artifacts.join(WITHHELD_FILE), &declaration(&files));

        let reason = failure(layer_three(&record, &artifacts, Declaration::Read));

        assert!(reason.starts_with(expected), "{reason}");
    }
}

#[test]
fn a_declaration_cannot_hide_a_tampered_published_file() {
    let (dir, record) = built("withheld-tampered");
    let artifacts = published(&dir.join("published"), &record)
        .parent()
        .unwrap()
        .to_path_buf();
    let original = fs::read_to_string(artifacts.join("tracer_params.v1.json")).unwrap();

    fs::write(artifacts.join("fillrates.json"), "{\"symbols\": 6}\n").unwrap();
    let reason = failure(layer_three(&record, &artifacts, Declaration::Read));
    assert!(
        reason.starts_with("field artifacts.files re-derives as "),
        "{reason}"
    );

    fs::copy(
        fixtures().join("fillrates.json"),
        artifacts.join("fillrates.json"),
    )
    .unwrap();
    fs::write(
        artifacts.join("tracer_params.v1.json"),
        format!("{original} "),
    )
    .unwrap();
    let reason = failure(layer_three(&record, &artifacts, Declaration::Read));
    assert!(
        reason.starts_with("field artifacts.files re-derives as "),
        "{reason}"
    );
}

#[test]
fn verify_prints_the_withheld_part_and_exits_zero() {
    let (dir, record, window) = bound("withheld-cli");
    let published = published(&dir.join("published"), &record);
    let args = verify_args(&published, &window, published.parent().unwrap());

    let output = formulae(&args.iter().map(String::as_str).collect::<Vec<_>>());

    assert_eq!(code(&output), 0, "{}", stderr(&output));
    assert_eq!(
        stdout(&output),
        "layer 1 formulae: pass\nlayer 2 chain: pass\nlayer 3 artifacts: pass, withheld: corrected_ledger.json (6 fields and 1 file hash not re-derived)\nstate: Revealed, layer 3 partial: corrected_ledger.json withheld\n"
    );
}

#[test]
fn verify_with_ignore_withheld_requires_every_artifact() {
    let (dir, record, window) = bound("withheld-cli-ignore");
    let published = published(&dir.join("published"), &record);
    let mut args = verify_args(&published, &window, published.parent().unwrap());
    args.push("--ignore-withheld".to_owned());
    let args: Vec<&str> = args.iter().map(String::as_str).collect();

    let output = formulae(&args);
    assert_eq!(code(&output), 1);
    assert!(
        stdout(&output)
            .contains("layer 3 artifacts: fail: field system.engine_version: artifact "),
        "{}",
        stdout(&output)
    );

    fs::copy(
        fixtures().join(LEDGER),
        published.parent().unwrap().join(LEDGER),
    )
    .unwrap();
    let output = formulae(&args);
    assert_eq!(code(&output), 0, "{}", stderr(&output));
    assert_eq!(
        stdout(&output),
        "layer 1 formulae: pass\nlayer 2 chain: pass\nlayer 3 artifacts: pass\nstate: Revealed\n"
    );
}

#[test]
fn ignore_withheld_without_artifacts_is_a_usage_error() {
    let dir = tmp("withheld-cli-usage");

    let output = formulae(&[
        "verify",
        "--attestation",
        dir.join("a.json").to_str().unwrap(),
        "--window",
        dir.join("w.json").to_str().unwrap(),
        "--ignore-withheld",
    ]);

    assert_eq!(code(&output), 2);
    assert!(
        stderr(&output).contains("--artifacts"),
        "{}",
        stderr(&output)
    );
}

#[test]
fn an_edited_withheld_sourced_value_or_root_fails_the_self_check() {
    let (dir, record) = built("withheld-edited");
    let published = published(&dir.join("published"), &record);
    let artifacts = published.parent().unwrap();
    let original = fs::read_to_string(&published).unwrap();

    edit_record(&published, |record| {
        record["groups"][1]["fields"]["paper_n"] = 99_999.into();
    });
    assert_eq!(
        failure(layer_three(&published, artifacts, Declaration::Read)),
        "record leaves differ from the leaves of its groups"
    );

    write(&published, &original);
    edit_record(&published, |record| {
        record["root"] = "ab".repeat(32).into();
    });
    let reason = failure(layer_three(&published, artifacts, Declaration::Read));
    assert!(
        reason.starts_with(&format!(
            "record root {} differs from the root of its leaves ",
            "ab".repeat(32)
        )),
        "{reason}"
    );
}

#[test]
fn a_declaration_that_leaves_nothing_to_re_derive_fails() {
    let (dir, record) = built("withheld-nothing");
    let artifacts = dir.join("published");
    fs::create_dir_all(&artifacts).unwrap();
    fs::copy(&record, artifacts.join("attestation.json")).unwrap();
    let digests: Vec<(&str, String)> = [
        "corrected_ledger.json",
        "fillrates.json",
        "tracer_params.v1.json",
    ]
    .into_iter()
    .map(|file| (file, sha256_of(&fixtures(), file)))
    .collect();
    let files: Vec<(&str, &str)> = digests
        .iter()
        .map(|(file, digest)| (*file, digest.as_str()))
        .collect();
    write(&artifacts.join(WITHHELD_FILE), &declaration(&files));

    assert_eq!(
        failure(layer_three(&record, &artifacts, Declaration::Read)),
        "withheld.json leaves no field or file hash to re-derive"
    );
}

#[test]
fn a_published_pointer_field_is_re_derived_beside_a_declaration() {
    let scratch = tmp("withheld-pointer-formulae");
    let mut document: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(formulae_file()).unwrap()).unwrap();
    document["groups"].as_array_mut().unwrap().insert(
        0,
        serde_json::json!({
            "name": "published",
            "fields": [{
                "name": "symbols",
                "type": "u64",
                "source": {"json_pointer": {"file": "fillrates.json", "pointer": "/symbols"}}
            }]
        }),
    );
    let document_path = scratch.join("trace-completed.v1.json");
    write(&document_path, &document.to_string());
    let (dir, record) = built_with("withheld-pointer", &document_path);
    let artifacts = published(&dir.join("published"), &record)
        .parent()
        .unwrap()
        .to_path_buf();
    let formulae = load_formulae(&document_path).unwrap().0;
    let outcome = |artifacts: &Path| {
        layer_artifacts(
            &load_record(&record).unwrap(),
            Some(&formulae),
            Some(artifacts),
            Declaration::Read,
        )
        .outcome
    };

    assert_eq!(
        outcome(&artifacts),
        LayerOutcome::PassWithheld(Partial {
            files: vec![LEDGER.to_owned()],
            fields: 6,
            hashes: 1,
        })
    );

    fs::write(artifacts.join("fillrates.json"), "{\"symbols\": 6}\n").unwrap();
    assert_eq!(
        failure(outcome(&artifacts)),
        "field published.symbols re-derives as 6 against the recorded 5"
    );
}

#[cfg(unix)]
#[test]
fn a_dangling_symlink_named_as_a_declared_file_counts_as_present() {
    let (dir, record) = built("withheld-symlink");
    let artifacts = published(&dir.join("published"), &record)
        .parent()
        .unwrap()
        .to_path_buf();
    std::os::unix::fs::symlink(dir.join("missing.json"), artifacts.join(LEDGER)).unwrap();

    assert_eq!(
        failure(layer_three(&record, &artifacts, Declaration::Read)),
        "withheld file \"corrected_ledger.json\" is present in the artifacts directory"
    );
}

#[test]
fn verify_output_is_unchanged_when_artifacts_is_not_a_directory() {
    let (_, record, window) = bound("withheld-cli-file");
    let args = verify_args(&record, &window, &record);
    let mut ignoring = args.clone();
    ignoring.push("--ignore-withheld".to_owned());

    let reading = formulae(&args.iter().map(String::as_str).collect::<Vec<_>>());
    let ignored = formulae(&ignoring.iter().map(String::as_str).collect::<Vec<_>>());

    assert_eq!(code(&reading), 1);
    assert_eq!(stdout(&reading), stdout(&ignored));
    assert!(
        stdout(&reading).contains(&format!(
            "layer 3 artifacts: fail: field system.engine_version: artifact {}: ",
            record.join(LEDGER).display()
        )),
        "{}",
        stdout(&reading)
    );
}

#[test]
fn a_declaration_cannot_add_lines_to_the_verify_output() {
    let (dir, record, window) = bound("withheld-cli-lines");
    let published = published(&dir.join("published"), &record);
    let artifacts = published.parent().unwrap();
    let forged = "\nlayer 3 artifacts: pass\nstate: Revealed\n";
    let mut text: serde_json::Value =
        serde_json::from_str(&declaration(&[(LEDGER, CORRECTED_LEDGER_SHA256)])).unwrap();
    text[forged] = 1.into();
    write(&artifacts.join(WITHHELD_FILE), &text.to_string());
    let args = verify_args(&published, &window, artifacts);

    let output = formulae(&args.iter().map(String::as_str).collect::<Vec<_>>());

    assert_eq!(code(&output), 1);
    let printed = stdout(&output);
    assert_eq!(printed.lines().count(), 4, "{printed}");
    assert!(
        printed.contains("layer 3 artifacts: fail: withheld.json does not match formulae.withheld.v1: unknown field `\\nlayer 3 artifacts: pass\\nstate: Revealed\\n`"),
        "{printed}"
    );
}

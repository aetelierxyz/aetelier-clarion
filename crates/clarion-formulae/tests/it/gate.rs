use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

use clarion_formulae::{
    Declaration, LayerOutcome, Partial, WITHHELD_FILE, formulae_path, layer_artifacts,
    layer_formulae, load_formulae, load_record,
};

use crate::support::{
    CORRECTED_LEDGER_SHA256, FORMULAE_ID_HEX, built, declaration, edit_record, fixtures,
    formulae_dir, published, repo_root, write,
};

fn golden_ids() -> BTreeMap<String, String> {
    fs::read_to_string(formulae_dir().join("ids.txt"))
        .unwrap()
        .lines()
        .map(|line| {
            let (name, id) = line.split_once(' ').unwrap();
            (name.to_owned(), id.to_owned())
        })
        .collect()
}

fn formulae_files() -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = fs::read_dir(formulae_dir())
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "json")
        })
        .collect();
    files.sort();
    files
}

fn records_under(dir: &Path, found: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries {
        let path = entry.unwrap().path();
        if path.is_dir() {
            records_under(&path, found);
        } else if path
            .file_name()
            .is_some_and(|name| name == "attestation.json")
        {
            found.push(path);
        }
    }
}

const WITHHOLDABLE: [&str; 1] = ["corrected_ledger.json"];

fn rejected(layer: u8, outcome: LayerOutcome) -> String {
    match outcome {
        LayerOutcome::Fail(reason) => format!("layer {layer} fail: {reason}"),
        outcome => format!("layer {layer} {outcome:?}"),
    }
}

fn gate(path: &Path) -> Result<LayerOutcome, String> {
    let record = load_record(path).map_err(|error| error.to_string())?;
    let dir = path.parent().ok_or("record has no directory")?;
    let document = formulae_path(&formulae_dir(), &record.formulae.name);
    let text = fs::read_to_string(&document)
        .map_err(|error| format!("{}: {error}", document.display()));
    let (first, formulae) =
        layer_formulae(&record, text.as_deref().map_err(Clone::clone));
    if first.outcome != LayerOutcome::Pass {
        return Err(rejected(1, first.outcome));
    }
    let third = layer_artifacts(&record, formulae.as_ref(), Some(dir), Declaration::Read);
    match third.outcome {
        LayerOutcome::Pass => Ok(LayerOutcome::Pass),
        LayerOutcome::PassWithheld(partial) => match partial
            .files
            .iter()
            .find(|file| !WITHHOLDABLE.contains(&file.as_str()))
        {
            Some(file) => Err(format!(
                "layer 3 withholds {file:?}; the records gate allows only {WITHHOLDABLE:?}"
            )),
            None => Ok(LayerOutcome::PassWithheld(partial)),
        },
        outcome => Err(rejected(3, outcome)),
    }
}

fn gate_failures(attestations: &Path) -> (usize, Vec<String>) {
    let mut records = Vec::new();
    records_under(attestations, &mut records);
    records.sort();
    let failures = records
        .iter()
        .filter_map(|path| {
            gate(path)
                .err()
                .map(|reason| format!("{}: {reason}", path.display()))
        })
        .collect();
    (records.len(), failures)
}

fn tree(test: &str) -> (PathBuf, PathBuf) {
    let (dir, record) = built(test);
    let attestations = dir.join("attestations");
    let published = published(
        &attestations.join("devnet").join("program").join("3"),
        &record,
    );
    (attestations, published.parent().unwrap().to_path_buf())
}

fn single_failure(attestations: &Path) -> String {
    let (records, failures) = gate_failures(attestations);
    assert_eq!(records, 1);
    assert_eq!(failures.len(), 1, "{failures:?}");
    failures[0].clone()
}

fn passes_alone(attestations: &Path) {
    assert_eq!(gate_failures(attestations), (1, Vec::<String>::new()));
}
#[test]
fn every_formulae_file_matches_the_golden_ids() {
    let golden = golden_ids();
    let files = formulae_files();

    assert_eq!(files.len(), golden.len(), "{files:?} against {golden:?}");
    for path in files {
        let (formulae, id) = load_formulae(&path).unwrap();
        let stem = path.file_stem().unwrap().to_str().unwrap();

        assert_eq!(formulae.name, stem, "{}", path.display());
        assert_eq!(golden.get(stem), Some(&id.hex()), "{}", path.display());
        assert_eq!(formulae_path(&formulae_dir(), &formulae.name), path);
    }
}

#[test]
fn every_committed_record_passes_layers_one_and_three_offline() {
    let attestations = repo_root().join("attestations");

    let (records, failures) = gate_failures(&attestations);

    assert_eq!(failures, Vec::<String>::new());
    assert_eq!(
        records > 0,
        attestations.exists(),
        "{records} records under {}",
        attestations.display()
    );
}

#[test]
fn gate_accepts_a_published_record_with_a_valid_declaration() {
    let (attestations, dir) = tree("gate-published");

    passes_alone(&attestations);
    assert_eq!(
        gate(&dir.join("attestation.json")),
        Ok(LayerOutcome::PassWithheld(Partial {
            files: vec!["corrected_ledger.json".to_owned()],
            fields: 6,
            hashes: 1,
        }))
    );
}

#[test]
fn gate_accepts_a_full_record_without_a_declaration() {
    let (attestations, dir) = tree("gate-full");
    fs::remove_file(dir.join(WITHHELD_FILE)).unwrap();
    fs::copy(
        fixtures().join("corrected_ledger.json"),
        dir.join("corrected_ledger.json"),
    )
    .unwrap();

    passes_alone(&attestations);
    assert_eq!(gate(&dir.join("attestation.json")), Ok(LayerOutcome::Pass));
}

#[test]
fn gate_rejects_an_undeclared_missing_file() {
    let (attestations, dir) = tree("gate-undeclared");
    fs::remove_file(dir.join(WITHHELD_FILE)).unwrap();

    let failure = single_failure(&attestations);

    assert!(failure.contains("corrected_ledger.json"), "{failure}");
    assert!(
        failure.contains("layer 3 fail: field system.engine_version: artifact "),
        "{failure}"
    );
}

#[test]
fn gate_rejects_an_invalid_declaration() {
    let (attestations, dir) = tree("gate-invalid");
    write(
        &dir.join(WITHHELD_FILE),
        &declaration(&[(
            "corrected_ledger.json",
            &CORRECTED_LEDGER_SHA256.to_uppercase(),
        )]),
    );

    let failure = single_failure(&attestations);

    assert!(
        failure.contains(
            "sha256 of \"corrected_ledger.json\" is not 64 lowercase hex digits"
        ),
        "{failure}"
    );
}

#[test]
fn gate_rejects_a_declared_file_that_is_present() {
    let (attestations, dir) = tree("gate-present");
    fs::copy(
        fixtures().join("corrected_ledger.json"),
        dir.join("corrected_ledger.json"),
    )
    .unwrap();

    let failure = single_failure(&attestations);

    assert!(
        failure.contains("withheld file \"corrected_ledger.json\" is present"),
        "{failure}"
    );
}

#[test]
fn gate_rejects_a_declared_hash_that_differs_from_the_record() {
    let (attestations, dir) = tree("gate-hash");
    write(
        &dir.join(WITHHELD_FILE),
        &declaration(&[("corrected_ledger.json", &"ab".repeat(32))]),
    );

    let failure = single_failure(&attestations);

    assert!(
        failure.contains(&format!(
            "withheld file \"corrected_ledger.json\" declares sha256 {} against the recorded {CORRECTED_LEDGER_SHA256}",
            "ab".repeat(32)
        )),
        "{failure}"
    );
}

#[test]
fn gate_rejects_a_declared_file_the_record_does_not_name() {
    let (attestations, dir) = tree("gate-unnamed");
    write(
        &dir.join(WITHHELD_FILE),
        &declaration(&[
            ("corrected_ledger.json", CORRECTED_LEDGER_SHA256),
            ("equity.json", &"ab".repeat(32)),
        ]),
    );

    let failure = single_failure(&attestations);

    assert!(
        failure.contains(
            "withheld file \"equity.json\" is not a file_sha256 entry of the record"
        ),
        "{failure}"
    );
}

#[test]
fn gate_rejects_a_tampered_published_file_beside_a_declaration() {
    let (attestations, dir) = tree("gate-tampered");
    fs::write(dir.join("fillrates.json"), "{\"symbols\": 6}\n").unwrap();

    let failure = single_failure(&attestations);

    assert!(
        failure.contains("field artifacts.files re-derives as "),
        "{failure}"
    );
}

#[test]
fn gate_rejects_an_edited_withheld_sourced_value() {
    let (attestations, dir) = tree("gate-edited");
    edit_record(&dir.join("attestation.json"), |record| {
        record["groups"][1]["fields"]["paper_n"] = 99_999.into();
        record["groups"][1]["fields"]["corrected_total_usd"] = 9_205_706_422_i64.into();
    });

    let failure = single_failure(&attestations);

    assert!(
        failure.ends_with(
            "layer 3 fail: record leaves differ from the leaves of its groups"
        ),
        "{failure}"
    );
}

#[test]
fn gate_rejects_a_withheld_file_outside_the_allowed_set() {
    let (attestations, dir) = tree("gate-allowed");
    let fillrates = clarion_formulae::Artifacts::new(&dir)
        .sha256_hex("fillrates.json")
        .unwrap();
    fs::remove_file(dir.join("fillrates.json")).unwrap();
    write(
        &dir.join(WITHHELD_FILE),
        &declaration(&[
            ("corrected_ledger.json", CORRECTED_LEDGER_SHA256),
            ("fillrates.json", &fillrates),
        ]),
    );

    let failure = single_failure(&attestations);

    assert!(
        failure.ends_with(
            "layer 3 withholds \"fillrates.json\"; the records gate allows only [\"corrected_ledger.json\"]"
        ),
        "{failure}"
    );
}

#[test]
fn gate_rejects_a_record_whose_formulae_id_differs() {
    let (attestations, dir) = tree("gate-formulae-id");
    edit_record(&dir.join("attestation.json"), |record| {
        record["formulae"]["id"] = "ab".repeat(32).into();
    });

    let failure = single_failure(&attestations);

    assert!(
        failure.contains(&format!(
            "layer 1 fail: formulae id {FORMULAE_ID_HEX} differs from the record id {}",
            "ab".repeat(32)
        )),
        "{failure}"
    );
}

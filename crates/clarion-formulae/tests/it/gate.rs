use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

use clarion_formulae::{
    LayerOutcome, formulae_path, layer_artifacts, layer_formulae, load_formulae,
    load_record,
};

use crate::support::{formulae_dir, repo_root};

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
    let mut records = Vec::new();
    records_under(&repo_root().join("attestations"), &mut records);
    records.sort();

    for path in records {
        let record = load_record(&path).unwrap();
        let document = formulae_path(&formulae_dir(), &record.formulae.name);
        let text = fs::read_to_string(&document)
            .map_err(|error| format!("{}: {error}", document.display()));
        let (first, formulae) =
            layer_formulae(&record, text.as_deref().map_err(Clone::clone));
        let third = layer_artifacts(&record, formulae.as_ref(), path.parent());

        assert_eq!(first.outcome, LayerOutcome::Pass, "{}", path.display());
        assert_eq!(third.outcome, LayerOutcome::Pass, "{}", path.display());
    }
}

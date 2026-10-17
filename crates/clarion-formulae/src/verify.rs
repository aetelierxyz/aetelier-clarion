use std::{collections::BTreeMap, fmt, fs, io, path::Path, str::FromStr};

use clarion_program_client::merkle_root_hex;
use serde_json::Value;
use solana_program::pubkey::Pubkey;

use crate::{
    formulae::{Field, Formulae, FormulaeId, Source, parse_formulae},
    record::{
        Artifacts, Inputs, Record, Root, WindowJson, evaluate, file_entry, leaves_of,
        root_of, window_matches,
    },
    withheld::{WITHHELD_FILE, Withheld, load_withheld},
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LayerOutcome {
    Pass,
    PassWithheld(Partial),
    Fail(String),
    Skipped,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Partial {
    pub files: Vec<String>,
    pub fields: usize,
    pub hashes: usize,
}

impl fmt::Display for Partial {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "withheld: {} ({} {} and {} {} not re-derived)",
            self.files.join(", "),
            self.fields,
            if self.fields == 1 { "field" } else { "fields" },
            self.hashes,
            if self.hashes == 1 {
                "file hash"
            } else {
                "file hashes"
            },
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Declaration {
    Read,
    Ignore,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LayerResult {
    pub layer: u8,
    pub name: &'static str,
    pub outcome: LayerOutcome,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecordState {
    Built,
    Committed,
    Revealed,
}

impl fmt::Display for RecordState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Built => "Built",
            Self::Committed => "Committed",
            Self::Revealed => "Revealed",
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Verification {
    pub layers: Vec<LayerResult>,
    pub state: RecordState,
}

impl Verification {
    pub fn passed(&self) -> bool {
        self.layers
            .iter()
            .all(|layer| !matches!(layer.outcome, LayerOutcome::Fail(_)))
    }
}

impl fmt::Display for Verification {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        for layer in &self.layers {
            write!(formatter, "layer {} {}: ", layer.layer, layer.name)?;
            match &layer.outcome {
                LayerOutcome::Pass => writeln!(formatter, "pass")?,
                LayerOutcome::PassWithheld(partial) => {
                    writeln!(formatter, "pass, {partial}")?;
                }
                LayerOutcome::Fail(reason) => {
                    writeln!(formatter, "fail: {}", printable(reason))?;
                }
                LayerOutcome::Skipped => writeln!(formatter, "skipped")?,
            }
        }
        write!(formatter, "state: {}", self.state)?;
        for layer in &self.layers {
            if let LayerOutcome::PassWithheld(partial) = &layer.outcome {
                write!(
                    formatter,
                    ", layer {} partial: {} withheld",
                    layer.layer,
                    partial.files.join(", ")
                )?;
            }
        }
        writeln!(formatter)
    }
}

fn printable(text: &str) -> String {
    text.chars()
        .map(|character| {
            if character.is_control() {
                character.escape_default().to_string()
            } else {
                character.to_string()
            }
        })
        .collect()
}

pub fn layer_formulae(
    record: &Record,
    formulae: Result<&str, String>,
) -> (LayerResult, Option<Formulae>) {
    let outcome = formulae
        .and_then(|text| parse_formulae(text).map_err(|error| error.to_string()))
        .and_then(|(formulae, id)| {
            if id.hex() != record.formulae.id {
                return Err(format!(
                    "formulae id {} differs from the record id {}",
                    id.hex(),
                    record.formulae.id
                ));
            }
            if formulae.name != record.formulae.name {
                return Err(format!(
                    "formulae name {} differs from the record name {}",
                    formulae.name, record.formulae.name
                ));
            }
            Ok(formulae)
        });
    match outcome {
        Ok(formulae) => (result(1, "formulae", LayerOutcome::Pass), Some(formulae)),
        Err(reason) => (result(1, "formulae", LayerOutcome::Fail(reason)), None),
    }
}

pub fn layer_chain(record: &Record, window: &WindowJson) -> LayerResult {
    result(
        2,
        "chain",
        match chain_check(record, window) {
            Ok(()) => LayerOutcome::Pass,
            Err(reason) => LayerOutcome::Fail(reason),
        },
    )
}

fn chain_check(record: &Record, window: &WindowJson) -> Result<(), String> {
    let chain = record
        .chain
        .as_ref()
        .ok_or_else(|| "record is not bound".to_owned())?;
    let program_id = Pubkey::from_str(&chain.program_id)
        .map_err(|error| format!("record program id {}: {error}", chain.program_id))?;
    if chain.window_id != window.window_id {
        return Err(format!(
            "record window id {} differs from the window id {}",
            chain.window_id, window.window_id
        ));
    }
    let root = recomputed_root(record)?;
    window_matches(&root, &program_id, window)?;
    if chain.commit_slot != window.commit_slot || chain.reveal_slot != window.reveal_slot
    {
        return Err(format!(
            "record slots {} and {} differ from the window slots {} and {}",
            chain.commit_slot, chain.reveal_slot, window.commit_slot, window.reveal_slot
        ));
    }
    Ok(())
}

fn recomputed_root(record: &Record) -> Result<Root, String> {
    let id = FormulaeId::parse(&record.formulae.id)
        .map_err(|error| format!("record formulae id: {error}"))?;
    let leaves = leaves_of(id, &record.groups).map_err(|error| error.to_string())?;
    let hex: Vec<String> = leaves.iter().map(merkle_root_hex).collect();
    if hex != record.leaves {
        return Err("record leaves differ from the leaves of its groups".to_owned());
    }
    let root = root_of(&leaves);
    if root.hex() != record.root {
        return Err(format!(
            "record root {} differs from the root of its leaves {}",
            record.root,
            root.hex()
        ));
    }
    Ok(root)
}

pub fn layer_artifacts(
    record: &Record,
    formulae: Option<&Formulae>,
    artifacts: Option<&Path>,
    declaration: Declaration,
) -> LayerResult {
    let Some(dir) = artifacts else {
        return result(3, "artifacts", LayerOutcome::Skipped);
    };
    let Some(formulae) = formulae else {
        return result(
            3,
            "artifacts",
            LayerOutcome::Fail("formulae is unusable".to_owned()),
        );
    };
    let withheld = match declaration {
        Declaration::Ignore => Ok(None),
        Declaration::Read => load_withheld(dir).map_err(|error| error.to_string()),
    };
    result(
        3,
        "artifacts",
        match withheld.and_then(|withheld| {
            artifacts_check(record, formulae, dir, withheld.as_ref())
        }) {
            Ok(None) => LayerOutcome::Pass,
            Ok(Some(partial)) => LayerOutcome::PassWithheld(partial),
            Err(reason) => LayerOutcome::Fail(reason),
        },
    )
}

fn artifacts_check(
    record: &Record,
    formulae: &Formulae,
    dir: &Path,
    withheld: Option<&Withheld>,
) -> Result<Option<Partial>, String> {
    let mut artifacts = Artifacts::new(dir);
    let names: Vec<&str> = record
        .groups
        .iter()
        .map(|group| group.group.as_str())
        .collect();
    let declared: Vec<&str> = formulae
        .groups
        .iter()
        .map(|group| group.name.as_str())
        .collect();
    if names != declared {
        return Err(format!(
            "record groups {names:?} differ from the formulae groups {declared:?}"
        ));
    }
    if let Some(withheld) = withheld {
        declaration_check(withheld, &recorded_hashes(record, formulae), dir)?;
    }
    let mut partial = Partial {
        files: withheld.map(Withheld::filenames).unwrap_or_default(),
        fields: 0,
        hashes: 0,
    };
    let mut rederived = 0;
    for (group, preimage) in formulae.groups.iter().zip(&record.groups) {
        let declared_fields: Vec<&String> =
            group.fields.iter().map(|field| &field.name).collect();
        let recorded_fields: Vec<&String> = preimage.fields.keys().collect();
        if let Some(extra) = recorded_fields
            .iter()
            .find(|name| !declared_fields.contains(name))
        {
            return Err(format!(
                "record field {}.{extra} is not in the formulae",
                group.name
            ));
        }
        for field in &group.fields {
            let recorded = preimage.fields.get(&field.name).ok_or_else(|| {
                format!("record field {}.{} is missing", group.name, field.name)
            })?;
            let derived = rederive(
                &group.name,
                field,
                &mut artifacts,
                withheld,
                &mut partial,
                &mut rederived,
            )
            .map_err(|reason| format!("field {}.{}: {reason}", group.name, field.name))?;
            let Some(derived) = derived else {
                continue;
            };
            if derived != *recorded {
                return Err(format!(
                    "field {}.{} re-derives as {derived} against the recorded {recorded}",
                    group.name, field.name
                ));
            }
        }
    }
    if withheld.is_none() {
        return Ok(None);
    }
    if rederived == 0 {
        return Err(format!(
            "{WITHHELD_FILE} leaves no field or file hash to re-derive"
        ));
    }
    recomputed_root(record)?;
    Ok(Some(partial))
}

fn rederive(
    group: &str,
    field: &Field,
    artifacts: &mut Artifacts<'_>,
    withheld: Option<&Withheld>,
    partial: &mut Partial,
    rederived: &mut usize,
) -> Result<Option<Value>, String> {
    let declared = |file: &str| withheld.and_then(|withheld| withheld.sha256_of(file));
    match &field.source {
        Source::Input => Ok(None),
        Source::JsonPointer { file, .. } if declared(file).is_some() => {
            partial.fields += 1;
            Ok(None)
        }
        Source::JsonPointer { .. } => {
            *rederived += 1;
            evaluate(group, field, artifacts, &Inputs::new()).map(Some)
        }
        Source::FileSha256 { files } => {
            let mut entries = Vec::with_capacity(files.len());
            for file in files {
                let digest = match declared(file) {
                    Some(sha256) => {
                        partial.hashes += 1;
                        sha256.to_owned()
                    }
                    None => {
                        *rederived += 1;
                        artifacts.sha256_hex(file)?
                    }
                };
                entries.push(file_entry(file, &digest));
            }
            Ok(Some(Value::Array(entries)))
        }
    }
}

fn recorded_hashes<'r>(
    record: &'r Record,
    formulae: &Formulae,
) -> BTreeMap<&'r str, &'r str> {
    let mut hashes = BTreeMap::new();
    for (group, preimage) in formulae.groups.iter().zip(&record.groups) {
        for field in &group.fields {
            if !matches!(field.source, Source::FileSha256 { .. }) {
                continue;
            }
            let Some(Value::Array(entries)) = preimage.fields.get(&field.name) else {
                continue;
            };
            for entry in entries {
                if let (Some(filename), Some(sha256)) = (
                    entry.get("filename").and_then(Value::as_str),
                    entry.get("sha256").and_then(Value::as_str),
                ) {
                    hashes.insert(filename, sha256);
                }
            }
        }
    }
    hashes
}

fn declaration_check(
    withheld: &Withheld,
    recorded: &BTreeMap<&str, &str>,
    dir: &Path,
) -> Result<(), String> {
    for file in &withheld.files {
        let Some(sha256) = recorded.get(file.filename.as_str()) else {
            return Err(format!(
                "withheld file {:?} is not a file_sha256 entry of the record",
                file.filename
            ));
        };
        if *sha256 != file.sha256 {
            return Err(format!(
                "withheld file {:?} declares sha256 {} against the recorded {sha256}",
                file.filename, file.sha256
            ));
        }
        match fs::symlink_metadata(dir.join(&file.filename)) {
            Ok(_) => {
                return Err(format!(
                    "withheld file {:?} is present in the artifacts directory",
                    file.filename
                ));
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(format!("withheld file {:?}: {error}", file.filename));
            }
        }
    }
    Ok(())
}

pub fn verify(
    record: &Record,
    formulae: Result<&str, String>,
    window: &WindowJson,
    artifacts: Option<&Path>,
    declaration: Declaration,
) -> Verification {
    let (first, parsed) = layer_formulae(record, formulae);
    let second = layer_chain(record, window);
    let third = layer_artifacts(record, parsed.as_ref(), artifacts, declaration);
    let state = match (&record.chain, &second.outcome) {
        (Some(_), LayerOutcome::Pass) => RecordState::Revealed,
        (Some(_), _) => RecordState::Built,
        (None, _) if window.merkle_root == record.root && window.reveal_slot == 0 => {
            RecordState::Committed
        }
        (None, _) => RecordState::Built,
    };
    Verification {
        layers: vec![first, second, third],
        state,
    }
}

fn result(layer: u8, name: &'static str, outcome: LayerOutcome) -> LayerResult {
    LayerResult {
        layer,
        name,
        outcome,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn partial(files: &[&str], fields: usize, hashes: usize) -> Partial {
        Partial {
            files: files.iter().map(|file| (*file).to_owned()).collect(),
            fields,
            hashes,
        }
    }

    #[test]
    fn partial_names_every_withheld_file_and_counts_what_was_not_re_derived() {
        assert_eq!(
            partial(&["corrected_ledger.json"], 6, 1).to_string(),
            "withheld: corrected_ledger.json (6 fields and 1 file hash not re-derived)"
        );
        assert_eq!(
            partial(&["a.json", "b.json"], 1, 2).to_string(),
            "withheld: a.json, b.json (1 field and 2 file hashes not re-derived)"
        );
        assert_eq!(
            partial(&["b.json"], 0, 1).to_string(),
            "withheld: b.json (0 fields and 1 file hash not re-derived)"
        );
    }

    #[test]
    fn a_withheld_layer_passes_but_marks_the_state_line_partial() {
        let verification = Verification {
            layers: vec![
                result(1, "formulae", LayerOutcome::Pass),
                result(2, "chain", LayerOutcome::Pass),
                result(
                    3,
                    "artifacts",
                    LayerOutcome::PassWithheld(partial(&["corrected_ledger.json"], 6, 1)),
                ),
            ],
            state: RecordState::Revealed,
        };

        assert!(verification.passed());
        assert_eq!(
            verification.to_string(),
            "layer 1 formulae: pass\nlayer 2 chain: pass\nlayer 3 artifacts: pass, withheld: corrected_ledger.json (6 fields and 1 file hash not re-derived)\nstate: Revealed, layer 3 partial: corrected_ledger.json withheld\n"
        );
    }

    #[test]
    fn a_failure_reason_cannot_add_lines_to_the_output() {
        let verification = Verification {
            layers: vec![result(
                3,
                "artifacts",
                LayerOutcome::Fail("x\nlayer 3 artifacts: pass\r\u{1b}[2K".to_owned()),
            )],
            state: RecordState::Built,
        };

        assert_eq!(
            verification.to_string(),
            "layer 3 artifacts: fail: x\\nlayer 3 artifacts: pass\\r\\u{1b}[2K\nstate: Built\n"
        );
    }

    #[test]
    fn a_presence_check_that_cannot_stat_the_declared_file_fails() {
        let filename = "a".repeat(300);
        let sha256 = "ab".repeat(32);
        let withheld = Withheld {
            schema: crate::withheld::WITHHELD_SCHEMA.to_owned(),
            files: vec![crate::withheld::WithheldFile {
                filename: filename.clone(),
                sha256: sha256.clone(),
            }],
        };
        let recorded = BTreeMap::from([(filename.as_str(), sha256.as_str())]);

        let reason =
            declaration_check(&withheld, &recorded, &std::env::temp_dir()).unwrap_err();

        assert!(
            reason.starts_with(&format!("withheld file {filename:?}: ")),
            "{reason}"
        );
        assert!(!reason.contains("is present"), "{reason}");
    }
}

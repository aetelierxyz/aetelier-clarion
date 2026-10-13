use std::{fmt, path::Path, str::FromStr};

use clarion_program_client::merkle_root_hex;
use solana_program::pubkey::Pubkey;

use crate::{
    formulae::{Formulae, FormulaeId, Source, parse_formulae},
    record::{
        Artifacts, Inputs, Record, Root, WindowJson, evaluate, leaves_of, root_of,
        window_matches,
    },
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LayerOutcome {
    Pass,
    Fail(String),
    Skipped,
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
                LayerOutcome::Fail(reason) => writeln!(formatter, "fail: {reason}")?,
                LayerOutcome::Skipped => writeln!(formatter, "skipped")?,
            }
        }
        writeln!(formatter, "state: {}", self.state)
    }
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
    result(
        3,
        "artifacts",
        match artifacts_check(record, formulae, dir) {
            Ok(()) => LayerOutcome::Pass,
            Err(reason) => LayerOutcome::Fail(reason),
        },
    )
}

fn artifacts_check(
    record: &Record,
    formulae: &Formulae,
    dir: &Path,
) -> Result<(), String> {
    let mut artifacts = Artifacts::new(dir);
    let inputs = Inputs::new();
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
            if field.source == Source::Input {
                continue;
            }
            let derived = evaluate(&group.name, field, &mut artifacts, &inputs).map_err(
                |reason| format!("field {}.{}: {reason}", group.name, field.name),
            )?;
            if derived != *recorded {
                return Err(format!(
                    "field {}.{} re-derives as {derived} against the recorded {recorded}",
                    group.name, field.name
                ));
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
) -> Verification {
    let (first, parsed) = layer_formulae(record, formulae);
    let second = layer_chain(record, window);
    let third = layer_artifacts(record, parsed.as_ref(), artifacts);
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

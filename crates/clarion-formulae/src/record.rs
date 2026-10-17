use std::{
    collections::BTreeMap,
    fs, io,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use clarion_program_client::{
    ARM_COUNT, Arm, ArmAggregates, RootError, leaf_hash_bytes, merkle_root_hex,
    merkle_root_of_leaves, parse_aggregates, parse_merkle_root, window_address,
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use solana_keypair::Signature;
use solana_program::pubkey::Pubkey;
use thiserror::Error;

use crate::{
    canonical::{CanonicalError, canonical_bytes, pretty, sha256, sorted},
    formulae::{Field, FieldType, Formulae, FormulaeId, LeafMode, Publication, Source},
};

pub const RECORD_SCHEMA: &str = "formulae.attestation.v1";
pub const REPORT_SCHEMA: &str = "formulae.report.v1";
pub const ARM_NAMES: [&str; ARM_COUNT] =
    ["fan_out", "tip_lane", "rpc_default", "fixed_priority_fee"];
const MICRO: f64 = 1_000_000.0;
const I64_LIMIT: f64 = 9_223_372_036_854_775_808.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Root(pub [u8; 32]);

impl Root {
    pub fn hex(&self) -> String {
        merkle_root_hex(&self.0)
    }

    pub fn parse(text: &str) -> Result<Self, RootError> {
        parse_merkle_root(text).map(Self)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct WindowId(pub u64);

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Record {
    pub schema: String,
    pub formulae: FormulaeRef,
    pub groups: Vec<GroupPreimage>,
    pub leaves: Vec<String>,
    pub root: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chain: Option<Chain>,
    pub meta: Meta,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FormulaeRef {
    pub name: String,
    pub id: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GroupPreimage {
    pub group: String,
    pub fields: Map<String, Value>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Chain {
    pub cluster: String,
    pub program_id: String,
    pub window_id: u64,
    pub commit_signature: String,
    pub commit_slot: u64,
    pub reveal_signature: String,
    pub reveal_slot: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Meta {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_repo: Option<String>,
    pub built_at_us: u64,
    pub tool_version: String,
}

impl Meta {
    pub fn now(source_repo: Option<String>) -> Self {
        let built_at_us = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|elapsed| u64::try_from(elapsed.as_micros()).unwrap_or(u64::MAX))
            .unwrap_or(0);
        Self {
            source_repo,
            built_at_us,
            tool_version: env!("CARGO_PKG_VERSION").to_owned(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Report {
    pub schema: String,
    pub problems: Vec<Problem>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Problem {
    pub field: String,
    pub reason: String,
}

impl Report {
    pub fn new(problems: Vec<Problem>) -> Self {
        Self {
            schema: REPORT_SCHEMA.to_owned(),
            problems,
        }
    }

    pub fn pretty(&self) -> Result<String, CanonicalError> {
        pretty(&to_value(self)?)
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WindowJson {
    pub address: String,
    pub discriminator: u8,
    pub window_id: u64,
    pub slot_start: u64,
    pub slot_end: u64,
    pub merkle_root: String,
    pub commit_slot: u64,
    pub reveal_slot: u64,
    pub aggregates: Vec<Value>,
    pub bump: u8,
}

pub struct Binding {
    pub cluster: String,
    pub program_id: Pubkey,
    pub commit_signature: Signature,
    pub reveal_signature: Signature,
}

pub type Inputs = BTreeMap<String, String>;

#[derive(Debug, Error)]
pub enum RecordError {
    #[error("input {text:?} is not group.field=value")]
    Input { text: String },
    #[error("{what} file {path}: {source}")]
    Read {
        what: &'static str,
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("{what} does not parse as JSON: {source}")]
    Json {
        what: &'static str,
        #[source]
        source: serde_json::Error,
    },
    #[error("{what} does not match its schema: {source}")]
    Shape {
        what: &'static str,
        #[source]
        source: serde_json::Error,
    },
    #[error("record schema is {actual:?}, expected {RECORD_SCHEMA:?}")]
    Schema { actual: String },
    #[error(transparent)]
    Canonical(#[from] CanonicalError),
    #[error("record root {0}")]
    Root(#[source] RootError),
    #[error("record is already bound to window {window_id} of {program_id} on {cluster}")]
    AlreadyBound {
        cluster: String,
        program_id: String,
        window_id: u64,
    },
    #[error("cluster {cluster:?} is not lowercase letters, digits and hyphens")]
    Cluster { cluster: String },
    #[error("window does not match the record: {0}")]
    Window(String),
    #[error("record file {path}: {source}")]
    Write {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

pub fn parse_input(text: &str) -> Result<(String, String), RecordError> {
    let invalid = || RecordError::Input {
        text: text.to_owned(),
    };
    let (key, value) = text.split_once('=').ok_or_else(invalid)?;
    let (group, field) = key.split_once('.').ok_or_else(invalid)?;
    if group.is_empty() || field.is_empty() || field.contains('.') || value.is_empty() {
        return Err(invalid());
    }
    Ok((key.to_owned(), value.to_owned()))
}

pub struct Artifacts<'a> {
    dir: &'a Path,
    bytes: BTreeMap<String, Vec<u8>>,
    json: BTreeMap<String, Value>,
}

impl<'a> Artifacts<'a> {
    pub fn new(dir: &'a Path) -> Self {
        Self {
            dir,
            bytes: BTreeMap::new(),
            json: BTreeMap::new(),
        }
    }

    fn bytes(&mut self, file: &str) -> Result<&[u8], String> {
        if !self.bytes.contains_key(file) {
            let path = self.dir.join(file);
            let bytes = fs::read(&path)
                .map_err(|error| format!("artifact {}: {error}", path.display()))?;
            self.bytes.insert(file.to_owned(), bytes);
        }
        Ok(&self.bytes[file])
    }

    pub fn sha256_hex(&mut self, file: &str) -> Result<String, String> {
        Ok(merkle_root_hex(&sha256(self.bytes(file)?)))
    }

    fn json(&mut self, file: &str) -> Result<&Value, String> {
        if !self.json.contains_key(file) {
            let value: Value =
                serde_json::from_slice(self.bytes(file)?).map_err(|error| {
                    format!("artifact {file} does not parse as JSON: {error}")
                })?;
            self.json.insert(file.to_owned(), value);
        }
        Ok(&self.json[file])
    }
}

pub fn evaluate(
    group: &str,
    field: &Field,
    artifacts: &mut Artifacts<'_>,
    inputs: &Inputs,
) -> Result<Value, String> {
    match &field.source {
        Source::Input => {
            let text = inputs
                .get(&format!("{group}.{}", field.name))
                .ok_or_else(|| "input is missing".to_owned())?;
            match field.kind {
                FieldType::U64 => text
                    .parse::<u64>()
                    .map(Value::from)
                    .map_err(|_| format!("input {text:?} is not an unsigned integer")),
                _ => Ok(Value::String(text.clone())),
            }
        }
        Source::JsonPointer { file, pointer } => {
            let document = artifacts.json(file)?;
            let value = document
                .pointer(pointer)
                .ok_or_else(|| format!("pointer {pointer} is absent in {file}"))?;
            convert(field.kind, value)
        }
        Source::FileSha256 { files } => {
            let mut entries = Vec::with_capacity(files.len());
            for file in files {
                entries.push(file_entry(file, &artifacts.sha256_hex(file)?));
            }
            Ok(Value::Array(entries))
        }
    }
}

pub fn file_entry(filename: &str, sha256: &str) -> Value {
    serde_json::json!({"filename": filename, "sha256": sha256})
}

fn convert(kind: FieldType, value: &Value) -> Result<Value, String> {
    match kind {
        FieldType::String => value
            .as_str()
            .map(|text| Value::String(text.to_owned()))
            .ok_or_else(|| "value is not a string".to_owned()),
        FieldType::U64 => value
            .as_u64()
            .map(Value::from)
            .ok_or_else(|| "value is not an unsigned integer".to_owned()),
        FieldType::I64Micro => {
            let number = value
                .as_f64()
                .ok_or_else(|| "value is not a number".to_owned())?;
            micro(number).map(Value::from)
        }
        FieldType::Map => {
            if !value.is_object() {
                return Err("value is not a map".to_owned());
            }
            leaf_safe(value, "")?;
            Ok(sorted(value))
        }
        FieldType::List => {
            if !value.is_array() {
                return Err("value is not a list".to_owned());
            }
            leaf_safe(value, "")?;
            Ok(sorted(value))
        }
    }
}

pub fn micro(number: f64) -> Result<i64, String> {
    if !number.is_finite() {
        return Err("value is not finite".to_owned());
    }
    let scaled = (number * MICRO).round_ties_even();
    if !(-I64_LIMIT..I64_LIMIT).contains(&scaled) {
        return Err(format!("value {number} exceeds the i64 micro range"));
    }
    Ok(scaled as i64)
}

fn leaf_safe(value: &Value, path: &str) -> Result<(), String> {
    match value {
        Value::Null => Err(format!("value holds null at {path:?}")),
        Value::Bool(_) => Err(format!("value holds a boolean at {path:?}")),
        Value::Number(number) if number.is_f64() => {
            Err(format!("value holds a float at {path:?}"))
        }
        Value::Number(_) | Value::String(_) => Ok(()),
        Value::Array(items) => items
            .iter()
            .enumerate()
            .try_for_each(|(index, item)| leaf_safe(item, &format!("{path}/{index}"))),
        Value::Object(map) => map
            .iter()
            .try_for_each(|(key, item)| leaf_safe(item, &format!("{path}/{key}"))),
    }
}

pub fn build(
    formulae: &Formulae,
    id: FormulaeId,
    artifacts_dir: &Path,
    inputs: &Inputs,
    meta: Meta,
) -> Result<Record, Report> {
    let mut problems = Vec::new();
    if formulae.publication != Publication::Preimages {
        problems.push(Problem {
            field: "publication".to_owned(),
            reason: "hashes_only is reserved for series 2".to_owned(),
        });
    }
    if formulae.leaf_mode != LeafMode::PerGroup {
        problems.push(Problem {
            field: "leaf_mode".to_owned(),
            reason: "per_element is reserved for series 2".to_owned(),
        });
    }
    let declared = formulae.declared_inputs();
    for key in inputs.keys().filter(|key| !declared.contains(key)) {
        problems.push(Problem {
            field: key.clone(),
            reason: "is not a declared input".to_owned(),
        });
    }
    let mut artifacts = Artifacts::new(artifacts_dir);
    let mut groups = Vec::with_capacity(formulae.groups.len());
    for group in &formulae.groups {
        let mut fields = Map::new();
        for field in &group.fields {
            match evaluate(&group.name, field, &mut artifacts, inputs) {
                Ok(value) => {
                    fields.insert(field.name.clone(), value);
                }
                Err(reason) => problems.push(Problem {
                    field: format!("{}.{}", group.name, field.name),
                    reason,
                }),
            }
        }
        groups.push(GroupPreimage {
            group: group.name.clone(),
            fields,
        });
    }
    if !problems.is_empty() {
        return Err(Report::new(problems));
    }
    let leaves = leaves_of(id, &groups).map_err(|error| {
        Report::new(vec![Problem {
            field: "groups".to_owned(),
            reason: error.to_string(),
        }])
    })?;
    Ok(Record {
        schema: RECORD_SCHEMA.to_owned(),
        formulae: FormulaeRef {
            name: formulae.name.clone(),
            id: id.hex(),
        },
        groups,
        leaves: leaves.iter().map(merkle_root_hex).collect(),
        root: root_of(&leaves).hex(),
        chain: None,
        meta,
    })
}

pub fn leaves_of(
    id: FormulaeId,
    groups: &[GroupPreimage],
) -> Result<Vec<[u8; 32]>, CanonicalError> {
    let mut leaves = Vec::with_capacity(groups.len() + 1);
    leaves.push(leaf_hash_bytes(&id.0));
    for group in groups {
        leaves.push(leaf_hash_bytes(&canonical_bytes(&to_value(group)?)?));
    }
    Ok(leaves)
}

pub fn root_of(leaves: &[[u8; 32]]) -> Root {
    Root(merkle_root_of_leaves(leaves))
}

pub fn window_matches(
    root: &Root,
    program_id: &Pubkey,
    window: &WindowJson,
) -> Result<(), String> {
    let (expected, _) = window_address(program_id, window.window_id);
    if window.address != expected.to_string() {
        return Err(format!(
            "window address {} is not the address {expected} of window {} under program {program_id}",
            window.address, window.window_id
        ));
    }
    let on_chain = Root::parse(&window.merkle_root)
        .map_err(|error| format!("window merkle root: {error}"))?;
    if on_chain != *root {
        return Err(format!(
            "window merkle root {} differs from the record root {}",
            window.merkle_root,
            root.hex()
        ));
    }
    if window.reveal_slot == 0 {
        return Err(format!("window {} is not revealed", window.window_id));
    }
    let names: Vec<&str> = window
        .aggregates
        .iter()
        .map(|entry| entry["arm"].as_str().unwrap_or_default())
        .collect();
    if names != ARM_NAMES {
        return Err(format!(
            "aggregates {names:?} are not in arm order {ARM_NAMES:?}"
        ));
    }
    let aggregates =
        parse_aggregates(&Value::Array(window.aggregates.clone()).to_string())
            .map_err(|error| format!("window aggregates: {error}"))?;
    for (aggregate, arm) in aggregates.iter().zip(Arm::ALL) {
        if *aggregate != ArmAggregates::empty(arm) {
            return Err(format!(
                "aggregates of arm {} are not zero",
                ARM_NAMES[arm as usize]
            ));
        }
    }
    Ok(())
}

pub fn bind(
    record: Record,
    window: &WindowJson,
    binding: &Binding,
) -> Result<Record, RecordError> {
    if let Some(chain) = &record.chain {
        return Err(RecordError::AlreadyBound {
            cluster: chain.cluster.clone(),
            program_id: chain.program_id.clone(),
            window_id: chain.window_id,
        });
    }
    if !is_cluster(&binding.cluster) {
        return Err(RecordError::Cluster {
            cluster: binding.cluster.clone(),
        });
    }
    let root = Root::parse(&record.root).map_err(RecordError::Root)?;
    window_matches(&root, &binding.program_id, window).map_err(RecordError::Window)?;
    Ok(Record {
        chain: Some(Chain {
            cluster: binding.cluster.clone(),
            program_id: binding.program_id.to_string(),
            window_id: window.window_id,
            commit_signature: binding.commit_signature.to_string(),
            commit_slot: window.commit_slot,
            reveal_signature: binding.reveal_signature.to_string(),
            reveal_slot: window.reveal_slot,
        }),
        ..record
    })
}

fn is_cluster(text: &str) -> bool {
    !text.is_empty()
        && text.chars().all(|character| {
            character.is_ascii_lowercase()
                || character.is_ascii_digit()
                || character == '-'
        })
}

impl Record {
    pub fn pretty(&self) -> Result<String, CanonicalError> {
        pretty(&to_value(self)?)
    }
}

pub fn read_record(text: &str) -> Result<Record, RecordError> {
    let value: Value =
        serde_json::from_str(text).map_err(|source| RecordError::Json {
            what: "record",
            source,
        })?;
    let record: Record =
        serde_json::from_value(value).map_err(|source| RecordError::Shape {
            what: "record",
            source,
        })?;
    if record.schema != RECORD_SCHEMA {
        return Err(RecordError::Schema {
            actual: record.schema,
        });
    }
    Ok(record)
}

pub fn load_record(path: &Path) -> Result<Record, RecordError> {
    read_record(&read("record", path)?)
}

pub fn read_window(text: &str) -> Result<WindowJson, RecordError> {
    let value: Value =
        serde_json::from_str(text).map_err(|source| RecordError::Json {
            what: "window",
            source,
        })?;
    serde_json::from_value(value).map_err(|source| RecordError::Shape {
        what: "window",
        source,
    })
}

pub fn load_window(path: &Path) -> Result<WindowJson, RecordError> {
    read_window(&read("window", path)?)
}

pub fn write_record(path: &Path, record: &Record) -> Result<(), RecordError> {
    let text = record.pretty()?;
    let mut temporary = path.as_os_str().to_owned();
    temporary.push(".tmp");
    let temporary = PathBuf::from(temporary);
    let failed = |source| RecordError::Write {
        path: path.to_path_buf(),
        source,
    };
    fs::write(&temporary, text).map_err(failed)?;
    fs::rename(&temporary, path).map_err(failed)
}

fn read(what: &'static str, path: &Path) -> Result<String, RecordError> {
    fs::read_to_string(path).map_err(|source| RecordError::Read {
        what,
        path: path.to_path_buf(),
        source,
    })
}

fn to_value<T: Serialize>(value: &T) -> Result<Value, CanonicalError> {
    serde_json::to_value(value).map_err(|error| CanonicalError::Render {
        path: String::new(),
        reason: error.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn micro_rounds_half_to_even_on_the_scaled_float() {
        assert_eq!(micro(2.5e-6), Ok(2));
        assert_eq!(micro(3.5e-6), Ok(4));
        assert_eq!(micro(-0.5e-6), Ok(0));
        assert_eq!(micro(1.0000005), Ok(1_000_001));
        assert_eq!(micro(-6205.706421958514), Ok(-6_205_706_422));
        assert_eq!(micro(0.0), Ok(0));
        assert!(micro(-(I64_LIMIT / MICRO)).is_ok());
    }

    #[test]
    fn micro_rejects_values_outside_i64_and_non_finite_values() {
        assert!(
            micro(9.223_372_036_854_776e12)
                .unwrap_err()
                .contains("exceeds")
        );
        assert!(micro(-9.3e12).unwrap_err().contains("exceeds"));
        assert!(micro(-9.223_372_036_854_776e12).is_ok());
        assert_eq!(micro(f64::NAN), Err("value is not finite".to_owned()));
        assert_eq!(micro(f64::INFINITY), Err("value is not finite".to_owned()));
    }

    #[test]
    fn convert_checks_the_declared_type() {
        assert_eq!(
            convert(FieldType::String, &json!("0.1.0")),
            Ok(json!("0.1.0"))
        );
        assert_eq!(convert(FieldType::U64, &json!(14433)), Ok(json!(14433)));
        assert_eq!(
            convert(FieldType::I64Micro, &json!(-6205.706421958514)),
            Ok(json!(-6_205_706_422_i64))
        );
        assert_eq!(convert(FieldType::I64Micro, &json!(3)), Ok(json!(3000000)));
        assert_eq!(
            convert(FieldType::Map, &json!({"b": 1, "a": {"d": "x", "c": [1]}})),
            Ok(json!({"a": {"c": [1], "d": "x"}, "b": 1}))
        );
        assert_eq!(
            convert(FieldType::List, &json!([1, "a"])),
            Ok(json!([1, "a"]))
        );
        assert!(
            convert(FieldType::String, &json!(1))
                .unwrap_err()
                .contains("not a string")
        );
        assert!(
            convert(FieldType::U64, &json!(-1))
                .unwrap_err()
                .contains("not an unsigned")
        );
        assert!(
            convert(FieldType::U64, &json!(1.0))
                .unwrap_err()
                .contains("not an unsigned")
        );
        assert!(
            convert(FieldType::I64Micro, &json!("1"))
                .unwrap_err()
                .contains("not a number")
        );
        assert!(
            convert(FieldType::Map, &json!([]))
                .unwrap_err()
                .contains("not a map")
        );
        assert!(
            convert(FieldType::List, &json!({}))
                .unwrap_err()
                .contains("not a list")
        );
    }

    #[test]
    fn maps_and_lists_reject_floats_booleans_and_null_at_any_depth() {
        assert_eq!(
            convert(FieldType::Map, &json!({"a": [1, {"b": 2.5}]})),
            Err("value holds a float at \"/a/1/b\"".to_owned())
        );
        assert_eq!(
            convert(FieldType::List, &json!([true])),
            Err("value holds a boolean at \"/0\"".to_owned())
        );
        assert_eq!(
            convert(FieldType::Map, &json!({"a": null})),
            Err("value holds null at \"/a\"".to_owned())
        );
    }

    #[test]
    fn parse_input_splits_group_field_and_value_once() {
        assert_eq!(
            parse_input("results.subject_id=replay").unwrap(),
            ("results.subject_id".to_owned(), "replay".to_owned())
        );
        assert_eq!(
            parse_input("system.source_commit=a=b").unwrap(),
            ("system.source_commit".to_owned(), "a=b".to_owned())
        );
        for text in [
            "subject_id=replay",
            "results.subject_id",
            "results.=x",
            ".x=1",
            "a.b.c=1",
            "a.b=",
        ] {
            assert!(
                matches!(parse_input(text), Err(RecordError::Input { .. })),
                "{text}"
            );
        }
    }

    #[test]
    fn leaves_start_with_the_formulae_id_leaf() {
        let id = FormulaeId([7; 32]);
        let groups = [GroupPreimage {
            group: "g".to_owned(),
            fields: Map::new(),
        }];

        let leaves = leaves_of(id, &groups).unwrap();

        assert_eq!(leaves.len(), 2);
        assert_eq!(leaves[0], leaf_hash_bytes(&[7; 32]));
        assert_eq!(
            leaves[1],
            leaf_hash_bytes(b"{\"fields\":{},\"group\":\"g\"}")
        );
        assert_eq!(root_of(&leaves).0, merkle_root_of_leaves(&leaves));
    }

    #[test]
    fn write_record_leaves_no_temporary_file() {
        let dir = std::env::temp_dir().join("clarion-formulae-record-write");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("attestation.json");
        let record = Record {
            schema: RECORD_SCHEMA.to_owned(),
            formulae: FormulaeRef {
                name: "x.v1".to_owned(),
                id: "00".repeat(32),
            },
            groups: Vec::new(),
            leaves: Vec::new(),
            root: "11".repeat(32),
            chain: None,
            meta: Meta::now(None),
        };

        write_record(&path, &record).unwrap();

        assert_eq!(load_record(&path).unwrap(), record);
        assert!(!dir.join("attestation.json.tmp").exists());
        assert!(
            fs::read_to_string(&path)
                .unwrap()
                .starts_with("{\n  \"formulae\": {")
        );
    }

    #[test]
    fn read_record_rejects_another_schema_and_unknown_keys() {
        let text = json!({
            "schema": "formulae.attestation.v2",
            "formulae": {"name": "x.v1", "id": "00".repeat(32)},
            "groups": [], "leaves": [], "root": "11".repeat(32),
            "meta": {"built_at_us": 1, "tool_version": "0.1.0"}
        })
        .to_string();
        let unknown = text.replacen("\"groups\"", "\"extra\":1,\"groups\"", 1);

        assert!(matches!(
            read_record(&text),
            Err(RecordError::Schema { .. })
        ));
        assert!(matches!(
            read_record(&unknown),
            Err(RecordError::Shape { .. })
        ));
        assert!(matches!(read_record("{"), Err(RecordError::Json { .. })));
    }
}

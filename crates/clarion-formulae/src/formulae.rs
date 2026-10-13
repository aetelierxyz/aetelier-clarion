use std::{
    fmt, fs, io,
    path::{Path, PathBuf},
};

use clarion_program_client::{
    EMPTY_PREFIX, LEAF_PREFIX, NODE_PREFIX, RootError, merkle_root_hex, parse_merkle_root,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

use crate::canonical::{CanonicalError, canonical_sha256};

pub const SCHEMA: &str = "formulae.v1";
pub const CANONICALIZATION: &str = "json_sorted_keys_no_ws.sha256";
pub const HASH: &str = "sha256";
pub const REVEAL: &str = "zero_arms";
pub const DISPLAY_HEX_LEN: usize = 12;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct FormulaeId(pub [u8; 32]);

impl FormulaeId {
    pub fn hex(&self) -> String {
        merkle_root_hex(&self.0)
    }

    pub fn parse(text: &str) -> Result<Self, RootError> {
        parse_merkle_root(text).map(Self)
    }
}

impl fmt::Display for FormulaeId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "formulae-{}", &self.hex()[..DISPLAY_HEX_LEN])
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Formulae {
    pub schema: String,
    pub name: String,
    pub canonicalization: String,
    pub hash: String,
    pub prefixes: Prefixes,
    #[serde(default)]
    pub publication: Publication,
    #[serde(default)]
    pub leaf_mode: LeafMode,
    pub reveal: String,
    pub groups: Vec<Group>,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Prefixes {
    pub leaf: u8,
    pub node: u8,
    pub empty: u8,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Publication {
    #[default]
    Preimages,
    HashesOnly,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LeafMode {
    #[default]
    PerGroup,
    PerElement,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Group {
    pub name: String,
    pub fields: Vec<Field>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Field {
    pub name: String,
    #[serde(rename = "type")]
    pub kind: FieldType,
    pub source: Source,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FieldType {
    String,
    U64,
    I64Micro,
    Map,
    List,
}

impl FieldType {
    pub fn name(self) -> &'static str {
        match self {
            Self::String => "string",
            Self::U64 => "u64",
            Self::I64Micro => "i64_micro",
            Self::Map => "map",
            Self::List => "list",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum Source {
    JsonPointer { file: String, pointer: String },
    FileSha256 { files: Vec<String> },
    Input,
}

impl Source {
    pub fn name(&self) -> &'static str {
        match self {
            Self::JsonPointer { .. } => "json_pointer",
            Self::FileSha256 { .. } => "file_sha256",
            Self::Input => "input",
        }
    }
}

#[derive(Debug, Error)]
pub enum FormulaeError {
    #[error("formulae file {path}: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("formulae does not parse as JSON: {0}")]
    Json(#[source] serde_json::Error),
    #[error("formulae does not match {SCHEMA}: {0}")]
    Shape(#[source] serde_json::Error),
    #[error(transparent)]
    Canonical(#[from] CanonicalError),
    #[error("formulae {key} is {actual:?}, expected {expected:?}")]
    Constant {
        key: &'static str,
        expected: &'static str,
        actual: String,
    },
    #[error("formulae prefixes are {leaf}, {node}, {empty}, expected 0, 1, 2")]
    Prefixes { leaf: u8, node: u8, empty: u8 },
    #[error(
        "formulae name {name:?} is not <name>.v<N> over lowercase letters, digits and hyphens"
    )]
    Name { name: String },
    #[error("formulae holds no groups")]
    NoGroups,
    #[error("group name {name:?} is not lowercase letters, digits and underscores")]
    GroupName { name: String },
    #[error("group {name} appears more than once")]
    DuplicateGroup { name: String },
    #[error("group {group} holds no fields")]
    NoFields { group: String },
    #[error(
        "field name {group}.{field:?} is not lowercase letters, digits and underscores"
    )]
    FieldName { group: String, field: String },
    #[error("field {group}.{field} appears more than once")]
    DuplicateField { group: String, field: String },
    #[error("field {group}.{field} of type {kind} cannot take source {origin}")]
    Incompatible {
        group: String,
        field: String,
        kind: &'static str,
        origin: &'static str,
    },
    #[error(
        "field {group}.{field} names file {file:?}; artifact names are plain file names"
    )]
    FileName {
        group: String,
        field: String,
        file: String,
    },
    #[error(
        "field {group}.{field} pointer {pointer:?} is neither empty nor starting with a slash"
    )]
    Pointer {
        group: String,
        field: String,
        pointer: String,
    },
}

impl Formulae {
    pub fn validate(&self) -> Result<(), FormulaeError> {
        constant("schema", SCHEMA, &self.schema)?;
        constant("canonicalization", CANONICALIZATION, &self.canonicalization)?;
        constant("hash", HASH, &self.hash)?;
        constant("reveal", REVEAL, &self.reveal)?;
        let Prefixes { leaf, node, empty } = self.prefixes;
        if (leaf, node, empty) != (LEAF_PREFIX, NODE_PREFIX, EMPTY_PREFIX) {
            return Err(FormulaeError::Prefixes { leaf, node, empty });
        }
        if !is_versioned_name(&self.name) {
            return Err(FormulaeError::Name {
                name: self.name.clone(),
            });
        }
        if self.groups.is_empty() {
            return Err(FormulaeError::NoGroups);
        }
        for (index, group) in self.groups.iter().enumerate() {
            group.validate()?;
            if self.groups[..index]
                .iter()
                .any(|seen| seen.name == group.name)
            {
                return Err(FormulaeError::DuplicateGroup {
                    name: group.name.clone(),
                });
            }
        }
        Ok(())
    }

    pub fn declared_inputs(&self) -> Vec<String> {
        self.groups
            .iter()
            .flat_map(|group| {
                group
                    .fields
                    .iter()
                    .filter(|field| field.source == Source::Input)
                    .map(move |field| format!("{}.{}", group.name, field.name))
            })
            .collect()
    }
}

impl Group {
    fn validate(&self) -> Result<(), FormulaeError> {
        if !is_identifier(&self.name) {
            return Err(FormulaeError::GroupName {
                name: self.name.clone(),
            });
        }
        if self.fields.is_empty() {
            return Err(FormulaeError::NoFields {
                group: self.name.clone(),
            });
        }
        for (index, field) in self.fields.iter().enumerate() {
            field.validate(&self.name)?;
            if self.fields[..index]
                .iter()
                .any(|seen| seen.name == field.name)
            {
                return Err(FormulaeError::DuplicateField {
                    group: self.name.clone(),
                    field: field.name.clone(),
                });
            }
        }
        Ok(())
    }
}

impl Field {
    fn validate(&self, group: &str) -> Result<(), FormulaeError> {
        if !is_identifier(&self.name) {
            return Err(FormulaeError::FieldName {
                group: group.to_owned(),
                field: self.name.clone(),
            });
        }
        let compatible = match (&self.source, self.kind) {
            (Source::Input, FieldType::String | FieldType::U64) => true,
            (Source::Input, _) => false,
            (Source::FileSha256 { files }, FieldType::List) => !files.is_empty(),
            (Source::FileSha256 { .. }, _) => false,
            (Source::JsonPointer { .. }, _) => true,
        };
        if !compatible {
            return Err(FormulaeError::Incompatible {
                group: group.to_owned(),
                field: self.name.clone(),
                kind: self.kind.name(),
                origin: self.source.name(),
            });
        }
        let files: Vec<&String> = match &self.source {
            Source::JsonPointer { file, .. } => vec![file],
            Source::FileSha256 { files } => files.iter().collect(),
            Source::Input => Vec::new(),
        };
        if let Some(file) = files.into_iter().find(|file| !is_file_name(file)) {
            return Err(FormulaeError::FileName {
                group: group.to_owned(),
                field: self.name.clone(),
                file: file.clone(),
            });
        }
        if let Source::JsonPointer { pointer, .. } = &self.source
            && !(pointer.is_empty() || pointer.starts_with('/'))
        {
            return Err(FormulaeError::Pointer {
                group: group.to_owned(),
                field: self.name.clone(),
                pointer: pointer.clone(),
            });
        }
        Ok(())
    }
}

fn constant(
    key: &'static str,
    expected: &'static str,
    actual: &str,
) -> Result<(), FormulaeError> {
    if actual == expected {
        return Ok(());
    }
    Err(FormulaeError::Constant {
        key,
        expected,
        actual: actual.to_owned(),
    })
}

fn is_versioned_name(name: &str) -> bool {
    let Some((base, version)) = name.rsplit_once(".v") else {
        return false;
    };
    !base.is_empty()
        && base.chars().all(|character| {
            character.is_ascii_lowercase()
                || character.is_ascii_digit()
                || character == '-'
        })
        && !version.is_empty()
        && version.chars().all(|character| character.is_ascii_digit())
}

fn is_identifier(text: &str) -> bool {
    let mut characters = text.chars();
    characters
        .next()
        .is_some_and(|first| first.is_ascii_lowercase())
        && characters.all(|character| {
            character.is_ascii_lowercase()
                || character.is_ascii_digit()
                || character == '_'
        })
}

fn is_file_name(file: &str) -> bool {
    !file.is_empty() && file != "." && file != ".." && !file.contains(['/', '\\'])
}

pub fn parse_formulae(text: &str) -> Result<(Formulae, FormulaeId), FormulaeError> {
    let value: Value = serde_json::from_str(text).map_err(FormulaeError::Json)?;
    let id = FormulaeId(canonical_sha256(&value)?);
    let formulae: Formulae =
        serde_json::from_value(value).map_err(FormulaeError::Shape)?;
    formulae.validate()?;
    Ok((formulae, id))
}

pub fn load_formulae(path: &Path) -> Result<(Formulae, FormulaeId), FormulaeError> {
    let text = fs::read_to_string(path).map_err(|source| FormulaeError::Read {
        path: path.to_path_buf(),
        source,
    })?;
    parse_formulae(&text)
}

pub fn formulae_path(dir: &Path, name: &str) -> PathBuf {
    dir.join(format!("{name}.json"))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    const DOCUMENT: &str = include_str!("../../../formulae/trace-completed.v1.json");
    const DOCUMENT_ID_HEX: &str =
        "d283858a6801c56d54fffa6ab4afdced38048313cf9c05e6cb8004ee503f2564";

    fn document() -> Value {
        serde_json::from_str(DOCUMENT).unwrap()
    }

    fn parse(value: &Value) -> Result<(Formulae, FormulaeId), FormulaeError> {
        parse_formulae(&value.to_string())
    }

    fn edited(
        edit: impl FnOnce(&mut Value),
    ) -> Result<(Formulae, FormulaeId), FormulaeError> {
        let mut value = document();
        edit(&mut value);
        parse(&value)
    }

    #[test]
    fn the_shipped_document_has_the_reference_id() {
        let (formulae, id) = parse_formulae(DOCUMENT).unwrap();

        assert_eq!(id.hex(), DOCUMENT_ID_HEX);
        assert_eq!(id.to_string(), "formulae-d283858a6801");
        assert_eq!(formulae.name, "trace-completed.v1");
        assert_eq!(formulae.publication, Publication::Preimages);
        assert_eq!(formulae.leaf_mode, LeafMode::PerGroup);
        assert_eq!(
            formulae.declared_inputs(),
            ["system.source_commit", "results.subject_id"]
        );
    }

    #[test]
    fn the_id_ignores_whitespace_and_key_order_but_not_values() {
        let compact = document().to_string();
        let reordered = {
            let mut value = document();
            let object = value.as_object_mut().unwrap();
            let groups = object.remove("groups").unwrap();
            object.insert("groups".to_owned(), groups);
            serde_json::to_string_pretty(&value).unwrap()
        };

        assert_eq!(parse_formulae(&compact).unwrap().1.hex(), DOCUMENT_ID_HEX);
        assert_eq!(parse_formulae(&reordered).unwrap().1.hex(), DOCUMENT_ID_HEX);
        let (_, renamed) =
            edited(|value| value["name"] = json!("trace-completed.v2")).unwrap();
        assert_ne!(renamed.hex(), DOCUMENT_ID_HEX);
    }

    #[test]
    fn id_parses_from_its_hex() {
        let id = FormulaeId::parse(DOCUMENT_ID_HEX).unwrap();

        assert_eq!(id.hex(), DOCUMENT_ID_HEX);
        assert!(FormulaeId::parse("formulae-d283858a6801").is_err());
    }

    #[test]
    fn defaults_apply_when_the_optional_keys_are_absent() {
        let (formulae, id) = edited(|value| {
            value.as_object_mut().unwrap().remove("publication");
            value.as_object_mut().unwrap().remove("leaf_mode");
        })
        .unwrap();

        assert_eq!(formulae.publication, Publication::Preimages);
        assert_eq!(formulae.leaf_mode, LeafMode::PerGroup);
        assert_ne!(id.hex(), DOCUMENT_ID_HEX);
    }

    #[test]
    fn reserved_values_parse() {
        let (formulae, _) = edited(|value| {
            value["publication"] = json!("hashes_only");
            value["leaf_mode"] = json!("per_element");
        })
        .unwrap();

        assert_eq!(formulae.publication, Publication::HashesOnly);
        assert_eq!(formulae.leaf_mode, LeafMode::PerElement);
    }

    #[test]
    fn rejects_text_that_is_not_json() {
        assert!(matches!(parse_formulae("{"), Err(FormulaeError::Json(_))));
    }

    #[test]
    fn rejects_a_float_anywhere_in_the_document() {
        let error = edited(|value| value["prefixes"]["leaf"] = json!(0.0)).unwrap_err();

        assert!(matches!(error, FormulaeError::Canonical(_)), "{error}");
    }

    #[test]
    fn rejects_an_unknown_key_and_a_missing_key() {
        let unknown = edited(|value| value["version"] = json!(1)).unwrap_err();
        let missing = edited(|value| {
            value.as_object_mut().unwrap().remove("hash");
        })
        .unwrap_err();

        assert!(matches!(unknown, FormulaeError::Shape(_)), "{unknown}");
        assert!(unknown.to_string().contains("version"), "{unknown}");
        assert!(matches!(missing, FormulaeError::Shape(_)), "{missing}");
        assert!(missing.to_string().contains("hash"), "{missing}");
    }

    #[test]
    fn rejects_every_constant_key_with_another_value() {
        for (key, other) in [
            ("schema", "formulae.v2"),
            ("canonicalization", "cbor"),
            ("hash", "sha512"),
            ("reveal", "arms"),
        ] {
            let error = edited(|value| value[key] = json!(other)).unwrap_err();

            assert!(
                matches!(&error, FormulaeError::Constant { key: seen, actual, .. }
                    if *seen == key && actual == other),
                "{key}: {error}"
            );
        }
    }

    #[test]
    fn rejects_other_prefixes() {
        let error = edited(|value| value["prefixes"]["node"] = json!(2)).unwrap_err();

        assert!(
            matches!(
                error,
                FormulaeError::Prefixes {
                    leaf: 0,
                    node: 2,
                    empty: 2
                }
            ),
            "{error}"
        );
    }

    #[test]
    fn rejects_a_name_without_a_version_suffix_or_with_other_characters() {
        for name in [
            "trace-completed",
            "Trace.v1",
            "trace_completed.v1",
            ".v1",
            "x.v",
            "x.va",
        ] {
            let error = edited(|value| value["name"] = json!(name)).unwrap_err();

            assert!(
                matches!(error, FormulaeError::Name { .. }),
                "{name}: {error}"
            );
        }
        assert!(edited(|value| value["name"] = json!("a-1.v12")).is_ok());
    }

    #[test]
    fn rejects_no_groups_a_bad_group_name_and_a_repeated_group() {
        let none = edited(|value| value["groups"] = json!([])).unwrap_err();
        let bad =
            edited(|value| value["groups"][0]["name"] = json!("System")).unwrap_err();
        let repeated =
            edited(|value| value["groups"][1]["name"] = json!("system")).unwrap_err();

        assert!(matches!(none, FormulaeError::NoGroups), "{none}");
        assert!(matches!(bad, FormulaeError::GroupName { .. }), "{bad}");
        assert!(
            matches!(&repeated, FormulaeError::DuplicateGroup { name } if name == "system"),
            "{repeated}"
        );
    }

    #[test]
    fn rejects_no_fields_a_bad_field_name_and_a_repeated_field() {
        let none = edited(|value| value["groups"][0]["fields"] = json!([])).unwrap_err();
        let bad = edited(|value| {
            value["groups"][0]["fields"][0]["name"] = json!("engine-version")
        })
        .unwrap_err();
        let repeated = edited(|value| {
            value["groups"][0]["fields"][1]["name"] = json!("engine_version")
        })
        .unwrap_err();

        assert!(matches!(none, FormulaeError::NoFields { .. }), "{none}");
        assert!(matches!(bad, FormulaeError::FieldName { .. }), "{bad}");
        assert!(
            matches!(&repeated, FormulaeError::DuplicateField { group, field }
                if group == "system" && field == "engine_version"),
            "{repeated}"
        );
    }

    #[test]
    fn rejects_incompatible_source_and_type_pairs() {
        type Edit = fn(&mut Value);
        let cases: [(Edit, &str); 3] = [
            (
                |value| value["groups"][0]["fields"][2]["type"] = json!("map"),
                "map",
            ),
            (
                |value| value["groups"][2]["fields"][0]["type"] = json!("string"),
                "string",
            ),
            (
                |value| {
                    value["groups"][2]["fields"][0]["source"]["file_sha256"]["files"] =
                        json!([])
                },
                "list",
            ),
        ];
        for (edit, expected) in cases {
            let error = edited(edit).unwrap_err();

            assert!(
                matches!(&error, FormulaeError::Incompatible { kind, .. } if *kind == expected),
                "{expected}: {error}"
            );
        }
    }

    #[test]
    fn rejects_a_file_path_and_a_pointer_without_a_slash() {
        let path = edited(|value| {
            value["groups"][0]["fields"][0]["source"]["json_pointer"]["file"] =
                json!("../corrected_ledger.json");
        })
        .unwrap_err();
        let pointer = edited(|value| {
            value["groups"][0]["fields"][0]["source"]["json_pointer"]["pointer"] =
                json!("engine_version");
        })
        .unwrap_err();

        assert!(matches!(path, FormulaeError::FileName { .. }), "{path}");
        assert!(
            matches!(pointer, FormulaeError::Pointer { .. }),
            "{pointer}"
        );
        assert!(
            edited(|value| {
                value["groups"][0]["fields"][0]["source"]["json_pointer"]["pointer"] =
                    json!("");
            })
            .is_ok()
        );
    }

    #[test]
    fn rejects_an_unknown_source_and_a_positional_source() {
        let unknown = edited(|value| {
            value["groups"][0]["fields"][0]["source"] = json!({"env": {"name": "X"}});
        })
        .unwrap_err();
        let positional = edited(|value| {
            value["groups"][0]["fields"][0]["source"] =
                json!(["corrected_ledger.json", "/x"]);
        })
        .unwrap_err();

        assert!(matches!(unknown, FormulaeError::Shape(_)), "{unknown}");
        assert!(
            matches!(positional, FormulaeError::Shape(_)),
            "{positional}"
        );
    }

    #[test]
    fn formulae_path_joins_the_name_with_the_json_extension() {
        assert_eq!(
            formulae_path(Path::new("formulae"), "trace-completed.v1"),
            PathBuf::from("formulae/trace-completed.v1.json")
        );
    }

    #[test]
    fn load_reports_the_missing_file_path() {
        let error = load_formulae(Path::new("/nonexistent/x.json")).unwrap_err();

        assert!(matches!(error, FormulaeError::Read { .. }), "{error}");
        assert!(
            error
                .to_string()
                .starts_with("formulae file /nonexistent/x.json: ")
        );
    }
}

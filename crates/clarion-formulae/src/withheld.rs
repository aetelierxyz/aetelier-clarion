use std::{
    collections::BTreeSet,
    fs, io,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

use crate::formulae::is_file_name;

pub const WITHHELD_SCHEMA: &str = "formulae.withheld.v1";
pub const WITHHELD_FILE: &str = "withheld.json";
const SHA256_HEX_LEN: usize = 64;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Withheld {
    pub schema: String,
    pub files: Vec<WithheldFile>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WithheldFile {
    pub filename: String,
    pub sha256: String,
}

#[derive(Debug, Error)]
pub enum WithheldError {
    #[error("withheld file {path}: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("{WITHHELD_FILE} does not parse as JSON: {0}")]
    Json(#[source] serde_json::Error),
    #[error("{WITHHELD_FILE} does not match {WITHHELD_SCHEMA}: {0}")]
    Shape(#[source] serde_json::Error),
    #[error("{WITHHELD_FILE} schema is {actual:?}, expected {WITHHELD_SCHEMA:?}")]
    Schema { actual: String },
    #[error("{WITHHELD_FILE} declares no files")]
    NoFiles,
    #[error(
        "{WITHHELD_FILE} names file {filename:?}; withheld names are plain printable ASCII file names"
    )]
    FileName { filename: String },
    #[error("{WITHHELD_FILE} declares {filename:?} more than once")]
    Duplicate { filename: String },
    #[error("{WITHHELD_FILE} sha256 of {filename:?} is not 64 lowercase hex digits")]
    Digest { filename: String },
}

impl Withheld {
    pub fn filenames(&self) -> Vec<String> {
        self.files
            .iter()
            .map(|file| file.filename.clone())
            .collect()
    }

    pub fn sha256_of(&self, filename: &str) -> Option<&str> {
        self.files
            .iter()
            .find(|file| file.filename == filename)
            .map(|file| file.sha256.as_str())
    }

    fn validate(&self) -> Result<(), WithheldError> {
        if self.schema != WITHHELD_SCHEMA {
            return Err(WithheldError::Schema {
                actual: self.schema.clone(),
            });
        }
        if self.files.is_empty() {
            return Err(WithheldError::NoFiles);
        }
        let mut seen = BTreeSet::new();
        for file in &self.files {
            if !is_withheld_name(&file.filename) {
                return Err(WithheldError::FileName {
                    filename: file.filename.clone(),
                });
            }
            if !seen.insert(file.filename.as_str()) {
                return Err(WithheldError::Duplicate {
                    filename: file.filename.clone(),
                });
            }
            if !is_sha256_hex(&file.sha256) {
                return Err(WithheldError::Digest {
                    filename: file.filename.clone(),
                });
            }
        }
        Ok(())
    }
}

fn is_withheld_name(filename: &str) -> bool {
    is_file_name(filename)
        && filename
            .chars()
            .all(|character| character.is_ascii_graphic())
}

fn is_sha256_hex(text: &str) -> bool {
    text.len() == SHA256_HEX_LEN
        && text.chars().all(|character| {
            character.is_ascii_digit() || ('a'..='f').contains(&character)
        })
}

pub fn read_withheld(text: &str) -> Result<Withheld, WithheldError> {
    serde_json::from_str::<Value>(text).map_err(WithheldError::Json)?;
    let withheld: Withheld = serde_json::from_str(text).map_err(WithheldError::Shape)?;
    withheld.validate()?;
    Ok(withheld)
}

pub fn load_withheld(dir: &Path) -> Result<Option<Withheld>, WithheldError> {
    let path = dir.join(WITHHELD_FILE);
    match fs::read_to_string(&path) {
        Ok(text) => read_withheld(&text).map(Some),
        Err(source)
            if matches!(
                source.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
            ) =>
        {
            Ok(None)
        }
        Err(source) => Err(WithheldError::Read { path, source }),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    const DIGEST: &str =
        "1fd27639deedd39171ddcd0326f806a6faf2292007d174e898d35b46416c6c4c";

    fn declaration(files: Value) -> String {
        json!({"schema": WITHHELD_SCHEMA, "files": files}).to_string()
    }

    fn one(filename: &str, sha256: &str) -> String {
        declaration(json!([{"filename": filename, "sha256": sha256}]))
    }

    #[test]
    fn reads_a_declaration_of_one_file() {
        let withheld = read_withheld(&one("corrected_ledger.json", DIGEST)).unwrap();

        assert_eq!(withheld.filenames(), ["corrected_ledger.json"]);
        assert_eq!(withheld.sha256_of("corrected_ledger.json"), Some(DIGEST));
        assert_eq!(withheld.sha256_of("fillrates.json"), None);
    }

    #[test]
    fn rejects_text_that_is_not_json() {
        assert!(matches!(read_withheld("{"), Err(WithheldError::Json(_))));
    }

    #[test]
    fn rejects_unknown_and_missing_keys() {
        let top =
            json!({"schema": WITHHELD_SCHEMA, "files": [], "reason": "x"}).to_string();
        let entry =
            declaration(json!([{"filename": "a.json", "sha256": DIGEST, "size": 1}]));
        let missing = declaration(json!([{"filename": "a.json"}]));
        let no_files = json!({"schema": WITHHELD_SCHEMA}).to_string();

        for text in [top, entry, missing, no_files] {
            assert!(
                matches!(read_withheld(&text), Err(WithheldError::Shape(_))),
                "{text}"
            );
        }
    }

    #[test]
    fn rejects_another_schema() {
        let text = json!({"schema": "formulae.withheld.v2", "files": []}).to_string();

        assert!(matches!(
            read_withheld(&text),
            Err(WithheldError::Schema { actual }) if actual == "formulae.withheld.v2"
        ));
    }

    #[test]
    fn rejects_an_empty_file_list() {
        assert!(matches!(
            read_withheld(&declaration(json!([]))),
            Err(WithheldError::NoFiles)
        ));
    }

    #[test]
    fn rejects_names_that_are_not_plain_file_names() {
        for filename in [
            "",
            ".",
            "..",
            "dir/a.json",
            "../a.json",
            "a\\b.json",
            "a\njson",
            "a\u{1b}[2Kb.json",
            "a b.json",
            "\u{e9}.json",
        ] {
            assert!(
                matches!(
                    read_withheld(&one(filename, DIGEST)),
                    Err(WithheldError::FileName { .. })
                ),
                "{filename:?}"
            );
        }
    }

    #[test]
    fn rejects_a_file_declared_twice() {
        let text = declaration(json!([
            {"filename": "a.json", "sha256": DIGEST},
            {"filename": "a.json", "sha256": DIGEST}
        ]));

        assert!(matches!(
            read_withheld(&text),
            Err(WithheldError::Duplicate { filename }) if filename == "a.json"
        ));
    }

    #[test]
    fn rejects_duplicate_keys_at_the_top_and_in_an_entry() {
        let top = format!(
            "{{\"schema\":\"{WITHHELD_SCHEMA}\",\"files\":[{{\"filename\":\"fillrates.json\",\"sha256\":\"{DIGEST}\"}}],\"files\":[{{\"filename\":\"corrected_ledger.json\",\"sha256\":\"{DIGEST}\"}}]}}"
        );
        let entry = format!(
            "{{\"schema\":\"{WITHHELD_SCHEMA}\",\"files\":[{{\"filename\":\"fillrates.json\",\"filename\":\"corrected_ledger.json\",\"sha256\":\"{DIGEST}\"}}]}}"
        );

        for (text, key) in [(top, "files"), (entry, "filename")] {
            let error = read_withheld(&text).unwrap_err();
            assert!(matches!(error, WithheldError::Shape(_)), "{error}");
            assert!(
                error
                    .to_string()
                    .contains(&format!("duplicate field `{key}`")),
                "{error}"
            );
        }
    }

    #[test]
    fn rejects_a_digest_that_is_not_64_lowercase_hex() {
        let upper = DIGEST.to_uppercase();
        let short = &DIGEST[..63];
        let long = format!("{DIGEST}0");
        let other = DIGEST.replacen('1', "g", 1);

        for digest in [upper.as_str(), short, long.as_str(), other.as_str(), ""] {
            assert!(
                matches!(
                    read_withheld(&one("a.json", digest)),
                    Err(WithheldError::Digest { .. })
                ),
                "{digest}"
            );
        }
    }

    #[test]
    fn a_missing_declaration_is_none_and_an_unreadable_one_is_an_error() {
        let dir = std::env::temp_dir().join("clarion-formulae-withheld-load");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        assert!(load_withheld(&dir).unwrap().is_none());

        fs::write(dir.join("plain"), "").unwrap();
        assert!(load_withheld(&dir.join("plain")).unwrap().is_none());

        fs::create_dir(dir.join(WITHHELD_FILE)).unwrap();
        let error = load_withheld(&dir).unwrap_err();
        assert!(matches!(error, WithheldError::Read { .. }));
        assert!(
            error.to_string().starts_with(&format!(
                "withheld file {}: ",
                dir.join(WITHHELD_FILE).display()
            )),
            "{error}"
        );

        fs::remove_dir(dir.join(WITHHELD_FILE)).unwrap();
        fs::write(dir.join(WITHHELD_FILE), one("a.json", DIGEST)).unwrap();
        assert_eq!(
            load_withheld(&dir).unwrap().unwrap().filenames(),
            ["a.json"]
        );
    }
}

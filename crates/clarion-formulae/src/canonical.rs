use serde_json::{Map, Value};
use solana_program::hash::hashv;
use thiserror::Error;

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum CanonicalError {
    #[error("value at {path} is a float; canonical JSON holds integers only")]
    Float { path: String },
    #[error("value at {path} does not render as JSON: {reason}")]
    Render { path: String, reason: String },
}

pub fn sha256(bytes: &[u8]) -> [u8; 32] {
    hashv(&[bytes]).to_bytes()
}

pub fn canonical_bytes(value: &Value) -> Result<Vec<u8>, CanonicalError> {
    let mut out = Vec::new();
    write(value, "", &mut out)?;
    Ok(out)
}

pub fn canonical_sha256(value: &Value) -> Result<[u8; 32], CanonicalError> {
    Ok(sha256(&canonical_bytes(value)?))
}

pub fn sorted(value: &Value) -> Value {
    match value {
        Value::Array(items) => Value::Array(items.iter().map(sorted).collect()),
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            let mut rebuilt = Map::new();
            for key in keys {
                rebuilt.insert(key.clone(), sorted(&map[key]));
            }
            Value::Object(rebuilt)
        }
        other => other.clone(),
    }
}

pub fn pretty(value: &Value) -> Result<String, CanonicalError> {
    let mut text = serde_json::to_string_pretty(&sorted(value)).map_err(|error| {
        CanonicalError::Render {
            path: String::new(),
            reason: error.to_string(),
        }
    })?;
    text.push('\n');
    Ok(text)
}

fn write(value: &Value, path: &str, out: &mut Vec<u8>) -> Result<(), CanonicalError> {
    match value {
        Value::Null => out.extend_from_slice(b"null"),
        Value::Bool(flag) => {
            out.extend_from_slice(if *flag { b"true" } else { b"false" })
        }
        Value::Number(number) if number.is_f64() => {
            return Err(CanonicalError::Float {
                path: path.to_owned(),
            });
        }
        Value::Number(number) => out.extend_from_slice(number.to_string().as_bytes()),
        Value::String(text) => write_string(text, path, out)?,
        Value::Array(items) => {
            out.push(b'[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push(b',');
                }
                write(item, &format!("{path}/{index}"), out)?;
            }
            out.push(b']');
        }
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            out.push(b'{');
            for (index, key) in keys.iter().enumerate() {
                if index > 0 {
                    out.push(b',');
                }
                write_string(key, path, out)?;
                out.push(b':');
                write(&map[*key], &format!("{path}/{key}"), out)?;
            }
            out.push(b'}');
        }
    }
    Ok(())
}

fn write_string(text: &str, path: &str, out: &mut Vec<u8>) -> Result<(), CanonicalError> {
    let rendered = serde_json::to_vec(text).map_err(|error| CanonicalError::Render {
        path: path.to_owned(),
        reason: error.to_string(),
    })?;
    out.extend_from_slice(&rendered);
    Ok(())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|byte| format!("{byte:02x}")).collect()
    }

    #[test]
    fn sorts_keys_at_every_depth_and_writes_no_whitespace() {
        let value = json!({"b": [1, {"d": "é", "c": null}], "a": "x\"y\\z\n"});

        let bytes = canonical_bytes(&value).unwrap();

        assert_eq!(
            String::from_utf8(bytes).unwrap(),
            "{\"a\":\"x\\\"y\\\\z\\n\",\"b\":[1,{\"c\":null,\"d\":\"é\"}]}"
        );
    }

    #[test]
    fn canonical_sha256_matches_the_python_reference() {
        let value = json!({"b": [1, {"d": "é", "c": null}], "a": "x\"y\\z\n"});

        assert_eq!(
            hex(&canonical_sha256(&value).unwrap()),
            "f36d500e7a8ae710a765aa991d91f12f4fce55465558006901cdf9122dcabd13"
        );
    }

    #[test]
    fn key_order_in_the_source_does_not_change_the_bytes() {
        let first: Value =
            serde_json::from_str(r#"{"z": 1, "a": {"y": 2, "b": 3}}"#).unwrap();
        let second: Value =
            serde_json::from_str(r#"{"a": {"b": 3, "y": 2}, "z": 1}"#).unwrap();

        assert_eq!(canonical_bytes(&first), canonical_bytes(&second));
        assert_eq!(sorted(&first), sorted(&second));
    }

    #[test]
    fn integers_render_as_decimal_digits() {
        let value = json!({"n": u64::MAX, "m": i64::MIN, "z": 0});

        assert_eq!(
            String::from_utf8(canonical_bytes(&value).unwrap()).unwrap(),
            format!("{{\"m\":{},\"n\":{},\"z\":0}}", i64::MIN, u64::MAX)
        );
    }

    #[test]
    fn rejects_a_float_and_names_its_path() {
        let value = json!({"a": [1, {"b": 2.5}]});

        assert_eq!(
            canonical_bytes(&value),
            Err(CanonicalError::Float {
                path: "/a/1/b".to_owned()
            })
        );
        assert_eq!(
            canonical_sha256(&json!(1.0)),
            Err(CanonicalError::Float {
                path: String::new()
            })
        );
    }

    #[test]
    fn control_characters_escape_as_json_does() {
        let value = json!({"t": "\u{1}\t\u{7f}"});

        assert_eq!(
            String::from_utf8(canonical_bytes(&value).unwrap()).unwrap(),
            "{\"t\":\"\\u0001\\t\u{7f}\"}"
        );
    }

    #[test]
    fn pretty_output_sorts_keys_and_ends_with_a_newline() {
        let value = json!({"b": 1, "a": {"d": [1, 2], "c": "x"}});

        assert_eq!(
            pretty(&value).unwrap(),
            "{\n  \"a\": {\n    \"c\": \"x\",\n    \"d\": [\n      1,\n      2\n    ]\n  },\n  \"b\": 1\n}\n"
        );
    }

    #[test]
    fn sha256_of_bytes_matches_the_known_digest() {
        assert_eq!(
            hex(&sha256(b"abc")),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn errors_render_on_one_line() {
        let error = CanonicalError::Float {
            path: "/a".to_owned(),
        };

        assert_eq!(
            error.to_string(),
            "value at /a is a float; canonical JSON holds integers only"
        );
    }
}

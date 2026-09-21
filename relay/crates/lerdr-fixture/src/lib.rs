//! Golden fixture suite loader (`fixtures/README.md`).
//!
//! Parses a suite envelope (`format_version`, `suite`, `source`, `vectors[]`)
//! into [`serde_json::Value`] entries and provides typed decode helpers for
//! the fields inside them. No business logic — assertions live in the
//! consuming conformance tests.
//!
//! All binary fixture data is base64 **RawURLEncoding** (no padding); all hex
//! is lowercase.

use std::path::{Path, PathBuf};

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use serde::Deserialize;
use serde_json::Value;

/// Provenance block of a suite file.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Source {
    pub repo: String,
    pub commit: String,
    pub package: String,
    pub generator: String,
}

/// A parsed fixture suite envelope. `vectors` stays as raw `Value`s: field
/// shapes differ per suite and are asserted by the domain harnesses.
#[derive(Debug, Clone, Deserialize)]
pub struct Suite {
    pub format_version: u64,
    pub suite: String,
    pub source: Source,
    pub vectors: Vec<Value>,
}

impl Suite {
    /// Load `<fixtures_root>/<dir>/<suite>.json`.
    pub fn load(dir: &str, suite: &str) -> Result<Self, FixtureError> {
        Self::load_path(fixtures_root().join(dir).join(format!("{suite}.json")))
    }

    /// Load a suite file by path. Rejects unknown `format_version` and empty
    /// `vectors` (a zero-vector suite fails CI per fixtures/README.md).
    pub fn load_path(path: impl AsRef<Path>) -> Result<Self, FixtureError> {
        let path = path.as_ref();
        let raw = std::fs::read(path).map_err(|source| FixtureError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        let suite: Suite = serde_json::from_slice(&raw).map_err(|source| FixtureError::Json {
            path: path.to_path_buf(),
            source,
        })?;
        if suite.format_version != 1 {
            return Err(FixtureError::UnsupportedVersion {
                suite: suite.suite,
                format_version: suite.format_version,
            });
        }
        if suite.vectors.is_empty() {
            return Err(FixtureError::Empty(suite.suite));
        }
        Ok(suite)
    }
}

/// Root of the fixture tree. `LERDR_FIXTURES_DIR` overrides the default, which
/// resolves `fixtures/` relative to this crate (`<repo>/fixtures`).
pub fn fixtures_root() -> PathBuf {
    if let Ok(dir) = std::env::var("LERDR_FIXTURES_DIR") {
        return PathBuf::from(dir);
    }
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../../fixtures")
        .canonicalize()
        .expect("fixtures directory must exist")
}

#[derive(Debug, thiserror::Error)]
pub enum FixtureError {
    #[error("read {path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("parse {path}: {source}")]
    Json {
        path: PathBuf,
        source: serde_json::Error,
    },
    #[error("suite {suite:?} has unsupported format_version {format_version}")]
    UnsupportedVersion { suite: String, format_version: u64 },
    #[error("suite {0:?} has zero vectors")]
    Empty(String),
    #[error(transparent)]
    Field(#[from] FieldError),
}

#[derive(Debug, thiserror::Error)]
pub enum FieldError {
    #[error("missing field {0:?}")]
    Missing(String),
    #[error("field {field:?}: expected {expected}, got {actual}")]
    Type {
        field: String,
        expected: &'static str,
        actual: String,
    },
    #[error("field {field:?}: invalid base64 (RawURLEncoding): {source}")]
    Base64 {
        field: String,
        source: base64::DecodeError,
    },
    #[error("field {field:?}: invalid hex: {source}")]
    Hex {
        field: String,
        source: hex::FromHexError,
    },
}

fn type_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "bool",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

/// `vector[name]`, failing if absent.
pub fn field<'a>(vector: &'a Value, name: &str) -> Result<&'a Value, FieldError> {
    vector
        .get(name)
        .ok_or_else(|| FieldError::Missing(name.to_string()))
}

/// `vector[name]` as `&str`.
pub fn str_field<'a>(vector: &'a Value, name: &str) -> Result<&'a str, FieldError> {
    let value = field(vector, name)?;
    value.as_str().ok_or_else(|| FieldError::Type {
        field: name.to_string(),
        expected: "string",
        actual: type_name(value).to_string(),
    })
}

/// `vector[name]` as `u64` (fixture integers are non-negative).
pub fn u64_field(vector: &Value, name: &str) -> Result<u64, FieldError> {
    let value = field(vector, name)?;
    value.as_u64().ok_or_else(|| FieldError::Type {
        field: name.to_string(),
        expected: "u64",
        actual: type_name(value).to_string(),
    })
}

/// `vector[name]` as `bool`.
pub fn bool_field(vector: &Value, name: &str) -> Result<bool, FieldError> {
    let value = field(vector, name)?;
    value.as_bool().ok_or_else(|| FieldError::Type {
        field: name.to_string(),
        expected: "bool",
        actual: type_name(value).to_string(),
    })
}

/// `vector[name]` as a JSON array.
pub fn array_field<'a>(vector: &'a Value, name: &str) -> Result<&'a [Value], FieldError> {
    let value = field(vector, name)?;
    value
        .as_array()
        .map(Vec::as_slice)
        .ok_or_else(|| FieldError::Type {
            field: name.to_string(),
            expected: "array",
            actual: type_name(value).to_string(),
        })
}

/// Decode a base64 RawURLEncoding (unpadded) string — the fixture binary
/// encoding.
pub fn b64_decode(value: &str) -> Result<Vec<u8>, base64::DecodeError> {
    URL_SAFE_NO_PAD.decode(value)
}

/// `vector[name]` decoded from base64 RawURLEncoding.
pub fn b64_field(vector: &Value, name: &str) -> Result<Vec<u8>, FieldError> {
    let value = str_field(vector, name)?;
    b64_decode(value).map_err(|source| FieldError::Base64 {
        field: name.to_string(),
        source,
    })
}

/// `vector[name]` decoded from lowercase hex.
pub fn hex_field(vector: &Value, name: &str) -> Result<Vec<u8>, FieldError> {
    let value = str_field(vector, name)?;
    hex::decode(value).map_err(|source| FieldError::Hex {
        field: name.to_string(),
        source,
    })
}

/// `vector["name"]` — every vector entry is required to carry one.
pub fn vector_name(vector: &Value) -> &str {
    str_field(vector, "name").unwrap_or("<unnamed>")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loads_every_crypto_suite() {
        for suite in [
            "crypto.handshake.credential",
            "crypto.handshake.invitation",
            "crypto.frames.json",
            "crypto.frames.binary",
            "crypto.failures",
        ] {
            let loaded = Suite::load("crypto", suite).expect(suite);
            assert_eq!(loaded.suite, suite);
            assert!(!loaded.vectors.is_empty());
        }
    }

    #[test]
    fn decode_helpers() {
        let vector = serde_json::json!({
            "name": "t",
            "b64": "AAH_",   // [0x00, 0x01, 0xff]
            "hex": "00ff",
            "n": 7,
            "ok": true
        });
        assert_eq!(b64_field(&vector, "b64").unwrap(), vec![0x00, 0x01, 0xff]);
        assert_eq!(hex_field(&vector, "hex").unwrap(), vec![0x00, 0xff]);
        assert_eq!(u64_field(&vector, "n").unwrap(), 7);
        assert!(bool_field(&vector, "ok").unwrap());
        assert_eq!(vector_name(&vector), "t");
        assert!(matches!(
            u64_field(&vector, "b64"),
            Err(FieldError::Type { .. })
        ));
        assert!(matches!(
            field(&vector, "absent"),
            Err(FieldError::Missing(_))
        ));
    }
}

//! Go `encoding/json`-compatible serialization.
//!
//! The wire contract is defined by Go's `json.Marshal` output: compact form,
//! HTML-safe string escaping (`<`/`>`/`&` -> `<`/`<`/`&`, plus
//! U+2028/U+2029), `\u00xx` (lowercase hex) for control bytes without a short
//! escape (notably `\u0008` and `\u000c`, which serde_json would write as `\b`
//! / `\f`), and sorted map keys. [`to_vec`] produces exactly those bytes.

use std::io;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::ser::{CharEscape, Formatter};

/// `serde_json::ser::Formatter` that emits Go `encoding/json` byte output.
#[derive(Clone, Copy, Debug, Default)]
pub struct GoFormatter;

impl Formatter for GoFormatter {
    fn write_string_fragment<W>(&mut self, writer: &mut W, fragment: &str) -> io::Result<()>
    where
        W: ?Sized + io::Write,
    {
        // serde_json calls this for the runs it considers safe, which include
        // the characters Go escapes for HTML safety. Re-escape them here.
        let mut start = 0;
        for (index, ch) in fragment.char_indices() {
            let escape: &[u8] = match ch {
                '<' => b"\\u003c",
                '>' => b"\\u003e",
                '&' => b"\\u0026",
                '\u{2028}' => b"\\u2028",
                '\u{2029}' => b"\\u2029",
                _ => continue,
            };
            writer.write_all(&fragment.as_bytes()[start..index])?;
            writer.write_all(escape)?;
            start = index + ch.len_utf8();
        }
        writer.write_all(&fragment.as_bytes()[start..])
    }

    fn write_char_escape<W>(&mut self, writer: &mut W, escape: CharEscape) -> io::Result<()>
    where
        W: ?Sized + io::Write,
    {
        // Go uses \u00xx for every control byte except \n, \r, \t — it never
        // emits the \b or \f shortcuts serde_json prefers.
        match escape {
            CharEscape::Quote => writer.write_all(b"\\\""),
            CharEscape::ReverseSolidus => writer.write_all(b"\\\\"),
            CharEscape::Solidus => writer.write_all(b"/"),
            CharEscape::Backspace => writer.write_all(b"\\u0008"),
            CharEscape::FormFeed => writer.write_all(b"\\u000c"),
            CharEscape::LineFeed => writer.write_all(b"\\n"),
            CharEscape::CarriageReturn => writer.write_all(b"\\r"),
            CharEscape::Tab => writer.write_all(b"\\t"),
            CharEscape::AsciiControl(byte) => write!(writer, "\\u{byte:04x}"),
        }
    }
}

/// Serialize `value` exactly as Go's `json.Marshal` would.
pub fn to_vec<T>(value: &T) -> Result<Vec<u8>, serde_json::Error>
where
    T: ?Sized + Serialize,
{
    let mut buf = Vec::new();
    let mut serializer = serde_json::Serializer::with_formatter(&mut buf, GoFormatter);
    value.serialize(&mut serializer)?;
    Ok(buf)
}

/// [`to_vec`] as a `String`; the output is always valid UTF-8.
pub fn to_string<T>(value: &T) -> Result<String, serde_json::Error>
where
    T: ?Sized + Serialize,
{
    // JSON output of the formatter above is ASCII-or-UTF-8 by construction.
    Ok(String::from_utf8(to_vec(value)?).expect("JSON output is valid UTF-8"))
}

/// A field that distinguishes JSON `null` from a real value.
///
/// Wire messages in the Go relay are mostly `map[string]any`, where a key can
/// be absent, present-but-`null`, or a value — and all three serialize
/// differently. Model such fields as `Option<MaybeNull<T>>` (absent -> skip,
/// `null` -> emit `null`, value -> emit it) or as bare `MaybeNull<T>` with
/// `#[serde(default)]` for Go `any`/slice fields without `omitempty` (absent
/// or `null` -> emit `null`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum MaybeNull<T> {
    #[default]
    Null,
    Value(T),
}

impl<T> MaybeNull<T> {
    pub fn value(&self) -> Option<&T> {
        match self {
            Self::Null => None,
            Self::Value(value) => Some(value),
        }
    }

    pub fn into_value(self) -> Option<T> {
        match self {
            Self::Null => None,
            Self::Value(value) => Some(value),
        }
    }

    pub fn is_null(&self) -> bool {
        matches!(self, Self::Null)
    }
}

impl<T> From<T> for MaybeNull<T> {
    fn from(value: T) -> Self {
        Self::Value(value)
    }
}

impl<T: Serialize> Serialize for MaybeNull<T> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Null => serializer.serialize_none(),
            Self::Value(value) => value.serialize(serializer),
        }
    }
}

impl<'de, T: Deserialize<'de>> Deserialize<'de> for MaybeNull<T> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Option::<T>::deserialize(deserializer)?.map_or(Self::Null, Self::Value))
    }
}

/// `json.RawMessage` — a verbatim JSON payload that round-trips
/// byte-for-byte. `PartialEq` compares the raw text (Go's `RawMessage` is a
/// `[]byte`; equality is byte-wise).
#[derive(Debug, Clone)]
pub struct RawJson(pub Box<serde_json::value::RawValue>);

impl RawJson {
    /// The raw JSON text.
    pub fn get(&self) -> &str {
        self.0.get()
    }
}

impl PartialEq for RawJson {
    fn eq(&self, other: &Self) -> bool {
        self.0.get() == other.0.get()
    }
}

impl Eq for RawJson {}

impl Serialize for RawJson {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.0.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for RawJson {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Box::<serde_json::value::RawValue>::deserialize(deserializer).map(Self)
    }
}

/// Deserializer for `Option<MaybeNull<T>>` fields: serde's own `Option`
/// handling maps a present `null` to `None`, losing the distinction the Go
/// maps rely on. This keeps it as `Some(MaybeNull::Null)`.
pub fn de_nullable<'de, D, T>(deserializer: D) -> Result<Option<MaybeNull<T>>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    MaybeNull::<T>::deserialize(deserializer).map(Some)
}

/// Go's `json.Unmarshal` treats `null` as a no-op for non-pointer targets,
/// leaving the zero value. serde errors instead; this restores the behavior.
pub fn de_default<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de> + Default,
{
    Ok(Option::<T>::deserialize(deserializer)?.unwrap_or_default())
}

/// `skip_serializing_if` for `Option<Vec<T>>` mirroring Go `omitempty` on
/// slices: absent, `null`, and `[]` all omit the key.
pub fn opt_vec_is_empty<T>(value: &Option<Vec<T>>) -> bool {
    value.as_ref().is_none_or(Vec::is_empty)
}

/// `skip_serializing_if` for `Option<String>` mirroring Go `omitempty` on
/// strings: absent, `null`, and `""` all omit the key.
pub fn opt_str_is_empty(value: &Option<String>) -> bool {
    value.as_ref().is_none_or(String::is_empty)
}

/// `skip_serializing_if` for `Option<bool>` mirroring Go `omitempty` on
/// bools: absent, `null`, and `false` all omit the key.
pub fn opt_bool_is_false(value: &Option<bool>) -> bool {
    value.as_ref() != Some(&true)
}

/// `skip_serializing_if` for `Option<i64>`/`Option<u64>`/`Option<i32>`-style
/// numerics mirroring Go `omitempty` on ints: absent, `null`, and `0` omit.
pub fn opt_num_is_zero<T>(value: &Option<T>) -> bool
where
    T: Default + PartialEq + Copy,
{
    value.is_none_or(|v| v == T::default())
}

/// `skip_serializing_if` for `Option<BTreeMap>`-style maps mirroring Go
/// `omitempty`: absent, `null`, and `{}` all omit the key.
pub fn opt_map_is_empty<K, V>(value: &Option<std::collections::BTreeMap<K, V>>) -> bool {
    value.as_ref().is_none_or(|m| m.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn escapes_like_go() {
        let value = json!({"s": "\u{1b}[1m<bold> & \"quoted\"\u{1b}[0m \u{2028}"});
        let out = to_string(&value).unwrap();
        // Go escapes <, >, & for HTML safety and always escapes U+2028/U+2029.
        assert_eq!(
            out,
            "{\"s\":\"\\u001b[1m\\u003cbold\\u003e \\u0026 \\\"quoted\\\"\\u001b[0m \\u2028\"}"
        );
    }

    #[test]
    fn control_bytes_use_go_form() {
        let value = json!({"s": "\u{8}\u{c}\n\r\t\u{1f}"});
        let out = to_string(&value).unwrap();
        assert_eq!(out, "{\"s\":\"\\u0008\\u000c\\n\\r\\t\\u001f\"}");
    }

    #[test]
    fn unicode_line_separators_escaped() {
        let value = json!("\u{2028}\u{2029}");
        assert_eq!(to_string(&value).unwrap(), "\"\\u2028\\u2029\"");
    }

    #[test]
    fn maybe_null_serializes_three_states() {
        #[derive(Serialize)]
        struct M {
            #[serde(skip_serializing_if = "Option::is_none")]
            a: Option<MaybeNull<i64>>,
            #[serde(skip_serializing_if = "Option::is_none")]
            b: Option<MaybeNull<i64>>,
            #[serde(skip_serializing_if = "Option::is_none")]
            c: Option<MaybeNull<i64>>,
        }
        let m = M {
            a: None,
            b: Some(MaybeNull::Null),
            c: Some(MaybeNull::Value(3)),
        };
        assert_eq!(to_string(&m).unwrap(), "{\"b\":null,\"c\":3}");
    }
}

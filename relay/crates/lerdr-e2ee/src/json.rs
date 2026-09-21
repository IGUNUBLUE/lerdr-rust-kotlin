//! `encoding/json`-compatible JSON helpers.
//!
//! Go's `json.Marshal` HTML-escapes (`<` `>` `&` → `<` `>` `&`,
//! U+2028/U+2029 escaped) and renders `\b`/`\f` as ``/`\f` rather
//! than the `\b`/`\f` shorthands `serde_json` emits. Encoders in this crate
//! build wire bytes with [`escape_string`] so output is byte-identical to the
//! Go reference for any field content, not just fixture-shaped content.
//!
//! On the decode side, `json.Unmarshal` treats an explicit JSON `null` as a
//! no-op that leaves the Go zero value in place; [`null_default`] mirrors that
//! for serde.

use serde::Deserialize;

/// Append `s` to `out` as a JSON string literal, byte-identical to Go's
/// `json.Marshal` of the same string (HTML escaping on, lowercase `\u00xx`,
/// `\n` `\r` `\t` shorthands, `\b`/`\f` escaped as ``/`\f`).
pub(crate) fn escape_string(out: &mut String, s: &str) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '<' => out.push_str("\\u003c"),
            '>' => out.push_str("\\u003e"),
            '&' => out.push_str("\\u0026"),
            '\u{2028}' => out.push_str("\\u2028"),
            '\u{2029}' => out.push_str("\\u2029"),
            c if (c as u32) < 0x20 => {
                use std::fmt::Write;
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

/// Serde helper matching Go `json.Unmarshal` null semantics: an explicit
/// `null` deserializes to `T::default()` instead of failing.
pub(crate) fn null_default<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Default + Deserialize<'de>,
{
    Ok(Option::<T>::deserialize(deserializer)?.unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes_like_go() {
        let mut out = String::new();
        escape_string(&mut out, "a<b>&\"\\\u{8}\u{c}\n\t\u{2028}é");
        assert_eq!(
            out,
            "\"a\\u003cb\\u003e\\u0026\\\"\\\\\\u0008\\u000c\\n\\t\\u2028é\""
        );
    }
}

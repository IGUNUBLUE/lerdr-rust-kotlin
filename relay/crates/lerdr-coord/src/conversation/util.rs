//! Shared text/encoding helpers — ports of the oracle's `history.NormalizeLine`,
//! `sanitizeText`, `clampText`, `textValue`, `stableRowID`, and the
//! `encoding/json` marshalling behaviour `textValue` relies on for non-string
//! tool payloads.

use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

/// `history.ansiRe` — strips the ANSI/VT sequences pane snapshots carry into
/// transcript text. Five alternatives, in order:
/// CSI `ESC [ [0-9;]* [a-zA-Z]`, OSC `ESC ] [^BEL]* BEL`, charset
/// `ESC (|) [0-9A-B]`, `ESC [>=<]`, and `ESC [ ? [0-9;]* [hlJKHfG]` (the
/// private-parameter CSI the first alternative cannot reach).
fn strip_ansi(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == 0x1b {
            if let Some(consumed) = ansi_escape_len(&bytes[i..]) {
                i += consumed;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    // The input was valid UTF-8 and only ASCII escapes are removed.
    match String::from_utf8(out) {
        Ok(text) => text,
        Err(err) => String::from_utf8_lossy(err.as_bytes()).into_owned(),
    }
}

/// Length of the ANSI escape at `bytes[0] == 0x1b`, or `None`.
fn ansi_escape_len(bytes: &[u8]) -> Option<usize> {
    debug_assert_eq!(bytes[0], 0x1b);
    let second = *bytes.get(1)?;
    match second {
        b'[' => {
            // Two Go alternatives share this prefix: `ESC [ [0-9;]* [a-zA-Z]`
            // (no private marker) and `ESC [ ? [0-9;]* [hlJKHfG]` (a '?' run
            // requires one of those explicit finals).
            let mut i = 2;
            let private = if bytes.get(i) == Some(&b'?') {
                i += 1;
                true
            } else {
                false
            };
            while i < bytes.len() && (bytes[i].is_ascii_digit() || bytes[i] == b';') {
                i += 1;
            }
            match bytes.get(i) {
                Some(&final_byte) if private => {
                    if matches!(final_byte, b'h' | b'l' | b'J' | b'K' | b'H' | b'f' | b'G') {
                        Some(i + 1)
                    } else {
                        None
                    }
                }
                Some(&final_byte) if final_byte.is_ascii_alphabetic() => Some(i + 1),
                _ => None,
            }
        }
        b']' => {
            // OSC: ESC ] anything-not-BEL* BEL
            let mut i = 2;
            while i < bytes.len() && bytes[i] != 0x07 {
                i += 1;
            }
            if i < bytes.len() {
                Some(i + 1)
            } else {
                None
            }
        }
        b'(' | b')' => match bytes.get(2) {
            Some(&c) if c.is_ascii_digit() || (b'A'..=b'B').contains(&c) => Some(3),
            _ => None,
        },
        b'>' | b'=' | b'<' => Some(2),
        _ => None,
    }
}

/// `history.NormalizeLine`: strip ANSI, drop carriage returns, trim trailing
/// spaces/tabs/newlines.
pub(crate) fn normalize_line(line: &str) -> String {
    let line = strip_ansi(line);
    let line = line.replace('\r', "");
    line.trim_end_matches([' ', '\t', '\n']).to_string()
}

/// `sanitizeText`: normalize + drop NULs + unicode whitespace trim.
pub(crate) fn sanitize_text(text: &str) -> String {
    let text = normalize_line(text);
    text.replace('\x00', "").trim().to_string()
}

/// `clampText`: byte clamp with UTF-8 boundary backoff, then whitespace trim.
/// Returns `(clipped, truncated)`.
pub(crate) fn clamp_text(text: &str, limit: usize) -> (String, bool) {
    if text.len() <= limit {
        return (text.to_string(), false);
    }
    let mut end = limit;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    (text[..end].trim().to_string(), true)
}

/// `normalizedAgent`: lowercase, strip `-`, `_`, spaces.
pub(crate) fn normalized_agent(agent: &str) -> String {
    agent.trim().to_lowercase().replace(['-', '_', ' '], "")
}

/// `normalizedBlockType`: same normalization for record/block type strings.
pub(crate) fn normalized_block_type(value: &Value) -> String {
    string_value(value)
        .to_lowercase()
        .replace(['-', '_', ' '], "")
}

/// `stringValue`: the value when it is a JSON string, else "".
pub(crate) fn string_value(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        _ => String::new(),
    }
}

/// `firstString`: first named key holding a non-empty string.
pub(crate) fn first_string(record: &Map<String, Value>, keys: &[&str]) -> String {
    for key in keys {
        let value = string_value(record.get(*key).unwrap_or(&Value::Null));
        if !value.is_empty() {
            return value;
        }
    }
    String::new()
}

/// `firstValue`: first named key holding a non-null value.
pub(crate) fn first_value<'a>(record: &'a Map<String, Value>, keys: &[&str]) -> Option<&'a Value> {
    for key in keys {
        if let Some(value) = record.get(*key) {
            if !value.is_null() {
                return Some(value);
            }
        }
    }
    None
}

/// Text blocks: strings pass through; arrays contribute each text-like
/// block's `text` (types `text`, `input_text`, `output_text`).
pub(crate) fn text_block_list(value: &Value) -> Vec<String> {
    let Some(blocks) = value.as_array() else {
        return Vec::new();
    };
    let mut texts = Vec::new();
    for block in blocks {
        let Some(block) = block.as_object() else {
            continue;
        };
        let type_name = string_value(block.get("type").unwrap_or(&Value::Null));
        if type_name != "text" && type_name != "input_text" && type_name != "output_text" {
            continue;
        }
        let text = string_value(block.get("text").unwrap_or(&Value::Null));
        if !text.is_empty() {
            texts.push(text);
        }
    }
    texts
}

/// `textBlocks`: string → itself; array → text blocks joined by '\n'.
pub(crate) fn text_blocks(value: &Value) -> String {
    if let Value::String(text) = value {
        return text.clone();
    }
    text_block_list(value).join("\n")
}

/// `textValue`: string → itself; block list → joined text; anything else →
/// Go `json.Marshal` output ("" for null).
pub(crate) fn text_value(value: &Value) -> String {
    if let Value::String(text) = value {
        return text.clone();
    }
    let text = text_blocks(value);
    if !text.is_empty() {
        return text;
    }
    let data = go_json_marshal(value);
    if data == "null" {
        return String::new();
    }
    data
}

/// Go `encoding/json.Marshal` for a `serde_json::Value`: compact separators,
/// object keys sorted (Go sorts map keys; `serde_json::Map` is a BTreeMap and
/// iterates sorted too), and Go's HTML-safe string escaping
/// (`<` `>` `&` → `\u003c` `\u003e` `\u0026`, U+2028/U+2029 escaped).
pub(crate) fn go_json_marshal(value: &Value) -> String {
    let mut out = String::new();
    write_go_json(value, &mut out);
    out
}

fn write_go_json(value: &Value, out: &mut String) {
    match value {
        Value::Null => out.push_str("null"),
        Value::Bool(true) => out.push_str("true"),
        Value::Bool(false) => out.push_str("false"),
        Value::Number(number) => out.push_str(&go_json_number(number)),
        Value::String(text) => write_go_json_string(text, out),
        Value::Array(items) => {
            out.push('[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                write_go_json(item, out);
            }
            out.push(']');
        }
        Value::Object(map) => {
            out.push('{');
            for (index, (key, item)) in map.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                write_go_json_string(key, out);
                out.push(':');
                write_go_json(item, out);
            }
            out.push('}');
        }
    }
}

fn write_go_json_string(text: &str, out: &mut String) {
    out.push('"');
    for ch in text.chars() {
        match ch {
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
                out.push_str(&format!("\\u{:04x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

/// Go float formatting for JSON: `'f'` shortest unless `abs != 0` and
/// `abs < 1e-6` or `abs >= 1e21`, where `'e'` applies with the `e+09` → `e+9`
/// cleanup. Integers pass through unchanged.
fn go_json_number(number: &serde_json::Number) -> String {
    if let Some(int) = number.as_i64() {
        return int.to_string();
    }
    if let Some(int) = number.as_u64() {
        return int.to_string();
    }
    let Some(float) = number.as_f64() else {
        return "0".to_string();
    };
    let abs = float.abs();
    if abs != 0.0 && (abs < 1e-6 || abs >= 1e21) {
        // Rust `{:e}` gives e.g. "1.5e-7" / "1e21"; Go wants "1.5e-7" / "1e+21".
        let raw = format!("{:e}", float);
        match raw.split_once('e') {
            Some((mantissa, exponent)) => {
                let (sign, digits) = match exponent.strip_prefix('-') {
                    Some(digits) => ("-", digits),
                    None => ("+", exponent),
                };
                let digits = digits.trim_start_matches('0');
                let digits = if digits.is_empty() { "0" } else { digits };
                format!("{mantissa}e{sign}{digits}")
            }
            None => raw,
        }
    } else {
        // Rust Display matches Go's shortest 'f' output for this range.
        format!("{float}")
    }
}

/// `stableRowID`: `sha256(raw line)[:12]` hex + `-N` suffix for repeats.
pub(crate) fn stable_row_id(
    line: &[u8],
    seen: &mut std::collections::HashMap<String, u64>,
) -> String {
    let digest = Sha256::digest(line);
    let base = hex::encode(&digest[..12]);
    let occurrence = seen.entry(base.clone()).or_insert(0);
    let id = if *occurrence == 0 {
        base
    } else {
        format!("{base}-{occurrence}")
    };
    *occurrence += 1;
    id
}

/// `toolAssociationKey`: `sha256(agent + "\x00" + id)` hex — a map key, never
/// on the wire.
pub(crate) fn tool_association_key(agent: &str, id: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(agent.as_bytes());
    hasher.update(b"\x00");
    hasher.update(id.as_bytes());
    hex::encode(hasher.finalize())
}

/// `json.RawMessage` equivalent for a named object member: returns the raw
/// byte slice of `src`'s `name` member (whitespace-trimmed), preserving key
/// order and spacing exactly as stored — Go's `string(part.State.Input)`
/// semantics without re-marshalling.
pub(crate) fn json_member_raw<'a>(src: &'a str, name: &str) -> Option<&'a str> {
    let bytes = src.as_bytes();
    let mut i = skip_ws(bytes, 0);
    if bytes.get(i) != Some(&b'{') {
        return None;
    }
    i += 1;
    loop {
        i = skip_ws(bytes, i);
        if bytes.get(i) == Some(&b'}') {
            return None;
        }
        let (key, next) = parse_json_string(bytes, i)?;
        i = skip_ws(bytes, next);
        if bytes.get(i) != Some(&b':') {
            return None;
        }
        i = skip_ws(bytes, i + 1);
        let value_start = i;
        let value_end = json_value_end(bytes, i)?;
        if key == name {
            return src.get(value_start..value_end);
        }
        i = skip_ws(bytes, value_end);
        match bytes.get(i) {
            Some(&b',') => i += 1,
            Some(&b'}') => return None,
            _ => return None,
        }
    }
}

fn skip_ws(bytes: &[u8], mut i: usize) -> usize {
    while i < bytes.len() && matches!(bytes[i], b' ' | b'\t' | b'\n' | b'\r') {
        i += 1;
    }
    i
}

/// Parse a JSON string at `bytes[i] == '"'`, returning the unescaped string
/// and the index just past the closing quote.
fn parse_json_string(bytes: &[u8], i: usize) -> Option<(String, usize)> {
    if bytes.get(i) != Some(&b'"') {
        return None;
    }
    let mut out = Vec::new();
    let mut j = i + 1;
    while j < bytes.len() {
        match bytes[j] {
            b'"' => {
                let text = String::from_utf8_lossy(&out).into_owned();
                return Some((text, j + 1));
            }
            b'\\' => {
                j += 1;
                match bytes.get(j)? {
                    b'n' => out.push(b'\n'),
                    b't' => out.push(b'\t'),
                    b'r' => out.push(b'\r'),
                    b'b' => out.push(0x08),
                    b'f' => out.push(0x0c),
                    b'u' => {
                        // Only the key comparison uses this; encode the
                        // codepoint lossily rather than full surrogate pairs.
                        let hex_digits = bytes.get(j + 1..j + 5)?;
                        let code =
                            u32::from_str_radix(std::str::from_utf8(hex_digits).ok()?, 16).ok()?;
                        let ch = char::from_u32(code).unwrap_or('\u{fffd}');
                        let mut buf = [0u8; 4];
                        out.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
                        j += 4;
                    }
                    &escaped => out.push(escaped),
                }
                j += 1;
            }
            byte => {
                out.push(byte);
                j += 1;
            }
        }
    }
    None
}

/// End index (exclusive) of the JSON value starting at `bytes[i]`.
fn json_value_end(bytes: &[u8], i: usize) -> Option<usize> {
    match bytes.get(i)? {
        b'"' => {
            let (_, end) = parse_json_string(bytes, i)?;
            Some(end)
        }
        b'{' | b'[' => {
            // The source is already known to parse (callers only scan valid
            // JSON), so a single shared depth counter balances both bracket
            // kinds.
            let mut depth = 0usize;
            let mut j = i;
            while j < bytes.len() {
                match bytes[j] {
                    b'"' => {
                        let (_, end) = parse_json_string(bytes, j)?;
                        j = end;
                        continue;
                    }
                    b'{' | b'[' => depth += 1,
                    b'}' | b']' => {
                        depth -= 1;
                        if depth == 0 {
                            return Some(j + 1);
                        }
                    }
                    _ => {}
                }
                j += 1;
            }
            None
        }
        _ => {
            // Scalars: number / true / false / null — run until a delimiter.
            let mut j = i;
            while j < bytes.len()
                && !matches!(bytes[j], b',' | b'}' | b']' | b' ' | b'\t' | b'\n' | b'\r')
            {
                j += 1;
            }
            if j == i {
                None
            } else {
                Some(j)
            }
        }
    }
}

/// `innerTag`: trimmed contents of the first `<name>...</name>` pair.
pub(crate) fn inner_tag(text: &str, name: &str) -> String {
    let start_token = format!("<{name}>");
    let end_token = format!("</{name}>");
    let Some(start) = text.find(&start_token) else {
        return String::new();
    };
    let start = start + start_token.len();
    let Some(end) = text[start..].find(&end_token) else {
        return String::new();
    };
    text[start..start + end].trim().to_string()
}

/// RFC 3339 / `time.RFC3339Nano` from Unix epoch seconds+nanos: no fractional
/// part when zero, otherwise trimmed to the needed precision. Matches Go's
/// `time.Time.UTC().Format(time.RFC3339Nano)`.
pub(crate) fn format_rfc3339_nano(secs: i64, nanos: i64) -> String {
    let days = secs.div_euclid(86_400);
    let secs_of_day = secs.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    let hour = secs_of_day / 3600;
    let minute = (secs_of_day % 3600) / 60;
    let second = secs_of_day % 60;
    let mut out = format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}");
    if nanos > 0 {
        let mut frac = format!("{nanos:09}");
        while frac.ends_with('0') {
            frac.pop();
        }
        out.push('.');
        out.push_str(&frac);
    }
    out.push('Z');
    out
}

/// `time.UnixMilli(ms).UTC().Format(time.RFC3339Nano)`.
pub(crate) fn format_unix_millis(ms: i64) -> String {
    format_rfc3339_nano(ms.div_euclid(1000), ms.rem_euclid(1000) * 1_000_000)
}

/// Hermes `hermesTimestamp`: `time.Unix(0, int64(v*1e9))` — float seconds with
/// truncation toward zero, `""` when `v <= 0`.
pub(crate) fn format_unix_seconds_float(seconds: f64) -> String {
    if seconds <= 0.0 {
        return String::new();
    }
    let nanos = (seconds * 1e9) as i64;
    format_rfc3339_nano(
        nanos.div_euclid(1_000_000_000),
        nanos.rem_euclid(1_000_000_000),
    )
}

/// Howard Hinnant's `civil_from_days` — days since 1970-01-01 → (y, m, d).
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

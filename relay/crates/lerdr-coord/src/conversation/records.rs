//! JSONL record handling — the port of `jsonl_records.go` plus the per-agent
//! record grammars from `reader.go` (`parseTranscript`, `parseToolActivity`,
//! `parseClaudeRecord`, `parseCodexRecord`, `parsePiRecord`) and the tool
//! normalization bounds.
//!
//! Go hashes raw line bytes for entry ids and tolerates invalid UTF-8 inside
//! strings (`encoding/json` substitutes U+FFFD); this port parses lines as
//! bytes — a line whose bytes are not valid UTF-8/JSON is skipped, which is
//! the same observable outcome for every well-formed transcript.

use std::collections::HashMap;

use serde_json::Value;

use super::types::{Entry, ToolActivity};
use super::util::{
    clamp_text, first_string, first_value, inner_tag, normalized_agent, normalized_block_type,
    sanitize_text, stable_row_id, string_value, text_block_list, text_blocks, text_value,
    tool_association_key,
};

pub(crate) const MAX_ENTRY_BYTES: usize = 128 * 1024;
pub(crate) const MAX_TOOL_COUNT: usize = 128;
pub(crate) const MAX_TOOL_ID_BYTES: usize = 256;
pub(crate) const MAX_TOOL_NAME_BYTES: usize = 160;
pub(crate) const MAX_TOOL_INPUT_BYTES: usize = 1024 * 1024;
pub(crate) const MAX_TOOL_OUTPUT_BYTES: usize = 1024 * 1024;
pub(crate) const DEFAULT_PAGE_SIZE: usize = 80;
pub(crate) const MAX_PAGE_SIZE: usize = 200;

/// `toolResult` — a tool output waiting to be attached to a pending call.
pub(crate) struct ToolResult {
    pub id: String,
    pub output: String,
    pub failed: bool,
}

/// `toolLocation` — entry index + tool index of a pending call.
#[derive(Clone, Copy)]
struct ToolLocation {
    entry: usize,
    tool: usize,
}

/// `JSONLRecord` — one raw line with byte-range bookkeeping.
#[derive(Debug, Clone, Default)]
pub(crate) struct JsonlRecord {
    pub start: i64,
    pub end: i64,
    pub raw: Vec<u8>,
    pub complete: bool,
    pub oversized: bool,
    pub starts_inside: bool,
    pub trailing: bool,
}

/// `collectJSONLRecordsBytes`: split `data` (the bytes at `[start, start+len)`)
/// into records. When `starts_inside` the window begins mid-line and the
/// first record is consumed but not returned. Records whose accumulated
/// content exceeds `max_bytes` are counted as oversized and skipped.
pub(crate) fn collect_jsonl_records_bytes(
    data: &[u8],
    start: i64,
    starts_inside: bool,
    max_bytes: i64,
) -> (Vec<JsonlRecord>, usize) {
    let end = start + data.len() as i64;
    let mut position = start;
    let mut records = Vec::new();
    let mut oversized_count = 0usize;
    let mut first = true;
    while position < end {
        let mut record = JsonlRecord {
            start: position,
            ..JsonlRecord::default()
        };
        let mut raw: Vec<u8> = Vec::new();
        let mut raw_bytes: i64 = 0;
        let mut over = false;
        let mut had_newline = false;
        loop {
            // ReadSlice('\n') over the in-memory window.
            let rel = (position - start) as usize;
            let fragment = match memchr(b'\n', &data[rel..]) {
                Some(newline) => {
                    let frag_end = rel + newline + 1;
                    &data[rel..frag_end]
                }
                None => &data[rel..],
            };
            if !fragment.is_empty() {
                position += fragment.len() as i64;
                let mut content: &[u8] = fragment;
                if content.last() == Some(&b'\n') {
                    had_newline = true;
                    content = &content[..content.len() - 1];
                    if content.last() == Some(&b'\r') {
                        content = &content[..content.len() - 1];
                    }
                }
                if !over {
                    if raw_bytes + content.len() as i64 > max_bytes {
                        over = true;
                        raw = Vec::new();
                    } else {
                        raw.extend_from_slice(content);
                        raw_bytes += content.len() as i64;
                    }
                }
            }
            // A fragment with a newline is a complete ReadSlice; otherwise the
            // window is exhausted (io.EOF).
            if fragment.last() == Some(&b'\n') || position >= end {
                break;
            }
        }
        record.end = position;
        if starts_inside && first {
            first = false;
            continue;
        }
        first = false;
        if over {
            oversized_count += 1;
            continue;
        }
        if raw.iter().all(|b| b.is_ascii_whitespace()) {
            continue;
        }
        record.raw = raw;
        record.trailing = !had_newline;
        record.complete = had_newline || serde_json::from_slice::<Value>(&record.raw).is_ok();
        if !record.complete {
            record.trailing = true;
        }
        record.starts_inside = starts_inside && record.start == start;
        records.push(record);
    }
    (records, oversized_count)
}

fn memchr(needle: u8, haystack: &[u8]) -> Option<usize> {
    haystack.iter().position(|b| *b == needle)
}

/// `parseTranscript` — the flat JSONL projection. `text` is the (possibly
/// tail-clipped) transcript bytes; lines are hashed raw for entry ids.
pub(crate) fn parse_transcript(agent: &str, text: &[u8]) -> Vec<Entry> {
    let normalized = normalized_agent(agent);
    let mut entries: Vec<Entry> = Vec::new();
    let mut seen_ids: HashMap<String, u64> = HashMap::new();
    let mut pending_tools: HashMap<String, ToolLocation> = HashMap::new();
    for line in text.split(|b| *b == b'\n') {
        if line.iter().all(|b| b.is_ascii_whitespace()) {
            continue;
        }
        let Ok(record) = serde_json::from_slice::<Value>(line) else {
            continue;
        };
        let Some(record) = record.as_object() else {
            continue;
        };
        let (calls, results) = parse_tool_activity(&normalized, record);
        for result in results {
            let key = tool_association_key(&normalized, &result.id);
            let Some(location) = pending_tools.get(&key).copied() else {
                continue;
            };
            if location.entry >= entries.len()
                || location.tool >= entries[location.entry].tools.len()
            {
                continue;
            }
            let output = sanitize_text(&result.output);
            let (output, truncated) = clamp_text(&output, MAX_ENTRY_BYTES);
            let tool = &mut entries[location.entry].tools[location.tool];
            tool.output = output;
            tool.error = result.failed;
            tool.truncated = tool.truncated || truncated;
            pending_tools.remove(&key);
        }

        let (role, timestamp, body) = visible_record(&normalized, record);
        let body = sanitize_text(&body);
        let role = if role.is_empty() && !calls.is_empty() {
            "assistant".to_string()
        } else {
            role
        };
        if role.is_empty() || (body.is_empty() && calls.is_empty()) {
            continue;
        }
        let (body, truncated) = clamp_text(&body, MAX_ENTRY_BYTES);
        let mut entry = Entry {
            id: String::new(),
            timestamp,
            role,
            text: body,
            tools: calls,
            truncated,
        };
        normalize_entry_tools(&mut entry);
        entry.id = stable_row_id(line, &mut seen_ids);
        let entry_index = entries.len();
        entries.push(entry);
        for tool_index in 0..entries[entry_index].tools.len() {
            let id = tool_association_id(&entries[entry_index].tools[tool_index]);
            if !id.is_empty() {
                pending_tools.insert(
                    tool_association_key(&normalized, &id),
                    ToolLocation {
                        entry: entry_index,
                        tool: tool_index,
                    },
                );
            }
        }
    }
    entries
}

/// `visibleRecord`: `(role, timestamp, body)` for one parsed record.
pub(crate) fn visible_record(
    agent: &str,
    record: &serde_json::Map<String, Value>,
) -> (String, String, String) {
    let timestamp = string_value(record.get("timestamp").unwrap_or(&Value::Null));
    let (role, body) = match agent {
        "claude" | "claudecode" | "qoder" | "qodercli" => parse_claude_record(record),
        "codex" | "openaicodex" => parse_codex_record(record),
        "pi" | "picodingagent" | "omp" | "ohmypi" | "omo" | "ohmyopencode" => {
            parse_pi_record(record)
        }
        _ => (String::new(), String::new()),
    };
    (role, timestamp, body)
}

/// `parseToolActivity` — `(calls, results)` carried by one record.
fn parse_tool_activity(
    agent: &str,
    record: &serde_json::Map<String, Value>,
) -> (Vec<ToolActivity>, Vec<ToolResult>) {
    match agent {
        "claude" | "claudecode" | "qoder" | "qodercli" => {
            if record.get("isSidechain") == Some(&Value::Bool(true)) {
                return (Vec::new(), Vec::new());
            }
            let message = record
                .get("message")
                .and_then(Value::as_object)
                .cloned()
                .unwrap_or_default();
            let blocks = message
                .get("content")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            tools_from_blocks(&blocks)
        }
        "codex" | "openaicodex" => {
            if string_value(record.get("type").unwrap_or(&Value::Null)) != "response_item" {
                return (Vec::new(), Vec::new());
            }
            let Some(payload) = record.get("payload").and_then(Value::as_object) else {
                return (Vec::new(), Vec::new());
            };
            match normalized_block_type(payload.get("type").unwrap_or(&Value::Null)).as_str() {
                "functioncall" | "customtoolcall" | "localshellcall" => {
                    let call = new_tool_activity(
                        &first_string(payload, &["call_id", "id"]),
                        &first_string(payload, &["name", "tool_name"]),
                        first_value(payload, &["arguments", "input"]),
                    );
                    (vec![call], Vec::new())
                }
                "functioncalloutput" | "customtoolcalloutput" | "localshellcalloutput" => (
                    Vec::new(),
                    vec![ToolResult {
                        id: first_string(payload, &["call_id", "id"]).trim().to_string(),
                        output: text_value(
                            first_value(payload, &["output", "content"]).unwrap_or(&Value::Null),
                        ),
                        failed: payload.get("is_error") == Some(&Value::Bool(true)),
                    }],
                ),
                _ => (Vec::new(), Vec::new()),
            }
        }
        "pi" | "picodingagent" | "omp" | "ohmypi" | "omo" | "ohmyopencode" => {
            if string_value(record.get("type").unwrap_or(&Value::Null)) != "message" {
                return (Vec::new(), Vec::new());
            }
            let Some(message) = record.get("message").and_then(Value::as_object) else {
                return (Vec::new(), Vec::new());
            };
            if normalized_block_type(message.get("role").unwrap_or(&Value::Null)) == "toolresult" {
                return (
                    Vec::new(),
                    vec![ToolResult {
                        id: first_string(message, &["toolCallId", "tool_call_id", "id"])
                            .trim()
                            .to_string(),
                        output: text_value(message.get("content").unwrap_or(&Value::Null)),
                        failed: message.get("isError") == Some(&Value::Bool(true))
                            || message.get("is_error") == Some(&Value::Bool(true)),
                    }],
                );
            }
            let blocks = message
                .get("content")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            tools_from_blocks(&blocks)
        }
        _ => (Vec::new(), Vec::new()),
    }
}

/// `toolsFromBlocks`: `tool_use`/`tool_call` blocks become calls,
/// `tool_result` blocks become results.
fn tools_from_blocks(blocks: &[Value]) -> (Vec<ToolActivity>, Vec<ToolResult>) {
    let mut calls = Vec::new();
    let mut results = Vec::new();
    for raw in blocks {
        let Some(block) = raw.as_object() else {
            continue;
        };
        match normalized_block_type(block.get("type").unwrap_or(&Value::Null)).as_str() {
            "tooluse" | "toolcall" => calls.push(new_tool_activity(
                &first_string(block, &["id", "toolCallId", "tool_call_id"]),
                &first_string(block, &["name", "toolName", "tool_name"]),
                first_value(block, &["input", "arguments"]),
            )),
            "toolresult" => results.push(ToolResult {
                id: first_string(block, &["tool_use_id", "toolCallId", "tool_call_id", "id"])
                    .trim()
                    .to_string(),
                output: text_value(block.get("content").unwrap_or(&Value::Null)),
                failed: block.get("is_error") == Some(&Value::Bool(true))
                    || block.get("isError") == Some(&Value::Bool(true)),
            }),
            _ => {}
        }
    }
    (calls, results)
}

/// `newToolActivity`: default name "Tool", input sanitized + clamped to
/// `maxEntryBytes/2`, association id = the trimmed source id.
pub(crate) fn new_tool_activity(id: &str, name: &str, input: Option<&Value>) -> ToolActivity {
    let name = if name.trim().is_empty() { "Tool" } else { name };
    let input_text = sanitize_text(&text_value(input.unwrap_or(&Value::Null)));
    let (input_text, truncated) = clamp_text(&input_text, MAX_ENTRY_BYTES / 2);
    let tool = ToolActivity {
        id: id.trim().to_string(),
        name: name.trim().to_string(),
        association_id: id.trim().to_string(),
        input: input_text,
        output: String::new(),
        error: false,
        truncated,
    };
    let (normalized, _) = normalize_tool_activity(tool);
    normalized
}

/// `toolAssociationID`: the pre-normalization id when present, else the
/// trimmed public id.
pub(crate) fn tool_association_id(tool: &ToolActivity) -> String {
    if !tool.association_id.is_empty() {
        return tool.association_id.clone();
    }
    tool.id.trim().to_string()
}

fn normalize_tool_id(value: &str) -> String {
    clamp_text(value.trim(), MAX_TOOL_ID_BYTES).0
}

/// `normalizeToolActivity`: id ≤256B, name ≤160B, input/output ≤1MiB; any
/// change marks the tool truncated.
pub(crate) fn normalize_tool_activity(mut tool: ToolActivity) -> (ToolActivity, bool) {
    let original_id = tool.id.clone();
    let original_name = tool.name.clone();
    let original_input = tool.input.clone();
    let original_output = tool.output.clone();
    let original_error = tool.error;
    let original_truncated = tool.truncated;
    tool.association_id = tool_association_id(&tool);
    tool.id = normalize_tool_id(&tool.association_id.clone());
    tool.name = clamp_text(tool.name.trim(), MAX_TOOL_NAME_BYTES).0;
    tool.input = clamp_text(&tool.input, MAX_TOOL_INPUT_BYTES).0;
    tool.output = clamp_text(&tool.output, MAX_TOOL_OUTPUT_BYTES).0;
    if tool.name.is_empty() {
        tool.name = "Tool".to_string();
    }
    let changed = tool.id != original_id
        || tool.name != original_name
        || tool.input != original_input
        || tool.output != original_output
        || tool.error != original_error
        || tool.truncated != original_truncated;
    if changed {
        tool.truncated = true;
    }
    (tool, changed)
}

/// `normalizeEntryTools`: cap tools at 128, normalize each; returns
/// `(omitted_tools, omitted_payloads)` and sets `entry.truncated`.
pub(crate) fn normalize_entry_tools(entry: &mut Entry) -> (i64, i64) {
    if entry.tools.is_empty() {
        return (0, 0);
    }
    let mut omitted_tools = 0i64;
    let mut omitted_payloads = 0i64;
    if entry.tools.len() > MAX_TOOL_COUNT {
        omitted_tools = (entry.tools.len() - MAX_TOOL_COUNT) as i64;
        entry.tools.truncate(MAX_TOOL_COUNT);
        entry.truncated = true;
    }
    for index in 0..entry.tools.len() {
        let (normalized, changed) = normalize_tool_activity(entry.tools[index].clone());
        if changed || normalized.truncated {
            omitted_payloads += 1;
            entry.truncated = true;
        }
        entry.tools[index] = normalized;
    }
    (omitted_tools, omitted_payloads)
}

/// `normalizeEntriesForResponse` — normalize in place, count omissions.
pub(crate) fn normalize_entries_for_response(entries: &mut [Entry]) -> (i64, i64) {
    let mut tools = 0;
    let mut payloads = 0;
    for entry in entries.iter_mut() {
        let (t, p) = normalize_entry_tools(entry);
        tools += t;
        payloads += p;
    }
    (tools, payloads)
}

/// `parseClaudeRecord` — claude/qoder grammar.
fn parse_claude_record(record: &serde_json::Map<String, Value>) -> (String, String) {
    let role = string_value(record.get("type").unwrap_or(&Value::Null));
    if (role != "user" && role != "assistant")
        || record.get("isSidechain") == Some(&Value::Bool(true))
    {
        return (String::new(), String::new());
    }
    let Some(message) = record.get("message").and_then(Value::as_object) else {
        return (String::new(), String::new());
    };
    let content = message.get("content").unwrap_or(&Value::Null);
    if let Value::String(raw) = content {
        if role != "user" {
            return (role, raw.clone());
        }
        return (role, human_claude_text(raw));
    }
    if role == "user" {
        // Filter per block, like the oracle: envelope checks anchor on the
        // start of each block's text.
        let blocks = text_block_list(content);
        let kept: Vec<String> = blocks
            .iter()
            .map(|block| human_claude_text(block))
            .filter(|text| !text.trim().is_empty())
            .collect();
        if kept.is_empty() {
            return (String::new(), String::new());
        }
        return (role, kept.join("\n"));
    }
    (role, text_blocks(content))
}

/// `humanClaudeText`: filter envelope prefixes and expand `<command-name>`.
fn human_claude_text(raw: &str) -> String {
    let trimmed = raw.trim();
    for envelope in [
        "<system-reminder>",
        "<local-command-caveat>",
        "<task-notification>",
        "<local-command-stdout>",
    ] {
        if trimmed.starts_with(envelope) {
            return String::new();
        }
    }
    if trimmed.starts_with("<command-name>") {
        let name = inner_tag(trimmed, "command-name");
        let arguments = inner_tag(trimmed, "command-args");
        return format!("{name} {arguments}").trim().to_string();
    }
    raw.to_string()
}

/// `parseCodexRecord` — codex `response_item` messages only.
fn parse_codex_record(record: &serde_json::Map<String, Value>) -> (String, String) {
    if string_value(record.get("type").unwrap_or(&Value::Null)) != "response_item" {
        return (String::new(), String::new());
    }
    let Some(payload) = record.get("payload").and_then(Value::as_object) else {
        return (String::new(), String::new());
    };
    if string_value(payload.get("type").unwrap_or(&Value::Null)) != "message" {
        return (String::new(), String::new());
    }
    let role = string_value(payload.get("role").unwrap_or(&Value::Null));
    if role != "user" && role != "assistant" {
        return (String::new(), String::new());
    }
    let text = text_blocks(payload.get("content").unwrap_or(&Value::Null));
    if role == "user" && text.trim_start().starts_with("<environment_context>") {
        return (String::new(), String::new());
    }
    (role, text)
}

/// `parsePiRecord` — pi/omp/omo `type:"message"` records.
fn parse_pi_record(record: &serde_json::Map<String, Value>) -> (String, String) {
    if string_value(record.get("type").unwrap_or(&Value::Null)) != "message" {
        return (String::new(), String::new());
    }
    let Some(message) = record.get("message").and_then(Value::as_object) else {
        return (String::new(), String::new());
    };
    let role = string_value(message.get("role").unwrap_or(&Value::Null));
    if role != "user" && role != "assistant" {
        return (String::new(), String::new());
    }
    (
        role,
        text_blocks(message.get("content").unwrap_or(&Value::Null)),
    )
}

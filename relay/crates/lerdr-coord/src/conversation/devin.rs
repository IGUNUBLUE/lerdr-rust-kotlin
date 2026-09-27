//! Devin transcript reader — the agent writes one JSON document per session
//! at `<data root>/transcripts/<session_id>.json` (ATIF `steps[]` records
//! with `source` ∈ system/user/agent). Unlike the flat JSONL readers the
//! whole document is parsed at once; `steps` is the projection surface.
//!
//! - `system` steps (sysprompt, telemetry envelopes) never surface.
//! - `agent` steps carry `tool_calls[]` plus an `observation.results[]` list
//!   keyed by `source_call_id` — outputs attach to the call in the same step,
//!   so there is no pending-call bookkeeping across entries.
//! - Entry ids are the ATIF `step_id` values — stable across reads and unique
//!   within the file, which is all the `before` cursor needs.
//! - `reasoning_content`/`metrics`/`extra` are dropped: the feed has no
//!   projections for them.

use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};

use serde_json::Value;

use super::reader::{
    contained_regular_file, open_conversation_source, safe_session_id, Error, Reader,
    DEFAULT_PAGE_SIZE, MAX_CONVERSATION_BYTES, MAX_PAGE_SIZE,
};
use super::records::{new_tool_activity, normalize_entry_tools, MAX_ENTRY_BYTES};
use super::roots;
use super::types::{Entry, Location, Page};
use super::util::{clamp_text, sanitize_text, string_value, text_value};

/// `devinTranscripts` — `<root>/transcripts`.
fn devin_transcripts_dir(root: &str) -> PathBuf {
    Path::new(root).join("transcripts")
}

/// `locateDevin` — `transcripts/<session_id>.json` under the first root that
/// contains it; absolute `.json` session paths are containment-checked like
/// the flat readers' `resolvePathOrSession`.
pub(crate) fn devin_locate(roots: &[String], session_id: &str) -> Location {
    if Path::new(session_id).is_absolute() && session_id.to_lowercase().ends_with(".json") {
        for root in roots {
            if let Some(path) =
                contained_regular_file(Path::new(session_id), &devin_transcripts_dir(root))
            {
                return Location {
                    path: path.to_string_lossy().into_owned(),
                    root: root.clone(),
                    title: String::new(),
                };
            }
        }
        return Location::default();
    }
    if !safe_session_id(session_id) {
        return Location::default();
    }
    let filename = format!("{session_id}.json");
    for root in roots {
        let dir = devin_transcripts_dir(root);
        if let Some(path) = contained_regular_file(&dir.join(&filename), &dir) {
            return Location {
                path: path.to_string_lossy().into_owned(),
                root: root.clone(),
                title: String::new(),
            };
        }
    }
    Location::default()
}

/// `atifStepId` — `step_id` arrives as a string in current transcripts; a
/// number is tolerated so the cursor stays stable if the writer switches.
fn atif_step_id(step: &Value) -> String {
    match step.get("step_id") {
        Some(Value::String(id)) => id.clone(),
        Some(Value::Number(n)) => n.to_string(),
        _ => String::new(),
    }
}

/// `atifStepTimestamp` — `timestamp` is RFC 3339 text.
fn atif_step_timestamp(step: &Value) -> String {
    string_value(step.get("timestamp").unwrap_or(&Value::Null))
}

/// `atifAgentTools` — calls from `tool_calls[]`, outputs wired from the same
/// step's `observation.results[]` via `source_call_id`.
fn atif_agent_tools(step: &Value) -> Vec<super::types::ToolActivity> {
    let mut tools = Vec::new();
    let Some(calls) = step.get("tool_calls").and_then(Value::as_array) else {
        return tools;
    };
    for call in calls {
        let Some(call) = call.as_object() else {
            continue;
        };
        let id = string_value(call.get("tool_call_id").unwrap_or(&Value::Null));
        let name = string_value(call.get("function_name").unwrap_or(&Value::Null));
        tools.push(new_tool_activity(&id, &name, call.get("arguments")));
    }
    if tools.is_empty() {
        return tools;
    }
    let mut outputs: HashMap<String, (String, bool)> = HashMap::new();
    if let Some(results) = step
        .get("observation")
        .and_then(|obs| obs.get("results"))
        .and_then(Value::as_array)
    {
        for result in results {
            let Some(result) = result.as_object() else {
                continue;
            };
            let key = string_value(result.get("source_call_id").unwrap_or(&Value::Null));
            if key.is_empty() {
                continue;
            }
            let output = sanitize_text(&text_value(result.get("content").unwrap_or(&Value::Null)));
            let failed = result.get("is_error") == Some(&Value::Bool(true))
                || result.get("error") == Some(&Value::Bool(true));
            outputs.insert(key, (output, failed));
        }
    }
    for tool in tools.iter_mut() {
        let key = if tool.association_id.is_empty() {
            tool.id.clone()
        } else {
            tool.association_id.clone()
        };
        if let Some((output, failed)) = outputs.get(&key) {
            let (output, truncated) = clamp_text(output, MAX_ENTRY_BYTES);
            tool.output = output.clone();
            tool.error = *failed;
            tool.truncated = tool.truncated || truncated;
        }
    }
    tools
}

impl Reader {
    /// `locateDevinWithProject` — session files are global (keyed by slug,
    /// not by project), so the project context only feeds the cache key.
    fn devin_locate(&self, session_id: &str) -> Location {
        let roots = roots::devin_data_roots(&self.home_str(), self.env());
        devin_locate(&roots, session_id)
    }

    /// `readDevin` — locate + parse the ATIF document + page. A document that
    /// fails to parse (truncated mid-write, wrong schema) reports
    /// `source_corrupt` rather than an empty page, so the app can distinguish
    /// "no transcript" from "transcript unreadable".
    pub(crate) fn devin_read_for(
        &self,
        _cwd: &str,
        session_id: &str,
        before: &str,
        limit: usize,
    ) -> Result<Page, Error> {
        let session_id = session_id.trim();
        if session_id.is_empty() {
            return Ok(Page::unavailable(
                "invalid_session",
                "This agent has not reported a conversation session yet.",
            ));
        }
        let location = self.devin_locate(session_id);
        if location.path.is_empty() {
            return Ok(Page::unavailable(
                "invalid_session",
                "No conversation log is available for this session.",
            ));
        }
        let file = open_conversation_source(Path::new(&location.path))
            .map_err(|err| Error::new(format!("read conversation log: {err}")))?;
        let mut bytes = Vec::new();
        file.take(MAX_CONVERSATION_BYTES as u64)
            .read_to_end(&mut bytes)
            .map_err(|err| Error::new(format!("read conversation log: {err}")))?;
        let Ok(document) = serde_json::from_slice::<Value>(&bytes) else {
            return Ok(Page::unavailable(
                "source_corrupt",
                "The conversation log is not a readable Devin transcript.",
            ));
        };
        let steps = document
            .get("steps")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let mut entries: Vec<Entry> = Vec::new();
        for step in &steps {
            let source = string_value(step.get("source").unwrap_or(&Value::Null));
            let (role, body, tools) = match source.as_str() {
                "user" => (
                    "user",
                    string_value(step.get("message").unwrap_or(&Value::Null)),
                    Vec::new(),
                ),
                "agent" => (
                    "assistant",
                    string_value(step.get("message").unwrap_or(&Value::Null)),
                    atif_agent_tools(step),
                ),
                _ => continue,
            };
            let body = sanitize_text(&body);
            if body.is_empty() && tools.is_empty() {
                continue;
            }
            let (body, truncated) = clamp_text(&body, MAX_ENTRY_BYTES);
            let mut entry = Entry {
                id: atif_step_id(step),
                timestamp: atif_step_timestamp(step),
                role: role.to_string(),
                text: body,
                tools,
                truncated,
            };
            normalize_entry_tools(&mut entry);
            entries.push(entry);
        }
        let limit = if limit < 1 {
            DEFAULT_PAGE_SIZE
        } else {
            limit.min(MAX_PAGE_SIZE)
        };
        let mut end = entries.len();
        if !before.is_empty() {
            for (index, entry) in entries.iter().enumerate() {
                if entry.id == before {
                    end = index;
                    break;
                }
            }
        }
        let start = end.saturating_sub(limit);
        let page_entries: Vec<_> = entries[start..end].to_vec();
        Ok(Page {
            available: true,
            entries: page_entries,
            has_more: start > 0,
            total: entries.len() as i64,
            source_path: location.path.clone(),
            probe_path: location.path.clone(),
            ..Page::default()
        })
    }
}

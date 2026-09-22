//! Scenario files: the ordered client script (`steps`) plus the `compare`
//! section that steers normalization and bucketing in [`crate::compare`].
//!
//! ```json
//! {
//!   "name": "core",
//!   "vars": {"workspace_id": "wE", "pane_id": "wE:p1"},
//!   "compare": {"unordered_types": ["agents"], "drop_keys": ["at"]},
//!   "steps": [
//!     {"op": "expect", "label": "startup", "match": {"type": "push_config"}},
//!     {"op": "send", "label": "worktree", "request_id": "req-wt",
//!      "frame": {"type": "worktree_list", "protocol": 3,
//!                "request_id": "{request_id}", "workspace_id": "{workspace_id}"},
//!      "until": {"type": "command_result", "request_id": "{request_id}"}}
//!   ]
//! }
//! ```
//!
//! `{name}` placeholders inside frame strings resolve against step scope
//! (`request_id`) then `vars`. A string that is exactly `{name}` substitutes
//! the raw JSON value; a `{name}` embedded in a longer string interpolates
//! the value's string form.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use crate::{Result, ShadowError};

/// `protocol` on every inbound action — v3 is the wire contract.
pub const PROTOCOL_VERSION: i64 = 3;

const DEFAULT_TIMEOUT_MS: u64 = 8_000;
const DEFAULT_QUIESCE_MS: u64 = 350;

#[derive(Debug, Deserialize)]
pub struct Scenario {
    #[serde(default)]
    pub name: String,
    /// `{name}` placeholder values shared by every step.
    #[serde(default)]
    pub vars: Map<String, Value>,
    #[serde(default)]
    pub compare: CompareConfig,
    pub steps: Vec<Step>,
}

impl Scenario {
    pub fn load(path: &Path) -> Result<Self> {
        let raw = std::fs::read_to_string(path)?;
        let mut scenario: Scenario = serde_json::from_str(&raw)
            .map_err(|e| ShadowError::msg(format!("scenario {}: {e}", path.display())))?;
        if scenario.name.is_empty() {
            scenario.name = path
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| "scenario".to_owned());
        }
        Ok(scenario)
    }
}

/// Normalization + bucketing knobs. Serialized into the trace `meta` record
/// so a trace is self-describing when diffed later.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompareConfig {
    /// Frame types excluded from comparison entirely (still traced).
    #[serde(default)]
    pub drop_types: BTreeSet<String>,
    /// Predicate drops: any frame matching a `{type, contains}` clause is
    /// excluded — e.g. Go's empty startup `activity_history`
    /// (`{"type":"activity_history","contains":{"activities":null}}`).
    /// Distinct from `drop_types`: the frame survives when populated.
    #[serde(default)]
    pub drop_matches: Vec<Match>,
    /// Async/publish types whose arrival interleaving is scheduler-dependent.
    /// They leave the positional step buckets and are compared as a sorted
    /// multiset ("async pool").
    #[serde(default)]
    pub unordered_types: BTreeSet<String>,
    /// Object keys dropped at any depth (volatile timestamps/sequences).
    #[serde(default)]
    pub drop_keys: BTreeSet<String>,
    /// Object keys replaced by a fixed marker at any depth (random ids,
    /// keys, absolute paths that legitimately differ per process).
    #[serde(default)]
    pub map_keys: BTreeSet<String>,
    /// Per-type key drops: `type_drop_keys["agents"]` lists keys removed
    /// (recursively) only inside `agents` frames. Use for documented
    /// implementation-specific fields one relay emits and the other does
    /// not — every entry is a declared known delta, visible in the report
    /// header via `notes`.
    #[serde(default)]
    pub type_drop_keys: BTreeMap<String, BTreeSet<String>>,
    /// Compare the async pool as a *set*: identical repeats collapse, so a
    /// re-polled `workspaces` frame does not count as a diff.
    #[serde(default = "default_true")]
    pub async_dedupe: bool,
    /// Free-form notes echoed into the diff report header.
    #[serde(default)]
    pub notes: Vec<String>,
}

impl Default for CompareConfig {
    /// `async_dedupe` defaults on — matches `#[serde(default = "default_true")]`
    /// for the case where the whole `compare` object is absent.
    fn default() -> Self {
        Self {
            drop_types: BTreeSet::new(),
            drop_matches: Vec::new(),
            unordered_types: BTreeSet::new(),
            drop_keys: BTreeSet::new(),
            map_keys: BTreeSet::new(),
            type_drop_keys: BTreeMap::new(),
            async_dedupe: true,
            notes: Vec::new(),
        }
    }
}

fn default_true() -> bool {
    true
}

fn default_timeout() -> u64 {
    DEFAULT_TIMEOUT_MS
}

fn default_quiesce() -> u64 {
    DEFAULT_QUIESCE_MS
}

/// Frame predicate: `type` must equal; `request_id`/`contains` refine it.
/// `contains` is a deep subset — every leaf must appear equal in the frame.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Match {
    pub r#type: String,
    #[serde(default)]
    pub request_id: Option<String>,
    #[serde(default)]
    pub contains: Option<Value>,
}

impl Match {
    pub fn is_match(&self, frame: &Value) -> bool {
        if frame.get("type").and_then(Value::as_str) != Some(self.r#type.as_str()) {
            return false;
        }
        if let Some(request_id) = &self.request_id {
            if frame.get("request_id").and_then(Value::as_str) != Some(request_id.as_str()) {
                return false;
            }
        }
        if let Some(needle) = &self.contains {
            if !deep_contains(frame, needle) {
                return false;
            }
        }
        true
    }

    pub fn describe(&self) -> String {
        let mut out = format!("type={:?}", self.r#type);
        if let Some(id) = &self.request_id {
            out.push_str(&format!(" request_id={id:?}"));
        }
        if let Some(c) = &self.contains {
            out.push_str(&format!(" contains={c}"));
        }
        out
    }

    /// Resolve `{...}` placeholders inside `contains`/`request_id`.
    pub fn render(&self, vars: &Map<String, Value>, scope: &Map<String, Value>) -> Result<Self> {
        Ok(Self {
            r#type: self.r#type.clone(),
            request_id: match &self.request_id {
                Some(id) => Some(render_str(id, vars, scope)?),
                None => None,
            },
            contains: match &self.contains {
                Some(v) => Some(render(v, vars, scope)?),
                None => None,
            },
        })
    }
}

/// One scenario step — see module docs for the shape.
#[derive(Debug, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Step {
    /// Ordering marker only — the differ splits its analysis at the last
    /// fence (startup frames live before it, deterministic traffic after).
    Fence { label: String },
    /// Sleep `ms` — lets async noise settle between steps.
    Settle { label: String, ms: u64 },
    /// Fail unless a frame matching `match` has been observed by deadline.
    Expect {
        label: String,
        r#match: Match,
        #[serde(default = "default_timeout")]
        timeout_ms: u64,
    },
    /// Seal+send `frame`, then collect until `until` matches and the socket
    /// stays quiet for `quiesce_ms` (bounded by `timeout_ms`).
    Send {
        label: String,
        /// Overrides the auto `req-<index>` request id; also bound to the
        /// `{request_id}` placeholder.
        #[serde(default)]
        request_id: Option<String>,
        frame: Value,
        /// Frame types that belong to this step's bucket even though their
        /// type is listed in `compare.unordered_types` (e.g. the `push_policy`
        /// answer to `push_policy_get`).
        #[serde(default)]
        capture: Vec<String>,
        #[serde(default)]
        until: Option<Match>,
        #[serde(default = "default_timeout")]
        timeout_ms: u64,
        #[serde(default = "default_quiesce")]
        quiesce_ms: u64,
    },
    /// Collect without sending — drains a wake-triggered publish burst.
    Collect {
        label: String,
        #[serde(default)]
        capture: Vec<String>,
        #[serde(default)]
        until: Option<Match>,
        #[serde(default = "default_timeout")]
        timeout_ms: u64,
        #[serde(default = "default_quiesce")]
        quiesce_ms: u64,
    },
}

impl Step {
    pub fn label(&self) -> &str {
        match self {
            Step::Fence { label }
            | Step::Settle { label, .. }
            | Step::Expect { label, .. }
            | Step::Send { label, .. }
            | Step::Collect { label, .. } => label,
        }
    }

    pub fn op_name(&self) -> &'static str {
        match self {
            Step::Fence { .. } => "fence",
            Step::Settle { .. } => "settle",
            Step::Expect { .. } => "expect",
            Step::Send { .. } => "send",
            Step::Collect { .. } => "collect",
        }
    }

    /// The request id this step sends under (`req-<index>` when unset).
    pub fn request_id(&self, index: usize) -> Option<String> {
        match self {
            Step::Send { request_id, .. } => {
                Some(request_id.clone().unwrap_or_else(|| format!("req-{index}")))
            }
            _ => None,
        }
    }

    pub fn capture(&self) -> &[String] {
        match self {
            Step::Send { capture, .. } | Step::Collect { capture, .. } => capture,
            _ => &[],
        }
    }
}

/// `contains ⊆ frame` — object keys must all match recursively, array items
/// must each be contained by some element, scalars compare equal.
pub fn deep_contains(haystack: &Value, needle: &Value) -> bool {
    match (haystack, needle) {
        (Value::Object(h), Value::Object(n)) => n
            .iter()
            .all(|(k, nv)| h.get(k).is_some_and(|hv| deep_contains(hv, nv))),
        (Value::Array(h), Value::Array(n)) => {
            n.iter().all(|nv| h.iter().any(|hv| deep_contains(hv, nv)))
        }
        (h, n) => h == n,
    }
}

/// Render `{name}` placeholders. `scope` (per-step, e.g. `request_id`) wins
/// over `vars`.
pub fn render(
    template: &Value,
    vars: &Map<String, Value>,
    scope: &Map<String, Value>,
) -> Result<Value> {
    match template {
        Value::String(s) => Ok(render_string(s, vars, scope)?),
        Value::Array(items) => Ok(Value::Array(
            items
                .iter()
                .map(|item| render(item, vars, scope))
                .collect::<Result<Vec<_>>>()?,
        )),
        Value::Object(map) => Ok(Value::Object(
            map.iter()
                .map(|(k, v)| Ok((k.clone(), render(v, vars, scope)?)))
                .collect::<Result<Map<String, Value>>>()?,
        )),
        other => Ok(other.clone()),
    }
}

fn render_string(s: &str, vars: &Map<String, Value>, scope: &Map<String, Value>) -> Result<Value> {
    // Whole-string placeholder: substitute the raw JSON value.
    if let Some(name) = s
        .strip_prefix('{')
        .and_then(|inner| inner.strip_suffix('}'))
        .filter(|name| !name.contains('{') && !name.is_empty())
    {
        if let Some(value) = lookup(name, vars, scope) {
            return Ok(value.clone());
        }
    }
    if !s.contains('{') {
        return Ok(Value::String(s.to_owned()));
    }
    Ok(Value::String(render_str(s, vars, scope)?))
}

/// String interpolation form — `{name}` inside a longer string. The
/// referenced value must be a string or number.
pub fn render_str(
    s: &str,
    vars: &Map<String, Value>,
    scope: &Map<String, Value>,
) -> Result<String> {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        let Some(close) = after.find('}') else {
            return Err(ShadowError::msg(format!("unclosed placeholder in {s:?}")));
        };
        let name = &after[..close];
        let value = lookup(name, vars, scope)
            .ok_or_else(|| ShadowError::msg(format!("unknown placeholder {{{name}}} in {s:?}")))?;
        match value {
            Value::String(v) => out.push_str(v),
            Value::Number(n) => out.push_str(&n.to_string()),
            Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
            other => {
                return Err(ShadowError::msg(format!(
                    "placeholder {{{name}}} is {other}, not interpolatable"
                )))
            }
        }
        rest = &after[close + 1..];
    }
    out.push_str(rest);
    Ok(out)
}

fn lookup<'a>(
    name: &str,
    vars: &'a Map<String, Value>,
    scope: &'a Map<String, Value>,
) -> Option<&'a Value> {
    scope.get(name).or_else(|| vars.get(name))
}

/// `{request_id}` binds the step's effective request id when rendering the
/// frame/`until` of a `send` step.
pub fn step_scope(request_id: Option<&str>) -> Map<String, Value> {
    let mut scope = Map::new();
    if let Some(id) = request_id {
        scope.insert("request_id".to_owned(), Value::String(id.to_owned()));
    }
    scope
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn whole_string_placeholder_substitutes_raw_value() {
        let vars = Map::from_iter([("ws".into(), json!("wE")), ("n".into(), json!(3))]);
        let scope = Map::new();
        assert_eq!(render(&json!("{ws}"), &vars, &scope).unwrap(), json!("wE"));
        assert_eq!(render(&json!("{n}"), &vars, &scope).unwrap(), json!(3));
    }

    #[test]
    fn embedded_placeholder_interpolates() {
        let vars = Map::from_iter([("ws".into(), json!("wE"))]);
        let scope = Map::new();
        assert_eq!(
            render(&json!("pane-{ws}"), &vars, &scope).unwrap(),
            json!("pane-wE")
        );
    }

    #[test]
    fn scope_beats_vars() {
        let vars = Map::from_iter([("request_id".into(), json!("vars"))]);
        let scope = step_scope(Some("scope"));
        assert_eq!(
            render(&json!("{request_id}"), &vars, &scope).unwrap(),
            json!("scope")
        );
    }

    #[test]
    fn unknown_placeholder_is_an_error() {
        assert!(render_str("{missing}", &Map::new(), &Map::new()).is_err());
    }

    #[test]
    fn deep_contains_subset() {
        let frame = json!({"type": "agents", "agents": [{"pane_id": "p1", "agent": "claude"}]});
        assert!(deep_contains(
            &frame,
            &json!({"agents": [{"pane_id": "p1"}]})
        ));
        assert!(!deep_contains(
            &frame,
            &json!({"agents": [{"pane_id": "p2"}]})
        ));
    }

    #[test]
    fn match_type_request_id_contains() {
        let m = Match {
            r#type: "command_result".into(),
            request_id: Some("r1".into()),
            contains: Some(json!({"phase": "completed"})),
        };
        assert!(m.is_match(
            &json!({"type": "command_result", "request_id": "r1", "phase": "completed", "extra": 1})
        ));
        assert!(!m
            .is_match(&json!({"type": "command_result", "request_id": "r1", "phase": "prepared"})));
        assert!(!m.is_match(
            &json!({"type": "command_result", "request_id": "r2", "phase": "completed"})
        ));
    }

    #[test]
    fn scenario_parses_ops() {
        let scenario: Scenario = serde_json::from_value(json!({
            "name": "t",
            "steps": [
                {"op": "fence", "label": "end"},
                {"op": "settle", "label": "s", "ms": 10},
                {"op": "expect", "label": "e", "match": {"type": "push_config"}},
                {"op": "send", "label": "w", "frame": {"type": "worktree_list"},
                 "until": {"type": "command_result"}, "capture": ["worktrees"]},
                {"op": "collect", "label": "c"}
            ]
        }))
        .unwrap();
        assert_eq!(scenario.steps.len(), 5);
        assert_eq!(scenario.steps[3].request_id(3).as_deref(), Some("req-3"));
        assert!(scenario.compare.async_dedupe);
    }
}

//! Serde types for the Herdr socket API (protocol 22, schema_version 1,
//! verified against `herdr api schema --json` on herdr 0.9.1).
//!
//! Response structs follow the upstream JSON Schema; optional/nullable wire
//! fields are `Option<T>` with `#[serde(default)]` so newer Herdr builds can
//! add fields without breaking decodes. Wire enums that can grow upstream
//! carry an `Unrecognized` catch-all so a new variant degrades to data instead
//! of a decode error.

use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::{Map, Value};

/// Agent lifecycle status as classified by Herdr's detection rules.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentStatus {
    #[default]
    Unknown,
    Idle,
    Working,
    Blocked,
    Done,
    /// A status this build does not know — kept as data so topology
    /// projections survive a Herdr upgrade.
    #[serde(other)]
    Unrecognized,
}

impl fmt::Display for AgentStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            AgentStatus::Idle => "idle",
            AgentStatus::Working => "working",
            AgentStatus::Blocked => "blocked",
            AgentStatus::Done => "done",
            AgentStatus::Unknown | AgentStatus::Unrecognized => "unknown",
        };
        f.write_str(s)
    }
}

/// How an agent session reference is stored.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentSessionRefKind {
    #[default]
    Id,
    Path,
    #[serde(other)]
    Unrecognized,
}

/// Agent session attachment reported on panes/agents.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct AgentSessionInfo {
    pub source: String,
    pub agent: String,
    pub kind: AgentSessionRefKind,
    pub value: String,
}

/// Terminal scroll metrics (`scroll` on `PaneInfo`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaneScrollInfo {
    pub offset_from_bottom: u64,
    pub max_offset_from_bottom: u64,
    pub viewport_rows: u64,
}

/// Worktree provenance on a workspace record.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct WorkspaceWorktreeInfo {
    pub repo_key: String,
    pub repo_name: String,
    pub repo_root: String,
    pub checkout_path: String,
    pub is_linked_worktree: bool,
}

/// `workspace.list` / `session.snapshot` workspace record.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct WorkspaceInfo {
    pub workspace_id: String,
    pub number: u32,
    pub label: String,
    pub focused: bool,
    pub pane_count: u32,
    pub tab_count: u32,
    pub active_tab_id: String,
    pub agent_status: AgentStatus,
    #[serde(default)]
    pub tokens: BTreeMap<String, String>,
    #[serde(default)]
    pub worktree: Option<WorkspaceWorktreeInfo>,
}

/// `tab.list` / `session.snapshot` tab record.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct TabInfo {
    pub tab_id: String,
    pub workspace_id: String,
    pub number: u32,
    pub label: String,
    pub focused: bool,
    pub pane_count: u32,
    pub agent_status: AgentStatus,
}

/// `pane.list` / `session.snapshot` pane record.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PaneInfo {
    pub pane_id: String,
    pub terminal_id: String,
    pub workspace_id: String,
    pub tab_id: String,
    pub focused: bool,
    pub agent_status: AgentStatus,
    #[serde(default)]
    pub agent: Option<String>,
    #[serde(default)]
    pub display_agent: Option<String>,
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub foreground_cwd: Option<String>,
    #[serde(default)]
    pub revision: u64,
    #[serde(default)]
    pub agent_session: Option<AgentSessionInfo>,
    #[serde(default)]
    pub scroll: Option<PaneScrollInfo>,
    #[serde(default)]
    pub terminal_title: Option<String>,
    #[serde(default)]
    pub terminal_title_stripped: Option<String>,
    #[serde(default)]
    pub tokens: BTreeMap<String, String>,
    #[serde(default)]
    pub state_labels: BTreeMap<String, String>,
}

/// `agent.list` / `session.snapshot` agent record — the agent's pane plus
/// detection metadata.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct AgentInfo {
    pub pane_id: String,
    pub terminal_id: String,
    pub workspace_id: String,
    pub tab_id: String,
    pub focused: bool,
    pub agent_status: AgentStatus,
    #[serde(default)]
    pub agent: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub display_agent: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub foreground_cwd: Option<String>,
    #[serde(default)]
    pub revision: u64,
    #[serde(default)]
    pub state_change_seq: u64,
    #[serde(default)]
    pub interactive_ready: bool,
    #[serde(default)]
    pub launch_pending: bool,
    #[serde(default)]
    pub screen_detection_skipped: bool,
    #[serde(default)]
    pub agent_session: Option<AgentSessionInfo>,
    #[serde(default)]
    pub terminal_title: Option<String>,
    #[serde(default)]
    pub terminal_title_stripped: Option<String>,
    #[serde(default)]
    pub tokens: BTreeMap<String, String>,
    #[serde(default)]
    pub state_labels: BTreeMap<String, String>,
}

/// Rectangle in terminal cells.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaneLayoutRect {
    pub x: u16,
    pub y: u16,
    pub width: u16,
    pub height: u16,
}

/// One pane inside a [`PaneLayoutSnapshot`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaneLayoutPane {
    pub pane_id: String,
    pub focused: bool,
    pub rect: PaneLayoutRect,
}

/// One split inside a [`PaneLayoutSnapshot`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PaneLayoutSplit {
    pub id: String,
    pub direction: SplitDirection,
    pub ratio: f64,
    pub rect: PaneLayoutRect,
}

/// Per-tab layout inside `session.snapshot` (`layouts[]`) and the payload of
/// `layout.updated` events.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PaneLayoutSnapshot {
    pub workspace_id: String,
    pub tab_id: String,
    pub zoomed: bool,
    pub area: PaneLayoutRect,
    pub focused_pane_id: String,
    #[serde(default)]
    pub panes: Vec<PaneLayoutPane>,
    #[serde(default)]
    pub splits: Vec<PaneLayoutSplit>,
}

/// `session.snapshot` result — the one-call full topology reconcile used as
/// the bootstrap base and the `events_lost` recovery base.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SessionSnapshot {
    pub version: String,
    pub protocol: u32,
    #[serde(default)]
    pub workspaces: Vec<WorkspaceInfo>,
    #[serde(default)]
    pub tabs: Vec<TabInfo>,
    #[serde(default)]
    pub panes: Vec<PaneInfo>,
    #[serde(default)]
    pub layouts: Vec<PaneLayoutSnapshot>,
    #[serde(default)]
    pub agents: Vec<AgentInfo>,
    #[serde(default)]
    pub focused_workspace_id: Option<String>,
    #[serde(default)]
    pub focused_tab_id: Option<String>,
    #[serde(default)]
    pub focused_pane_id: Option<String>,
}

/// Which snapshot of a pane to read (`pane.read`, `pane.wait_for_output`,
/// `pane.output_matched` subscriptions).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReadSource {
    /// Currently rendered viewport.
    Visible,
    /// Recent rendered output including soft wraps.
    Recent,
    /// Recent output with soft wraps joined — the transcript source.
    #[default]
    RecentUnwrapped,
    /// Plain-text bottom-buffer snapshot used by agent detection.
    Detection,
    #[serde(other)]
    Unrecognized,
}

impl ReadSource {
    /// Accept the CLI's kebab-case spellings (`recent-unwrapped`).
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "visible" => Some(ReadSource::Visible),
            "recent" => Some(ReadSource::Recent),
            "recent_unwrapped" | "recent-unwrapped" => Some(ReadSource::RecentUnwrapped),
            "detection" => Some(ReadSource::Detection),
            _ => None,
        }
    }
}

/// `pane.read` output encoding.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReadFormat {
    /// Plain text with ANSI sequences stripped.
    #[default]
    Text,
    /// Raw output including ANSI styling.
    Ansi,
    #[serde(other)]
    Unrecognized,
}

/// `pane.read` result payload.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PaneReadResult {
    pub pane_id: String,
    pub workspace_id: String,
    pub tab_id: String,
    pub source: ReadSource,
    pub format: ReadFormat,
    pub text: String,
    pub revision: u64,
    pub truncated: bool,
}

/// `pane.wait_for_output` result (`output_matched`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct OutputMatchedResult {
    pub pane_id: String,
    pub revision: u64,
    pub read: PaneReadResult,
    #[serde(default)]
    pub matched_line: Option<String>,
}

/// `ping` result.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Pong {
    pub version: String,
    pub protocol: u32,
    #[serde(default)]
    pub capabilities: Option<ServerCapabilities>,
}

/// Optional capabilities the server advertises in `pong`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ServerCapabilities {
    pub live_handoff: bool,
    #[serde(default)]
    pub detached_server_daemon: bool,
    #[serde(default)]
    pub endpoint_protocol_generation: Option<u32>,
    #[serde(default)]
    pub surface_interest: bool,
    #[serde(default)]
    pub health_check: bool,
}

/// `agent.view.set` / `agent.view.clear` result.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct AgentViewState {
    pub active: bool,
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub source: Option<String>,
}

/// `notification.show` disposition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NotificationShowReason {
    Shown,
    Disabled,
    RateLimited,
    NoForegroundClient,
    Busy,
    #[serde(other)]
    Unrecognized,
}

/// `notification.show` result.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NotificationOutcome {
    pub shown: bool,
    pub reason: NotificationShowReason,
}

/// `layout.apply` / `layout.export` result — the applied layout tree.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct LayoutDescription {
    pub workspace_id: String,
    pub tab_id: String,
    pub zoomed: bool,
    pub focused_pane_id: String,
    pub root: LayoutNode,
}

/// A layout tree node for `layout.apply`/`layout.export`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum LayoutNode {
    Pane {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pane_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        label: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cwd: Option<String>,
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        env: BTreeMap<String, String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        command: Option<Vec<String>>,
    },
    Split {
        direction: SplitDirection,
        ratio: f64,
        first: Box<LayoutNode>,
        second: Box<LayoutNode>,
    },
}

impl Default for LayoutNode {
    fn default() -> Self {
        LayoutNode::Pane {
            pane_id: None,
            label: None,
            cwd: None,
            env: BTreeMap::new(),
            command: None,
        }
    }
}

/// Split axis for [`LayoutNode::Split`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SplitDirection {
    Right,
    Down,
    #[serde(other)]
    Unrecognized,
}

/// `plugin.action.invoke` result (`plugin_action_invoked`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PluginActionInvocation {
    pub action: PluginActionInfo,
    pub context: PluginInvocationContext,
    pub log: PluginCommandLogInfo,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PluginActionInfo {
    pub plugin_id: String,
    pub action_id: String,
    pub title: String,
    #[serde(default)]
    pub command: Vec<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub contexts: Vec<String>,
    #[serde(default)]
    pub platforms: Option<Vec<String>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PluginCommandStatus {
    Running,
    Succeeded,
    Failed,
    Cancelled,
    #[serde(other)]
    Unrecognized,
}

/// One entry from the plugin command log.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PluginCommandLogInfo {
    pub log_id: String,
    pub plugin_id: String,
    #[serde(default)]
    pub action_id: Option<String>,
    #[serde(default)]
    pub event: Option<String>,
    #[serde(default)]
    pub command: Vec<String>,
    #[serde(default)]
    pub status: Option<PluginCommandStatus>,
    #[serde(default)]
    pub exit_code: Option<i32>,
    #[serde(default)]
    pub stdout: Option<String>,
    #[serde(default)]
    pub stderr: Option<String>,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub started_unix_ms: Option<u64>,
    #[serde(default)]
    pub finished_unix_ms: Option<u64>,
}

// ---------------------------------------------------------------------------
// Request params
// ---------------------------------------------------------------------------

/// `pane.read` params. `strip_ansi` defaults the way the Go client derives
/// it: `format != ansi`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PaneReadParams {
    pub pane_id: String,
    pub source: ReadSource,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lines: Option<u32>,
    pub format: ReadFormat,
    pub strip_ansi: bool,
}

impl PaneReadParams {
    pub fn new(
        pane_id: impl Into<String>,
        source: ReadSource,
        lines: u32,
        format: ReadFormat,
    ) -> Self {
        PaneReadParams {
            pane_id: pane_id.into(),
            source,
            lines: Some(lines.max(1)),
            format,
            strip_ansi: format != ReadFormat::Ansi,
        }
    }
}

/// Server-side output matcher (`pane.wait_for_output`,
/// `pane.output_matched` subscription).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum OutputMatch {
    Substring { value: String },
    Regex { value: String },
}

impl OutputMatch {
    pub fn substring(value: impl Into<String>) -> Self {
        OutputMatch::Substring {
            value: value.into(),
        }
    }

    pub fn regex(value: impl Into<String>) -> Self {
        OutputMatch::Regex {
            value: value.into(),
        }
    }
}

/// `pane.wait_for_output` params.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PaneWaitForOutputParams {
    pub pane_id: String,
    pub source: ReadSource,
    #[serde(rename = "match")]
    pub match_: OutputMatch,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub strip_ansi: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lines: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
}

/// `agent.wait` params.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AgentWaitParams {
    pub target: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub until: Vec<AgentStatus>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
}

/// `events.wait` match clause. Event names on this wire use the legacy
/// snake_case spellings (`pane_agent_status_changed`) — [`EventMatch::named`]
/// accepts canonical dotted names and converts.
///
/// The schema defines per-event required fields; rather than a closed enum
/// this is `event` + a filter map, with typed constructors for the matches
/// the relay uses.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct EventMatch {
    pub event: String,
    #[serde(flatten)]
    pub filters: Map<String, Value>,
}

impl EventMatch {
    /// Build a match for a raw (snake_case) or canonical (dotted) event name
    /// with the given extra filter fields.
    pub fn new(event: impl Into<String>, filters: Map<String, Value>) -> Self {
        EventMatch {
            event: crate::events::wire_event_name(&event.into()).to_owned(),
            filters,
        }
    }

    /// `pane_agent_status_changed` — currently the only match kind the
    /// server implements (`unsupported_event_wait_match` otherwise).
    pub fn pane_agent_status_changed(pane_id: impl Into<String>, status: AgentStatus) -> Self {
        let mut filters = Map::new();
        filters.insert("pane_id".into(), Value::String(pane_id.into()));
        filters.insert(
            "agent_status".into(),
            serde_json::to_value(status).unwrap_or(Value::Null),
        );
        EventMatch {
            event: "pane_agent_status_changed".into(),
            filters,
        }
    }

    /// `pane_output_changed` — match output revisions at or past
    /// `min_revision`.
    pub fn pane_output_changed(pane_id: impl Into<String>, min_revision: Option<u64>) -> Self {
        let mut filters = Map::new();
        filters.insert("pane_id".into(), Value::String(pane_id.into()));
        if let Some(rev) = min_revision {
            filters.insert("min_revision".into(), Value::from(rev));
        }
        EventMatch {
            event: "pane_output_changed".into(),
            filters,
        }
    }

    /// `pane_created` (optional `pane_id`/`workspace_id` filters).
    pub fn pane_created() -> Self {
        EventMatch {
            event: "pane_created".into(),
            filters: Map::new(),
        }
    }
}

/// `pane.send_input` params — routed through Herdr's input path so
/// paste-mode is honored.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct PaneSendInputParams {
    pub pane_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub keys: Vec<String>,
}

/// `notification.show` params.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct NotificationShowParams {
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub position: Option<ToastPosition>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sound: Option<NotificationSound>,
}

/// Toast corner placement.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ToastPosition {
    #[serde(rename = "top-left")]
    TopLeft,
    #[serde(rename = "top-right")]
    TopRight,
    #[serde(rename = "bottom-left")]
    BottomLeft,
    #[serde(rename = "bottom-right")]
    BottomRight,
    #[serde(other)]
    Unrecognized,
}

/// `notification.show` sound policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NotificationSound {
    None,
    Done,
    Request,
    #[serde(other)]
    Unrecognized,
}

/// `layout.apply` params.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LayoutApplyParams {
    pub root: LayoutNode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tab_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tab_label: Option<String>,
    #[serde(default)]
    pub focus: bool,
}

/// `command.invoke` params — endpoint-issued command ids validated against
/// the pane's content revision.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CommandInvokeParams {
    pub command_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pane_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tab_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selection: Option<PaneSelection>,
}

/// Client-owned selection coordinates for [`CommandInvokeParams`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaneSelection {
    pub pane_id: String,
    pub anchor: PaneTextPoint,
    pub cursor: PaneTextPoint,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_revision: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaneTextPoint {
    pub row: u32,
    pub col: u16,
}

/// `plugin.action.invoke` params.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PluginActionInvokeParams {
    pub action_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plugin_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<PluginInvocationContext>,
}

/// Invocation context for plugin actions — the server fills missing fields
/// from the active workspace/tab/focused pane.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PluginInvocationContext {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub invocation_source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub correlation_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_cwd: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tab_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tab_label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub focused_pane_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub focused_pane_status: Option<AgentStatus>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub focused_pane_agent: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub focused_pane_cwd: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selected_text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clicked_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link_handler_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree: Option<WorkspaceWorktreeInfo>,
}

/// `agent.view.set` params — the transient declarative filter+sort projection
/// that drives Herdr's sidebar and mobile Agents list.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AgentViewSetParams {
    /// Owner identity. Plugins use `plugin:<HERDR_PLUGIN_ID>`; the relay uses
    /// `plugin:lerdr.events` for the canonical attention-sorted view.
    pub source: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filter: Option<AgentViewFilter>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sort: Vec<AgentViewSort>,
}

/// Filter node for [`AgentViewSetParams`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum AgentViewFilter {
    All {
        filters: Vec<AgentViewFilter>,
    },
    Any {
        filters: Vec<AgentViewFilter>,
    },
    Not {
        filter: Box<AgentViewFilter>,
    },
    Eq {
        field: AgentViewField,
        value: AgentViewValue,
    },
    In {
        field: AgentViewField,
        values: Vec<AgentViewValue>,
    },
    Exists {
        field: AgentViewField,
    },
}

/// A filterable field: a builtin name or `{"token":"<name>"}` for
/// plugin-reported pane metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentViewField {
    Builtin(BuiltinViewField),
    Token(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BuiltinViewField {
    Status,
    WorkspaceId,
    TabId,
    PaneId,
    Agent,
    Seen,
    StateChangeSeq,
    #[serde(other)]
    Unrecognized,
}

impl Serialize for AgentViewField {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            AgentViewField::Builtin(field) => field.serialize(serializer),
            AgentViewField::Token(token) => {
                #[derive(Serialize)]
                struct TokenRef<'a> {
                    token: &'a str,
                }
                TokenRef { token }.serialize(serializer)
            }
        }
    }
}

impl<'de> Deserialize<'de> for AgentViewField {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = Value::deserialize(deserializer)?;
        match value {
            Value::String(name) => BuiltinViewField::deserialize(Value::String(name))
                .map(AgentViewField::Builtin)
                .map_err(serde::de::Error::custom),
            Value::Object(mut obj) => match obj.remove("token") {
                Some(Value::String(token)) => Ok(AgentViewField::Token(token)),
                _ => Err(serde::de::Error::custom(
                    "agent view field object requires `token`",
                )),
            },
            _ => Err(serde::de::Error::custom(
                "agent view field must be a string or {\"token\":…}",
            )),
        }
    }
}

/// A comparable value in an agent view filter: string, bool, u64, or a UI
/// context reference (`current_workspace_id` / `current_tab_id`).
#[derive(Debug, Clone, PartialEq)]
pub enum AgentViewValue {
    Text(String),
    Bool(bool),
    Number(u64),
    Context(ViewContext),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ViewContext {
    CurrentWorkspaceId,
    CurrentTabId,
    #[serde(other)]
    Unrecognized,
}

impl Serialize for AgentViewValue {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            AgentViewValue::Text(s) => s.serialize(serializer),
            AgentViewValue::Bool(b) => b.serialize(serializer),
            AgentViewValue::Number(n) => n.serialize(serializer),
            AgentViewValue::Context(ctx) => {
                #[derive(Serialize)]
                struct CtxRef {
                    context: ViewContext,
                }
                CtxRef { context: *ctx }.serialize(serializer)
            }
        }
    }
}

impl<'de> Deserialize<'de> for AgentViewValue {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        match Value::deserialize(deserializer)? {
            Value::String(s) => Ok(AgentViewValue::Text(s)),
            Value::Bool(b) => Ok(AgentViewValue::Bool(b)),
            Value::Number(n) if n.as_u64().is_some() => {
                Ok(AgentViewValue::Number(n.as_u64().unwrap_or(0)))
            }
            Value::Object(mut obj) => match obj.remove("context") {
                Some(v) => ViewContext::deserialize(v)
                    .map(AgentViewValue::Context)
                    .map_err(serde::de::Error::custom),
                None => Err(serde::de::Error::custom(
                    "agent view value object requires `context`",
                )),
            },
            _ => Err(serde::de::Error::custom("invalid agent view value")),
        }
    }
}

/// One sort clause for [`AgentViewSetParams`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentViewSort {
    pub field: AgentViewSortField,
    #[serde(default)]
    pub order: AgentViewSortOrder,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentViewSortField {
    Builtin(BuiltinViewSortField),
    Token(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BuiltinViewSortField {
    WorkspaceOrder,
    TabOrder,
    PaneOrder,
    Attention,
    Status,
    Agent,
    Seen,
    StateChangeSeq,
    #[serde(other)]
    Unrecognized,
}

impl Serialize for AgentViewSortField {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            AgentViewSortField::Builtin(field) => field.serialize(serializer),
            AgentViewSortField::Token(token) => {
                #[derive(Serialize)]
                struct TokenRef<'a> {
                    token: &'a str,
                }
                TokenRef { token }.serialize(serializer)
            }
        }
    }
}

impl<'de> Deserialize<'de> for AgentViewSortField {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        AgentViewField::deserialize(deserializer).map(|field| match field {
            AgentViewField::Builtin(b) => AgentViewSortField::Builtin(match b {
                BuiltinViewField::Status => BuiltinViewSortField::Status,
                BuiltinViewField::Agent => BuiltinViewSortField::Agent,
                BuiltinViewField::Seen => BuiltinViewSortField::Seen,
                BuiltinViewField::StateChangeSeq => BuiltinViewSortField::StateChangeSeq,
                _ => BuiltinViewSortField::Unrecognized,
            }),
            AgentViewField::Token(t) => AgentViewSortField::Token(t),
        })
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentViewSortOrder {
    #[default]
    Asc,
    Desc,
    #[serde(other)]
    Unrecognized,
}

//! Server -> client messages.
//!
//! Most Go emit sites build `map[string]any`, which `json.Marshal` emits with
//! **sorted keys** — the structs below declare fields in that sorted order so
//! a decode + encode round-trips byte-exact. The exceptions are struct-built
//! payloads (`push_config`, `AgentState`, `activity.Entry`, `Workspace`,
//! `question.Interaction`, `HerdrStatus`), which keep their Go struct order.
//!
//! Presence model per field:
//! - `Option<T>` + `skip_serializing_if` — key absent or `null` -> omitted.
//! - `Option<MaybeNull<T>>` — absent -> omitted, `null` -> emitted `null`,
//!   value -> value. Required because Go maps emit explicit nulls
//!   (`"interaction":null`, `"options":null`).
//! - `MaybeNull<T>` + `#[serde(default)]` — Go `any`/slice fields without
//!   `omitempty`: absent or `null` -> emits `null`.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::inbound::TargetRef;
use super::types::{ActionReceipt, ActionReceiptPhase, ApiError, VERSION};
use crate::delta::Segment;
use crate::json::{
    de_default, de_nullable, opt_map_is_empty, opt_vec_is_empty, MaybeNull, RawJson,
};

// ---------------------------------------------------------------------------
// Structured payloads (Go struct field order).
// ---------------------------------------------------------------------------

/// `question.Option` — one selectable option in a structured question.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct QuestionOption {
    #[serde(default, deserialize_with = "de_default")]
    pub index: i64,
    #[serde(default, deserialize_with = "de_default")]
    pub label: String,
    #[serde(default, deserialize_with = "de_default")]
    pub description: String,
    #[serde(default, deserialize_with = "de_default")]
    pub selected: bool,
    #[serde(default, skip_serializing_if = "opt_vec_is_empty")]
    pub summary: Option<Vec<SummaryEntry>>,
}

/// `question.SummaryEntry`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SummaryEntry {
    #[serde(default, deserialize_with = "de_default")]
    pub q: String,
    #[serde(default, deserialize_with = "de_default")]
    pub a: String,
}

/// `question.Other` — the free-text "other" choice.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct QuestionOther {
    #[serde(default, deserialize_with = "de_default")]
    pub selected: bool,
    #[serde(default, deserialize_with = "de_default")]
    pub text: String,
    #[serde(
        default,
        deserialize_with = "de_default",
        skip_serializing_if = "String::is_empty"
    )]
    pub label: String,
    #[serde(
        default,
        deserialize_with = "de_default",
        skip_serializing_if = "String::is_empty"
    )]
    pub placeholder: String,
    #[serde(
        default,
        deserialize_with = "de_default",
        skip_serializing_if = "std::ops::Not::not"
    )]
    pub allow_empty: bool,
    #[serde(
        default,
        deserialize_with = "de_default",
        skip_serializing_if = "std::ops::Not::not"
    )]
    pub hidden: bool,
}

/// `question.Interaction` — structured input request embedded in
/// `pane_content`/`blocked`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct QuestionInteraction {
    #[serde(default, deserialize_with = "de_default")]
    pub id: String,
    #[serde(default, deserialize_with = "de_default")]
    pub kind: String,
    #[serde(default, deserialize_with = "de_default")]
    pub question: String,
    /// `[]Option` without `omitempty`: nil -> `null`, not absent.
    #[serde(default)]
    pub options: MaybeNull<Vec<QuestionOption>>,
    #[serde(default, deserialize_with = "de_default")]
    pub other: QuestionOther,
    #[serde(default, deserialize_with = "de_default")]
    pub submit_label: String,
    #[serde(default, deserialize_with = "de_default")]
    pub can_chat: bool,
    #[serde(default, deserialize_with = "de_default")]
    pub can_go_back: bool,
    #[serde(
        default,
        deserialize_with = "de_default",
        skip_serializing_if = "is_zero"
    )]
    pub question_index: i64,
    #[serde(
        default,
        deserialize_with = "de_default",
        skip_serializing_if = "is_zero"
    )]
    pub question_total: i64,
}

fn is_zero(v: &i64) -> bool {
    *v == 0
}

/// `coordinator.AgentState` — one entry of the `agents` snapshot.
/// `event_id`..`question_layout` are attention fields, emitted only when set.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct AgentState {
    #[serde(default, deserialize_with = "de_default")]
    pub pane_id: String,
    #[serde(default, deserialize_with = "de_default")]
    pub raw_pane_id: String,
    #[serde(default, deserialize_with = "de_default")]
    pub terminal_id: String,
    #[serde(
        default,
        deserialize_with = "de_default",
        skip_serializing_if = "String::is_empty"
    )]
    pub server_session_id: String,
    #[serde(default, deserialize_with = "de_default")]
    pub generation: i64,
    #[serde(
        default,
        deserialize_with = "de_default",
        skip_serializing_if = "String::is_empty"
    )]
    pub agent_session_id: String,
    #[serde(default, deserialize_with = "de_default")]
    pub tab_id: String,
    #[serde(default, deserialize_with = "de_default")]
    pub tab_label: String,
    #[serde(default, deserialize_with = "de_default")]
    pub tab_number: i64,
    #[serde(
        default,
        deserialize_with = "de_default",
        skip_serializing_if = "is_zero"
    )]
    pub tab_order: i64,
    #[serde(default, deserialize_with = "de_default")]
    pub workspace_id: String,
    #[serde(default, deserialize_with = "de_default")]
    pub agent: String,
    #[serde(default, deserialize_with = "de_default")]
    pub name: String,
    #[serde(default, deserialize_with = "de_default")]
    pub status: String,
    /// `_focused` is always emitted (no `omitempty` in Go).
    #[serde(rename = "_focused", default, deserialize_with = "de_default")]
    pub focused: bool,
    #[serde(default, deserialize_with = "de_default")]
    pub cwd: String,
    #[serde(default, deserialize_with = "de_default")]
    pub project: String,
    #[serde(default, deserialize_with = "de_default")]
    pub host: String,
    #[serde(default, deserialize_with = "de_default")]
    pub session: String,
    #[serde(default, deserialize_with = "de_default")]
    pub session_name: String,
    #[serde(default, deserialize_with = "de_default")]
    pub updated_at: i64,
    #[serde(
        default,
        deserialize_with = "de_default",
        skip_serializing_if = "is_zero"
    )]
    pub last_active_at: i64,
    #[serde(
        default,
        deserialize_with = "de_default",
        skip_serializing_if = "is_zero"
    )]
    pub last_seen_at: i64,
    #[serde(
        default,
        deserialize_with = "de_default",
        skip_serializing_if = "is_zero"
    )]
    pub activity_seq: i64,
    #[serde(
        default,
        deserialize_with = "de_default",
        skip_serializing_if = "String::is_empty"
    )]
    pub event_id: String,
    #[serde(
        default,
        deserialize_with = "de_default",
        skip_serializing_if = "String::is_empty"
    )]
    pub attention_kind: String,
    #[serde(
        default,
        deserialize_with = "de_default",
        skip_serializing_if = "String::is_empty"
    )]
    pub prompt: String,
    #[serde(
        default,
        deserialize_with = "de_default",
        skip_serializing_if = "String::is_empty"
    )]
    pub command: String,
    #[serde(
        default,
        deserialize_with = "de_default",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub options: Vec<String>,
    #[serde(
        default,
        deserialize_with = "de_default",
        skip_serializing_if = "String::is_empty"
    )]
    pub approval_fingerprint: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interaction: Option<QuestionInteraction>,
    #[serde(
        default,
        deserialize_with = "de_default",
        skip_serializing_if = "std::ops::Not::not"
    )]
    pub question_layout: bool,
    #[serde(
        default,
        deserialize_with = "de_default",
        skip_serializing_if = "std::ops::Not::not"
    )]
    pub conversation_history_available: bool,
    /// Go field `StateRevision`, tagged `pane_revision`.
    #[serde(
        default,
        deserialize_with = "de_default",
        skip_serializing_if = "is_zero"
    )]
    pub pane_revision: i64,
    /// Herdr-reported display metadata (`pane.report_metadata`) — named
    /// state labels and tokens; omitted until a hook reports them.
    #[serde(
        default,
        deserialize_with = "de_default",
        skip_serializing_if = "BTreeMap::is_empty"
    )]
    pub state_labels: BTreeMap<String, String>,
    #[serde(
        default,
        deserialize_with = "de_default",
        skip_serializing_if = "BTreeMap::is_empty"
    )]
    pub tokens: BTreeMap<String, String>,
}

/// `activity.Entry` — one journal row in `activity`/`activity_history`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ActivityEntry {
    #[serde(default, deserialize_with = "de_default")]
    pub id: String,
    /// `activity.MilliTimestamp` — milliseconds since epoch.
    #[serde(default, deserialize_with = "de_default")]
    pub timestamp: i64,
    #[serde(default, deserialize_with = "de_default")]
    pub kind: String,
    #[serde(default, deserialize_with = "de_default")]
    pub status: String,
    #[serde(default, deserialize_with = "de_default")]
    pub summary: String,
    #[serde(
        default,
        deserialize_with = "de_default",
        skip_serializing_if = "String::is_empty"
    )]
    pub host: String,
    #[serde(
        default,
        deserialize_with = "de_default",
        skip_serializing_if = "String::is_empty"
    )]
    pub pane_id: String,
    #[serde(
        default,
        deserialize_with = "de_default",
        skip_serializing_if = "String::is_empty"
    )]
    pub agent: String,
    #[serde(
        default,
        deserialize_with = "de_default",
        skip_serializing_if = "String::is_empty"
    )]
    pub project: String,
    #[serde(
        default,
        deserialize_with = "de_default",
        skip_serializing_if = "String::is_empty"
    )]
    pub request_id: String,
    #[serde(
        default,
        deserialize_with = "de_default",
        skip_serializing_if = "String::is_empty"
    )]
    pub extract: String,
    #[serde(
        default,
        deserialize_with = "de_default",
        skip_serializing_if = "String::is_empty"
    )]
    pub session: String,
    #[serde(default, skip_serializing_if = "opt_map_is_empty")]
    pub details: Option<BTreeMap<String, serde_json::Value>>,
}

/// `herdr.Workspace` — one entry of the `workspaces` snapshot.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Workspace {
    #[serde(default, deserialize_with = "de_default")]
    pub workspace_id: String,
    #[serde(default, deserialize_with = "de_default")]
    pub number: i64,
    #[serde(default, deserialize_with = "de_default")]
    pub label: String,
    #[serde(default, deserialize_with = "de_default")]
    pub focused: bool,
    #[serde(default, deserialize_with = "de_default")]
    pub pane_count: i64,
    #[serde(default, deserialize_with = "de_default")]
    pub tab_count: i64,
    #[serde(default, deserialize_with = "de_default")]
    pub active_tab_id: String,
    #[serde(default, deserialize_with = "de_default")]
    pub agent_status: String,
    #[serde(
        default,
        deserialize_with = "de_default",
        skip_serializing_if = "String::is_empty"
    )]
    pub cwd: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree: Option<WorkspaceWorktree>,
    /// `workspace.report_metadata` tokens — sidebar row badges.
    #[serde(
        default,
        deserialize_with = "de_default",
        skip_serializing_if = "BTreeMap::is_empty"
    )]
    pub tokens: BTreeMap<String, String>,
}

/// `herdr.WorkspaceWorktree`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceWorktree {
    #[serde(default, deserialize_with = "de_default")]
    pub repo_key: String,
    #[serde(default, deserialize_with = "de_default")]
    pub repo_name: String,
    #[serde(default, deserialize_with = "de_default")]
    pub repo_root: String,
    #[serde(default, deserialize_with = "de_default")]
    pub checkout_path: String,
    #[serde(default, deserialize_with = "de_default")]
    pub is_linked_worktree: bool,
}

/// `protocol.HerdrFeatureStatus`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HerdrFeatureStatus {
    #[serde(default, deserialize_with = "de_default")]
    pub state: String,
    #[serde(default, deserialize_with = "de_default")]
    pub reason: String,
    #[serde(default, deserialize_with = "de_default")]
    pub generation: u64,
}

/// `protocol.HerdrStatus` — embedded in `push_config` and `herdr_status`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct HerdrStatus {
    #[serde(
        default,
        deserialize_with = "de_default",
        skip_serializing_if = "String::is_empty"
    )]
    pub installed_client_version: String,
    #[serde(
        default,
        deserialize_with = "de_default",
        skip_serializing_if = "String::is_empty"
    )]
    pub server_version: String,
    #[serde(
        default,
        deserialize_with = "de_default",
        skip_serializing_if = "is_zero"
    )]
    pub server_protocol: i64,
    /// Always emitted (no `omitempty`).
    #[serde(default, deserialize_with = "de_default")]
    pub server_protocol_known: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub endpoint_protocol_generation: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub surface_interest: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub health_check: Option<bool>,
    /// Always emitted (no `omitempty`).
    #[serde(default, deserialize_with = "de_default")]
    pub generation: u64,
    /// `map[string]HerdrFeatureStatus` without `omitempty`: nil -> `null`.
    #[serde(default)]
    pub features: MaybeNull<BTreeMap<String, HerdrFeatureStatus>>,
}

/// One entry of `speech_voices.voices` — map-built in Go (sorted keys); all
/// five keys are unconditional, so zero values are still emitted.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpeechVoice {
    #[serde(default, deserialize_with = "de_default")]
    pub bytes: i64,
    #[serde(default, deserialize_with = "de_default")]
    pub engine: String,
    #[serde(default, deserialize_with = "de_default")]
    pub installed: bool,
    #[serde(default, deserialize_with = "de_default")]
    pub language: String,
    #[serde(default, deserialize_with = "de_default")]
    pub name: String,
}

// ---------------------------------------------------------------------------
// Envelope structs (map-built in Go -> fields declared in sorted-key order).
// ---------------------------------------------------------------------------

/// `{"receipt":{...},"request_id":?,"type":"action_receipt"}`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ActionReceiptMessage {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub receipt: Option<ActionReceipt>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    /// Envelope discriminator. Constructors set the canonical constant.
    #[serde(default, deserialize_with = "de_default")]
    pub r#type: String,
}

/// `{"activity":{...},"type":"activity"}` — single journal entry broadcast.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ActivityMessage {
    #[serde(
        default,
        deserialize_with = "de_nullable",
        skip_serializing_if = "Option::is_none"
    )]
    pub activity: Option<MaybeNull<ActivityEntry>>,
    /// Envelope discriminator. Constructors set the canonical constant.
    #[serde(default, deserialize_with = "de_default")]
    pub r#type: String,
}

/// `{"activities":[...],"type":"activity_history"}`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ActivityHistoryMessage {
    #[serde(
        default,
        deserialize_with = "de_nullable",
        skip_serializing_if = "Option::is_none"
    )]
    pub activities: Option<MaybeNull<Vec<ActivityEntry>>>,
    /// Envelope discriminator. Constructors set the canonical constant.
    #[serde(default, deserialize_with = "de_default")]
    pub r#type: String,
}

/// `agent_update` — flat sorted map broadcast on inventory transitions.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct AgentUpdateMessage {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attention_kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generation: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pane_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pane_revision: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw_pane_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server_session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tab_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tab_label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tab_number: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminal_id: Option<String>,
    /// Envelope discriminator. Constructors set the canonical constant.
    #[serde(default, deserialize_with = "de_default")]
    pub r#type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
}

/// `{"agents":[AgentState...],"type":"agents"}` — inventory snapshot.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct AgentsMessage {
    #[serde(
        default,
        deserialize_with = "de_nullable",
        skip_serializing_if = "Option::is_none"
    )]
    pub agents: Option<MaybeNull<Vec<AgentState>>>,
    /// Envelope discriminator. Constructors set the canonical constant.
    #[serde(default, deserialize_with = "de_default")]
    pub r#type: String,
}

/// `{"app_deploy":{...},"type":"app_deploy_status"}` — deploy state is an
/// opaque `any` payload in Go; kept verbatim.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct AppDeployStatusMessage {
    #[serde(
        default,
        deserialize_with = "de_nullable",
        skip_serializing_if = "Option::is_none"
    )]
    pub app_deploy: Option<MaybeNull<RawJson>>,
    /// Envelope discriminator. Constructors set the canonical constant.
    #[serde(default, deserialize_with = "de_default")]
    pub r#type: String,
}

/// `blocked` — attention transition broadcast (sorted flat map).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct BlockedMessage {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approval_fingerprint: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attention_kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generation: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
    #[serde(
        default,
        deserialize_with = "de_nullable",
        skip_serializing_if = "Option::is_none"
    )]
    pub interaction: Option<MaybeNull<QuestionInteraction>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interaction_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(
        default,
        deserialize_with = "de_nullable",
        skip_serializing_if = "Option::is_none"
    )]
    pub options: Option<MaybeNull<Vec<String>>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pane_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pane_revision: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub question_layout: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw_pane_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server_session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tab_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tab_label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tab_number: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminal_id: Option<String>,
    /// Envelope discriminator. Constructors set the canonical constant.
    #[serde(default, deserialize_with = "de_default")]
    pub r#type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
}

/// `{"capabilities":[...],"type":"caps_update"}` — Phase-5 §0: the
/// server's advertised capability set mid-session. Replaces the
/// `push_config.capabilities` list wholesale (docs/13).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CapsUpdateMessage {
    #[serde(
        default,
        deserialize_with = "de_nullable",
        skip_serializing_if = "Option::is_none"
    )]
    pub capabilities: Option<MaybeNull<Vec<String>>>,
    /// Envelope discriminator. Constructors set the canonical constant.
    #[serde(default, deserialize_with = "de_default")]
    pub r#type: String,
}

/// `command_result` — correlated unary action result.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CommandResultMessage {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action: Option<String>,
    /// Present only when the command produced data (`result.Data != nil`).
    #[serde(
        default,
        deserialize_with = "de_nullable",
        skip_serializing_if = "Option::is_none"
    )]
    pub data: Option<MaybeNull<RawJson>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ok: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pane_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phase: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    /// Envelope discriminator. Constructors set the canonical constant.
    #[serde(default, deserialize_with = "de_default")]
    pub r#type: String,
}

/// `conversation_update` — Phase-5 §2.3 per-pane conversation push
/// (`convo_sub`). `messages` carries the same entry objects
/// `get_conversation_history` returns — serialized once by the coordinator
/// and embedded verbatim. `reset:true` marks a rebuilt history (initial
/// frame, source rotation, pane replacement) — the client drops its cache;
/// `reset:false` is append-only. `generation` is the pane's current epoch
/// (`TargetRef` conventions) so a stale feed is detectable; `target` echoes
/// the resolved subscription target.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ConversationUpdateMessage {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generation: Option<u64>,
    #[serde(
        default,
        deserialize_with = "de_nullable",
        skip_serializing_if = "Option::is_none"
    )]
    pub messages: Option<MaybeNull<RawJson>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reset: Option<bool>,
    #[serde(
        default,
        deserialize_with = "de_nullable",
        skip_serializing_if = "Option::is_none"
    )]
    pub target: Option<MaybeNull<TargetRef>>,
    /// Envelope discriminator. Constructors set the canonical constant.
    #[serde(default, deserialize_with = "de_default")]
    pub r#type: String,
}

/// `{"error":{code,args?},"request_id":?,"type":"error"}`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ErrorMessage {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<ApiError>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    /// Envelope discriminator. Constructors set the canonical constant.
    #[serde(default, deserialize_with = "de_default")]
    pub r#type: String,
}

/// `{"capabilities":[...],"status":{...},"type":"herdr_status"}`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct HerdrStatusMessage {
    #[serde(
        default,
        deserialize_with = "de_nullable",
        skip_serializing_if = "Option::is_none"
    )]
    pub capabilities: Option<MaybeNull<Vec<String>>>,
    #[serde(
        default,
        deserialize_with = "de_nullable",
        skip_serializing_if = "Option::is_none"
    )]
    pub status: Option<MaybeNull<HerdrStatus>>,
    /// Envelope discriminator. Constructors set the canonical constant.
    #[serde(default, deserialize_with = "de_default")]
    pub r#type: String,
}

/// `inventory_status` — inventory readiness broadcast.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct InventoryStatusMessage {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_attempt_at: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_success_at: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stale: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,
    /// Envelope discriminator. Constructors set the canonical constant.
    #[serde(default, deserialize_with = "de_default")]
    pub r#type: String,
}

/// `pane_content` — full frame or read response. The error variant carries
/// only `{content:"",error,format,pane_id,type}`.
///
/// Phase-5 §2.2 (`frame_zstd`): when the capability is negotiated both ways,
/// `content` travels compressed — `encoding:"zstd"` + `payload` hold
/// base64(zstd(`{"content":"…"}`)) and the `content` key is absent. An absent
/// `encoding` always means plaintext JSON.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PaneContent {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ack_required: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attention_kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_fingerprint: Option<String>,
    /// Phase-5 §2.2 — `"zstd"` when `payload` carries the compressed
    /// `content` member; absent on plaintext frames.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub encoding: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub format: Option<String>,
    #[serde(
        default,
        deserialize_with = "de_nullable",
        skip_serializing_if = "Option::is_none"
    )]
    pub interaction: Option<MaybeNull<QuestionInteraction>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub no_echo: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub no_echo_prompt: Option<String>,
    #[serde(
        default,
        deserialize_with = "de_nullable",
        skip_serializing_if = "Option::is_none"
    )]
    pub options: Option<MaybeNull<Vec<String>>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pane_id: Option<String>,
    /// Phase-5 §2.2 — base64(zstd(payload-json)) carrying the compressed
    /// members (`{"content":"…"}`); present iff `encoding` is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payload: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub question_layout: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resize_settling: Option<bool>,
    #[serde(
        default,
        deserialize_with = "de_nullable",
        skip_serializing_if = "Option::is_none"
    )]
    pub target: Option<MaybeNull<TargetRef>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub truncated: Option<bool>,
    /// Envelope discriminator. Constructors set the canonical constant.
    #[serde(default, deserialize_with = "de_default")]
    pub r#type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub viewport_only: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub viewport_rows: Option<i64>,
}

/// `pane_delta` — `pane_content` minus `content`, plus `base_fingerprint` and
/// `segments`. Never coalesced; chained on `base_fingerprint`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PaneDelta {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ack_required: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attention_kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_fingerprint: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_fingerprint: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub format: Option<String>,
    #[serde(
        default,
        deserialize_with = "de_nullable",
        skip_serializing_if = "Option::is_none"
    )]
    pub interaction: Option<MaybeNull<QuestionInteraction>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub no_echo: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub no_echo_prompt: Option<String>,
    #[serde(
        default,
        deserialize_with = "de_nullable",
        skip_serializing_if = "Option::is_none"
    )]
    pub options: Option<MaybeNull<Vec<String>>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pane_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub question_layout: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resize_settling: Option<bool>,
    #[serde(
        default,
        deserialize_with = "de_nullable",
        skip_serializing_if = "Option::is_none"
    )]
    pub segments: Option<MaybeNull<Vec<Segment>>>,
    #[serde(
        default,
        deserialize_with = "de_nullable",
        skip_serializing_if = "Option::is_none"
    )]
    pub target: Option<MaybeNull<TargetRef>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub truncated: Option<bool>,
    /// Envelope discriminator. Constructors set the canonical constant.
    #[serde(default, deserialize_with = "de_default")]
    pub r#type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub viewport_only: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub viewport_rows: Option<i64>,
}

/// `pane_probe` — cheap probe result inside the watch loop.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PaneProbe {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub format: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pane_id: Option<String>,
    /// Envelope discriminator. Constructors set the canonical constant.
    #[serde(default, deserialize_with = "de_default")]
    pub r#type: String,
}

/// `pane_resync` — stale-ack nudge; the client re-reads.
///
/// Phase-5 §2.2 lists this frame in the `frame_zstd` schema — the optional
/// `encoding`/`payload` members decode here for parity — but the message
/// carries no payload field to compress, so this relay never emits them.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PaneResync {
    /// Phase-5 §2.2 — tolerated on decode; never emitted (no payload).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub encoding: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pane_id: Option<String>,
    /// Phase-5 §2.2 — tolerated on decode; never emitted (no payload).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payload: Option<String>,
    #[serde(
        default,
        deserialize_with = "de_nullable",
        skip_serializing_if = "Option::is_none"
    )]
    pub target: Option<MaybeNull<TargetRef>>,
    /// Envelope discriminator. Constructors set the canonical constant.
    #[serde(default, deserialize_with = "de_default")]
    pub r#type: String,
}

/// `pane_unchanged` — read_pane answered by a fingerprint hit.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PaneUnchanged {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_fingerprint: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pane_id: Option<String>,
    #[serde(
        default,
        deserialize_with = "de_nullable",
        skip_serializing_if = "Option::is_none"
    )]
    pub target: Option<MaybeNull<TargetRef>>,
    /// Envelope discriminator. Constructors set the canonical constant.
    #[serde(default, deserialize_with = "de_default")]
    pub r#type: String,
}

/// `push_config` — the **first** message after handshake. Struct-built in Go:
/// field order is declaration order, NOT sorted.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PushConfig {
    /// Envelope discriminator. Constructors set the canonical constant.
    #[serde(default, deserialize_with = "de_default")]
    pub r#type: String,
    #[serde(default, deserialize_with = "de_default")]
    pub vapid_public_key: String,
    #[serde(default, deserialize_with = "de_default")]
    pub host: String,
    #[serde(default, deserialize_with = "de_default")]
    pub home: String,
    #[serde(default, deserialize_with = "de_default")]
    pub protocol: i64,
    #[serde(default, deserialize_with = "de_default")]
    pub version: String,
    #[serde(default, deserialize_with = "de_default")]
    pub release_version: String,
    #[serde(default, deserialize_with = "de_default")]
    pub revision: String,
    /// `any` in Go — always emitted (`null` when unset).
    #[serde(default)]
    pub update: MaybeNull<RawJson>,
    #[serde(default)]
    pub app_deploy: MaybeNull<RawJson>,
    #[serde(default)]
    pub capabilities: MaybeNull<Vec<String>>,
    #[serde(default, deserialize_with = "de_default")]
    pub herdr_status: HerdrStatus,
    #[serde(default, skip_serializing_if = "opt_vec_is_empty")]
    pub speech_languages: Option<Vec<String>>,
    #[serde(default)]
    pub inventory: MaybeNull<RawJson>,
    #[serde(default)]
    pub agent_profiles: MaybeNull<RawJson>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hybrid: Option<RawJson>,
}

/// `{"policy":{...},"type":"push_policy"}` — policy payload kept verbatim
/// (the object is sorted-map JSON in Go; raw bytes round-trip exactly).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PushPolicyMessage {
    #[serde(
        default,
        deserialize_with = "de_nullable",
        skip_serializing_if = "Option::is_none"
    )]
    pub policy: Option<MaybeNull<RawJson>>,
    /// Envelope discriminator. Constructors set the canonical constant.
    #[serde(default, deserialize_with = "de_default")]
    pub r#type: String,
}

/// `push_policy_result` — validation failures carry `code` instead of a
/// policy.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PushPolicyResultMessage {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ok: Option<bool>,
    #[serde(
        default,
        deserialize_with = "de_nullable",
        skip_serializing_if = "Option::is_none"
    )]
    pub policy: Option<MaybeNull<RawJson>>,
    /// Envelope discriminator. Constructors set the canonical constant.
    #[serde(default, deserialize_with = "de_default")]
    pub r#type: String,
}

/// `{"ok":bool,"type":...}` — push_subscribed / push_unsubscribed /
/// push_viewed_pane_result share the shape; the type differs.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct OkMessage {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ok: Option<bool>,
    /// Envelope discriminator. Constructors set the canonical constant.
    #[serde(default, deserialize_with = "de_default")]
    pub r#type: String,
}

/// `{"stage":"queued"|"dropped"|"rate_limited","type":"push_test_result"}`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PushTestResultMessage {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stage: Option<String>,
    /// Envelope discriminator. Constructors set the canonical constant.
    #[serde(default, deserialize_with = "de_default")]
    pub r#type: String,
}

/// `speech_voices` — voice catalog broadcast.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SpeechVoicesMessage {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_dir: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub engine_installed: Option<bool>,
    #[serde(
        default,
        deserialize_with = "de_nullable",
        skip_serializing_if = "Option::is_none"
    )]
    pub languages: Option<MaybeNull<Vec<String>>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub management_supported: Option<bool>,
    /// Envelope discriminator. Constructors set the canonical constant.
    #[serde(default, deserialize_with = "de_default")]
    pub r#type: String,
    #[serde(
        default,
        deserialize_with = "de_nullable",
        skip_serializing_if = "Option::is_none"
    )]
    pub voices: Option<MaybeNull<Vec<SpeechVoice>>>,
}

/// `{"type":"update_status","update":{...}}` — update state is opaque.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct UpdateStatusMessage {
    /// Envelope discriminator. Constructors set the canonical constant.
    #[serde(default, deserialize_with = "de_default")]
    pub r#type: String,
    #[serde(
        default,
        deserialize_with = "de_nullable",
        skip_serializing_if = "Option::is_none"
    )]
    pub update: Option<MaybeNull<RawJson>>,
}

/// `upload_{begin,cancel,chunk,finish}_result` — `{error?|result?}` plus the
/// request correlation. `error` is an `ApiError`; `result` stays raw.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct UploadResultMessage {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<ApiError>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    #[serde(
        default,
        deserialize_with = "de_nullable",
        skip_serializing_if = "Option::is_none"
    )]
    pub result: Option<MaybeNull<RawJson>>,
    /// Envelope discriminator. Constructors set the canonical constant.
    #[serde(default, deserialize_with = "de_default")]
    pub r#type: String,
}

/// `webrtc_answer` — relay answer to a client offer.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WebrtcAnswerMessage {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sdp: Option<String>,
    /// Envelope discriminator. Constructors set the canonical constant.
    #[serde(default, deserialize_with = "de_default")]
    pub r#type: String,
}

/// `webrtc_closed`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WebrtcClosedMessage {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    /// Envelope discriminator. Constructors set the canonical constant.
    #[serde(default, deserialize_with = "de_default")]
    pub r#type: String,
}

/// `webrtc_ice` — relay-originated ICE candidate.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WebrtcIceMessage {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub candidate: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sdp_mid: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sdp_mline_index: Option<i64>,
    /// Envelope discriminator. Constructors set the canonical constant.
    #[serde(default, deserialize_with = "de_default")]
    pub r#type: String,
}

/// `{"type":"workspaces","workspaces":[Workspace...]}`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct WorkspacesMessage {
    /// Envelope discriminator. Constructors set the canonical constant.
    #[serde(default, deserialize_with = "de_default")]
    pub r#type: String,
    #[serde(
        default,
        deserialize_with = "de_nullable",
        skip_serializing_if = "Option::is_none"
    )]
    pub workspaces: Option<MaybeNull<Vec<Workspace>>>,
}

// ---------------------------------------------------------------------------
// Outbound enum.
// ---------------------------------------------------------------------------

/// A decoded server -> client message.
///
/// `Unknown` keeps the raw bytes — the relay forwards plenty of message types
/// it does not interpret, and they must survive verbatim.
#[derive(Debug, Clone, PartialEq)]
pub enum Outbound {
    ActionReceipt(ActionReceiptMessage),
    Activity(ActivityMessage),
    ActivityHistory(ActivityHistoryMessage),
    AgentUpdate(AgentUpdateMessage),
    Agents(AgentsMessage),
    AppDeployStatus(AppDeployStatusMessage),
    Blocked(Box<BlockedMessage>),
    CapsUpdate(CapsUpdateMessage),
    CommandResult(CommandResultMessage),
    ConversationUpdate(ConversationUpdateMessage),
    Error(ErrorMessage),
    HerdrStatus(HerdrStatusMessage),
    InventoryStatus(InventoryStatusMessage),
    PaneContent(Box<PaneContent>),
    PaneDelta(Box<PaneDelta>),
    PaneProbe(PaneProbe),
    PaneResync(PaneResync),
    PaneUnchanged(PaneUnchanged),
    PushConfig(Box<PushConfig>),
    PushPolicy(PushPolicyMessage),
    PushPolicyResult(PushPolicyResultMessage),
    PushSubscribed(OkMessage),
    PushTestResult(PushTestResultMessage),
    PushUnsubscribed(OkMessage),
    PushViewedPaneResult(OkMessage),
    SpeechVoices(SpeechVoicesMessage),
    UpdateStatus(UpdateStatusMessage),
    UploadBeginResult(UploadResultMessage),
    UploadCancelResult(UploadResultMessage),
    UploadChunkResult(UploadResultMessage),
    UploadFinishResult(UploadResultMessage),
    WebrtcAnswer(WebrtcAnswerMessage),
    WebrtcClosed(WebrtcClosedMessage),
    WebrtcIce(WebrtcIceMessage),
    Workspaces(WorkspacesMessage),
    /// Any `type` not modeled above — raw envelope, byte-preserved.
    Unknown(RawJson),
}

#[derive(Deserialize)]
struct TypeProbe {
    /// A non-string `type` decodes to "" (Go's `.(string)` ok-pattern) and
    /// falls through to `Unknown`.
    #[serde(rename = "type", default, deserialize_with = "de_default")]
    kind: String,
}

impl Outbound {
    /// Decode a wire frame into the typed view. Unknown message types are
    /// preserved as [`Outbound::Unknown`].
    ///
    /// Decode is wire-faithful: a §2.2-compressed `pane_content` keeps its
    /// `encoding`/`payload` members (so `decode` + [`encode`](Self::encode)
    /// round-trips byte-exact). Call
    /// [`PaneContent::decompress_payload`] for the logical, inflated view.
    pub fn decode(bytes: &[u8]) -> Result<Self, serde_json::Error> {
        let probe: TypeProbe = serde_json::from_slice(bytes)?;
        macro_rules! typed {
            ($variant:ident, $struct:ty) => {
                Outbound::$variant(serde_json::from_slice::<$struct>(bytes)?)
            };
        }
        Ok(match probe.kind.as_str() {
            "action_receipt" => typed!(ActionReceipt, ActionReceiptMessage),
            "activity" => typed!(Activity, ActivityMessage),
            "activity_history" => typed!(ActivityHistory, ActivityHistoryMessage),
            "agent_update" => typed!(AgentUpdate, AgentUpdateMessage),
            "agents" => typed!(Agents, AgentsMessage),
            "app_deploy_status" => typed!(AppDeployStatus, AppDeployStatusMessage),
            "blocked" => typed!(Blocked, Box<BlockedMessage>),
            "caps_update" => typed!(CapsUpdate, CapsUpdateMessage),
            "command_result" => typed!(CommandResult, CommandResultMessage),
            "conversation_update" => typed!(ConversationUpdate, ConversationUpdateMessage),
            "error" => typed!(Error, ErrorMessage),
            "herdr_status" => typed!(HerdrStatus, HerdrStatusMessage),
            "inventory_status" => typed!(InventoryStatus, InventoryStatusMessage),
            "pane_content" => typed!(PaneContent, Box<PaneContent>),
            "pane_delta" => typed!(PaneDelta, Box<PaneDelta>),
            "pane_probe" => typed!(PaneProbe, PaneProbe),
            "pane_resync" => typed!(PaneResync, PaneResync),
            "pane_unchanged" => typed!(PaneUnchanged, PaneUnchanged),
            "push_config" => typed!(PushConfig, Box<PushConfig>),
            "push_policy" => typed!(PushPolicy, PushPolicyMessage),
            "push_policy_result" => typed!(PushPolicyResult, PushPolicyResultMessage),
            "push_subscribed" => typed!(PushSubscribed, OkMessage),
            "push_test_result" => typed!(PushTestResult, PushTestResultMessage),
            "push_unsubscribed" => typed!(PushUnsubscribed, OkMessage),
            "push_viewed_pane_result" => typed!(PushViewedPaneResult, OkMessage),
            "speech_voices" => typed!(SpeechVoices, SpeechVoicesMessage),
            "update_status" => typed!(UpdateStatus, UpdateStatusMessage),
            "upload_begin_result" => typed!(UploadBeginResult, UploadResultMessage),
            "upload_cancel_result" => typed!(UploadCancelResult, UploadResultMessage),
            "upload_chunk_result" => typed!(UploadChunkResult, UploadResultMessage),
            "upload_finish_result" => typed!(UploadFinishResult, UploadResultMessage),
            "webrtc_answer" => typed!(WebrtcAnswer, WebrtcAnswerMessage),
            "webrtc_closed" => typed!(WebrtcClosed, WebrtcClosedMessage),
            "webrtc_ice" => typed!(WebrtcIce, WebrtcIceMessage),
            "workspaces" => typed!(Workspaces, WorkspacesMessage),
            _ => Outbound::Unknown(serde_json::from_slice::<RawJson>(bytes)?),
        })
    }

    /// Serialize exactly as Go's `json.Marshal` would.
    pub fn encode(&self) -> Vec<u8> {
        macro_rules! enc {
            ($m:expr) => {
                crate::json::to_vec($m).expect("outbound message serialization cannot fail")
            };
        }
        match self {
            Outbound::ActionReceipt(m) => enc!(m),
            Outbound::Activity(m) => enc!(m),
            Outbound::ActivityHistory(m) => enc!(m),
            Outbound::AgentUpdate(m) => enc!(m),
            Outbound::Agents(m) => enc!(m),
            Outbound::AppDeployStatus(m) => enc!(m),
            Outbound::Blocked(m) => enc!(m),
            Outbound::CapsUpdate(m) => enc!(m),
            Outbound::CommandResult(m) => enc!(m),
            Outbound::ConversationUpdate(m) => enc!(m),
            Outbound::Error(m) => enc!(m),
            Outbound::HerdrStatus(m) => enc!(m),
            Outbound::InventoryStatus(m) => enc!(m),
            Outbound::PaneContent(m) => enc!(m),
            Outbound::PaneDelta(m) => enc!(m),
            Outbound::PaneProbe(m) => enc!(m),
            Outbound::PaneResync(m) => enc!(m),
            Outbound::PaneUnchanged(m) => enc!(m),
            Outbound::PushConfig(m) => enc!(m),
            Outbound::PushPolicy(m) => enc!(m),
            Outbound::PushPolicyResult(m) => enc!(m),
            Outbound::PushSubscribed(m) => enc!(m),
            Outbound::PushTestResult(m) => enc!(m),
            Outbound::PushUnsubscribed(m) => enc!(m),
            Outbound::PushViewedPaneResult(m) => enc!(m),
            Outbound::SpeechVoices(m) => enc!(m),
            Outbound::UpdateStatus(m) => enc!(m),
            Outbound::UploadBeginResult(m)
            | Outbound::UploadCancelResult(m)
            | Outbound::UploadChunkResult(m)
            | Outbound::UploadFinishResult(m) => enc!(m),
            Outbound::WebrtcAnswer(m) => enc!(m),
            Outbound::WebrtcClosed(m) => enc!(m),
            Outbound::WebrtcIce(m) => enc!(m),
            Outbound::Workspaces(m) => enc!(m),
            Outbound::Unknown(raw) => raw.get().as_bytes().to_vec(),
        }
    }

    /// Serialize with the negotiated Phase-5 transport upgrades applied —
    /// one [`Negotiated`] flag per Track-B feature that reshapes wire
    /// bytes. Every other message, and every frame while its gate is off,
    /// encodes exactly like [`encode`].
    ///
    /// [`encode`]: Self::encode
    pub fn encode_negotiated(&self, negotiated: Negotiated) -> Vec<u8> {
        match self {
            Outbound::PaneContent(message) if negotiated.frame_zstd => {
                let mut message = (**message).clone();
                if !message.compress_payload() {
                    return self.encode();
                }
                crate::json::to_vec(&message).expect("outbound message serialization cannot fail")
            }
            Outbound::UploadBeginResult(message) if negotiated.upload_binary => {
                let mut message = message.clone();
                if !crate::uploadbinary::mark_chunk_encoding(&mut message) {
                    return self.encode();
                }
                crate::json::to_vec(&message).expect("outbound message serialization cannot fail")
            }
            _ => self.encode(),
        }
    }
}

/// The negotiated Phase-5 Track-B transport upgrades — one flag per
/// feature that reshapes wire bytes. The session's capability
/// intersection (advertised ∩ announced, docs/13 §0) computes it per
/// frame; a flag is set only while its capability is live on both lists.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Negotiated {
    /// §2.2 — fold a `pane_content.content` member into the compressed
    /// `payload`.
    pub frame_zstd: bool,
    /// §2.4 — stamp `chunk_encoding:"binary"` on `upload_begin_result`
    /// payloads.
    pub upload_binary: bool,
}

// ---------------------------------------------------------------------------
// Response constructors (protocol.go helpers).
// ---------------------------------------------------------------------------

/// `protocol.ErrorResponse` — `{"type":"error","error":{...},"request_id"?}`.
pub fn error_response(request_id: &str, error: ApiError) -> ErrorMessage {
    ErrorMessage {
        error: Some(error),
        request_id: (!request_id.is_empty()).then(|| request_id.to_owned()),
        r#type: "error".to_owned(),
    }
}

/// `protocol.ActionReceiptResponse`.
pub fn action_receipt_response(request_id: &str, receipt: ActionReceipt) -> ActionReceiptMessage {
    ActionReceiptMessage {
        receipt: Some(receipt),
        request_id: (!request_id.is_empty()).then(|| request_id.to_owned()),
        r#type: "action_receipt".to_owned(),
    }
}

/// `protocol.IncompatibleResponse` — `failed_before_dispatch` receipt carrying
/// `incompatible_protocol` args `{"received","required"}`.
pub fn incompatible_response(message: &super::inbound::Inbound) -> ActionReceiptMessage {
    let received = if message.protocol == 0 {
        "invalid".to_owned()
    } else {
        message.protocol.to_string()
    };
    let mut args = BTreeMap::new();
    args.insert("received".to_owned(), serde_json::Value::from(received));
    args.insert(
        "required".to_owned(),
        serde_json::Value::from(VERSION.to_string()),
    );
    action_receipt_response(
        &message.request_id,
        ActionReceipt {
            action_id: message.action_id.clone(),
            phase: ActionReceiptPhase(ActionReceiptPhase::FAILED_BEFORE_DISPATCH.to_owned()),
            error: Some(ApiError::new(
                super::types::error_codes::INCOMPATIBLE_PROTOCOL,
                args,
            )),
        },
    )
}

/// `protocol.DecodeFailureResponse` — `invalid_request` error, echoing
/// `request_id` when the raw map carried one.
pub fn decode_failure_response(raw: &serde_json::Map<String, serde_json::Value>) -> ErrorMessage {
    let request_id = raw
        .get("request_id")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    error_response(
        request_id,
        ApiError::new(super::types::error_codes::INVALID_REQUEST, BTreeMap::new()),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn caps_update_round_trips() {
        let outbound = Outbound::CapsUpdate(CapsUpdateMessage {
            capabilities: Some(MaybeNull::Value(vec![
                "attention_classification".to_owned(),
                "focus".to_owned(),
            ])),
            r#type: "caps_update".to_owned(),
        });
        let encoded = String::from_utf8(outbound.encode()).unwrap();
        assert_eq!(
            encoded,
            r#"{"capabilities":["attention_classification","focus"],"type":"caps_update"}"#
        );
        // The envelope decoder picks the typed variant, and an empty
        // advertised set round-trips as `[]` (not `null`/absent).
        let decoded = Outbound::decode(encoded.as_bytes()).unwrap();
        let Outbound::CapsUpdate(message) = decoded else {
            panic!("expected CapsUpdate, got {decoded:?}");
        };
        assert_eq!(
            message.capabilities.as_ref().and_then(MaybeNull::value),
            Some(&vec![
                "attention_classification".to_owned(),
                "focus".to_owned()
            ])
        );

        let empty = Outbound::decode(br#"{"capabilities":[],"type":"caps_update"}"#).unwrap();
        let Outbound::CapsUpdate(message) = empty else {
            panic!("expected CapsUpdate");
        };
        assert_eq!(
            message.capabilities.as_ref().and_then(MaybeNull::value),
            Some(&Vec::<String>::new())
        );
        // `null` and absent both read `None`-ish — the list semantics live
        // on the sender (wholesale replace when present).
        let null_list = Outbound::decode(br#"{"capabilities":null,"type":"caps_update"}"#).unwrap();
        let Outbound::CapsUpdate(message) = null_list else {
            panic!("expected CapsUpdate");
        };
        assert!(matches!(message.capabilities, Some(MaybeNull::Null) | None));
    }

    /// docs/13 §2.3 — `conversation_update` emits the spec's flat shape:
    /// sorted keys, `messages` verbatim, `target` a TargetRef.
    #[test]
    fn conversation_update_round_trips() {
        let messages = serde_json::value::RawValue::from_string(
            r#"[{"id":"e1","role":"user","text":"hi"}]"#.to_owned(),
        )
        .unwrap();
        let outbound = Outbound::ConversationUpdate(ConversationUpdateMessage {
            generation: Some(2),
            messages: Some(MaybeNull::Value(RawJson(messages))),
            reset: Some(true),
            target: Some(MaybeNull::Value(TargetRef {
                server_session_id: "primary".to_owned(),
                pane_id: "wE:p1".to_owned(),
                terminal_id: "term-1".to_owned(),
                generation: 2,
                agent_session_id: "sess-1".to_owned(),
                ..TargetRef::default()
            })),
            r#type: "conversation_update".to_owned(),
        });
        let encoded = String::from_utf8(outbound.encode()).unwrap();
        assert_eq!(
            encoded,
            r#"{"generation":2,"messages":[{"id":"e1","role":"user","text":"hi"}],"reset":true,"target":{"server_session_id":"primary","pane_id":"wE:p1","terminal_id":"term-1","generation":2,"agent_session_id":"sess-1"},"type":"conversation_update"}"#
        );
        let decoded = Outbound::decode(encoded.as_bytes()).unwrap();
        let Outbound::ConversationUpdate(message) = decoded else {
            panic!("expected ConversationUpdate, got {decoded:?}");
        };
        assert_eq!(message.generation, Some(2));
        assert_eq!(message.reset, Some(true));
        assert_eq!(
            message
                .messages
                .as_ref()
                .and_then(MaybeNull::value)
                .map(RawJson::get),
            Some(r#"[{"id":"e1","role":"user","text":"hi"}]"#)
        );
        let target = message.target.and_then(MaybeNull::into_value).unwrap();
        assert_eq!(target.pane_id, "wE:p1");
        assert_eq!(target.agent_session_id, "sess-1");
    }
}

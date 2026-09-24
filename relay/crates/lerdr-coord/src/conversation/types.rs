//! Wire and reader-level types for `get_conversation_history`.
//!
//! Two page shapes exist in the oracle and are preserved here:
//!
//! - [`Page`] mirrors Go `conversation.Page` — the reader-level result the
//!   `conversation.page.*` fixtures pin (`before` = entry id, `total` a plain
//!   int, `source_corrupt`/`file_truncated`/`continuation_*` flags).
//! - [`BrowsePage`] mirrors Go `conversation.BrowsePage` — the wire payload
//!   embedded in `command_result.data` and consumed by the app's
//!   `ConversationProjector` (`state`, `mode`, `next_cursor`, `diagnostics`,
//!   nullable `total`, `error`).
//!
//! Field names and `omitempty` behaviour match the Go `encoding/json` tags.

use serde::{Deserialize, Serialize};

/// One visible conversation row (Go `Entry`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    /// `sha256(raw line)[:24]` for JSONL readers (with a `-N` suffix for
    /// duplicate identical lines), the message id for OpenCode, the sqlite
    /// `logical_id` for Hermes. Claude continuation segments are prefixed
    /// `sha256(sessionID+"\x00"+fileRevision)[:12] + "-"`.
    pub id: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub timestamp: String,
    pub role: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub text: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<ToolActivity>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub truncated: bool,
}

/// One tool call attached to an [`Entry`] (Go `ToolActivity`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ToolActivity {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub id: String,
    pub name: String,
    /// The id used to match a later tool-result row/block. Mirrors Go's
    /// unexported `associationID`: preserved from the source record before the
    /// public `id` is clamped for the wire.
    #[serde(skip)]
    pub association_id: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub input: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub output: String,
    #[serde(default, skip_serializing_if = "is_false")]
    pub error: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub truncated: bool,
}

/// Reader-level page — Go `conversation.Page` field-for-field.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Page {
    pub available: bool,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub reason_code: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub reason: String,
    pub entries: Vec<Entry>,
    pub has_more: bool,
    /// Go `int` — always emitted, `0` on unavailable pages.
    pub total: i64,
    #[serde(default, skip_serializing_if = "is_false")]
    pub file_truncated: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub source_corrupt: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub continuation_incomplete: bool,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub continuation_reason: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub omo_plan: Option<OmoTodoState>,
    /// Oldest selected message/row id for the native readers — the fallback
    /// cursor anchor when a page carries no entries (Go `nativePage`'s
    /// `rawBefore`). Internal only; never serialized.
    #[serde(skip)]
    pub cursor_before: String,
    /// Resolved source path — the transcript file or database — used by the
    /// browser to compute `source_revision`. Internal only.
    #[serde(skip)]
    pub source_path: String,
    /// The file a subscriber stats to decide whether the transcript moved —
    /// the chain TIP for Claude (`continued-in` links and new records both
    /// land in the newest segment), the located source everywhere else.
    /// Empty means the read captured no statable source. Internal only.
    #[serde(skip)]
    pub probe_path: String,
}

impl Page {
    /// `unavailableCode` — an unavailable page carrying a reason code.
    pub fn unavailable(code: &str, reason: &str) -> Self {
        Page {
            available: false,
            reason_code: code.to_string(),
            reason: reason.to_string(),
            entries: Vec::new(),
            ..Page::default()
        }
    }
}

/// `state` field on the wire page (Go `BrowseState`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum BrowseState {
    Ready,
    Preparing,
    Failed,
}

/// `mode` field on the wire page (Go `BrowseMode`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum BrowseMode {
    Recent,
    Snapshot,
    Native,
}

/// `progress` — preparation progress (Go `BrowseProgress`). Never emitted by
/// this port: there are no background prepare jobs.
#[derive(Debug, Clone, Serialize)]
pub struct BrowseProgress {
    pub phase: String,
    pub scanned_bytes: i64,
    pub source_bytes: i64,
}

/// `diagnostics` — reader self-report (Go `BrowseDiagnostics`).
/// `oversized_records` and `corrupt_records` are always emitted (no omitempty
/// in Go); the rest are optional.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct BrowseDiagnostics {
    pub oversized_records: i64,
    pub corrupt_records: i64,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub omitted_tools: i64,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub omitted_payloads: i64,
    #[serde(default, skip_serializing_if = "is_false")]
    pub plan_corrupt: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub source_truncated: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub continuation_incomplete: bool,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub continuation_reason: String,
}

/// `error` — structured failure on `state = "failed"` pages (Go `BrowseError`).
#[derive(Debug, Clone, Serialize)]
pub struct BrowseError {
    pub code: String,
    pub message: String,
    pub retryable: bool,
}

/// Wire page — Go `conversation.BrowsePage`, embedded verbatim in the
/// `command_result.data` payload of `get_conversation_history`.
///
/// Cursors are the native form: `next_cursor` is the first entry id of the
/// page (the value to pass back as `cursor`/`before` for the next-older
/// page). The oracle wraps the same value in a signed `hb1.` envelope; the
/// raw id is opaque to the client and round-trips through this reader.
#[derive(Debug, Clone, Serialize)]
pub struct BrowsePage {
    pub available: bool,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub reason_code: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub reason: String,
    pub entries: Vec<Entry>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub next_cursor: String,
    pub has_more: bool,
    pub state: BrowseState,
    pub mode: BrowseMode,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub source_revision: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub snapshot_id: String,
    /// Go `*int` — always emitted, `null` on unavailable/failed pages and on
    /// tail windows of clipped sources.
    pub total: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub progress: Option<BrowseProgress>,
    pub diagnostics: BrowseDiagnostics,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<BrowseError>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub omo_plan: Option<OmoTodoState>,
    /// The file a conversation subscriber stats between reads — the chain
    /// tip for Claude, `source_path` elsewhere. Internal only.
    #[serde(skip)]
    pub probe_path: String,
}

/// OMO todo-state projection (Go `OMOTodoState`).
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct OmoTodoState {
    pub available: bool,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub reason_code: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub session_id: String,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub version: i64,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub updated_at: String,
    pub phases: Vec<OmoTodoPhase>,
    pub truncated: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct OmoTodoPhase {
    pub name: String,
    pub tasks: Vec<OmoTodoTask>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct OmoTodoTask {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub id: String,
    pub content: String,
    pub status: String,
}

/// Request scope — Go `BrowseScope`. Only `provider`, `cwd`,
/// `foreground_cwd` and `session_id` affect reads; the rest are carried for
/// API parity (cursor scoping in the oracle's signed-token browser).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrowseScope {
    pub provider: String,
    pub cwd: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub foreground_cwd: String,
    pub session_id: String,
    #[serde(default)]
    pub pane_id: String,
    #[serde(default)]
    pub server_session_id: String,
    #[serde(default)]
    pub terminal_id: String,
    #[serde(default)]
    pub generation: i64,
}

/// One `get_conversation_history` request (Go `BrowseRequest`).
#[derive(Debug, Clone, Default)]
pub struct BrowseRequest {
    pub scope: BrowseScope,
    /// Opaque cursor from a previous page's `next_cursor` — the entry id the
    /// next page should end before. `None`/empty reads the latest page.
    pub cursor: Option<String>,
    pub limit: i64,
    /// Forces re-resolution past the reader's location cache. The oracle uses
    /// it to retry failed background jobs; with a synchronous reader the only
    /// cached state worth bypassing is the tuple→location map.
    pub retry: bool,
}

/// The located transcript for a pane (Go `Location`). `path` is the
/// canonicalized, containment-checked file; `root` is the configured root it
/// was found under.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Location {
    pub path: String,
    pub root: String,
    pub title: String,
}

/// Pane/foreground directory hints (Go `ProjectContext`). `PartialEq` backs
/// the server's agent-changed recheck (`sameConversationTuple`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProjectContext {
    pub cwd: String,
    pub foreground_cwd: String,
}

pub(crate) fn is_false(v: &bool) -> bool {
    !*v
}

pub(crate) fn is_zero(v: &i64) -> bool {
    *v == 0
}

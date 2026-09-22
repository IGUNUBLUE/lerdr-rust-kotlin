//! Conversation transcript reader/browser — the Rust port of the oracle's
//! `internal/conversation` package, backing the `get_conversation_history`
//! action.
//!
//! Layout mirrors the oracle's responsibilities rather than its files:
//!
//! - [`types`] — wire/reader structs (`Entry`, `ToolActivity`, `Page`,
//!   `BrowsePage`, `BrowseScope`, `BrowseRequest`, `OmoTodoState`) with the
//!   oracle's JSON field names and `omitempty` behaviour.
//! - [`util`] — `NormalizeLine`/sanitize/clamp helpers, Go `encoding/json`
//!   marshalling for tool payloads, RFC 3339 formatting, `sha256` ids.
//! - [`records`] — the JSONL scanner and per-agent record grammars
//!   (`parseTranscript`, `parseToolActivity`, `parse{Claude,Codex,Pi}Record`).
//! - [`roots`] — `agentroots` root resolution (`HERDR_*`/`LERDR_*` lists,
//!   agent env vars, discovered profiles, home defaults last).
//! - [`reader`] — `Reader` with the bounded tuple→location cache and the flat
//!   `read_with_project` path.
//! - [`claude`] — `continued-in` chain resolution + namespaced entry ids.
//! - [`sqlite`], [`opencode`], [`hermes`] — the `sqlite3` CLI readers (same
//!   subprocess strategy as the oracle; no SQLite crate needed).
//! - [`omo`] — OMO filename/identity/todo-state handling.
//! - [`browser`] — `ConversationBrowser::read_page`, the synchronous
//!   equivalent of `Browser.ReadPage`: cursors are raw entry ids rather than
//!   the oracle's signed `hb1.` byte-range tokens (the wire value stays
//!   opaque — clients round-trip `next_cursor` unchanged).
//!
//! Deliberate omissions versus the oracle: the background snapshot/prepare
//! scheduler, the signed cursor envelope, and the race-detector harness —
//! none are visible in the `conversation.page.*` fixtures, and the reader
//! semantics they mediate (entry-id `before`, newest page at the tail,
//! `source_changed`/`invalid_cursor` codes) are preserved.

pub(crate) mod browser;
pub(crate) mod claude;
pub(crate) mod hermes;
pub(crate) mod omo;
pub(crate) mod opencode;
pub(crate) mod reader;
pub(crate) mod records;
pub(crate) mod roots;
pub(crate) mod sqlite;
pub(crate) mod types;
pub(crate) mod util;

pub use browser::ConversationBrowser;
pub use reader::{supported, Error, Reader};
pub use types::{
    BrowseDiagnostics, BrowseError, BrowseMode, BrowsePage, BrowseProgress, BrowseRequest,
    BrowseScope, BrowseState, Entry, Location, OmoTodoPhase, OmoTodoState, OmoTodoTask, Page,
    ProjectContext, ToolActivity,
};

#[cfg(test)]
mod fixture_tests;

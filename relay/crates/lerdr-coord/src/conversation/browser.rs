//! `ConversationBrowser` — the wire-facing `read_page` adapter over `Reader`.
//!
//! The oracle's `Browser` is asynchronous: signed `hb1.` cursors, byte-range
//! offsets, background prepare jobs, and snapshot indexes. This port is
//! synchronous, so the cursor contract narrows to the entry-id `before`
//! semantics the `conversation.page.*` fixtures pin: `next_cursor` is the
//! first entry id of the returned page, and passing it back as `cursor`
//! returns the next-older page. The reader-level invariants (location cache,
//! continuation chains, native id cursors, corruption flags) are unchanged.

use std::path::PathBuf;
use std::sync::Arc;

use sha2::{Digest, Sha256};

use super::reader::{
    is_hermes_agent, normalize_project_context, Reader, DEFAULT_PAGE_SIZE, MAX_PAGE_SIZE,
};
use super::types::{
    BrowseDiagnostics, BrowseError, BrowseMode, BrowsePage, BrowseRequest, BrowseScope,
    BrowseState, OmoTodoState, Page,
};
use super::util::{clamp_text, normalized_agent};

/// `DefaultBrowserOptions().ResponseBytes` — 2 MiB wire-page budget.
const RESPONSE_BYTES: usize = 2 * 1024 * 1024;

/// The oracle's `Browser`: one shared `Reader` behind a synchronous facade.
pub struct ConversationBrowser {
    reader: Arc<Reader>,
}

impl ConversationBrowser {
    /// `NewBrowser` minus the cache root / options — defaults apply.
    pub fn new(home: PathBuf) -> Self {
        Self::with_reader(Arc::new(Reader::new(home)))
    }

    /// `NewBrowserWithReader` — share the reader's tuple→location cache and
    /// provider handles with the resolver/subscription paths so transcript
    /// source decisions stay consistent across consumers.
    pub fn with_reader(reader: Arc<Reader>) -> Self {
        ConversationBrowser { reader }
    }

    /// Access to the underlying reader (title resolution shares it).
    pub fn reader(&self) -> &Reader {
        &self.reader
    }

    /// `ReadPage` — synchronous equivalent. `cursor` is the previous page's
    /// `next_cursor` (a raw entry/native id); `retry` drops the cached
    /// location so a fixed-up source is re-resolved. Reader errors are already
    /// mapped onto the page (`state:"failed"`), so `Err` is reserved for
    /// future faults.
    pub async fn read_page(&self, req: BrowseRequest) -> Result<BrowsePage, super::reader::Error> {
        Ok(self.read_page_sync(req))
    }

    /// The synchronous body of [`read_page`](Self::read_page) —
    /// `convo_sub` feed tasks run it inside `spawn_blocking` (bounded file
    /// I/O plus a ~3s-capped `sqlite3` subprocess for the native readers
    /// must not stall the executor).
    pub(crate) fn read_page_sync(&self, request: BrowseRequest) -> BrowsePage {
        let scope = normalize_browse_scope(request.scope);
        let limit = if request.limit < 1 {
            DEFAULT_PAGE_SIZE as i64
        } else {
            request.limit.min(MAX_PAGE_SIZE as i64)
        };
        if scope.session_id.is_empty() {
            return browse_unavailable(
                "invalid_session",
                "This agent has not reported a conversation session yet.",
            );
        }
        let provider = normalized_agent(&scope.provider);
        if !super::reader::supported(&provider) {
            return browse_unavailable(
                "invalid_provider",
                "Conversation history is not available for this agent.",
            );
        }
        if request.retry {
            self.reader
                .evict_location(&provider, &scope.cwd, &scope.session_id);
        }
        let before = request.cursor.clone().unwrap_or_default();
        let project = super::types::ProjectContext {
            cwd: scope.cwd.clone(),
            foreground_cwd: scope.foreground_cwd.clone(),
        };
        let native = provider == "opencode" || is_hermes_agent(&provider);
        let page = match self.reader.read_with_project(
            &provider,
            project,
            &scope.session_id,
            Some(before.as_str()).filter(|b| !b.is_empty()),
            limit.max(0) as usize,
        ) {
            Ok(page) => page,
            Err(err) => {
                return browse_failure(
                    "source_unavailable",
                    "Conversation history is unavailable.",
                    BrowseError {
                        code: "source_unavailable".to_string(),
                        message: err.to_string(),
                        retryable: true,
                    },
                )
            }
        };
        wire_page(page, &provider, native, self.reader.home.as_path())
    }
}

/// `normalizeBrowseScope` — provider and fields trimmed, foreground hint kept
/// only for absolute Claude dirs, `server_session_id` defaults to "primary".
fn normalize_browse_scope(mut scope: BrowseScope) -> BrowseScope {
    scope.provider = normalized_agent(&scope.provider);
    // `normalizeBrowseProjectContext`: non-Claude providers keep the browser's
    // historical cwd trimming and never accept a foreground hint.
    if !super::reader::is_claude_provider(&scope.provider) {
        scope.cwd = scope.cwd.trim().to_string();
    }
    let project = normalize_project_context(
        &scope.provider,
        super::types::ProjectContext {
            cwd: scope.cwd,
            foreground_cwd: scope.foreground_cwd,
        },
    );
    scope.cwd = project.cwd;
    scope.foreground_cwd = project.foreground_cwd;
    scope.session_id = scope.session_id.trim().to_string();
    scope.pane_id = scope.pane_id.trim().to_string();
    scope.server_session_id = scope.server_session_id.trim().to_string();
    scope.terminal_id = scope.terminal_id.trim().to_string();
    if scope.server_session_id.is_empty() {
        scope.server_session_id = "primary".to_string();
    }
    if scope.generation < 0 {
        scope.generation = 0;
    }
    scope
}

/// `browseUnavailable`.
fn browse_unavailable(code: &str, reason: &str) -> BrowsePage {
    BrowsePage {
        available: false,
        reason_code: code.to_string(),
        reason: reason.to_string(),
        entries: Vec::new(),
        next_cursor: String::new(),
        has_more: false,
        state: BrowseState::Ready,
        mode: BrowseMode::Recent,
        source_revision: String::new(),
        snapshot_id: String::new(),
        total: None,
        progress: None,
        diagnostics: BrowseDiagnostics::default(),
        error: None,
        omo_plan: None,
        probe_path: String::new(),
    }
}

/// `browseFailure` — available stays true, state `failed`.
fn browse_failure(code: &str, reason: &str, error: BrowseError) -> BrowsePage {
    BrowsePage {
        available: true,
        reason_code: code.to_string(),
        reason: reason.to_string(),
        entries: Vec::new(),
        next_cursor: String::new(),
        has_more: false,
        state: BrowseState::Failed,
        mode: BrowseMode::Recent,
        source_revision: String::new(),
        snapshot_id: String::new(),
        total: None,
        progress: None,
        diagnostics: BrowseDiagnostics::default(),
        error: Some(error),
        omo_plan: None,
        probe_path: String::new(),
    }
}

/// `Page` → `BrowsePage`: entry-id `next_cursor` when older entries remain,
/// `source_revision` for native databases and captured files, `total` omitted
/// on clipped tails, and the 2 MiB serialized budget applied last.
fn wire_page(page: Page, provider: &str, native: bool, home: &std::path::Path) -> BrowsePage {
    let _ = home;
    if !page.available {
        let mut wire = browse_unavailable(&page.reason_code, &page.reason);
        wire.omo_plan = page
            .omo_plan
            .and_then(|plan| bound_todo_plan(plan, RESPONSE_BYTES / 4));
        return wire;
    }
    let mut diagnostics = BrowseDiagnostics {
        source_truncated: page.file_truncated,
        continuation_incomplete: page.continuation_incomplete,
        continuation_reason: page.continuation_reason.clone(),
        ..BrowseDiagnostics::default()
    };
    if page.source_corrupt {
        diagnostics.corrupt_records = 1;
    }
    let mut entries = page.entries;
    let (omitted_tools, omitted_payloads) =
        super::records::normalize_entries_for_response(&mut entries);
    diagnostics.omitted_tools += omitted_tools;
    diagnostics.omitted_payloads += omitted_payloads;
    let next_cursor = if page.has_more {
        entries
            .first()
            .map(|entry| entry.id.clone())
            .unwrap_or_else(|| page.cursor_before.clone())
    } else {
        String::new()
    };
    let mut reason = page.reason.clone();
    if page.file_truncated && reason.is_empty() {
        reason =
            "Showing recent messages. Older history can be loaded from this computer.".to_string();
    }
    let source_revision = native_source_revision(&page.source_path, provider);
    let omo_plan = page
        .omo_plan
        .and_then(|plan| bound_todo_plan(plan, RESPONSE_BYTES / 4));
    let mut wire = BrowsePage {
        available: true,
        reason_code: page.reason_code.clone(),
        reason,
        entries: Vec::new(),
        next_cursor,
        has_more: page.has_more,
        state: BrowseState::Ready,
        mode: if native {
            BrowseMode::Native
        } else {
            BrowseMode::Recent
        },
        source_revision,
        snapshot_id: String::new(),
        total: if page.file_truncated {
            None
        } else {
            Some(page.total)
        },
        progress: None,
        diagnostics,
        error: None,
        omo_plan,
        // Flat readers report the located file itself; Claude reports the
        // chain tip. Anything that captured no statable source leaves it
        // empty — a subscriber then re-reads instead of probing.
        probe_path: if page.probe_path.is_empty() {
            page.source_path.clone()
        } else {
            page.probe_path.clone()
        },
    };
    wire.entries = entries;
    enforce_page_budget(&mut wire);
    wire
}

/// `nativeSourceIdentityRevision` — `sha256(json.Marshal([cleanPath,
/// fileIdentity]))` of the located source; "" when no source was captured.
fn native_source_revision(source_path: &str, provider: &str) -> String {
    if source_path.is_empty() {
        return String::new();
    }
    let _ = provider;
    let clean = super::roots::clean_path(source_path);
    let identity = std::fs::metadata(source_path)
        .map(|info| super::claude::file_identity(&info))
        .unwrap_or_default();
    let json = super::util::go_json_marshal(&serde_json::json!([clean, identity]));
    hex::encode(Sha256::digest(json.as_bytes()))
}

/// `boundTodoPlan` — clamp names/content, then drop tasks/phases from the end
/// until the serialized plan fits the budget.
fn bound_todo_plan(plan: OmoTodoState, budget: usize) -> Option<OmoTodoState> {
    let mut plan = plan;
    for phase in plan.phases.iter_mut() {
        phase.name = clamp_text(&phase.name, 512).0;
        for task in phase.tasks.iter_mut() {
            let (content, truncated) = clamp_text(&task.content, 1024);
            task.content = content;
            plan.truncated = plan.truncated || truncated;
        }
    }
    loop {
        let size = serde_json::to_vec(&plan)
            .map(|v| v.len())
            .unwrap_or(usize::MAX);
        if size <= budget {
            return Some(plan);
        }
        let Some(last) = plan.phases.last_mut() else {
            return Some(plan);
        };
        if !last.tasks.is_empty() {
            last.tasks.pop();
        } else {
            plan.phases.pop();
        }
        plan.truncated = true;
    }
}

/// `enforcePageBudget` — trim trailing tools, halve text, then drop the newest
/// entry until the serialized page fits `ResponseBytes`.
fn enforce_page_budget(page: &mut BrowsePage) {
    while page_size(page) > RESPONSE_BYTES && !page.entries.is_empty() {
        let index = page.entries.len() - 1;
        let mut entry = page.entries[index].clone();
        if !entry.tools.is_empty() {
            entry.tools.pop();
            page.diagnostics.omitted_tools += 1;
            entry.truncated = true;
        } else if !entry.text.is_empty() {
            entry.text = clamp_text(&entry.text, entry.text.len() / 2).0;
            page.diagnostics.omitted_payloads += 1;
            entry.truncated = true;
        } else {
            page.entries.pop();
            continue;
        }
        page.entries[index] = entry;
    }
    if page_size(page) > RESPONSE_BYTES {
        if page.omo_plan.is_some() {
            page.diagnostics.omitted_payloads += 1;
        }
        page.omo_plan = None;
    }
}

/// `pageSize` — serialized length; `usize::MAX` when serialization fails.
fn page_size(page: &BrowsePage) -> usize {
    serde_json::to_vec(page)
        .map(|v| v.len())
        .unwrap_or(usize::MAX)
}

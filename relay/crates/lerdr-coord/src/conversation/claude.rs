//! Claude continuation chains — port of `claude_chain.go` plus the
//! `fileSource`/`captureFileSource`/`fileRevision` helpers from `browser.go`
//! that the chain resolver depends on.

use std::collections::HashSet;
use std::os::unix::fs::FileExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use serde_json::Value;
use sha2::{Digest, Sha256};

use super::reader::{
    contained_regular_file, open_conversation_source, safe_session_id, Error, Reader,
    MAX_CONVERSATION_BYTES,
};
use super::records::{
    collect_jsonl_records_bytes, parse_transcript, DEFAULT_PAGE_SIZE, MAX_PAGE_SIZE,
};
use super::types::{Location, Page};
use super::util::string_value;

const CLAUDE_CONTINUATION_FOOTER_BYTES: i64 = 64 * 1024;
const CLAUDE_CONTINUATION_MAX_SEGMENTS: usize = 64;
const CLAUDE_CONTINUATION_FOOTER_BUDGET: i64 = 4 * 1024 * 1024;
const CLAUDE_CONTINUATION_RECORD_BYTES: i64 = 1024 * 1024;

pub(crate) const CONTINUATION_MISSING_SOURCE: &str = "missing_source";
pub(crate) const CONTINUATION_INVALID_LINK: &str = "invalid_link";
pub(crate) const CONTINUATION_AMBIGUOUS: &str = "ambiguous_link";
pub(crate) const CONTINUATION_CYCLE: &str = "cycle";
pub(crate) const CONTINUATION_LIMIT: &str = "resolution_limit";
pub(crate) const CONTINUATION_PARTIAL: &str = "partial_link";

/// `fileSource` — an open, containment-checked transcript plus its captured
/// boundary (`end`) and content-anchored `revision`.
pub(crate) struct FileSource {
    pub location: Location,
    pub file: std::fs::File,
    pub info: std::fs::Metadata,
    pub end: i64,
    pub revision: String,
}

/// `fileIdentity` — unix stat fields `Dev`/`Ino` joined as the oracle formats
/// them (`"Dev=…,Ino=…"`).
pub(crate) fn file_identity(info: &std::fs::Metadata) -> String {
    format!("Dev={},Ino={}", info.dev(), info.ino())
}

/// `fileChangeToken` — `Ctim={sec nsec}` (Go prints `syscall.Timespec` as
/// `{Sec Nsec}`); falls back to the modification time.
fn file_change_token(info: &std::fs::Metadata) -> String {
    format!("Ctim={{{} {}}}", info.ctime(), info.ctime_nsec())
}

/// `fileRevision` — `sha256(identity + "\x00" + sha256(firstRecord))` where
/// `firstRecord` is the first ≤64 KiB anchor truncated at its first newline.
fn file_revision(file: &std::fs::File, info: &std::fs::Metadata) -> std::io::Result<String> {
    const ANCHOR_BYTES: i64 = 64 * 1024;
    let mut digest = Sha256::new();
    let section_size = info.len().min(ANCHOR_BYTES as u64);
    if section_size > 0 {
        let mut anchor = vec![0u8; section_size as usize];
        let read = file.read_at(&mut anchor, 0)?;
        let mut first_record = &anchor[..read];
        if let Some(newline) = first_record.iter().position(|b| *b == b'\n') {
            first_record = &first_record[..newline + 1];
        }
        digest.update(first_record);
    }
    let mut result = Sha256::new();
    result.update(file_identity(info).as_bytes());
    result.update(b"\x00");
    result.update(hex::encode(digest.finalize()).as_bytes());
    Ok(hex::encode(result.finalize()))
}

/// `openContainedConversationFile` — containment check, no-follow open,
/// regular-file fstat, and a post-open re-resolution to catch swaps.
pub(crate) fn open_contained_conversation_file(
    location: &Location,
) -> std::io::Result<(std::fs::File, std::fs::Metadata)> {
    if location.path.is_empty() || location.root.is_empty() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "conversation source is not contained",
        ));
    }
    let path = Path::new(&location.path);
    let root = Path::new(&location.root);
    let resolved = contained_regular_file(path, root);
    let clean_path_str = super::roots::clean_path(&location.path);
    match &resolved {
        Some(resolved)
            if super::roots::clean_path(&resolved.to_string_lossy()) == clean_path_str => {}
        _ => {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "conversation source is not contained",
            ))
        }
    }
    let file = open_conversation_source(path)?;
    let info = file.metadata()?;
    if !info.is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "conversation source is not a regular file",
        ));
    }
    let resolved_again = std::fs::canonicalize(path).ok();
    let contained_again = contained_regular_file(path, root);
    let unchanged = matches!(&resolved_again, Some(p) if super::roots::clean_path(&p.to_string_lossy()) == clean_path_str)
        && matches!(&contained_again, Some(p) if p.to_string_lossy() == location.path);
    if !unchanged {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "conversation source changed while opening",
        ));
    }
    let path_info = std::fs::metadata(path)?;
    if !path_info.is_file() || path_info.dev() != info.dev() || path_info.ino() != info.ino() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "conversation source changed while opening",
        ));
    }
    Ok((file, info))
}

/// `captureFileSource` — open + stat + revision.
pub(crate) fn capture_file_source(location: &Location) -> std::io::Result<FileSource> {
    let (file, info) = open_contained_conversation_file(location)?;
    let revision = file_revision(&file, &info)?;
    Ok(FileSource {
        location: location.clone(),
        file,
        end: info.len() as i64,
        info,
        revision,
    })
}

/// `readFileRange` — `ReadAt` of exactly `end-start` bytes (EOF is an error).
fn read_file_range(file: &std::fs::File, start: i64, end: i64) -> std::io::Result<Vec<u8>> {
    if start < 0 || end < start {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "invalid file range",
        ));
    }
    let length = (end - start) as usize;
    if length == 0 {
        return Ok(Vec::new());
    }
    let mut data = vec![0u8; length];
    let read = file.read_at(&mut data, start as u64)?;
    if read != length {
        return Err(std::io::Error::new(
            std::io::ErrorKind::UnexpectedEof,
            "unexpected EOF",
        ));
    }
    Ok(data)
}

#[derive(Debug, Default)]
pub(crate) struct ClaudeSegment {
    pub session_id: String,
    pub location: Location,
    pub captured_end: i64,
    pub file_revision: String,
    pub source_change_token: String,
    pub file_identity: String,
    pub footer_start: i64,
    pub footer_end: i64,
}

#[derive(Debug, Default)]
pub(crate) struct ClaudeChain {
    pub segments: Vec<ClaudeSegment>,
    pub incomplete_reason: String,
}

/// `partialClaudeContinuationType` — `"type"\s*:\s*"continued-in"` scanned
/// byte-wise (no regex dependency): find `"type"`, whitespace, `:`, whitespace,
/// `"continued-in"`.
fn has_partial_continuation_type(raw: &[u8]) -> bool {
    const NEEDLE: &[u8] = b"\"type\"";
    const VALUE: &[u8] = b"\"continued-in\"";
    let mut search_from = 0usize;
    while let Some(offset) = find_subslice(&raw[search_from..], NEEDLE) {
        let mut i = search_from + offset + NEEDLE.len();
        while i < raw.len() && raw[i].is_ascii_whitespace() {
            i += 1;
        }
        if i < raw.len() && raw[i] == b':' {
            i += 1;
            while i < raw.len() && raw[i].is_ascii_whitespace() {
                i += 1;
            }
            if raw[i..].starts_with(VALUE) {
                return true;
            }
        }
        search_from = search_from + offset + 1;
    }
    false
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

/// `resolveClaudeChain` — follows same-project `continued-in` footer links
/// from the located anchor; every failure degrades to a `*_reason` flag once
/// the first segment is captured.
pub(crate) fn resolve_claude_chain(
    anchor: &Location,
    anchor_session_id: &str,
) -> Result<ClaudeChain, Error> {
    let anchor_session_id = anchor_session_id.trim();
    if anchor.path.is_empty() || anchor.root.is_empty() || !safe_session_id(anchor_session_id) {
        return Err(Error::new("invalid Claude chain anchor"));
    }
    let project_dir = Path::new(&anchor.path)
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_default();
    let mut chain = ClaudeChain {
        segments: Vec::with_capacity(4),
        ..ClaudeChain::default()
    };
    let mut seen_sessions: HashSet<String> = HashSet::new();
    let mut seen_files: HashSet<PathBuf> = HashSet::new();
    let mut seen_identities: HashSet<String> = HashSet::new();
    let mut location = anchor.clone();
    let mut session_id = anchor_session_id.to_string();
    let mut footer_budget: i64 = 0;

    for _hop in 0..CLAUDE_CONTINUATION_MAX_SEGMENTS {
        let source = match capture_file_source(&location) {
            Ok(source) => source,
            Err(err) => {
                if chain.segments.is_empty() {
                    return Err(Error::new(err.to_string()));
                }
                chain.incomplete_reason = CONTINUATION_MISSING_SOURCE.to_string();
                return Ok(chain);
            }
        };
        let (segment, child_id, inspect_reason, inspect_err) =
            inspect_claude_segment_footer(&source, &session_id);
        let mut identity = file_identity(&source.info);
        if identity.is_empty() {
            identity = super::roots::clean_path(&segment.location.path);
        }
        drop(source);
        if let Some(err) = inspect_err {
            if chain.segments.is_empty() {
                return Err(err);
            }
            chain.incomplete_reason = CONTINUATION_MISSING_SOURCE.to_string();
            return Ok(chain);
        }
        footer_budget += segment.footer_end - segment.footer_start;
        if footer_budget > CLAUDE_CONTINUATION_FOOTER_BUDGET {
            chain.incomplete_reason = CONTINUATION_LIMIT.to_string();
            return Ok(chain);
        }
        let file_key = PathBuf::from(super::roots::clean_path(&segment.location.path));
        if seen_sessions.contains(&session_id)
            || seen_files.contains(&file_key)
            || seen_identities.contains(&identity)
        {
            chain.incomplete_reason = CONTINUATION_CYCLE.to_string();
            return Ok(chain);
        }
        seen_sessions.insert(session_id.clone());
        seen_files.insert(file_key);
        seen_identities.insert(identity.clone());
        let mut segment = segment;
        segment.file_identity = identity;
        chain.segments.push(segment);
        if !inspect_reason.is_empty() {
            chain.incomplete_reason = inspect_reason;
            return Ok(chain);
        }
        if child_id.is_empty() {
            return Ok(chain);
        }
        if chain.segments.len() >= CLAUDE_CONTINUATION_MAX_SEGMENTS {
            chain.incomplete_reason = CONTINUATION_LIMIT.to_string();
            return Ok(chain);
        }
        if !safe_session_id(&child_id) {
            chain.incomplete_reason = CONTINUATION_INVALID_LINK.to_string();
            return Ok(chain);
        }
        let child_path = project_dir.join(format!("{child_id}.jsonl"));
        let resolved = contained_regular_file(&child_path, Path::new(&anchor.root));
        let contained = match &resolved {
            Some(resolved) => {
                let parent = resolved
                    .parent()
                    .map(|p| super::roots::clean_path(&p.to_string_lossy()))
                    .unwrap_or_default();
                parent == super::roots::clean_path(&project_dir.to_string_lossy())
            }
            None => false,
        };
        if !contained {
            chain.incomplete_reason = CONTINUATION_MISSING_SOURCE.to_string();
            return Ok(chain);
        }
        location = Location {
            path: resolved.unwrap().to_string_lossy().into_owned(),
            root: anchor.root.clone(),
            title: String::new(),
        };
        session_id = child_id;
    }
    chain.incomplete_reason = CONTINUATION_LIMIT.to_string();
    Ok(chain)
}

/// `inspectClaudeSegmentFooter` — read the trailing ≤64 KiB, collect records,
/// and extract the single `continued-in` link (or the reason there is none).
fn inspect_claude_segment_footer(
    source: &FileSource,
    session_id: &str,
) -> (ClaudeSegment, String, String, Option<Error>) {
    if !safe_session_id(session_id) {
        return (
            ClaudeSegment::default(),
            String::new(),
            CONTINUATION_MISSING_SOURCE.to_string(),
            Some(Error::new("Claude chain source is unavailable")),
        );
    }
    let end = source.end;
    let mut start = end - CLAUDE_CONTINUATION_FOOTER_BYTES;
    if start < 0 {
        start = 0;
    }
    let footer = match read_file_range(&source.file, start, end) {
        Ok(footer) => footer,
        Err(err) => {
            return (
                ClaudeSegment::default(),
                String::new(),
                CONTINUATION_MISSING_SOURCE.to_string(),
                Some(Error::new(err.to_string())),
            )
        }
    };
    // The oracle also retains `FooterDigest`; it is only consumed by the
    // background evidence machinery this port does not carry.
    let segment = ClaudeSegment {
        session_id: session_id.to_string(),
        location: source.location.clone(),
        captured_end: end,
        file_revision: source.revision.clone(),
        source_change_token: file_change_token(&source.info),
        footer_start: start,
        footer_end: end,
        ..ClaudeSegment::default()
    };
    let mut starts_inside = false;
    if start > 0 {
        let mut previous = [0u8; 1];
        match source.file.read_at(&mut previous, (start - 1) as u64) {
            Ok(1) => {
                starts_inside = previous[0] != b'\n';
            }
            Ok(_) => {}
            Err(err) => {
                return (
                    segment,
                    String::new(),
                    CONTINUATION_MISSING_SOURCE.to_string(),
                    Some(Error::new(err.to_string())),
                )
            }
        }
    }
    if starts_inside && claude_footer_boundary_uninspectable(&footer) {
        return (segment, String::new(), CONTINUATION_LIMIT.to_string(), None);
    }
    let (records, oversized) = collect_jsonl_records_bytes(
        &footer,
        start,
        starts_inside,
        CLAUDE_CONTINUATION_RECORD_BYTES,
    );
    if oversized > 0 {
        return (segment, String::new(), CONTINUATION_LIMIT.to_string(), None);
    }
    let mut links: HashSet<String> = HashSet::new();
    let mut invalid_link = false;
    let mut partial_link = false;
    for record in &records {
        if record.oversized || record.raw.len() as i64 > CLAUDE_CONTINUATION_FOOTER_BYTES {
            return (segment, String::new(), CONTINUATION_LIMIT.to_string(), None);
        }
        let parsed = serde_json::from_slice::<Value>(&record.raw);
        if !record.complete || parsed.is_err() {
            if record.trailing && has_partial_continuation_type(&record.raw) {
                partial_link = true;
            }
            continue;
        }
        let raw = parsed.unwrap();
        let Some(raw) = raw.as_object() else {
            continue;
        };
        if string_value(raw.get("type").unwrap_or(&Value::Null)) != "continued-in"
            || raw.get("isSidechain") == Some(&Value::Bool(true))
        {
            continue;
        }
        if let Some(value) = raw.get("sessionId") {
            let provided = string_value(value);
            // A non-string or mismatched session marker is not safe evidence.
            if !value.is_string() || provided.is_empty() || provided != session_id {
                invalid_link = true;
                continue;
            }
        }
        let child = string_value(raw.get("continuedInSessionId").unwrap_or(&Value::Null))
            .trim()
            .to_string();
        if child.is_empty() || !safe_session_id(&child) {
            invalid_link = true;
            continue;
        }
        links.insert(child);
    }
    if links.len() > 1 {
        return (
            segment,
            String::new(),
            CONTINUATION_AMBIGUOUS.to_string(),
            None,
        );
    }
    if invalid_link {
        return (
            segment,
            String::new(),
            CONTINUATION_INVALID_LINK.to_string(),
            None,
        );
    }
    if links.is_empty() {
        if partial_link {
            return (
                segment,
                String::new(),
                CONTINUATION_PARTIAL.to_string(),
                None,
            );
        }
        return (segment, String::new(), String::new(), None);
    }
    let child = links.into_iter().next().unwrap();
    (segment, child, String::new(), None)
}

/// `claudeFooterBoundaryUninspectableBytes` — true when the window opens
/// mid-record and that record cannot be safely skipped (no newline, a newline
/// only at the last byte, or a boundary line longer than the 64 KiB window).
fn claude_footer_boundary_uninspectable(footer: &[u8]) -> bool {
    let Some(newline) = footer.iter().position(|b| *b == b'\n') else {
        return true;
    };
    let line_bytes = newline as i64 + 1;
    if newline + 1 >= footer.len() {
        return true;
    }
    line_bytes > CLAUDE_CONTINUATION_FOOTER_BYTES
}

/// `readCapturedSegment` — re-capture and verify the descriptor, then read at
/// most `limit` bytes ending at `captured_end` (dropping a partial first line
/// when clipped).
fn read_captured_segment(segment: &ClaudeSegment, limit: i64) -> Result<(Vec<u8>, bool), Error> {
    let source =
        capture_file_source(&segment.location).map_err(|err| Error::new(err.to_string()))?;
    if source.revision != segment.file_revision
        || source.end < segment.captured_end
        || (source.end == segment.captured_end
            && !segment.source_change_token.is_empty()
            && file_change_token(&source.info) != segment.source_change_token)
    {
        return Err(Error::new("Claude chain source changed"));
    }
    let end = segment.captured_end;
    if end == 0 {
        return Ok((Vec::new(), false));
    }
    if limit < 1 {
        return Err(Error::new("invalid Claude segment budget"));
    }
    let clipped = end > limit;
    let start = if clipped { end - limit } else { 0 };
    let mut data =
        read_file_range(&source.file, start, end).map_err(|err| Error::new(err.to_string()))?;
    if clipped {
        match data.iter().position(|b| *b == b'\n') {
            Some(newline) => {
                data.drain(..=newline);
            }
            None => data.clear(),
        };
    }
    Ok((data, clipped))
}

/// `namespaceClaudeEntries` — non-anchor segment ids are prefixed with
/// `sha256(sessionID + "\x00" + fileRevision)[:12]hex + "-"`.
fn namespace_claude_entries(
    entries: &mut [super::types::Entry],
    segment: &ClaudeSegment,
    anchor: bool,
) {
    if anchor || entries.is_empty() {
        return;
    }
    let mut hasher = Sha256::new();
    hasher.update(segment.session_id.as_bytes());
    hasher.update(b"\x00");
    hasher.update(segment.file_revision.as_bytes());
    let digest = hasher.finalize();
    let prefix = hex::encode(digest)[..12].to_string();
    for entry in entries.iter_mut() {
        entry.id = format!("{prefix}-{}", entry.id);
    }
}

/// `continuationDiagnostic`.
pub(crate) fn continuation_diagnostic(reason: &str) -> (bool, String) {
    match reason {
        CONTINUATION_MISSING_SOURCE
        | CONTINUATION_INVALID_LINK
        | CONTINUATION_AMBIGUOUS
        | CONTINUATION_CYCLE
        | CONTINUATION_LIMIT
        | CONTINUATION_PARTIAL => (true, reason.to_string()),
        _ => (false, String::new()),
    }
}

impl Reader {
    /// `readClaudeChain` — resolve the chain, read newest→oldest under the
    /// shared 16 MiB budget, and page the concatenated projection. An unknown
    /// `before` yields `source_changed` on an empty available page.
    pub(crate) fn claude_read_chain(
        &self,
        session_id: &str,
        anchor: Location,
        before: &str,
        limit: usize,
    ) -> Result<Page, Error> {
        let mut chain = resolve_claude_chain(&anchor, session_id)
            .map_err(|err| Error::new(format!("resolve Claude conversation chain: {err}")))?;
        if chain.segments.is_empty() {
            return Ok(Page::unavailable(
                "invalid_session",
                "No conversation log is available for this session.",
            ));
        }
        let limit = if limit < 1 {
            DEFAULT_PAGE_SIZE
        } else {
            limit.min(MAX_PAGE_SIZE)
        };
        let mut groups: Vec<Vec<super::types::Entry>> =
            (0..chain.segments.len()).map(|_| Vec::new()).collect();
        let mut remaining = MAX_CONVERSATION_BYTES;
        let mut clipped = false;
        for index in (0..chain.segments.len()).rev() {
            if remaining <= 0 {
                clipped = true;
                break;
            }
            let segment = &chain.segments[index];
            let budget = remaining.min(segment.captured_end);
            let (text, segment_clipped) = match read_captured_segment(segment, budget) {
                Ok(result) => result,
                Err(err) => {
                    if index == chain.segments.len() - 1 {
                        return Err(Error::new(format!(
                            "read Claude conversation segment: {err}"
                        )));
                    }
                    chain.incomplete_reason = CONTINUATION_MISSING_SOURCE.to_string();
                    clipped = true;
                    break;
                }
            };
            let mut entries = parse_transcript("claude", &text);
            namespace_claude_entries(&mut entries, segment, index == 0);
            groups[index] = entries;
            let consumed = segment.captured_end.min(remaining);
            remaining -= consumed;
            clipped = clipped || segment_clipped;
            if segment_clipped {
                if index > 0 {
                    clipped = true;
                }
                break;
            }
        }
        let mut entries = Vec::new();
        for group in groups {
            entries.extend(group);
        }
        let mut end = entries.len();
        if !before.is_empty() {
            let mut found = false;
            for (index, entry) in entries.iter().enumerate() {
                if entry.id == before {
                    end = index;
                    found = true;
                    break;
                }
            }
            if !found {
                return Ok(Page {
                    available: true,
                    reason_code: "source_changed".to_string(),
                    reason: "The conversation chain changed; reload history.".to_string(),
                    entries: Vec::new(),
                    ..Page::default()
                });
            }
        }
        let start = end.saturating_sub(limit);
        let mut page = Page {
            available: true,
            entries: entries[start..end].to_vec(),
            has_more: start > 0,
            total: entries.len() as i64,
            file_truncated: clipped,
            source_path: anchor.path.clone(),
            ..Page::default()
        };
        if !chain.incomplete_reason.is_empty() {
            let (incomplete, reason) = continuation_diagnostic(&chain.incomplete_reason);
            page.continuation_incomplete = incomplete;
            page.continuation_reason = reason;
        }
        Ok(page)
    }
}

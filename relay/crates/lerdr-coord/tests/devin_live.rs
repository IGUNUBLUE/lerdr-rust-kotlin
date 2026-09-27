//! Live smoke test for the Devin (ATIF) transcript reader against the real
//! `~/.local/share/devin/cli/transcripts/` tree.
//!
//! Gated: only runs with `LERDR_DEVIN_LIVE=1` — it reads operator state and
//! is skipped in CI. Picks the most recently modified transcript so any
//! current Devin CLI session satisfies it.

use std::path::PathBuf;

use lerdr_coord::conversation::Reader;

#[test]
fn devin_real_transcript() {
    if std::env::var("LERDR_DEVIN_LIVE").as_deref() != Ok("1") {
        eprintln!("skipped: set LERDR_DEVIN_LIVE=1 to run against real transcripts");
        return;
    }
    let home = PathBuf::from(std::env::var("HOME").expect("HOME unset"));
    let transcripts = home.join(".local/share/devin/cli/transcripts");
    let mut newest: Option<(std::time::SystemTime, PathBuf)> = None;
    for entry in std::fs::read_dir(&transcripts).expect("transcripts dir missing") {
        let path = entry.expect("read_dir").path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let mtime = path
            .metadata()
            .and_then(|m| m.modified())
            .unwrap_or(std::time::UNIX_EPOCH);
        if newest.as_ref().is_none_or(|(t, _)| mtime > *t) {
            newest = Some((mtime, path));
        }
    }
    let (_, path) = newest.expect("no devin transcripts found");
    let session_id = path
        .file_stem()
        .and_then(|s| s.to_str())
        .expect("session slug")
        .to_string();
    eprintln!("reading {session_id} ({})", path.display());

    let reader = Reader::new(home);
    let page = reader
        .read("devin", &session_id, None, 10)
        .expect("read devin transcript");
    assert!(page.available, "page unavailable: {}", page.reason_code);
    assert!(
        !page.entries.is_empty(),
        "transcript parsed to zero entries"
    );
    for entry in &page.entries {
        assert!(
            entry.role == "user" || entry.role == "assistant",
            "unexpected role {:?} in step {}",
            entry.role,
            entry.id
        );
    }
    eprintln!(
        "available total={} tail={:?}",
        page.total,
        page.entries
            .iter()
            .map(|e| e.id.as_str())
            .collect::<Vec<_>>()
    );
}

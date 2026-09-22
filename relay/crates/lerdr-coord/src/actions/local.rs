//! Local-only actions — `list_directories` (`fsutil.ListDirectories`) and
//! `qr_code` (`setuphelper.PackedQR`). Neither touches Herdr; both answer
//! with a bare `command_result` like the oracle's `sendCommandResult`.

use std::path::{Path, PathBuf};

use lerdr_core::json::{MaybeNull, RawJson};
use lerdr_core::protocol::{CommandResultMessage, Inbound, Outbound};
use serde::Serialize;

/// `setuphelper.MaxQRBytes` — the pairing payload bound.
const MAX_QR_BYTES: usize = 512;

/// Bare `command_result` — the shared shape for the no-receipt actions.
pub(crate) fn command_result(
    request_id: &str,
    action: &str,
    ok: bool,
    phase: &str,
    error: &str,
    pane_id: &str,
    data: Option<serde_json::Value>,
) -> Outbound {
    Outbound::CommandResult(CommandResultMessage {
        r#type: "command_result".to_owned(),
        request_id: (!request_id.is_empty()).then(|| request_id.to_owned()),
        action: Some(action.to_owned()),
        ok: Some(ok),
        phase: Some(phase.to_owned()),
        error: Some(error.to_owned()),
        pane_id: Some(pane_id.to_owned()),
        data: data.and_then(|value| {
            serde_json::value::to_raw_value(&value)
                .ok()
                .map(|raw| MaybeNull::Value(RawJson(raw)))
        }),
    })
}

#[derive(Serialize)]
struct DirEntry {
    name: String,
    path: String,
}

#[derive(Serialize)]
struct Current {
    path: String,
    label: String,
}

#[derive(Serialize)]
struct DirListing {
    current: Current,
    parent: String,
    directories: Vec<DirEntry>,
}

/// `list_directories` — jail `path` under the resolved home, list non-hidden
/// directories that also resolve under home, case-insensitive name sort.
pub(crate) fn list_directories(request_id: &str, message: &Inbound) -> Outbound {
    let home = home_dir()
        .and_then(|h| std::fs::canonicalize(h).ok())
        .unwrap_or_else(|| PathBuf::from("."));
    let listing = list_directories_in(&home, &message.path);
    command_result(
        request_id,
        "list_directories",
        true,
        "completed",
        "",
        "",
        Some(serde_json::to_value(&listing).unwrap_or_default()),
    )
}

/// `fsutil.ListDirectories` — the jail + listing; `home` must already be
/// resolved.
fn list_directories_in(home: &Path, path: &str) -> DirListing {
    let path = path.trim();
    let mut candidate = if path.is_empty() {
        home.to_path_buf()
    } else {
        PathBuf::from(path)
    };
    if candidate.is_relative() {
        candidate = home.join(candidate);
    }
    // Clean "." / ".." before resolving so a missing final component still
    // lands somewhere meaningful.
    let candidate = normalize(&candidate);
    let mut resolved = std::fs::canonicalize(&candidate).unwrap_or(candidate);
    if !resolved.starts_with(home) {
        resolved = home.to_path_buf();
    }

    let mut listing = DirListing {
        current: Current {
            path: resolved.to_string_lossy().into_owned(),
            label: display_path(&resolved, home),
        },
        parent: String::new(),
        directories: Vec::new(),
    };
    if resolved != home {
        listing.parent = resolved
            .parent()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default();
    }
    let Ok(entries) = std::fs::read_dir(&resolved) else {
        return listing;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') {
            continue;
        }
        // `EvalSymlinks` + jail check, then `IsDir` on the resolved path —
        // a symlink to a directory inside home counts, one escaping it is
        // dropped.
        let Ok(child) = std::fs::canonicalize(entry.path()) else {
            continue;
        };
        if !child.starts_with(home) {
            continue;
        }
        let Ok(meta) = std::fs::metadata(&child) else {
            continue;
        };
        if !meta.is_dir() {
            continue;
        }
        listing.directories.push(DirEntry {
            name,
            path: resolved
                .join(entry.file_name())
                .to_string_lossy()
                .into_owned(),
        });
    }
    listing.directories.sort_by_key(|e| e.name.to_lowercase());
    listing
}

/// `filepath.Clean` on a possibly non-existent path — the pieces `Path`'s
/// lexical cleanup needs before `canonicalize`.
fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::ParentDir => {
                out.pop();
            }
            std::path::Component::CurDir => {}
            other => out.push(other),
        }
    }
    out
}

/// `displayPath` — `~` for home, `~/x` inside it, absolute otherwise.
fn display_path(path: &Path, home: &Path) -> String {
    if path == home {
        return "~".to_owned();
    }
    if let Ok(rest) = path.strip_prefix(home) {
        return format!("~/{}", rest.to_string_lossy());
    }
    path.to_string_lossy().into_owned()
}

pub(crate) fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

/// `qr_code` — `PackedQR`: EC level M, no quiet zone, modules packed
/// row-major MSB-first and base64'd.
pub(crate) fn qr_code(request_id: &str, message: &Inbound) -> Outbound {
    let fail = || {
        command_result(
            request_id,
            "qr_code",
            false,
            "failed",
            "This computer could not encode that QR code",
            "",
            None,
        )
    };
    let value = message.text.as_str();
    if value.trim().is_empty() || value.len() > MAX_QR_BYTES {
        return fail();
    }
    let Ok(code) = qrcode::QrCode::with_error_correction_level(value, qrcode::EcLevel::M) else {
        return fail();
    };
    // `DisableBorder` ↔ the raw module matrix (no quiet-zone arg on
    // `to_colors`); dark modules are `Color::Dark`.
    let colors = code.to_colors();
    let size = code.width();
    if size == 0 {
        return fail();
    }
    let mut packed = vec![0u8; (size * size).div_ceil(8)];
    for (index, color) in colors.iter().enumerate() {
        if *color == qrcode::Color::Dark {
            packed[index / 8] |= 1 << (7 - index % 8);
        }
    }
    use base64::Engine;
    command_result(
        request_id,
        "qr_code",
        true,
        "completed",
        "",
        "",
        Some(serde_json::json!({
            "size": size,
            "modules": base64::engine::general_purpose::STANDARD.encode(packed),
        })),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use lerdr_core::protocol::Inbound;

    fn inbound(fields: serde_json::Value) -> Inbound {
        serde_json::from_value(fields).expect("inbound")
    }

    fn result_of(out: Outbound) -> CommandResultMessage {
        match out {
            Outbound::CommandResult(m) => m,
            other => panic!("expected command_result, got {other:?}"),
        }
    }

    #[test]
    fn list_directories_lists_non_hidden_dirs_under_home() {
        let tmp = tempfile();
        std::fs::create_dir_all(tmp.join("alpha")).unwrap();
        std::fs::create_dir_all(tmp.join("Beta")).unwrap();
        std::fs::create_dir_all(tmp.join(".hidden")).unwrap();
        std::fs::write(tmp.join("file.txt"), b"x").unwrap();
        let home = std::fs::canonicalize(&tmp).unwrap();
        let listing = list_directories_in(&home, "");
        let parsed = serde_json::to_value(&listing).unwrap();
        let names: Vec<&str> = parsed["directories"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["name"].as_str().unwrap())
            .collect();
        assert_eq!(names, vec!["alpha", "Beta"]);
        assert_eq!(parsed["current"]["label"], "~");
        // A path that escapes home falls back to home.
        let escaped = list_directories_in(&home, "..");
        assert_eq!(
            serde_json::to_value(&escaped).unwrap()["current"]["label"],
            "~"
        );
        std::fs::remove_dir_all(&tmp).ok();
    }

    fn tempfile() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "lerdr-listdir-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn qr_code_packs_modules_base64() {
        let out = qr_code(
            "r1",
            &inbound(serde_json::json!({ "text": "lerdr://pair" })),
        );
        let result = result_of(out);
        assert_eq!(result.ok, Some(true));
        let data: serde_json::Value =
            serde_json::from_str(result.data.unwrap().value().expect("qr data").get()).unwrap();
        assert!(data["size"].as_i64().unwrap() > 0);
        assert!(!data["modules"].as_str().unwrap().is_empty());
    }

    #[test]
    fn qr_code_rejects_empty_and_oversize() {
        let out = qr_code("r1", &inbound(serde_json::json!({ "text": "   " })));
        assert_eq!(result_of(out).ok, Some(false));
        let out = qr_code(
            "r1",
            &inbound(serde_json::json!({ "text": "x".repeat(513) })),
        );
        assert_eq!(result_of(out).ok, Some(false));
    }
}

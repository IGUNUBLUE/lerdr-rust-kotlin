//! Release management — a port of the oracle's `internal/release/manifest.go`
//! plus the `internal/update` helpers the CLI surface exposes
//! (`Activate`, `PruneOldReleases`).
//!
//! The schema-1 `release-manifest.json` records a bundle's identity
//! (version/revision/target), its transport capabilities, and a sha256 map
//! of every regular file in the tree. [`verify`] re-hashes the tree and
//! enforces canonical slash paths, the required-file list, and an
//! executable relay binary; [`build`] stamps a staged tree; [`seal`] makes
//! an installed tree read-only.
//!
//! Deliberate deviations from the Go oracle — both match
//! `scripts/release-manifest.py`, the mirror that owns the *Rust* bundle
//! contract (`package-release.sh` stages `lerdr-relay` + `scripts/*.sh`, no
//! PWA):
//!
//! - `REQUIRED_FILES` names the Rust tarball's contents; the Go list names
//!   its own bundle (`lerdr`, `web/index.html`, `LICENSE`, `relay/*.sh`).
//! - `web_hash` is honest-when-present: a bundle without `web/` entries
//!   verifies (the Go verifier hard-requires a web bundle). A manifest that
//!   does claim `web_hash` must still match its `web/` files exactly.
//! - `VerifyWebDescriptor` is not ported — Rust bundles never carry
//!   `web/release.json`.

use std::collections::{BTreeMap, HashSet};
use std::ffi::OsString;
use std::fs;
use std::io::{self, Read, Write};
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Deserializer, Serialize};
use sha2::{Digest, Sha256};

use lerdr_core::protocol::ENCRYPTED_WEBSOCKET_SUBPROTOCOL;

pub const MANIFEST_NAME: &str = "release-manifest.json";
pub const MANIFEST_SCHEMA: i64 = 1;

/// The bundle contract for the Rust tarball — `release-manifest.py`'s
/// `REQUIRED_FILES` (binary + README + the operator-facing script set).
const REQUIRED_FILES: &[&str] = &[
    "lerdr-relay",
    "README.md",
    "scripts/common.sh",
    "scripts/plugin-on-event.sh",
    "scripts/plugin-on-startup.sh",
    "scripts/setup-link.sh",
    "scripts/tailscale-serve.sh",
    "scripts/tailscale-service.sh",
];

/// The executable whose mode gets the final `&0o111` check.
const RELAY_BINARY: &str = "lerdr-relay";

/// Verification/management failures — messages keep the oracle's wording
/// (`manifest_test.go` asserts on substrings like "hash mismatch").
#[derive(Debug, thiserror::Error)]
pub enum ReleaseError {
    /// Bare I/O failure; callers wrap with context where the oracle does
    /// (`verify %s: %w` style).
    #[error(transparent)]
    Io(#[from] io::Error),
    /// Validation or verification failure.
    #[error("{0}")]
    Failed(String),
}

fn failed(message: impl Into<String>) -> ReleaseError {
    ReleaseError::Failed(message.into())
}

/// `release.Manifest` — schema 1. Field order matches the Go struct so the
/// emitted JSON reads identically; `omitempty` maps to `skip_serializing_if`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    #[serde(default, deserialize_with = "null_default")]
    pub schema: i64,
    #[serde(default, deserialize_with = "null_default")]
    pub version: String,
    #[serde(default, deserialize_with = "null_default")]
    pub revision: String,
    #[serde(default, deserialize_with = "null_default")]
    pub target: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub web_hash: Option<String>,
    #[serde(
        default,
        deserialize_with = "null_default",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub app_transports: Vec<String>,
    #[serde(
        default,
        deserialize_with = "null_default",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub relay_transports: Vec<String>,
    #[serde(default, deserialize_with = "null_default")]
    pub files: BTreeMap<String, String>,
}

/// Go's `encoding/json` treats `null` as a no-op for non-pointer fields —
/// a `"files": null` manifest must fail on the required-field check, not on
/// a serde type error.
fn null_default<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: Default + Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer).map(Option::unwrap_or_default)
}

/// `release.CurrentTarget` — `runtime.GOOS + "/" + runtime.GOARCH` spelled
/// the Go way (darwin/amd64, not macos/x86_64).
pub fn current_target() -> String {
    let os = match std::env::consts::OS {
        "macos" => "darwin",
        other => other,
    };
    let arch = match std::env::consts::ARCH {
        "x86_64" => "amd64",
        "aarch64" => "arm64",
        "x86" => "386",
        other => other,
    };
    format!("{os}/{arch}")
}

/// The compile-time identity stamp — the oracle's `main.version` /
/// `main.revision` ldflags slots plus `release.CurrentTarget`. Injected into
/// [`verify_identity`] so tests can stamp arbitrary builds.
#[derive(Debug)]
pub struct BinaryStamp {
    pub version: &'static str,
    pub revision: &'static str,
    pub target: String,
}

pub fn binary_stamp() -> BinaryStamp {
    BinaryStamp {
        version: lerdr_core::release_version(),
        revision: option_env!("LERDR_REVISION").unwrap_or("dev"),
        target: current_target(),
    }
}

/// `release.Load` — read and shape-check `release-manifest.json`.
pub fn load(root: &Path) -> Result<Manifest, ReleaseError> {
    let data = fs::read(root.join(MANIFEST_NAME))
        .map_err(|e| failed(format!("read release manifest: {e}")))?;
    let manifest: Manifest = serde_json::from_slice(&data)
        .map_err(|e| failed(format!("parse release manifest: {e}")))?;
    if manifest.schema != MANIFEST_SCHEMA {
        return Err(failed(format!(
            "unsupported release manifest schema {}",
            manifest.schema
        )));
    }
    if manifest.version.trim().is_empty() || manifest.revision.trim().is_empty() {
        return Err(failed("release manifest version and revision are required"));
    }
    if manifest.target.is_empty() || manifest.files.is_empty() {
        return Err(failed("release manifest target and files are required"));
    }
    Ok(manifest)
}

/// `release.Verify` — manifest shape, file hashes, `web_hash`, the tree
/// walk (no unlisted files, no links, nothing non-regular), the
/// required-file list, and the executable bit.
pub fn verify(root: &Path, expected_target: &str) -> Result<Manifest, ReleaseError> {
    let root = abs_path(root)?;
    let manifest = load(&root)?;
    if !expected_target.is_empty() && manifest.target != expected_target {
        return Err(failed(format!(
            "release target {:?} does not match {expected_target:?}",
            manifest.target
        )));
    }

    let mut listed: HashSet<String> = HashSet::with_capacity(manifest.files.len());
    for (name, expected) in &manifest.files {
        let clean = clean_relative(name)
            .map_err(|e| failed(format!("invalid manifest path {name:?}: {e}")))?;
        if clean != *name {
            return Err(failed(format!("manifest path {name:?} is not canonical")));
        }
        if !valid_sha256(expected) {
            return Err(failed(format!("invalid SHA-256 for {name}")));
        }
        let actual =
            hash_regular_file(&root, &clean).map_err(|e| failed(format!("verify {name}: {e}")))?;
        if actual != expected.to_lowercase() {
            return Err(failed(format!("hash mismatch for {name}")));
        }
        listed.insert(clean);
    }
    // `release-manifest.py` semantics: a `web/` bundle may be absent, but a
    // claimed web_hash must match the manifest's web/ entries exactly.
    if manifest.web_hash != hash_file_map(&manifest.files, "web/") {
        return Err(failed(
            "release manifest web hash does not match its web files",
        ));
    }

    walk_tree(&root, |path, file_type| {
        if path == root {
            return Ok(());
        }
        let relative = slash_relative(&root, path);
        if file_type.is_symlink() {
            return Err(failed(format!("release contains symlink {relative}")));
        }
        if file_type.is_dir() {
            return Ok(());
        }
        if relative == MANIFEST_NAME {
            return Ok(());
        }
        if !file_type.is_file() {
            return Err(failed(format!(
                "release contains non-regular file {relative}"
            )));
        }
        if !listed.contains(relative.as_str()) {
            return Err(failed(format!(
                "release file is not listed in manifest: {relative}"
            )));
        }
        Ok(())
    })?;

    for required in REQUIRED_FILES {
        if !listed.contains(*required) {
            return Err(failed(format!("release manifest is missing {required}")));
        }
    }
    let executable = fs::metadata(root.join(RELAY_BINARY))
        .map(|meta| is_executable(&meta))
        .unwrap_or(false);
    if !executable {
        return Err(failed("release relay binary is not executable"));
    }
    Ok(manifest)
}

/// `verifyReleaseIdentity` — the expected-* flags are candidate filters; the
/// binary's own stamp is always authoritative. `--allow-cross-target`
/// relaxes only the target leg so a build host can verify another
/// platform's bundle.
pub fn verify_identity(
    manifest: &Manifest,
    expected_version: &str,
    expected_revision: &str,
    expected_target: &str,
    allow_cross_target: bool,
    binary: &BinaryStamp,
) -> Result<(), ReleaseError> {
    if !expected_version.is_empty() && manifest.version != expected_version {
        return Err(failed(format!(
            "release manifest version {:?} does not match expected version {expected_version:?}",
            manifest.version
        )));
    }
    if !expected_revision.is_empty() && manifest.revision != expected_revision {
        return Err(failed(format!(
            "release manifest revision {:?} does not match expected revision {expected_revision:?}",
            manifest.revision
        )));
    }
    if !expected_target.is_empty() && manifest.target != expected_target {
        return Err(failed(format!(
            "release manifest target {:?} does not match expected target {expected_target:?}",
            manifest.target
        )));
    }
    if manifest.version != binary.version {
        return Err(failed(format!(
            "release manifest version {:?} does not match binary version {:?}",
            manifest.version, binary.version
        )));
    }
    if manifest.revision != binary.revision {
        return Err(failed(format!(
            "release manifest revision {:?} does not match binary revision {:?}",
            manifest.revision, binary.revision
        )));
    }
    if !allow_cross_target && manifest.target != binary.target {
        return Err(failed(format!(
            "release manifest target {:?} does not match binary target {:?}",
            manifest.target, binary.target
        )));
    }
    Ok(())
}

/// `release.Build` — hash every regular file under `root` into a schema-1
/// manifest, stamp the release identity, and write `release-manifest.json`.
pub fn build(
    root: &Path,
    version: &str,
    revision: &str,
    target: &str,
) -> Result<Manifest, ReleaseError> {
    if version.trim().is_empty() || revision.trim().is_empty() || target.trim().is_empty() {
        return Err(failed("version, revision, and target are required"));
    }
    let root = abs_path(root)?;
    let mut files = BTreeMap::new();
    walk_tree(&root, |path, file_type| {
        if path == root {
            return Ok(());
        }
        let relative = slash_relative(&root, path);
        if file_type.is_symlink() {
            return Err(failed(format!("release contains symlink {relative}")));
        }
        if file_type.is_dir() || relative == MANIFEST_NAME {
            return Ok(());
        }
        if !file_type.is_file() {
            return Err(failed(format!(
                "release contains non-regular file {relative}"
            )));
        }
        let hash = hash_regular_file(&root, &relative)?;
        files.insert(relative, hash);
        Ok(())
    })?;
    // This release supports the encrypted WebSocket path only; like the
    // oracle it deliberately does not claim the retired E2EE v1.
    let manifest = Manifest {
        schema: MANIFEST_SCHEMA,
        version: version.to_string(),
        revision: revision.to_string(),
        target: target.to_string(),
        web_hash: hash_file_map(&files, "web/"),
        app_transports: vec![ENCRYPTED_WEBSOCKET_SUBPROTOCOL.to_string()],
        relay_transports: vec![ENCRYPTED_WEBSOCKET_SUBPROTOCOL.to_string()],
        files,
    };
    write_manifest(&root, &manifest)?;
    Ok(manifest)
}

/// `release.Seal` — verify first (any target), then strip write permission
/// across the tree so platform metadata (Finder's `.DS_Store` et al.) can't
/// invalidate the manifest between install and activation.
pub fn seal(root: &Path) -> Result<(), ReleaseError> {
    verify(root, "")?;
    walk_tree(root, |path, _file_type| {
        let info = fs::symlink_metadata(path)?;
        set_readonly_mode(path, &info)
    })
}

/// `update.Activate` — swap `RELEASE_ROOT/current` onto `release_dir` via a
/// temp symlink + rename (atomic), storing the *relative* path so the link
/// survives a relocated root.
pub fn activate(release_root: &Path, release_dir: &Path) -> Result<(), ReleaseError> {
    let relative = rel_path(release_root, release_dir)
        .map_err(|_| failed("release directory is outside release root"))?;
    if relative.starts_with("..") {
        return Err(failed("release directory is outside release root"));
    }
    let temp = release_root.join(format!(".current-{}", std::process::id()));
    let _ = fs::remove_file(&temp);
    create_dir_link(&relative, &temp)?;
    let outcome = fs::rename(&temp, release_root.join("current"));
    let _ = fs::remove_file(&temp);
    outcome?;
    Ok(())
}

/// `update.PruneOldReleases` — delete stale releases under
/// `<root>/releases`, keeping the named directories, `.update-*` inflight
/// dirs, and anything that fails verification (it may still be running).
pub fn prune_old_releases(release_root: &Path, keep: &[PathBuf]) -> Result<(), ReleaseError> {
    if !release_root.is_absolute() || clean_path(release_root) == Path::new("/") {
        return Err(failed("release root must be a non-root absolute path"));
    }
    let releases_dir = release_root.join("releases");
    let mut kept: HashSet<PathBuf> = HashSet::with_capacity(keep.len());
    for item in keep {
        if item.as_os_str().is_empty() {
            continue;
        }
        if let Ok(absolute) = abs_path(item) {
            kept.insert(absolute);
        }
    }
    for entry in fs::read_dir(&releases_dir)? {
        let entry = entry?;
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if !file_type.is_dir() || file_type.is_symlink() {
            continue;
        }
        if entry.file_name().to_string_lossy().starts_with(".update-") {
            continue;
        }
        let candidate = releases_dir.join(entry.file_name());
        match abs_path(&candidate) {
            Ok(absolute) if kept.contains(&absolute) => continue,
            Err(_) => continue,
            _ => {}
        }
        if verify(&candidate, &current_target()).is_err() {
            continue;
        }
        make_release_directories_writable(&candidate)?;
        fs::remove_dir_all(&candidate)?;
    }
    Ok(())
}

/// `makeReleaseDirectoriesWritable` — owner rwx on every directory so
/// `remove_dir_all` can empty a sealed tree.
fn make_release_directories_writable(root: &Path) -> Result<(), ReleaseError> {
    walk_tree(root, |path, file_type| {
        if !file_type.is_dir() {
            return Ok(());
        }
        add_owner_write(path)
    })
}

// ---------------------------------------------------------------------------
// internals

/// `filepath.WalkDir` semantics for release trees: lstat types (links are
/// never followed — a symlinked *root* yields itself and nothing else),
/// children visited in lexical filename order.
fn walk_tree(
    root: &Path,
    mut f: impl FnMut(&Path, fs::FileType) -> Result<(), ReleaseError>,
) -> Result<(), ReleaseError> {
    fn descend(
        path: &Path,
        file_type: fs::FileType,
        f: &mut dyn FnMut(&Path, fs::FileType) -> Result<(), ReleaseError>,
    ) -> Result<(), ReleaseError> {
        f(path, file_type)?;
        if !file_type.is_dir() {
            return Ok(());
        }
        let mut entries = fs::read_dir(path)?.collect::<Result<Vec<_>, io::Error>>()?;
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            descend(&entry.path(), entry.file_type()?, f)?;
        }
        Ok(())
    }
    let file_type = fs::symlink_metadata(root)?.file_type();
    descend(root, file_type, &mut f)
}

/// `filepath.Rel(root, path)` + `ToSlash` for paths beneath a walked root.
fn slash_relative(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .components()
        .filter_map(|component| match component {
            Component::Normal(part) => Some(part.to_string_lossy()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("/")
}

/// `cleanRelative` — non-empty slash-separated relative path that cannot
/// escape the root; callers compare `clean == name` for canonicality.
fn clean_relative(name: &str) -> Result<String, ReleaseError> {
    if name.is_empty() || name.contains('\\') || name.starts_with('/') {
        return Err(failed(
            "path must be a non-empty slash-separated relative path",
        ));
    }
    let clean = clean_slash(name);
    if clean == "." || clean == ".." || clean.starts_with("../") {
        return Err(failed("path escapes release root"));
    }
    Ok(clean)
}

/// `path.Clean` for slash paths — always relative by the time it runs
/// (a leading `/` is rejected by [`clean_relative`]).
fn clean_slash(name: &str) -> String {
    let mut out: Vec<&str> = Vec::new();
    for part in name.split('/') {
        match part {
            "" | "." => {}
            ".." => match out.last() {
                Some(&last) if last != ".." => {
                    out.pop();
                }
                _ => out.push(".."),
            },
            part => out.push(part),
        }
    }
    if out.is_empty() {
        ".".to_string()
    } else {
        out.join("/")
    }
}

/// `filepath.Clean` for filesystem paths — lexical only; never resolves
/// symlinks (a `current` link stays a link, which is what the walk relies
/// on to skip it).
fn clean_path(path: &Path) -> PathBuf {
    let rooted = path.is_absolute();
    let mut out: Vec<OsString> = Vec::new();
    for component in path.components() {
        match component {
            Component::Prefix(_) | Component::RootDir | Component::CurDir => {}
            Component::ParentDir => match out.last().map(OsString::as_os_str) {
                Some(last) if last != ".." => {
                    out.pop();
                }
                None if rooted => {}
                _ => out.push(OsString::from("..")),
            },
            Component::Normal(part) => out.push(part.to_os_string()),
        }
    }
    let mut cleaned = PathBuf::new();
    if rooted {
        cleaned.push("/");
    }
    for element in out {
        cleaned.push(element);
    }
    if cleaned.as_os_str().is_empty() {
        cleaned.push(".");
    }
    cleaned
}

/// `filepath.Abs` — `Clean(join(cwd, path))`; no filesystem access beyond
/// `getcwd`, no symlink resolution.
fn abs_path(path: &Path) -> Result<PathBuf, ReleaseError> {
    if path.is_absolute() {
        return Ok(clean_path(path));
    }
    Ok(clean_path(&std::env::current_dir()?.join(path)))
}

/// `filepath.Rel` (unix semantics) — the lexical path that joins `base` to
/// `targ`. Errors when the two disagree on absoluteness or the first
/// differing base element is `..` (which would falsely escape).
fn rel_path(base: &Path, targ: &Path) -> Result<PathBuf, ReleaseError> {
    let base = clean_path(base);
    let targ = clean_path(targ);
    if base == targ {
        return Ok(PathBuf::from("."));
    }
    if base.is_absolute() != targ.is_absolute() {
        return Err(failed(format!(
            "Rel: can't make {} relative to {}",
            targ.display(),
            base.display()
        )));
    }
    let base_elems = path_elems(&base);
    let targ_elems = path_elems(&targ);
    let mut shared = 0;
    while shared < base_elems.len()
        && shared < targ_elems.len()
        && base_elems[shared] == targ_elems[shared]
    {
        shared += 1;
    }
    if base_elems
        .get(shared)
        .is_some_and(|elem| elem.as_os_str() == "..")
    {
        return Err(failed(format!(
            "Rel: can't make {} relative to {}",
            targ.display(),
            base.display()
        )));
    }
    let mut relative = PathBuf::new();
    for _ in shared..base_elems.len() {
        relative.push("..");
    }
    for element in &targ_elems[shared..] {
        relative.push(element);
    }
    if relative.as_os_str().is_empty() {
        relative.push(".");
    }
    Ok(relative)
}

/// Path elements after cleaning — `..` survives only as a leading run on
/// relative paths (rooted paths can't escape `/`).
fn path_elems(path: &Path) -> Vec<OsString> {
    path.components()
        .filter_map(|component| match component {
            Component::Normal(part) => Some(part.to_os_string()),
            Component::ParentDir => Some(OsString::from("..")),
            _ => None,
        })
        .collect()
}

/// `hashRegularFile` — lstat (links reject as non-regular) + sha256 bytes.
fn hash_regular_file(root: &Path, name: &str) -> Result<String, ReleaseError> {
    let path = root.join(name);
    let info = fs::symlink_metadata(&path)?;
    if !info.file_type().is_file() {
        return Err(failed("not a regular file"));
    }
    let mut file = fs::File::open(&path)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hex::encode(hasher.finalize()))
}

/// `validSHA256` — exactly 64 lowercase hex digits.
fn valid_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// `hashFileMap` — sha256 over `name \x00 hash \n` for the sorted entries
/// under `prefix` (`BTreeMap` iterates sorted); `None` — Go's `""` —
/// when nothing carries the prefix.
fn hash_file_map(files: &BTreeMap<String, String>, prefix: &str) -> Option<String> {
    let mut hasher = Sha256::new();
    let mut any = false;
    for (name, hash) in files {
        if !name.starts_with(prefix) {
            continue;
        }
        any = true;
        hasher.update(name.as_bytes());
        hasher.update([0]);
        hasher.update(hash.as_bytes());
        hasher.update(b"\n");
    }
    any.then(|| hex::encode(hasher.finalize()))
}

/// `writeManifest` — 2-space-indented JSON + trailing newline, written via
/// temp+rename at 0o644.
fn write_manifest(root: &Path, manifest: &Manifest) -> Result<(), ReleaseError> {
    let mut data = serde_json::to_string_pretty(manifest).map_err(|e| failed(e.to_string()))?;
    data.push('\n');
    let temp_path = root.join(format!(".{MANIFEST_NAME}.{}", std::process::id()));
    let outcome = (|| {
        let mut temp = fs::File::create(&temp_path)?;
        set_file_mode(&temp_path, 0o644)?;
        temp.write_all(data.as_bytes())?;
        temp.sync_all()?;
        drop(temp);
        fs::rename(&temp_path, root.join(MANIFEST_NAME))
    })();
    if outcome.is_err() {
        let _ = fs::remove_file(&temp_path);
    }
    outcome?;
    Ok(())
}

/// `temp.Chmod` — the manifest lands at 0o644 regardless of umask.
#[cfg(unix)]
fn set_file_mode(path: &Path, mode: u32) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(mode))
}

#[cfg(not(unix))]
fn set_file_mode(_path: &Path, _mode: u32) -> io::Result<()> {
    Ok(())
}

#[cfg(unix)]
fn is_executable(meta: &fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    meta.permissions().mode() & 0o111 != 0
}

#[cfg(not(unix))]
fn is_executable(meta: &fs::Metadata) -> bool {
    meta.is_file() && !meta.permissions().readonly()
}

#[cfg(unix)]
fn set_readonly_mode(path: &Path, info: &fs::Metadata) -> Result<(), ReleaseError> {
    use std::os::unix::fs::PermissionsExt;
    let mode = info.permissions().mode() & 0o777 & !0o222;
    fs::set_permissions(path, fs::Permissions::from_mode(mode))?;
    Ok(())
}

#[cfg(not(unix))]
fn set_readonly_mode(path: &Path, info: &fs::Metadata) -> Result<(), ReleaseError> {
    let mut permissions = info.permissions();
    permissions.set_readonly(true);
    fs::set_permissions(path, permissions)?;
    Ok(())
}

#[cfg(unix)]
fn add_owner_write(path: &Path) -> Result<(), ReleaseError> {
    use std::os::unix::fs::PermissionsExt;
    let info = fs::symlink_metadata(path)?;
    let mode = (info.permissions().mode() & 0o777) | 0o700;
    fs::set_permissions(path, fs::Permissions::from_mode(mode))?;
    Ok(())
}

#[cfg(not(unix))]
fn add_owner_write(path: &Path) -> Result<(), ReleaseError> {
    let info = fs::symlink_metadata(path)?;
    let mut permissions = info.permissions();
    if permissions.readonly() {
        permissions.set_readonly(false);
        fs::set_permissions(path, permissions)?;
    }
    Ok(())
}

#[cfg(unix)]
fn create_dir_link(target: &Path, link: &Path) -> io::Result<()> {
    std::os::unix::fs::symlink(target, link)
}

#[cfg(windows)]
fn create_dir_link(target: &Path, link: &Path) -> io::Result<()> {
    std::os::windows::fs::symlink_dir(target, link)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    /// The Rust bundle layout — mirrors `release-manifest.py`'s
    /// REQUIRED_FILES plus one `web/` file so `web_hash` is exercised.
    fn test_release() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        for (name, contents) in [
            ("lerdr-relay", "binary"),
            ("README.md", "readme"),
            ("scripts/common.sh", "#!/bin/sh\n"),
            ("scripts/plugin-on-event.sh", "#!/bin/sh\n"),
            ("scripts/plugin-on-startup.sh", "#!/bin/sh\n"),
            ("scripts/setup-link.sh", "#!/bin/sh\n"),
            ("scripts/tailscale-serve.sh", "#!/bin/sh\n"),
            ("scripts/tailscale-service.sh", "#!/bin/sh\n"),
            ("web/index.html", "<html></html>"),
        ] {
            let path = root.join(name);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            let mode = if name == "lerdr-relay" || name.ends_with(".sh") {
                0o755
            } else {
                0o644
            };
            fs::write(&path, contents).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(mode)).unwrap();
        }
        dir
    }

    fn unwrap_failed(error: ReleaseError) -> String {
        match error {
            ReleaseError::Failed(message) => message,
            ReleaseError::Io(error) => panic!("expected a validation error, got {error}"),
        }
    }

    #[test]
    fn build_and_verify_manifest_round_trip() {
        let root = test_release();
        let manifest = build(root.path(), "1.2.3", "abc123", "linux/amd64").unwrap();
        assert!(manifest.web_hash.is_some(), "web hash is empty");
        let transports = vec![ENCRYPTED_WEBSOCKET_SUBPROTOCOL.to_string()];
        assert_eq!(manifest.app_transports, transports);
        assert_eq!(manifest.relay_transports, transports);
        let verified = verify(root.path(), "linux/amd64").unwrap();
        assert_eq!(verified.version, "1.2.3");
        assert_eq!(verified, manifest);
    }

    #[test]
    fn build_and_verify_without_web_bundle() {
        // Rust tarballs ship no PWA — web_hash stays absent and verify is
        // honest about it (the Python mirror's contract).
        let dir = tempfile::tempdir().unwrap();
        for name in [
            "lerdr-relay",
            "README.md",
            "scripts/common.sh",
            "scripts/plugin-on-event.sh",
            "scripts/plugin-on-startup.sh",
            "scripts/setup-link.sh",
            "scripts/tailscale-serve.sh",
            "scripts/tailscale-service.sh",
        ] {
            let path = dir.path().join(name);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, "x").unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        }
        let manifest = build(dir.path(), "1.2.3", "abc123", "linux/amd64").unwrap();
        assert_eq!(manifest.web_hash, None);
        let written = fs::read_to_string(dir.path().join(MANIFEST_NAME)).unwrap();
        assert!(!written.contains("web_hash"));
        verify(dir.path(), "linux/amd64").unwrap();
    }

    #[test]
    fn verify_rejects_tampering_and_unlisted_files() {
        let root = test_release();
        build(root.path(), "1.2.3", "abc123", "linux/amd64").unwrap();
        fs::write(root.path().join("web/index.html"), "changed").unwrap();
        let error = verify(root.path(), "linux/amd64").unwrap_err();
        assert!(error.to_string().contains("hash mismatch"), "{error}");

        let root = test_release();
        build(root.path(), "1.2.3", "abc123", "linux/amd64").unwrap();
        fs::write(root.path().join("extra"), "no").unwrap();
        let error = verify(root.path(), "linux/amd64").unwrap_err();
        assert!(error.to_string().contains("not listed"), "{error}");
    }

    #[test]
    fn verify_rejects_wrong_target_and_symlink() {
        let root = test_release();
        build(root.path(), "1.2.3", "abc123", "linux/amd64").unwrap();
        assert!(verify(root.path(), "darwin/arm64").is_err());
        std::os::unix::fs::symlink("index.html", root.path().join("web/linked")).unwrap();
        let error = verify(root.path(), "linux/amd64").unwrap_err();
        assert!(error.to_string().contains("symlink"), "{error}");
    }

    #[test]
    fn verify_rejects_invalid_web_hash_and_non_executable_binary() {
        let root = test_release();
        build(root.path(), "1.2.3", "abc123", "linux/amd64").unwrap();
        let manifest_path = root.path().join(MANIFEST_NAME);
        let data = fs::read_to_string(&manifest_path).unwrap().replacen(
            "\"web_hash\": \"",
            "\"web_hash\": \"00",
            1,
        );
        fs::write(&manifest_path, data).unwrap();
        let error = verify(root.path(), "linux/amd64").unwrap_err();
        assert!(error.to_string().contains("web hash"), "{error}");

        let root = test_release();
        build(root.path(), "1.2.3", "abc123", "linux/amd64").unwrap();
        fs::set_permissions(
            root.path().join("lerdr-relay"),
            fs::Permissions::from_mode(0o644),
        )
        .unwrap();
        let error = verify(root.path(), "linux/amd64").unwrap_err();
        assert!(error.to_string().contains("not executable"), "{error}");
    }

    #[test]
    fn verify_rejects_noncanonical_and_escaping_manifest_paths() {
        let root = test_release();
        build(root.path(), "1.2.3", "abc123", "linux/amd64").unwrap();
        let manifest_path = root.path().join(MANIFEST_NAME);
        let original: Manifest =
            serde_json::from_str(&fs::read_to_string(&manifest_path).unwrap()).unwrap();
        for (name, part) in [
            ("./lerdr-relay", "not canonical"),
            ("scripts//common.sh", "not canonical"),
            ("../escape", "invalid manifest path"),
            ("a\\b", "invalid manifest path"),
            ("/absolute", "invalid manifest path"),
        ] {
            let mut manifest = original.clone();
            manifest.files.insert(name.to_string(), "0".repeat(64));
            fs::write(
                &manifest_path,
                format!("{}\n", serde_json::to_string_pretty(&manifest).unwrap()),
            )
            .unwrap();
            let error = verify(root.path(), "linux/amd64").unwrap_err();
            assert!(error.to_string().contains(part), "{name}: {error}");
        }
    }

    #[test]
    fn verify_rejects_missing_required_and_empty_manifest() {
        let root = test_release();
        fs::remove_file(root.path().join("README.md")).unwrap();
        build(root.path(), "1.2.3", "abc123", "linux/amd64").unwrap();
        let error = verify(root.path(), "linux/amd64").unwrap_err();
        assert!(error.to_string().contains("missing README.md"), "{error}");

        let empty = tempfile::tempdir().unwrap();
        fs::write(empty.path().join(MANIFEST_NAME), "{}").unwrap();
        let error = verify(empty.path(), "").unwrap_err();
        assert!(error.to_string().contains("schema 0"), "{error}");
    }

    #[test]
    fn seal_makes_verified_tree_read_only() {
        let root = test_release();
        build(root.path(), "1.2.3", "abc123", "linux/amd64").unwrap();
        seal(root.path()).unwrap();
        verify(root.path(), "linux/amd64").unwrap();
        for name in [".", "web", MANIFEST_NAME, "lerdr-relay"] {
            let mode = fs::metadata(root.path().join(name))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o222, 0, "{name} remains writable: {mode:o}");
        }
        let binary = fs::metadata(root.path().join("lerdr-relay")).unwrap();
        assert_ne!(binary.permissions().mode() & 0o111, 0);
        // Unseal so TempDir cleanup can't fail on a read-only directory.
        walk_tree(root.path(), |path, file_type| {
            if file_type.is_dir() {
                add_owner_write(path)?;
            }
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn verify_skips_tree_walk_for_symlinked_root() {
        // `filepath.WalkDir` lstats the root: `releases/current` resolves
        // through the link for file checks but the tree walk never descends.
        let base = tempfile::tempdir().unwrap();
        let root = test_release();
        build(root.path(), "1.2.3", "abc123", "linux/amd64").unwrap();
        let link = base.path().join("current");
        std::os::unix::fs::symlink(root.path(), &link).unwrap();
        verify(&link, "linux/amd64").unwrap();
        // A stray file inside the target is invisible through the link —
        // matching the oracle's blind spot on `current`.
        fs::write(root.path().join("stray"), "x").unwrap();
        verify(&link, "linux/amd64").unwrap();
        verify(root.path(), "linux/amd64").unwrap_err();
    }

    #[test]
    fn activate_swaps_current_symlink() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let release_dir = root.join("releases").join("one");
        fs::create_dir_all(&release_dir).unwrap();
        activate(root, &release_dir).unwrap();
        let target = fs::read_link(root.join("current")).unwrap();
        assert_eq!(target, Path::new("releases").join("one"));
        // Re-activation replaces the link atomically.
        let next = root.join("releases").join("two");
        fs::create_dir_all(&next).unwrap();
        activate(root, &next).unwrap();
        assert_eq!(
            fs::read_link(root.join("current")).unwrap(),
            Path::new("releases").join("two")
        );
    }

    #[test]
    fn activate_rejects_directories_outside_the_root() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("install");
        let outside = dir.path().join("elsewhere");
        fs::create_dir_all(&root).unwrap();
        fs::create_dir_all(&outside).unwrap();
        let error = activate(&root, &outside).unwrap_err();
        assert_eq!(
            unwrap_failed(error),
            "release directory is outside release root"
        );
        assert!(!root.join("current").exists());
        let error = activate(Path::new("relative/root"), &dir.path().join("abs")).unwrap_err();
        assert_eq!(
            unwrap_failed(error),
            "release directory is outside release root"
        );
    }

    #[test]
    fn prune_keeps_current_rollback_and_unverifiable() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("installed");
        let releases = root.join("releases");
        let current = releases.join("current-release");
        let previous = releases.join("previous-release");
        let old = releases.join("old-release");
        let inflight = releases.join(".update-inflight");
        let broken = releases.join("broken-release");
        for directory in [&current, &previous, &old, &inflight, &broken] {
            fs::create_dir_all(directory).unwrap();
        }
        for directory in [&current, &previous, &old] {
            write_test_release(directory);
        }
        seal(&old).unwrap();
        prune_old_releases(&root, &[current.clone(), previous.clone()]).unwrap();
        for kept in [&current, &previous, &inflight, &broken] {
            assert!(kept.exists(), "kept release {}", kept.display());
        }
        assert!(!old.exists(), "old release was not pruned");
    }

    #[test]
    fn prune_requires_absolute_non_root() {
        let error = prune_old_releases(Path::new("relative"), &[]).unwrap_err();
        assert_eq!(
            unwrap_failed(error),
            "release root must be a non-root absolute path"
        );
        assert!(prune_old_releases(Path::new("/"), &[]).is_err());
    }

    fn write_test_release(root: &Path) {
        for name in [
            "lerdr-relay",
            "README.md",
            "scripts/common.sh",
            "scripts/plugin-on-event.sh",
            "scripts/plugin-on-startup.sh",
            "scripts/setup-link.sh",
            "scripts/tailscale-serve.sh",
            "scripts/tailscale-service.sh",
        ] {
            let path = root.join(name);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, format!("{name}\n")).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        }
        build(root, "1.2.3", "abc123", &current_target()).unwrap();
    }

    #[test]
    fn verify_identity_checks() {
        let stamp = BinaryStamp {
            version: "1.2.3",
            revision: "candidate-revision",
            target: "linux/amd64".to_string(),
        };
        let manifest = Manifest {
            version: "1.2.3".to_string(),
            revision: "candidate-revision".to_string(),
            target: "linux/amd64".to_string(),
            ..Manifest::default()
        };
        verify_identity(
            &manifest,
            "1.2.3",
            "candidate-revision",
            "linux/amd64",
            false,
            &stamp,
        )
        .unwrap();

        struct Case {
            name: &'static str,
            manifest: Manifest,
            expected_version: &'static str,
            expected_revision: &'static str,
            expected_target: &'static str,
            error_part: &'static str,
        }
        let cases = [
            Case {
                name: "workflow version",
                manifest: manifest.clone(),
                expected_version: "1.2.4",
                expected_revision: "candidate-revision",
                expected_target: "linux/amd64",
                error_part: "expected version",
            },
            Case {
                name: "workflow revision",
                manifest: manifest.clone(),
                expected_version: "1.2.3",
                expected_revision: "other-revision",
                expected_target: "linux/amd64",
                error_part: "expected revision",
            },
            Case {
                name: "workflow target",
                manifest: manifest.clone(),
                expected_version: "1.2.3",
                expected_revision: "candidate-revision",
                expected_target: "other/target",
                error_part: "expected target",
            },
            Case {
                name: "binary version",
                manifest: Manifest {
                    version: "1.2.4".to_string(),
                    ..manifest.clone()
                },
                expected_version: "",
                expected_revision: "",
                expected_target: "linux/amd64",
                error_part: "binary version",
            },
            Case {
                name: "binary revision",
                manifest: Manifest {
                    revision: "other-revision".to_string(),
                    ..manifest.clone()
                },
                expected_version: "",
                expected_revision: "",
                expected_target: "linux/amd64",
                error_part: "binary revision",
            },
            Case {
                name: "binary target",
                manifest: Manifest {
                    target: "other/target".to_string(),
                    ..manifest.clone()
                },
                expected_version: "",
                expected_revision: "",
                expected_target: "other/target",
                error_part: "binary target",
            },
        ];
        for case in &cases {
            let error = verify_identity(
                &case.manifest,
                case.expected_version,
                case.expected_revision,
                case.expected_target,
                false,
                &stamp,
            )
            .unwrap_err();
            assert!(
                error.to_string().contains(case.error_part),
                "{}: {error}",
                case.name
            );
        }

        // Cross-target: a build host verifies another platform's bundle —
        // version/revision still checked against the binary stamp.
        let cross = Manifest {
            target: "other/target".to_string(),
            ..manifest.clone()
        };
        verify_identity(&cross, "", "", "other/target", true, &stamp).unwrap();
    }

    #[test]
    fn clean_and_rel_paths() {
        assert_eq!(clean_slash("a/./b"), "a/b");
        assert_eq!(clean_slash("a//b"), "a/b");
        assert_eq!(clean_slash("a/b/"), "a/b");
        assert_eq!(clean_slash("./a"), "a");
        assert_eq!(clean_slash("a/../b"), "b");
        assert_eq!(clean_slash(".."), "..");
        assert_eq!(clean_slash("../a"), "../a");
        assert_eq!(clean_slash("."), ".");

        assert_eq!(
            rel_path(Path::new("/a/b"), Path::new("/a/b/c/d")).unwrap(),
            Path::new("c/d")
        );
        assert_eq!(
            rel_path(Path::new("/a/b/c"), Path::new("/a/x")).unwrap(),
            Path::new("../..").join("x")
        );
        assert_eq!(
            rel_path(Path::new("/root"), Path::new("/root/releases/one")).unwrap(),
            Path::new("releases/one")
        );
        assert_eq!(
            rel_path(Path::new("/a/"), Path::new("/a")).unwrap(),
            Path::new(".")
        );
        assert!(rel_path(Path::new("/a/../x"), Path::new("/x/y")).is_ok());
        assert!(rel_path(Path::new("rel"), Path::new("/abs")).is_err());
    }

    #[test]
    fn hash_file_map_matches_oracle() {
        // sha256("web/index.html\x00<hash>\n") — the oracle's hashFileMap
        // folds sorted prefixed entries; empty prefix set → None.
        let mut files = BTreeMap::new();
        assert_eq!(hash_file_map(&files, "web/"), None);
        files.insert("a".to_string(), "0".repeat(64));
        assert_eq!(hash_file_map(&files, "web/"), None);
        files.insert("web/b".to_string(), "1".repeat(64));
        files.insert("web/a".to_string(), "2".repeat(64));
        let mut hasher = Sha256::new();
        for (name, hash) in [("web/a", "2".repeat(64)), ("web/b", "1".repeat(64))] {
            hasher.update(name.as_bytes());
            hasher.update([0]);
            hasher.update(hash.as_bytes());
            hasher.update(b"\n");
        }
        assert_eq!(
            hash_file_map(&files, "web/"),
            Some(hex::encode(hasher.finalize()))
        );
    }
}

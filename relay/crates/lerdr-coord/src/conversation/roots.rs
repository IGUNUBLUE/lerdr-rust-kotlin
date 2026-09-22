//! Provider transcript roots — a port of `internal/agentroots` (`resolve`,
//! `expandTilde`, `profileAgentDirs`, and the per-provider entry points).
//!
//! Ordering contract (the oracle doc comment is authoritative): the relay's
//! `HERDR_<AGENT>_CONFIG_DIRS`/`*_DATA_DIRS` list first (colon-separated, like
//! `PATH`), then the agent's own single-directory variable, then discovered
//! profile bases, then the home default — which is always moved to the end
//! even when named explicitly. Entries are trimmed, a leading `~`/`~/` is
//! expanded against `home`, and anything still relative is dropped.
//!
//! The Go package caches `profileAgentDirs` for 60 s (5 s when a dangling
//! symlink is present); this port resolves on demand — the caller caches
//! locations, so the extra `readdir` per locate is not observable.

use std::path::Path;

/// `configEnv`: a `HERDR_*` name consults `LERDR_*` first, then the `HERDR_`
/// spelling; any other name is looked up verbatim.
fn config_env(name: &str, env: &EnvLookup) -> String {
    if let Some(rest) = name.strip_prefix("HERDR_") {
        if let Some(value) = env(&format!("LERDR_{rest}")) {
            if !value.is_empty() {
                return value;
            }
        }
    }
    env(name).unwrap_or_default()
}

/// `expandTilde`: `~` alone means home, `~/x` joins home — `~user` is left
/// untouched (and later rejected as relative).
pub(crate) fn expand_home(path: &str, home: &str) -> String {
    if path == "~" {
        return home.to_string();
    }
    if let Some(rest) = path.strip_prefix("~/") {
        return join_clean(home, rest);
    }
    path.to_string()
}

/// Env provider — `os.Getenv`-equivalent for tests.
pub(crate) type EnvLookup<'a> = dyn Fn(&str) -> Option<String> + Send + Sync + 'a;

/// `filepath.Join(base, leaf)` — `leaf` empty reports `base` itself.
fn join_clean(base: &str, leaf: &str) -> String {
    clean_path(&Path::new(base).join(leaf).to_string_lossy())
}

/// `filepath.Clean` (lexical, unix flavour — the relay targets unix).
pub(crate) fn clean_path(path: &str) -> String {
    let absolute = path.starts_with('/');
    let mut parts: Vec<&str> = Vec::new();
    for component in path.split('/') {
        match component {
            "" | "." => {}
            ".." => {
                if parts.last() == Some(&"..") || (parts.is_empty() && !absolute) {
                    parts.push("..");
                } else if !parts.is_empty() {
                    parts.pop();
                }
                // Absolute path: ".." at root is dropped.
            }
            c => parts.push(c),
        }
    }
    let joined = parts.join("/");
    if absolute {
        if joined.is_empty() {
            "/".to_string()
        } else {
            format!("/{joined}")
        }
    } else if joined.is_empty() {
        ".".to_string()
    } else {
        joined
    }
}

/// `resolve` — see module docs for the ordering contract.
fn resolve(
    home: &str,
    list_env: &str,
    single_env: &str,
    home_base: &str,
    leaf: &str,
    discovered: &[String],
    env: &EnvLookup,
) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    let mut roots: Vec<String> = Vec::new();
    let add =
        |roots: &mut Vec<String>, seen: &mut std::collections::HashSet<String>, base: &str| {
            let base = base.trim();
            if base.is_empty() {
                return;
            }
            let base = expand_home(base, home);
            if !Path::new(&base).is_absolute() {
                return;
            }
            let root = join_clean(&base, leaf);
            if seen.insert(root.clone()) {
                roots.push(root);
            }
        };
    // filepath.SplitList on unix: ':'-separated, "" → empty list.
    for base in split_list(&config_env(list_env, env)) {
        add(&mut roots, &mut seen, &base);
    }
    if !single_env.is_empty() {
        add(&mut roots, &mut seen, &config_env(single_env, env));
    }
    for base in discovered {
        add(&mut roots, &mut seen, base);
    }
    add(&mut roots, &mut seen, home_base);

    // The home default is a fallback even when named explicitly: remove any
    // earlier occurrence and append it once at the end.
    let home_root = clean_path(&join_clean(&expand_home(home_base.trim(), home), leaf));
    if Path::new(&home_root).is_absolute() {
        roots.retain(|root| root != &home_root);
        roots.push(home_root);
    }
    roots
}

/// `filepath.SplitList` on unix.
fn split_list(value: &str) -> Vec<String> {
    if value.is_empty() {
        return Vec::new();
    }
    value.split(':').map(str::to_string).collect()
}

/// `profileAgentDirs`: `<configRoot>/profiles/<name>/agent` for each entry that
/// stats (following symlinks) as a directory. ReadDir order is deterministic.
fn profile_agent_dirs(config_root: &str) -> Vec<String> {
    if !Path::new(config_root).is_absolute() {
        return Vec::new();
    }
    let profiles = Path::new(config_root).join("profiles");
    if !profiles.is_dir() {
        return Vec::new();
    }
    let Ok(entries) = std::fs::read_dir(&profiles) else {
        return Vec::new();
    };
    let mut dirs = Vec::new();
    for entry in entries.flatten() {
        let candidate = entry.path();
        // os.Stat follows symlinks — a symlinked profile dir is searched.
        if candidate.is_dir() {
            dirs.push(candidate.join("agent").to_string_lossy().into_owned());
        }
    }
    dirs
}

/// `agentroots.Claude`.
pub(crate) fn claude_roots(home: &str, env: &EnvLookup) -> Vec<String> {
    resolve(
        home,
        "HERDR_CLAUDE_CONFIG_DIRS",
        "CLAUDE_CONFIG_DIR",
        &Path::new(home).join(".claude").to_string_lossy(),
        "projects",
        &[],
        env,
    )
}

/// `agentroots.Qoder`.
pub(crate) fn qoder_roots(home: &str, env: &EnvLookup) -> Vec<String> {
    resolve(
        home,
        "HERDR_QODER_CONFIG_DIRS",
        "",
        &Path::new(home).join(".qoder").to_string_lossy(),
        "projects",
        &[],
        env,
    )
}

/// `agentroots.Codex`.
pub(crate) fn codex_roots(home: &str, env: &EnvLookup) -> Vec<String> {
    resolve(
        home,
        "HERDR_CODEX_CONFIG_DIRS",
        "CODEX_HOME",
        &Path::new(home).join(".codex").to_string_lossy(),
        "sessions",
        &[],
        env,
    )
}

/// `agentroots.Pi` — config root `~/.pi`, profiles under `profiles/*/agent`.
pub(crate) fn pi_roots(home: &str, env: &EnvLookup) -> Vec<String> {
    let config_root = Path::new(home).join(".pi");
    let discovered = profile_agent_dirs(&config_root.to_string_lossy());
    resolve(
        home,
        "HERDR_PI_CONFIG_DIRS",
        "PI_CODING_AGENT_DIR",
        &config_root.join("agent").to_string_lossy(),
        "sessions",
        &discovered,
        env,
    )
}

/// `agentroots.OMP` — same shape as Pi under `~/.omp`.
pub(crate) fn omp_roots(home: &str, env: &EnvLookup) -> Vec<String> {
    let config_root = Path::new(home).join(".omp");
    let discovered = profile_agent_dirs(&config_root.to_string_lossy());
    resolve(
        home,
        "HERDR_OMP_CONFIG_DIRS",
        "PI_CODING_AGENT_DIR",
        &config_root.join("agent").to_string_lossy(),
        "sessions",
        &discovered,
        env,
    )
}

/// `agentroots.OMO` — branded `.omo/agent`, standalone `.senpi/agent` default,
/// plus the legacy `.omo` layout only when `settings.json` is a regular file.
pub(crate) fn omo_roots(home: &str, env: &EnvLookup) -> Vec<String> {
    let branded = Path::new(home).join(".omo").join("agent");
    let legacy = Path::new(home).join(".omo");
    let mut discovered = vec![
        env("SENPI_CODING_AGENT_DIR").unwrap_or_default(),
        env("PI_CODING_AGENT_DIR").unwrap_or_default(),
        branded.to_string_lossy().into_owned(),
    ];
    let settings = legacy.join("settings.json");
    if settings.is_file() {
        discovered.push(legacy.to_string_lossy().into_owned());
    }
    resolve(
        home,
        "HERDR_OMO_CONFIG_DIRS",
        "OMO_CODING_AGENT_DIR",
        &Path::new(home)
            .join(".senpi")
            .join("agent")
            .to_string_lossy(),
        "sessions",
        &discovered,
        env,
    )
}

/// `agentroots.OpenCodeData` — data directories, not database files.
pub(crate) fn opencode_data_roots(home: &str, env: &EnvLookup) -> Vec<String> {
    let mut xdg_data = env("XDG_DATA_HOME").unwrap_or_default().trim().to_string();
    if xdg_data.is_empty() {
        xdg_data = Path::new(home)
            .join(".local")
            .join("share")
            .to_string_lossy()
            .into_owned();
    }
    resolve(
        home,
        "HERDR_OPENCODE_DATA_DIRS",
        "",
        &Path::new(&xdg_data).join("opencode").to_string_lossy(),
        "",
        &[],
        env,
    )
}

/// `agentroots.OpenCodeDBs` — `opencode.db` under each data root.
pub(crate) fn opencode_dbs(home: &str, env: &EnvLookup) -> Vec<String> {
    opencode_data_roots(home, env)
        .iter()
        .map(|root| join_clean(root, "opencode.db"))
        .collect()
}

/// `agentroots.HermesData`.
pub(crate) fn hermes_data_roots(home: &str, env: &EnvLookup) -> Vec<String> {
    resolve(
        home,
        "HERDR_HERMES_DATA_DIRS",
        "HERMES_HOME",
        &Path::new(home).join(".hermes").to_string_lossy(),
        "",
        &[],
        env,
    )
}

/// `agentroots.HermesDBs` — `state.db` under each data root.
pub(crate) fn hermes_dbs(home: &str, env: &EnvLookup) -> Vec<String> {
    hermes_data_roots(home, env)
        .iter()
        .map(|root| join_clean(root, "state.db"))
        .collect()
}

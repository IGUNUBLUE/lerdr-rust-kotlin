//! Agent launch profiles — the `internal/profiles` `Resolver` port.
//!
//! Discovery order (unchanged from the oracle):
//!
//! 1. `<config_home>/herdr/agent-profiles.ini` — `[profiles]` custom labels,
//!    `[config] replace_profiles`, `[aliases]` agent-name→profile mapping.
//! 2. `defaultCandidates` filtered by executable presence on `PATH`
//!    (`binaryPath` → `exec.LookPath` semantics).
//! 3. `integration.list` targets whose state is `current`/`outdated` — the
//!    socket equivalent of the oracle's `integration status` text parse.
//!
//! The one structural difference is async: Go's `discoverIntegrations`
//! blocks on a 5 s `integration status` subprocess; the socket port fetches
//! `integration.list` up front in [`Resolver::profiles`], so the INI/PATH
//! half stays synchronous.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use super::COMMAND_DEADLINE;

/// `cached` TTL — the oracle re-discovers every five minutes.
const CACHE_TTL: Duration = Duration::from_secs(5 * 60);

/// `defaultCandidates` — order matters (the phone presents them this way).
const DEFAULT_CANDIDATES: &[(&str, &str)] = &[
    ("codex", "Codex"),
    ("claude", "Claude Code"),
    ("opencode", "OpenCode"),
    ("pi", "Pi"),
    ("omp", "Oh My Pi"),
    ("kimi", "Kimi"),
    ("hermes", "Hermes"),
];

/// `defaultAliases`.
fn default_aliases() -> HashMap<String, String> {
    HashMap::from([
        ("claude-code".to_owned(), "claude".to_owned()),
        ("claude code".to_owned(), "claude".to_owned()),
        ("pi-coding-agent".to_owned(), "pi".to_owned()),
    ])
}

/// `integrationLabels`.
fn integration_label(id: &str) -> &str {
    match id {
        "qodercli" => "Qoder",
        other => other,
    }
}

/// `isHerdrKind` — profile ids Herdr can `agent.start --kind` natively.
fn is_herdr_kind(id: &str) -> bool {
    matches!(
        id,
        "agy"
            | "amp"
            | "claude"
            | "cline"
            | "codex"
            | "copilot"
            | "cursor"
            | "devin"
            | "droid"
            | "gemini"
            | "grok"
            | "hermes"
            | "kilo"
            | "kimi"
            | "kiro"
            | "maki"
            | "mastracode"
            | "omp"
            | "opencode"
            | "pi"
            | "qodercli"
    )
}

/// `profiles.Profile` — `kind` drives `agent.start`; empty `kind` means the
/// profile is a raw argv launched through pane input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Profile {
    pub id: String,
    pub label: String,
    pub kind: String,
    pub argv: Vec<String>,
}

struct ResolverInner {
    config_home: PathBuf,
    /// `(profiles, expires)` — `None` until the first discovery.
    cached: Mutex<Option<(Vec<Profile>, Instant)>>,
    remembered: Mutex<HashMap<String, String>>,
    /// INI `[aliases]` + defaults, refreshed by every discovery.
    aliases: Mutex<HashMap<String, String>>,
}

/// `profiles.Resolver` — clone-cheap shared handle.
#[derive(Clone)]
pub(crate) struct Resolver {
    inner: std::sync::Arc<ResolverInner>,
}

impl Resolver {
    /// `NewResolver(configHome, herdr)` — `config_home` is the XDG config
    /// root (`$XDG_CONFIG_HOME`, else `~/.config`).
    pub(crate) fn new() -> Self {
        let config_home = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .filter(|p| !p.as_os_str().is_empty())
            .or_else(|| super::workspace::home_dir().map(|home| home.join(".config")))
            .unwrap_or_else(|| PathBuf::from("."));
        Self::with_config_home(config_home)
    }

    /// Explicit config root (tests pin a tempdir).
    pub(crate) fn with_config_home(config_home: PathBuf) -> Self {
        Resolver {
            inner: std::sync::Arc::new(ResolverInner {
                config_home,
                cached: Mutex::new(None),
                remembered: Mutex::new(HashMap::new()),
                aliases: Mutex::new(default_aliases()),
            }),
        }
    }

    /// `Profiles()` — cached discovery. `client` feeds the integration pass;
    /// a failed/absent socket degrades to PATH-only candidates exactly like
    /// the oracle's `discoverIntegrations` error swallow.
    pub(crate) async fn profiles(&self, client: &lerdr_herdr::Client) -> Vec<Profile> {
        {
            let cached = self.inner.cached.lock().expect("profiles cache poisoned");
            if let Some((profiles, expires)) = &*cached {
                if Instant::now() < *expires {
                    return profiles.clone();
                }
            }
        }
        let integrations = integration_targets(client).await;
        let discovered = self.discover(&integrations);
        *self.inner.cached.lock().expect("profiles cache poisoned") =
            Some((discovered.clone(), Instant::now() + CACHE_TTL));
        discovered
    }

    /// `discover` — INI + default candidates + integrations, PATH-filtered.
    fn discover(&self, integrations: &[String]) -> Vec<Profile> {
        let loaded = load_ini(&self.inner.config_home.join("herdr/agent-profiles.ini"));
        *self.inner.aliases.lock().expect("aliases poisoned") = loaded.aliases.clone();

        let mut seen = std::collections::HashSet::new();
        let configured_ids: std::collections::HashSet<&str> =
            loaded.configured.iter().map(|p| p.id.as_str()).collect();

        let mut candidates = if loaded.replace {
            loaded.configured.clone()
        } else {
            merge_profiles(&default_candidates(), &loaded.configured)
        };
        if candidates.is_empty() {
            candidates = default_candidates();
        }

        let mut result = Vec::new();
        for mut profile in candidates {
            let Some(path) = binary_path(&profile.id) else {
                if configured_ids.contains(profile.id.as_str()) {
                    tracing::warn!(profile_id = %profile.id, "configured agent profile executable is unavailable");
                }
                continue;
            };
            if seen.insert(profile.id.clone()) {
                profile.argv = vec![path];
                if is_herdr_kind(&profile.id) {
                    profile.kind = profile.id.clone();
                }
                result.push(profile);
            }
        }

        if !loaded.replace {
            for id in integrations {
                if seen.insert(id.clone()) {
                    let mut profile = Profile {
                        id: id.clone(),
                        label: integration_label(id).to_owned(),
                        kind: id.clone(),
                        argv: Vec::new(),
                    };
                    if let Some(path) = binary_path(id) {
                        profile.argv = vec![path];
                    }
                    result.push(profile);
                }
            }
        }
        result
    }

    /// `Profile(id)`.
    pub(crate) async fn profile(&self, client: &lerdr_herdr::Client, id: &str) -> Option<Profile> {
        self.profiles(client).await.into_iter().find(|p| p.id == id)
    }

    /// `ResolvePane` — the remember map wins over the reported agent name.
    pub(crate) async fn resolve_pane(
        &self,
        client: &lerdr_herdr::Client,
        pane_id: &str,
        reported_agent: &str,
    ) -> String {
        let key = pane_id.trim().to_lowercase();
        if let Some(id) = self
            .inner
            .remembered
            .lock()
            .expect("remembered poisoned")
            .get(&key)
        {
            return id.clone();
        }
        self.profile_id_for_agent(client, reported_agent).await
    }

    /// `ProfileIDForAgent` — direct id match first, then the alias map.
    async fn profile_id_for_agent(&self, client: &lerdr_herdr::Client, agent: &str) -> String {
        let agent_lower = agent.trim().to_lowercase();
        let profiles = self.profiles(client).await;
        for profile in &profiles {
            if agent_lower == profile.id.to_lowercase() {
                return profile.id.clone();
            }
        }
        let alias = self
            .inner
            .aliases
            .lock()
            .expect("aliases poisoned")
            .get(&agent_lower)
            .cloned()
            .unwrap_or_default();
        for profile in &profiles {
            if alias == profile.id {
                return alias;
            }
        }
        String::new()
    }

    /// `Remember(paneID, profileID)`.
    pub(crate) fn remember(&self, pane_id: &str, profile_id: &str) {
        self.inner
            .remembered
            .lock()
            .expect("remembered poisoned")
            .insert(pane_id.trim().to_lowercase(), profile_id.to_owned());
    }

    /// `Forget(paneID)`.
    pub(crate) fn forget(&self, pane_id: &str) {
        self.inner
            .remembered
            .lock()
            .expect("remembered poisoned")
            .remove(&pane_id.trim().to_lowercase());
    }
}

fn default_candidates() -> Vec<Profile> {
    DEFAULT_CANDIDATES
        .iter()
        .map(|(id, label)| Profile {
            id: id.to_string(),
            label: label.to_string(),
            kind: String::new(),
            argv: Vec::new(),
        })
        .collect()
}

/// `mergeProfiles` — configured profiles override matching default labels,
/// extras append after them.
fn merge_profiles(defaults: &[Profile], configured: &[Profile]) -> Vec<Profile> {
    let mut result = defaults.to_vec();
    let mut positions: HashMap<String, usize> = result
        .iter()
        .enumerate()
        .map(|(i, p)| (p.id.clone(), i))
        .collect();
    for profile in configured {
        if let Some(&index) = positions.get(profile.id.as_str()) {
            result[index].label = profile.label.clone();
            continue;
        }
        positions.insert(profile.id.clone(), result.len());
        result.push(profile.clone());
    }
    result
}

/// `binaryPath` — `exec.LookPath` for a bare executable name: walk `PATH`,
/// require a regular file with an execute bit.
fn binary_path(name: &str) -> Option<String> {
    if name.is_empty() {
        return None;
    }
    // LookPath resolves names containing a separator directly.
    if name.contains('/') {
        return executable(Path::new(name)).then(|| name.to_owned());
    }
    let path_var = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path_var) {
        let candidate = dir.join(name);
        if executable(&candidate) {
            return Some(candidate.to_string_lossy().into_owned());
        }
    }
    None
}

#[cfg(unix)]
fn executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn executable(path: &Path) -> bool {
    path.is_file()
}

/// `discoverIntegrations` — the socket's `integration.list` supplies the
/// same target list the oracle parses out of `integration status` text:
/// `current`/`outdated` entries only.
async fn integration_targets(client: &lerdr_herdr::Client) -> Vec<String> {
    #[derive(serde::Deserialize)]
    struct Entry {
        target: String,
        #[serde(default)]
        state: String,
    }
    #[derive(serde::Deserialize, Default)]
    struct List {
        #[serde(default)]
        integrations: Vec<Entry>,
    }
    let Ok(value) = client
        .call_with_timeout(
            "integration.list",
            &serde_json::json!({}),
            Some(COMMAND_DEADLINE),
        )
        .await
    else {
        return Vec::new();
    };
    let parsed: List = serde_json::from_value(value).unwrap_or_default();
    parsed
        .integrations
        .into_iter()
        .filter(|e| e.state == "current" || e.state == "outdated")
        .map(|e| e.target)
        .collect()
}

/// The INI file's contribution to discovery.
struct IniProfiles {
    configured: Vec<Profile>,
    replace: bool,
    aliases: HashMap<String, String>,
}

/// `loadINI` — `[config] replace_profiles`, `[profiles] id = label`,
/// `[aliases] name = profile_id`. Section/key names lowercase; a malformed
/// file contributes nothing (the oracle's parse error → empty INI).
fn load_ini(path: &Path) -> IniProfiles {
    let mut out = IniProfiles {
        configured: Vec::new(),
        replace: false,
        aliases: default_aliases(),
    };
    let Ok(data) = std::fs::read_to_string(path) else {
        return out;
    };
    let Some(ini) = parse_ini(&data) else {
        return out;
    };
    if let Some(config) = ini.get("config") {
        if let Some(value) = config.get("replace_profiles") {
            out.replace = parse_bool(value);
        }
    }
    if let Some(profiles) = ini.get("profiles") {
        let mut ids: Vec<&String> = profiles.keys().collect();
        ids.sort();
        for id in ids {
            let label = profiles[id].trim();
            let id = id.trim().to_lowercase();
            if !id.is_empty() && !label.is_empty() {
                out.configured.push(Profile {
                    id,
                    label: label.to_owned(),
                    kind: String::new(),
                    argv: Vec::new(),
                });
            }
        }
    }
    if let Some(aliases) = ini.get("aliases") {
        for (name, profile_id) in aliases {
            let name = name.trim().to_lowercase();
            let profile_id = profile_id.trim().to_lowercase();
            if !name.is_empty() && !profile_id.is_empty() {
                out.aliases.insert(name, profile_id);
            }
        }
    }
    out
}

/// `config.ParseINI` — sections `[name]`, `key = value`/`key: value`,
/// `#`/`;` comments, lowercase section+key names, last section wins on
/// repeat, error (→ empty) on unterminated header/missing delimiter/
/// duplicate key.
fn parse_ini(data: &str) -> Option<BTreeMap<String, BTreeMap<String, String>>> {
    let mut ini: BTreeMap<String, BTreeMap<String, String>> = BTreeMap::new();
    ini.insert(String::new(), BTreeMap::new());
    let mut current = String::new();
    for raw_line in data.lines() {
        let trimmed = raw_line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with(';') {
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix('[') {
            let end = rest.find(']')?;
            current = rest[..end].trim().to_lowercase();
            ini.entry(current.clone()).or_default();
            continue;
        }
        let pos = trimmed.find(['=', ':'])?;
        let key = trimmed[..pos].trim().to_lowercase();
        let value = trimmed[pos + 1..].trim().to_owned();
        let section = ini.entry(current.clone()).or_default();
        if section.insert(key, value).is_some() {
            return None; // duplicate key
        }
    }
    Some(ini)
}

/// `parseBool`.
fn parse_bool(value: &str) -> bool {
    matches!(
        value.trim().to_lowercase().as_str(),
        "true" | "yes" | "1" | "on"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ini_parses_sections_and_lowercases_keys() {
        let ini = parse_ini("[Profiles]\nFoo = My Agent\n[aliases]\nClaude Code = claude\n")
            .expect("parse");
        assert_eq!(ini["profiles"]["foo"], "My Agent");
        assert_eq!(ini["aliases"]["claude code"], "claude");
    }

    #[test]
    fn ini_rejects_duplicate_key_and_missing_delimiter() {
        assert!(parse_ini("[a]\nk=1\nk=2").is_none());
        assert!(parse_ini("[a]\nno delimiter").is_none());
        assert!(parse_ini("[unterminated").is_none());
    }

    #[test]
    fn merge_overrides_labels_and_appends() {
        let configured = vec![
            Profile {
                id: "claude".into(),
                label: "Claude (work)".into(),
                kind: String::new(),
                argv: vec![],
            },
            Profile {
                id: "mycli".into(),
                label: "My CLI".into(),
                kind: String::new(),
                argv: vec![],
            },
        ];
        let merged = merge_profiles(&default_candidates(), &configured);
        assert_eq!(merged[1].label, "Claude (work)");
        assert_eq!(merged.last().unwrap().id, "mycli");
    }

    #[test]
    fn binary_path_finds_executables() {
        assert!(binary_path("sh").is_some());
        assert!(binary_path("definitely-not-a-real-binary-xyz").is_none());
    }
}

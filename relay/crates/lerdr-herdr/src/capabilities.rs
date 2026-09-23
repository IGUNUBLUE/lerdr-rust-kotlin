//! Capability ledger — the port of the oracle's
//! `internal/herdr/capabilities.go` `capabilityManager`.
//!
//! The ledger answers "can the phone drive X on this Herdr build" as
//! per-feature evidence (`supported`/`unsupported`/`unknown` + `reason` +
//! `generation`) projected into `herdr_status.features`.
//!
//! Collection order on every refresh
//! ([`Client::collect_capabilities`](crate::Client::collect_capabilities)):
//!
//! 1. `herdr --version` — installed-binary evidence (best-effort, 1s).
//! 2. `ping` — `ordinary_json` plus server identity/capabilities. A failed
//!    ping ends the refresh early: every feature stays
//!    `unknown`/`not_checked` and `ordinary_json` carries the transport
//!    reason — the oracle's `refresh()` early return.
//! 3. Schema path — when `herdr api schema --json` yields a
//!    [`SchemaRegistry`] whose protocol/version match the answering server,
//!    every tracked method is adjudicated `schema_advertised` /
//!    `schema_absent` and no probe sockets are burned.
//! 4. Probe path — schema absent or untrusted: the oracle's three
//!    optimistic probes (`workspace.move_block`, `tab.move`, `pane.read`)
//!    with validation-refusal semantics, plus the `workspace.reordered`
//!    subscription outcome recorded by `subscribe_topology`.
//! 5. Observed evidence — call-site notes (`operation_succeeded`,
//!    `method_not_supported`, `subscription_*`) merge over schema/probe
//!    verdicts while the note's server identity still matches.
//!
//! Generation semantics: `report.generation` is the refresh tick (bumps on
//! every applied refresh and every note); each feature's `generation` is the
//! tick at which *its* evidence last changed — an unchanged refresh leaves
//! the published status byte-identical, which is what lets
//! `Topology::set_herdr_status` suppress no-change republishes.

use std::collections::BTreeMap;
use std::io;
use std::time::Duration;

use serde_json::{json, Value};

use crate::cli::{resolve_herdr_bin, run_cli};
use crate::error::{DispatchPhase, HerdrError};
use crate::schema::SchemaRegistry;
use crate::types::Pong;
use crate::Client;

/// Feature keys — the oracle's seven, plus method-name keys for the rest of
/// the relay's socket surface when schema evidence exists.
pub mod features {
    /// Socket speaks ordinary JSON at all — ping reached and answered.
    pub const ORDINARY_JSON: &str = "ordinary_json";
    /// `workspace.move_block` method.
    pub const WORKSPACE_MOVE_BLOCK: &str = "workspace.move_block";
    /// `workspace.reordered` subscription/event variant.
    pub const WORKSPACE_REORDERED: &str = "workspace.reordered";
    /// `pane.output_changed` subscription variant — the event's
    /// `events.subscribe` entry, not its `EventData` payload: 0.9.1 lists
    /// `pane_output_changed` among streamed events but has no matching
    /// `Subscription` variant, so only the subscription table counts as
    /// evidence the server will accept it.
    pub const PANE_OUTPUT_CHANGED: &str = "pane.output_changed";
    /// `pane.read` method.
    pub const PANE_READ: &str = "pane.read";
    /// `tab.move` method.
    pub const TAB_MOVE: &str = "tab.move";
    /// Server advertises `endpoint_protocol_generation` in `pong` — the
    /// `client_shell.endpoint` capability surface.
    pub const CLIENT_SHELL_ENDPOINT: &str = "client_shell.endpoint";
    /// Direct-terminal transport — never probed; stays `unknown`.
    pub const DIRECT_TERMINAL: &str = "direct_terminal";
    /// `agent.view.set` — the declarative agent projection the relay
    /// re-asserts after bootstrap/handoff.
    pub const AGENT_VIEW_SET: &str = "agent.view.set";
    /// `pane.focus` method — Phase-5 `focus_pane`.
    pub const PANE_FOCUS: &str = "pane.focus";
    /// `tab.focus` method — Phase-5 `focus_tab`.
    pub const TAB_FOCUS: &str = "tab.focus";
    /// `workspace.focus` method — Phase-5 `focus_workspace`.
    pub const WORKSPACE_FOCUS: &str = "workspace.focus";
    /// `agent.focus` method — Phase-5 `focus_agent`.
    pub const AGENT_FOCUS: &str = "agent.focus";
    /// The four methods behind the Phase-5 `focus` wire capability —
    /// lerdr-coord refutes the capability only once EVERY member reads
    /// `unsupported` (a partial family still serves the members Herdr
    /// ships).
    pub const FOCUS_METHODS: &[&str] = &[PANE_FOCUS, TAB_FOCUS, WORKSPACE_FOCUS, AGENT_FOCUS];
    /// `pane.report_metadata` — the relay's watch annotation token.
    pub const PANE_REPORT_METADATA: &str = "pane.report_metadata";
    /// `workspace.report_metadata` — the connected-device count token.
    pub const WORKSPACE_REPORT_METADATA: &str = "workspace.report_metadata";
    /// `client.window_title.set` — "lerdr: N device(s)" on Herdr's chrome.
    pub const CLIENT_WINDOW_TITLE_SET: &str = "client.window_title.set";
    /// `client.window_title.clear` — restore the title on last disconnect.
    pub const CLIENT_WINDOW_TITLE_CLEAR: &str = "client.window_title.clear";
}

/// Refusal codes that mean "the server does not implement this method" —
/// `noteSocketFeature`'s switch in the oracle.
pub(crate) const UNKNOWN_METHOD_CODES: &[&str] =
    &["unknown_method", "method_not_found", "unsupported_method"];

/// The seven keys every report carries — `initialServerStatus`'s set.
const BASE_FEATURES: &[&str] = &[
    features::ORDINARY_JSON,
    features::WORKSPACE_MOVE_BLOCK,
    features::WORKSPACE_REORDERED,
    features::PANE_READ,
    features::TAB_MOVE,
    features::CLIENT_SHELL_ENDPOINT,
    features::DIRECT_TERMINAL,
];

/// The socket-observed features a bootstrap invalidates —
/// `InvalidateLiveCapabilities`'s set (everything in [`BASE_FEATURES`]
/// except `direct_terminal`, which is never probed), plus the focus
/// methods: their `operation_succeeded`/`method_not_supported` notes are
/// socket evidence and must not survive a handoff to a different build.
const LIVE_FEATURES: &[&str] = &[
    features::ORDINARY_JSON,
    features::WORKSPACE_MOVE_BLOCK,
    features::WORKSPACE_REORDERED,
    features::PANE_READ,
    features::TAB_MOVE,
    features::CLIENT_SHELL_ENDPOINT,
    features::PANE_FOCUS,
    features::TAB_FOCUS,
    features::WORKSPACE_FOCUS,
    features::AGENT_FOCUS,
];

/// Methods beyond the oracle's three probes that get a schema verdict —
/// the relay's whole socket surface, so the phone can hide UI the installed
/// Herdr cannot serve. Kept in sync with `lerdr-coord`'s dispatch surface
/// plus `lerdr-herdr`'s typed wrappers.
const TRACKED_METHODS: &[&str] = &[
    "agent.explain",
    "agent.focus",
    "agent.get",
    "agent.list",
    "agent.prompt",
    "agent.rename",
    "agent.start",
    "agent.view.clear",
    "agent.view.set",
    "agent.wait",
    "client.window_title.clear",
    "client.window_title.set",
    "client_shell.surface.set",
    "command.invoke",
    "events.subscribe",
    "events.wait",
    "integration.install",
    "integration.list",
    "integration.uninstall",
    "layout.apply",
    "notification.show",
    "pane.close",
    "pane.focus",
    "pane.list",
    "pane.process_info",
    "pane.read",
    "pane.report_metadata",
    "pane.send_input",
    "pane.send_keys",
    "pane.send_text",
    "pane.wait_for_output",
    "plugin.action.invoke",
    "plugin.disable",
    "plugin.enable",
    "plugin.log.list",
    "plugin.pane.close",
    "plugin.pane.focus",
    "plugin.pane.open",
    "server.agent_manifests",
    "server.reload_agent_manifests",
    "server.reload_config",
    "session.snapshot",
    "tab.create",
    "tab.focus",
    "tab.list",
    "tab.move",
    "tab.rename",
    "workspace.close",
    "workspace.create",
    "workspace.focus",
    "workspace.list",
    "workspace.move",
    "workspace.move_block",
    "workspace.rename",
    "workspace.report_metadata",
    "worktree.create",
    "worktree.list",
    "worktree.open",
    "worktree.remove",
];

/// Methods whose call outcomes are recorded as observed evidence —
/// `noteSocketFeature`'s call sites (`workspace.move_block`, `tab.move`,
/// `pane.read`) plus `agent.view.set`, the projection the relay owns,
/// the Phase-5 focus family (an `unknown_method` refusal there retracts
/// the advertised `focus` capability through `caps_update`), and the
/// tier-2 surface: every relay-driven call gets schema adjudication plus
/// observed notes so an absent method stops being attempted after the
/// first definitive refusal.
pub(crate) const NOTED_METHODS: &[&str] = &[
    "workspace.move_block",
    "tab.move",
    "pane.read",
    "agent.view.set",
    "agent.focus",
    "pane.focus",
    "tab.focus",
    "workspace.focus",
    "pane.report_metadata",
    "workspace.report_metadata",
    "client.window_title.set",
    "client.window_title.clear",
    "plugin.pane.open",
    "plugin.pane.focus",
    "plugin.pane.close",
    "plugin.enable",
    "plugin.disable",
    "plugin.log.list",
    "server.reload_config",
    "server.agent_manifests",
    "server.reload_agent_manifests",
    "integration.install",
    "integration.uninstall",
];

/// `supported` / `unsupported` / `unknown` — the wire strings verbatim.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FeatureState {
    /// Evidence says the server provides it.
    Supported,
    /// Evidence says the server does not.
    Unsupported,
    /// Not checked, or evidence could not decide.
    Unknown,
}

impl FeatureState {
    /// The wire spelling (`herdr_status.features.*.state`).
    pub fn as_str(self) -> &'static str {
        match self {
            FeatureState::Supported => "supported",
            FeatureState::Unsupported => "unsupported",
            FeatureState::Unknown => "unknown",
        }
    }
}

/// One feature's evidence — `protocol.FeatureEvidence`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FeatureEvidence {
    pub state: FeatureState,
    /// Machine-readable reason (`not_checked`, `schema_advertised`,
    /// `subscription_rejected`, `operation_succeeded`, …).
    pub reason: String,
    /// Ledger generation the evidence was stamped with.
    pub generation: u64,
}

/// The full server-status surface — `protocol.ServerStatus`. The whole
/// report flows through `Topology::set_herdr_status` →
/// `herdrStatusPayload`, field-for-field.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CapabilityReport {
    /// `herdr --version` first line (empty when the binary is missing).
    pub installed_client_version: String,
    /// `pong.version` (empty while no ping has succeeded).
    pub server_version: String,
    /// `pong.protocol` (0 while unknown).
    pub server_protocol: i64,
    /// `true` once a ping answered with a protocol field.
    pub server_protocol_known: bool,
    /// `pong.capabilities.endpoint_protocol_generation`.
    pub endpoint_protocol_generation: Option<i64>,
    /// `pong.capabilities.surface_interest`.
    pub surface_interest: Option<bool>,
    /// `pong.capabilities.health_check`.
    pub health_check: Option<bool>,
    /// Ledger generation — bumps on every applied refresh and every
    /// recorded note.
    pub generation: u64,
    /// Feature name → evidence. Always contains the seven base keys.
    pub features: BTreeMap<String, FeatureEvidence>,
}

impl CapabilityReport {
    /// `ServerStatus.Feature` — absent keys read as `unknown/not_checked`.
    pub fn feature(&self, name: &str) -> FeatureEvidence {
        self.features
            .get(name)
            .cloned()
            .unwrap_or_else(|| FeatureEvidence {
                state: FeatureState::Unknown,
                reason: "not_checked".to_owned(),
                generation: self.generation,
            })
    }

    /// `ServerStatus.Supports` — `true` only on positive evidence.
    pub fn supports(&self, name: &str) -> bool {
        self.feature(name).state == FeatureState::Supported
    }
}

/// An observed verdict from a real call or the subscription handshake.
#[derive(Debug, Clone)]
struct FeatureNote {
    state: FeatureState,
    reason: String,
    /// Server identity the observation was made against (`""` before the
    /// first successful ping — such notes are adopted into the first
    /// identified server, matching the oracle's carry-over rule).
    identity: String,
}

/// The mutable ledger behind [`Client::capability_status`].
#[derive(Debug, Default)]
pub(crate) struct CapabilityLedger {
    generation: u64,
    /// `liveEpoch` — bumped by every bootstrap invalidation. A note or
    /// refresh captured under a stale epoch is dropped: its evidence was
    /// collected against the previous connection's server.
    live_epoch: u64,
    /// `pingIdentity` of the last successfully pinged server — `""` until
    /// then. A changed identity means a different server build answered
    /// (live handoff, socket retarget): old notes stop applying.
    identity: String,
    report: CapabilityReport,
    notes: BTreeMap<String, FeatureNote>,
}

impl CapabilityLedger {
    /// `updateFeature` — record observed evidence immediately: it lands in
    /// the published report (the observation was just made against whatever
    /// server is answering now) and in the note table consulted by future
    /// refreshes, tagged with the current server identity.
    pub(crate) fn note(&mut self, name: &str, state: FeatureState, reason: impl Into<String>) {
        self.note_at(self.live_epoch, name, state, reason);
    }

    /// `updateFeatureAt` — a note captured under `epoch` lands only while
    /// that epoch is still live; a bootstrap that ran meanwhile means the
    /// reply came from the previous server.
    pub(crate) fn note_at(
        &mut self,
        epoch: u64,
        name: &str,
        state: FeatureState,
        reason: impl Into<String>,
    ) {
        if epoch != self.live_epoch {
            return;
        }
        let reason = reason.into();
        let note = FeatureNote {
            state,
            reason: reason.clone(),
            identity: self.identity.clone(),
        };
        self.notes.insert(name.to_owned(), note);
        self.set_published(name, state, &reason);
    }

    /// Write evidence into the published report, bumping the generation when
    /// it actually changes something (`updateFeature`'s early return).
    fn set_published(&mut self, name: &str, state: FeatureState, reason: &str) {
        let current = self.report.feature(name);
        if current.state == state && current.reason == reason {
            return;
        }
        self.generation += 1;
        self.report.generation = self.generation;
        self.report.features.insert(
            name.to_owned(),
            FeatureEvidence {
                state,
                reason: reason.to_owned(),
                generation: self.generation,
            },
        );
    }

    /// `reusableFeature` — a note survives a refresh when the server
    /// identity still matches, or when the note predates identification
    /// (`""` — adopted into the first identified server by
    /// [`apply_refresh`](Self::apply_refresh); notes are only ever tagged
    /// `""` before the first successful ping).
    fn note_for(&self, name: &str, identity: &str) -> Option<&FeatureNote> {
        let note = self.notes.get(name)?;
        if note.identity == identity || note.identity.is_empty() {
            Some(note)
        } else {
            None
        }
    }

    /// The currently published report (read-only access for `client.rs`).
    pub(crate) fn report(&self) -> &CapabilityReport {
        &self.report
    }

    /// `m.epoch()` — the live epoch callers capture before dispatching.
    pub(crate) fn live_epoch(&self) -> u64 {
        self.live_epoch
    }

    /// One fresh note's verdict — `client.rs`'s per-probe lookup.
    pub(crate) fn fresh_note(&self, name: &str, identity: &str) -> Option<(FeatureState, String)> {
        self.note_for(name, identity)
            .map(|n| (n.state, n.reason.clone()))
    }

    /// Every note still applicable to `identity` — the refresh overlay.
    /// Same freshness rule as [`note_for`](Self::note_for).
    pub(crate) fn fresh_notes(&self, identity: &str) -> Vec<(String, FeatureState, String)> {
        self.notes
            .iter()
            .filter(|(_, note)| note.identity == identity || note.identity.is_empty())
            .map(|(name, note)| (name.clone(), note.state, note.reason.clone()))
            .collect()
    }

    /// `InvalidateLiveCapabilities` — a bootstrap is underway: the verdicts
    /// collected against the previous connection may describe a different
    /// server build (live handoff, socket retarget), so every
    /// socket-observed feature drops back to `unknown`/`reconnect_required`
    /// and the server identity clears. Notes keep their recorded identity:
    /// they stop applying to whatever answers next, and apply again only if
    /// a ping re-identifies that same server. Generation bumps once for the
    /// sweep, like the oracle's `invalidateMany`.
    pub(crate) fn invalidate_live(&mut self) {
        self.live_epoch += 1;
        self.identity.clear();
        self.report.server_version.clear();
        self.report.server_protocol = 0;
        self.report.server_protocol_known = false;
        self.report.endpoint_protocol_generation = None;
        self.report.surface_interest = None;
        self.report.health_check = None;
        let mut changed = false;
        for name in LIVE_FEATURES {
            let current = self.report.feature(name);
            if current.state == FeatureState::Unknown && current.reason == "reconnect_required" {
                continue;
            }
            changed = true;
            self.report.features.insert(
                (*name).to_owned(),
                FeatureEvidence {
                    state: FeatureState::Unknown,
                    reason: "reconnect_required".to_owned(),
                    generation: 0,
                },
            );
        }
        if changed {
            self.generation += 1;
            self.report.generation = self.generation;
            for name in LIVE_FEATURES {
                if let Some(feature) = self.report.features.get_mut(*name) {
                    if feature.reason == "reconnect_required" {
                        feature.generation = self.generation;
                    }
                }
            }
        }
    }

    /// `applyRefresh` — install a freshly built report: bump the ledger
    /// generation (the report's tick counter), stamp each feature with the
    /// generation its evidence last *changed* — unchanged evidence keeps
    /// its generation, so a no-change refresh leaves the published status
    /// identical and `Topology::set_herdr_status` stays silent — and
    /// adopt the discovered server identity (pre-identification notes are
    /// attributed to it). A refresh that started under a stale epoch
    /// (a bootstrap invalidated mid-collect) is dropped.
    pub(crate) fn apply_refresh(&mut self, epoch: u64, mut report: CapabilityReport) {
        if epoch != self.live_epoch {
            return;
        }
        self.generation += 1;
        report.generation = self.generation;
        for (name, evidence) in report.features.iter_mut() {
            evidence.generation = match self.report.features.get(name) {
                Some(prev) if prev.state == evidence.state && prev.reason == evidence.reason => {
                    prev.generation
                }
                _ => self.generation,
            };
        }
        if self.identity.is_empty() && !report.server_version.is_empty() {
            for note in self.notes.values_mut() {
                if note.identity.is_empty() {
                    note.identity = report_identity(&report);
                }
            }
        }
        self.identity = report_identity(&report);
        self.report = report;
    }
}

/// `pingIdentity` — version + protocol + endpoint generation + interest
/// flags; `""` while the server has never answered.
fn report_identity(report: &CapabilityReport) -> String {
    if report.server_version.is_empty() && !report.server_protocol_known {
        return String::new();
    }
    format!(
        "{}\0{}\0{}\0{}\0{}",
        report.server_version,
        report.server_protocol,
        report.endpoint_protocol_generation.unwrap_or(0),
        report.surface_interest.unwrap_or(false),
        report.health_check.unwrap_or(false),
    )
}

/// `capabilityTransportReason` — NotStarted means nothing reached the
/// socket; a timeout anywhere in flight is `timeout`; bytes out but no
/// usable reply is `server_reply_unavailable`.
fn transport_reason(err: &HerdrError) -> &'static str {
    let timed_out = err
        .io_error()
        .is_some_and(|e| e.kind() == io::ErrorKind::TimedOut);
    if timed_out {
        return "timeout";
    }
    match err.phase() {
        DispatchPhase::NotStarted => "server_unavailable",
        DispatchPhase::DispatchedUnknown => "server_reply_unavailable",
        DispatchPhase::Refused => "probe_failed",
    }
}

/// The probes' shared tail: a recognized validation refusal proves the
/// method exists; an unknown-method refusal proves it does not; everything
/// else is inconclusive. Mirrors `probeWorkspaceMoveBlock`/`probeTabMove`/
/// `probePaneRead`'s error tail exactly.
fn probe_verdict(err: &HerdrError, validation_code: &str) -> (FeatureState, &'static str) {
    if let HerdrError::Refused { code, .. } = err {
        if code == validation_code {
            return (FeatureState::Supported, "recognized_validation_refusal");
        }
        if UNKNOWN_METHOD_CODES.contains(&code.as_str()) {
            return (FeatureState::Unsupported, "method_not_supported");
        }
        // Any other refusal is a definitive answer about *this* request —
        // the method exists but rejected the probe shape.
        return (FeatureState::Unknown, "probe_failed");
    }
    if err
        .io_error()
        .is_some_and(|e| e.kind() == io::ErrorKind::TimedOut)
    {
        return (FeatureState::Unknown, "timeout");
    }
    match err.phase() {
        DispatchPhase::NotStarted => (FeatureState::Unknown, "server_unavailable"),
        // Wrote the request but could not classify the answer — the oracle
        // calls this `probe_failed`, not `server_reply_unavailable`.
        DispatchPhase::DispatchedUnknown => (FeatureState::Unknown, "probe_failed"),
        DispatchPhase::Refused => unreachable!("refused handled above"),
    }
}

/// `probeWorkspaceMoveBlock` — an empty `workspace_ids` list moves nothing;
/// success or the validation refusal both prove the method. Probes run
/// untracked: the refresh adjudicates the evidence itself, and a note
/// written mid-collect would be tagged with the *previous* server identity.
async fn probe_workspace_move_block(client: &Client) -> (FeatureState, &'static str) {
    match client
        .call_untracked(
            "workspace.move_block",
            &json!({ "workspace_ids": Vec::<String>::new() }),
        )
        .await
    {
        Ok(raw) => {
            if raw.get("type").and_then(Value::as_str) == Some("workspace_list") {
                (FeatureState::Supported, "probe_succeeded")
            } else {
                (FeatureState::Unknown, "unexpected_probe_result")
            }
        }
        Err(err) => probe_verdict(&err, "workspace_move_block_failed"),
    }
}

/// `probeTabMove` — a blank `tab_id` can only ever fail validation.
async fn probe_tab_move(client: &Client) -> (FeatureState, &'static str) {
    match client
        .call_untracked("tab.move", &json!({ "tab_id": "", "insert_index": 0 }))
        .await
    {
        Ok(_) => (FeatureState::Unknown, "unexpected_probe_success"),
        Err(err) => probe_verdict(&err, "tab_not_found"),
    }
}

/// `probePaneRead` — a blank `pane_id` is validated before any output is
/// touched, so the refusal proves the method without reading a terminal.
async fn probe_pane_read(client: &Client) -> (FeatureState, &'static str) {
    match client
        .call_untracked(
            "pane.read",
            &json!({
                "pane_id": "",
                "source": "visible",
                "lines": 1,
                "format": "ansi",
                "strip_ansi": false,
            }),
        )
        .await
    {
        Ok(_) => (FeatureState::Unknown, "unexpected_probe_success"),
        Err(err) => probe_verdict(&err, "pane_not_found"),
    }
}

/// `installedClientVersion` — `herdr --version`, first line, ≤64 chars, ""
/// on any failure (the binary may simply not exist).
async fn installed_client_version(client: &Client) -> String {
    let bin = resolve_herdr_bin(client.herdr_bin().as_deref());
    match run_cli(
        &bin,
        client.socket_path_hint().as_deref(),
        &["--version"],
        Duration::from_secs(1),
    )
    .await
    {
        Ok(out) => {
            let text = String::from_utf8_lossy(&out);
            let line = text.lines().next().unwrap_or("").trim();
            line.chars().take(64).collect()
        }
        Err(_) => String::new(),
    }
}

/// `decodePing` — distinguish a malformed `pong` (`malformed_ping`) from a
/// transport failure so the evidence says which.
fn decode_pong(raw: &Value) -> Result<Pong, ()> {
    #[derive(serde::Deserialize)]
    struct PongWire {
        version: Option<String>,
        protocol: Option<i64>,
        #[serde(default)]
        capabilities: Option<crate::types::ServerCapabilities>,
    }
    if raw.get("type").and_then(Value::as_str) != Some("pong") {
        return Err(());
    }
    let wire: PongWire = serde_json::from_value(raw.clone()).map_err(|_| ())?;
    let (version, protocol) = match (wire.version, wire.protocol) {
        (Some(v), Some(p)) if !v.is_empty() && p > 0 => (v, p),
        _ => return Err(()),
    };
    Ok(Pong {
        version,
        protocol: protocol as u32,
        capabilities: wire.capabilities,
    })
}

/// Whether the schema belongs to the answering server: a stated protocol
/// must match `pong.protocol`, and a resolved installed version must match
/// `pong.version` when both are known. `herdr api schema` prints the schema
/// bundled into the binary — a binary older/newer than the running server
/// would describe the wrong API surface.
fn schema_trustworthy(schema: &SchemaRegistry, pong: &Pong, installed: &str) -> bool {
    if let Some(protocol) = schema.protocol() {
        if protocol != u64::from(pong.protocol) {
            return false;
        }
    }
    let installed_token = installed
        .split_whitespace()
        .last()
        .unwrap_or("")
        .trim_start_matches('v');
    if !installed_token.is_empty()
        && !pong.version.is_empty()
        && installed_token != pong.version.trim_start_matches('v')
    {
        return false;
    }
    true
}

/// The refresh itself — `capabilityManager.refresh`. Never fails: every
/// failure mode lands as `unknown` evidence with a reason.
pub(crate) async fn collect_capabilities(client: &Client) -> CapabilityReport {
    // One refresh at a time — the oracle's refreshMu.
    let _guard = client.capability_refresh_lock().await;
    // `epoch := m.epoch()` — captured before any socket I/O; a bootstrap
    // invalidating mid-collect makes this refresh's report stale.
    let epoch = client.capability_epoch();

    let installed = installed_client_version(client).await;
    let mut next = CapabilityReport {
        installed_client_version: installed.clone(),
        ..CapabilityReport::default()
    };
    for name in BASE_FEATURES {
        next.features.insert(
            (*name).to_owned(),
            FeatureEvidence {
                state: FeatureState::Unknown,
                reason: "not_checked".to_owned(),
                generation: 0,
            },
        );
    }

    // ping — the ordinary_json probe and the server's identity source.
    let pong = match client.call("ping", &json!({})).await {
        Ok(raw) => match decode_pong(&raw) {
            Ok(pong) => pong,
            Err(()) => {
                next.features
                    .get_mut(features::ORDINARY_JSON)
                    .unwrap()
                    .reason = "malformed_ping".to_owned();
                return apply_and_clone(client, epoch, next);
            }
        },
        Err(err) => {
            next.features
                .get_mut(features::ORDINARY_JSON)
                .unwrap()
                .reason = transport_reason(&err).to_owned();
            return apply_and_clone(client, epoch, next);
        }
    };

    next.server_version = pong.version.clone();
    next.server_protocol = i64::from(pong.protocol);
    next.server_protocol_known = true;
    let caps = pong.capabilities.clone().unwrap_or_default();
    next.endpoint_protocol_generation = caps.endpoint_protocol_generation.map(i64::from);
    next.surface_interest = Some(caps.surface_interest);
    next.health_check = Some(caps.health_check);
    next.features.insert(
        features::ORDINARY_JSON.to_owned(),
        FeatureEvidence {
            state: FeatureState::Supported,
            reason: "ping".to_owned(),
            generation: 0,
        },
    );
    next.features.insert(
        features::CLIENT_SHELL_ENDPOINT.to_owned(),
        FeatureEvidence {
            state: if caps.endpoint_protocol_generation.is_some() {
                FeatureState::Supported
            } else {
                FeatureState::Unknown
            },
            reason: if caps.endpoint_protocol_generation.is_some() {
                "server_advertised"
            } else {
                "not_advertised"
            }
            .to_owned(),
            generation: 0,
        },
    );

    // `pingIdentity` — the server identity notes are attributed to.
    let identity = format!(
        "{}\0{}\0{}\0{}\0{}",
        pong.version,
        pong.protocol,
        caps.endpoint_protocol_generation.unwrap_or(0),
        caps.surface_interest,
        caps.health_check,
    );

    // Schema evidence — trusted only when it describes the answering server.
    let schema = client
        .api_schema()
        .await
        .ok()
        .filter(|reg| reg.is_usable() && schema_trustworthy(reg, &pong, &installed));

    match schema {
        Some(registry) => {
            for method in TRACKED_METHODS {
                let evidence = if registry.supports_method(method) {
                    FeatureEvidence {
                        state: FeatureState::Supported,
                        reason: "schema_advertised".to_owned(),
                        generation: 0,
                    }
                } else {
                    FeatureEvidence {
                        state: FeatureState::Unsupported,
                        reason: "schema_absent".to_owned(),
                        generation: 0,
                    }
                };
                next.features.insert((*method).to_owned(), evidence);
            }
            // The event variant, not a method — adjudicated from the
            // subscription/event tables. An observed subscription outcome
            // (already recorded by `subscribe_topology` this bootstrap)
            // still wins below.
            let reordered = if registry.supports_event(features::WORKSPACE_REORDERED)
                || registry.supports_subscription(features::WORKSPACE_REORDERED)
            {
                FeatureEvidence {
                    state: FeatureState::Supported,
                    reason: "schema_advertised".to_owned(),
                    generation: 0,
                }
            } else {
                FeatureEvidence {
                    state: FeatureState::Unsupported,
                    reason: "schema_absent".to_owned(),
                    generation: 0,
                }
            };
            next.features
                .insert(features::WORKSPACE_REORDERED.to_owned(), reordered);
            // `pane.output_changed` — adjudicated on the *subscription*
            // table alone: 0.9.1 ships `pane_output_changed` in `EventData`
            // but no matching `Subscription` variant (the live server
            // rejects it as an unknown variant), so `supports_event`'s
            // event-table leg would be a false positive here.
            let output_changed = if registry.supports_subscription(features::PANE_OUTPUT_CHANGED) {
                FeatureEvidence {
                    state: FeatureState::Supported,
                    reason: "schema_advertised".to_owned(),
                    generation: 0,
                }
            } else {
                FeatureEvidence {
                    state: FeatureState::Unsupported,
                    reason: "schema_absent".to_owned(),
                    generation: 0,
                }
            };
            next.features
                .insert(features::PANE_OUTPUT_CHANGED.to_owned(), output_changed);
        }
        None => {
            // Probe fallback — schema absent or untrusted. Reusable
            // observed evidence (same server identity) skips the probe,
            // matching `reusableFeature`.
            for name in [
                features::WORKSPACE_MOVE_BLOCK,
                features::TAB_MOVE,
                features::PANE_READ,
            ] {
                let (state, reason) = match client.ledger_note_for(name, &identity) {
                    Some((state, reason)) => (state, reason),
                    None => {
                        let (state, reason) = match name {
                            n if n == features::WORKSPACE_MOVE_BLOCK => {
                                probe_workspace_move_block(client).await
                            }
                            n if n == features::TAB_MOVE => probe_tab_move(client).await,
                            _ => probe_pane_read(client).await,
                        };
                        (state, reason.to_owned())
                    }
                };
                next.features.insert(
                    name.to_owned(),
                    FeatureEvidence {
                        state,
                        reason,
                        generation: 0,
                    },
                );
            }
        }
    }

    // Observed evidence beats inference: overlay every note whose server
    // identity still matches (or that predates identification). This is how
    // the `workspace.reordered` subscription outcome reaches the report —
    // `subscribe_topology` records `subscription_acknowledged` /
    // `subscription_rejected` via `note_feature`, so a *skipped* attempt
    // (schema said absent) keeps its `schema_absent` reason instead of
    // being mislabeled a rejection.
    for (name, state, reason) in client.ledger_notes_for(&identity) {
        next.features.insert(
            name,
            FeatureEvidence {
                state,
                reason,
                generation: 0,
            },
        );
    }

    apply_and_clone(client, epoch, next)
}

fn apply_and_clone(client: &Client, epoch: u64, report: CapabilityReport) -> CapabilityReport {
    client.apply_capability_report(epoch, report)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn feature_state_strings() {
        assert_eq!(FeatureState::Supported.as_str(), "supported");
        assert_eq!(FeatureState::Unsupported.as_str(), "unsupported");
        assert_eq!(FeatureState::Unknown.as_str(), "unknown");
    }

    #[test]
    fn absent_feature_reads_unknown() {
        let report = CapabilityReport {
            generation: 7,
            ..CapabilityReport::default()
        };
        let f = report.feature("pane.read");
        assert_eq!(f.state, FeatureState::Unknown);
        assert_eq!(f.reason, "not_checked");
        assert_eq!(f.generation, 7);
    }

    #[test]
    fn note_updates_published_and_generation() {
        let mut ledger = CapabilityLedger::default();
        ledger.note("pane.read", FeatureState::Supported, "operation_succeeded");
        assert_eq!(
            ledger.report.feature("pane.read").state,
            FeatureState::Supported
        );
        assert_eq!(ledger.generation, 1);
        // Same evidence again — no churn.
        ledger.note("pane.read", FeatureState::Supported, "operation_succeeded");
        assert_eq!(ledger.generation, 1);
    }

    #[test]
    fn notes_stop_applying_after_identity_change() {
        let mut ledger = CapabilityLedger {
            identity: "old\0x".to_owned(),
            ..CapabilityLedger::default()
        };
        ledger.note(
            "pane.read",
            FeatureState::Unsupported,
            "method_not_supported",
        );
        assert!(ledger.note_for("pane.read", "new\0x").is_none());
        assert!(ledger.note_for("pane.read", "old\0x").is_some());
    }

    #[test]
    fn invalidate_drops_stale_epoch_notes_and_refreshes() {
        let mut ledger = CapabilityLedger::default();
        ledger.note("pane.read", FeatureState::Supported, "operation_succeeded");
        let stale_epoch = ledger.live_epoch();

        ledger.invalidate_live();
        // Live features reset to unknown/reconnect_required.
        let f = ledger.report().feature("pane.read");
        assert_eq!(f.state, FeatureState::Unknown);
        assert_eq!(f.reason, "reconnect_required");

        // A note captured before the bootstrap lands nowhere.
        ledger.note_at(
            stale_epoch,
            "pane.read",
            FeatureState::Unsupported,
            "method_not_supported",
        );
        assert_eq!(
            ledger.report().feature("pane.read").reason,
            "reconnect_required"
        );

        // Same for a refresh that started under the stale epoch.
        let mut report = CapabilityReport::default();
        report.features.insert(
            "pane.read".to_owned(),
            FeatureEvidence {
                state: FeatureState::Supported,
                reason: "schema_advertised".to_owned(),
                generation: 0,
            },
        );
        let before = ledger.report().clone();
        ledger.apply_refresh(stale_epoch, report);
        assert_eq!(ledger.report(), &before);

        // Current-epoch writes still land.
        ledger.note(
            "pane.read",
            FeatureState::Unsupported,
            "method_not_supported",
        );
        assert_eq!(
            ledger.report().feature("pane.read").state,
            FeatureState::Unsupported
        );
    }

    #[test]
    fn invalidation_is_idempotent() {
        let mut ledger = CapabilityLedger::default();
        ledger.note("pane.read", FeatureState::Supported, "operation_succeeded");
        ledger.invalidate_live();
        let once = ledger.report().clone();
        ledger.invalidate_live();
        // A second invalidate bumps the epoch but leaves the published
        // features already at reconnect_required.
        assert_eq!(
            ledger.report().feature("pane.read"),
            once.feature("pane.read")
        );
    }
}

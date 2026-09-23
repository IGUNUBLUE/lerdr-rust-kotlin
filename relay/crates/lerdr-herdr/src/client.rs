//! The Herdr socket client: one fresh connection per request, bounded by a
//! dial semaphore, with singleflight fan-out on read-only methods.
//!
//! Every `call*` returns [`HerdrError`] with the dispatch boundary intact —
//! callers can always distinguish "safe to retry" (`NotStarted`) from "may
//! have applied" (`DispatchedUnknown`) from "definitively did not apply"
//! (`Refused`).

use std::io;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, AtomicU8, Ordering};
use std::sync::Arc;
use std::time::Duration;

use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::{json, Value};
use tokio::sync::Semaphore;
use tokio::time::Instant;
use tracing::instrument;

use crate::capabilities::{
    features, CapabilityLedger, CapabilityReport, FeatureEvidence, FeatureState, NOTED_METHODS,
    UNKNOWN_METHOD_CODES,
};
use crate::error::{BootstrapError, DispatchPhase, HerdrError, SubscribeError};
use crate::events::{
    self, topology_subscriptions, Bootstrap, Event, EventStream, EventSupervisor, Subscription,
    SupervisorStream, EVENTS_REQUEST_ID,
};
use crate::schema::{SchemaError, SchemaRegistry, SchemaSource};
use crate::singleflight::Singleflight;
use crate::transport::{default_socket_path, Transport, UnixTransport};
use crate::types::*;
use crate::wire;

/// Client configuration.
#[derive(Debug, Clone)]
pub struct ClientConfig {
    /// Cap on simultaneous requests — each in-flight request is one open fd
    /// to Herdr, so a thundering herd of watch probes cannot fd-storm it.
    /// Default 16.
    pub max_in_flight: usize,
    /// Deadline covering dial + write + response read for a unary call.
    /// Default 15s (the Go client's `defaultTimeout`).
    pub request_timeout: Duration,
    /// Cap on a single response/event line. Default 4 MiB (Herdr's own
    /// `maxOutputBytes`).
    pub max_response_bytes: usize,
    /// Events buffered between the socket reader task and the stream
    /// consumer before `EventStreamError::Lagged` fires. Default 1024.
    pub event_queue_capacity: usize,
    /// Retry `pane.read` once when the first attempt fails non-definitively
    /// (`NotStarted`/`DispatchedUnknown`). Mirrors the Go client's
    /// read-then-retry behavior; `Refused` is never retried.
    pub read_retry: bool,
    /// Request-id prefix (`lerdr-api-N`).
    pub id_prefix: String,
    /// Explicit `herdr` binary for CLI introspection (`api schema`,
    /// `--version`). `None` resolves `HERDR_BIN`/`HERDR_BIN_PATH`, `PATH`,
    /// then the known install locations — `findHerdrBin` in the oracle.
    pub herdr_bin: Option<PathBuf>,
    /// Where the API schema comes from at capability refresh. Default
    /// [`SchemaSource::Cli`]; tests inject [`SchemaSource::Static`] or
    /// [`SchemaSource::Disabled`] to stay off the host's `herdr` binary.
    pub schema_source: SchemaSource,
}

impl Default for ClientConfig {
    fn default() -> Self {
        ClientConfig {
            max_in_flight: 16,
            request_timeout: wire::DEFAULT_REQUEST_TIMEOUT,
            max_response_bytes: wire::MAX_LINE_BYTES,
            event_queue_capacity: 1024,
            read_retry: true,
            id_prefix: "lerdr-api".to_string(),
            herdr_bin: None,
            schema_source: SchemaSource::default(),
        }
    }
}

/// Methods routed through singleflight: pure reads where N concurrent
/// identical calls collapse to one socket round-trip. Waits are deliberately
/// excluded — each wait has its own timeout semantics, and cancelling one
/// waiter must not cancel another's.
const SINGLEFLIGHT_METHODS: &[&str] = &[
    "ping",
    "session.snapshot",
    "agent.list",
    "agent.explain",
    "pane.list",
    "pane.read",
    "workspace.list",
    "tab.list",
];

/// Optional-subscription capability tri-states — probed optimistically on
/// each bootstrap, matching the Go client's reset→attempt→note lifecycle.
const SUBSCRIPTION_UNKNOWN: u8 = 0;
const SUBSCRIPTION_SUPPORTED: u8 = 1;
const SUBSCRIPTION_UNSUPPORTED: u8 = 2;

/// The optional subscription variants one `subscribe_topology` attempt
/// still carries — entries drop out as Herdr's `unknown variant` refusals
/// name them, so a server accepting the reduced set stops the retry.
#[derive(Clone, Copy)]
struct SubscriptionAttempt {
    workspace_reordered: bool,
    pane_output_changed: bool,
}

impl SubscriptionAttempt {
    /// Drop the entry Herdr's `unknown variant` refusal named. `false`
    /// when the refusal names a variant this attempt is not requesting —
    /// nothing left to retry, the error surfaces.
    fn drop_rejected(&mut self, variant: &str) -> bool {
        if variant == features::WORKSPACE_REORDERED && self.workspace_reordered {
            self.workspace_reordered = false;
            return true;
        }
        if variant == features::PANE_OUTPUT_CHANGED && self.pane_output_changed {
            self.pane_output_changed = false;
            return true;
        }
        false
    }
}

struct ClientInner {
    transport: Arc<dyn Transport>,
    config: ClientConfig,
    /// One permit per in-flight request — bounds concurrent fds to Herdr.
    dial_semaphore: Semaphore,
    seq: AtomicU64,
    flights: Singleflight,
    workspace_reordered: AtomicU8,
    pane_output_changed: AtomicU8,
    /// The capability ledger — last published report plus observed notes.
    /// `std::sync::Mutex`: mutations are short map writes, never held across
    /// an `.await`.
    capabilities: std::sync::Mutex<CapabilityLedger>,
    /// Serializes `collect_capabilities` refreshes (`refreshMu`).
    capability_refresh: tokio::sync::Mutex<()>,
}

/// A client for the Herdr socket API. Cheap to clone — all state is shared.
///
/// Construct via [`Client::unix`], [`Client::from_env`], or
/// [`Client::new`] with a custom [`Transport`].
#[derive(Clone)]
pub struct Client {
    inner: Arc<ClientInner>,
}

impl Client {
    /// Wrap an arbitrary transport (test doubles, a future Windows named
    /// pipe).
    pub fn new(transport: Arc<dyn Transport>, config: ClientConfig) -> Self {
        Client {
            inner: Arc::new(ClientInner {
                transport,
                dial_semaphore: Semaphore::new(config.max_in_flight.max(1)),
                config,
                seq: AtomicU64::new(0),
                flights: Singleflight::default(),
                workspace_reordered: AtomicU8::new(SUBSCRIPTION_UNKNOWN),
                pane_output_changed: AtomicU8::new(SUBSCRIPTION_UNKNOWN),
                capabilities: std::sync::Mutex::new(CapabilityLedger::default()),
                capability_refresh: tokio::sync::Mutex::new(()),
            }),
        }
    }

    /// Client for a Unix socket path.
    pub fn unix(path: impl Into<PathBuf>) -> Self {
        Client::new(
            Arc::new(UnixTransport::new(path.into())),
            ClientConfig::default(),
        )
    }

    /// Client for a Unix socket path with an explicit config.
    pub fn unix_with(path: impl Into<PathBuf>, config: ClientConfig) -> Self {
        Client::new(Arc::new(UnixTransport::new(path.into())), config)
    }

    /// Client for the socket resolved from the environment
    /// (`HERDR_SOCKET_PATH` → `HERDR_SESSION` → `~/.config/herdr/herdr.sock`).
    /// `None` when no path can be resolved.
    pub fn from_env() -> Option<Self> {
        default_socket_path().map(Client::unix)
    }

    /// The transport target, for logs.
    pub fn describe(&self) -> String {
        self.inner.transport.describe()
    }

    fn next_id(&self) -> String {
        format!(
            "{}-{}",
            self.inner.config.id_prefix,
            self.inner.seq.fetch_add(1, Ordering::Relaxed) + 1
        )
    }

    /// Raw NDJSON call: `{"id","method","params"}` → the `result` payload as a
    /// [`Value`], or a [`HerdrError`] carrying the dispatch boundary.
    ///
    /// This is the escape hatch for methods without a typed wrapper — and for
    /// `command.invoke`, whose result type the upstream schema does not pin
    /// down.
    #[instrument(skip_all, fields(method, transport = %self.inner.transport.describe()))]
    pub async fn call<P: Serialize + Sync>(
        &self,
        method: &str,
        params: &P,
    ) -> Result<Value, HerdrError> {
        self.call_inner(
            method,
            params,
            Some(self.inner.config.request_timeout),
            true,
        )
        .await
    }

    /// Like [`call`](Self::call) with an explicit deadline — `None` waits
    /// indefinitely (server-side waits use this).
    pub async fn call_with_timeout<P: Serialize + Sync>(
        &self,
        method: &str,
        params: &P,
        timeout: Option<Duration>,
    ) -> Result<Value, HerdrError> {
        self.call_inner(method, params, timeout, true).await
    }

    /// A request that leaves no capability note — the capability probes
    /// adjudicate their own evidence, and a note written mid-collect would
    /// be tagged with the *previous* server's identity (the ledger only
    /// learns the new one at `apply_refresh`).
    pub(crate) async fn call_untracked<P: Serialize + Sync>(
        &self,
        method: &str,
        params: &P,
    ) -> Result<Value, HerdrError> {
        self.call_inner(
            method,
            params,
            Some(self.inner.config.request_timeout),
            false,
        )
        .await
    }

    async fn call_inner<P: Serialize + Sync>(
        &self,
        method: &str,
        params: &P,
        timeout: Option<Duration>,
        track: bool,
    ) -> Result<Value, HerdrError> {
        // `epoch := c.capabilityEpoch()` — captured before dispatch so a
        // reply arriving after a bootstrap invalidation is dropped as
        // previous-server evidence.
        let epoch = self.capability_epoch();
        let deadline = Instant::now() + timeout.unwrap_or(events::FAR_FUTURE);
        let request_id = self.next_id();
        let payload = wire::encode_request(&request_id, method, params)?;

        // One fd per request: hold a semaphore permit for the connection's
        // whole lifetime. A stalled acquire inside the deadline means the
        // request never started.
        let _permit = tokio::time::timeout_at(deadline, self.inner.dial_semaphore.acquire())
            .await
            .map_err(|_| {
                HerdrError::not_started(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "herdr dial semaphore wait timed out",
                ))
            })?
            .map_err(|_| {
                HerdrError::not_started(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "herdr dial semaphore closed",
                ))
            })?;

        let mut conn = tokio::time::timeout_at(deadline, self.inner.transport.dial())
            .await
            .map_err(|_| {
                HerdrError::not_started(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "herdr dial timed out",
                ))
            })?
            .map_err(HerdrError::not_started)?;

        wire::write_request(&mut conn, &payload, deadline).await?;
        let line = wire::read_line(&mut conn, deadline, self.inner.config.max_response_bytes)
            .await
            .map_err(HerdrError::dispatched_io)?;
        let response = wire::decode_response(&line)?;
        let result = wire::classify_response(response, &request_id)
            .map(|raw| serde_json::from_str(raw.get()).unwrap_or(Value::Null));
        // `noteSocketFeature` — a definitive answer about a tracked method
        // is capability evidence, whatever the caller does with it. The
        // epoch was captured before dispatch: a bootstrap that ran
        // meanwhile means this reply came from the previous server.
        if track && NOTED_METHODS.contains(&method) {
            self.note_socket_feature(epoch, method, &result);
        }
        result
    }

    /// Route through singleflight when `method` is read-only.
    async fn call_shared<P: Serialize + Sync>(
        &self,
        method: &str,
        params: &P,
    ) -> Result<Value, HerdrError> {
        if !SINGLEFLIGHT_METHODS.contains(&method) {
            return self.call(method, params).await;
        }
        let params_value = serde_json::to_value(params)
            .map_err(|e| HerdrError::not_started(io::Error::new(io::ErrorKind::InvalidData, e)))?;
        let key = format!("{method}\0{params_value}");
        self.inner
            .flights
            .execute(key, || self.call(method, &params_value))
            .await
    }

    /// Decode `result` as `T` after asserting its `type` tag — a mismatched
    /// tag means the response cannot be trusted (`DispatchedUnknown`, like
    /// the Go client's `decodeSocketResult`).
    async fn call_result<P, T>(
        &self,
        method: &str,
        params: &P,
        want_type: &str,
    ) -> Result<T, HerdrError>
    where
        P: Serialize + Sync,
        T: DeserializeOwned,
    {
        let raw = self.call_shared(method, params).await?;
        Self::decode_result(raw, want_type)
    }

    /// Same, but never singleflighted and with an explicit timeout (waits and
    /// mutations).
    async fn call_result_opts<P, T>(
        &self,
        method: &str,
        params: &P,
        want_type: &str,
        timeout: Option<Duration>,
    ) -> Result<T, HerdrError>
    where
        P: Serialize + Sync,
        T: DeserializeOwned,
    {
        let raw = self.call_with_timeout(method, params, timeout).await?;
        Self::decode_result(raw, want_type)
    }

    fn decode_result<T: DeserializeOwned>(raw: Value, want_type: &str) -> Result<T, HerdrError> {
        let got = raw.get("type").and_then(Value::as_str);
        if got != Some(want_type) {
            return Err(HerdrError::dispatched_msg(format!(
                "herdr returned result type {got:?}, want {want_type:?}"
            )));
        }
        serde_json::from_value(raw)
            .map_err(|e| HerdrError::dispatched_io(io::Error::new(io::ErrorKind::InvalidData, e)))
    }

    /// Client timeout for a server-side wait: the server's `timeout_ms` plus
    /// margin, or no timeout when the server wait is indefinite.
    fn wait_timeout(timeout_ms: Option<u64>) -> Option<Duration> {
        timeout_ms.map(|ms| Duration::from_millis(ms).saturating_add(Duration::from_secs(10)))
    }

    // -- server ------------------------------------------------------------

    /// `ping` → `pong`: version, protocol, and advertised capabilities.
    pub async fn ping(&self) -> Result<Pong, HerdrError> {
        #[derive(serde::Deserialize)]
        struct PongResult {
            version: String,
            protocol: u32,
            #[serde(default)]
            capabilities: Option<ServerCapabilities>,
        }
        let r: PongResult = self.call_result("ping", &json!({}), "pong").await?;
        Ok(Pong {
            version: r.version,
            protocol: r.protocol,
            capabilities: r.capabilities,
        })
    }

    /// `session.snapshot` — the one-call full topology reconcile and the
    /// `events_lost` recovery base.
    pub async fn session_snapshot(&self) -> Result<SessionSnapshot, HerdrError> {
        #[derive(serde::Deserialize)]
        struct R {
            snapshot: SessionSnapshot,
        }
        Ok(self
            .call_result::<_, R>("session.snapshot", &json!({}), "session_snapshot")
            .await?
            .snapshot)
    }

    // -- inventory reads ---------------------------------------------------

    /// `agent.list` → all detected agents.
    pub async fn agent_list(&self) -> Result<Vec<AgentInfo>, HerdrError> {
        #[derive(serde::Deserialize)]
        struct R {
            agents: Vec<AgentInfo>,
        }
        Ok(self
            .call_result::<_, R>("agent.list", &json!({}), "agent_list")
            .await?
            .agents)
    }

    /// `pane.list` → all panes (optionally scoped to one workspace).
    pub async fn pane_list(&self, workspace_id: Option<&str>) -> Result<Vec<PaneInfo>, HerdrError> {
        #[derive(serde::Deserialize)]
        struct R {
            panes: Vec<PaneInfo>,
        }
        Ok(self
            .call_result::<_, R>(
                "pane.list",
                &json!({ "workspace_id": workspace_id }),
                "pane_list",
            )
            .await?
            .panes)
    }

    /// `workspace.list` → all workspaces.
    pub async fn workspace_list(&self) -> Result<Vec<WorkspaceInfo>, HerdrError> {
        #[derive(serde::Deserialize)]
        struct R {
            workspaces: Vec<WorkspaceInfo>,
        }
        Ok(self
            .call_result::<_, R>("workspace.list", &json!({}), "workspace_list")
            .await?
            .workspaces)
    }

    /// `tab.list` → all tabs (optionally scoped to one workspace).
    pub async fn tab_list(&self, workspace_id: Option<&str>) -> Result<Vec<TabInfo>, HerdrError> {
        #[derive(serde::Deserialize)]
        struct R {
            tabs: Vec<TabInfo>,
        }
        Ok(self
            .call_result::<_, R>(
                "tab.list",
                &json!({ "workspace_id": workspace_id }),
                "tab_list",
            )
            .await?
            .tabs)
    }

    /// `pane.read` — the hot path. Singleflighted, and retried once on
    /// non-definitive failures per the Go client's read-then-retry behavior
    /// (`Refused` is returned immediately).
    pub async fn pane_read(
        &self,
        pane_id: &str,
        source: ReadSource,
        lines: u32,
        format: ReadFormat,
    ) -> Result<PaneReadResult, HerdrError> {
        self.pane_read_opts(&PaneReadParams::new(pane_id, source, lines, format))
            .await
    }

    /// `pane.read` with full param control — callers that need an explicit
    /// `strip_ansi` (e.g. ANSI reads that still strip) build
    /// [`PaneReadParams`] directly. Singleflight keys on the whole
    /// serialized tuple `(pane_id, source, lines, format, strip_ansi)`, so
    /// two reads differing in any field never collapse.
    pub async fn pane_read_opts(
        &self,
        params: &PaneReadParams,
    ) -> Result<PaneReadResult, HerdrError> {
        #[derive(serde::Deserialize)]
        struct R {
            read: PaneReadResult,
        }
        let mut last_err = None;
        for attempt in 0..2u8 {
            match self
                .call_result::<_, R>("pane.read", params, "pane_read")
                .await
            {
                Ok(r) => return Ok(r.read),
                Err(e) => {
                    // `stale_content` is a refusal but a transient one —
                    // the fenced revision raced an in-flight write, so a
                    // re-read lands on the settled write (Herdr's seqlock
                    // contract). One retry, on top of the transport-level
                    // `read_retry` rule.
                    let stale = e.refusal_code() == Some("stale_content");
                    let retry = attempt == 0
                        && (stale
                            || (self.inner.config.read_retry
                                && e.phase() != DispatchPhase::Refused));
                    if !retry {
                        return Err(e);
                    }
                    last_err = Some(e);
                }
            }
        }
        Err(last_err.expect("retry loop ran at least once"))
    }

    /// `pane.send_input` — text and/or logical keys through Herdr's input
    /// path (paste-mode honored). Mutating; no singleflight.
    pub async fn pane_send_input(
        &self,
        pane_id: &str,
        text: Option<&str>,
        keys: Vec<String>,
    ) -> Result<(), HerdrError> {
        self.call_result::<_, Value>(
            "pane.send_input",
            &PaneSendInputParams {
                pane_id: pane_id.to_string(),
                text: text.map(str::to_string),
                keys,
            },
            "ok",
        )
        .await?;
        Ok(())
    }

    // -- waits (server-side, hold the connection until match/timeout) -------

    /// `pane.wait_for_output` — resolve when `match` fires on the pane's
    /// selected source. Server timeout elapses → `Refused{code:"timeout"}`.
    pub async fn pane_wait_for_output(
        &self,
        pane_id: &str,
        source: ReadSource,
        match_: OutputMatch,
        strip_ansi: Option<bool>,
        lines: Option<u32>,
        timeout_ms: Option<u64>,
    ) -> Result<OutputMatchedResult, HerdrError> {
        let params = PaneWaitForOutputParams {
            pane_id: pane_id.to_string(),
            source,
            match_,
            strip_ansi,
            lines,
            timeout_ms,
        };
        self.call_result_opts(
            "pane.wait_for_output",
            &params,
            "output_matched",
            Self::wait_timeout(timeout_ms),
        )
        .await
    }

    /// `agent.wait` — resolve when the resolved agent reaches one of `until`
    /// (empty `until` uses Herdr's settled-state defaults). Returns the
    /// `agent_info` snapshot that satisfied the wait.
    pub async fn agent_wait(
        &self,
        target: &str,
        until: Vec<AgentStatus>,
        timeout_ms: Option<u64>,
    ) -> Result<AgentInfo, HerdrError> {
        #[derive(serde::Deserialize)]
        struct R {
            agent: AgentInfo,
        }
        Ok(self
            .call_result_opts::<_, R>(
                "agent.wait",
                &AgentWaitParams {
                    target: target.to_string(),
                    until,
                    timeout_ms,
                },
                "agent_info",
                Self::wait_timeout(timeout_ms),
            )
            .await?
            .agent)
    }

    /// `events.wait` — one-shot wait for a matching event. Server timeout
    /// elapses → `Refused{code:"timeout"}`. Note the server currently only
    /// honors `pane_agent_status_changed` matches
    /// (`unsupported_event_wait_match` otherwise).
    pub async fn events_wait(
        &self,
        match_: EventMatch,
        timeout_ms: Option<u64>,
    ) -> Result<Event, HerdrError> {
        #[derive(Serialize)]
        struct Params {
            match_event: EventMatch,
            #[serde(skip_serializing_if = "Option::is_none")]
            timeout_ms: Option<u64>,
        }
        #[derive(serde::Deserialize)]
        struct R {
            event: Event,
        }
        Ok(self
            .call_result_opts::<_, R>(
                "events.wait",
                &Params {
                    match_event: match_,
                    timeout_ms,
                },
                "wait_matched",
                Self::wait_timeout(timeout_ms),
            )
            .await?
            .event)
    }

    /// `agent.explain` — the server's detection snapshot for a pane: matched
    /// rule, evaluated evidence, skip reasons. The `explain` object is
    /// free-form upstream (`"explain": true` in the schema), so this returns
    /// raw JSON.
    pub async fn agent_explain(&self, target: &str) -> Result<Value, HerdrError> {
        #[derive(serde::Deserialize)]
        struct R {
            explain: Value,
        }
        Ok(self
            .call_result::<_, R>(
                "agent.explain",
                &json!({ "target": target }),
                "agent_explain",
            )
            .await?
            .explain)
    }

    // -- mutations ----------------------------------------------------------

    /// `agent.view.set` — install the transient declarative projection that
    /// drives Herdr's sidebar and its mobile Agents list.
    pub async fn agent_view_set(
        &self,
        params: AgentViewSetParams,
    ) -> Result<AgentViewState, HerdrError> {
        self.call_result("agent.view.set", &params, "agent_view")
            .await
    }

    /// `agent.view.clear` — clear the projection, optionally only when
    /// `source` still owns it.
    pub async fn agent_view_clear(
        &self,
        source: Option<&str>,
    ) -> Result<AgentViewState, HerdrError> {
        #[derive(Serialize)]
        struct P<'a> {
            #[serde(skip_serializing_if = "Option::is_none")]
            source: Option<&'a str>,
        }
        self.call_result("agent.view.clear", &P { source }, "agent_view")
            .await
    }

    /// `layout.apply` — apply a whole layout tree (workspace templates from
    /// the phone).
    pub async fn layout_apply(
        &self,
        params: LayoutApplyParams,
    ) -> Result<LayoutDescription, HerdrError> {
        #[derive(serde::Deserialize)]
        struct R {
            layout: LayoutDescription,
        }
        self.call_result::<_, R>("layout.apply", &params, "layout_apply")
            .await
            .map(|r| r.layout)
    }

    /// `command.invoke` — an endpoint-issued command id validated against the
    /// pane's content revision. The upstream schema does not pin the success
    /// result type, so the raw result payload is returned.
    pub async fn command_invoke(&self, params: CommandInvokeParams) -> Result<Value, HerdrError> {
        self.call("command.invoke", &params).await
    }

    /// `notification.show` — a desktop toast for phone-originated actions.
    pub async fn notification_show(
        &self,
        params: NotificationShowParams,
    ) -> Result<NotificationOutcome, HerdrError> {
        self.call_result("notification.show", &params, "notification_show")
            .await
    }

    /// `plugin.action.invoke` — drive an installed Herdr plugin's manifest
    /// action through the socket.
    pub async fn plugin_action_invoke(
        &self,
        params: PluginActionInvokeParams,
    ) -> Result<PluginActionInvocation, HerdrError> {
        self.call_result("plugin.action.invoke", &params, "plugin_action_invoked")
            .await
    }

    // -- metadata reporting ---------------------------------------------------

    /// `pane.report_metadata` — merge relay-owned metadata onto a pane:
    /// tokens/state_labels merge per key (`None` token values delete it),
    /// `ttl_ms` bounds the annotation server-side, `seq` orders reports per
    /// `(pane_id, source)`. Answers `ok`.
    pub async fn pane_report_metadata(
        &self,
        params: &PaneReportMetadataParams,
    ) -> Result<(), HerdrError> {
        self.call_result::<_, Value>("pane.report_metadata", params, "ok")
            .await?;
        Ok(())
    }

    /// `workspace.report_metadata` — same merge semantics at workspace
    /// scope (`tokens` is required upstream, an empty map is legal).
    pub async fn workspace_report_metadata(
        &self,
        params: &WorkspaceReportMetadataParams,
    ) -> Result<(), HerdrError> {
        self.call_result::<_, Value>("workspace.report_metadata", params, "ok")
            .await?;
        Ok(())
    }

    // -- client chrome --------------------------------------------------------

    /// `client.window_title.set` — surface a title on Herdr's desktop
    /// chrome ("lerdr: N device(s)" while phones are connected).
    pub async fn client_window_title_set(
        &self,
        title: &str,
    ) -> Result<ClientWindowTitleOutcome, HerdrError> {
        self.call_result(
            "client.window_title.set",
            &ClientWindowTitleSetParams {
                title: title.to_owned(),
            },
            "client_window_title",
        )
        .await
    }

    /// `client.window_title.clear` — restore the default title.
    pub async fn client_window_title_clear(&self) -> Result<ClientWindowTitleOutcome, HerdrError> {
        self.call_result(
            "client.window_title.clear",
            &json!({}),
            "client_window_title",
        )
        .await
    }

    // -- server admin ---------------------------------------------------------

    /// `server.reload_config` — re-read `config.toml` in place.
    pub async fn server_reload_config(&self) -> Result<ConfigReloadOutcome, HerdrError> {
        self.call_result("server.reload_config", &json!({}), "config_reload")
            .await
    }

    /// `server.agent_manifests` — agent-detection manifest status.
    pub async fn server_agent_manifests(&self) -> Result<AgentManifestStatus, HerdrError> {
        self.call_result(
            "server.agent_manifests",
            &json!({}),
            "agent_manifest_status",
        )
        .await
    }

    /// `server.reload_agent_manifests` — reload remote/local manifests and
    /// return the refreshed status rows.
    pub async fn server_reload_agent_manifests(
        &self,
    ) -> Result<Vec<AgentManifestInfo>, HerdrError> {
        #[derive(serde::Deserialize)]
        struct R {
            manifests: Vec<AgentManifestInfo>,
        }
        Ok(self
            .call_result::<_, R>(
                "server.reload_agent_manifests",
                &json!({}),
                "agent_manifest_reload",
            )
            .await?
            .manifests)
    }

    // -- integrations ---------------------------------------------------------

    /// `integration.install` — install a Herdr agent integration's hooks
    /// into its target CLI.
    pub async fn integration_install(
        &self,
        target: IntegrationTarget,
    ) -> Result<IntegrationInstallOutcome, HerdrError> {
        self.call_result(
            "integration.install",
            &IntegrationInstallParams { target },
            "integration_install",
        )
        .await
    }

    /// `integration.uninstall` — remove an integration's hooks.
    pub async fn integration_uninstall(
        &self,
        target: IntegrationTarget,
    ) -> Result<IntegrationUninstallOutcome, HerdrError> {
        self.call_result(
            "integration.uninstall",
            &IntegrationUninstallParams { target },
            "integration_uninstall",
        )
        .await
    }

    // -- plugin driving ---------------------------------------------------------

    /// `plugin.pane.open` — open a manifest-declared pane entrypoint.
    /// Returns the opened pane's record (`plugin_pane_opened`).
    pub async fn plugin_pane_open(
        &self,
        params: &PluginPaneOpenParams,
    ) -> Result<PluginPaneInfo, HerdrError> {
        #[derive(serde::Deserialize)]
        struct R {
            plugin_pane: PluginPaneInfo,
        }
        Ok(self
            .call_result::<_, R>("plugin.pane.open", params, "plugin_pane_opened")
            .await?
            .plugin_pane)
    }

    /// `plugin.pane.focus` — focus an open plugin pane.
    pub async fn plugin_pane_focus(&self, pane_id: &str) -> Result<PluginPaneInfo, HerdrError> {
        #[derive(serde::Deserialize)]
        struct R {
            plugin_pane: PluginPaneInfo,
        }
        Ok(self
            .call_result::<_, R>(
                "plugin.pane.focus",
                &PluginPaneFocusParams {
                    pane_id: pane_id.to_owned(),
                },
                "plugin_pane_focused",
            )
            .await?
            .plugin_pane)
    }

    /// `plugin.pane.close` — close an open plugin pane; returns the closed
    /// `pane_id` echo.
    pub async fn plugin_pane_close(&self, pane_id: &str) -> Result<String, HerdrError> {
        #[derive(serde::Deserialize)]
        struct R {
            pane_id: String,
        }
        Ok(self
            .call_result::<_, R>(
                "plugin.pane.close",
                &PluginPaneCloseParams {
                    pane_id: pane_id.to_owned(),
                },
                "plugin_pane_closed",
            )
            .await?
            .pane_id)
    }

    /// `plugin.enable` — enable an installed plugin.
    pub async fn plugin_enable(&self, plugin_id: &str) -> Result<InstalledPluginInfo, HerdrError> {
        #[derive(serde::Deserialize)]
        struct R {
            plugin: InstalledPluginInfo,
        }
        Ok(self
            .call_result::<_, R>(
                "plugin.enable",
                &PluginSetEnabledParams {
                    plugin_id: plugin_id.to_owned(),
                },
                "plugin_enabled",
            )
            .await?
            .plugin)
    }

    /// `plugin.disable` — disable an installed plugin.
    pub async fn plugin_disable(&self, plugin_id: &str) -> Result<InstalledPluginInfo, HerdrError> {
        #[derive(serde::Deserialize)]
        struct R {
            plugin: InstalledPluginInfo,
        }
        Ok(self
            .call_result::<_, R>(
                "plugin.disable",
                &PluginSetEnabledParams {
                    plugin_id: plugin_id.to_owned(),
                },
                "plugin_disabled",
            )
            .await?
            .plugin)
    }

    /// `plugin.log.list` — the plugin command log (optionally scoped to one
    /// plugin, newest entries last; `limit` caps the count).
    pub async fn plugin_log_list(
        &self,
        plugin_id: Option<&str>,
        limit: Option<u64>,
    ) -> Result<Vec<PluginCommandLogInfo>, HerdrError> {
        #[derive(serde::Deserialize)]
        struct R {
            logs: Vec<PluginCommandLogInfo>,
        }
        Ok(self
            .call_result::<_, R>(
                "plugin.log.list",
                &PluginLogListParams {
                    plugin_id: plugin_id.map(str::to_owned),
                    limit,
                },
                "plugin_log_list",
            )
            .await?
            .logs)
    }

    // -- events -------------------------------------------------------------

    /// `events.subscribe` — perform the handshake and return the live event
    /// stream. `subscription_started` is consumed here; every later line is
    /// an event or a terminal error (`events_lost`).
    ///
    /// The dial semaphore is held only for the handshake — a subscription is
    /// long-lived, not an in-flight request.
    pub async fn subscribe_events(
        &self,
        subscriptions: &[Subscription],
    ) -> Result<EventStream, SubscribeError> {
        let timeout = self.inner.config.request_timeout;
        let (conn, lines) = {
            let _permit = tokio::time::timeout(timeout, self.inner.dial_semaphore.acquire())
                .await
                .map_err(|_| {
                    SubscribeError::transport(HerdrError::not_started(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "herdr dial semaphore wait timed out",
                    )))
                })?
                .map_err(|_| {
                    SubscribeError::transport(HerdrError::not_started_msg(
                        "herdr dial semaphore closed",
                    ))
                })?;
            events::subscribe_on(
                self.inner.transport.as_ref(),
                subscriptions,
                EVENTS_REQUEST_ID,
                timeout,
                self.inner.config.max_response_bytes,
            )
            .await?
        };
        Ok(EventStream::spawn(
            conn,
            lines,
            self.inner.config.event_queue_capacity,
        ))
    }

    /// The topology subscription set with the optional-variant fallback:
    /// attempt each of `workspace.reordered` / `pane.output_changed` while
    /// its capability is not known-unsupported
    /// (`ShouldAttemptWorkspaceReordered` generalized), and on Herdr's
    /// `unknown variant` refusal resubscribe with the named entry dropped
    /// — the Go client's `Bootstrap` retry looped over both optional
    /// entries. Every outcome is recorded into the ledger
    /// (`subscription_acknowledged` / `subscription_rejected`).
    ///
    /// Called standalone, the ledger's published verdict gates each
    /// attempt — a schema `schema_absent` or an earlier same-server
    /// rejection skips the doomed round-trip (0.9.1 lists
    /// `pane_output_changed` among streamed events but ships no matching
    /// `Subscription` variant, so the schema gate is what keeps the
    /// request clean there). Called through [`Client::bootstrap_with`]
    /// the live verdicts have just been invalidated
    /// (`reconnect_required`), so `workspace.reordered`'s consult reads
    /// `unknown` and the variant is re-probed — the oracle's
    /// reset→attempt ordering verbatim. `pane.output_changed` is *not* a
    /// live verdict — its published schema adjudication stays consulted
    /// across reconnects.
    pub async fn subscribe_topology(&self) -> Result<EventStream, SubscribeError> {
        let mut attempt = SubscriptionAttempt {
            workspace_reordered: self.should_attempt_workspace_reordered(),
            pane_output_changed: self.should_attempt_pane_output_changed(),
        };
        // `None`/`Some(_)` from the `*_supported` accessors describes this
        // bootstrap's probe — a skipped attempt probed nothing.
        self.inner
            .workspace_reordered
            .store(SUBSCRIPTION_UNKNOWN, Ordering::Relaxed);
        self.inner
            .pane_output_changed
            .store(SUBSCRIPTION_UNKNOWN, Ordering::Relaxed);
        loop {
            match self
                .subscribe_events(&topology_subscriptions(
                    attempt.workspace_reordered,
                    attempt.pane_output_changed,
                ))
                .await
            {
                Ok(stream) => {
                    if attempt.workspace_reordered {
                        self.inner
                            .workspace_reordered
                            .store(SUBSCRIPTION_SUPPORTED, Ordering::Relaxed);
                        self.note_feature(
                            features::WORKSPACE_REORDERED,
                            FeatureState::Supported,
                            "subscription_acknowledged",
                        );
                    }
                    if attempt.pane_output_changed {
                        self.inner
                            .pane_output_changed
                            .store(SUBSCRIPTION_SUPPORTED, Ordering::Relaxed);
                        self.note_feature(
                            features::PANE_OUTPUT_CHANGED,
                            FeatureState::Supported,
                            "subscription_acknowledged",
                        );
                    }
                    return Ok(stream);
                }
                Err(err) => {
                    let Some(variant) = err.rejected_variant() else {
                        return Err(err);
                    };
                    if !attempt.drop_rejected(variant) {
                        // The refusal named an entry we are not
                        // requesting — nothing to drop, so this is a
                        // genuine handshake failure, not a capability
                        // negotiation.
                        return Err(err);
                    }
                    let flag = if variant == features::WORKSPACE_REORDERED {
                        &self.inner.workspace_reordered
                    } else {
                        &self.inner.pane_output_changed
                    };
                    flag.store(SUBSCRIPTION_UNSUPPORTED, Ordering::Relaxed);
                    self.note_feature(variant, FeatureState::Unsupported, "subscription_rejected");
                }
            }
        }
    }

    /// The documented bootstrap/recovery step verbatim: subscribe →
    /// `subscription_started` → `session.snapshot` on a second connection →
    /// drain the events that arrived in the gap (`gap_events` — invalidation
    /// signals only, never replayed onto the snapshot).
    pub async fn bootstrap(
        &self,
        subscriptions: &[Subscription],
    ) -> Result<Bootstrap, BootstrapError> {
        self.bootstrap_with(subscriptions, false).await
    }

    /// `bootstrap()` over the topology subscription set, including the
    /// `workspace.reordered` capability fallback.
    pub async fn bootstrap_topology(&self) -> Result<Bootstrap, BootstrapError> {
        self.bootstrap_with(&[], true).await
    }

    pub(crate) async fn bootstrap_with(
        &self,
        subscriptions: &[Subscription],
        topology_fallback: bool,
    ) -> Result<Bootstrap, BootstrapError> {
        // `workspaceReorderedReset` → `InvalidateLiveCapabilities`: a
        // (re)connect may answer with a different server build, so
        // socket-observed verdicts drop to `reconnect_required` before the
        // subscription consult — which therefore attempts
        // `workspace.reordered` again unless a *standalone* caller left a
        // same-server verdict in place.
        self.invalidate_live_capabilities();
        let mut stream = if topology_fallback {
            self.subscribe_topology()
                .await
                .map_err(BootstrapError::Subscribe)?
        } else {
            self.subscribe_events(subscriptions)
                .await
                .map_err(BootstrapError::Subscribe)?
        };
        let snapshot = self
            .session_snapshot()
            .await
            .map_err(BootstrapError::Snapshot)?;
        let gap_events = stream.drain();
        Ok(Bootstrap {
            snapshot,
            stream,
            gap_events,
        })
    }

    /// Run the supervised resync loop — resubscribe → snapshot → forward
    /// events as invalidation signals; on any stream end back off and repeat.
    /// Drop the returned stream to stop the task.
    pub fn supervise_events(&self, supervisor: EventSupervisor) -> SupervisorStream {
        supervisor.run(self.clone())
    }

    /// Whether `workspace.reordered` was confirmed (Some(true)), rejected
    /// (Some(false)), or not yet probed (None) by the last bootstrap.
    pub fn workspace_reordered_supported(&self) -> Option<bool> {
        match self.inner.workspace_reordered.load(Ordering::Relaxed) {
            SUBSCRIPTION_SUPPORTED => Some(true),
            SUBSCRIPTION_UNSUPPORTED => Some(false),
            _ => None,
        }
    }

    /// Whether `pane.output_changed` was confirmed (Some(true)), rejected
    /// (Some(false)), or not yet probed (None) by the last topology
    /// subscription. Distinct from the schema's `pane_output_changed`
    /// event payload — this reports the *subscription* outcome.
    pub fn pane_output_changed_supported(&self) -> Option<bool> {
        match self.inner.pane_output_changed.load(Ordering::Relaxed) {
            SUBSCRIPTION_SUPPORTED => Some(true),
            SUBSCRIPTION_UNSUPPORTED => Some(false),
            _ => None,
        }
    }

    // -- capability ledger (`herdrCapabilities.go`) -------------------------

    /// Recompute the capability report: introspect `herdr api schema --json`
    /// (or the configured [`SchemaSource`]), fall back to a single ping
    /// probe, overlay observed notes, publish the diff, and return the
    /// report. Serialized internally — concurrent callers share one
    /// refresh. Never fails: every failure mode degrades features to
    /// `unknown` rather than aborting.
    pub async fn collect_capabilities(&self) -> CapabilityReport {
        crate::capabilities::collect_capabilities(self).await
    }

    /// The last published report — zero before the first
    /// [`Client::collect_capabilities`].
    pub fn capability_status(&self) -> CapabilityReport {
        self.ledger().report().clone()
    }

    /// Evidence for one feature (`FeatureState::Unknown` when untouched).
    pub fn feature(&self, name: &str) -> FeatureEvidence {
        self.capability_status().feature(name)
    }

    /// `Supports` — true only on `FeatureState::Supported`.
    pub fn supports(&self, name: &str) -> bool {
        self.feature(name).state == FeatureState::Supported
    }

    /// `ShouldAttemptWorkspaceReordered` — attempt unless known-unsupported.
    pub fn should_attempt_workspace_reordered(&self) -> bool {
        self.feature(features::WORKSPACE_REORDERED).state != FeatureState::Unsupported
    }

    /// Same consult for `pane.output_changed` — attempt unless
    /// known-unsupported. The published schema adjudication counts here:
    /// `schema_absent` (0.9.1's event-payload-only listing) suppresses the
    /// attempt entirely, so the doomed round-trip is never paid.
    pub fn should_attempt_pane_output_changed(&self) -> bool {
        self.feature(features::PANE_OUTPUT_CHANGED).state != FeatureState::Unsupported
    }

    /// `InvalidateLiveCapabilities` — run at the top of every bootstrap.
    /// Socket-observed verdicts may describe a different build now
    /// (`server.live_handoff`, socket retarget), so they reset to
    /// `unknown`/`reconnect_required` until the next refresh re-derives
    /// them. Identity-scoped notes survive on the ledger.
    pub(crate) fn invalidate_live_capabilities(&self) {
        self.ledger().invalidate_live();
    }

    /// Record a caller-supplied verdict for `name` (replay enrichers etc.).
    pub fn note_feature(&self, name: &str, state: FeatureState, reason: &str) {
        self.ledger().note(name, state, reason);
    }

    /// `herdr api schema --json` — parsed into a [`SchemaRegistry`]. The
    /// raw method catalog for anything that wants more than the curated
    /// feature list; `collect_capabilities` consumes it internally.
    pub async fn api_schema(&self) -> Result<SchemaRegistry, SchemaError> {
        match &self.inner.config.schema_source {
            SchemaSource::Static(reg) => Ok(reg.clone()),
            SchemaSource::Disabled => {
                Err(SchemaError::Cli("schema introspection disabled".to_owned()))
            }
            SchemaSource::Cli => {
                let bin = crate::cli::resolve_herdr_bin(self.inner.config.herdr_bin.as_deref());
                let out = crate::cli::run_cli(
                    &bin,
                    self.inner.transport.socket_path_hint().as_deref(),
                    &["api", "schema", "--json"],
                    self.inner.config.request_timeout,
                )
                .await
                .map_err(|e| SchemaError::Cli(e.to_string()))?;
                let registry = SchemaRegistry::parse(&out)?;
                if registry.is_usable() {
                    Ok(registry)
                } else {
                    Err(SchemaError::Empty)
                }
            }
        }
    }

    /// A definitive socket answer for a tracked method is capability
    /// evidence (`noteSocketFeature`): success supports it, an
    /// unknown-method refusal refutes it. `DispatchedUnknown` is
    /// deliberately silent — the method may have applied. `epoch` is the
    /// ledger epoch captured at dispatch; the note is dropped when a
    /// bootstrap has since invalidated the live set.
    fn note_socket_feature(&self, epoch: u64, name: &str, result: &Result<Value, HerdrError>) {
        let verdict = match result {
            Ok(_) => Some((FeatureState::Supported, "operation_succeeded")),
            Err(HerdrError::Refused { code, .. })
                if UNKNOWN_METHOD_CODES.contains(&code.as_str()) =>
            {
                Some((FeatureState::Unsupported, "method_not_supported"))
            }
            _ => None,
        };
        if let Some((state, reason)) = verdict {
            self.ledger().note_at(epoch, name, state, reason);
        }
    }

    // -- ledger plumbing for `capabilities` -------------------------------

    fn ledger(&self) -> std::sync::MutexGuard<'_, CapabilityLedger> {
        self.inner
            .capabilities
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    pub(crate) async fn capability_refresh_lock(&self) -> tokio::sync::MutexGuard<'_, ()> {
        self.inner.capability_refresh.lock().await
    }

    /// `capabilityEpoch` — the ledger's live epoch, captured before a
    /// dispatch or refresh so stale results are dropped.
    pub(crate) fn capability_epoch(&self) -> u64 {
        self.ledger().live_epoch()
    }

    pub(crate) fn herdr_bin(&self) -> Option<PathBuf> {
        self.inner.config.herdr_bin.clone()
    }

    pub(crate) fn socket_path_hint(&self) -> Option<PathBuf> {
        self.inner.transport.socket_path_hint()
    }

    pub(crate) fn ledger_note_for(
        &self,
        name: &str,
        identity: &str,
    ) -> Option<(FeatureState, String)> {
        self.ledger().fresh_note(name, identity)
    }

    pub(crate) fn ledger_notes_for(&self, identity: &str) -> Vec<(String, FeatureState, String)> {
        self.ledger().fresh_notes(identity)
    }

    /// `applyRefresh` — publish `report` if it is still current (epoch
    /// match) and return the now-current report (generation bumps applied).
    pub(crate) fn apply_capability_report(
        &self,
        epoch: u64,
        report: CapabilityReport,
    ) -> CapabilityReport {
        let mut ledger = self.ledger();
        ledger.apply_refresh(epoch, report);
        ledger.report().clone()
    }
}

impl std::fmt::Debug for Client {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Client")
            .field("transport", &self.inner.transport.describe())
            .field("config", &self.inner.config)
            .finish()
    }
}
